#!/usr/bin/env python3
"""Merge raw QJS drain records into CSV files.

Input: a directory containing

    stats-<testcase>.jsonl   one JSON line per drain_pool call (QJS_DRAIN_STATS_OUT)
    wall-<testcase>.txt      whole-process wall time in seconds (/usr/bin/time -f '%e')

Each JSON line looks like

    {"thread":"Script(1,1)","pid":123,"runtime":1,"ctx":"native_call",
     "elapsed_ns":1234,"values":5}

`elapsed_ns` is null for an empty drain (nothing was released, so it was not
timed). The Rust side does no aggregation: this script computes the counts,
totals, maxima and histograms.

The wall-*.txt files are optional. Without --wall, the script never reads them,
drain_pool(%) stays empty, and no testcase is skipped for a missing wall time.

Output: three CSV files in --out-dir (default: the input directory)

    drain-summary.csv   one row per DrainContext, plus an ALL row per runtime
    drain-hist.csv      long format: one row per (runtime, context, bucket)
    drain-skipped.csv   testcases without any drain record, or without wall time

Because every drain is written immediately and flushed, a process killed by
SIGKILL keeps the records written so far; such a testcase still produces
summary rows, computed over the part that ran.
"""

import argparse
import json
from collections import defaultdict
from pathlib import Path

import pandas as pd

# Fixed order of DrainContext in the Rust output. Keep in sync with DrainContext::name.
CONTEXTS = [
    "scope_drop",
    "native_call",
    "runtime_destroy",
    "proxy_trap",
    "job_callback",
    "job_run",
    "task",
]

SUMMARY_COLUMNS = [
    "testcase_name",
    "context",
    "thread",
    "pid",
    "runtime",
    "calls",
    "empty",
    "total_time",  # ms
    "max_time",    # ms
    "value_count",
    "drain_pool(%)",
]

HIST_COLUMNS = [
    "testcase_name",
    "context",
    "thread",
    "pid",
    "runtime",
    "bucket",
    "count",
    "percentage",
]

SKIPPED_COLUMNS = ["testcase_name", "wall_seconds", "reason"]

NS_PER_MS = 1_000_000


def bucket(ns: int) -> int:
    """Histogram bucket of a duration; mirrors `bucket` in the Rust producer.

    Below 4 ns each nanosecond is its own bucket; above that there are four
    buckets per power of two.
    """
    if ns < 4:
        return ns
    exponent = ns.bit_length() - 1
    return exponent * 4 + ((ns >> (exponent - 2)) & 0b11) - 4


def bucket_lo(index: int) -> int:
    """Smallest duration in `bucket(index)`; `bucket_lo(index + 1)` is its end."""
    if index < 4:
        return index
    bits = index + 4
    exponent = bits // 4
    sub = bits % 4
    return (4 + sub) << (exponent - 2)


def read_wall_seconds(path: Path) -> float | None:
    """Read the number of seconds written by `/usr/bin/time -f '%e'`."""
    if not path.is_file():
        return None
    try:
        return float(path.read_text().strip())
    except ValueError:
        return None


def parse_stats_file(path: Path) -> list[dict]:
    """Parse the JSONL file into a list of drain records.

    Blank lines are ignored. Malformed lines are reported and skipped; a process
    killed mid-write can leave one.
    """
    records = []
    for line_number, line in enumerate(path.read_text().splitlines(), start=1):
        line = line.strip()
        if not line:
            continue
        try:
            records.append(json.loads(line))
        except json.JSONDecodeError as error:
            print(f"warning: {path}:{line_number}: {error}")
    return records


def aggregate(records: list[dict]) -> dict[tuple, dict]:
    """Group drain records by runtime and context.

    Key: (thread, pid, runtime, context). Value: calls, empty, total_ns, max_ns,
    values, and a histogram of bucket index -> count.
    """
    groups = defaultdict(
        lambda: {
            "calls": 0,
            "empty": 0,
            "total_ns": 0,
            "max_ns": 0,
            "values": 0,
            "hist": defaultdict(int),
        }
    )
    for record in records:
        key = (
            record.get("thread", ""),
            record.get("pid", 0),
            record.get("runtime", 0),
            record.get("ctx", ""),
        )
        group = groups[key]
        group["calls"] += 1
        group["values"] += record.get("values", 0)

        elapsed_ns = record.get("elapsed_ns")
        if elapsed_ns is None:
            # Empty drain: counted, but not timed and not in the histogram.
            group["empty"] += 1
            continue
        group["total_ns"] += elapsed_ns
        group["max_ns"] = max(group["max_ns"], elapsed_ns)
        group["hist"][bucket(elapsed_ns)] += 1
    return groups


def summary_rows(testcase: str, groups: dict[tuple, dict], wall_seconds: float | None) -> list[dict]:
    """Build the seven context rows plus the ALL row for each runtime."""
    # Wall time in milliseconds; the denominator of drain_pool(%).
    wall_ms = wall_seconds * 1000 if wall_seconds is not None else None

    # Group contexts by runtime, keeping the fixed context order.
    runtimes: dict[tuple, dict[str, dict]] = defaultdict(dict)
    for (thread, pid, runtime, context), group in groups.items():
        runtimes[(thread, pid, runtime)][context] = group

    rows = []
    for (thread, pid, runtime), by_context in sorted(runtimes.items()):
        base = {
            "testcase_name": testcase,
            "thread": thread,
            "pid": pid,
            "runtime": runtime,
        }
        total_calls = 0
        total_empty = 0
        total_ns = 0
        max_ns = 0
        total_values = 0

        for context in CONTEXTS:
            group = by_context.get(context)
            if group is None:
                # No drain of this kind happened; still emit the row, all zero.
                group = {
                    "calls": 0,
                    "empty": 0,
                    "total_ns": 0,
                    "max_ns": 0,
                    "values": 0,
                }
            total_calls += group["calls"]
            total_empty += group["empty"]
            total_ns += group["total_ns"]
            max_ns = max(max_ns, group["max_ns"])
            total_values += group["values"]

            rows.append(
                {
                    **base,
                    "context": context,
                    "calls": group["calls"],
                    "empty": group["empty"],
                    "total_time": round(group["total_ns"] / NS_PER_MS, 3),
                    "max_time": round(group["max_ns"] / NS_PER_MS, 3),
                    "value_count": group["values"],
                    "drain_pool(%)": (
                        round(group["total_ns"] / NS_PER_MS / wall_ms * 100, 4)
                        if wall_ms
                        else None
                    ),
                }
            )

        # Pseudo-context ALL: the sum of the seven contexts for this runtime.
        rows.append(
            {
                **base,
                "context": "ALL",
                "calls": total_calls,
                "empty": total_empty,
                "total_time": round(total_ns / NS_PER_MS, 3),
                "max_time": round(max_ns / NS_PER_MS, 3),
                "value_count": total_values,
                "drain_pool(%)": (
                    round(total_ns / NS_PER_MS / wall_ms * 100, 4) if wall_ms else None
                ),
            }
        )
    return rows


def hist_rows(testcase: str, groups: dict[tuple, dict]) -> list[dict]:
    """Build long-format histogram rows.

    `bucket` is a half-open interval [lo, hi). `percentage` is the share of the
    non-empty drains of the same (runtime, context).
    """
    rows = []
    for (thread, pid, runtime, context), group in sorted(groups.items()):
        hist = group["hist"]
        total = sum(hist.values())
        if total == 0:
            continue
        for index in sorted(hist):
            lo = bucket_lo(index)
            hi = bucket_lo(index + 1)
            rows.append(
                {
                    "testcase_name": testcase,
                    "context": context,
                    "thread": thread,
                    "pid": pid,
                    "runtime": runtime,
                    "bucket": f"[{lo}, {hi})",
                    "count": hist[index],
                    "percentage": round(hist[index] / total * 100, 4),
                }
            )
    return rows


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Merge raw QJS drain records (JSONL) into CSV."
    )
    parser.add_argument(
        "directory",
        nargs="?",
        default=".",
        type=Path,
        help="directory with stats-*.jsonl and wall-*.txt (default: .)",
    )
    parser.add_argument(
        "--out-dir",
        type=Path,
        default=None,
        help="where to write the CSV files (default: the input directory)",
    )
    parser.add_argument(
        "--wall",
        action="store_true",
        help="read wall-*.txt and compute drain_pool(%%); off by default",
    )
    args = parser.parse_args()

    stats_dir = args.directory
    out_dir = args.out_dir or stats_dir
    out_dir.mkdir(parents=True, exist_ok=True)

    # Discover testcases from the stats files. Only with --wall are wall files
    # scanned too, so one without stats is reported as skipped.
    testcases = set()
    for path in stats_dir.glob("stats-*.jsonl"):
        testcases.add(path.name[len("stats-"): -len(".jsonl")])
    if args.wall:
        for path in stats_dir.glob("wall-*.txt"):
            testcases.add(path.name[len("wall-"): -len(".txt")])

    all_summary = []
    all_hist = []
    skipped = []

    for testcase in sorted(testcases):
        stats_path = stats_dir / f"stats-{testcase}.jsonl"
        wall_seconds = (
            read_wall_seconds(stats_dir / f"wall-{testcase}.txt") if args.wall else None
        )

        records = parse_stats_file(stats_path) if stats_path.is_file() else []
        if not records:
            # No drain ran at all: the page never executed JS, or died too early.
            skipped.append(
                {
                    "testcase_name": testcase,
                    "wall_seconds": wall_seconds,
                    "reason": "no-data",
                }
            )
            continue

        if args.wall and wall_seconds is None:
            # Still emit summary rows; drain_pool(%) is left empty.
            skipped.append(
                {
                    "testcase_name": testcase,
                    "wall_seconds": None,
                    "reason": "no-wall",
                }
            )

        groups = aggregate(records)
        all_summary.extend(summary_rows(testcase, groups, wall_seconds))
        all_hist.extend(hist_rows(testcase, groups))

    summary_df = pd.DataFrame(all_summary, columns=SUMMARY_COLUMNS)
    hist_df = pd.DataFrame(all_hist, columns=HIST_COLUMNS)
    skipped_df = pd.DataFrame(skipped, columns=SKIPPED_COLUMNS)

    summary_df.to_csv(out_dir / "drain-summary.csv", index=False)
    hist_df.to_csv(out_dir / "drain-hist.csv", index=False)
    skipped_df.to_csv(out_dir / "drain-skipped.csv", index=False)

    print(
        f"wrote {len(summary_df)} summary rows, {len(hist_df)} hist rows, "
        f"{len(skipped_df)} skipped testcases to {out_dir}"
    )


if __name__ == "__main__":
    main()
