#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Verify SPDX headers, required notices and third-party manifests.

This is a packaging guard, not a replacement for a full REUSE/license audit.
It checks:

* every repository Rust/Python source and the launcher/Makefile starts with an
  allowed SPDX-License-Identifier;
* the root GPL/NOTICE files exist and still mention kitty's authorship;
* the bundled shell-integration and terminfo copies have a NOTICE plus a
  SHA256SUMS manifest whose hashes match the files on disk;
* the scripts added for packaging are executable.
"""

from __future__ import annotations

import argparse
import hashlib
import os
import re
import sys
from pathlib import Path

ALLOWED_LICENSES = {
    "GPL-3.0-only",
    "GPL-3.0-or-later",
}
SPDX_RE = re.compile(r"SPDX-License-Identifier:\s*([A-Za-z0-9.+-]+)")
SKIP_DIRS = {".git", "target", "build", "artifacts", "node_modules", "__pycache__"}
EXCLUDED_PREFIXES = (
    "packaging/shell-integration/",
    "packaging/terminfo/",
)


def rel(root: Path, path: Path) -> str:
    return path.relative_to(root).as_posix()


def source_files(root: Path) -> list[Path]:
    found: set[Path] = set()
    for pattern in ("*.rs", "*.py"):
        for path in root.rglob(pattern):
            r = rel(root, path)
            if any(part in SKIP_DIRS for part in Path(r).parts):
                continue
            if r.startswith(EXCLUDED_PREFIXES):
                continue
            found.add(path)
    for name in ("Makefile", "packaging/bin/kitr"):
        path = root / name
        if path.is_file():
            found.add(path)
    return sorted(found)


def read_head(path: Path, lines: int = 8) -> str:
    try:
        with path.open("r", encoding="utf-8", errors="replace") as f:
            return "".join(f.readline() for _ in range(lines))
    except OSError as e:
        raise SystemExit(f"error: cannot read {path}: {e}")


def check_spdx(root: Path, errors: list[str]) -> int:
    checked = 0
    for path in source_files(root):
        head = read_head(path)
        match = SPDX_RE.search(head)
        if not match:
            errors.append(f"{rel(root, path)}: missing SPDX-License-Identifier in first 8 lines")
            continue
        if match.group(1) not in ALLOWED_LICENSES:
            errors.append(f"{rel(root, path)}: unexpected license {match.group(1)!r}")
        checked += 1
    return checked


def check_required_notices(root: Path, errors: list[str]) -> None:
    license_path = root / "LICENSE"
    notice_path = root / "NOTICE"
    third_party = root / "THIRD_PARTY_NOTICES.md"

    if not license_path.is_file():
        errors.append("LICENSE is missing")
    else:
        text = license_path.read_text(encoding="utf-8", errors="replace")
        if "GNU GENERAL PUBLIC LICENSE" not in text or "Version 3" not in text:
            errors.append("LICENSE does not look like the GNU GPL version 3 text")

    if not notice_path.is_file():
        errors.append("NOTICE is missing")
    else:
        text = notice_path.read_text(encoding="utf-8", errors="replace")
        for needle in ("kitty", "Kovid Goyal", "GPL-3.0-or-later"):
            if needle not in text:
                errors.append(f"NOTICE does not mention {needle!r}")

    if not third_party.is_file():
        errors.append("THIRD_PARTY_NOTICES.md is missing")


def verify_sums(manifest_dir: Path, errors: list[str]) -> int:
    sums = manifest_dir / "SHA256SUMS"
    if not sums.is_file():
        errors.append(f"{rel(manifest_dir, sums)} is missing")
        return 0
    checked = 0
    for lineno, line in enumerate(sums.read_text(encoding="utf-8").splitlines(), 1):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split(None, 1)
        if len(parts) != 2:
            errors.append(f"{rel(manifest_dir, sums)}:{lineno}: malformed line")
            continue
        digest, name = parts
        name = name.lstrip("*")
        path = manifest_dir / name
        if not path.is_file():
            errors.append(f"{rel(manifest_dir, sums)}:{lineno}: missing file {name}")
            continue
        actual = hashlib.sha256(path.read_bytes()).hexdigest()
        if actual.lower() != digest.lower():
            errors.append(f"{rel(manifest_dir, sums)}:{lineno}: hash mismatch for {name}")
        checked += 1
    return checked


def check_bundles(root: Path, errors: list[str]) -> int:
    checked = 0
    for directory, needle in (
        (root / "packaging/shell-integration", "GPL-3.0-or-later"),
        (root / "packaging/terminfo", "GPL-3.0-or-later"),
    ):
        notice = directory / "NOTICE"
        if not notice.is_file():
            errors.append(f"{rel(root, notice)} is missing")
        else:
            text = notice.read_text(encoding="utf-8", errors="replace")
            if needle not in text or "kitty" not in text:
                errors.append(f"{rel(root, notice)} lacks GPL/kitty attribution")
        if not (directory / "MANIFEST.md").is_file():
            errors.append(f"{rel(root, directory / 'MANIFEST.md')} is missing")
        checked += verify_sums(directory, errors)
    return checked


def check_executables(root: Path, errors: list[str]) -> None:
    required = [
        root / "scripts/screenshot_parity.py",
        root / "scripts/check_licenses.py",
        root / "packaging/bin/kitr",
    ]
    for path in required:
        if not path.is_file():
            errors.append(f"{rel(root, path)} is missing")
        elif not os.access(path, os.X_OK):
            errors.append(f"{rel(root, path)} is not executable")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument("--quiet", action="store_true")
    args = parser.parse_args(argv)

    root = args.root.resolve()
    if not (root / "Cargo.toml").is_file():
        print(f"error: {root} does not look like the kitr repository", file=sys.stderr)
        return 2

    errors: list[str] = []
    sources = check_spdx(root, errors)
    check_required_notices(root, errors)
    bundled = check_bundles(root, errors)
    check_executables(root, errors)

    if errors:
        for error in errors:
            print(f"license-check: {error}", file=sys.stderr)
        print(f"license-check: FAILED ({len(errors)} problem(s))", file=sys.stderr)
        return 1
    if not args.quiet:
        print(f"license-check: OK ({sources} source files, {bundled} bundled files)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
