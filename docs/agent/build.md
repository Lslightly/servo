# 构建方式：event flame / drain stats

> 以下命令都在 `servo-ohos/` 根目录执行。
> 所有构建统一使用 **`CARGO_TARGET_DIR=target/drain-stats`**。
> 都需要 QuickJS 后端（`js-quickjs` feature）和 patched mozjs（`--config` 那行）。

## 1. event flame（记录 adopt / retain / release，生成 flame chart）

### 构建

```sh
CARGO_TARGET_DIR=target/drain-stats cargo build -p servoshell \
  --features tracing,js-quickjs \
  --config 'patch.crates-io.mozjs.path="../servo-quickjs/mozjs"'
```

要点：

1. `tracing` 是**编译期**开关；变量 `SERVO_EVENT_FLAME_DIR` 是**运行时**开关。
2. 事件来自 servo-quickjs，所以同时要 `js-quickjs`。

### 运行

```sh
rm -rf flames/*
SERVO_EVENT_FLAME_DIR=./flames \
SERVO_EVENT_FLAME_CLASS=HTMLDivElement \
SERVO_TRACING=mozjs_gc=trace \
./target/drain-stats/debug/servoshell --headless tests/gc/2-remove-subtree.html
python event_flame_to_html.py flames --yes
```

要点：

1. `SERVO_EVENT_FLAME_DIR` 不设 → 不记录任何事件。
2. `SERVO_EVENT_FLAME_CLASS` 只保留类名包含该串的对象；不设 = 不过滤。
3. **进程必须正常退出**，否则不写 `.folded` 文件。
4. `python event_flame_to_html.py` 会先删旧产物，交互确认；脚本里加 `--yes`。

## 2. drain stats（统计每次 drain_pool 的耗时）

### 构建

```sh
DRAIN_STATS=1 CARGO_TARGET_DIR=target/drain-stats cargo build -p servoshell \
  --features js-quickjs \
  --config 'patch.crates-io.mozjs.path="../servo-quickjs/mozjs"'
```

要点：`DRAIN_STATS` 是**编译期**开关，运行时设置无效。

### 采集 + 合并

```sh
mkdir -p out
/usr/bin/time -q -f '%e' -o out/wall-demo.txt \
  env QJS_DRAIN_STATS_OUT=out/qjs-drain-stats-demo.jsonl \
  ./target/drain-stats/debug/servoshell --headless tests/gc/2-remove-subtree.html
python etc/ci/performance/merge_drain_stats.py out
```

要点：

1. `QJS_DRAIN_STATS_OUT` 指定 JSONL 输出；`/usr/bin/time` 的 `%e` 是墙钟秒数，合并时算百分比。
2. **进程必须自然退出**：数据在 runtime 销毁时写盘，被 `SIGKILL` 杀死的进程没有数据。
3. 批量采集可直接用 `etc/ci/performance/reproduce-drain-gc.sh`；
   它默认 `TARGET_DIR` 就是 `target/drain-stats`，已有二进制时加 `SKIP_BUILD=1`。
4. 详细字段与 CSV 说明见 `etc/ci/performance/README-drain-stats.md`。

## 3. 一条命令同时开 event flame 和 drain stats

### 构建（两个开关一起给）

```sh
DRAIN_STATS=1 CARGO_TARGET_DIR=target/drain-stats cargo build -p servoshell \
  --features tracing,js-quickjs \
  --config 'patch.crates-io.mozjs.path="../servo-quickjs/mozjs"'
```

### 运行 + 出结果

```sh
mkdir -p out flames; rm -rf flames/*
/usr/bin/time -q -f '%e' -o out/wall-demo.txt \
  env QJS_DRAIN_STATS_OUT=out/qjs-drain-stats-demo.jsonl \
      SERVO_EVENT_FLAME_DIR=./flames \
      SERVO_EVENT_FLAME_CLASS=HTMLDivElement \
      SERVO_TRACING=mozjs_gc=trace \
  ./target/drain-stats/debug/servoshell --headless tests/gc/2-remove-subtree.html
python event_flame_to_html.py flames --yes
python etc/ci/performance/merge_drain_stats.py out
```

注意：

1. 同时跑时 **tracing 有额外开销，drain 耗时会偏大**；这组数字只适合相对比较，不要当作纯 drain 的基准。
2. 仍然要求**自然退出**，两个输出都依赖退出时落盘。
3. 测例换成自己的页面即可，其余不变。
