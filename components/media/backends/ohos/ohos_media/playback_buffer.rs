/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::collections::VecDeque;

use log::debug;

/// The most bytes of the media resource that a player keeps.
const CAPACITY: usize = 8 * 1024 * 1024;
/// How many bytes before the furthest read are kept, so that AVPlayer can read them again.
const BACK_BUFFER: u64 = 1024 * 1024;
/// A stopped fetch resumes at the end of the buffered bytes once fewer bytes than this are left to
/// read.
const REFILL_THRESHOLD: u64 = CAPACITY as u64 / 4;
/// How far after the buffered bytes a read waits for the fetch to arrive, instead of restarting
/// the fetch at the read position.
const MAX_READ_AHEAD: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq)]
enum FetchState {
    /// The pushed bytes continue the buffered bytes.
    Active,
    /// The buffer was full, so a pushed chunk was refused, and the media element keeps it until it
    /// is asked for data again.
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
    /// Asks the media element to push data again. It is called with the buffer locked, so that the
    /// request reaches the media element before a restart of the fetch that a later read asks for.
    need_data_closure: Option<Box<dyn Fn() + Send + Sync>>,
    /// The buffered bytes, starting at `start` in the media resource.
    data: VecDeque<u8>,
    start: u64,
    /// The furthest position that AVPlayer read up to, or waits at, since the buffer was emptied.
    /// Evicting bytes relative to the furthest read, rather than the last one, makes sure that
    /// reads at two positions cannot keep each other from progressing.
    furthest_read: u64,
    fetch_state: FetchState,
    /// Whether the media element was asked to push data again and has not pushed since. Its next
    /// chunk is the refused one, or the first one of a fetch that it restarted at the end of the
    /// buffered bytes.
    resumed: bool,
    size: Option<u64>,
}

impl PlaybackBuffer {
    pub fn new(
        enough_data_closure: Option<Box<dyn Fn() + Send + Sync>>,
        need_data_closure: Option<Box<dyn Fn() + Send + Sync>>,
    ) -> Self {
        PlaybackBuffer {
            enough_data_closure,
            need_data_closure,
            data: VecDeque::with_capacity(CAPACITY),
            start: 0,
            furthest_read: 0,
            fetch_state: FetchState::Active,
            resumed: false,
            size: None,
        }
    }

    fn end(&self) -> u64 {
        self.start + self.data.len() as u64
    }

    pub fn set_size(&mut self, size: u64) {
        self.size = Some(size);
    }

    /// Copies the bytes at `pos` into `dest`. Returns how many bytes were copied, and the position
    /// at which the media element has to restart the fetch, if any.
    pub fn read_data(&mut self, dest: &mut [u8], pos: u64) -> (usize, Option<u64>) {
        let end = self.end();
        if (self.start..end).contains(&pos) {
            let read_len = dest.len().min((end - pos) as usize);
            self.copy_to(pos - self.start, &mut dest[..read_len]);
            self.furthest_read = self.furthest_read.max(pos + read_len as u64);
            if self.fetch_state == FetchState::Stopped &&
                end - self.furthest_read < REFILL_THRESHOLD
            {
                self.resume();
            }
            return (read_len, None);
        }

        if self.size.is_some_and(|size| pos >= size) {
            return (0, None);
        }
        if pos >= end && pos - end <= MAX_READ_AHEAD {
            self.furthest_read = self.furthest_read.max(pos);
            if self.fetch_state == FetchState::Stopped {
                self.resume();
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

    /// Accepts pushed bytes again, and asks the media element for them. It pushes the refused
    /// chunk first, so the fetch continues at the end of the buffered bytes without a restart.
    fn resume(&mut self) {
        self.fetch_state = FetchState::Active;
        self.resumed = true;
        if let Some(need_data_closure) = &self.need_data_closure {
            need_data_closure();
        }
    }

    fn restart_at(&mut self, pos: u64) -> u64 {
        self.data.clear();
        self.start = pos;
        self.furthest_read = pos;
        self.fetch_state = FetchState::Restarting;
        self.resumed = false;
        pos
    }

    /// Must be called once the media element answered the restart of the fetch, before it can push
    /// the bytes of the restarted fetch.
    pub fn restarted(&mut self, success: bool) {
        if success && self.fetch_state == FetchState::Restarting {
            self.fetch_state = FetchState::Active;
        }
    }

    /// Appends bytes of the fetch. Returns false if the buffer is full, in which case none of the
    /// bytes were buffered, and the media element has to push them again once it is asked for
    /// data.
    pub fn push_buffer(&mut self, data: &[u8]) -> bool {
        match self.fetch_state {
            FetchState::Active => {},
            FetchState::Stopped => return false,
            // The bytes of a fetch that is being restarted, or that already ended, are not needed.
            FetchState::Ended | FetchState::Restarting => return true,
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
        // A chunk that does not fit is still taken whole after a resume, and when AVPlayer read all
        // buffered bytes, as refusing it then would only stop the fetch again, or stall AVPlayer.
        // The buffer overshoots its capacity by at most that chunk.
        let fits = self.data.len() + data.len() <= CAPACITY;
        if fits || self.resumed || self.furthest_read >= self.end() {
            if fits && self.data.capacity() > CAPACITY {
                self.data.shrink_to(CAPACITY);
            }
            self.resumed = false;
            self.data.extend(data);
            return true;
        }
        debug!(
            "Buffer is full, buffered {}..{}, furthest read {}",
            self.start,
            self.end(),
            self.furthest_read
        );
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

    fn counter() -> (Arc<AtomicUsize>, Box<dyn Fn() + Send + Sync>) {
        let count = Arc::new(AtomicUsize::new(0));
        let closure_count = count.clone();
        let closure = Box::new(move || {
            closure_count.fetch_add(1, Ordering::SeqCst);
        });
        (count, closure)
    }

    /// Returns a buffer, and how often it signalled EnoughData and NeedData.
    fn buffer() -> (PlaybackBuffer, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let (enough_data_count, enough_data_closure) = counter();
        let (need_data_count, need_data_closure) = counter();
        let buffer = PlaybackBuffer::new(Some(enough_data_closure), Some(need_data_closure));
        (buffer, enough_data_count, need_data_count)
    }

    fn count(counter: &AtomicUsize) -> usize {
        counter.load(Ordering::SeqCst)
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
        assert_eq!(buffer.end(), pos);
        pos
    }

    /// Reads from `from` until the buffer asks for data again, and returns where the reads ended.
    fn read_until_resumed(buffer: &mut PlaybackBuffer, need_data: &AtomicUsize, from: u64) -> u64 {
        let needed = count(need_data);
        let mut pos = from;
        while count(need_data) == needed {
            assert_eq!(read(buffer, pos, CHUNK), (bytes(pos, CHUNK), None));
            pos += CHUNK as u64;
        }
        assert_eq!(count(need_data), needed + 1);
        pos
    }

    #[test]
    fn reads_the_pushed_bytes_and_waits_at_the_end() {
        let (mut buffer, _, _) = buffer();
        assert!(buffer.push_buffer(&bytes(0, 1000)));
        assert_eq!(read(&mut buffer, 100, 50), (bytes(100, 50), None));
        assert_eq!(read(&mut buffer, 990, 50), (bytes(990, 10), None));
        assert_eq!(read(&mut buffer, 1000, 50), (vec![], None));
    }

    #[test]
    fn slides_with_a_reader_that_keeps_up() {
        let (mut buffer, enough_data_count, need_data_count) = buffer();
        let mut pos = 0;
        while pos < 3 * CAPACITY as u64 {
            assert!(buffer.push_buffer(&bytes(pos, CHUNK)));
            assert_eq!(read(&mut buffer, pos, CHUNK), (bytes(pos, CHUNK), None));
            pos += CHUNK as u64;
        }
        assert_eq!(count(&enough_data_count), 0);
        assert_eq!(count(&need_data_count), 0);
        assert_eq!(
            read(&mut buffer, pos - BACK_BUFFER, CHUNK),
            (bytes(pos - BACK_BUFFER, CHUNK), None)
        );
    }

    #[test]
    fn refuses_a_chunk_that_does_not_fit_whole() {
        let (mut buffer, enough_data_count, _) = buffer();
        let chunk = 100_000;
        let mut pos = 0;
        while buffer.push_buffer(&bytes(pos, chunk)) {
            pos += chunk as u64;
        }
        assert!(pos < CAPACITY as u64);
        assert_eq!(buffer.end(), pos);
        assert_eq!(count(&enough_data_count), 1);
        assert!(!buffer.push_buffer(&bytes(pos, 10)));
        assert_eq!(buffer.end(), pos);
        assert_eq!(count(&enough_data_count), 1);
    }

    #[test]
    fn resumes_at_the_end_before_the_reader_gets_there() {
        let (mut buffer, enough_data_count, need_data_count) = buffer();
        let end = fill(&mut buffer, 0);
        let mut pos = read_until_resumed(&mut buffer, &need_data_count, 0);
        assert!(end - pos < REFILL_THRESHOLD);
        assert_eq!(read(&mut buffer, pos, CHUNK), (bytes(pos, CHUNK), None));
        pos += CHUNK as u64;
        assert_eq!(count(&need_data_count), 1);

        // The refused chunk comes first and continues the buffered bytes.
        let new_end = fill(&mut buffer, end);
        assert!(new_end > end);
        assert_eq!(buffer.start, pos - BACK_BUFFER);
        assert_eq!(count(&enough_data_count), 2);
        pos = read_until_resumed(&mut buffer, &need_data_count, pos);
        while pos < new_end {
            assert_eq!(read(&mut buffer, pos, CHUNK), (bytes(pos, CHUNK), None));
            pos += CHUNK as u64;
        }
        assert_eq!(read(&mut buffer, new_end, CHUNK), (vec![], None));
        assert_eq!(count(&need_data_count), 2);
    }

    #[test]
    fn asks_for_data_before_a_restart_that_a_later_read_asks_for() {
        let (mut buffer, _, need_data_count) = buffer();
        let end = fill(&mut buffer, 0);
        assert_eq!(read(&mut buffer, end, CHUNK), (vec![], None));
        assert_eq!(count(&need_data_count), 1);
        let far = end + MAX_READ_AHEAD + 1;
        assert_eq!(read(&mut buffer, far, CHUNK), (vec![], Some(far)));

        // The refused chunk arrives after the restart was asked for, and is dropped.
        assert!(buffer.push_buffer(&bytes(end, CHUNK)));
        assert_eq!(buffer.end(), far);
        buffer.restarted(true);
        assert!(buffer.push_buffer(&bytes(far, CHUNK)));
        assert!(buffer.push_buffer(&bytes(far + CHUNK as u64, CHUNK)));
        assert_eq!(
            read(&mut buffer, far, 2 * CHUNK),
            (bytes(far, 2 * CHUNK), None)
        );
        assert_eq!(count(&need_data_count), 1);
    }

    #[test]
    fn restarts_a_stopped_fetch_at_a_read_outside_the_window() {
        let (mut buffer, _, need_data_count) = buffer();
        let end = fill(&mut buffer, 0);
        let far = end + MAX_READ_AHEAD + 1;
        assert_eq!(read(&mut buffer, far, CHUNK), (vec![], Some(far)));
        assert_eq!(count(&need_data_count), 0);
        assert!(buffer.push_buffer(&bytes(end, CHUNK)));
        assert_eq!(buffer.end(), far);
        buffer.restarted(true);
        assert!(buffer.push_buffer(&bytes(far, CHUNK)));
        assert_eq!(read(&mut buffer, far, CHUNK), (bytes(far, CHUNK), None));
    }

    #[test]
    fn restarts_at_a_read_outside_the_window() {
        let (mut buffer, _, _) = buffer();
        assert!(buffer.push_buffer(&bytes(0, CHUNK)));
        let far = CHUNK as u64 + MAX_READ_AHEAD + 1;
        assert_eq!(read(&mut buffer, far, CHUNK), (vec![], Some(far)));

        // Reads elsewhere wait for the pending restart, and the previous fetch is dropped.
        assert_eq!(read(&mut buffer, 0, CHUNK), (vec![], None));
        assert!(buffer.push_buffer(&bytes(CHUNK as u64, CHUNK)));
        assert_eq!(buffer.end(), far);
        buffer.end_of_stream();

        buffer.restarted(true);
        assert!(buffer.push_buffer(&bytes(far, CHUNK)));
        assert_eq!(read(&mut buffer, far, CHUNK), (bytes(far, CHUNK), None));
        assert_eq!(read(&mut buffer, 0, CHUNK), (vec![], Some(0)));
    }

    #[test]
    fn waits_for_a_read_shortly_after_the_buffered_bytes() {
        let (mut buffer, _, _) = buffer();
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
        let (mut buffer, _, _) = buffer();
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
    fn resumes_for_a_read_shortly_after_a_full_buffer() {
        let (mut buffer, _, need_data_count) = buffer();
        let end = fill(&mut buffer, 0);
        assert_eq!(read(&mut buffer, end + 10, CHUNK), (vec![], None));
        assert_eq!(read(&mut buffer, end + 10, CHUNK), (vec![], None));
        assert_eq!(count(&need_data_count), 1);
        assert_eq!(buffer.start, 0);
        assert_eq!(read(&mut buffer, 0, CHUNK), (bytes(0, CHUNK), None));
        assert!(buffer.push_buffer(&bytes(end, CHUNK)));
        assert_eq!(read(&mut buffer, end, CHUNK), (bytes(end, CHUNK), None));
    }

    #[test]
    fn takes_a_chunk_larger_than_the_buffer_once_the_reader_caught_up() {
        let (mut buffer, _, need_data_count) = buffer();
        let len = CAPACITY + CHUNK;
        assert!(buffer.push_buffer(&bytes(0, len)));
        assert!(!buffer.push_buffer(&bytes(len as u64, len)));
        let last = len as u64 - 100;
        assert_eq!(read(&mut buffer, last, CHUNK), (bytes(last, 100), None));
        assert_eq!(count(&need_data_count), 1);
        assert!(buffer.push_buffer(&bytes(len as u64, len)));
        assert_eq!(buffer.start, len as u64 - BACK_BUFFER);
        assert_eq!(
            read(&mut buffer, 2 * len as u64 - 100, CHUNK),
            (bytes(2 * len as u64 - 100, 100), None)
        );
    }

    #[test]
    fn takes_a_chunk_that_does_not_fit_once_after_a_resume() {
        let (mut buffer, enough_data_count, need_data_count) = buffer();
        let end = fill(&mut buffer, 0);
        let mut pos = read_until_resumed(&mut buffer, &need_data_count, 0);
        assert!(buffer.push_buffer(&bytes(end, CAPACITY)));
        let new_end = end + CAPACITY as u64;
        assert!(buffer.data.len() > CAPACITY);
        assert!(!buffer.push_buffer(&bytes(new_end, CHUNK)));
        assert_eq!(count(&enough_data_count), 2);
        assert_eq!(buffer.end(), new_end);

        pos = read_until_resumed(&mut buffer, &need_data_count, pos);
        while pos < new_end {
            assert_eq!(read(&mut buffer, pos, CHUNK), (bytes(pos, CHUNK), None));
            pos += CHUNK as u64;
        }
        let grown_capacity = buffer.data.capacity();
        assert!(grown_capacity > CAPACITY);
        assert!(buffer.push_buffer(&bytes(new_end, CHUNK)));
        assert!(buffer.data.capacity() < grown_capacity);
        assert_eq!(
            read(&mut buffer, new_end, CHUNK),
            (bytes(new_end, CHUNK), None)
        );
    }

    #[test]
    fn ends_a_resumed_fetch() {
        let (mut buffer, _, need_data_count) = buffer();
        let size = CAPACITY as u64 + 2 * CHUNK as u64;
        buffer.set_size(size);
        let end = fill(&mut buffer, 0);
        let mut pos = read_until_resumed(&mut buffer, &need_data_count, 0);
        assert!(buffer.push_buffer(&bytes(end, (size - end) as usize)));
        buffer.end_of_stream();
        assert_eq!(buffer.fetch_state, FetchState::Ended);
        while pos < size {
            assert_eq!(read(&mut buffer, pos, CHUNK), (bytes(pos, CHUNK), None));
            pos += CHUNK as u64;
        }
        assert_eq!(read(&mut buffer, size, CHUNK), (vec![], None));
        assert_eq!(count(&need_data_count), 1);
    }

    #[test]
    fn ignores_the_end_of_a_stopped_fetch() {
        let (mut buffer, _, need_data_count) = buffer();
        let end = fill(&mut buffer, 0);
        buffer.end_of_stream();
        assert_eq!(read(&mut buffer, end, CHUNK), (vec![], None));
        assert_eq!(count(&need_data_count), 1);
    }

    #[test]
    fn drops_the_bytes_after_a_failed_restart() {
        let (mut buffer, _, _) = buffer();
        assert_eq!(
            read(&mut buffer, 2 * MAX_READ_AHEAD, CHUNK).1,
            Some(2 * MAX_READ_AHEAD)
        );
        buffer.restarted(false);
        assert!(buffer.push_buffer(&bytes(2 * MAX_READ_AHEAD, CHUNK)));
        assert_eq!(buffer.end(), 2 * MAX_READ_AHEAD);
        assert_eq!(read(&mut buffer, 0, CHUNK), (vec![], None));
    }

    #[test]
    fn progresses_with_reads_at_two_positions() {
        let (mut buffer, _, need_data_count) = buffer();
        let end = fill(&mut buffer, 0);
        assert_eq!(read(&mut buffer, end, CHUNK), (vec![], None));
        assert_eq!(count(&need_data_count), 1);
        assert_eq!(read(&mut buffer, 0, CHUNK), (bytes(0, CHUNK), None));
        assert!(buffer.push_buffer(&bytes(end, CHUNK)));
        assert_eq!(read(&mut buffer, end, CHUNK), (bytes(end, CHUNK), None));
    }

    #[test]
    fn keeps_the_unread_bytes_after_a_read_past_the_end_of_the_resource() {
        let (mut buffer, _, _) = buffer();
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
        let (mut buffer, _, _) = buffer();
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
        let (mut buffer, _, _) = buffer();
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
