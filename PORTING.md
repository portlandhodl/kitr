# Porting kitty to Rust: analysis and plan

Status: planning document. No C build was run; no source files were modified.
Repository examined: `/home/qrsnap/kitty` (kitty 0.4x-era tree, C/Python/Go).
Target workspace: `/home/qrsnap/kitty/rust`.

The goal is a literal 1:1 port: same observable terminal behavior, same
configuration syntax and key/mouse semantics, same remote-control protocol,
same kittens, and the same cell layout and dirty-line/rendering model, with the
Python C-extension layer removed. This is not a redesign of kitty.

---

## 1. Inventory

All counts below were produced from the checked-out tree with `find`, `ls`, and
`wc -l`. They exclude `.git`.

| Area | Files | LOC | Command used |
|---|---:|---:|---|
| `kitty/*.c` | 58 | 57,492 | `wc -l kitty/*.c` |
| `kitty/*.h` | 62 | 28,636 | `wc -l kitty/*.h` |
| `kitty/*.m` | 2 | 3,211 | `wc -l kitty/*.m` |
| **kitty C/H/ObjC total** | **122** | **89,339** | `wc -l kitty/*.c kitty/*.h kitty/*.m` |
| `kitty/launcher/*.c` | 3 | 1,199 | `wc -l kitty/launcher/*.c` |
| `kitty/*.py` (top level) | 52 | 29,427 | `wc -l kitty/*.py` |
| `kitty/**/*.py` (including `options/`, `layout/`, `rc/`, `fonts/`, `conf/`) | 125 | 53,925 | `find kitty -name '*.py' \| xargs wc -l` |
| `kittens/**/*.py` | 65 | 8,446 | `find kittens -name '*.py' -print0 \| xargs -0 wc -l` |
| `bypy/**/*.py` | 4 | 971 | `find bypy -name '*.py' -print0 \| xargs -0 wc -l` |
| `kitty_tests/*.py` | 51 | 28,424 | `wc -l kitty_tests/*.py` |
| `gen/*.py` | 11 | 4,410 | `find gen -name '*.py' -print0 \| xargs -0 wc -l` |
| **all Python** | **271** | **102,337** | `find . -name '*.py' -not -path './.git/*' -print0 \| xargs -0 wc -l` |
| **all Go** | **354** | **85,719** | `find . -name '*.go' -not -path './.git/*' -print0 \| xargs -0 wc -l` |
| `glfw/*.c`, `glfw/*.h`, `glfw/*.m` (vendored fork) | 68 | 42,351 | `wc -l glfw/*.c glfw/*.h glfw/*.m` |
| `3rdparty/**/*.c`, `3rdparty/**/*.h` | 54 | 8,527 | `find 3rdparty -name '*.c' -o -name '*.h' \| xargs wc -l` |
| Rust workspace source | 6 `.rs` files | 1,484 | `wc -l rust/crates/*/src/*.rs` |
| `setup.py` / `benchmark.py` | 2 | 2,877 | `wc -l setup.py benchmark.py` |

Go is split as: `kittens/` 120 files, `tools/` 231 files, root `embeds.go`, and
`bypy/` 2 files. The Go tree is a second, substantial implementation of kitty's
CLI/kitten tooling and is not part of the C extension. Python is split as:
`kitty/**` 125, `kittens/**` 65, `kitty_tests/**` 51, `gen/**` 11, `bypy/**` 4,
plus root scripts and `setup.py`.

Vendored third-party code:

| Component | Contents | LOC | Notes |
|---|---:|---:|---|
| `3rdparty/base64` | 44 `.c`, 7 `.h` | 5,935 | SIMD base64 encoder/decoder, BSD-style |
| `3rdparty/ringbuf` | 1 `.c`, 1 `.h` | 646 | byte ring buffer |
| `3rdparty/verstable.h` | 1 header | 1,946 | open-addressing hash table, MIT |
| `glfw/` | 34 `.c`, 28 `.h`, 6 `.m`, plus protocol XML/JSON | 42,351 | kitty's vendored GLFW fork (zlib/libpng license), with Wayland, X11, Cocoa, D-Bus, IBus, session-monitor and notification changes |
| `glad/generate.py` | generator only | 46 | GL loader sources are generated, not checked in |

### 1.1 Every `kitty/*.c`, grouped

Terminal core:
`screen.c`, `line.c`, `line-buf.c`, `history.c`, `vt-parser.c`, `charsets.c`,
`wcswidth.c`, `text-cache.c`, `state.c`, `cursor.c`, `colors.c`,
`char-props.c`, `rowcolumn-diacritics.c`, `hyperlink.c`, `resize.c`,
`simd-string.c`, `simd-string-128.c`, `simd-string-256.c`,
`simd-string-512.c`.

Rendering:
`gl.c`, `gl-wrapper.c`, `shaders.c`, `decorations.c`, `animation.c`,
`cursor_trail.c`, `window_logo.c`, `graphics.c`, `png-reader.c`,
`glyph-cache.c`, `mime-cache.c`.

Fonts and text shaping:
`fonts.c`, `freetype.c`, `fontconfig.c`, `font-names.c`,
`freetype_render_ui_text.c`, `shaped-run-cache.c`, `disk-cache.c`.

Input and UI:
`keys.c`, `mouse.c`, `key_encoding.c`, `dnd.c`.

Platform:
`child.c`, `child-monitor.c`, `glfw.c`, `glfw-wrapper.c`, `loop-utils.c`,
`monotonic.c`, `logging.c`, `cleanup.c`, `utmp.c`, `systemd.c`, `desktop.c`,
`macos_process_info.c`.

Remote control, kittens and serialization:
`crypto.c`, `fast-file-copy.c`, `kittens.c`, `shlex.c`, `data-types.c`.

That is 58 files. `kitty/launcher/{main,cmdline,single-instance}.c` are additional
launcher sources, and `kitty/{cocoa_window,core_text}.m` are ObjC sources.

### 1.2 Every `kitty/*.h`, grouped

Terminal core:
`screen.h`, `line.h`, `lineops.h`, `line-buf.h`, `history.h`, `vt-parser.h`, `charsets.h`,
`wcswidth.h`, `char-props.h`, `char-props-data.h`, `unicode-data.h`,
`text-cache.h`, `state.h`, `colors.h`, `hyperlink.h`, `resize.h`,
`control-codes.h`, `modes.h`, `data-types.h`, `arena.h`,
`fixed_size_deque.h`, `kitty-verstable.h`, `iqsort.h`, `simd-string.h`,
`simd-string-impl.h`.

Rendering and graphics:
`gl.h`, `gl-wrapper.h`, `graphics.h`, `decorations.h`, `animation.h`,
`animation-parse.h`, `png-reader.h`, `window_logo.h`, `glyph-cache.h`,
`parse-graphics-command.h`, `print-graphics.h`, `srgb_gamma.h`,
`color-names.h`, `parse-multicell-command.h`.

Fonts and shaping:
`fonts.h`, `freetype_render_ui_text.h`, `shaped-run-cache.h`,
`disk-cache.h`.

Input and UI:
`keys.h`, `dnd.h`, `parse-dnd-command.h`.

Platform:
`cocoa_window.h`, `glfw-wrapper.h`, `loop-utils.h`, `monotonic.h`,
`threading.h`, `cleanup.h`, `cross-platform-random.h`, `backtrace.h`,
`safe-wrappers.h`, `terminfo.h`, `arches.h`, `banned.h`.

Remote control and serialization:
`crypto.h`, `fast-file-copy.h`, `base64.h`, `binary.h`.

That is 62 headers.

### 1.3 Generated files and build inputs

- `kitty/options/to-c-generated.h` (1,825 lines) and
  `kitty/options/to-c.h` (760 lines) are generated from
  `kitty/options/definition.py` by `gen/config.py`.
- `kitty/shaders/*.slang` (39 files) are compiled by `gen/`/`setup.py` into GL
  shader code; the shaders are part of the renderer contract.
- `kitty/char-props-data.h` and `kitty/rowcolumn-diacritics.c` depend on
  generated Unicode tables (`gen/wcwidth.py`, `gen/rowcolumn-diacritics.txt`).
- `kitty/vt-parser-dump.c` is not checked in; `setup.py` compiles
  `kitty/vt-parser.c` a second time with `DUMP_COMMANDS` for the parser-dump
  test binary (`setup.py:907`).
- `kitty/fast_data_types` is the Python C extension built by
  `setup.py:1434` from all `kitty/*.c` plus `3rdparty/ringbuf` and
  `3rdparty/base64`.

---

## 2. Subsystem map

### 2.1 C subsystem map

| Subsystem | C files | Main headers | Notes |
|---|---|---|---|
| Terminal core | `screen.c`, `line.c`, `line-buf.c`, `history.c`, `vt-parser.c`, `charsets.c`, `wcswidth.c`, `text-cache.c`, `state.c`, `cursor.c`, `colors.c`, `char-props.c`, `rowcolumn-diacritics.c`, `hyperlink.c`, `resize.c`, `simd-string*.c` | `screen.h`, `line.h`, `lineops.h`, `line-buf.h`, `history.h`, `vt-parser.h`, `charsets.h`, `wcswidth.h`, `text-cache.h`, `state.h`, `colors.h`, `control-codes.h`, `modes.h`, `unicode-data.h`, `char-props*.h`, `simd-string*.h`, `arena.h`, `fixed_size_deque.h`, `kitty-verstable.h`, `iqsort.h` | `lineops.h` is header-only inline code; there is no `lineops.c`. `state.c` is cross-cutting global/OS state and option storage. `screen.c` is the largest core file (7,367 lines). |
| Rendering | `gl.c`, `gl-wrapper.c`, `shaders.c`, `decorations.c`, `animation.c`, `cursor_trail.c`, `window_logo.c`, `graphics.c`, `png-reader.c`, `glyph-cache.c`, `mime-cache.c` | `gl.h`, `gl-wrapper.h`, `shaders` sources, `decorations.h`, `animation.h`, `animation-parse.h`, `window_logo.h`, `glyph-cache.h`, `graphics.h`, `png-reader.h`, `parse-graphics-command.h`, `print-graphics.h`, `srgb_gamma.h` | There is no `background.c`/`background_image.c`. Background images live in `state.c` plus `shaders.c`, driven from Python `boss.py`/`rc/set_background_image.py`. Borders are Python `borders.py` plus `shaders.c`/`decorations.c`. |
| Fonts and shaping | `fonts.c`, `freetype.c`, `fontconfig.c`, `font-names.c`, `freetype_render_ui_text.c`, `shaped-run-cache.c`, `disk-cache.c`, plus macOS `core_text.m` | `fonts.h`, `freetype_render_ui_text.h`, `shaped-run-cache.h`, `disk-cache.h`, `char-props.h` | Uses FreeType, HarfBuzz and Fontconfig on Linux; CoreText on macOS. `disk-cache.c` is the on-disk font/face cache. |
| Input/UI | `keys.c`, `mouse.c`, `key_encoding.c`, `dnd.c` | `keys.h`, `dnd.h`, `parse-dnd-command.h`, `modes.h`, `control-codes.h` | `dnd.c` implements terminal drag-and-drop encoding/decoding; it depends on `glfw-wrapper.h` and `screen.h`. |
| Platform | `child.c`, `child-monitor.c`, `glfw.c`, `glfw-wrapper.c`, `loop-utils.c`, `monotonic.c`, `logging.c`, `cleanup.c`, `utmp.c`, `systemd.c`, `desktop.c`, `macos_process_info.c`, plus `cocoa_window.m` | `glfw-wrapper.h`, `loop-utils.h`, `monotonic.h`, `threading.h`, `cleanup.h`, `cross-platform-random.h`, `backtrace.h`, `safe-wrappers.h`, `terminfo.h`, `cocoa_window.h` | `child.c`/`child-monitor.c` do PTY creation, fork/exec, the I/O thread and child lifecycle. `glfw.c` wraps the vendored fork. Linux-specific paths include signalfd/eventfd (`loop-utils.c`), D-Bus/IBus/notifications in `glfw/`, and Wayland/X11 backends in `glfw/wl_*.c`, `glfw/x11_*.c`. |
| Remote control | `crypto.c`, `fast-file-copy.c`, `kittens.c`, `shlex.c`, `data-types.c` (serialization half) | `crypto.h`, `fast-file-copy.h`, `base64.h`, `binary.h`, `control-codes.h` | Control codes are in `control-codes.h`; the Python side is `boss.py`, `remote_control.py`, `client.py` and `kitty/rc/*.py`. |
| Serialization/protocol | `data-types.c`, `crypto.c`, `shlex.c`, `simd-string*.c` (base64/xor primitives) | `data-types.h`, `binary.h`, `base64.h`, `crypto.h`, `cross-platform-random.h` | `data-types.c` is also the Python extension entry point; that module disappears in Rust. |

### 2.2 Python application layer

Top-level `kitty/*.py` (52 files). Target crates are indicative; several files
split across crates in practice.

| Python file | Role | Target crate |
|---|---|---|
| `actions.py` | action dispatch | `kitty-input` / binary |
| `bash.py` | shell integration helpers | `kitty-os` |
| `borders.py` | border geometry/layout | `kitty-render` |
| `boss.py` | top-level window/tab/OS orchestration, remote command handlers | binary + `kitty-remote` |
| `child.py` | child process/PTY Python wrapper | `kitty-os` |
| `choose_entry.py` | chooser UI | `kitty-kittens` |
| `client.py` | remote-control client | `kitty-remote` |
| `clipboard.py` | clipboard integration | `kitty-os` |
| `cli.py` | CLI parser and option definitions | `kitty-config` |
| `cli_stub.py` | generated CLI type stubs | `kitty-config` |
| `colors.py` | color table/background-image options | `kitty-screen` / render |
| `config.py` | config file loading | `kitty-config` |
| `constants.py` | shared constants | core/config |
| `debug_config.py` | debug dump | `kitty-config` |
| `entry_points.py` | launcher dispatch | binary |
| `file_transmission.py` | file transfer protocol | `kitty-remote` |
| `guess_mime_type.py` | MIME detection | `kitty-graphics` |
| `__init__.py` | package marker | n/a |
| `key_encoding.py` | key encoding protocol | `kitty-input` |
| `key_names.py` | key names | `kitty-input` |
| `keys.py` | key maps and mode stack | `kitty-input` |
| `launch.py` | process launch | `kitty-os` |
| `logout_session.py` | session logout | `kitty-os` |
| `machine_id.py` | machine identity | `kitty-os` |
| `main.py` | application entry point | binary |
| `marks.py` | marks | `kitty-screen` / UI |
| `multiprocessing.py` | process helpers | `kitty-os` |
| `notifications.py` | desktop notifications | `kitty-os` |
| `open_actions.py` | open URL/file | `kitty-os` |
| `os_window_size.py` | OS window sizing | `kitty-os` |
| `program_status.py` | child status | render/UI |
| `progress.py` | progress reporting | render/UI |
| `remote_control.py` | remote protocol framing/crypto | `kitty-remote` (server/client subset ported) |
| `render_cache.py` | render cache | `kitty-render` |
| `rgb.py` | color parsing | protocol/core |
| `search_query_parser.py` | scrollback search syntax | `kitty-input` |
| `session.py` | session save/restore | binary |
| `shell_integration.py` | shell integration | `kitty-os` |
| `shm.py` | shared memory images | `kitty-os` |
| `short_uuid.py` | ID generation | protocol |
| `simple_cli_definitions.py` | CLI specs | `kitty-config` |
| `tab_bar.py` | tab bar layout/drawing | render/UI |
| `tabs.py` | tab model | binary/UI |
| `terminfo.py` | terminfo generation | protocol |
| `types.py` | Python type definitions | protocol |
| `typing_compat.py` | typing shims | protocol |
| `update_check.py` | update check | `kitty-os` |
| `utils.py` | utility/marshalling helpers | protocol/core |
| `window_list.py` | window list model | UI |
| `window.py` | window model, escape-code output | binary/UI |
| `window_title_bar.py` | title bar rendering | render/UI |
| `xdg.py` | XDG paths | `kitty-os` |

Python subpackages: `kitty/options/` (5 files, 12,865 LOC including generated
`to-c-generated.h`; `definition.py`, `parse.py`, `types.py`, `utils.py`),
`kitty/layout/` (8 files; splits/tall/grid/stack/vertical geometry),
`kitty/rc/` (44 files; one module per remote-control action),
`kitty/fonts/` (7 files; fontconfig/CoreText/render/list), and `kitty/conf/`
(4 files; config type system).

### 2.3 Python binding layer that disappears in Rust

`kitty/fast_data_types` is built by `setup.py:1434`. Its module init is
`PyInit_fast_data_types` in `kitty/data-types.c:942`; it calls `init_*`
functions spread over the C files and adds module constants and methods.
37 `kitty/*.py` files import it. The C types exported to Python are:

| C type | Defining file |
|---|---|
| `Line` | `line.c` |
| `LineBuf` | `line-buf.c` |
| `HistoryBuf` | `history.c` |
| `Cursor` | `cursor.c` |
| `Screen` | `screen.c` |
| `Parser` | `vt-parser.c` |
| `ChildMonitor` | `child-monitor.c` |
| `Region`, `SigInfo` | `state.c`, `loop-utils.c` |
| `Shlex` | `shlex.c` |
| `DiskCache` | `disk-cache.c` |
| `Color`, `ColorProfile` | `colors.c` |
| `KeyEvent`, `SingleKey` | `keys.c` |
| `Face`, `ParsedFontFeature` | `freetype.c`, `fonts.c` |
| `CTFace` | `core_text.m` |
| `GraphicsManager` | `graphics.c` |
| `Secret`, `EllipticCurveKey`, `AES256GCMEncrypt`, `AES256GCMDecrypt` | `crypto.c` |
| `StreamingBase64Encoder`, `StreamingBase64Decoder` | `data-types.c` |

`PyGetSetDef` arrays are present in `colors.c`, `screen.c`, `graphics.c`,
`crypto.c`, `cursor.c`, `keys.c`, `line.c`, `line-buf.c`, `history.c`,
`state.c`, `child-monitor.c`, `disk-cache.c`, `fonts.c`, `freetype.c`,
`loop-utils.c`, `mouse.c`, `shlex.c`, and `vt-parser.c`. In the Rust port these
become ordinary Rust methods/fields on the owning types, or are deleted where
they only existed to marshal Python objects.

`data-types.c` also exports the module-level functions used by Python:
`monotonic`, `safe_pipe`, `read_command_response`, `send_data_to_peer`,
`set_options`, `get_options`, `get_boss`, `set_borders_rects`,
`set_tab_bar_render_data`, `find_in_memoryview`, `terminfo_data`, and the
cursor/mode/attribute constants. All of this is replaced by direct Rust calls or
by the remote-control/CLI boundary.

### 2.4 Kittens and Go tools

`kittens/` is 65 Python files plus 120 Go files. Python kittens include
`choose_files`, `choose_fonts`, `clipboard`, `diff`, `dnd`, `hints`, `icat`,
`notify`, `panel`, `remote_file`, `resize_window`, `show_key`, `ssh`,
`themes`, `transfer`, `tui`, `unicode_input`. `kittens/transfer/algorithm.c`
(1,149 lines) is the one C file outside `kitty/`. `tools/` contains 231 Go
files implementing the newer CLI/TUI/graphics/unicode utilities; it is a
parallel implementation and should be treated as a protocol/behavior reference,
not as a porting source for the C core.

---

## 3. Proposed Rust crate graph

The graph is deliberately narrow: core types do not depend on rendering or OS
code, and the binary is the only crate that knows about all subsystems.

```
kitty (binary)
├── kitty-screen ── kitty-core
│                └─ kitty-vt ── kitty-core
├── kitty-fonts ─── kitty-core
├── kitty-render ── kitty-core, kitty-screen, kitty-fonts
├── kitty-graphics ─ kitty-core, kitty-screen
├── kitty-input ─── kitty-core, kitty-screen, kitty-config
├── kitty-config ── kitty-core
├── kitty-os ────── kitty-core
├── kitty-remote ── kitty-core, kitty-screen, kitty-config
└── kitty-kittens ─ kitty-core, kitty-remote, kitty-config
```

| Crate | Responsibility | Replaces | Depends on | Justified external crates |
|---|---|---|---|---|
| `kitty-core` | Cell/line layout, line buffer, text cache, history buffer, line helpers, base64/binary primitives | `line.h`, `line.c` (cell helpers), `lineops.h`, `line-buf.c/.h`, `text-cache.c/.h`, `history.c/.h`, `arena.h`, `fixed_size_deque.h`, `kitty-verstable.h`, base64/binary parts of `data-types.c` | none | None required. |
| `kitty-simd` | Scalar and runtime-dispatched SSE2/AVX2/AVX-512 string and image primitives, incremental UTF-8 decoder | `simd-string.c/.h`, `simd-string-impl.h`, `simd-string-128/256/512.c` | none | None. Uses `core::arch` with `is_x86_feature_detected!`; hand-written scalar fallback. |
| `kitty-unicode` | Character properties, grapheme segmentation, ANSI-aware wcswidth, generated Unicode tables | `char-props.c/.h`, `char-props-data.h`, `wcswidth.c/.h` (plus the generator that produces the tables) | `kitty-core` | None. Do **not** substitute `unicode-width`; kitty's tables and grapheme rules are part of parity. |
| `kitty-vt` | VT parser state machine, CSI parameter model, OSC/DCS framing, charset designation dispatch, diacritics, terminal constants/control codes | `vt-parser.c/.h`, `charsets.c/.h`, `unicode-data.h`, `rowcolumn-diacritics.c`, `control-codes.h`, `modes.h` | `kitty-core`, `kitty-simd`, `kitty-unicode` | None. |
| `kitty-screen` | `Screen`, cursor, colors, selection, hyperlinks, resize/rewrap, scrollback integration, dirty-line state | `screen.c/.h`, `cursor.c`, `colors.c/.h`, `hyperlink.c/.h`, `resize.c/.h`, `parse-multicell-command.h` | `kitty-core`, `kitty-vt` | None. |
| `kitty-fonts` | Font discovery, fallback, FreeType/CoreText faces, HarfBuzz shaping, shaped-run cache, glyph metrics, on-disk font cache | `fonts.c/.h`, `freetype.c`, `fontconfig.c`, `core_text.m`, `font-names.c`, `freetype_render_ui_text.c/.h`, `shaped-run-cache.c/.h`, `disk-cache.c/.h`, `kitty/fonts/*.py` | `kitty-core` | `freetype-sys`, `harfbuzz-sys`, `fontconfig-sys` (or hand-written FFI to system libraries) for Linux; `objc2-core-text`/`objc2-foundation` on macOS. This keeps the same shaping/rasterization engines instead of gambling on a pure-Rust substitute. |
| `kitty-render` | OpenGL context/loader, shader programs, decorations, borders, background images, animations, cursor trail, window logo, glyph atlas | `gl.c/.h`, `gl-wrapper.c/.h`, `shaders.c`, `decorations.c/.h`, `animation.c/.h`, `cursor_trail.c`, `window_logo.c/.h`, `glyph-cache.c/.h`, `srgb_gamma.h`, `kitty/shaders/*.slang` | `kitty-core`, `kitty-screen`, `kitty-fonts` | `glow` for GL bindings/loader. Avoid `wgpu` in a 1:1 port: it would require rewriting the shaders and the cell/quad pipeline. Border/background Python geometry moves here. |
| `kitty-graphics` | Kitty graphics protocol, PNG decode, MIME/file-read policy, image placement/placement references | `graphics.c/.h`, `png-reader.c/.h`, `mime-cache.c`, `parse-graphics-command.h`, `print-graphics.h`, `guess_mime_type.py` | `kitty-core`, `kitty-screen` | `png` or `libpng-sys`; `memmap2` if images are mmap-backed. `png` is justified only if pixel-exact behavior is validated against libpng. |
| `kitty-input` | Key events/encoding, key maps, mouse events/modes, drag-and-drop decoding | `keys.c/.h`, `mouse.c`, `key_encoding.c`, `dnd.c/.h`, `parse-dnd-command.h`, `keys.py`, `key_encoding.py`, `key_names.py`, `actions.py`, `search_query_parser.py` | `kitty-core`, `kitty-screen`, `kitty-config` | No general-purpose crate. Linux key handling may need direct `xkbcommon` FFI (`libc` + `xkbcommon`). |
| `kitty-config` | Option definitions/defaults, config-file parser, CLI parser, runtime global state, shlex | `state.c/.h`, `shlex.c`, `kitty/options/*`, `config.py`, `cli.py`, `cli_stub.py`, `simple_cli_definitions.py`, `debug_config.py` | `kitty-core` | `serde_json` only if JSON is needed on this boundary; keep the custom config/CLI grammar rather than `serde`/`toml`/`clap`, to preserve error text and option semantics. |
| `kitty-os` | PTY/fork/exec, child monitor/I/O thread, GLFW window/event loop, signals, monotonic clock, logging, cleanup, terminfo, utmp, systemd, desktop integration, macOS process info, Cocoa window | `child.c`, `child-monitor.c`, `glfw.c`, `glfw-wrapper.c/.h`, `loop-utils.c/.h`, `monotonic.c/.h`, `logging.c`, `cleanup.c/.h`, `utmp.c`, `systemd.c`, `desktop.c`, `macos_process_info.c`, `cocoa_window.m/.h`, `glfw/` | `kitty-core` | `libc` and/or `nix` for PTY/termios/fork/exec/signalfd/eventfd/Unix sockets; `memmap2` for mmap; `glfw-sys`/hand-written FFI if the vendored C GLFW is retained. On macOS, `objc2`/`objc2-app-kit` for Cocoa. `tokio` is not justified: kitty's event loop and I/O-thread model are part of its latency behavior. |
| `kitty-remote` | Remote-control framing, encryption, JSON payloads, async responses, file copy, boss command dispatch | `crypto.c/.h`, `fast-file-copy.c/.h`, protocol half of `data-types.c`, `boss.py`, `remote_control.py`, `client.py`, `rc/*.py`, `file_transmission.py` | `kitty-core`, `kitty-screen`, `kitty-config` | `serde_json` for JSON, `openssl` (or direct `libcrypto` FFI) for the existing ECDH/AES-GCM protocol. Avoid a different base64 crate until wire compatibility is proven. |
| `kitty-kittens` | Kitten discovery/launch, built-in kitten implementations and their protocol helpers | `kittens.c`, `kittens/**` (Python/Go), `kittens/transfer/algorithm.c`, `choose_entry.py` | `kitty-core`, `kitty-remote`, `kitty-config` | `serde_json` where JSON is part of a kitten protocol. No `clap`/`tokio` unless a specific kitten requires it. |
| `kitty` (binary) | `main`, app lifecycle, tabs/windows/layout, session save/restore, packaging entry points | `main.py`, `entry_points.py`, `launch.py`, `tabs.py`, `window.py`, `window_list.py`, `session.py`, `layout/*.py`, `boss.py` (orchestration half) | all of the above | `libc` for low-level startup; otherwise std. Avoid `clap` to preserve generated CLI behavior. |

Dependency rule: if a type is needed by both `kitty-screen` and
`kitty-render`, it belongs in `kitty-core`; if it needs OS handles, it belongs
in `kitty-os`. Do not create a `kitty-utils` crate; `utils.py` is mostly
serialization and belongs in `kitty-remote`/`kitty-core`.

---

## 4. Porting phases and exit criteria

Each phase ends with (a) the listed C behavior demonstrable from Rust, (b) the
listed existing tests passing or a written reason they cannot yet run, and
(c) a differential check against the C implementation where feasible.

### Phase 0 — workspace and foundation

Files: `line.h`, `line.c`, `lineops.h`, `line-buf.c/.h`, `text-cache.c/.h`,
`arena.h`, `fixed_size_deque.h`, `kitty-verstable.h`, `data-types.h` (the
cell/utility parts). Existing Rust files: `cell.rs`, `line.rs`, `linebuf.rs`,
`text_cache.rs`.

Tests: `kitty_tests/datatypes.py` `test_linebuf`, `test_line`,
`test_rewrap_simple`, `test_rewrap_wider`, `test_rewrap_narrower`,
`test_historybuf`, `test_historybuf_continued_after_wraparound`;
`kitty_tests/screen.py::test_text_cache_garbage_collection`.

Exit criteria:
- `cargo test` passes all core unit tests.
- Compile-time assertions for `size_of`/`align_of`/field offsets of `CpuCell`,
  `GpuCell`, `CellAttrs`, `LineAttrs` match the C build (little-endian).
- A C/Rust differential harness produces byte-identical cell rows for a corpus
  of lines, including multi-codepoint cells, wide chars, hyperlinks, and
  wrapped flags.
- No per-cell heap allocation in line construction/clearing.
- Text-cache GC remaps all live indices correctly.

### Phase 1 — VT parser, screen, line operations, history, charsets, Unicode

Files: `vt-parser.c/.h`, `screen.c/.h`, `history.c/.h`, `charsets.c/.h`,
`wcswidth.c/.h`, `char-props.c/.h`, `char-props-data.h`, `unicode-data.h`,
`rowcolumn-diacritics.c`, `cursor.c`, `colors.c/.h`, `hyperlink.c/.h`,
`resize.c/.h`, `simd-string*.c/.h`, `control-codes.h`, `modes.h`,
`parse-multicell-command.h`.

Tests: all of `kitty_tests/parser.py` (notably `test_charsets`,
`test_utf8_simd_decode`, `test_esc_codes`, `test_csi_codes`, `test_osc_codes`,
`test_dcs_codes`, `test_graphics_command`), most of `kitty_tests/screen.py`
(drawing, SGR, cursor movement, margins, resize/rewrap, selection, serialization,
hyperlinks, text-cache GC), and the screen-related portions of
`kitty_tests/datatypes.py`.

Exit criteria:
- Parser differential fuzzing against the C `vt-parser-dump` binary over random
  bytes, malformed CSI/OSC/DCS, and all valid sequences.
- `screen.py` semantics reproduced, including wide characters at line
  boundaries, `REP`, origin mode, DECAWM, alt screen, scrollback wrap flags,
  and selection serialization.
- Unicode width/grapheme results match `GraphemeBreakTest.json` and generated
  kitty tables for the full code-point range.
- Throughput for the `ascii`, `unicode`, `unique_unicode`, `csi`, and
  `long_escape_codes` scenarios is no worse than the C baseline on the same
  machine (with a tolerance agreed before the phase).

### Phase 2 — input, configuration, keys, mouse, options

Files: `keys.c/.h`, `mouse.c`, `key_encoding.c`, `state.c/.h`,
`kitty/options/*` (definitions/parser/types), `config.py`, `cli.py`,
`cli_stub.py`, `simple_cli_definitions.py`, `actions.py`, `keys.py`,
`key_encoding.py`, `key_names.py`, `search_query_parser.py`, `dnd.c/.h`,
`parse-dnd-command.h`, `shlex.c`.

Tests: `kitty_tests/keys.py` (key encoding, mouse encoding, mappings,
fallbacks), `kitty_tests/mouse.py`, `kitty_tests/options.py` (config parsing,
launcher, CLI parsing, font-size clamping, session option expansion),
`kitty_tests/completion.py`, `kitty_tests/search_query_parser.py`, and the
DnD parsing portions of `kitty_tests/dnd.py`.

Exit criteria:
- Config files round-trip to the same effective option set, including
  deprecations and environment overrides.
- Key/mouse encodings match the existing table (`key_encoding.json`) for all
  modifiers, keyboard modes, and kitty protocol variants.
- CLI help/error/exit behavior matches the C/Python implementation for the
  supported commands.
- No `fast_data_types` import remains on these paths.

### Phase 3 — fonts and text shaping

Files: `fonts.c/.h`, `freetype.c`, `fontconfig.c`, `core_text.m`,
`font-names.c`, `freetype_render_ui_text.c/.h`, `shaped-run-cache.c/.h`,
`disk-cache.c/.h`, `kitty/fonts/*.py`, relevant parts of `char-props.c`.

Tests: all of `kitty_tests/fonts.py` (font selection, fallback matching, color
emoji, shaping, shaped-run cache, combining chars, decorations, box drawing),
`kitty_tests/multicell.py`, and the font-cache parts of
`kitty_tests/graphics.py` (`test_disk_cache*`).

Exit criteria:
- Same selected face/fallback chain for the test font corpus
  (`FiraCode-Medium.otf`, `CascadiaCode-Regular.otf`, `iosevka-regular.ttf`,
  `LiberationMono-Regular.ttf`, `ComfyCode-Regular.ttf`,
  `twemoji_smiley-cff2_colr_1.otf`).
- HarfBuzz glyph IDs, clusters, advances and offsets match the C implementation
  for the shaping tests, including ligatures and combining sequences.
- Font disk-cache format and invalidation behavior are compatible (or a
  migration path is defined).
- No regressions in cell width/placement for color emoji and multicell text.

### Phase 4 — GPU renderer and OpenGL

Files: `gl.c/.h`, `gl-wrapper.c/.h`, `shaders.c`, `decorations.c/.h`,
`animation.c/.h`, `cursor_trail.c`, `window_logo.c/.h`, `glyph-cache.c/.h`,
`srgb_gamma.h`, `kitty/shaders/*.slang`, `graphics.c/.h`, `png-reader.c/.h`,
`mime-cache.c`, `parse-graphics-command.h`, `print-graphics.h` (graphics may be
split into `kitty-graphics` but is validated here).

Tests: `kitty_tests/graphics.py` (PNG loading, image placement, z-layers,
placeholders, animation, delete/scroll), `kitty_tests/slang.py`,
`kitty_tests/datatypes.py::test_png_from_32bit_rgba_data`, and
`kitty_tests/screen.py` rendering-adjacent tests.

Exit criteria:
- Shader compilation succeeds with the same uniform/attribute contract and the
  same cell/quad batching.
- Golden-frame tests: rendered frames match the C renderer within an explicit
  per-pixel tolerance for text, decorations, borders, cursor, background image,
  and graphics.
- Glyph atlas upload/dirty-line batching does not regress frame time on the
  scroll benchmark.
- The graphics protocol passes the existing test suite, including image IDs,
  placement IDs, parent/child images, and delete semantics.

### Phase 5 — windowing, platform and child loop

Files: `child.c`, `child-monitor.c`, `glfw.c`, `glfw-wrapper.c/.h`,
`loop-utils.c/.h`, `monotonic.c/.h`, `logging.c`, `cleanup.c/.h`, `utmp.c`,
`systemd.c`, `desktop.c`, `macos_process_info.c`, `cocoa_window.m/.h`,
`glfw/` (or its retained C library), `kitty/launcher/*`.

Tests: `kitty_tests/child.py`, `kitty_tests/glfw.py`, `kitty_tests/utmp.py`,
`kitty_tests/single_instance.py`, `kitty_tests/sessions.py`,
`kitty_tests/shell_integration.py`, `kitty_tests/notifications.py`,
`kitty_tests/notify.py`, `kitty_tests/shm.py`, and the resize/geometry tests in
`kitty_tests/layout.py`.

Exit criteria:
- A real shell runs in the Rust binary: PTY allocation, fork/exec, signals,
  window resize (SIGWINCH), child exit reporting, and scrollback work.
- X11, Wayland, and macOS/Cocoa at least build and run the supported
  integration tests on their native platform; D-Bus/IBus/notification/session
  monitor behavior is either retained through the vendored GLFW or explicitly
  reimplemented and tested.
- Single-instance launch, session save/restore, and shell integration pass.
- No busy loops; input latency and idle CPU are measured against the C build.

### Phase 6 — remote control and kittens

Files: `crypto.c/.h`, `fast-file-copy.c/.h`, protocol half of `data-types.c`,
`kittens.c`, `shlex.c`, `boss.py`, `remote_control.py`, `client.py`,
`kitty/rc/*.py`, `kitty/kittens/*`, `kittens/**`, `file_transmission.py`,
`clipboard.py`, `choose_entry.py`.

Tests: `kitty_tests/remote_control.py`, `kitty_tests/crypto.py`,
`kitty_tests/file_transmission.py`, `kitty_tests/clipboard.py`,
`kitty_tests/dnd_kitten.py`, `kitty_tests/ssh.py`, `kitty_tests/hints.py`,
`kitty_tests/choose_files.py`, `kitty_tests/command_palette.py`,
`kitty_tests/open_actions.py`, `kitty_tests/notify.py`.

Exit criteria:
- The Rust binary interoperates with the existing Python `kitty @` client and
  with an unmodified C kitty acting as client/server.
- ECDH/AES-GCM framing, JSON command names, async responses, and error shapes
  are byte-compatible.
- Each built-in kitten used by the test suite runs end-to-end; external kittens
  still launch and can talk to the Rust binary over the control socket.
- No remote command is silently dropped relative to the C implementation.

### Phase 7 — parity testing, performance and packaging

Files: `kitty_tests/**`, `benchmark.py`, `tools/cmd/benchmark`, packaging and
launcher scripts, license/notice files.

Tests: the complete `kitty_tests` suite, `benchmark.py` scenarios, build and
packaging smoke tests, and cross-platform CI. The tests currently import
`kitty.fast_data_types`; Phase 7 must either provide a compatibility shim or
maintain a split suite where Rust-native tests replace Python-API tests with
identical assertions.

Exit criteria:
- Full supported-platform matrix green, with a documented list of excluded
  tests and why.
- Throughput on `ascii`, `unicode`, `unique_unicode`, `csi`, `images`,
  `long_escape_codes` and tab-bar update time are measured against the C
  baseline and meet the agreed target (at minimum no regression; stretch goal
  is improvement where the port removes the Python/GIL overhead).
- Binary packaging, terminfo, shell integration, desktop files and man pages
  work without a Python runtime for the core terminal.
- License/notice audit is complete.

---

## 5. Performance plan

### 5.1 Data layout and allocation

- Keep `#[repr(C)]`/`#[repr(transparent)]` cell types and the existing
  compile-time `size_of`/`align_of` assertions. `CpuCell` is 12 bytes,
  `GpuCell` 20 bytes, `CellAttrs` 4 bytes, `LineAttrs` 1 byte, little-endian.
  Do not replace cells with Rust enums or `Option`-heavy types in hot paths.
- Keep the line buffer as one flat `Vec<CpuCell>` plus one flat `Vec<GpuCell>`,
  with a `line_map: Vec<u32>` indirection for scrolling. Never use
  `Vec<Vec<Cell>>` for the screen.
- Preserve the C ring-buffer semantics for history. A ring of rows with stable
  slots and an index map avoids `memmove` on scroll; expose contiguous slices
  only where the logical row is not wrapped.
- Replace the current `TextCache` per-entry `Box<[CharType]>` plus `HashMap`
  with a bump/arena allocator equivalent to `CharsMonotonicArena`, and a
  hash map keyed by arena slice. The GC must have the same
  "remap every live index" contract as `tc_gc_*`.
- Avoid per-cell allocation and per-cell `String`. Multi-codepoint cell text is
  capped at `MAX_NUM_CODEPOINTS_PER_CELL` (24); use an inline array or arena
  slice, as `ListOfChars` already does in `line.rs`.
- Remove `Rc<RefCell<TextCache>>` from the hot path. The C code is
  single-threaded per screen; use an owned cache with explicit `&mut` or
  index-based access. `RefCell` checks on every cell read/write are measurable.
- Use `u32` indices, wrapping arithmetic where C uses unsigned wraparound, and
  checked/saturating helpers only where the C code relies on clamping.

### 5.2 Parser and terminal core

- Feed the parser a borrowed `&[u8]` from a reusable PTY read buffer. Do not
  convert the whole stream to `String`/`&str`; decode UTF-8 incrementally into
  stack arrays or a reusable `Vec<u32>`.
- Keep the parser state machine allocation-free. Parameters and intermediates
  live in fixed arrays, matching `vt-parser.c`. OSC/DCS payloads should use a
  growable buffer only for the duration of the sequence.
- Avoid a virtual call per byte. The parser's `Handler` may be a trait, but the
  hot path should be monomorphized (`impl Handler`) or use a static dispatch
  table; dyn dispatch per printed character is not acceptable.
- Preserve the C dirty-line model: a line is marked dirty on write, GPU data is
  rebuilt only for dirty lines, and rendering consumes a compact dirty set.
- Batch printable ASCII runs (the C code already does this via
  `printable_ascii_run_length`), and process SGR runs in bulk rather than
  re-parsing attribute changes per cell.

### 5.3 SIMD

- Port the four performance-critical primitives from `simd-string.c`:
  `utf8_decode_to_esc`, `find_either_of_two_bytes`, `printable_ascii_run_length`,
  `xor_data64`, plus `blend_over_straight`, `blend_over_opaque` and
  `composite_alpha_mask`.
- Implement scalar fallbacks first, then SSE/AVX2/AVX-512 and NEON variants
  selected at runtime with `is_x86_feature_detected!`/`std::arch` target
  features. `std::simd` is still nightly-only; do not make the port require a
  nightly compiler solely for it.
- Keep the scalar and SIMD implementations in the same test matrix; compare
  their outputs over boundary lengths (0, 1, 15, 16, 31, 32, 63, 64, 127,
  128, 255, 256, 4096) and mismatched alignment.
- SIMD is not a substitute for algorithmic fixes: the first goal is the same
  fast path selection and batching as C, then wider vectors.

### 5.4 GPU and rendering

- Preserve the cell program / decoration program / background-program split and
  the existing batching. Build vertex data per dirty line into reusable,
  persistently mapped buffers; one draw per program per frame, not per cell.
- Keep the glyph atlas and shaped-run cache; upload only new glyphs. Avoid
  re-shaping text on every frame.
- Batch graphics/image updates separately from text; do not force a full-screen
  repaint for a small image change.
- Measure both throughput and frame latency. A faster parser that produces more
  draw calls is a regression.

### 5.5 Benchmark methodology

Baseline: C kitty built from the same commit, same machine, same font files,
same window size, same `kitty.conf` (or defaults), same GPU/driver. Record CPU,
GPU, kernel, fontconfig/FreeType/HarfBuzz versions, and terminal geometry in
the results file.

Throughput scenarios (from `benchmark.py` / `tools/cmd/benchmark`):
`ascii`, `unicode`, `unique_unicode`, `csi`, `images`, `long_escape_codes`,
plus tab-bar layout/update time for `fade`, `separator`, `powerline`, `slant`
at 10/50/200 tabs. These report MB/s and median time; also capture p50/p95
frame time and process CPU.

Microbenchmarks: `criterion` for cells/linebuf/text-cache, parser dispatch,
SIMD primitives, UTF-8 decode, wcswidth, SGR application, resize/rewrap, and
shaping-cache hit paths. Use `cargo bench --offline` with a populated
`~/.cargo`/vendor directory; the environment may not have network access, so
do not design a benchmark that downloads crates or fonts at runtime.

Offline constraints:
- `benchmark.py` currently imports `kitty.fast_data_types` and starts a
  `kitten __benchmark__` subprocess. It cannot measure a pure Rust binary until
  Phase 6/7 provides a compatibility path. Until then, use it only for the C
  baseline and run equivalent Rust-native harnesses (criterion or a small Rust
  binary) against the same byte streams.
- Keep font fixtures in-tree (`kitty_tests/*.otf`, `*.ttf`); do not fetch fonts.
- Use `--offline` for Cargo and pin `Cargo.lock`. Report when a dependency
  cannot be fetched rather than silently enabling network access.
- Run each measurement at least 5 times on an idle machine; use median and
  report variance. Reject comparisons where the C baseline itself varies by
  more than the claimed improvement.

### 5.6 Where C behavior must win over idiomatic Rust

Preserve, even when it looks wrong:
- Escape-sequence parsing order and malformed-sequence recovery.
- SGR application order, color rounding/indexing, and reverse/dim/decorations
  interactions.
- Cursor edge cases: wide characters at the right margin, trailers, origin
  mode, margins, and `REP`.
- Scrollback wrap/continuation flags and rewrap behavior on resize.
- Selection semantics and serialization, including trailing spaces and
  hyperlinks.
- Unicode width/grapheme rules, including kitty's own exceptions and emoji
  handling.
- Graphics protocol ordering, image IDs, placement IDs, z-layers, and delete
  semantics.
- Remote-control JSON field names, error strings, and encryption framing.
- Key/mouse encoding tables and fallback behavior.

Do not "fix" a C bug that the tests or downstream behavior depend on unless it
is explicitly scoped and tested as a behavior change.

---

## 6. Risk list

| Risk | Severity | Why | Mitigation |
|---|---|---|---|
| GLFW vendoring | High | kitty does not use stock GLFW: 42k lines of fork with Wayland/X11/Cocoa, layer-shell, IME, D-Bus, notifications, session monitor, and kitty-specific window behavior. Replacing it is a project of its own. | Retain the vendored C GLFW behind a narrow FFI boundary for the first port, replace pieces incrementally, and only then decide whether a Rust windowing layer is worth the parity risk. |
| X11/Wayland/Cocoa integration | High | Protocols, input methods, clipboard, monitors, scale factors, decorations, and macOS menu behavior are platform-specific and largely untested outside real sessions. | Keep the existing C/ObjC backends initially. Add per-platform CI/smoke hosts. Port Cocoa last or behind `objc2`, with a feature flag. |
| Python kittens/API compatibility | High | 65 Python kittens, 120 Go kitten files, 44 remote-control modules, CLI parser generation, and external third-party kittens all assume the current CLI/JSON/Python API. The C extension disappearing does not remove the need for those interfaces. | Define the external contract (CLI, control socket, JSON, env vars) early. Keep a compatibility shim/launcher for Python kittens. Port in-process kittens only after the protocol is frozen. |
| Font shaping parity | High | FreeType/HarfBuzz/CoreText fallback selection, ligatures, cluster mapping, color emoji, variable fonts and synthetic styles are subtle. A pure-Rust shaper will not match glyph-for-glyph without large work. | Keep FreeType/HarfBuzz/Fontconfig/CoreText via FFI for the 1:1 port. Use `rustybuzz`/`swash` only as optional experiments, never as a silent replacement. |
| Unicode tables generation | Medium-High | `char-props-data.h`, width tables, grapheme rules and row/column diacritics are generated; Unicode version drift changes widths and line breaking. | Port `gen/wcwidth.py` and related generators, check in generated Rust tables keyed to the same Unicode version, and add a full-range differential test. |
| Remote-control protocol | Medium-High | ECDH/AES-GCM framing, JSON schemas, async responses, and command names are an external ABI used by scripts and kittens. | Implement byte-level compatibility tests against the C binary and the Python client before replacing the server. Use `serde_json` only with explicit field-order/error-shape tests. |
| Tests assume the Python API | High | 28k lines of `kitty_tests` import `fast_data_types` and Python classes (`Screen`, `LineBuf`, `HistoryBuf`, `ChildMonitor`, ...). A Rust binary cannot run them unchanged. | Build a Rust-native test harness with the same assertions, keep a thin compatibility extension for the transition, and mark Python-API-only tests as migration work rather than pretending they pass. |
| License and notices | Medium | kitty is GPL-3.0-or-later; vendored base64 is BSD-style, Verstable is MIT, GLFW is zlib/libpng. Ported files must retain copyright notices and SPDX identifiers. | Keep the existing `SPDX-License-Identifier: GPL-3.0-or-later` plus "ported from" notices in every Rust file. Audit third-party licenses and include notices in packaging. |
| Performance regression | Medium | Removing Python helps, but a different allocation strategy, extra bounds checks, `RefCell`, or shaper FFI overhead can erase the gain. | Land criterion benchmarks with Phase 0, gate each phase on throughput/latency, and profile with `perf` before/after. |
| Generated build inputs | Medium | Config headers, shader code and parser-dump code are generated by Python/Go scripts; the Rust build must reproduce them or consume pinned artifacts. | Keep generation in the build pipeline, check generated outputs into the repo or a build cache, and make the Rust build fail loudly on version mismatch. |
| Go tool tree divergence | Medium | `tools/` (231 Go files) is a parallel implementation of CLI/TUI/kittens. It may be tempting to port Go instead of C, causing behavior drift. | Treat Go as a reference for CLI/protocol behavior only; the 1:1 source of truth for terminal behavior remains the C core. |

---

## 7. C file to Rust module mapping and status

`cell.rs` is the port of `line.h`; there is no `cell.c`, so it is noted here
rather than in the table. Status as of the initial Phase 0 landing:

- `line-buf.c`, `text-cache.c`, `history.c` and the `line.h`/`lineops.h` cell
  helpers are ported and tested. `line-buf` rewrap/ANSI serialization and
  `history` pager/rewrap paths are still TODO.
- `vt-parser.c`'s state machine, UTF-8 decoding, C0/ESC/CSI/OSC/DCS/APC/PM/SOS
  handling and charset dispatch are ported to `kitty-vt`; the `Screen` action
  dispatchers (`dispatch_csi`/`dispatch_osc`/... in C) are not yet ported, so
  the C file remains in progress.
- `charsets.c` translation tables are ported as a helper module; applying them
  belongs to the future `kitty-screen` crate, matching C (`map_char` in
  `screen.c`).

| C file | Target crate/module | Status |
|---|---|---|
| `animation.c` | `kitty-graphics/src/animation.rs` + `kitty-render` | **ported** (delta frames, composition/timing, optional disk spill) |
| `char-props.c` | `kitty-unicode/src/char_props.rs` | **ported** |
| `charsets.c` | `kitty-vt/src/charsets.rs` | in progress (tables + UTF-8 DFA ported, Screen application pending) |
| `child.c` | `kitty-os/src/child.rs` | **ported** |
| `child-monitor.c` | `kitty-os/src/child_monitor.rs` | in progress (single-child Linux monitor; batching/parser-input modes deferred) |
| `cleanup.c` | `kitty-os/src/cleanup.rs` | planned |
| `colors.c` | `kitty-screen/src/colors.rs` | planned |
| `crypto.c` | `kitty-remote/src/crypto.rs` | **ported** |
| `cursor.c` | `kitty-screen/src/cursor.rs` | **ported** (core cursor/SGR/shape; beam/underline rendering pending) |
| `cursor_trail.c` | `kitty-render/src/cursor_trail.rs` | planned |
| `data-types.c` | `kitty-remote/src/{base64,binary}.rs` + `kitty-vt/src/constants.rs` (module dissolved) | in progress (base64/base85/binary ported; other exports pending) |
| `decorations.c` | `kitty-render/src/decorations.rs` | **ported** (geometry/masks) |
| `desktop.c` | `kitty-os/src/desktop.rs` | planned |
| `disk-cache.c` | `kitty-fonts/src/disk_cache.rs` | planned |
| `dnd.c` | `kitty-input/src/dnd.rs` | planned |
| `fast-file-copy.c` | `kitty-remote/src/fast_file_copy.rs` | in progress (descriptor copy + chunked extension; kitty wire protocol pending) |
| `fontconfig.c` | `kitty-fonts/src/fontconfig.rs` | **ported** (matching + coverage; candidate cache pending) |
| `font-names.c` | `kitty-fonts/src/font_names.rs` | in progress (family style/name resolution; full OpenType name tables pending) |
| `fonts.c` | `kitty-fonts/src/fonts.rs` | in progress (specs, metrics, fallback, shaping; rasterization/atlas pending) |
| `freetype.c` | `kitty-fonts/src/freetype.rs` | in progress (metrics/glyph lookup; rendering/variable axes pending) |
| `freetype_render_ui_text.c` | `kitty-fonts/src/ui_text.rs` | planned |
| `gl.c` | `kitty-render/src/{egl,gl_backend}.rs` | in progress (headless EGL/GL 3.3 core backend; window/presentation pending) |
| `glfw.c` | `kitty-window/src/{lib,wayland}.rs` | in progress (Linux X11 + Wayland including IME/DnD, touch and `zwp_tablet_v2`; Cocoa pending) |
| `glfw-wrapper.c` | `kitty-window/src/lib.rs` | in progress (X11/EGL equivalent backend) |
| `gl-wrapper.c` | `kitty-render/src/egl.rs` | in progress (function loading via eglGetProcAddress) |
| `glyph-cache.c` | `kitty-render/src/atlas.rs` | in progress (CPU atlas; shaped-run sprites pending) |
| `graphics.c` | `kitty-graphics/src/graphics.rs` + `kitty-render/src/images.rs` | in progress (parser, placements, textures, z-order, animation/composition and shm; remaining gaps in `PARITY.md`) |
| `history.c` | `kitty-core/src/history.rs` | **ported** (core ring; pager/rewrap pending) |
| `hyperlink.c` | `kitty-screen/src/hyperlink.rs` | **ported** |
| `key_encoding.c` | `kitty-input/src/key_encoding.rs` | **ported** |
| `keys.c` | `kitty-input/src/keys.rs` | **ported** (key map + dispatch; Screen/options integration pending) |
| `kittens.c` | `kitty-kittens/src/lib.rs` | in progress (discovery, native kitten host, panel/QAT, portal, remote-file and SSH engines; macOS-only kittens skipped) |
| `line-buf.c` | `kitty-core/src/linebuf.rs` | **ported** (core; rewrap/ANSI pending) |
| `line.c` | `kitty-core/src/line.rs` | in progress (cell/line helpers; ANSI/URL pending) |
| `logging.c` | `kitty-os/src/logging.rs` | planned |
| `loop-utils.c` | `kitty-os/src/loop_utils.rs` | **ported** |
| `macos_process_info.c` | `kitty-os/src/macos_process_info.rs` | planned |
| `mime-cache.c` | `kitty-graphics/src/mime_cache.rs` | in progress (embedded image table; XDG mime.cache pending) |
| `monotonic.c` | `kitty-os/src/monotonic.rs` | **ported** |
| `mouse.c` | `kitty-input/src/mouse.rs` | **ported** (protocols/tracking; Screen selection integration pending) |
| `png-reader.c` | `kitty-graphics/src/png_reader.rs` | **ported** (no ICC/lcms color management) |
| `resize.c` | `kitty-screen/src/screen.rs` (rewrap) | **ported** |
| `rowcolumn-diacritics.c` | `kitty-vt/src/diacritics.rs` | planned |
| `screen.c` | `kitty-screen/src/screen.rs` | in progress (draw/cursor/edit/scroll/alt-screen/CSI subset; graphics/resize/mouse/DnD/OSC mutations pending) |
| `shaders.c` | `kitty-render/src/gl_shaders.rs` | in progress (functional GLSL subset; kitty Slang programs pending) |
| `shaped-run-cache.c` | `kitty-fonts/src/shaped_run_cache.rs` | **ported** (bounded cache; cell-group representation pending) |
| `shlex.c` | `kitty-config/src/shlex.rs` | **ported** |
| `simd-string.c` | `kitty-simd/src/{lib,scalar,utf8}.rs` | **ported** |
| `simd-string-128.c` | `kitty-simd/src/x86/sse2.rs` | **ported** |
| `simd-string-256.c` | `kitty-simd/src/x86/avx2.rs` | **ported** |
| `simd-string-512.c` | `kitty-simd/src/x86/avx512.rs` | **ported** |
| `state.c` | `kitty-config/src/options.rs` | in progress (typed options; runtime global state pending) |
| `systemd.c` | `kitty-os/src/systemd.rs` | planned |
| `text-cache.c` | `kitty-core/src/text_cache.rs` | **ported** |
| `utmp.c` | `kitty-os/src/utmp.rs` | planned |
| `vt-parser.c` | `kitty-vt/src/parser.rs` | in progress (state machine ported; Screen dispatch pending) |
| `wcswidth.c` | `kitty-unicode/src/wcswidth.rs` | **ported** |
| `window_logo.c` | `kitty-render/src/window_logo.rs` | planned |

Additional Rust status: `kitty-core/src/cell.rs` (ported from `line.h`),
`kitty-core/src/line.rs` (partial), `kitty-core/src/linebuf.rs` (ported core),
`kitty-core/src/history.rs` (ported core), `kitty-core/src/text_cache.rs`
(ported), `kitty-vt/src/{parser,params,charsets}.rs` (state machine ported),
`kitty-simd` (scalar + SSE2/AVX2/AVX-512 primitives, UTF-8 decoder) and
`kitty-unicode` (generated Unicode 17 tables, grapheme segmentation,
`wcswidth`), `kitty-screen` (core screen/cursor/lineops), `kitty-input`
(keyboard encoding, kitty protocol, mouse protocols) and `kitty-os` (Linux
PTY/fork/exec, poll loop, SIGCHLD monitor). The workspace still has no external
crate dependencies and no `kitty-fonts`, `kitty-render`, `kitty-graphics`,
`kitty-config`, `kitty-remote`, `kitty-kittens`, or binary crate.

---

## 8. Effort estimate

These are engineer-months (EM) for one experienced systems/Rust engineer, with
existing familiarity with kitty. They assume the C implementation remains
available as a reference and that FreeType/HarfBuzz/Fontconfig/GLFW are retained
via FFI rather than rewritten. They are estimates, not commitments.

| Phase | Scope | Estimate (EM) |
|---|---|---:|
| 0 | Workspace, cells/layout/text-cache/linebuf | 1-2 |
| 1 | VT parser, screen, lineops, history, charsets, Unicode | 8-12 |
| 2 | Input, config, keys, mouse, options | 4-7 |
| 3 | Fonts and text shaping | 7-11 |
| 4 | GPU renderer/OpenGL and graphics protocol | 9-15 |
| 5 | Windowing, platform, child loop, GLFW/Cocoa | 12-18 |
| 6 | Remote control, kittens, protocol compatibility | 9-15 |
| 7 | Parity testing, performance, packaging | 8-12 |
| **Total** | | **58-92** |

That is roughly five to eight years for one engineer, or two to three years
with a small team, before a literal 1:1 replacement is credible. A full rewrite
that also replaces the vendored GLFW/Wayland/X11/Cocoa stack, the Python kitten
ecosystem, and the Go tools would be larger, realistically a multi-year effort
even with several engineers. The estimate is dominated by platform and
compatibility work, not by the terminal data structures.

The first useful milestone is not "Rust terminal runs a shell"; it is Phase 1
with a parser/screen differential harness and Phase 0's layout invariants
locked down. Without those, later phases cannot tell whether a visual difference
is a renderer bug or a core semantics bug.
