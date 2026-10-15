/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use core::slice;
use std::sync::{Arc, Mutex};
use std::thread;

use log::warn;
use ohos_media_sys::avbuffer::OH_AVBuffer;
use ohos_media_sys::avcodec_base::OH_AVDataSourceExt;
use ohos_media_sys::avplayer::OH_AVPlayer_SetDataSource;
use ohos_media_sys::avplayer_base::OH_AVPlayer;

use crate::ohos_media::playback_buffer::PlaybackBuffer;
use crate::ohos_media::source_builder::MediaSourceBuilder;

pub struct MediaSourceWrapper {
    pub(crate) data_src: ohos_media_sys::avcodec_base::OH_AVDataSourceExt,
    total_media_source_size: usize,
    playback_buffer: Arc<Mutex<PlaybackBuffer>>,
    closure_handle: *mut Box<dyn Fn(*mut u8, u32, i64) -> i32>,
}

impl MediaSourceWrapper {
    pub fn builder() -> MediaSourceBuilder {
        MediaSourceBuilder {
            enough_data: None,
            seek_data: None,
        }
    }
}

impl MediaSourceWrapper {
    pub fn new(source_cb: MediaSourceBuilder) -> Self {
        let playback_buffer = Arc::new(Mutex::new(PlaybackBuffer::new(source_cb.enough_data)));

        let playback_buffer_clone = playback_buffer.clone();

        let read_at_closure = move |buffer: *mut u8, length: u32, pos: i64| -> i32 {
            log::debug!(
                "Inside Read At Closure: {:p}, length: {}, pos: {}",
                buffer,
                length,
                pos
            );
            let Ok(pos) = u64::try_from(pos) else {
                return 0;
            };
            let buffer = unsafe { slice::from_raw_parts_mut(buffer, length as usize) };
            let (read_bytes, restart_pos) = {
                let mut playback_buffer_lock = playback_buffer_clone.lock().unwrap();
                playback_buffer_lock.read_data(buffer, pos)
            };
            // The playback_buffer lock must be released before calling the seek
            // closure, which blocks on IPC with the script thread. Holding the
            // lock here would deadlock if the script thread is simultaneously
            // trying to push_data (which also acquires this lock).
            if let Some(restart_pos) = restart_pos &&
                let Some(seek_closure) = &source_cb.seek_data
            {
                let restart = {
                    let seek_closure = seek_closure.clone();
                    let playback_buffer = playback_buffer_clone.clone();
                    move || {
                        seek_closure(restart_pos, &|success| {
                            playback_buffer.lock().unwrap().restarted(success)
                        })
                    }
                };
                // Restarting the fetch waits for the script thread, which must not hold up the
                // reads of AVPlayer.
                if let Err(error) = thread::Builder::new()
                    .name("Media Fetch Restart".to_owned())
                    .spawn(restart)
                {
                    warn!("Could not restart the fetch on its own thread: {error}");
                    seek_closure(restart_pos, &|success| {
                        playback_buffer_clone.lock().unwrap().restarted(success)
                    });
                }
            }
            read_bytes as i32
        };

        let box_closure: Box<dyn Fn(*mut u8, u32, i64) -> i32> = Box::new(read_at_closure);
        // Double boxing is needed because we need to convert the closure into a raw pointer to pass to C,
        // but Rust does not allow us to directly convert a Box<dyn Fn> into a raw pointer, we need to first box it
        // and then convert the box into a raw pointer.
        let double_box_closure = Box::new(box_closure);

        extern "C" fn oh_avdatasource_read_at_callback(
            data: *mut OH_AVBuffer,
            length: i32,
            pos: i64,
            user_data: *mut std::ffi::c_void,
        ) -> i32 {
            assert!(
                !user_data.is_null(),
                "oh_avdatasource_read_at_callback: user_data must not be null"
            );
            let f = unsafe { &*(user_data as *mut Box<dyn Fn(*mut u8, u32, i64) -> i32>) };
            let buffer_addr = unsafe { ohos_media_sys::avbuffer::OH_AVBuffer_GetAddr(data) };
            f(buffer_addr, length as u32, pos)
        }
        let data_src = OH_AVDataSourceExt {
            size: 0,
            readAt: Some(oh_avdatasource_read_at_callback),
        };

        let raw_ptr_f = Box::into_raw(double_box_closure);

        Self {
            data_src,
            total_media_source_size: 0,
            playback_buffer,
            closure_handle: raw_ptr_f,
        }
    }

    pub fn set_input_size(&mut self, size: usize) {
        log::debug!("Setting input size to {}", size);
        if self.total_media_source_size == 0 {
            self.total_media_source_size = size;
            self.data_src.size = size as i64;
            self.playback_buffer.lock().unwrap().set_size(size as u64);
        }
    }

    pub fn set_seekable(&self, seekable: bool) {
        self.playback_buffer.lock().unwrap().set_seekable(seekable);
    }

    pub fn push_data(&self, data: Vec<u8>) -> bool {
        let mut playback_buffer_lock = self.playback_buffer.lock().unwrap();
        playback_buffer_lock.push_buffer(&data)
    }

    pub fn end_of_stream(&self) {
        self.playback_buffer.lock().unwrap().end_of_stream();
    }

    pub fn set_data_src(&mut self, av_player: *mut OH_AVPlayer) {
        unsafe {
            OH_AVPlayer_SetDataSource(
                av_player,
                &mut self.data_src as *mut OH_AVDataSourceExt,
                self.closure_handle as *mut std::ffi::c_void,
            );
        }
    }
}

impl Drop for MediaSourceWrapper {
    fn drop(&mut self) {
        unsafe {
            let box_closure = Box::from_raw(self.closure_handle);
            drop(box_closure);
        }
    }
}
