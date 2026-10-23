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
make package            # stage /usr install, smoke-check metadata, write tarball
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
- `icons/` contains original SVG artwork and rendered hicolor PNGs;
  `make install` installs every available size plus the scalable SVG.
- `metainfo/io.github.portlandhodl.kitr.metainfo.xml` is an AppStream skeleton.
  The release is a packaging placeholder for version 0.1.3.
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

## Distribution artifacts

`make package` builds the workspace, stages a `DESTDIR` install under
`dist/stage` with `PREFIX=/usr`, checks the installed layout and launcher, runs
`scripts/package_smoke.py`, and writes
`dist/kitr-0.1.3-linux-<arch>.tar.gz`.

Distribution metadata skeletons live in:

- `debian/` — Debian/Ubuntu `debian/rules`, `control`, `changelog` and
  `copyright`; `make deb` runs `dpkg-buildpackage -b` when available and falls
  back to `scripts/build_deb.sh`, a direct `ar`/`tar` builder that needs only
  `xz`, `tar` and `ar`.
- `rpm/kitr.spec` — Fedora/RHEL/openSUSE RPM spec; `make rpm` builds a source
  tarball and runs `rpmbuild -bb`.
- `flatpak/io.github.portlandhodl.kitr.yml` — reproducible Flatpak manifest.
  `packaging/flatpak/cargo-sources.json` is generated from `Cargo.lock` by
  `scripts/generate_flatpak_cargo_sources.py` (one archive per registry crate
  plus the vendored-source config); `make flatpak-sources-check` detects stale
  output.

The staged-install smoke checks remain offline. CI installs the distro build
dependencies and runs `make deb`, `make rpm` and `flatpak-builder` for real.

## License check

`scripts/check_licenses.py` verifies that repository Rust/Python sources and
the launcher/Makefile carry an allowed SPDX identifier, that `LICENSE`,
`NOTICE` and `THIRD_PARTY_NOTICES.md` exist with kitty attribution, and that
the bundled shell-integration/terminfo `SHA256SUMS` match. It also checks the
packaging scripts are executable.

## CI

`.github/workflows/ci.yml` runs rustfmt, CPU and all-features clippy, workspace
tests, all-features GUI tests under `xvfb-run`, a release build, terminfo
compilation, license/metadata checks, generated Flatpak source verification,
Debian and RPM package builds, a Flatpak bundle build, and the full
screenshot-parity corpus against a distro C kitty. Cargo registry/git and
`target/` are cached. The parity job is `continue-on-error` because a distro C
kitty version or its font/rendering environment can differ from the reference
build; its report and artifacts are always uploaded.

## Known packaging gaps

- **Wayland packaging is portal-light.** The Rust backend, layer-shell panel/QAT
  hosting and desktop portal kitten work, but the Flatpak has no separate
  finish-arg profile for portal-file access beyond the manifest defaults.
- **No macOS/Cocoa app bundle.** Only the Linux ELF binary is covered.
- **Kitten coverage is Linux/portable only.** The `kitten` binary is packaged
  alongside kitr with the native kitten set; Python-only and macOS-specific
  kittens are not shipped. The parity script still uses a separately installed
  C kitty reference.
- **No automatic shell-integration injection.** Scripts are shipped, not wired
  into shell startup by kitr.
- **Packages are unsigned.** deb, RPM and Flatpak artifacts are built in CI,
  but there is no repository signing/key management. Snap/Arch packaging and
  system-wide completion generation are absent.
- **Flatpak build needs a runtime.** CI installs the 24.08 Platform/SDK and the
  Rust SDK extension; offline builds require that runtime to be present.
- **Best-effort parity only.** There is no golden-image suite, font pinning or
  perceptual comparison; thresholds are deliberately conservative defaults.
