/* Self-check for lib/airplay_video.c :: adjust_yt_condensed_playlist().
 *
 * The function expands YouTube's condensed media playlist (#YT-EXT-CONDENSED-URL)
 * into a normal one. Its output buffer was sized `count * (base_uri_len +
 * params_len)`, which is wrong by (prefix_len - 2) FOR EVERY CHUNK: each chunk
 * writes two '/' separators per parameter, and the comma between parameters is
 * not copied. So:
 *
 *   prefix_len < 2  ->  the expansion writes past the allocation.
 *   prefix_len > 2  ->  the buffer is longer than the content, the gap is
 *                       uninitialised malloc memory, and the NUL sits at the end
 *                       of the ALLOCATION rather than the end of the text.
 *                       http_handlers.h sets the HTTP response length with
 *                       strlen() on this buffer, so the media playlist handed to
 *                       the player carries heap garbage after #EXT-X-ENDLIST —
 *                       for a ~1000-chunk YouTube VOD, kilobytes of it. GStreamer
 *                       answers with "Internal data stream error".
 *
 * The `assert(byte_count == new_len)` in the function could never catch this:
 * every shipped build is -DNDEBUG.
 *
 * The defect is upstream (present at UxPlay fc126fd, untouched by our patches).
 *
 * Build (against the fixed fork, which must be ASan-clean and report equal):
 *   cc -O0 -fsanitize=address -I<UxPlay>/lib -o t_yt t_yt_playlist_expand.c \
 *      <UxPlay>/lib/airplay_video.c ...   # or paste the function in, as below
 *
 * This file carries its own copy of the function so it can be built standalone;
 * keep it in sync when touching the original. It exists because the arithmetic is
 * the kind you cannot check by reading.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int g_byte_count, g_new_len;

/* ---- verbatim copy, with the two length values captured at the end ------- */
static char *adjust_yt_condensed_playlist(const char *media_playlist) {
    const char *base_uri_begin, *params_begin, *prefix_begin;
    size_t base_uri_len, params_len, prefix_len;
    const char* ptr = strstr(media_playlist, "#EXTM3U\n");
    ptr += strlen("#EXTM3U\n");
    if (strncmp(ptr, "#YT-EXT-CONDENSED-URL", strlen("#YT-EXT-CONDENSED-URL"))) {
        size_t len = strlen(media_playlist);
        char *c = (char *) malloc(len + 1);
        memcpy(c, media_playlist, len); c[len] = '\0';
        return c;
    }
    ptr = strstr(ptr, "BASE-URI=");
    base_uri_begin = strchr(ptr, '"'); base_uri_begin++;
    ptr = strchr(base_uri_begin, '"');
    base_uri_len = ptr - base_uri_begin;
    char *base_uri = (char *) calloc(base_uri_len + 1, sizeof(char));
    memcpy(base_uri, base_uri_begin, base_uri_len);

    ptr = strstr(ptr, "PARAMS=");
    params_begin = strchr(ptr, '"'); params_begin++;
    ptr = strchr(params_begin,'"');
    params_len = ptr - params_begin;
    char *params = (char *) calloc(params_len + 1, sizeof(char));
    memcpy(params, params_begin, params_len);

    ptr = strstr(ptr, "PREFIX=");
    prefix_begin = strchr(ptr, '"'); prefix_begin++;
    ptr = strchr(prefix_begin,'"');
    prefix_len = ptr - prefix_begin;
    char *prefix = (char *) calloc(prefix_len + 1, sizeof(char));
    memcpy(prefix, prefix_begin, prefix_len);

    int nparams = 0;
    int *params_size = NULL;
    const char **params_start = NULL;
    if (strlen(params)) {
        nparams = 1;
        const char *comma = strchr(params, ',');
        while (comma) { nparams++; comma++; comma = strchr(comma, ','); }
        params_start = (const char **) calloc(nparams, sizeof(char *));
        params_size = (int *) calloc(nparams, sizeof(int));
        ptr = params;
        for (int i = 0; i < nparams; i++) {
            comma = strchr(ptr, ',');
            params_start[i] = ptr;
            if (comma) { params_size[i] = (int)(comma - ptr); ptr = comma; ptr++; }
            else { params_size[i] = (int)(params + params_len - ptr); break; }
        }
    }

    int count = 0;
    ptr = strstr(media_playlist, "#EXTINF");
    while (ptr) { count++; ptr = strstr(++ptr, "#EXTINF"); }

    size_t old_size = strlen(media_playlist);
    size_t new_len = old_size;
    new_len += count * (base_uri_len + params_len + 2);   /* FIXED (was: without +2) */

    int byte_count = 0;
    char *new_playlist = (char *) malloc(new_len + 1);
    /* Poison, so uninitialised tail bytes are visible instead of accidentally 0. */
    memset(new_playlist, 'Z', new_len + 1);   /* poison, so a gap is visible */
    const char *old_pos = media_playlist;
    char *new_pos = new_playlist;
    ptr = strstr(old_pos, "#EXTINF:");
    size_t len = ptr - old_pos;
    memcpy(new_pos, old_pos, len);
    byte_count += len; old_pos += len; new_pos += len;
    while (ptr) {
        const char *end = NULL;
        const char *start = strstr(ptr, prefix);
        len = start - ptr;
        memcpy(new_pos, old_pos, len);
        byte_count += len; old_pos += len; new_pos += len;
        memcpy(new_pos, base_uri, base_uri_len);
        byte_count += base_uri_len; new_pos += base_uri_len;
        old_pos += prefix_len;
        ptr = strstr(old_pos, "#EXTINF:");
        end = old_pos;
        int last = nparams - 1;
        for (int i = 0; i < nparams; i++) {
            if (i != last) end = strchr(end, '/');
            else end = strstr(end, "#EXT");
            *new_pos = '/'; byte_count++; new_pos++;
            memcpy(new_pos, params_start[i], params_size[i]);
            byte_count += params_size[i]; new_pos += params_size[i];
            *new_pos = '/'; byte_count++; new_pos++;
            len = end - old_pos; end++;
            memcpy(new_pos, old_pos, len);
            byte_count += len; new_pos += len; old_pos += len;
            if (i != last) old_pos++;
        }
    }
    len = media_playlist + strlen(media_playlist) - old_pos;
    memcpy(new_pos, old_pos, len);
    byte_count += len; new_pos += len; old_pos += len;

    *new_pos = '\0';   /* FIXED: terminate where the content ends */
    g_byte_count = byte_count; g_new_len = (int) new_len;
    free(prefix); free(base_uri); free(params);
    free(params_size); free(params_start);
    return new_playlist;
}
/* ------------------------------------------------------------------------- */

static char *build_condensed(int chunks, const char *prefix) {
    /* Shaped like YouTube's condensed media playlist. */
    static char buf[4 << 20];
    int n = snprintf(buf, sizeof(buf),
        "#EXTM3U\n#YT-EXT-CONDENSED-URL BASE-URI=\"https://rr4---sn-4g5e6nez."
        "googlevideo.com/videoplayback/id/9f8e7d6c5b4a/itag/234\","
        "PARAMS=\"sq,dur\",PREFIX=\"%s\"\n", prefix);
    for (int i = 0; i < chunks; i++)
        n += snprintf(buf + n, sizeof(buf) - n,
                      "#EXTINF:5.000,\n%s%d/5.000\n", prefix, i);
    snprintf(buf + n, sizeof(buf) - n, "#EXT-X-ENDLIST\n");
    return buf;
}

int main(void) {
    int bad = 0;
    const char *prefixes[] = { "s", "s/", "sq/", "seg/", "/videoplayback/sq/" };
    printf("%-20s %6s %10s %10s %8s   %s\n",
           "PREFIX", "chunks", "allocated", "written", "slack", "served strlen vs written");
    for (unsigned p = 0; p < sizeof(prefixes)/sizeof(*prefixes); p++) {
        for (int c = 1; c <= 1000; c *= 10) {
            char *in = build_condensed(c, prefixes[p]);
            char *out = adjust_yt_condensed_playlist(in);
            int served = (int) strlen(out);   /* http_handlers.h does exactly this */
            int ok = (served == g_byte_count) && (g_byte_count <= g_new_len);
            if (!ok) bad++;
            printf("%-20s %6d %10d %10d %8d   %s\n",
                   prefixes[p], c, g_new_len, g_byte_count, g_new_len - g_byte_count,
                   ok ? "equal" : "MISMATCH — heap garbage would be served");
            free(out);
        }
    }
    /* Two invariants, and the second is the one the original violated silently:
       what strlen() reports (which IS the served Content-Length) must equal what
       was written, and what was written must fit in what was allocated. */
    printf("\n%s\n", bad ? "FAIL" : "PASS");
    return bad ? 1 : 0;
}
