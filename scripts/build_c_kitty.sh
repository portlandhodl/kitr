#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Build the C reference implementation (the real kitty, not kitr) from this
# source tree without modifying any C source.
#
# Defaults are tuned for the parity host:
#   Python/build deps: /tmp/opencode/py312
#   build cache:       /tmp/opencode/kitty-build-cache
# Override with PY_PREFIX, PYTHON, KITTY_BUILD_CACHE, CC, CFLAGS, SLANGC, etc.
#
# Usage:
#   scripts/build_c_kitty.sh            # debug build (make debug equivalent)
#   scripts/build_c_kitty.sh --release  # optimised build
#   scripts/build_c_kitty.sh --clean    # remove previous build first
#
# The script only installs extra conda packages if a required pkg-config
# package is missing.  It downloads pinned copies of slangc and the Symbols
# Nerd Font only when they are not already available.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PY_PREFIX="${PY_PREFIX:-/tmp/opencode/py312}"
PYTHON="${PYTHON:-$PY_PREFIX/bin/python3}"
BUILD_CACHE="${KITTY_BUILD_CACHE:-/tmp/opencode/kitty-build-cache}"
MODE="debug"
CLEAN=0

while [ $# -gt 0 ]; do
    case "$1" in
        --release) MODE="release" ;;
        --debug) MODE="debug" ;;
        --clean) CLEAN=1 ;;
        -h|--help)
            sed -n '2,25p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "build_c_kitty.sh: unknown argument: $1" >&2
            exit 2
            ;;
    esac
    shift
done

if [ ! -x "$PYTHON" ]; then
    echo "build_c_kitty.sh: Python not found at $PYTHON" >&2
    echo "Set PYTHON=/path/to/python3 or PY_PREFIX=/path/to/conda/env" >&2
    exit 1
fi

export PATH="$PY_PREFIX/bin:$PATH"
export PKG_CONFIG_PATH="$PY_PREFIX/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
export LD_LIBRARY_PATH="$PY_PREFIX/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
# Clang 23 promotes this vendored-GLFW warning to an error with -Werror.
export CFLAGS="${CFLAGS:-} -Wno-unused-but-set-global"

required_pc=(
    freetype2 harfbuzz fontconfig cairo lcms2 libpng openssl simde
    xkbcommon xkbcommon-x11
)
conda_pkgs=(
    freetype harfbuzz fontconfig cairo lcms2 libpng openssl simde libxkbcommon
)

missing=()
for pc in "${required_pc[@]}"; do
    if ! pkg-config --exists "$pc"; then
        missing+=("$pc")
    fi
done

if [ ${#missing[@]} -gt 0 ]; then
    echo "build_c_kitty.sh: missing pkg-config packages: ${missing[*]}" >&2
    if command -v conda >/dev/null 2>&1; then
        echo "build_c_kitty.sh: installing conda packages into $PY_PREFIX" >&2
        conda install -y -p "$PY_PREFIX" "${conda_pkgs[@]}"
        export PKG_CONFIG_PATH="$PY_PREFIX/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
        for pc in "${missing[@]}"; do
            pkg-config --exists "$pc" || {
                echo "build_c_kitty.sh: pkg-config still cannot find $pc" >&2
                exit 1
            }
        done
    else
        echo "build_c_kitty.sh: install the matching development packages or set" >&2
        echo "  PY_PREFIX to a conda environment that already has them" >&2
        exit 1
    fi
fi

download() {
    local url="$1" dest="$2"
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --retry 3 -o "$dest" "$url"
    elif command -v wget >/dev/null 2>&1; then
        wget -q -O "$dest" "$url"
    else
        echo "build_c_kitty.sh: need curl or wget to download $url" >&2
        return 1
    fi
}

ensure_slangc() {
    if command -v slangc >/dev/null 2>&1; then
        echo "build_c_kitty.sh: using slangc from $(command -v slangc)"
        return
    fi
    local version="2026.20"
    local archive="slang-${version}-linux-x86_64-glibc-2.28.tar.gz"
    local url="https://github.com/shader-slang/slang/releases/download/v${version}/${archive}"
    local checksum="84ebd777968caceea26b21bfd014341078be6047be6c5a963a69f984f896cebd"
    local dest="$BUILD_CACHE/slang-${version}"
    if [ ! -x "$dest/bin/slangc" ]; then
        mkdir -p "$BUILD_CACHE"
        local tmp="$BUILD_CACHE/.${archive}.tmp"
        echo "build_c_kitty.sh: downloading slangc $version" >&2
        download "$url" "$tmp"
        echo "$checksum  $tmp" | sha256sum -c - >/dev/null
        rm -rf "$dest"
        mkdir -p "$dest"
        tar -xzf "$tmp" -C "$dest"
        rm -f "$tmp"
    fi
    export PATH="$dest/bin:$PATH"
    echo "build_c_kitty.sh: using slangc $version from $dest/bin"
}

ensure_nerd_font() {
    local dest="$ROOT/fonts/SymbolsNerdFontMono-Regular.ttf"
    if [ -s "$dest" ]; then
        return
    fi
    mkdir -p "$ROOT/fonts"
    local found=""
    if command -v fc-list >/dev/null 2>&1; then
        local listing
        listing="$(fc-list --format '%{file}\n%{postscriptname}\n' :postscriptname=SymbolsNFM 2>/dev/null || true)"
        found="$(printf '%s\n' "$listing" | sed -n '1p')"
        local psname
        psname="$(printf '%s\n' "$listing" | sed -n '2p')"
        if [ "$psname" != "SymbolsNFM" ] || [ ! -s "$found" ]; then
            found=""
        fi
    fi
    if [ -z "$found" ]; then
        local version="v3.4.0"
        local url="https://raw.githubusercontent.com/ryanoasis/nerd-fonts/${version}/patched-fonts/NerdFontsSymbolsOnly/SymbolsNerdFontMono-Regular.ttf"
        local checksum="f0f624d9b474bea1662cf7e862d44aebe1ae1f6c7f9cb7a0ca5d0e5ac9561c60"
        mkdir -p "$BUILD_CACHE"
        local tmp="$BUILD_CACHE/SymbolsNerdFontMono-Regular.ttf"
        echo "build_c_kitty.sh: downloading Symbols Nerd Font Mono $version" >&2
        download "$url" "$tmp"
        echo "$checksum  $tmp" | sha256sum -c - >/dev/null
        found="$tmp"
    fi
    cp "$found" "$dest"
    chmod 0644 "$dest"
    echo "build_c_kitty.sh: installed $dest"
}

ensure_slangc
ensure_nerd_font

cd "$ROOT"
if [ "$CLEAN" = 1 ]; then
    echo "build_c_kitty.sh: cleaning previous build"
    "$PYTHON" setup.py clean
fi

echo "build_c_kitty.sh: building kitty ($MODE)"
if [ "$MODE" = "release" ]; then
    "$PYTHON" setup.py build
else
    "$PYTHON" setup.py build --debug
fi

KITTY_BIN="$ROOT/kitty/launcher/kitty"
[ -x "$KITTY_BIN" ] || {
    echo "build_c_kitty.sh: build finished but $KITTY_BIN is missing" >&2
    exit 1
}
echo -n "build_c_kitty.sh: "
"$KITTY_BIN" --version
