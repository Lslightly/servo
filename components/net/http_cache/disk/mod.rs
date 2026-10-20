/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! A disk-primary store: one file per entry, an index in memory, and a dedicated
//! I/O thread.
//!
//! Bodies live only on disk, so the page cache is the memory tier and the net
//! process never holds a complete cached body. Nothing here is ever allowed to
//! fail a fetch: a missing, truncated or unreadable entry is reported as a miss.
//!
//! The store must survive its directory being emptied at any instant, which is
//! what OpenHarmony does to `cacheDir` under storage pressure and on the user's
//! "Clear cache". The index is therefore a derived artifact, rebuilt by scanning,
//! and every read path treats `ENOENT` as a miss.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use crc32fast::Hasher;
use futures::future::BoxFuture;
use log::debug;
use malloc_size_of::{MallocSizeOf, MallocSizeOfOps};
use net_traits::CacheEntryDescriptor;
use servo_config::pref;

use crate::http_cache::disk::index::{Index, Variant, entry_file_name, now_secs};
use crate::http_cache::disk::io_thread::IoThread;
use crate::http_cache::key::{CacheKey, EntryId, entry_id, entry_key_hash, entry_slot};
use crate::http_cache::store::{
    BATCH_BYTES, BodyRange, BodyReader, CacheStore, EntryMeta, EntryWriter, StoreError, WriteOp,
    clamp_range, entry_cap_for, split_into_chunks,
};

mod entry;
mod index;
mod io_thread;

/// How long the index may stay dirty in memory before it is written out. A crash
/// inside the window costs a directory scan on the next start, never an entry.
const INDEX_SAVE_INTERVAL: Duration = Duration::from_secs(20);

/// Entries are metered per uid on OpenHarmony, so the entry count is capped as
/// well as the byte budget.
const MAX_ENTRIES: usize = 20_000;

/// Chromium's `PreferredCacheSize` constants, used for the desktop default.
const DEFAULT_CACHE_SIZE: u64 = 80 * 1024 * 1024;
const MAX_CACHE_SIZE: u64 = 320 * 1024 * 1024;
/// Chromium scales its default up by this much outside Windows.
const CACHE_SIZE_SCALE: u64 = 4;
/// What Android WebView and ArkWeb document per app.
const MOBILE_CACHE_SIZE: u64 = 100 * 1024 * 1024;

/// Open the store for the public cache from the embedder's configuration, or
/// `None` when there is no cache directory to use.
pub fn open_default_store() -> Option<Arc<dyn CacheStore>> {
    let opts = servo_config::opts::get();
    if opts.temporary_storage || pref!(network_http_cache_disabled) {
        return None;
    }
    let dir = opts.http_cache_dir.clone()?;
    // The free-space ladder measures the volume the cache is on, so the directory
    // has to exist before it is asked.
    if let Err(error) = fs::create_dir_all(&dir) {
        debug!("could not create the HTTP cache directory: {error}");
        return None;
    }
    let max_bytes = match pref!(network_http_disk_cache_size) {
        0 => automatic_cache_size(&dir),
        configured => configured,
    };
    match DiskStore::open(dir, max_bytes) {
        Ok(store) => Some(Arc::new(store) as Arc<dyn CacheStore>),
        Err(error) => {
            debug!("could not open the HTTP disk cache, falling back to memory: {error}");
            None
        },
    }
}

/// How big the cache may grow when the embedder did not say. Mobile gets a fixed
/// budget; elsewhere the size follows the free space of the volume the cache is
/// on, using Chromium's ladder.
fn automatic_cache_size(dir: &Path) -> u64 {
    if cfg!(any(target_env = "ohos", target_os = "android")) {
        return MOBILE_CACHE_SIZE;
    }
    let Some(available) = available_bytes(dir) else {
        return DEFAULT_CACHE_SIZE * CACHE_SIZE_SCALE;
    };
    let preferred = if available < DEFAULT_CACHE_SIZE * 10 {
        available / 10 * 8
    } else if available < DEFAULT_CACHE_SIZE * 100 {
        DEFAULT_CACHE_SIZE
    } else {
        (available / 100).min(MAX_CACHE_SIZE)
    };
    (preferred * CACHE_SIZE_SCALE).min(MAX_CACHE_SIZE * CACHE_SIZE_SCALE)
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn available_bytes(dir: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let path = CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `path` is a valid NUL-terminated string and `stats` is a valid,
    // writable `statvfs` for the duration of the call.
    if unsafe { libc::statvfs(path.as_ptr(), &mut stats) } != 0 {
        return None;
    }
    (stats.f_bavail as u64).checked_mul(stats.f_frsize as u64)
}

#[cfg(not(unix))]
fn available_bytes(_dir: &Path) -> Option<u64> {
    None
}

/// Holds a store's directory for as long as the store is open, so that two
/// processes never write it at once: they would overwrite each other's entries.
struct DirectoryLock {
    _file: File,
}

impl DirectoryLock {
    fn acquire(dir: &Path) -> io::Result<DirectoryLock> {
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).write(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // Without sharing, no other process can open the file while this one has it.
            options.share_mode(0);
        }
        let file = options.open(dir.join(index::LOCK_FILE))?;
        #[cfg(unix)]
        lock_exclusively(&file)?;
        Ok(DirectoryLock { _file: file })
    }
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn lock_exclusively(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    // SAFETY: the descriptor stays valid while `file` is borrowed.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::WouldBlock {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "the HTTP cache directory is in use by another process",
        ));
    }
    Err(error)
}

/// What tells a directory apart from one created in its place.
#[cfg(unix)]
fn directory_identity(dir: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;

    fs::metadata(dir)
        .ok()
        .map(|metadata| (metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn directory_identity(dir: &Path) -> Option<(u64, u64)> {
    fs::metadata(dir).ok().map(|_| (0, 0))
}

/// A body being written to a temporary file.
struct EntryFile {
    file: File,
    body_len: u64,
    crc: Hasher,
}

impl EntryFile {
    fn append(&mut self, chunks: &[Bytes]) -> io::Result<()> {
        for chunk in chunks {
            self.file.write_all(chunk)?;
            self.crc.update(chunk);
            self.body_len += chunk.len() as u64;
        }
        Ok(())
    }
}

/// Everything a store and its in-flight writers share.
struct StoreCore {
    dir: PathBuf,
    entries_dir: PathBuf,
    /// Where bodies are written until they are committed. Emptied on open.
    tmp_dir: PathBuf,
    /// The identity of `entries_dir`, to notice the system replacing it.
    entries_identity: Mutex<Option<(u64, u64)>>,
    io: IoThread,
    index: Mutex<Index>,
    /// Bumped on every index change. A scheduled flush only fires once this stops
    /// moving, which makes the delay "20 s after the last change" rather than
    /// "at most every 20 s", matching Chromium's `PostponeWritingToDisk`.
    dirty_generation: AtomicU64,
    /// Whether a debounced flush task is already waiting.
    flush_scheduled: AtomicBool,
    /// Whether the on-disk stamp currently says the index is up to date.
    stamp_clean: AtomicBool,
    /// Kept alongside the index so `disk_bytes` needs no lock.
    disk_bytes: AtomicU64,
    max_bytes: u64,
    max_entries: usize,
    /// The largest response that may be stored at all.
    max_entry_bytes: u64,
}

/// A file-per-entry [`CacheStore`].
pub struct DiskStore {
    core: Arc<StoreCore>,
    _lock: DirectoryLock,
}

impl DiskStore {
    /// Open, and if necessary create or repair, a store at `dir`. Fails if another
    /// store has the directory open, or if the directory holds files of something
    /// else under the store's names.
    pub fn open(dir: PathBuf, max_bytes: u64) -> io::Result<DiskStore> {
        fs::create_dir_all(&dir)?;
        let lock = DirectoryLock::acquire(&dir)?;
        let was_clean = index::prepare_directory(&dir)?;
        let entries_dir = dir.join(index::ENTRIES_DIR);
        let loaded = index::load(&dir, &entries_dir, was_clean);
        let disk_bytes = AtomicU64::new(loaded.bytes());
        let max_entry_bytes = entry_cap_for(max_bytes);
        Ok(DiskStore {
            core: Arc::new(StoreCore {
                io: IoThread::spawn("HttpCacheIO".to_owned()),
                index: Mutex::new(loaded),
                dirty_generation: AtomicU64::new(0),
                flush_scheduled: AtomicBool::new(false),
                stamp_clean: AtomicBool::new(false),
                tmp_dir: dir.join(index::TMP_DIR),
                entries_identity: Mutex::new(directory_identity(&entries_dir)),
                dir,
                entries_dir,
                max_bytes,
                max_entries: MAX_ENTRIES,
                max_entry_bytes,
                disk_bytes,
            }),
            _lock: lock,
        })
    }
}

impl StoreCore {
    fn path_for(&self, key_hash: u64, slot: u8) -> PathBuf {
        self.entries_dir.join(entry_file_name(key_hash, slot))
    }

    fn temp_path_for(&self, key_hash: u64, slot: u8) -> PathBuf {
        self.tmp_dir
            .join(format!("{}.tmp", entry_file_name(key_hash, slot)))
    }

    fn path_of(&self, id: EntryId) -> PathBuf {
        self.path_for(entry_key_hash(id), entry_slot(id))
    }

    /// Notice that the system emptied the directory while the store runs, which
    /// OpenHarmony does to `cacheDir`, and start over with an empty index rather
    /// than keep counting, and later persist as trusted, entries that are gone.
    /// Blocking; runs on the I/O thread.
    fn check_directory(&self) {
        let mut known = self.entries_identity.lock().unwrap();
        let current = directory_identity(&self.entries_dir);
        if current.is_some() && current == *known {
            return;
        }
        debug!("the HTTP cache directory was emptied underneath the store");
        {
            let mut index = self.index.lock().unwrap();
            index.clear();
            self.disk_bytes.store(0, Ordering::Relaxed);
        }
        let _ = fs::create_dir_all(&self.entries_dir);
        let _ = fs::create_dir_all(&self.tmp_dir);
        index::write_stamp(&self.dir, false);
        self.stamp_clean.store(false, Ordering::Release);
        *known = directory_identity(&self.entries_dir);
    }

    fn insert_variant(&self, key_hash: u64, slot: u8, size: u64) {
        let mut index = self.index.lock().unwrap();
        index.insert(
            key_hash,
            Variant {
                slot,
                size: size.min(u32::MAX as u64) as u32,
                last_used: now_secs(),
            },
        );
        self.disk_bytes.store(index.bytes(), Ordering::Relaxed);
    }

    /// Drop an entry the index believed in but the file system does not have.
    fn forget(&self, id: EntryId) {
        let mut index = self.index.lock().unwrap();
        index.remove(id);
        self.disk_bytes.store(index.bytes(), Ordering::Relaxed);
    }

    fn unlink_all(&self, ids: Vec<EntryId>) {
        if ids.is_empty() {
            return;
        }
        let paths: Vec<PathBuf> = ids.into_iter().map(|id| self.path_of(id)).collect();
        // Unlinking costs 16 to 66 us per file on device, so a large eviction must
        // never happen on the fetch path.
        self.io.detach(move || {
            for path in paths {
                let _ = fs::remove_file(path);
            }
        });
    }

    /// Note that the index no longer matches the directory, and arrange for it to
    /// be written once things go quiet.
    fn mark_index_dirty(self: &Arc<Self>) {
        self.dirty_generation.fetch_add(1, Ordering::AcqRel);
        // The index on disk is now stale; say so, so that a kill before the flush
        // makes the next start reconcile instead of trusting it.
        if self.stamp_clean.swap(false, Ordering::AcqRel) {
            let dir = self.dir.clone();
            self.io.detach(move || index::write_stamp(&dir, false));
        }
        if self.flush_scheduled.swap(true, Ordering::AcqRel) {
            return;
        }
        let core = self.clone();
        tokio::spawn(async move {
            // Wait for the changes to stop before writing, so a burst of commits
            // costs one index write rather than one per 20 s window.
            loop {
                let generation = core.dirty_generation.load(Ordering::Acquire);
                tokio::time::sleep(INDEX_SAVE_INTERVAL).await;
                if core.dirty_generation.load(Ordering::Acquire) == generation {
                    break;
                }
            }
            core.flush_scheduled.store(false, Ordering::Release);
            let writer = core.clone();
            core.io.detach(move || writer.write_index());
        });
    }

    /// Write the index and, if nothing changed while it was being written, record
    /// that it now describes the directory exactly.
    fn write_index(&self) {
        self.check_directory();
        let generation = self.dirty_generation.load(Ordering::Acquire);
        let wrote = index::save(&self.dir, || {
            let mut index = self.index.lock().unwrap();
            let bytes = index.serialize(index::dir_mtime(&self.entries_dir));
            if bytes.is_some() {
                index.mark_clean();
            }
            bytes
        });
        if wrote && self.dirty_generation.load(Ordering::Acquire) == generation {
            index::write_stamp(&self.dir, true);
            self.stamp_clean.store(true, Ordering::Release);
        }
    }

    /// Apply the byte and entry budgets. Runs after a commit, off the fetch path.
    fn evict(&self) {
        let evicted = {
            let mut index = self.index.lock().unwrap();
            let evicted = index.evict_to(self.max_bytes, self.max_entries);
            self.disk_bytes.store(index.bytes(), Ordering::Relaxed);
            evicted
        };
        if !evicted.is_empty() {
            debug!("evicting {} HTTP cache entries", evicted.len());
        }
        self.unlink_all(evicted);
    }
}

impl MallocSizeOf for DiskStore {
    fn size_of(&self, _ops: &mut MallocSizeOfOps) -> usize {
        // Only the index is in memory; the bodies are files. The per-entry cost is
        // the record plus its share of the map and the per-key `SmallVec`.
        self.core.index.lock().unwrap().count() * (std::mem::size_of::<Variant>() + 24)
    }
}

impl CacheStore for DiskStore {
    fn lookup<'a>(&'a self, key: &'a CacheKey) -> BoxFuture<'a, Vec<(EntryId, EntryMeta)>> {
        let core = self.core.clone();
        Box::pin(async move {
            let key_hash = key.hash();
            let slots: Vec<u8> = core
                .index
                .lock()
                .unwrap()
                .variants(key_hash)
                .map(|variant| variant.slot)
                .collect();

            let mut found = Vec::with_capacity(slots.len());
            for slot in slots {
                let id = entry_id(key_hash, slot);
                let path = core.path_for(key_hash, slot);
                let tail = core
                    .io
                    .run(move || {
                        let mut file = File::open(&path)?;
                        entry::read_tail(&mut file)
                    })
                    .await;
                match tail {
                    Some(Ok(tail)) if tail.meta.key == *key => found.push((id, tail.meta)),
                    // A hash collision: the file belongs to another URL.
                    Some(Ok(_)) => {},
                    Some(Err(error)) => {
                        debug!("dropping unreadable cache entry: {error}");
                        core.forget(id);
                        core.unlink_all(vec![id]);
                    },
                    None => {},
                }
            }
            found
        })
    }

    fn open(
        &self,
        id: EntryId,
        meta: &EntryMeta,
        range: Option<BodyRange>,
    ) -> BoxFuture<'_, Result<BodyReader, StoreError>> {
        let core = self.core.clone();
        let path = core.path_of(id);
        let encoding = meta.content_encoding;
        let body_len = meta.body_len;
        let stamp = meta.stamp;
        let range = range.map(|range| clamp_range(range, body_len).ok_or(StoreError::Missing));
        Box::pin(async move {
            let range = range.transpose()?;
            let start = entry::HEADER_LEN + range.map(|range| range.start).unwrap_or(0);
            let len = range
                .map(|range| range.end - range.start + 1)
                .unwrap_or(body_len);

            let opened = core
                .io
                .run(move || -> io::Result<Option<(File, u32)>> {
                    let mut file = File::open(&path)?;
                    let tail = entry::read_tail(&mut file)?;
                    if tail.meta.stamp != stamp {
                        // Another entry took the id since the caller looked it up.
                        return Ok(None);
                    }
                    if tail.body_len != body_len {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "cache entry changed underneath us",
                        ));
                    }
                    file.seek(SeekFrom::Start(start))?;
                    Ok(Some((file, tail.body_crc)))
                })
                .await
                .ok_or(StoreError::Missing)?;

            let (file, body_crc) = match opened {
                Ok(Some(opened)) => opened,
                Ok(None) => return Err(StoreError::Missing),
                Err(error) => {
                    debug!("could not open a cache entry: {error}");
                    core.forget(id);
                    return Err(StoreError::Missing);
                },
            };
            // A checksum only means anything for a read that covers the whole body.
            let expected_crc = range.is_none().then_some(body_crc);
            Ok(BodyReader::new(
                len,
                encoding,
                read_stream(core, id, file, len, expected_crc),
            ))
        })
    }

    fn create(&self, meta: EntryMeta) -> BoxFuture<'_, Result<EntryWriter, StoreError>> {
        let core = self.core.clone();
        Box::pin(async move {
            let key_hash = meta.key.hash();
            let slot = core
                .index
                .lock()
                .unwrap()
                .reserve_slot(key_hash)
                .ok_or(StoreError::Rejected)?;

            let temporary = core.temp_path_for(key_hash, slot);
            let creator = core.clone();
            let created = core
                .io
                .run({
                    let temporary = temporary.clone();
                    move || -> io::Result<File> {
                        let mut file = match File::create(&temporary) {
                            Ok(file) => file,
                            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                                // The directory was removed underneath us, which is
                                // routine on OpenHarmony.
                                creator.check_directory();
                                File::create(&temporary)?
                            },
                            Err(error) => return Err(error),
                        };
                        entry::write_header(&mut file)?;
                        Ok(file)
                    }
                })
                .await;

            let file = match created {
                Some(Ok(file)) => file,
                other => {
                    if let Some(Err(error)) = other {
                        debug!("could not create a cache entry: {error}");
                    }
                    core.index.lock().unwrap().release_slot(key_hash, slot);
                    return Err(StoreError::Rejected);
                },
            };

            let (writer, ops) = EntryWriter::channel();
            tokio::spawn(write_entry(
                core.clone(),
                Slot {
                    key_hash,
                    slot,
                    temporary,
                },
                EntryFile {
                    file,
                    body_len: 0,
                    crc: Hasher::new(),
                },
                meta,
                ops,
            ));
            Ok(writer)
        })
    }

    fn update_meta(&self, id: EntryId, meta: EntryMeta) -> BoxFuture<'_, Result<(), StoreError>> {
        let core = self.core.clone();
        let path = core.path_of(id);
        Box::pin(async move {
            let size = core
                .io
                .run(move || -> io::Result<Option<u64>> {
                    let mut file = OpenOptions::new().read(true).write(true).open(&path)?;
                    if entry::read_tail(&mut file)?.meta.stamp != meta.stamp {
                        // Another entry took the id since the caller looked it up.
                        return Ok(None);
                    }
                    entry::rewrite_tail(&mut file, &meta)?;
                    file.seek(SeekFrom::End(0)).map(Some)
                })
                .await
                .ok_or(StoreError::Missing)?;

            match size {
                Ok(None) => Err(StoreError::Missing),
                Ok(Some(size)) => {
                    {
                        let mut index = core.index.lock().unwrap();
                        if index.get(id).is_some() {
                            index.insert(
                                entry_key_hash(id),
                                Variant {
                                    slot: entry_slot(id),
                                    size: size.min(u32::MAX as u64) as u32,
                                    last_used: now_secs(),
                                },
                            );
                        }
                        core.disk_bytes.store(index.bytes(), Ordering::Relaxed);
                    }
                    core.mark_index_dirty();
                    Ok(())
                },
                Err(error) => {
                    debug!("could not freshen a cache entry: {error}");
                    core.forget(id);
                    // The tail was being rewritten, so the file may no longer parse.
                    core.unlink_all(vec![id]);
                    Err(StoreError::Missing)
                },
            }
        })
    }

    fn remove(&self, id: EntryId, stamp: u64) -> BoxFuture<'_, ()> {
        let core = self.core.clone();
        let path = core.path_of(id);
        Box::pin(async move {
            // An entry that does not read back is removed whatever its stamp.
            let replaced = core
                .io
                .run(move || -> bool {
                    let replaced = File::open(&path)
                        .and_then(|mut file| entry::read_tail(&mut file))
                        .is_ok_and(|tail| tail.meta.stamp != stamp);
                    if !replaced {
                        let _ = fs::remove_file(&path);
                    }
                    replaced
                })
                .await
                .unwrap_or(false);
            if !replaced {
                core.forget(id);
            }
        })
    }

    fn remove_key<'a>(&'a self, key: &'a CacheKey) -> BoxFuture<'a, ()> {
        let core = self.core.clone();
        Box::pin(async move {
            let removed = {
                let mut index = core.index.lock().unwrap();
                let removed = index.remove_key(key.hash());
                core.disk_bytes.store(index.bytes(), Ordering::Relaxed);
                removed
            };
            core.unlink_all(removed);
        })
    }

    fn clear(&self) -> BoxFuture<'_, ()> {
        let core = self.core.clone();
        Box::pin(async move {
            {
                let mut index = core.index.lock().unwrap();
                index.clear();
                core.disk_bytes.store(0, Ordering::Relaxed);
            }
            let entries_dir = core.entries_dir.clone();
            let writer = core.clone();
            core.io
                .run(move || {
                    let _ = fs::remove_dir_all(&entries_dir);
                    let _ = fs::create_dir_all(&entries_dir);
                    *writer.entries_identity.lock().unwrap() = directory_identity(&entries_dir);
                    // On the I/O thread, so that it cannot race the index write the
                    // debounce may already have queued there.
                    writer.write_index();
                })
                .await;
        })
    }

    fn touch(&self, id: EntryId) {
        self.core.index.lock().unwrap().touch(id);
    }

    fn descriptors(&self) -> BoxFuture<'_, Vec<CacheEntryDescriptor>> {
        let core = self.core.clone();
        Box::pin(async move {
            let paths: Vec<PathBuf> = core
                .index
                .lock()
                .unwrap()
                .all()
                .into_iter()
                .map(|id| core.path_of(id))
                .collect();
            // One hand-off for the whole listing: this reads every entry's
            // metadata, and it is only ever asked for by devtools.
            core.io
                .run(move || {
                    paths
                        .into_iter()
                        .filter_map(|path| {
                            let mut file = File::open(&path).ok()?;
                            let tail = entry::read_tail(&mut file).ok()?;
                            Some(CacheEntryDescriptor::new(tail.meta.key.url().to_string()))
                        })
                        .collect()
                })
                .await
                .unwrap_or_default()
        })
    }

    fn disk_bytes(&self) -> u64 {
        self.core.disk_bytes.load(Ordering::Relaxed)
    }

    fn max_entry_bytes(&self) -> u64 {
        self.core.max_entry_bytes
    }

    fn flush(&self) -> BoxFuture<'_, ()> {
        let core = self.core.clone();
        Box::pin(async move {
            if !core.index.lock().unwrap().is_dirty() {
                return;
            }
            let writer = core.clone();
            core.io.run(move || writer.write_index()).await;
        })
    }

    fn shutdown(&self) -> BoxFuture<'_, ()> {
        self.flush()
    }
}

/// Which entry a writer is filling.
struct Slot {
    key_hash: u64,
    slot: u8,
    temporary: PathBuf,
}

/// Drive one entry's body from the writer's queue onto disk, and publish it if it
/// is committed. Dropping the queue without committing throws the entry away.
async fn write_entry(
    core: Arc<StoreCore>,
    slot: Slot,
    mut entry_file: EntryFile,
    meta: EntryMeta,
    mut ops: tokio::sync::mpsc::Receiver<WriteOp>,
) {
    let mut failed = false;
    let mut committed = false;

    while let Some(op) = ops.recv().await {
        match op {
            WriteOp::Append(chunks) => {
                if failed {
                    continue;
                }
                let added: u64 = chunks.iter().map(|chunk| chunk.len() as u64).sum();
                if entry_file.body_len + added > core.max_entry_bytes {
                    debug!("cache entry outgrew the per-entry cap, dropping it");
                    failed = true;
                    continue;
                }
                match core
                    .io
                    .run(move || {
                        let outcome = entry_file.append(&chunks);
                        (entry_file, outcome)
                    })
                    .await
                {
                    Some((file, Ok(()))) => entry_file = file,
                    Some((file, Err(error))) => {
                        debug!("could not write a cache entry: {error}");
                        entry_file = file;
                        failed = true;
                    },
                    None => break,
                }
            },
            WriteOp::Commit(reply) => {
                if failed {
                    let _ = reply.send(Err(StoreError::Rejected));
                    break;
                }
                let mut meta = meta.clone();
                meta.body_len = entry_file.body_len;
                let final_path = core.path_for(slot.key_hash, slot.slot);
                let temporary = slot.temporary.clone();
                let committer = core.clone();
                let (key_hash, slot_number) = (slot.key_hash, slot.slot);
                let committed_size = core
                    .io
                    .run(move || -> io::Result<u64> {
                        let body_len = entry_file.body_len;
                        let body_crc = entry_file.crc.finalize();
                        entry::write_tail(&mut entry_file.file, &meta, body_len, body_crc)?;
                        let size = entry_file.file.seek(SeekFrom::End(0))?;
                        // Renaming an open file fails on Windows.
                        drop(entry_file.file);
                        if let Err(error) = fs::rename(&temporary, &final_path) {
                            if error.kind() == io::ErrorKind::NotFound {
                                committer.check_directory();
                            }
                            return Err(error);
                        }
                        // In the job that renames, so that an index write, which runs
                        // on this thread too, sees either both or neither.
                        committer.insert_variant(key_hash, slot_number, size);
                        Ok(size)
                    })
                    .await;

                let result = match committed_size {
                    Some(Ok(_)) => Ok(entry_id(slot.key_hash, slot.slot)),
                    other => {
                        if let Some(Err(error)) = other {
                            debug!("could not commit a cache entry: {error}");
                        }
                        Err(StoreError::Rejected)
                    },
                };
                committed = result.is_ok();
                let _ = reply.send(result);
                break;
            },
        }
    }

    core.index
        .lock()
        .unwrap()
        .release_slot(slot.key_hash, slot.slot);
    if committed {
        core.evict();
        core.mark_index_dirty();
    } else {
        let temporary = slot.temporary;
        core.io.detach(move || {
            let _ = fs::remove_file(temporary);
        });
    }
}

/// Stream a body off disk, keeping one batch read in flight while the previous one
/// is consumed, so a cold read overlaps decoding.
fn read_stream(
    core: Arc<StoreCore>,
    id: EntryId,
    file: File,
    len: u64,
    expected_crc: Option<u32>,
) -> futures::stream::BoxStream<'static, io::Result<Bytes>> {
    struct State {
        core: Arc<StoreCore>,
        id: EntryId,
        file: Option<File>,
        queue: VecDeque<Bytes>,
        remaining: u64,
        hasher: Option<Hasher>,
        expected_crc: Option<u32>,
        failed: bool,
    }

    impl State {
        /// A body that could not be read is not a body: the entry is dropped so the
        /// next fetch of this URL goes to the network instead of failing again.
        fn discard_entry(&mut self) {
            self.failed = true;
            self.core.forget(self.id);
            self.core.unlink_all(vec![self.id]);
        }
    }

    let state = State {
        core,
        id,
        file: Some(file),
        queue: VecDeque::new(),
        remaining: len,
        hasher: expected_crc.map(|_| Hasher::new()),
        expected_crc,
        failed: false,
    };

    Box::pin(futures::stream::unfold(state, |mut state| async move {
        loop {
            if let Some(chunk) = state.queue.pop_front() {
                return Some((Ok(chunk), state));
            }
            if state.failed {
                return None;
            }
            if state.remaining == 0 {
                if let (Some(hasher), Some(expected)) =
                    (state.hasher.take(), state.expected_crc.take()) &&
                    hasher.finalize() != expected
                {
                    state.discard_entry();
                    return Some((
                        Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "cached body failed its checksum",
                        )),
                        state,
                    ));
                }
                return None;
            }

            let mut file = state.file.take()?;
            let want = state.remaining.min(BATCH_BYTES as u64) as usize;
            let read = state
                .core
                .io
                .run(move || {
                    let mut buffer = vec![0u8; want];
                    let outcome = io::Read::read_exact(&mut file, &mut buffer).map(|()| buffer);
                    (file, outcome)
                })
                .await;

            match read {
                Some((file, Ok(buffer))) => {
                    state.file = Some(file);
                    state.remaining -= buffer.len() as u64;
                    if let Some(hasher) = state.hasher.as_mut() {
                        hasher.update(&buffer);
                    }
                    state.queue.extend(split_into_chunks(Bytes::from(buffer)));
                },
                Some((_, Err(error))) => {
                    state.discard_entry();
                    return Some((Err(error), state));
                },
                None => {
                    // The I/O thread is gone, so the body would be truncated.
                    // Report it rather than handing on a short body.
                    state.failed = true;
                    return Some((Err(io::Error::other("the cache I/O thread stopped")), state));
                },
            }
        }
    }))
}
