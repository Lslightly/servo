---
name: fix-get-jsobject
description: 当 js-quickjs 构建下出现 `get_jsobject() called on a reflector whose object is a JSDomStub` 的 panic / WPT CRASH（回溯里有 `<script_bindings::reflector::Reflector>::get_jsobject`），需要从崩溃信息定位具体是哪个 `get_jsobject()` 调用点、并产出 `jsobject_log/<测试html同名>.md` 分析报告时使用。
---

# 定位 get_jsobject 的 JSDomStub 崩溃

## 概述

QuickJS 下 DOM 对象首次交给 JS 前，`Reflector.object` 是一个 `JSDomStub`，不是真正的
`JSObject`。任何在 materialize 之前调用 `Reflector::get_jsobject()` 的代码都会在
`components/script_bindings/reflector.rs:264` panic：

```
get_jsobject() called on a reflector whose object is a JSDomStub
```

**核心原则：`reflector.rs:264` 是断言位置，不是调用点。** 真正的调用点是回溯中
`get_jsobject` 帧下一层的**Rust 帧**。本 skill 只产出一份分析报告，不改代码。

## 输入

- 崩溃信息：panic 行 + 带符号的回溯（来自 wpt 日志、终端或用户粘贴）。
- 关联的测试 html（可从中提取，见步骤 1）。
- 可选：`SERVO_MATERIALIZE_TRACE` 输出、`etc/run_materialize_trace.py` 的结果。

## 已知崩溃签名（先核对）

命中下表时可直接跳到对应 `file:line` 核对，再按步骤 4–7 出报告。遇到新签名请补充到这张表。

**报告与测试列表按签名聚合（见步骤 1、7）**：同一调用点只写一份报告，测试名追加到
`<key>.tests.txt`；不要为每个测试重复生成报告。

| 帧 7 / 帧 8 | 调用点 | 接收者 | 常见触发 |
|---|---|---|---|
| `Reflector::get_jsobject` / `html_constructor` | `components/script/dom/bindings/constructor.rs:231` | `result`（`DomRoot<Element>`） | `document.createElement(已注册自定义元素)`、`new CustomElement()` |
| `get_compiled_handler`（`get_compiled_event_handler` 被内联） | `components/script/dom/event/eventtarget.rs:755` | `element`（`&Element`） | 解析器创建的 `<iframe onload=...>` 等元素派发事件、编译内联处理器 |

### 签名 A：html_constructor

```rust
rooted!(&in(cx) let mut element = result.reflector().get_jsobject().get());
unsafe {
    if !JS_WrapObject(cx, element.handle_mut()) { return Err(()); }
    if !JS_SetPrototype(cx, element.handle(), prototype.handle()) { return Err(()); }
    result.to_jsval(cx, MutableHandleValue::from_raw(call_args.rval()));
}
```

- 元素来自 Step 7 的 `HTMLElement::new` / `create_native_html_element`，或 Step 9 的
  `element.as_rooted()`；两条分支都可能是 stub，修复要对 `result` 统一处理。
- 第 241 行 `result.to_jsval(...)` 不崩溃：`ToJSValConvertible for Reflector<T>`
  （`components/script_bindings/conversions.rs:176-189`）会先 materialize。
- 结论：方案 B。指针随后进入 `JS_WrapObject` / `JS_SetPrototype` / 返回给 JS，方案 A 不适用。

### 签名 B：get_compiled_handler / get_compiled_event_handler

`get_compiled_handler` 体内没有 `get_jsobject`，真正调用点在它调用的
`EventTarget::get_compiled_event_handler` 的作用域链构建处：

```rust
if let Some(element) = element {
    scopechain.append(document.reflector().get_jsobject().get());       // 顶层文档通常已 materialize
    if let Some(form_owner) = form_owner {
        scopechain.append(form_owner.reflector().get_jsobject().get()); // iframe 等场景为 None
    }
    scopechain.append(element.reflector().get_jsobject().get());        // 崩溃点：755
}
```

- 崩溃接收者是 `element`：解析器创建、带内容属性 `onload` / `onerror` 的元素。
  `set_event_handler_uncompiled`（`eventtarget.rs:648-683`）只保存原始文本，不 materialize。
- `document` 是顶层文档，测试脚本 / testharness 已访问过（已 materialize）；`form_owner` 通常为 `None`。
- 结论：方案 B。指针要进入 `EnvironmentChain` 并交给 `CompileFunction`
  （`eventtarget.rs:758-768`），必须是真 `JSObject`。

## 步骤

### 1. 确定测试 html 和签名 key

按优先级取 html 路径：

1. 用户明确给出的 html 路径。
2. 崩溃所在的 `▶ CRASH [expected OK] /dom/.../foo.html` 块的标题行。
3. 回溯之前最近的 `[n/845] /dom/.../foo.html` 进度行。

**报告按签名聚合，不按测试重复。** 签名 key 由帧 7/8 的函数名与源文件 stem 组成：

```
帧 8 html_constructor + components/script/dom/bindings/constructor.rs
  → key: html_constructor.constructor
帧 7 get_compiled_handler + components/script/dom/event/eventtarget.rs
  → key: get_compiled_handler.eventtarget
```

- 同一函数内出现**不同接收者或不同修复**时，追加 `.<receiver>` 消歧。
- 行号**不进文件名**（rebasing 会移动），行号写在报告里。

文件：

| 内容 | 路径 |
|---|---|
| 签名报告 | `jsobject_log/<key>.md` |
| 测试列表 | `jsobject_log/<key>.tests.txt`（每行 `<commit> <html 路径>`） |
| 索引 | `jsobject_log/INDEX.md` |


### 2. 从回溯找嫌疑调用函数

回溯通常长这样：

```
7: <script_bindings::reflector::Reflector>::get_jsobject
8: script::dom::bindings::constructor::html_constructor      ← 嫌疑函数 F
```

若 `get_jsobject` 被内联，回溯里**没有**这一帧，帧 7 直接就是调用者（见"已知崩溃签名"签名 B）：

```
7: script::dom::event::eventtarget::get_compiled_handler     ← 嫌疑函数 F
8: script::dom::event::event::inner_invoke
```

从 `get_jsobject` 帧（存在时）往帧号增大方向，跳过基础设施帧，第一个**手写(不像编译器生成或者标准库函数的) Rust 帧**就是嫌疑函数 F：

- 跳过 `std::` / `core::` / `__rustc::` / `mozjs::` / `servoshell::`。
- 跳过 `script_bindings::codegen::*` —— 这是构建期从 WebIDL 生成的文件，**永远不是修改点**。
- 跳过 `script_bindings::settings_stack::run_a_script`、`script_bindings::panic::wrap_panic`。
- 跳过纯 C 帧（`JS_*`、`js_*`、`async_func_resume` 等）。

去掉符号里的泛型参数和闭包后缀，得到可搜索的名字：

```
script::dom::bindings::constructor::html_constructor
  → 函数 html_constructor
  → components/script/dom/bindings/constructor.rs
```

同时把 F 之后 2–3 帧（尤其 codegen 帧）记录进报告，用来说明调用路径。

### 3. 符号 → 源文件

符号的第一段是 crate 的 lib name（见对应 `Cargo.toml` 的 `[package] name`），
其后是模块路径，最后一段是函数/类型：

| crate | 目录 |
|---|---|
| `script` | `components/script/` |
| `script_bindings` | `components/script_bindings/` |

定位手段，按优先级：

1. `codegraph_explore "<函数名或符号>"`（本仓库已索引，一次调用返回源码+调用路径）。
2. `git grep -n "fn <函数名>" components/`。
3. 对 `<T as Trait>::method` 这类符号，用 codegraph 搜 `method`，或
   `git grep -n "fn <method>" components/` 后逐个核对 impl。

### 4. 找 get_jsobject 调用与接收者

在 F 的函数体内找调用：

```sh
git grep -n -E 'get_jsobject\(\)|\.jsobject\(\)' -- <file>
```

- 取 F 体内、崩溃路径上会执行到的那一行。
- 记录**接收者**，它就是要 materialize 的对象：

```rust
// components/script/dom/bindings/constructor.rs:231
rooted!(&in(cx) let mut element = result.reflector().get_jsobject().get());
//                                  ^^^^^^ receiver
```

- `ThisReflector::jsobject()`（`components/script_bindings/callback.rs:45`）同样算作
  `get_jsobject` 调用点，接收者是 `self`。

如果 F 体内**没有**任何 `get_jsobject`，说明真正调用点在 F 直接调用的、被内联的手写函数里
（编译器内联、宏展开、trait 默认实现）。不要停下，按顺序继续：

1. 在 F 体内找转发调用（如 `owner.get_compiled_event_handler(cx, handler, ty)`），用 codegraph
   确认被调函数是否含 `get_jsobject`；同文件、小函数、私有方法的可能性最大。
2. 在内联目标函数里按本节规则确定调用行与接收者；报告里把 F 与内联目标都写上。
3. 只有 F 及其直接被调函数都找不到时，才写"`get_jsobject`调用点未知"，并列出排查过的函数。

**同一函数有多个 `get_jsobject` 时如何选接收者**（例如
`components/script/dom/event/eventtarget.rs:750-756` 的 `document` / `form_owner` / `element`）：

- 只把崩溃路径上会执行到、且**可能是 stub** 的接收者列为嫌疑。
- 判断 stub 状态：刚由解析器/构造器创建、且从未被 JS 访问的对象是 stub；
  测试脚本已经通过 `document.*` 等方法访问过的对象已 materialize。
- 结构上排除：`Option` 为 `None` 的接收者不会执行；不在当前 `if` 分支内的接收者不执行。
- 回读触发测试的 html/js（如 `<iframe onload=...>`）确认对象来源与首次暴露时机。

### 5. 推断为什么仍是 stub

在报告里用一两句说明，依据是 F 的语义，不要空泛：

- 对象刚被创建、还没交给过 JS（构造器返回、`createElement`、`create_native_html_element`）→ 正常的 lazy 状态。
- 对象本应已 materialize（曾被 `to_jsval` / wrap 过），说明走的是另一条未 materialize 的路径。
- 对象来自 `Reflector::new()` 后直接绑定，或来自 `Promise`/回调对象，其反射器从未经过 wrap。

### 6. 解决方案

**方案 A：改用 `rootable().get()`**，绕过 `get_jsobject()` 的 stub 断言。

```diff
- x.reflector().get_jsobject().get()
+ x.reflector().rootable().get()
```

适用条件（必须同时满足）：

- 调用点只需要一个 `*mut JSObject` 原始指针：引用计数（`retain_object` / `release_object`）、
  身份比较、进入 realm（`AutoRealm`）、`GlobalScope::from_object`、`quickjs_refs` 注册等。
- 后续**不**把它当作真正的 `JSObject` 交给 JSAPI：不用于 `JS_WrapObject`、`JS_SetPrototype`、
  `JS_GetProperty` / `JS_SetProperty`、`JS_Call*`，也不作为值返回给 JS。

`rootable()` 返回 `&Heap<*mut JSObject>`，`.get()` 既不 materialize，也不检查 stub，
选它等于明确承认「这里拿到 stub 也可以」。若消费者其实需要真对象，这会把一次清晰的 panic
换成 UB —— 不满足条件时用方案 B。

现有写法（`components/script_bindings/refcounted.rs`，引用计数只需要池里的对象指针，
stub 也可以）：

```rust
// 注册引用：LIVE_DOM_REFERENCES 的 Vacant 分支
#[cfg(feature = "js-quickjs")]
unsafe {
    js::quickjs::retain_object((*(ptr as *const Reflector)).rootable().get())
};

// 扫描并释放引用：sweep_live_references
unsafe {
    js::quickjs::release_object((*(*ptr as *const Reflector)).rootable().get())
};
```

这两处原本都是 `get_jsobject().get()`，改成 `rootable().get()` 后不再因 stub panic。

注意：`get_jsobject()` 返回 `HandleObject<'_>`，`rootable().get()` 返回 `*mut JSObject`。
只有原本就是 `x.reflector().get_jsobject().get()`（或对返回值再取 `.get()`）的位置能直接替换；
需要 `HandleObject` 参数的位置不能用方案 A。`rootable()` 在 SM/QuickJS 下都存在，无需 `cfg`。

**方案 B：在 F 内、紧邻 `get_jsobject()` 调用之前，对同一 receiver：**

```rust
#[cfg(feature = "js-quickjs")]
unsafe { result.reflector().materialize_if_needed(); }
rooted!(&in(cx) let mut element = result.reflector().get_jsobject().get());
```

规则：

- receiver 已是 `Reflector<T>` 时直接 `unsafe { receiver.materialize_if_needed(); }`。
- receiver 必须在调用点已可用且非空；若它是在 `rooted!` 的初始化表达式里构造的，
  把 materialize 拆成独立语句放在 `rooted!` **之前**。
- 同一 receiver 在 F 内有多处 `get_jsobject` 时，一次 materialize 放在第一处之前即可。
- `#[cfg(feature = "js-quickjs")]` 和 `unsafe` 缺一不可（SpiderMonkey 构建也要能编译）。
- materialize 必须在 `get_jsobject` **之前**，且不能在它返回的 `HandleObject` 已被使用之后。

**方案 C（方案 B 无法实施时）**，按顺序考虑并在报告里说明理由：

1. 调用点在生成代码/宏里 → hoist 到最近的手写调用者，在对象离开 Rust 边界前 materialize。
2. 崩溃经 `to_jsval` 触发 → 检查 `ToJSValConvertible for Reflector<T>`
   （`components/script_bindings/conversions.rs`）是否已覆盖该类型；未覆盖则在那加。
3. 对象是 `Promise` / `RootedCallback` / `CallbackObject` → 参照
   `components/script/dom/promise/promise.rs:387` 的 `append_native_handler` 写法。
4. 调用点拿不到 receiver → 在对象的创建/返回处 materialize，并说明为什么不是首选。

### 7. 写报告（按签名聚合）

1. 先读 `jsobject_log/INDEX.md`（很小）。若已有该 key：
   **不要重写报告**，只把本次测试追加到列表；先 grep 去重，不读列表全文：

```sh
sha=$(git rev-parse --short HEAD)
f=jsobject_log/<key>.tests.txt
grep -qxF "$sha $html" "$f" || printf '%s %s\n' "$sha" "$html" >> "$f"
```

2. 若没有该 key：写一份签名级报告 `<key>.md`、建立 `<key>.tests.txt`，
   并向 `jsobject_log/INDEX.md` 追加一行索引。首次创建时先写表头：

```markdown
| 签名 | 调用点 | 报告 | 测试列表 |
|---|---|---|---|
| `html_constructor` | `components/script/dom/bindings/constructor.rs:231` | [html_constructor.constructor.md](html_constructor.constructor.md) | [html_constructor.constructor.tests.txt](html_constructor.constructor.tests.txt) |
```

报告必须包含下面模板里的 **REQUIRED** 段（缺失任何一段都算未完成）。
报告只写一次；多个测试共用时，差异写进"关键回溯（观察到的变体）"。

```markdown
# get_jsobject / JSDomStub 崩溃：<key>

- 签名：`<帧 7/8 符号>`
- 调用点：`components/...:NN`
- 接收者：`...`（类型 `...`）
- 适用测试：见 `<key>.tests.txt`（每行 `<commit> <html 路径>`）
- 源码版本：`<git rev-parse --short HEAD>`（`<commit subject>`）
- 生成时间：<YYYY-MM-DD HH:MM>

## 关键回溯（观察到的变体）
| 帧 | 符号 |
|----|------|
| 7 | `<script_bindings::reflector::Reflector>::get_jsobject`，或帧 7 直接是手写函数 |
| 8 | `<嫌疑函数 F>` / `<内联目标>` |
| 9+ | `<触发路径；不同测试不一致时并列>` |

## get_jsobject 调用点（REQUIRED）
- 文件/行：`components/...:NN`
- 函数：`...`（若被内联，写明真实调用函数）
- 接收者：`...`（类型 `...`）
- 代码：
```rust
<原样摘录包含 get_jsobject 的语句>
```

## 为什么仍是 JSDomStub（推断）
<一两句，依据 F 的语义；内联推断要标注置信度>

## 建议的修复（REQUIRED）

### 方案 A：改用 `rootable().get()`
- 适用性：<调用点只使用原始指针：refcount / 身份比较 / 进入 realm / 注册；不交给 JSAPI>
- 位置：`components/...:NN`
- 改动：
```diff
- x.reflector().get_jsobject().get()
+ x.reflector().rootable().get()
```
- 依据：<为什么 stub 在这里可接受；可引用同类既有代码的 file:line>

### 方案 B：在调用前 materialize_if_needed
- 适用性：<对象会作为真正的 JSObject 使用；或方案 A 的条件不满足>
- 位置：`components/...:NN` 之前
- 改动：
```diff
+ #[cfg(feature = "js-quickjs")]
+ unsafe { <receiver>.reflector().materialize_if_needed(); }
  <原有 get_jsobject 语句>
```
- 说明：<receiver 不可直接取用时的 hoist 位置>

### 结论
<选 A 还是 B，一句话理由>

## 常见错误

| 错误 | 纠正 |
|------|------|
| 把 `reflector.rs:264` 当作调用点 | 那是 panic 断言位置；调用点在回溯下一层手写帧 |
| 把 `*_Binding::*` / `script_bindings::codegen::*` 当调用点 | 生成代码，改手写调用者 |
| 取错接收者（同一行/同一 block 有多个反射器） | 按创建来源与 materialize 状态判定：解析器/构造器新建且 JS 未访问过的是 stub；已通过 `document.*` 访问过的已 materialize；`None` 分支排除 |
| F 体内没有 `get_jsobject` 就写"调用点未知" | 先查 F 直接调用的手写函数（内联目标），如 `get_compiled_handler` → `get_compiled_event_handler` |
| 把只有 `RemoteDisconnected` / `libEGL` 的 crash 当成本 panic | 没有 `get_jsobject()` panic 行和 Rust 回溯，是另一类崩溃，不属于本 skill |
| 重跑段里同一测试重复分析、重复出报告 | 按测试名去重，按签名归并 |
| 插入点放在 `get_jsobject()` 之后 | 必须之前；materialize 会替换 `object` 并清缓存 |
| 在需要真 `JSObject` 的地方用 `rootable().get()` | 只适用于原始指针语义（refcount / 身份 / realm）；交给 JSAPI 时必须方案 B |
| 直接替换返回 `HandleObject` 的 `get_jsobject()` | `rootable().get()` 返回 `*mut JSObject`，只在原本已 `.get()` 取指针的位置可替换 |
| 忘记 `#[cfg(feature = "js-quickjs")]` 或 `unsafe` | 二者都必须，否则 SM 构建失败/编译错误 |
| 报告只给结论不给代码位置 | 报告的核心是 file:line + 原样代码 + diff 形态的插入点 |
| 同一调用点为每个测试重复写一份报告 | 按签名只写一份 `<key>.md`，测试追加到 `<key>.tests.txt` |
| `>>` 追加前没去重，tests.txt 出现重复行 | 先 `grep -qxF "$sha $html"` 再追加 |
| 把行号写进文件名 | rebase 后行号会移动；文件名用 `<函数>.<文件stem>`，行号写在报告里 |

## 批量分析 WPT crash 日志

`mach test-wpt` 日志的结构：

1. `▶ CRASH [expected OK] /dom/...` 块 + panic 明细（逐测试）。
2. `Ran N tests finished in ...` 之后的 `Tests with unexpected results:` 明细段。
3. `Running N tests in web-platform-tests to detect flaky tests` 重跑段，通常**重复**同样的签名。

处理规则：

- **按测试名去重**：重跑段只用于确认 flaky，不重复分析、不重复出报告。
- 只有 `http.client.RemoteDisconnected` + `libEGL` / `MESA` 警告、没有 Rust 回溯的块，
  是渲染/进程级崩溃，**不属于**本 skill 范围，不要硬套。
- panic 行 `get_jsobject() called ...` 出现在 `RemoteDisconnected` 之后，提取时不要漏。
- 按**签名**出报告：多个测试常归并到同一 `file:line`，测试名追加到 `<key>.tests.txt`，不要重复写报告。

按签名聚合（在日志上跑）：

```sh
awk '/▶ (CRASH|TIMEOUT)/ {t=$0} /get_jsobject\(\) called/ {print t}' <log> | sort | uniq -c
awk '/^  │    7: /' <log> | sed 's/.*│ *//' | sort | uniq -c
```

## 检查清单

- [ ] 从 CRASH 块/进度行确定了 html 路径，并按调用点得到签名 key
- [ ] 同一签名已有报告时，只追加 tests.txt，未重写报告
- [ ] tests.txt 追加前已用 grep 去重，未读入文件全文
- [ ] 新签名已向 INDEX.md 追加一行
- [ ] 回溯里定位到 `get_jsobject` 之后（或被内联时直接取帧 7）的第一层手写帧 F
- [ ] F 体内无 `get_jsobject` 时，已向 F 的被调函数（内联目标）继续查找，而不是直接报未知
- [ ] 多接收者时，已按创建来源 / 是否已被 JS 访问判定哪个是 stub
- [ ] 批量日志已按测试去重、按签名归并；无回溯的渲染崩溃未混入
- [ ] 符号已映射到具体 `file:line`，并核对了函数体
- [ ] 报告写明调用点、接收者、原始代码
- [ ] 报告同时给出方案 A（`rootable().get()`）与方案 B（`materialize_if_needed`），并说明选择依据
- [ ] 方案 A 的适用条件已核对：只用原始指针、不交给 JSAPI、原调用已是 `.get()` 取指针
- [ ] 插入点符合：receiver 可用、在 get_jsobject 之前、`#[cfg(feature = "js-quickjs")]` + `unsafe`
- [ ] 报告包含验证命令（与崩溃构建的 profile 一致）
- [ ] 未修改任何源码（本 skill 只产出报告）
