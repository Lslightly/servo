# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
"""Shared helpers for the ohos-main stack tooling (STACK.md parsing, git)."""

import re
import subprocess
from dataclasses import dataclass, field
from pathlib import Path

STACK_MD = Path(__file__).with_name("STACK.md")
PATCH_DIR = Path(__file__).with_name("patches")
REPO = Path(__file__).resolve().parents[2]
# Vendored third-party code that third_party/patches/<name>/update.sh writes. It is kept out of the
# patch files, which would otherwise carry megabytes of generated sources; the scripts check it
# instead.
GENERATED_DIRS = ["third_party/freetype-sys", "third_party/stylo"]
ENTRY = re.compile(r"^(\d+)\. `([^`]+)` — (\S+) · (\S+)(?: · watch: (.*))?$")


@dataclass
class Patch:
    number: int
    subject: str
    tier: str
    coverage: str
    watch: list[str] = field(default_factory=list)


def git(*args: str, check: bool = True) -> str:
    return subprocess.run(["git", *args], check=check, capture_output=True, text=True).stdout


def excluded_pathspecs() -> list[str]:
    """Pathspecs for what the patch files leave out."""
    return [f":!{PATCH_DIR.relative_to(REPO)}", *(f":!{d}" for d in GENERATED_DIRS)]


def vendor_scripts() -> list[Path]:
    return sorted(REPO.glob("third_party/patches/*/update.sh"))


def read_manifest() -> list[Patch]:
    section = STACK_MD.read_text().split("## Sync log")[0]
    patches = []
    for line in section.splitlines():
        m = ENTRY.match(line)
        if m:
            watch = [] if m.group(5) in (None, "-") else m.group(5).split()
            patches.append(Patch(int(m.group(1)), m.group(2), m.group(3), m.group(4), watch))
    return patches


def stack_commits(base: str, head: str = "HEAD") -> list[tuple[str, str]]:
    """(hash, subject) for base..head, oldest first."""
    out = git("log", "--reverse", "--format=%h %s", f"{base}..{head}")
    return [tuple(line.split(" ", 1)) for line in out.splitlines()]
