//! Shared integer identities used at Library and consumer boundaries.

use std::fmt;

use percent_encoding::{NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};

#[derive(
    Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
#[serde(transparent)]
pub struct SourceId(String);

impl SourceId {
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        assert!(!value.is_empty(), "SourceId cannot be empty");
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SourceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

pub fn source_entity_uri(source: &SourceId, entity_kind: &str, object_id: &str) -> String {
    debug_assert!(matches!(entity_kind, "track" | "album" | "artist"));
    format!(
        "{}{}",
        source_entity_prefix(source, entity_kind),
        encode(object_id)
    )
}

pub(crate) fn source_entity_prefix(source: &SourceId, entity_kind: &str) -> String {
    debug_assert!(matches!(entity_kind, "track" | "album" | "artist"));
    format!("rufin:source/{entity_kind}/{}/", encode(source.as_str()))
}

pub fn source_entity_parts(uri: &str) -> Option<(SourceId, String, String)> {
    let mut parts = uri.strip_prefix("rufin:source/")?.split('/');
    let kind = parts.next()?;
    if !matches!(kind, "track" | "album" | "artist") {
        return None;
    }
    let source = decode(parts.next()?)?;
    let object = decode(parts.next()?)?;
    if source.is_empty() || object.is_empty() || parts.next().is_some() {
        return None;
    }
    Some((SourceId::new(source), kind.to_string(), object))
}

pub fn cue_media_uri(
    cue_identity: &str,
    file_uri: &str,
    start_millis: i64,
    end_millis: i64,
) -> String {
    format!(
        "rufin:cue/{}/{}/{start_millis}/{end_millis}",
        encode(cue_identity),
        encode(file_uri),
    )
}

pub fn cue_media_parts(uri: &str) -> Option<(String, String, i64, i64)> {
    let mut parts = uri.strip_prefix("rufin:cue/")?.split('/');
    let cue_identity = decode(parts.next()?)?;
    let file_uri = decode(parts.next()?)?;
    let start = parts.next()?.parse().ok()?;
    let end = parts.next()?.parse().ok()?;
    if cue_identity.is_empty()
        || file_uri.is_empty()
        || start < 0
        || end <= start
        || parts.next().is_some()
    {
        return None;
    }
    Some((cue_identity, file_uri, start, end))
}

pub fn normalize_direct_media_uri(uri: &str) -> Option<String> {
    let mut parsed = url::Url::parse(uri).ok()?;
    if !matches!(parsed.scheme(), "file" | "http" | "https")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return None;
    }
    parsed.set_fragment(None);
    Some(parsed.to_string())
}

pub fn file_media_path(uri: &str) -> Option<std::path::PathBuf> {
    let parsed = url::Url::parse(uri).ok()?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return None;
    }
    parsed.to_file_path().ok()
}

fn encode(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

fn decode(value: &str) -> Option<String> {
    percent_decode_str(value)
        .decode_utf8()
        .ok()
        .map(|value| value.into_owned())
}

macro_rules! integer_key {
    ($name:ident) => {
        #[derive(
            Clone,
            Copy,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
            serde::Deserialize,
            serde::Serialize,
            sqlx::Type,
        )]
        #[repr(transparent)]
        #[sqlx(transparent)]
        pub struct $name(i64);

        impl $name {
            pub const fn from_raw(value: i64) -> Self {
                Self(value)
            }

            pub const fn raw(self) -> i64 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

integer_key!(SourceKey);
integer_key!(TrackKey);
integer_key!(AlbumKey);
integer_key!(ArtistKey);
integer_key!(GenreKey);
integer_key!(MoodKey);
integer_key!(FolderKey);
integer_key!(PlaylistKey);
integer_key!(PlaylistEntryKey);
integer_key!(SmartPlaylistKey);
integer_key!(ListenKey);
integer_key!(ListenOutboxKey);
integer_key!(LocalFileKey);
integer_key!(LocalAccessFileKey);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_identity_round_trips_percent_encoded_components() {
        let source = SourceId::new("same-kind/account:a/b%20");
        let uri = source_entity_uri(&source, "track", "opaque:/track%id");
        assert!(uri.contains("%2F"));
        assert_eq!(
            source_entity_parts(&uri),
            Some((source, "track".to_string(), "opaque:/track%id".to_string()))
        );
    }

    #[test]
    fn split_cue_identity_keeps_segment_and_backing_file() {
        let uri = cue_media_uri("cue/track:1", "file:///Music/disc.flac", 1_500, 52_000);
        assert_eq!(
            cue_media_parts(&uri),
            Some((
                "cue/track:1".to_string(),
                "file:///Music/disc.flac".to_string(),
                1_500,
                52_000
            ))
        );
    }

    #[test]
    fn direct_identity_rejects_credentials_and_normalizes_paths() {
        assert_eq!(
            normalize_direct_media_uri("https://example.test/a/../song.flac#fragment").as_deref(),
            Some("https://example.test/song.flac")
        );
        assert!(normalize_direct_media_uri("https://user:secret@example.test/song").is_none());
    }

    #[test]
    fn file_operation_paths_decode_spaces_unicode_and_literal_percent_signs() {
        let path = std::env::temp_dir()
            .join("Rufin music")
            .join("Björk 100% #1.flac");
        let uri = url::Url::from_file_path(&path).unwrap();
        assert_eq!(file_media_path(uri.as_str()), Some(path));
        assert!(file_media_path("https://example.test/song.flac").is_none());
        assert!(file_media_path("file://user:secret@localhost/song.flac").is_none());
    }
}
