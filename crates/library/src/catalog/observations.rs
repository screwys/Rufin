use crate::{
    AlbumMetadata, ArtistMetadata, FolderMetadata, NativeUserState, TagMetadata, TrackMetadata,
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
    pub loudness_analysis_key: [u8; 32],
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

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GenreObservation {
    pub object_id: String,
    pub metadata: TagMetadata,
    pub artwork_binding: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MoodObservation {
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
    pub genres: Vec<GenreObservation>,
    pub moods: Vec<MoodObservation>,
    pub folders: Vec<FolderObservation>,
    pub playlists: Vec<PlaylistObservation>,
}
