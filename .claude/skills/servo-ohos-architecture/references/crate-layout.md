# Crate 布局、DOM 对象约定与 codegen 边界

适用问题：某段逻辑该改哪个 crate/文件；哪些是 WebIDL 生成代码不能改；
`#[dom_struct]`/`Dom<T>`/`DomRoot<T>` 的约定；从哪里开始找一个 DOM 符号。

## 1. 相关 crate

| 路径 | 作用 | 什么时候改 |
| --- | --- | --- |
| `components/script/dom/**` | DOM 接口的 Rust 实现（`Document`、`EventTarget`、`HTMLFormElement` …），按规范组织子模块 | 改行为、加 DOM 逻辑 |
| `components/script_bindings/` | 绑定基础设施：`reflector.rs`、`wrap.rs`、`conversions.rs`、`root.rs`、`codegen/`、`proxyhandler.rs`、`refcounted.rs`、`realms.rs` | 改 JS↔Rust 边界、wrap/materialize、rooting |
| `components/script_bindings/webidls/**` | WebIDL 定义（接口成员、属性、`[LegacyUnforgeable]` 等） | 加/改 IDL 接口或成员 |
| `components/script_bindings/codegen/codegen.py` | 从 webidl 生成绑定、`PrototypeList`、`*Binding` trait | 改生成规则（很少） |
| `components/dom_struct/` | `#[dom_struct]` 过程宏 | 改 DOM struct 布局规则（很少） |
| `components/script_webgpu/` | WebGPU 的 DOM 实现（同样用 `#[dom_struct]`） | WebGPU |
| `servo-quickjs/mozjs` | QuickJS 后端的 mozjs API 实现（`engine/` 实现 + `wrap!` 生成的 safe wrapper） | 新增/修复 JSAPI，见 `quickjs-shim-jsapi` skill |
| `servo-quickjs/servo-quickjs` | bindgen 生成的 FFI + QuickJS 源码（`bindings.rs` 勿手改） | 一般不动 |

crate lib name 与目录的对应：回溯里的 `script::...` → `components/script/`，
`script_bindings::...` → `components/script_bindings/`。`js` 是 mozjs shim 的 crate 名。

## 2. 生成代码边界

构建期 `components/script_bindings/build.rs` 调用 `codegen/run.py`，从 `webidls/*.webidl`
生成到 `OUT_DIR`，再由 `codegen/mod.rs` 以模块形式引入：

- `*Binding` / `*Binding::*` 方法（IDL 成员的具体绑定）；
- `PrototypeList`、`DomTypeHolder`、接口注册表、`GetProtoObject` 等；
- `script_bindings::codegen::*` 下的所有内容。

**永远不要手改生成代码**，也不要把它当调用点。要改行为就改：

1. 手写的 DOM 实现（`components/script/`）；
2. 绑定基础设施（`script_bindings/` 的非 `codegen/` 文件）；
3. 生成规则本身（`codegen.py` / `.webidl`），并接受全量重新生成。

## 3. DOM 对象约定

文档：`components/script/dom/mod.rs` 顶部注释（反射器、内存管理、继承）。

- `#[dom_struct] struct Foo { parent: Bar, ... }`：**第一个字段是父类，按值存储**，
  保证 `&Foo` 可安全 `upcast`/`downcast`；继承链最底层的 `EventTarget` 第一个字段是
  `Reflector`。因此 `&Reflector` 的地址 == 整个 DOM struct 的地址
  （`Reflector::get_dom_obj_ptr` 依赖这一点）。
- 注意：lazy binding 下 `Reflector::object` 可能是 `JSDomStub`，不要用 `get_jsobject()`
  的返回值当稳定身份；需要稳定 Node 身份用 `get_dom_obj_ptr()`。
- Rooting：
  - 栈上/返回值：`Root<T>`、`DomRoot<T>`；
  - 结构体字段：`Dom<T>`、`MutDom<T>`、`MutNullableDom<T>`；需要 GC 追踪的容器用
    `HashMapTracedValues`、`DomRefCell` 等；
  - 跨线程/通道：`Trusted<T>`；
  - 字段默认会被 trace；不参与 GC 的字段标 `#[no_trace]`（见各类 struct 定义）。
- 类型转换：`dom::bindings::inheritance::Castable` 的 `is::<T>()` / `downcast::<T>()` /
  `upcast::<T>()`；多类型分支可用生成的 `type_id()` + `match`。
- IDL 实现：在 `script_bindings/webidls/Foo.webidl` 声明，Rust 侧
  `impl FooMethods<crate::DomTypeHolder> for Foo`。

## 4. 常用绑定基础设施文件

| 文件 | 内容 |
| --- | --- |
| `script_bindings/reflector.rs` | `Reflector`、`get_jsobject`、`rootable`、`materialize_if_needed`、materialize cache、trace |
| `script_bindings/wrap.rs` | `wrap` / `wrap_object`：创建 stub 或 proxy |
| `script_bindings/conversions.rs` | `ToJSValConvertible`（含自动 materialize）、`FromJSValConvertible`、`DOM_OBJECT_SLOT` |
| `script_bindings/root.rs` | 栈上 root：`Root`、`DomRoot`、`MaybeUnreflectedDom` |
| `script_bindings/dom.rs` | 字段/智能指针：`MutDom`、`MutNullableDom` |
| `script_bindings/refcounted.rs` | QuickJS 引用计数池（`retain_object`/`release_object` 的调用点） |
| `script_bindings/proxyhandler.rs` | proxy 对象（WindowProxy、跨源对象），proxy 从不是 stub |
| `script_bindings/realms.rs` | realm 进入/切换（`enter_auto_realm`） |
| `script_bindings/callback.rs` | `RootedCallback`、`ThisReflector::jsobject()` 等回调基础设施 |

## 5. 找符号的顺序

1. `codegraph_explore "<符号/问题>"`——本仓库已索引，一次拿源码 + 调用路径；
2. WebIDL：接口名/成员名在 `script_bindings/webidls/*.webidl`；
3. `rg -n "fn <name>" components/` 兜底；trait 方法在具体 impl 里确认接收者类型；
4. 回溯定位时跳过 `codegen`、`std/core`、纯 C 帧（`JS_*`/`js_*`）。

## 6. 交叉引用

- lazy binding / materialize：`references/lazy-binding-materialize.md`
- 内联事件处理器：`references/dom-event-handlers.md`
- form owner：`references/dom-form-owner.md`
- QuickJS shim 加 API：`.claude/skills/quickjs-shim-jsapi/SKILL.md`
- 从崩溃反查 `get_jsobject` 调用点：`.claude/skills/fix-get-jsobject/SKILL.md`
