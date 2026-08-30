# `window.<attr>` 属性访问实现分析

> 目标：梳理 `components/script/dom/window` 模块下 `window.<attr>`（JS 属性访问）的完整调用链，
> 记录相关函数与代码行号，并设计基于 `servo_tracing::instrument` 的插桩方案，用于分析
> `tests/html/window-named-properties.html` 中 `window.reports` 涉及的 API 调用。
> 该测试页 `<div id="reports">` 是 window 的 **named property**（命名属性），走的是
> HTML 规范 "Named access on the Window object"。

## 1. 总体调用链

`window.reports` 的访问在 JavaScript 引擎（SpiderMonkey）侧触发 **WindowProxy** 的代理 trap，
随后落到 Rust DOM 层的 named property getter。全链路如下：

```
JS: window.reports
 │
 ├─① WindowProxy `get` trap
 │   - 先查子浏览上下文命名属性 GetSubframeWindowProxy (index / iframe name) [windowproxy.rs:1169]
 │   - 未命中则 JS_ForwardGetPropertyTo 转发到 target(全局 Window 对象)   [windowproxy.rs:1308]
 │
 ├─② 原型链查找（target 无自有属性 "reports"）
 │    Window 对象 → Window.prototype(接口原型, 存放 IDL 方法/属性)
 │              → WindowProperties(named properties 代理对象)
 │              → EventTarget.prototype → ... → Object.prototype
 │   （原型链装配见 §5）
 │
 ├─③ WindowProperties 代理 `getOwnPropertyDescriptor` trap  [window_named_properties.rs:81]
 │    - Symbol.toStringTag 直接返回 "WindowProperties"        [window_named_properties.rs:91-105]
 │    - 原型链上已有该属性? has_property_on_prototype         [window_named_properties.rs:108-119]
 │    - jsid 转字符串 "reports"                               [window_named_properties.rs:121-133]
 │    - 否则作为 named property 调用 window.NamedGetter       [window_named_properties.rs:140]
 │
 ├─④ Window::NamedGetter(cx, "reports")                       [window.rs:2302]
 │    - document.iframes() 按浏览上下文 name 匹配, 返回 WindowProxy  [window.rs:2309-2345]
 │    - get_elements_with_name / get_elements_with_id          [document.rs:4785 / 4777]
 │      (id_map / name_map 查询，命中 <div id="reports">)
 │    - 单元素 → NamedPropertyValue::Element
 │    - 多元素 → NamedPropertyValue::HTMLCollection
 │    - 无匹配 → undefined
 │
 └─⑤ 结果 safe_to_jsval 回填 property descriptor → JS 可见
```

`window.reports` 场景落在 **④ 的 id 分支**：`is_named_element_with_id_attribute`
（HTML/SVG 元素 + id 匹配，[window.rs:4214]）命中 `<div id="reports">`，单元素 →
`Element` 作为属性值。

## 2. 函数与行号清单

### 2.1 WindowProxy 代理（入口，高频触发）

| 文件 | 行号 | 函数 | 作用 |
|---|---|---|---|
| [windowproxy.rs](components/script/dom/window/windowproxy.rs#L1169) | 1169 | `GetSubframeWindowProxy` | 数字 index / iframe name 命中的子浏览上下文 WindowProxy；被下列三个 trap 共用 |
| [windowproxy.rs](components/script/dom/window/windowproxy.rs#L1223) | 1223 | `get_own_property_descriptor` | WindowProxy `getOwnPropertyDescriptor` trap（非标 named 属性 / 普通属性透传 target） |
| [windowproxy.rs](components/script/dom/window/windowproxy.rs#L1278) | 1278 | `has` | `in` 判断 trap，转发 `JS_HasPropertyById` 到 target |
| [windowproxy.rs](components/script/dom/window/windowproxy.rs#L1308) | 1308 | `get` | **属性 get trap 主入口**，`GetSubframeWindowProxy` 未命中则 `JS_ForwardGetPropertyTo` 转发原型链 |
| [windowproxy.rs](components/script/dom/window/windowproxy.rs#L1342) | 1342 | `set` | set trap，数字属性只读拒绝 |
| [windowproxy.rs](components/script/dom/window/windowproxy.rs#L1406) | 1406 | `PROXY_TRAPS` | 同源代理 trap 表，装配到 WindowProxyHandler |
| [windowproxy.rs](components/script/dom/window/windowproxy.rs#L1492) | 1492 | `new_window_proxy` | `NewWindowProxy` 创建 JS 代理对象 |

### 2.2 WindowProperties 命名属性对象（named property 判定核心）

| 文件 | 行号 | 函数 | 作用 |
|---|---|---|---|
| [window_named_properties.rs](components/script/window_named_properties.rs#L81) | 81 | `get_own_property_descriptor` | **named property 解析入口**：原型链已有则跳过，否则调 `window.NamedGetter` |
| [window_named_properties.rs](components/script/window_named_properties.rs#L154) | 154 | `own_property_keys` | 枚举（仅返回 `Symbol.toStringTag`，TODO 待补, 参考 gecko） |
| [window_named_properties.rs](components/script/window_named_properties.rs#L170) | 170 | `define_property` | named property 禁止 define，抛 `JSMSG_CANT_DEFINE_WINDOW_NAMED_PROPERTY` |
| [window_named_properties.rs](components/script/window_named_properties.rs#L184) | 184 | `delete` | named property 禁止 delete |
| [window_named_properties.rs](components/script/window_named_properties.rs#L254) | 254 | `create` | `NewProxyObject` 创建 WindowProperties 代理对象，原型不可变 |

### 2.3 Window 命名 getter（真正的 named property 逻辑）

| 文件 | 行号 | 函数 | 作用 |
|---|---|---|---|
| [window.rs](components/script/dom/window/window.rs#L2302) | 2302 | `NamedGetter` | **核心**：iframe name → id/name 元素 → Element/HTMLCollection，见 §1 ④ |
| [window.rs](components/script/dom/window/window.rs#L2392) | 2392 | `SupportedPropertyNames` | 支持属性名枚举（转发 Document） |
| [window.rs](components/script/dom/window/window.rs#L2421) | 2421 | `create_named_properties_object` | 构造函数路径调用 `window_named_properties::create` |
| [window.rs](components/script/dom/window/window.rs#L4228) | 4228 | `WindowHelpers` impl `create_named_properties_object` | 被 WebIDL codegen 引用（见 §5） |
| [window.rs](components/script/dom/window/window.rs#L4200) | 4200 | `is_named_element_with_name_attribute` | name 属性匹配白名单（embed/form/img/object） |
| [window.rs](components/script/dom/window/window.rs#L4214) | 4214 | `is_named_element_with_id_attribute` | id 匹配白名单（HTML/SVG 元素） |

### 2.4 Document 元素查找（window getter 的下游）

| 文件 | 行号 | 函数 | 作用 |
|---|---|---|---|
| [document.rs](components/script/dom/document/document.rs#L4777) | 4777 | `get_elements_with_id` | `id_map` 查询，`window.reports` 在此命中 |
| [document.rs](components/script/dom/document/document.rs#L4785) | 4785 | `get_elements_with_name` | `name_map` 查询 |
| [document.rs](components/script/dom/document/document.rs#L6388) | 6388 | `Document::NamedGetter` | Document 自身的 named getter（同一套 `id_map`/`name_map` 基础设施） |

### 2.5 原型链装配 / WebIDL 接线（参考，不需插桩）

| 文件 | 行号 | 作用 |
|---|---|---|
| [Window.webidl](components/script_bindings/webidls/Window.webidl#L47) | 47 | `getter NamedPropertyValue (DOMString name);` 声明 named getter |
| [Document.webidl](components/script_bindings/webidls/Document.webidl#L240) | 240 | `typedef (WindowProxy or Element or HTMLCollection) NamedPropertyValue;` |
| [constructor.rs](components/script_bindings/constructor.rs#L171) | 171-200 | `create_interface`：`has_named_properties_object` 时把 WindowProperties 插入接口原型与父原型之间 |
| [interfaces.rs](components/script_bindings/interfaces.rs#L118) | 118 | `WindowHelpers::create_named_properties_object` trait |
| [codegen.py](components/script_bindings/codegen/codegen.py#L3862) | 3862/3894 | codegen 为 `Window` 填充 `has_named_properties_object`（指向 `create_named_properties_object`） |
| [Bindings.conf](components/script_bindings/codegen/Bindings.conf#L1370) | 1370 | Window 的 `additionalTraits: WindowHelpers, HasOrigin` |
| [utils.rs](components/script_bindings/utils.rs#L335) | 335 | `has_property_on_prototype`（WindowsProperties trap 判原型链） |

### 2.6 绑定/初始化层（对象构造路径）

`window.<attr>` 可用依赖 JS 侧的 **WindowProperties 命名属性对象** 与 **WindowProxy**。
它们的装配发生在 Window 全局对象构造时，属于 bug 排查时的上游参考。

初始化链路：

```
Window 全局对象创建
 │
 ├─① codegen 生成 WindowBinding::CreateInterfaceObjects  [codegen.py:3780, 4060]
 │    装配 InterfaceInit { init_type: Parent(EventTarget), has_named_properties_object: Some(D::Window::create_named_properties_object), ... }
 │
 ├─② create_interface()                                  [constructor.rs:171]
 │    - has_named_properties_object 命中时, 调用 Window::create_named_properties_object
 │      把 WindowProperties 代理插到 接口原型 与 EventTarget.prototype 之间  [constructor.rs:192-200]
 │    - 随后 create_interface_prototype_object()      [interface.rs:252] 创建接口原型
 │
 ├─③ Window::create_named_properties_object               [window.rs:4228 / 2421]
 │    └─ window_named_properties::create()        [window_named_properties.rs:254] NewProxyObject 创建代理
 │
 └─④ Window::new() 构造全局                                       [window.rs:3884]
      └─ WindowProxy::new() → new_window_proxy() 创建 JS 代理对象   [windowproxy.rs:184 / 1492]
         SetWindowProxy 把代理注册到全局  [windowproxy.rs:233]
```

| 文件 | 行号 | 函数 | 作用 |
|---|---|---|---|
| [codegen.py](components/script_bindings/codegen/codegen.py#L3780) | 3780/4060 | `CreateInterfaceObjects`（生成代码） | 生成 Window 的接口对象初始化入口，含 named props 装配 |
| [constructor.rs](components/script_bindings/constructor.rs#L171) | 171 | `create_interface` | 通用接口初始化：named props 对象插入原型链（192-200） |
| [interface.rs](components/script_bindings/interface.rs#L252) | 252 | `create_interface_prototype_object` | 通用接口原型对象创建 |
| [window.rs](components/script/dom/window/window.rs#L4228) | 4228 | `WindowHelpers::create_named_properties_object` | WebIDL 回调，转调 `window_named_properties::create` |
| [window_named_properties.rs](components/script/window_named_properties.rs#L254) | 254 | `create` | 创建 WindowProperties 代理对象（HANDLER + CLASS） |
| [window.rs](components/script/dom/window/window.rs#L3884) | 3884 | `Window::new` | Window 全局对象构造 |
| [windowproxy.rs](components/script/dom/window/windowproxy.rs#L184) | 184/1492 | `WindowProxy::new` / `new_window_proxy` | 创建 JS WindowProxy 代理对象 |

## 3. 插桩设计（等待确认后再改代码）

参照 [layout/flow/root.rs:172](components/layout/flow/root.rs#L172) 的既有插桩方式：
`#[servo_tracing::instrument(...)]`。该宏展开为
`#[cfg_attr(feature = "tracing", tracing::instrument(name=.., fields(servo_profiling=true), level="trace"))]`，
feature 关闭时零开销。

### 3.1 建议插桩点（按优先级）

**P0 — named property 解析核心（必插）**

| 位置 | 建议 instrument 写法 |
|---|---|
| [window.rs:2302](components/script/dom/window/window.rs#L2302) `Window::NamedGetter` | `name = "Window::NamedGetter"`, `fields(prop = %name)`, `skip_all` |
| [window_named_properties.rs:81](components/script/window_named_properties.rs#L81) `get_own_property_descriptor` | `name = "WindowProperties::getOwnPropertyDescriptor"`, `skip_all` |
| [document.rs:4777](components/script/dom/document/document.rs#L4777) `get_elements_with_id` | `name = "Document::get_elements_with_id"`, `fields(id = %id)`, `skip_all` |
| [document.rs:4785](components/script/dom/document/document.rs#L4785) `get_elements_with_name` | `name = "Document::get_elements_with_name"`, `fields(name = %name)`, `skip_all` |

**P1 — WindowProxy 入口（补全链路，trace 级）**

| 位置 | 建议 instrument 写法 |
|---|---|
| [windowproxy.rs:1308](components/script/dom/window/windowproxy.rs#L1308) `get` trap | `name = "WindowProxy::get"`, `skip_all` |
| [windowproxy.rs:1223](components/script/dom/window/windowproxy.rs#L1223) `get_own_property_descriptor` trap | `name = "WindowProxy::getOwnPropertyDescriptor"`, `skip_all` |
| [windowproxy.rs:1278](components/script/dom/window/windowproxy.rs#L1278) `has` trap | `name = "WindowProxy::has"`, `skip_all` |
| [windowproxy.rs:1169](components/script/dom/window/windowproxy.rs#L1169) `GetSubframeWindowProxy` | `name = "GetSubframeWindowProxy"`, `skip_all` |

**P-Init — 绑定/初始化层（观察对象装配，构造期一次）**

| 位置 | 建议 instrument 写法 |
|---|---|
| [window_named_properties.rs:254](components/script/window_named_properties.rs#L254) `create` | `name = "WindowProperties::create"`, `skip_all` |
| [window.rs:4228](components/script/dom/window/window.rs#L4228) `WindowHelpers::create_named_properties_object` | `name = "Window::create_named_properties_object"`, `skip_all` |
| [constructor.rs:171](components/script_bindings/constructor.rs#L171) `create_interface` | `name = "create_interface"`, `fields(proto = ?something)`, `skip_all`（通用，所有接口共用） |
| [interface.rs:252](components/script_bindings/interface.rs#L252) `create_interface_prototype_object` | `name = "create_interface_prototype_object"`, `skip_all`（通用，所有接口共用） |
| [window.rs:3884](components/script/dom/window/window.rs#L3884) `Window::new` | `name = "Window::new"`, `skip_all` |
| [windowproxy.rs:184](components/script/dom/window/windowproxy.rs#L184) `WindowProxy::new` | `name = "WindowProxy::new"`, `skip_all` |
| [windowproxy.rs:1492](components/script/dom/window/windowproxy.rs#L1492) `new_window_proxy` | `name = "WindowProxy::new_window_proxy"`, `skip_all` |

> 说明：`create_interface` / `create_interface_prototype_object` 是所有接口共用的通用函数，
> 插桩后每个接口初始化都会出 span（默认 `level="trace"`）。只想看 Window 相关的，
> 过滤 `SERVO_TRACING='script_bindings=trace'` 再叠加 span 名即可。
> `CreateInterfaceObjects`（codegen 生成）不建议改 codegen.py，用上面的手写函数足够定位。

**P2 — 补充（按需）**
- [window.rs:2392](components/script/dom/window/window.rs#L2392) `SupportedPropertyNames`
- [document.rs:6388](components/script/dom/document/document.rs#L6388) `Document::NamedGetter`
- [window_named_properties.rs:154](components/script/window_named_properties.rs#L154) `own_property_keys`

### 3.2 示例写法

```rust
// 参照 components/layout/flow/root.rs:172
#[servo_tracing::instrument(name = "Fragment Tree Construction", skip_all)]
pub(crate) fn layout(&self, layout_context: &LayoutContext, ...) -> FragmentTree { ... }
```

本模块的对应写法：

```rust
// script/dom/window/window.rs
#[servo_tracing::instrument(name = "Window::NamedGetter", level = "trace", skip_all, fields(prop = %name))]
fn NamedGetter(&self, cx: &mut JSContext, name: DOMString) -> Option<NamedPropertyValue> {
    // ...
}
```

```rust
// script/window_named_properties.rs —— unsafe extern "C" 钩子也能加，
// 参数(raw 指针/HandleId) 不可 Debug，必须 skip_all
#[servo_tracing::instrument(name = "WindowProperties::getOwnPropertyDescriptor", level = "trace", skip_all)]
unsafe extern "C" fn get_own_property_descriptor(cx: *mut JSContext, proxy: HandleObject, id: HandleId, ...) -> bool {
    // ...
}
```

> 注意：`unsafe extern "C"` SM 钩子是否可用 `servo_tracing::instrument` 需先用
> `cargo check` 验证（宏对 `unsafe extern "C" fn` 的解析/展开），若不支持则退回
> P0 全部落在普通 Rust 函数上，效果一致。

### 3.3 启用与查看输出

1. 以 `tracing` feature 构建 servoshell（现在默认不带，见
   [servoshell/Cargo.toml:68](ports/servoshell/Cargo.toml#L68)）：
   ```sh
   ./mach build --release --features tracing
   ```
2. 运行测试页并过滤 span（`SERVO_TRACING` 或 `--tracing-filter`，见
   [servoshell/lib.rs:50](ports/servoshell/lib.rs#L50) 与 [prefs.rs:538](ports/servoshell/prefs.rs#L538)）：
   ```sh
   SERVO_TRACING='script=trace' ./mach run -b -r tests/html/window-named-properties.html
   # 或
   ./mach run -b -r --tracing-filter 'script=trace' tests/html/window-named-properties.html
   ```
3. 预期看到嵌套 span 树（trace 事件会输出到 stderr）：
   ```
   WindowProxy::get          # script::dom::window::windowproxy
   ├ WindowProperties::getOwnPropertyDescriptor   # script::window_named_properties
   │  ├ Window::NamedGetter                        # script::dom::window
   │  │  └ Document::get_elements_with_id          # script::dom::document
   ```

### 3.4 注意事项

- **高频触发**：window 上的每次属性读写都会走代理 trap，故全链路统一用 `level="trace"`，
  避免低级别日志刷屏；`SERVO_TRACING` 可按 crate 精确过滤（如只开 `script=debug`）。
- **`skip_all` 必要性**：代理 trap 入参为 `*mut JSContext`/`HandleId`/`RawHandleObject`
  等原始指针类型，不实现 `Debug`，`instrument` 默认抓参会编译失败。
- **`fields(prop = %name)` 只对 `String`/`DOMString` 入参可用**；jsid 无法在宏内字符串化，
  因此 P1 层只记录 span 名。若需在代理 trap 里看到具体属性名，需另加
  `tracing::event!`（须 `use tracing` 并受 `feature = "tracing"` 约束），本轮暂不采用。
- **feature 门控**：`servo_tracing::instrument` 已自动 `cfg_attr(feature = "tracing")`；
  script crate 的 `tracing` feature 定义见 [script/Cargo.toml:29](components/script/Cargo.toml#L29)。