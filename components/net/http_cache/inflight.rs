/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Coordination between concurrent fetches of the same resource.
//!
//! Exactly one fetch per key writes to the store. Once its response turns out to
//! be fresh, or its whole body has arrived, the others wait for it to resolve and
//! then read the committed entry, or go to the network if it aborted or is
//! suspended. Before that they go to the network at once, since the response may
//! need validation anyway, and so do fetches whose request the response does not
//! vary for. No fetch waits longer than [`WAIT_TIMEOUT`], or once it is cancelled.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use http::HeaderMap;
use http_cache_rules::VaryKey;
use log::debug;
use rustc_hash::{FxHashMap, FxHashSet};
use tokio::sync::watch;

use crate::fetch::methods::FetchFlowState;
use crate::http_cache::key::{CacheKey, EntryId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InFlightState {
    /// A fetch will write this entry, but its response has not arrived, or will
    /// need validation before use, so waiting for it saves nothing.
    NotShared,
    /// A fetch is writing a fresh response to this entry.
    Writing,
    /// The writing fetch received the whole body, and the store is committing it.
    Committing,
    /// The entry was committed and can be read.
    Committed(EntryId),
    /// The entry will not appear: the response was not storable, the fetch
    /// failed, or it was cancelled.
    Aborted,
}

/// How long a fetch waits for another fetch's write before it goes to the network
/// itself, so that a stalled or trickling response cannot hold up every fetch of
/// its URL.
const WAIT_TIMEOUT: Duration = Duration::from_secs(5);

/// The set of keys currently being written.
pub(crate) struct InFlight {
    writers: Mutex<FxHashMap<CacheKey, Writer>>,
    /// [`WAIT_TIMEOUT`] in milliseconds, which tests shorten.
    wait_timeout_ms: AtomicU64,
}

impl Default for InFlight {
    fn default() -> Self {
        Self {
            writers: Default::default(),
            wait_timeout_ms: AtomicU64::new(WAIT_TIMEOUT.as_millis() as u64),
        }
    }
}

struct Writer {
    state: watch::Sender<InFlightState>,
    /// The flow state of the fetch that writes the entry.
    producer: watch::Receiver<FetchFlowState>,
    /// The `Vary` of the response being written, once its headers have arrived.
    vary: Arc<OnceLock<VaryKey>>,
}

/// What a fetch waits on while another one writes the key it wants.
pub(crate) struct Waiter {
    state: watch::Receiver<InFlightState>,
    producer: watch::Receiver<FetchFlowState>,
    vary: Arc<OnceLock<VaryKey>>,
    timeout: Duration,
}

impl InFlight {
    /// Become the writer for `key` with the fetch whose flow state is `producer`,
    /// or get a [`Waiter`] for the fetch that already is.
    pub(crate) fn register(
        self: &Arc<Self>,
        key: &CacheKey,
        producer: watch::Receiver<FetchFlowState>,
    ) -> Result<InFlightWriter, Waiter> {
        let mut writers = self.writers.lock().unwrap();
        if let Some(writer) = writers.get(key) {
            return Err(Waiter {
                state: writer.state.subscribe(),
                producer: writer.producer.clone(),
                vary: writer.vary.clone(),
                timeout: Duration::from_millis(self.wait_timeout_ms.load(Ordering::Relaxed)),
            });
        }
        let (state, _) = watch::channel(InFlightState::NotShared);
        let vary = Arc::new(OnceLock::new());
        writers.insert(
            key.clone(),
            Writer {
                state,
                producer,
                vary: vary.clone(),
            },
        );
        Ok(InFlightWriter {
            inflight: self.clone(),
            key: key.clone(),
            vary,
            resolved: false,
        })
    }

    #[cfg(feature = "test-util")]
    pub(crate) fn set_wait_timeout(&self, timeout: Duration) {
        self.wait_timeout_ms
            .store(timeout.as_millis() as u64, Ordering::Relaxed);
    }

    /// How many fetches wait for the one that writes `key`, or `None` if there is
    /// nothing to wait for.
    #[cfg(feature = "test-util")]
    pub(crate) fn waiter_count(&self, key: &CacheKey) -> Option<usize> {
        let writers = self.writers.lock().unwrap();
        let writer = writers.get(key)?;
        matches!(
            *writer.state.borrow(),
            InFlightState::Writing | InFlightState::Committing
        )
        .then(|| writer.state.receiver_count())
    }

    fn update(&self, key: &CacheKey, state: InFlightState) {
        if let Some(writer) = self.writers.lock().unwrap().get(key) {
            writer.state.send_replace(state);
        }
    }

    fn resolve(&self, key: &CacheKey, state: InFlightState) {
        let writer = self.writers.lock().unwrap().remove(key);
        if let Some(writer) = writer {
            // Receivers keep the last value readable after the sender is dropped.
            let _ = writer.state.send(state);
        }
    }
}

/// Wait for the fetch that writes a fresh response to a key, or that is committing
/// one, to resolve it, for a request with `request_headers` made by the fetch whose
/// flow state is `own`.
///
/// Returns [`InFlightState::NotShared`] when there is nothing to wait for: the
/// response has not arrived, needs validation, or varies on a field in which this
/// request differs; and as soon as this fetch is cancelled. Returns
/// [`InFlightState::Writing`] when the writing fetch is suspended, since it may not
/// resume for a long time, and when the wait times out.
pub(crate) async fn wait(
    waiter: Waiter,
    mut own: watch::Receiver<FetchFlowState>,
    request_headers: &HeaderMap,
) -> InFlightState {
    let Waiter {
        mut state,
        mut producer,
        vary,
        timeout,
    } = waiter;
    let deadline = tokio::time::Instant::now() + timeout;
    let mut producer_alive = true;
    let mut own_alive = true;
    loop {
        let current = *state.borrow_and_update();
        if !matches!(current, InFlightState::Writing | InFlightState::Committing) {
            return current;
        }
        if vary
            .get()
            .is_some_and(|vary| !vary.matches(request_headers))
        {
            return InFlightState::NotShared;
        }
        // Once the body is complete, a suspension of its fetch holds nothing up.
        let watch_producer = producer_alive && current == InFlightState::Writing;
        let changed = tokio::select! {
            changed = state.changed() => changed,
            suspended = producer.wait_for(|flow| *flow == FetchFlowState::Suspended), if watch_producer => {
                if suspended.is_ok() {
                    return InFlightState::Writing;
                }
                producer_alive = false;
                continue;
            },
            cancelled = own.wait_for(|flow| *flow == FetchFlowState::Cancelled), if own_alive => {
                if cancelled.is_ok() {
                    return InFlightState::NotShared;
                }
                own_alive = false;
                continue;
            },
            _ = tokio::time::sleep_until(deadline) => return InFlightState::Writing,
        };
        if changed.is_err() {
            // The writer was dropped without resolving, which its own `Drop` should
            // have prevented. Treat it as an abort rather than waiting forever.
            let state = *state.borrow();
            return match state {
                InFlightState::Writing | InFlightState::Committing => InFlightState::Aborted,
                state => state,
            };
        }
    }
}

/// Held by the one fetch that may write a given key. Resolves the key on drop, so
/// a panicking or cancelled fetch cannot leave waiters stuck.
pub(crate) struct InFlightWriter {
    inflight: Arc<InFlight>,
    key: CacheKey,
    vary: Arc<OnceLock<VaryKey>>,
    resolved: bool,
}

impl InFlightWriter {
    pub(crate) fn commit(mut self, id: EntryId) {
        self.resolved = true;
        self.inflight
            .resolve(&self.key, InFlightState::Committed(id));
    }

    /// The response may be stored and varies on `vary`: a fetch only waits for it
    /// if its own request matches.
    pub(crate) fn set_vary(&self, vary: VaryKey) {
        let _ = self.vary.set(vary);
    }

    /// The response is fresh: let other fetches of the key wait for the entry
    /// instead of going to the network.
    pub(crate) fn share(&self) {
        self.inflight.update(&self.key, InFlightState::Writing);
    }

    /// The whole body arrived: let other fetches of the key wait for the commit,
    /// whatever the response, since the entry is about to exist.
    pub(crate) fn committing(&self) {
        self.inflight.update(&self.key, InFlightState::Committing);
    }

    pub(crate) fn abort(mut self) {
        self.resolved = true;
        self.inflight.resolve(&self.key, InFlightState::Aborted);
    }
}

impl Drop for InFlightWriter {
    fn drop(&mut self) {
        if !self.resolved {
            debug!("in-flight cache entry for {} dropped", self.key.url());
            self.inflight.resolve(&self.key, InFlightState::Aborted);
        }
    }
}

/// Single-flight guard for `stale-while-revalidate` background refreshes, so a
/// stale entry served to many consumers is only revalidated once.
#[derive(Default)]
pub struct RevalidationGuards {
    keys: Mutex<FxHashSet<CacheKey>>,
}

impl RevalidationGuards {
    pub(crate) fn try_acquire(self: &Arc<Self>, key: &CacheKey) -> Option<RevalidationGuard> {
        if !self.keys.lock().unwrap().insert(key.clone()) {
            return None;
        }
        Some(RevalidationGuard {
            guards: self.clone(),
            key: key.clone(),
        })
    }
}

/// Held for as long as a background revalidation of one key is in flight.
pub struct RevalidationGuard {
    guards: Arc<RevalidationGuards>,
    key: CacheKey,
}

impl Drop for RevalidationGuard {
    fn drop(&mut self) {
        self.guards.keys.lock().unwrap().remove(&self.key);
    }
}
