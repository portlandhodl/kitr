# kitr packaging

This directory adds packaging, terminfo, shell-integration and parity tooling
around the Rust Cargo workspace. It does not modify any Rust crate source.

`kitr` is an early Linux/X11 port. The packaging below covers the `kitr`
binary, its `xterm-kitty` terminfo entry, upstream kitty shell-integration
scripts, XDG desktop metadata and a man page. Wayland/macOS backends, kittens,
remote control and automatic shell-integration injection are **not**
implemented by kitr yet; nothing here pretends otherwise.

## Requirements

Build and install:

- Rust 1.89 or newer and Cargo
- system linker dependencies: `libfreetype`, `libharfbuzz`, `libfontconfig`,
  `libEGL`, `libX11`, `libpng`, `zlib`, `libcrypto`
- `tic` from ncurses for terminfo
- `install` from coreutils

Parity tooling additionally needs Python 3, Xvfb, ImageMagick
(`convert`/`magick`) and a C kitty installation with both `kitty` and `kitten`
on `PATH`.

## Make targets

Run these from the Cargo workspace root `rust/`:

```sh
make build              # cargo build --release --workspace
make test               # workspace tests, then all-features tests
make lint               # rustfmt --check, CPU clippy, all-features clippy
make terminfo           # compile build/terminfo/{x,78}/xterm-kitty
make install            # binary, terminfo, shell integration, desktop, man
make uninstall          # remove the installed files
make screenshot-parity  # best-effort C kitty vs kitr pixel comparison
make license-check      # SPDX headers, notices, bundled-file manifests
make validate           # desktop-file-validate + appstreamcli validate
make clean              # cargo clean and generated artifacts
```

`PREFIX` defaults to `/usr/local` and `DESTDIR` is supported:

```sh
make install DESTDIR=/tmp/stage PREFIX=/usr
make uninstall DESTDIR=/tmp/stage PREFIX=/usr
```

The installed layout is:

```
$PREFIX/bin/kitr
$PREFIX/libexec/kitr/kitr
$PREFIX/share/terminfo/{x,78}/xterm-kitty
$PREFIX/share/kitr/shell-integration/...
$PREFIX/share/fish/vendor_conf.d/kitty-shell-integration.fish
$PREFIX/share/applications/kitr.desktop
$PREFIX/share/metainfo/io.github.portlandhodl.kitr.metainfo.xml
$PREFIX/share/man/man1/kitr.1
```

`bin/kitr` is a POSIX `sh` launcher wrapper. It locates
`libexec/kitr/kitr`, picks `TERM=xterm-kitty` when the terminfo entry is
available (otherwise falls back to `xterm-256color`), sets `TERM_PROGRAM=kitr`
and `COLORTERM=truecolor`, and exposes `KITR_SHELL_INTEGRATION_DIR` /
`KITTY_INSTALLATION_DIR`. Overrides: `KITR_REAL_BINARY`, `KITR_TERM`,
`KITR_SHELL_INTEGRATION`. No Python is used by the wrapper or the packaged
shell scripts.

## Terminfo

The vendored `terminfo/kitty.terminfo` and `kitty.termcap` are unmodified
copies of kitty's generated sources (GPL-3.0-or-later, see
`terminfo/NOTICE`). `make terminfo` runs:

```sh
tic -x -o build/terminfo packaging/terminfo/kitty.terminfo
```

and installs the result under both `x/` and the legacy `78/` directory.

**Byte identity:** in this checkout, GNU ncurses `tic -x` produces a
**byte-identical** file to C kitty's checked-in `terminfo/x/xterm-kitty` and
`terminfo/78/xterm-kitty`:

```
sha256 75a5836628e596ab1c236aeff22a298558ed50e2301248f30b8e236e8e52aabd
size   3721 bytes
```

`tic` implementations/versions differ, so a different ncurses release may
produce a functionally equivalent but not byte-identical database. `make
terminfo` prints the comparison when `../terminfo/x/xterm-kitty` exists.

**Fallback:** if the terminfo entry cannot be installed or found, use
`TERM=xterm-256color` (or `KITR_TERM=xterm-256color` with the wrapper). The
wrapper detects the missing entry automatically.

## Shell integration

`shell-integration/` contains byte-for-byte copies of kitty's bash, zsh and
fish integration scripts under GPL-3.0-or-later. `NOTICE`, `MANIFEST.md` and
`SHA256SUMS` record provenance and hashes; `make license-check` verifies them.
The Python SSH bootstrap and Python-dependent completion scripts are
deliberately excluded.

kitr does not yet implement kitty's automatic injection (`KITTY_BASH_INJECT`,
zsh `ZDOTDIR`, fish `XDG_DATA_DIRS` manipulation). To try the scripts manually,
for example:

```sh
# bash (after sourcing your normal bashrc)
KITTY_SHELL_INTEGRATION=enabled source \
  "$PREFIX/share/kitr/shell-integration/bash/kitty.bash"

# zsh: add the packaged integration directory to fpath or source it directly
KITTY_SHELL_INTEGRATION=enabled source \
  "$PREFIX/share/kitr/shell-integration/zsh/kitty.zsh"

# fish: the file is also installed in share/fish/vendor_conf.d; enable with
set -gx KITTY_SHELL_INTEGRATION enabled
```

These scripts emit kitty escape sequences. kitr may not implement all of the
corresponding terminal features yet; this is packaged for compatibility and
experimentation, not as a claim of full shell-integration support.

## Desktop metadata and man page

- `desktop/kitr.desktop` references `Icon=kitr`.
- `icons/README.md` documents the expected icon paths. No artwork is bundled;
  `make install` installs `icons/kitr.png`/`kitr.svg` only if you provide them.
- `metainfo/io.github.portlandhodl.kitr.metainfo.xml` is an AppStream skeleton.
  The release is a packaging placeholder for version 0.1.0.
- `man/kitr.1` documents the current CLI: `--headless`, `--gui`, `--config`,
  `--size`, `--screenshot`, `--timeout`, `-e`/`--`, plus the currently
  implemented `--dump-screen`, `--scrollback`, `--title`, `-c`, `-h`, `-V`.

## Screenshot parity (best effort)

`scripts/screenshot_parity.py` runs the same `/bin/sh -c` command in C kitty and
kitr, under Xvfb (or an existing `DISPLAY`), then:

1. asks C kitty for a PNG via `kitten @ --to unix:<socket> screenshot`;
2. runs `kitr --gui --screenshot out.ppm --size COLSxROWS`;
3. converts both images to PPM with ImageMagick;
4. reports the percentage of pixels whose channel delta exceeds
   `--pixel-threshold`, writes a diff PPM/PNG and `report.json` under
   `artifacts/screenshot-parity/`.

```sh
make screenshot-parity
# or with explicit paths/options:
python3 scripts/screenshot_parity.py \
  --kitty /usr/bin/kitty --kitten /usr/bin/kitten \
  --kitr target/release/kitr \
  --command "printf 'hello\\n'" \
  --size 80x24 --threshold 5 --artifacts artifacts/parity
```

Exit codes: `0` pass or skip, `1` diff above threshold, `2` error, `3` missing
prerequisite. `--strict` turns a skip into an error. Missing kitty/kitten,
Xvfb or ImageMagick is reported and skipped gracefully by default.

This is a **smoke test, not a conformance oracle**. Fonts, font size, DPI,
window decorations, tab bar settings, EGL/GL rasterisation and toolkit
differences can move pixels even when terminal behaviour matches. The script
sets `tab_bar_style=hidden`, zero padding, opaque background and no
decorations for C kitty, but does not attempt to match fonts or scale.

## License check

`scripts/check_licenses.py` verifies that repository Rust/Python sources and
the launcher/Makefile carry an allowed SPDX identifier, that `LICENSE`,
`NOTICE` and `THIRD_PARTY_NOTICES.md` exist with kitty attribution, and that
the bundled shell-integration/terminfo `SHA256SUMS` match. It also checks the
packaging scripts are executable.

## CI

`.github/workflows/ci.yml` runs on Ubuntu with rustfmt, CPU and all-features
clippy, workspace tests, all-features GUI tests under `xvfb-run`, a release
build, terminfo compilation, license/metadata checks, and an optional
screenshot-parity smoke step. Cargo registry/git and `target/` are cached. The
parity step is `continue-on-error` because a C kitty installation may be
unavailable or its rendering may differ.

## Known packaging gaps

- **No Wayland backend.** Only Linux/X11 is packaged. There is no Wayland
  desktop file variant or portal integration.
- **No macOS/Cocoa app bundle.** Only the Linux ELF binary is covered.
- **No kittens or remote control in kitr.** `kitten` itself is not packaged
  with kitr; the parity script uses a separately installed C kitty.
- **No automatic shell-integration injection.** Scripts are shipped, not wired
  into shell startup by kitr.
- **No icon artwork.** The desktop entry references the name `kitr`; an icon
  must be supplied by the packager.
- **No distro packages.** There is no deb/rpm/Flatpak/Snap/Arch packaging or
  system-wide completion generation here.
- **Best-effort parity only.** There is no golden-image suite, font pinning or
  perceptual comparison; thresholds are deliberately conservative defaults.
