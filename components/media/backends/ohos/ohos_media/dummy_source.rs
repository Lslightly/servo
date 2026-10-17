/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */
use std::sync::Arc;

use servo_media_cache::MediaReader;

pub struct MediaSourceWrapper {}

impl MediaSourceWrapper {
    pub fn new(_reader: Arc<MediaReader>) -> Self {
        Self {}
    }

    pub fn length(&self) -> Option<u64> {
        None
    }

    pub fn set_data_src(
        &mut self,
        _av_player: *mut ohos_media_sys::avplayer_base::OH_AVPlayer,
        _size: Option<u64>,
    ) {
        // No-op for dummy source.
    }

    pub fn release(&self) {}
}
