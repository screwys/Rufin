use serde::{Deserialize, Serialize};

use super::layout::{
    LibraryField, LibraryListKey, MAX_RESTORED_WINDOW_HEIGHT, MAX_RESTORED_WINDOW_WIDTH,
    MIN_RESTORED_WINDOW_HEIGHT, MIN_RESTORED_WINDOW_WIDTH, default_true,
};

pub fn available_sort_fields(key: LibraryListKey) -> &'static [LibraryField] {
    match key {
        LibraryListKey::Albums | LibraryListKey::ArtistAlbums => &[
            LibraryField::Title,
            LibraryField::AlbumArtist,
            LibraryField::Year,
            LibraryField::ReleaseDate,
            LibraryField::DateAdded,
            LibraryField::LastPlayed,
            LibraryField::PlayCount,
            LibraryField::UserRating,
            LibraryField::SongCount,
            LibraryField::Duration,
            LibraryField::Favorite,
        ],
        LibraryListKey::Artists | LibraryListKey::AlbumArtists => &[
            LibraryField::Title,
            LibraryField::AlbumCount,
            LibraryField::SongCount,
            LibraryField::LastPlayed,
            LibraryField::PlayCount,
            LibraryField::UserRating,
            LibraryField::Favorite,
        ],
        LibraryListKey::Genres => &[
            LibraryField::Title,
            LibraryField::AlbumCount,
            LibraryField::SongCount,
        ],
        LibraryListKey::Moods => &[
            LibraryField::Title,
            LibraryField::SongCount,
            LibraryField::Duration,
        ],
        LibraryListKey::Playlists => &[
            LibraryField::RowIndex,
            LibraryField::Title,
            LibraryField::SongCount,
            LibraryField::Duration,
        ],
        LibraryListKey::SmartPlaylists => &[
            LibraryField::RowIndex,
            LibraryField::Title,
            LibraryField::SongCount,
            LibraryField::Duration,
        ],
        LibraryListKey::PlaylistTracks => &[
            LibraryField::RowIndex,
            LibraryField::Title,
            LibraryField::Artist,
            LibraryField::Album,
        ],
        LibraryListKey::History => &[LibraryField::LastPlayed],
        LibraryListKey::Tracks
        | LibraryListKey::FavoriteTracks
        | LibraryListKey::AlbumDetailTracks
        | LibraryListKey::ArtistTracks
        | LibraryListKey::GenreTracks
        | LibraryListKey::MoodTracks
        | LibraryListKey::SmartPlaylistTracks => &[
            LibraryField::TrackNumber,
            LibraryField::Title,
            LibraryField::Artist,
            LibraryField::AlbumArtist,
            LibraryField::Album,
            LibraryField::Year,
            LibraryField::ReleaseDate,
            LibraryField::DateAdded,
            LibraryField::LastPlayed,
            LibraryField::PlayCount,
            LibraryField::UserRating,
            LibraryField::Genre,
            LibraryField::Bpm,
            LibraryField::Duration,
            LibraryField::Favorite,
        ],
    }
}
pub(super) fn default_row_fields(key: LibraryListKey) -> Vec<LibraryField> {
    match key {
        LibraryListKey::Albums | LibraryListKey::ArtistAlbums => vec![
            LibraryField::TitleMerged,
            LibraryField::PlayCount,
            LibraryField::Year,
            LibraryField::Favorite,
        ],
        LibraryListKey::Artists | LibraryListKey::AlbumArtists => vec![
            LibraryField::Image,
            LibraryField::Title,
            LibraryField::AlbumCount,
            LibraryField::Favorite,
        ],
        LibraryListKey::Genres => vec![
            LibraryField::Title,
            LibraryField::AlbumCount,
            LibraryField::SongCount,
        ],
        LibraryListKey::Moods => vec![
            LibraryField::Title,
            LibraryField::SongCount,
            LibraryField::Duration,
        ],
        LibraryListKey::Playlists => vec![
            LibraryField::Image,
            LibraryField::Title,
            LibraryField::SongCount,
        ],
        LibraryListKey::SmartPlaylists => vec![
            LibraryField::Image,
            LibraryField::Title,
            LibraryField::SongCount,
            LibraryField::Duration,
        ],
        LibraryListKey::Tracks => vec![
            LibraryField::RowIndex,
            LibraryField::TitleMerged,
            LibraryField::Album,
            LibraryField::Year,
            LibraryField::Favorite,
        ],
        LibraryListKey::FavoriteTracks => vec![
            LibraryField::RowIndex,
            LibraryField::TitleMerged,
            LibraryField::Album,
            LibraryField::Year,
            LibraryField::PlayCount,
        ],
        LibraryListKey::History => vec![
            LibraryField::RowIndex,
            LibraryField::TitleMerged,
            LibraryField::Album,
            LibraryField::LastPlayed,
            LibraryField::Favorite,
        ],
        LibraryListKey::AlbumDetailTracks => default_detail_track_fields(),
        LibraryListKey::ArtistTracks => vec![
            LibraryField::RowIndex,
            LibraryField::TitleMerged,
            LibraryField::Album,
            LibraryField::Year,
            LibraryField::PlayCount,
        ],
        LibraryListKey::GenreTracks
        | LibraryListKey::MoodTracks
        | LibraryListKey::PlaylistTracks => {
            vec![
                LibraryField::RowIndex,
                LibraryField::TitleMerged,
                LibraryField::Album,
                LibraryField::Duration,
                LibraryField::Favorite,
            ]
        }
        LibraryListKey::SmartPlaylistTracks => vec![
            LibraryField::RowIndex,
            LibraryField::TitleMerged,
            LibraryField::Album,
            LibraryField::PlayCount,
        ],
    }
}
pub(super) fn default_grid_fields(key: LibraryListKey) -> Vec<LibraryField> {
    match key {
        LibraryListKey::Albums | LibraryListKey::ArtistAlbums => {
            vec![LibraryField::AlbumArtist, LibraryField::Year]
        }
        LibraryListKey::Artists | LibraryListKey::AlbumArtists => Vec::new(),
        LibraryListKey::Genres => Vec::new(),
        LibraryListKey::Moods => vec![LibraryField::SongCount, LibraryField::Duration],
        LibraryListKey::Playlists => vec![LibraryField::SongCount],
        LibraryListKey::SmartPlaylists => vec![LibraryField::SongCount, LibraryField::Duration],
        LibraryListKey::History => vec![
            LibraryField::Artist,
            LibraryField::Album,
            LibraryField::LastPlayed,
        ],
        LibraryListKey::Tracks
        | LibraryListKey::FavoriteTracks
        | LibraryListKey::AlbumDetailTracks
        | LibraryListKey::ArtistTracks
        | LibraryListKey::GenreTracks
        | LibraryListKey::MoodTracks
        | LibraryListKey::PlaylistTracks
        | LibraryListKey::SmartPlaylistTracks => {
            vec![
                LibraryField::Artist,
                LibraryField::Album,
                LibraryField::Duration,
            ]
        }
    }
}
pub fn available_detail_track_fields() -> &'static [LibraryField] {
    &[
        LibraryField::RowIndex,
        LibraryField::TrackNumber,
        LibraryField::Title,
        LibraryField::Duration,
    ]
}
pub(super) fn default_detail_track_fields() -> Vec<LibraryField> {
    vec![
        LibraryField::RowIndex,
        LibraryField::Title,
        LibraryField::Duration,
    ]
}
pub(super) fn default_sort_key(key: LibraryListKey) -> LibraryField {
    match key {
        LibraryListKey::Albums
        | LibraryListKey::Artists
        | LibraryListKey::AlbumArtists
        | LibraryListKey::Genres
        | LibraryListKey::Moods
        | LibraryListKey::ArtistAlbums
        | LibraryListKey::Tracks
        | LibraryListKey::FavoriteTracks => LibraryField::Title,
        LibraryListKey::History => LibraryField::LastPlayed,
        LibraryListKey::Playlists
        | LibraryListKey::SmartPlaylists
        | LibraryListKey::PlaylistTracks => LibraryField::RowIndex,
        LibraryListKey::AlbumDetailTracks
        | LibraryListKey::ArtistTracks
        | LibraryListKey::GenreTracks
        | LibraryListKey::MoodTracks
        | LibraryListKey::SmartPlaylistTracks => LibraryField::TrackNumber,
    }
}
pub(super) fn default_descending(key: LibraryListKey) -> bool {
    key == LibraryListKey::History
}
pub(super) fn sanitize_optional_fields(fields: &mut Vec<LibraryField>, available: &[LibraryField]) {
    let mut seen = Vec::new();
    fields.retain(|field| {
        if !available.contains(field) || seen.contains(field) {
            return false;
        }
        seen.push(*field);
        true
    });
}
pub(super) fn sanitize_required_fields(
    fields: &mut Vec<LibraryField>,
    available: &[LibraryField],
    fallback: Vec<LibraryField>,
) {
    sanitize_optional_fields(fields, available);
    if fields.is_empty() {
        *fields = fallback;
    }
}
pub(super) fn ensure_usable_row_field(fields: &mut Vec<LibraryField>, fallback: Vec<LibraryField>) {
    if fields.iter().any(|field| row_field_is_usable(*field)) {
        return;
    }
    if let Some(field) = fallback
        .into_iter()
        .find(|field| row_field_is_usable(*field))
    {
        fields.push(field);
    }
}
fn row_field_is_usable(field: LibraryField) -> bool {
    !matches!(
        field,
        LibraryField::RowIndex
            | LibraryField::Image
            | LibraryField::TrackNumber
            | LibraryField::DiscNumber
            | LibraryField::Favorite
    )
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExternalSiteLinkSettings {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub lastfm: bool,
    #[serde(default = "default_true")]
    pub musicbrainz: bool,
    #[serde(default = "default_true")]
    pub server: bool,
}

impl Default for ExternalSiteLinkSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            lastfm: true,
            musicbrainz: true,
            server: true,
        }
    }
}
pub fn sanitized_window_size(width: Option<i32>, height: Option<i32>) -> Option<(i32, i32)> {
    let (width, height) = (width?, height?);
    if width < MIN_RESTORED_WINDOW_WIDTH || height < MIN_RESTORED_WINDOW_HEIGHT {
        return None;
    }
    Some((
        width.clamp(MIN_RESTORED_WINDOW_WIDTH, MAX_RESTORED_WINDOW_WIDTH),
        height.clamp(MIN_RESTORED_WINDOW_HEIGHT, MAX_RESTORED_WINDOW_HEIGHT),
    ))
}
