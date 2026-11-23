# JSDomStub to JSObject Reference-Count Transfer

This document describes how QuickJS reference counts are transferred when a lazy
DOM binding changes from a `JSDomStub` to a real `JSObject`.

The relevant implementation is split between:

- `components/script_bindings/reflector.rs`: DOM-side materialization.
- `servo-quickjs/mozjs/src/engine/mod.rs`: autorelease-pool accounting.
- `servo-quickjs/mozjs/src/engine/class.rs`: stub creation, unbinding, and pool
  count queries.
- `servo-quickjs/mozjs/src/quickjs.rs`: test hooks around pool draining and GC.

## Object Model

Before materialization, a non-proxy DOM object is represented by a small
`JSDomStub`:

```text
Rust DOM object
  Reflector.object ──> JSDomStub
                         dom_ptr = address of the Reflector
                         realm   = owning QuickJS realm
```

The stub owns the DOM node until one of these happens:

1. The stub is finalized while still bound to the node.
2. The DOM node is transferred to a real `JSObject` and the stub is unbound.

After materialization:

```text
Rust DOM object
  Reflector.object ──> JSObject
                         DOM_OBJECT_SLOT = address of the Reflector
```

The old stub may remain allocated until all references held by the autorelease
pool are drained. After unbinding, those old stub references no longer own or
finalize the DOM node.

## Reference Count Categories

At the beginning of materialization, define:

- `k`: the stub's current total QuickJS reference count.
- `m`: the number of references to the stub currently held by the QuickJS
  autorelease pool.
- `t = k - m`: references owned by Rust or JavaScript outside the pool.

Only `t` represents persistent ownership that must move to the new `JSObject`.
The `m` pool references will be released by a later `drain_pool`; they must not
be transferred.

The pool contains references added through operations such as `adopt`,
`retain`, rooted values, and deferred releases. It is an ownership-tracking
layer, not a second independent JS reference count.

## Why the New Object Receives `t`, Not `t - 1`

The new `JSObject` is created with an initial reference count of one:

```rust
JS_NewObjectWithGivenProto(...)
```

However, that initial count is adopted into the same autorelease pool and is
released when the current native call or task drains the pool. It cannot serve
as one of the persistent references transferred from the stub.

For this reason, materialization retains the new object `t` times:

```text
new JSObject persistent count += t
old JSDomStub non-pool count   -= t
```

Using `t - 1` under-retains the new object. This can cause premature collection,
manifesting as:

- a crash in `__JS_FreeValueRT`;
- a `gc_decref_child` assertion that `ref_count > 0`;
- use-after-free during script execution or runtime destruction.

Transferring more than `t` over-retains the object. The most visible case is an
object materialized during the same native call that created it, such as
`document.createElement(...)`: the stub's creation-time pool entry is still
present, but it belongs to the stub and will be drained as a stub reference. If
it is also treated as a permanent count on the new object, the object leaks
along with everything reachable from it.

## Autorelease-Pool Accounting

Materialization needs `m`, but reading the stub's `dom_ptr` is not sufficient:

- multiple pool entries can reference the same stub;
- the stub may be unbound after the count is read;
- non-stub values must not pay for DOM-stub-specific field reads on every pool
  pop.

The implementation therefore keeps a thread-local table:

```text
DOM_STUB_POOL_REFS: HashMap<JSDomStub pointer, pool entry count>
```

The key is the stub object's pointer, not the `Reflector` address.

### Push

When a value enters the autorelease pool:

1. Check the value tag and whether it is a `JSDomStub`.
2. If it is a stub, increment the entry for its object pointer.

### Pop

When a value leaves the pool:

1. Do nothing for non-object values.
2. If the auxiliary table is empty, return before hashing the pointer.
3. Otherwise, look up the object pointer.
4. Decrement and remove the entry on a hit.
5. On a miss, continue releasing the actual QuickJS value.

The pop path does not call `JS_IsDomStub` or read stub fields.

### Unbind

`JS_UnbindDomStub` removes the stub's auxiliary table entry before clearing:

```c
stub->dom_ptr = NULL;
stub->realm = NULL;
```

Once unbound, the stub can no longer be materialized, so it no longer needs to
participate in `m` calculations. Removing the key makes subsequent pops of the
old stub a cheap miss.

This does not change the real JS reference count. Every remaining pool entry is
still released with `JS_FreeValueRT`; only the auxiliary materialization
bookkeeping is removed.

## Materialization Sequence

The current order in `Reflector::materialize_if_needed` is:

1. Return immediately if the object is not a `JSDomStub`.
2. Read `k = JS_GetDomStubRefCount(stub)`.
3. Read `m = JS_GetDomStubPoolRefCount(stub)`.
4. Compute `t = k - m` and assert that `t >= 1`.
5. Enter the realm recorded by the stub.
6. Create the interface's canonical prototype and the new `JSObject`.
7. Call `retain_object` on the new object `t` times.
8. Set `DOM_OBJECT_SLOT` and copy legacy-unforgeable properties.
9. Leave the realm.
10. Call `JS_UnbindDomStub`.
11. Call `release_object` on the old stub `t` times.
12. Rebind the `Reflector` to the new object.

Unbinding must happen before the old stub's non-pool references are released.
Otherwise, the last release can finalize the stub during cycle collection while
the new object also owns the DOM node.

## Invariants

- A proxy is never a `JSDomStub`.
- A materialized object is never treated as a stub.
- Pool-held references are never transferred to the new object.
- Every persistent non-pool reference on the old stub is replaced by one
  persistent reference on the new object.
- The old stub never finalizes a DOM node after it has been unbound.
- Removing the auxiliary pool entry does not suppress real `JS_FreeValueRT`
  calls for the old stub.

## Regression Tests

QuickJS shim tests:

- `mozjs::qjs_pool::dom_stub_pool_entries_are_counted`
- `mozjs::qjs_pool::unbind_clears_stub_pool_tracking`

Servo lazy-binding tests:

- `tests/gc/lazy-binding/12-materialize-gc.html`
- `tests/gc/lazy-binding/13-same-call-materialize-gc.html`

Test 12 verifies that a detached DOM element can be collected after ordinary
materialization.

Test 13 samples 20,000 `document.createElement("div")` results with 200
`WeakRef` probes. It forces pool drains and GC in later jobs, then requires all
probes to be collected. This catches the extra persistent count left by
materializing an object during its creating native call.
