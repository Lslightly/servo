/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The in-memory index of a disk store, and the file it is persisted to.
//!
//! The index is a derived artifact: losing it costs a directory scan, never an
//! entry. It holds no metadata beyond what eviction needs, so the RAM per entry
//! stays at Chromium's scale; the response headers and policy are read from the
//! entry file on lookup.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{fs, io};

use crc32fast::Hasher;
use log::{debug, info};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

use crate::http_cache::key::{EntryId, MAX_SLOTS, entry_id, entry_key_hash, entry_slot};
use crate::http_cache::store::CACHE_FORMAT;

const INDEX_MAGIC: u64 = 0x5845_444e_495f_4341;
/// Length of the header of `index-data`: magic, format, count, dir mtime, crc.
const INDEX_HEADER_LEN: usize = 8 + 4 + 4 + 8 + 4;

/// The version stamp. A mismatch discards the store's files.
pub(crate) const STAMP_FILE: &str = "index";
/// The index itself.
pub(crate) const INDEX_FILE: &str = "index-data";
/// Where entry files live.
pub(crate) const ENTRIES_DIR: &str = "entries";
/// Where entry files are written until they are committed.
pub(crate) const TMP_DIR: &str = "tmp";
/// Locked by the store that has the directory open.
pub(crate) const LOCK_FILE: &str = "lock";

/// Seconds since the epoch, which is all the resolution eviction needs.
pub(crate) fn now_secs() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs() as u32)
        .unwrap_or_default()
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
struct IndexRecord {
    key_hash: u64,
    slot: u8,
    size: u32,
    last_used: u32,
}

/// One stored variant, as the index knows it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Variant {
    pub slot: u8,
    pub size: u32,
    pub last_used: u32,
}

/// The index of a disk store.
#[derive(Default)]
pub(crate) struct Index {
    entries: FxHashMap<u64, SmallVec<[Variant; 1]>>,
    /// Slots handed to writers that have not committed or aborted yet.
    reserved: FxHashMap<u64, SmallVec<[u8; 1]>>,
    bytes: u64,
    count: usize,
    dirty: bool,
}

impl Index {
    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }

    pub(crate) fn count(&self) -> usize {
        self.count
    }

    pub(crate) fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub(crate) fn variants(&self, key_hash: u64) -> impl Iterator<Item = Variant> + '_ {
        self.entries
            .get(&key_hash)
            .into_iter()
            .flat_map(|variants| variants.iter().copied())
    }

    pub(crate) fn get(&self, id: EntryId) -> Option<Variant> {
        self.entries
            .get(&entry_key_hash(id))?
            .iter()
            .find(|variant| variant.slot == entry_slot(id))
            .copied()
    }

    pub(crate) fn all(&self) -> Vec<EntryId> {
        self.entries
            .iter()
            .flat_map(|(hash, variants)| {
                variants
                    .iter()
                    .map(move |variant| entry_id(*hash, variant.slot))
            })
            .collect()
    }

    /// Reserve a slot for a writer, or `None` when this key already has as many
    /// variants as it may have.
    pub(crate) fn reserve_slot(&mut self, key_hash: u64) -> Option<u8> {
        let used = self.entries.get(&key_hash);
        let reserved = self.reserved.get(&key_hash);
        let slot = (0..MAX_SLOTS).find(|slot| {
            !used.is_some_and(|variants| variants.iter().any(|variant| variant.slot == *slot)) &&
                !reserved.is_some_and(|slots| slots.contains(slot))
        })?;
        self.reserved.entry(key_hash).or_default().push(slot);
        Some(slot)
    }

    pub(crate) fn release_slot(&mut self, key_hash: u64, slot: u8) {
        if let Some(slots) = self.reserved.get_mut(&key_hash) {
            slots.retain(|reserved| *reserved != slot);
            if slots.is_empty() {
                self.reserved.remove(&key_hash);
            }
        }
    }

    pub(crate) fn insert(&mut self, key_hash: u64, variant: Variant) {
        self.remove(entry_id(key_hash, variant.slot));
        self.bytes += variant.size as u64;
        self.count += 1;
        self.entries.entry(key_hash).or_default().push(variant);
        self.dirty = true;
    }

    pub(crate) fn remove(&mut self, id: EntryId) -> bool {
        let key_hash = entry_key_hash(id);
        let Some(variants) = self.entries.get_mut(&key_hash) else {
            return false;
        };
        let Some(position) = variants
            .iter()
            .position(|variant| variant.slot == entry_slot(id))
        else {
            return false;
        };
        self.bytes -= variants[position].size as u64;
        self.count -= 1;
        variants.remove(position);
        if variants.is_empty() {
            self.entries.remove(&key_hash);
        }
        self.dirty = true;
        true
    }

    pub(crate) fn remove_key(&mut self, key_hash: u64) -> Vec<EntryId> {
        let Some(variants) = self.entries.remove(&key_hash) else {
            return Vec::new();
        };
        for variant in &variants {
            self.bytes -= variant.size as u64;
            self.count -= 1;
        }
        self.dirty = true;
        variants
            .iter()
            .map(|variant| entry_id(key_hash, variant.slot))
            .collect()
    }

    pub(crate) fn clear(&mut self) -> Vec<EntryId> {
        let ids = self.all();
        self.entries.clear();
        self.bytes = 0;
        self.count = 0;
        self.dirty = true;
        ids
    }

    pub(crate) fn touch(&mut self, id: EntryId) {
        let slot = entry_slot(id);
        if let Some(variants) = self.entries.get_mut(&entry_key_hash(id)) &&
            let Some(variant) = variants.iter_mut().find(|variant| variant.slot == slot)
        {
            variant.last_used = now_secs();
            self.dirty = true;
        }
    }

    /// The least recently used entries to drop so that the store fits in its
    /// budgets again, with headroom so eviction does not run on every commit.
    pub(crate) fn evict_to(&mut self, max_bytes: u64, max_entries: usize) -> Vec<EntryId> {
        if self.bytes <= max_bytes && self.count <= max_entries {
            return Vec::new();
        }
        let target_bytes = max_bytes / 10 * 9;
        let target_entries = max_entries / 10 * 9;

        let mut candidates: Vec<(u32, EntryId, u32)> = self
            .entries
            .iter()
            .flat_map(|(hash, variants)| {
                variants.iter().map(move |variant| {
                    (
                        variant.last_used,
                        entry_id(*hash, variant.slot),
                        variant.size,
                    )
                })
            })
            .collect();
        candidates.sort_unstable_by_key(|(last_used, id, _)| (*last_used, *id));

        let mut evicted = Vec::new();
        for (_, id, _) in candidates {
            if self.bytes <= target_bytes && self.count <= target_entries {
                break;
            }
            if self.remove(id) {
                evicted.push(id);
            }
        }
        evicted
    }

    pub(crate) fn serialize(&self, dir_mtime: u64) -> Option<Vec<u8>> {
        let records: Vec<IndexRecord> = self
            .entries
            .iter()
            .flat_map(|(hash, variants)| {
                variants.iter().map(move |variant| IndexRecord {
                    key_hash: *hash,
                    slot: variant.slot,
                    size: variant.size,
                    last_used: variant.last_used,
                })
            })
            .collect();
        let payload = postcard::to_stdvec(&records).ok()?;
        let mut hasher = Hasher::new();
        hasher.update(&payload);

        let mut bytes = Vec::with_capacity(INDEX_HEADER_LEN + payload.len());
        bytes.extend_from_slice(&INDEX_MAGIC.to_le_bytes());
        bytes.extend_from_slice(&(CACHE_FORMAT as u32).to_le_bytes());
        bytes.extend_from_slice(&(records.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&dir_mtime.to_le_bytes());
        bytes.extend_from_slice(&hasher.finalize().to_le_bytes());
        bytes.extend_from_slice(&payload);
        Some(bytes)
    }

    fn deserialize(bytes: &[u8]) -> Option<(Index, u64)> {
        if bytes.len() < INDEX_HEADER_LEN {
            return None;
        }
        if u64::from_le_bytes(bytes[..8].try_into().ok()?) != INDEX_MAGIC {
            return None;
        }
        if u32::from_le_bytes(bytes[8..12].try_into().ok()?) != CACHE_FORMAT as u32 {
            return None;
        }
        let dir_mtime = u64::from_le_bytes(bytes[16..24].try_into().ok()?);
        let crc = u32::from_le_bytes(bytes[24..28].try_into().ok()?);
        let payload = &bytes[INDEX_HEADER_LEN..];
        let mut hasher = Hasher::new();
        hasher.update(payload);
        if hasher.finalize() != crc {
            return None;
        }

        let records: Vec<IndexRecord> = postcard::from_bytes(payload).ok()?;
        let mut index = Index::default();
        for record in records {
            index.insert(
                record.key_hash,
                Variant {
                    slot: record.slot,
                    size: record.size,
                    last_used: record.last_used,
                },
            );
        }
        index.dirty = false;
        Some((index, dir_mtime))
    }

    pub(crate) fn mark_clean(&mut self) {
        self.dirty = false;
    }
}

/// Parse an entry file name back into the key hash and slot it stands for.
pub(crate) fn parse_entry_name(name: &str) -> Option<(u64, u8)> {
    let (hash, slot) = name.split_once('_')?;
    Some((
        u64::from_str_radix(hash, 16).ok()?,
        slot.parse::<u8>().ok()?,
    ))
}

pub(crate) fn entry_file_name(key_hash: u64, slot: u8) -> String {
    format!("{key_hash:014x}_{slot}")
}

/// Nanoseconds, not seconds: two changes within the same second must still be
/// distinguishable, or a directory that changed after the index was written can
/// look like one that did not.
pub(crate) fn dir_mtime(dir: &Path) -> u64 {
    fs::metadata(dir)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|since| since.as_nanos() as u64)
        .unwrap_or_default()
}

/// Load the index, repairing or rebuilding it as needed. Blocking; only run at
/// store construction.
pub(crate) fn load(dir: &Path, entries_dir: &Path, was_clean: bool) -> Index {
    let mut index = match fs::read(dir.join(INDEX_FILE)).ok().and_then(|bytes| {
        let bytes = Index::deserialize(&bytes)?;
        Some(bytes)
    }) {
        // Trust the index only if the previous run said it was up to date *and*
        // nothing changed the directory since: the flag catches our own unclean
        // exit, the mtime catches the system emptying `cacheDir` behind our back.
        Some((index, stamped_mtime)) if was_clean && stamped_mtime == dir_mtime(entries_dir) => {
            // Marker asserted by ports/arkweb/tools/test_http_cache_index.py.
            info!(
                "http-cache: index loaded from disk, {} entries",
                index.count()
            );
            return index;
        },
        Some((index, _)) => index,
        None => {
            debug!("HTTP cache index is missing or unreadable, rebuilding it");
            Index::default()
        },
    };
    reconcile(&mut index, entries_dir);
    info!(
        "http-cache: index rebuilt by scanning, {} entries",
        index.count()
    );
    index
}

/// Bring an index back in line with what is actually on disk, which the system
/// is allowed to change behind the cache's back.
fn reconcile(index: &mut Index, entries_dir: &Path) {
    let Ok(dir) = fs::read_dir(entries_dir) else {
        // The whole directory is gone; every record is stale.
        index.clear();
        index.mark_clean();
        return;
    };

    let mut present: FxHashMap<u64, SmallVec<[u8; 1]>> = FxHashMap::default();
    let mut unknown: Vec<(u64, u8, PathBuf)> = Vec::new();
    for entry in dir.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.ends_with(".tmp") {
            // A writer's file from before temporary files moved to their own
            // directory.
            let _ = fs::remove_file(entry.path());
            continue;
        }
        let Some((key_hash, slot)) = parse_entry_name(name) else {
            continue;
        };
        present.entry(key_hash).or_default().push(slot);
        if index.get(entry_id(key_hash, slot)).is_none() {
            unknown.push((key_hash, slot, entry.path()));
        }
    }

    for id in index.all() {
        let known = present
            .get(&entry_key_hash(id))
            .is_some_and(|slots| slots.contains(&entry_slot(id)));
        if !known {
            index.remove(id);
        }
    }

    // Only files the index did not know about are stat'ed; the routine path never
    // touches the file system for sizes or recency.
    for (key_hash, slot, path) in unknown {
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        index.insert(
            key_hash,
            Variant {
                slot,
                size: metadata.len().min(u32::MAX as u64) as u32,
                last_used: metadata
                    .modified()
                    .ok()
                    .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
                    .map(|since| since.as_secs() as u32)
                    .unwrap_or_else(now_secs),
            },
        );
    }
}

/// The version stamp: magic, format, and one byte saying whether the previous run
/// closed the store cleanly.
fn stamp_bytes(clean: bool) -> Vec<u8> {
    let mut stamp = Vec::with_capacity(13);
    stamp.extend_from_slice(&INDEX_MAGIC.to_le_bytes());
    stamp.extend_from_slice(&(CACHE_FORMAT as u32).to_le_bytes());
    stamp.push(clean as u8);
    stamp
}

/// Record whether the index on disk currently describes the directory exactly.
/// Cleared as soon as anything changes, set again once a write has caught up.
pub(crate) fn write_stamp(dir: &Path, clean: bool) {
    let _ = fs::write(dir.join(STAMP_FILE), stamp_bytes(clean));
}

/// Check the version stamp, discarding the store's files when they are of another
/// format, and make sure the layout exists. The caller holds the directory's lock,
/// so the temporary files in it belong to writers that are gone.
///
/// Returns whether the previous run left the index describing the directory. This
/// is Firefox's dirty-flag trick: a directory mtime cannot tell an unclean exit
/// from a clean one, because a writer may have been in flight when the index was
/// last written.
pub(crate) fn prepare_directory(dir: &Path) -> io::Result<bool> {
    let stamp_path = dir.join(STAMP_FILE);
    let version = &stamp_bytes(false)[..12];
    let was_clean = match fs::read(&stamp_path) {
        Ok(stamp) if stamp.len() >= 12 && &stamp[..12] == version => {
            stamp.get(12).copied() == Some(1)
        },
        Ok(stamp) if is_own_stamp(&stamp) => {
            // Another format of this store. There is no migration.
            debug!("HTTP cache directory has another format, discarding it");
            remove_store_files(dir);
            false
        },
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "the HTTP cache directory holds an `index` file of something else",
            ));
        },
        // No stamp at all: either a fresh directory, or one the system emptied
        // while Servo was not running. Neither can be trusted.
        Err(_) => false,
    };
    let _ = fs::remove_dir_all(dir.join(TMP_DIR));
    fs::create_dir_all(dir.join(ENTRIES_DIR))?;
    fs::create_dir_all(dir.join(TMP_DIR))?;
    // We are about to start changing the directory.
    write_stamp(dir, false);
    Ok(was_clean)
}

/// Whether `stamp` was written by this store, in any format. A stamp cut short by
/// a kill while it was being written still counts.
fn is_own_stamp(stamp: &[u8]) -> bool {
    let magic = INDEX_MAGIC.to_le_bytes();
    let len = stamp.len().min(magic.len());
    stamp[..len] == magic[..len]
}

/// Remove the files the store keeps in `dir`, and nothing else there.
fn remove_store_files(dir: &Path) {
    let _ = fs::remove_dir_all(dir.join(ENTRIES_DIR));
    let _ = fs::remove_dir_all(dir.join(TMP_DIR));
    let _ = fs::remove_file(dir.join(INDEX_FILE));
    let _ = fs::remove_file(dir.join(format!("{INDEX_FILE}.tmp")));
}

/// Write the index through a temporary file and a rename, so a crash leaves
/// either the old index or the new one. Blocking; runs on the I/O thread.
///
/// `bytes_of` serializes the index and, while it holds the index lock, records the
/// directory's modification time. Reading the two together is what keeps the
/// recorded time from describing a directory the index does not: a commit renames
/// its file and inserts its record in one job on the I/O thread, which is where
/// this runs too, and any later change to the directory makes the time no longer
/// match, so the next start rescans.
pub(crate) fn save(dir: &Path, bytes_of: impl FnOnce() -> Option<Vec<u8>>) -> bool {
    let Some(bytes) = bytes_of() else {
        return false;
    };
    let temporary = dir.join(format!("{INDEX_FILE}.tmp"));
    // No fsync: the index is a derived artifact, and fdatasync costs 8 ms on eMMC.
    if fs::write(&temporary, &bytes).is_err() {
        return false;
    }
    fs::rename(&temporary, dir.join(INDEX_FILE)).is_ok()
}
