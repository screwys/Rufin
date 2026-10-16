//! Accepted metadata, shared by catalog storage, current views and transfers.
use crate::AudioProperties;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TrackMetadata {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_display_artist: Option<String>,
    pub sort_text: String,
    pub duration_millis: i64,
    pub disc_number: i64,
    pub track_number: i64,
    pub year: Option<i64>,
    pub release_date: Option<String>,
    pub date_added: Option<String>,
    pub source_format: Option<String>,
    pub audio_properties: AudioProperties,
    pub comment: Option<String>,
    pub bpm: Option<i64>,
    pub musicbrainz_recording_id: Option<String>,
    pub musicbrainz_release_track_id: Option<String>,
    pub musicbrainz_album_id: Option<String>,
    pub musicbrainz_release_group_id: Option<String>,
    pub primary_artist_musicbrainz_id: Option<String>,
    pub artists: Vec<ArtistCredit>,
    pub album_artists: Vec<ArtistCredit>,
    pub genres: Vec<TagReference>,
    pub moods: Vec<TagReference>,
    pub album_tags: Option<AlbumTags>,
    pub source_loudness: Option<NativeGain>,
    pub album_loudness: Option<NativeGain>,
}

/// None on TrackMetadata means the old cache never observed raw album tags.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AlbumTags {
    pub sort_text: String,
    pub is_compilation: Option<bool>,
    pub release_types: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AlbumMetadata {
    pub title: String,
    pub artist: String,
    pub sort_text: String,
    pub year: Option<i64>,
    pub release_date: Option<String>,
    pub date_added: Option<String>,
    pub musicbrainz_release_id: Option<String>,
    pub musicbrainz_release_group_id: Option<String>,
    pub is_compilation: Option<bool>,
    pub album_artists: Vec<ArtistCredit>,
    pub genres: Vec<TagReference>,
    pub release_types: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ArtistMetadata {
    pub name: String,
    pub sort_text: Option<String>,
    pub musicbrainz_artist_id: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TagMetadata {
    pub name: String,
    pub sort_text: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct FolderMetadata {
    pub name: Option<String>,
    pub sort_text: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ArtistCredit {
    pub object_id: String,
    pub name: String,
    pub sort_text: Option<String>,
    pub musicbrainz_artist_id: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TagReference {
    pub object_id: String,
    pub name: String,
    pub sort_text: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct NativeGain {
    pub integrated_lufs: Option<f64>,
    pub true_peak: Option<f64>,
    pub replay_gain_db: Option<f64>,
    pub replay_gain_peak: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, sqlx::FromRow)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TrackReference {
    pub media_uri: String,
    #[serde(flatten)]
    #[sqlx(json)]
    pub metadata: TrackMetadata,
    #[serde(skip)]
    #[sqlx(skip)]
    pub artwork_binding: Option<Vec<u8>>,
}

impl<'de> Deserialize<'de> for TrackReference {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut value = serde_json::Value::deserialize(deserializer)?;
        let media_uri = value
            .get("media_uri")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if let Some(metadata) = value.get_mut("metadata") {
            value = metadata.take();
        }
        if let Some(object) = value.as_object_mut() {
            object.retain(|_, value| !value.is_null());
        }
        let metadata = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
        Ok(Self {
            media_uri,
            metadata,
            artwork_binding: None,
        })
    }
}
