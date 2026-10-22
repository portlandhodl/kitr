# Benchmarks: kitr vs kitty

This compares the Rust port against kitty's C implementation on the subsystems
that are actually ported. **kitr is not a runnable terminal yet**, so this is a
component benchmark, not an end-to-end terminal latency test. There is no
rendering, font shaping, PTY I/O or input loop in the numbers below.

## Environment

- CPU: Intel Xeon E5-2699 v4 (2.20 GHz base, 2 sockets, 88 threads); no AVX-512,
  SSE4.2 and AVX2 available
- OS: Linux 5.14.0-757.el9.x86_64
- Rust: rustc 1.98.1, `--release`, `opt-level=3`, fat LTO, one codegen unit
- C: clang 23.1.3, `-O3 -flto -march=native` (kitty release build, **not**
  the debug build)
- Python binding: CPython 3.12 (kitty requires >= 3.12)
- Single-threaded, pinned to one core with `taskset`. Each number is the median
  of 5 alternating C/Rust runs (screen/parser/wcswidth on one core, SIMD on
  another). The host is shared and was under load, so treat differences under
  ~10% as noise.

## Method

C screen/parser/wcswidth numbers use kitty's own `kitty.fast_data_types`
extension through its Python API (`benchmarks/bench_kitty.py`). Rust numbers use
`crates/kitty-screen/examples/compare.rs`, which mirrors the same workloads.

The C SIMD primitives are measured by `benchmarks/bench_simd.c`, a native C
harness linked against the compiled kitty objects. kitty's Python test wrappers
allocate and copy the buffer per call, which hides the primitive speed; the
harness calls the same functions in place with the same sizes as the Rust side
(`cargo run --release -p kitty-simd --example simd_bench -- --c-harness`).

Reproduce:

```sh
# In the kitty checkout (release build):
python3 setup.py build

# C component benchmarks
KITTY_SRC=/path/to/kitty taskset -c 7 python3 benchmarks/bench_kitty.py

# C SIMD harness (build command in benchmarks/bench_simd.c)
taskset -c 6 ./bench_simd

# Rust
taskset -c 7 cargo run --release -p kitty-screen --example compare
taskset -c 6 cargo run --release -p kitty-simd --example simd_bench -- --c-harness
```

## Screen, parser and width

| Workload | kitty (C) | kitr (Rust) | Rust / C |
|---|---:|---:|---:|
| `screen_draw_ascii_cells_s` | 31.56 M cells/s | 126.24 M cells/s | **4.00x** |
| `screen_draw_wide_cells_s` | 35.47 M cells/s | 58.55 M cells/s | **1.65x** |
| `parser_screen_MiB_s` (16 MiB mixed stream, 4 passes) | 35.37 MiB/s | 83.89 MiB/s | **2.37x** |
| `wcswidth_ascii_Mchars_s` | 109.58 Mchar/s | 4489.71 Mchar/s | **40.97x** |
| `wcswidth_unicode_Mchars_s` | 142.23 Mchar/s | 157.88 Mchar/s | **1.11x** |
| `wcswidth_ansi_Mchars_s` | 184.15 Mchar/s | 300.81 Mchar/s | **1.63x** |

The screen and parser wins are partly structural: the Rust API takes `&[u32]`
directly and has no Python object marshalling. The C screen also does work the
Rust subset does not implement yet (hyperlinks, image placeholders, marks,
selections, multicell commands), so the draw figures are an upper bound for the
ported subset, not a claim that the full screen is 4x faster. The parser stream
includes truecolor SGR, CUP and ED, and is dispatched into the screen in both
implementations.

The ASCII `wcswidth` number comes from a provably-equivalent fast path over
runs of printable ASCII (the C implementation steps one char at a time). The
fast path is cross-checked against the compiled C `wcswidth` over all code
points and randomized ANSI/Unicode strings; Unicode and ANSI rows are the more
representative comparison.

## SIMD primitives

Sizes: 16 MiB for `find`, `printable`, `xor`; 4 MiB for UTF-8. Ratios are
Rust / C; >1 means kitr faster. C scalar/default and 128/256-bit paths are
called directly; the machine has no AVX-512.

| Primitive | Width | kitty (C) | kitr (Rust) | Rust / C |
|---|---|---:|---:|---:|
| find either of two bytes | scalar | 2.53 GiB/s | 4.21 GiB/s | **1.66x** |
| | SSE (128) | 10.96 GiB/s | 13.46 GiB/s | **1.23x** |
| | AVX2 (256) | 17.65 GiB/s | 19.91 GiB/s | **1.13x** |
| printable ASCII run | scalar | 6.38 GiB/s | 7.47 GiB/s | **1.17x** |
| | SSE (128) | 7.46 GiB/s | 7.27 GiB/s | 0.97x |
| | AVX2 (256) | 8.60 GiB/s | 8.58 GiB/s | 1.00x |
| XOR with 64-byte key | scalar | 1.47 GiB/s | 14.07 GiB/s | **9.57x** |
| | SSE (128) | 13.93 GiB/s | 13.45 GiB/s | 0.97x |
| | AVX2 (256) | 13.60 GiB/s | 14.79 GiB/s | **1.09x** |
| UTF-8 decode, ASCII | scalar | 161.90 MiB/s | 254.32 MiB/s | **1.57x** |
| | SSE (128) | 623.26 MiB/s | 655.61 MiB/s | **1.05x** |
| | AVX2 (256) | 1414.43 MiB/s | 1980.05 MiB/s | **1.40x** |
| UTF-8 decode, multibyte | scalar | 197.43 MiB/s | 351.50 MiB/s | **1.78x** |
| | SSE (128) | 464.62 MiB/s | 497.94 MiB/s | **1.07x** |
| | AVX2 (256) | 684.48 MiB/s | 824.66 MiB/s | **1.20x** |

The earlier multibyte UTF-8 regression (0.67x at AVX2) is fixed. The remaining
sub-1.0 rows are within 3% and are noise-level on this host; re-measuring tends
to flip them. Scalar XOR is unusually fast in Rust because LLVM auto-vectorizes
the 64-byte-chunk loop even in the "scalar" implementation.

## Not benchmarked yet

- End-to-end terminal latency, frame time, GPU upload and rendering
- Font loading/shaping/rasterization
- PTY/child I/O and event-loop wakeup latency
- Keyboard/mouse encoding (functional tests only)
- `LineBuf`/`HistoryBuf` hot loops through a comparable C harness (the Python
  API overhead per operation dominates and would make the numbers meaningless)
- Memory usage and startup time

## Verdict

On the ported pieces, kitr is now at parity or ahead on every measured row,
usually by 1.1x-2x, with the largest wins in native screen/parser dispatch and
the ASCII width fast path. The result still says nothing about terminal
performance as a whole until the renderer, fonts and I/O loop exist. Re-run
these benchmarks after each phase rather than treating them as a product claim.

## GUI tabs and remote control

`crates/kitty/examples/bench_tabs_remote.rs` measures the new multi-tab GUI and
the remote-control round trip. It starts its own Xvfb when `$DISPLAY` is unset,
uses two `/bin/sh -c 'sleep 20'` tabs for the tab loop, then runs 200
`get-text` requests over a Unix socket against a GUI loop processing one
`step()` per iteration. Tab-switch numbers include selecting the tab and a full
frame render (all panes plus the tab strip).

Reproduce:

```sh
cargo run -p kitty --release --example bench_tabs_remote
```

Measured on the same host as the table above (release build, Xvfb, no DRI3
hardware acceleration):

| Workload | Result |
|---|---:|
| tab switch + render | 1.21 ms/op (829 ops/s) |
| remote `get-text` round trip | 1.174 ms/op (852 ops/s) |

These are GUI-loop throughput numbers, not end-to-end typing latency. The
remote round trip is intentionally capped by a 5 ms GUI poll interval while a
remote server is active, so the floor is roughly poll scheduling plus socket
and JSON framing.
