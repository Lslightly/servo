# Lazy binding：JSDomStub 与 materialize

适用问题：

- DOM 对象什么时候才是真正的 `JSObject`？
- 为什么 `get_jsobject()` 会 panic，怎么判断该用 `rootable().get()` 还是 `materialize_if_needed()`？
- 某个新写的调用点/新 API 会不会踩到 stub？

关键文件：`components/script_bindings/wrap.rs`、`components/script_bindings/reflector.rs`、
`components/script_bindings/conversions.rs`、`servo-quickjs/mozjs/src/rust.rs`。

> 行号是记录时快照，rebase 后按符号名定位。

## 1. 模型

`js-quickjs` 构建下，DOM 对象在 `wrap()` 时**不创建真正的 `JSObject`**，而是创建一个
`JSDomStub`（QuickJS 侧的一种轻量对象），并把 materialize 所需信息记在
`Reflector::materialize_cache` 里：

```text
Rust DOM struct ──reflector.object──> JSDomStub        （lazy 状态）
                                        │  materialize_if_needed()
                                        ▼
                                      JSObject          （可交给 JSAPI）
```

- `wrap_object`（`components/script_bindings/wrap.rs:64-110`）：
  - `config.is_proxy == false` → `JS_NewDomStub`，并 `set_materialize_cache`；
  - `is_proxy == true`（WindowProxy、跨源对象等）→ 直接创建真对象，从不需要 materialize。
- stub 与真对象都通过 `DOM_OBJECT_SLOT` 指向同一个 Rust DOM 对象。
- `Reflector` 字段：`object`、`materialize_cache`（`Option<MaterializeCache>`，
  非空=还是 stub）、`counted`（QuickJS 引用计数用）。

## 2. 什么时候 materialize

`materialize_if_needed()`（`components/script_bindings/reflector.rs:425-533`）：

- 若 `materialize_cache` 为 `None`（已 materialize / proxy）→ 直接返回，幂等；
- 否则：读 stub 上记录的 class/realm/global，`EnterRealm` 到 stub 自己的 realm，
  惰性创建接口的 canonical prototype，`JS_UnbindDomStub` 旧 stub，
  用 `JS_NewObjectWithGivenProto` 创建真对象，把 stub 的引用计数搬过去，
  复制 `[LegacyUnforgeable]` 成员，`rebind_jsobject` + 清 cache。

触发方式只有两类：

| 方式 | 位置 | 说明 |
| --- | --- | --- |
| 自动：DOM 对象作为 JS 值交出去 | `ToJSValConvertible for Reflector<T>`，`components/script_bindings/conversions.rs:176-190` | `DomRoot`/`Reflector` 转 `JSVal` 时自动 materialize；`to_jsval` 是最常见的边界 |
| 显式调用 `materialize_if_needed` | 见下表 | 需要裸 `*mut JSObject` 但对象可能还是 stub 时 |

当前显式调用点（写新代码前先 `rg -n "materialize_if_needed"` 核对最新列表）：

| 位置 | 场景 |
| --- | --- |
| `components/script/dom/bindings/constructor.rs:232` | `html_constructor` 返回新元素前 |
| `components/script/dom/promise/promise.rs:387` | promise 的 native handler 转交 JS 前 |
| `components/script/dom/event/eventtarget.rs:756` | 编译内联事件处理器，把 `element` 放进 scope chain 前 |

注意 `document`/`form_owner` 在 `eventtarget.rs:751-753` **没有** materialize，
直接 `get_jsobject()`；这是已知的潜在 panic 点，详见
`references/dom-event-handlers.md` 与 `docs/agent/dom/event/eventtarget.md`。

## 3. 三个取对象指针的 API

| API | 返回 | 对 stub 的行为 | 适用 |
| --- | --- | --- | --- |
| `get_jsobject()` | `HandleObject<'_>` | **panic**：`get_jsobject() called on a reflector whose object is a JSDomStub`（`reflector.rs:257-265`） | 确定已是真对象，或马上要用它当 `JSObject` |
| `rootable().get()` | `*mut JSObject` | 不检查、不 materialize，stub 也照拿 | 只需要原始指针：引用计数、身份比较、`AutoRealm`、`GlobalScope::from_object`、`quickjs_refs` 注册 |
| `materialize_if_needed()` | `()`（`unsafe`） | 把 stub 换成真对象 | 后面要交给 JSAPI，且对象可能是 stub |

`is_js_binded()`（`reflector.rs:363-372`）返回 "cache 已清空"，可用于调试/断言，
业务代码里一般不需要。

## 4. 判断与修复

先用 `codegraph_explore` 或回溯找到调用点与 **receiver**，再判断 receiver 是否可能是 stub：

- 刚由解析器 / `createElement` / 构造函数创建，且从未被 JS 访问 → **是 stub**；
- 测试脚本通过 `document.*`、属性 getter 等拿到过，值经过了 `to_jsval` → 已 materialize；
- 条件是 `Option`/分支，路径不会执行到 → 不相关；
- 同一个函数里有多个 `get_jsobject` 时，只看崩溃路径上会执行的 receiver。

修复二选一（完整流程见 `fix-get-jsobject` skill）：

**A. `get_jsobject().get()` → `rootable().get()`**，仅当后续**不**把指针交给 JSAPI：
不用于 `JS_WrapObject` / `JS_SetPrototype` / `JS_GetProperty` / `JS_Call*` / 作为值返回。
典型：`components/script_bindings/refcounted.rs` 里的 `retain_object`/`release_object`。
注意 `get_jsobject()` 返回 `HandleObject`，`rootable().get()` 返回 `*mut JSObject`，
只有原本已经 `.get()` 取指针的位置能直接替换。

**B. 在原调用前插入 materialize**：

```rust
#[cfg(feature = "js-quickjs")]
unsafe { receiver.reflector().materialize_if_needed(); }
let obj = receiver.reflector().get_jsobject().get();
```

规则：

- 必须在 `get_jsobject()` **之前**执行；materialize 会换掉 `object` 指针，
  之后再 materialize 会留下已 unbound 的旧 stub 指针（放进环境链/GC 结构里尤其危险）。
- 同一 receiver 多处调用，一次放在第一处之前即可。
- `#[cfg(feature = "js-quickjs")]` + `unsafe` 两者都要，SpiderMonkey 构建也要能编译。
- receiver 在 `rooted!` 初始化表达式里构造时，把 materialize 拆成独立语句放在 `rooted!` 之前。
- `materialize_if_needed` 自己会切到 stub 记录的 realm 再切回来，调用处不需要额外 `EnterRealm`。
- 拿不到 receiver（生成代码/宏里）→ hoist 到最近的手写调用者；
  或在对象创建/返回处 materialize，并在报告里说明原因。

## 5. 常见坑

| 坑 | 纠正 |
| --- | --- |
| 把 `reflector.rs` 的 panic 断言行当调用点 | 调用点是回溯里下一层手写帧 |
| 改 `script_bindings::codegen::*` / `*_Binding::*` | WebIDL 生成代码，改手写调用者 |
| 在 materialize 之前 `get_jsobject()`，或放在之后 | 必须之前 |
| 需要 `HandleObject` 的地方用 `rootable().get()` | 类型不对；要么 materialize 后用 `get_jsobject()`，要么确认只需要裸指针 |
| 需要真 `JSObject` 的地方用 `rootable().get()` | 从清晰 panic 变成 UB |
| 漏 `#[cfg(feature = "js-quickjs")]` 或 `unsafe` | 二者都必须 |
| 以为 proxy 也会是 stub | proxy 从不进 materialize_cache；`is_js_binded()` 恒真 |
| 以为 `to_jsval` 覆盖了所有边界 | `EnvironmentChain::append`、`JS_*` 直接调用都绕过它 |
