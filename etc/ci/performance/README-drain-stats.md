# Drain stats 采集与合并流程

统计每次 `drain_pool` 的耗时，以及它占整个进程执行时间的比例。

## 1. 构建

```sh
DRAIN_STATS=1 SERVO_TARGET_DIR=$PWD/target/drain-stats \
  just servo "build --features js-quickjs"
```

`DRAIN_STATS` 是**编译期**开关，运行时设置无效。

## 2. 采集一个测例

约定文件放在同一目录：

- `qjs-drain-stats-<testcase>.jsonl`：**每次 drain 写一行**；
- `wall-<testcase>.txt`：`time` 记录的整个进程墙钟秒数。

```sh
mkdir -p out
/usr/bin/time -q -f '%e' -o out/wall-demo.txt \
  env QJS_DRAIN_STATS_OUT=out/qjs-drain-stats-demo.jsonl \
  target/drain-stats/debug/servoshell out/demo.html --headless
```

一行格式：

```json
{"thread":"Script(1,1)","pid":123,"runtime":1,"ctx":"native_call","elapsed_ns":1234,"values":5}
```

注意：

1. 每次 drain 立即写一行并 flush，**进程被 `timeout -s KILL` 杀死也能保留已写入的数据**；
   被杀之后的 drain 不存在，不会被当作空 drain。
2. 一个测例可以有多个线程，用 `pid + runtime` 区分。
3. 空 drain 的 `elapsed_ns` 为 `null`，不计时。
4. 文件在进程第一次写入时**截断**，之后同进程的 runtime 追加：旧文件会被覆盖，
   不会混入上一次运行的数据。
5. 旧版聚合 JSON（`drains` 数组、`final` 字段）不再兼容，请重新采集。

## 3. 合并多个测例

```sh
python etc/ci/performance/merge_drain_stats.py out
```

如果同时采集了 `wall-<testcase>.txt`，加 `--wall` 才会读取它们并计算 `drain_pool(%)`：

```sh
python etc/ci/performance/merge_drain_stats.py out --wall
```

可选参数：

- `--out-dir DIR`：CSV 输出目录，默认与输入目录相同。
- `--wall`：读取 `wall-*.txt`。**默认关闭**；关闭时不读 wall 文件，`drain_pool(%)` 留空，
  也不会因缺 wall time 而跳过测例。

输入目录里所有 `qjs-drain-stats-*.jsonl` 都会按文件名中的 `<testcase>` 合并。

## 4. 输出

### `drain-summary.csv`

每个 `(thread, pid, runtime)` 7 行 context，加 1 行 `ALL` 合计。脚本在 Python 中完成
计数、求和、最大值和直方图分桶；Rust 只写原始行。

| 列 | 含义 |
|---|---|
| `testcase_name` | 测例名（文件名中 `<testcase>`） |
| `context` | `scope_drop` / `native_call` / `runtime_destroy` / `proxy_trap` / `job_callback` / `job_run` / `task` / `ALL` |
| `thread`, `pid`, `runtime` | 线程与 runtime 标识 |
| `calls` | drain 调用次数 |
| `empty` | 空 drain 次数（不计时） |
| `total_time` | 非空 drain 的总耗时，毫秒 |
| `max_time` | 单次最大耗时，毫秒 |
| `value_count` | drain 掉的 value 数量 |
| `drain_pool(%)` | 该行 `total_time / wall time × 100`；仅 `--wall` 时有值 |

`ALL` 行：`calls`、`empty`、`value_count`、`total_time` 求和，`max_time` 取最大。

### `drain-hist.csv`

长表，主键：`testcase_name, context, thread, pid, runtime, bucket`。

| 列 | 含义 |
|---|---|
| `bucket` | 耗时区间 `[lo_ns, hi_ns)` |
| `count` | 落在该区间的 drain 操作数 |
| `percentage` | 该区间占同一主键下所有 drain 操作的百分比 |

### `drain-skipped.csv`

| 列 | 含义 |
|---|---|
| `testcase_name` | 被跳过的测例 |
| `wall_seconds` | 墙钟秒数（有则记录） |
| `reason` | `no-data`：完全没有 drain 记录；`no-wall`：有记录但缺 wall time，主表仍输出，百分比留空 |

## 5. 相关文档

- `servo-quickjs/docs/agents/drain-stats.md`：JSON 字段与 Rust 侧行为。
- `reproduce-drain-gc.sh`：批量复现脚本。
