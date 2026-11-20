# materialize 改造实施计划

> 目标分支：`lazy-binding-rebase-1112`
> 设计依据：`docs/agent/draft-lazy-binding.md`（下文简称"草稿"）
> 行号只是本文写作时的快照；实施时按符号名定位。
>
> **后续变更（已实施）**：`JSDomStub` 不再保存 `given_proto`。带 given proto 的构造
> （子类构造器、`Reflect.construct` 的 `newTarget.prototype`）在 `wrap()` 时直接创建真
> `JSObject`，从不进入 stub/materialize 路径。`JS_NewDomStub` 现在是
> `(cx, clasp, dom_ptr)`，`JS_GetDomStubGivenProto` 已删除；下文涉及 given_proto 的描述
> 是当时的设计快照。

---

## 1. 背景

当前 QuickJS 懒绑定下，一个还没交给 JS 的 DOM 节点由三部分组成：

```
Rust DOM struct
  └─ Reflector {
       object: JSDomStub ─────────────┐
       materialize_cache: Some(...)   │ 24B：given_proto + proto_object_fn
     }                                │
                                      ▼
                          JSDomStub（QuickJS 侧，40B）
                            └─ opaque ──> ObjectData（32B）+ slot 数组（≥16B）
                                            clasp / realm / slots
```

materialize 时，`Reflector::materialize_if_needed()` 从 `materialize_cache` 取
`given_proto` / `proto_object_fn` / `has_legacy_unforgeable_members`，再从 stub 的
`ObjectData` 取 `clasp` / `realm` / global / shim context，创建真正的 `JSObject`。

这带来三个问题：

1. **每个未访问节点都背着 `ObjectData` + slot 数组**（至少 48B），只为了存
   `clasp` 和 `realm`；`DOM_OBJECT_SLOT` 也要在 stub 上占一个 slot。
2. **`MaterializeCache` 是每节点数据，但里面两项是每接口数据**：
   `proto_object_fn` 和 `has_legacy_unforgeable_members` 对所有同接口对象都一样，
   每节点存一份纯属浪费（24B/节点）。
3. materialize 的输入分散在 cache 和 `ObjectData` 两处，任何一处改动都要同时改两条路径。

改造后：

| 数据 | 现在 | 改造后 |
| --- | --- | --- |
| stub 携带的每节点数据 | `ObjectData`(32B) + slots(≥16B) | `dom_ptr`(8B) + `realm`(8B) + `given_proto`(8B) + `class_id`(2B) |
| `Reflector` 每节点状态 | `MaterializeCache`(24B) | 无 |
| 每接口数据 | 每节点 cache 一份 | `DOMClass` 每接口一份 |

---

## 2. 目标结构

### 2.1 JSDomStub

```c
typedef struct JSDomStub {
    JSGCObjectHeader header;   // 16B，必须第一个
    uint16_t class_id;         // 2B + 6B padding
    void *dom_ptr;             // 8B：DOM 节点（Reflector 地址），unbind 后 NULL
    void *realm;               // 8B：RealmData*，stub 持有的一份引用，unbind 后 NULL
    void *given_proto;         // 8B：wrap 时调用方指定的 prototype（已 outerize），
                               //      没有则为 NULL；unbind 后 NULL
} JSDomStub;                   // payload 48B，加 8B JSMallocBlockHeader = 56B
```

不变量：

- `dom_ptr != NULL` ⇔ stub 拥有 DOM 节点（负责 finalize）**且**持有一份 realm 引用；
- `JS_UnbindDomStub` 之后 `dom_ptr == NULL && realm == NULL && given_proto == NULL`，
  stub 只是池里等待释放的空壳；
- `given_proto` 是每节点数据，无法从 `class_id` / `realm` 推导，必须存到 materialize；
  它本身已经是 `JSObject`，不需要再 materialize；
- `class_id` 在整个生命周期内不变，unbound 后仍可读；
- stub 从不暴露给 JS；所有交给 JS 的路径都先 materialize。

### 2.2 DOMClass

```rust
pub struct DOMClass {
    /* 现有字段：interface_chain / depth / type_id / malloc_size_of / global */

    /// 该接口的 canonical prototype 创建函数（codegen 生成的 `GetProtoObject::<D>`）。
    pub proto_object_fn: ProtoObjectFn,
    /// 该接口是否有 `[LegacyUnforgeable]` 成员。
    pub has_legacy_unforgeable_members: bool,
}
```

这两个字段是每接口数据，materialize 时通过 `get_dom_class(stub)` 取得，不再进每节点结构。

### 2.3 materialize 的输入

| 输入 | 改造后来源 |
| --- | --- |
| `dom_ptr` | `JS_GetDomStubDomPtr(stub)` |
| `given_proto` | `JS_GetDomStubGivenProto(stub)`（NULL 表示用 canonical prototype） |
| `class` | `clasp_of_id(JS_GetClassID(stub))`（经 stub 的 `class_id`） |
| `realm` / global / raw cx | `JS_GetDomStubRealm(stub)` → `RealmData` |
| `proto_object_fn` | `get_dom_class(stub).proto_object_fn` |
| `has_legacy_unforgeable_members` | `get_dom_class(stub).has_legacy_unforgeable_members` |

---

## 3. 实施步骤

两个阶段各自都能编译、运行和单独提交。**建议先落阶段 1，跑完整测试后再做阶段 2**，
这样出问题时能明确是哪一层。

### 阶段 0：基线

在改动前记录：

```sh
./mach build --js-engine quickjs --config 'patch.crates-io.mozjs.path="servo-quickjs/mozjs"'
./tests/gc/lazy-binding/run.sh --binary target/debug/servoshell
```

保存一份 `heaptrack` 单节点数据（至少 `document.createElement("div")` 后不交给 JS 的场景），
用于改造后对比。"未 materialize 单节点"的构成按 §2.1 计算，实测以 heaptrack 为准。

---

### 阶段 1：stub 不再携带 `ObjectData`

这一阶段保留 `MaterializeCache`，只把 stub 的 payload 换成 `dom_ptr` / `realm`，
并让 shim 里所有依赖 `object_data(stub)` 的路径能直接读 stub 字段。

#### 1.1 `servo-quickjs/servo-quickjs/quickjs/quickjs.h`

更新/新增声明，签名与 `quickjs.c` 逐字一致：

```c
/* Servo: allocate a new JSDomStub. It owns `dom_ptr` and one reference to `realm`;
   `given_proto` may be NULL and is not retained (see the GC note below). */
JSValue JS_NewDomStub(JSContext *ctx, uint16_t class_id, void *dom_ptr, void *realm,
                      void *given_proto);
/* Servo: detach the DOM node, realm and given prototype from a stub without freeing the stub. */
void JS_UnbindDomStub(JSContext *ctx, JSValue obj);
BOOL JS_IsDomStub(JSValueConst v);
/* Servo: read the fields of a JSDomStub (NULL for a non-stub). */
void *JS_GetDomStubDomPtr(JSValueConst v);
void *JS_GetDomStubRealm(JSValueConst v);
void *JS_GetDomStubGivenProto(JSValueConst v);
```

#### 1.2 `quickjs.c`

| 位置 | 改动 |
| --- | --- |
| `struct JSDomStub` | `void *opaque` → `void *dom_ptr; void *realm; void *given_proto;`（`class_id` 保留） |
| `JS_NewDomStub` | 参数改为 `(ctx, class_id, dom_ptr, realm, given_proto)`，初始化四个字段 |
| `JS_UnbindDomStub` | `p->dom_ptr = NULL; p->realm = NULL; p->given_proto = NULL;` |
| `free_dom_stub` | 用 `p->dom_ptr`（而不是 `p->opaque`）判断是否调用 class finalizer |
| 新增 | `JS_GetDomStubDomPtr` / `JS_GetDomStubRealm` / `JS_GetDomStubGivenProto`：先 `JS_IsDomStub`，否则返回 `NULL` |
| `JS_GetClassID` | 不变 |
| GC mark dispatch（`JS_GC_OBJ_TYPE_DOM_STUB`） | 不变，仍按 `class_array[class_id].gc_mark` 派发 |
| `JS_GetOpaque` | stub 分支改为返回 `NULL`（已经没有 opaque） |

`sizeofJSDomStub` 仍用 `sizeof(JSDomStub)`，`js_trigger_gc` 自动跟随新尺寸。

#### 1.3 重新生成 QuickJS 绑定

```sh
cd servo-quickjs
LIBCLANG_PATH=/usr/lib/llvm-21/lib CARGO_TARGET_DIR="$PWD/target" cargo run -p gen-bindings
```

不要手改 `servo-quickjs/servo-quickjs/src/bindings.rs`。检查 `JS_NewDomStub` /
`JS_GetDomStubDomPtr` / `JS_GetDomStubRealm` 的签名与头文件一致。

#### 1.4 `servo-quickjs/mozjs/upstream-bindgen/jsapi.rs`

在现有 DomStub 声明旁更新/新增：

```rust
extern "C" {
    pub fn JS_GetDomStubDomPtr(obj: *mut JSObject) -> *const c_void;
}
```

`JS_NewDomStub` / `JS_UnbindDomStub` / `JS_GetDomStubInfo` 的 Rust 侧签名同步调整。

#### 1.5 `servo-quickjs/mozjs/src/engine/class.rs`

| 函数 | 改动 |
| --- | --- |
| `JS_NewDomStub(cx, clasp, dom_ptr, given_proto)` | 不再 `ObjectData::new`；`hold_realm(cx.current_realm())` 后把 `dom_ptr` / `realm` / `given_proto` 传给 C。`given_proto` 只存裸指针，不额外 retain |
| `JS_UnbindDomStub(cx, obj)` | 先读出 realm，调 C 解绑（同时清 `given_proto`），再 `release_realm(realm)`；不再释放 `ObjectData` |
| `JS_GetDomStubDomPtr(obj)` | 转发 `q::JS_GetDomStubDomPtr`，返回 `*const c_void` |
| `JS_GetDomStubGivenProto(obj)` | 转发 `q::JS_GetDomStubGivenProto`，返回 `*mut root::JSObject`（NULL 表示没有） |
| `JS_GetDomStubInfo` | 改为 `clasp_of_id(q::JS_GetClassID(v))` + `q::JS_GetDomStubRealm(v)` → `(*realm).global` / `context_of_realm(realm)` |
| `finalize` | **开头**加 stub 分支：`JS_IsDomStub(val)` 时取 realm，`dom_ptr` 非空则用 `clasp_of_id` 调 `cOps.finalize`，最后 `release_realm(realm)` 并 `return` |
| `gc_mark` | **开头**加 stub 分支：`dom_ptr` 为空直接返回；否则 `mark_realm(rt, realm, mark)` + 调 `cOps.trace`，不遍历 slots。**不标记 `given_proto`**：stub 没有持有它的引用计数，标记会让 decref pass 减掉一个不属于 stub 的计数（见 §8.5） |
| `object_class` | **最前面**（`object_data` / proxy / `JS_IsFunction` 之前）加 stub 分支，返回 `clasp_of_id(q::JS_GetClassID(obj_val(obj)))`。生成 hook 走 §1.8 的早退后不再依赖它，但 trace verifier（`verify.rs`）等仍可能拿到 stub |
| `known_object_realm` | **最前面**加 stub 分支，返回 `q::JS_GetDomStubRealm(...)`；`object_realm` 对 stub 的兜底 |
| `JS_GetReservedSlot` / `JS_SetReservedSlot` | 不加 stub 分支：stub 上不应有槽访问。生成 finalize hook 的 observable 清理改为在 hook 里判断（§1.10） |

`ObjectData` 本身保留：普通 `JSObject`、global、function、proxy 仍在用。

#### 1.6 `servo-quickjs/mozjs/src/jsapi2_wrappers.in.rs`

```rust
wrap!(jsapi: pub fn JS_NewDomStub(cx: &mut JSContext, clasp: *const JSClass, dom_ptr: *const c_void, given_proto: *mut JSObject) -> *mut JSObject);
wrap!(jsapi: pub fn JS_GetDomStubDomPtr(obj: *mut JSObject) -> *const c_void);
wrap!(jsapi: pub fn JS_GetDomStubGivenProto(obj: *mut JSObject) -> *mut JSObject);
```

`JS_UnbindDomStub` / `JS_GetDomStubInfo` 保持已有 wrap 条目，按新参数调整。

#### 1.7 `components/script_bindings/wrap.rs`

非 proxy 分支：

```rust
// 原逻辑不变：跨 realm 时先 JS_WrapObject，再取指针；没有则为 NULL。
let given = match given_proto { /* ... */ };
rval.set(JS_NewDomStub(
    cx,
    config.class.unwrap(),
    raw.as_ptr() as *const c_void,
    given as *const c_void,
));
assert!(!rval.is_null());
// 删除 JS_SetReservedSlot(DOM_OBJECT_SLOT) —— stub 不再有槽
```

`given_proto` 从 `MaterializeCache` 移到 stub；`set_materialize_cache` 此阶段只记录
`proto_object_fn` / `has_legacy_unforgeable_members` 两项。

#### 1.8 `components/script_bindings/conversions.rs`

`private_from_object`：

```rust
if unsafe { JS_IsDomStub(obj) } {
    return unsafe { JS_GetDomStubDomPtr(obj) };
}
// 原 is_dom_object / JS_GetReservedSlot / proxy 逻辑不变
```

`private_from_proto_check_static` 开头直接早退：

```rust
if is_dom_stub(obj) {
    // stub 的 class_id 已经是具体接口，不需要也不能做原型检查。
    return Ok(private_from_object(obj));
}
```

这样所有生成代码的 `native_from_object_static`（finalize / trace hook、方法序言）
拿 stub 时都直接得到 `dom_ptr`，不会把 stub 当普通 `JSObject` 去查 DOMClass 和原型链。

#### 1.9 materialize 的调用顺序（`reflector.rs`）

本阶段仍从 `MaterializeCache` 取 `proto_object_fn` / `has_legacy_unforgeable_members`，
但要把 `JS_UnbindDomStub` 移到"新对象创建 + 槽写入 + unforgeable 复制"之后：

```
读 ref_count / class / scope / raw_cx / given_proto
EnterRealm
proto_object_fn -> canonical_proto
proto = given_proto 非空 ? given_proto : canonical_proto
JS_NewObjectWithGivenProto(class, proto)
retain ref_count 次
JS_SetReservedSlot(DOM_OBJECT_SLOT, dom_ptr)
unforgeable 复制
LeaveRealm
JS_UnbindDomStub(cx, stub)     // 会清 given_proto 并 release_realm
rebind_jsobject
```

原因：阶段 2 的 `JS_UnbindDomStub` 会释放 stub 的那一份 realm 引用；必须先让新对象
（`ObjectData::new` 内 `hold_realm`）持有 realm，再释放。提前在本阶段调整顺序，
阶段 2 不需要再动。

> 安全性：materialize 期间 stub 由 live Rust 引用（`LIVE_DOM_REFERENCES`）计数保活，
> 创建新对象时即使触发 GC 也不会被回收。

#### 1.10 生成代码路径：stub 取 `this` 与 finalize hook

`finalizeHook()`（`codegen.py:7282`）是 codegen 的一个 Python 函数，负责生成每个 DOM
接口的 finalize hook 函数体 `_finalize`（`FINALIZE_HOOK_NAME`）。`CGClassFinalizeHook`
（`codegen.py:7394`）把它包装成：

```rust
unsafe extern "C" fn _finalize<D: DomTypes>(_cx: *mut GCContext, obj: *mut JSObject) {
    let this = native_from_object_static::<具体接口>(obj).unwrap();  // 通用序言
    // finalizeHook() 生成的部分
}
```

生成的 Rust 写到 `$OUT_DIR/Bindings/*.rs`（`target/*/build/script_bindings-*/out/Bindings/`），
由 `lib.rs` 的 `include!(concat!(env!("OUT_DIR"), "/Bindings/mod.rs"))` 引入；
`_finalize::<D>` 在 `init_domjs_class<D>()` 里以 `finalize_hook:` 注册。

它生成的内容只有两块：

1. **observable 槽清理**（`codegen.py:7283-7298`）：对 descriptor 的每个
   `[ObservableArray]` 成员生成一段代码。该属性第一次被访问时，会把一个
   ObservableArray proxy 存进 owner 对象的 reserved slot；proxy 的
   `OBSERVABLE_ARRAY_OWNER_SLOT`（`observablearray.rs:40`）里存的是 owner DOM 节点的
   裸指针。owner 被 finalize 时必须把这个反指针清成 undefined
   （`clear_owner_slot`，`observablearray.rs:173`），否则 proxy 之后还会解引用已释放的
   DOM 节点。本树里只有 `DocumentOrShadowRoot.adoptedStyleSheets` 用了
   `[ObservableArray]`，因此只有 `Document` / `ShadowRoot` 的 descriptor 会生成这段；
   其它接口 `cleanup` 为空，只生成下面的释放调用。
2. **释放 DOM 节点**：`finalize_common(this)` / `finalize_weak_referenceable(this)` /
   `finalize_global(obj, this)`。

第 1 块读的是 `obj` 自己的 reserved slot（`JS_GetReservedSlot(obj, slotIndex, ...)`），
不经过 `this`，所以 §1.8 的早退管不到它；改造后 stub 没有 `ObjectData`、也没有槽数组，
在 stub 上读槽是非法访问（不是读到 undefined）。而 stub 必然从未交给 JS，不可能创建过
observable proxy，跳过这段在语义上也正确。第 2 块必须执行：未 materialize 的节点正是
靠它释放 DOM 节点。
具体改动：

1. 在 `components/script_bindings/conversions.rs` 增加引擎无关的助手：

```rust
/// 该对象是否为尚未 materialize 的 `JSDomStub`（非 QuickJS 构建恒为 false）。
#[inline]
pub fn is_dom_stub(obj: *mut JSObject) -> bool {
    #[cfg(feature = "js-quickjs")]
    { unsafe { JS_IsDomStub(obj) } }
    #[cfg(not(feature = "js-quickjs"))]
    { let _ = obj; false }
}
```

`JS_IsDomStub` 从 `js::rust::wrappers2` 引入（与 `reflector.rs` 相同）。

2. `private_from_proto_check_static` 开头早退（见 §1.8），生成代码的
   `native_from_object_static` 对 stub 直接返回 `dom_ptr`，不做原型检查。

3. `finalizeHook()` 只给第 1 块加守卫（第 2 块不动）。以 `Document` 的
   `adoptedStyleSheets` 为例，生成结果形如：

```rust
unsafe extern "C" fn _finalize<D: DomTypes>(_cx: *mut GCContext, obj: *mut JSObject) {
    let this = native_from_object_static::<D::Document>(obj).unwrap();
    if !crate::conversions::is_dom_stub(obj) {
        let mut slot = UndefinedValue();
        JS_GetReservedSlot(obj, /* slotIndex */, &mut slot);
        if !slot.is_undefined() {
            unsafe { crate::observablearray::clear_owner_slot(slot.to_object()) };
        }
    }
    finalize_common(this);
}
```

   注意守卫是在 Python 模板层加的：`cleanup` 列表为空（没有 observable 成员的接口）
   时不生成任何守卫，finalize hook 与现在完全一致。

4. `finalize_common(this)` / `finalize_weak_referenceable(this)` / `finalize_global(obj, this)`
   照常执行：stub 的 DOM 节点正是由 `finalize_common` 通过 `dom_ptr` 释放的，
   不能因为它是 stub 就整体 return。

5. trace hook 不需要改：`(*this).trace(trc)` 不碰 JS 槽；只有 global 的 trace 路径读槽，
   而 global 从不创建 stub。

#### 1.11 阶段 1 验收

```sh
cd servo-quickjs && CARGO_TARGET_DIR="$PWD/target" cargo check -p mozjs
./mach check --js-engine quickjs
./mach build --js-engine quickjs
./tests/gc/lazy-binding/run.sh --binary target/debug/servoshell
```

重点确认：stub 上的 finalize / trace / `private_from_object` / `get_jsobject` panic
没有新增签名。

---

### 阶段 2：删除 `MaterializeCache`

#### 2.1 `components/script_bindings/utils.rs`

给 `DOMClass` 加两个字段（见 §2.2），并 `use crate::reflector::ProtoObjectFn;`。
`ProtoObjectFn` 从 `pub(crate)` 改为 `pub`，避免 private-in-public lint。

#### 2.2 `components/script_bindings/codegen/codegen.py`

`DOMClass()`（约 `2676`）输出新增两项：

```python
return f"""
DOMClass {{
    interface_chain: [ {prototypeChainString} ],
    depth: {descriptor.prototypeDepth},
    type_id: {DOMClassTypeId(descriptor)},
    malloc_size_of: {mallocSizeOf} as unsafe fn(&mut _, _) -> _,
    global: Globals::{globals_},
    proto_object_fn: GetProtoObject::<D>,
    has_legacy_unforgeable_members: {'true' if descriptor.hasLegacyUnforgeableMembers else 'false'},
}}"""
```

- `DOMClass` 字面量由 `init_domjs_class<D: DomTypes>()` 构造，`D` 在作用域内，
  `GetProtoObject::<D>` 无需 cast，Rust 会 coerce 到 `ProtoObjectFn`。
- 检查 codegen 里所有构造 `DOMClass` 的位置（包括 proxy handler 的 DOM class，
  如果存在）都已补上字段；漏一个就编译失败。

#### 2.3 `components/script_bindings/reflector.rs`

删除：

- `MaterializeCache` 结构体；
- `materialize_cache` 字段及 `Reflector::new` 里的初始化；
- `set_materialize_cache` / `clear_materialize_cache`；
- `materialize_if_needed` 里对 cache 的读取。

`is_js_binded()` 改为：

```rust
pub fn is_js_binded(&self) -> bool {
    #[cfg(feature = "js-quickjs")]
    { unsafe { !JS_IsDomStub(self.object.get()) } }
    #[cfg(not(feature = "js-quickjs"))]
    { true }
}
```

`materialize_if_needed` 新流程：

```rust
pub unsafe fn materialize_if_needed(&self) {
    crate::lazy_binding_stats::record_call();
    let stub = self.object.get();
    if !JS_IsDomStub(stub) {
        return;
    }
    crate::lazy_binding_stats::record_materialized();

    let dom_ptr = JS_GetDomStubDomPtr(stub);
    assert!(!dom_ptr.is_null(), "JSDomStub without a DOM object");
    let given_proto = JS_GetDomStubGivenProto(stub);

    let mut class = ptr::null();
    let mut scope = ptr::null_mut();
    let mut raw_cx = ptr::null_mut();
    JS_GetDomStubInfo(stub, &mut class, &mut scope, &mut raw_cx);

    let dom_class = {
        let clasp = js::rust::get_object_class(stub);
        assert!(is_dom_class(&*clasp), "stub is not a DOM class");
        &(*(clasp as *const DOMJSClass)).dom_class
    };

    let mut cx = JSContext::from_ptr(NonNull::new_unchecked(raw_cx));
    let cx = &mut cx;
    let old_realm = EnterRealm(cx, scope);
    // ... 创建 canonical_proto；
    //     proto = given_proto 非空 ? given_proto : canonical_proto；
    //     新对象、迁移 refcount、写槽、unforgeable（同阶段 1）
    LeaveRealm(cx, old_realm);
    JS_UnbindDomStub(cx, stub);   // 释放 stub 的 realm 引用
    self.rebind_jsobject(rval.get());
}
```

要点：

- `proto_object_fn` / `has_legacy_unforgeable_members` 来自 `dom_class`；
- `JS_UnbindDomStub` 仍在创建新对象之后调用。

#### 2.4 `components/script_bindings/wrap.rs`

非 proxy 分支改为：

```rust
if !config.is_proxy {
    // 原逻辑不变：跨 realm 时 JS_WrapObject，然后取指针；没有则为 NULL。
    let given = match given_proto { /* ... */ };
    rval.set(JS_NewDomStub(
        cx,
        config.class.unwrap(),
        raw.as_ptr() as *const c_void,
        given,
    ));
    return;
}
```

`given_proto` 始终随 stub 一起保存；materialize 时才决定用它还是 canonical prototype。

删除 `set_materialize_cache` 调用。

#### 2.5 `components/script_bindings/refcounted.rs` / `root.rs`

不需要改逻辑，但检查所有 `is_js_binded`、`rootable().get()` 的断言仍成立。

#### 2.6 统计与注释

- `lazy_binding_stats` 语义不变：`record_call` 仍在入口，`record_materialized` 在
  `JS_IsDomStub` 判定之后。
- 更新 `reflector.rs`、`wrap.rs`、`conversions.rs` 里提到 `ObjectData` / cache 的注释。

---

## 4. 逐文件改动清单

| 文件 | 改动 |
| --- | --- |
| `servo-quickjs/servo-quickjs/quickjs/quickjs.h` | 更新 `JS_NewDomStub`；新增三个访问器声明 |
| `servo-quickjs/servo-quickjs/quickjs/quickjs.c` | `JSDomStub` 布局；new/unbind/free/accessors；`JS_GetOpaque` stub 返回 NULL |
| `servo-quickjs/servo-quickjs/src/bindings.rs` | `gen-bindings` 重新生成，不手改 |
| `servo-quickjs/mozjs/upstream-bindgen/jsapi.rs` | 更新/新增 extern 声明 |
| `servo-quickjs/mozjs/src/engine/class.rs` | new/unbind/get-info/get-dom-ptr/get-given-proto/finalize/gc_mark/object_class/known_object_realm/slot 访问器 |
| `servo-quickjs/mozjs/src/jsapi2_wrappers.in.rs` | 更新 wrap 条目 |
| `components/script_bindings/wrap.rs` | 不写槽；阶段 2 删除 cache 调用 |
| `components/script_bindings/reflector.rs` | 阶段 1 调整 unbind 顺序；阶段 2 删 cache、重写 materialize |
| `components/script_bindings/conversions.rs` | `private_from_object` 先判 `JS_IsDomStub`；`private_from_proto_check_static` 对 stub 早退；新增 `is_dom_stub` 助手 |
| `components/script_bindings/utils.rs` | 阶段 2：`DOMClass` 加两字段 |
| `components/script_bindings/codegen/codegen.py` | 阶段 1 `finalizeHook()` 跳过 stub 的槽清理；阶段 2 `DOMClass()` 输出两字段 |
| `.claude/skills/servo-ohos-architecture/references/lazy-binding-materialize.md` | 改造后更新模型、输入来源、行号 |
| `.claude/skills/lazy-binding-tests/SKILL.md` | 若计数语义/触发点变化，同步更新 |

---

## 5. 正确性检查表

| 场景 | 必须满足 |
| --- | --- |
| stub 未 materialize 就被 GC | `free_dom_stub` 判 `dom_ptr`；非空才调 class finalizer；class.rs `finalize` 的 stub 分支再 `release_realm` |
| stub 已 unbound 后被释放 | `dom_ptr == NULL`，不调 finalizer，`realm == NULL`，不重复 release |
| materialize | 先创建新对象（`ObjectData::new` 持有 realm），再 `JS_UnbindDomStub` 释放 stub 的 realm 引用；顺序不可反 |
| refcount 迁移 | 保持现有 `JS_GetDomStubRefCount` + `retain_object` 循环不变 |
| `private_from_object(stub)` | 先判 `JS_IsDomStub`，返回 `dom_ptr`，不读槽 |
| `private_from_proto_check_static(stub)` | 开头早退到 `private_from_object`，不对 stub 做原型检查 |
| `given_proto(stub)` | 只存裸指针，不 retain、不在 `gc_mark` 中标记；materialize 时非空则替代 canonical proto；unbind 清空 |
| `object_class(stub)` | 必须在 `object_data` / `JS_IsFunction` 之前，按 `class_id` 返回 clasp |
| `known_object_realm(stub)` | 返回 stub 的 realm，不能落到 `object_data` / `realm_of_global` |
| finalize hook on stub | `native_from_object_static` 经 `private_from_proto_check_static` 早退拿到 `dom_ptr`；用 `is_dom_stub` 跳过 observable 槽清理；`finalize_common` 照常执行 |
| trace hook on stub | 不读槽；`(*this).trace(trc)` 正常追踪 Rust 侧 DOM 字段 |
| stub 上的槽访问 | 不应存在：不必给 `JS_GetReservedSlot` / `JS_SetReservedSlot` 加 stub 分支 |
| `gc_mark(stub)` | 标记 stub 的 realm；unbound（`dom_ptr == NULL`）直接返回 |
| proxy / global | 从不创建 stub，行为不变 |
| weakReferenceable | 走同一 finalize hook，只用 DOM 指针，不依赖槽 |
| SpiderMonkey 构建 | 所有 shim 逻辑在 `#[cfg(feature = "js-quickjs")]` 内；`DOMClass` 多两个字段在 SM 下也能编译 |

---

## 6. 验证

### 6.1 编译

```sh
cd servo-quickjs && CARGO_TARGET_DIR="$PWD/target" cargo check -p mozjs
./mach check --js-engine quickjs
./mach fmt
./mach test-tidy
```

阶段 2 额外确认 SpiderMonkey 构建仍能编译（至少 `cargo check -p servo-script-bindings`
不带 `js-quickjs`，或按 CI 的 SM 构建命令）。

### 6.2 lazy binding 回归

```sh
./mach build --js-engine quickjs
./tests/gc/lazy-binding/run.sh --binary target/debug/servoshell
```

重点用例：

- `document.createElement`、`getElementById`、`querySelectorAll` 索引访问；
- 内联事件处理器编译（`eventtarget.rs` 的 element 进入 scope chain）；
- 自定义元素 `html_constructor`；
- `new DOMRect()` / `new ImageData()` 等构造函数传入 prototype 的情况；
- `WeakRef` / LegacyUnforgeable（`event.isTrusted`）；
- iframe / 解析器创建的 `<iframe onload=...>`。

### 6.3 WPT

```sh
./mach test-wpt --release /dom/
./mach test-wpt --release /custom-elements/
./mach test-wpt --release /html/dom/
```

先确认没有新增 `get_jsobject() called ... JSDomStub` 签名。

### 6.4 内存对比

用阶段 0 的 heaptrack 数据对比：

- 未 materialize 节点：`JSDomStub` 40B → 56B（多了 `dom_ptr` / `realm` / `given_proto`），
  但少了 `ObjectData` + slot（≥48B）和 `MaterializeCache` 24B；净减少约 56B/节点（不含 `Reflector`）。
- `Reflector`：40B → 16B（`Reflector<()>`）、40B → 24B（`Reflector<AssociatedMemory>`）。
- materialize 后：JS 对象大小不变，`Reflector` 少 24B。

### 6.5 统计

```sh
SERVO_LAZY_BINDING_STATS=1 target/debug/servoshell ...
```

`materialize_count()` 的语义不变。

---

## 7. 提交拆分

按 fork 的 `CONTRIBUTING.md`，每个提交都要有 `Validation:` 行；AI 生成代码需加
`AI-assisted` trailer。

1. **`script_bindings: store dom_ptr and realm directly in JSDomStub`**
   - 阶段 1：C 结构（含 `given_proto`）、shim、wrap、private_from_object、unbind 顺序。
   - Validation: `./tests/gc/lazy-binding/run.sh --binary target/debug/servoshell`;
     WPT `/dom/`; heaptrack 单节点对比。
2. **`script_bindings: move materialize inputs into DOMClass and drop MaterializeCache`**
   - 阶段 2：DOMClass + codegen + materialize 重写。
   - Validation: 同 1，加 `/custom-elements/` 与 `new DOMRect()`。
3. **`docs: update lazy binding references`**
   - 更新 skill reference 与本文。

如果阶段 2 改动过大，可再拆：
`DOMClass + codegen`、`删除 MaterializeCache` 两个提交，
每个都保持可编译。

---

## 8. 风险与未决点

1. **生成代码里残留的槽访问**：目前只有 `finalizeHook` 的 observable 清理会读槽，
   已用 `is_dom_stub` 跳过；实施时确认没有其它生成代码在 stub 上调用
   `JS_GetReservedSlot`（global 的 finalize/trace 路径不会遇到 stub）。
2. **生成代码取 `this`**：`private_from_proto_check_static` 的 stub 早退是生成
   finalize/trace hook 能工作的前提；确认所有生成代码都经由 `native_from_object_static`
   取 `this`，没有别的地方直接对 stub 调 `get_dom_class`。
3. **`ProtoObjectFn` 的可见性**：`DOMClass` 是 `pub`，字段类型需 `pub`；
   `GetProtoObject::<D>` 在 `init_domjs_class<D>` 里可用，无需额外 import。
4. **proxy 的 `DOMClass`**：检查 codegen 是否有第二处构造 `DOMClass`；有则同步补字段。
5. **`given_proto` 的裸指针**：`JSDomStub` 只保存指针，不持有引用，也不在 `gc_mark`
   里标记——与现在的 `MaterializeCache` 一致。若要在 stub 里真正保活它，必须让 stub
   `JS_DupValue` 一份并在 unbind / finalize 时释放；只加 `JS_MarkValue` 会让 decref pass
   减掉 stub 并不拥有的计数。本次保持现状，不作为目标。
6. **额外引用泄漏**：materialize 时 `retain_object` 补齐的计数没有对应 pool 条目，
   这是改造前就存在的问题，本次不改变其语义。
7. **变体 A（去掉 `class_id`）不在本次范围**：收益只有未 materialize 节点的 8B，
   需要 `Reflector` 重排 + runtime 钩子，后续单独评估。
