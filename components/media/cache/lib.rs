/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! A cache of media resources shared by the media players of a content process.
//!
//! The cache keeps resources in blocks of 64 KiB, in memory for small resources and otherwise
//! in the slots of one anonymous temporary file, and evicts the blocks that it predicts to be
//! used last. It fetches each resource through a [`Fetcher`] on behalf of its readers: one main
//! fetch that it suspends once enough is loaded ahead of every reader and resumes when a reader
//! comes close, and a second fetch for a read far from it, such as the probe of an index at the
//! end of a file. Readers never block: a read of data that is not there yet returns
//! [`ReadResult::Pending`], and the reader's waker is called once it is.
//!
//! Every method may be called on any thread. The cache calls fetchers, fetch handles, wakers
//! and observers on whichever thread runs its queue of calls at that moment, in the order it
//! queued them, so they may call back into the cache. They must not block. The cache holds no
//! lock while it calls them, except that [`FetchHandle`] methods run with the lock of the
//! handles held, which their calls into the cache do not take.

mod cache;
mod clock;
mod resource;
mod schedule;
mod store;
#[cfg(test)]
mod tests;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use bytes::Bytes;
use malloc_size_of::{MallocShallowSizeOf, MallocSizeOfOps};

use crate::cache::{Inner, Settings};
use crate::clock::{Clock, RealClock};
use crate::resource::{ReaderId, ResourceId};

/// How a media resource is requested, as part of what identifies it.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CorsMode {
    NoCors,
    Anonymous,
    UseCredentials,
}

/// What identifies a media resource: the requested URL, the CORS mode of the request and the
/// origin of the document that requests it.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ResourceKey {
    pub url: String,
    pub cors_mode: CorsMode,
    pub origin: String,
}

/// A strong validator of a response, sent back in `If-Range`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Validator {
    ETag(String),
    LastModified(String),
}

/// A fetch of the resource from `start` to its end.
#[derive(Clone, Debug)]
pub struct FetchRequest {
    pub start: u64,
    pub if_range: Option<Validator>,
}

/// What the cache needs to know about a response.
#[derive(Clone, Debug)]
pub struct ResponseInfo {
    /// Whether the response is a 206.
    pub partial: bool,
    /// The position of the first byte of the body in the resource: the start of `Content-Range`
    /// for a 206, 0 otherwise.
    pub start: u64,
    /// The length of the whole resource, from `Content-Range` or `Content-Length`, if known.
    pub total_length: Option<u64>,
    pub validator: Option<Validator>,
    /// Whether the body has a `Content-Encoding`, so that byte positions do not apply to it.
    pub encoded: bool,
    /// Whether the response is CORS-cross-origin, so that the media data taints its uses.
    pub cors_tainted: bool,
    /// The origin of the response's final URL, `None` if the response has no URL
    /// ("rewritten").
    pub origin: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FetchError {
    Network,
    Status(u16),
    RangeNotSatisfiable,
    Blocked,
}

/// Controls one fetch. Dropping it cancels the fetch.
pub trait FetchHandle: Send {
    /// Asks to stop reading the body. The fetch answers through [`FetchSink::suspended`].
    fn suspend(&self);
    fn resume(&self);
    fn cancel(&self);
}

/// Receives the progress of one fetch, on any thread.
pub trait FetchSink: Send + Sync {
    fn response(&self, info: ResponseInfo);
    fn data(&self, bytes: Bytes);
    /// Whether a suspend took effect.
    fn suspended(&self, honoured: bool);
    fn end(&self, result: Result<(), FetchError>);
}

/// Starts fetches of a resource on behalf of the documents that use it.
pub trait Fetcher: Send + Sync {
    fn start(&self, request: FetchRequest, sink: Arc<dyn FetchSink>) -> Box<dyn FetchHandle>;
}

/// What a reader needs, which sets how far ahead of it the cache loads.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ReaderActivity {
    /// Playback: loads `media_cache_readahead_seconds` of media ahead at the rate if it is
    /// known, else 8 MiB, within the reader's share of the budget.
    Playing { bytes_per_second: Option<f64> },
    /// Paused playback: loads as `Playing`, so that playback can resume without waiting.
    Paused { bytes_per_second: Option<f64> },
    /// Only the metadata is needed, as for `preload=metadata`.
    Metadata,
    /// No data is needed until further notice, as for an element that is not in a fully
    /// active document. Only reads that return `Pending` fetch data.
    Idle,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReaderError {
    Network,
    /// A later response did not match the validator of the first one.
    ResourceChanged,
}

#[derive(Debug, PartialEq)]
pub enum ReadResult {
    Data(usize),
    /// Not available yet; the waker is called once it is.
    Pending,
    EndOfStream,
    Error(ReaderError),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ReaderEvent {
    Metadata {
        length: Option<u64>,
        seekable: bool,
        cors_tainted: bool,
    },
    Progress,
    DownloadSuspended,
    DownloadResumed,
    /// The whole resource is cached.
    Complete,
    Error(ReaderError),
}

/// What the media cache keeps, in bytes, for memory reports.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MediaCacheMemory {
    /// The heap memory of the resources that are kept in memory.
    pub resources_in_memory: usize,
    /// The heap memory of the data of other resources that is not written to the file yet.
    pub write_buffers: usize,
    /// The heap memory of the data of other resources that is kept in memory because the file
    /// cannot be used.
    pub memory_fallback: usize,
    /// The data in the temporary file, which is in the page cache rather than in the memory of
    /// the process.
    pub file: usize,
}

static CACHE: OnceLock<Arc<MediaCache>> = OnceLock::new();

/// The media resources of a content process.
pub struct MediaCache {
    inner: Arc<Inner>,
}

impl MediaCache {
    /// The cache of this process, created on first use with the `media_cache_*` preferences.
    pub fn get() -> Arc<MediaCache> {
        CACHE
            .get_or_init(|| {
                Arc::new(MediaCache::with_clock(
                    Settings::from_prefs(),
                    Arc::new(RealClock::new()),
                ))
            })
            .clone()
    }

    /// The cache of this process, if it was used.
    pub fn existing() -> Option<Arc<MediaCache>> {
        CACHE.get().cloned()
    }

    pub(crate) fn with_clock(settings: Settings, clock: Arc<dyn Clock>) -> Self {
        Self {
            inner: Inner::new(settings, clock),
        }
    }

    /// Opens a reader of the resource, sharing it with the readers that already use it.
    pub fn open(&self, key: ResourceKey, fetcher: Arc<dyn Fetcher>) -> MediaReader {
        let (resource, reader) = self.inner.update(|state| state.open(key, fetcher));
        MediaReader {
            inner: self.inner.clone(),
            resource,
            reader,
            closed: AtomicBool::new(false),
        }
    }

    /// Drops cached data until at most `target_bytes` are used.
    pub fn trim(&self, target_bytes: u64) {
        self.inner.update(|state| state.trim(target_bytes));
    }
}

impl MediaCache {
    /// Measures the data that the cache keeps.
    pub fn measure_memory(&self, ops: &mut MallocSizeOfOps) -> MediaCacheMemory {
        let state = self.inner.state.lock();
        let memory_fallback = state.store.ram_mode();
        let mut memory = MediaCacheMemory::default();
        for resource in state.resources.values() {
            for block in resource.blocks.values() {
                let ram = block.ram.shallow_size_of(ops);
                if resource.ram_backed {
                    memory.resources_in_memory += ram;
                } else if memory_fallback {
                    memory.memory_fallback += ram;
                } else {
                    memory.write_buffers += ram;
                }
                memory.file += block.file_len;
            }
        }
        memory
    }
}

/// One reader of a media resource. Dropping it detaches the reader.
pub struct MediaReader {
    inner: Arc<Inner>,
    resource: ResourceId,
    reader: ReaderId,
    /// Whether [`MediaReader::close`] detached the reader.
    closed: AtomicBool,
}

impl MediaReader {
    /// Copies the bytes at `pos` into `dest`. Never blocks.
    pub fn read_at(&self, pos: u64, dest: &mut [u8]) -> ReadResult {
        self.inner.read(self.resource, self.reader, pos, dest)
    }

    /// Called once data is readable at the position of the last `Pending` read, or the stream
    /// ended or failed.
    ///
    /// The closure must not own this reader, which would keep it alive. It may still be called
    /// shortly after the reader's `Drop` returns, if another thread runs the cache's calls.
    pub fn set_waker(&self, waker: Box<dyn Fn() + Send + Sync>) {
        self.inner
            .update(|state| state.set_waker(self.resource, self.reader, Arc::from(waker)));
    }

    pub fn length(&self) -> Option<u64> {
        self.inner.state.lock().length(self.resource)
    }

    pub fn seekable(&self) -> bool {
        self.inner.state.lock().seekable(self.resource)
    }

    /// Whether a response for the resource was CORS-cross-origin, as the `cors_tainted` of
    /// [`ReaderEvent::Metadata`].
    pub fn cors_tainted(&self) -> bool {
        self.inner.state.lock().cors_tainted(self.resource)
    }

    pub fn set_activity(&self, activity: ReaderActivity) {
        self.inner
            .update(|state| state.set_activity(self.resource, self.reader, activity));
    }

    /// Called with the events of the download. The closure must not own this reader, and may
    /// still be called shortly after the reader's `Drop` returns, as the waker.
    pub fn set_observer(&self, observer: Box<dyn Fn(ReaderEvent) + Send + Sync>) {
        self.inner
            .update(|state| state.set_observer(self.resource, self.reader, Arc::from(observer)));
    }

    /// Detaches the reader as dropping it does, for a reader that others, such as a player,
    /// still hold. Its waker is called once more, so that a pending read learns of it, and every
    /// later read returns [`ReaderError::Network`].
    pub fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let waker = self
            .inner
            .update(|state| state.close(self.resource, self.reader));
        if let Some(waker) = waker {
            waker();
        }
    }
}

impl Drop for MediaReader {
    fn drop(&mut self) {
        if !*self.closed.get_mut() {
            self.inner
                .update(|state| state.detach(self.resource, self.reader));
        }
    }
}
