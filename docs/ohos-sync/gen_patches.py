# /// script
# requires-python = ">=3.11"
# ///
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
"""Regenerate docs/ohos-sync/patches from `git log <base>..HEAD`.

Usage: uv run docs/ohos-sync/gen_patches.py <base-ref> [head-ref]

Commit hashes are zeroed so a patch file only changes when the patch itself
(diff, message, trailers) or the upstream blobs it touches change. The
patches directory is excluded from the diff of the commit that carries it, and
so is the vendored code that `third_party/*/update.sh` regenerates.
"""

import shutil
import sys

from stacklib import PATCH_DIR, excluded_pathspecs, git


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    base, head = sys.argv[1], sys.argv[2] if len(sys.argv) > 2 else "HEAD"
    shutil.rmtree(PATCH_DIR, ignore_errors=True)
    PATCH_DIR.mkdir()
    out = git(
        "format-patch",
        "--zero-commit",
        "--no-signature",
        # Keep empty commits, e.g. superseded patches kept as placeholders for a sync; the
        # pathspec below would otherwise drop them from the walk.
        "--always",
        "--full-history",
        "--sparse",
        "-N",
        "-o",
        str(PATCH_DIR),
        f"{base}..{head}",
        "--",
        ".",
        *excluded_pathspecs(),
    )
    print(f"wrote {len(out.splitlines())} patches to {PATCH_DIR}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
