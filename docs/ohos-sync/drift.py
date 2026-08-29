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
patch's `watch:` paths from STACK.md, with the files they share, plus upstream
commits whose subject matches KEYWORDS. Lockfiles, workspace manifests and the
WPT manifests are counted, not listed. Then the same hits by upstream commit,
and the upstream commits that a sync always checks: those that change which
WPT directories run, or the mozjs version. Prints markdown; the verdict column
is filled in by the person or agent running the sync.
"""

import re
import sys
from collections import OrderedDict

from stacklib import git, read_manifest, stack_commits

KEYWORDS = r"ohos|openharmony|harmony|arkweb|clipboard|pasteboard|\bime\b|keyboard|indexeddb|webstorage|cookie"
NOISE = re.compile(r"^(Cargo\.lock|Cargo\.toml|tests/wpt/[^/]+/meta/MANIFEST\.json)$")


def upstream_hits(old: str, new: str, paths: list[str]) -> OrderedDict[str, tuple[str, list[str]]]:
    """{hash: (subject, files of `paths` it touched)} of the upstream commits touching `paths`."""
    hits: OrderedDict[str, tuple[str, list[str]]] = OrderedDict()
    if not paths:
        return hits
    out = git("log", "--format=%x00%h %s", "--name-only", f"{old}..{new}", "--", *paths)
    for chunk in out.split("\0")[1:]:
        lines = [line for line in chunk.splitlines() if line]
        h, s = lines[0].split(" ", 1)
        hits[h] = (s, lines[1:])
    return hits


def files_note(files: list[str]) -> str:
    shown = ", ".join(f.split("/")[-1] for f in files[:3])
    return f" ({shown}{f', +{len(files) - 3}' if len(files) > 3 else ''})" if files else ""


def fmt(hits: OrderedDict) -> str:
    return "<br>".join(f"`{k}` {s[:60]}{files_note(files)}" for k, (s, files) in hits.items()) or "—"


def main() -> int:
    if len(sys.argv) < 3:
        print(__doc__)
        return 2
    old, new = sys.argv[1], sys.argv[2]
    head = sys.argv[3] if len(sys.argv) > 3 else "HEAD"
    manifest = {p.subject: p for p in read_manifest()}
    by_commit: OrderedDict[str, tuple[str, list[str]]] = OrderedDict()

    print("| # | patch | own files hit | watch hit | verdict |")
    print("|---|---|---|---|---|")
    for i, (h, subject) in enumerate(stack_commits(new, head), 1):
        p = manifest.get(subject)
        files = git("show", "--format=", "--name-only", h).split()
        own = upstream_hits(old, new, [f for f in files if not NOISE.match(f)])
        noise = upstream_hits(old, new, [f for f in files if NOISE.match(f)])
        watch = upstream_hits(old, new, p.watch if p else [])
        for k in own:
            watch.pop(k, None)
        for kind, hits in (("own", own), ("watch", watch)):
            for k, (s, _) in hits.items():
                by_commit.setdefault(k, (s, []))[1].append(f"{i} ({kind})")
        own_cell = fmt(own)
        if noise:
            own_cell += f"<br>(+{len(noise)} lockfile, workspace-manifest or WPT-manifest commits)"
        print(f"| {i} | `{h}` {subject[:50]} | {own_cell} | {fmt(watch)} | |")

    print("\nThe same hits by upstream commit:\n")
    print("| upstream | patches |")
    print("|---|---|")
    for k, (s, patches) in by_commit.items():
        print(f"| `{k}` {s[:60]} | {', '.join(patches)} |")

    print("\nAlways checked (WPT directories that run, the mozjs version):\n")
    for line in git("log", "--format=%h %s", f"{old}..{new}", "--", "tests/wpt/include.ini").splitlines():
        print(f"- `{line.split(' ', 1)[0]}` {line.split(' ', 1)[1]} (tests/wpt/include.ini)")
    for line in git("log", "--format=%h %s", "-G", r"mozjs", f"{old}..{new}", "--", "Cargo.toml").splitlines():
        print(f"- `{line.split(' ', 1)[0]}` {line.split(' ', 1)[1]} (mozjs in Cargo.toml)")

    print("\nUpstream subjects matching keywords (judge against the whole stack):\n")
    for line in git("log", "--format=%h %s", "-i", "-E", f"--grep={KEYWORDS}", f"{old}..{new}").splitlines():
        print(f"- `{line.split(' ', 1)[0]}` {line.split(' ', 1)[1]}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
