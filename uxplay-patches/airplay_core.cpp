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
#include <cctype>    /* isspace(), for the quote-aware argv splitter below */
#include <thread>
#include <mutex>
#include <cstring>

/* Entry points exported by uxplay.cpp (same TU set, C linkage). */
extern "C" int  airplay_run_blocking(int argc, char *argv[]);
extern "C" void airplay_request_shutdown(void);
/* Arms the engine's shutdown latch for a new run.  MUST be called from this
 * thread before the worker is spawned -- the worker cannot clear it itself
 * without losing a stop that arrives while it is still starting up. */
extern "C" void airplay_clear_shutdown_request(void);
extern "C" void airplay_set_host_window(void *hwnd);
/* uxplay.cpp's single log-forward hook: log() forwards every line (BEFORE the
 * level filter) so the host can detect markers ("Begin streaming", "Open
 * connections: 0", "begin video stream wxh = ..."). Same signature as
 * airplay_log_cb, so we forward the host's callback straight through. */
extern "C" void airplay_set_log_forward(airplay_log_cb fn, void *user);
/* When embedded as a library, cleanup() must RETURN, not exit() the host process
 * (UxPlay's standalone cleanup() ends with exit(0)). Without this, airplay_core_stop()
 * -> ... -> cleanup() would kill the whole tray app on Stop/window-close. */
extern "C" void airplay_set_library_mode(int on);

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

/* Split the host's option string into argv tokens.
 *
 * MUST STAY QUOTE-AWARE.  The Windows host passes a videosink whose value has
 * spaces in it -- -vs "d3d11videosink fullscreen-toggle-mode=none ..." -- as a
 * SINGLE option value (see engine.rs).  A whitespace-only split re-splits that,
 * parse_arguments() then meets "fullscreen-toggle-mode=none" as an unknown
 * option, and the engine refuses to start on every Start.  The shipping
 * uxplay-core.dll was built from a quote-aware source; this is that behaviour,
 * back in the tree (audit finding 9), so a rebuild from here matches it.
 *
 * A token that OPENS with a quote runs to the next quote (quotes stripped, an
 * unterminated one runs to end of string); anything else runs to whitespace, so
 * a quote in the middle of a token is an ordinary character.  No backslash
 * escapes.  That is deliberately narrow, and it is copied VERBATIM from the
 * canonical fork (Recluse/UxPlay, branch popyachsa-integration) rather than
 * reimplemented: matching the shipping DLL byte-for-byte is the entire point of
 * audit finding 9, and a "better" splitter here would recreate the divergence in
 * a subtler form. Change it there first, then copy it here. */
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
    airplay_set_library_mode(1); /* cleanup() must return, not exit() the host */
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

    /* Hand the host HWND to the renderer before the engine inits video. */
    airplay_set_host_window(c->hwnd);
    /* Wire the host log callback so it receives UxPlay's log markers (connect/
     * disconnect/size). Without this the callback is stored but never called. */
    airplay_set_log_forward(c->log_cb, c->log_user);

    int argc = static_cast<int>(c->argv_ptrs.size() - 1);
    char **argv = c->argv_ptrs.data();

    /* Arm the latch here, under c->lifecycle, so it is ordered against
     * airplay_core_stop(): from now until the worker returns, a stop cannot be
     * lost, not even one issued before the worker has entered the engine. */
    airplay_clear_shutdown_request();

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
    airplay_set_host_window(nullptr);
    airplay_set_log_forward(nullptr, nullptr);
}

extern "C" AIRPLAY_API void airplay_core_destroy(airplay_core_t *c) {
    if (!c) return;
    airplay_core_stop(c);
    delete c;
}
