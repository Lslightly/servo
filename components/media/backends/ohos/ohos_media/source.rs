/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::ffi::c_void;
use std::ptr::NonNull;
use std::slice;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use log::{debug, warn};
use ohos_media_sys::avbuffer::{OH_AVBuffer, OH_AVBuffer_GetAddr, OH_AVBuffer_GetCapacity};
use ohos_media_sys::avcodec_base::OH_AVDataSourceExt;
use ohos_media_sys::avplayer::OH_AVPlayer_SetDataSource;
use ohos_media_sys::avplayer_base::OH_AVPlayer;
use servo_media_cache::{MediaReader, ReadResult};

/// What `readAt` returns at the end of the media resource (`SOURCE_ERROR_EOF`).
const END_OF_STREAM: i32 = -1;
/// What `readAt` returns when the media resource cannot be read (`SOURCE_ERROR_IO`), after which
/// AVPlayer reports an error.
const READ_ERROR: i32 = -2;
/// How long a read waits for data before it returns none. AVPlayer reads again 100 to 500 ms
/// after a read that returns none, and gives up after 20 of them in a row, about 4.6 s, which can
/// fail a seek. Waiting here gives a slow download about 25 s. AVPlayer reads on a thread of its
/// own.
const MAX_READ_WAIT: Duration = Duration::from_secs(1);

/// The data source of AVPlayer, which reads the media resource from the media cache.
pub struct MediaSourceWrapper {
    /// Owned: leaked from a `Box` and freed on drop. AVPlayer keeps pointers to it until it is
    /// released, so it is only accessed through shared references once AVPlayer has it.
    data_source: NonNull<DataSource>,
}

struct DataSource {
    ext: OH_AVDataSourceExt,
    reader: Arc<MediaReader>,
    /// Where the next read starts when AVPlayer reads in sequence, without a position, which it
    /// does for a data source of unknown size.
    sequential_pos: Mutex<u64>,
    wake: Arc<Wake>,
}

/// Wakes the reads that wait for data.
#[derive(Default)]
struct Wake {
    state: Mutex<WakeState>,
    condvar: Condvar,
}

#[derive(Default)]
struct WakeState {
    /// How often the reader's waker was called.
    wakes: u64,
    /// Whether AVPlayer is being released, after which reads don't wait.
    released: bool,
}

impl Wake {
    fn wakes(&self) -> u64 {
        self.state.lock().unwrap().wakes
    }

    fn wake(&self) {
        self.state.lock().unwrap().wakes += 1;
        self.condvar.notify_all();
    }

    fn release(&self) {
        self.state.lock().unwrap().released = true;
        self.condvar.notify_all();
    }

    /// Waits until the waker is called after it was called `wakes` times, unless AVPlayer is
    /// being released or `deadline` passes. Returns whether it was called.
    fn wait(&self, wakes: u64, deadline: Instant) -> bool {
        let mut state = self.state.lock().unwrap();
        while state.wakes == wakes && !state.released {
            let Some(timeout) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            state = self.condvar.wait_timeout(state, timeout).unwrap().0;
        }
        !state.released
    }
}

impl MediaSourceWrapper {
    pub fn new(reader: Arc<MediaReader>) -> Self {
        let wake = Arc::new(Wake::default());
        let waker_wake = wake.clone();
        reader.set_waker(Box::new(move || waker_wake.wake()));
        let data_source = Box::new(DataSource {
            ext: OH_AVDataSourceExt {
                size: -1,
                readAt: Some(read_at),
            },
            reader,
            sequential_pos: Mutex::new(0),
            wake,
        });
        Self {
            data_source: NonNull::from(Box::leak(data_source)),
        }
    }

    fn data_source(&self) -> &DataSource {
        // SAFETY: `data_source` is valid until `self` is dropped, and is only mutated before
        // AVPlayer has it, in `set_data_src`, which takes `&mut self`.
        unsafe { self.data_source.as_ref() }
    }

    pub fn length(&self) -> Option<u64> {
        self.data_source().reader.length()
    }

    /// Hands the data source to AVPlayer, which reads its size only now. Without a size, AVPlayer
    /// treats it as a live stream, which it reads in sequence and cannot seek in.
    pub fn set_data_src(&mut self, av_player: *mut OH_AVPlayer, size: Option<u64>) {
        let data_source = self.data_source.as_ptr();
        // SAFETY: `data_source` is valid, and nothing else accesses it until AVPlayer has it, as
        // `OhosPlayer` hands it over once.
        unsafe {
            (*data_source).ext.size = size.and_then(|size| i64::try_from(size).ok()).unwrap_or(-1);
        }
        // SAFETY: AVPlayer reads the data source and calls `read_at` with `data_source` until it
        // is released, which waits for the running reads. The owner of the AVPlayer wrapper
        // releases it before the wrapper, which owns `self`, is dropped.
        let ret = unsafe {
            OH_AVPlayer_SetDataSource(av_player, &raw mut (*data_source).ext, data_source.cast())
        };
        debug!("OH_AVPlayer_SetDataSource of size {size:?}: {ret:?}");
    }

    /// Ends the reads that wait for data, as releasing AVPlayer waits for the running reads.
    pub fn release(&self) {
        self.data_source().wake.release();
    }
}

impl Drop for MediaSourceWrapper {
    fn drop(&mut self) {
        // SAFETY: `data_source` was leaked from a `Box`, and AVPlayer, which is released before
        // `self` is dropped, no longer uses it.
        drop(unsafe { Box::from_raw(self.data_source.as_ptr()) });
    }
}

impl DataSource {
    fn read(&self, pos: i64, dest: &mut [u8]) -> i32 {
        let deadline = Instant::now() + MAX_READ_WAIT;
        loop {
            // Counting the wakes before reading makes sure that a wake right after a pending read
            // is not lost.
            let wakes = self.wake.wakes();
            let result = self.read_once(pos, dest);
            if result != ReadResult::Pending || !self.wake.wait(wakes, deadline) {
                return self.read_result(pos, result);
            }
        }
    }

    fn read_once(&self, pos: i64, dest: &mut [u8]) -> ReadResult {
        match u64::try_from(pos) {
            Ok(pos) => self.reader.read_at(pos, dest),
            Err(_) => {
                let mut sequential_pos = self.sequential_pos.lock().unwrap();
                let result = self.reader.read_at(*sequential_pos, dest);
                if let ReadResult::Data(len) = result {
                    *sequential_pos += len as u64;
                }
                result
            },
        }
    }

    fn read_result(&self, pos: i64, result: ReadResult) -> i32 {
        match result {
            // At most `dest.len()`, which came from an `i32`.
            ReadResult::Data(len) => len as i32,
            // AVPlayer reads again 100 to 500 ms later.
            ReadResult::Pending => 0,
            ReadResult::EndOfStream => END_OF_STREAM,
            ReadResult::Error(error) => {
                warn!("Could not read the media resource at {pos}: {error:?}");
                READ_ERROR
            },
        }
    }
}

extern "C" fn read_at(
    data: *mut OH_AVBuffer,
    length: i32,
    pos: i64,
    user_data: *mut c_void,
) -> i32 {
    assert!(!user_data.is_null(), "read_at: user_data must not be null");
    // SAFETY: `user_data` is the `DataSource` that `set_data_src` passed, which outlives AVPlayer.
    let data_source = unsafe { &*(user_data as *const DataSource) };
    // SAFETY: AVPlayer passes a valid buffer, which these functions check.
    let (addr, capacity) = unsafe { (OH_AVBuffer_GetAddr(data), OH_AVBuffer_GetCapacity(data)) };
    let Ok(len) = usize::try_from(length.min(capacity)) else {
        return READ_ERROR;
    };
    if addr.is_null() {
        return READ_ERROR;
    }
    // SAFETY: the buffer holds `capacity` bytes at `addr`, and nothing else uses it during the read.
    let dest = unsafe { slice::from_raw_parts_mut(addr, len) };
    data_source.read(pos, dest)
}
