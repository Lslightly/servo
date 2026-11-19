# JSDomStub only realm and dom_ptr

JSDomStub只保留realm和dom_ptr, Reflector中添加`DOMJSClass`指针。

## 1. 目标

未 materialize 的 DOM 节点只背一个最小的 `JSDomStub`，其余 materialize 所需数据都从
`dom_ptr`（指向 Rust `Reflector`）和每接口共享的 `DOMJSClass` 取得。

改造前 stub 上曾存/曾被考虑的字段：

| 名称: 类型 | 大小 | 改造后来源 |
| ---------- |  --- | --- |
| `given_proto: *mut JSObject` | 8 | 删除，始终用 canonical proto |
| `proto_object_fn: fn` | 8 | `DOMJSClass.dom_class.proto_object_fn` |
| `has_legacy_unforgeable_members: bool` | 1 | `DOMJSClass.dom_class.has_legacy_unforgeable_members` |
| `class: *const JSClass` | 8 | `DOMJSClass.base` |
| `scope: *mut JSObject` | 8 | stub 的 `realm` → `RealmData.global` |
| `raw_cx: *mut JSContext` | 8 | stub 的 `realm` → `context_of_realm` |
| `class_id: uint16_t` | 2 (+6 padding) | `Reflector.dom_js_class`（见下） |

`given_proto`在绑定`JSObject`时直接不使用，当做null，用`canonical_proto`即可。

## 2. 结论：`realm` 暂时保留

原计划想把它也删掉，从 `dom_ptr` 取到 `Reflector` 后直接拿 `global`。但做不到：

- `Reflector.object` 指向的**就是**这个 `JSDomStub` 本身，realm 只存在 stub 上；
- `dom_ptr` 只给出 `T: DomObject`，它在没有 JS 对象的情况下没有任何取 realm 的接口；
- 从 `Reflector` 再回到 stub 只是循环，不是新的来源。

因此 `JSDomStub` 保留 `realm`，`scope` / `raw_cx` 在 materialize 时由它取得。

## 3. 关键设计：class 不再存在 stub 上

C 侧只有三处依赖 stub 的 `class_id`：

1. `JS_GetClassID` 的 stub 分支；
2. `free_dom_stub` 取 class finalizer；
3. GC mark 的 `JS_GC_OBJ_TYPE_DOM_STUB` 分支取 `gc_mark`。

去掉 `class_id` 后**不引入共享 class**：stub 仍然没有自己的 class，这三处都通过
`dom_ptr` 反查接口 class 得到 QuickJS 侧注册的 class id：

```text
dom_ptr (= &Reflector)                    shim 侧 ReflectorHeader
  └─ field 0: dom_js_class: Cell<*const DOMJSClass>
       ├─ base       → JSClass（DOMJSClass 首字段，指针相同）
       └─ dom_class  → proto_object_fn / has_legacy_unforgeable_members
                            │
        class_id(rt, clasp) ┘ → rt->class_array[id] 的 finalizer / gc_mark / call / exotic
```

### 3.1 `Reflector.dom_js_class` 必须放在第一个字段

shim（`servo-quickjs/mozjs/src/engine/class.rs`）只有裸 `dom_ptr`，没有 `T` 的泛型信息；
`Reflector<T>` 的字段偏移随 `T` 变化，所以只能约定：**`dom_js_class` 是 `Reflector` 的第一个字段**。

shim 用等价的 `#[repr(C)] struct ReflectorHeader { dom_js_class: Cell<*const root::JSClass> }`
（必须是 `Cell`，与 `Reflector` 保持一致），直接把它当 `Reflector` 的首部读。
`DOMJSClass::base` 是 `JSClass` 且在首位，所以同一个指针也指向 `JSClass`。

`components/script_bindings/reflector.rs` 用
`offset_of!(Reflector<()>, dom_js_class) == 0` 的编译期断言把这个约定钉住。

### 3.2 C 侧：weak 符号回调 shim

`quickjs.c` 属于 `servo-quickjs` crate，不能直接链接 mozjs（standalone 构建也要能链接），
所以用一个 **weak 符号**把"从 `dom_ptr` 取 class id"这件事交给 shim：

```c
/* quickjs.c：shim（mozjs/src/engine/class.rs）实现；standalone 链接时为 NULL */
extern JSClassID servo_dom_stub_class_id(JSRuntime *rt, void *dom_ptr)
    __attribute__((weak));

static JSClassID dom_stub_class_id(JSRuntime *rt, const JSDomStub *p)
{
    if (!p->dom_ptr || !servo_dom_stub_class_id)
        return JS_INVALID_CLASS_ID;
    return servo_dom_stub_class_id(rt, p->dom_ptr);
}
```

- `struct JSDomStub { header; void *dom_ptr; void *realm; }`：只有这两个字段；
- `JS_NewDomStub(JSContext *ctx, void *dom_ptr, void *realm)`：不再有 `class_id` / `given_proto`；
  `JS_GetDomStubGivenProto` 删除；
- `JS_GetClassID(stub)` → `dom_stub_class_id(NULL, p)`（这个 API 没有 runtime，传 NULL）；
- `free_dom_stub` → `dom_stub_class_id(rt, p)` 取 finalizer；
- GC mark 的 `JS_GC_OBJ_TYPE_DOM_STUB` 分支 → `dom_stub_class_id(rt, p)` 取 `gc_mark`。

三处都必须先 `JS_IsRegisteredClass(rt, id)` 再索引 `rt->class_array[id]`（unbound / standalone
时 id 是 `JS_INVALID_CLASS_ID`）。

### 3.3 Rust shim

- `#[no_mangle] pub unsafe extern "C" fn servo_dom_stub_class_id(rt, dom_ptr)`：
  `stub_class_from_ptr(dom_ptr)` 读 `ReflectorHeader.dom_js_class`，为 null 返回
  `JS_INVALID_CLASS_ID`；`rt` 为 NULL 时用 `super::try_current()` 的 `cx.rt` 兜底；
  最后调 `class_id(rt, clasp)`（`engine/class.rs` 已有的注册表）；
- `JS_NewDomStub(cx, clasp, dom_ptr)` 显式收接口 class，先 `class_id(cx.rt, clasp)` 注册，
  再 `q::JS_NewDomStub(cx.qctx(), dom_ptr, realm)`。class 由 `wrap` 传入（见 §4.4），
  shim 不在这里回读 `Reflector`；
- `object_class` / `class_name` 的 stub 分支走 `stub_class(val)`；
- `JS_GetDomStubInfo` 只输出 `global` + `raw_cx`（class 从 `Reflector` 取）。

## 4. 实施步骤

1. **C 侧**：`quickjs.h` / `quickjs.c` 按 §3.2 改；重新生成 `src/bindings.rs`
   （如工具链不可用则同步手工修改，签名与头文件逐字一致）。
2. **shim engine**：`engine/class.rs` 按 §3.3 改（`ReflectorHeader`、`servo_dom_stub_class_id`、
   `JS_NewDomStub` 收 `clasp`）。
3. **shim 声明**：`upstream-bindgen/jsapi.rs` 里的 Servo 扩展区同步改签名 ——
   `JS_NewDomStub(cx, clasp, dom_ptr)`（3 参数，去掉 `given_proto`）、
   `JS_GetDomStubInfo(stub, global, cx)`（3 参数，去掉 `clasp` 输出）。
   这两份声明的**参数个数必须与 `engine/class.rs` 的实现一致**，否则 `stub_bindings.py` 生成的
   转发会退化成 `qjs_todo!`，或 `wrap!` 展开后报 E0061。签名一致后
   `jsapi2_wrappers.in.rs` 只需 `wrap!(jsapi: ...)`。
4. **Reflector**：加 `dom_js_class: Cell<*const DOMJSClass>` 字段（第一位）、存取器、offset 断言；
   `new()` 初始化为 null。
5. **wrap**：非 proxy 分支先 `set_dom_js_class(class)`，再
   `JS_NewDomStub(cx, class, dom_ptr as *mut c_void)`。
6. **materialize**：class / dom_class 从 `self.dom_js_class()` 取，`scope` / `raw_cx` 从 stub
   realm 取，proto 恒为 canonical proto；`JS_UnbindDomStub` 仍在新对象创建之后。
7. 更新 `docs/lazy-binding-with-gc-in-rust.md` 与实际实现不一致的描述（如 `given_proto`）。

## 5. 验证

```sh
cd servo-quickjs && CARGO_TARGET_DIR="$PWD/target" cargo check -p mozjs
./mach check --js-engine quickjs
./mach fmt
./mach build --js-engine quickjs
./tests/gc/lazy-binding/run.sh --binary target/debug/servoshell
```

重点确认：

- 未 materialize 节点被 GC：`free_dom_stub` → `servo_dom_stub_class_id(rt, dom_ptr)` →
  接口 class id → 接口 `cOps.finalize`，realm 正确释放；
- GC mark 分支同理取到接口的 `gc_mark`，materialize 前的 trace 不漏；
- materialize：新对象创建后再 `JS_UnbindDomStub`（此时 `dom_ptr` 为 NULL，
  `servo_dom_stub_class_id` 返回 `JS_INVALID_CLASS_ID`，三处都跳过）；
- `JS_GetClassID(stub)` 返回接口 class id（rt 传 NULL，走 `try_current()`）；
- proxy / global 行为不变（从不创建 stub）。

## 6. 风险

- **布局约定**：`dom_js_class` 一旦不在首位，shim 会读错；靠编译期 `offset_of` 断言兜底，
  且 `ReflectorHeader` 必须是 `Cell<*const JSClass>`（与 `Reflector` 逐字一致）。
- **未 materialize 的 stub 查询 class**：`dom_ptr` 为空（已 unbound）时
  `servo_dom_stub_class_id` 返回 `JS_INVALID_CLASS_ID`，`object_class` 返回占位 class，
  不 panic；C 侧三处都用 `JS_IsRegisteredClass` 守卫，不会索引 `class_array[0]`。
- **standalone 链接**：没有 shim 时 weak 符号为 NULL，`dom_stub_class_id` 返回
  `JS_INVALID_CLASS_ID`，stub 没有 class（servo-quickjs 自己的测试不涉及 DOM）。
- **given_proto 的行为变化**：QuickJS 构建下 `reflect_dom_object_with_proto` 不再使用调用方
  指定的 prototype，一律用 canonical proto。这是本优化的既定取舍。
