# QJS Pool / GC 可观测性实施计划

> **采用新的elapsed计算方式，内容待更新。**
> 旧的 Rust 侧 `elapsed_ms` 已删除；当前占比用外部 `/usr/bin/time` + `--wall-seconds` / `--wall-ms`。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给 pool stats 补上“占程序执行时间多少”（外部墙钟分母）和自动结论，先确认 dom-modify 是否触发引擎 GC，再决定是否给 `JS_GC` 加计时并扩大负载。

**Architecture:** 两个仓库配合。`servo-quickjs` 负责插桩：`pool_stats.rs` 写 JSONL，`runtime.rs` / `misc.rs` 是 GC 入口。`servo-ohos` 负责分析：`analyze_pool_stats.py` 读 JSONL，生成带结论的 Markdown。顺序是“先测量、后判读、最后才谈优化”；每个测量任务都产出可复现的数字。

**Tech Stack:** Rust（mozjs / servo-quickjs）、Python 3 标准库、JSONL、Dromaeo。

**Spec:** `/home/l00982027/mygit/qjs-diff/report.md`（现有数据与字段说明）；本次会话确定的目标与顺序：补分母 → 自动结论 → 验证 GC → GC 计时 → 多负载。

## Global Constraints

1. **默认构建不变**：所有新插桩都在 `#[cfg(qjs_pool_stats)]` 下；`QJS_POOL_STATS` 未设置时零行为差异。
2. **JSONL 向后兼容**：只新增字段，不改已有字段名和含义；分析器读旧文件不能崩。
3. **分析器降级**：缺外部墙钟、缺 `gc`、只有 1 条快照时，必须输出“不可用”，不能猜。
4. **报告由脚本生成**：`--report report.md` 会整文件覆盖；不允许手写内容，复现命令也必须由脚本输出。
5. **提交格式**：每个 commit 带 `Validation: ...` 行；AI 参与生成的代码带 `AI-assisted: ...` trailer（fork 规则）。下面模板只写 `Validation`，执行时在 body 末尾补一行 `AI-assisted: Yes (agent session; written and tested by the agent)`。
6. **测试命令固定**：Rust 用 `just test-pool-stats <filter>`；Python 用 `python3 -m unittest discover -s etc/ci/performance -p 'test_analyze_pool_stats.py' -v`。

## File Structure

| 文件 | 责任 |
|---|---|
| `servo-quickjs/mozjs/src/engine/pool_stats.rs` | 新增 `gc` 字段与 `gc_ran()`；JSON 序列化 |
| `servo-quickjs/mozjs/src/engine/runtime.rs` | `JS_GC` 只给 `JS_RunGC` 计时 |
| `servo-quickjs/mozjs/src/engine/misc.rs` | `NonIncrementalGC` / `StartIncrementalGC` 的 `JS_RunGC` 计时 |
| `servo-quickjs/mozjs/tests/qjs_pool_stats.rs` | Rust 侧断言：字段存在、GC 计数 |
| `servo-ohos/etc/ci/performance/analyze_pool_stats.py` | `conclusions()`、`source_section()`、`slow_calls` |
| `servo-ohos/etc/ci/performance/test_analyze_pool_stats.py` | 新文件：结论块与降级路径的单元测试 |
| `servo-ohos/etc/ci/performance/gc-baseline.md` | 任务 0 的基线记录：GC 是否发生 |
| `servo-ohos/etc/ci/performance/pool-gc-findings.md` | 任务 4 的多负载对比与候选优化点 |

## Review Focus

1. **没有外部墙钟**：分析器必须打印“无法算占比”，不能除零或崩溃。由任务 2 的 `test_conclusion_without_wall` 覆盖。
2. **只有 1 条快照**：没有窗口，Pearson r 不可用。由任务 2 的 `test_conclusion_short_correlation` 覆盖。
3. **直方图边界**：1 ms 不是桶边界，只能报告“至少 N 次”。由任务 2 的 `test_conclusion_slow_tail` 覆盖。
4. **GC 未触发**：没有 `gc` 字段时必须输出“没有记录到 `JS_GC`”。由任务 3 的 `test_conclusion_no_gc` 覆盖。

---

### Task 0: 基线确认 —— dom-modify 是否触发 `JS_GC`

**Files:**
- Create: `etc/ci/performance/gc-baseline.md`

**Interfaces:**
- Consumes: 现有 `servo-ohos/target/pool-stats/debug/servoshell`。
- Produces: 一个布尔结论，决定任务 3 是否执行；`gc-baseline.md` 记录命令与计数。

- [ ] **Step 1: 跑 dom-modify，收集 GC 日志**

```sh
cd /home/l00982027/mygit/qjs-diff
QJS_GC_STATS=1 QJS_POOL_STATS_SAMPLE=0 \
  ./servo-ohos/target/pool-stats/debug/servoshell \
  ./servo-ohos/tests/dromaeo/dromaeo/web/tests/dom-modify.html \
  2> /tmp/qjs-gc-dom-modify.log

grep -c '^qjs-gc:' /tmp/qjs-gc-dom-modify.log
```

预期：输出一个整数。`0` 表示没有走 `JS_GC` 入口。

- [ ] **Step 2: 若为 0，再跑两个更重的页面**

```sh
cd /home/l00982027/mygit/qjs-diff
for page in dom-traverse jslib-modify-jquery; do
  QJS_GC_STATS=1 QJS_POOL_STATS_SAMPLE=0 \
    ./servo-ohos/target/pool-stats/debug/servoshell \
    ./servo-ohos/tests/dromaeo/dromaeo/web/tests/$page.html \
    2> /tmp/qjs-gc-$page.log
  echo "$page: $(grep -c '^qjs-gc:' /tmp/qjs-gc-$page.log)"
done
```

- [ ] **Step 3: 写结论文件**

`etc/ci/performance/gc-baseline.md` 必须包含：

1. 三个命令与各自计数。
2. 一句判定：`有 GC` → 任务 3 执行；`全部为 0` → 任务 3 跳过，只做 pool。
3. 一条限制：`QJS_GC_STATS` 只覆盖 `JS_GC` 入口；`misc.rs` 里直接调 `JS_RunGC` 的路径不计入（当前 Servo 未使用）。

- [ ] **Step 4: Commit**

```sh
cd /home/l00982027/mygit/qjs-diff/servo-ohos
git add etc/ci/performance/gc-baseline.md
git commit -m "$(cat <<'EOF'
docs(perf): record whether dom-modify triggers JS_GC

Validation: rerun the three commands in gc-baseline.md and confirm the counts.
EOF
)"
```

---

### ~~Task 1: 给 JSONL 加 `elapsed_ms`~~

> **采用新的elapsed计算方式，内容待更新。**
> 旧的 Rust 侧 `elapsed_ms` 已删除；当前占比用外部 `/usr/bin/time` + `--wall-seconds` / `--wall-ms`。

---

### Task 2: 报告加“数据来源”和“结论”块

**Files:**
- Modify: `etc/ci/performance/analyze_pool_stats.py`（`drain_rows()` 约 161–183 行；`main()` 约 242 行；报告组装约 503 行）
- Create: `etc/ci/performance/test_analyze_pool_stats.py`

**Interfaces:**
- Consumes: `metrics(record)`、`drain_rows(agg)`、`delta_windows(history)` 的返回值。
- Produces:
  - `slow_calls: int`：该行 `hist` 中 `lo_ns >= 1_000_000` 的计数之和。
  - `conclusions(records, rows, windows) -> str`：固定 5 行。
  - `source_section(paths, commands, records) -> str`：复现命令 + `sample_every` 说明。
  - CLI：`--source CMD`（可重复）。

- [ ] **Step 1: 写失败测试**

新文件 `etc/ci/performance/test_analyze_pool_stats.py`：

```python
import unittest

import analyze_pool_stats as a


def record(**over):
    r = {
        "pid": 1, "runtime": 1, "thread": "Script#1",
        "pushes": {"result": 10, "borrowed": 0, "root": 0, "deferred": 0},
        "apis": [{"api": "X", "kind": "result", "total": 10,
                  "fresh": 10, "rooted": 10, "kept": 0, "dropped": 0}],
        "drains": [
            {"ctx": "native_call", "calls": 100, "empty": 0, "total_ns": 150_000_000,
             "max_ns": 20_000_000, "values": 1000,
             "hist": [{"lo_ns": 1_048_576, "count": 1},
                      {"lo_ns": 1_000, "count": 99}]},
        ],
    }
    r.update(over)
    return r


def rows_of(*records):
    return a.drain_rows(a.aggregate(list(records), "ctx"))


class ConclusionsTest(unittest.TestCase):
    def test_conclusion_with_wall(self):
        text = a.conclusions([record()], rows_of(record()), [], wall_ms=200)
        self.assertIn("占程序执行时间 75.0%", text)

    def test_conclusion_without_wall(self):
        text = a.conclusions([record()], rows_of(record()), [])
        self.assertIn("未提供外部墙钟", text)

    def test_conclusion_slow_tail(self):
        text = a.conclusions([record()], rows_of(record()), [])
        self.assertIn("至少 1 次 drain ≥ 1 ms", text)
        self.assertIn("20.0 ms", text)

    def test_conclusion_short_correlation(self):
        text = a.conclusions([record()], rows_of(record()), [])
        self.assertIn("相关性不可用", text)

    def test_conclusion_dropped(self):
        rec = record()
        rec["apis"][0]["dropped"] = 3
        text = a.conclusions([rec], rows_of(rec), [])
        self.assertIn("dropped = 3", text)


class SourceSectionTest(unittest.TestCase):
    def test_source_commands_are_fenced(self):
        text = a.source_section(["stats.jsonl"], ["QJS_POOL_STATS_SAMPLE=0 ./servoshell page.html"],
                                [record()])
        self.assertIn("```sh", text)
        self.assertIn("QJS_POOL_STATS_SAMPLE=0 ./servoshell page.html", text)
        self.assertIn("analyze_pool_stats.py", text)
```

- [ ] **Step 2: 跑测试确认失败**

```sh
cd /home/l00982027/mygit/qjs-diff/servo-ohos
python3 -m unittest discover -s etc/ci/performance -p 'test_analyze_pool_stats.py' -v
```

预期：FAIL，`AttributeError: module 'analyze_pool_stats' has no attribute 'conclusions'`。

- [ ] **Step 3: 实现 `slow_calls`**

`drain_rows()` 的 `buckets` 已经是 `{lo_ns: count}`。添加：

```python
"slow_calls": sum(n for lo, n in buckets.items() if lo >= 1_000_000),
```

- [ ] **Step 4: 实现 `conclusions()`**

放在 `drain_rows()` 之后：

```python
def conclusions(records, rows, windows, wall_ms=None):
    """顶部结论：5 行，固定顺序。缺数据时明说不可用，不猜。"""
    total_ms = sum(metrics(r)["drain_ns"] for r in records) / 1e6
    if wall_ms is not None:
        share = 100 * total_ms / wall_ms
        first = (f"1. 总 drain {total_ms:.1f} ms，占程序执行时间 {share:.1f}%"
                 f"（分母为外部 time 测的整条命令墙钟）。")
    else:
        first = (f"1. 总 drain {total_ms:.1f} ms；未提供外部墙钟"
                 f"（--wall-ms / --wall-seconds），无法算占程序执行时间的比例。")

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
```

- [ ] **Step 5: 实现 `source_section()`**

```python
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
```

- [ ] **Step 6: 接入 `main()` 和报告**

在 `main()` 里：

```python
parser.add_argument("--source", action="append", default=[], metavar="CMD",
                    help="reproduction command; repeatable")
```

在 `windows = delta_windows(history)`（约 308 行）之后：

```python
conclusion = conclusions(records, rows, windows)
source = source_section(args.paths, args.source, records)
```

报告组装列表，在 `intro` 之后插入两项：

```python
"## 结论\n\n" + conclusion,
"## 数据来源与复现命令\n\n" + source,
```

- [ ] **Step 7: 跑测试确认通过**

```sh
cd /home/l00982027/mygit/qjs-diff/servo-ohos
python3 -m unittest discover -s etc/ci/performance -p 'test_analyze_pool_stats.py' -v
```

预期：6 个测试全部 PASS。

- [ ] **Step 8: 重新生成报告**

```sh
cd /home/l00982027/mygit/qjs-diff
python3 servo-ohos/etc/ci/performance/analyze_pool_stats.py \
  --report report.md \
  --source "QJS_POOL_STATS_OUT=stats.jsonl QJS_POOL_STATS_SAMPLE=0 ./servo-ohos/target/pool-stats/debug/servoshell ./servo-ohos/tests/dromaeo/dromaeo/web/tests/dom-modify.html" \
  stats.jsonl
```

预期：`report.md` 顶部出现 `## 结论` 与 `## 数据来源与复现命令`；第 1 行显示 drain 占比。

- [ ] **Step 9: Commit**

```sh
cd /home/l00982027/mygit/qjs-diff/servo-ohos
git add etc/ci/performance/analyze_pool_stats.py \
        etc/ci/performance/test_analyze_pool_stats.py
git commit -m "$(cat <<'EOF'
feat(perf): lead the pool report with conclusions and sources

Validation: python3 -m unittest discover -s etc/ci/performance -p 'test_analyze_pool_stats.py' -v
EOF
)"
```

---

### Task 3: 给 `JS_GC` 计时（仅当 Task 0 判定“有 GC”）

**Files:**
- Modify: `mozjs/src/engine/pool_stats.rs`（`State`、`gc_ran()`、`json()`）
- Modify: `mozjs/src/engine/runtime.rs:124-138`
- Modify: `mozjs/src/engine/misc.rs:91-120`
- Test: `mozjs/tests/qjs_pool_stats.rs`
- Modify: `etc/ci/performance/analyze_pool_stats.py`（`conclusions()`）
- Test: `etc/ci/performance/test_analyze_pool_stats.py`

**Interfaces:**
- Consumes: `DrainStats`（已有 `calls/total_ns/max_ns/hist`）。
- Produces:
  - `pub(crate) fn gc_ran(elapsed: Duration)`。
  - JSON 顶层 `"gc":{"calls":N,"total_ns":N,"max_ns":N,"hist":[...]}`。
  - `conclusions()` 第 6 行：GC 次数/总耗时/最长。

- [ ] **Step 1: 写失败测试（Rust）**

在 `mozjs/tests/qjs_pool_stats.rs` 新增 `gc_runs_are_counted`：

1. 按现有测试构造 runtime 和 global。
2. 调用 `unsafe { JS_GC(cx, GCReason::API) }`，`JS_GC` 来自 `mozjs::rust::wrappers2`，`GCReason` 来自 `mozjs::jsapi`。
3. `let json = pool_stats_json();`
4. 断言 `json.contains("\"gc\":{\"calls\":1,")` 且 `json.contains("\"max_ns\":")`。

Run: `just test-pool-stats gc_runs_are_counted`
Expected: FAIL，缺少 `"gc"`。

- [ ] **Step 2: 实现 `gc_ran()` 和 JSON**

`State` 增加 `gc: DrainStats`（`DrainStats` 已实现 `Default`）。

```rust
/// Records one full collection. Only `JS_RunGC` is timed; `QJS_GC_STATS` printing is not.
pub(crate) fn gc_ran(elapsed: Duration) {
    let _ = STATE.try_with(|s| {
        let stats = &mut s.borrow_mut().gc;
        let ns = u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX);
        stats.calls += 1;
        stats.total_ns += ns;
        stats.max_ns = stats.max_ns.max(ns);
        stats.hist[bucket(ns)] += 1;
    });
}
```

`json()` 在 `"drains":[` 之前输出 `"gc":{...}`，复用非零桶的写法（把 drains 的 hist 输出抽成 `fn hist_json(out: &mut String, hist: &[u64])`，两处共用）。

- [ ] **Step 3: 给三个 `JS_RunGC` 调用点计时**

`runtime.rs` 的 `JS_GC`：

```rust
#[cfg(qjs_pool_stats)]
let start = std::time::Instant::now();
q::JS_RunGC(rt);
#[cfg(qjs_pool_stats)]
super::pool_stats::gc_ran(start.elapsed());
```

`misc.rs` 的 `NonIncrementalGC` 和 `StartIncrementalGC` 同样处理；`Instant` 的 `use` 也加 `#[cfg(qjs_pool_stats)]`。

注意：`JS_GC` 里的 `print_gc_stats(rt)`、`JS_SetGCRootsHook` 都在计时之外。

- [ ] **Step 4: 跑 Rust 测试**

```sh
cd /home/l00982027/mygit/qjs-diff/servo-quickjs
just test-pool-stats gc_runs_are_counted
```

预期：PASS。

- [ ] **Step 5: 写失败测试（Python）**

在 `test_analyze_pool_stats.py` 加：

```python
def test_conclusion_no_gc(self):
    text = a.conclusions([record()], rows_of(record()), [])
    self.assertIn("没有记录到 `JS_GC`", text)

def test_conclusion_gc(self):
    rec = record(gc={"calls": 2, "total_ns": 3_000_000, "max_ns": 2_000_000, "hist": []})
    text = a.conclusions([rec], rows_of(rec), [])
    self.assertIn("GC：2 次", text)
    self.assertIn("最长 2.0 ms", text)
```

Run: `python3 -m unittest discover -s etc/ci/performance -p 'test_analyze_pool_stats.py' -v`
Expected: FAIL，缺少 `6.` 行。

- [ ] **Step 6: 实现第 6 行**

`conclusions()` 末尾追加：

```python
gc = [r["gc"] for r in records if isinstance(r.get("gc"), dict)]
if gc:
    calls = sum(g["calls"] for g in gc)
    total = sum(g["total_ns"] for g in gc) / 1e6
    longest = max(g["max_ns"] for g in gc) / 1e6
    sixth = f"6. GC：{calls} 次，总耗时 {total:.1f} ms，最长 {longest:.1f} ms。"
else:
    sixth = "6. GC：没有记录到 `JS_GC`。"
```

返回列表改为 6 项。

- [ ] **Step 7: 跑 Python 测试并重建**

```sh
cd /home/l00982027/mygit/qjs-diff/servo-ohos
python3 -m unittest discover -s etc/ci/performance -p 'test_analyze_pool_stats.py' -v

cd /home/l00982027/mygit/qjs-diff/servo-quickjs
QJS_POOL_STATS=1 \
SERVO_TARGET_DIR=/home/l00982027/mygit/qjs-diff/servo-ohos/target/pool-stats \
just servo "build --features js-quickjs"
```

预期：Python 8 个测试 PASS；servoshell 构建成功。

- [ ] **Step 8: 端到端验证**

跑 Task 0 里有 GC 的那个页面，另存 `stats-gc.jsonl`，再生成 `--report /tmp/report-gc.md`。预期：报告第 6 行出现 GC 次数。

- [ ] **Step 9: Commit**

两个仓库分别提交：

```sh
cd /home/l00982027/mygit/qjs-diff/servo-quickjs
git add mozjs/src/engine/pool_stats.rs mozjs/src/engine/runtime.rs \
        mozjs/src/engine/misc.rs mozjs/tests/qjs_pool_stats.rs
git commit -m "$(cat <<'EOF'
feat(pool-stats): time JS_RunGC

Validation: just test-pool-stats gc_runs_are_counted; rerun a page with GC and check the gc field.
EOF
)"
```

```sh
cd /home/l00982027/mygit/qjs-diff/servo-ohos
git add etc/ci/performance/analyze_pool_stats.py etc/ci/performance/test_analyze_pool_stats.py
git commit -m "$(cat <<'EOF'
feat(perf): report GC count and pause

Validation: python3 -m unittest discover -s etc/ci/performance -p 'test_analyze_pool_stats.py' -v
EOF
)"
```

---

### Task 4: 多负载对比，产出候选优化点

**Files:**
- Create: `etc/ci/performance/pool-gc-findings.md`

**Interfaces:**
- Consumes: Task 2 的结论块、Task 3 的 `gc`（若执行）。
- Produces: 一张对比表 + 排序后的候选优化点；不写生产代码。

- [ ] **Step 1: 跑 5 个页面，各自一个文件**

```sh
cd /home/l00982027/mygit/qjs-diff
for page in dom-attr dom-query dom-traverse jslib-modify-jquery jslib-modify-prototype; do
  QJS_POOL_STATS_OUT=stats-$page.jsonl QJS_POOL_STATS_SAMPLE=0 \
    ./servo-ohos/target/pool-stats/debug/servoshell \
    ./servo-ohos/tests/dromaeo/dromaeo/web/tests/$page.html
done
```

- [ ] **Step 2: 为每个页面生成报告**

```sh
cd /home/l00982027/mygit/qjs-diff
for page in dom-attr dom-query dom-traverse jslib-modify-jquery jslib-modify-prototype; do
  python3 servo-ohos/etc/ci/performance/analyze_pool_stats.py \
    --report /tmp/report-$page.md \
    --source "QJS_POOL_STATS_OUT=stats-$page.jsonl QJS_POOL_STATS_SAMPLE=0 ./servo-ohos/target/pool-stats/debug/servoshell ./servo-ohos/tests/dromaeo/dromaeo/web/tests/$page.html" \
    stats-$page.jsonl
done
```

预期：5 个文件都有 `## 结论`。

- [ ] **Step 3: 填 `pool-gc-findings.md`**

表格列：`page | drain_ms | 主导 ctx | max drain ms | GC 次数 | GC max ms`；占比列需要外部墙钟，内容待更新。

每行从对应报告的结论块抄数字，不重新计算。

- [ ] **Step 4: 给候选优化点排序**

按下面顺序写 1–3 条，每条必须有数字支撑：

1. `drain% > 5%` 或单次 `max drain > 16 ms` → pool 侧优先。
2. `GC max > 16 ms` 或 `GC 次数 > 10` → GC 侧优先。
3. 都低于阈值 → 写“当前负载下不值得优化”，并说明下一步应换更重的负载。

- [ ] **Step 5: Commit**

```sh
cd /home/l00982027/mygit/qjs-diff/servo-ohos
git add etc/ci/performance/pool-gc-findings.md
git commit -m "$(cat <<'EOF'
docs(perf): compare pool and GC cost across Dromaeo pages

Validation: rerun the loop in the document and confirm the table's shares.
EOF
)"
```

---

## Self-Review

1. **Spec coverage**：外部墙钟（内容待更新）、结论化（任务 2）、GC 是否发生（任务 0）、GC 计时（任务 3）、多负载（任务 4）都有对应任务。
2. **Step scan**：每个实现步骤都有签名、文件、断言；没有“处理边界情况”这类空话。
3. **Type consistency**：`slow_calls` 只在 `drain_rows()` 定义一次；`conclusions()` 的 5/6 行顺序在任务 2/3 一致。
4. **Review Focus**：5 条都有测试或端到端验证。
5. **Proportion**：计划描述接口与断言，没有贴完整实现；`conclusions()` 因措辞必须精确，保留了函数体。
