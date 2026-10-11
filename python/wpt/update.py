# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
# pylint: disable=missing-docstring

import dataclasses
import glob
import os
import re
import shutil
import subprocess
import sys
import tempfile
from argparse import ArgumentParser, Namespace
from collections.abc import Generator
from typing import Any

from wptrunner import manifestexpected, metadata, wpttest
from wptrunner import wptcommandline  # noqa: F401
from wptrunner.manifestupdate import get_test_name
from wptrunner.update import WPTUpdate, setup_logging  # noqa: F401
from wptrunner.update.base import exit_unclean  # noqa: F401

from . import WPT_PATH, manifestupdate

TEST_ROOT = os.path.join(WPT_PATH, "tests")
META_ROOTS = [os.path.join(WPT_PATH, "meta"), os.path.join(WPT_PATH, "meta-legacy")]
GITHUB_ACTION_RUN_URL_REGEX = re.compile(r"github\.com\/([\w.-]+\/[\w.-]+)\/actions\/runs\/(\d+)$")

# The run-info property of the JavaScript engine, and the engine of the expectations without a condition on it.
JS_ENGINE = "js_engine"
DEFAULT_JS_ENGINE = "spidermonkey"

# The wptrunner functions that `use_js_engine_property` wraps.
WPT_UPDATE_RESULTS = metadata.update_results
WPT_SUITE_START = metadata.ExpectedUpdater.suite_start

# The JavaScript engines of the logs that the current update has read.
LOG_JS_ENGINES: set[str] = set()


def do_sync(**kwargs: str) -> int:
    last_commit = subprocess.check_output(["git", "log", "-1"])

    # Commits should always be authored by the GitHub Actions bot.
    os.environ["GIT_AUTHOR_NAME"] = "Servo WPT Sync"
    os.environ["GIT_AUTHOR_EMAIL"] = "ghbot+wpt-sync@servo.org"
    os.environ["GIT_COMMITTER_NAME"] = os.environ["GIT_AUTHOR_NAME"]
    os.environ["GIT_COMMITTER_EMAIL"] = os.environ["GIT_AUTHOR_EMAIL"]

    print("Updating WPT from upstream...")
    run_update(**kwargs)

    if last_commit == subprocess.check_output(["git", "log", "-1"]):
        return 255

    # Update the manifest twice to reach a fixed state
    # (https://github.com/servo/servo/issues/22275).
    print("Updating test manifests...")
    manifestupdate.update(check_clean=False)
    manifestupdate.update(check_clean=False)

    remove_unused_metadata()

    if subprocess.check_call(["git", "commit", "-a", "--amend", "--no-edit", "-q"]) != 0:
        print("Ammending commit failed. Bailing out.")
        return 1

    return 0


def remove_unused_metadata() -> None:
    print("Removing unused results...")
    unused_files = []
    unused_dirs = []

    for meta_root in META_ROOTS:
        for base_dir, dir_names, files in os.walk(meta_root):
            # Skip recursing into any directories that were previously found to be missing.
            if any([os.path.commonpath([base_dir, unused_dir]) == unused_dir for unused_dir in unused_dirs]):
                continue

            for dir_name in dir_names:
                dir_path = os.path.join(base_dir, dir_name)

                # Skip any known directories that are meta-metadata.
                if dir_name == ".cache":
                    unused_dirs.append(dir_path)
                    continue

                # Turn tests/wpt/meta/foo into tests/wpt/tests/foo.
                test_dir = os.path.join(TEST_ROOT, os.path.relpath(dir_path, meta_root))
                if not os.path.exists(test_dir):
                    unused_dirs.append(dir_path)

            for fname in files:
                # Skip any known files that are meta-metadata.
                if not fname.endswith(".ini") or fname == "__dir__.ini":
                    continue

                # Turn tests/wpt/meta/foo/bar.html.ini into tests/wpt/tests/foo/bar.html.
                test_file = os.path.join(TEST_ROOT, os.path.relpath(base_dir, meta_root), fname[:-4])

                if not os.path.exists(test_file):
                    unused_files.append(os.path.join(base_dir, fname))

    for file in unused_files:
        print(f"    - {file}")
        os.remove(file)
    for directory in unused_dirs:
        print(f"    - {directory}")
        shutil.rmtree(directory)


def update_tests(**kwargs: Any) -> int:
    def set_if_none(args: dict, key: str, value: str) -> None:
        if key not in args or args[key] is None:
            args[key] = value

    set_if_none(kwargs, "config", os.path.join(WPT_PATH, "config.ini"))
    kwargs["product"] = "servo"
    kwargs["store_state"] = False

    wptcommandline.set_from_config(kwargs)
    use_js_engine_property(kwargs)
    if hasattr(wptcommandline, "check_paths"):
        wptcommandline.check_paths(kwargs["test_paths"])

    if kwargs.get("sync", False):
        return do_sync(**kwargs)

    return 0 if run_update(**kwargs) else 1


def use_js_engine_property(kwargs: dict[str, Any]) -> None:
    """Lets the metadata update write conditions on the JavaScript engine. Logs without the property are from
    SpiderMonkey runs."""
    product = kwargs["product"]
    properties, dependents = product.update_properties
    if JS_ENGINE not in properties:
        kwargs["product"] = dataclasses.replace(product, update_properties=([*properties, JS_ENGINE], dependents))

    if metadata.update_results is not update_results_per_engine:
        metadata.update_results = update_results_per_engine
        metadata.ExpectedUpdater.suite_start = suite_start_with_js_engine
    LOG_JS_ENGINES.clear()


def suite_start_with_js_engine(self: metadata.ExpectedUpdater, data: dict[str, Any]) -> None:
    data["run_info"].setdefault(JS_ENGINE, DEFAULT_JS_ENGINE)
    LOG_JS_ENGINES.add(data["run_info"][JS_ENGINE])
    WPT_SUITE_START(self, data)


def update_results_per_engine(
    id_test_map: dict,
    update_properties: Any,
    full_update: bool,
    disable_intermittent: Any,
    update_intermittent: bool,
    remove_intermittent: bool,
) -> Generator:
    if full_update and len(LOG_JS_ENGINES) < 2:
        # A full update rebuilds the conditions from the logs, which would drop the other engine's.
        raise SystemExit(
            "`--full` needs logs of SpiderMonkey and of QuickJS runs, or it drops the other's expectations"
        )
    with_intermittents = add_default_engine_results(id_test_map, update_properties)
    # The value of a new condition on `js_engine` is computed without the current value, which drops its
    # intermittent statuses, and differing results of the default engine would make the other engine's result
    # the default, unless they are updated as intermittent.
    yield from WPT_UPDATE_RESULTS(
        {test_id: test_file for test_id, test_file in id_test_map.items() if test_file in with_intermittents},
        update_properties,
        full_update,
        disable_intermittent,
        update_intermittent=True,
        remove_intermittent=remove_intermittent,
    )
    yield from WPT_UPDATE_RESULTS(
        {test_id: test_file for test_id, test_file in id_test_map.items() if test_file not in with_intermittents},
        update_properties,
        full_update,
        disable_intermittent,
        update_intermittent=update_intermittent,
        remove_intermittent=remove_intermittent,
    )


def add_default_engine_results(id_test_map: dict, update_properties: Any) -> set:
    """Gives every result of another engine a result of the default engine, from the current expectations,
    unless the logs have one for the same configuration. Without it, an update from QuickJS logs alone would
    make the QuickJS results the expectations of both engines. Returns the test files where results of both
    engines meet intermittent or differing statuses of the default engine."""
    properties, dependents = update_properties
    keys = [key for key in [*properties, *(d for values in dependents.values() for d in values)] if key != JS_ENGINE]

    def configuration(run_info: metadata.RunInfo) -> tuple:
        return tuple(run_info.data.get(key) for key in keys)

    status = metadata.prop_intern.store("status")
    with_intermittents = set()
    for test_file in set(id_test_map.values()):
        if not test_file.requires_update or test_file.item_type is None:
            continue
        test_cls = wpttest.manifest_test_cls[test_file.item_type]
        manifests: dict[tuple, Any] = {}
        for test_id, subtests in test_file.data.items():
            for subtest_id, results in subtests.items():
                result_cls = test_cls.subtest_result_cls if subtest_id is not None else test_cls.result_cls
                if result_cls is None:
                    continue
                statuses = [(run_info, value) for prop, run_info, value in results if prop == "status"]
                others = [run_info for run_info, _ in statuses if run_info.data.get(JS_ENGINE) != DEFAULT_JS_ENGINE]
                if not others:
                    continue
                defaults: dict[tuple, list] = {}
                for run_info, value in statuses:
                    if run_info.data.get(JS_ENGINE) == DEFAULT_JS_ENGINE:
                        defaults.setdefault(configuration(run_info), []).append(value)
                for run_info in others:
                    if configuration(run_info) in defaults:
                        continue
                    default_run_info = metadata.RunInfo({**run_info.data, JS_ENGINE: DEFAULT_JS_ENGINE})
                    if configuration(run_info) not in manifests:
                        manifests[configuration(run_info)] = manifestexpected.get_manifest(
                            test_file.metadata_path, test_file.test_path, default_run_info.data
                        )
                    manifest = manifests[configuration(run_info)]
                    node = manifest.get_test(get_test_name(test_id)) if manifest else None
                    if node is not None and subtest_id is not None:
                        node = node.get_subtest(subtest_id)
                    # pyrefly: ignore[missing-attribute]
                    expected, known_intermittent = result_cls.default_expected, []
                    if node is not None and node.has_key("expected"):
                        expected, known_intermittent = node.expected, node.known_intermittent
                    result = metadata.pack_result({"status": expected, "known_intermittent": known_intermittent})
                    results.append(status, metadata.run_info_intern.store(default_run_info), result)
                    defaults[configuration(run_info)] = [result]
                for values in defaults.values():
                    unpacked = [metadata.unpack_result(value) for value in values]
                    if len(set(unpacked)) > 1 or any(known_intermittent for _, known_intermittent in unpacked):
                        with_intermittents.add(test_file)
    return with_intermittents


def run_update(**kwargs: Any) -> bool:
    """Run the update process returning True if the process is successful."""
    if any([GITHUB_ACTION_RUN_URL_REGEX.search(argument) for argument in sys.argv]):
        return download_run_results_and_then_run_update(kwargs)

    logger = setup_logging(kwargs, {"mach": sys.stdout})
    return WPTUpdate(logger, **kwargs).run() != exit_unclean


def download_run_results_and_then_run_update(kwargs: dict[str, Any]) -> bool:
    """If any of the arguments passed to `./mach update-wpt` are URLs, attempt to
    interpret them as GitHub Action Run URLs and download any test results, passing
    the downloaded results as the input to the WPT metadata update."""
    with tempfile.TemporaryDirectory() as directory:
        downloaded_run_logs = []
        for argument in sys.argv:
            if argument.startswith("-") or argument == "update-wpt":
                continue

            match = GITHUB_ACTION_RUN_URL_REGEX.search(argument)
            if not match:
                downloaded_run_logs.append(argument)
                continue

            repository = match.group(1)
            run_id = match.group(2)
            run_path = os.path.join(directory, run_id)
            os.makedirs(run_path)

            print(f"Downloading unexpected stable results from run {run_id} of {repository}")
            if (
                subprocess.check_call(
                    [
                        "gh",
                        "run",
                        "download",
                        run_id,
                        "-R",
                        repository,
                        "--dir",
                        run_path,
                        "--pattern",
                        "*stable-unexpected-results-linux*",
                        "--pattern",
                        "*stable-unexpected-results-quickjs*",
                    ]
                )
                != 0
            ):
                print(f"Could not download artifact from run id {run_id}.")
                print("Is `gh` installed and authenticated?")
                return False
            downloaded_run_logs += glob.glob(os.path.join(run_path, "*", "stable-unexpected-results.log"))

        kwargs["run_log"] = downloaded_run_logs
        logger = setup_logging(kwargs, {"mach": sys.stdout})
        return WPTUpdate(logger, **kwargs).run() != exit_unclean


def create_parser() -> ArgumentParser:
    return wptcommandline.create_parser_update()


def parse_args_update(update_paths: list[str]) -> Namespace:
    parser = wptcommandline.create_parser_update()
    return parser.parse_args(update_paths)
