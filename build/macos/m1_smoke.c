/* M1 smoke harness (macOS): dlopen uxplay-core.dylib, resolve the 8 C-ABI
 * exports, call airplay_core_create()/destroy(). No GStreamer pipeline is
 * started (create() only allocates), so this validates: (1) the dylib LOADS
 * with RTLD_NOW -> all GStreamer/openssl/libplist deps resolve, (2) all 8
 * airplay_core_* symbols are present, (3) create() returns non-NULL.
 *
 * build: clang -o m1_smoke m1_smoke.c
 * run:   ./m1_smoke /path/to/uxplay-core.dylib
 */
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>

typedef void *(*fn_create)(void);
typedef int   (*fn_set_window)(void *, void *);
typedef int   (*fn_set_str)(void *, const char *);
typedef void  (*fn_set_logcb)(void *, void *, void *);
typedef int   (*fn_start)(void *);
typedef void  (*fn_void)(void *);

static const char *EXPORTS[] = {
    "airplay_core_create", "airplay_core_set_window", "airplay_core_set_device_name",
    "airplay_core_set_options", "airplay_core_set_log_callback", "airplay_core_start",
    "airplay_core_stop", "airplay_core_destroy",
};

int main(int argc, char **argv) {
    const char *path = (argc > 1) ? argv[1] : "uxplay-core.dylib";
    void *h = dlopen(path, RTLD_NOW | RTLD_LOCAL);
    if (!h) { fprintf(stderr, "[smoke] dlopen FAILED: %s\n", dlerror()); return 2; }
    printf("[smoke] dlopen OK: %s\n", path);

    int missing = 0;
    for (size_t i = 0; i < sizeof(EXPORTS)/sizeof(EXPORTS[0]); i++) {
        void *sym = dlsym(h, EXPORTS[i]);
        printf("[smoke]   %-32s %s\n", EXPORTS[i], sym ? "FOUND" : "MISSING");
        if (!sym) missing++;
    }
    if (missing) { fprintf(stderr, "[smoke] %d export(s) missing\n", missing); return 3; }

    fn_create create   = (fn_create) dlsym(h, "airplay_core_create");
    fn_void   destroy  = (fn_void)   dlsym(h, "airplay_core_destroy");
    void *c = create();
    printf("[smoke] airplay_core_create() -> %p (%s)\n", c, c ? "non-NULL OK" : "NULL FAIL");
    if (!c) return 4;
    destroy(c);
    printf("[smoke] airplay_core_destroy() OK\n");
    printf("[smoke] PASS\n");
    return 0;
}
