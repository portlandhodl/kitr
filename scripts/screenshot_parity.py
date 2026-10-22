#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Best-effort pixel comparison between C kitty and kitr screenshots.

Both terminals run the same shell command under Xvfb. C kitty is asked for a
PNG screenshot through its remote-control ``screenshot`` command; kitr writes a
binary PPM with ``--screenshot``. ImageMagick is used only to decode/normalise
both images to PPM, then a small pure-Python comparator reports the percentage
of differing pixels and writes artifacts.

This is a smoke test. Fonts, font sizes, DPI, window decorations, toolkit
rasterisation and kitty's tab bar can all change the pixels even when terminal
behaviour is identical. Treat failures as an investigation prompt, not proof
of a behavioural regression.

Exit codes: 0 pass/skip, 1 threshold exceeded, 2 error, 3 missing prerequisite
(unless --strict is given, in which case missing prerequisites return 2).
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
from dataclasses import dataclass
from pathlib import Path

PASS = 0
DIFF = 1
ERROR = 2
SKIP = 3

DEFAULT_SCRIPT = r"printf '\033[2J\033[Hkitr screenshot parity\nsecond line\n'"


class ParityError(RuntimeError):
    pass


class MissingPrerequisite(ParityError):
    pass


@dataclass
class Ppm:
    width: int
    height: int
    pixels: bytes


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
        for attempt in range(64):
            number = 200 + (os.getpid() + attempt) % 700
            name = f":{number}"
            lock = Path(f"/tmp/.X{number}-lock")
            socket = Path(f"/tmp/.X11-unix/X{number}")
            if lock.exists() or socket.exists():
                continue
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


def run_kitty(
    kitty: str,
    kitten: str,
    script: str,
    size: tuple[int, int],
    settle: float,
    timeout: float,
    artifacts: Path,
    extra: list[str],
    env: dict[str, str],
) -> Path:
    out = artifacts / "kitty.png"
    sock = Path(tempfile.mkdtemp(prefix="kitr-parity-")) / "rc.sock"
    log = artifacts / "kitty.log"
    rc_log = artifacts / "kitty-rc.log"
    cols, rows = size
    cmd = [
        kitty,
        "--listen-on",
        f"unix:{sock}",
        "-o",
        "allow_remote_control=socket-only",
        "-o",
        "tab_bar_style=hidden",
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
        *extra,
        "--hold",
        "--",
        "/bin/sh",
        "-c",
        script,
    ]
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
    script: str,
    size: tuple[int, int],
    settle: float,
    timeout: float,
    artifacts: Path,
    extra: list[str],
    env: dict[str, str],
) -> Path:
    out = artifacts / "kitr.ppm"
    log = artifacts / "kitr.log"
    cols, rows = size
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
        *extra,
        "--",
        "/bin/sh",
        "-c",
        script,
    ]
    with log.open("ab") as f:
        f.write(("$ " + " ".join(cmd) + "\n").encode())
        f.flush()
        result = subprocess.run(
            cmd,
            stdin=subprocess.DEVNULL,
            stdout=f,
            stderr=subprocess.STDOUT,
            timeout=run_timeout + 20.0,
            env=env,
        )
    if not out.is_file() or out.stat().st_size == 0:
        raise ParityError(f"kitr did not write a screenshot (exit {result.returncode}, see {log})")
    return out


def compare(
    kitty: Ppm,
    kitr: Ppm,
    pixel_threshold: int,
    artifacts: Path,
) -> dict[str, object]:
    width = min(kitty.width, kitr.width)
    height = min(kitty.height, kitr.height)
    if width == 0 or height == 0:
        raise ParityError("screenshots have no overlapping pixels")
    total = width * height
    diff_pixels = 0
    channel_delta_sum = 0
    diff_pixels_out = bytearray(width * height * 3)
    for y in range(height):
        k_row = y * kitty.width * 3
        r_row = y * kitr.width * 3
        for x in range(width):
            k = k_row + x * 3
            r = r_row + x * 3
            dr = abs(kitty.pixels[k] - kitr.pixels[r])
            dg = abs(kitty.pixels[k + 1] - kitr.pixels[r + 1])
            db = abs(kitty.pixels[k + 2] - kitr.pixels[r + 2])
            delta = max(dr, dg, db)
            channel_delta_sum += dr + dg + db
            o = (y * width + x) * 3
            if delta > pixel_threshold:
                diff_pixels += 1
                diff_pixels_out[o] = 255
                diff_pixels_out[o + 1] = 0
                diff_pixels_out[o + 2] = 0
            else:
                grey = 24 + (kitty.pixels[k] + kitty.pixels[k + 1] + kitty.pixels[k + 2]) // 24
                grey = min(grey, 96)
                diff_pixels_out[o] = grey
                diff_pixels_out[o + 1] = grey
                diff_pixels_out[o + 2] = grey
    write_ppm(artifacts / "diff.ppm", width, height, bytes(diff_pixels_out))
    return {
        "width": width,
        "height": height,
        "compared_pixels": total,
        "different_pixels": diff_pixels,
        "diff_percent": (diff_pixels * 100.0) / total,
        "mean_channel_delta": channel_delta_sum / (total * 3),
    }


def write_report(artifacts: Path, report: dict[str, object]) -> None:
    (artifacts / "report.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")


def skip(artifacts: Path, report: dict[str, object], message: str, strict: bool) -> int:
    report["status"] = "skipped"
    report["message"] = message
    write_report(artifacts, report)
    print(f"screenshot-parity: SKIP: {message}")
    return ERROR if strict else SKIP


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Best-effort C kitty vs kitr screenshot comparison",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    source = parser.add_mutually_exclusive_group()
    source.add_argument("--command", help="shell snippet run in both terminals")
    source.add_argument("--script", type=Path, help="file containing the shell snippet")
    parser.add_argument("--kitty", default=None, help="C kitty binary")
    parser.add_argument("--kitten", default=None, help="C kitty 'kitten' binary")
    parser.add_argument("--kitr", default="target/release/kitr", help="kitr binary")
    parser.add_argument("--size", default="80x24", help="terminal size COLSxROWS")
    parser.add_argument("--timeout", type=float, default=10.0, help="startup/run timeout in seconds")
    parser.add_argument("--settle", type=float, default=1.5, help="capture delay after startup")
    parser.add_argument("--threshold", type=float, default=5.0, help="maximum differing-pixel percentage")
    parser.add_argument("--pixel-threshold", type=int, default=16, help="per-channel delta treated as different")
    parser.add_argument("--artifacts", type=Path, default=Path("artifacts/screenshot-parity"))
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
    try:
        cols_raw, rows_raw = args.size.lower().split("x", 1)
        cols, rows = int(cols_raw), int(rows_raw)
        if cols <= 0 or rows <= 0:
            raise ValueError
    except ValueError:
        parser.error("--size must be COLSxROWS with positive integers")

    if args.script is not None:
        try:
            script = args.script.read_text(encoding="utf-8")
        except OSError as e:
            print(f"screenshot-parity: error: cannot read script: {e}", file=sys.stderr)
            return ERROR
    else:
        script = args.command or DEFAULT_SCRIPT
    if not script.strip():
        print("screenshot-parity: error: empty command/script", file=sys.stderr)
        return ERROR

    artifacts = args.artifacts.resolve()
    artifacts.mkdir(parents=True, exist_ok=True)
    for stale in ("kitty.png", "kitty.ppm", "kitr.ppm", "diff.ppm", "diff.png", "report.json"):
        try:
            (artifacts / stale).unlink()
        except OSError:
            pass
    for log_name in ("kitty.log", "kitty-rc.log", "kitr.log", "display.log", "convert.log"):
        try:
            (artifacts / log_name).unlink()
        except OSError:
            pass

    report: dict[str, object] = {
        "status": "running",
        "script": script,
        "size": args.size,
        "threshold_percent": args.threshold,
        "pixel_channel_threshold": args.pixel_threshold,
        "artifacts": str(artifacts),
    }

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
        return skip(artifacts, report, "; ".join(missing), args.strict)

    report["c_kitty"] = kitty
    report["kitten"] = kitten
    report["kitr_binary"] = str(kitr)
    report["convert"] = convert[0]

    display: VirtualDisplay | None = None
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
        report["display"] = display.name
        env = dict(os.environ)
        env["DISPLAY"] = display.name
        env.setdefault("XLIB_SKIP_ARGB_VISUALS", "1")

        print(f"screenshot-parity: running C kitty on DISPLAY={display.name}")
        kitty_png = run_kitty(
            kitty,
            kitten,
            script,
            (cols, rows),
            args.settle,
            args.timeout,
            artifacts,
            args.kitty_arg,
            env,
        )
        print("screenshot-parity: running kitr")
        kitr_ppm = run_kitr(
            str(kitr),
            script,
            (cols, rows),
            args.settle,
            args.timeout,
            artifacts,
            args.kitr_arg,
            env,
        )

        kitty_ppm = artifacts / "kitty.ppm"
        kitr_ppm_common = artifacts / "kitr-common.ppm"
        convert_to_ppm(convert, kitty_png, kitty_ppm, artifacts / "convert.log")
        convert_to_ppm(convert, kitr_ppm, kitr_ppm_common, artifacts / "convert.log")
        kitty_image = read_ppm(kitty_ppm)
        kitr_image = read_ppm(kitr_ppm_common)

        result = compare(kitty_image, kitr_image, args.pixel_threshold, artifacts)
        report.update(result)
        report["dimensions"] = {
            "kitty": [kitty_image.width, kitty_image.height],
            "kitr": [kitr_image.width, kitr_image.height],
        }
        status = "pass" if float(result["diff_percent"]) <= args.threshold else "fail"
        report["status"] = status
        report["artifacts"] = {
            "kitty_png": str(kitty_png),
            "kitr_ppm": str(kitr_ppm),
            "kitty_ppm": str(kitty_ppm),
            "kitr_common_ppm": str(kitr_ppm_common),
            "diff_ppm": str(artifacts / "diff.ppm"),
        }
        diff_png = artifacts / "diff.png"
        try:
            with (artifacts / "convert.log").open("ab") as f:
                subprocess.run(
                    [convert[1], str(artifacts / "diff.ppm"), str(diff_png)],
                    stdout=f,
                    stderr=subprocess.STDOUT,
                )
            if diff_png.is_file():
                report["artifacts"]["diff_png"] = str(diff_png)
        except OSError:
            pass
        write_report(artifacts, report)
        print(
            "screenshot-parity: {status}: {diff:.3f}% differing pixels "
            "({pixels}/{total}, threshold {threshold:.3f}%), mean channel delta {mean:.2f}".format(
                status=status.upper(),
                diff=float(result["diff_percent"]),
                pixels=result["different_pixels"],
                total=result["compared_pixels"],
                threshold=args.threshold,
                mean=float(result["mean_channel_delta"]),
            )
        )
        print(f"screenshot-parity: artifacts in {artifacts}")
        if kitty_image.width != kitr_image.width or kitty_image.height != kitr_image.height:
            print(
                "screenshot-parity: note: image dimensions differ "
                f"(kitty {kitty_image.width}x{kitty_image.height}, "
                f"kitr {kitr_image.width}x{kitr_image.height}); compared the overlap"
            )
        return PASS if status == "pass" else DIFF
    except MissingPrerequisite as e:
        return skip(artifacts, report, str(e), args.strict)
    except (ParityError, OSError, subprocess.SubprocessError) as e:
        report["status"] = "error"
        report["message"] = str(e)
        write_report(artifacts, report)
        print(f"screenshot-parity: ERROR: {e}", file=sys.stderr)
        return ERROR
    finally:
        if display is not None:
            display.stop()


if __name__ == "__main__":
    raise SystemExit(main())
