#!/usr/bin/env python3
"""Plot a comparison of two or more pss-rounds.sh runs.

Usage:
    uv run --with pandas --with matplotlib python pss-compare.py a=a.csv b=b.csv -o compare.png
    # or, with pandas/matplotlib already installed:
    python3 pss-compare.py baseline=/path/to/baseline.csv lazy=/path/to/lazy.csv

The CSVs can come from different checkouts/machines: this script only takes file paths and
has no dependency on this repo. Each CSV is the output of pss-rounds.sh (columns
round,pss_kb,pss_anon_kb,...). Values are converted to MiB. Panels are small multiples sharing
the round axis; the two panes never share a y-scale, so no dual axis is involved. Use --panels
to add PSS file/shmem panes.

Colors are the validated categorical slots 1-3 (blue/orange/aqua, light surface #fcfcfb).
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
import pandas as pd
from matplotlib.lines import Line2D
from matplotlib.ticker import FuncFormatter, MaxNLocator

# Validated categorical palette (light surface). Fixed order, never cycled.
SERIES = ["#2a78d6", "#eb6834", "#1baf7a"]
SURFACE = "#fcfcfb"
INK = "#0b0b0b"
SECONDARY = "#52514e"
MUTED = "#898781"
GRID = "#e1e0d9"
AXIS = "#c3c2b7"

PANELS = {
    "pss": ("pss_kb", "PSS total"),
    "anon": ("pss_anon_kb", "PSS anon"),
    "file": ("pss_file_kb", "PSS file"),
    "shmem": ("pss_shmem_kb", "PSS shmem"),
}


def parse_run(spec: str) -> tuple[str, Path]:
    label, sep, path = spec.partition("=")
    if not sep:
        path, label = label, Path(label).stem
    p = Path(path)
    if not p.is_file():
        sys.exit(f"pss-compare: no such CSV: {p}")
    return label, p


def load_run(path: Path) -> pd.DataFrame:
    df = pd.read_csv(path)
    missing = {"round", "pss_kb"} - set(df.columns)
    if missing:
        sys.exit(f"pss-compare: {path} is missing column(s): {', '.join(sorted(missing))}")
    df = df.apply(pd.to_numeric, errors="coerce").dropna(subset=["round", "pss_kb"])
    df = df.sort_values("round").drop_duplicates("round", keep="last")
    return df.reset_index(drop=True)


def style_axes(ax: plt.Axes) -> None:
    ax.set_facecolor(SURFACE)
    ax.grid(True, axis="y", color=GRID, linewidth=0.8, zorder=0)
    ax.set_axisbelow(True)
    for side in ("top", "right"):
        ax.spines[side].set_visible(False)
    for side in ("left", "bottom"):
        ax.spines[side].set_color(AXIS)
        ax.spines[side].set_linewidth(0.8)
    ax.tick_params(colors=MUTED, labelcolor=SECONDARY, labelsize=9, length=3, width=0.8)
    ax.yaxis.set_major_locator(MaxNLocator(nbins=4))
    ax.yaxis.set_major_formatter(FuncFormatter(lambda v, _: f"{v:,.0f}"))
    ax.xaxis.set_major_locator(MaxNLocator(nbins=8, integer=True))


def place_end_labels(ax: plt.Axes, ends: list[tuple[float, float, str]], min_gap_px: float = 16) -> None:
    """Direct-label each line's last value; stack with leader lines when they collide."""
    if not ends:
        return
    fig = ax.figure
    fig.canvas.draw()
    dpi = fig.dpi
    disp = [ax.transData.transform((x, y)) for x, y, _ in ends]
    ys = [d[1] for d in disp]
    order = sorted(range(len(ends)), key=lambda i: ys[i], reverse=True)
    target = [0.0] * len(ends)
    prev = None
    for i in order:
        t = ys[i] if prev is None else min(ys[i], prev - min_gap_px)
        target[i] = t
        prev = t
    inv = ax.transData.inverted()
    x_last = ends[0][0]
    for i, (x, y, text) in enumerate(ends):
        y_data = inv.transform((disp[i][0], target[i]))[1]
        dy_pt = (target[i] - disp[i][1]) * 72.0 / dpi
        needs_leader = abs(target[i] - disp[i][1]) > 0.5
        ax.annotate(
            text,
            xy=(x, y),
            xytext=(9, dy_pt) if needs_leader else (9, 0),
            textcoords="offset points",
            ha="left",
            va="center",
            fontsize=9,
            color=SECONDARY,
            annotation_clip=False,
            arrowprops=(
                {"arrowstyle": "-", "color": AXIS, "linewidth": 0.8, "shrinkA": 0, "shrinkB": 3}
                if needs_leader
                else None
            ),
        )


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("runs", nargs="+", metavar="LABEL=CSV", help="CSV from pss-rounds.sh")
    ap.add_argument("-o", "--output", default="pss-compare.png", help="output image (default: pss-compare.png)")
    ap.add_argument(
        "--panels",
        default="total,anon",
        help=f"comma-separated panes: {', '.join(PANELS)} (default: total,anon; 'total' == 'pss')",
    )
    ap.add_argument("--title", default="PSS by round", help="chart title")
    args = ap.parse_args()

    if len(args.runs) > len(SERIES):
        sys.exit(f"pss-compare: at most {len(SERIES)} runs can be compared in one chart")

    runs = [(label, load_run(path)) for label, path in map(parse_run, args.runs)]

    panel_keys = [p.strip() for p in args.panels.split(",") if p.strip()]
    panel_keys = ["pss" if k == "total" else k for k in panel_keys]
    unknown = [k for k in panel_keys if k not in PANELS]
    if unknown:
        sys.exit(f"pss-compare: unknown panel(s): {', '.join(unknown)}")

    plt.rcParams.update(
        {
            "font.family": "sans-serif",
            "font.sans-serif": ["DejaVu Sans", "system-ui", "sans-serif"],
            "axes.unicode_minus": False,
        }
    )

    height = 2.9 * len(panel_keys) + 2.1
    fig, axes = plt.subplots(
        len(panel_keys), 1, sharex=True, figsize=(10, height), dpi=160,
        gridspec_kw={"hspace": 0.45},
    )
    axes = [axes] if len(panel_keys) == 1 else list(axes)
    fig.patch.set_facecolor(SURFACE)

    colors = {label: SERIES[i] for i, (label, _) in enumerate(runs)}

    for ax, key in zip(axes, panel_keys):
        column, title = PANELS[key]
        style_axes(ax)
        ax.set_title(title, loc="left", fontsize=11, color=SECONDARY, pad=8, fontweight="normal")
        ends: list[tuple[float, float, str]] = []
        for label, df in runs:
            if column not in df.columns:
                continue
            x = df["round"]
            y = df[column] / 1024.0
            color = colors[label]
            ax.plot(
                x, y, color=color, linewidth=2, solid_capstyle="round", solid_joinstyle="round",
                zorder=3, label=label,
            )
            if len(x):
                ax.plot(
                    [x.iloc[-1]], [y.iloc[-1]], marker="o", markersize=5,
                    markerfacecolor=color, markeredgecolor=SURFACE, markeredgewidth=2, zorder=4,
                )
                ends.append((float(x.iloc[-1]), float(y.iloc[-1]), f"{y.iloc[-1]:.1f} MiB"))
        ax.set_ylabel("MiB", color=SECONDARY, fontsize=9, labelpad=8)
        ax.margins(x=0.12)
        place_end_labels(ax, ends)

    axes[-1].set_xlabel("round (each round: GC'd, then sampled while idle)", color=SECONDARY, fontsize=9)
    fig.suptitle(args.title, x=0.06, y=0.985, ha="left", fontsize=14, color=INK, fontweight="bold")

    summary = []
    for label, df in runs:
        first, last = float(df["pss_kb"].iloc[0]) / 1024, float(df["pss_kb"].iloc[-1]) / 1024
        summary.append(f"{label}: {first:.1f} → {last:.1f} MiB (Δ {last - first:+.1f})")
    fig.text(0.06, 0.945, "     ".join(summary), color=SECONDARY, fontsize=9.5, ha="left", va="top")

    if len(runs) >= 2:
        handles = [Line2D([], [], color=colors[label], linewidth=2, label=label) for label, _ in runs]
        fig.legend(
            handles=handles, loc="upper right", bbox_to_anchor=(0.97, 0.99), ncol=len(runs),
            frameon=False, handlelength=1.6, fontsize=9.5, labelcolor=SECONDARY,
        )

    fig.text(
        0.06, 0.012,
        "Source: /proc/<pid>/smaps_rollup per round. PSS splits shared pages between mapping "
        "processes; it has no high-water mark.",
        color=MUTED, fontsize=8, ha="left",
    )
    fig.subplots_adjust(left=0.075, right=0.90, top=0.87, bottom=0.10)

    out = Path(args.output)
    fig.savefig(out, dpi=160, facecolor=SURFACE)
    plt.close(fig)
    print(f"wrote {out}")
    for line in summary:
        print(line)


if __name__ == "__main__":
    main()
