/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! A byte-bounded in-process store.
//!
//! Used for private browsing, for `--temporary-storage`, for embedders that give
//! Servo no cache directory, and by the store test suite, so that every store test
//! runs against both backends.

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures::future::BoxFuture;
use malloc_size_of::{MallocSizeOf, MallocSizeOfOps};
use net_traits::CacheEntryDescriptor;
use rustc_hash::FxHashMap;
use smallvec::SmallVec;

use crate::http_cache::key::{CacheKey, EntryId, MAX_SLOTS, entry_id, entry_key_hash, entry_slot};
use crate::http_cache::store::{
    BodyRange, BodyReader, CacheStore, EntryMeta, EntryWriter, StoreError, WriteOp, clamp_range,
    entry_cap_for, split_into_chunks, stream_of_chunks,
};

struct MemoryVariant {
    slot: u8,
    meta: EntryMeta,
    chunks: Vec<Bytes>,
    bytes: usize,
    last_used: u64,
}

#[derive(Default)]
struct MemoryStoreInner {
    entries: FxHashMap<u64, SmallVec<[MemoryVariant; 1]>>,
    /// Slots handed out to writers that have not committed or aborted yet.
    reserved: FxHashMap<u64, SmallVec<[u8; 1]>>,
    usage: usize,
    clock: u64,
}

impl MemoryStoreInner {
    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn variant(&self, id: EntryId) -> Option<&MemoryVariant> {
        self.entries
            .get(&entry_key_hash(id))?
            .iter()
            .find(|variant| variant.slot == entry_slot(id))
    }

    fn free_slot(&self, key_hash: u64) -> Option<u8> {
        let used = self.entries.get(&key_hash);
        let reserved = self.reserved.get(&key_hash);
        (0..MAX_SLOTS).find(|slot| {
            !used.is_some_and(|variants| variants.iter().any(|variant| variant.slot == *slot)) &&
                !reserved.is_some_and(|slots| slots.contains(slot))
        })
    }

    fn release(&mut self, key_hash: u64, slot: u8) {
        if let Some(slots) = self.reserved.get_mut(&key_hash) {
            slots.retain(|reserved| *reserved != slot);
            if slots.is_empty() {
                self.reserved.remove(&key_hash);
            }
        }
    }

    fn remove(&mut self, id: EntryId) {
        let key_hash = entry_key_hash(id);
        let Some(variants) = self.entries.get_mut(&key_hash) else {
            return;
        };
        if let Some(index) = variants
            .iter()
            .position(|variant| variant.slot == entry_slot(id))
        {
            self.usage -= variants[index].bytes;
            variants.remove(index);
        }
        if variants.is_empty() {
            self.entries.remove(&key_hash);
        }
    }

    /// Drop least recently used entries until the budget is met.
    fn evict(&mut self, max_bytes: usize) {
        while self.usage > max_bytes {
            let Some(oldest) = self
                .entries
                .iter()
                .flat_map(|(hash, variants)| {
                    variants
                        .iter()
                        .map(move |variant| (entry_id(*hash, variant.slot), variant.last_used))
                })
                .min_by_key(|(_, last_used)| *last_used)
                .map(|(id, _)| id)
            else {
                return;
            };
            self.remove(oldest);
        }
    }
}

/// An in-memory [`CacheStore`] holding bodies as the chunks they arrived in.
pub struct MemoryStore {
    inner: Arc<Mutex<MemoryStoreInner>>,
    max_bytes: usize,
}

impl MemoryStore {
    /// A store that holds at most `max_bytes` of stored bodies.
    pub fn new(max_bytes: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(MemoryStoreInner::default())),
            max_bytes,
        }
    }

    /// Trim the store down to `target_bytes`, for use under memory pressure.
    pub fn trim(&self, target_bytes: usize) {
        self.inner.lock().unwrap().evict(target_bytes);
    }
}

impl MallocSizeOf for MemoryStore {
    fn size_of(&self, _ops: &mut MallocSizeOfOps) -> usize {
        self.inner.lock().unwrap().usage
    }
}

impl CacheStore for MemoryStore {
    fn lookup<'a>(&'a self, key: &'a CacheKey) -> BoxFuture<'a, Vec<(EntryId, EntryMeta)>> {
        let key_hash = key.hash();
        let inner = self.inner.clone();
        Box::pin(async move {
            let inner = inner.lock().unwrap();
            inner
                .entries
                .get(&key_hash)
                .map(|variants| {
                    variants
                        .iter()
                        .filter(|variant| variant.meta.key == *key)
                        .map(|variant| (entry_id(key_hash, variant.slot), variant.meta.clone()))
                        .collect()
                })
                .unwrap_or_default()
        })
    }

    fn open(
        &self,
        id: EntryId,
        range: Option<BodyRange>,
    ) -> BoxFuture<'_, Result<BodyReader, StoreError>> {
        let inner = self.inner.clone();
        Box::pin(async move {
            let inner = inner.lock().unwrap();
            let variant = inner.variant(id).ok_or(StoreError::Missing)?;
            let encoding = variant.meta.content_encoding;
            let len = variant.bytes as u64;
            let chunks = match range {
                None => variant.chunks.clone(),
                Some(range) => {
                    let range = clamp_range(range, len).ok_or(StoreError::Missing)?;
                    slice_chunks(&variant.chunks, range)
                },
            };
            let len = chunks.iter().map(|chunk| chunk.len() as u64).sum();
            Ok(BodyReader::new(len, encoding, stream_of_chunks(chunks)))
        })
    }

    fn create(&self, meta: EntryMeta) -> BoxFuture<'_, Result<EntryWriter, StoreError>> {
        let inner = self.inner.clone();
        let max_bytes = self.max_bytes;
        let max_entry_bytes = self.max_entry_bytes();
        Box::pin(async move {
            let key_hash = meta.key.hash();
            let slot = {
                let mut guard = inner.lock().unwrap();
                let slot = guard.free_slot(key_hash).ok_or(StoreError::Rejected)?;
                guard.reserved.entry(key_hash).or_default().push(slot);
                slot
            };

            let (writer, mut ops) = EntryWriter::channel();
            tokio::spawn(async move {
                let mut chunks: Vec<Bytes> = Vec::new();
                let mut bytes = 0usize;
                let mut committed = false;
                while let Some(op) = ops.recv().await {
                    match op {
                        WriteOp::Append(batch) => {
                            for chunk in batch {
                                bytes += chunk.len();
                                chunks.push(chunk);
                            }
                            // A response without a `Content-Length` is only found
                            // to be outsized here, and holding the rest of it would
                            // mean buffering a body the commit is going to decline.
                            if bytes as u64 > max_entry_bytes {
                                chunks.clear();
                            }
                        },
                        WriteOp::Commit(reply) => {
                            let mut guard = inner.lock().unwrap();
                            guard.release(key_hash, slot);
                            let result =
                                if bytes > max_bytes || bytes as u64 > max_entry_bytes {
                                    Err(StoreError::Rejected)
                                } else {
                                    let last_used = guard.tick();
                                    let mut meta = meta.clone();
                                    meta.body_len = bytes as u64;
                                    guard.usage += bytes;
                                    guard.entries.entry(key_hash).or_default().push(
                                        MemoryVariant {
                                            slot,
                                            meta,
                                            chunks: std::mem::take(&mut chunks),
                                            bytes,
                                            last_used,
                                        },
                                    );
                                    guard.evict(max_bytes);
                                    Ok(entry_id(key_hash, slot))
                                };
                            committed = result.is_ok();
                            let _ = reply.send(result);
                            break;
                        },
                    }
                }
                if !committed {
                    inner.lock().unwrap().release(key_hash, slot);
                }
            });
            Ok(writer)
        })
    }

    fn update_meta(&self, id: EntryId, meta: EntryMeta) -> BoxFuture<'_, Result<(), StoreError>> {
        let inner = self.inner.clone();
        Box::pin(async move {
            let mut guard = inner.lock().unwrap();
            let last_used = guard.tick();
            let slot = entry_slot(id);
            let variants = guard
                .entries
                .get_mut(&entry_key_hash(id))
                .ok_or(StoreError::Missing)?;
            let variant = variants
                .iter_mut()
                .find(|variant| variant.slot == slot && variant.meta.stamp == meta.stamp)
                .ok_or(StoreError::Missing)?;
            let body_len = variant.meta.body_len;
            variant.meta = meta;
            variant.meta.body_len = body_len;
            variant.last_used = last_used;
            Ok(())
        })
    }

    fn remove(&self, id: EntryId, stamp: u64) -> BoxFuture<'_, ()> {
        let inner = self.inner.clone();
        Box::pin(async move {
            let mut inner = inner.lock().unwrap();
            if inner
                .variant(id)
                .is_some_and(|variant| variant.meta.stamp == stamp)
            {
                inner.remove(id);
            }
        })
    }

    fn remove_key<'a>(&'a self, key: &'a CacheKey) -> BoxFuture<'a, ()> {
        let key_hash = key.hash();
        let inner = self.inner.clone();
        Box::pin(async move {
            let mut guard = inner.lock().unwrap();
            if let Some(variants) = guard.entries.remove(&key_hash) {
                let bytes: usize = variants.iter().map(|variant| variant.bytes).sum();
                guard.usage -= bytes;
            }
        })
    }

    fn clear(&self) -> BoxFuture<'_, ()> {
        let inner = self.inner.clone();
        Box::pin(async move {
            let mut guard = inner.lock().unwrap();
            guard.entries.clear();
            guard.usage = 0;
        })
    }

    fn touch(&self, id: EntryId) {
        let mut guard = self.inner.lock().unwrap();
        let last_used = guard.tick();
        let slot = entry_slot(id);
        if let Some(variants) = guard.entries.get_mut(&entry_key_hash(id)) &&
            let Some(variant) = variants.iter_mut().find(|variant| variant.slot == slot)
        {
            variant.last_used = last_used;
        }
    }

    fn descriptors(&self) -> Vec<CacheEntryDescriptor> {
        self.inner
            .lock()
            .unwrap()
            .entries
            .values()
            .flat_map(|variants| variants.iter())
            .map(|variant| CacheEntryDescriptor::new(variant.meta.key.url().to_string()))
            .collect()
    }

    fn stored_bytes(&self) -> usize {
        self.inner.lock().unwrap().usage
    }

    fn max_entry_bytes(&self) -> u64 {
        entry_cap_for(self.max_bytes as u64)
    }

    fn shutdown(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

/// Take an inclusive byte range out of a list of chunks without copying whole chunks.
fn slice_chunks(chunks: &[Bytes], range: BodyRange) -> Vec<Bytes> {
    let mut out = Vec::new();
    let mut offset = 0u64;
    for chunk in chunks {
        let chunk_end = offset + chunk.len() as u64;
        if chunk_end <= range.start {
            offset = chunk_end;
            continue;
        }
        if offset > range.end {
            break;
        }
        let start = range.start.saturating_sub(offset) as usize;
        let end = (range.end + 1 - offset).min(chunk.len() as u64) as usize;
        out.extend(split_into_chunks(chunk.slice(start..end)));
        offset = chunk_end;
    }
    out
}
