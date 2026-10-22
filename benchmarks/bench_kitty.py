# SPDX-License-Identifier: GPL-3.0-or-later
"""Benchmark the C implementation of kitty against the Rust port.

This script is the C side of `benchmarks/`. It imports the compiled
`kitty.fast_data_types` extension from a kitty source checkout and measures the
screen/parser/wcswidth workloads that `crates/kitty-screen/examples/compare.rs`
mirrors. SIMD primitives are measured by `bench_simd.c` instead, so that the
Python buffer marshalling of kitty's test wrappers does not dominate.

Environment:
  KITTY_SRC   path to the kitty source checkout (default: /home/qrsnap/kitty)
  PYTHONPATH   must allow importing the kitty package from KITTY_SRC

Build the C extension first (release, not --debug). In the kitty checkout:

  python3 setup.py build   # requires Python >= 3.12 and kitty's build deps

Then:

  taskset -c 3 python3 benchmarks/bench_kitty.py
"""

import os
import sys
import time

KITTY_SRC = os.environ.get('KITTY_SRC', '/home/qrsnap/kitty')
sys.path.insert(0, KITTY_SRC)

import kitty.fast_data_types as f  # noqa: E402
from kitty_tests.base import BaseTest, parse_bytes  # noqa: E402

COLS, ROWS = 120, 40


def timed(name, work, fn, iters, warm=1):
    for _ in range(warm):
        fn()
    t0 = time.perf_counter()
    for _ in range(iters):
        fn()
    dt = time.perf_counter() - t0
    print(f"{name}\t{work * iters / dt:.2f}", flush=True)


def main():
    bt = BaseTest()
    bt.set_options()

    s = bt.create_screen(cols=COLS, lines=ROWS, scrollback=1000)
    pattern = 'The quick brown fox jumps over the lazy dog 0123456789 '
    text = (pattern * ((COLS * ROWS // len(pattern)) + 1))[:COLS * ROWS]
    timed('screen_draw_ascii_cells_s', len(text), lambda: s.draw(text), 5000)

    s = bt.create_screen(cols=COLS, lines=ROWS, scrollback=1000)
    wide = '界' * (COLS * ROWS // 2)
    timed('screen_draw_wide_cells_s', len(wide) * 2, lambda: s.draw(wide), 2000)

    s = bt.create_screen(cols=COLS, lines=ROWS, scrollback=1000)
    stream = bytearray()
    line = b'\x1b[38;2;255;128;0mThe quick brown fox jumps over the lazy dog 0123456789\x1b[0m\r\n'
    lines = 0
    while len(stream) < 16 * 1024 * 1024:
        stream += line
        if lines % 64 == 0:
            stream += b'\x1b[1;1H\x1b[2J'
        lines += 1
    stream = bytes(stream)
    t0 = time.perf_counter()
    for _ in range(4):
        parse_bytes(s, stream)
    dt = time.perf_counter() - t0
    print(f"parser_screen_MiB_s\t{len(stream) * 4 / dt / (1024 * 1024):.2f}", flush=True)

    ascii_str = ('The quick brown fox 0123456789 ' * (1_000_000 // 32 + 1))[:1_000_000]
    unicode_str = ('a界😀e\u0301ก\u0e33' * (1_000_000 // 8 + 1))[:1_000_000]
    ansi_str = ('a\x1b[31mred\x1b[0m界\n' * (1_000_000 // 16 + 1))[:1_000_000]
    for name, value in (
        ('wcswidth_ascii_Mchars_s', ascii_str),
        ('wcswidth_unicode_Mchars_s', unicode_str),
        ('wcswidth_ansi_Mchars_s', ansi_str),
    ):
        timed(name, len(value) / 1e6, lambda t=value: f.wcswidth(t), 10)


if __name__ == '__main__':
    main()
