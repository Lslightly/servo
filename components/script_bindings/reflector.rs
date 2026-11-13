/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

#![cfg_attr(crown, allow(crown::jscontext_first_arg))]

use std::cell::Cell;
#[cfg(feature = "js-quickjs")]
use std::ffi::c_void;
use std::rc::Rc;

use js::context::JSContext;
use js::jsapi::{AddAssociatedMemory, Heap, JSClass, JSObject, MemoryUse, RemoveAssociatedMemory};
#[cfg(feature = "js-quickjs")]
use js::jsapi::JS_SetReservedSlot;
#[cfg(feature = "js-quickjs")]
use js::jsval::{PrivateValue, UndefinedValue};
use js::rust::HandleObject;
#[cfg(feature = "js-quickjs")]
use js::rust::wrappers2::{
    EnterRealm, JS_GetDomStubRefCount, JS_InitializePropertiesFromCompatibleNativeObject,
    JS_NewObjectWithGivenProto, JS_UnbindDomStub, LeaveRealm,
};
use malloc_size_of_derive::MallocSizeOf;

#[cfg(feature = "js-quickjs")]
use crate::conversions::{DOM_OBJECT_SLOT, private_from_object};
use crate::conversions::DerivedFrom;
#[cfg(feature = "js-quickjs")]
use crate::import::module::JS_GetReservedSlot;
use crate::interfaces::GlobalScopeHelpers;
use crate::iterable::{Iterable, IterableIterator};
use crate::realms::enter_auto_realm;
use crate::root::{Dom, DomRoot, Root};
#[cfg(feature = "js-quickjs")]
use crate::utils::DOM_PROTO_UNFORGEABLE_HOLDER_SLOT;
use crate::{DomTypes, JSTraceable};

pub trait AssociatedMemorySize: Default {
    fn size(&self) -> usize;
}

impl AssociatedMemorySize for () {
    fn size(&self) -> usize {
        0
    }
}

#[derive(Default, MallocSizeOf)]
pub struct AssociatedMemory(Cell<usize>);

impl AssociatedMemorySize for AssociatedMemory {
    fn size(&self) -> usize {
        self.0.get()
    }
}

#[cfg(feature = "js-quickjs")]
struct LazyCache {
    proto: Cell<*mut JSObject>,
    canonical_proto: Cell<*mut JSObject>,
    /// The class the reflector was created with: the object's actual interface class, which may
    /// be more derived than the static type the object is currently handled as (e.g. an
    /// `HTMLDivElement` returned as `Element`). `None` for proxies, which have no class of their
    /// own.
    class: Option<*const JSClass>,
    /// Whether that class has `[LegacyUnforgeable]` members to copy into the object.
    has_legacy_unforgeable_members: bool,
}

#[cfg(feature = "js-quickjs")]
impl LazyCache {
    fn new(
        class: Option<*const JSClass>,
        has_legacy_unforgeable_members: bool,
    ) -> Self {
        LazyCache {
            proto: Cell::new(std::ptr::null_mut()),
            canonical_proto: Cell::new(std::ptr::null_mut()),
            class,
            has_legacy_unforgeable_members,
        }
    }
}

/// A struct to store a reference to the reflector of a DOM object.
#[derive(MallocSizeOf)]
#[cfg_attr(crown, crown::unrooted_must_root_lint::must_root)]
// If you're renaming or moving this field, update the path in plugins::reflector as well
// QuickJS: `Dom<T>` reads `object` through a `*const Reflector<()>`, whatever `T` is.
#[cfg_attr(feature = "js-quickjs", repr(C))]
pub struct Reflector<T = ()> {
    #[ignore_malloc_size_of = "defined and measured in rust-mozjs"]
    object: Heap<*mut JSObject>,
    /// Associated memory size (of rust side). Used for memory reporting to SM.
    size: T,
    /// Cached prototype ID for fast type checks.
    proto_id: Cell<u16>,
    /// QuickJS: whether `object` holds a count, see `hold_object`.
    #[cfg(feature = "js-quickjs")]
    counted: Cell<bool>,
    /// QuickJS: the lazy-wrap cache. `Some` while `object` is a `JSDomStub` that has not been
    /// materialized into a real `JSObject` yet; cleared once the object is bound to a real
    /// `JSObject` (see [`Self::materialize_if_needed`]).
    #[cfg(feature = "js-quickjs")]
    #[ignore_malloc_size_of = "raw JS object pointers, owned by the JS engine"]
    proto_cache: Cell<Option<LazyCache>>,
}

unsafe impl<T> js::gc::Traceable for Reflector<T> {
    unsafe fn trace(&self, tracer: *mut js::jsapi::JSTracer) {
        // QuickJS: only a counted object may be marked.
        #[cfg(feature = "js-quickjs")]
        if !self.counted.get() {
            return;
        }
        unsafe {
            self.object.trace(tracer);
        }
    }
}

/// QuickJS: an uncounted object pointer must not go through the `Heap` drop barrier. This matters
/// for reflectors owned by Rust (e.g. a `RootedPromise`'s) rather than by their object; a counted
/// one (`hold_object`) releases its count through the barrier.
#[cfg(feature = "js-quickjs")]
impl<T> Drop for Reflector<T> {
    fn drop(&mut self) {
        crate::root::quickjs_refs::unregister(self as *const Self as *const Reflector);
        if !self.counted.get() {
            unsafe { *self.object.get_unsafe() = std::ptr::null_mut() };
        }
    }
}

impl<T> PartialEq for Reflector<T> {
    fn eq(&self, other: &Reflector<T>) -> bool {
        self.object.get() == other.object.get()
    }
}

impl<T> Reflector<T> {
    /// Get the object the reflector points at, materializing it first if it is still a lazy
    /// `JSDomStub`, so that it can be handed to the engine, which reads it as a regular
    /// `JSObject`. Paths that must not allocate, or that must see the object as it currently is
    /// (GC tracing, finalization, reference counting — a stub's count carries over to the
    /// materialized object), read the pointer through [`Self::rootable`] instead.
    #[inline]
    pub fn get_jsobject(&self) -> HandleObject<'_> {
        #[cfg(feature = "js-quickjs")]
        unsafe {
            self.materialize_if_needed();
        }
        // We're rooted, so it's safe to hand out a handle to object in Heap
        unsafe { HandleObject::from_raw(self.object.handle()) }
    }

    /// Return a raw pointer to the DOM object that owns this `Reflector`.
    ///
    /// By Servo convention, every DOM struct has its `Reflector` as its first field, so the
    /// address of the `Reflector` is also the address of the surrounding DOM struct. This is the
    /// pointer stored in `DOM_OBJECT_SLOT` of the corresponding JS object.
    ///
    /// Unlike [`Self::get_jsobject`], this does not change when a `JSDomStub` is materialized into
    /// a real `JSObject`, so it can be used as a stable node identity (e.g. for `OpaqueNode`).
    #[cfg(feature = "js-quickjs")]
    #[inline]
    pub fn get_dom_obj_ptr(&self) -> *mut c_void {
        self as *const Reflector<T> as *mut c_void
    }

    /// Get the cached prototype ID.
    #[inline]
    pub fn proto_id(&self) -> u16 {
        self.proto_id.get()
    }

    /// Set the cached prototype ID.
    #[inline]
    pub fn set_proto_id(&self, id: u16) {
        self.proto_id.set(id);
    }

    /// Initialize the reflector. (May be called only once.)
    ///
    /// # Safety
    ///
    /// The provided [`JSObject`] pointer must point to a valid [`JSObject`].
    unsafe fn set_jsobject(&self, object: *mut JSObject) {
        assert!(self.object.get().is_null());
        assert!(!object.is_null());
        // QuickJS: the reflector does not hold a count on its own object (that would be a
        // self-cycle only the cycle collector could break), so bypass the write barrier.
        #[cfg(feature = "js-quickjs")]
        unsafe {
            *self.object.get_unsafe() = object;
            crate::root::quickjs_refs::register(self as *const Self as *const Reflector, object);
        }
        #[cfg(not(feature = "js-quickjs"))]
        self.object.set(object);
    }

    /// QuickJS: makes the reflector hold a count on its object, which tracing marks and dropping
    /// releases. For reflectors owned by Rust (a `TracedPromise`'s); the reflector of a DOM object
    /// holds none on its own object.
    ///
    /// # Safety
    /// The reflector must be initialized and must not move afterwards.
    #[cfg(feature = "js-quickjs")]
    pub unsafe fn hold_object(&self) {
        unsafe { js::quickjs::retain_object(self.object.get()) };
        self.counted.set(true);
    }

    /// QuickJS: clears the uncounted object pointer before the reflector is dropped.
    #[cfg(feature = "js-quickjs")]
    pub(crate) unsafe fn clear_for_finalize(&self) {
        unsafe { *self.object.get_unsafe() = std::ptr::null_mut() };
    }

    /// QuickJS: replaces the object the reflector points at, e.g. when a `JSDomStub` is
    /// materialized into a real `JSObject`. The old object must already have been freed or
    /// otherwise made unreachable.
    ///
    /// # Safety
    ///
    /// The provided [`JSObject`] pointer must point to a valid [`JSObject`].
    #[cfg(feature = "js-quickjs")]
    pub(crate) unsafe fn rebind_jsobject(&self, object: *mut JSObject) {
        assert!(!self.object.get().is_null(), "Reflector not initialized");
        assert!(!object.is_null());
        assert!(!self.counted.get(), "Cannot rebind a counted reflector");
        unsafe {
            *self.object.get_unsafe() = object;
            crate::root::quickjs_refs::register(self as *const Self as *const Reflector, object);
        }
    }

    /// Return a pointer to the memory location at which the JS reflector
    /// object is stored. Used to root the reflector, as
    /// required by the JSAPI rooting APIs.
    pub fn rootable(&self) -> &Heap<*mut JSObject> {
        &self.object
    }

    /// Whether the object is backed by a real `JSObject`.
    ///
    /// QuickJS: this is true once the lazy cache has been cleared, i.e. after materializing a
    /// `JSDomStub`. Proxies cache their wrap info as well, so callers that care about proxies
    /// must check `is_proxy` too (see [`Self::materialize_if_needed`]).
    pub fn is_js_binded(&self) -> bool {
        #[cfg(feature = "js-quickjs")]
        {
            unsafe { (*self.proto_cache.as_ptr()).is_none() }
        }
        #[cfg(not(feature = "js-quickjs"))]
        {
            true
        }
    }

    /// Cache the prototype, canonical prototype and wrap info of this DOM object, for lazily
    /// creating its real `JSObject` later.
    pub fn cache_proto(
        &self,
        proto: HandleObject<'_>,
        canonical_proto: HandleObject<'_>,
        class: Option<&'static JSClass>,
        has_legacy_unforgeable_members: bool,
    ) {
        #[cfg(feature = "js-quickjs")]
        {
            let cache = LazyCache::new(
                class.map(|class| class as *const JSClass),
                has_legacy_unforgeable_members,
            );
            cache.proto.set(proto.get());
            cache.canonical_proto.set(canonical_proto.get());
            self.proto_cache.set(Some(cache));
        }
        #[cfg(not(feature = "js-quickjs"))]
        {
            let _ = (proto, canonical_proto, class, has_legacy_unforgeable_members);
        }
    }

    /// QuickJS: drops the cached prototypes once `object` is a real `JSObject`.
    #[cfg(feature = "js-quickjs")]
    pub(crate) fn clear_proto_cache(&self) {
        self.proto_cache.set(None);
    }

    /// Return a handle to the cached prototype. The handle points into the reflector, so it stays
    /// valid as long as the reflector does.
    pub fn get_proto(&self) -> HandleObject<'_> {
        #[cfg(feature = "js-quickjs")]
        {
            let cache = unsafe { (*self.proto_cache.as_ptr()).as_ref() }
                .expect("Lazy cache not set");
            let cell = &cache.proto;
            assert!(!unsafe { *cell.as_ptr() }.is_null(), "Lazy cache not set");
            unsafe { HandleObject::from_marked_location(cell.as_ptr()) }
        }
        #[cfg(not(feature = "js-quickjs"))]
        {
            unimplemented!()
        }
    }

    /// Return a handle to the cached canonical prototype.
    pub fn get_canonical_proto(&self) -> HandleObject<'_> {
        #[cfg(feature = "js-quickjs")]
        {
            let cache = unsafe { (*self.proto_cache.as_ptr()).as_ref() }
                .expect("Lazy cache not set");
            let cell = &cache.canonical_proto;
            assert!(!unsafe { *cell.as_ptr() }.is_null(), "Lazy cache not set");
            unsafe { HandleObject::from_marked_location(cell.as_ptr()) }
        }
        #[cfg(not(feature = "js-quickjs"))]
        {
            unimplemented!()
        }
    }

    /// QuickJS: the class the reflector was created with. Panics for proxies, which have none.
    #[cfg(feature = "js-quickjs")]
    pub(crate) fn get_class(&self) -> *const JSClass {
        unsafe { (*self.proto_cache.as_ptr()).as_ref() }
            .expect("Lazy cache not set")
            .class
            .expect("Proxy has no class")
    }

    /// QuickJS: whether the reflector's class has `[LegacyUnforgeable]` members.
    #[cfg(feature = "js-quickjs")]
    pub(crate) fn has_legacy_unforgeable_members(&self) -> bool {
        unsafe { (*self.proto_cache.as_ptr()).as_ref() }
            .expect("Lazy cache not set")
            .has_legacy_unforgeable_members
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
    #[cfg(feature = "js-quickjs")]
    pub(crate) unsafe fn materialize_if_needed(&self) {
        // Proxies are backed by a real `JSObject` from the start and must not be materialized. An
        // object that has already been materialized has had its cache cleared.
        if self.is_js_binded() {
            return;
        }

        unsafe {
            let cx = &mut JSContext::get_from_thread().expect("JS runtime has shut down");
            rooted!(&in(cx) let mut rval = std::ptr::null_mut::<JSObject>());

            let proto = self.get_proto();
            let canonical_proto = self.get_canonical_proto();
            let class = self.get_class();
            let has_legacy_unforgeable_members = self.has_legacy_unforgeable_members();

            // Detach the DOM node from the old JSDomStub: the stub may still be referenced from the
            // pool, so it is freed when the pool drains; by then it must not finalize the node.
            // The pointer is read through the `object` field directly, not `get_jsobject`: this
            // function is what `get_jsobject` calls to materialize, so reading through it would
            // recurse.
            let stub = self.object.get();
            let dom_ptr = private_from_object(stub);
            assert!(!dom_ptr.is_null(), "JSDomStub without a DOM object");
            let ref_count = JS_GetDomStubRefCount(stub);
            JS_UnbindDomStub(cx, stub);

            // Materialization can be triggered from any realm (e.g. the top window touching a node
            // of an iframe). The shim records `cx.current_realm()` in the new object's
            // `ObjectData`, and `GlobalScope::from_object` reads it back through
            // `GetNonCCWObjectGlobal`, so the object must be created in the realm it belongs to
            // rather than in the caller's realm. The cached canonical prototype is in that realm:
            // it was created when the DOM object was wrapped, inside the `JSAutoRealm` of its
            // global.
            let old_realm = EnterRealm(cx, canonical_proto.get());
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
                rooted!(&in(cx) let mut unforgeable_holder = std::ptr::null_mut::<JSObject>());
                unforgeable_holder.handle_mut().set(slot.to_object());
                assert!(copy_fn(cx, rval.handle(), unforgeable_holder.handle()));
            }

            LeaveRealm(cx, old_realm);
            self.rebind_jsobject(rval.get());
            self.clear_proto_cache();
        }
    }
}

impl<T: AssociatedMemorySize> Reflector<T> {
    /// Create an uninitialized `Reflector`.
    // These are used by the bindings and do not need `default()` functions.
    #[expect(clippy::new_without_default)]
    pub fn new() -> Reflector<T> {
        Reflector {
            object: Heap::default(),
            proto_id: Cell::new(u16::MAX),
            size: T::default(),
            #[cfg(feature = "js-quickjs")]
            counted: Cell::new(false),
            #[cfg(feature = "js-quickjs")]
            proto_cache: Cell::new(None),
        }
    }

    pub fn rust_size<D>(&self, _: &D) -> usize {
        size_of::<D>() + size_of::<Box<D>>() + self.size.size()
    }

    /// This function should be called from finalize of the DOM objects
    pub fn drop_memory<D>(&self, d: &D) {
        unsafe {
            RemoveAssociatedMemory(self.object.get(), self.rust_size(d), MemoryUse::DOMBinding);
        }
    }
}

impl Reflector<AssociatedMemory> {
    /// Update the associated memory size.
    pub fn update_memory_size<D>(&self, d: &D, new_size: usize) {
        if self.size.size() == new_size {
            return;
        }
        unsafe {
            RemoveAssociatedMemory(self.object.get(), self.rust_size(d), MemoryUse::DOMBinding);
            self.size.0.set(new_size);
            AddAssociatedMemory(self.object.get(), self.rust_size(d), MemoryUse::DOMBinding);
        }
    }
}

/// A trait to provide access to the `Reflector` for a DOM object.
pub trait DomObject: js::gc::Traceable + 'static {
    type ReflectorType: AssociatedMemorySize;
    /// Returns the receiver's reflector.
    fn reflector(&self) -> &Reflector<Self::ReflectorType>;
}

impl DomObject for Reflector<()> {
    type ReflectorType = ();

    fn reflector(&self) -> &Reflector<Self::ReflectorType> {
        self
    }
}

impl DomObject for Reflector<AssociatedMemory> {
    type ReflectorType = AssociatedMemory;

    fn reflector(&self) -> &Reflector<Self::ReflectorType> {
        self
    }
}

/// A trait to initialize the `Reflector` for a DOM object.
pub trait MutDomObject: DomObject {
    /// Initializes the Reflector
    ///
    /// # Safety
    ///
    /// The provided [`JSObject`] pointer must point to a valid [`JSObject`].
    /// The provided [`JSObject`] pointer must not be allocated in the nursery.
    unsafe fn init_reflector<D>(&self, obj: *mut JSObject);

    /// Initializes the Reflector without recording any associated memory usage.
    ///
    /// # Safety
    ///
    /// The provided [`JSObject`] pointer must point to a valid [`JSObject`].
    unsafe fn init_reflector_without_associated_memory(&self, obj: *mut JSObject);
}

impl MutDomObject for Reflector<()> {
    unsafe fn init_reflector<D>(&self, obj: *mut JSObject) {
        unsafe {
            js::jsapi::AddAssociatedMemory(
                obj,
                size_of::<D>() + size_of::<Box<D>>(),
                MemoryUse::DOMBinding,
            );
            self.init_reflector_without_associated_memory(obj);
        }
    }

    unsafe fn init_reflector_without_associated_memory(&self, obj: *mut JSObject) {
        unsafe {
            self.set_jsobject(obj);
        }
    }
}

impl MutDomObject for Reflector<AssociatedMemory> {
    unsafe fn init_reflector<D>(&self, obj: *mut JSObject) {
        unsafe {
            js::jsapi::AddAssociatedMemory(
                obj,
                size_of::<D>() + size_of::<Box<D>>(),
                MemoryUse::DOMBinding,
            );
            self.init_reflector_without_associated_memory(obj);
        }
    }

    unsafe fn init_reflector_without_associated_memory(&self, obj: *mut JSObject) {
        unsafe {
            self.set_jsobject(obj);
        }
    }
}

pub trait DomGlobalGeneric<D: DomTypes>: DomObject {
    /// Returns the [`GlobalScope`] of the realm that the [`DomObject`] was created in.  If this
    /// object is a `Node`, this will be different from it's owning `Document` if adopted by. For
    /// `Node`s it's almost always better to use `NodeTraits::owning_global`.
    fn global_from_reflector(&self) -> DomRoot<D::GlobalScope>
    where
        Self: Sized,
    {
        // SAFETY: We only use this `cx` to enter a realm. That does not
        // incur a GC and hence is safe to perform. We do not want to
        // pass a `cx` as parameter to this function, as this used in
        // loads of places. At the same time, it also isn't necessary in
        // nearly all cases to enter realm, since we are already in the
        // correct realm.
        //
        // However, there are cases where it is difficult to ensure that
        // we are in the correct realm. Hence we always enter a realm here
        // even if that is unnecessary at times.
        let cx = unsafe { JSContext::get_from_thread() };
        let cx = &mut cx.expect("JS runtime has shut down");
        let _realm = enter_auto_realm::<D>(cx, self);
        D::GlobalScope::from_reflector(self)
    }
}

impl<D: DomTypes, T: DomObject> DomGlobalGeneric<D> for T {}

/// A trait to provide a function pointer to wrap function for DOM objects.
pub trait DomObjectWrap<D: DomTypes>: Sized + DomObject + DomGlobalGeneric<D> {
    /// Function pointer to the general wrap function type
    #[expect(clippy::type_complexity)]
    const WRAP: unsafe fn(
        &mut JSContext,
        &D::GlobalScope,
        Option<HandleObject>,
        Box<Self>,
    ) -> Root<Dom<Self>>;
}

/// A trait to provide a function pointer to wrap function for DOM objects.
pub trait WeakReferenceableDomObjectWrap<D: DomTypes>:
    Sized + DomObject + DomGlobalGeneric<D>
{
    /// Function pointer to the general wrap function type
    #[expect(clippy::type_complexity)]
    const WRAP: unsafe fn(
        &mut JSContext,
        &D::GlobalScope,
        Option<HandleObject>,
        Rc<Self>,
    ) -> Root<Dom<Self>>;
}

/// A trait to provide a function pointer to wrap function for
/// DOM iterator interfaces.
pub trait DomObjectIteratorWrap<D: DomTypes>: DomObjectWrap<D> + JSTraceable + Iterable {
    /// Function pointer to the wrap function for `IterableIterator<T>`
    #[expect(clippy::type_complexity)]
    const ITER_WRAP: unsafe fn(
        &mut JSContext,
        &D::GlobalScope,
        Option<HandleObject>,
        Box<IterableIterator<D, Self>>,
    ) -> Root<Dom<IterableIterator<D, Self>>>;
}

/// Create the reflector for a new DOM object and yield ownership to the
/// reflector.
pub fn reflect_dom_object<D, T, U>(cx: &mut JSContext, obj: Box<T>, global: &U) -> DomRoot<T>
where
    D: DomTypes,
    T: DomObject + DomObjectWrap<D>,
    U: DerivedFrom<D::GlobalScope>,
{
    let global_scope = global.upcast();
    unsafe { T::WRAP(cx, global_scope, None, obj) }
}

pub fn reflect_dom_object_with_proto<D, T, U>(
    cx: &mut JSContext,
    obj: Box<T>,
    global: &U,
    proto: Option<HandleObject>,
) -> DomRoot<T>
where
    D: DomTypes,
    T: DomObject + DomObjectWrap<D>,
    U: DerivedFrom<D::GlobalScope>,
{
    let global_scope = global.upcast();
    unsafe { T::WRAP(cx, global_scope, proto, obj) }
}

/// Create the reflector for a new DOM object and yield ownership to the
/// reflector.
pub fn reflect_weak_referenceable_dom_object<D, T, U>(
    cx: &mut JSContext,
    obj: Rc<T>,
    global: &U,
) -> DomRoot<T>
where
    D: DomTypes,
    T: DomObject + WeakReferenceableDomObjectWrap<D>,
    U: DerivedFrom<D::GlobalScope>,
{
    let global_scope = global.upcast();
    unsafe { T::WRAP(cx, global_scope, None, obj) }
}

pub fn reflect_weak_referenceable_dom_object_with_proto<D, T, U>(
    cx: &mut JSContext,
    obj: Rc<T>,
    global: &U,
    proto: Option<HandleObject>,
) -> DomRoot<T>
where
    D: DomTypes,
    T: DomObject + WeakReferenceableDomObjectWrap<D>,
    U: DerivedFrom<D::GlobalScope>,
{
    let global_scope = global.upcast();
    unsafe { T::WRAP(cx, global_scope, proto, obj) }
}

type WrapFn<D, AbstractType> = unsafe fn(
    &mut JSContext,
    &<D as DomTypes>::GlobalScope,
    Option<HandleObject>,
    Box<AbstractType>,
) -> DomRoot<AbstractType>;

/// Create the reflector for a new DOM object and yield ownership to the
/// reflector.
pub fn reflect_dom_object_with_proto_and_wrap<D, AbstractType, GlobalType>(
    cx: &mut JSContext,
    obj: Box<AbstractType>,
    global: &GlobalType,
    proto: Option<HandleObject>,
    wrap: WrapFn<D, AbstractType>,
) -> DomRoot<AbstractType>
where
    D: DomTypes,
    AbstractType: DomObject,
    GlobalType: DerivedFrom<D::GlobalScope>,
    Box<AbstractType>: From<Box<AbstractType>>,
    DomRoot<AbstractType>: From<DomRoot<AbstractType>>,
{
    let global_scope = global.upcast();
    unsafe { wrap(cx, global_scope, proto, obj) }
}

/// Create the reflector for a new DOM object and yield ownership to the
/// reflector.
pub fn reflect_dom_object_with_wrap<D, AbstractType, GlobalType>(
    cx: &mut JSContext,
    obj: Box<AbstractType>,
    global: &GlobalType,
    wrap: WrapFn<D, AbstractType>,
) -> DomRoot<AbstractType>
where
    D: DomTypes,
    AbstractType: DomObject,
    GlobalType: DerivedFrom<D::GlobalScope>,
    Box<AbstractType>: From<Box<AbstractType>>,
    DomRoot<AbstractType>: From<DomRoot<AbstractType>>,
{
    let global_scope = global.upcast();
    unsafe { wrap(cx, global_scope, None, obj) }
}

type WrapFnRc<D, AbstractType> = unsafe fn(
    &mut JSContext,
    &<D as DomTypes>::GlobalScope,
    Option<HandleObject>,
    Rc<AbstractType>,
) -> DomRoot<AbstractType>;

/// Create the reflector for a new DOM object and yield ownership to the
/// reflector.
pub fn reflect_weak_referenceable_dom_object_with_wrap<D, AbstractType, GlobalType>(
    cx: &mut JSContext,
    obj: Rc<AbstractType>,
    global: &GlobalType,
    wrap: WrapFnRc<D, AbstractType>,
) -> DomRoot<AbstractType>
where
    D: DomTypes,
    AbstractType: DomObject,
    GlobalType: DerivedFrom<D::GlobalScope>,
    Rc<AbstractType>: From<Rc<AbstractType>>,
    DomRoot<AbstractType>: From<DomRoot<AbstractType>>,
{
    let global_scope = global.upcast();
    unsafe { wrap(cx, global_scope, None, obj) }
}
