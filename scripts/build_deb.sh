#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Build a Debian binary package without debhelper or dpkg-buildpackage.
set -euo pipefail

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
version=${VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n1)}
machine=${ARCH:-$(uname -m)}
case "$machine" in
    x86_64 | amd64) arch=amd64 ;;
    aarch64 | arm64) arch=arm64 ;;
    armv7l | armhf) arch=armhf ;;
    *) arch=$machine ;;
esac
out=${OUT:-$root/dist/deb-src}
package="kitr_${version}-1_${arch}.deb"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
stage=$work/root
control=$work/DEBIAN
mkdir -p "$stage" "$control" "$out"

make -C "$root" PREFIX=/usr DESTDIR="$stage" install >/dev/null
install -Dm644 "$root/packaging/debian/copyright" "$stage/usr/share/doc/kitr/copyright"
install -Dm644 "$root/README.md" "$stage/usr/share/doc/kitr/README.md"
install -Dm644 "$root/NOTICE" "$stage/usr/share/doc/kitr/NOTICE"

# Map direct ELF dependencies to Debian package names. Transitive libraries are
# resolved by apt; mapping only NEEDED entries avoids stale guesses such as
# libpcre3 on distributions that removed PCRE1.
declare -A package_for=(
    [libc.so.6]='libc6'
    [libm.so.6]='libc6'
    [libgcc_s.so.1]='libgcc-s1'
    [libz.so.1]='zlib1g'
    [libcrypto.so.3]='libssl3t64 | libssl3'
    [libssl.so.3]='libssl3t64 | libssl3'
    [libEGL.so.1]='libegl1'
    [libGL.so.1]='libgl1'
    [libGLdispatch.so.0]='libgl1'
    [libX11.so.6]='libx11-6'
    [libXext.so.6]='libxext6'
    [libXi.so.6]='libxi6'
    [libxcb.so.1]='libxcb1'
    [libXau.so.6]='libxau6'
    [libXdmcp.so.6]='libxdmcp6'
    [libfontconfig.so.1]='libfontconfig1'
    [libfreetype.so.6]='libfreetype6'
    [libharfbuzz.so.0]='libharfbuzz0b'
    [libpng16.so.16]='libpng16-16'
    [libbz2.so.1]='libbz2-1.0'
    [liblzma.so.5]='liblzma5'
    [libglib-2.0.so.0]='libglib2.0-0t64 | libglib2.0-0'
    [libpcre.so.1]='libpcre3'
    [libpcre2-8.so.0]='libpcre2-8-0'
    [libxml2.so.2]='libxml2'
    [libwayland-client.so.0]='libwayland-client0'
    [libwayland-egl.so.1]='libwayland-egl1'
    [libxkbcommon.so.0]='libxkbcommon0'
)
declare -A chosen=()
while read -r lib; do
    case "$lib" in
        linux-vdso.so.1 | ld-linux*.so.* | /lib64/ld-linux*) continue ;;
    esac
    pkg=${package_for[$lib]:-}
    if [[ -z "$pkg" ]]; then
        printf 'warning: no Debian package mapping for %s\n' "$lib" >&2
        continue
    fi
    chosen[$pkg]=1
done < <(
    { objdump -p "$root/target/release/kitr"
      objdump -p "$root/target/release/kitten"; } \
        | awk '/NEEDED/{print $2}' | sort -u
)
depends=$(printf '%s\n' "${!chosen[@]}" | sort | paste -sd, - | sed 's/,/, /g')

installed_size=$(du -sk "$stage" | cut -f1)
cat > "$control/control" <<EOF
Package: kitr
Version: ${version}-1
Section: x11
Priority: optional
Architecture: ${arch}
Maintainer: kitr contributors <maintainers@portlandhodl.example>
Installed-Size: ${installed_size}
Depends: ${depends}
Recommends: fonts-dejavu-core, xdg-utils
Provides: x-terminal-emulator
Conflicts: kitty
Homepage: https://github.com/portlandhodl/kitr
Description: fast GPU-accelerated terminal emulator (kitty-compatible Rust port)
 kitr is a Rust port of the kitty terminal emulator. It implements the
 kitty graphics protocol, Unicode placeholder images, live indexed-colour
 palettes, split layouts, tabs, sessions, remote control and the common
 kittens on Linux.
EOF

(
    cd "$stage"
    find . -type f ! -path './DEBIAN/*' -print0 \
        | sort -z \
        | xargs -0 md5sum
) > "$control/md5sums"

(
    cd "$stage"
    tar --owner=0 --group=0 --numeric-owner --exclude='./DEBIAN' -cf - .
) | xz -9 -T0 > "$work/data.tar.xz"
(
    cd "$control"
    tar --owner=0 --group=0 --numeric-owner -cf - .
) | xz -9 -T0 > "$work/control.tar.xz"
printf '2.0\n' > "$work/debian-binary"

(cd "$work" && ar rcs "$out/$package" debian-binary control.tar.xz data.tar.xz)
sha256sum "$out/$package" > "$out/$package.sha256"
printf '%s\n' "$out/$package"
