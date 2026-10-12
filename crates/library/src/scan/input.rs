use crate::AudioProperties;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrackInput {
    pub object_id: String,
    pub album_object_id: Option<String>,
    pub title: String,
    pub display_album: String,
    pub display_artist: String,
    pub sort_text: String,
    pub duration_millis: i64,
    pub disc_number: i64,
    pub track_number: i64,
    pub year: Option<i64>,
    pub release_date: Option<String>,
    pub date_added: Option<String>,
    pub media_uri: Option<String>,
    pub source_format: Option<String>,
    pub comment: Option<String>,
    pub bpm: Option<i64>,
    pub musicbrainz_recording_id: Option<String>,
    pub musicbrainz_release_track_id: Option<String>,
    pub cue_path: Option<String>,
    pub cue_start_millis: Option<i64>,
    pub cue_end_millis: Option<i64>,
    pub artwork_binding: Option<Vec<u8>>,
    pub favorite: Option<bool>,
    pub rating: Option<i64>,
    pub first_seen_at: Option<i64>,
    pub source_play_count: Option<i64>,
    pub source_skip_count: Option<i64>,
    pub source_last_played: Option<i64>,
    pub source_path: Option<String>,
    pub loudness_analysis_key: [u8; 32],
    pub audio_properties: AudioProperties,
    pub relations: Option<TrackRelationsInput>,
    pub source_loudness: Option<LoudnessInput>,
    pub album_loudness: Option<LoudnessInput>,
    pub folders: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AlbumInput {
    pub object_id: String,
    pub title: String,
    pub display_artist: String,
    pub sort_text: String,
    pub year: Option<i64>,
    pub release_date: Option<String>,
    pub date_added: Option<String>,
    pub musicbrainz_release_id: Option<String>,
    pub musicbrainz_release_group_id: Option<String>,
    pub is_compilation: Option<bool>,
    pub artwork_binding: Option<Vec<u8>>,
    pub favorite: Option<bool>,
    pub rating: Option<i64>,
    pub first_seen_at: Option<i64>,
    pub relations: Option<AlbumRelationsInput>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ArtistInput {
    pub object_id: String,
    pub name: String,
    pub sort_text: Option<String>,
    pub musicbrainz_artist_id: Option<String>,
    pub artwork_binding: Option<Vec<u8>>,
    pub favorite: Option<bool>,
    pub rating: Option<i64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GenreInput {
    pub object_id: String,
    pub name: String,
    pub sort_text: Option<String>,
    pub artwork_binding: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TrackRelationsInput {
    pub artists: Vec<ArtistInput>,
    pub album_artists: Vec<ArtistInput>,
    pub genres: Vec<GenreInput>,
    pub moods: Vec<MoodInput>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MoodInput {
    pub object_id: String,
    pub name: String,
    pub sort_text: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FolderInput {
    pub object_id: String,
    pub name: Option<String>,
    pub sort_text: Option<String>,
    pub artwork_binding: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PlaylistInput {
    pub object_id: String,
    pub name: String,
    pub sort_text: String,
    pub artwork_binding: Option<Vec<u8>>,
    pub writable: Option<bool>,
    pub metadata_writable: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RecordInput {
    Track(Box<TrackInput>),
    Album(AlbumInput),
    Artist(ArtistInput),
    Genre(GenreInput),
    Folder(FolderInput),
    Playlist(PlaylistInput),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AlbumRelationsInput {
    pub artists: Vec<ArtistInput>,
    pub album_artists: Vec<ArtistInput>,
    pub genres: Vec<GenreInput>,
    pub release_types: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LoudnessInput {
    pub integrated_lufs: Option<f64>,
    pub true_peak: Option<f64>,
    pub replay_gain_db: Option<f64>,
    pub replay_gain_peak: Option<f64>,
}
