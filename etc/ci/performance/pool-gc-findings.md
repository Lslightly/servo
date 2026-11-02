# Pool 与 GC 成本：5 个 Dromaeo 页面

## 数据

- 构建：`QJS_POOL_STATS=1` 的 **debug** servoshell（`target/pool-stats/debug`），未优化，绝对耗时偏大；比例仍有参考价值。
- 运行：直接打开页面 + `--headless`，12 s 后 `SIGKILL`；`QJS_POOL_STATS_SNAPSHOT_MS=1000`，`QJS_POOL_STATS_SAMPLE=0`。
- 页面直接打开时 `htmlrunner.js` 回退为 `test(name, fn){ fn(); }`，每个测试只跑一遍。
- 数字抄自各页报告的 `## 结论` 块；GC 次数用 `QJS_GC_STATS=1` 统计 `^qjs-gc:` 行数，为 0。
- `drain%` 的分母是墙钟时间（含空闲），是上限，不是 CPU 占比。

| page | elapsed_ms | drain_ms | drain% | 主导 ctx | max drain ms | GC 次数 | GC max ms |
|---|---:|---:|---:|---|---:|---:|---:|
| dom-attr | 6074 | 26.5 | 0.4 | task | 24.6 | 0 | — |
| dom-query | 4262 | 31.0 | 0.7 | task | 28.5 | 0 | — |
| dom-traverse | 6350 | 26.0 | 0.4 | task | 23.7 | 0 | — |
| jslib-modify-jquery | 5598 | 76.5 | 1.4 | native_call (58.2%) | 28.0 | 0 | — |
| jslib-modify-prototype | 6397 | 30.5 | 0.5 | task (92.8%) | 26.2 | 0 | — |

## 候选优化点（按计划阈值排序）

1. **池侧优先：单次 `task` drain 24–28 ms，5 个页面都出现。**
   - `task` 占各页 drain 时间 92.8%–99.9%，一次 drain 释放约 20 400 个值。
   - `ns/value` = 1.27–1.52 µs（debug 构建）；即慢在“单个值释放”或“一次释放太多”，不是调用次数。
   - 下一步：用 release 构建复测，排除 debug 放大；再看这 20k 值的 kind/fate 分布，确认是否一次清空整棵 DOM 子树。

2. **jslib-modify-jquery 的 `native_call` 长尾。**
   - 4 994 次调用，合计 44.6 ms（58.2%），mean 8.92 µs，单次 max 21.3 ms。
   - 与 `task` 的单次 28.0 ms 叠加，这一页总 drain 76.5 ms，是 5 页中最高。
   - 下一步：定位 21.3 ms 那次调用对应的 API（`Top API call sites` 表 + backtrace 采样）。

3. **GC 侧：当前负载下没有 GC，不值得优化。**
   - 5 个页面 `JS_GC` 调用次数都是 0。
   - 这不能证明“GC 永不发生”：统计只覆盖 `JS_GC` 入口，不覆盖 `misc.rs` 里直接调用的 `JS_RunGC`；而且当前负载很轻。
   - 下一步：换更重的负载（WPT、真实站点）或 release 构建，再决定是否给 GC 加计时。

## 限制

- 12 s 后 `SIGKILL`，没有优雅关闭；最后一帧是快照，不是 `final` 行。
- `drain` 时间是 `Cx::drain_pool`，不是引擎 GC 暂停时间。
- 每页只有 2–3 个快照窗口，`## 结论` 第 5 行的相关性不可用。
