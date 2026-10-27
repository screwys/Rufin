//! One row per tag kind drives the tag writers and the per-kind lists. Both kinds share the
//! `tags` table.

use crate::{AlbumMetadata, TagReference, TrackMetadata};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TagKind {
    Genre,
    Mood,
}

pub(crate) struct TagTable {
    pub(crate) kind: TagKind,
    /// The entity word in source URIs.
    pub(crate) name: &'static str,
    /// The tags a track's metadata names.
    pub(crate) track_tags: fn(&TrackMetadata) -> &[TagReference],
    /// The tags an album header names, for kinds albums carry.
    pub(crate) album_tags: Option<fn(&AlbumMetadata) -> &[TagReference]>,
}

pub(crate) const TAG_TABLES: [TagTable; 2] = [
    TagTable {
        kind: TagKind::Genre,
        name: "genre",
        track_tags: |metadata| &metadata.genres,
        album_tags: Some(|metadata| &metadata.genres),
    },
    TagTable {
        kind: TagKind::Mood,
        name: "mood",
        track_tags: |metadata| &metadata.moods,
        album_tags: None,
    },
];

impl TagKind {
    pub(crate) fn table(self) -> &'static TagTable {
        &TAG_TABLES[self as usize]
    }
}
