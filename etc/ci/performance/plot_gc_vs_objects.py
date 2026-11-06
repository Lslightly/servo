#!/usr/bin/env python3
"""Plot GC time against the number of objects a GC collected.

Input: the per-GC JSONL files written by `runtime.rs` with `QJS_GC_STATS=1`,
one line per `JS_GC` holding `gc_time(us)` and the live heap before and after
the collection.

`collected = before.objects - after.objects`. Rows are grouped into bins of
`collected` (power-of-two values by default, with 0 as its own bin); within
each bin this plots the mean, p50, p99 and max of `gc_time(us)`, and the number
of GCs in the bin (`n=` under the tick and in the printed table).

Usage:
    QJS_GC_STATS=1 QJS_GC_STATS_OUT=out/qjs-gc.jsonl \\
        target/debug/servoshell out/demo.html --headless
    python etc/ci/performance/plot_gc_vs_objects.py out [-o gc-vs-objects.png]
"""

import argparse
import json
import math
import sys
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt  # noqa: E402
import numpy as np  # noqa: E402
from matplotlib import font_manager, ticker  # noqa: E402

# Categorical slots 1-4 of the reference palette, in fixed order.
COLORS = {
    "mean": "#2a78d6",
    "p50": "#eb6834",
    "p99": "#1baf7a",
    "max": "#eda100",
}
SERIES = ["mean", "p50", "p99", "max"]
# Vertical offsets (points) for the right-hand direct labels, which would otherwise
# pile up where the lines converge on a single-sample bin.
LABEL_OFFSET = {"mean": 22, "p50": -14, "p99": 44, "max": -36}
# Bins with at most this many samples get their exact values printed on the plot.
SPARSE_LABEL_MAX = 10
TIME_KEY = "gc_time(us)"
TEXT_PRIMARY = "#0b0b0b"
TEXT_SECONDARY = "#52514e"
GRID = "#d9d8d4"
SURFACE = "#fcfcfb"

CJK_FONTS = ["Noto Sans CJK SC", "WenQuanYi Zen Hei", "Droid Sans Fallback", "Source Han Sans SC"]


def use_cjk_font() -> bool:
    """Use a CJK font if one is installed; the caller picks English labels otherwise."""
    installed = {f.name for f in font_manager.fontManager.ttflist}
    for name in CJK_FONTS:
        if name in installed:
            # Some CJK fonts carry no Latin glyphs, so DejaVu goes first and the
            # CJK font fills in per glyph (matplotlib walks this list in order).
            plt.rcParams["font.family"] = ["DejaVu Sans", name]
            plt.rcParams["axes.unicode_minus"] = False
            return True
    return False


def load_records(paths: list[Path], thread: str | None) -> list[tuple[int, int]]:
    """Return (objects collected, gc_time_us) for every GC record.

    Lines without `gc_time(us)` (for example drain-stats lines in the same
    directory) are ignored. A GC whose object count went up is skipped: the
    delta is not a collected-object count.
    """
    files = []
    for path in paths:
        if path.is_dir():
            gc_files = sorted(path.glob("qjs-gc-*.jsonl"))
            files.extend(gc_files or sorted(path.glob("*.jsonl")))
        else:
            files.append(path)

    records = []
    negative = 0
    for path in files:
        for line_number, line in enumerate(path.read_text().splitlines(), start=1):
            line = line.strip()
            if not line or TIME_KEY not in line:
                continue
            try:
                record = json.loads(line)
                elapsed = record[TIME_KEY]
                before = record["before"]["objects"]
                after = record["after"]["objects"]
            except (json.JSONDecodeError, KeyError, TypeError) as error:
                print(f"warning: {path}:{line_number}: {error}", file=sys.stderr)
                continue
            if not isinstance(elapsed, int) or elapsed < 0:
                continue
            if not (isinstance(before, int) and isinstance(after, int)):
                continue
            if thread is not None and record.get("thread") != thread:
                continue
            collected = before - after
            if collected < 0:
                negative += 1
                continue
            records.append((collected, elapsed))

    if negative:
        print(
            f"warning: skipped {negative:,} GCs whose object count did not drop",
            file=sys.stderr,
        )
    if not records:
        raise SystemExit(
            "error: no usable GC records "
            f"(expected {TIME_KEY}, before.objects and after.objects)"
        )
    return records


def equal_count_bins(values: np.ndarray, count: int) -> list[tuple[int, int, int]]:
    """Split sorted `values` into ~`count` bins of equal size, on value boundaries.

    Returns inclusive (lo, hi) value ranges plus the bin's sample count.
    """
    distinct, counts = np.unique(values, return_counts=True)
    total = len(values)
    target = total / count

    bins = []
    lo = distinct[0]
    accumulated = 0
    for value, n in zip(distinct, counts):
        accumulated += int(n)
        hi = int(value)
        if accumulated >= target:
            bins.append((int(lo), hi, accumulated))
            lo = value + 1  # next distinct value starts the next bin
            accumulated = 0
    if accumulated:
        if bins and accumulated < target / 2:
            # Fold a small remainder into the previous bin.
            prev_lo, _, prev_n = bins[-1]
            bins[-1] = (prev_lo, int(distinct[-1]), prev_n + accumulated)
        else:
            bins.append((int(lo), int(distinct[-1]), accumulated))
    return bins


def log_bins(values: np.ndarray, min_count: int) -> list[tuple[int, int, int]]:
    """Power-of-two value ranges, with 0 as its own range.

    Ranges too sparse to be stable are carried forward.
    """
    top = int(values.max())
    spans = []
    zero = int(np.count_nonzero(values == 0))
    if zero:
        spans.append([0, 0, zero])

    edges = [1]
    while edges[-1] <= top:
        edges.append(edges[-1] * 2)
    for lo, hi in zip(edges, edges[1:]):
        hi = min(hi - 1, top)
        n = int(np.count_nonzero((values >= lo) & (values <= hi)))
        if n:
            spans.append([lo, hi, n])

    # Walk forward, closing a bin once it holds `min_count` samples.
    bins = []
    pending = None
    for lo, hi, n in spans:
        if pending is None:
            pending = [lo, hi, n]
        else:
            pending[1], pending[2] = hi, pending[2] + n
        if pending[2] >= min_count:
            bins.append(tuple(pending))
            pending = None
    if pending:
        if bins:
            lo, _, _ = bins[-1]
            bins[-1] = (lo, pending[1], bins[-1][2] + pending[2])
        else:
            bins.append(tuple(pending))
    return bins


def summarize(objects: np.ndarray, elapsed: np.ndarray, bins) -> dict[str, list[float]]:
    stats = {key: [] for key in SERIES}
    stats["count"] = []
    for lo, hi, _ in bins:
        mask = (objects >= lo) & (objects <= hi)
        sample = elapsed[mask]
        stats["count"].append(int(sample.size))
        stats["mean"].append(float(np.mean(sample)))
        stats["p50"].append(float(np.percentile(sample, 50)))
        stats["p99"].append(float(np.percentile(sample, 99)))
        stats["max"].append(float(np.max(sample)))
    return stats


def bin_label(lo: int, hi: int) -> str:
    return f"{lo}" if lo == hi else f"{lo}–{hi}"


def fmt_us(value: float) -> str:
    return f"{value:.2f}" if value < 10 else f"{value:.0f}"


def plot(bins, stats, out: Path, title: str, cjk: bool) -> None:
    labels = [bin_label(lo, hi) for lo, hi, _ in bins]
    x = np.arange(len(bins))

    fig, ax = plt.subplots(figsize=(10, 6), dpi=150)
    fig.patch.set_facecolor(SURFACE)
    ax.set_facecolor(SURFACE)

    for key in SERIES:
        y = np.maximum(stats[key], 0.001)
        ax.plot(
            x,
            y,
            color=COLORS[key],
            linewidth=2,
            marker="o",
            markersize=5,
            markeredgecolor=SURFACE,
            markeredgewidth=1.5,
            label=key,
            zorder=3,
        )
        # Direct label at the right end: yellow is below 3:1 contrast on this surface.
        # The lines converge on bins holding a single sample, so the labels are stacked.
        value = y[-1]
        shown = f"{value:.2f}" if value < 10 else f"{value:.0f}"
        ax.annotate(
            f"{key} {shown}µs",
            (x[-1], y[-1]),
            xytext=(10, LABEL_OFFSET[key]),
            textcoords="offset points",
            color=COLORS[key],
            fontsize=10,
            fontweight="bold",
            va="center",
        )

    # Sparse bins hold only a handful of samples; print each point's value so the
    # tail can be read without estimating positions on a log axis. The last bin is
    # skipped: its values already appear in the right-hand labels.
    for i in range(len(bins) - 1):
        if stats["count"][i] > SPARSE_LABEL_MAX:
            continue
        # Label only values that are visually distinct on the log axis; otherwise
        # two labels land on nearly the same spot and overprint each other.
        labelled = []
        for key in SERIES:
            value = stats[key][i]
            if any(abs(math.log(max(value, 1e-9) / other)) < math.log(1.15) for other in labelled):
                continue
            labelled.append(max(value, 1e-9))
            ax.annotate(
                fmt_us(value),
                (x[i], max(value, 0.001)),
                xytext=(7, 0),
                textcoords="offset points",
                ha="left",
                va="center",
                fontsize=7,
                color=COLORS[key],
                zorder=4,
            )

    ax.set_yscale("log")
    # Leave room on the right so the end labels stay inside the axes.
    ax.set_xlim(-0.4, len(bins) + 2.6)
    # Headroom for the stacked end labels, which sit above the highest line.
    bottom, top = ax.get_ylim()
    ax.set_ylim(bottom, top * 3)
    ax.set_xticks(x)
    ax.set_xticklabels(
        [f"{lab}\n(n={n:,})" for lab, n in zip(labels, stats["count"])], fontsize=8
    )

    xlabel = (
        "GC 回收的对象数量（2 的幂分箱，括号内为 GC 次数）"
        if cjk
        else "objects collected by the GC (power-of-two bins; n = GCs per bin)"
    )
    ax.set_xlabel(xlabel, color=TEXT_SECONDARY, fontsize=11)
    ax.set_ylabel("GC 耗时 / µs（对数轴）" if cjk else "GC time / µs (log scale)",
                  color=TEXT_SECONDARY, fontsize=11)
    ax.set_title(title, color=TEXT_PRIMARY, fontsize=13, loc="left", pad=14)

    ax.grid(axis="y", color=GRID, linewidth=0.8, zorder=0)
    ax.set_axisbelow(True)
    for side in ("top", "right"):
        ax.spines[side].set_visible(False)
    for side in ("left", "bottom"):
        ax.spines[side].set_color(GRID)
    ax.tick_params(colors=TEXT_SECONDARY, length=0)
    # Powers of ten alone leave one label per decade, so a range like 1000–10000
    # says nothing in between. Label 2x and 5x as major ticks as well.
    ax.yaxis.set_major_locator(ticker.LogLocator(base=10, subs=(1.0, 2.0, 5.0)))
    ax.yaxis.set_major_formatter(ticker.FuncFormatter(lambda v, _: f"{v:g}"))
    ax.yaxis.set_minor_locator(ticker.NullLocator())
    ax.yaxis.set_minor_formatter(ticker.NullFormatter())

    ax.legend(frameon=False, loc="upper left", labelcolor=TEXT_PRIMARY, fontsize=10)
    fig.tight_layout()
    fig.savefig(out, facecolor=SURFACE)
    svg = out.with_suffix(".svg")
    fig.savefig(svg, facecolor=SURFACE)
    plt.close(fig)
    print(f"wrote {out} and {svg}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("paths", nargs="+", type=Path, help="JSONL files or directories")
    parser.add_argument("-n", "--bins", type=int, default=8, help="target number of bins (default: 8)")
    parser.add_argument(
        "--binning",
        choices=["log", "quantile"],
        default="log",
        help="log: power-of-two collected counts; quantile: equal sample counts (default: log)",
    )
    parser.add_argument(
        "--min-count",
        type=int,
        default=0,
        help="fewest samples in a log bin before it is merged into the next (default: 0, keep every non-empty range)",
    )
    parser.add_argument("--thread", help="only this thread name, e.g. Script(1,1)")
    parser.add_argument("-o", "--out", type=Path, default=Path("gc-vs-objects.png"))
    args = parser.parse_args()

    records = load_records(args.paths, args.thread)
    objects = np.array([r[0] for r in records], dtype=np.int64)
    elapsed = np.array([r[1] for r in records], dtype=np.int64)

    order = np.argsort(objects)
    objects, elapsed = objects[order], elapsed[order]

    bins = (
        log_bins(objects, args.min_count)
        if args.binning == "log"
        else equal_count_bins(objects, args.bins)
    )
    stats = summarize(objects, elapsed, bins)

    cjk = use_cjk_font()
    scope = args.thread or "all threads"
    title = (
        f"GC 耗时 vs 回收对象数 · {scope} · {len(records):,} 次 GC"
        if cjk
        else f"GC time vs objects collected · {scope} · {len(records):,} GCs"
    )
    plot(bins, stats, args.out, title, cjk)

    print(f"{'collected':>12} {'GCs':>8} {'mean':>10} {'p50':>10} {'p99':>10} {'max':>10}  (µs)")
    for (lo, hi, _), i in zip(bins, range(len(bins))):
        print(
            f"{bin_label(lo, hi):>12} {stats['count'][i]:>8,}"
            f" {stats['mean'][i]:>10.2f} {stats['p50'][i]:>10.2f}"
            f" {stats['p99'][i]:>10.2f} {stats['max'][i]:>10.2f}"
        )


if __name__ == "__main__":
    main()
