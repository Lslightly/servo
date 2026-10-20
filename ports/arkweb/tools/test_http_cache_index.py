# /// script
# requires-python = ">=3.9"
# dependencies = ["pytest>=7", "hdc-py>=0.2.0", "pillow>=10"]
# ///
"""HTTP cache index survives being backgrounded and killed.

A phone application is frozen and then killed rather than shut down, so the cache
index -- which is otherwise only written once changes stop -- would never reach
disk and every cold start would pay a directory scan. `ServoNWeb::OnPause` (which
ACE calls on window hide and app background) therefore flushes it synchronously.

This is the regression guard for that path, end to end on a device:

  1. start from an empty cache directory,
  2. load a cacheable page so the store has something to write down,
  3. send the app to the home screen and assert the index reaches disk,
  4. force-stop, i.e. the kill an OS reaper would do,
  5. restart and assert the index was *loaded*, not rebuilt by scanning.

Step 5 is the one that matters and the one a purely functional check would miss:
the cache still answers from disk either way, because a rebuild finds the same
entries. The two paths are told apart by the `http-cache: index ...` markers in
`components/net/http_cache/disk/index.rs`.
"""

from __future__ import annotations

import time

import pytest

from conftest import BUNDLE, clear_log, force_stop, key, read_log, sh, wait_log

KEYCODE_HOME = 1

LOADED = "http-cache: index loaded from disk"
REBUILT = "http-cache: index rebuilt by scanning"

# The cache dir is found rather than hard-coded: a layout change should fail
# loudly here, not silently make the test assert nothing.
CACHE_GLOB = f"/data/app/el2/100/base/{BUNDLE}"


def find_cache_dir(device) -> str | None:
    found = sh(
        device,
        f"find {CACHE_GLOB} -maxdepth 5 -type d -name http-cache 2>/dev/null",
        check=False,
    ).strip()
    return found.splitlines()[0].strip() if found else None


def wait_for_file(device, path: str, timeout: float = 15.0) -> bool:
    deadline = time.time() + timeout
    while time.time() < deadline:
        probe = sh(device, f"if [ -f {path} ]; then echo __YES__; fi", check=False)
        if "__YES__" in probe:
            return True
        time.sleep(0.5)
    return False


def test_index_is_flushed_on_background_and_reused_on_restart(device, http_server, launch):
    # (1) An empty cache directory. The app may never have run, so the directory
    # is located after a first launch and wiped before the run that matters.
    force_stop(device)
    existing = find_cache_dir(device)
    if existing:
        sh(device, f"rm -rf {existing}", check=False)

    # (2) A page whose subresource is cacheable, so the store commits entries.
    launch(url=f"{http_server}/cacheable.html")

    cache_dir = find_cache_dir(device)
    assert cache_dir, (
        "no http-cache directory appeared under the app data dir; the port may not "
        "be setting Opts::http_cache_dir any more"
    )
    assert cache_dir.startswith(f"{CACHE_GLOB}/cache/web/"), (
        f"the cache must be under web/ in the app's cache dir, next to ArkWeb's own data: {cache_dir}"
    )
    index_data = f"{cache_dir}/index-data"

    entries = sh(device, f"ls {cache_dir}/entries 2>/dev/null | wc -l", check=False).strip()
    assert entries and int(entries) > 0, f"nothing was cached, so the test proves nothing ({entries=})"
    assert not wait_for_file(device, index_data, timeout=2.0), (
        "index-data exists already; the debounce should not have fired this soon, so "
        "step (3) would not be testing the background flush"
    )

    # (3) Home screen -> OnPause -> synchronous flush.
    key(device, KEYCODE_HOME)
    assert wait_for_file(device, index_data), "backgrounding did not flush the cache index to disk"

    # (4) The kill an OS reaper performs on a frozen application.
    time.sleep(1.0)
    force_stop(device)

    # (5) The restart must trust the index instead of scanning for it.
    clear_log(device)
    launch(url=f"{http_server}/cacheable.html")
    assert wait_log(device, LOADED), (
        f"restart did not load the index from disk; log said:\n"
        f"{[line for line in read_log(device).splitlines() if 'http-cache:' in line]}"
    )
    assert REBUILT not in read_log(device), "the index was rebuilt by scanning despite being flushed"


if __name__ == "__main__":
    import sys

    sys.exit(pytest.main([__file__, "-v"]))
