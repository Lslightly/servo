# Copyright 2026 The Servo Project Developers. See the COPYRIGHT
# file at the top-level directory of this distribution.
#
# Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
# http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
# <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
# option. This file may not be copied, modified, or distributed
# except according to those terms.

"""Chooses the CI jobs of an `ohos-main` pull request from the files it changes.

Every pull request runs the Linux unit tests and lint. On top of that:

- OHOS code: the OHOS build. The Linux WPT run never executes that code.
- The QuickJS workflow: the QuickJS build.
- WPT tests or expectations: a Linux WPT run of the directories of those tests.
- Files that no build or test reads, such as documentation: nothing.
- Anything else: the OHOS and QuickJS builds and the whole Linux WPT suite.

The path lists are explicit on purpose: a path missing from them only costs a full run.
"""

from __future__ import annotations

import json
import logging
import math
import subprocess
import sys
import unittest
from collections.abc import Callable, Iterable

from .try_parser import Config, Workflow

# The jobs that every pull request runs.
ALWAYS = "linux-unit-tests lint"

# The jobs of a change to code that every platform builds.
FULL = f"{ALWAYS} linux-wpt ohos quickjs"

# Files that no build or test reads, besides Markdown files.
NOT_BUILT = (
    ".github/workflows/integrity.yml",
    "docs/",
)

# Code that is only built for OpenHarmony.
OHOS_ONLY = (
    ".github/workflows/ohos-emulator.yml",
    ".github/workflows/ohos.yml",
    "components/fonts/platform/freetype/ohos/",
    "components/media/backends/ohos/",
    "components/storage/client_storage_ohos_rdb.rs",
    "components/storage/indexeddb/engines/ohos_rdb.rs",
    "components/storage/ohos_rdb/",
    "components/storage/webstorage/engines/ohos_rdb.rs",
    "etc/ci/ohos_emulator_smoke_test.py",
    "ports/arkweb/",
    "ports/servoshell/egl/ohos/",
    "ports/servoshell/platform/openharmony/",
    "python/wpt/ohos_test_parser.js",
    "python/wpt/ohos_webdriver_test.py",
    "support/openharmony/",
)

# Files that only the QuickJS build reads.
QUICKJS_ONLY = (".github/workflows/quickjs.yml",)

# Test and expectation roots, mapped to the directory that holds their tests.
WPT_ROOTS = {
    "tests/wpt/tests/": "tests/wpt/tests/",
    "tests/wpt/meta/": "tests/wpt/tests/",
    "tests/wpt/mozilla/tests/": "tests/wpt/mozilla/tests/",
    "tests/wpt/mozilla/meta/": "tests/wpt/mozilla/tests/",
}

# Generated from the tests that change with them.
WPT_MANIFESTS = ("tests/wpt/meta/MANIFEST.json", "tests/wpt/mozilla/meta/MANIFEST.json")

# Top-level WPT directories whose files are used by tests all over the suite.
WPT_SHARED = {"common", "fonts", "images", "interfaces", "media", "resources", "tools"}

# Directories of files that tests next to them use; a change there runs the parent directory.
WPT_SUPPORT = {"reference", "references", "resources", "support"}

# More directories than this run the whole suite.
MAX_DIRECTORIES = 30

# Files per WPT chunk.
FILES_PER_CHUNK = 1000


def is_not_built(path: str) -> bool:
    return path.startswith(NOT_BUILT) or path.endswith(".md")


def wpt_directory(path: str) -> str | None:
    """The test directory to run for a changed WPT file: "" for a manifest, None for a file that is
    not a test or expectation, or that is used by tests all over the suite."""
    if path in WPT_MANIFESTS:
        return ""
    for root, tests in WPT_ROOTS.items():
        if path.startswith(root):
            parts = path.removeprefix(root).split("/")[:-1]
            for i, part in enumerate(parts):
                if part in WPT_SUPPORT:
                    parts = parts[:i]
                    break
            if not parts or (tests == "tests/wpt/tests/" and parts[0] in WPT_SHARED):
                return None
            return tests + "/".join(parts)
    return None


def outermost(directories: Iterable[str]) -> list[str]:
    result: list[str] = []
    for directory in sorted(directories):
        if not any(directory == other or directory.startswith(other + "/") for other in result):
            result.append(directory)
    return result


def select(changed: Iterable[str], count_files: Callable[[str], int]) -> Config:
    """Chooses the jobs for the changed paths. `count_files` returns the number of files in a
    directory of the merged tree, 0 if it does not exist."""
    ohos = quickjs = False
    directories = set()
    for path in changed:
        if is_not_built(path):
            continue
        if path.startswith(OHOS_ONLY):
            ohos = True
            continue
        if path.startswith(QUICKJS_ONLY):
            quickjs = True
            continue
        directory = wpt_directory(path)
        if directory is None:
            return Config(FULL)
        if directory:
            directories.add(directory)

    counts = {directory: count_files(directory) for directory in outermost(directories)}
    counts = {directory: count for directory, count in counts.items() if count}
    if len(counts) > MAX_DIRECTORIES:
        return Config(FULL)

    # The WPT run is added after the unit tests, so that both share one Linux job.
    jobs = [ALWAYS, *(["ohos"] if ohos else []), *(["quickjs"] if quickjs else []), *(["linux-wpt"] if counts else [])]
    config = Config(" ".join(jobs))
    for job in config.matrix:
        if job.workflow is Workflow.LINUX and counts:
            job.wpt_args = " ".join(f"./{directory}" for directory in counts)
            job.number_of_wpt_chunks = min(20, math.ceil(sum(counts.values()) / FILES_PER_CHUNK))
    return config


def git(*args: str) -> str:
    return subprocess.run(["git", *args], stdout=subprocess.PIPE, text=True, check=True).stdout


def main() -> None:
    base = sys.argv[1]
    changed = git("diff", "--name-only", "--no-renames", base, "HEAD").splitlines()

    def count_files(directory: str) -> int:
        return len(git("ls-tree", "-r", "--name-only", "HEAD", "--", directory).splitlines())

    print(select(changed, count_files).to_json())


if __name__ == "__main__":
    main()


class TestSelect(unittest.TestCase):
    FILES = {
        "tests/wpt/tests/css/css-flexbox": 2500,
        "tests/wpt/tests/dom/nodes": 400,
        "tests/wpt/mozilla/tests/mozilla/svg": 30,
    }

    def select(self, *changed: str) -> dict:
        return json.loads(select(changed, lambda directory: self.FILES.get(directory, 0)).to_json())

    def jobs(self, *changed: str) -> list[tuple[str, bool, bool, str, int]]:
        return [
            (job["workflow"], job["unit_tests"], job["wpt"], job["wpt_args"], job["number_of_wpt_chunks"])
            for job in self.select(*changed)["matrix"]
        ]

    LINUX = ("linux", True, False, "", 20)
    LINT = ("lint", False, False, "", 20)
    OHOS = ("ohos", False, False, "", 20)
    QUICKJS = ("quickjs", False, False, "", 20)

    def test_shared_code_runs_everything(self) -> None:
        full = json.loads(Config(FULL).to_json())
        self.assertEqual(
            self.jobs("components/layout/flow.rs"),
            [("linux", True, True, "", 20), self.LINT, self.OHOS, self.QUICKJS],
        )
        self.assertEqual(self.select("components/layout/flow.rs"), full)
        self.assertEqual(self.select("ports/arkweb/src/lib.rs", "Cargo.lock"), full)
        self.assertEqual(self.select("components/storage/webstorage/mod.rs"), full)

    def test_ohos_only(self) -> None:
        self.assertEqual(
            self.jobs(
                "ports/arkweb/src/lib.rs",
                "components/fonts/platform/freetype/ohos/font_list.rs",
                "etc/ci/ohos_emulator_smoke_test.py",
            ),
            [self.LINUX, self.LINT, self.OHOS],
        )

    def test_quickjs_workflow(self) -> None:
        self.assertEqual(
            self.jobs(".github/workflows/quickjs.yml", "ports/arkweb/src/lib.rs"),
            [self.LINUX, self.LINT, self.OHOS, self.QUICKJS],
        )

    def test_not_built_only(self) -> None:
        self.assertEqual(
            self.jobs("docs/ohos-sync/STACK.md", "README.md", ".github/workflows/integrity.yml"),
            [self.LINUX, self.LINT],
        )

    def test_tests_run_their_directories(self) -> None:
        self.assertEqual(
            self.jobs(
                "tests/wpt/tests/css/css-flexbox/align-items-001.html",
                "tests/wpt/tests/css/css-flexbox/support/test.css",
                "tests/wpt/meta/dom/nodes/Node-cloneNode.html.ini",
                "tests/wpt/meta/MANIFEST.json",
            ),
            [("linux", True, True, "./tests/wpt/tests/css/css-flexbox ./tests/wpt/tests/dom/nodes", 3), self.LINT],
        )

    def test_mozilla_tests_and_ohos_code(self) -> None:
        self.assertEqual(
            self.jobs("tests/wpt/mozilla/meta/mozilla/svg/svg-text-web-font.html.ini", "ports/arkweb/src/lib.rs"),
            [("linux", True, True, "./tests/wpt/mozilla/tests/mozilla/svg", 1), self.LINT, self.OHOS],
        )

    def test_shared_test_files_run_everything(self) -> None:
        full = json.loads(Config(FULL).to_json())
        self.assertEqual(self.select("tests/wpt/tests/resources/testharness.js"), full)
        self.assertEqual(self.select("tests/wpt/meta/__dir__.ini"), full)
        self.assertEqual(self.select("tests/wpt/include.ini"), full)

    def test_removed_tests_run_nothing(self) -> None:
        self.assertEqual(self.jobs("tests/wpt/tests/css/css-gone/test.html"), [self.LINUX, self.LINT])

    def test_too_many_directories_run_everything(self) -> None:
        changed = [f"tests/wpt/tests/css/dir{i}/test.html" for i in range(MAX_DIRECTORIES + 1)]
        self.FILES = {f"tests/wpt/tests/css/dir{i}": 1 for i in range(MAX_DIRECTORIES + 1)}
        self.assertEqual(self.select(*changed), json.loads(Config(FULL).to_json()))


def run_tests() -> bool:
    verbosity = 1 if logging.getLogger().level >= logging.WARN else 2
    suite = unittest.TestLoader().loadTestsFromTestCase(TestSelect)
    return unittest.TextTestRunner(verbosity=verbosity).run(suite).wasSuccessful()
