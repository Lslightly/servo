# /// script
# requires-python = ">=3.11"
# ///
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
"""Per-patch upstream drift report for a sync.

Usage: uv run docs/ohos-sync/drift.py <old-base> <new-base> [head-ref]

For every patch on <new-base>..<head> lists the upstream commits in
<old-base>..<new-base> that touched (a) files the patch modifies or (b) the
patch's `watch:` paths from STACK.md, plus upstream commits whose subject
matches KEYWORDS. Prints a markdown table; the verdict column is filled in by
the person or agent running the sync.
"""

import re
import sys
from collections import OrderedDict

from stacklib import git, read_manifest, stack_commits

KEYWORDS = r"ohos|openharmony|harmony|arkweb|clipboard|pasteboard|\bime\b|keyboard|indexeddb|webstorage|cookie"


def upstream_hits(old: str, new: str, paths: list[str]) -> OrderedDict:
    hits: OrderedDict[str, str] = OrderedDict()
    if not paths:
        return hits
    for line in git("log", "--format=%h %s", f"{old}..{new}", "--", *paths).splitlines():
        h, s = line.split(" ", 1)
        hits[h] = s
    return hits


def fmt(hits: OrderedDict) -> str:
    return "<br>".join(f"`{k}` {v[:60]}" for k, v in hits.items()) or "—"


def main() -> int:
    if len(sys.argv) < 3:
        print(__doc__)
        return 2
    old, new = sys.argv[1], sys.argv[2]
    head = sys.argv[3] if len(sys.argv) > 3 else "HEAD"
    manifest = {p.subject: p for p in read_manifest()}
    noise = re.compile(r"^(Cargo\.lock|Cargo\.toml)$")

    print("| # | patch | own files hit | watch hit | verdict |")
    print("|---|---|---|---|---|")
    for i, (h, subject) in enumerate(stack_commits(new, head), 1):
        p = manifest.get(subject)
        files = git("show", "--format=", "--name-only", h).split()
        own = upstream_hits(old, new, [f for f in files if not noise.match(f)])
        lock = upstream_hits(old, new, [f for f in files if noise.match(f)])
        watch = upstream_hits(old, new, p.watch if p else [])
        for k in own:
            watch.pop(k, None)
        own_cell = fmt(own)
        if lock:
            own_cell += f"<br>(+{len(lock)} lockfile/workspace-manifest commits)"
        print(f"| {i} | `{h}` {subject[:50]} | {own_cell} | {fmt(watch)} | |")

    print("\nUpstream subjects matching keywords (judge against the whole stack):\n")
    for line in git("log", "--format=%h %s", "-i", "-E", f"--grep={KEYWORDS}", f"{old}..{new}").splitlines():
        print(f"- `{line.split(' ', 1)[0]}` {line.split(' ', 1)[1]}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
