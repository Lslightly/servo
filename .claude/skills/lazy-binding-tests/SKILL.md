---
name: lazy-binding-tests
description: servo-ohos QuickJS lazy binding 的 materialize 次数回归测试速查：计数器语义（calls vs materialized）、SERVO_LAZY_BINDING_STATS 开关、window.runtime.materialize_count/materialize_calls/reset_materialize_stats、tests/gc/lazy-binding 用例与 run.sh 运行方式、哪些 DOM 操作会 materialize、哪些内建接口是 proxy（Document/Window/NodeList）。当需要新增/修改 materialize 相关测试、判断某段代码应产生几次 materialize、或排查计数与预期不符时先读本文件。
---

# Lazy binding materialize 测试

本 fork 用 `JSDomStub` 延迟创建 DOM 的 `JSObject`。`materialize_if_needed()`
（`components/script_bindings/reflector.rs`）把 stub 换成真对象，是内存/性能收益的关键路径。
本 skill 记录如何用计数器 + `tests/gc/lazy-binding/` 用例看护它。

## 1. 模型（30 秒）

```text
wrap()  ──> JSDomStub（约 40B，无属性、无 prototype）
                │ materialize_if_needed()
                ▼
             真 JSObject（class/realm/proto 取自 stub 记录的接口）
```

- 非 proxy 的 DOM 对象在 `wrap()` 时都是 stub；proxy（`NodeList`/`HTMLCollection`/
  cross-origin/WindowProxy，以及**带 named getter 的接口，如 `Document`**）直接创建真对象，
  **从不**进入 materialize 路径。
- `Document` 在 `Document.webidl` 里有 named getter（`getter NamedPropertyValue (DOMString name);`），
  `configuration.py` 会据此把 `descriptor.proxy` 置为 true，所以 `void document` 永远 +0。
  不要把它当成 lazy 对象，也不要拿它做 delta 的 warm-up 基准。
- `get_jsobject()` 在 stub 上 panic，且**不会** materialize；需要发往 JS 时先
  `materialize_if_needed()`。
- 主要触发点是 `ToJSValConvertible for Reflector`
  （`components/script_bindings/conversions.rs`），即「DOM 对象作为 JS 值交出去」。
- materialize 幂等：同一对象第二次被访问 +0。

## 2. 计数器与 API

实现：

- `components/script_bindings/lazy_binding_stats.rs`：thread-local `Cell<Stats>`；
- `materialize_if_needed` 入口 `record_call()`，通过 `materialize_cache` 检查后
  `record_materialized()`；
- `components/script_bindings/quickjs_runtime_helper.rs` 暴露到已有的 `window.runtime`
  对象上。

| JS 接口 | 含义 |
|---|---|
| `runtime.materialize_count()` | 有效 materialize 次数（stub→JSObject 真正发生） |
| `runtime.materialize_calls()` | 进入 `materialize_if_needed` 的次数（含幂等空转） |
| `runtime.reset_materialize_stats()` | 两个计数清零 |

开关：只有进程环境变量 `SERVO_LAZY_BINDING_STATS=1` 时这三个方法才存在；默认 undefined。
`runtime` 本身在所有 QuickJS 构建里都有（现有 debug hook）。

**断言只用 `materialize_count()`**。`materialize_calls()` 会随任何对象→JSVal 转换增长
（`conversions.rs` 每次都调用），数值随实现变化，只适合诊断（如 `calls > 0`）。

注意测量语句自身的干扰：`window` 是 Rust accessor，返回 `WindowProxy`
（`Window.webidl:9`），每次求值都会走 `ToJSValConvertible for Reflector` 并 `record_call()`；
`WindowProxy` 的 reflector 是手工 `init_reflector` 的，cache 永远是 `None`，所以只增加
`calls`、不增加 `materialized`。同理 `window.document` 会依次求值 `window` 和 `document`
两个 accessor。要断言 `calls` 的 delta 时，先把 `var runtime = window.runtime`
缓存起来，并且测量区间内不要再出现 `window.*`。用例见
`tests/gc/lazy-binding/10-window-accessor.html`。

**线程局部**：每个 script thread（每个 pipeline）独立计数。本组用例不使用 iframe；
iframe 里的 materialize 在父页面读不到。

## 3. 哪些操作会 +1

| 操作 | materialized delta | 说明 |
|---|---|---|
| 首次 `void document` / 任何 `document.*` | 0 | `Document` 有 named getter，是 proxy（同 Window），从不 materialize；只增加 `calls` |
| 每次求值 `window` | 0 | accessor 返回 `WindowProxy`，每次转换都进 `materialize_if_needed`；proxy 的 cache 是 `None`，只增加 `calls` |
| `document.documentElement` 首次 | +1 | `Element` 不是 proxy，是 lazy 对象 |
| `document.createElement("div")` | +1 / 次 | 返回值经 `to_jsval` |
| `document.getElementById("x")` 首次 | +1 | 解析器创建的节点首次交给 JS |
| 同一对象再次访问 | 0 | 幂等 |
| `document.querySelectorAll(...)` | 0 | 返回 NodeList，是 proxy |
| `list[0]`（NodeList/HTMLCollection 索引） | +1 / 个 | getter 返回 Element |
| 内联 handler 首次派发（如 `<body onload=...>`） | +1 | +1 来自被派发的 **Event** 对象；编译 handler 只是把目标元素加入 scope chain，目标元素仍是 stub，之后 JS 取到它时再 +1 |
| 自定义元素 upgrade（`html_constructor`） | +1 | |
| DOM 对象传给任何 JS 函数/属性赋值 | +1（首次） | 走 `to_jsval` |
| 纯 JS 运算、`console.log` 数字 | 0 | |

## 4. 写用例的固定骨架

```js
// Document 是 named-properties proxy，不会 materialize；取它只是为了拿到 doc 句柄。
var doc = document;                 // +0（Document 是 proxy）
window.runtime.reset_materialize_stats();   // 从这里开始计数

var before = window.runtime.materialize_count();
var el = doc.createElement("div");  // 被测操作
var delta = window.runtime.materialize_count() - before;

LB.expect("createElement materializes once", 1, delta);
```

规则：

- **被测操作之前 reset**：Document/Window/NodeList 等 proxy 本身不贡献 +1，不需要为它们
  「预热」；reset 只是把口径固定在被测操作前。
- 被测操作前后各读一次计数，断言差值（delta），不断言绝对总数。
- `runtime` / `console.log` / 纯 JS 变量不产生 materialize；不要为了“看看”而打印 DOM 对象
  （在 refcount 用例里打印对象会改变引用计数，在这里虽不影响 materialize，但保持习惯）。
- 每个页面最后 `window.close()`。
- 输出用 `[PASS]` / `[FAIL]`（沿用 `tests/gc` 约定），由 runner grep。

## 5. 运行

```sh
./mach build --features js-quickjs --config 'patch.crates-io.mozjs.path="servo-quickjs/mozjs"'

./tests/gc/lazy-binding/run.sh --binary target/debug/servoshell
./tests/gc/lazy-binding/run.sh --binary target/release/servoshell --filter 04
```

runner 为每个页面设置 `SERVO_LAZY_BINDING_STATS=1`，用 `servoshell -z`（headless）打开
`file://` 页面，页面自己 `window.close()` 退出，超时兜底。

## 6. 常见坑

| 现象 | 原因 / 纠正 |
|---|---|
| 期待 `document` +1 但得到 0 | `Document` 有 named getter，是 proxy，永远不 materialize；lazy 的是 `documentElement`、`createElement` 的返回值等 |
| `materialize_count is not a function` | 不是 QuickJS 构建，或进程没设 `SERVO_LAZY_BINDING_STATS` |
| 断言 `calls` 具体数值 | `conversions.rs` 每次转换都调用入口，数值随实现变；只断言 `materialized` |
| `calls` 比预期多 1 | 测量语句里的 `window.runtime.materialize_calls()` 求值了 `window`（WindowProxy accessor），先缓存 `var runtime = window.runtime` |
| `querySelectorAll()` 期待 +1 | NodeList 是 proxy，+0；索引取值才 +1 |
| 内联 handler 的 +1 记到了目标元素上 | +1 实际是派发的 `Event`；编译 handler 后目标元素仍是 stub，随后 JS 取到它才 +1（见 `06-inline-handler.html`） |
| 在 iframe 里测 | 计数是 script thread 局部，父页面读不到 |
| 页面不退出 / 超时 | 忘记 `window.close()` |

## 7. 维护

- 新增会 materialize 的边界时，在 §3 表里加一行，并在 `tests/gc/lazy-binding/` 补用例。
- 新增用例后更新本文的期望值说明；行号会漂移，按符号名定位。
