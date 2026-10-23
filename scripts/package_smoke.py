#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Offline smoke checks for the packaged install tree and distro metadata."""

from __future__ import annotations

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def require(path: Path, needle: str | None = None) -> str:
    if not path.is_file():
        raise SystemExit(f"package-smoke: missing {path.relative_to(ROOT)}")
    text = path.read_text(encoding="utf-8")
    if needle is not None and needle not in text:
        raise SystemExit(
            f"package-smoke: {path.relative_to(ROOT)} does not contain {needle!r}"
        )
    return text


def check_stage(stage: Path) -> None:
    expected = [
        "usr/bin/kitr",
        "usr/libexec/kitr/kitr",
        "usr/share/terminfo/x/xterm-kitty",
        "usr/share/terminfo/78/xterm-kitty",
        "usr/share/applications/kitr.desktop",
        "usr/share/metainfo/io.github.portlandhodl.kitr.metainfo.xml",
        "usr/share/man/man1/kitr.1",
    ]
    for rel in expected:
        path = stage / rel
        if not path.exists():
            raise SystemExit(f"package-smoke: staged install is missing {rel}")
    launcher = stage / "usr/bin/kitr"
    if not launcher.read_text(encoding="utf-8").startswith("#!/bin/sh"):
        raise SystemExit("package-smoke: launcher is not a POSIX sh script")
    binary = stage / "usr/libexec/kitr/kitr"
    if not binary.stat().st_mode & 0o111:
        raise SystemExit("package-smoke: staged binary is not executable")


def main() -> int:
    if len(sys.argv) != 2:
        raise SystemExit("usage: package_smoke.py STAGE_DIR")
    stage = Path(sys.argv[1])
    check_stage(stage)

    control = require(ROOT / "packaging/debian/control")
    for field in ("Source: kitr", "Package: kitr", "Architecture: any"):
        if field not in control:
            raise SystemExit(f"package-smoke: deb control is missing {field!r}")

    spec = require(ROOT / "packaging/rpm/kitr.spec")
    for field in ("Name:", "Version:", "Release:", "License:", "%install", "%files"):
        if field not in spec:
            raise SystemExit(f"package-smoke: RPM spec is missing {field!r}")

    manifest = require(ROOT / "packaging/flatpak/io.github.portlandhodl.kitr.yml")
    for field in ("app-id:", "runtime:", "command:", "modules:"):
        if field not in manifest:
            raise SystemExit(f"package-smoke: Flatpak manifest is missing {field!r}")

    print("package-smoke: staged install, deb control, RPM spec and Flatpak manifest OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
