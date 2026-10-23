#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Generate Flatpak Cargo sources from Cargo.lock.

The output matches the format consumed by flatpak-builder's
flatpak-cargo-generator: one archive source per registry crate plus an inline
Cargo config that redirects crates.io to the vendored tree.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

PACKAGE_RE = re.compile(r'^name = "(?P<name>.+)"$')
VERSION_RE = re.compile(r'^version = "(?P<version>.+)"$')
SOURCE_RE = re.compile(r'^source = "(?P<source>.+)"$')
CHECKSUM_RE = re.compile(r'^checksum = "(?P<checksum>[0-9a-fA-F]+)"$')
REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"


def parse_lock(path: Path) -> list[dict[str, str]]:
    packages: list[dict[str, str]] = []
    current: dict[str, str] | None = None
    for line in path.read_text(encoding="utf-8").splitlines():
        if line == "[[package]]":
            if current:
                packages.append(current)
            current = {}
            continue
        if current is None:
            continue
        for key, pattern in (
            ("name", PACKAGE_RE),
            ("version", VERSION_RE),
            ("source", SOURCE_RE),
            ("checksum", CHECKSUM_RE),
        ):
            match = pattern.match(line)
            if match:
                current[key] = match.group(key)
                break
    if current:
        packages.append(current)
    return packages


def generate(lock: Path) -> list[dict[str, str]]:
    packages = [
        package
        for package in parse_lock(lock)
        if package.get("source") == REGISTRY
    ]
    name_counts: dict[str, int] = {}
    for package in packages:
        name_counts[package.get("name", "")] = (
            name_counts.get(package.get("name", ""), 0) + 1
        )
    sources: list[dict[str, str]] = []
    for package in packages:
        name = package.get("name", "")
        version = package.get("version", "")
        checksum = package.get("checksum", "")
        if not name or not version or not checksum:
            raise SystemExit(f"error: incomplete registry package in {lock}: {package}")
        # Cargo vendor uses name-version directories when a crate appears more
        # than once (for example syn 1.x and 2.x).
        directory = name if name_counts[name] == 1 else f"{name}-{version}"
        sources.append(
            {
                "type": "archive",
                "url": f"https://crates.io/api/v1/crates/{name}/{version}/download",
                "sha256": checksum,
                "dest": f"cargo/vendor/{directory}",
            }
        )
    sources.sort(key=lambda item: item["url"])
    sources.append(
        {
            "type": "inline",
            "contents": (
                "[source.crates-io]\n"
                'replace-with = "vendored-sources"\n'
                "\n"
                "[source.vendored-sources]\n"
                'directory = "cargo/vendor"\n'
            ),
            "dest": "cargo/config.toml",
        }
    )
    return sources


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("lock", nargs="?", type=Path, default=Path("Cargo.lock"))
    parser.add_argument(
        "-o",
        "--output",
        type=Path,
        default=Path("packaging/flatpak/cargo-sources.json"),
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="fail if the output file is not up to date",
    )
    args = parser.parse_args(argv)
    if not args.lock.is_file():
        print(f"error: {args.lock} does not exist", file=sys.stderr)
        return 2
    text = json.dumps(generate(args.lock), indent=2, sort_keys=True) + "\n"
    if args.check:
        try:
            current = args.output.read_text(encoding="utf-8")
        except OSError as error:
            print(f"error: cannot read {args.output}: {error}", file=sys.stderr)
            return 1
        if current != text:
            print(
                f"error: {args.output} is stale; run scripts/generate_flatpak_cargo_sources.py",
                file=sys.stderr,
            )
            return 1
        print(f"flatpak-cargo-sources: {args.output} is up to date")
        return 0
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(text, encoding="utf-8")
    print(f"flatpak-cargo-sources: wrote {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
