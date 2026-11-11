/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */
use std::ffi::c_void;
use std::ptr;

use js::JSCLASS_IS_GLOBAL;
use js::context::JSContext;
use js::gc::{HandleObject, MutableHandleObject};
use js::glue::SetProxyReservedSlot;
use js::jsapi::{JS_SetReservedSlot, JSAutoRealm, JSClass, JSObject};
use js::jsval::{PrivateValue, UndefinedValue};
use js::rust::wrappers2::{
    JS_GetDomStubRefCount, JS_InitializePropertiesFromCompatibleNativeObject, JS_NewDomStub,
    JS_NewObjectWithGivenProto, JS_UnbindDomStub, JS_WrapObject, NewProxyObject,
};
use js::rust::{Handle, get_context_realm, get_object_class, get_object_realm};

use crate::codegen::PrototypeList;
use crate::conversions::{DOM_OBJECT_SLOT, private_from_object};
use crate::import::module::JS_GetReservedSlot;
use crate::proxyhandler::ensure_expando_object;
use crate::reflector::Reflector;
use crate::root::{DomRoot, MaybeUnreflectedDom, Root};
use crate::utils::DOM_PROTO_UNFORGEABLE_HOLDER_SLOT;
use crate::{DomObject, DomTypes, MutDomObject};

type ProtoObjectFn = fn(&mut js::context::JSContext, HandleObject, MutableHandleObject);

/// TODO: unforgeable is missing
pub(crate) struct WrapConfig {
    pub(crate) is_maybe_cross_origin_object: bool,
    pub(crate) is_proxy: bool,
    pub(crate) proxy_handler: Option<*const c_void>,
    pub(crate) prototype_id: PrototypeList::ID,
    pub(crate) class: Option<&'static JSClass>,
    // this function has to be more general because we do not have the correct type for globalscope.
    pub(crate) proto_object_fn: ProtoObjectFn,
    pub(crate) has_legacy_unforgeable_members: bool,
}

#[cfg_attr(crown, allow(crown::unrooted_must_root))]
pub(crate) unsafe fn wrap<T: MutDomObject, D: DomTypes>(
    cx: &mut JSContext,
    scope: &D::GlobalScope,
    given_proto: Option<js::rust::Handle<*mut JSObject>>,
    raw: Root<MaybeUnreflectedDom<T>>,
    config: WrapConfig,
) -> DomRoot<T> {
    unsafe {
        rooted!(&in(cx) let mut obj = ptr::null_mut::<JSObject>());
        wrap_object::<T, D>(
            cx,
            &scope.reflector().get_jsobject(),
            given_proto,
            &raw,
            &config,
            obj.handle_mut(),
        );
        let root = raw.reflect_with(obj.get());
        root.reflector().set_proto_id(config.prototype_id as u16);
        root
    }
}

// `wrap` is instantiated many times, so we move most of the code into this non-generic helper.
unsafe fn wrap_object<T: MutDomObject, D: DomTypes>(
    cx: &mut JSContext,
    scope: &HandleObject,
    given_proto: Option<js::rust::Handle<*mut JSObject>>,
    raw: &Root<MaybeUnreflectedDom<T>>,
    config: &WrapConfig,
    mut rval: MutableHandleObject,
) {
    unsafe {
        assert!(!scope.get().is_null());
        assert!(((*get_object_class(scope.get())).flags & JSCLASS_IS_GLOBAL) != 0);
        let _ac = JSAutoRealm::new(cx.raw_cx(), scope.get());

        rooted!(&in(cx) let mut canonical_proto = ptr::null_mut::<JSObject>());
        (config.proto_object_fn)(cx, *scope, canonical_proto.handle_mut());
        assert!(!canonical_proto.is_null());

        rooted!(&in(cx) let mut proto = ptr::null_mut::<JSObject>());
        match given_proto {
            Some(given) if !config.is_proxy => {
                proto.set(*given);
                if get_context_realm(cx.raw_cx()) != get_object_realm(*given) {
                    assert!(JS_WrapObject(cx, proto.handle_mut()));
                }
            },
            _ => proto.set(*canonical_proto),
        }

        // Cache the proto, canonical proto and wrap info in the reflector, for materializing the
        // object later. Proxies are cached too, so that `new_and_bindto_jsobject` can tell them
        // apart from an object that still needs materializing.
        (*raw.as_ptr()).reflector().cache_proto(
            proto.handle(),
            canonical_proto.handle(),
            config.class,
            config.is_proxy,
            config.has_legacy_unforgeable_members,
        );

        // not proxy object, use JS_NewDomStub to create a JSDomStub object.
        if !config.is_proxy {
            // The stub owns `ObjectData` (allocated by `JS_NewDomStub`) and, through it, the DOM
            // node until it is replaced by a real `JSObject`. Like a materialized object, the
            // stub keeps the node in `DOM_OBJECT_SLOT`.
            rval.set(JS_NewDomStub(cx, config.class.unwrap()));
            assert!(!rval.is_null());
            JS_SetReservedSlot(
                rval.get(),
                DOM_OBJECT_SLOT,
                &PrivateValue(raw.as_ptr() as *const c_void),
            );
            return;
        }

        {
            let handler: *const libc::c_void = config.proxy_handler.unwrap();

            if config.is_maybe_cross_origin_object {
                rval.set(NewProxyObject(
                    cx,
                    handler,
                    Handle::undefined(),
                    ptr::null_mut(),
                    ptr::null(),
                    true,
                ));
            } else {
                rval.set(NewProxyObject(
                    cx,
                    handler,
                    Handle::undefined(),
                    canonical_proto.get(),
                    ptr::null(),
                    false,
                ));
            };

            assert!(!rval.is_null());
            SetProxyReservedSlot(rval.get(), 0, &PrivateValue(raw.as_ptr() as *const c_void));
        }

        // From here on we copy the legacy unforgeable properties to instance which was originally in a different function.
        if config.has_legacy_unforgeable_members {
            rooted!(&in(cx) let mut expando = ptr::null_mut::<JSObject>());
            if config.is_proxy {
                ensure_expando_object(cx, rval.handle(), expando.handle_mut());
            }

            let copy_fn = JS_InitializePropertiesFromCompatibleNativeObject;

            let mut slot = UndefinedValue();
            JS_GetReservedSlot(
                canonical_proto.get(),
                DOM_PROTO_UNFORGEABLE_HOLDER_SLOT,
                &mut slot,
            );
            rooted!(&in(cx) let mut unforgeable_holder = ptr::null_mut::<JSObject>());
            unforgeable_holder.handle_mut().set(slot.to_object());
            if config.is_proxy {
                assert!(copy_fn(cx, expando.handle(), unforgeable_holder.handle()));
            } else {
                assert!(copy_fn(cx, rval.handle(), unforgeable_holder.handle()));
            }
        }
    }
}

/// Materialize the object the reflector points at, if it is still a `JSDomStub`.
///
/// The reflector's old `JSDomStub` is unbound from the DOM node: it stays allocated until the
/// pool drops the references it holds, but no longer finalizes the node. The stub's reference
/// count is carried over to the new `JSObject`. Only the object's initial reference is owned by
/// the pool; the rest are duplicated without corresponding pool entries and leak for now.
/// Afterwards the reflector points at the new `JSObject` and the cached prototypes are dropped.
///
/// The new object is created with the class, prototype and wrap info cached when the reflector
/// was created (the object's actual interface), not with those of the static type it is currently
/// handled as. E.g. `document.getElementById` returns `Element`, but the object may be an
/// `HTMLDivElement`: using `Element` would fail `this` checks for members declared on
/// subinterfaces and would finalize the object through the wrong Rust type. For the same reason
/// the DOM node is read from the stub's reserved slot rather than taken from a typed reference.
///
/// This must run before the object is handed to JS: QuickJS inspects the value as a regular
/// object in some conversions (e.g. `ResolvePromise` reads its `then` property), which a
/// `JSDomStub` cannot support.
pub(crate) unsafe fn materialize_if_needed<T>(cx: &mut JSContext, reflector: &Reflector<T>) {
    // Proxies are backed by a real `JSObject` from the start and must not be materialized. An
    // object that has already been materialized has had its cache cleared.
    if reflector.is_js_binded() {
        return;
    }
    if reflector.is_proxy() {
        return;
    }

    unsafe {
        rooted!(&in(cx) let mut rval = ptr::null_mut::<JSObject>());

        let proto = reflector.get_proto();
        let canonical_proto = reflector.get_canonical_proto();
        let class = reflector.get_class();
        let has_legacy_unforgeable_members = reflector.has_legacy_unforgeable_members();

        // Detach the DOM node from the old JSDomStub: the stub may still be referenced from the
        // pool, so it is freed when the pool drains; by then it must not finalize the node.
        let stub = reflector.get_jsobject().get();
        let dom_ptr = private_from_object(stub);
        assert!(!dom_ptr.is_null(), "JSDomStub without a DOM object");
        let ref_count = JS_GetDomStubRefCount(stub);
        JS_UnbindDomStub(cx, stub);

        // Bind to a real JSObject, carrying over the stub's reference count. `JS_NewObjectWithGivenProto`
        // creates the object with a count of 1, and the pool holds a reference for it, so duplicate it
        // `ref_count - 1` times to match the stub's count. These extra references are not backed by
        // pool entries, so they leak for now.
        rval.set(JS_NewObjectWithGivenProto(cx, class, proto));
        assert!(!rval.is_null());
        for _ in 0..ref_count {
            js::quickjs::retain_object(rval.get());
        }
        JS_SetReservedSlot(rval.get(), DOM_OBJECT_SLOT, &PrivateValue(dom_ptr));

        // From here on we copy the legacy unforgeable properties to instance which was originally in a different function.
        if has_legacy_unforgeable_members {
            let copy_fn = JS_InitializePropertiesFromCompatibleNativeObject;

            let mut slot = UndefinedValue();
            JS_GetReservedSlot(
                canonical_proto.get(),
                DOM_PROTO_UNFORGEABLE_HOLDER_SLOT,
                &mut slot,
            );
            rooted!(&in(cx) let mut unforgeable_holder = ptr::null_mut::<JSObject>());
            unforgeable_holder.handle_mut().set(slot.to_object());
            assert!(copy_fn(cx, rval.handle(), unforgeable_holder.handle()));
        }

        reflector.rebind_jsobject(rval.get());
        reflector.clear_proto_cache();
    }
}
