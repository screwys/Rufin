use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{SourceEntityKind, SourceId};
use localization::msgid;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum MetadataFieldKind {
    Text,
    Number,
    Date,
    Boolean,
    List,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MetadataDraft {
    pub kind: SourceEntityKind,
    pub fields: Vec<MetadataField>,
    pub artwork: ArtworkEditing,
    pub revision: Option<String>,
    pub track_count: usize,
}

impl MetadataDraft {
    pub fn values(&self) -> MetadataChanges {
        self.fields
            .iter()
            .map(|field| (field.key.clone(), field.value.clone()))
            .collect()
    }

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
            revision: None,
            track_count: 0,
        }
    }

    pub fn merge(&mut self, next: Self) {
        for field in &mut self.fields {
            if let Some(next) = next.fields.iter().find(|next| next.key == field.key) {
                field.mixed |= field.value != next.value || next.mixed;
                field.writable &= next.writable;
            } else if !(self.kind == SourceEntityKind::Artist
                && matches!(
                    field.key.as_str(),
                    "title" | "sort_title" | "musicbrainz_artist_id"
                ))
            {
                field.mixed |= !field.value.is_empty();
                field.writable = false;
            }
        }
        for mut field in next.fields {
            if !self.fields.iter().any(|existing| existing.key == field.key) {
                if !(self.kind == SourceEntityKind::Artist
                    && matches!(
                        field.key.as_str(),
                        "title" | "sort_title" | "musicbrainz_artist_id"
                    ))
                {
                    field.mixed = !field.value.is_empty();
                    field.writable = false;
                }
                self.fields.push(field);
            }
        }
        self.artwork.can_embed &= next.artwork.can_embed;
        if self.artwork.binding != next.artwork.binding {
            self.artwork.binding = None;
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

impl SourceMetadataError {
    pub fn combine(mut errors: Vec<Self>) -> Result<(), Self> {
        match errors.len() {
            0 => Ok(()),
            1 => Err(errors.pop().unwrap()),
            _ => Err(Self::Write(
                errors
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n"),
            )),
        }
    }
}

impl From<crate::SourceError> for SourceMetadataError {
    fn from(error: crate::SourceError) -> Self {
        Self::Write(error.to_string())
    }
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ArtworkStorage {
    Embedded,
    #[default]
    Folder,
    Server,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum LyricsStorage {
    Server,
    Embedded,
    Sidecar,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtworkEdit {
    pub change: ArtworkChange,
    pub storage: ArtworkStorage,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ArtworkChange {
    Replace(std::sync::Arc<ImageBytes>),
    Remove,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArtworkEditing {
    pub binding: Option<Vec<u8>>,
    pub storage: ArtworkStorage,
    pub can_embed: bool,
}

impl ArtworkEditing {
    pub fn with_binding(mut self, binding: Option<Vec<u8>>, server: bool) -> Self {
        let image = binding
            .as_deref()
            .and_then(|bytes| serde_json::from_slice::<Vec<library::ArtworkCandidate>>(bytes).ok())
            .and_then(|images| images.into_iter().next());
        self.storage = if server {
            ArtworkStorage::Server
        } else if self.can_embed
            && image.is_some_and(|image| {
                matches!(
                    image.image,
                    library::ArtworkImage::Local {
                        picture_index: Some(_),
                        ..
                    }
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
    let image = serde_json::from_slice::<Vec<library::ArtworkCandidate>>(binding?)
        .ok()?
        .into_iter()
        .next()?;
    match image.image {
        library::ArtworkImage::Local {
            path,
            picture_index: None,
            ..
        } => Some(path),
        _ => None,
    }
}
