#!/usr/bin/env python3

# Copyright 2026 The Servo Project Developers. See the COPYRIGHT
# file at the top-level directory of this distribution.
#
# Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
# http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
# <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
# option. This file may not be copied, modified, or distributed
# except according to those terms.

"""Checks that servoshell runs on an OpenHarmony emulator.

Installs the HAP and opens a page that this script serves. The page reports back once it has
rendered a frame, and a screenshot must then show its background color. Writes the screenshot
and the device log to the output directory.
"""

from __future__ import annotations

import argparse
import json
import queue
import struct
import subprocess
import sys
import threading
import time
import zlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlparse

BUNDLE = "org.servo.servo"
ABILITY = "EntryAbility"

# This machine, as seen from a QEMU guest with user-mode networking.
GUEST_HOST = "10.0.2.2"

BACKGROUND = (0, 128, 255)
TEXT_WIDTH = 120

# The share of the screen that must show the page's background, which fills the web view.
MIN_BACKGROUND_SHARE = 0.25

PAGE = f"""<!doctype html>
<meta charset="utf-8">
<title>Servo smoke test</title>
<body style="margin: 0; background: rgb{BACKGROUND}">
<h1 id="text" style="width: {TEXT_WIDTH}px; color: white">Smoke test</h1>
<script>
  const report = (path, data) => fetch(path + "?" + new URLSearchParams(data));
  window.onerror = message => report("/error", {{ message }});
  window.addEventListener("load", () => {{
    const width = document.getElementById("text").getBoundingClientRect().width;
    requestAnimationFrame(() => requestAnimationFrame(() => {{
      report("/done", {{ width, userAgent: navigator.userAgent }});
    }}));
  }});
</script>
""".encode()


class Reports(BaseHTTPRequestHandler):
    reports: queue.Queue[tuple[str, dict[str, str]]] = queue.Queue()

    def do_GET(self) -> None:
        url = urlparse(self.path)
        if url.path == "/":
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.end_headers()
            self.wfile.write(PAGE)
            return
        if url.path in ("/done", "/error"):
            self.reports.put((url.path, {key: values[0] for key, values in parse_qs(url.query).items()}))
        self.send_response(204)
        self.end_headers()

    def log_message(self, format: str, *args: object) -> None:
        print(f"server: {format % args}")


def hdc(target: str | None, *args: str, timeout: float = 120) -> str:
    """Runs hdc, which exits with 0 even when a command fails, and reports failures in its output."""
    command = ["hdc", *(["-t", target] if target else []), *args]
    result = subprocess.run(command, capture_output=True, timeout=timeout, check=True)
    output = result.stdout.decode(errors="replace")
    if output.lstrip().startswith("[Fail]"):
        raise RuntimeError(f"{' '.join(command)}: {output.strip()}")
    return output


def png_pixels(data: bytes) -> tuple[int, bytes]:
    """Decodes an 8-bit, non-interlaced RGB or RGBA PNG into its channel count and pixel data."""
    chunks: dict[bytes, bytes] = {}
    pos = 8
    while pos < len(data):
        (length,) = struct.unpack(">I", data[pos : pos + 4])
        kind = data[pos + 4 : pos + 8]
        chunks[kind] = chunks.get(kind, b"") + data[pos + 8 : pos + 8 + length]
        pos += 12 + length
    width, height, depth, color, _, _, interlace = struct.unpack(">IIBBBBB", chunks[b"IHDR"])
    if depth != 8 or color not in (2, 6) or interlace:
        raise ValueError(f"unsupported PNG: depth {depth}, color type {color}, interlace {interlace}")
    channels = 3 if color == 2 else 4
    stride = width * channels
    raw = zlib.decompress(chunks[b"IDAT"])
    previous = bytearray(stride)
    pixels = bytearray()
    for y in range(height):
        kind = raw[y * (stride + 1)]
        line = bytearray(raw[y * (stride + 1) + 1 : (y + 1) * (stride + 1)])
        for x in range(stride):
            left = line[x - channels] if x >= channels else 0
            up = previous[x]
            up_left = previous[x - channels] if x >= channels else 0
            if kind == 1:
                line[x] = (line[x] + left) & 0xFF
            elif kind == 2:
                line[x] = (line[x] + up) & 0xFF
            elif kind == 3:
                line[x] = (line[x] + (left + up) // 2) & 0xFF
            elif kind == 4:
                estimate = left + up - up_left
                distances = (abs(estimate - left), abs(estimate - up), abs(estimate - up_left))
                predictor = (left, up, up_left)[distances.index(min(distances))]
                line[x] = (line[x] + predictor) & 0xFF
        pixels += line
        previous = line
    return channels, bytes(pixels)


def background_share(png: bytes) -> float:
    channels, pixels = png_pixels(png)
    count = len(pixels) // channels
    matching = sum(
        1 for i in range(0, len(pixels), channels) if all(abs(pixels[i + c] - BACKGROUND[c]) <= 8 for c in range(3))
    )
    return matching / count


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("hap", type=Path)
    parser.add_argument("--target", help="hdc connect-key of the emulator, needed when several devices are connected")
    parser.add_argument("--out", type=Path, default=Path("smoke-test"), help="directory for the screenshot and log")
    parser.add_argument("--timeout", type=float, default=120, help="seconds to wait for the page to render")
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Reports)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    url = f"http://{GUEST_HOST}:{server.server_address[1]}/"

    print(hdc(args.target, "install", "-r", str(args.hap), timeout=300).strip())
    hdc(args.target, "shell", "aa", "force-stop", BUNDLE)
    hdc(args.target, "shell", "hilog", "-r")
    print(hdc(args.target, "shell", "aa", "start", "-a", ABILITY, "-b", BUNDLE, "-U", url).strip())

    failures = []
    try:
        path, report = Reports.reports.get(timeout=args.timeout)
        print(f"page reported {path}: {json.dumps(report)}")
        if path == "/error":
            failures.append(f"script error: {report.get('message')}")
        elif float(report.get("width", "nan")) != TEXT_WIDTH:
            failures.append(f"the heading is {report.get('width')} px wide instead of {TEXT_WIDTH} px")
    except queue.Empty:
        failures.append(f"the page did not report back within {args.timeout:.0f} s")

    # The frame that the page waited for may still be on its way to the display.
    time.sleep(2)
    hdc(args.target, "shell", "uitest", "screenCap", "-p", "/data/local/tmp/smoke-test.png")
    screenshot = args.out / "screenshot.png"
    hdc(args.target, "file", "recv", "/data/local/tmp/smoke-test.png", str(screenshot))
    share = background_share(screenshot.read_bytes())
    print(f"{share:.0%} of the screenshot shows the page's background")
    if share < MIN_BACKGROUND_SHARE:
        failures.append(f"only {share:.0%} of the screenshot shows the page's background")

    log = hdc(args.target, "shell", "hilog", "-x")
    (args.out / "hilog.txt").write_text(log)
    if not hdc(args.target, "shell", "pidof", BUNDLE).strip():
        failures.append(f"{BUNDLE} is no longer running")
    failures += [f"log: {line.strip()}" for line in log.splitlines() if "panicked at" in line]

    server.shutdown()
    for failure in failures:
        print(f"FAIL: {failure}")
    if not failures:
        print("PASS: servoshell rendered the page")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
