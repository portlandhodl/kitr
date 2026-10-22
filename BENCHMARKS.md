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
- Single-threaded, pinned to one core with `taskset`; one run after warmup.
  Expect roughly 5-10% run-to-run variation on this shared machine.

## Method

C screen/parser/wcswidth numbers use kitty's own `kitty.fast_data_types`
extension through its Python API (`benchmarks/bench_kitty.py`). Rust numbers use
`crates/kitty-screen/examples/compare.rs`, which mirrors the same workloads.

The C SIMD primitives are measured by `benchmarks/bench_simd.c`, a native C
harness linked against the compiled kitty objects. kitty's Python test wrappers
allocate and copy the buffer per call, which hides the primitive speed; the
harness calls the same functions in place with the same sizes as the Rust side.

Reproduce:

```sh
# In the kitty checkout (release build):
python3 setup.py build

# C component benchmarks
KITTY_SRC=/path/to/kitty taskset -c 3 python3 benchmarks/bench_kitty.py

# C SIMD harness (see the build command in benchmarks/bench_simd.c)
taskset -c 4 ./bench_simd

# Rust
cargo run --release -p kitty-screen --example compare
```

## Screen, parser and width

| Workload | kitty (C) | kitr (Rust) | Rust / C |
|---|---:|---:|---:|
| `screen_draw_ascii_cells_s` | 32.07 M cells/s | 98.85 M cells/s | **3.08x** |
| `screen_draw_wide_cells_s` | 37.33 M cells/s | 46.81 M cells/s | **1.25x** |
| `parser_screen_MiB_s` (16 MiB mixed stream, 4 passes) | 37.98 MiB/s | 49.46 MiB/s | **1.30x** |
| `wcswidth_ascii_Mchars_s` | 126.40 Mchar/s | 116.81 Mchar/s | 0.92x |
| `wcswidth_unicode_Mchars_s` | 135.21 Mchar/s | 135.63 Mchar/s | 1.00x |
| `wcswidth_ansi_Mchars_s` | 183.14 Mchar/s | 174.56 Mchar/s | 0.95x |

The screen and parser wins are partly structural: the Rust API takes `&[u32]`
directly and has no Python object marshalling. The C screen also does work the
Rust subset does not implement yet (hyperlinks, image placeholders, marks,
selections, multicell commands), so the draw figures are an upper bound for the
ported subset, not a claim that the full screen is 3x faster.

`wcswidth` is implemented in both languages from the same Unicode 17 tables;
parity here is expected, and C is currently slightly faster on ASCII/ANSI.

## SIMD primitives

Sizes: 16 MiB for `find`, `printable`, `xor`; 4 MiB for UTF-8. Ratios are
Rust / C; >1 means kitr faster. C scalar/default and 128/256-bit paths are
called directly; the machine has no AVX-512.

| Primitive | Width | kitty (C) | kitr (Rust) | Rust / C |
|---|---|---:|---:|---:|
| find either of two bytes | scalar | 2.48 GiB/s | 2.52 GiB/s | 1.02x |
| | SSE (128) | 10.83 GiB/s | 14.49 GiB/s | **1.34x** |
| | AVX2 (256) | 15.41 GiB/s | 18.18 GiB/s | **1.18x** |
| printable ASCII run | scalar | 5.31 GiB/s | 6.42 GiB/s | **1.21x** |
| | SSE (128) | 6.51 GiB/s | 6.91 GiB/s | 1.06x |
| | AVX2 (256) | 6.82 GiB/s | 8.00 GiB/s | **1.17x** |
| XOR with 64-byte key | scalar | 1.45 GiB/s | 1.27 GiB/s | 0.88x |
| | SSE (128) | 12.55 GiB/s | 13.57 GiB/s | 1.08x |
| | AVX2 (256) | 13.30 GiB/s | 14.28 GiB/s | 1.07x |
| UTF-8 decode, ASCII | scalar | 150.17 MiB/s | 173.58 MiB/s | 1.16x |
| | SSE (128) | 520.90 MiB/s | 1042.19 MiB/s | **2.00x** |
| | AVX2 (256) | 1170.48 MiB/s | 1701.38 MiB/s | **1.45x** |
| UTF-8 decode, multibyte | scalar | 201.45 MiB/s | 180.61 MiB/s | 0.90x |
| | SSE (128) | 463.26 MiB/s | 392.67 MiB/s | 0.85x |
| | AVX2 (256) | 742.16 MiB/s | 494.69 MiB/s | **0.67x** |

Find, printable-ASCII and ASCII UTF-8 decoding are faster in Rust. The clear
regression is **multibyte UTF-8 decoding**, especially on the AVX2 path: kitr is
33% slower. That path is the highest-priority optimization target before the
parser is switched from its scalar DFA to `kitty-simd`.

## Not benchmarked yet

- End-to-end terminal latency, frame time, GPU upload and rendering
- Font loading/shaping/rasterization
- PTY/child I/O and event-loop wakeup latency
- Keyboard/mouse encoding (functional tests only)
- `LineBuf`/`HistoryBuf` hot loops through a comparable C harness (the Python
  API overhead per operation dominates and would make the numbers meaningless)
- Memory usage and startup time

## Verdict

On the ported pieces, kitr is between parity and about 3x faster, with one
real regression in multibyte UTF-8 SIMD decoding. That does not say anything
about terminal performance as a whole until the renderer, fonts and I/O loop
exist. Re-run this benchmark after each phase rather than treating these numbers
as a product claim.
