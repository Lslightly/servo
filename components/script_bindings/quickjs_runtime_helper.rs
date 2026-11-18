/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! A `runtime` object exposed on the global object, for tests that need to poke
//! at the QuickJS engine from a page:
//!
//! ```js
//! runtime.qjs_gc();            // force a full QuickJS GC
//! runtime.qjs_refcount(div);   // the QuickJS reference count of `div`
//! runtime.drain_pool();        // release the autorelease pool now
//! runtime.drain_pool_gc();     // release the pool, then run a full GC
//! runtime.materialize_count(); // DOM objects materialized on this script thread
//! runtime.materialize_calls(); // calls to `materialize_if_needed`, including no-ops
//! runtime.reset_materialize_stats();
//! ```
//!
//! The `materialize_*` functions only exist when the process environment variable
//! `SERVO_LAZY_BINDING_STATS` is set; see `tests/gc/lazy-binding/`.
//!
//! This is a debug hook, not a web API: it is installed on every `Window` and
//! should not be relied on by pages.

use std::ffi::CStr;
use std::ptr::NonNull;

use js::context::JSContext;
use js::jsapi::{
    CallArgs, GCReason, JS_GetFunctionObject, JSContext as RawJSContext, JSPROP_ENUMERATE,
};
use js::jsval::{DoubleValue, Int32Value, JSVal, UndefinedValue};
use js::realm::AutoRealm;
use js::rust::wrappers2::{JS_DefineProperty3, JS_GC, JS_NewFunction, JS_NewPlainObject};
use js::rust::HandleObject;

/// The QuickJS reference count of the object behind `val`, or `0` for values
/// that do not carry one (numbers, strings, booleans, null, undefined).
///
/// This is the engine's own count: it includes every `Dup` on the object, i.e.
/// references from JS, from the Servo-side `Dom<T>` wrappers, and the
/// autorelease pool. It is not the number of JS variables pointing at it.
///
/// Reading a value passed to a native function sees one extra count: the VM
/// stack copy the caller made when pushing the argument, freed after the call.
/// `qjs_refcount` subtracts it; other callers must do the same if they want the
/// count at rest.
#[expect(unsafe_code)]
pub fn get_ref_count(val: &JSVal) -> i32 {
    let val = val.to_qjs();
    if !val.has_ref_count() || val.ptr().is_null() {
        return 0;
    }
    // SAFETY: `val` carries a reference count, so the count lives in the
    // `JSGCObjectHeader` its pointer points to.
    unsafe { js::qjs::JS_GetRefCount(val) }
}

#[expect(unsafe_code)]
unsafe extern "C" fn qjs_gc(cx: *mut RawJSContext, _argc: u32, _vp: *mut JSVal) -> bool {
    let mut cx = unsafe {
        // SAFETY: We are in a JS hook, so `cx` is a valid `JSContext`.
        JSContext::from_ptr(NonNull::new(cx).expect("JSContext is not null in a JS hook"))
    };
    // SAFETY: `cx` is valid; `GCReason::API` is the generic "the embedder asked" reason.
    unsafe { JS_GC(&mut cx, GCReason::API) };
    true
}

#[expect(unsafe_code)]
unsafe extern "C" fn qjs_refcount(_cx: *mut RawJSContext, argc: u32, vp: *mut JSVal) -> bool {
    // SAFETY: We are in a JS hook, and `vp`/`argc` describe its arguments.
    let args = unsafe { CallArgs::from_vp(vp, argc) };
    // Reading the argument we are handling is itself a reference: the caller's
    // `get_loc`/`get_field` duplicates the value onto the VM stack, and the
    // caller frees that copy once this function returns (`OP_call` in
    // quickjs.c, `for (i = -1; i < call_argc; i++) JS_FreeValue(...)`).
    // Subtract it so the result is the count the object has at rest.
    let count = get_ref_count(&args.get(0).get()).saturating_sub(1);
    args.rval().set(Int32Value(count));
    true
}

/// Drains the autorelease pool immediately, instead of at the end of the task.
///
/// Every value the engine handed to Rust, and every last reference Rust
/// dropped, is released now. Values the JS engine still holds (globals, locals,
/// the operand stack) keep their own references, so they survive; but a Rust
/// frame further up the stack that is holding a raw value from the pool will
/// have it freed underneath it.
#[expect(unsafe_code)]
unsafe extern "C" fn drain_pool(_cx: *mut RawJSContext, _argc: u32, _vp: *mut JSVal) -> bool {
    js::quickjs::drain_autorelease_pool();
    true
}

/// Drains the autorelease pool and then runs a full GC, as one operation, so a page can measure
/// the combined cost of both. With `DRAIN_STATS=1` the total time is recorded as one
/// `drain_pool_gc` line.
#[expect(unsafe_code)]
unsafe extern "C" fn drain_pool_gc(_cx: *mut RawJSContext, _argc: u32, _vp: *mut JSVal) -> bool {
    js::quickjs::drain_pool_and_gc();
    true
}

/// A JS number for a counter value: `Int32Value` while it fits, `DoubleValue` above that.
///
/// Both are JS Numbers (`typeof` is `"number"`); the int tag is just the compact
/// representation. Values stay exact well past anything these counters reach.
fn count_value(count: u64) -> JSVal {
    if count <= i32::MAX as u64 {
        Int32Value(count as i32)
    } else {
        DoubleValue(count as f64)
    }
}

/// Number of DOM objects actually materialized on this script thread.
#[expect(unsafe_code)]
unsafe extern "C" fn materialize_count(_cx: *mut RawJSContext, argc: u32, vp: *mut JSVal) -> bool {
    // SAFETY: We are in a JS hook, and `vp`/`argc` describe its arguments.
    let args = unsafe { CallArgs::from_vp(vp, argc) };
    let stats = crate::lazy_binding_stats::stats();
    args.rval().set(count_value(stats.materialized));
    true
}

/// Number of calls to `materialize_if_needed` on this script thread, including no-ops.
#[expect(unsafe_code)]
unsafe extern "C" fn materialize_calls(_cx: *mut RawJSContext, argc: u32, vp: *mut JSVal) -> bool {
    // SAFETY: We are in a JS hook, and `vp`/`argc` describe its arguments.
    let args = unsafe { CallArgs::from_vp(vp, argc) };
    let stats = crate::lazy_binding_stats::stats();
    args.rval().set(count_value(stats.calls));
    true
}

/// Clears both materialization counters for this script thread.
#[expect(unsafe_code)]
unsafe extern "C" fn reset_materialize_stats(
    _cx: *mut RawJSContext,
    argc: u32,
    vp: *mut JSVal,
) -> bool {
    crate::lazy_binding_stats::reset();
    // SAFETY: We are in a JS hook, and `vp`/`argc` describe its arguments.
    let args = unsafe { CallArgs::from_vp(vp, argc) };
    args.rval().set(UndefinedValue());
    true
}

/// Defines `name` on `obj` as a native function calling `call`.
#[expect(unsafe_code)]
unsafe fn define_method(
    cx: &mut JSContext,
    obj: HandleObject,
    name: &CStr,
    call: unsafe extern "C" fn(*mut RawJSContext, u32, *mut JSVal) -> bool,
) {
    // SAFETY: `cx` is in the realm of `obj`, and every pointer checked below
    // comes from the engine.
    unsafe {
        let function = JS_NewFunction(cx, Some(call), 0, 0, name.as_ptr());
        assert!(!function.is_null());
        let function_object = JS_GetFunctionObject(function);
        rooted!(&in(cx) let mut function_object = function_object);
        let defined = JS_DefineProperty3(
            cx,
            obj,
            name.as_ptr(),
            function_object.handle(),
            JSPROP_ENUMERATE as u32,
        );
        assert!(defined);
    }
}

/// Installs the `runtime` object on `global` (the Window's global object).
#[expect(unsafe_code)]
pub fn init_runtime_helper(cx: &mut JSContext, global: HandleObject) {
    let mut realm = AutoRealm::new_from_handle(cx, global);
    let cx = &mut realm;
    unsafe {
        rooted!(&in(cx) let mut runtime = JS_NewPlainObject(cx));
        let runtime = runtime.handle();
        define_method(cx, runtime, c"qjs_gc", qjs_gc);
        define_method(cx, runtime, c"qjs_refcount", qjs_refcount);
        define_method(cx, runtime, c"drain_pool", drain_pool);
        define_method(cx, runtime, c"drain_pool_gc", drain_pool_gc);
        // Off by default: the accessors are a test hook, not a web API, and exposing them
        // changes the properties visible on `window.runtime`.
        if crate::lazy_binding_stats::enabled() {
            define_method(cx, runtime, c"materialize_count", materialize_count);
            define_method(cx, runtime, c"materialize_calls", materialize_calls);
            define_method(
                cx,
                runtime,
                c"reset_materialize_stats",
                reset_materialize_stats,
            );
        }
        let defined = JS_DefineProperty3(
            cx,
            global,
            c"runtime".as_ptr(),
            runtime,
            JSPROP_ENUMERATE as u32,
        );
        assert!(defined);
    }
}
