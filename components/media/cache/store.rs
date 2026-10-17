/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The slots of the temporary file in which the cache keeps the blocks of large resources.

use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use log::warn;

use crate::resource::ResourceId;

pub(crate) const BLOCK_SIZE: u64 = 64 * 1024;

pub(crate) type SlotId = usize;

/// The memory fallback keeps at least this many slots for blocks of resources that are not
/// kept in memory, so that they can still play.
const MIN_FALLBACK_SLOTS: usize = 16;

enum FileState {
    NotCreated,
    Created(Arc<File>),
    Failed,
}

/// Hands out the slots of the anonymous temporary file, one block each, up to the budget. If
/// the file cannot be created or written, the slots are only an account of a RAM budget, and
/// the blocks keep their data in memory.
pub(crate) struct Store {
    directory: Option<PathBuf>,
    file: FileState,
    /// Whether a read or write of the file failed, so that blocks stay in memory.
    io_failed: bool,
    file_slots: usize,
    fallback_slots: usize,
    /// The block that owns each slot. A slot that is released while it is pinned stays out of
    /// `free` until it is unpinned.
    owners: Vec<Option<(ResourceId, u64)>>,
    /// The file I/O in progress on each slot.
    pins: Vec<u32>,
    free: Vec<SlotId>,
    used: usize,
    /// The slots' worth of memory that resources kept in memory may use, which the memory
    /// fallback does not offer to other blocks.
    reserved_slots: usize,
}

impl Store {
    pub(crate) fn new(file_budget: u64, fallback_budget: u64, directory: Option<PathBuf>) -> Self {
        Self {
            directory,
            file: FileState::NotCreated,
            io_failed: false,
            file_slots: (file_budget / BLOCK_SIZE).max(1) as usize,
            fallback_slots: (fallback_budget / BLOCK_SIZE).max(1) as usize,
            owners: Vec::new(),
            pins: Vec::new(),
            free: Vec::new(),
            used: 0,
            reserved_slots: 0,
        }
    }

    /// Whether blocks stay in memory because the file cannot be used.
    pub(crate) fn ram_mode(&self) -> bool {
        matches!(self.file, FileState::Failed) || self.io_failed
    }

    pub(crate) fn capacity(&self) -> usize {
        if self.ram_mode() {
            self.fallback_slots
                .saturating_sub(self.reserved_slots)
                .max(MIN_FALLBACK_SLOTS.min(self.fallback_slots))
        } else {
            self.file_slots
        }
    }

    pub(crate) fn set_reserved(&mut self, bytes: u64) {
        self.reserved_slots = bytes.div_ceil(BLOCK_SIZE) as usize;
    }

    pub(crate) fn used(&self) -> usize {
        self.used
    }

    /// The file to read the slots from, if it exists.
    pub(crate) fn file(&self) -> Option<Arc<File>> {
        match &self.file {
            FileState::Created(file) => Some(file.clone()),
            _ => None,
        }
    }

    /// The file to write blocks to, unless the cache keeps them in memory.
    pub(crate) fn writable_file(&self) -> Option<Arc<File>> {
        if self.io_failed {
            return None;
        }
        self.file()
    }

    #[cfg(test)]
    pub(crate) fn file_created(&self) -> bool {
        matches!(self.file, FileState::Created(_))
    }

    fn ensure_file(&mut self) {
        if !matches!(self.file, FileState::NotCreated) {
            return;
        }
        let file = match &self.directory {
            Some(directory) => tempfile::tempfile_in(directory),
            None => tempfile::tempfile(),
        };
        self.file = match file {
            Ok(file) => FileState::Created(Arc::new(file)),
            Err(error) => {
                warn!(
                    "Media cache: cannot create its temporary file ({error}); keeping at most {} MiB in memory instead",
                    self.fallback_slots as u64 * BLOCK_SIZE / (1024 * 1024)
                );
                FileState::Failed
            },
        };
    }

    /// A read or write of the file failed: keep new blocks in memory from now on.
    pub(crate) fn io_error(&mut self, error: io::Error) {
        if !self.io_failed {
            warn!("Media cache: cannot use its temporary file ({error}); keeping blocks in memory");
            self.io_failed = true;
        }
    }

    /// A free slot for a block, unless the budget is used up.
    pub(crate) fn allocate(&mut self, owner: (ResourceId, u64)) -> Option<SlotId> {
        self.ensure_file();
        if self.used >= self.capacity() {
            return None;
        }
        let slot = match self.free.pop() {
            Some(slot) => slot,
            None => {
                self.owners.push(None);
                self.pins.push(0);
                self.owners.len() - 1
            },
        };
        self.owners[slot] = Some(owner);
        self.used += 1;
        Some(slot)
    }

    pub(crate) fn release(&mut self, slot: SlotId) {
        if self.owners[slot].take().is_some() {
            self.used -= 1;
            if self.pins[slot] == 0 {
                self.free.push(slot);
            }
        }
    }

    pub(crate) fn pin(&mut self, slot: SlotId) {
        self.pins[slot] += 1;
    }

    pub(crate) fn unpin(&mut self, slot: SlotId) {
        self.pins[slot] -= 1;
        if self.pins[slot] == 0 && self.owners[slot].is_none() {
            self.free.push(slot);
        }
    }

    pub(crate) fn pinned(&self, slot: SlotId) -> bool {
        self.pins[slot] > 0
    }
}

pub(crate) fn slot_offset(slot: SlotId) -> u64 {
    slot as u64 * BLOCK_SIZE
}

#[cfg(unix)]
pub(crate) fn write_at(file: &File, data: &[u8], offset: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.write_all_at(data, offset)
}

#[cfg(unix)]
pub(crate) fn read_at(file: &File, dest: &mut [u8], offset: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(dest, offset)
}

#[cfg(windows)]
pub(crate) fn write_at(file: &File, data: &[u8], offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut done = 0;
    while done < data.len() {
        match file.seek_write(&data[done..], offset + done as u64)? {
            0 => return Err(io::ErrorKind::WriteZero.into()),
            written => done += written,
        }
    }
    Ok(())
}

#[cfg(windows)]
pub(crate) fn read_at(file: &File, dest: &mut [u8], offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut done = 0;
    while done < dest.len() {
        match file.seek_read(&mut dest[done..], offset + done as u64)? {
            0 => return Err(io::ErrorKind::UnexpectedEof.into()),
            read => done += read,
        }
    }
    Ok(())
}
