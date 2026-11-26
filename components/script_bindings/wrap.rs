/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */
use std::ffi::c_void;
use std::ptr;

use js::JSCLASS_IS_GLOBAL;
use js::context::JSContext;
use js::gc::{HandleObject, MutableHandleObject};
use js::glue::SetProxyReservedSlot;
use js::jsapi::{JSAutoRealm, JSClass, JSObject};
use js::jsval::{PrivateValue, UndefinedValue};
#[cfg(feature = "js-quickjs")]
use js::rust::wrappers2::JS_NewDomStub;
use js::rust::wrappers2::{
    JS_InitializePropertiesFromCompatibleNativeObject, JS_NewObjectWithGivenProto, JS_WrapObject,
    NewProxyObject,
};
use js::rust::{Handle, get_context_realm, get_object_class, get_object_realm};

use crate::codegen::PrototypeList;
use crate::conversions::DOM_OBJECT_SLOT;
use crate::import::module::{JS_GetReservedSlot, JS_SetReservedSlot};
use crate::proxyhandler::ensure_expando_object;
use crate::reflector::ProtoObjectFn;
use crate::root::{DomRoot, MaybeUnreflectedDom, Root};
use crate::utils::DOM_PROTO_UNFORGEABLE_HOLDER_SLOT;
#[cfg(feature = "js-quickjs")]
use crate::utils::DOMJSClass;
use crate::{DomObject, DomTypes, MutDomObject};

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
        let dom_ptr = raw.as_ptr();
        #[cfg(feature = "js-quickjs")]
        if !config.is_proxy {
            // Record the interface class on the reflector; the shim reads it back from the node
            // pointer to find the interface's finalizer and trace hook.
            (*dom_ptr)
                .reflector()
                .set_dom_js_class(config.class.unwrap() as *const JSClass as *const DOMJSClass);
        }
        wrap_object(
            cx,
            &scope.reflector().get_jsobject(),
            given_proto,
            dom_ptr as *const c_void,
            &config,
            obj.handle_mut(),
        );
        let root = raw.reflect_with(obj.get());
        root.reflector().set_proto_id(config.prototype_id as u16);
        root
    }
}

// `wrap` is instantiated many times, so we move most of the code into this non-generic helper.
unsafe fn wrap_object(
    cx: &mut JSContext,
    scope: &HandleObject,
    given_proto: Option<js::rust::Handle<*mut JSObject>>,
    raw: *const c_void,
    config: &WrapConfig,
    mut rval: MutableHandleObject,
) {
    unsafe {
        assert!(!scope.get().is_null());
        assert!(((*get_object_class(scope.get())).flags & JSCLASS_IS_GLOBAL) != 0);
        let _ac = JSAutoRealm::new(cx.raw_cx(), scope.get());

        // Not a proxy object: either a lazily created `JSDomStub` (the common case in QuickJS),
        // or a real object created eagerly.
        if !config.is_proxy {
            let class = config.class.unwrap();

            // QuickJS, the common case: no caller-given prototype. Keep the object lazy. The
            // canonical prototype, and this interface's unforgeable holder with it, is created the
            // first time the object is handed to JS (see `Reflector::materialize_if_needed`): an
            // object that is never exposed does not create its interface's prototype.
            #[cfg(feature = "js-quickjs")]
            if given_proto.is_none() {
                rval.set(JS_NewDomStub(cx, class, raw as *mut c_void));
                assert!(!rval.is_null());
                return;
            }

            // The canonical prototype still has to exist: it holds this interface's unforgeable
            // members, even when the instance does not use it.
            rooted!(&in(cx) let mut canonical_proto = ptr::null_mut::<JSObject>());
            (config.proto_object_fn)(cx, *scope, canonical_proto.handle_mut());
            assert!(!canonical_proto.is_null());

            // A caller-given prototype (a subclassed DOM constructor, i.e.
            // `new.target.prototype`) cannot be carried by a `JSDomStub` in QuickJS: a stub is
            // always materialized with the interface's canonical prototype. Create the real
            // object now, exactly as `Reflector::materialize_if_needed` would.
            rooted!(&in(cx) let mut proto = canonical_proto.get());
            if let Some(given) = given_proto {
                proto.set(*given);
                // The proto may come from another realm (e.g. a subclassed DOM constructor called
                // across realms); make it a wrapper in this one first.
                if get_context_realm(cx.raw_cx()) != get_object_realm(proto.get()) {
                    assert!(JS_WrapObject(cx, proto.handle_mut()));
                }
            }

            rval.set(JS_NewObjectWithGivenProto(cx, class, proto.handle()));
            assert!(!rval.is_null());
            JS_SetReservedSlot(rval.get(), DOM_OBJECT_SLOT, &PrivateValue(raw));

            if config.has_legacy_unforgeable_members {
                copy_legacy_unforgeable_members(cx, rval, canonical_proto.get());
            }
            return;
        }

        // Proxies are real objects from the start and need the canonical prototype now.
        rooted!(&in(cx) let mut canonical_proto = ptr::null_mut::<JSObject>());
        (config.proto_object_fn)(cx, *scope, canonical_proto.handle_mut());
        assert!(!canonical_proto.is_null());

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
            SetProxyReservedSlot(rval.get(), 0, &PrivateValue(raw));
        }

        // A proxy cannot hold the properties itself: they go on its expando object, which the
        // proxy handler forwards to.
        if config.has_legacy_unforgeable_members {
            rooted!(&in(cx) let mut expando = ptr::null_mut::<JSObject>());
            ensure_expando_object(cx, rval.handle(), expando.handle_mut());
            copy_legacy_unforgeable_members(cx, expando.handle_mut(), canonical_proto.get());
        }
    }
}

/// Copies the interface's legacy unforgeable properties onto `target`, taking them from the
/// unforgeable holder recorded on the interface's canonical prototype. They become
/// non-configurable own properties of `target`.
unsafe fn copy_legacy_unforgeable_members(
    cx: &mut JSContext,
    target: MutableHandleObject,
    canonical_proto: *mut JSObject,
) {
    unsafe {
        let mut slot = UndefinedValue();
        JS_GetReservedSlot(
            canonical_proto,
            DOM_PROTO_UNFORGEABLE_HOLDER_SLOT,
            &mut slot,
        );
        rooted!(&in(cx) let mut unforgeable_holder = ptr::null_mut::<JSObject>());
        unforgeable_holder.handle_mut().set(slot.to_object());
        assert!(JS_InitializePropertiesFromCompatibleNativeObject(
            cx,
            target.handle(),
            unforgeable_holder.handle(),
        ));
    }
}
