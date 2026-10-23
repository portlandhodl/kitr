#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Pixel comparison between C kitty and kitr screenshots.

Both terminals run the same shell command (or kitty session file) under Xvfb.
C kitty is asked for a PNG screenshot through its remote-control ``screenshot``
command; kitr writes a binary PPM with ``--screenshot``.  ImageMagick decodes
and normalises both images to PPM, then a small pure-Python comparator reports
exact (any channel difference), tolerant (per-channel delta) and aligned
metrics and writes diff images.

kitr reserves a tab-bar strip only when kitty's visibility rules show one
(hidden style and the single-tab autohide case reserve zero pixels).  The
comparator still searches for the small integer (dx, dy) offset with the lowest
mean error and reports both raw and aligned numbers, because C kitty's
screenshot includes a 1px right/bottom border and some startup paths can leave
the two terminals one row apart.  The raw numbers include any such chrome gap;
the aligned numbers are the useful behavioural signal.  Font rasterisation is
never bit-identical between two FreeType pipelines, so the tolerant metrics are
the pass/fail criterion.

Besides a one-off --command/--script, a corpus directory of ``*.sh`` scripts and
``*.session`` files is supported.  Optional ``<name>.json`` sidecars can set
``size``, ``tab_bar_style``, ``kitty_args``, ``kitr_args``, ``skip``, ``tags``,
``threshold``, ``settle`` and ``timeout``.

Exit codes: 0 all cases pass (skips allowed), 1 threshold exceeded, 2 error,
3 missing prerequisite (unless --strict is given, in which case missing
prerequisites return 2).
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, field
from pathlib import Path

PASS = 0
DIFF = 1
ERROR = 2
SKIP = 3

DEFAULT_SCRIPT = r"printf '\033[2J\033[Hkitr screenshot parity\nsecond line\n'; sleep 30"
DEFAULT_FONT_FAMILY = "DejaVu Sans Mono"
DEFAULT_FONT_SIZE = 12.0


class ParityError(RuntimeError):
    pass


class MissingPrerequisite(ParityError):
    pass


@dataclass
class Ppm:
    width: int
    height: int
    pixels: bytes


@dataclass
class Case:
    name: str
    mode: str
    script: str | None = None
    session: Path | None = None
    source: Path | None = None
    size: tuple[int, int] = (80, 24)
    tab_bar_style: str = "hidden"
    kitty_args: list[str] = field(default_factory=list)
    kitr_args: list[str] = field(default_factory=list)
    tags: list[str] = field(default_factory=list)
    skip: str | None = None
    threshold: float | None = None
    settle: float | None = None
    timeout: float | None = None


def _ppm_token(data: bytes, offset: int) -> tuple[bytes, int]:
    length = len(data)
    while offset < length:
        byte = data[offset]
        if byte in b" \t\r\n":
            offset += 1
            continue
        if byte == ord("#"):
            end = data.find(b"\n", offset)
            offset = length if end < 0 else end + 1
            continue
        break
    start = offset
    while offset < length and data[offset] not in b" \t\r\n":
        offset += 1
    if start == offset:
        raise ParityError("truncated PPM header")
    return data[start:offset], offset


def read_ppm(path: Path) -> Ppm:
    data = path.read_bytes()
    magic, offset = _ppm_token(data, 0)
    if magic != b"P6":
        raise ParityError(f"{path}: expected binary PPM (P6), got {magic!r}")
    width_raw, offset = _ppm_token(data, offset)
    height_raw, offset = _ppm_token(data, offset)
    maxval_raw, offset = _ppm_token(data, offset)
    try:
        width = int(width_raw)
        height = int(height_raw)
        maxval = int(maxval_raw)
    except ValueError as e:
        raise ParityError(f"{path}: invalid PPM header") from e
    if width <= 0 or height <= 0 or maxval != 255:
        raise ParityError(f"{path}: unsupported PPM dimensions/maxval ({width}x{height}, {maxval})")
    if offset < len(data) and data[offset] in b" \t\r\n":
        offset += 1
    expected = width * height * 3
    pixels = data[offset : offset + expected]
    if len(pixels) != expected:
        raise ParityError(f"{path}: PPM data is short ({len(pixels)} of {expected} bytes)")
    return Ppm(width, height, pixels)


def write_ppm(path: Path, width: int, height: int, pixels: bytes) -> None:
    with path.open("wb") as f:
        f.write(f"P6\n{width} {height}\n255\n".encode("ascii"))
        f.write(pixels)


def find_convert(explicit: str | None) -> tuple[str, str] | None:
    if explicit:
        path = shutil.which(explicit) or (explicit if Path(explicit).is_file() else None)
        if not path:
            return None
        return Path(path).name, path
    for name in ("magick", "convert"):
        path = shutil.which(name)
        if path:
            return name, path
    return None


def convert_to_ppm(tool: tuple[str, str], src: Path, dst: Path, log: Path) -> None:
    name, binary = tool
    cmd = [
        binary,
        str(src),
        "-background",
        "black",
        "-alpha",
        "remove",
        "-alpha",
        "off",
        "-depth",
        "8",
        f"ppm:{dst}",
    ]
    with log.open("ab") as f:
        f.write(("$ " + " ".join(cmd) + "\n").encode())
        f.flush()
        result = subprocess.run(cmd, stdout=f, stderr=subprocess.STDOUT)
    if result.returncode != 0 or not dst.is_file():
        raise ParityError(f"{name} failed to convert {src} to PPM (see {log})")


def _display_ready(display: str, xdpyinfo: str | None) -> bool:
    socket = "/tmp/.X11-unix/X" + display.lstrip(":").split(".")[0]
    if not Path(socket).exists():
        return False
    if not xdpyinfo:
        return True
    result = subprocess.run(
        [xdpyinfo, "-display", display],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    return result.returncode == 0


class VirtualDisplay:
    def __init__(self, name: str, process: subprocess.Popen[bytes] | None = None):
        self.name = name
        self.process = process

    @classmethod
    def start(cls, xvfb: str, geometry: str, log: Path) -> "VirtualDisplay":
        xdpyinfo = shutil.which("xdpyinfo")
        # Stale lock/socket files accumulate across parallel runs, so scan a
        # wide range and try several genuinely free display numbers. (The old
        # narrow pid-derived range could be almost entirely occupied.)
        attempts = 0
        for number in range(100, 20000):
            if attempts >= 64:
                break
            name = f":{number}"
            lock = Path(f"/tmp/.X{number}-lock")
            socket = Path(f"/tmp/.X11-unix/X{number}")
            if lock.exists() or socket.exists():
                continue
            attempts += 1
            f = log.open("ab")
            f.write(f"$ {xvfb} {name} -screen 0 {geometry} -nolisten tcp -ac\n".encode())
            f.flush()
            process = subprocess.Popen(
                [xvfb, name, "-screen", "0", geometry, "-nolisten", "tcp", "-ac"],
                stdin=subprocess.DEVNULL,
                stdout=f,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
            deadline = time.monotonic() + 8
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    break
                if _display_ready(name, xdpyinfo):
                    time.sleep(0.15)
                    f.close()
                    return cls(name, process)
                time.sleep(0.05)
            terminate_process(process)
            f.close()
        raise MissingPrerequisite("could not start Xvfb; install xvfb or provide DISPLAY")

    def stop(self) -> None:
        if self.process is not None:
            terminate_process(self.process)
            self.process = None


def terminate_process(process: subprocess.Popen[bytes], timeout: float = 5.0) -> None:
    if process.poll() is not None:
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except OSError:
        try:
            process.terminate()
        except OSError:
            return
    try:
        process.wait(timeout=timeout)
        return
    except subprocess.TimeoutExpired:
        pass
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except OSError:
        try:
            process.kill()
        except OSError:
            return
    try:
        process.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        pass


def wait_for_path(path: Path, process: subprocess.Popen[bytes], timeout: float, log: Path) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if path.exists():
            return
        if process.poll() is not None:
            raise ParityError(f"process exited before {path} appeared (see {log})")
        time.sleep(0.05)
    raise ParityError(f"timed out waiting for {path} (see {log})")


def kitty_options(case: Case, font_family: str, font_size: float) -> list[str]:
    cols, rows = case.size
    return [
        "-o",
        f"font_family={font_family}",
        "-o",
        f"font_size={font_size:g}",
        "-o",
        f"tab_bar_style={case.tab_bar_style}",
        "-o",
        "remember_window_size=no",
        "-o",
        f"initial_window_width={cols}c",
        "-o",
        f"initial_window_height={rows}c",
        "-o",
        "window_padding_width=0",
        "-o",
        "background_opacity=1",
        "-o",
        "hide_window_decorations=yes",
        "-o",
        "cursor_blink_interval=0",
        "-o",
        "cursor_shape=block",
    ]


def write_kitr_config(path: Path, case: Case, font_family: str, font_size: float) -> None:
    path.write_text(
        "\n".join(
            [
                f"font_family {font_family}",
                f"font_size {font_size:g}",
                f"tab_bar_style {case.tab_bar_style}",
                "window_padding_width 0",
                "background_opacity 1",
                "cursor_blink_interval 0",
                "cursor_shape block",
                "",
            ]
        ),
        encoding="utf-8",
    )


def run_kitty(
    kitty: str,
    kitten: str,
    case: Case,
    settle: float,
    timeout: float,
    artifacts: Path,
    font_family: str,
    font_size: float,
    env: dict[str, str],
) -> Path:
    out = artifacts / "kitty.png"
    sock = Path(tempfile.mkdtemp(prefix="kitr-parity-")) / "rc.sock"
    log = artifacts / "kitty.log"
    rc_log = artifacts / "kitty-rc.log"
    opts = kitty_options(case, font_family, font_size) + case.kitty_args
    cmd = [kitty, "--listen-on", f"unix:{sock}", "-o", "allow_remote_control=socket-only", *opts]
    if case.mode == "session":
        assert case.session is not None
        cmd += ["--session", str(case.session.resolve())]
    else:
        cmd += ["--hold", "--", "/bin/sh", "-c", case.script or DEFAULT_SCRIPT]
    with log.open("ab") as f:
        f.write(("$ " + " ".join(cmd) + "\n").encode())
        f.flush()
        process = subprocess.Popen(
            cmd,
            stdin=subprocess.DEVNULL,
            stdout=f,
            stderr=subprocess.STDOUT,
            env=env,
            start_new_session=True,
        )
    try:
        wait_for_path(sock, process, timeout, log)
        time.sleep(max(0.0, settle))
        rc_cmd = [kitten, "@", "--to", f"unix:{sock}", "screenshot", str(out)]
        with rc_log.open("ab") as f:
            f.write(("$ " + " ".join(rc_cmd) + "\n").encode())
            f.flush()
            result = subprocess.run(
                rc_cmd,
                stdout=f,
                stderr=subprocess.STDOUT,
                timeout=max(10.0, timeout + 15.0),
                env=env,
            )
        if result.returncode != 0 or not out.is_file() or out.stat().st_size == 0:
            raise ParityError(f"C kitty remote screenshot failed (see {rc_log})")
    finally:
        terminate_process(process)
        try:
            sock.unlink()
        except OSError:
            pass
        try:
            sock.parent.rmdir()
        except OSError:
            pass
    return out


def run_kitr(
    kitr: str,
    case: Case,
    settle: float,
    timeout: float,
    artifacts: Path,
    font_family: str,
    font_size: float,
    env: dict[str, str],
) -> Path:
    out = artifacts / "kitr.ppm"
    log = artifacts / "kitr.log"
    config = artifacts / "kitr.conf"
    write_kitr_config(config, case, font_family, font_size)
    cols, rows = case.size
    run_timeout = max(timeout, settle + 2.0)
    cmd = [
        kitr,
        "--gui",
        "--screenshot",
        str(out),
        "--size",
        f"{cols}x{rows}",
        "--timeout",
        f"{run_timeout:g}",
        "--config",
        str(config),
        *case.kitr_args,
    ]
    if case.mode == "session":
        assert case.session is not None
        cmd += ["--session", str(case.session.resolve())]
    else:
        cmd += ["--", "/bin/sh", "-c", case.script or DEFAULT_SCRIPT]
    with log.open("ab") as f:
        f.write(("$ " + " ".join(cmd) + "\n").encode())
        f.flush()
        process = subprocess.Popen(
            cmd,
            stdin=subprocess.DEVNULL,
            stdout=f,
            stderr=subprocess.STDOUT,
            env=env,
            start_new_session=True,
        )
    try:
        deadline = time.monotonic() + run_timeout + 20.0
        while time.monotonic() < deadline:
            if out.is_file() and out.stat().st_size > 0:
                try:
                    process.wait(timeout=5.0)
                except subprocess.TimeoutExpired:
                    pass
                break
            if process.poll() is not None:
                break
            time.sleep(0.05)
    finally:
        terminate_process(process)
    if not out.is_file() or out.stat().st_size == 0:
        raise ParityError(f"kitr did not write a screenshot (see {log})")
    return out


def _metrics(
    a: Ppm,
    b: Ppm,
    dx: int,
    dy: int,
    pixel_threshold: int,
    diff_path: Path | None = None,
) -> dict[str, object]:
    x0 = max(0, -dx)
    y0 = max(0, -dy)
    x1 = min(a.width, b.width - dx)
    y1 = min(a.height, b.height - dy)
    if x1 <= x0 or y1 <= y0:
        raise ParityError("screenshots have no overlapping pixels")
    width = x1 - x0
    height = y1 - y0
    total = width * height
    diff_pixels = 0
    channel_delta_sum = 0
    diff_out = bytearray(width * height * 3) if diff_path is not None else None
    ap = a.pixels
    bp = b.pixels
    a_stride = a.width * 3
    b_stride = b.width * 3
    for y in range(y0, y1):
        a_row = y * a_stride
        b_row = (y + dy) * b_stride
        out_row = (y - y0) * width * 3
        for x in range(width):
            ai = a_row + (x + x0) * 3
            bi = b_row + (x + x0 + dx) * 3
            d0 = ap[ai] - bp[bi]
            d1 = ap[ai + 1] - bp[bi + 1]
            d2 = ap[ai + 2] - bp[bi + 2]
            if d0 < 0:
                d0 = -d0
            if d1 < 0:
                d1 = -d1
            if d2 < 0:
                d2 = -d2
            channel_delta_sum += d0 + d1 + d2
            delta = max(d0, d1, d2)
            oi = out_row + (x - x0) * 3
            if delta > pixel_threshold:
                diff_pixels += 1
                if diff_out is not None:
                    diff_out[oi] = 255
            elif diff_out is not None:
                grey = 24 + (ap[ai] + ap[ai + 1] + ap[ai + 2]) // 24
                grey = min(grey, 96)
                diff_out[oi] = grey
                diff_out[oi + 1] = grey
                diff_out[oi + 2] = grey
    if diff_out is not None and diff_path is not None:
        write_ppm(diff_path, width, height, bytes(diff_out))
    return {
        "width": width,
        "height": height,
        "offset_x": dx,
        "offset_y": dy,
        "compared_pixels": total,
        "different_pixels": diff_pixels,
        "diff_percent": (diff_pixels * 100.0) / total,
        "mean_channel_delta": channel_delta_sum / (total * 3),
    }


def _score(a: Ppm, b: Ppm, dx: int, dy: int, stride: int) -> float:
    x0 = max(0, -dx)
    y0 = max(0, -dy)
    x1 = min(a.width, b.width - dx)
    y1 = min(a.height, b.height - dy)
    if x1 <= x0 or y1 <= y0:
        return float("inf")
    ap = a.pixels
    bp = b.pixels
    a_stride = a.width * 3
    b_stride = b.width * 3
    total = 0
    delta_sum = 0
    for y in range(y0, y1, stride):
        a_row = y * a_stride
        b_row = (y + dy) * b_stride
        for x in range(x0, x1, stride):
            ai = a_row + x * 3
            bi = b_row + (x + dx) * 3
            delta_sum += (
                abs(ap[ai] - bp[bi])
                + abs(ap[ai + 1] - bp[bi + 1])
                + abs(ap[ai + 2] - bp[bi + 2])
            )
            total += 3
    return delta_sum / total if total else float("inf")


def find_offset(a: Ppm, b: Ppm, max_shift: int) -> tuple[int, int]:
    """Find the (dx, dy) that best aligns kitty pixels with kitr pixels."""
    best_score = float("inf")
    best = (0, 0)
    for dy in range(-max_shift, max_shift + 1):
        score = _score(a, b, 0, dy, 8)
        if score < best_score:
            best_score = score
            best = (0, dy)
    for dx in range(-4, 5):
        score = _score(a, b, dx, best[1], 4)
        if score < best_score:
            best_score = score
            best = (dx, best[1])
    base_dy = best[1]
    for dy in range(base_dy - 2, base_dy + 3):
        score = _score(a, b, best[0], dy, 2)
        if score < best_score:
            best_score = score
            best = (best[0], dy)
    return best


def compare(
    kitty: Ppm,
    kitr: Ppm,
    pixel_threshold: int,
    artifacts: Path,
    max_shift: int,
) -> dict[str, object]:
    dx, dy = find_offset(kitty, kitr, max_shift)
    exact_aligned = _metrics(kitty, kitr, dx, dy, 0, artifacts / "diff.ppm")
    tolerant_aligned = _metrics(kitty, kitr, dx, dy, pixel_threshold)
    exact_raw = _metrics(kitty, kitr, 0, 0, 0)
    tolerant_raw = _metrics(kitty, kitr, 0, 0, pixel_threshold)
    return {
        "offset": {"x": dx, "y": dy},
        "exact": {"aligned": exact_aligned, "raw": exact_raw},
        "tolerant": {"aligned": tolerant_aligned, "raw": tolerant_raw},
    }


def clean_case_artifacts(artifacts: Path) -> None:
    for name in (
        "kitty.png",
        "kitty.ppm",
        "kitr.ppm",
        "kitr-common.ppm",
        "diff.ppm",
        "diff.png",
        "report.json",
        "kitty.log",
        "kitty-rc.log",
        "kitr.log",
        "convert.log",
        "kitr.conf",
    ):
        try:
            (artifacts / name).unlink()
        except OSError:
            pass


def write_report(artifacts: Path, report: dict[str, object]) -> None:
    (artifacts / "report.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")


def skip(artifacts: Path, report: dict[str, object], message: str, strict: bool) -> int:
    report["status"] = "skipped"
    report["message"] = message
    write_report(artifacts, report)
    print(f"screenshot-parity: SKIP: {message}")
    return ERROR if strict else SKIP


def _read_sidecar(path: Path) -> dict[str, object]:
    try:
        raw = path.read_text(encoding="utf-8")
    except OSError:
        return {}
    try:
        value = json.loads(raw)
    except ValueError as e:
        raise ParityError(f"{path}: invalid JSON: {e}") from e
    if not isinstance(value, dict):
        raise ParityError(f"{path}: expected a JSON object")
    return value


def _case_from_sidecar(
    name: str,
    mode: str,
    source: Path,
    sidecar: dict[str, object],
    default_size: tuple[int, int],
) -> Case:
    size = default_size
    raw_size = sidecar.get("size")
    if raw_size is not None:
        try:
            cols, rows = str(raw_size).lower().split("x", 1)
            size = (int(cols), int(rows))
        except ValueError as e:
            raise ParityError(f"{name}: invalid size {raw_size!r}") from e
        if size[0] <= 0 or size[1] <= 0:
            raise ParityError(f"{name}: size must be positive")
    script = source.read_text(encoding="utf-8") if mode == "command" else None
    session = source if mode == "session" else None
    return Case(
        name=name,
        mode=mode,
        script=script,
        session=session,
        source=source,
        size=size,
        tab_bar_style=str(sidecar.get("tab_bar_style", "hidden")),
        kitty_args=[str(x) for x in sidecar.get("kitty_args", [])],
        kitr_args=[str(x) for x in sidecar.get("kitr_args", [])],
        tags=[str(x) for x in sidecar.get("tags", [])],
        skip=str(sidecar["skip"]) if sidecar.get("skip") else None,
        threshold=float(sidecar["threshold"]) if sidecar.get("threshold") is not None else None,
        settle=float(sidecar["settle"]) if sidecar.get("settle") is not None else None,
        timeout=float(sidecar["timeout"]) if sidecar.get("timeout") is not None else None,
    )


def load_corpus(path: Path, default_size: tuple[int, int]) -> list[Case]:
    if not path.is_dir():
        raise ParityError(f"corpus directory not found: {path}")
    cases: list[Case] = []
    seen: set[str] = set()
    for source in sorted(path.glob("*.sh")):
        if source.stem in seen:
            raise ParityError(f"duplicate case name {source.stem!r}")
        seen.add(source.stem)
        sidecar = _read_sidecar(path / f"{source.stem}.json")
        cases.append(_case_from_sidecar(source.stem, "command", source, sidecar, default_size))
    for source in sorted(path.glob("*.session")):
        if source.stem in seen:
            raise ParityError(f"duplicate case name {source.stem!r}")
        seen.add(source.stem)
        sidecar = _read_sidecar(path / f"{source.stem}.json")
        cases.append(_case_from_sidecar(source.stem, "session", source, sidecar, default_size))
    if not cases:
        raise ParityError(f"no .sh or .session cases found in {path}")
    return cases


def _fmt(value: object) -> str:
    return "n/a" if value is None else f"{float(value):.3f}%"


def _case_markdown(result: dict[str, object], markdown_dir: Path) -> str:
    name = str(result["name"])
    status = str(result["status"])
    if status in ("skipped", "error"):
        note = str(result.get("message") or result.get("skip") or "")
        return f"| {name} | {status} | - | - | - | - | - | {note} |"
    exact = result["exact"]  # type: ignore[index]
    tolerant = result["tolerant"]  # type: ignore[index]
    offset = result["offset"]  # type: ignore[index]
    aligned_tol = float(tolerant["aligned"]["diff_percent"])  # type: ignore[index]
    artifact_dir = Path(str(result.get("artifacts", "")))
    try:
        rel = artifact_dir.resolve().relative_to(markdown_dir.resolve())
    except ValueError:
        rel = artifact_dir
    link = f"[{artifact_dir.name}]({rel.as_posix()})"
    return (
        f"| {name} | {status} | "
        f"{_fmt(exact['aligned']['diff_percent'])} | {_fmt(aligned_tol)} | "  # type: ignore[index]
        f"{_fmt(exact['raw']['diff_percent'])} | {_fmt(tolerant['raw']['diff_percent'])} | "  # type: ignore[index]
        f"({offset['x']},{offset['y']}) | {link} |"  # type: ignore[index]
    )


def write_markdown(
    path: Path,
    suite: dict[str, object],
    command: str,
    gaps: str | None,
) -> None:
    cases = suite["cases"]  # type: ignore[index]
    counts: dict[str, int] = {}
    for case in cases:  # type: ignore[union-attr]
        status = str(case["status"])
        counts[status] = counts.get(status, 0) + 1
    lines = [
        "# kitr vs C kitty screenshot parity",
        "",
        f"Generated: {time.strftime('%Y-%m-%d %H:%M:%S %Z')}",
        "",
        "## Environment",
        "",
        f"- C kitty: `{suite['c_kitty']}`",
        f"- kitten: `{suite['kitten']}`",
        f"- kitr: `{suite['kitr_binary']}`",
        f"- Python: `{sys.version.split()[0]}`",
        f"- display: `{suite.get('display', 'unavailable')}` (Xvfb unless `--display` was used)",
        f"- font: `{suite['font_family']}` at `{suite['font_size']}pt`",
        f"- kitr startup settle: `{suite['settle']}s`",
        "",
        "## Command",
        "",
        "```sh",
        command,
        "```",
        "",
        "`exact` counts any non-zero channel difference. `tolerant` ignores "
        "per-channel differences up to the pixel threshold. `aligned` uses the "
        "best integer offset (shown as `(dx,dy)`); `raw` compares at `(0,0)` and "
        "therefore includes tab-bar/border chrome differences. Pass/fail uses "
        "aligned tolerant percentage.",
        "",
        "## Results",
        "",
        "| Case | Status | Exact aligned | Tolerant aligned | Exact raw | Tolerant raw | Offset | Artifacts |",
        "| --- | --- | ---: | ---: | ---: | ---: | ---: | --- |",
    ]
    for case in cases:  # type: ignore[union-attr]
        lines.append(_case_markdown(case, path.parent))
    lines += [
        "",
        f"Summary: {counts.get('pass', 0)} pass, {counts.get('fail', 0)} fail, "
        f"{counts.get('skipped', 0)} skipped, {counts.get('error', 0)} error.",
        "",
        "Artifacts for each case contain `kitty.png`, `kitr.ppm`, normalized "
        "`kitty.ppm`, `diff.ppm`, `diff.png`, logs and the per-case `report.json`.",
        "",
    ]
    if gaps:
        lines += ["## Reproduction notes and known visual gaps", "", gaps.rstrip(), ""]
    path.write_text("\n".join(lines), encoding="utf-8")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="C kitty vs kitr screenshot comparison and corpus runner",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    source = parser.add_mutually_exclusive_group()
    source.add_argument("--command", help="shell snippet run in both terminals")
    source.add_argument("--script", type=Path, help="file containing the shell snippet")
    source.add_argument("--corpus", type=Path, help="directory of .sh/.session parity cases")
    parser.add_argument("--case", action="append", default=[], help="only run matching corpus case (repeatable)")
    parser.add_argument("--kitty", default=None, help="C kitty binary")
    parser.add_argument("--kitten", default=None, help="C kitty 'kitten' binary")
    parser.add_argument("--kitr", default="rust/target/release/kitr", help="kitr binary")
    parser.add_argument("--size", default="80x24", help="default terminal size COLSxROWS")
    parser.add_argument("--font-family", default=DEFAULT_FONT_FAMILY, help="font family used by both terminals")
    parser.add_argument("--font-size", type=float, default=DEFAULT_FONT_SIZE, help="font size in points")
    parser.add_argument("--timeout", type=float, default=10.0, help="startup/run timeout in seconds")
    parser.add_argument("--settle", type=float, default=1.5, help="capture delay after startup")
    parser.add_argument("--threshold", type=float, default=8.0, help="maximum tolerated aligned diff percentage")
    parser.add_argument("--pixel-threshold", type=int, default=16, help="per-channel delta treated as different")
    parser.add_argument("--max-shift", type=int, default=48, help="maximum vertical alignment search in pixels")
    parser.add_argument("--artifacts", type=Path, default=Path("artifacts/screenshot-parity"))
    parser.add_argument("--markdown", type=Path, default=None, help="write a markdown report here")
    parser.add_argument("--json", type=Path, default=None, help="write the suite JSON report here")
    parser.add_argument("--gaps", type=Path, default=None, help="markdown file appended as known visual gaps")
    parser.add_argument("--xvfb", default="Xvfb", help="Xvfb executable")
    parser.add_argument("--convert", default=None, help="ImageMagick convert/magick executable")
    parser.add_argument("--display", default=None, help="use this DISPLAY instead of starting Xvfb")
    parser.add_argument("--kitty-arg", action="append", default=[], help="extra C kitty argument (repeatable)")
    parser.add_argument("--kitr-arg", action="append", default=[], help="extra kitr argument (repeatable)")
    parser.add_argument("--strict", action="store_true", help="treat missing prerequisites as errors")
    args = parser.parse_args(argv)

    if args.threshold < 0:
        parser.error("--threshold must be >= 0")
    if args.pixel_threshold < 0 or args.pixel_threshold > 255:
        parser.error("--pixel-threshold must be between 0 and 255")
    if args.max_shift < 0:
        parser.error("--max-shift must be >= 0")
    if args.font_size <= 0:
        parser.error("--font-size must be > 0")
    try:
        cols_raw, rows_raw = args.size.lower().split("x", 1)
        default_size = (int(cols_raw), int(rows_raw))
        if default_size[0] <= 0 or default_size[1] <= 0:
            raise ValueError
    except ValueError:
        parser.error("--size must be COLSxROWS with positive integers")

    try:
        if args.corpus is not None:
            cases = load_corpus(args.corpus, default_size)
        else:
            if args.script is not None:
                try:
                    script = args.script.read_text(encoding="utf-8")
                except OSError as e:
                    print(f"screenshot-parity: error: cannot read script: {e}", file=sys.stderr)
                    return ERROR
                name = args.script.stem
            else:
                script = args.command or DEFAULT_SCRIPT
                name = "command"
            if not script.strip():
                print("screenshot-parity: error: empty command/script", file=sys.stderr)
                return ERROR
            cases = [Case(name=name, mode="command", script=script, size=default_size)]
    except ParityError as e:
        print(f"screenshot-parity: error: {e}", file=sys.stderr)
        return ERROR

    if args.case:
        wanted = set(args.case)
        cases = [c for c in cases if c.name in wanted]
        if not cases:
            print(f"screenshot-parity: error: no case matched {sorted(wanted)}", file=sys.stderr)
            return ERROR

    if args.kitty_arg:
        for case in cases:
            case.kitty_args.extend(args.kitty_arg)
    if args.kitr_arg:
        for case in cases:
            case.kitr_args.extend(args.kitr_arg)

    artifacts = args.artifacts.resolve()
    artifacts.mkdir(parents=True, exist_ok=True)

    kitty = shutil.which(args.kitty) if args.kitty else shutil.which("kitty")
    kitten = shutil.which(args.kitten) if args.kitten else shutil.which("kitten")
    kitr = Path(args.kitr).resolve()
    convert = find_convert(args.convert)

    missing: list[str] = []
    if not kitty:
        missing.append("C kitty binary (set --kitty or install kitty)")
    if not kitten:
        missing.append("C kitty 'kitten' binary (set --kitten)")
    if not kitr.is_file() or not os.access(kitr, os.X_OK):
        missing.append(f"kitr binary at {kitr} (run 'make build' or pass --kitr)")
    if convert is None:
        missing.append("ImageMagick convert/magick (set --convert)")
    if missing:
        report = {"status": "skipped", "message": "; ".join(missing), "artifacts": str(artifacts)}
        write_report(artifacts, report)
        print(f"screenshot-parity: SKIP: {'; '.join(missing)}")
        return ERROR if args.strict else SKIP

    command = " ".join([Path(sys.argv[0]).as_posix(), *sys.argv[1:]])
    suite: dict[str, object] = {
        "status": "running",
        "command": command,
        "c_kitty": kitty,
        "kitten": kitten,
        "kitr_binary": str(kitr),
        "convert": convert[0],
        "font_family": args.font_family,
        "font_size": args.font_size,
        "settle": args.settle,
        "pixel_channel_threshold": args.pixel_threshold,
        "threshold_percent": args.threshold,
        "artifacts": str(artifacts),
        "cases": [],
    }
    gaps_text = None
    if args.gaps is not None:
        try:
            gaps_text = args.gaps.read_text(encoding="utf-8")
        except OSError as e:
            print(f"screenshot-parity: warning: cannot read gaps file: {e}", file=sys.stderr)

    display: VirtualDisplay | None = None
    exit_code = PASS
    try:
        if args.display:
            display = VirtualDisplay(args.display)
        elif os.environ.get("DISPLAY"):
            display = VirtualDisplay(os.environ["DISPLAY"])
        else:
            xvfb = shutil.which(args.xvfb) or (args.xvfb if Path(args.xvfb).is_file() else None)
            if not xvfb:
                raise MissingPrerequisite("no DISPLAY and Xvfb not found")
            display = VirtualDisplay.start(xvfb, "1280x800x24", artifacts / "display.log")
        suite["display"] = display.name
        env = dict(os.environ)
        env["DISPLAY"] = display.name
        env.setdefault("XLIB_SKIP_ARGB_VISUALS", "1")
        env.setdefault("LIBGL_ALWAYS_SOFTWARE", "1")
        env.setdefault("MESA_LOADER_DRIVER_OVERRIDE", "llvmpipe")

        case_results: list[dict[str, object]] = []
        for case in cases:
            case_dir = artifacts / "cases" / case.name
            case_dir.mkdir(parents=True, exist_ok=True)
            clean_case_artifacts(case_dir)
            print(f"screenshot-parity: case {case.name} (C kitty)")
            if case.skip:
                result = {
                    "name": case.name,
                    "status": "skipped",
                    "message": case.skip,
                    "artifacts": str(case_dir),
                }
                case_results.append(result)
                write_report(case_dir, result)
                print(f"screenshot-parity: {case.name}: SKIP: {case.skip}")
                continue
            case_settle = args.settle if case.settle is None else case.settle
            case_timeout = args.timeout if case.timeout is None else case.timeout
            case_threshold = args.threshold if case.threshold is None else case.threshold
            try:
                kitty_png = run_kitty(
                    kitty,
                    kitten,
                    case,
                    case_settle,
                    case_timeout,
                    case_dir,
                    args.font_family,
                    args.font_size,
                    env,
                )
                print(f"screenshot-parity: case {case.name} (kitr)")
                kitr_ppm = run_kitr(
                    str(kitr),
                    case,
                    case_settle,
                    case_timeout,
                    case_dir,
                    args.font_family,
                    args.font_size,
                    env,
                )
                kitty_ppm = case_dir / "kitty.ppm"
                kitr_common = case_dir / "kitr-common.ppm"
                convert_to_ppm(convert, kitty_png, kitty_ppm, case_dir / "convert.log")
                convert_to_ppm(convert, kitr_ppm, kitr_common, case_dir / "convert.log")
                kitty_image = read_ppm(kitty_ppm)
                kitr_image = read_ppm(kitr_common)
                result = compare(kitty_image, kitr_image, args.pixel_threshold, case_dir, args.max_shift)
                result.update(
                    {
                        "name": case.name,
                        "mode": case.mode,
                        "size": f"{case.size[0]}x{case.size[1]}",
                        "tags": case.tags,
                        "threshold_percent": case_threshold,
                        "pixel_channel_threshold": args.pixel_threshold,
                        "dimensions": {
                            "kitty": [kitty_image.width, kitty_image.height],
                            "kitr": [kitr_image.width, kitr_image.height],
                        },
                        "artifacts": str(case_dir),
                    }
                )
                diff_png = case_dir / "diff.png"
                with (case_dir / "convert.log").open("ab") as f:
                    subprocess.run(
                        [convert[1], str(case_dir / "diff.ppm"), str(diff_png)],
                        stdout=f,
                        stderr=subprocess.STDOUT,
                    )
                if diff_png.is_file():
                    result["diff_png"] = str(diff_png)
                tolerant = result["tolerant"]  # type: ignore[index]
                aligned = float(tolerant["aligned"]["diff_percent"])  # type: ignore[index]
                status = "pass" if aligned <= case_threshold else "fail"
                result["status"] = status
                case_results.append(result)
                write_report(case_dir, result)
                print(
                    f"screenshot-parity: {case.name}: {status.upper()}: aligned "
                    f"exact {_fmt(result['exact']['aligned']['diff_percent'])} "  # type: ignore[index]
                    f"tolerant {_fmt(aligned)} (threshold {case_threshold:.3f}%), "
                    f"raw tolerant {_fmt(result['tolerant']['raw']['diff_percent'])}, "  # type: ignore[index]
                    f"offset {result['offset']}"  # type: ignore[index]
                )
                if status == "fail" and exit_code == PASS:
                    exit_code = DIFF
            except (ParityError, OSError, subprocess.SubprocessError) as e:
                result = {
                    "name": case.name,
                    "mode": case.mode,
                    "status": "error",
                    "message": str(e),
                    "artifacts": str(case_dir),
                }
                case_results.append(result)
                write_report(case_dir, result)
                print(f"screenshot-parity: {case.name}: ERROR: {e}", file=sys.stderr)
                exit_code = ERROR

        suite["cases"] = case_results
        statuses = [str(c["status"]) for c in case_results]
        if "error" in statuses:
            suite["status"] = "error"
            exit_code = ERROR
        elif "fail" in statuses:
            suite["status"] = "fail"
            exit_code = DIFF
        else:
            suite["status"] = "pass"
            exit_code = PASS
        write_report(artifacts, suite)
        if args.json is not None:
            args.json.parent.mkdir(parents=True, exist_ok=True)
            args.json.write_text(json.dumps(suite, indent=2) + "\n", encoding="utf-8")
        if args.markdown is not None:
            args.markdown.parent.mkdir(parents=True, exist_ok=True)
            write_markdown(args.markdown, suite, command, gaps_text)
        counts: dict[str, int] = {}
        for status in statuses:
            counts[status] = counts.get(status, 0) + 1
        print(
            "screenshot-parity: {status}: {passn} pass, {failn} fail, "
            "{skipn} skipped, {errn} error. Artifacts in {artifacts}".format(
                status=str(suite["status"]).upper(),
                passn=counts.get("pass", 0),
                failn=counts.get("fail", 0),
                skipn=counts.get("skipped", 0),
                errn=counts.get("error", 0),
                artifacts=artifacts,
            )
        )
        return exit_code
    except MissingPrerequisite as e:
        suite["status"] = "skipped"
        suite["message"] = str(e)
        write_report(artifacts, suite)
        if args.markdown is not None:
            args.markdown.parent.mkdir(parents=True, exist_ok=True)
            write_markdown(args.markdown, suite, command, gaps_text)
        print(f"screenshot-parity: SKIP: {e}")
        return ERROR if args.strict else SKIP
    except (ParityError, OSError, subprocess.SubprocessError) as e:
        suite["status"] = "error"
        suite["message"] = str(e)
        write_report(artifacts, suite)
        print(f"screenshot-parity: ERROR: {e}", file=sys.stderr)
        return ERROR
    finally:
        if display is not None:
            display.stop()


if __name__ == "__main__":
    raise SystemExit(main())
