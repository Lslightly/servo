/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

#![deny(unsafe_code)]

mod font_descriptor;
mod font_identifier;
mod font_template;
mod system_font_service_proxy;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

pub use font_descriptor::*;
pub use font_identifier::*;
pub use font_template::*;
use malloc_size_of_derive::MallocSizeOf;
pub use memmap2::Mmap;
use serde::{Deserialize, Serialize};
use servo_arc::Arc as ServoArc;
use servo_base::generic_channel::GenericSharedMemory;
use style::font_face::Descriptors;
use style::stylesheets::LockedFontFaceRule;
pub use system_font_service_proxy::*;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum WebFontLoadEvent {
    LoadedSuccessfully,
    UnblockedFontReadyPromise,
}

pub type StylesheetWebFontLoadFinishedCallback =
    Arc<dyn Fn(WebFontLoadEvent) + Send + Sync + 'static>;

#[derive(Serialize, Deserialize)]
struct SerializableFontData(Arc<GenericSharedMemory>);

impl From<FontData> for SerializableFontData {
    fn from(value: FontData) -> Self {
        match value {
            FontData::MemoryMapped(mmap) => {
                SerializableFontData(Arc::new(GenericSharedMemory::from_bytes(&mmap)))
            },
            FontData::SharedMemory(generic_shared_memory) => {
                SerializableFontData(generic_shared_memory)
            },
        }
    }
}

impl From<SerializableFontData> for FontData {
    fn from(value: SerializableFontData) -> Self {
        FontData::SharedMemory(value.0)
    }
}

/// A data structure to store data for fonts. Data is stored internally in an
/// [`GenericSharedMemory`] handle, so that it can be sent without serialization
/// across IPC channels.
#[derive(Clone, Deserialize, Serialize, MallocSizeOf)]
#[serde(from = "SerializableFontData", into = "SerializableFontData")]
pub enum FontData {
    MemoryMapped(#[conditional_malloc_size_of] Arc<Mmap>),
    SharedMemory(#[conditional_malloc_size_of] Arc<GenericSharedMemory>),
}

impl FontData {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self::SharedMemory(Arc::new(GenericSharedMemory::from_bytes(bytes)))
    }

    pub fn from_mmap(mmap: Mmap) -> Self {
        Self::MemoryMapped(Arc::new(mmap))
    }

    pub fn as_ipc_shared_memory(self) -> Arc<GenericSharedMemory> {
        match self {
            FontData::MemoryMapped(mmap) => Arc::new(GenericSharedMemory::from_bytes(&mmap)),
            FontData::SharedMemory(generic_shared_memory) => generic_shared_memory,
        }
    }

    /// This is in single process mode more efficient because we do not have to copy the vector.
    pub fn from_vec(bytes: Vec<u8>) -> Self {
        Self::SharedMemory(Arc::new(GenericSharedMemory::from_vec(bytes)))
    }
}

impl AsRef<[u8]> for FontData {
    fn as_ref(&self) -> &[u8] {
        match &self {
            FontData::MemoryMapped(mmap) => mmap,
            FontData::SharedMemory(generic_shared_memory) => generic_shared_memory,
        }
    }
}

/// Raw font data and an index
///
/// If the font data is of a TTC (TrueType collection) file, then the index of a specific font within
/// the collection. If the font data is for is single font then the index will always be 0.
#[derive(Deserialize, Clone, Serialize, MallocSizeOf)]
pub struct FontDataAndIndex {
    /// The raw font file data (.ttf, .otf, .ttc, etc)
    pub data: FontData,
    /// The index of the font within the file (0 if the file is not a ttc)
    pub index: u32,
}

#[derive(Copy, Clone, PartialEq)]
pub enum FontDataError {
    FailedToLoad,
}

/// Describes how the set of active `@font-face` rules was changed after a call to `FontContext::rebuild_font_face_set`.
#[derive(Clone, Default)]
pub struct WebFontSetDifference {
    /// A list of `@font-face` rules that were added in this update.
    pub added_font_faces: Vec<ServoArc<FontFaceRuleInfo>>,
    /// A list of `@font-face` rules that were removed in this update.
    pub removed_font_faces: Vec<ServoArc<FontFaceRuleInfo>>,
    /// Whether the cascade index of any `@font-face` rule changed during this update.
    ///
    /// This can cause different fonts to be selected during font matching.
    pub cascade_index_of_any_rule_changed: bool,
}

impl WebFontSetDifference {
    /// Returns `true` iff the font face set remained unchanged by the update.
    pub fn is_empty(&self) -> bool {
        self.added_font_faces.is_empty() && self.removed_font_faces.is_empty()
    }
}

/// How far the font described by an `@font-face` rule has got towards being usable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WebFontLoadState {
    /// The face has not started loading.
    Unloaded,
    /// The face is being fetched, or is waiting to be.
    Loading,
    /// The face has a usable [`FontTemplate`].
    Loaded,
    /// None of the sources of the face could be used.
    Failed,
}

impl From<usize> for WebFontLoadState {
    fn from(value: usize) -> Self {
        match value {
            0 => Self::Unloaded,
            1 => Self::Loading,
            2 => Self::Loaded,
            _ => Self::Failed,
        }
    }
}

#[derive(MallocSizeOf)]
pub struct FontFaceRuleInfo {
    /// The index of this `@font-face` in the cascade, relative to all
    /// other `@font-face` rules.
    pub cascade_index: AtomicUsize,
    /// The descriptors on the `@font-face` rule.
    pub descriptors: Descriptors,
    /// The CSS rule that created this `@font-face`.
    ///
    /// This does *not* uniquely identify this struct across updates
    /// to the set of live `@font-face` rules.
    #[conditional_malloc_size_of]
    pub rule: ServoArc<LockedFontFaceRule>,
    /// The [`WebFontLoadState`] of this rule, shared with the `FontFace` object that
    /// exposes it to script.
    load_state: AtomicUsize,
}

impl FontFaceRuleInfo {
    pub fn new(
        cascade_index: usize,
        descriptors: Descriptors,
        rule: ServoArc<LockedFontFaceRule>,
    ) -> Self {
        Self {
            cascade_index: AtomicUsize::new(cascade_index),
            descriptors,
            rule,
            load_state: AtomicUsize::new(WebFontLoadState::Unloaded as usize),
        }
    }

    pub fn load_state(&self) -> WebFontLoadState {
        self.load_state.load(Ordering::SeqCst).into()
    }

    pub fn set_load_state(&self, state: WebFontLoadState) {
        self.load_state.store(state as usize, Ordering::SeqCst);
    }
}
