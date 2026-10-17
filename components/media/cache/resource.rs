/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The state of a cached resource: its blocks, readers and fetches.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use crate::store::{BLOCK_SIZE, SlotId};
use crate::{Fetcher, ReaderActivity, ReaderError, ReaderEvent, ResourceKey, Validator};

pub(crate) type ResourceId = u64;
pub(crate) type ReaderId = u64;
pub(crate) type FetchId = u64;

pub(crate) type Waker = Arc<dyn Fn() + Send + Sync>;
pub(crate) type Observer = Arc<dyn Fn(ReaderEvent) + Send + Sync>;

/// How far a fetch reads through data that is already cached instead of stopping, and how far
/// ahead of a fetch a read waits for it instead of starting another fetch. Gecko's
/// `SEEK_VS_READ_THRESHOLD`.
pub(crate) const READ_THROUGH: u64 = 1024 * 1024;
/// The read-ahead and resume thresholds of a reader whose rate is not known.
pub(crate) const DEFAULT_READAHEAD: u64 = 8 * 1024 * 1024;
pub(crate) const DEFAULT_RESUME: u64 = 2 * 1024 * 1024;
/// The read-ahead and resume thresholds of a reader that only needs the metadata.
pub(crate) const METADATA_READAHEAD: u64 = 1024 * 1024;
pub(crate) const METADATA_RESUME: u64 = 512 * 1024;
/// How many recent read positions of a reader count, and for how long. A demuxer of a file
/// whose tracks are far apart reads at several places in turn.
const RECENT_POSITIONS: usize = 3;
const RECENT_WINDOW: Duration = Duration::from_secs(2);
/// The rate assumed for the eviction of blocks near a reader whose rate is not known: the
/// default read-ahead in 30 s.
pub(crate) const DEFAULT_RATE: f64 = DEFAULT_READAHEAD as f64 / 30.;

/// The data of one block: always a prefix of the block, `[0, len())`.
pub(crate) struct Block {
    /// The slot of the block, for resources that are not kept in memory.
    pub slot: Option<SlotId>,
    /// The bytes `[0, file_len)` are in the slot of the file.
    pub file_len: usize,
    /// The bytes that follow `file_len`: the whole block for a resource in memory, the write
    /// buffer otherwise.
    pub ram: Vec<u8>,
    /// Whether a write of `ram` to the file is in progress.
    pub flushing: bool,
    /// The fetch that is filling the block.
    pub writer: Option<FetchId>,
    pub last_use: Duration,
    /// Whether the block was last read for the metadata, as an index at the end of a file,
    /// which is needed again on a seek rather than when playback gets there.
    pub metadata: bool,
}

impl Block {
    pub(crate) fn len(&self) -> usize {
        self.file_len + self.ram.len()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Control {
    Running,
    /// A suspend was sent and not answered yet.
    Suspending,
    Suspended,
}

pub(crate) struct Fetch {
    pub id: FetchId,
    /// What started the fetch, on behalf of a document that one of the readers belongs to.
    pub fetcher: Arc<dyn Fetcher>,
    /// The position that was requested.
    pub start: u64,
    /// The position of the next byte of the body.
    pub pos: u64,
    pub responded: bool,
    pub control: Control,
    /// The suspends whose answer has not arrived yet.
    pub pending_replies: u32,
    pub suspended_since: Option<Duration>,
    /// Whether the fetch delivered data that was not cached before.
    pub progress: bool,
    /// Whether a suspend was declined since the last honoured one. A fetch without range
    /// support then keeps running.
    pub declined: bool,
    /// Bytes before this position are not kept, unless a reader needs them: the prefix that a
    /// server without range support sends again.
    pub discard_before: u64,
}

pub(crate) struct Reader {
    /// The position after the last read.
    pub pos: u64,
    /// The positions after recent reads at other places, and when they were read.
    pub recent: Vec<(u64, Duration)>,
    /// The position of the last `Pending` read, until the waker is called.
    pub pending: Option<u64>,
    pub activity: ReaderActivity,
    /// The last known playback rate in bytes per second.
    pub rate: Option<f64>,
    pub waker: Option<Waker>,
    pub observer: Option<Observer>,
    pub fetcher: Arc<dyn Fetcher>,
    pub last_progress: Option<Duration>,
}

impl Reader {
    /// Notes a read from `start` that ended at `end`.
    pub(crate) fn note_read(&mut self, start: u64, end: u64, now: Duration) {
        self.pos = end;
        match self
            .recent
            .iter_mut()
            .find(|(pos, _)| pos.abs_diff(start) <= READ_THROUGH)
        {
            Some(entry) => *entry = (end, now),
            None => {
                self.recent.push((end, now));
                if self.recent.len() > RECENT_POSITIONS {
                    let oldest = (0..self.recent.len())
                        .min_by_key(|index| self.recent[*index].1)
                        .unwrap_or(0);
                    self.recent.remove(oldest);
                }
            },
        }
    }

    /// The positions at which the reader reads: its last one, and the other recent ones.
    pub(crate) fn positions(&self, now: Duration) -> Vec<u64> {
        let mut positions = vec![self.pos];
        for &(pos, at) in &self.recent {
            if now.saturating_sub(at) <= RECENT_WINDOW &&
                positions
                    .iter()
                    .all(|other| other.abs_diff(pos) > READ_THROUGH)
            {
                positions.push(pos);
            }
        }
        positions
    }

    /// How far ahead of the reader to load, and below how much loaded ahead to resume.
    pub(crate) fn thresholds(&self, readahead: Duration, resume: Duration, cap: u64) -> (u64, u64) {
        let (ahead, resume) = match (self.activity, self.rate) {
            (ReaderActivity::Metadata, _) => (METADATA_READAHEAD, METADATA_RESUME),
            (ReaderActivity::Idle, _) => (0, 0),
            (_, Some(rate)) => (
                (rate * readahead.as_secs_f64()) as u64,
                (rate * resume.as_secs_f64()) as u64,
            ),
            (_, None) => (DEFAULT_READAHEAD, DEFAULT_RESUME),
        };
        let ahead = ahead.min(cap).max(BLOCK_SIZE);
        (ahead, resume.min(ahead / 2))
    }

    /// Whether the reader needs data ahead of what it reads.
    pub(crate) fn is_idle(&self) -> bool {
        self.activity == ReaderActivity::Idle
    }

    pub(crate) fn eviction_rate(&self) -> f64 {
        self.rate.unwrap_or(DEFAULT_RATE)
    }
}

/// What the readers were last told about the download.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Announced {
    Loading,
    Suspended,
    Complete,
}

/// The origin of the partial responses of a resource, as HTML's media resource origin.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ResponseOrigin {
    None,
    /// Of responses without a URL.
    Rewritten,
    Multiple,
    Origin(String),
}

/// What the first response told about the resource.
pub(crate) struct Meta {
    /// A 206 to a range request with an unencoded body.
    pub partial: bool,
    pub validator: Option<Validator>,
    /// Whether any response was CORS-cross-origin.
    pub cors_tainted: bool,
    /// The length that the first response announced.
    pub announced_length: Option<u64>,
}

pub(crate) struct Resource {
    pub key: ResourceKey,
    pub meta: Option<Meta>,
    pub origin: ResponseOrigin,
    /// The length of the resource, if known. It can be learned at the end of a fetch.
    pub length: Option<u64>,
    /// Whether the blocks are kept in memory, without slots.
    pub ram_backed: bool,
    pub blocks: BTreeMap<u64, Block>,
    /// The sum of the lengths of the blocks.
    pub cached_bytes: u64,
    pub fetches: Vec<Fetch>,
    /// By increasing id, so the last one was opened last.
    pub readers: BTreeMap<ReaderId, Reader>,
    pub error: Option<ReaderError>,
    pub last_used: Duration,
    pub announced: Announced,
    /// A running fetch ended early after it delivered data: restart it without waiting for the
    /// resume threshold.
    pub restart_now: bool,
    /// Whether `open` finds this resource by its key.
    pub listed: bool,
    /// A requested position that a 206 answered from a later one, so that the data there
    /// could not be had.
    pub skipped_start: Option<u64>,
}

impl Resource {
    pub(crate) fn new(key: ResourceKey, now: Duration) -> Self {
        Self {
            key,
            meta: None,
            origin: ResponseOrigin::None,
            length: None,
            ram_backed: false,
            blocks: BTreeMap::new(),
            cached_bytes: 0,
            fetches: Vec::new(),
            readers: BTreeMap::new(),
            error: None,
            last_used: now,
            announced: Announced::Loading,
            restart_now: false,
            listed: true,
            skipped_start: None,
        }
    }

    /// Whether positions in the resource can be requested: a 206 to a range request, with a
    /// known length.
    pub(crate) fn seekable(&self) -> bool {
        self.meta.as_ref().is_some_and(|meta| meta.partial) && self.length.is_some()
    }

    pub(crate) fn end(&self) -> u64 {
        self.length.unwrap_or(u64::MAX)
    }

    /// The length of block `index` once it is complete.
    pub(crate) fn full_len(&self, index: u64) -> usize {
        let start = index * BLOCK_SIZE;
        match self.length {
            Some(length) => length.saturating_sub(start).min(BLOCK_SIZE) as usize,
            None => BLOCK_SIZE as usize,
        }
    }

    /// Whether a read at `pos` returns something other than `Pending`.
    pub(crate) fn readable_at(&self, pos: u64) -> bool {
        self.error.is_some() || pos >= self.end() || self.is_cached(pos)
    }

    pub(crate) fn is_cached(&self, pos: u64) -> bool {
        self.blocks
            .get(&(pos / BLOCK_SIZE))
            .is_some_and(|block| block.len() as u64 > pos % BLOCK_SIZE)
    }

    /// The first position at or after `pos` that is not cached, or `limit` if everything up
    /// to it is.
    pub(crate) fn cached_until(&self, pos: u64, limit: u64) -> u64 {
        let limit = limit.min(self.end());
        let mut at = pos;
        for (&index, block) in self.blocks.range(pos / BLOCK_SIZE..) {
            if at >= limit {
                break;
            }
            let start = index * BLOCK_SIZE;
            if start > at || start + block.len() as u64 <= at {
                break;
            }
            at = start + block.len() as u64;
            if block.len() < self.full_len(index) {
                break;
            }
        }
        at.min(limit)
    }

    /// Where a fetch of the data at `pos`, which is not cached, starts: after the data that
    /// its block holds, so that the block stays a prefix.
    pub(crate) fn fetch_start(&self, pos: u64) -> u64 {
        let index = pos / BLOCK_SIZE;
        let held = self
            .blocks
            .get(&index)
            .map_or(0, |block| block.len() as u64);
        index * BLOCK_SIZE + held.min(pos % BLOCK_SIZE)
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.length
            .is_some_and(|length| self.cached_bytes >= length)
    }

    /// The fetcher of the reader that was opened last.
    pub(crate) fn fetcher(&self) -> Option<Arc<dyn Fetcher>> {
        self.readers
            .values()
            .next_back()
            .map(|reader| reader.fetcher.clone())
    }

    /// Whether a reader at `pos` gets to `fetch_pos` by reading on through cached data.
    pub(crate) fn reaches(&self, pos: u64, fetch_pos: u64) -> bool {
        pos <= fetch_pos.saturating_add(READ_THROUGH) &&
            self.cached_until(pos, fetch_pos)
                .saturating_add(READ_THROUGH) >=
                fetch_pos
    }

    pub(crate) fn fetch_mut(&mut self, id: FetchId) -> Option<&mut Fetch> {
        self.fetches.iter_mut().find(|fetch| fetch.id == id)
    }
}
