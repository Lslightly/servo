/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The state of the cache and the entry points that change it.
//!
//! Locks, in the order in which they may be taken:
//!
//! 1. `Inner::handles`, held only by the thread that runs the effects, while it calls a
//!    [`FetchHandle`] method. A sink callback from inside such a call takes `state`, queues
//!    its effects and returns without running them.
//! 2. `Inner::state`, which holds everything else. It is never held while the cache calls
//!    out: fetchers, fetch handles, wakers and observers are called from the effect queue,
//!    which one thread at a time runs, in order, without `state`. File I/O happens without
//!    `state` too, on slots that are pinned meanwhile, and what may hold wakers, observers
//!    or fetchers is dropped once `state` is released.
//!
//! The clock's own lock is taken only to register timers, after `state` is released.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::Duration;

use bytes::Bytes;
use log::debug;
use parking_lot::{Mutex, MutexGuard};

use crate::clock::Clock;
use crate::resource::{
    Announced, Block, Control, Fetch, FetchId, Meta, Observer, Reader, ReaderId, Resource,
    ResourceId, ResponseOrigin, Waker,
};
use crate::schedule::{Action, ReaderView, Tier, decide, rank};
use crate::store::{BLOCK_SIZE, SlotId, Store, read_at, slot_offset, write_at};
use crate::{
    FetchError, FetchHandle, FetchRequest, FetchSink, Fetcher, ReadResult, ReaderActivity,
    ReaderError, ReaderEvent, ResourceKey, ResponseInfo,
};

const MIB: u64 = 1024 * 1024;
/// How often a reader hears of progress at most, as the `progress` event of media elements.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(350);

#[derive(Clone, Debug)]
pub(crate) struct Settings {
    pub file_budget: u64,
    pub memory_resource_limit: u64,
    pub memory_fallback: u64,
    pub readahead: Duration,
    pub resume: Duration,
    pub suspend_timeout: Duration,
    pub reuse: Duration,
    /// The directory of the temporary file, or the system temporary directory.
    pub directory: Option<PathBuf>,
}

impl Settings {
    pub(crate) fn from_prefs() -> Self {
        let prefs = servo_config::prefs::get();
        Self {
            file_budget: prefs.media_cache_file_budget_mb.saturating_mul(MIB),
            memory_resource_limit: prefs
                .media_cache_memory_resource_limit_kb
                .saturating_mul(1024),
            memory_fallback: prefs.media_cache_memory_fallback_mb.saturating_mul(MIB),
            readahead: Duration::from_secs(prefs.media_cache_readahead_seconds),
            resume: Duration::from_secs(prefs.media_cache_resume_seconds),
            suspend_timeout: Duration::from_secs(prefs.media_cache_suspend_seconds),
            reuse: Duration::from_secs(prefs.media_cache_reuse_seconds),
            directory: Some(&prefs.media_cache_directory)
                .filter(|directory| !directory.is_empty())
                .map(PathBuf::from),
        }
    }
}

/// A call out of the cache, queued under the lock and made without it.
enum Effect {
    Start {
        resource: ResourceId,
        fetch: FetchId,
        fetcher: Arc<dyn Fetcher>,
        request: FetchRequest,
        sink: Arc<dyn FetchSink>,
    },
    Suspend(FetchId),
    Resume(FetchId),
    Cancel(FetchId),
    /// Drops the handle of a fetch that ended.
    Forget(FetchId),
    Wake {
        resource: ResourceId,
        reader: ReaderId,
        waker: Waker,
    },
    Event {
        resource: ResourceId,
        reader: ReaderId,
        observer: Observer,
        event: ReaderEvent,
    },
}

/// A write of a block's buffer to its slot.
pub(crate) struct FlushJob {
    resource: ResourceId,
    index: u64,
    slot: SlotId,
    offset: u64,
    data: Vec<u8>,
    file: Arc<File>,
}

/// A read of part of a block from its slot, into `dest[dest_offset..dest_offset + len]`.
struct FilePart {
    dest_offset: usize,
    len: usize,
    offset: u64,
    slot: SlotId,
    index: u64,
}

enum ReadPlan {
    Done(ReadResult),
    Copied {
        len: usize,
        file: Option<Arc<File>>,
        parts: Vec<FilePart>,
    },
}

/// Work to do once `state` is released.
#[derive(Default)]
struct Deferred {
    flushes: Vec<FlushJob>,
    timers: Vec<Duration>,
    /// What may hold wakers, observers or fetchers, whose drop may run code of the embedder.
    garbage: Vec<Box<dyn Send>>,
}

pub(crate) struct State {
    pub(crate) settings: Settings,
    pub(crate) store: Store,
    pub(crate) resources: HashMap<ResourceId, Resource>,
    by_key: HashMap<ResourceKey, ResourceId>,
    effects: VecDeque<Effect>,
    /// Whether a thread is running the effects.
    draining: bool,
    deferred: Deferred,
    next_id: u64,
    now: Duration,
    weak: Weak<Inner>,
}

pub(crate) struct Inner {
    pub(crate) state: Mutex<State>,
    handles: Mutex<HashMap<FetchId, Box<dyn FetchHandle>>>,
    clock: Arc<dyn Clock>,
    weak: Weak<Inner>,
    #[cfg(test)]
    pub(crate) faults: IoFaults,
}

/// Failures and interruptions of the file I/O, for tests.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct IoFaults {
    pub fail_reads: std::sync::atomic::AtomicBool,
    pub fail_writes: std::sync::atomic::AtomicBool,
    /// Runs once, just before the next read of the file.
    pub before_read: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Runs once, just before the next write of the file.
    pub before_write: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl Inner {
    pub(crate) fn new(settings: Settings, clock: Arc<dyn Clock>) -> Arc<Self> {
        Arc::new_cyclic(|weak| Inner {
            state: Mutex::new(State {
                store: Store::new(
                    settings.file_budget,
                    settings.memory_fallback,
                    settings.directory.clone(),
                ),
                settings,
                resources: HashMap::new(),
                by_key: HashMap::new(),
                effects: VecDeque::new(),
                draining: false,
                deferred: Deferred::default(),
                next_id: 1,
                now: Duration::ZERO,
                weak: weak.clone(),
            }),
            handles: Mutex::new(HashMap::new()),
            clock,
            weak: weak.clone(),
            #[cfg(test)]
            faults: IoFaults::default(),
        })
    }

    fn read_file(&self, file: &File, dest: &mut [u8], offset: u64) -> std::io::Result<()> {
        #[cfg(test)]
        {
            let hook = self.faults.before_read.lock().take();
            if let Some(hook) = hook {
                hook();
            }
            if self
                .faults
                .fail_reads
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                return Err(std::io::Error::other("injected read error"));
            }
        }
        read_at(file, dest, offset)
    }

    fn write_file(&self, file: &File, data: &[u8], offset: u64) -> std::io::Result<()> {
        #[cfg(test)]
        {
            let hook = self.faults.before_write.lock().take();
            if let Some(hook) = hook {
                hook();
            }
            if self
                .faults
                .fail_writes
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                return Err(std::io::Error::other("injected write error"));
            }
        }
        write_at(file, data, offset)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        let mut state = self.state.lock();
        state.now = self.clock.now();
        state
    }

    /// Changes the state, then does what that requires without the lock.
    pub(crate) fn update<R>(&self, change: impl FnOnce(&mut State) -> R) -> R {
        let (result, deferred) = {
            let mut state = self.lock();
            let result = change(&mut state);
            (result, std::mem::take(&mut state.deferred))
        };
        self.finish(deferred);
        result
    }

    fn finish(&self, mut deferred: Deferred) {
        let mut garbage = std::mem::take(&mut deferred.garbage);
        loop {
            for at in deferred.timers.drain(..) {
                let weak = self.weak.clone();
                self.clock.call_at(
                    at,
                    Box::new(move || {
                        if let Some(inner) = weak.upgrade() {
                            inner.update(State::run_timers);
                        }
                    }),
                );
            }
            if deferred.flushes.is_empty() {
                break;
            }
            let results: Vec<_> = deferred
                .flushes
                .drain(..)
                .map(|job| {
                    let result = self.write_file(&job.file, &job.data, job.offset);
                    (job, result)
                })
                .collect();
            let mut state = self.lock();
            for (job, result) in results {
                state.finish_flush(job, result);
            }
            let next = std::mem::take(&mut state.deferred);
            drop(state);
            deferred = next;
            garbage.append(&mut deferred.garbage);
        }
        drop(garbage);
        self.drain();
    }

    pub(crate) fn read(
        &self,
        resource: ResourceId,
        reader: ReaderId,
        pos: u64,
        dest: &mut [u8],
    ) -> ReadResult {
        let (plan, deferred) = {
            let mut state = self.lock();
            let plan = state.read(resource, reader, pos, dest);
            (plan, std::mem::take(&mut state.deferred))
        };
        let result = match plan {
            ReadPlan::Done(result) => result,
            ReadPlan::Copied { len, file, parts } => {
                let mut valid = len;
                let mut failed = None;
                if let Some(file) = file {
                    for part in &parts {
                        let dest = &mut dest[part.dest_offset..part.dest_offset + part.len];
                        if let Err(error) = self.read_file(&file, dest, part.offset) {
                            valid = part.dest_offset;
                            failed = Some((part.index, error));
                            break;
                        }
                    }
                }
                if parts.is_empty() {
                    ReadResult::Data(len)
                } else {
                    self.update(|state| {
                        state.finish_read(resource, reader, pos, &parts, valid, failed)
                    })
                }
            },
        };
        self.finish(deferred);
        result
    }

    /// Runs the queued effects, unless another thread, or a caller further up this thread's
    /// stack, already does.
    fn drain(&self) {
        {
            let mut state = self.state.lock();
            if state.draining || state.effects.is_empty() {
                return;
            }
            state.draining = true;
        }
        let _guard = DrainGuard(self);
        loop {
            let mut stale = Vec::new();
            let effect = {
                let mut state = self.state.lock();
                loop {
                    match state.effects.pop_front() {
                        None => {
                            state.draining = false;
                            drop(state);
                            drop(stale);
                            return;
                        },
                        Some(effect) if state.is_stale(&effect) => stale.push(effect),
                        Some(effect) => break effect,
                    }
                }
            };
            drop(stale);
            self.execute(effect);
        }
    }

    fn execute(&self, effect: Effect) {
        match effect {
            Effect::Start {
                fetch,
                fetcher,
                request,
                sink,
                ..
            } => {
                let handle = fetcher.start(request, sink);
                self.handles.lock().insert(fetch, handle);
            },
            Effect::Suspend(fetch) => {
                if let Some(handle) = self.handles.lock().get(&fetch) {
                    handle.suspend();
                }
            },
            Effect::Resume(fetch) => {
                if let Some(handle) = self.handles.lock().get(&fetch) {
                    handle.resume();
                }
            },
            Effect::Cancel(fetch) => {
                let handle = self.handles.lock().remove(&fetch);
                if let Some(handle) = handle {
                    handle.cancel();
                }
            },
            Effect::Forget(fetch) => {
                let handle = self.handles.lock().remove(&fetch);
                drop(handle);
            },
            Effect::Wake { waker, .. } => waker(),
            Effect::Event {
                observer, event, ..
            } => observer(event),
        }
    }
}

/// Lets another thread run the effects if a callback panics.
struct DrainGuard<'a>(&'a Inner);

impl Drop for DrainGuard<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.state.lock().draining = false;
        }
    }
}

/// The sink of one fetch.
struct CacheSink {
    inner: Weak<Inner>,
    resource: ResourceId,
    fetch: FetchId,
}

impl CacheSink {
    fn update(&self, change: impl FnOnce(&mut State, ResourceId, FetchId)) {
        if let Some(inner) = self.inner.upgrade() {
            inner.update(|state| change(state, self.resource, self.fetch));
        }
    }
}

impl FetchSink for CacheSink {
    fn response(&self, info: ResponseInfo) {
        self.update(|state, resource, fetch| state.on_response(resource, fetch, info));
    }

    fn data(&self, bytes: Bytes) {
        self.update(|state, resource, fetch| state.on_data(resource, fetch, &bytes));
    }

    fn suspended(&self, honoured: bool) {
        self.update(|state, resource, fetch| state.on_suspended(resource, fetch, honoured));
    }

    fn end(&self, result: Result<(), FetchError>) {
        self.update(|state, resource, fetch| state.on_end(resource, fetch, result));
    }
}

impl State {
    fn new_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn is_stale(&self, effect: &Effect) -> bool {
        match effect {
            Effect::Start {
                resource, fetch, ..
            } => !self
                .resources
                .get(resource)
                .is_some_and(|res| res.fetches.iter().any(|f| f.id == *fetch)),
            Effect::Wake {
                resource, reader, ..
            } |
            Effect::Event {
                resource, reader, ..
            } => !self
                .resources
                .get(resource)
                .is_some_and(|res| res.readers.contains_key(reader)),
            _ => false,
        }
    }

    pub(crate) fn open(
        &mut self,
        key: ResourceKey,
        fetcher: Arc<dyn Fetcher>,
    ) -> (ResourceId, ReaderId) {
        self.sweep();
        let existing = self
            .by_key
            .get(&key)
            .copied()
            .filter(|id| self.resources[id].error.is_none());
        let resource = match existing {
            Some(id) => id,
            None => {
                let id = self.new_id();
                debug!("Media cache: new resource {id} for {}", key.url);
                self.resources
                    .insert(id, Resource::new(key.clone(), self.now));
                self.by_key.insert(key, id);
                id
            },
        };
        let reader = self.new_id();
        self.resources
            .get_mut(&resource)
            .expect("resource exists")
            .readers
            .insert(
                reader,
                Reader {
                    pos: 0,
                    recent: Vec::new(),
                    pending: None,
                    activity: ReaderActivity::Playing {
                        bytes_per_second: None,
                    },
                    rate: None,
                    waker: None,
                    observer: None,
                    fetcher,
                    last_progress: None,
                },
            );
        self.schedule_all();
        (resource, reader)
    }

    /// Drops the resources that no reader uses and that cannot be reused.
    fn sweep(&mut self) {
        let now = self.now;
        let reuse = self.settings.reuse;
        let expired: Vec<ResourceId> = self
            .resources
            .iter()
            .filter(|(_, res)| {
                res.readers.is_empty() &&
                    (res.error.is_some() ||
                        !res.listed ||
                        now.saturating_sub(res.last_used) > reuse)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            self.remove_resource(id);
        }
    }

    fn remove_resource(&mut self, id: ResourceId) {
        let Some(res) = self.resources.remove(&id) else {
            return;
        };
        debug!("Media cache: dropping resource {id}");
        for fetch in &res.fetches {
            self.effects.push_back(Effect::Cancel(fetch.id));
        }
        for block in res.blocks.values() {
            if let Some(slot) = block.slot {
                self.store.release(slot);
            }
        }
        if self.by_key.get(&res.key) == Some(&id) {
            self.by_key.remove(&res.key);
        }
        if res.ram_backed {
            self.update_reservation();
        }
        self.deferred.garbage.push(Box::new(res));
    }

    /// The memory that resources kept in memory may grow to.
    fn ram_reserved(&self) -> u64 {
        self.resources
            .values()
            .filter(|res| res.ram_backed && res.error.is_none())
            .filter_map(|res| res.length)
            .sum()
    }

    fn update_reservation(&mut self) {
        let reserved = self.ram_reserved();
        self.store.set_reserved(reserved);
    }

    /// Whether resource `id`, of `length` bytes, fits in memory with the other resources there,
    /// after dropping those without readers that were used least recently if needed. With the
    /// file unavailable, the memory budget is that of the fallback, and new resources use it.
    fn make_room_in_memory(&mut self, id: ResourceId, length: u64) -> bool {
        if self.store.ram_mode() {
            return false;
        }
        let unused: u64 = self
            .resources
            .iter()
            .filter(|(other, res)| {
                **other != id && res.ram_backed && res.error.is_none() && res.readers.is_empty()
            })
            .filter_map(|(_, res)| res.length)
            .sum();
        if self.ram_reserved().saturating_sub(unused) + length > self.settings.memory_fallback {
            return false;
        }
        loop {
            if self.ram_reserved() + length <= self.settings.memory_fallback {
                return true;
            }
            let unused = self
                .resources
                .iter()
                .filter(|(other, res)| **other != id && res.ram_backed && res.readers.is_empty())
                .min_by_key(|(_, res)| res.last_used)
                .map(|(other, _)| *other);
            match unused {
                Some(other) => self.remove_resource(other),
                None => return false,
            }
        }
    }

    pub(crate) fn detach(&mut self, id: ResourceId, reader: ReaderId) {
        let now = self.now;
        let Some(res) = self.resources.get_mut(&id) else {
            return;
        };
        if let Some(reader) = res.readers.remove(&reader) {
            self.deferred.garbage.push(Box::new(reader));
        }
        if res.readers.is_empty() {
            res.last_used = now;
            if res.error.is_some() || !res.listed {
                self.remove_resource(id);
            } else {
                self.deferred
                    .timers
                    .push(now + self.settings.reuse + Duration::from_millis(1));
            }
        } else {
            // A fetch must not run for a document that is gone, and a seekable resource's fetch
            // that no remaining reader gets to, such as that of a probe, is not needed any more.
            let seekable = res.seekable();
            let unneeded: Vec<FetchId> = res
                .fetches
                .iter()
                .filter(|fetch| {
                    let fetcher_alive = res.readers.values().any(|reader| {
                        std::ptr::addr_eq(Arc::as_ptr(&reader.fetcher), Arc::as_ptr(&fetch.fetcher))
                    });
                    let reached = !seekable ||
                        res.readers.values().any(|reader| {
                            !reader.is_idle() &&
                                reader
                                    .positions(now)
                                    .into_iter()
                                    .any(|pos| res.reaches(pos, fetch.pos))
                        });
                    !fetcher_alive || !reached
                })
                .map(|fetch| fetch.id)
                .collect();
            for fetch in unneeded {
                self.remove_fetch(id, fetch, true);
            }
        }
        self.schedule_all();
    }

    /// Detaches a reader that is closed while others still hold it, and returns its waker, for a
    /// pending read to learn of it.
    pub(crate) fn close(&mut self, id: ResourceId, reader: ReaderId) -> Option<Waker> {
        let waker = self
            .reader_mut(id, reader)
            .and_then(|reader| reader.waker.clone());
        self.detach(id, reader);
        waker
    }

    fn reader_mut(&mut self, id: ResourceId, reader: ReaderId) -> Option<&mut Reader> {
        self.resources.get_mut(&id)?.readers.get_mut(&reader)
    }

    pub(crate) fn set_waker(&mut self, id: ResourceId, reader: ReaderId, waker: Waker) {
        if let Some(reader) = self.reader_mut(id, reader) &&
            let Some(old) = reader.waker.replace(waker)
        {
            self.deferred.garbage.push(Box::new(old));
        }
        self.wake_readers(id);
    }

    pub(crate) fn set_activity(
        &mut self,
        id: ResourceId,
        reader: ReaderId,
        activity: ReaderActivity,
    ) {
        let Some(reader) = self.reader_mut(id, reader) else {
            return;
        };
        reader.activity = activity;
        if let ReaderActivity::Playing { bytes_per_second } |
        ReaderActivity::Paused { bytes_per_second } = activity
        {
            reader.rate = bytes_per_second.filter(|rate| rate.is_finite() && *rate > 0.);
        }
        self.schedule(id);
    }

    pub(crate) fn set_observer(&mut self, id: ResourceId, reader_id: ReaderId, observer: Observer) {
        let Some(res) = self.resources.get_mut(&id) else {
            return;
        };
        let mut events = Vec::new();
        if let Some(meta) = &res.meta {
            events.push(ReaderEvent::Metadata {
                length: res.length,
                seekable: res.seekable(),
                cors_tainted: meta.cors_tainted,
            });
        }
        if let Some(error) = &res.error {
            events.push(ReaderEvent::Error(error.clone()));
        } else {
            match res.announced {
                Announced::Loading => {},
                Announced::Suspended => events.push(ReaderEvent::DownloadSuspended),
                Announced::Complete => events.push(ReaderEvent::Complete),
            }
        }
        let Some(reader) = res.readers.get_mut(&reader_id) else {
            return;
        };
        if let Some(old) = reader.observer.replace(observer.clone()) {
            self.deferred.garbage.push(Box::new(old));
        }
        for event in events {
            self.effects.push_back(Effect::Event {
                resource: id,
                reader: reader_id,
                observer: observer.clone(),
                event,
            });
        }
    }

    pub(crate) fn length(&self, id: ResourceId) -> Option<u64> {
        self.resources.get(&id)?.length
    }

    pub(crate) fn seekable(&self, id: ResourceId) -> bool {
        self.resources.get(&id).is_some_and(Resource::seekable)
    }

    pub(crate) fn cors_tainted(&self, id: ResourceId) -> bool {
        self.resources
            .get(&id)
            .and_then(|res| res.meta.as_ref())
            .is_some_and(|meta| meta.cors_tainted)
    }

    fn read(&mut self, id: ResourceId, reader_id: ReaderId, pos: u64, dest: &mut [u8]) -> ReadPlan {
        let now = self.now;
        let file = self.store.file();
        let Some(res) = self
            .resources
            .get_mut(&id)
            .filter(|res| res.readers.contains_key(&reader_id))
        else {
            return ReadPlan::Done(ReadResult::Error(ReaderError::Network));
        };
        if let Some(error) = &res.error {
            return ReadPlan::Done(ReadResult::Error(error.clone()));
        }
        let end = res.end();
        if pos >= end {
            if let Some(reader) = res.readers.get_mut(&reader_id) {
                reader.pos = pos;
            }
            return ReadPlan::Done(ReadResult::EndOfStream);
        }
        if dest.is_empty() {
            return ReadPlan::Done(ReadResult::Data(0));
        }
        let limit = (end - pos).min(dest.len() as u64) as usize;
        let metadata = res
            .readers
            .get(&reader_id)
            .is_some_and(|reader| reader.activity == ReaderActivity::Metadata);
        let mut len = 0;
        let mut parts = Vec::new();
        while len < limit {
            let at = pos + len as u64;
            let index = at / BLOCK_SIZE;
            let offset = (at % BLOCK_SIZE) as usize;
            let Some(block) = res.blocks.get_mut(&index) else {
                break;
            };
            if offset >= block.len() {
                break;
            }
            let count = (block.len() - offset).min(limit - len);
            let in_file = block.file_len.saturating_sub(offset).min(count);
            if in_file > 0 {
                let slot = block.slot.expect("a block in the file has a slot");
                self.store.pin(slot);
                parts.push(FilePart {
                    dest_offset: len,
                    len: in_file,
                    offset: slot_offset(slot) + offset as u64,
                    slot,
                    index,
                });
            }
            if count > in_file {
                let ram_offset = offset + in_file - block.file_len;
                dest[len + in_file..len + count]
                    .copy_from_slice(&block.ram[ram_offset..ram_offset + count - in_file]);
            }
            block.last_use = now;
            block.metadata = metadata;
            len += count;
        }
        let Some(reader) = res.readers.get_mut(&reader_id) else {
            return ReadPlan::Done(ReadResult::Error(ReaderError::Network));
        };
        let old_block = reader.pos / BLOCK_SIZE;
        if len == 0 {
            let repeated = reader.pending == Some(pos) && reader.pos == pos;
            reader.note_read(pos, pos, now);
            reader.pending = Some(pos);
            if !repeated {
                self.schedule(id);
            }
            return ReadPlan::Done(ReadResult::Pending);
        }
        reader.note_read(pos, pos + len as u64, now);
        if reader
            .pending
            .is_some_and(|pending| (pos..pos + len as u64).contains(&pending))
        {
            reader.pending = None;
        }
        if reader.pos / BLOCK_SIZE != old_block {
            self.schedule(id);
        }
        if parts.is_empty() {
            return ReadPlan::Done(ReadResult::Data(len));
        }
        ReadPlan::Copied { len, file, parts }
    }

    fn finish_read(
        &mut self,
        id: ResourceId,
        reader_id: ReaderId,
        pos: u64,
        parts: &[FilePart],
        valid: usize,
        failed: Option<(u64, std::io::Error)>,
    ) -> ReadResult {
        for part in parts {
            self.store.unpin(part.slot);
        }
        let Some((index, error)) = failed else {
            return ReadResult::Data(valid);
        };
        // The block is lost; as for a write error, keep the next ones in memory, so that a
        // refetch does not fail the same way again.
        self.store.io_error(error);
        let failed_slot = parts
            .iter()
            .find(|part| part.index == index)
            .map(|part| part.slot);
        let owned = self
            .resources
            .get(&id)
            .and_then(|res| res.blocks.get(&index))
            .is_some_and(|block| block.slot.is_some() && block.slot == failed_slot);
        if owned {
            self.evict_block(id, index);
        }
        let Some(reader) = self.reader_mut(id, reader_id) else {
            return ReadResult::Error(ReaderError::Network);
        };
        reader.pos = pos + valid as u64;
        if valid > 0 {
            return ReadResult::Data(valid);
        }
        reader.pending = Some(pos);
        self.schedule(id);
        ReadResult::Pending
    }

    pub(crate) fn on_response(&mut self, id: ResourceId, fetch_id: FetchId, info: ResponseInfo) {
        let limit = self.settings.memory_resource_limit;
        let Some(res) = self.resources.get_mut(&id) else {
            return;
        };
        let Some(fetch) = res.fetch_mut(fetch_id) else {
            return;
        };
        if fetch.responded {
            return;
        }
        fetch.responded = true;
        fetch.pos = if info.partial { info.start } else { 0 };
        // A 206 from after the requested position is acceptable, as HTML's "verify a media
        // response" says, but leaves out the data that was requested; give up when a retry is
        // answered the same way.
        if info.partial && info.start > fetch.start {
            let start = fetch.start;
            if res.skipped_start == Some(start) {
                debug!("Media cache: resource {id} cannot be fetched at {start}");
                self.fail(id, ReaderError::Network);
                return;
            }
            res.skipped_start = Some(start);
        }
        let changed = res.meta.as_ref().map(|meta| {
            matches!((&meta.validator, &info.validator), (Some(old), Some(new)) if old != new) ||
                (!info.encoded &&
                    matches!((meta.announced_length, info.total_length), (Some(old), Some(new)) if old != new))
        });
        // A 200 to a range request is acceptable, as HTML's "verify a media response" says:
        // the server answers from the start, so load the resource sequentially from now on.
        let lost_ranges = changed == Some(false) &&
            !info.partial &&
            res.meta.as_ref().is_some_and(|meta| meta.partial);
        if lost_ranges {
            if let Some(fetch) = res.fetch_mut(fetch_id) {
                fetch.discard_before = fetch
                    .discard_before
                    .max(fetch.start / BLOCK_SIZE * BLOCK_SIZE);
            }
            if let Some(meta) = res.meta.as_mut() {
                meta.partial = false;
            }
            debug!("Media cache: resource {id} lost range support");
        }
        match changed {
            None => {
                let length = if info.encoded {
                    None
                } else {
                    info.total_length
                };
                res.meta = Some(Meta {
                    partial: info.partial && !info.encoded,
                    validator: info.validator.clone(),
                    cors_tainted: info.cors_tainted,
                    announced_length: length,
                });
                res.length = length;
                let ram_backed = length
                    .filter(|length| *length <= limit)
                    .is_some_and(|length| self.make_room_in_memory(id, length));
                let res = self.resources.get_mut(&id).expect("resource exists");
                res.ram_backed = ram_backed;
                debug!(
                    "Media cache: resource {id} has length {length:?}, seekable {}, in {}",
                    res.seekable(),
                    if res.ram_backed { "memory" } else { "the file" }
                );
                self.update_reservation();
                self.broadcast_metadata(id);
            },
            Some(true) => {
                debug!("Media cache: resource {id} changed");
                self.fail(id, ReaderError::ResourceChanged);
                return;
            },
            Some(false) => {},
        }
        if lost_ranges {
            let others: Vec<FetchId> = self.resources[&id]
                .fetches
                .iter()
                .map(|fetch| fetch.id)
                .filter(|other| *other != fetch_id)
                .collect();
            for other in others {
                self.remove_fetch(id, other, true);
            }
            self.broadcast_metadata(id);
        }
        if !self.verify_response(id, &info) {
            debug!("Media cache: a response of resource {id} mixes origins");
            self.fail(id, ReaderError::Network);
            return;
        }
        if let Some(meta) = self
            .resources
            .get_mut(&id)
            .and_then(|res| res.meta.as_mut())
            .filter(|meta| info.cors_tainted && !meta.cors_tainted)
        {
            meta.cors_tainted = true;
            self.broadcast_metadata(id);
        }
        self.schedule(id);
    }

    /// HTML's "verify a media response": a partial response must not mix a CORS-cross-origin
    /// response with responses of another origin.
    fn verify_response(&mut self, id: ResourceId, info: &ResponseInfo) -> bool {
        let Some(res) = self.resources.get_mut(&id) else {
            return false;
        };
        if !info.partial {
            return true;
        }
        let origin = match &info.origin {
            Some(origin) => ResponseOrigin::Origin(origin.clone()),
            None => ResponseOrigin::Rewritten,
        };
        let same = match (&res.origin, &origin) {
            (ResponseOrigin::None, _) | (ResponseOrigin::Rewritten, ResponseOrigin::Rewritten) => {
                true
            },
            (ResponseOrigin::Origin(previous), ResponseOrigin::Origin(origin)) => {
                previous == origin
            },
            _ => false,
        };
        if same {
            res.origin = origin;
        } else if info.cors_tainted {
            return false;
        } else {
            res.origin = ResponseOrigin::Multiple;
        }
        true
    }

    pub(crate) fn on_data(&mut self, id: ResourceId, fetch_id: FetchId, bytes: &[u8]) {
        let now = self.now;
        let cap = self.readahead_cap();
        let Some(res) = self.resources.get_mut(&id) else {
            return;
        };
        let end = res.end();
        let Some(fetch) = res.fetch_mut(fetch_id) else {
            return;
        };
        if !fetch.responded || bytes.is_empty() {
            return;
        }
        let had_progress = fetch.progress;
        let start = fetch.pos;
        let discard_before = fetch.discard_before;
        let max = end.saturating_sub(start).min(bytes.len() as u64) as usize;
        fetch.pos += bytes.len() as u64;
        // The blocks that readers need even if they lie in the discarded prefix.
        let wanted: Vec<(u64, u64)> = if start < discard_before {
            res.readers
                .values()
                .map(|reader| {
                    let (ahead, _) = reader.thresholds(
                        self.settings.readahead,
                        self.settings.resume,
                        if res.ram_backed { u64::MAX } else { cap },
                    );
                    (
                        reader.pos / BLOCK_SIZE,
                        reader.pos.saturating_add(ahead).div_ceil(BLOCK_SIZE),
                    )
                })
                .collect()
        } else {
            Vec::new()
        };

        let mut stored = 0;
        let mut pos = start;
        let mut data = &bytes[..max];
        while !data.is_empty() {
            let index = pos / BLOCK_SIZE;
            let offset = (pos % BLOCK_SIZE) as usize;
            let count = (BLOCK_SIZE as usize - offset).min(data.len());
            let keep = pos + count as u64 > discard_before ||
                wanted
                    .iter()
                    .any(|(first, end)| (*first..*end).contains(&index));
            if keep {
                stored += self.append(id, fetch_id, index, offset, &data[..count]);
            }
            pos += count as u64;
            data = &data[count..];
        }

        let Some(res) = self.resources.get_mut(&id) else {
            return;
        };
        // Only new data is progress: a restart that only reads through cached data, or
        // discards it, does not get the resource any further.
        if stored > 0 &&
            let Some(fetch) = res.fetch_mut(fetch_id)
        {
            fetch.progress = true;
        }
        if stored > 0 {
            for (&reader_id, reader) in res.readers.iter_mut() {
                let Some(observer) = &reader.observer else {
                    continue;
                };
                if reader
                    .last_progress
                    .is_some_and(|last| now.saturating_sub(last) < PROGRESS_INTERVAL)
                {
                    continue;
                }
                reader.last_progress = Some(now);
                self.effects.push_back(Effect::Event {
                    resource: id,
                    reader: reader_id,
                    observer: observer.clone(),
                    event: ReaderEvent::Progress,
                });
            }
        }
        if res
            .skipped_start
            .is_some_and(|skipped| res.is_cached(skipped))
        {
            res.skipped_start = None;
        }
        let crossed = start / BLOCK_SIZE != pos / BLOCK_SIZE;
        let complete = res.is_complete();
        self.wake_readers(id);
        if (stored > 0 && !had_progress) || crossed || complete {
            self.schedule(id);
        }
    }

    /// Appends `chunk`, the data at `offset` in block `index`, as far as it extends what the
    /// block holds. Returns how many bytes were new.
    fn append(
        &mut self,
        id: ResourceId,
        fetch: FetchId,
        index: u64,
        offset: usize,
        chunk: &[u8],
    ) -> u64 {
        let now = self.now;
        let res = self.resources.get_mut(&id).expect("resource exists");
        let current = res.blocks.get(&index).map_or(0, Block::len);
        if offset > current || offset + chunk.len() <= current {
            return 0;
        }
        let chunk = &chunk[current - offset..];
        let full_len = res.full_len(index);
        if !res.blocks.contains_key(&index) {
            let slot = if res.ram_backed {
                None
            } else {
                match self.allocate_slot(id, index) {
                    Some(slot) => Some(slot),
                    None => {
                        debug!("Media cache: no room for block {index} of resource {id}");
                        return 0;
                    },
                }
            };
            let res = self.resources.get_mut(&id).expect("resource exists");
            res.blocks.insert(
                index,
                Block {
                    slot,
                    file_len: 0,
                    ram: Vec::with_capacity(full_len),
                    flushing: false,
                    writer: None,
                    last_use: now,
                    metadata: false,
                },
            );
        }
        let res = self.resources.get_mut(&id).expect("resource exists");
        let block = res.blocks.get_mut(&index).expect("block exists");
        let count = chunk.len().min(full_len.saturating_sub(block.len()));
        if block.ram.is_empty() {
            block
                .ram
                .reserve_exact(full_len.saturating_sub(block.len()));
        }
        block.ram.extend_from_slice(&chunk[..count]);
        block.last_use = now;
        block.writer = (block.len() < full_len).then_some(fetch);
        res.cached_bytes += count as u64;
        self.maybe_flush(id, index);
        count as u64
    }

    /// Starts writing the buffer of a block to its slot once the block is full or no fetch
    /// fills it any more.
    fn maybe_flush(&mut self, id: ResourceId, index: u64) {
        let Some(file) = self.store.writable_file() else {
            return;
        };
        let Some(res) = self.resources.get_mut(&id) else {
            return;
        };
        let full_len = res.full_len(index);
        let Some(block) = res.blocks.get_mut(&index) else {
            return;
        };
        let Some(slot) = block.slot else {
            return;
        };
        if block.ram.is_empty() ||
            block.flushing ||
            (block.writer.is_some() && block.len() < full_len)
        {
            return;
        }
        block.flushing = true;
        self.store.pin(slot);
        self.deferred.flushes.push(FlushJob {
            resource: id,
            index,
            slot,
            offset: slot_offset(slot) + block.file_len as u64,
            data: block.ram.clone(),
            file,
        });
    }

    fn finish_flush(&mut self, job: FlushJob, result: std::io::Result<()>) {
        self.store.unpin(job.slot);
        let Some(block) = self
            .resources
            .get_mut(&job.resource)
            .and_then(|res| res.blocks.get_mut(&job.index))
        else {
            return;
        };
        if block.slot != Some(job.slot) || !block.flushing {
            return;
        }
        block.flushing = false;
        match result {
            Ok(()) => {
                block.file_len += job.data.len();
                block.ram.drain(..job.data.len());
                if block.ram.is_empty() {
                    block.ram = Vec::new();
                }
                self.maybe_flush(job.resource, job.index);
            },
            Err(error) => self.store.io_error(error),
        }
    }

    pub(crate) fn on_suspended(&mut self, id: ResourceId, fetch_id: FetchId, honoured: bool) {
        let Some(res) = self.resources.get_mut(&id) else {
            return;
        };
        let seekable = res.seekable();
        let Some(fetch) = res.fetch_mut(fetch_id) else {
            return;
        };
        fetch.pending_replies = fetch.pending_replies.saturating_sub(1);
        if fetch.control != Control::Suspending || fetch.pending_replies > 0 {
            return;
        }
        if honoured {
            fetch.control = Control::Suspended;
            fetch.declined = false;
            self.update_download_state(id);
        } else if seekable || fetch.declined {
            debug!("Media cache: suspend of fetch {fetch_id} declined, cancelling it");
            self.remove_fetch(id, fetch_id, true);
            self.schedule(id);
        } else {
            debug!("Media cache: suspend of fetch {fetch_id} declined, it keeps loading");
            fetch.control = Control::Running;
            fetch.suspended_since = None;
            fetch.declined = true;
            self.schedule(id);
        }
    }

    pub(crate) fn on_end(
        &mut self,
        id: ResourceId,
        fetch_id: FetchId,
        result: Result<(), FetchError>,
    ) {
        let Some(fetch) = self.remove_fetch(id, fetch_id, false) else {
            return;
        };
        let res = self.resources.get_mut(&id).expect("resource exists");
        let mut length_changed = false;
        let early = match result {
            Ok(()) => match res.length {
                None if fetch.responded => {
                    res.length = Some(fetch.pos);
                    length_changed = true;
                    false
                },
                None => true,
                Some(length) => fetch.pos < length,
            },
            Err(FetchError::RangeNotSatisfiable) => {
                if res.length.is_none_or(|length| fetch.start < length) {
                    if res.meta.is_none() {
                        res.meta = Some(Meta {
                            partial: false,
                            validator: None,
                            cors_tainted: false,
                            announced_length: Some(fetch.start),
                        });
                    }
                    self.truncate(id, fetch.start);
                    length_changed = true;
                }
                false
            },
            Err(_) => true,
        };
        debug!(
            "Media cache: fetch {fetch_id} of resource {id} ended at {} with {result:?}",
            fetch.pos
        );
        if early {
            if !fetch.progress {
                self.fail(id, ReaderError::Network);
                return;
            }
            if fetch.control == Control::Running {
                self.resources
                    .get_mut(&id)
                    .expect("resource exists")
                    .restart_now = true;
            }
        }
        if length_changed {
            self.broadcast_metadata(id);
        }
        self.wake_readers(id);
        self.schedule(id);
    }

    /// The resource ends at `length`.
    fn truncate(&mut self, id: ResourceId, length: u64) {
        let res = self.resources.get_mut(&id).expect("resource exists");
        res.length = Some(length);
        let beyond: Vec<u64> = res
            .blocks
            .range(length.div_ceil(BLOCK_SIZE)..)
            .map(|(index, _)| *index)
            .collect();
        for index in beyond {
            self.evict_block(id, index);
        }
        self.update_reservation();
    }

    pub(crate) fn run_timers(&mut self) {
        self.sweep();
        let now = self.now;
        let timeout = self.settings.suspend_timeout;
        let expired: Vec<(ResourceId, FetchId)> = self
            .resources
            .iter()
            .filter(|(_, res)| res.seekable())
            .flat_map(|(id, res)| {
                res.fetches
                    .iter()
                    .filter(move |fetch| {
                        fetch.control != Control::Running &&
                            fetch
                                .suspended_since
                                .is_some_and(|since| now >= since + timeout)
                    })
                    .map(move |fetch| (*id, fetch.id))
            })
            .collect();
        for (id, fetch) in expired {
            debug!("Media cache: fetch {fetch} suspended for too long, cancelling it");
            self.remove_fetch(id, fetch, true);
            self.schedule(id);
        }
    }

    /// Removes a fetch, and cancels it unless it ended.
    fn remove_fetch(&mut self, id: ResourceId, fetch_id: FetchId, cancel: bool) -> Option<Fetch> {
        let res = self.resources.get_mut(&id)?;
        let position = res.fetches.iter().position(|fetch| fetch.id == fetch_id)?;
        let fetch = res.fetches.remove(position);
        self.effects.push_back(if cancel {
            Effect::Cancel(fetch_id)
        } else {
            Effect::Forget(fetch_id)
        });
        let index = fetch.pos / BLOCK_SIZE;
        if let Some(block) = res.blocks.get_mut(&index) &&
            block.writer == Some(fetch_id)
        {
            block.writer = None;
        }
        self.maybe_flush(id, index);
        self.deferred.garbage.push(Box::new(fetch.fetcher.clone()));
        Some(fetch)
    }

    fn fail(&mut self, id: ResourceId, error: ReaderError) {
        let Some(res) = self.resources.get_mut(&id) else {
            return;
        };
        if res.error.is_some() {
            return;
        }
        res.error = Some(error.clone());
        let indices: Vec<u64> = res.blocks.keys().copied().collect();
        let fetches: Vec<FetchId> = res.fetches.iter().map(|fetch| fetch.id).collect();
        if self.by_key.get(&res.key) == Some(&id) {
            self.by_key.remove(&res.key);
        }
        res.listed = false;
        for index in indices {
            self.evict_block(id, index);
        }
        for fetch in fetches {
            self.remove_fetch(id, fetch, true);
        }
        self.update_reservation();
        self.broadcast(id, ReaderEvent::Error(error));
        self.wake_readers(id);
    }

    fn broadcast_metadata(&mut self, id: ResourceId) {
        let Some(res) = self.resources.get(&id) else {
            return;
        };
        let Some(meta) = &res.meta else {
            return;
        };
        let event = ReaderEvent::Metadata {
            length: res.length,
            seekable: res.seekable(),
            cors_tainted: meta.cors_tainted,
        };
        self.broadcast(id, event);
    }

    fn broadcast(&mut self, id: ResourceId, event: ReaderEvent) {
        let Some(res) = self.resources.get(&id) else {
            return;
        };
        for (&reader, state) in &res.readers {
            if let Some(observer) = &state.observer {
                self.effects.push_back(Effect::Event {
                    resource: id,
                    reader,
                    observer: observer.clone(),
                    event: event.clone(),
                });
            }
        }
    }

    /// Calls the wakers of the readers that can read at their pending position now. A reader
    /// without a waker stays pending until it gets one.
    fn wake_readers(&mut self, id: ResourceId) {
        let Some(res) = self.resources.get_mut(&id) else {
            return;
        };
        let ready: Vec<ReaderId> = res
            .readers
            .iter()
            .filter(|(_, reader)| {
                reader.waker.is_some() && reader.pending.is_some_and(|pos| res.readable_at(pos))
            })
            .map(|(id, _)| *id)
            .collect();
        for reader_id in ready {
            let Some(reader) = res.readers.get_mut(&reader_id) else {
                continue;
            };
            reader.pending = None;
            if let Some(waker) = &reader.waker {
                self.effects.push_back(Effect::Wake {
                    resource: id,
                    reader: reader_id,
                    waker: waker.clone(),
                });
            }
        }
    }

    /// Tells the readers when the download stops or starts again.
    fn update_download_state(&mut self, id: ResourceId) {
        let Some(res) = self.resources.get_mut(&id) else {
            return;
        };
        if res.error.is_some() || res.readers.is_empty() {
            return;
        }
        let active = res
            .fetches
            .iter()
            .any(|fetch| fetch.control != Control::Suspended);
        let (state, event) = if active {
            (Announced::Loading, ReaderEvent::DownloadResumed)
        } else if res.is_complete() {
            (Announced::Complete, ReaderEvent::Complete)
        } else {
            (Announced::Suspended, ReaderEvent::DownloadSuspended)
        };
        if res.announced == state {
            return;
        }
        res.announced = state;
        self.broadcast(id, event);
    }

    fn schedule_all(&mut self) {
        let ids: Vec<ResourceId> = self
            .resources
            .iter()
            .filter(|(_, res)| !res.readers.is_empty() || !res.fetches.is_empty())
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.schedule(id);
        }
    }

    /// The largest read-ahead of a reader of a resource in the file: together, the readers
    /// that load ahead may protect half of the budget.
    fn readahead_cap(&self) -> u64 {
        let readers: usize = self
            .resources
            .values()
            .filter(|res| !res.ram_backed && !res.is_complete())
            .map(|res| {
                res.readers
                    .values()
                    .filter(|reader| Self::loads_ahead(res, reader))
                    .count()
            })
            .sum();
        self.store.capacity() as u64 * BLOCK_SIZE / (2 * readers.max(1) as u64)
    }

    /// Whether a reader loads ahead, and so takes a share of the read-ahead budget.
    fn loads_ahead(res: &Resource, reader: &Reader) -> bool {
        !matches!(
            reader.activity,
            ReaderActivity::Metadata | ReaderActivity::Idle
        ) && reader.pos < res.end()
    }

    /// The protected read-ahead of a reader of a resource without range support in the file:
    /// together, those readers may protect half of the budget, since their resources can only
    /// be fetched again from the start.
    fn unseekable_share(&self) -> u64 {
        let readers: usize = self
            .resources
            .values()
            .filter(|res| !res.ram_backed && !res.seekable() && !res.is_complete())
            .map(|res| {
                res.readers
                    .values()
                    .filter(|reader| Self::loads_ahead(res, reader))
                    .count()
            })
            .sum();
        self.store.capacity() as u64 * BLOCK_SIZE / (2 * readers.max(1) as u64)
    }

    fn schedule(&mut self, id: ResourceId) {
        let cap = self.readahead_cap();
        let unseekable_share = self.unseekable_share();
        let Some(res) = self.resources.get(&id) else {
            return;
        };
        let actions = decide(res, &self.settings, cap, unseekable_share, self.now);
        let now = self.now;
        for action in actions {
            match action {
                Action::Suspend(fetch_id) => {
                    let Some(res) = self.resources.get_mut(&id) else {
                        return;
                    };
                    let seekable = res.seekable();
                    let Some(fetch) = res.fetch_mut(fetch_id) else {
                        continue;
                    };
                    debug!("Media cache: suspending fetch {fetch_id} at {}", fetch.pos);
                    fetch.control = Control::Suspending;
                    fetch.pending_replies += 1;
                    fetch.suspended_since = Some(now);
                    self.effects.push_back(Effect::Suspend(fetch_id));
                    if seekable {
                        self.deferred
                            .timers
                            .push(now + self.settings.suspend_timeout);
                    }
                },
                Action::Resume(fetch_id) => {
                    let Some(fetch) = self
                        .resources
                        .get_mut(&id)
                        .and_then(|res| res.fetch_mut(fetch_id))
                    else {
                        continue;
                    };
                    debug!("Media cache: resuming fetch {fetch_id} at {}", fetch.pos);
                    fetch.control = Control::Running;
                    fetch.suspended_since = None;
                    self.effects.push_back(Effect::Resume(fetch_id));
                },
                Action::Cancel(fetch_id) => {
                    debug!("Media cache: cancelling fetch {fetch_id}");
                    self.remove_fetch(id, fetch_id, true);
                },
                Action::Start {
                    start,
                    discard_before,
                } => self.start_fetch(id, start, discard_before),
            }
        }
        if let Some(res) = self.resources.get_mut(&id) {
            res.restart_now = false;
        }
        self.update_download_state(id);
    }

    fn start_fetch(&mut self, id: ResourceId, start: u64, discard_before: u64) {
        let fetch_id = self.new_id();
        let weak = self.weak.clone();
        let Some(res) = self.resources.get_mut(&id) else {
            return;
        };
        let Some(fetcher) = res.fetcher() else {
            return;
        };
        let if_range = res.meta.as_ref().and_then(|meta| meta.validator.clone());
        if res.skipped_start.is_some_and(|skipped| skipped != start) {
            res.skipped_start = None;
        }
        debug!("Media cache: fetch {fetch_id} of resource {id} from {start}");
        res.fetches.push(Fetch {
            id: fetch_id,
            fetcher: fetcher.clone(),
            start,
            pos: start,
            responded: false,
            control: Control::Running,
            pending_replies: 0,
            suspended_since: None,
            progress: false,
            declined: false,
            discard_before,
        });
        self.effects.push_back(Effect::Start {
            resource: id,
            fetch: fetch_id,
            fetcher,
            request: FetchRequest { start, if_range },
            sink: Arc::new(CacheSink {
                inner: weak,
                resource: id,
                fetch: fetch_id,
            }),
        });
    }

    fn reader_views(&self) -> HashMap<ResourceId, Vec<ReaderView>> {
        let cap = self.readahead_cap();
        let unseekable_share = self.unseekable_share();
        self.resources
            .iter()
            .map(|(id, res)| {
                let cap = if res.ram_backed { u64::MAX } else { cap };
                let views = res
                    .readers
                    .values()
                    .filter(|reader| !reader.is_idle())
                    .flat_map(|reader| {
                        let (ahead, _) =
                            reader.thresholds(self.settings.readahead, self.settings.resume, cap);
                        let ahead = if res.seekable() {
                            ahead
                        } else {
                            ahead.max(unseekable_share)
                        };
                        reader
                            .positions(self.now)
                            .into_iter()
                            .map(move |pos| ReaderView {
                                pos,
                                rate: reader.eviction_rate(),
                                protect_end: pos.saturating_add(ahead).min(res.end()),
                            })
                    })
                    .collect();
                (*id, views)
            })
            .collect()
    }

    /// The blocks that may be evicted, best candidates first, at most `limit` of them.
    fn eviction_candidates(
        &self,
        file_only: bool,
        max_tier: Tier,
        limit: usize,
    ) -> Vec<(ResourceId, u64, Tier)> {
        let views = self.reader_views();
        let mut candidates: Vec<(Tier, f64, ResourceId, u64)> = Vec::new();
        for (id, res) in &self.resources {
            if file_only && res.ram_backed {
                continue;
            }
            let views = &views[id];
            for (&index, block) in &res.blocks {
                let pinned = block.slot.is_some_and(|slot| self.store.pinned(slot));
                if pinned || block.flushing || block.writer.is_some() {
                    continue;
                }
                let (tier, next_use) = rank(views, index, block, self.now);
                if tier <= max_tier {
                    candidates.push((tier, next_use, *id, index));
                }
            }
        }
        let order = |a: &(Tier, f64, ResourceId, u64), b: &(Tier, f64, ResourceId, u64)| {
            a.0.cmp(&b.0).then(b.1.total_cmp(&a.1))
        };
        if limit < candidates.len() {
            candidates.select_nth_unstable_by(limit, order);
            candidates.truncate(limit);
        }
        candidates.sort_by(order);
        candidates
            .into_iter()
            .map(|(tier, _, id, index)| (id, index, tier))
            .collect()
    }

    /// A slot for a new block, evicting other blocks if the budget is used up.
    fn allocate_slot(&mut self, id: ResourceId, index: u64) -> Option<SlotId> {
        if let Some(slot) = self.store.allocate((id, index)) {
            return Some(slot);
        }
        let capacity = self.store.capacity();
        let needed = (self.store.used() + 1).saturating_sub(capacity).max(1);
        let batch = (capacity / 32).max(1);
        let candidates = self.eviction_candidates(true, Tier::Protected, needed.max(batch));
        for (evicted, (victim, victim_index, tier)) in candidates.into_iter().enumerate() {
            let limit = if tier == Tier::Evictable {
                needed.max(batch)
            } else {
                needed
            };
            if evicted >= limit {
                break;
            }
            self.evict_block(victim, victim_index);
        }
        self.store.allocate((id, index))
    }

    fn evict_block(&mut self, id: ResourceId, index: u64) {
        let Some(res) = self.resources.get_mut(&id) else {
            return;
        };
        let Some(block) = res.blocks.remove(&index) else {
            return;
        };
        res.cached_bytes -= block.len() as u64;
        if let Some(slot) = block.slot {
            self.store.release(slot);
        }
    }

    pub(crate) fn trim(&mut self, target: u64) {
        let mut used: u64 = self.resources.values().map(|res| res.cached_bytes).sum();
        let mut idle: Vec<(Duration, ResourceId)> = self
            .resources
            .iter()
            .filter(|(_, res)| res.readers.is_empty())
            .map(|(id, res)| (res.last_used, *id))
            .collect();
        idle.sort();
        for (_, id) in idle {
            if used <= target {
                return;
            }
            used -= self.resources[&id].cached_bytes;
            self.remove_resource(id);
        }
        for (id, index, _) in self.eviction_candidates(false, Tier::Evictable, usize::MAX) {
            if used <= target {
                break;
            }
            used -= self.resources[&id].blocks[&index].len() as u64;
            self.evict_block(id, index);
        }
        self.schedule_all();
    }

    /// The bytes of RAM that the blocks use, for memory reports.
    pub(crate) fn blocks(&self) -> impl Iterator<Item = &Block> {
        self.resources.values().flat_map(|res| res.blocks.values())
    }

    #[cfg(test)]
    pub(crate) fn resource(&self, key: &ResourceKey) -> Option<&Resource> {
        self.resources.get(self.by_key.get(key)?)
    }
}
