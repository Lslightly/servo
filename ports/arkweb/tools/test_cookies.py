# /// script
# requires-python = ">=3.9"
# dependencies = ["pytest>=7", "hdc-py>=0.2.0", "pillow>=10"]
# ///
"""Cookie manager (`WebCookieManager`) backed by Servo's cookie jar.

Drives ControllerPage's cookie buttons against a page served by the host fixture server
(cookies need an http(s) origin). The page gets one cookie from a `Set-Cookie` header and one
from `document.cookie`; `fetchCookieSync` must return both, and a cookie set with
`configCookieSync` must reach the server on the next load, which paints the page green.

Not covered: persistence across app restarts and `fetchCookieSync` without any cookie, both
known not to work yet.
"""

from __future__ import annotations

import http.server
import subprocess
import threading

import pytest

from conftest import CMD_TIMEOUT, is_green, sample, wait_for_pixel, wait_log
from test_controller import invoke

COOKIE_PAGE = """<html><head><title>cookie-page</title></head>
<body style="margin:0;background:#cc0000">
<script>document.cookie = "fromjs=3; path=/";</script></body></html>"""
COOKIE_OK_PAGE = "<html><head><title>cookie-ok</title></head><body style='margin:0;background:#00cc00'></body></html>"
PROBE = (360, 900)


class _CookieHandler(http.server.BaseHTTPRequestHandler):
    def do_GET(self) -> None:  # noqa: N802 (BaseHTTPRequestHandler API)
        cookies = self.headers.get("Cookie", "")
        received = "fromarkts=2" in cookies
        body = (COOKIE_OK_PAGE if received else COOKIE_PAGE).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/html")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        if not received:
            self.send_header("Set-Cookie", "fromserver=1; Path=/")
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format: str, *args) -> None:  # quiet
        pass


@pytest.fixture
def cookie_server(device, hdc_target: str):
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), _CookieHandler)
    port = server.server_address[1]
    threading.Thread(target=server.serve_forever, daemon=True).start()
    forward = f"tcp:{port}"
    subprocess.run(["hdc", "-t", hdc_target, "rport", forward, forward], capture_output=True, timeout=CMD_TIMEOUT)
    yield f"http://127.0.0.1:{port}/cookie"
    subprocess.run(["hdc", "-t", hdc_target, "fport", "rm", forward, forward], capture_output=True, timeout=CMD_TIMEOUT)
    server.shutdown()


def test_cookie_manager_round_trip(launch, device, dump, cap, cookie_server):
    launch(page="controller", url=cookie_server)
    assert wait_log(device, "ArkWebTest: pageEnd="), "cookie page never finished loading"
    tree = dump()

    cookies = invoke(device, tree, "getCookie")
    assert "fromserver=1" in cookies, f"Set-Cookie cookie missing: {cookies!r}"
    assert "fromjs=3" in cookies, f"document.cookie cookie missing: {cookies!r}"

    assert invoke(device, tree, "setCookie") == "ok"
    assert "fromarkts=2" in invoke(device, tree, "getCookie")

    assert invoke(device, tree, "reload") == "ok"
    img = wait_for_pixel(cap, PROBE, is_green, timeout=10.0)
    assert is_green(sample(img, PROBE)), "cookie set through WebCookieManager did not reach the server"


if __name__ == "__main__":
    import sys

    sys.exit(pytest.main([__file__, "-v"]))
