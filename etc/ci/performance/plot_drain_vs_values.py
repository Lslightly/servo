#!/usr/bin/env python3
"""Plot drain time against the number of values a drain removed.

Input: the per-drain JSONL files written by `drain_stats.rs`
(`DRAIN_STATS=1`), one line per drain with `elapsed_ns`, `values`, and the
`free(==1)` / `free(>=2)` split.

Rows are grouped into equal-count bins of `values`. The upper panel plots the
mean, p50, p99 and max of `elapsed_ns`; the lower panel stacks the mean
`free(==1)` (the last reference, so the JS object dies with the free) under the
mean `free(>=2)` (still shared, only the refcount drops). The bar height is the
mean `values` per drain, so the lower panel shows both the absolute number of
frees and the share that actually drops objects; each bar is labelled with that
share.

Equal-count binning is done on *distinct* values, never inside a run of equal
values: in a real capture most drains remove the same handful of values, so a
plain quantile cut would collapse into a few bins. A value whose count already
exceeds the target becomes its own bin.

Files written before `free(==1)` / `free(>=2)` were added have neither field;
those records still count towards the upper panel and are skipped by the lower
one.

Usage:
    python etc/ci/performance/plot_drain_vs_values.py target/debug/out [-o chart.png]
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
# The lower panel is part-to-whole, not four identities: emphasize the frees that
# drop an object (slot 1) and leave the still-shared rest in muted gray.
FREE_LAST_COLOR = COLORS["mean"]
FREE_SHARED_COLOR = "#898781"
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


def load_records(paths: list[Path], context: str | None) -> list[tuple[int, int, int | None, int | None]]:
    """Return (values, elapsed_ns, free(==1), free(>=2)) for every timed drain.

    The free counts are `None` for records written before those fields existed;
    empty drains are skipped.
    """
    files = []
    for path in paths:
        if path.is_dir():
            files.extend(sorted(path.glob("*.jsonl")))
        else:
            files.append(path)

    records = []
    for path in files:
        for line_number, line in enumerate(path.read_text().splitlines(), start=1):
            line = line.strip()
            if not line:
                continue
            try:
                record = json.loads(line)
                elapsed = record["elapsed_ns"]
                values = record["values"]
            except (json.JSONDecodeError, KeyError) as error:
                print(f"warning: {path}:{line_number}: {error}", file=sys.stderr)
                continue
            if elapsed is None or values is None or values <= 0 or elapsed < 0:
                continue
            if context is not None and record.get("ctx") != context:
                continue
            last = record.get("free(==1)")
            shared = record.get("free(>=2)")
            if not (isinstance(last, int) and isinstance(shared, int)) or last < 0 or shared < 0:
                last = shared = None
            records.append((int(values), int(elapsed), last, shared))

    if not records:
        raise SystemExit("error: no usable drain records")
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
    """Power-of-two value ranges; ranges too sparse to be stable are carried forward."""
    top = int(values.max())
    edges = [1]
    edge = 1
    while edge <= top:
        edge *= 2
        edges.append(edge)

    # Each non-empty power-of-two range, before merging.
    spans = []
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


def summarize(
    values: np.ndarray,
    elapsed: np.ndarray,
    free_last: np.ndarray,
    free_shared: np.ndarray,
    bins,
) -> dict[str, list[float]]:
    stats = {key: [] for key in SERIES}
    stats["count"] = []
    stats["free_count"] = []
    stats["last"] = []
    stats["shared"] = []
    for lo, hi, _ in bins:
        mask = (values >= lo) & (values <= hi)
        sample = elapsed[mask]
        stats["count"].append(int(sample.size))
        stats["mean"].append(float(np.mean(sample)) / 1000)
        stats["p50"].append(float(np.percentile(sample, 50)) / 1000)
        stats["p99"].append(float(np.percentile(sample, 99)) / 1000)
        stats["max"].append(float(np.max(sample)) / 1000)

        # Records without the free split are skipped here, not counted as zero.
        last = free_last[mask]
        shared = free_shared[mask]
        known = ~np.isnan(last)
        stats["free_count"].append(int(known.sum()))
        if known.any():
            stats["last"].append(float(np.mean(last[known])))
            stats["shared"].append(float(np.mean(shared[known])))
        else:
            stats["last"].append(float("nan"))
            stats["shared"].append(float("nan"))
    return stats


def bin_label(lo: int, hi: int) -> str:
    return f"{lo}" if lo == hi else f"{lo}–{hi}"


def fmt_us(value: float) -> str:
    return f"{value:.2f}" if value < 10 else f"{value:.0f}"


def plot(bins, stats, out: Path, title: str, cjk: bool) -> None:
    labels = [bin_label(lo, hi) for lo, hi, _ in bins]
    x = np.arange(len(bins))
    tick_labels = [f"{lab}\n(n={n:,})" for lab, n in zip(labels, stats["count"])]

    free_last = np.asarray(stats["last"], dtype=float)
    free_shared = np.asarray(stats["shared"], dtype=float)
    has_free = not np.isnan(free_last).all()

    if has_free:
        fig, (ax_time, ax_free) = plt.subplots(
            2,
            1,
            sharex=True,
            figsize=(10, 9.6),
            dpi=150,
            gridspec_kw={"height_ratios": [1.35, 1]},
        )
    else:
        fig, ax_time = plt.subplots(figsize=(10, 6), dpi=150)
        ax_free = None
    fig.patch.set_facecolor(SURFACE)
    ax_time.set_facecolor(SURFACE)

    for key in SERIES:
        y = np.maximum(stats[key], 0.001)
        ax_time.plot(
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
        ax_time.annotate(
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
            ax_time.annotate(
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

    ax_time.set_yscale("log")
    # Leave room on the right so the end labels stay inside the axes.
    ax_time.set_xlim(-0.4, len(bins) + 2.6)
    # Headroom for the stacked end labels, which sit above the highest line.
    bottom, top = ax_time.get_ylim()
    ax_time.set_ylim(bottom, top * 3)
    ax_time.set_xticks(x)
    if ax_free is None:
        ax_time.set_xticklabels(tick_labels, fontsize=8)
    else:
        ax_time.tick_params(labelbottom=False)

    xlabel = (
        "移除的 value 数量（2 的幂分箱，括号内为 drain 次数）"
        if cjk
        else "values removed (power-of-two bins; n = drains per bin)"
    )
    ax_time.set_ylabel("drain 耗时 / µs（对数轴）" if cjk else "drain time / µs (log scale)",
                       color=TEXT_SECONDARY, fontsize=11)
    ax_time.set_title(title, color=TEXT_PRIMARY, fontsize=13, loc="left", pad=14)

    ax_time.grid(axis="y", color=GRID, linewidth=0.8, zorder=0)
    ax_time.set_axisbelow(True)
    for side in ("top", "right"):
        ax_time.spines[side].set_visible(False)
    for side in ("left", "bottom"):
        ax_time.spines[side].set_color(GRID)
    ax_time.tick_params(colors=TEXT_SECONDARY, length=0)
    ax_time.yaxis.set_major_formatter(ticker.FuncFormatter(lambda v, _: f"{v:g}"))
    ax_time.yaxis.set_minor_formatter(ticker.NullFormatter())
    ax_time.legend(frameon=False, loc="upper left", labelcolor=TEXT_PRIMARY, fontsize=10)

    if ax_free is not None:
        known = ~np.isnan(free_last)
        ax_free.set_facecolor(SURFACE)
        ax_free.bar(
            x[known],
            free_last[known],
            width=0.62,
            color=FREE_LAST_COLOR,
            edgecolor=SURFACE,
            linewidth=1.2,
            zorder=3,
            label=(
                "free(==1)：最后一个引用，对象被回收"
                if cjk
                else "free(==1): last ref, object freed"
            ),
        )
        ax_free.bar(
            x[known],
            free_shared[known],
            bottom=free_last[known],
            width=0.62,
            color=FREE_SHARED_COLOR,
            edgecolor=SURFACE,
            linewidth=1.2,
            zorder=3,
            label=(
                "free(>=2)：仍有其他引用，只减计数"
                if cjk
                else "free(>=2): still shared, refcount only"
            ),
        )

        # The bar height already gives the absolute mean `values`; the share is
        # the one number the stacked segments cannot be read off precisely.
        for i in np.flatnonzero(known):
            total = free_last[i] + free_shared[i]
            if total <= 0:
                continue
            ax_free.annotate(
                f"{free_last[i] / total:.0%}",
                (x[i], total),
                xytext=(0, 5),
                textcoords="offset points",
                ha="center",
                va="bottom",
                fontsize=8,
                color=TEXT_SECONDARY,
            )

        tallest = float(np.nanmax(free_last + free_shared))
        ax_free.set_ylim(0, tallest * 1.2 if tallest > 0 else 1)
        ax_free.set_ylabel(
            "每个 drain 平均释放的 value 数（百分比 = free(==1) 占比）"
            if cjk
            else "values freed per drain, mean\n(percentage = free(==1) share)",
            color=TEXT_SECONDARY,
            fontsize=11,
        )
        ax_free.set_xlabel(xlabel, color=TEXT_SECONDARY, fontsize=11)
        ax_free.grid(axis="y", color=GRID, linewidth=0.8, zorder=0)
        ax_free.set_axisbelow(True)
        for side in ("top", "right"):
            ax_free.spines[side].set_visible(False)
        for side in ("left", "bottom"):
            ax_free.spines[side].set_color(GRID)
        ax_free.tick_params(colors=TEXT_SECONDARY, length=0)
        ax_free.yaxis.set_major_formatter(ticker.FuncFormatter(lambda v, _: f"{v:g}"))
        ax_free.set_xticks(x)
        ax_free.set_xticklabels(tick_labels, fontsize=8)
        ax_free.legend(frameon=False, loc="upper left", labelcolor=TEXT_PRIMARY, fontsize=10)
    else:
        ax_time.set_xlabel(xlabel, color=TEXT_SECONDARY, fontsize=11)

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
        help="log: power-of-two value ranges; quantile: equal sample counts (default: log)",
    )
    parser.add_argument(
        "--min-count",
        type=int,
        default=0,
        help="fewest samples in a log bin before it is merged into the next (default: 0, keep every non-empty range)",
    )
    parser.add_argument("--context", help="only this DrainContext, e.g. native_call")
    parser.add_argument("-o", "--out", type=Path, default=Path("drain-vs-values.png"))
    args = parser.parse_args()

    records = load_records(args.paths, args.context)
    values = np.array([r[0] for r in records], dtype=np.int64)
    elapsed = np.array([r[1] for r in records], dtype=np.int64)
    free_last = np.array([np.nan if r[2] is None else r[2] for r in records], dtype=float)
    free_shared = np.array([np.nan if r[3] is None else r[3] for r in records], dtype=float)

    missing_free = int(np.isnan(free_last).sum())
    if missing_free:
        print(
            f"warning: {missing_free:,} of {len(records):,} drains have no "
            "free(==1)/free(>=2) fields; the lower panel skips them",
            file=sys.stderr,
        )

    order = np.argsort(values)
    values, elapsed = values[order], elapsed[order]
    free_last, free_shared = free_last[order], free_shared[order]

    bins = (
        log_bins(values, args.min_count)
        if args.binning == "log"
        else equal_count_bins(values, args.bins)
    )
    stats = summarize(values, elapsed, free_last, free_shared, bins)

    cjk = use_cjk_font()
    scope = args.context or "all contexts"
    title = (
        f"drain 耗时 vs 移除的 value 数 · {scope} · {len(records):,} 次 drain"
        if cjk
        else f"Drain time vs values removed · {scope} · {len(records):,} drains"
    )
    plot(bins, stats, args.out, title, cjk)

    print(
        f"{'values':>12} {'n':>8} {'mean':>10} {'p50':>10} {'p99':>10} {'max':>10}"
        f" {'free==1':>9} {'free>=2':>9} {'last%':>7}  (µs; free cols = count/drain)"
    )
    for (lo, hi, _), i in zip(bins, range(len(bins))):
        last = stats["last"][i]
        shared = stats["shared"][i]
        if math.isnan(last):
            free_cells = f" {'—':>9} {'—':>9} {'—':>7}"
        else:
            total = last + shared
            share = last / total if total > 0 else 0.0
            free_cells = f" {last:>9.2f} {shared:>9.2f} {share:>6.0%}"
        print(
            f"{bin_label(lo, hi):>12} {stats['count'][i]:>8,}"
            f" {stats['mean'][i]:>10.2f} {stats['p50'][i]:>10.2f}"
            f" {stats['p99'][i]:>10.2f} {stats['max'][i]:>10.2f}{free_cells}"
        )


if __name__ == "__main__":
    main()
