# /// script
# requires-python = ">=3.9"
# dependencies = ["pytest>=7", "hdc-py>=0.2.0", "pillow>=10"]
# ///
"""System focus of the Servo WebView.

Servo has the embedder report system focus per WebView (`WebView::set_focused`), and the
port gives it to a WebView when ACE focuses it and when it is built. A page in a newly
built WebView must therefore see `document.hasFocus()` as in a focused window.
"""

from __future__ import annotations

import pytest

from conftest import data_url, is_green, sample, wait_for_pixel

PAGE = """<html><body style="margin:0;height:100vh;background:#cc0000">
<script>
function paint() {
  document.body.style.background = document.hasFocus() ? "#00cc00" : "#cc0000";
}
addEventListener("focus", paint);
addEventListener("blur", paint);
addEventListener("load", () => setTimeout(paint, 300));
</script></body></html>"""
PROBE = (360, 600)


def test_new_webview_has_system_focus(launch, cap):
    launch(url=data_url(PAGE))
    img = wait_for_pixel(cap, PROBE, is_green, timeout=10.0)
    assert is_green(sample(img, PROBE)), "document.hasFocus() is false in a newly built WebView"


if __name__ == "__main__":
    import sys

    sys.exit(pytest.main([__file__, "-v"]))
