/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */
use std::sync::Arc;

#[cfg(not(sdk_api_21))]
use crate::ohos_media::dummy_source::MediaSourceWrapper;
#[cfg(sdk_api_21)]
use crate::ohos_media::source::MediaSourceWrapper;

/// Asks the media element to restart the fetch at a position. Unless the player was released, it
/// calls the given closure with whether the media element did, before the media element can push
/// the bytes of the restarted fetch.
type SeekDataClosure = Arc<dyn Fn(u64, &dyn Fn(bool)) + Send + Sync>;

pub struct MediaSourceBuilder {
    pub enough_data: Option<Box<dyn Fn() + Send + Sync>>,
    pub seek_data: Option<SeekDataClosure>,
}

impl MediaSourceBuilder {
    pub fn set_enough_data<F: Fn() + Send + Sync + Clone + 'static>(mut self, callback: F) -> Self {
        self.enough_data = Some(Box::new(callback));
        self
    }

    pub fn set_seek_data<F: Fn(u64, &dyn Fn(bool)) + Send + Sync + Clone + 'static>(
        mut self,
        callback: F,
    ) -> Self {
        self.seek_data = Some(Arc::new(callback));
        self
    }

    pub fn build(self) -> MediaSourceWrapper {
        #[cfg(not(sdk_api_21))]
        {
            MediaSourceWrapper::new()
        }
        #[cfg(sdk_api_21)]
        {
            MediaSourceWrapper::new(self)
        }
    }
}
