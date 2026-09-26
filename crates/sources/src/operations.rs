use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::SourceId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetadataFieldKind {
    Text,
    Number,
    Date,
    Boolean,
    List,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataField {
    pub key: String,
    pub label: String,
    pub kind: MetadataFieldKind,
    pub value: String,
    pub writable: bool,
    pub mixed: bool,
}

pub type MetadataChanges = std::collections::BTreeMap<String, String>;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TrackMetadataValues {
    pub title: String,
    pub sort_title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub track_number: Option<u16>,
    pub disc_number: Option<u16>,
    pub year: Option<u16>,
    pub genre: Option<String>,
    pub comment: Option<String>,
    pub bpm: Option<u16>,
    pub locked: Option<bool>,
    pub musicbrainz_recording_id: Option<String>,
    pub musicbrainz_release_track_id: Option<String>,
    pub musicbrainz_album_id: Option<String>,
    pub musicbrainz_release_group_id: Option<String>,
    pub musicbrainz_artist_id: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TrackMetadataWritable {
    pub title: bool,
    pub sort_title: bool,
    pub artist: bool,
    pub album: bool,
    pub album_artist: bool,
    pub track_number: bool,
    pub disc_number: bool,
    pub year: bool,
    pub genre: bool,
    pub comment: bool,
    pub bpm: bool,
    pub locked: bool,
    pub musicbrainz_recording_id: bool,
    pub musicbrainz_release_track_id: bool,
    pub musicbrainz_album_id: bool,
    pub musicbrainz_release_group_id: bool,
    pub musicbrainz_artist_id: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrackMetadataEdit {
    pub extra: MetadataChanges,
    pub values: TrackMetadataValues,
    pub changed: TrackMetadataWritable,
    pub artwork: Option<ArtworkEdit>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrackMetadata {
    pub extra: Vec<MetadataField>,
    pub artwork: ArtworkEditing,
    pub writable: TrackMetadataWritable,
    pub source_search: bool,
    pub revision: Option<String>,
    pub source_values: TrackMetadataValues,
    pub values: TrackMetadataValues,
    pub rufin_filled: TrackMetadataWritable,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AlbumMetadataValues {
    pub title: String,
    pub sort_title: Option<String>,
    pub artist: Option<String>,
    pub album_artist: Option<String>,
    pub year: Option<u16>,
    pub genre: Option<String>,
    pub comment: Option<String>,
    pub locked: Option<bool>,
    pub musicbrainz_album_id: Option<String>,
    pub musicbrainz_release_group_id: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AlbumMetadataWritable {
    pub title: bool,
    pub sort_title: bool,
    pub artist: bool,
    pub album_artist: bool,
    pub year: bool,
    pub genre: bool,
    pub comment: bool,
    pub locked: bool,
    pub musicbrainz_album_id: bool,
    pub musicbrainz_release_group_id: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlbumMetadataEdit {
    pub extra: MetadataChanges,
    pub values: AlbumMetadataValues,
    pub changed: AlbumMetadataWritable,
    pub artwork: Option<ArtworkEdit>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlbumMetadata {
    pub extra: Vec<MetadataField>,
    pub artwork: ArtworkEditing,
    pub writable: AlbumMetadataWritable,
    pub source_search: bool,
    pub revision: Option<String>,
    pub source_values: AlbumMetadataValues,
    pub values: AlbumMetadataValues,
    pub rufin_filled: AlbumMetadataWritable,
    pub track_count: usize,
    pub mixed: AlbumMetadataMixed,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AlbumMetadataMixed {
    pub title: bool,
    pub sort_title: bool,
    pub artist: bool,
    pub album_artist: bool,
    pub year: bool,
    pub genre: bool,
    pub comment: bool,
    pub musicbrainz_album_id: bool,
    pub musicbrainz_release_group_id: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ArtistMetadataValues {
    pub name: String,
    pub sort_name: Option<String>,
    pub genre: Option<String>,
    pub comment: Option<String>,
    pub locked: Option<bool>,
    pub musicbrainz_artist_id: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ArtistMetadataWritable {
    pub name: bool,
    pub sort_name: bool,
    pub genre: bool,
    pub comment: bool,
    pub locked: bool,
    pub musicbrainz_artist_id: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtistMetadataEdit {
    pub extra: MetadataChanges,
    pub values: ArtistMetadataValues,
    pub changed: ArtistMetadataWritable,
    pub artwork: Option<ArtworkEdit>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtistMetadata {
    pub extra: Vec<MetadataField>,
    pub artwork: ArtworkEditing,
    pub writable: ArtistMetadataWritable,
    pub source_search: bool,
    pub revision: Option<String>,
    pub source_values: ArtistMetadataValues,
    pub values: ArtistMetadataValues,
    pub rufin_filled: ArtistMetadataWritable,
    pub track_count: usize,
    pub mixed: ArtistMetadataMixed,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ArtistMetadataMixed {
    pub name: bool,
    pub sort_name: bool,
    pub genre: bool,
    pub comment: bool,
    pub musicbrainz_artist_id: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MetadataEdit {
    Track(TrackMetadataEdit),
    Album(AlbumMetadataEdit),
    Artist(ArtistMetadataEdit),
}

impl MetadataEdit {
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::Track(_) => "track",
            Self::Album(_) => "album",
            Self::Artist(_) => "artist",
        }
    }

    pub fn artwork(&self) -> Option<&ArtworkEdit> {
        match self {
            Self::Track(edit) => edit.artwork.as_ref(),
            Self::Album(edit) => edit.artwork.as_ref(),
            Self::Artist(edit) => edit.artwork.as_ref(),
        }
    }

    pub(crate) fn artwork_mut(&mut self) -> &mut Option<ArtworkEdit> {
        match self {
            Self::Track(edit) => &mut edit.artwork,
            Self::Album(edit) => &mut edit.artwork,
            Self::Artist(edit) => &mut edit.artwork,
        }
    }

    pub(crate) fn tags_changed(&self) -> bool {
        !self.extra().is_empty()
            || match self {
                Self::Track(edit) => edit.changed != Default::default(),
                Self::Album(edit) => edit.changed != Default::default(),
                Self::Artist(edit) => edit.changed != Default::default(),
            }
    }

    pub(crate) fn extra(&self) -> &MetadataChanges {
        match self {
            Self::Track(edit) => &edit.extra,
            Self::Album(edit) => &edit.extra,
            Self::Artist(edit) => &edit.extra,
        }
    }

    pub(crate) fn artist_name(&self) -> Option<&str> {
        match self {
            Self::Artist(edit) => Some(&edit.values.name),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum SourceMetadataError {
    #[error("metadata editing is unavailable")]
    Unavailable,
    #[error("metadata changed before it was saved")]
    Conflict,
    #[error("local access is required for {source_path}")]
    LocalAccessRequired {
        source_id: SourceId,
        source_path: String,
    },
    #[error("metadata was saved but its source refresh failed: {0}")]
    SavedRefreshFailed(String),
    #[error("some changes were saved: {message}")]
    PartiallySaved {
        message: String,
        outcome: Option<library::ScanOutcome>,
    },
    #[error("metadata failed: {0}")]
    Write(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImageBytes {
    pub bytes: Vec<u8>,
    pub content_type: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ArtworkStorage {
    Embedded,
    #[default]
    Folder,
    Server,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtworkEdit {
    pub change: ArtworkChange,
    pub storage: ArtworkStorage,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ArtworkChange {
    Replace(std::sync::Arc<ImageBytes>),
    Remove,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ArtworkEditing {
    pub binding: Option<Vec<u8>>,
    pub storage: ArtworkStorage,
    pub can_embed: bool,
}

impl ArtworkEditing {
    pub(crate) fn with_binding(mut self, binding: Option<Vec<u8>>, server: bool) -> Self {
        let source = binding
            .as_deref()
            .and_then(|bytes| serde_json::from_slice::<library::ArtistArtworkBinding>(bytes).ok());
        let image = source
            .as_ref()
            .map_or(binding.as_deref(), |artist| artist.source.as_deref());
        self.storage = if server {
            ArtworkStorage::Server
        } else if self.can_embed
            && image.is_some_and(|bytes| {
                matches!(
                    serde_json::from_slice::<crate::LocalImageRef>(bytes),
                    Ok(crate::LocalImageRef::Embedded { .. })
                )
            })
        {
            ArtworkStorage::Embedded
        } else {
            ArtworkStorage::Folder
        };
        self.binding = binding;
        self
    }
}

pub(crate) fn finish_metadata_save(
    result: Result<(), SourceMetadataError>,
    refresh: Result<library::ScanOutcome, SourceMetadataError>,
    saved: bool,
) -> Result<library::ScanOutcome, SourceMetadataError> {
    let message = match result {
        Ok(()) => {
            return refresh.map_err(|error| match error {
                SourceMetadataError::SavedRefreshFailed(_) => error,
                error => SourceMetadataError::SavedRefreshFailed(error.to_string()),
            });
        }
        Err(SourceMetadataError::PartiallySaved { message, .. }) => message,
        Err(error) if saved => error.to_string(),
        Err(error) => return Err(error),
    };
    let (message, outcome) = match refresh {
        Ok(outcome) => (message, Some(outcome)),
        Err(error) => (
            format!("{message}. Source refresh also failed: {error}"),
            None,
        ),
    };
    Err(SourceMetadataError::PartiallySaved { message, outcome })
}

pub(crate) fn artwork_file(binding: Option<&[u8]>) -> Option<String> {
    let binding = binding?;
    if let Ok(artist) = serde_json::from_slice::<library::ArtistArtworkBinding>(binding) {
        return artwork_file(artist.source.as_deref());
    }
    match serde_json::from_slice::<crate::LocalImageRef>(binding).ok()? {
        crate::LocalImageRef::File { path, .. } => Some(path),
        crate::LocalImageRef::Embedded { .. } => None,
    }
}
