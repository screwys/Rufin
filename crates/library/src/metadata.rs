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

/// The TrackMetadata fields stored as typed `tracks` columns. `track_details.metadata` holds
/// the rest.
const THIN_TRACK_FIELDS: [&str; 13] = [
    "title",
    "artist",
    "album",
    "sort_text",
    "duration_millis",
    "disc_number",
    "track_number",
    "year",
    "release_date",
    "date_added",
    "source_format",
    "comment",
    "bpm",
];

/// The AlbumMetadata fields stored as typed `albums` columns.
const THIN_ALBUM_FIELDS: [&str; 7] = [
    "title",
    "artist",
    "sort_text",
    "year",
    "release_date",
    "date_added",
    "is_compilation",
];

fn without(value: impl Serialize, fields: &[&str]) -> serde_json::Result<String> {
    serde_json::to_string(&strip(serde_json::to_value(value)?, fields))
}

fn strip(mut value: serde_json::Value, fields: &[&str]) -> serde_json::Value {
    if let Some(object) = value.as_object_mut() {
        for field in fields {
            object.remove(*field);
        }
        object.retain(|_, value| {
            !value.is_null() && value.as_array().is_none_or(|items| !items.is_empty())
        });
    }
    value
}

/// The details row's JSON of whole TrackMetadata JSON, unknown fields included.
pub(crate) fn track_details_value(value: serde_json::Value) -> serde_json::Value {
    strip(value, &THIN_TRACK_FIELDS)
}

/// The album row's metadata JSON of whole AlbumMetadata JSON, unknown fields included.
pub(crate) fn album_details_value(value: serde_json::Value) -> serde_json::Value {
    strip(value, &THIN_ALBUM_FIELDS)
}

impl TrackMetadata {
    /// The details row's JSON: everything the typed columns do not hold.
    pub(crate) fn details_json(&self) -> serde_json::Result<String> {
        without(self, &THIN_TRACK_FIELDS)
    }
}

impl AlbumMetadata {
    /// The album row's metadata JSON: everything the typed columns do not hold.
    pub(crate) fn details_json(&self) -> serde_json::Result<String> {
        without(self, &THIN_ALBUM_FIELDS)
    }
}

/// SQL for the whole TrackMetadata JSON of a track row and its LEFT JOINed details row.
pub(crate) fn track_metadata_sql(track: &str, details: &str) -> String {
    format!(
        "json_patch(coalesce(json({details}.metadata),'{{}}'),json_object('title',{track}.title,'artist',{track}.display_artist,'album',{track}.display_album,'sort_text',{track}.sort_text,'duration_millis',{track}.duration_millis,'disc_number',{track}.disc_number,'track_number',{track}.track_number,'year',{track}.year,'release_date',{track}.release_date,'date_added',{track}.date_added,'source_format',{track}.source_format,'comment',{track}.comment,'bpm',{track}.bpm))"
    )
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

/// A date as ISO 8601 (`YYYY`, `YYYY-MM` or `YYYY-MM-DD`) from the forms tags and servers
/// write: ISO with or without a time, `DD.MM.YYYY`, `DD/MM/YYYY`, `YYYY/MM/DD` and `YYYYMMDD`.
/// Other text stays as written. Release dates sort by it and years come from it.
pub fn normalized_date(value: Option<String>) -> Option<String> {
    let value = value?.trim().to_string();
    if value.is_empty() {
        return None;
    }
    if let Some(prefix) = value.get(..10)
        && prefix.as_bytes().get(4) == Some(&b'-')
        && prefix.as_bytes().get(7) == Some(&b'-')
    {
        return Some(prefix.to_string());
    }
    let number = |text: &str,
                  digits: std::ops::RangeInclusive<usize>,
                  range: std::ops::RangeInclusive<u16>| {
        (digits.contains(&text.len()) && text.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| text.parse::<u16>().ok())
            .flatten()
            .filter(|value| range.contains(value))
    };
    let parts = match value.split(['.', '/']).collect::<Vec<_>>()[..] {
        [year, month, day] if year.len() == 4 => Some((year, month, day)),
        [day, month, year] => Some((year, month, day)),
        _ if value.len() == 8 => value
            .get(..4)
            .zip(value.get(4..6))
            .zip(value.get(6..))
            .map(|((year, month), day)| (year, month, day)),
        _ => None,
    };
    let Some((year, month, day)) = parts else {
        return Some(value);
    };
    match (
        number(year, 4..=4, 1..=9999),
        number(month, 1..=2, 1..=12),
        number(day, 1..=2, 1..=31),
    ) {
        (Some(year), Some(month), Some(day)) => Some(format!("{year:04}-{month:02}-{day:02}")),
        _ => Some(value),
    }
}
