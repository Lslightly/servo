---
name: servo-ohos-architecture
description: servo-ohos 分支的架构/机制速查：QuickJS lazy binding 与 JSDomStub materialize、DOM 对象生命周期、内联事件处理器编译、form owner、crate 布局与 codegen 边界。当需要判断 DOM 对象何时变成真 JSObject、排查/预防 "get_jsobject() called on a reflector whose object is a JSDomStub"、理解 form owner 或事件处理器作用域链、或决定改 script 还是 script_bindings 时使用；先读本 skill 的 reference，不要直接全仓 grep。
---

# servo-ohos architecture

本分支在 Servo 之上做了 **QuickJS shim + lazy DOM binding**。很多在 upstream 里理所当然的假设
（"DOM 对象的 reflector 一定是 `JSObject`"）在这里不成立，重复 grep 才能确认的机制都记在
`references/` 下。

## 用法

1. 先按下面的索引打开对应 reference，再决定是否看源码。
2. reference 里的 `file:line` 是记录时的快照；rebase 后行号会漂移，**按符号名在文件里定位**，
   行号只当提示。
3. 新结论写回对应 reference（或新建一个），不要只留在对话里；这是本 skill 存在的目的。
4. 只查单个符号/调用链时仍优先 `codegraph_explore "<符号或问题>"`，比 grep 快。

## References

| 主题 | 文件 | 内容 |
| --- | --- | --- |
| Lazy binding / materialize | `references/lazy-binding-materialize.md` | `JSDomStub`、`wrap`、`materialize_if_needed`、`get_jsobject` panic、`rootable().get()`、判断 stub 与两种修复方案、trace |
| Form owner | `references/dom-form-owner.md` | form owner 的定义/存储/重算触发点、Document 的查找与失效通知、常见消费方 |
| 内联事件处理器 | `references/dom-event-handlers.md` | 何时编译、编译入口、scope chain（document/form owner/element）、Window/Element 差异 |
| Crate 布局与 codegen | `references/crate-layout.md` | `script` vs `script_bindings`、`#[dom_struct]` 约定、生成代码边界、root/reflector 常用 API |

## 30 秒速查

- **看到 `JSDomStub` panic**：调用点不是 `reflector.rs` 的断言处，而是回溯里下一层手写帧。
  详细流程用 `fix-get-jsobject` skill；机制和两种修复（`rootable().get()` vs
  `materialize_if_needed()`）见 `references/lazy-binding-materialize.md`。
- **对象何时 materialize**：`wrap()` 之后默认是 stub；只有 `ToJSValConvertible for Reflector`
  （对象作为 JS 值交出去时）和少数显式调用点会 materialize。`get_jsobject()` 不做这件事。
- **`get_jsobject()` vs `rootable().get()`**：前者要求真 `JSObject`，stub 时 panic；后者只取
  裸指针（refcount / 身份 / realm 可用，交给 JSAPI 不行）。
- **form owner 不是存在 Document 里**：存在控件自身；Document 只负责按 `form="id"` 查找和
  在 ID 增删时通知重算。
- **不要改 `script_bindings::codegen::*`**：WebIDL 生成代码，改手写调用者。

## 维护

- 新增 reference 时，在上表加一行，并在 reference 顶部写清"适用问题 / 关键文件"。
- 修正行号时顺手确认符号与行为仍一致；行为变了要改正文，不只是改行号。
