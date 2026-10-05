use super::*;

#[derive(uniffi::Record)]
pub struct AndroidPlaylistImport {
    pub row: AndroidBrowseRow,
    pub skipped: u64,
}

#[derive(uniffi::Record)]
pub struct AndroidRatingState {
    pub value: Option<u8>,
    pub half_stars: bool,
    pub visible: bool,
}

#[derive(uniffi::Record)]
pub struct AndroidRouteDescriptor {
    pub id: String,
    pub title: String,
    pub icon_name: String,
    pub selected_icon_name: String,
    pub route: String,
}

#[derive(uniffi::Record)]
pub struct AndroidRouteSetting {
    pub descriptor: AndroidRouteDescriptor,
    pub visible: bool,
}

#[derive(uniffi::Record)]
pub struct AndroidSortField {
    pub id: String,
    pub title: String,
}

#[derive(Clone, uniffi::Record)]
pub struct AndroidFieldValue {
    pub id: String,
    pub text: String,
}

#[derive(uniffi::Record)]
pub struct AndroidSortSelection {
    pub id: String,
    pub title: String,
    pub descending: bool,
}

#[derive(uniffi::Record)]
pub struct AndroidBrowseDisplay {
    pub layout: String,
    pub show_header: bool,
    pub layouts: Vec<AndroidSortField>,
    pub size: String,
    pub grid_spacing: String,
    pub sizes: Vec<AndroidSortField>,
    pub grid_spacings: Vec<AndroidSortField>,
}

#[derive(uniffi::Record)]
pub struct AndroidDetailLink {
    pub title: String,
    pub url: Option<String>,
    pub icon_id: String,
}

#[derive(uniffi::Record)]
pub struct AndroidArtistLink {
    pub start: u32,
    pub end: u32,
    pub title: String,
    pub route: String,
    pub artwork_identity: Option<Vec<u8>>,
}

impl AndroidArtistLink {
    pub(crate) fn from_detail_links(
        links: &rufin_core::route::detail_links::DetailLinks,
    ) -> Result<Vec<Self>, AndroidError> {
        let display = links.display_text();
        links
            .spans()
            .iter()
            .map(|span| {
                let prefix = display
                    .get(..span.range.start)
                    .expect("Detail link text boundary");
                let text = display
                    .get(span.range.clone())
                    .expect("Detail link text boundaries");
                let start = u32::try_from(prefix.encode_utf16().count()).map_err(error)?;
                let end = start + u32::try_from(text.encode_utf16().count()).map_err(error)?;
                Ok(Self {
                    start,
                    end,
                    title: text.to_string(),
                    route: route_json(span.route.clone()),
                    artwork_identity: None,
                })
            })
            .collect()
    }
}

#[derive(uniffi::Record)]
pub struct AndroidDetailSummary {
    pub row: AndroidBrowseRow,
    pub artwork_bindings: Vec<Vec<u8>>,
    pub artist_text: String,
    pub artist_links: Vec<AndroidArtistLink>,
    pub label: String,
    pub mode: String,
    pub album_count: u64,
    pub links: Vec<AndroidDetailLink>,
    pub favorite_route: Option<String>,
    pub favorite_total: u64,
    pub discography_route: Option<String>,
    pub tracks_route: Option<String>,
    pub show_track_images: bool,
    pub show_track_header: bool,
}

#[derive(uniffi::Record)]
pub struct AndroidArtistReleaseGroup {
    pub id: String,
    pub title: String,
    pub total: u64,
    pub index: u64,
    pub route: String,
}

#[derive(uniffi::Record)]
pub struct AndroidMusicFolder {
    pub id: Option<String>,
    pub title: String,
}

#[derive(uniffi::Record)]
pub struct AndroidMusicFolderState {
    pub selected_id: Option<String>,
    pub choices: Vec<AndroidMusicFolder>,
}

#[derive(uniffi::Record)]
pub struct AndroidBreadcrumb {
    pub title: String,
    pub route: String,
}

#[derive(uniffi::Record)]
pub struct AndroidScrollSection {
    pub title: String,
    pub index: u64,
}

#[derive(uniffi::Record)]
pub struct AndroidCollectionCategory {
    pub id: String,
    pub title: String,
    pub icon_name: String,
}

#[derive(uniffi::Record)]
pub struct AndroidPinPage {
    pub rows: Vec<AndroidBrowseRow>,
    pub next_offset: Option<u64>,
    pub total: u64,
}

#[derive(Clone, uniffi::Record)]
pub struct AndroidBrowseRow {
    pub kind: String,
    pub key: String,
    pub media_uri: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub fields: Vec<AndroidFieldValue>,
    pub subtitle: String,
    pub favorite: bool,
    pub duration_millis: u64,
    pub detail_route: Option<String>,
    pub playback_context_id: Option<String>,
    pub pin: Option<String>,
    pub section: String,
    pub section_id: String,
    pub section_kind: String,
    pub section_refreshable: bool,
    pub year: Option<i64>,
    pub track_count: u64,
    pub artwork_identity: Option<Vec<u8>>,
    pub downloaded: bool,
    pub source_id: Option<String>,
    pub source_name: String,
    pub last_played: Option<i64>,
    pub writable: bool,
}

impl From<library::SmartPlaylistRow> for AndroidBrowseRow {
    fn from(row: library::SmartPlaylistRow) -> Self {
        let title = rufin_core::playlists::smart_playlist_display_name(&row);
        let pin = SidebarPin::SmartPlaylist {
            playlist_id: row.object_id.clone(),
        };
        Self {
            kind: "smart_playlist".into(),
            playback_context_id: Some(
                rufin_core::playback::PlaybackTarget::SmartPlaylist(row.smart_playlist_key)
                    .context_id(),
            ),
            fields: Vec::new(),
            artist: String::new(),
            album: String::new(),
            key: row.object_id,
            media_uri: String::new(),
            title,
            subtitle: String::new(),
            favorite: false,
            duration_millis: row.duration_millis.max(0) as u64,
            detail_route: Some(route_json(Route::SmartPlaylistDetail(
                row.smart_playlist_key,
            ))),
            pin: Some(serde_json::to_string(&pin).expect("Pin serialization")),
            section: String::new(),
            section_id: String::new(),
            section_kind: String::new(),
            section_refreshable: false,
            year: None,
            track_count: row.track_count.max(0) as u64,
            source_id: None,
            source_name: String::new(),
            last_played: None,
            writable: false,
            downloaded: row.track_count > 0 && row.downloaded_count == row.track_count,
            artwork_identity: row
                .artwork_binding
                .or_else(|| row.artwork_bindings.into_iter().next()),
        }
    }
}

impl From<library::PlaylistRow> for AndroidBrowseRow {
    fn from(row: library::PlaylistRow) -> Self {
        let pin = SidebarPin::Playlist {
            source_id: row.source_id.clone().map(sources::SourceId::new),
            playlist_id: row.object_id.clone(),
        };
        Self {
            kind: "playlist".into(),
            playback_context_id: Some(
                rufin_core::playback::PlaybackTarget::Playlist(row.playlist_key).context_id(),
            ),
            fields: Vec::new(),
            artist: String::new(),
            album: String::new(),
            key: row.object_id,
            media_uri: String::new(),
            title: row.name,
            subtitle: String::new(),
            favorite: false,
            duration_millis: row.duration_millis.max(0) as u64,
            detail_route: Some(route_json(Route::PlaylistDetail(row.playlist_key))),
            pin: Some(serde_json::to_string(&pin).expect("Pin serialization")),
            section: String::new(),
            section_id: String::new(),
            section_kind: String::new(),
            section_refreshable: false,
            year: None,
            track_count: row.track_count.max(0) as u64,
            source_id: row.source_id,
            source_name: String::new(),
            last_played: None,
            writable: row.writable,
            downloaded: row.track_count > 0 && row.downloaded_count == row.track_count,
            artwork_identity: row
                .artwork_binding
                .or_else(|| row.representative_artwork.into_iter().next()),
        }
    }
}

impl From<library::GenreRow> for AndroidBrowseRow {
    fn from(row: library::GenreRow) -> Self {
        let pin = SidebarPin::Genre {
            source_id: sources::SourceId::new(row.source_id.clone()),
            genre_id: row.object_id.clone(),
        };
        Self {
            kind: "genre".into(),
            playback_context_id: Some(
                rufin_core::playback::PlaybackTarget::Genre(row.genre_key).context_id(),
            ),
            fields: Vec::new(),
            artist: String::new(),
            album: String::new(),
            key: row.object_id,
            media_uri: String::new(),
            title: row.name,
            subtitle: String::new(),
            favorite: false,
            duration_millis: row.duration_millis.max(0) as u64,
            detail_route: Some(route_json(Route::GenreDetail(row.genre_key))),
            pin: Some(serde_json::to_string(&pin).expect("Pin serialization")),
            section: String::new(),
            section_id: String::new(),
            section_kind: String::new(),
            section_refreshable: false,
            year: None,
            track_count: row.track_count.max(0) as u64,
            source_id: Some(row.source_id),
            source_name: String::new(),
            last_played: None,
            writable: false,
            downloaded: row.track_count > 0 && row.downloaded_count == row.track_count,
            artwork_identity: row
                .artwork_binding
                .or_else(|| row.representative_artwork.into_iter().next()),
        }
    }
}

impl From<library::MoodRow> for AndroidBrowseRow {
    fn from(row: library::MoodRow) -> Self {
        Self {
            kind: "mood".into(),
            playback_context_id: Some(
                rufin_core::playback::PlaybackTarget::Mood(row.mood_key).context_id(),
            ),
            key: row.mood_key.to_string(),
            media_uri: String::new(),
            title: row.name,
            artist: String::new(),
            album: String::new(),
            fields: Vec::new(),
            subtitle: String::new(),
            favorite: false,
            duration_millis: row.duration_millis.max(0) as u64,
            detail_route: Some(route_json(Route::MoodDetail(row.mood_key))),
            pin: None,
            section: String::new(),
            section_id: String::new(),
            section_kind: String::new(),
            section_refreshable: false,
            year: None,
            track_count: row.track_count.max(0) as u64,
            source_id: None,
            source_name: String::new(),
            last_played: None,
            writable: false,
            downloaded: row.track_count > 0 && row.downloaded_count == row.track_count,
            artwork_identity: row.representative_artwork.into_iter().next(),
        }
    }
}

impl From<library::AlbumRow> for AndroidBrowseRow {
    fn from(row: library::AlbumRow) -> Self {
        let pin = library::source_entity_parts(&row.media_uri).map(|(source_id, _, album_id)| {
            SidebarPin::Album {
                source_id,
                album_id,
            }
        });
        Self {
            kind: "album".into(),
            playback_context_id: Some(
                rufin_core::playback::PlaybackTarget::Album(row.media_uri.clone()).context_id(),
            ),
            fields: Vec::new(),
            artist: row.display_artist.clone(),
            album: row.title.clone(),
            key: row.object_id,
            detail_route: Some(route_json(Route::AlbumDetail(row.media_uri.clone()))),
            media_uri: row.media_uri,
            title: row.title,
            subtitle: row.display_artist,
            favorite: row.favorite,
            duration_millis: row.duration_millis.max(0) as u64,
            pin: pin.map(|pin| serde_json::to_string(&pin).expect("Pin serialization")),
            section: String::new(),
            section_id: String::new(),
            section_kind: String::new(),
            section_refreshable: false,
            year: row.year,
            track_count: row.track_count.max(0) as u64,
            source_id: None,
            source_name: String::new(),
            last_played: None,
            writable: false,
            downloaded: row.track_count > 0 && row.downloaded_count == row.track_count,
            artwork_identity: row.artwork_binding,
        }
    }
}

impl From<library::HomeAlbumRow> for AndroidBrowseRow {
    fn from(row: library::HomeAlbumRow) -> Self {
        let mut result: Self = row.album.into();
        result.title = row.title;
        result
    }
}

impl From<library::HomeTrackRow> for AndroidBrowseRow {
    fn from(row: library::HomeTrackRow) -> Self {
        let subtitle = format!("{}\n{}", row.track.artist, row.track.album);
        let mut result: Self = row.track.into();
        result.title = row.title;
        result.subtitle = subtitle;
        result
    }
}

impl From<library::TrackRow> for AndroidBrowseRow {
    fn from(row: library::TrackRow) -> Self {
        Self {
            kind: "track".into(),
            playback_context_id: None,
            fields: Vec::new(),
            artist: row.artist.clone(),
            album: row.album.clone(),
            key: row.media_uri.clone(),
            media_uri: row.media_uri,
            title: row.title,
            subtitle: format!("{} · {}", row.artist, row.album),
            favorite: row.favorite,
            duration_millis: row.duration_millis.max(0) as u64,
            detail_route: None,
            pin: None,
            section: String::new(),
            section_id: String::new(),
            section_kind: String::new(),
            section_refreshable: false,
            year: row.year,
            track_count: 1,
            source_id: Some(row.source_id),
            source_name: row.source_name,
            last_played: row.last_played,
            writable: false,
            downloaded: row.is_downloaded,
            artwork_identity: row.artwork_binding,
        }
    }
}

impl From<rufin_core::detail_links::DetailLink> for AndroidDetailLink {
    fn from(link: rufin_core::detail_links::DetailLink) -> Self {
        Self {
            title: localization::tr(link.title),
            url: link.url,
            icon_id: link.icon_name.into(),
        }
    }
}
