#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Build a source tarball and an RPM from this workspace.
set -euo pipefail

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
version=${VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n1)}
topdir=${RPM_TOPDIR:-$root/dist/rpmbuild}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

mkdir -p "$topdir"/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS}
name="kitr-$version"
mkdir -p "$work/$name"
tar -C "$root" \
    --exclude=.git --exclude=target --exclude=dist --exclude=artifacts \
    --exclude=build --exclude=__pycache__ \
    -cf - . | tar -C "$work/$name" -xf -
tar -C "$work" -czf "$topdir/SOURCES/$name.tar.gz" "$name"
rpmbuild -bb \
    --define "_topdir $topdir" \
    --define "version $version" \
    "$root/packaging/rpm/kitr.spec"
find "$topdir/RPMS" -type f -name '*.rpm' -print
