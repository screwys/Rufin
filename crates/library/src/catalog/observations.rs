use crate::{
    AlbumMetadata, ArtistMetadata, FolderMetadata, NativeUserState, TagKind, TagMetadata,
    TrackMetadata,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrackObservation {
    pub object_id: String,
    pub media_uri: Option<String>,
    pub album_object_id: Option<String>,
    pub metadata: TrackMetadata,
    pub source_path: Option<String>,
    pub cue_path: Option<String>,
    pub cue_start_millis: Option<i64>,
    pub cue_end_millis: Option<i64>,
    pub first_seen_at: Option<i64>,
    /// When the audio may have changed: the file or server revision plus the CUE window.
    pub audio_revision: Option<[u8; 32]>,
    /// A hash of the audio stream, when the source knows it without decoding.
    pub audio_hash: Option<[u8; 32]>,
    pub source_lyrics: Option<String>,
    pub artwork_binding: Option<Vec<u8>>,
    pub user_data: NativeUserState,
    pub folders: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AlbumObservation {
    pub object_id: String,
    pub metadata: AlbumMetadata,
    pub artwork_binding: Option<Vec<u8>>,
    pub user_data: NativeUserState,
    pub first_seen_at: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ArtistObservation {
    pub object_id: String,
    pub metadata: ArtistMetadata,
    pub artwork_binding: Option<Vec<u8>>,
    pub user_data: NativeUserState,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TagObservation {
    pub kind: TagKind,
    pub object_id: String,
    pub metadata: TagMetadata,
    pub artwork_binding: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FolderObservation {
    pub object_id: String,
    pub metadata: FolderMetadata,
    pub artwork_binding: Option<Vec<u8>>,
    pub tracks: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlaylistObservation {
    pub object_id: String,
    pub name: String,
    pub sort_text: String,
    pub artwork_binding: Option<Vec<u8>>,
    pub writable: Option<bool>,
    pub metadata_writable: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaylistEntryObservation {
    pub object_id: String,
    pub track_id: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CatalogBatch {
    pub reference: bool,
    pub tracks: Vec<TrackObservation>,
    pub albums: Vec<AlbumObservation>,
    pub artists: Vec<ArtistObservation>,
    pub tags: Vec<TagObservation>,
    pub folders: Vec<FolderObservation>,
    pub playlists: Vec<PlaylistObservation>,
}
