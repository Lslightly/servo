/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Cache keys and the identifiers of the entries stored under them.

use malloc_size_of_derive::MallocSizeOf;
use net_traits::request::Request;
use serde::{Deserialize, Serialize};
use servo_url::ServoUrl;
use sha1::{Digest, Sha1};

/// Identifies one stored variant: the upper 56 bits are [`CacheKey::hash`], the
/// lower 8 bits the variant slot under that key.
pub type EntryId = u64;

/// Number of bits an [`EntryId`] reserves for the variant slot.
const SLOT_BITS: u32 = 8;

/// How many variants may be stored under a single key.
pub const MAX_SLOTS: u8 = 32;

pub fn entry_id(key_hash: u64, slot: u8) -> EntryId {
    (key_hash << SLOT_BITS) | slot as u64
}

pub fn entry_key_hash(id: EntryId) -> u64 {
    id >> SLOT_BITS
}

pub fn entry_slot(id: EntryId) -> u8 {
    (id & ((1 << SLOT_BITS) - 1)) as u8
}

/// The key used to differentiate requests in the cache.
#[derive(Clone, Debug, Deserialize, Eq, Hash, MallocSizeOf, PartialEq, Serialize)]
pub struct CacheKey {
    url: ServoUrl,
    /// The HTTP cache partition (fetch step 8.23). Reserved: always `None` until
    /// cache partitioning lands, so that adding it is not a format bump.
    partition: Option<String>,
}

impl CacheKey {
    /// Create a cache-key from a request.
    pub fn new(request: &Request) -> CacheKey {
        CacheKey::from_url(request.current_url())
    }

    /// Create a cache-key from a resolved URL. The credentials and the fragment of
    /// the URL are left out: neither names another resource, and the key is
    /// written into the entry.
    pub fn from_url(mut url: ServoUrl) -> CacheKey {
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_fragment(None);
        CacheKey {
            url,
            partition: None,
        }
    }

    /// The URL this key was built from.
    pub fn url(&self) -> &ServoUrl {
        &self.url
    }

    /// A stable 56-bit hash of the key, used to name entry files and to index
    /// them in memory. Collisions are resolved by comparing the full key stored
    /// in the entry's metadata.
    pub fn hash(&self) -> u64 {
        let mut hasher = Sha1::new();
        if let Some(partition) = &self.partition {
            hasher.update(partition.as_bytes());
        }
        hasher.update(b"\0");
        hasher.update(self.url.as_str().as_bytes());
        let digest = hasher.finalize();
        let leading: [u8; 8] = digest[..8].try_into().expect("sha1 digest is 20 bytes");
        u64::from_be_bytes(leading) >> SLOT_BITS
    }
}
