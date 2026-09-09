/*
 * airplay_core.h — flat C ABI for embedding the UxPlay AirPlay engine
 * in-process (Plan B).  Built into uxplay-core.dll (MinGW/UCRT64) and consumed
 * by an MSVC-built Rust host via hand-written FFI.  Only POD + opaque pointers
 * cross this boundary — NO GLib / GStreamer / C++ types — so the ABI is stable
 * across compilers.
 *
 * The engine is single-instance: airplay_core_create() hands back an opaque
 * handle, but internally drives the file-static UxPlay globals.  Do not run two
 * instances concurrently.
 */
#ifndef AIRPLAY_CORE_H
#define AIRPLAY_CORE_H

#include <stdint.h>

#ifdef _WIN32
#  ifdef AIRPLAY_CORE_BUILDING
#    define AIRPLAY_API __declspec(dllexport)
#  else
#    define AIRPLAY_API __declspec(dllimport)
#  endif
#else
#  define AIRPLAY_API
#endif

#ifdef __cplusplus
extern "C" {
#endif

/* Opaque handle.  Single-instance internally; reserved for a future multi-
 * instance refactor. */
typedef struct airplay_core airplay_core_t;

/* level mirrors UxPlay's logger.h numeric levels (LOGGER_ERR=3 .. DEBUG). */
typedef void (*airplay_log_cb)(int level, const char *msg, void *user);

AIRPLAY_API airplay_core_t *airplay_core_create(void);

/* Renderer host window (HWND on Windows).  Pass the window WE own; the engine
 * renders into it via GstVideoOverlay instead of creating its own.  Pass NULL
 * to let the engine create its own window (B2 smoke-test mode). */
AIRPLAY_API int  airplay_core_set_window(airplay_core_t *c, void *hwnd);

/* Name shown in the iPhone/Mac AirPlay picker (UxPlay -n). UTF-8. */
AIRPLAY_API int  airplay_core_set_device_name(airplay_core_t *c, const char *utf8);

/* Extra UxPlay-style argv tokens as one whitespace-separated string, e.g.
 * "-vs d3d11videosink -vd nvh264dec -fps 60 -nh -nohold".  Split QUOTE-AWARE:
 * an option value containing spaces must be wrapped in "..." to stay one
 * token, as the Windows host does for its -vs value. */
AIRPLAY_API int  airplay_core_set_options(airplay_core_t *c, const char *argv_tail);

AIRPLAY_API void airplay_core_set_log_callback(airplay_core_t *c, airplay_log_cb cb, void *user);

/* Start the RAOP server + GStreamer on a dedicated worker thread; returns
 * immediately.  Non-zero return = failed to start (e.g. already running). */
AIRPLAY_API int  airplay_core_start(airplay_core_t *c);

/* Signal the internal GMainLoop to quit and join the worker.  Idempotent. */
AIRPLAY_API void airplay_core_stop(airplay_core_t *c);

AIRPLAY_API void airplay_core_destroy(airplay_core_t *c);

#ifdef __cplusplus
}
#endif

#endif /* AIRPLAY_CORE_H */
