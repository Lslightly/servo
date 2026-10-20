/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Servo's side of the RFC 9111 rules, which come from `http-cache-rules`: the
//! configuration of the cache, and the storage decisions Servo makes on top of
//! the rules.

use std::ops::Deref;
use std::sync::LazyLock;

use headers::{ContentLength, ContentRange, HeaderMapExt};
use http::{HeaderMap, StatusCode, header};
use http_cache_rules::{CacheConfig, StoredPolicy};
use malloc_size_of::{MallocSizeOf, MallocSizeOfOps};
use net_traits::request::{Destination, Request};
use serde::{Deserialize, Serialize};

/// Servo's cache is a private cache that stores 206 responses and serves ranges
/// from them, and stores bodies encoded, so a 304 must not change their
/// `Content-Encoding`, which the default configuration already provides.
pub(crate) static CONFIG: LazyLock<CacheConfig> = LazyLock::new(|| CacheConfig {
    partial_content: true,
    ..CacheConfig::default()
});

/// The RFC 9111 state of a stored entry.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(transparent)]
pub struct EntryPolicy(pub StoredPolicy);

impl Deref for EntryPolicy {
    type Target = StoredPolicy;

    fn deref(&self) -> &StoredPolicy {
        &self.0
    }
}

impl MallocSizeOf for EntryPolicy {
    fn size_of(&self, _ops: &mut MallocSizeOfOps) -> usize {
        self.0.heap_size()
    }
}

/// The largest response to an audio or video request that the cache stores.
const MAX_STORED_MEDIA_RESPONSE_SIZE: u64 = 8 * 1024 * 1024;

/// Whether the cache may store a response with `status` and `headers` to `request` under its
/// rules for audio and video.
///
/// Media elements request `bytes=N-` ranges and start every load at byte 0, so a stored partial
/// response is rarely used. A 200 or 206 to a media request is only stored if it holds the whole
/// resource, if its size is at most 8 MiB, as given by Content-Length or, for a 206, by
/// Content-Range and any Content-Length, and if it has no Content-Encoding. Other statuses follow
/// the usual rules.
pub(crate) fn may_store_media_response(
    request: &Request,
    status: StatusCode,
    headers: &HeaderMap,
) -> bool {
    if !matches!(request.destination, Destination::Audio | Destination::Video) {
        return true;
    }
    let content_length = headers.typed_get::<ContentLength>().map(|length| length.0);
    let size = if status == StatusCode::OK {
        content_length
    } else if status == StatusCode::PARTIAL_CONTENT {
        let content_range = headers.typed_get::<ContentRange>();
        match content_range.map(|range| (range.bytes_range(), range.bytes_len())) {
            Some((Some((0, last)), Some(length))) if last.checked_add(1) == Some(length) => {
                Some(length.max(content_length.unwrap_or(0)))
            },
            _ => None,
        }
    } else {
        return true;
    };
    !headers.contains_key(header::CONTENT_ENCODING) &&
        size.is_some_and(|size| size <= MAX_STORED_MEDIA_RESPONSE_SIZE)
}
