# 懒绑定 v2：极简 JSDomStub（删除 ObjectData 与 MaterializeCache）

本文取代 v1 的结论（保留 `ObjectData` + 每接口注册表）。v2 的前提是：

> **假设 `JSDomStub` 里什么都没有，只保留 QuickJS 非有不可的字段。**
> `ObjectData`、`MaterializeCache`、`DOM_OBJECT_SLOT` 全部删除；
> materialize 需要的其余信息从 Rust 世界（DOM 对象、每接口的 `DOMClass`、`PrototypeList`）重新获取。

对应回答的问题是 `docs/dom-gc-survey/sections/servo-quickjs-lazy-binding.md:53-59`
列出的 5 项「必须缓存」的输入还能不能继续缩减。

---

## 0. 结论速览

**推荐的 v2 stub（变体 B）：**

```c
typedef struct JSDomStub {
    JSGCObjectHeader header;   // 16B，必须第一个
    uint16_t class_id;         // 2B（对齐后占 8B）
    void *dom_ptr;             // 8B：DOM 节点（即 Reflector）地址，unbind 后为 NULL
    void *realm;               // 8B：RealmData*，持有引用，unbind 后为 NULL
} JSDomStub;                   // payload 40B，加 8B JSMallocBlockHeader = 分配 48B
```

| 项目 | 现状 | v2 | 变化 |
|---|---:|---:|---:|
| `JSDomStub`（含 8B malloc header） | 40 | 48 | +8 |
| `ObjectData` + 槽数组（1 slot） | 32 + 16 | 0 | −48 |
| `DOM_OBJECT_SLOT` 写入 | 有 | 无 | — |
| `MaterializeCache`（每节点，Rust） | 24 | 0 | −24 |
| `Reflector<T>` | 40 | 16 | −24 |
| **未 materialize 单节点（JS + cache）** | **112** | **48** | **−64** |
| **未 materialize 单节点（含 Reflector）** | **128** | **64** | **−64** |
| **materialize 后单节点（JS + cache）** | **176** | **152** | **−24** |
| **materialize 后单节点（含 Reflector）** | **192** | **168** | **−24** |

5 项输入的归宿：

| # | 输入 | 大小 | v2 处理 | 每节点成本 |
|---|---|---:|---|---:|
| 1 | `given_proto` | 8 | `Some` ⇒ wrap 时立即 materialize；不创建 stub | 0 |
| 2 | `proto_object_fn` | 8 | 每接口数据，放进 `DOMClass`（codegen 生成） | 0 |
| 3 | `has_legacy_unforgeable_members` | 1 | 同上，放进 `DOMClass` | 0 |
| 4 | `clasp` | 8 | stub 只存 `class_id`，经现有 `CLASPS_BY_ID` 还原 | 8（含对齐） |
| 5 | `realm` | 8 | stub 直接存 `RealmData*` | 8 |
| — | `dom_ptr` | 8 | stub 直接存，替代 `DOM_OBJECT_SLOT` | 8 |
| — | GC 元数据 | 24 | 不变 | 24 |

也就是说：**每节点真正无法省掉的只有 `dom_ptr` 与 `realm`（各 8B）；`class_id` 只是为了
复用 QuickJS 现有的 `rt->class_array[class_id]` 派发才保留（8B，见 §3.5 的变体 A）；
另外两项是每接口数据，`given_proto` 根本不需要保存。**

---

## 1. 为什么 stub 不能再小

一个 stub 分配必然包含：

| 组成 | 大小 | 能否去掉 |
|---|---:|---|
| `JSMallocBlockHeader`（`ref_count`、`gc_obj_type`、`mark`） | 8 | 不能。QuickJS 的引用计数与 GC 类型挂在分配头里，属于分配器元数据 |
| `JSGCObjectHeader`（`list_head`） | 16 | 不能。stub 必须在 runtime 的 GC 对象链表上 |
| `dom_ptr` | 8 | 不能。stub 是 QuickJS 唯一看到的东西，回收时必须靠它找到 DOM 节点；反向指针（DOM 的 `Reflector.object`）指向 stub，方向不可逆 |
| `realm` | 8 | 不能。见 §2.5 |

`quickjs.c:493` 的 TODO（把 stub 结构嵌入 `Reflector`）依然不可行：`JSMallocBlockHeader`
的 `ref_count`/`gc_obj_type` 是**按分配**而不是按对象存的，且 `js_free_rt` 假定这块内存由
`js_malloc` 分配。嵌入要把 stub 的引用计数、GC 链表和回收所有权全部搬到 Rust 侧，
就是 survey 文档里「在 rust 上 reflector 上加引用计数」那条路，改动面远大于收益。

---

## 2. 五项输入逐项重审（`servo-quickjs-lazy-binding.md:53-59`）

### 2.1 `given_proto` —— 不保存，`Some` 时立即 materialize

`given_proto` 只在 `wrap` 的调用现场存在：

- 来源是 JS 构造路径，codegen 生成的 `Wrap` 把 `get_desired_proto()` 的结果作为
  `Some(desired_proto)` 传入（`codegen.py:3456-3475`、`codegen.py:1568-1577`）；
- 之后的 DOM 对象与 stub 都不记录它，materialize 时 `new.target` 已不存在，无法重算。

但这类对象在同一调用里就会被消费：构造器返回前 `wrapForType()` 会走
`(result).to_jsval(...)`，`ToJSValConvertible for Reflector<T>`
（`components/script_bindings/conversions.rs:176-190`）立刻调用 `materialize_if_needed()`。

所以 v2 直接把这条路径改成**立即创建对象、根本不产生 stub**：

```rust
// wrap_object() 非 proxy 分支
if let Some(given) = given_proto {
    // 用给定 proto 直接创建真正的 JSObject（复用现有 create 逻辑）
    create_dom_object(cx, scope, given, config, raw.as_ptr(), rval);
    return;
}
```

顺带消掉 v1 指出的隐患：cache 里的 `given_proto` 是裸指针、不 retain 也不 trace
（`reflector.rs:101-113`），从 wrap 到 materialize 之间并不保活。

代价：构造途中抛异常的极端情况下会提前创建一次对象，不影响正确性。

### 2.2 `proto_object_fn` —— 每接口数据，进 `DOMClass`

它不是每节点数据，而是每个接口一个：

- 定义是 codegen 生成的 `GetProtoObject::<D>`（`CGGetPerInterfaceObject`，
  `codegen.py:4142-4158`），签名正好是 `ProtoObjectFn`；
- 它的产物 canonical prototype 也是每 (接口, realm) 一份，`ProtoOrIfaceArray` 已经缓存。

每节点存一份是浪费。v2 放进 `DOMClass`（`components/script_bindings/utils.rs:57-73`）：

```rust
pub struct DOMClass {
    /* 现有字段 */
    pub interface_chain: [PrototypeList::ID; MAX_PROTO_CHAIN_LENGTH],
    pub depth: u8,
    pub type_id: TopTypeId,
    pub malloc_size_of: unsafe fn(&mut MallocSizeOfOps, *const c_void) -> usize,
    pub global: Globals,

    /// v2：该接口的 canonical prototype 创建函数。
    pub proto_object_fn: ProtoObjectFn,
    /// v2：该接口是否有 [LegacyUnforgeable] 成员。
    pub has_legacy_unforgeable_members: bool,
}
```

`DOMClass` 全仓库只有 codegen 一处构造（`codegen.py:2676-2696`），加两行即可：

```python
protoObjectFn = "GetProtoObject::<D> as ProtoObjectFn" \
    if not descriptor.isGlobal() else "GetProtoObject::<D> as ProtoObjectFn"
# DOMClass { ..., proto_object_fn: <上面>, has_legacy_unforgeable_members: <bool> }
```

可达性：materialize 时 `get_dom_class(stub)`（`conversions.rs:218-237`）已经通过
stub 的 class 拿到 `*const DOMJSClass`，其 `dom_class` 就是这张表。**不需要新的注册表、
不需要新的初始化顺序、没有多 `D` 的问题**（v1 方案 C1 的三项成本全部消失）。

`has_legacy_unforgeable_members` 也可以不存，改为创建出 canonical proto 后读 prototype
class 的 reserved-slot 数（`CGPrototypeJSClass`，`codegen.py:2812-2842`，只在
`hasLegacyUnforgeableMembers && !isGlobal` 时申请 1 个 slot）。但那是隐式约定，
放进 `DOMClass` 是显式的，成本也一样是每接口一份，推荐后者。

### 2.3 `clasp` —— 每接口数据，stub 里只留 2B 的 `class_id`

`clasp` 本身是每接口的（`WrapConfig.class = Some(&Class.get().base)`），但 materialize
需要从「一个 stub」找回它，而 stub 是 QuickJS 的对象，必须能用 O(1) 的方式查到
`rt->class_array[class_id].finalize/gc_mark`。因此 stub 里保留 `class_id`（u16），
通过已有的 `CLASPS_BY_ID`（`servo-quickjs/mozjs/src/engine/class.rs:69-150`）还原
`*const JSClass`。

这是**索引**而不是 `clasp` 指针：2B 数据被 8B 对齐放大成 8B，但没有额外语义。

### 2.4 `has_legacy_unforgeable_members` —— 见 2.2

### 2.5 `realm` —— 只能存在 stub 里

materialize 需要 realm 提供三样东西：

| 用途 | 来源 |
|---|---|
| `EnterRealm(cx, global)` 的 global | `(*realm).global` |
| shim `JSContext` | `context_of_realm(realm)` |
| 保证 realm 在 stub 存活期间不被释放 | `hold_realm(realm)`（`engine/realm.rs:37-43`） |

为什么不能从 Rust 世界重新获取：

1. **DOM 结构体普遍不保存 `GlobalScope`。** `reflect_dom_object*` 只在创建时用到
   `scope`；`DOMRect`、`CanvasRenderingContext2D` 这类结构体没有 global 字段。
2. **materialize 只有一个 type-erased 的 `dom_ptr`。** `Document.getElementById` 返回
   `Element`，实际可能是 `HTMLDivElement`；从 `*const c_void` 上没有任何 vtable 能
   回到具体类型的 global。
3. 即使某些类型能拿到 global，那时 realm 可能已经被销毁——stub 必须**自己持有**
   `hold_realm`，这本来就要求存一个指针。

**不能搬进 `Reflector`：** 搬过去每节点永久 +8B（字段永远存在）。未 materialize 时
总量是 40 + 8 = 48B，与 v2 的 48B 相同；materialize 后是 152 + 8 = 160B，比 v2 的 152B 差。
而且 `Reflector::drop` 没有 runtime/context 上下文来 `release_realm`
（`engine/realm.rs:47-53` 需要 realm 里的 qctx）。所以 realm 留在 stub 里，随 stub 一起释放。

---

## 3. JSDomStub v2

### 3.1 布局

```c
/* quickjs.c: ZSDomStub */
typedef struct JSDomStub {
    JSGCObjectHeader header;   // 16B
    uint16_t class_id;         // 2B + 6B padding
    void *dom_ptr;             // 8B
    void *realm;               // 8B
} JSDomStub;
```

`sizeof = 40`，加 8B `JSMallocBlockHeader` 后每次分配 48B。

字段顺序可以换成 `{ header, dom_ptr, realm, class_id }`：payload 仍是 40（`32 + 2` 向上对齐）。
没有更省的对齐方案。

### 3.2 不变量

1. `dom_ptr != NULL` ⇔ stub 拥有 DOM 节点（负责调用该 class 的 finalizer）**且**持有一个
   `realm` 引用。
2. `JS_UnbindDomStub` 之后 `dom_ptr == NULL && realm == NULL`：stub 只是池里等待释放的
   空壳，不再 finalize 节点，也不再持有 realm。
3. `class_id` 在 stub 整个生命周期内不变，即使 unbound 之后仍然可读
   （`JS_GetClassID` 依赖它；unbound stub 不应再触发 class hook）。
4. stub 从不暴露给 JS。它只存在于 `Reflector.object`，所有交给 JS 的路径都先 materialize。

### 3.3 生命周期

```
wrap:        JS_NewDomStub(class_id, dom_ptr, hold_realm(current_realm))
             Reflector.object = stub                       // counted 语义不变
materialize: 读 dom_ptr / realm / class / DOMClass
             EnterRealm((*realm).global)
             proto_object_fn(canonical_proto)              // 懒创建 prototype
             JS_NewObjectWithGivenProto(...)               // 新 ObjectData 自己 hold_realm
             拷贝 unforgeable、迁移 refcount
             JS_UnbindDomStub + release_realm(stub 的 realm)
             Reflector.object = 新对象
未 materialize 就被回收:
             free_dom_stub → finalizer(stub)
               → class 的 finalize hook（经 private_from_object(stub) 拿到 dom_ptr）
               → release_realm(stub.realm)
```

**顺序要求**：materialize 必须先创建新 `JSObject`（其 `ObjectData::new` 会
`hold_realm(cx.current_realm())`），再 unbind 并释放 stub 的那一份 realm 引用。
反过来会在创建新对象前把 realm 释放掉。

### 3.4 变体 A：去掉 `class_id`（40B）

理论上还可以再省 8B：

```
stub = { header(16), dom_ptr(8), realm(8) } = 32B payload + 8B = 40B
```

代价是把 class 解析从 C 的 `rt->class_array` 搬到 Rust：

- `Reflector` 字段重排，让 `proto_id` 在固定偏移
  （现在 `size: T` 在 `object` 与 `proto_id` 之间，`Reflector<()>` 与
  `Reflector<AssociatedMemory>` 的 `proto_id` 偏移不同）；
- `wrap()` 里 `set_proto_id`（`wrap.rs:58`）必须提前到 `JS_NewDomStub` 之前；
- 新增 `proto_id → *const JSClass` 的生成表；
- QuickJS runtime 增加 stub 的 finalize/gc_mark 钩子（GC 期间 C→Rust 回调），
  `JS_GetClassID(stub)` 语义要重新定义，`object_class`/`object_data` 改走 Rust 解析。

收益只有未 materialize 节点的 8B（materialize 后不变）。**建议作为后续可选优化，
第一版不做。**

### 3.5 `Reflector` v2

```rust
#[cfg_attr(feature = "js-quickjs", repr(C))]
pub struct Reflector<T = ()> {
    object: Heap<*mut JSObject>,
    size: T,
    proto_id: Cell<u16>,
    counted: Cell<bool>,
    // materialize_cache 删除
}
```

尺寸：`Reflector<()>` = 16B，`Reflector<AssociatedMemory>` = 24B（比现状各少 24B）。
`is_js_binded()` 改为：

```rust
#[cfg(feature = "js-quickjs")]
unsafe { !JS_IsDomStub(self.object.get()) }
```

`proto_id` 保留（`is::<T>()`、弱引用断言等仍在使用），但 materialize 不再依赖它，
所以 `wrap()` 里「先 wrap_object、后 set_proto_id」的顺序可以保持不变。

---

## 4. `materialize_if_needed` v2

```rust
pub unsafe fn materialize_if_needed(&self) {
    let stub = self.object.get();
    if !JS_IsDomStub(stub) {
        return;                       // 已 materialize 或 proxy
    }

    // 1. 从 stub 与每接口表取输入
    let dom_ptr = JS_GetDomStubDomPtr(stub);
    let realm   = JS_GetDomStubRealm(stub);
    assert!(!dom_ptr.is_null() && !realm.is_null());

    let class     = get_object_class(stub);              // 经 class_id，见 §6.2
    let dom_class = get_dom_class(stub).expect("stub is not a DOM class");

    let mut cx = JSContext::from_ptr(context_of_realm(realm));
    let cx = &mut cx;

    rooted!(&in(cx) let mut rval = ptr::null_mut::<JSObject>());
    let old_realm = EnterRealm(cx, (*realm).global);

    // 2. 懒创建 canonical prototype
    rooted!(&in(cx) let mut proto = ptr::null_mut::<JSObject>());
    (dom_class.proto_object_fn)(cx, HandleObject::from_marked_location(&(*realm).global),
                                proto.handle_mut());
    assert!(!proto.is_null());

    // 3. 用 stub 的 class（实际接口，不是静态类型）创建对象
    let ref_count = JS_GetDomStubRefCount(stub);
    rval.set(JS_NewObjectWithGivenProto(cx, class, HandleObject::from_marked_location(&proto.get())));
    assert!(!rval.is_null());
    for _ in 0..ref_count { js::quickjs::retain_object(rval.get()); }

    // 4. legacy unforgeable
    if dom_class.has_legacy_unforgeable_members {
        let mut slot = UndefinedValue();
        JS_GetReservedSlot(proto.get(), DOM_PROTO_UNFORGEABLE_HOLDER_SLOT, &mut slot);
        // ... JS_InitializePropertiesFromCompatibleNativeObject
    }

    LeaveRealm(cx, old_realm);

    // 5. 先建好新对象，再解绑并释放 stub 的 realm 引用
    JS_UnbindDomStub(cx, stub);       // 内部：C 侧清空两个字段，再 release_realm
    self.rebind_jsobject(rval.get()); // 含 DOM_OBJECT_SLOT 写入（见下）
}
```

**`DOM_OBJECT_SLOT` 的写入**：stub 上不再写；但物化后的对象仍需要它。`rebind_jsobject`
里对新的 `JSObject` 写一次：

```rust
JS_SetReservedSlot(rval.get(), DOM_OBJECT_SLOT, &PrivateValue(dom_ptr));
```

`JS_SetReservedSlot` 在 stub 上应当 `debug_assert!` 失败（不允许），`JS_GetReservedSlot`
在 stub 上返回 `undefined`（生成代码的 finalize hook 会读 observable-array 槽，见 §7.2）。

---

## 5. `wrap_object` v2

```rust
unsafe fn wrap_object<T: MutDomObject>(
    cx: &mut JSContext, scope: &HandleObject,
    given_proto: Option<Handle<*mut JSObject>>,
    raw: &Root<MaybeUnreflectedDom<T>>, config: &WrapConfig,
    mut rval: MutableHandleObject,
) {
    let _ac = JSAutoRealm::new(cx.raw_cx(), scope.get());

    if !config.is_proxy {
        if let Some(given) = given_proto {
            // 立即 materialize：不创建 stub，不保存 given_proto。
            create_dom_object(cx, scope, *given, config, raw.as_ptr(), rval);
            return;
        }
        // 懒绑定：stub 自己带 dom_ptr 与 realm，Rust 侧没有任何每节点状态。
        rval.set(JS_NewDomStub(cx, config.class.unwrap(), raw.as_ptr() as *mut c_void));
        assert!(!rval.is_null());
        return;
    }
    /* proxy 分支不变 */
}
```

`set_materialize_cache` / `clear_materialize_cache` / `MaterializeCache` 全部删除。

---

## 6. 改动清单

行号为当前分支的位置，实施时会移动。

### 6.1 `servo-quickjs/servo-quickjs/quickjs/quickjs.c`

| 位置 | 改动 |
|---|---|
| `494-501` `struct JSDomStub` | `opaque` → `dom_ptr` + `realm`；`class_id` 保留 |
| `6626-6638` `JS_NewDomStub` | 参数改为 `(ctx, class_id, dom_ptr, realm)`；`js_trigger_gc(sizeofJSDomStub)` 仍用新尺寸 |
| `6644-6647` `JS_UnbindDomStub` | `p->dom_ptr = NULL; p->realm = NULL;` |
| `6659-6678` `free_dom_stub` | 判 `p->dom_ptr`（而不是 `p->opaque`）决定是否调用 class finalizer |
| `4051-4061` `JS_GetClassID` | 不变（仍读 `class_id`） |
| `6974-6982` GC mark | 不变（仍按 `class_array[class_id].gc_mark` 派发） |
| `11468-11484` `JS_GetOpaque` | stub 分支返回 `NULL`（已经没有 opaque 了） |
| 新增 | `JS_GetDomStubDomPtr(JSValue)`、`JS_GetDomStubRealm(JSValue)` 访问器 |

### 6.2 `servo-quickjs/mozjs/src/engine/class.rs`

| 位置 | 改动 |
|---|---|
| `687-701` `JS_NewDomStub` | 不再 `ObjectData::new`；`hold_realm(cx.current_realm())` 并把两个指针传给 C；签名加 `dom_ptr` |
| `703-718` `JS_UnbindDomStub` | 读出 realm → C 解绑 → `release_realm(realm)`；不再释放 `Box<ObjectData>` |
| `733-743` `JS_GetDomStubInfo` | 去掉 `object_data` 依赖，改为 `class_id → clasp` + 两个访问器；返回值加 `realm` |
| `224-237` `finalize` | 开头加 stub 分支：`dom_ptr` 非空时调用 class 的 `cOps.finalize`（参数仍传 stub，生成 hook 经 `private_from_object` 解析），再 `release_realm`；unbound stub 直接返回 |
| `369-391` `gc_mark` | stub 分支：`mark_realm(rt, stub.realm, mark)` + 调用 class 的 `cOps.trace`（保持今天 stub 也 trace DOM 结构体的语义）；不再遍历 slots |
| `184-196` `object_class` | 在 `proxy_class` / `JS_IsFunction` **之前**加 stub 分支：`clasp_of_id(JS_GetClassID(obj_val(obj)))`。否则 `JS_IsFunction` 会把 stub 当 `JSObject` 读 `p->class_id` |
| `208-222` `known_object_realm` | stub 分支返回 stub 的 realm（放在 `object_data` 之前） |
| `759-770` `JS_GetReservedSlot` | stub 分支返回 `undefined`（生成 finalize hook 会读 observable 槽） |
| `772+` `JS_SetReservedSlot` | stub 分支 `debug_assert!(false)`（wrap 不再写槽） |
| `jsapi2_wrappers.in.rs:682-687` | 更新 `JS_NewDomStub` / `JS_GetDomStubInfo` 签名，导出新访问器 |

`ObjectData` 本身保留（普通 `JSObject`、global、function 仍在用），只是 stub 不再分配它。

### 6.3 `components/script_bindings/`

| 位置 | 改动 |
|---|---|
| `reflector.rs:64-76` | 删除 `MaterializeCache` |
| `reflector.rs:92-99` | 删除 `materialize_cache` 字段 |
| `reflector.rs:237-250` | `is_js_binded` → `!JS_IsDomStub` |
| `reflector.rs:252-279` | 删除 `set_materialize_cache` / `clear_materialize_cache` |
| `reflector.rs:303-404` | 按 §4 重写 |
| `reflector.rs:407-421` | `new()` 去掉 cache 初始化 |
| `wrap.rs:77-110` | 按 §5 重写；删除槽写入 |
| `conversions.rs:256-275` | `private_from_object` 先判 `JS_IsDomStub` → `JS_GetDomStubDomPtr`，否则走槽 |
| `utils.rs:57-73` | `DOMClass` 加 `proto_object_fn` / `has_legacy_unforgeable_members` |

### 6.4 `components/script_bindings/codegen/codegen.py`

| 位置 | 改动 |
|---|---|
| `2676-2696` `DOMClass()` | 输出新增两个字段，`proto_object_fn: GetProtoObject::<D> as ProtoObjectFn` |
| 生成模块的 `use` | 确保 `ProtoObjectFn` 在 scope 内（`GetProtoObject` 与本定义同模块） |

---

## 7. 正确性边界

| 场景 | 处理 |
|---|---|
| **stub 被 GC 回收（未 materialize）** | `free_dom_stub` → 该 class 的 finalizer → shim 的 stub 分支：调用生成 finalize hook（`private_from_object(stub)` 拿到 dom_ptr）、`release_realm` |
| **stub 上的生成 finalize hook** | 会经 `getReservedSlot(obj, memberSlot)` 读 observable-array 槽；`JS_GetReservedSlot(stub)` 必须返回 `undefined`，否则 panic |
| **stub 上的 gc_mark** | 标记 stub 的 realm，并调用 class 的 trace hook——今天 stub 有 `ObjectData`，这条路径已经在 trace DOM 结构体里的 JS 值，不能退化 |
| **`get_object_class(stub)`** | 必须在 `JS_IsFunction` 探测前走 `class_id`。否则把 stub 当 `JSObject` 读字段 |
| **`known_object_realm(stub)`** | 返回 stub 的 realm；不能落到 `object_data`/`realm_of_global` |
| **`private_from_object(stub)`** | 先判 `JS_IsDomStub`，返回 `dom_ptr` |
| **`is_dom_object(stub)`** | 修好 `object_class` 后自然为真；`private_from_proto_check` 的链检查照常工作 |
| **realm 时序** | wrap 时 `hold_realm`；materialize 先建新对象再 unbind/release；未 materialize 回收时在 finalizer 里 release；unbound stub 不得重复 release |
| **refcount 迁移** | `JS_GetDomStubRefCount` 不变，仍在新对象上 `retain_object` 补齐 |
| **proxy / global** | 从不创建 stub（`CGWrapGlobalMethod` 直接建对象），不受影响 |
| **weakReferenceable** | 走同一个 finalize hook，`finalize_weak_referenceable(this)` 只用 DOM 指针，不需要槽 |
| **observable array** | 未 materialize 时槽恒为 `undefined`，finalize 清理天然跳过 |
| **SpiderMonkey 构建** | `DOMClass` 多两个字段在 SM 下也能编译；所有 stub 逻辑都在 mozjs/shim 与 `cfg(feature = "js-quickjs")` 内 |

---

## 8. 内存对比

### 8.1 单节点（B）

| 状态 | 现状 | v2 | 变化 |
|---|---:|---:|---:|
| 未访问：stub | 40 | 48 | +8 |
| 未访问：`ObjectData` + 槽数组 | 32 + 16 | 0 | −48 |
| 未访问：Rust cache | 24 | 0 | −24 |
| **未访问：JS + cache** | **112** | **48** | **−64** |
| **未访问：含 `Reflector`（40→16）** | **128** | **64** | **−64** |
| materialize 后：JS | 152 | 152 | 0 |
| materialize 后：cache / Reflector | 24 + 40 | 0 + 16 | −48 |
| **materialize 后：含 `Reflector`** | **192** | **168** | **−24** |

（未把重分配/碎片算进去；materialize 时 stub 释放要等 autorelease pool 排空。）

### 8.2 一类接口（N 个节点 + prototype P，1.1KB–45KB）

| 场景 | 现状 | v2 | 变化 |
|---|---:|---:|---:|
| N 个均未被 JS 访问 | 112N | 48N | −64N |
| N 个均已被 JS 访问 | 176N + P | 152N + P | −24N |
| P 的创建时机 | 首个节点 wrap 时 | 首个节点 materialize 时 | 全未触达时 P = 0 |

### 8.3 变体 A（若将来采用）

未访问节点 40N（再省 8N），materialize 后不变。

---

## 9. 实施顺序

1. **删除 `ObjectData`（stub 加 `dom_ptr` / `realm`）**
   QJS 结构体、shim 的 new/unbind/finalize/gc_mark、`object_class` / `known_object_realm` /
   `private_from_object` / 槽访问器，`wrap` 改为传 `dom_ptr`。
   此时 `MaterializeCache` 仍在，materialize 仍可从 cache 取 proto 信息。
   验证：`lazy-binding-tests`、`get_jsobject` 相关崩溃签名不再出现。
2. **删除 `MaterializeCache`**
   `DOMClass` 加两字段 + codegen；`given_proto` 立即 materialize；重写 materialize。
   验证：见 §10。
3. **（可选）变体 A**
   去掉 `class_id`，做 runtime 钩子与 `proto_id` 重排。

---

## 10. 验证

1. `./mach test-unit -p script_bindings`、`./mach test-wpt` 的 DOM / custom elements /
   `document.createElement` / `new DOMRect()` 用例（`given_proto` 路径）。
2. `lazy_binding_stats`：`record_materialized` 的计数在去掉 cache 后应当只增不减；
   带 `given_proto` 的构造路径不应再出现 stub。
3. heaptrack 对比单节点：未访问节点应从 112B 降到 48B；materialize 后从 176B 降到 152B。
4. `./mach fmt` + `./mach test-tidy`；`js-quickjs` 与 SpiderMonkey 两种 feature 都要编译。
5. 重点回归：解析器创建的 `<iframe onload=...>`（stub 上触发 finalize/trace）、
   自定义元素构造、WeakRef、`event.isTrusted` 之类 LegacyUnforgeable 成员。

---

## 附：一句话回答

- **能删的**：`ObjectData`（32+16B）、`MaterializeCache`（24B）、`DOM_OBJECT_SLOT` 写入。
- **不能从 Rust 世界恢复的**：`realm`（DOM 结构体不存 global，type-erased 指针拿不到）
  和 `given_proto`（只存在于调用现场）——后者改成 `Some` 时立即 materialize，不需要存。
- **本来就不是每节点数据的**：`proto_object_fn`、`has_legacy_unforgeable_members`，
  放进每接口一份的 `DOMClass`。
- **剩下能省但代价大的**：`class_id`（8B），需要 QuickJS runtime 钩子 + `Reflector` 重排（变体 A）。
- **最终**：未 materialize 节点 48B（含 GC 头），materialize 后与无懒绑定持平（152B）。
