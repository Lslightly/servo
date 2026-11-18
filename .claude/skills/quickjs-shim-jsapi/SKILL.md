---
name: quickjs-shim-jsapi
description: 在 servo-ohos 的 QuickJS shim（servo-quickjs）中新增一个 mozjs/JSAPI 函数（例如 JS_IsDomStub）并暴露给 servo 的 js::rust::wrappers2。当需要新增跨 servo → mozjs shim → QuickJS 的 FFI 接口，或遇到 "unresolved import js::rust::wrappers2::JS_X"、重新生成后 bindings.rs 里缺少函数、stub_bindings 名称/参数个数不匹配时使用。
---

# 给 QuickJS shim 添加一个 JSAPI

本仓库用 `servo-quickjs` 替代 SpiderMonkey 实现 mozjs API。新增一个 shim 专用 API 要穿过 5 层，
每一层都必须存在，否则要么编译不过，要么链接不到实现：

```
servo 代码（如 components/script_bindings/reflector.rs）
  └─ use js::rust::wrappers2::JS_X;      ← wrap! 生成的 safe wrapper
       └─ jsapi::JS_X                    ← mozjs/upstream-bindgen/jsapi.rs 的 extern 声明
            └─ engine::JS_X              ← mozjs/src/engine/*.rs 实现；build.rs 自动生成 extern "C" 转发
                 └─ q::JS_X              ← servo-quickjs/src/bindings.rs（bindgen 生成，勿手改）
                      └─ quickjs.c 实现  ← 对应 quickjs.h 的声明
```

以 `JS_IsDomStub` 为完整示例，下面每一步都给出实际改动点。

## 步骤

### 1. quickjs.h：声明 C 接口

文件：`servo-quickjs/servo-quickjs/quickjs/quickjs.h`（路径从 servo 仓库根算起）。

- 放在同一类 API 附近（如 `JS_GetClassID` 旁），用 `/* Servo: ... */` 注释说明用途。
- **签名必须和 `quickjs.c` 里的定义逐字一致**（返回类型、参数类型、const、名称）。
  bindgen 只读头文件；头文件与定义不一致会产生错误的 Rust 签名，或链接期/运行期 UB。
- 例：

```c
/* Servo: allocate a new JSDomStub, i.e. a lazy DOM object that is not yet a real JSObject.
   `opaque` becomes the stub's class context and is owned by the stub. */
JSValue JS_NewDomStub(JSContext *ctx, uint16_t class_id, void *opaque);
/* Servo: detach the class context from its stub without freeing the stub. */
void JS_UnbindDomStub(JSContext *ctx, JSValue obj);
/* Servo: whether `v` is a JSDomStub ... */
JS_BOOL JS_IsDomStub(JSValueConst v);
```

注意 `JSDomStub.class_id` 是 `uint16_t`，所以头文件必须写 `uint16_t` 而不是 `JSClassID`
（`JSClassID` 是 `uint32_t`）。Rust 调用侧相应写 `id as u16`，并可加 `debug_assert!`。

### 2. 重新生成原始绑定

```sh
cd servo-quickjs
LIBCLANG_PATH=/usr/lib/llvm-21/lib CARGO_TARGET_DIR="$PWD/target" cargo run -p gen-bindings
```

- `gen-bindings` 的 allowlist 是 `(__)?JS_.*|js_.*`，`quickjs.h` 中所有 `JS_` 前缀函数都会进来。
- **不要手改 `servo-quickjs/servo-quickjs/src/bindings.rs`**，它是生成物。
- 生成后用 grep 确认签名符合预期：

```sh
grep -n -A3 'JS_IsDomStub' servo-quickjs/servo-quickjs/src/bindings.rs
# pub fn JS_IsDomStub(v: JSValue) -> ::std::os::raw::c_int;
```

- 已知坑：本机 libclang / glibc 与生成该文件时不同，重新生成会顺带改写无关的
  libc/`_IO_FILE` 段落。用 `git diff --stat` 检查；能接受就带上，否则用匹配的工具链生成。

### 3. jsapi.rs：声明 shim 的 Rust 侧签名

文件：`servo-quickjs/mozjs/upstream-bindgen/jsapi.rs`，在 `pub mod root { ... }` 内、
其他 Servo 扩展（`JS_NewDomStub`、`JS_GetDomStubRefCount` 等）旁边：

```rust
extern "C" {
    pub fn JS_IsDomStub(obj: *mut JSObject) -> bool;
}
```

这一层的类型可以按 servo 调用习惯设计（例如用 `*mut JSObject` 而不是 `JSValue`），
但 **名称与参数个数必须与下一步的 engine 函数一致**。

### 4. engine 实现

文件：`servo-quickjs/mozjs/src/engine/<合适的模块>.rs`（DomStub 相关在 `class.rs`）。

- 函数必须是 `pub(crate) unsafe fn`，名称/参数个数与 jsapi.rs 一致；`build.rs` 里的
  `stub_bindings.py` 会扫描 `src/engine/**` 自动生成 `extern "C"` 转发，所以不需要手写。
- 用 `q::`（= `servo_quickjs`）调用第 2 步生成的原始函数，用 `obj_val`/`val_obj`
  在两个世界之间转换；`root::` 前缀是 mozjs sys 类型。
- **指针要先判空**：`obj_val(null)` 会造出 tag 为 OBJECT 的空指针，C 侧会解引用。

```rust
/// Whether `obj` is a `JSDomStub` that has not been materialized into a real `JSObject` yet.
pub(crate) unsafe fn JS_IsDomStub(obj: *mut root::JSObject) -> bool {
    !obj.is_null() && q::JS_IsDomStub(obj_val(obj)) != 0
}
```

`JS_BOOL` 在 bindings 里是 `c_int`，在 engine 边界转成 `bool`。

### 5. wrap!：生成 safe wrapper

文件：`servo-quickjs/mozjs/src/jsapi2_wrappers.in.rs`：

```rust
wrap!(jsapi: pub fn JS_IsDomStub(obj: *mut JSObject) -> bool);
```

`wrap!` 会把它放进 `js::rust::wrappers2`。`-> bool`、指针、`Handle` 等类型都支持。

### 6. servo 侧调用

```rust
#[cfg(feature = "js-quickjs")]
use js::rust::wrappers2::{
    EnterRealm, JS_GetDomStubInfo, JS_IsDomStub, /* ... */
    LeaveRealm,
};
```

**所有 shim 专用 API 的 import 和调用都必须 `#[cfg(feature = "js-quickjs")]`**：
SpiderMonkey 构建也要能编译（PR 因漏 gate 在 CI 上失败过）。用 `#[cfg(feature = "js-quickjs")] unsafe { ... }`
包住调用块。

## 验证

1. shim 自身（会跑 stub_bindings，能立刻暴露名称/参数不匹配、u16 之类的签名问题）：

   ```sh
   cd servo-quickjs && CARGO_TARGET_DIR="$PWD/target" cargo check -p mozjs
   ```

2. servo 侧：

   ```sh
   ./mach check --js-engine quickjs
   ```

   本仓库的 `.cargo/config.toml` 已全局带 `--cfg feature="js-quickjs"`，也可以直接查单个 crate。
   注意 `cargo -p` 用**包名**而不是 target 名：

   ```sh
   cargo check -p servo-script-bindings
   ```

3. 提交前 `./mach fmt`；需要时可跑 `./mach test-unit -p <package>`；正式构建
   `./mach build --js-engine quickjs`。

## 检查清单

- [ ] `quickjs.h` 声明与 `quickjs.c` 定义逐字一致，位置在相关 Servo API 旁
- [ ] `gen-bindings` 已重新运行，`bindings.rs` 中有该函数（未手改生成物）
- [ ] `jsapi.rs` 有 `extern "C"` 声明
- [ ] `engine/*.rs` 有同名字、同参数个数的实现，空指针已判空
- [ ] `jsapi2_wrappers.in.rs` 有 `wrap!(jsapi: ...)`
- [ ] servo 侧 import/调用有 `#[cfg(feature = "js-quickjs")]`
- [ ] `cargo check -p mozjs` 与 servo 侧 check 都通过
