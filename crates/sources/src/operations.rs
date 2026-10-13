use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{SourceEntityKind, SourceId};
use localization::msgid;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum MetadataFieldKind {
    Text,
    Number,
    Date,
    Boolean,
    List,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct MetadataField {
    pub key: String,
    pub label: String,
    pub kind: MetadataFieldKind,
    pub value: String,
    pub writable: bool,
    pub mixed: bool,
    pub suggestion: Option<String>,
}

pub type MetadataChanges = std::collections::BTreeMap<String, String>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataDraft {
    pub kind: SourceEntityKind,
    pub fields: Vec<MetadataField>,
    pub artwork: ArtworkEditing,
    pub source_search: bool,
    pub revision: Option<String>,
    pub track_count: usize,
}

impl MetadataDraft {
    pub fn new(kind: SourceEntityKind, mut fields: Vec<MetadataField>) -> Self {
        if kind != SourceEntityKind::Track
            && let Some(field) = fields.iter_mut().find(|field| field.key == "comment")
        {
            field.label = msgid("Overview").into();
        }
        Self {
            kind,
            fields,
            artwork: ArtworkEditing::default(),
            source_search: false,
            revision: None,
            track_count: 0,
        }
    }

    pub(crate) fn suggest(&mut self, key: &str, value: Option<String>) {
        if let Some(field) = self.fields.iter_mut().find(|field| field.key == key)
            && field.value.is_empty()
        {
            field.suggestion = value.filter(|value| !value.is_empty());
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataEdit {
    pub kind: SourceEntityKind,
    pub changes: MetadataChanges,
    pub artwork: Option<ArtworkEdit>,
}

impl MetadataField {
    pub fn new(key: &str, value: impl Into<String>, writable: bool) -> Self {
        use MetadataFieldKind::*;
        let (label, kind) = match key {
            "title" => (msgid("Title"), Text),
            "sort_title" => (msgid("Sort title"), Text),
            "artist" => (msgid("Artists"), List),
            "album" => (msgid("Album"), Text),
            "album_artist" => (msgid("Album artists"), List),
            "track_number" => (msgid("Track number"), Number),
            "disc_number" => (msgid("Disc number"), Number),
            "year" => (msgid("Year"), Number),
            "recording_date" => (msgid("Date"), Date),
            "genre" => (msgid("Genres"), List),
            "comment" => (msgid("Comment"), Text),
            "bpm" => (msgid("BPM"), Number),
            "locked" => (msgid("Lock metadata"), Boolean),
            "musicbrainz_recording_id" => (msgid("MusicBrainz recording ID"), Text),
            "musicbrainz_release_track_id" => (msgid("MusicBrainz release track ID"), Text),
            "musicbrainz_album_id" => (msgid("MusicBrainz release ID"), Text),
            "musicbrainz_release_group_id" => (msgid("MusicBrainz release group ID"), Text),
            "musicbrainz_artist_id" => (msgid("MusicBrainz artist ID"), Text),
            _ => (key, Text),
        };
        Self {
            key: key.into(),
            label: label.into(),
            kind,
            value: value.into(),
            writable,
            mixed: false,
            suggestion: None,
        }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Error))]
pub enum SourceMetadataError {
    #[error("{0}")]
    InvalidInput(String),
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
    #[error("some changes were saved: {reason}")]
    PartiallySaved { reason: String },
    #[error("metadata failed: {0}")]
    Write(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct ImageBytes {
    pub bytes: Vec<u8>,
    pub content_type: Option<String>,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl ImageBytes {
    pub fn bytes(&self) -> Vec<u8> {
        self.bytes.clone()
    }

    pub fn content_type(&self) -> Option<String> {
        self.content_type.clone()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
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
    pub fn with_binding(mut self, binding: Option<Vec<u8>>, server: bool) -> Self {
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

pub(crate) fn metadata_refresh_error(error: crate::SourceError) -> SourceMetadataError {
    SourceMetadataError::SavedRefreshFailed(error.to_string())
}

pub fn finish_metadata_save(
    result: Result<(), SourceMetadataError>,
    refresh: Result<(), SourceMetadataError>,
    saved: bool,
) -> Result<(), SourceMetadataError> {
    let message = match result {
        Ok(()) => {
            return refresh.map_err(|error| match error {
                SourceMetadataError::SavedRefreshFailed(_)
                | SourceMetadataError::PartiallySaved { .. } => error,
                error => SourceMetadataError::SavedRefreshFailed(error.to_string()),
            });
        }
        Err(SourceMetadataError::PartiallySaved { reason }) => reason,
        Err(error) if saved => error.to_string(),
        Err(error) => return Err(error),
    };
    let message = match refresh {
        Ok(()) => message,
        Err(error) => format!("{message}. Source refresh also failed: {error}"),
    };
    Err(SourceMetadataError::PartiallySaved { reason: message })
}

pub fn artwork_file(binding: Option<&[u8]>) -> Option<String> {
    let binding = binding?;
    if let Ok(artist) = serde_json::from_slice::<library::ArtistArtworkBinding>(binding) {
        return artwork_file(artist.source.as_deref());
    }
    match serde_json::from_slice::<crate::LocalImageRef>(binding).ok()? {
        crate::LocalImageRef::File { path, .. } => Some(path),
        crate::LocalImageRef::Embedded { .. } => None,
    }
}
