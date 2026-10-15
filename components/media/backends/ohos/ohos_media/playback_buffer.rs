/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::collections::VecDeque;

use log::debug;

/// The most bytes of the media resource that a player keeps.
const CAPACITY: usize = 8 * 1024 * 1024;
/// How many bytes before the furthest read are kept, so that AVPlayer can read them again.
const BACK_BUFFER: u64 = 1024 * 1024;
/// A stopped fetch restarts at the end of the buffered bytes once fewer bytes than this are left
/// to read.
const REFILL_THRESHOLD: u64 = CAPACITY as u64 / 4;
/// How far after the buffered bytes a read waits for the fetch to arrive, instead of restarting
/// the fetch at the read position.
const MAX_READ_AHEAD: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq)]
enum FetchState {
    /// The pushed bytes continue the buffered bytes.
    Active,
    /// The buffer was full, so the media element was asked to stop fetching, and the pushed bytes
    /// are dropped until the fetch restarts at the end of the buffered bytes.
    Stopped,
    /// The fetch reached the end of the media resource.
    Ended,
    /// The media element was asked to restart the fetch, and the pushed bytes are dropped until it
    /// answers.
    Restarting,
}

/// A window of the media resource that slides with the reads of AVPlayer. The media element pushes
/// bytes from the script thread, and AVPlayer reads them from its own threads.
pub struct PlaybackBuffer {
    enough_data_closure: Option<Box<dyn Fn() + Send + Sync>>,
    /// The buffered bytes, starting at `start` in the media resource.
    data: VecDeque<u8>,
    start: u64,
    /// The furthest position that AVPlayer read up to, or waits at, since the buffer was emptied.
    /// Evicting bytes relative to the furthest read, rather than the last one, makes sure that
    /// reads at two positions cannot keep each other from progressing.
    furthest_read: u64,
    fetch_state: FetchState,
    /// Whether the server answers range requests, so that a fetch can restart at the end of the
    /// buffered bytes without fetching the media resource from its start again.
    seekable: bool,
    size: Option<u64>,
}

impl PlaybackBuffer {
    pub fn new(enough_data_closure: Option<Box<dyn Fn() + Send + Sync>>) -> Self {
        PlaybackBuffer {
            enough_data_closure,
            data: VecDeque::with_capacity(CAPACITY),
            start: 0,
            furthest_read: 0,
            fetch_state: FetchState::Active,
            seekable: false,
            size: None,
        }
    }

    fn end(&self) -> u64 {
        self.start + self.data.len() as u64
    }

    pub fn set_size(&mut self, size: u64) {
        self.size = Some(size);
    }

    pub fn set_seekable(&mut self, seekable: bool) {
        self.seekable = seekable;
    }

    /// Copies the bytes at `pos` into `dest`. Returns how many bytes were copied, and the position
    /// at which the media element has to restart the fetch, if any.
    pub fn read_data(&mut self, dest: &mut [u8], pos: u64) -> (usize, Option<u64>) {
        let end = self.end();
        if (self.start..end).contains(&pos) {
            let read_len = dest.len().min((end - pos) as usize);
            self.copy_to(pos - self.start, &mut dest[..read_len]);
            self.furthest_read = self.furthest_read.max(pos + read_len as u64);
            let refill = self.fetch_state == FetchState::Stopped &&
                self.seekable &&
                end - self.furthest_read < REFILL_THRESHOLD;
            return (read_len, refill.then(|| self.restart_at(end)));
        }

        if self.size.is_some_and(|size| pos >= size) {
            return (0, None);
        }
        if pos >= end && pos - end <= MAX_READ_AHEAD {
            self.furthest_read = self.furthest_read.max(pos);
            if self.fetch_state == FetchState::Stopped {
                return (0, Some(self.restart_at(end)));
            }
            return (0, None);
        }
        if self.fetch_state == FetchState::Restarting {
            return (0, None);
        }
        debug!(
            "No data at position {pos}, buffered {}..{end}, fetch {:?}",
            self.start, self.fetch_state
        );
        (0, Some(self.restart_at(pos)))
    }

    fn copy_to(&self, offset: u64, dest: &mut [u8]) {
        let offset = offset as usize;
        let (front, back) = self.data.as_slices();
        let (first, second) = match front.get(offset..) {
            Some(first) => (first, back),
            None => (&[][..], &back[offset - front.len()..]),
        };
        let first_len = first.len().min(dest.len());
        let (dest_first, dest_second) = dest.split_at_mut(first_len);
        dest_first.copy_from_slice(&first[..first_len]);
        dest_second.copy_from_slice(&second[..dest_second.len()]);
    }

    fn restart_at(&mut self, pos: u64) -> u64 {
        if pos != self.end() {
            self.data.clear();
            self.start = pos;
            self.furthest_read = pos;
        }
        self.fetch_state = FetchState::Restarting;
        pos
    }

    /// Must be called once the media element answered the restart of the fetch, before it can push
    /// the bytes of the restarted fetch.
    pub fn restarted(&mut self, success: bool) {
        if success && self.fetch_state == FetchState::Restarting {
            self.fetch_state = FetchState::Active;
        }
    }

    /// Appends bytes of the fetch. Returns false if they were not all buffered.
    pub fn push_buffer(&mut self, data: &[u8]) -> bool {
        if self.fetch_state != FetchState::Active {
            return false;
        }
        let excess = (self.data.len() + data.len()).saturating_sub(CAPACITY);
        if excess > 0 {
            let evictable = self
                .furthest_read
                .saturating_sub(BACK_BUFFER)
                .saturating_sub(self.start)
                .min(self.data.len() as u64) as usize;
            let evicted = excess.min(evictable);
            self.data.drain(..evicted);
            self.start += evicted as u64;
        }
        let room = CAPACITY - self.data.len();
        if data.len() <= room {
            self.data.extend(data);
            return true;
        }
        debug!(
            "Buffer is full, buffered {}..{}, furthest read {}",
            self.start,
            self.end(),
            self.furthest_read
        );
        self.data.extend(&data[..room]);
        self.fetch_state = FetchState::Stopped;
        if let Some(enough_data_closure) = &self.enough_data_closure {
            enough_data_closure();
        }
        false
    }

    pub fn end_of_stream(&mut self) {
        if self.fetch_state == FetchState::Active {
            self.fetch_state = FetchState::Ended;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    const CHUNK: usize = 64 * 1024;

    fn byte_at(pos: u64) -> u8 {
        (pos % 251) as u8
    }

    fn bytes(pos: u64, len: usize) -> Vec<u8> {
        (pos..pos + len as u64).map(byte_at).collect()
    }

    fn buffer(seekable: bool) -> (PlaybackBuffer, Arc<AtomicUsize>) {
        let enough_data_count = Arc::new(AtomicUsize::new(0));
        let count = enough_data_count.clone();
        let mut buffer = PlaybackBuffer::new(Some(Box::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
        })));
        buffer.set_seekable(seekable);
        (buffer, enough_data_count)
    }

    fn read(buffer: &mut PlaybackBuffer, pos: u64, len: usize) -> (Vec<u8>, Option<u64>) {
        let mut dest = vec![0; len];
        let (read_len, restart) = buffer.read_data(&mut dest, pos);
        dest.truncate(read_len);
        (dest, restart)
    }

    /// Pushes the bytes from `from` until the buffer is full, and returns where they stopped.
    fn fill(buffer: &mut PlaybackBuffer, from: u64) -> u64 {
        let mut pos = from;
        while buffer.push_buffer(&bytes(pos, CHUNK)) {
            pos += CHUNK as u64;
        }
        buffer.end()
    }

    #[test]
    fn reads_the_pushed_bytes_and_waits_at_the_end() {
        let (mut buffer, _) = buffer(true);
        assert!(buffer.push_buffer(&bytes(0, 1000)));
        assert_eq!(read(&mut buffer, 100, 50), (bytes(100, 50), None));
        assert_eq!(read(&mut buffer, 990, 50), (bytes(990, 10), None));
        assert_eq!(read(&mut buffer, 1000, 50), (vec![], None));
    }

    #[test]
    fn slides_with_a_reader_that_keeps_up() {
        let (mut buffer, enough_data_count) = buffer(true);
        let mut pos = 0;
        while pos < 3 * CAPACITY as u64 {
            assert!(buffer.push_buffer(&bytes(pos, CHUNK)));
            assert_eq!(read(&mut buffer, pos, CHUNK), (bytes(pos, CHUNK), None));
            pos += CHUNK as u64;
        }
        assert_eq!(enough_data_count.load(Ordering::SeqCst), 0);
        assert_eq!(
            read(&mut buffer, pos - BACK_BUFFER, CHUNK),
            (bytes(pos - BACK_BUFFER, CHUNK), None)
        );
    }

    #[test]
    fn drops_the_bytes_after_a_full_buffer() {
        let (mut buffer, enough_data_count) = buffer(true);
        let end = fill(&mut buffer, 0);
        assert_eq!(end, CAPACITY as u64);
        assert_eq!(enough_data_count.load(Ordering::SeqCst), 1);
        assert!(!buffer.push_buffer(&bytes(end + 100, 10)));
        assert_eq!(buffer.end(), end);
        assert_eq!(enough_data_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn refills_at_the_end_before_the_reader_gets_there() {
        let (mut buffer, _) = buffer(true);
        let end = fill(&mut buffer, 0);
        let mut pos = 0;
        let restart = loop {
            let (read_bytes, restart) = read(&mut buffer, pos, CHUNK);
            assert_eq!(read_bytes, bytes(pos, CHUNK));
            pos += CHUNK as u64;
            if restart.is_some() {
                break restart;
            }
        };
        assert_eq!(restart, Some(end));
        assert!(end - pos < REFILL_THRESHOLD);

        // The bytes of the stopped fetch are dropped until the restart is answered, while the
        // buffered bytes can still be read.
        assert!(!buffer.push_buffer(&bytes(end + 4096, CHUNK)));
        assert_eq!(read(&mut buffer, pos, CHUNK), (bytes(pos, CHUNK), None));
        pos += CHUNK as u64;

        buffer.restarted(true);
        let new_end = fill(&mut buffer, end);
        assert!(new_end > end);
        assert_eq!(buffer.start, pos - BACK_BUFFER);
        while pos < new_end {
            assert_eq!(read(&mut buffer, pos, CHUNK).0, bytes(pos, CHUNK));
            pos += CHUNK as u64;
        }
    }

    #[test]
    fn refills_only_at_the_end_without_range_requests() {
        let (mut buffer, _) = buffer(false);
        let end = fill(&mut buffer, 0);
        let mut pos = 0;
        while pos < end {
            assert_eq!(read(&mut buffer, pos, CHUNK), (bytes(pos, CHUNK), None));
            pos += CHUNK as u64;
        }
        assert_eq!(read(&mut buffer, end, CHUNK), (vec![], Some(end)));
        buffer.restarted(true);
        assert!(buffer.push_buffer(&bytes(end, CHUNK)));
        assert_eq!(read(&mut buffer, end, CHUNK), (bytes(end, CHUNK), None));
    }

    #[test]
    fn restarts_at_a_read_outside_the_window() {
        let (mut buffer, _) = buffer(true);
        assert!(buffer.push_buffer(&bytes(0, CHUNK)));
        let far = CHUNK as u64 + MAX_READ_AHEAD + 1;
        assert_eq!(read(&mut buffer, far, CHUNK), (vec![], Some(far)));

        // Reads elsewhere wait for the pending restart, and the previous fetch is dropped.
        assert_eq!(read(&mut buffer, 0, CHUNK), (vec![], None));
        assert!(!buffer.push_buffer(&bytes(CHUNK as u64, CHUNK)));
        buffer.end_of_stream();

        buffer.restarted(true);
        assert!(buffer.push_buffer(&bytes(far, CHUNK)));
        assert_eq!(read(&mut buffer, far, CHUNK), (bytes(far, CHUNK), None));
        assert_eq!(read(&mut buffer, 0, CHUNK), (vec![], Some(0)));
    }

    #[test]
    fn waits_for_a_read_shortly_after_the_buffered_bytes() {
        let (mut buffer, _) = buffer(true);
        assert!(buffer.push_buffer(&bytes(0, CHUNK)));
        let near = CHUNK as u64 + MAX_READ_AHEAD;
        assert_eq!(read(&mut buffer, near, CHUNK), (vec![], None));
        let mut pos = CHUNK as u64;
        while pos <= near {
            assert!(buffer.push_buffer(&bytes(pos, CHUNK)));
            pos += CHUNK as u64;
        }
        assert_eq!(read(&mut buffer, near, CHUNK), (bytes(near, CHUNK), None));
    }

    #[test]
    fn restarts_before_the_buffered_bytes() {
        let (mut buffer, _) = buffer(true);
        let mut pos = 0;
        while pos < 2 * CAPACITY as u64 {
            assert!(buffer.push_buffer(&bytes(pos, CHUNK)));
            assert_eq!(read(&mut buffer, pos, CHUNK).0, bytes(pos, CHUNK));
            pos += CHUNK as u64;
        }
        assert_eq!(buffer.start, pos - CAPACITY as u64);
        let before = buffer.start - 1;
        assert_eq!(read(&mut buffer, before, CHUNK), (vec![], Some(before)));
    }

    #[test]
    fn keeps_the_bytes_that_fit_into_a_full_buffer() {
        let (mut buffer, _) = buffer(false);
        let chunk = 100_000;
        let mut pos = 0;
        while buffer.push_buffer(&bytes(pos, chunk)) {
            pos += chunk as u64;
        }
        assert_eq!(buffer.end(), CAPACITY as u64);
        assert!(!buffer.push_buffer(&bytes(pos + chunk as u64, 10)));
        let last = CAPACITY as u64 - 100;
        assert_eq!(read(&mut buffer, last, CHUNK), (bytes(last, 100), None));
    }

    #[test]
    fn restarts_at_the_end_for_a_read_shortly_after_a_full_buffer() {
        let (mut buffer, _) = buffer(true);
        let end = fill(&mut buffer, 0);
        assert_eq!(read(&mut buffer, end + 10, CHUNK), (vec![], Some(end)));
        assert_eq!(buffer.start, 0);
        assert_eq!(read(&mut buffer, 0, CHUNK), (bytes(0, CHUNK), None));
    }

    #[test]
    fn ignores_the_end_of_a_stopped_fetch() {
        let (mut buffer, _) = buffer(false);
        let end = fill(&mut buffer, 0);
        buffer.end_of_stream();
        assert_eq!(read(&mut buffer, end, CHUNK), (vec![], Some(end)));
    }

    #[test]
    fn drops_the_bytes_after_a_failed_restart() {
        let (mut buffer, _) = buffer(true);
        assert_eq!(
            read(&mut buffer, 2 * MAX_READ_AHEAD, CHUNK).1,
            Some(2 * MAX_READ_AHEAD)
        );
        buffer.restarted(false);
        assert!(!buffer.push_buffer(&bytes(2 * MAX_READ_AHEAD, CHUNK)));
        assert_eq!(read(&mut buffer, 0, CHUNK), (vec![], None));
    }

    #[test]
    fn progresses_with_reads_at_two_positions() {
        let (mut buffer, _) = buffer(true);
        let end = fill(&mut buffer, 0);
        assert_eq!(read(&mut buffer, end, CHUNK), (vec![], Some(end)));
        buffer.restarted(true);
        assert_eq!(read(&mut buffer, 0, CHUNK), (bytes(0, CHUNK), None));
        assert!(buffer.push_buffer(&bytes(end, CHUNK)));
        assert_eq!(read(&mut buffer, end, CHUNK), (bytes(end, CHUNK), None));
    }

    #[test]
    fn keeps_the_unread_bytes_after_a_read_past_the_end_of_the_resource() {
        let (mut buffer, _) = buffer(true);
        let size = CAPACITY as u64 + CHUNK as u64;
        buffer.set_size(size);
        let end = fill(&mut buffer, 0);
        assert_eq!(read(&mut buffer, size, CHUNK), (vec![], None));
        buffer.fetch_state = FetchState::Active;
        assert!(!buffer.push_buffer(&bytes(end, CHUNK)));
        assert_eq!(buffer.start, 0);
    }

    #[test]
    fn does_not_restart_after_the_end_of_the_resource() {
        let (mut buffer, _) = buffer(true);
        buffer.set_size(1000);
        assert!(buffer.push_buffer(&bytes(0, 1000)));
        buffer.end_of_stream();
        assert_eq!(read(&mut buffer, 1000, CHUNK), (vec![], None));
        assert_eq!(
            read(&mut buffer, 1000 + 2 * MAX_READ_AHEAD, CHUNK),
            (vec![], None)
        );
        assert_eq!(read(&mut buffer, 900, CHUNK), (bytes(900, 100), None));
    }

    #[test]
    fn reads_across_the_wrap_of_the_ring() {
        let (mut buffer, _) = buffer(true);
        let end = fill(&mut buffer, 0);
        buffer.furthest_read = end;
        buffer.fetch_state = FetchState::Active;
        let pos = end - 100;
        assert!(buffer.push_buffer(&bytes(end, 1000)));
        assert!(!buffer.data.as_slices().1.is_empty());
        assert_eq!(read(&mut buffer, pos, 1100), (bytes(pos, 1100), None));
        assert_eq!(read(&mut buffer, end, 1000), (bytes(end, 1000), None));
        assert_eq!(
            read(&mut buffer, end + 10, 1000),
            (bytes(end + 10, 990), None)
        );
    }
}
