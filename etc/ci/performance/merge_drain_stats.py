#!/usr/bin/env python3
"""Merge QJS drain statistics from several testcases into CSV files.

Input: a directory containing

    stats-<testcase>.jsonl   one JSON line per thread/runtime (QJS_DRAIN_STATS_OUT)
    wall-<testcase>.txt      whole-process wall time in seconds (/usr/bin/time -f '%e')

Output: three CSV files in --out-dir (default: the input directory)

    drain-summary.csv   one row per DrainContext, plus an ALL row per thread
    drain-hist.csv      long format: one row per (thread, context, bucket)
    drain-skipped.csv   testcases without usable JSON, or without wall time

The JSONL format has no `final` field and no periodic snapshots: a line is written
only when a runtime is destroyed. A testcase killed by SIGKILL therefore has no
data and is recorded as skipped.
"""

import argparse
import json
from pathlib import Path

import pandas as pd

# Fixed order of DrainContext in the Rust output. Keep in sync with DRAIN_CONTEXTS.
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
# Number of histogram buckets in drain_stats.rs (HIST_BUCKETS).
HIST_BUCKETS = 252


def bucket_lo(index: int) -> int:
    """Lower bound of a histogram bucket; mirrors `bucket_lo` in drain_stats.rs.

    Also accepts index == HIST_BUCKETS so the last bucket can get an upper bound.
    """
    if index < 4:
        return index
    bits = index + 4
    exponent = bits // 4
    sub = bits % 4
    return (4 + sub) << (exponent - 2)


# Map lo_ns -> bucket index, so a sparse histogram can still be turned into intervals.
LO_TO_INDEX = {bucket_lo(i): i for i in range(HIST_BUCKETS)}


def read_wall_seconds(path: Path) -> float | None:
    """Read the number of seconds written by `/usr/bin/time -f '%e'`."""
    if not path.is_file():
        return None
    try:
        return float(path.read_text().strip())
    except ValueError:
        return None


def parse_stats_file(path: Path) -> list[dict]:
    """Parse a JSONL file into a list of JSON objects.

    Blank lines are ignored. Malformed lines are reported and skipped.
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


def summary_rows(testcase: str, record: dict, wall_seconds: float | None) -> list[dict]:
    """Build the seven context rows plus the ALL row for one JSON line."""
    thread = record.get("thread", "")
    pid = record.get("pid", 0)
    runtime = record.get("runtime", 0)

    # Index the JSON's drain entries by context name.
    by_context = {entry["ctx"]: entry for entry in record.get("drains", [])}
    base = {
        "testcase_name": testcase,
        "thread": thread,
        "pid": pid,
        "runtime": runtime,
    }

    # Wall time in milliseconds; used as the denominator of drain_pool(%).
    wall_ms = wall_seconds * 1000 if wall_seconds is not None else None

    rows = []
    total_calls = 0
    total_empty = 0
    total_ns = 0
    max_ns = 0
    total_values = 0

    for context in CONTEXTS:
        entry = by_context.get(context, {})
        calls = entry.get("calls", 0)
        empty = entry.get("empty", 0)
        context_ns = entry.get("total_ns", 0)
        context_max_ns = entry.get("max_ns", 0)
        values = entry.get("values", 0)

        total_calls += calls
        total_empty += empty
        total_ns += context_ns
        max_ns = max(max_ns, context_max_ns)
        total_values += values

        rows.append(
            {
                **base,
                "context": context,
                "calls": calls,
                "empty": empty,
                "total_time": round(context_ns / NS_PER_MS, 3),
                "max_time": round(context_max_ns / NS_PER_MS, 3),
                "value_count": values,
                "drain_pool(%)": (
                    round(context_ns / NS_PER_MS / wall_ms * 100, 4)
                    if wall_ms
                    else None
                ),
            }
        )

    # Pseudo-context ALL: the sum of the seven contexts for this thread.
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


def hist_rows(testcase: str, record: dict) -> list[dict]:
    """Build long-format histogram rows for one JSON line.

    `bucket` is a half-open interval. Its upper bound is the next non-empty
    bucket's lower bound; for the last one it is the next bucket's theoretical
    lower bound.
    """
    thread = record.get("thread", "")
    pid = record.get("pid", 0)
    runtime = record.get("runtime", 0)
    rows = []

    for entry in record.get("drains", []):
        context = entry["ctx"]
        # Sort by lower bound; the JSON may list buckets in any order.
        hist = sorted(entry.get("hist", []), key=lambda item: item["lo_ns"])
        total = sum(item["count"] for item in hist)
        if total == 0:
            continue

        for position, item in enumerate(hist):
            lo = item["lo_ns"]
            if position + 1 < len(hist):
                hi = hist[position + 1]["lo_ns"]
            else:
                index = LO_TO_INDEX.get(lo)
                hi = bucket_lo(index + 1) if index is not None else lo + 1
            rows.append(
                {
                    "testcase_name": testcase,
                    "context": context,
                    "thread": thread,
                    "pid": pid,
                    "runtime": runtime,
                    "bucket": f"[{lo}, {hi})",
                    "count": item["count"],
                    "percentage": round(item["count"] / total * 100, 4),
                }
            )
    return rows


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Merge QJS drain statistics JSONL files into CSV."
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
    args = parser.parse_args()

    stats_dir = args.directory
    out_dir = args.out_dir or stats_dir
    out_dir.mkdir(parents=True, exist_ok=True)

    # Discover testcases from both file kinds, so a wall file without stats is
    # still reported as skipped.
    testcases = set()
    for path in stats_dir.glob("stats-*.jsonl"):
        testcases.add(path.name[len("stats-"): -len(".jsonl")])
    for path in stats_dir.glob("wall-*.txt"):
        testcases.add(path.name[len("wall-"): -len(".txt")])

    all_summary = []
    all_hist = []
    skipped = []

    for testcase in sorted(testcases):
        stats_path = stats_dir / f"stats-{testcase}.jsonl"
        wall_path = stats_dir / f"wall-{testcase}.txt"
        wall_seconds = read_wall_seconds(wall_path)

        records = parse_stats_file(stats_path) if stats_path.is_file() else []
        if not records:
            # No runtime reached destruction: usually the process was killed.
            skipped.append(
                {
                    "testcase_name": testcase,
                    "wall_seconds": wall_seconds,
                    "reason": "timeout/no-data",
                }
            )
            continue

        if wall_seconds is None:
            skipped.append(
                {
                    "testcase_name": testcase,
                    "wall_seconds": None,
                    "reason": "no-wall",
                }
            )

        for record in records:
            all_summary.extend(summary_rows(testcase, record, wall_seconds))
            all_hist.extend(hist_rows(testcase, record))

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
