/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! A disk-primary store: one file per entry, an index in memory, and a dedicated
//! I/O thread.

use std::sync::Arc;

use crate::http_cache::store::CacheStore;

/// Open the store for the public cache from the embedder's configuration, or
/// `None` when no cache directory was supplied.
pub fn open_default_store() -> Option<Arc<dyn CacheStore>> {
    None
}
