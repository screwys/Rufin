use super::*;

#[derive(uniffi::Record)]
pub struct AndroidPlaylistImport {
    pub row: AndroidBrowseRow,
    pub skipped: u64,
}

#[derive(uniffi::Record)]
pub struct AndroidRouteDescriptor {
    pub item: SidebarRouteItem,
    pub id: String,
    pub title: String,
    pub icon_name: String,
    pub selected_icon_name: String,
    pub route: Route,
}

#[derive(uniffi::Record)]
pub struct AndroidRouteSetting {
    pub descriptor: AndroidRouteDescriptor,
    pub visible: bool,
}

#[derive(uniffi::Record)]
pub struct AndroidSortField {
    pub id: LibraryField,
    pub title: String,
}

#[derive(Clone, uniffi::Record)]
pub struct AndroidFieldValue {
    pub id: LibraryField,
    pub text: String,
}

#[derive(uniffi::Record)]
pub struct AndroidBrowseDisplay {
    pub settings: LibraryListSettings,
    pub layouts: Vec<LibraryLayout>,
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
    pub route: Route,
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
                    route: span.route.clone(),
                    artwork_identity: None,
                })
            })
            .collect()
    }
}

#[derive(uniffi::Record)]
pub struct AndroidBrowsePage {
    pub count: u64,
    pub rows: Vec<AndroidBrowseRow>,
    pub summary: Option<AndroidDetailSummary>,
    pub groups: Vec<AndroidArtistReleaseGroup>,
    pub favorite_rows: Vec<AndroidBrowseRow>,
}

#[derive(uniffi::Record)]
pub struct AndroidDetailSummary {
    pub row: AndroidBrowseRow,
    pub artwork_bindings: Vec<Vec<u8>>,
    pub artist_text: String,
    pub artist_links: Vec<AndroidArtistLink>,
    pub label: String,
    pub mode: Route,
    pub album_count: u64,
    pub links: Vec<AndroidDetailLink>,
    pub favorite_route: Option<Route>,
    pub favorite_total: u64,
    pub discography_route: Option<Route>,
    pub tracks_route: Option<Route>,
    pub show_track_images: bool,
    pub show_track_header: bool,
}

#[derive(uniffi::Record)]
pub struct AndroidArtistReleaseGroup {
    pub first_rows: Vec<AndroidBrowseRow>,
    pub id: String,
    pub title: String,
    pub total: u64,
    pub index: u64,
    pub route: Route,
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
    pub route: Route,
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

#[derive(Clone, uniffi::Enum)]
pub enum AndroidBrowseItem {
    Track {
        row: library::TrackView,
    },
    PlaylistEntry {
        row: library::PlaylistEntryView,
    },
    Album {
        row: library::AlbumView,
    },
    Artist {
        row: library::ArtistView,
        album_artist: bool,
    },
    Genre {
        row: library::GenreView,
    },
    Mood {
        row: library::MoodView,
    },
    Playlist {
        row: library::PlaylistRow,
    },
    SmartPlaylist {
        row: library::SmartPlaylistRow,
    },
    Recent {
        row: rufin_core::settings::app::RecentSearchResult,
    },
    Folder {
        key: String,
        title: String,
        route: Route,
        artwork: Option<Vec<u8>>,
    },
}

#[derive(Clone, uniffi::Record)]
pub struct AndroidBrowseRow {
    pub item: AndroidBrowseItem,
    pub fields: Vec<AndroidFieldValue>,
    pub display_title: Option<String>,
    pub album_header: bool,
    pub pin: Option<SidebarPin>,
    pub section: String,
    pub section_id: String,
    pub section_kind: String,
    pub section_refreshable: bool,
    pub context_source_id: Option<String>,
    pub context_source_name: String,
}

impl AndroidBrowseRow {
    pub(crate) fn new(item: AndroidBrowseItem) -> Self {
        Self {
            item,
            fields: Vec::new(),
            display_title: None,
            album_header: false,
            pin: None,
            section: String::new(),
            section_id: String::new(),
            section_kind: String::new(),
            section_refreshable: false,
            context_source_id: None,
            context_source_name: String::new(),
        }
    }

    pub(super) fn track_key(&self) -> Option<library::TrackKey> {
        match &self.item {
            AndroidBrowseItem::Track { row } => Some(row.track_key),
            AndroidBrowseItem::PlaylistEntry { row } => Some(row.track.track_key),
            _ => None,
        }
    }

    pub(super) fn matches_filter(&self, filter: &str) -> bool {
        let (title, subtitle) = match &self.item {
            AndroidBrowseItem::Track { row } => (
                &row.metadata.title,
                format!("{} · {}", row.metadata.artist, row.metadata.album),
            ),
            AndroidBrowseItem::Album { row } => (&row.metadata.title, row.metadata.artist.clone()),
            _ => return true,
        };
        format!(
            "{} {}",
            self.display_title.as_ref().unwrap_or(title),
            subtitle
        )
        .to_lowercase()
        .contains(filter)
    }
}

impl From<library::TrackView> for AndroidBrowseRow {
    fn from(row: library::TrackView) -> Self {
        Self::new(AndroidBrowseItem::Track { row })
    }
}
impl From<library::AlbumView> for AndroidBrowseRow {
    fn from(row: library::AlbumView) -> Self {
        let pin = library::source_entity_parts(&row.media_uri).map(|(source_id, _, album_id)| {
            SidebarPin::Album {
                source_id,
                album_id,
            }
        });
        Self {
            pin,
            ..Self::new(AndroidBrowseItem::Album { row })
        }
    }
}
impl From<library::GenreView> for AndroidBrowseRow {
    fn from(row: library::GenreView) -> Self {
        let pin = Some(SidebarPin::Genre {
            source_id: sources::SourceId::new(row.source_id.clone()),
            genre_id: row.object_id.clone(),
        });
        Self {
            context_source_id: Some(row.source_id.clone()),
            pin,
            ..Self::new(AndroidBrowseItem::Genre { row })
        }
    }
}
impl From<library::MoodView> for AndroidBrowseRow {
    fn from(row: library::MoodView) -> Self {
        Self::new(AndroidBrowseItem::Mood { row })
    }
}
impl From<library::PlaylistRow> for AndroidBrowseRow {
    fn from(row: library::PlaylistRow) -> Self {
        let pin = Some(SidebarPin::Playlist {
            source_id: row.source_id.as_ref().map(sources::SourceId::new),
            playlist_id: row.object_id.clone(),
        });
        Self {
            context_source_id: row.source_id.clone(),
            pin,
            ..Self::new(AndroidBrowseItem::Playlist { row })
        }
    }
}
impl From<library::SmartPlaylistRow> for AndroidBrowseRow {
    fn from(row: library::SmartPlaylistRow) -> Self {
        let title = Some(rufin_core::playlists::smart_playlist_display_name(&row));
        let pin = Some(SidebarPin::SmartPlaylist {
            playlist_id: row.object_id.clone(),
        });
        Self {
            display_title: title,
            pin,
            ..Self::new(AndroidBrowseItem::SmartPlaylist { row })
        }
    }
}
impl From<library::HomeAlbumRow> for AndroidBrowseRow {
    fn from(row: library::HomeAlbumRow) -> Self {
        Self {
            display_title: Some(row.title),
            ..row.album.into()
        }
    }
}
impl From<library::HomeTrackRow> for AndroidBrowseRow {
    fn from(row: library::HomeTrackRow) -> Self {
        Self {
            display_title: Some(row.title),
            ..row.track.into()
        }
    }
}

impl From<rufin_core::detail_links::DetailLink> for AndroidDetailLink {
    fn from(link: rufin_core::detail_links::DetailLink) -> Self {
        Self {
            title: link.title.to_string(),
            url: link.url,
            icon_id: link.icon_name.into(),
        }
    }
}

#[derive(uniffi::Record)]
pub struct AndroidTargetDownloadStatus {
    pub total: u64,
    pub downloaded: u64,
}
