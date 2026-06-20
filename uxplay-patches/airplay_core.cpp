/*
 * UxPlay - An open-source AirPlay mirroring server
 * Copyright (C) 2021-24 F. Duncanh
 * uxplay-core embeddable-library additions
 * Copyright (C) 2026 Recluse
 *
 * This program is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation; either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program; if not, write to the Free Software Foundation,
 * Inc., 51 Franklin Street, Fifth Floor, Boston, MA 02110-1301  USA
 */

/*
 * airplay_core.cpp — implementation of the flat C ABI declared in
 * airplay_core.h.  Drives the single-instance UxPlay engine: builds an argv
 * vector from the host's settings and runs the (refactored) main() body
 * airplay_run_blocking() on a worker thread.
 *
 * Compiled into uxplay-core.dll alongside uxplay.cpp + the airplay/renderers
 * static libs.
 */
#define AIRPLAY_CORE_BUILDING
#include "airplay_core.h"

#include <string>
#include <vector>
#include <sstream>
#include <thread>
#include <mutex>
#include <cstring>
#include <cctype>

/* Entry points exported by uxplay.cpp (same TU set, C linkage). */
extern "C" int  airplay_run_blocking(int argc, char *argv[]);
extern "C" void airplay_request_shutdown(void);
extern "C" void airplay_set_host_window(void *hwnd);
extern "C" void airplay_set_library_mode(int on);
extern "C" void airplay_set_log_forward(airplay_log_cb fn, void *user);

struct airplay_core {
    std::string       device_name;
    std::string       options;      /* extra argv tail, whitespace-separated */
    void             *hwnd = nullptr;

    airplay_log_cb    log_cb = nullptr;    void *log_user = nullptr;

    std::thread       worker;
    std::mutex        lifecycle;
    bool              running = false;

    /* argv storage kept alive for the worker's lifetime */
    std::vector<std::string> argv_store;
    std::vector<char *>      argv_ptrs;
};

/* Whitespace-split, but a double-quoted run is one token with its inner spaces
 * preserved (and the quotes stripped).  Needed because UxPlay options like
 * `-vs "d3d11videosink prop=a prop=b"` carry a single multi-word argument. */
static void split_args(const std::string &s, std::vector<std::string> &out) {
    size_t i = 0, n = s.size();
    while (i < n) {
        while (i < n && std::isspace((unsigned char) s[i])) i++;
        if (i >= n) break;
        std::string tok;
        if (s[i] == '"') {
            i++;                                   /* skip opening quote */
            while (i < n && s[i] != '"') tok.push_back(s[i++]);
            if (i < n) i++;                        /* skip closing quote */
        } else {
            while (i < n && !std::isspace((unsigned char) s[i])) tok.push_back(s[i++]);
        }
        out.push_back(tok);
    }
}

extern "C" AIRPLAY_API airplay_core_t *airplay_core_create(void) {
    return new (std::nothrow) airplay_core();
}

extern "C" AIRPLAY_API int airplay_core_set_window(airplay_core_t *c, void *hwnd) {
    if (!c) return -1;
    c->hwnd = hwnd;
    return 0;
}

extern "C" AIRPLAY_API int airplay_core_set_device_name(airplay_core_t *c, const char *utf8) {
    if (!c) return -1;
    c->device_name = (utf8 ? utf8 : "");
    return 0;
}

extern "C" AIRPLAY_API int airplay_core_set_options(airplay_core_t *c, const char *argv_tail) {
    if (!c) return -1;
    c->options = (argv_tail ? argv_tail : "");
    return 0;
}

extern "C" AIRPLAY_API void airplay_core_set_log_callback(airplay_core_t *c, airplay_log_cb cb, void *user) {
    if (!c) return;
    c->log_cb = cb; c->log_user = user;
}

extern "C" AIRPLAY_API int airplay_core_start(airplay_core_t *c) {
    if (!c) return -1;
    std::lock_guard<std::mutex> lk(c->lifecycle);
    if (c->running) return -2;

    /* Build argv: [prog, -n, <name>, <options...>]. */
    c->argv_store.clear();
    c->argv_ptrs.clear();
    c->argv_store.push_back("uxplay-core");
    if (!c->device_name.empty()) {
        c->argv_store.push_back("-n");
        c->argv_store.push_back(c->device_name);
    }
    split_args(c->options, c->argv_store);

    c->argv_ptrs.reserve(c->argv_store.size() + 1);
    for (auto &s : c->argv_store) {
        c->argv_ptrs.push_back(const_cast<char *>(s.c_str()));
    }
    c->argv_ptrs.push_back(nullptr);

    /* Library mode: cleanup() must not exit() the host process. */
    airplay_set_library_mode(1);
    /* Forward UxPlay log lines to the host's callback (connection markers). */
    if (c->log_cb) {
        airplay_set_log_forward(c->log_cb, c->log_user);
    }
    /* Hand the host HWND to the renderer before the engine inits video. */
    airplay_set_host_window(c->hwnd);

    int argc = static_cast<int>(c->argv_ptrs.size() - 1);
    char **argv = c->argv_ptrs.data();

    c->running = true;
    c->worker = std::thread([c, argc, argv]() {
        airplay_run_blocking(argc, argv);
    });
    return 0;
}

extern "C" AIRPLAY_API void airplay_core_stop(airplay_core_t *c) {
    if (!c) return;
    std::lock_guard<std::mutex> lk(c->lifecycle);
    if (!c->running) return;
    airplay_request_shutdown();
    if (c->worker.joinable()) {
        c->worker.join();
    }
    c->running = false;
    airplay_set_log_forward(nullptr, nullptr);
    airplay_set_host_window(nullptr);
}

extern "C" AIRPLAY_API void airplay_core_destroy(airplay_core_t *c) {
    if (!c) return;
    airplay_core_stop(c);
    delete c;
}
