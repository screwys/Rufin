use serde::{Deserialize, Serialize};
use thiserror::Error;

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
    pub values: TrackMetadataValues,
    pub changed: TrackMetadataWritable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrackMetadata {
    pub track_key: library::TrackKey,
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
    pub values: AlbumMetadataValues,
    pub changed: AlbumMetadataWritable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlbumMetadata {
    pub album_key: library::AlbumKey,
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
    pub values: ArtistMetadataValues,
    pub changed: ArtistMetadataWritable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtistMetadata {
    pub artist_key: library::ArtistKey,
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

#[derive(Debug, Error, Eq, PartialEq)]
pub enum SourceMetadataError {
    #[error("metadata editing is unavailable")]
    Unavailable,
    #[error("metadata changed before it was saved")]
    Conflict,
    #[error("local access is required for {source_path}")]
    LocalAccessRequired { source_path: String },
    #[error("metadata was saved but its source refresh failed: {0}")]
    SavedRefreshFailed(String),
    #[error("metadata failed: {0}")]
    Write(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LyricsSearch {
    ServerOnly,
    ServerThenRemote,
    RemoteThenServer,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeLyricLine {
    pub text: String,
    pub start_millis: Option<u64>,
    pub end_millis: Option<u64>,
    pub cue_lines: Vec<NativeLyricCueLine>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeLyricCueLine {
    pub text: String,
    pub start_millis: Option<u64>,
    pub end_millis: Option<u64>,
    pub agent_id: Option<String>,
    pub cues: Vec<NativeLyricCue>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeLyricCue {
    pub text: String,
    pub start_millis: u64,
    pub end_millis: Option<u64>,
    pub byte_start: usize,
    pub byte_end_exclusive: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeLyricsRole {
    Original,
    Translation,
    Pronunciation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeLyricAgentRole {
    Main,
    Voice,
    Background,
    Group,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeLyricAgent {
    pub id: String,
    pub role: NativeLyricAgentRole,
    pub name: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeLyricsDocument {
    pub role: NativeLyricsRole,
    pub language: Option<String>,
    pub offset_millis: i64,
    pub lines: Vec<NativeLyricLine>,
    pub agents: Vec<NativeLyricAgent>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeLyrics {
    pub documents: Vec<NativeLyricsDocument>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImageBytes {
    pub bytes: Vec<u8>,
    pub content_type: Option<String>,
}
