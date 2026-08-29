# /// script
# requires-python = ">=3.11"
# ///
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
"""Check that base + patches reproduce <head-ref>, without a `base/<date>` tag.

Usage: uv run docs/ohos-sync/check_integrity.py [head-ref]

The base is derived as `<head-ref>~<number of STACK.md entries>`; a shallow
clone is deepened to reach it. Then `check_stack.py` runs against it, including
the `--check` runs of the vendoring scripts under third_party/, and the base
must be an ancestor of servo/servo `main` (checked via the GitHub API,
authenticated with $GITHUB_TOKEN if set). This is the `integrity` CI job.
"""

import json
import os
import sys
import urllib.error
import urllib.request

from check_stack import check_manifest, check_patches, check_vendored
from stacklib import git, read_manifest


def upstream_status(base: str) -> str:
    request = urllib.request.Request(
        f"https://api.github.com/repos/servo/servo/compare/{base}...main",
        headers={"Accept": "application/vnd.github+json"},
    )
    if token := os.environ.get("GITHUB_TOKEN"):
        request.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(request) as response:
            return json.load(response)["status"]
    except urllib.error.HTTPError as e:
        return f"HTTP {e.code}"


def main() -> int:
    head = git("rev-parse", sys.argv[1] if len(sys.argv) > 1 else "HEAD").strip()
    n = len(read_manifest())
    if git("rev-parse", "--is-shallow-repository").strip() == "true":
        git("fetch", "-q", "--no-tags", f"--depth={n + 1}", "origin", head)
    base = git("rev-parse", "--verify", "-q", f"{head}~{n}", check=False).strip()
    if not base:
        print(f"{head} has fewer than {n} ancestors, but STACK.md lists {n} patches")
        return 1
    print(f"base: {base} ({head}~{n})")

    ok = check_manifest(base, head)
    ok = check_patches(base, head) and ok
    ok = check_vendored() and ok
    status = upstream_status(base)
    if status in ("ahead", "identical"):
        print("base ok: ancestor of servo/servo main")
    else:
        print(f"base is not an ancestor of servo/servo main (compare status: {status})")
        ok = False
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
