---
name: wpt-tests
description: servo-ohos 的 WPT 测例速查：测例/期望值在哪里、哪些目录会被运行、testharness/reftest 怎么区分、如何按目录或单文件运行 `./mach test-wpt`、canvas/WebGL/WebGPU 子套件与 vello backend、更新 expectations。当需要确认某功能“有没有 WPT 测例”、定位失败的期望文件、或决定用哪条命令跑子集时使用；先读本 skill，不要 ls/find/rg 全仓找。
---

# WPT 测例速查

本仓库的 WPT 结构是标准 servo 布局：**上游测例树** + **本仓库期望值（meta）** + **include 白名单**。
所有“有没有测例/在哪/怎么跑”的问题都可以从下面三张表回答，不需要扫目录。

## 目录布局

| 路径 | 内容 |
| --- | --- |
| `tests/wpt/tests/` | 上游 WPT 测例树（由 `./mach update-wpt` 同步，**不要手改**）。测试名就是相对此目录的路径，如 `html/canvas/element/...` |
| `tests/wpt/meta/` | Servo 的期望值（expectations），层级 `.ini`，路径与 `tests/wpt/tests/` 一一对应；另有 `MANIFEST.json` |
| `tests/wpt/include.ini` | 决定哪些目录被运行：**默认 `skip: true`**，只有显式 `skip: false` 的才跑；键是路径段，可嵌套（如 `[html]` 下再写 `[cross-origin-embedder-policy] skip: true`）。`[html]`、`[_mozilla]`、`[_webgl]`、`[webgl]`、`[webidl]` 已启用；**完整启用列表直接看文件里的 `skip: false` 条目，不要靠记忆枚举** |
| `tests/wpt/config.ini` | products（`servo`/`servo_legacy`/`servo_ohos`）与四个 manifest：upstream(`tests/`+`meta/`)、`mozilla/`、`webgl/`、`webgpu/`。`include.ini` 管的是 upstream 树；`mozilla`/`webgl`/`webgpu` 三个独立树由各自的 manifest 挂载（`include.ini` 里 `[_mozilla]`、`[_webgl]` 键控制它们是否运行；目前没有 `[_webgpu]` 键，所以 webgpu 树默认不跑） |
| `tests/wpt/mozilla/tests/` | Servo 自己的测例（URL 前缀 `/_mozilla/`），期望值在 `tests/wpt/mozilla/meta/` |
| `tests/wpt/webgl/`、`tests/wpt/webgpu/` | WebGL / WebGPU 独立 manifest（`tests/` + `meta/`） |
| `tests/wpt/hosts` | 运行时的 `HOST_FILE`（本机域名解析） |
| `tests/wpt/vello_canvas_subsuite.json` | vello canvas 子套件：`include: ["/html/canvas/element"]`，`binary_args: ["--pref", "dom_canvas_backend=vello"]` |
| `python/wpt/run.py` | `mach test-wpt` 的实际入口（组装 wptrunner 参数） |

## 测例类型

`tests/wpt/tests/tools/wptrunner/wptrunner/wpttest.py` 的 `enabled_tests` 定义了全部类型：
`testharness`、`reftest`、`wdspec`、`crashtest`、`print-reftest`、`test262`、`aamtest`。

- **testharness**：`.html` 且引入 `testharness.js`；`.any.js` 会按 window/worker 各跑一遍。断言 PASS/FAIL。
- **reftest**：页面与参照页渲染结果对比。参照页命名 `xxx-expected.html`（或 `rel=match` 指向）。
- **crashtest**：只要求不崩溃（文件名常含 `crash`）。
- **wdspec**：WebDriver 的 Python 测试（`webdriver/tests/`），跑的是 `tests/wpt/tests/tools/webdriver`。

判断某目录属于哪类：看目录里是 `*-expected.html` → reftest；是 `testharness.js`/`.any.js` → testharness。

## Canvas：有测例，而且数量很大

`tests/wpt/tests/html/canvas/`（约 4.8k 文件）由 `include.ini` 的 `[html] skip: false` 启用。
注意下表是**文件数，不是测试数**：一个 `.html` 可含多个 subtest，`.any.js` 按 window/worker 各跑一遍；
`element/` 顶层还有散落的单文件测例，所以子目录之和小于总数。

| 子目录 | 规模（记录时快照） | 类型 |
| --- | --- | --- |
| `element/` | 1884 个 `.html`，其中 139 个 `*-expected.html` | 以 **reftest** 为主，混有 testharness |
| `offscreen/` | 1444 个 `.html` + 1033 个 `.js` | **testharness**（OffscreenCanvas；主用例 + `.worker.js` 成对） |
| `tools/` | 14 个 `.html` | 工具页，一般不作为正式断言 |
| `resources/` | JS helper | 测试资源 |

期望值在 `tests/wpt/meta/html/canvas/`（约 1170 个 `.ini`，层级与测例目录一一对应：
element 的在 `meta/html/canvas/element/`，offscreen 的在 `meta/html/canvas/offscreen/`）。

canvas 结果依赖 backend：默认 `mach test-wpt` 用默认 backend 和默认 meta；
`vello_canvas_subsuite.json` 只是另一套运行配置（`--pref dom_canvas_backend=vello` + `include: /html/canvas/element`），
不改变 meta 路径，但同一测例在两种 backend 下的结果可能不同——对比结果前先确认 backend 一致。

## 运行

```bash
# 整个 canvas element 套件（reftest 为主）
./mach test-wpt html/canvas/element

# OffscreenCanvas（testharness）
./mach test-wpt html/canvas/offscreen

# 单个文件
./mach test-wpt html/canvas/element/2d.conformance.requirements.basics.html

# 指定构建 profile（默认 debug）
./mach test-wpt --release html/canvas/element
./mach test-wpt --profile checked-release html/canvas/element

# vello canvas backend
./mach test-wpt --pref dom_canvas_backend=vello html/canvas/element

# QuickJS 引擎（期望值可能按 js_engine 区分）
./mach test-wpt --js-engine quickjs html/canvas/offscreen

# 其他平台
./mach test-wpt --ohos --profile checked-release --js-engine quickjs html/canvas/offscreen
```

测试路径**相对 `tests/wpt/tests/`**，写 `html/canvas/...` 即可；也接受 `tests/wpt/tests/...` 全路径。

常用参数（`mach test-wpt --help` 可看全量，来自 wptrunner + `python/wpt/__init__.py`）：

| 参数 | 用途 |
| --- | --- |
| `--release` / `--profile <cargo_profile>` | 选择构建产物 |
| `--processes N` / `--timeout-multiplier N` | 并发与超时（CI 用 2 倍超时） |
| `--pref k=v`（可重复） | 传 Servo 偏好，如 `dom_canvas_backend` |
| `--js-engine quickjs` | 期望值按引擎区分时必须显式给 |
| `-u` / `--update-expectations` | 跑完后按结果更新 `tests/wpt/meta/`（OHOS 上不支持） |
| `--log-raw <file>` | 原始结构化日志，便于定位单个失败 |
| `--always-succeed` | 总是返回 0，CI 中汇总结果用 |
| `--total-chunks N --this-chunk M` | 分片（CI 用） |
| `--ohos` | 在 OpenHarmony 设备上跑 headless servoshell（需先构建 headless binary） |
| `--multiprocess` / `-M` | 多进程模式（OHOS 不支持） |

## 期望值（meta）速查

- 文件：`tests/wpt/meta/<test-path>.ini`，例如 `tests/wpt/meta/html/canvas/element/...ini`；目录级 `__dir__.ini` 对整棵子树生效。
- 语法：
  ```ini
  [test-filename.html]
    expected: FAIL        # 整体结果：PASS/FAIL/TIMEOUT/CRASH/NOTRUN
    [subtest name]
      expected: FAIL      # testharness 子测试单独标记
    [test.any.js]
      expected: [FAIL, TIMEOUT]   # 允许多种结果
  ```
- 期望值可按 run_info 条件区分，条件是**属性值**，不是独立属性：
  ```ini
  [blockeduri-eval.html]
    [Eval violations have a blockedURI of 'eval']
      expected:
        if js_engine == "quickjs": FAIL
  ```
  无值属性（如 `disabled`）则写成 `disabled: if js_engine == "quickjs": <原因>`
  （见 `tests/wpt/meta/wasm/__dir__.ini`）。可用条件字段有 `js_engine`、`debug`、`os` 等。
  **canvas backend 不在默认 run_info 里**，vello 与默认 backend 共用同一份 meta，不会自动区分。
- 新增/修改测例后，期望值优先用 `-u` 生成，再人工检查，不要手写覆盖全部结果。
- 上游同步后行号/文件名可能漂移；`meta/` 里的路径按测例路径定位，不按行号。

## 常见错误

| 现象 | 原因/做法 |
| --- | --- |
| 某个目录“没有跑” | `include.ini` 默认 `skip: true`，需要显式启用该顶层目录 |
| `mach test-wpt css/...` 提示找不到测试 | 确认目录在 `include.ini` 中启用，且路径相对 `tests/wpt/tests/` |
| 期望值与实际差很多 | 先确认 `--js-engine`、profile（debug/release）、canvas backend 与期望值记录时一致 |
| 直接改 `tests/wpt/tests/` | 上游树，会被同步覆盖；改 `meta/` 或 `mozilla/tests/` |
| canvas offscreen 全部 SKIP | 该目录是 testharness，确认用的 binary 支持 OffscreenCanvas 且没被 meta 标记 TIMEOUT |

## 维护

新增可复用的 WPT 知识（新子套件、新 backend、include 规则变化）写回本文件；行号/数量只作快照提示，以符号和路径为准。
