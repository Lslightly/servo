# 内联事件处理器（event handler attributes）

适用问题：`onclick` 等 HTML 属性什么时候编译、在哪编译、为什么编译时要碰
document/form owner/element 的 `JSObject`、为什么 `onload` 属性的内联处理器会触发
`JSDomStub` panic。

关键文件：`components/script/dom/event/eventtarget.rs`、`components/script/dom/event/event.rs`。

> 行号是记录时快照，rebase 后按符号名定位。

## 1. 两阶段

```text
解析器遇到 onclick="..."        set_event_handler_uncompiled()   只存 InternalRawUncompiledHandler
                                 ▼
JS 读 element.onclick ──┐        get_compiled_event_handler()     编译成函数，缓存为
事件派发 invoke listener ┘                                          InlineEventListener::Compiled
```

- **未编译**：`set_event_handler_uncompiled`（`eventtarget.rs`）做 CSP 检查后，
  以 `InlineEventListener::Uncompiled(InternalRawUncompiledHandler { source, line, url })`
  存进 `EventTarget.handlers`；**不碰 JS**。
- **编译时机**有两个入口，都会走到同一个 `EventTarget::get_compiled_event_handler`：
  1. IDL getter：`element.onclick` → `EventTarget::get_event_handler_common`
     → `get_inline_event_listener` → `EventListeners::get_inline_listener`
     → `get_compiled_handler` → `owner.get_compiled_event_handler`；
  2. 事件派发：`dom/event/event.rs` 的 `inner_invoke` → `get_compiled_listener`
     → `get_compiled_handler` → `get_compiled_event_handler`。

  **重点**：派发路径不需要 JS 先读过 `onclick`。所以解析器创建、从未被 JS 访问过的元素
  （典型 `<iframe onload=...>`）也会触发编译，进而取 `get_jsobject()`。
- 编译结果缓存为 `Compiled(CommonEventHandler)`，之后复用；`get_compiled_handler`
  在借用期间先把 inline listener 换成 `Null`，编译后再写回。

## 2. 编译流程（对应规范 "getting the current value of the event handler"）

`EventTarget::get_compiled_event_handler`：

| 步骤 | 代码行为 |
| --- | --- |
| 3.1 | `element = self.downcast::<Element>()`；`document = element.owner_document()` 或 Window target 的 `window.Document()` |
| 3.2 | `document.scripting_enabled()` 否则返回 None |
| 3.3–3.4 | 源码转 UTF-16；行号来自 handler |
| **3.5** | `form_owner = element.as_maybe_form_control().and_then(|f| f.form_owner())` |
| 3.6/3.8 | settings objects：TODO |
| 3.7 | 解析错误留到 `CompileFunction` 失败时再报 |
| 3.9 | `enter_auto_realm(cx, window)`；函数名 `on<type>`；Element target 参数只有 `event`；`Window` 上的 `error` 有 5 个参数（`event, source, lineno, colno, error`） |
| **3.9 Scope** | 构建 `EnvironmentChain`：`document` → `form_owner`（可选）→ `element`（见下） |
| 3.12 | 用 `JS_GetFunctionObject` 包装成 `EventHandlerNonNull` / `OnErrorEventHandlerNonNull` / `OnBeforeUnloadEventHandlerNonNull` |

## 3. Scope chain

```rust
let scopechain = js::rust::EnvironmentChain::new(cx, SupportUnscopables::Yes);
if let Some(element) = element {
    scopechain.append(document.reflector().get_jsobject().get());
    if let Some(form_owner) = form_owner {
        scopechain.append(form_owner.reflector().get_jsobject().get());
    }
    #[cfg(feature = "js-quickjs")]
    unsafe { element.reflector().materialize_if_needed(); }   // 本 fork 新增
    scopechain.append(element.reflector().get_jsobject().get());
}
```

- 后 append 的在更内层：`element` 最内，`form_owner` 次之，`document` 最外。
  效果是处理器体里能按名字拿到 `this`/元素、owner form、document 的命名属性
  （如 `onclick="myForm.myInput.value = ''"`）。
- **只有 Element target 建这条链**；`Window` target 的 `element` 是 `None`，
  处理器直接跑在全局作用域。
- `EnvironmentChain` 实现在 `servo-quickjs/mozjs/src/rust.rs`；`append` 只收裸
  `*mut JSObject`，**不会** materialize。

## 4. 与 lazy binding 的交集（重要）

三处 `get_jsobject()` 的 materialize 状态：

| 接收者 | materialize | 风险 |
| --- | --- | --- |
| `element` | 有（显式，append 之前） | 已处理 |
| `document` | 无 | 通常已被 JS 访问过（`window.document`），但本函数无保证 |
| `form_owner` | 无 | 解析器创建的 `<form>` 很可能仍是 stub → panic |

复现路径：`<form id="f"><input onclick="..."></form>`，派发事件触发编译
（`event.rs` 的 `inner_invoke`），中间没有任何 JS 访问过 `<form>`。

修复要求：

- materialize 必须发生在对应 `append` **之前**（append 存裸指针；之后 materialize 会
  留下已 unbound 的旧 stub）；
- 三个对象都要处理，或先确认调用路径上确实不可能为 stub；
- `materialize_if_needed` 自己会切到 stub 的 realm，外层已有 `enter_auto_realm` 也没问题。

详细的机制、trace 与两种修复方案见 `references/lazy-binding-materialize.md`。

## 5. 相关但不同

- **`addEventListener` 的回调**不是 inline handler：存的是 `EventListenerType::Additive`
  （`CompiledEventListener::Listener`），不经过上面的编译流程，也不需要 scope chain。
- `BeforeUnloadEventHandler`、`ErrorEventHandler`、普通 `EventHandler` 只是编译结果的包装类型；
  `Window` 上的 `onerror` 参数个数不同。
- 处理器异常：`call_or_handle_event` 里 `ErrorEventHandler` 对 `Window`/`WorkerGlobalScope`
  有特殊处理（把 error event 转成返回值语义）。

## 6. 代码索引

| 内容 | 位置 |
| --- | --- |
| 未编译 handler 的存储 | `eventtarget.rs` 的 `set_event_handler_uncompiled`、`InlineEventListener` |
| 编译入口 | `eventtarget.rs` 的 `get_compiled_handler`、`get_compiled_event_handler` |
| getter 路径 | `eventtarget.rs` 的 `get_event_handler_common`、`get_inline_event_listener` |
| 派发路径 | `event.rs` 的 `inner_invoke` → `get_compiled_listener` → `call_or_handle_event` |
| scope chain | `eventtarget.rs` 的 `get_compiled_event_handler` 内 `EnvironmentChain` 段 |
| QuickJS 实现 | `servo-quickjs/mozjs/src/rust.rs` 的 `EnvironmentChain` |
| form owner | `references/dom-form-owner.md` |
