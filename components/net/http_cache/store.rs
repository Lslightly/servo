/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The storage layer of the HTTP cache.
//!
//! A store owns byte accounting and eviction and knows nothing about RFC 9111;
//! every caching decision is made above it, in [`crate::http_cache`]. Bodies are
//! written and read as streams, exactly as they arrived from the network, so a
//! complete body never has to exist in the net process.

use std::task::{Context, Poll};
use std::{fmt, io};

use bytes::Bytes;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use http::HeaderMap;
use malloc_size_of::MallocSizeOf;
use malloc_size_of_derive::MallocSizeOf as MallocSizeOfDerive;
use net_traits::CacheEntryDescriptor;
use net_traits::http_status::HttpStatus;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::PollSender;

use crate::decoder::DecoderType;
use crate::http_cache::key::{CacheKey, EntryId};
use crate::http_cache::policy::EntryPolicy;

/// Version of the stored entry format, which includes the version of the
/// serialized [`http_cache_rules::StoredPolicy`]. Bumping it makes existing
/// entries unreadable; a store discards what it has rather than migrating.
pub const CACHE_FORMAT: u16 = (3 << 8) | http_cache_rules::FORMAT_VERSION as u16;

const _: () = assert!(http_cache_rules::FORMAT_VERSION < 1 << 8);

/// The size of one read or write syscall, and of one `Bytes` slice handed to the
/// decoder. Measured as the throughput knee on both OHOS devices.
pub const CHUNK_BYTES: usize = 64 * 1024;

/// How much is accumulated before it is handed to a store's writer. A cross-thread
/// hand-off costs 28-45 us on device, several times a 64 KiB read, so it is
/// amortised over a few chunks.
pub const BATCH_BYTES: usize = 4 * CHUNK_BYTES;

/// How many batches may be queued towards a store before the network is
/// back-pressured.
const WRITE_QUEUE_DEPTH: usize = 4;

/// No entry may be larger than this share of a store's budget, and never less
/// than [`MIN_ENTRY_BYTES`]. Both are Chromium's values.
const ENTRY_BUDGET_SHARE: u64 = 8;
const MIN_ENTRY_BYTES: u64 = 5 * 1024 * 1024;

/// The per-entry cap for a store with the given budget, unless the
/// `network_http_cache_max_entry_size` pref overrides it.
pub(crate) fn entry_cap_for(max_bytes: u64) -> u64 {
    let cap = match servo_config::pref!(network_http_cache_max_entry_size) {
        0 => (max_bytes / ENTRY_BUDGET_SHARE).max(MIN_ENTRY_BYTES),
        configured => configured,
    };
    // The disk index records an entry's size in 32 bits, so a larger entry could
    // not be accounted for.
    cap.min(u32::MAX as u64)
}

/// A `HeaderMap` that can be serialized into a store.
#[derive(Clone, Debug, Deserialize, MallocSizeOfDerive, Serialize)]
pub struct SerializableHeaderMap(
    #[serde(
        deserialize_with = "hyper_serde::deserialize",
        serialize_with = "hyper_serde::serialize"
    )]
    pub HeaderMap,
);

impl std::ops::Deref for SerializableHeaderMap {
    type Target = HeaderMap;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<HeaderMap> for SerializableHeaderMap {
    fn from(value: HeaderMap) -> Self {
        SerializableHeaderMap(value)
    }
}

/// Everything about a stored response except its body.
#[derive(Clone, Debug, Deserialize, MallocSizeOfDerive, Serialize)]
pub struct EntryMeta {
    /// Format of the entry this metadata was written with.
    pub format: u16,
    /// Random, and kept when the metadata is replaced. An [`EntryId`] is reused
    /// once its entry is gone, so a store only acts on behalf of a caller whose
    /// stamp matches that of the entry it holds under the id.
    pub stamp: u64,
    /// The full key, compared on open so that a hash collision cannot serve the
    /// wrong resource.
    pub key: CacheKey,
    /// The RFC 9111 state of the entry.
    pub policy: EntryPolicy,
    /// The response header fields to store, as `http-cache-rules` selects them.
    pub headers: SerializableHeaderMap,
    /// The status line the response was received with.
    pub status: HttpStatus,
    /// The body is stored exactly as received, so this records how to decode it.
    pub content_encoding: Option<DecoderType>,
    /// Length of the stored (still encoded) body. Only meaningful once the entry
    /// has been committed.
    pub body_len: u64,
}

/// Why a store could not do what was asked. None of these ever fail a fetch.
#[derive(Debug)]
pub enum StoreError {
    /// The entry is not there: evicted, never committed, or removed behind the
    /// store's back. Always recoverable by fetching from the network.
    Missing,
    /// The store declined to take the entry (over budget, too large, shutting down).
    Rejected,
    /// The underlying storage failed.
    Io(io::Error),
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Missing => write!(formatter, "cache entry is missing"),
            StoreError::Rejected => write!(formatter, "cache store declined the entry"),
            StoreError::Io(error) => write!(formatter, "cache store I/O error: {error}"),
        }
    }
}

impl From<io::Error> for StoreError {
    fn from(error: io::Error) -> Self {
        if error.kind() == io::ErrorKind::NotFound {
            StoreError::Missing
        } else {
            StoreError::Io(error)
        }
    }
}

/// A byte range within a stored body, inclusive on both ends.
#[derive(Clone, Copy, Debug)]
pub struct BodyRange {
    /// First byte of the range.
    pub start: u64,
    /// Last byte of the range, inclusive.
    pub end: u64,
}

/// Streaming access to one stored body.
pub struct BodyReader {
    len: u64,
    content_encoding: Option<DecoderType>,
    stream: BoxStream<'static, io::Result<Bytes>>,
}

impl BodyReader {
    /// A reader over `stream`, which yields `len` bytes in `content_encoding`.
    pub fn new(
        len: u64,
        content_encoding: Option<DecoderType>,
        stream: BoxStream<'static, io::Result<Bytes>>,
    ) -> Self {
        Self {
            len,
            content_encoding,
            stream,
        }
    }

    /// Length of the bytes this reader will produce, still encoded.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether this reader will produce no bytes.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The coding the stored bytes are in, if any.
    pub fn content_encoding(&self) -> Option<DecoderType> {
        self.content_encoding
    }

    /// Take the stream of stored bytes.
    pub fn into_stream(self) -> BoxStream<'static, io::Result<Bytes>> {
        self.stream
    }
}

/// What a store's writer task is asked to do.
pub(crate) enum WriteOp {
    Append(Vec<Bytes>),
    Commit(oneshot::Sender<Result<EntryId, StoreError>>),
}

/// Writes one entry's body. The entry stays invisible to lookups until
/// [`EntryWriter::commit`] succeeds; dropping the writer aborts it.
pub struct EntryWriter {
    ops: Option<PollSender<WriteOp>>,
    batch: Vec<Bytes>,
    batch_len: usize,
}

impl EntryWriter {
    pub(crate) fn new(capacity: usize) -> (EntryWriter, mpsc::Receiver<WriteOp>) {
        let (sender, receiver) = mpsc::channel(capacity);
        (
            EntryWriter {
                ops: Some(PollSender::new(sender)),
                batch: Vec::new(),
                batch_len: 0,
            },
            receiver,
        )
    }

    pub(crate) fn channel() -> (EntryWriter, mpsc::Receiver<WriteOp>) {
        Self::new(WRITE_QUEUE_DEPTH)
    }

    /// A writer that throws everything away, used when the store declined the entry
    /// but the caller has already been handed a writer.
    pub fn sink() -> EntryWriter {
        EntryWriter {
            ops: None,
            batch: Vec::new(),
            batch_len: 0,
        }
    }

    /// Ready once another chunk can be accepted without buffering unboundedly.
    /// This is what back-pressures the network stream onto the store.
    pub fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        if self.batch_len < BATCH_BYTES {
            return Poll::Ready(());
        }
        let Some(ops) = self.ops.as_mut() else {
            self.discard_batch();
            return Poll::Ready(());
        };
        match ops.poll_reserve(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(_)) => {
                // The store is gone; keep accepting chunks and drop them.
                self.ops = None;
                self.discard_batch();
                Poll::Ready(())
            },
            Poll::Ready(Ok(())) => {
                let batch = std::mem::take(&mut self.batch);
                self.batch_len = 0;
                if ops.send_item(WriteOp::Append(batch)).is_err() {
                    self.ops = None;
                }
                Poll::Ready(())
            },
        }
    }

    /// Hand a chunk to the store. Zero-copy: `Bytes` is refcounted.
    ///
    /// Must be preceded by a `Poll::Ready` from [`EntryWriter::poll_ready`],
    /// otherwise the batch can grow past its bound.
    pub fn push(&mut self, chunk: Bytes) {
        if self.ops.is_none() {
            return;
        }
        self.batch_len += chunk.len();
        self.batch.push(chunk);
    }

    fn discard_batch(&mut self) {
        self.batch.clear();
        self.batch_len = 0;
    }

    /// Publish the entry. Only after this does a lookup see it.
    pub async fn commit(mut self) -> Result<EntryId, StoreError> {
        let Some(mut ops) = self.ops.take() else {
            return Err(StoreError::Rejected);
        };
        if !self.batch.is_empty() {
            let batch = std::mem::take(&mut self.batch);
            self.batch_len = 0;
            if send(&mut ops, WriteOp::Append(batch)).await.is_err() {
                return Err(StoreError::Rejected);
            }
        }
        let (sender, receiver) = oneshot::channel();
        if send(&mut ops, WriteOp::Commit(sender)).await.is_err() {
            return Err(StoreError::Rejected);
        }
        receiver.await.unwrap_or(Err(StoreError::Rejected))
    }

    /// Throw the entry away. Also happens on drop.
    pub fn abort(self) {}
}

/// A store keeps entries under byte and entry budgets, and never fails a fetch:
/// every error is reported as a miss.
pub trait CacheStore: MallocSizeOf + Send + Sync + 'static {
    /// All variants stored under `key`. The semantics layer picks one.
    fn lookup<'a>(&'a self, key: &'a CacheKey) -> BoxFuture<'a, Vec<(EntryId, EntryMeta)>>;

    /// Open the stored body of the entry `meta` describes for streaming. `range`
    /// selects a byte range of the stored (still encoded) bytes; a ranged read
    /// skips checksum verification.
    fn open(
        &self,
        id: EntryId,
        meta: &EntryMeta,
        range: Option<BodyRange>,
    ) -> BoxFuture<'_, Result<BodyReader, StoreError>>;

    /// Reserve a slot for a new variant. The entry is invisible until the writer
    /// commits.
    fn create(&self, meta: EntryMeta) -> BoxFuture<'_, Result<EntryWriter, StoreError>>;

    /// Replace an entry's metadata after a 304, keeping its body. Fails if the
    /// entry under `id` does not have the stamp of `meta`.
    fn update_meta(&self, id: EntryId, meta: EntryMeta) -> BoxFuture<'_, Result<(), StoreError>>;

    /// Drop one variant, if the entry under `id` has `stamp`.
    fn remove(&self, id: EntryId, stamp: u64) -> BoxFuture<'_, ()>;

    /// Drop every variant under `key`, for "Invalidating Stored Responses".
    fn remove_key<'a>(&'a self, key: &'a CacheKey) -> BoxFuture<'a, ()>;

    /// Drop everything.
    fn clear(&self) -> BoxFuture<'_, ()>;

    /// Mark an entry as recently used. Cheap and synchronous; on the fetch path.
    fn touch(&self, id: EntryId);

    /// The entries devtools lists.
    fn descriptors(&self) -> BoxFuture<'_, Vec<CacheEntryDescriptor>>;

    /// Bytes held in this process, for `about:memory`. Zero for a store whose
    /// bodies live in files.
    fn stored_bytes(&self) -> usize {
        0
    }

    /// Bytes held on disk, reported as non-heap so device measurements can see it.
    fn disk_bytes(&self) -> u64 {
        0
    }

    /// The largest response this store will take. Chromium's rule is an eighth of
    /// the budget, never below 5 MiB.
    fn max_entry_bytes(&self) -> u64;

    /// Persist anything that only lives in memory, and return once it is on disk.
    ///
    /// Called when the embedding application is backgrounded, because a
    /// backgrounded application may be killed without any further notice.
    fn flush(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    /// Flush anything that only lives in memory. Called when the resource thread exits.
    fn shutdown(&self) -> BoxFuture<'_, ()>;
}

/// Await room in the queue and hand over one operation.
async fn send(ops: &mut PollSender<WriteOp>, op: WriteOp) -> Result<(), ()> {
    futures::future::poll_fn(|cx| ops.poll_reserve(cx))
        .await
        .map_err(|_| ())?;
    ops.send_item(op).map_err(|_| ())
}

/// Split a buffer into chunk-sized `Bytes` slices without copying.
pub(crate) fn split_into_chunks(buffer: Bytes) -> Vec<Bytes> {
    let mut chunks = Vec::with_capacity(buffer.len().div_ceil(CHUNK_BYTES).max(1));
    let mut rest = buffer;
    while rest.len() > CHUNK_BYTES {
        chunks.push(rest.split_to(CHUNK_BYTES));
    }
    if !rest.is_empty() {
        chunks.push(rest);
    }
    chunks
}

/// A stream over a body that is already fully in memory.
pub(crate) fn stream_of_chunks(chunks: Vec<Bytes>) -> BoxStream<'static, io::Result<Bytes>> {
    Box::pin(futures::stream::iter(chunks.into_iter().map(Ok)))
}

/// Clamp a range to a body length, returning `None` when it is unsatisfiable.
pub(crate) fn clamp_range(range: BodyRange, len: u64) -> Option<BodyRange> {
    if len == 0 || range.start >= len {
        return None;
    }
    Some(BodyRange {
        start: range.start,
        end: range.end.min(len - 1),
    })
}
