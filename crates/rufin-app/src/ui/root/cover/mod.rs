use super::*;

mod cache_lookup;
mod decode_queue;
mod size_helpers;
mod tiles;
mod warming;

use size_helpers::*;

#[derive(Clone)]
pub(in crate::ui) struct CoverBinding {
    pub(in crate::ui) tile: ArtworkTileWeak,
    pub(in crate::ui) generation: u64,
}

#[derive(Clone)]
pub(in crate::ui) struct DecodedCover {
    pub(in crate::ui) pixbuf: Pixbuf,
    pub(in crate::ui) size: i32,
    pub(in crate::ui) bytes: usize,
    pub(in crate::ui) last_used: u64,
    pub(in crate::ui) priority: CoverDecodePriority,
}

pub(in crate::ui) struct DecodedCoverOrderEntry {
    pub(in crate::ui) key: String,
    pub(in crate::ui) last_used: u64,
}

pub(in crate::ui) struct CoverDecodeJob {
    pub(in crate::ui) key: String,
    pub(in crate::ui) path: PathBuf,
    pub(in crate::ui) size: i32,
    pub(in crate::ui) priority: CoverDecodePriority,
    pub(in crate::ui) requires_live_binding: bool,
}

pub(in crate::ui) struct CoverWarmJob {
    pub(in crate::ui) key: String,
    pub(in crate::ui) image_ref: ImageRef,
    pub(in crate::ui) fetch_size: u32,
    pub(in crate::ui) size: i32,
}

pub(in crate::ui) struct CoverPathLookupRequest {
    pub(in crate::ui) key: String,
    pub(in crate::ui) image_ref: ImageRef,
    pub(in crate::ui) fetch_size: u32,
    pub(in crate::ui) size: i32,
    pub(in crate::ui) intent: CoverPathLookupIntent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::ui) enum CoverPathLookupIntent {
    Visible,
    Warm,
}

impl CoverPathLookupIntent {
    fn coalesce(self, next: Self) -> Self {
        if self == Self::Visible || next == Self::Visible {
            Self::Visible
        } else {
            Self::Warm
        }
    }
}

pub(in crate::ui) fn record_cover_path_lookup_request(
    lookups: &mut HashMap<String, CoverPathLookupIntent>,
    key: String,
    intent: CoverPathLookupIntent,
) -> bool {
    if let Some(existing) = lookups.get_mut(&key) {
        *existing = existing.coalesce(intent);
        false
    } else {
        lookups.insert(key, intent);
        true
    }
}

pub(in crate::ui) struct FirstRunCoverPrimeJob {
    pub(in crate::ui) key: String,
    pub(in crate::ui) image_ref: ImageRef,
    pub(in crate::ui) fetch_size: u32,
    pub(in crate::ui) size: i32,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(in crate::ui) enum CoverDecodePriority {
    Visible,
    Warm,
}

impl CoverDecodePriority {
    pub(in crate::ui) fn glib_priority(self) -> glib::Priority {
        match self {
            Self::Visible => glib::Priority::DEFAULT,
            Self::Warm => glib::Priority::LOW,
        }
    }
}
