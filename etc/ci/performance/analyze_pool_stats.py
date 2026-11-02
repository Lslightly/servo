#!/usr/bin/env python3
"""Analyze QJS_POOL_STATS output: drain duration distribution, tables, Markdown report.

The input is the JSONL written by `mozjs/src/engine/pool_stats.rs` when servoshell is
built with `QJS_POOL_STATS=1`. Each line is a cumulative snapshot of one runtime; this
script keeps the last line per (file, pid, runtime), like `drain_stats.py` does.

Usage:
    python3 etc/ci/performance/analyze_pool_stats.py stats.jsonl
    python3 etc/ci/performance/analyze_pool_stats.py stats-*.jsonl.gz
    python3 etc/ci/performance/analyze_pool_stats.py --by-thread stats.jsonl
    python3 etc/ci/performance/analyze_pool_stats.py --report report.md stats.jsonl
    python3 etc/ci/performance/analyze_pool_stats.py --histogram task stats.jsonl
    python3 etc/ci/performance/analyze_pool_stats.py --csv stats.jsonl > drains.csv

What it reports:

1. An overview of every runtime (tasks, pool length, push counts).
2. One row per drain context (`scope_drop`, `native_call`, ..., `task`): calls, empty
   calls, released values, mean/p50/p90/p99/max duration, ns per value, and share of
   all drain time.
3. The duration histogram of one context, with bucket ranges and cumulative
   percentages. p50/p90/p99 are estimated from the histogram, not exact.
4. The top API call sites by how many values they pushed/pulled through the pool.
5. How API counts and drain time co-vary: Pearson r over the deltas between
   consecutive snapshots of the same runtime. Snapshots are cumulative, so a delta
   is what happened in that interval. One runtime gives one point per interval;
   many intervals make the correlation meaningful. This is aggregate co-variation,
   not a per-call attribution: a single snapshot cannot say which API caused a drain.

`drain` timing is not engine GC time: this measures `Cx::drain_pool`, and no cycle
collector (`JS_RunGC`, `gc_scan`, `gc_free_cycles`) is timed.
"""
import argparse
import collections
import gzip
import json
import math
import re
import sys

HIST_BUCKETS = 252

# Old binaries wrote `"hist":[buckets}` without closing the array: `[}`. Repairing is
# enough to read their data; rebuilding is the real fix.
REPAIR = re.compile(r'("hist":\[)((?:\{"lo_ns":\d+,"count":\d+\},?)*)\}')


def load_history(paths):
    """All records in file order per (file, pid, runtime): cumulative snapshots."""
    history = collections.defaultdict(list)
    repaired = 0
    for path in paths:
        opener = gzip.open if path.endswith(".gz") else open
        with opener(path, "rt", encoding="utf-8") as f:
            for n, line in enumerate(f):
                if not line.strip():
                    continue
                try:
                    r = json.loads(line)
                except json.JSONDecodeError:
                    fixed = REPAIR.sub(r"\1\2]}", line)
                    try:
                        r = json.loads(fixed)
                        repaired += 1
                    except json.JSONDecodeError as e:
                        sys.exit(f"{path}:{n + 1}: invalid JSON ({e})")
                history[(path, r.get("pid"), r.get("runtime"))].append(r)
    return history, repaired


def metrics(r):
    """Counter totals used for the per-runtime table and the correlations."""
    return {
        "pushes": sum(r.get("pushes", {}).values()),
        "api_total": sum(a.get("total", 0) for a in r.get("apis", [])),
        "drain_values": sum(d.get("values", 0) for d in r.get("drains", [])),
        "drain_ns": sum(d.get("total_ns", 0) for d in r.get("drains", [])),
    }


def pearson(xs, ys):
    n = len(xs)
    if n < 2:
        return None
    mx = sum(xs) / n
    my = sum(ys) / n
    sx = math.sqrt(sum((x - mx) ** 2 for x in xs))
    sy = math.sqrt(sum((y - my) ** 2 for y in ys))
    if sx == 0 or sy == 0:
        return None
    return sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / (sx * sy)


def delta_windows(history):
    """Deltas between consecutive snapshots of the same runtime; skips resets."""
    windows = []
    for recs in history.values():
        for prev, cur in zip(recs, recs[1:]):
            a, b = metrics(prev), metrics(cur)
            delta = {k: b[k] - a[k] for k in a}
            if all(v >= 0 for v in delta.values()):
                windows.append(delta)
    return windows


def bucket_lo(i):
    """Lower bound of bucket `i`, the inverse of `bucket()` in pool_stats.rs."""
    if i < 4:
        return i
    bits = i + 4
    return (4 + bits % 4) << (bits // 4 - 2)


LO = [bucket_lo(i) for i in range(HIST_BUCKETS)]


def fmt_ns(ns):
    if ns is None:
        return "∞"
    if ns < 1_000:
        return f"{ns} ns"
    if ns < 1_000_000:
        return f"{ns / 1_000:.2f} µs"
    if ns < 1_000_000_000:
        return f"{ns / 1_000_000:.2f} ms"
    return f"{ns / 1_000_000_000:.2f} s"


def quantile(buckets, p, max_ns):
    """Estimated p-quantile, assuming a uniform spread inside a bucket."""
    total = sum(buckets.values())
    if not total:
        return 0.0
    target = p * total
    seen = 0
    for lo in sorted(buckets):
        n = buckets[lo]
        if seen + n >= target:
            width = (LO[LO.index(lo) + 1] - lo) if LO.index(lo) + 1 < HIST_BUCKETS else 1
            return min(lo + (target - seen) / n * width, max_ns)
        seen += n
    return max_ns


def aggregate(records, group):
    """Merge drain counters per group ('ctx' or 'thread')."""
    agg = collections.defaultdict(collections.Counter)
    for r in records:
        for d in r.get("drains", []):
            key = (r.get("thread", "?"), d["ctx"]) if group == "thread" else d["ctx"]
            a = agg[key]
            for k in ("calls", "empty", "total_ns", "values"):
                a[k] += d[k]
            a["max_ns"] = max(a["max_ns"], d["max_ns"])
            for b in d["hist"]:
                a[b["lo_ns"]] += b["count"]
    return agg


def drain_rows(agg):
    grand = sum(a["total_ns"] for a in agg.values())
    rows = []
    for key, a in sorted(agg.items(), key=lambda kv: -kv[1]["total_ns"]):
        timed = a["calls"] - a["empty"]
        buckets = {k: v for k, v in a.items() if isinstance(k, int)}
        rows.append(
            {
                "group": key,
                "calls": a["calls"],
                "empty": a["empty"],
                "values": a["values"],
                "total_ms": a["total_ns"] / 1e6,
                "mean_us": a["total_ns"] / timed / 1e3 if timed else 0.0,
                "p50_us": quantile(buckets, 0.50, a["max_ns"]) / 1e3,
                "p90_us": quantile(buckets, 0.90, a["max_ns"]) / 1e3,
                "p99_us": quantile(buckets, 0.99, a["max_ns"]) / 1e3,
                "max_us": a["max_ns"] / 1e3,
                "ns_per_value": a["total_ns"] / a["values"] if a["values"] else 0.0,
                "time_pct": 100 * a["total_ns"] / grand if grand else 0.0,
                "slow_calls": sum(n for lo, n in buckets.items() if lo >= 1_000_000),
            }
        )
    return rows


def conclusions(records, rows, windows):
    """顶部结论：5 行，固定顺序。缺数据时明说不可用，不猜。"""
    total_ms = sum(metrics(r)["drain_ns"] for r in records) / 1e6
    elapsed = [r["elapsed_ms"] for r in records if isinstance(r.get("elapsed_ms"), (int, float))]
    if len(elapsed) == len(records) and sum(elapsed) > 0:
        share = 100 * total_ms / sum(elapsed)
        first = (f"1. 总 drain {total_ms:.1f} ms，占运行时间 {share:.1f}%"
                 f"（分母为墙钟时间，含空闲，是上限）。")
    else:
        first = f"1. 总 drain {total_ms:.1f} ms；缺 `elapsed_ms`，无法算占比。"

    top = rows[0] if rows else None
    if top and top["time_pct"] >= 50:
        second = f"2. 时间集中在 `{top['group']}`：{top['time_pct']:.1f}%。"
    elif top:
        second = (f"2. 没有单一 ctx 超过 50%；最高是 `{top['group']}`："
                  f"{top['time_pct']:.1f}%。")
    else:
        second = "2. 没有 drain 数据。"

    slow = sum(r["slow_calls"] for r in rows)
    max_ms = max((r["max_us"] for r in rows), default=0.0) / 1000
    if slow:
        third = f"3. 长尾：至少 {slow} 次 drain ≥ 1 ms；最长 {max_ms:.1f} ms。"
    else:
        third = f"3. 长尾：没有 ≥ 1 ms 的 drain；最长 {max_ms:.2f} ms。"

    dropped = sum(a.get("dropped", 0) for r in records for a in r.get("apis", []))
    tail = "（有值在 drain 时由 pool 释放最后一份）。" if dropped else "。"
    fourth = f"4. dropped = {dropped}{tail}"

    n = len(windows)
    fifth = (f"5. 相关性：{n} 个窗口，" +
             ("相关性不可用。" if n < 5 else "可参考。"))
    return "\n".join([first, second, third, fourth, fifth])


def source_section(paths, commands, records):
    lines = ["1. 生成 `stats.jsonl` 的 servoshell 命令：", ""]
    if commands:
        for cmd in commands:
            lines += ["```sh", cmd, "```", ""]
    else:
        lines += ["```sh", "# 用 --source 传入本页的 servoshell 命令", "```", ""]
    analysis = (f"python3 etc/ci/performance/analyze_pool_stats.py "
                f"--report report.md " + " ".join(paths))
    lines += ["2. 生成这份报告：", "", "```sh", analysis, "```", ""]
    if all(r.get("sample_every") == 0 for r in records):
        lines += ["3. `sample_every = 0`：采样回溯已关闭，`samples` 为空。"]
    return "\n".join(lines)


def per_window_rows(history, ctx):
    """Deltas between consecutive snapshots for one ctx: when did its drains happen?

    The JSONL has no timestamp, so the only time axis available is the snapshot index.
    """
    rows = []
    multi = len(history) > 1
    for recs in history.values():
        for i, (prev, cur) in enumerate(zip(recs, recs[1:]), 1):
            p = next((d for d in prev.get("drains", []) if d["ctx"] == ctx), None)
            c = next((d for d in cur.get("drains", []) if d["ctx"] == ctx), None)
            if p is None or c is None:
                continue
            calls = c["calls"] - p["calls"]
            if calls < 0:  # counter reset (a new table was built differently)
                continue
            pb = {b["lo_ns"]: b["count"] for b in p["hist"]}
            cb = {b["lo_ns"]: b["count"] for b in c["hist"]}
            delta = {k: cb.get(k, 0) - pb.get(k, 0) for k in set(pb) | set(cb)}
            delta = {k: v for k, v in delta.items() if v > 0}
            rows.append(
                {
                    "label": (f'快照{i}到快照{i + 1}'
                              + (f'（{cur.get("thread")}）' if multi else "")),
                    "calls": calls,
                    "empty": c["empty"] - p["empty"],
                    "values": c["values"] - p["values"],
                    "total_us": (c["total_ns"] - p["total_ns"]) / 1e3,
                    "p50_us": quantile(delta, 0.50, c["max_ns"]) / 1e3,
                    "p90_us": quantile(delta, 0.90, c["max_ns"]) / 1e3,
                }
            )
    return rows


def hist_rows(buckets):
    """(range, count, pct, cumulative pct) for the nonzero buckets."""
    total = sum(buckets.values())
    rows = []
    seen = 0
    for lo in sorted(buckets):
        n = buckets[lo]
        seen += n
        i = LO.index(lo)
        end = LO[i + 1] if i + 1 < HIST_BUCKETS else None
        rows.append((f"{fmt_ns(lo)} – {fmt_ns(end)}", n, 100 * n / total, 100 * seen / total))
    return rows


def table(header, rows, align=None):
    aligns = align or ["---"] * len(header)
    out = ["| " + " | ".join(header) + " |", "|" + "|".join(aligns) + "|"]
    out += ["| " + " | ".join(str(c) for c in row) + " |" for row in rows]
    return "\n".join(out)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("paths", nargs="+", help="QJS_POOL_STATS output (.jsonl[.gz])")
    parser.add_argument("--by-thread", action="store_true", help="also group by thread name")
    parser.add_argument("--histogram", default=None, metavar="CTX",
                        help="context to show the duration distribution for (default: the slowest)")
    parser.add_argument("--csv", action="store_true", help="print the drain table as CSV")
    parser.add_argument("--report", metavar="PATH", help="write a Markdown report to PATH")
    parser.add_argument("--source", action="append", default=[], metavar="CMD",
                        help="reproduction command; repeatable")
    args = parser.parse_args()

    history, repaired = load_history(args.paths)
    records = [recs[-1] for recs in history.values()]
    if not records:
        sys.exit("no records")
    if repaired:
        print(f"note: repaired {repaired} line(s) written by an older binary", file=sys.stderr)

    group = "thread" if args.by_thread else "ctx"
    agg = aggregate(records, group)
    if not agg:
        sys.exit("no `drains` field: rebuild with QJS_POOL_STATS=1")

    rows = drain_rows(agg)
    if args.csv:
        header = list(rows[0]) if rows else []
        print(",".join(header))
        for r in rows:
            print(",".join(str(r[h]) for h in header))
        return

    label = "thread / ctx" if group == "thread" else "ctx"
    drain_header = [label, "calls", "empty", "values", "total_ms", "mean_us",
                    "p50_us", "p90_us", "p99_us", "max_us", "ns/value", "time%"]
    fmt = [r["group"] if group == "ctx" else f'{r["group"][0]} / {r["group"][1]}' for r in rows]
    drain_table = table(
        drain_header,
        [
            [f, r["calls"], r["empty"], r["values"], f'{r["total_ms"]:.1f}',
             f'{r["mean_us"]:.2f}', f'{r["p50_us"]:.2f}', f'{r["p90_us"]:.2f}',
             f'{r["p99_us"]:.2f}', f'{r["max_us"]:.1f}', f'{r["ns_per_value"]:.1f}',
             f'{r["time_pct"]:.1f}']
            for f, r in zip(fmt, rows)
        ],
        ["---", "---:", "---:", "---:", "---:", "---:", "---:", "---:", "---:", "---:", "---:", "---:"],
    )

    # Overview, one line per runtime.
    overview_rows = []
    for r in records:
        m = metrics(r)
        overview_rows.append(
            [r.get("pid"), r.get("thread"), r.get("runtime"), r.get("final"),
             r.get("tasks"), r.get("max_len"),
             "{}/{}/{}/{}".format(*(r.get("pushes", {}).get(k, 0)
                                   for k in ("result", "borrowed", "root", "deferred"))),
             m["api_total"], m["drain_values"], f'{m["drain_ns"] / 1e6:.1f}',
             f'{m["drain_ns"] / m["drain_values"]:.1f}' if m["drain_values"] else "-"]
        )
    overview = table(
        ["pid", "thread", "runtime", "final", "tasks", "max_len", "pushes (r/b/root/d)",
         "apis total", "drain values", "drain ms", "ns/value"],
        overview_rows,
        ["---", "---", "---:", "---", "---:", "---:", "---:", "---:", "---:", "---:", "---:"],
    )

    # Co-variation between counts and drain time, over snapshot deltas.
    windows = delta_windows(history)
    conclusion = conclusions(records, rows, windows)
    source = source_section(args.paths, args.source, records)
    pairs = [
        ("API calls vs drain time", "api_total", "drain_ns"),
        ("pool pushes vs drain time", "pushes", "drain_ns"),
        ("values released vs drain time", "drain_values", "drain_ns"),
        ("API calls vs values released", "api_total", "drain_values"),
    ]
    corr_rows = []
    for name, x, y in pairs:
        r = pearson([w[x] for w in windows], [w[y] for w in windows])
        corr_rows.append([name, len(windows), "-" if r is None else f"{r:+.2f}"])
    corr_table = table(["pair", "windows", "Pearson r"], corr_rows, ["---", "---:", "---:"])

    # Distribution of the slowest context unless asked otherwise.
    ctx = args.histogram
    if ctx is None:
        best = max(agg, key=lambda k: agg[k]["total_ns"])
        ctx = best[1] if group == "thread" else best
    buckets = collections.Counter()
    for key, a in agg.items():
        if (key[1] if group == "thread" else key) == ctx:
            for k, v in a.items():
                if isinstance(k, int):
                    buckets[k] += v
    dist_table = table(
        ["duration bucket", "drains", "%", "cumulative %"],
        [[a, b, f"{c:.1f}", f"{d:.1f}"] for a, b, c, d in hist_rows(buckets)],
        ["---", "---:", "---:", "---:"],
    )

    # When did this ctx's drains happen? One row per interval between snapshots.
    win_rows = per_window_rows(history, ctx)
    window_table = table(
        ["window", "drains", "empty", "values", "total_us", "p50_us", "p90_us"],
        [
            [w["label"], w["calls"], w["empty"], w["values"],
             f'{w["total_us"]:.1f}', f'{w["p50_us"]:.2f}', f'{w["p90_us"]:.2f}']
            for w in win_rows
        ],
        ["---", "---:", "---:", "---:", "---:", "---:", "---:"],
    ) if win_rows else "_（只有一条快照，无法分窗口）_"

    gloss_window = "\n".join(
        [
            "**字段说明**",
            "",
            "1. `window` —— 快照 N 到快照 N+1 之间的区间，没有绝对时间。",
            "2. `drains` —— 这个区间内新增的 drain 次数。`0` 表示这个 ctx 已经停了。",
            "3. `empty` —— 其中池子为空的次数（不计时）。",
            "4. `values` —— 区间内释放的值的数量。",
            "5. `total_us` —— 区间内 drain 的总耗时。",
            "6. `p50_us` / `p90_us` —— 只用**这个区间**的直方图增量估算的分位数。",
            "",
            "**怎么用**：`drains` 一直为 0 → 该 ctx 不再发生；"
            "各窗口数值稳定 → 该 ctx 全程都在。",
        ]
    )

    apis = collections.defaultdict(collections.Counter)
    for r in records:
        for a in r.get("apis", []):
            c = apis[(a.get("api") or a.get("site") or "?", a["kind"])]
            for k in ("total", "fresh", "rooted", "kept", "dropped"):
                c[k] += a.get(k, 0)
    api_table = table(
        ["api / site", "kind", "total", "fresh", "rooted", "kept", "dropped"],
        [
            [k[0], k[1], c["total"], c["fresh"], c["rooted"], c["kept"], c["dropped"]]
            for k, c in sorted(apis.items(), key=lambda kv: -kv[1]["total"])[:10]
        ],
        ["---", "---", "---:", "---:", "---:", "---:", "---:"],
    )

    ctx_meanings = {
        "scope_drop": "`AutoreleasePoolScope` 被 drop",
        "native_call": "从 JS 调进来的 native / class 方法返回",
        "runtime_destroy": "runtime 销毁（`JS_DestroyContext`）",
        "proxy_trap": "proxy trap 返回",
        "job_callback": "`then()` / `await` 创建 reaction（`HostMakeJobCallback`）",
        "job_run": "promise job 取它的 incumbent global",
        "task": "一个任务结束，清空整个 pool（`drain_current_pool`）",
    }
    ctx_gloss = "\n".join(f"   - `{k}` —— {v}" for k, v in ctx_meanings.items())

    intro = (
        "**这是什么**：`servo-quickjs` **autorelease pool** 的入池与出池（drain）统计。"
        "值放进池里，等某个时机再统一释放。\n\n"
        "**这不是什么**：不是 QuickJS 引擎 GC 的暂停时间。代码没给 `JS_RunGC` / "
        "`gc_scan` / `gc_free_cycles` 计时。\n\n"
        "**怎么读**：先看 `Drain duration` 里谁占时间最多；再看 `Duration distribution` "
        "确认有没有尖峰；最后用 `Top API call sites` 找值是谁放进来的。"
    )

    gloss_runtimes = "\n".join(
        [
            "**字段说明**",
            "",
            "1. `pid` —— 进程号。多个测试进程追加到同一个文件时，靠它区分。",
            "2. `thread` —— 写这条记录的线程名（如 `Script#1`）。不同进程里可能重名。",
            "3. `runtime` —— 该进程内 QuickJS runtime 的序号。runtime 重建后会变。",
            "4. `final` —— `true` = runtime 销毁时写的最后一行；`false` = 只是中途快照，"
            "进程可能被 SIGKILL，数字还会增长。",
            "5. `tasks` —— 任务级 drain 的次数（每结束一个任务清空整个 pool 一次）。",
            "6. `max_len` —— pool 长度的峰值，即同时最多有多少个条目在池里。",
            "7. `pushes (r/b/root/d)` —— 累计入池次数，按来源分四类："
            "`r`=result（API 返回的新引用直接交给 pool），`b`=borrowed（借用值再保活一份），"
            "`root`=写进 Rooted / handle 根位置，`d`=deferred（计数读到 1，推迟释放）。",
            "8. `apis total` —— `apis[]` 里 `total` 之和，即这些 API 一共向 pool 放了多少个值"
            "（不同调用点的同名 API 已合并）。",
            "9. `drain values` —— 所有 drain 一共释放了多少个值。**不含 atom**。",
            "10. `drain ms` —— 所有 drain 的总耗时（毫秒）。空的 drain 不计时。",
            "11. `ns/value` —— 平均每释放一个值花多少 ns = `drain ms` 换算成 ns `/ drain values`。"
            "**不是**每次 API 调用的成本。",
        ]
    )

    gloss_drain = "\n".join(
        [
            "**字段说明**",
            "",
            "1. `ctx` —— drain 的触发原因，共 7 种：",
            ctx_gloss,
            "2. `calls` —— drain 被调用的总次数，**包含空 drain**。",
            "3. `empty` —— 池里没东西可释放的次数。空 drain 只计数、不计时。",
            "4. `values` —— 这些 drain 一共释放了多少个值（不含 atom）。",
            "5. `total_ms` —— 非空 drain 的总耗时（毫秒）。",
            "6. `mean_us` —— 平均每次**非空** drain 的耗时 = `total_ns` / (`calls` − `empty`)。",
            "7. `p50_us` / `p90_us` / `p99_us` —— 从直方图估算的分位数："
            "50% / 90% / 99% 的 drain 比这个快。桶内按均匀分布估算，所以是近似值。",
            "8. `max_us` —— 单次最长的一次 drain。",
            "9. `ns/value` —— 平均每释放一个值花多少 ns = `total_ns` / `values`。"
            "用来区分「次数多」还是「单次贵」。",
            "10. `time%` —— 该 ctx 占**总 drain 时间**的比例。"
            "分母是所有 drain 时间之和，**不是**程序运行时间。",
        ]
    )

    gloss_dist = ("\n".join(
        [
            "**字段说明**",
            "",
            "1. `duration bucket` —— 耗时区间。桶宽随数值增大而变宽（4 桶 / 2 的幂），"
            "所以表里区间宽度不一样。",
            "2. `drains` —— 落在这个区间的 drain 次数。",
            "3. `%` —— 占该 ctx 全部 drain 的百分比。",
            "4. `cumulative %` —— 小于等于该区间上界的累计百分比。",
            "",
            "**只看非空桶**：计数为 0 的区间被省略，因此相邻两行不代表区间相邻。",
        ]
    ))

    gloss_corr = "\n".join(
        [
            "**字段说明**",
            "",
            "1. `pair` —— 被比较的两个量。「A vs B」表示看 A 变多时 B 是否也变多。",
            "2. `windows` —— 参与计算的窗口数。每个窗口是**同一个 runtime 相邻两条快照的差值**。"
            "只有 1 条快照就没有窗口，r 显示 `-`。",
            "3. `Pearson r` —— 相关系数：`+1` 完全同向，`0` 无关，`-1` 完全反向。"
            "符号看方向，绝对值看强度。",
            "",
            "**限制**：每行 JSON 是累计值，所以只能比较窗口之间的共同变化；"
            "这不等于「某个 API 调用导致了某次 drain」。窗口少于 5 个时只能当提示，不要下结论。",
        ]
    )

    gloss_api = "\n".join(
        [
            "**字段说明**",
            "",
            "1. `api / site` —— API 函数名。同一 API 的不同调用点已合并成一行；"
            "没有 API 名时显示入池代码的位置（`文件:行号`）。",
            "2. `kind` —— 这个值以什么方式入池："
            "`result`（API 返回的新引用）、`borrowed`（借用值再加一份）。"
            "`root` / `deferred` 不在这张表里——它们在 drain 时不做 fate 判定。",
            "3. `total` —— 该 API 放进 pool 的值，在 drain 时被检查的总次数。",
            "4. `fresh` —— 入池时引用计数就是 1：说明它是新对象，之前没有别的持有者。",
            "5. `rooted` —— drain 时已经被写进根位置（`Rooted` / handle）。"
            "pool 这一份不是它最后一份。",
            "6. `kept` —— 没被 root，但 drain 时引用计数还大于池里的份数：别处还持有它。",
            "7. `dropped` —— 没被 root，且 pool 持有最后一份：这次 drain 会真正析构它。",
            "",
            "`rooted + kept + dropped = total`（按行成立）。",
        ]
    )

    premises = "\n".join(
        [
            "1. **drain ≠ 引擎 GC**。这里量的是 pool 出池的开销，不含 `JS_RunGC`。",
            "2. **数字是累计的**。中途快照会继续增长；分析时按 `(pid, runtime)` 取最后一行。",
            "3. **Dromaeo 结束会 SIGKILL**。别把 `QJS_POOL_STATS_SNAPSHOT_MS` 设成 0，"
            "否则可能一行都没有。短测试建议设 `1000`，这样相关性分析才有足够窗口。",
        ]
    )

    report = "\n\n".join(
        [
            "# QJS pool stats",
            intro,
            "## 结论\n\n" + conclusion,
            "## 数据来源与复现命令\n\n" + source,
            f"runtimes: {len(records)}" + (f" (repaired {repaired} old line(s))" if repaired else ""),
            "## Runtimes\n\n" + overview + "\n\n" + gloss_runtimes,
            "## Drain duration\n\n" + drain_table + "\n\n" + gloss_drain,
            f"## Duration distribution: `{ctx}`\n\n" + dist_table + "\n\n" + gloss_dist,
            f"## `{ctx}` 按时间窗口\n\n" + window_table + "\n\n" + gloss_window,
            "## Counts vs drain time\n\n" + corr_table + "\n\n" + gloss_corr,
            "## Top API call sites\n\n" + api_table + "\n\n" + gloss_api,
            "## 三个前提\n\n" + premises,
        ]
    )

    print(report)
    if args.report:
        with open(args.report, "w", encoding="utf-8") as f:
            f.write(report + "\n")
        print(f"\nreport written to {args.report}", file=sys.stderr)


if __name__ == "__main__":
    main()
