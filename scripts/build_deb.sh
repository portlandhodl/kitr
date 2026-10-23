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

installed_size=$(du -sk "$stage" | cut -f1)
cat > "$control/control" <<EOF
Package: kitr
Version: ${version}-1
Section: x11
Priority: optional
Architecture: ${arch}
Maintainer: kitr contributors <maintainers@portlandhodl.example>
Installed-Size: ${installed_size}
Depends: libc6, libgcc-s1, libegl1, libgl1, libx11-6, libxext6, libxi6,
 libfontconfig1, libfreetype6, libharfbuzz0b, libpng16-16, zlib1g,
 libssl3t64 | libssl3, libbz2-1.0, liblzma5, libglib2.0-0t64 | libglib2.0-0,
 libpcre3, libxml2, libxcb1, libxau6, libxdmcp6
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
