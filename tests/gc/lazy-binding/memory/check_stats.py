#!/usr/bin/env python3
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

"""Checks the memory-stats CSV produced by one lazy-binding memory page.

`runtime.memory_stats()` appends one row per call to `SERVO_MEMORY_STATS_PATH`
and returns undefined, so pages cannot assert the JSObject/pool numbers
themselves. Each page prints `[STATS] <label>` right after a sample; this script
pairs the i-th label with the i-th CSV row and checks the count relationships
that the page's explanation promises.

Usage: check_stats.py <page-name> <csv> <log>

Exits non-zero if the CSV cannot be paired with the labels or a check fails.
The engine appends two extra rows when the runtime is destroyed; they are
ignored.
"""

import csv
import sys

COLUMNS = [
    "obj_count",
    "obj_size",
    "dom_stub_count",
    "dom_stub_size",
    "prop_count",
    "prop_size",
    "pool_count",
    "pool_size",
]

# Number of nodes each page creates; keep in sync with the pages.
N_CREATE = 100  # 02
N_PARSER = 200  # 03
N_MIXED_JS = 50  # 04
N_MIXED_PARSER = 70  # 04
N_ROOT_CHILDREN = 64  # 05


class Failure(Exception):
    pass


def read_labels(log_path):
    labels = []
    with open(log_path, encoding="utf-8", errors="replace") as log:
        for line in log:
            if line.startswith("[STATS] "):
                labels.append(line[len("[STATS] ") :].strip())
    return labels


def read_rows(csv_path, count):
    with open(csv_path, newline="", encoding="utf-8") as csv_file:
        reader = csv.DictReader(csv_file)
        rows = []
        for row in reader:
            rows.append({name: int(row[name] or 0) for name in COLUMNS})
            if len(rows) == count:
                break
    return rows


def delta(after, before, column):
    return after[column] - before[column]


def require(rows, label):
    try:
        return rows[label]
    except KeyError:
        raise Failure(f"page never sampled {label!r} (labels: {', '.join(rows)})")


def between(name, actual, low, high):
    return (low <= actual <= high, f"{name}: expected [{low}, {high}], got {actual}")


def equal(name, actual, expected):
    return (actual == expected, f"{name}: expected {expected}, got {actual}")


def check_01(rows):
    sample = require(rows, "static-parsed")
    stubs = sample["dom_stub_count"]
    yield between("every static element has a stub", stubs, 120960, 121000)
    yield (
        sample["obj_count"] * 10 < stubs,
        f"lazy binding keeps objects below a tenth of the stubs "
        f"(obj_count {sample['obj_count']} vs dom_stub_count {stubs})",
    )
    yield equal("stub size is 32 bytes each", sample["dom_stub_size"], stubs * 32)


def check_02(rows):
    base = require(rows, "create-baseline")
    created = require(rows, "create-created")
    deleted = require(rows, "create-deleted")

    yield equal(
        "createElement creates one JSObject per node",
        delta(created, base, "obj_count"),
        N_CREATE,
    )
    yield equal(
        "createElement creates no net DOM stub",
        delta(created, base, "dom_stub_count"),
        0,
    )
    yield between(
        "deletion returns obj_count to the baseline",
        delta(deleted, base, "obj_count"),
        -2,
        2,
    )
    yield equal(
        "deletion returns dom_stub_count to the baseline",
        delta(deleted, base, "dom_stub_count"),
        0,
    )


def check_03(rows):
    base = require(rows, "parser-baseline")
    inserted = require(rows, "parser-inserted")
    queried = require(rows, "parser-queried")
    deleted = require(rows, "parser-deleted")

    yield between(
        "the parser creates a stub per element",
        delta(inserted, base, "dom_stub_count"),
        N_PARSER,
        N_PARSER + 5,
    )
    yield between(
        "parsing creates no JSObjects",
        delta(inserted, base, "obj_count"),
        0,
        2,
    )
    yield equal(
        "querying one parsed node materializes exactly one JSObject",
        delta(queried, inserted, "obj_count"),
        1,
    )
    yield equal(
        "querying one parsed node frees exactly its stub",
        delta(queried, inserted, "dom_stub_count"),
        -1,
    )
    yield between(
        "deletion returns obj_count to the baseline",
        delta(deleted, base, "obj_count"),
        -2,
        2,
    )
    yield between(
        "deletion returns dom_stub_count to the baseline",
        delta(deleted, base, "dom_stub_count"),
        -1,
        1,
    )


def check_04(rows):
    base = require(rows, "mixed-baseline")
    after_js = require(rows, "mixed-after-js")
    after_parser = require(rows, "mixed-after-parser")
    after_query = require(rows, "mixed-after-query")
    deleted = require(rows, "mixed-after-delete")

    yield equal(
        "createElement contributes one JSObject per node",
        delta(after_js, base, "obj_count"),
        N_MIXED_JS,
    )
    yield equal(
        "createElement contributes no net stub",
        delta(after_js, base, "dom_stub_count"),
        0,
    )
    yield between(
        "the parser contributes a stub per element",
        delta(after_parser, after_js, "dom_stub_count"),
        N_MIXED_PARSER,
        N_MIXED_PARSER + 5,
    )
    yield between(
        "parsing adds no JSObjects",
        delta(after_parser, after_js, "obj_count"),
        0,
        2,
    )
    yield equal(
        "the queried parsed node materializes one JSObject",
        delta(after_query, after_parser, "obj_count"),
        1,
    )
    yield equal(
        "the queried parsed node frees its stub",
        delta(after_query, after_parser, "dom_stub_count"),
        -1,
    )
    yield between(
        "deletion returns obj_count to the baseline",
        delta(deleted, base, "obj_count"),
        -2,
        2,
    )
    yield between(
        "deletion returns dom_stub_count to the baseline",
        delta(deleted, base, "dom_stub_count"),
        -1,
        1,
    )


def check_05(rows):
    base = require(rows, "root-baseline")
    parsed = require(rows, "root-parsed")
    queried = require(rows, "root-queried")
    deleted = require(rows, "root-deleted")

    yield between(
        "the parser creates the root and child stubs",
        delta(parsed, base, "dom_stub_count"),
        N_ROOT_CHILDREN + 1,
        N_ROOT_CHILDREN + 1 + 5,
    )
    yield between(
        "parsing the subtree creates no JSObjects",
        delta(parsed, base, "obj_count"),
        0,
        2,
    )
    yield equal(
        "querying the root materializes exactly the root",
        delta(queried, parsed, "obj_count"),
        1,
    )
    yield equal(
        "querying the root frees exactly the root's stub",
        delta(queried, parsed, "dom_stub_count"),
        -1,
    )
    yield between(
        "the children stay stubs after only the root is queried",
        delta(queried, base, "dom_stub_count"),
        N_ROOT_CHILDREN,
        N_ROOT_CHILDREN + 5,
    )
    yield between(
        "deletion returns obj_count to the baseline",
        delta(deleted, base, "obj_count"),
        -2,
        2,
    )
    yield between(
        "deletion returns dom_stub_count to the baseline",
        delta(deleted, base, "dom_stub_count"),
        -1,
        1,
    )


CHECKS = {
    "01-static-rainbow": check_01,
    "02-create-element": check_02,
    "03-parser-insertadjacenthtml": check_03,
    "04-mixed": check_04,
    "05-root-object-children-stubs": check_05,
}


def print_table(page, samples):
    print(f"    memory stats for {page}:")
    header = "      {:<28}".format("checkpoint") + "".join(
        f"{name:>14}" for name in COLUMNS
    )
    print(header)
    for label, row in samples.items():
        print(
            "      {:<28}".format(label)
            + "".join(f"{row[name]:>14}" for name in COLUMNS)
        )


def main():
    if len(sys.argv) != 4:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    page, csv_path, log_path = sys.argv[1:]

    labels = read_labels(log_path)
    if not labels:
        print(f"[FAIL] memory-stats {page}: the page printed no [STATS] markers")
        return 1

    try:
        rows = read_rows(csv_path, len(labels))
    except FileNotFoundError:
        print(f"[FAIL] memory-stats {page}: no CSV at {csv_path}")
        return 1
    except (csv.Error, KeyError, ValueError) as error:
        print(f"[FAIL] memory-stats {page}: malformed CSV {csv_path}: {error}")
        return 1

    if len(rows) < len(labels):
        print(
            f"[FAIL] memory-stats {page}: {len(labels)} [STATS] markers but only "
            f"{len(rows)} CSV rows"
        )
        return 1
    samples = dict(zip(labels, rows))
    print_table(page, samples)

    check = CHECKS.get(page)
    if check is None:
        print(f"[FAIL] memory-stats {page}: no checks defined for this page")
        return 1

    failed = 0
    try:
        results = list(check(samples))
    except Failure as error:
        print(f"[FAIL] memory-stats {page}: {error}")
        return 1
    for ok, message in results:
        print(("    [PASS] " if ok else "    [FAIL] ") + message)
        failed += not ok

    if failed:
        print(f"[FAIL] memory-stats {page}: {failed} check(s) failed")
        return 1
    print(f"[PASS] memory-stats {page}: {len(results)} checks")
    return 0


if __name__ == "__main__":
    sys.exit(main())
