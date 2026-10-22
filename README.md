# kitr

A Rust rewrite of the [kitty](https://github.com/kovidgoyal/kitty) terminal
emulator, ported subsystem by subsystem with the C implementation as the
behavioral source of truth.

`kitr` is not affiliated with or endorsed by the kitty project. It is a
derivative work of kitty and is licensed under **GPL-3.0-or-later**. See
[License](#license), [NOTICE](NOTICE) and
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## Status

This is an early, incomplete port. It is **not** a usable GUI terminal yet: the
windowing layer, OpenGL renderer, full graphics/remote-control integration and
Python kittens are not ported. A headless `kitr` binary can spawn a shell in a
PTY, maintain a screen and dump text. Do not treat any subsystem as complete until
it is marked **ported** in [PORTING.md](PORTING.md).

| Crate | Ported from | Status |
|---|---|---|
| `kitty-core` | `line.h`, `lineops.h`, `line-buf.c`, `text-cache.c`, `history.c` | core ported and tested; rewrap/ANSI/pager paths pending |
| `kitty-vt` | `vt-parser.c`, `charsets.c` | parser state machine ported; Screen dispatch in `kitty-screen` |
| `kitty-simd` | `simd-string*.c` | scalar + SSE2/AVX2/AVX-512, runtime dispatched |
| `kitty-unicode` | `char-props.c`, `char-props-data.h`, `wcswidth.c` | ported, Unicode 17 tables generated and checked in |
| `kitty-screen` | `screen.c`, `cursor.c`, `line.c`, `lineops.h` | core screen/cursor/lineops subset ported |
| `kitty-input` | `keys.c`, `key_encoding.c`, `mouse.c` | keyboard, kitty protocol, mouse protocols ported |
| `kitty-os` | `child.c`, `child-monitor.c`, `loop-utils.c`, `monotonic.c` | Linux PTY/fork/exec, poll loop, SIGCHLD monitor ported |
| `kitty-config` | `shlex.c`, `state.c`, `config.py`, `options/*` | shlex, typed options, colors, includes and overrides ported; advanced options pending |
| `kitty-fonts` | `fonts.c`, `freetype.c`, `fontconfig.c`, `font-names.c`, `shaped-run-cache.c` | FreeType/HarfBuzz/Fontconfig FFI, metrics, fallback and shaping; rasterization/cache pending |
| `kitty-graphics` | `graphics.c`, `png-reader.c`, `mime-cache.c`, graphics parser | PNG decode, MIME, base64, graphics command parser and image pipeline; Screen/GPU integration pending |
| `kitty-remote` | `crypto.c`, protocol half of `data-types.c`, `fast-file-copy.c` | X25519/ECDH, AES-256-GCM, base64/base85, DCS framing, file-copy primitives |
| `kitty-render` | `gl.c`, `shaders.c`, `decorations.c`, `glyph-cache.c` | scaffold only |
| `kitty-kittens` | `kittens.c`, Python/Go kittens | scaffold only |
| `kitty` (binary) | `main.py`, launcher | headless PTY terminal (`kitr`); no window/renderer yet |

[PORTING.md](PORTING.md) contains the complete C/Python inventory, the crate
dependency graph, the phase plan, per-file mapping, performance plan and risk
list.

## Build and test

Requires Rust 1.89 or newer (AVX-512 intrinsics).

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release
```

Headless terminal:

```sh
cargo run --release -p kitty -- --size 120x40 --dump-screen -- /bin/sh -c 'echo hello'
```

Examples (no external dependencies):

```sh
cargo run --release -p kitty-core --example bench
cargo run --release -p kitty-vt --example bench
cargo run --release -p kitty-simd --example bench
cargo run --release -p kitty-unicode --example bench
cargo run --release -p kitty-screen --example bench
cargo run --release -p kitty-input --example bench
```

See [BENCHMARKS.md](BENCHMARKS.md) for measured C-vs-Rust numbers and the
methodology. The C sources are not included in this repository. Ported files carry an SPDX
identifier and a "ported from" notice naming the original kitty file and its
copyright holder.

## License

Copyright (C) 2026 kitr contributors.

`kitr` is free software: you can redistribute it and/or modify it under the
terms of the GNU General Public License as published by the Free Software
Foundation, either version 3 of the License, or (at your option) any later
version. It is distributed in the hope that it will be useful, but **WITHOUT
ANY WARRANTY**; without even the implied warranty of MERCHANTABILITY or FITNESS
FOR A PARTICULAR PURPOSE. See [LICENSE](LICENSE) for the full text.

`kitr` is a derivative work of kitty:

> kitty is Copyright (C) 2016-2026 Kovid Goyal and kitty contributors,
> licensed under GPL-3.0-or-later.
> Upstream: <https://github.com/kovidgoyal/kitty>

If you distribute `kitr` or a modified version, you must comply with the GPL:
provide the complete corresponding source under GPL-3.0-or-later, retain
copyright and license notices, and state that the code has been modified.
