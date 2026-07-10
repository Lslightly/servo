# /// script
# requires-python = ">=3.9"
# dependencies = ["pytest>=7", "hdc-py>=0.2.0", "pillow>=10"]
# ///
"""Background/foreground rendering throttle.

Regression guard for the `paused_ || occluded_` throttle composition in `ServoNWeb`
(`OnPause`/`OnContinue` and `OnOccluded`/`OnUnoccluded` both feed `set_throttled`): sending
the app to the background must throttle Servo, foregrounding must unthrottle. Observed via
the `[arkweb] set_throttled` hilog marker.

Throttling hides the WebView, which also stops Servo painting it, so foregrounding must
show it again: the page has to repaint and handle input afterwards.

True `OnOccluded` coverage would need another window fully covering the web surface (the RS
occlusion callback); on the rk3568 that is not reliably triggerable from the harness, so
only the pause path is asserted.
"""

from __future__ import annotations

import pytest

from conftest import (
    clear_log,
    data_url,
    is_blue,
    is_green,
    key,
    launch_page,
    sample,
    tap,
    wait_for_pixel,
    wait_log,
)

KEYCODE_HOME = 1

PAGE = "<html><body style='background:#00cc00'>throttle</body></html>"

TAP_PAGE = (
    "<html><body style='margin:0;height:100vh;background:#00cc00' "
    "onclick=\"document.body.style.background='#0000cc'\">throttle</body></html>"
)
PROBE = (360, 600)


def test_background_throttles(launch, device):
    launch(url=data_url(PAGE))
    clear_log(device)

    key(device, KEYCODE_HOME)
    assert wait_log(device, "set_throttled id=1 true"), "backgrounding did not throttle Servo"

    launch_page(device)  # foreground again (no force-stop: same process, OnContinue path)
    assert wait_log(device, "set_throttled id=1 false"), "foregrounding did not unthrottle Servo"


def test_foreground_repaints_and_takes_input(launch, device, cap):
    launch(url=data_url(TAP_PAGE))
    assert is_green(sample(wait_for_pixel(cap, PROBE, is_green), PROBE)), "page did not render"
    clear_log(device)

    key(device, KEYCODE_HOME)
    assert wait_log(device, "set_throttled id=1 true"), "backgrounding did not throttle Servo"
    # The test app ignores a start URL equal to the current one, so this foregrounds the
    # same page instead of navigating.
    launch_page(device, url=data_url(TAP_PAGE))
    assert wait_log(device, "set_throttled id=1 false"), "foregrounding did not unthrottle Servo"

    img = wait_for_pixel(cap, PROBE, is_green)
    assert is_green(sample(img, PROBE)), "page not painted again after foregrounding"
    tap(device, PROBE)
    img = wait_for_pixel(cap, PROBE, is_blue)
    assert is_blue(sample(img, PROBE)), "tap after foregrounding did not repaint the page"


if __name__ == "__main__":
    import sys

    sys.exit(pytest.main([__file__, "-v"]))
