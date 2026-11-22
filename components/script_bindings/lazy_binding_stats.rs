/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Counters for `JSDomStub` materialization, used by the lazy-binding regression tests.
//!
//! They are thread-local: each script thread (each pipeline) has its own counts, so a page
//! never observes materializations performed for another pipeline's realm.
#![cfg(feature = "js-quickjs")]

use std::cell::Cell;
use std::sync::OnceLock;

/// Whether the counters are enabled for this process.
///
/// Reading a process environment variable on every conversion would be far more expensive than
/// the counters themselves, so it is read once. A disabled process pays only a `OnceLock` load
/// and a predictable branch per call.
#[inline]
pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("SERVO_LAZY_BINDING_STATS").is_some_and(|value| !value.is_empty())
    })
}

/// Accumulated materialization counters for the current script thread.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Every call to `Reflector::materialize_if_needed`, including no-ops on objects that were
    /// already materialized (or are proxies).
    pub calls: u64,
    /// Calls that actually turned a `JSDomStub` into a real `JSObject`.
    pub materialized: u64,
}

thread_local! {
    static STATS: Cell<Stats> = const { Cell::new(Stats { calls: 0, materialized: 0 }) };
}

#[inline]
pub(crate) fn record_call() {
    if !enabled() {
        return;
    }
    STATS.with(|stats| {
        let mut value = stats.get();
        value.calls = value.calls.wrapping_add(1);
        stats.set(value);
    });
}

#[inline]
pub(crate) fn record_materialized() {
    if !enabled() {
        return;
    }
    STATS.with(|stats| {
        let mut value = stats.get();
        value.materialized = value.materialized.wrapping_add(1);
        stats.set(value);
    });
}

pub fn stats() -> Stats {
    STATS.with(Cell::get)
}

pub fn reset() {
    STATS.with(|stats| stats.set(Stats::default()));
}
