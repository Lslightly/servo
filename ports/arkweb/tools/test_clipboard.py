# /// script
# requires-python = ">=3.9"
# dependencies = ["pytest>=7", "hdc-py>=0.2.0", "pillow>=10"]
# ///
"""Async Clipboard API (`navigator.clipboard`) backed by the OHOS system pasteboard.

A page served by the host fixture server (the API needs a secure context, which loopback is
and data: URLs are not) writes a token with `writeText` and reports the outcome back to the
server. The app is then force-stopped, which discards Servo's in-process fallback clipboard,
and a fresh launch reads the token back with `readText`, so it can only have come from the
system pasteboard. Reading needs ohos.permission.READ_PASTEBOARD (system_basic, granted with
`atm`); without it the read falls back to the empty in-process clipboard.
"""

from __future__ import annotations

import http.server
import queue
import re
import secrets
import subprocess
import threading
import urllib.parse

import pytest

from conftest import BUNDLE, CMD_TIMEOUT, read_log, sh

READ_PERMISSION = "ohos.permission.READ_PASTEBOARD"
FALLBACK_WARNING = "OHOS pasteboard"

CLIPBOARD_PAGE = """<html><head><title>clipboard-page</title></head><body>
<script>
const params = new URLSearchParams(location.search);
const report = (key, value) => fetch(`/report?${key}=${encodeURIComponent(value)}`);
(async () => {
  try {
    if (params.has("write")) {
      await navigator.clipboard.writeText(params.get("write"));
      report("write", "ok");
    } else {
      report("read", await navigator.clipboard.readText());
    }
  } catch (error) {
    report("error", String(error));
  }
})();
</script></body></html>"""


class _ClipboardHandler(http.server.BaseHTTPRequestHandler):
    def do_GET(self) -> None:  # noqa: N802 (BaseHTTPRequestHandler API)
        url = urllib.parse.urlsplit(self.path)
        if url.path == "/clipboard":
            body = CLIPBOARD_PAGE.encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/html")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(body)
        elif url.path == "/report":
            self.server.reports.put(dict(urllib.parse.parse_qsl(url.query, keep_blank_values=True)))  # type: ignore[attr-defined]
            self.send_response(204)
            self.end_headers()
        else:
            self.send_error(404)

    def log_message(self, format: str, *args) -> None:  # quiet
        pass


@pytest.fixture
def clipboard_server(device, hdc_target: str):
    """Yield a function that loads the clipboard page in a fresh app process and returns
    what the page reported."""
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), _ClipboardHandler)
    server.reports = queue.Queue()  # type: ignore[attr-defined]
    port = server.server_address[1]
    threading.Thread(target=server.serve_forever, daemon=True).start()
    forward = f"tcp:{port}"
    subprocess.run(["hdc", "-t", hdc_target, "rport", forward, forward], capture_output=True, timeout=CMD_TIMEOUT)

    def visit(launch, **query: str) -> dict[str, str]:
        while not server.reports.empty():  # type: ignore[attr-defined]
            server.reports.get_nowait()  # type: ignore[attr-defined]
        launch(url=f"http://127.0.0.1:{port}/clipboard?{urllib.parse.urlencode(query)}")
        try:
            return server.reports.get(timeout=20)  # type: ignore[attr-defined]
        except queue.Empty:
            pytest.fail("the clipboard page never reported back")

    yield visit
    subprocess.run(["hdc", "-t", hdc_target, "fport", "rm", forward, forward], capture_output=True, timeout=CMD_TIMEOUT)
    server.shutdown()


def set_read_permission(device, granted: bool) -> None:
    dump = sh(device, f"bm dump -n {BUNDLE}")
    token = re.search(r'"accessTokenId":\s*(\d+)', dump)
    assert token, "no accessTokenId in `bm dump`"
    option = "-g" if granted else "-c"
    result = sh(device, f"atm perm {option} -i {token.group(1)} -p {READ_PERMISSION}")
    assert "Success" in result, f"atm perm {option} {READ_PERMISSION} failed: {result!r}"


@pytest.fixture
def read_permission(device):
    set_read_permission(device, True)
    yield lambda granted: set_read_permission(device, granted)
    set_read_permission(device, True)


def test_text_round_trips_through_the_system_pasteboard(launch, device, clipboard_server, read_permission):
    token = f"SERVOCLIP-{secrets.token_hex(4)}"
    assert clipboard_server(launch, write=token) == {"write": "ok"}
    assert FALLBACK_WARNING not in read_log(device), "writing fell back to the in-process clipboard"

    assert clipboard_server(launch) == {"read": token}
    assert FALLBACK_WARNING not in read_log(device), "reading fell back to the in-process clipboard"


def test_read_without_permission_does_not_reach_the_pasteboard(launch, device, clipboard_server, read_permission):
    token = f"SERVOCLIP-{secrets.token_hex(4)}"
    assert clipboard_server(launch, write=token) == {"write": "ok"}

    read_permission(False)
    assert clipboard_server(launch) == {"read": ""}
    assert "OHOS pasteboard get_text failed" in read_log(device)


if __name__ == "__main__":
    import sys

    sys.exit(pytest.main([__file__, "-v"]))
