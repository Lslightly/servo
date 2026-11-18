# DOM form owner

适用问题：`form_owner` 是什么、存在哪、什么时候变；Document 在 form owner 机制里做什么；
内联事件处理器的 scope chain 为什么要放它。

关键文件：`components/script/dom/html/form_controls/htmlformelement.rs`、
`components/script/dom/document/document.rs`、`components/script/dom/html/htmlelement.rs`。

> 行号是记录时快照，rebase 后按符号名定位。更长的叙述见
> `docs/agent/dom/event/eventtarget.md`。

## 1. 概念

规范：<https://html.spec.whatwg.org/multipage/forms.html#form-owner>

form-associated element 的 **form owner** 是它当前关联的 `<form>`（或 null）。

| 情况 | owner |
| --- | --- |
| 有 `form` 内容属性且 connected | 文档/所在 shadow tree 中 tree order 第一个 ID 等于该属性值的 `<form>` |
| 否则 | 最近的 `<form>` 祖先 |
| 都没有 | `None` |

两条独立的数据方向：

- 控件 → form：就是 `form_owner`（反向引用，`input.form` 返回它）；
- form → 控件：`HTMLFormElement.controls`（`DomRefCell<Vec<Dom<Element>>>`）。

## 2. 存储与接口

- trait：`FormControl`，`components/script/dom/html/form_controls/htmlformelement.rs`
  `form_owner()` / `set_form_owner()` / `to_html_element()`；
- 普通控件字段：`form_owner: MutNullableDom<HTMLFormElement>`，例如
  - `htmlinputelement.rs`（字段 + `impl FormControl`，`set_form_owner` 还会通知 input type）；
  - `htmlbuttonelement.rs`、`htmlfieldsetelement.rs`、`htmltextareaelement.rs`、
    `htmloutputelement.rs`、`htmllegendelement.rs`、`htmlimageelement.rs`、`htmlobjectelement.rs`；
- form-associated custom element：`HTMLElement` 也实现 `FormControl`，
  owner 存在 `ElementInternals`（`html/internals/elementinternals.rs` 的 `form_owner` 字段）；
- `Element::as_maybe_form_control()`（`htmlformelement.rs` 末尾）是统一判定入口：
  上面的具体类型 + `is_form_associated_custom_element()` 的 `HTMLElement`。

## 3. 重算：reset the form owner

实现在 `FormControl::reset_form_owner`（`htmlformelement.rs`）：

```text
old = self.form_owner()
若 old 存在、元素没有 form 属性、且最近的 form 祖先 == old → 直接返回
new = (listed && 有 form 属性 && connected)
        ? document/shadow_root.GetElementById(form 值)，且必须是 <form>
        : 最近的 form 祖先
if old != new:
    old.remove_control(control)
    new.add_control(control)          // 按 tree order 插入；同时清 past_names_map
    custom element → 入队 CallbackReaction::FormAssociated（formAssociatedCallback）
    self.set_form_owner(new)
```

触发点：

| 时机 | 函数 |
| --- | --- |
| 解析器建元素时直接关联（跳过随后的 reset） | `set_form_owner_from_parser` + `NodeFlags::PARSER_ASSOCIATED_FORM_OWNER`；调用在 `servoparser/mod.rs`、`servoparser/async_html.rs` |
| 插入文档树 | `Element::bind_to_tree` → `bind_form_control_to_tree`（`dom/element/element.rs`） |
| 移出文档树 | `Element::unbind_from_tree` → `unbind_form_control_from_tree` |
| 节点移动 | `HTMLElement::moving_steps` → `FormControl::moving_steps` |
| `form` 属性增删/改值 | `form_attribute_mutated` |
| form 自己移出树 | `HTMLFormElement::unbind_from_tree`：对 `controls` 中不在同一 home subtree 的控件逐个 reset |
| 文档中某 ID 注册/注销 | `Document::reset_form_owner_for_listeners` |
| 自定义元素 upgrade 等 | `dom/node/customelementregistry.rs` |

`unbind_form_control_from_tree` 的顺序有讲究：先 `unregister_if_necessary()` 从 ID 监听表摘除，
再决定是否 reset；带 `form` 属性的控件要额外 reset，否则 form owner 之后 unbound 时它已不在表里、
不会再被通知（代码注释里有解释）。

## 4. Document 的角色

Document **没有** `form_owner` 字段。它做两件事：

1. **按 ID 查找**：`reset_form_owner` 里用 `GetElementById`（`Document.id_map`，
   由 `register_element_id` / `unregister_element_id` 维护）；在 shadow tree 里则查 shadow root。
2. **失效通知**：`Document::form_id_listener_map`
   （`document.rs`，字段注释 "Map from ID to set of form control elements..."）是
   `form 属性值 → 使用该 ID 的控件集合` 的反向索引。
   - 控件在 `register_if_necessary`（listed + `form` 非空 + connected）登记，
     `unregister_if_necessary` 注销；
   - 任意元素 ID 被 register/unregister 时，`register_element_id`/`unregister_element_id`
     调用 `reset_form_owner_for_listeners(id)`，遍历该 ID 下的控件逐个 `reset_form_owner`。

这覆盖 `<input form="f">` 与 `<form id="f">` 出现顺序任意、form 后插入/先删除等场景。
`name` 变化不触发（`form` 属性只看 ID）。

## 5. 谁在用

- `input.form`/`button.form` 等 IDL getter；
- 提交：submit button 必须属于目标 form（`dom/form/formdata.rs`）；
- 校验：控件让 owner form 更新 validity（`dom/form/validitystate.rs`）；
- 属性继承：`get_form_attribute` / `get_form_boolean_attribute` 在控件自身缺属性时取 owner 的
  `action`/`method`/`enctype` 等；
- `<label>` 找控件、radio group 判定、`fieldset` 有效性。

## 6. 与 lazy binding 的交集

内联事件处理器编译时，`form_owner` 会被放进 scope chain
（`eventtarget.rs` 的 `get_compiled_event_handler`）。那里**没有**对它调用
`materialize_if_needed()`，直接 `get_jsobject()`；而解析器创建的 `<form>` 很可能还是
`JSDomStub`。判断与修复见 `references/dom-event-handlers.md`、
`references/lazy-binding-materialize.md`。
