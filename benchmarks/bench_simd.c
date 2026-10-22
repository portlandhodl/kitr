/* SPDX-License-Identifier: GPL-3.0-or-later */
/*
 * Native C harness for kitty's SIMD primitives.
 *
 * kitty's Python test wrappers allocate and copy a buffer for every call. This
 * harness links the already-compiled kitty objects and calls the same functions
 * in place, so the comparison against kitty-simd measures the primitives rather
 * than PyBuffer overhead.
 *
 * Build after `python3 setup.py build` in the kitty checkout (release build):
 *
 *   clang -flto -O3 -march=native -Ikitty -I$PY_INCLUDE bench_simd.c \
 *     build/fast_data_types-kitty-simd-string{,-128,-256,-512}.c.o \
 *     build/fast_data_types-kitty-charsets.c.o \
 *     -L$PY_LIB -Wl,-rpath,$PY_LIB -lpython3.12 -lm -lpthread -ldl -lutil \
 *     -o bench_simd
 *
 * Run pinned: taskset -c 4 ./bench_simd
 */

#include "simd-string.h"
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

void log_error(const char *fmt, ...) { (void)fmt; }

static double now(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec + ts.tv_nsec / 1e9;
}

#define N (16u * 1024u * 1024u)
#define UN (4u * 1024u * 1024u)

static uint8_t *haystack, *xdata, *ascii_bytes, *utf8_bytes;
static uint32_t *chars;
static uint8_t key[64];
static volatile uint64_t sink;

int main(void) {
    posix_memalign((void **)&haystack, 64, N);
    posix_memalign((void **)&xdata, 64, N);
    posix_memalign((void **)&chars, 64, (size_t)N * 4);
    posix_memalign((void **)&ascii_bytes, 64, UN);
    posix_memalign((void **)&utf8_bytes, 64, UN);
    memset(haystack, 'a', N);
    haystack[N - 1] = '<';
    memset(xdata, 0x5a, N);
    for (size_t i = 0; i < N; i++) chars[i] = 32 + (i % 95);
    for (size_t i = 0; i < UN; i++) ascii_bytes[i] = 32 + (i % 95);
    const char *unit = "a界😀e\xcc\x81ก\u0e33";
    size_t ulen = strlen(unit), p = 0;
    while (p < UN) {
        size_t n = ulen;
        if (p + n > UN) n = UN - p;
        memcpy(utf8_bytes + p, unit, n);
        p += n;
    }
    memset(key, 0x42, 64);

    unsigned levels[3] = {0, 128, 256};
    for (int li = 0; li < 3; li++) {
        unsigned level = levels[li];
        const char *tag = level == 0 ? "scalar" : "simd";
        double t0, dt;

        t0 = now();
        for (int i = 0; i < 8; i++) {
            const uint8_t *r = level == 0 ? find_either_of_two_bytes(haystack, N, '<', '>')
                : level == 128 ? find_either_of_two_bytes_128(haystack, N, '<', '>')
                : find_either_of_two_bytes_256(haystack, N, '<', '>');
            sink += (uint64_t)(uintptr_t)r;
        }
        dt = now() - t0;
        printf("simd_find_%s_%u_GiB_s\t%.2f\n", tag, level, (double)N * 8 / dt / (1024.0 * 1024 * 1024));

        t0 = now();
        for (int i = 0; i < 8; i++) {
            sink += level == 0 ? printable_ascii_run_length(chars, N)
                : level == 128 ? printable_ascii_run_length_128(chars, N)
                : printable_ascii_run_length_256(chars, N);
        }
        dt = now() - t0;
        printf("simd_printable_%s_%u_GiB_s\t%.2f\n", tag, level, (double)N * 4 * 8 / dt / (1024.0 * 1024 * 1024));

        t0 = now();
        for (int i = 0; i < 8; i++) {
            if (level == 0) xor_data64(key, xdata, N);
            else if (level == 128) xor_data64_128(key, xdata, N);
            else xor_data64_256(key, xdata, N);
        }
        dt = now() - t0;
        printf("simd_xor_%s_%u_GiB_s\t%.2f\n", tag, level, (double)N * 8 / dt / (1024.0 * 1024 * 1024));

        UTF8Decoder d;
        memset(&d, 0, sizeof(d));
        utf8_decoder_ensure_capacity(&d, UN + 4096);
        t0 = now();
        for (int i = 0; i < 2; i++) {
            d.output.pos = 0;
            d.state.cur = d.state.prev = d.state.codep = 0;
            if (level == 0) utf8_decode_to_esc(&d, ascii_bytes, UN);
            else if (level == 128) utf8_decode_to_esc_128(&d, ascii_bytes, UN);
            else utf8_decode_to_esc_256(&d, ascii_bytes, UN);
        }
        dt = now() - t0;
        printf("simd_utf8_ascii_%s_%u_MiB_s\t%.2f\n", tag, level, (double)UN * 2 / dt / (1024.0 * 1024));

        t0 = now();
        for (int i = 0; i < 2; i++) {
            d.output.pos = 0;
            d.state.cur = d.state.prev = d.state.codep = 0;
            if (level == 0) utf8_decode_to_esc(&d, utf8_bytes, UN);
            else if (level == 128) utf8_decode_to_esc_128(&d, utf8_bytes, UN);
            else utf8_decode_to_esc_256(&d, utf8_bytes, UN);
        }
        dt = now() - t0;
        printf("simd_utf8_unicode_%s_%u_MiB_s\t%.2f\n", tag, level, (double)UN * 2 / dt / (1024.0 * 1024));
        utf8_decoder_free(&d);
    }

    free(haystack);
    free(xdata);
    free(chars);
    free(ascii_bytes);
    free(utf8_bytes);
    return 0;
}
