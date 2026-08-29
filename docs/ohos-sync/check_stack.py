# /// script
# requires-python = ">=3.11"
# ///
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
"""Check STACK.md and the checked-in patch set against `git log <base>..HEAD`.

Usage: uv run docs/ohos-sync/check_stack.py <base-ref> [head-ref]

1. The ordered subjects in STACK.md must equal the git log.
2. `git am` of docs/ohos-sync/patches/*.patch onto <base-ref> must reproduce
   the head tree (ignoring the patches directory and the vendored third-party
   code the patches leave out).
3. Each `third_party/patches/<name>/update.sh --check` must agree that the
   vendored code in the working tree is what the script produces. This needs
   network access.
"""

import subprocess
import sys
import tempfile

from stacklib import (
    PATCH_DIR,
    REPO,
    excluded_pathspecs,
    git,
    read_manifest,
    stack_commits,
    vendor_scripts,
)


def check_manifest(base: str, head: str) -> bool:
    expected = [p.subject for p in read_manifest()]
    log = [s for _, s in stack_commits(base, head)]
    if expected == log:
        print(f"manifest ok: {len(log)} patches")
        return True
    print("STACK.md and git log differ:")
    for i in range(max(len(expected), len(log))):
        e = expected[i] if i < len(expected) else None
        g = log[i] if i < len(log) else None
        mark = " " if e == g else "!"
        print(f"{mark} {i + 1:2}. manifest: {e!r}\n       git:      {g!r}")
    return False


def check_patches(base: str, head: str) -> bool:
    patches = sorted(PATCH_DIR.glob("*.patch"))
    n = len(stack_commits(base, head))
    if len(patches) != n:
        print(f"patch set has {len(patches)} files, stack has {n} commits")
        return False
    with tempfile.TemporaryDirectory() as tmp:
        git("worktree", "add", "--detach", "-q", tmp, base)
        try:
            r = subprocess.run(
                ["git", "-C", tmp, "am", "--committer-date-is-author-date", "--empty=keep", "-q", *map(str, patches)],
                capture_output=True,
                text=True,
            )
            if r.returncode != 0:
                print("git am failed:\n" + r.stderr + r.stdout)
                return False
            applied = git("-C", tmp, "rev-parse", "HEAD").strip()
        finally:
            git("worktree", "remove", "--force", tmp)
    diff = git("diff", "--stat", applied, head, "--", ".", *excluded_pathspecs())
    if diff:
        print("patch set does not reproduce the head tree:\n" + diff)
        return False
    print(f"patch set ok: {n} patches reproduce {head}")
    return True


def check_vendored() -> bool:
    """Run the third-party vendoring scripts against the working tree."""
    ok = True
    for script in vendor_scripts():
        r = subprocess.run([script, "--check"], capture_output=True, text=True)
        print(f"{script.relative_to(REPO)}: {r.stdout.strip() or r.stderr.strip()}")
        ok = r.returncode == 0 and ok
    return ok


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    base, head = sys.argv[1], sys.argv[2] if len(sys.argv) > 2 else "HEAD"
    ok = check_manifest(base, head)
    ok = check_patches(base, head) and ok
    ok = check_vendored() and ok
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
