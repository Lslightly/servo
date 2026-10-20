/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The on-disk format of one cache entry.
//!
//! ```text
//! 0                magic u64 | format u32 | flags u32
//! HEADER_LEN       body, exactly as it was received
//! + body_len       meta, postcard(EntryMeta)
//! + meta_len       meta_len u32 | meta_crc u32 | body_crc u32 | body_len u64 | eof magic u64
//! ```
//!
//! The metadata sits *after* the body, unlike the header-first layout other
//! caches use, so that a 304 can replace it by truncating and appending rather
//! than rewriting the body. A file without a valid trailer was never committed
//! and is ignored.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};

use crc32fast::Hasher;

use crate::http_cache::store::{CACHE_FORMAT, EntryMeta};

const ENTRY_MAGIC: u64 = 0x5345_5256_4f43_4143;
const EOF_MAGIC: u64 = 0x4341_434f_5652_4553;

pub(crate) const HEADER_LEN: u64 = 16;
pub(crate) const TRAILER_LEN: u64 = 28;

/// How much of the tail is read in one go when opening an entry. Large enough
/// that a typical entry's metadata comes back with the trailer.
const TAIL_READ_LEN: u64 = 8 * 1024;

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}

pub(crate) fn header_bytes() -> [u8; HEADER_LEN as usize] {
    let mut header = [0u8; HEADER_LEN as usize];
    header[..8].copy_from_slice(&ENTRY_MAGIC.to_le_bytes());
    header[8..12].copy_from_slice(&(CACHE_FORMAT as u32).to_le_bytes());
    header
}

pub(crate) fn write_header(file: &mut File) -> io::Result<()> {
    file.write_all(&header_bytes())
}

/// Append the metadata and the trailer that makes the entry complete.
pub(crate) fn write_tail(
    file: &mut File,
    meta: &EntryMeta,
    body_len: u64,
    body_crc: u32,
) -> io::Result<()> {
    let meta_bytes =
        postcard::to_stdvec(meta).map_err(|_| invalid("cache metadata is not serializable"))?;
    let meta_len =
        u32::try_from(meta_bytes.len()).map_err(|_| invalid("cache metadata too big"))?;
    let mut meta_hasher = Hasher::new();
    meta_hasher.update(&meta_bytes);

    let mut trailer = [0u8; TRAILER_LEN as usize];
    trailer[..4].copy_from_slice(&meta_len.to_le_bytes());
    trailer[4..8].copy_from_slice(&meta_hasher.finalize().to_le_bytes());
    trailer[8..12].copy_from_slice(&body_crc.to_le_bytes());
    trailer[12..20].copy_from_slice(&body_len.to_le_bytes());
    trailer[20..].copy_from_slice(&EOF_MAGIC.to_le_bytes());

    file.write_all(&meta_bytes)?;
    file.write_all(&trailer)
}

/// What the tail of a committed entry says about it.
pub(crate) struct EntryTail {
    pub meta: EntryMeta,
    pub body_len: u64,
    pub body_crc: u32,
}

/// Read an entry's metadata. Fails for a file that was never committed, whose
/// format does not match, or whose metadata does not check out.
pub(crate) fn read_tail(file: &mut File) -> io::Result<EntryTail> {
    let file_len = file.seek(SeekFrom::End(0))?;
    if file_len < HEADER_LEN + TRAILER_LEN {
        return Err(invalid("cache entry is too short to be complete"));
    }

    let mut header = [0u8; HEADER_LEN as usize];
    file.seek(SeekFrom::Start(0))?;
    file.read_exact(&mut header)?;
    if u64::from_le_bytes(header[..8].try_into().unwrap()) != ENTRY_MAGIC {
        return Err(invalid("not a cache entry"));
    }
    if u32::from_le_bytes(header[8..12].try_into().unwrap()) != CACHE_FORMAT as u32 {
        return Err(invalid("cache entry has another format"));
    }

    let tail_len = (file_len - HEADER_LEN).min(TAIL_READ_LEN + TRAILER_LEN);
    let tail_start = file_len - tail_len;
    let mut tail = vec![0u8; tail_len as usize];
    file.seek(SeekFrom::Start(tail_start))?;
    file.read_exact(&mut tail)?;

    let trailer = &tail[tail.len() - TRAILER_LEN as usize..];
    if u64::from_le_bytes(trailer[20..].try_into().unwrap()) != EOF_MAGIC {
        return Err(invalid("cache entry was never committed"));
    }
    let meta_len = u32::from_le_bytes(trailer[..4].try_into().unwrap()) as u64;
    let meta_crc = u32::from_le_bytes(trailer[4..8].try_into().unwrap());
    let body_crc = u32::from_le_bytes(trailer[8..12].try_into().unwrap());
    let body_len = u64::from_le_bytes(trailer[12..20].try_into().unwrap());

    if HEADER_LEN
        .checked_add(body_len)
        .and_then(|offset| offset.checked_add(meta_len))
        .and_then(|offset| offset.checked_add(TRAILER_LEN)) !=
        Some(file_len)
    {
        return Err(invalid("cache entry lengths do not add up"));
    }

    let meta_offset = HEADER_LEN + body_len;
    let meta_bytes = if meta_offset >= tail_start {
        let start = (meta_offset - tail_start) as usize;
        tail[start..start + meta_len as usize].to_vec()
    } else {
        let mut meta_bytes = vec![0u8; meta_len as usize];
        file.seek(SeekFrom::Start(meta_offset))?;
        file.read_exact(&mut meta_bytes)?;
        meta_bytes
    };

    let mut hasher = Hasher::new();
    hasher.update(&meta_bytes);
    if hasher.finalize() != meta_crc {
        return Err(invalid("cache metadata is corrupt"));
    }

    let mut meta: EntryMeta =
        postcard::from_bytes(&meta_bytes).map_err(|_| invalid("cache metadata is unreadable"))?;
    if meta.format != CACHE_FORMAT {
        return Err(invalid("cache metadata has another format"));
    }
    meta.body_len = body_len;

    Ok(EntryTail {
        meta,
        body_len,
        body_crc,
    })
}

/// Replace an entry's metadata, keeping its body. Used to freshen an entry after
/// a 304, which is why the metadata lives at the end of the file.
pub(crate) fn rewrite_tail(file: &mut File, meta: &EntryMeta) -> io::Result<()> {
    let existing = read_tail(file)?;
    file.set_len(HEADER_LEN + existing.body_len)?;
    file.seek(SeekFrom::Start(HEADER_LEN + existing.body_len))?;
    write_tail(file, meta, existing.body_len, existing.body_crc)
}
