use std::sync::Arc;
mod commands;
mod details;
mod folders;
mod projection;
mod queries;
mod records;
mod search;
pub use records::{
    AndroidArtistLink, AndroidArtistReleaseGroup, AndroidBreadcrumb, AndroidBrowseDisplay,
    AndroidBrowseItem, AndroidBrowsePage, AndroidBrowseRow, AndroidCollectionCategory,
    AndroidDetailSummary, AndroidFieldValue, AndroidMusicFolder, AndroidMusicFolderState,
    AndroidPinPage, AndroidPlaylistImport, AndroidRouteDescriptor, AndroidRouteSetting,
    AndroidScrollSection, AndroidSortField,
};

use library::{CatalogQuery, CatalogRows, ReadCancellation, TrackScope, TrackSelection};
use rufin_core::browse::{BrowseQuery, BrowseRequest, library_list_key};
use rufin_core::source::folders::FolderProjection;
use rufin_core::{
    route::{CollectionCategory, Route, sidebar_route_descriptor},
    runtime::SelectedLibrary,
    settings::{
        SidebarPin, SidebarRouteItem,
        layout::{DisplaySize, LibraryField, LibraryLayout, LibraryListKey, LibraryListSettings},
    },
};

use crate::{
    host::{AndroidError, error},
    library::AndroidLibrary,
};

const ARTIST_RELEASE_GROUPS: [(&str, &str); 6] = [
    ("Album", "Albums"),
    ("Ep", "EPs"),
    ("Single", "Singles"),
    ("Collection", "Collections"),
    ("Other", "Other releases"),
    ("AppearsOn", "Appears On"),
];

#[derive(Clone, Copy)]
enum ArtistReleaseSection {
    Class(library::AlbumReleaseClass),
    AppearsOn,
}

fn release_section(id: &str) -> Result<ArtistReleaseSection, AndroidError> {
    use library::AlbumReleaseClass;
    Ok(ArtistReleaseSection::Class(match id {
        "Album" => AlbumReleaseClass::Album,
        "Ep" => AlbumReleaseClass::Ep,
        "Single" => AlbumReleaseClass::Single,
        "Collection" => AlbumReleaseClass::Collection,
        "Other" => AlbumReleaseClass::Other,
        "AppearsOn" => return Ok(ArtistReleaseSection::AppearsOn),
        _ => return Err(error("Unknown release section")),
    }))
}

fn release_index(class: library::AlbumReleaseClass) -> usize {
    use library::AlbumReleaseClass;
    match class {
        AlbumReleaseClass::Album => 0,
        AlbumReleaseClass::Ep => 1,
        AlbumReleaseClass::Single => 2,
        AlbumReleaseClass::Collection => 3,
        AlbumReleaseClass::Other => 4,
    }
}

fn browse_display(key: LibraryListKey, settings: &LibraryListSettings) -> AndroidBrowseDisplay {
    AndroidBrowseDisplay {
        settings: settings.clone(),
        layouts: [
            LibraryLayout::Row,
            LibraryLayout::Grid,
            LibraryLayout::Detail,
        ]
        .into_iter()
        .filter(|layout| key.supports_layout(*layout))
        .collect(),
    }
}

fn queue_placement(value: &str) -> Result<rufin_core::playback::QueuePlacement, AndroidError> {
    match value {
        "now" => Ok(rufin_core::playback::QueuePlacement::Now),
        "next" => Ok(rufin_core::playback::QueuePlacement::Next),
        "append" => Ok(rufin_core::playback::QueuePlacement::Last),
        _ => Err(error("Unknown queue placement")),
    }
}

fn context_caption(
    route: &Route,
    title: Option<String>,
) -> Option<Arc<library::QueueContextTitle>> {
    let kind = match route {
        Route::AlbumDetail(_) => Some("Album"),
        Route::ArtistDetail(_)
        | Route::ArtistDiscography(_)
        | Route::ArtistTracks(_)
        | Route::ArtistFavoriteTracks(_)
        | Route::AlbumArtistDetail(_)
        | Route::AlbumArtistDiscography(_)
        | Route::AlbumArtistTracks(_)
        | Route::AlbumArtistFavoriteTracks(_) => Some("Artist"),
        Route::PlaylistDetail(_) | Route::SmartPlaylistDetail(_) => Some("Playlist"),
        Route::GenreDetail(_) => Some("Genre"),
        Route::MoodDetail(_) => Some("Mood"),
        _ => None,
    };
    title.map(|title| {
        Arc::new(library::QueueContextTitle {
            kind: kind.map(Arc::from),
            title: title.into(),
        })
    })
}

fn home_section(row: &mut AndroidBrowseRow, block: library::HomeBlockKind) {
    row.section_id = serde_json::to_value(block)
        .expect("Home block serialization")
        .as_str()
        .expect("Home block name")
        .to_string();
    row.section = localization::tr(rufin_core::settings::presentation::home_block_title(block));
    row.section_kind = if block == library::HomeBlockKind::Showcase {
        "showcase"
    } else if block == library::HomeBlockKind::Genres {
        "genres"
    } else {
        "section"
    }
    .into();
    row.section_refreshable = block.section_kind().is_some();
}

fn artist_row(row: library::ArtistView, album_artist: bool) -> AndroidBrowseRow {
    let pin = library::source_entity_parts(&row.media_uri).map(|(source_id, _, artist_id)| {
        SidebarPin::Artist {
            source_id,
            artist_id,
            album_artist,
        }
    });
    AndroidBrowseRow {
        pin,
        ..AndroidBrowseRow::new(AndroidBrowseItem::Artist { row, album_artist })
    }
}

#[derive(uniffi::Object)]
pub struct AndroidBrowseQuery {
    selected: Option<SelectedLibrary>,
    database: Arc<library::Database>,
    runtime: tokio::runtime::Handle,
    playback: std::sync::Arc<rufin_core::playback::PlaybackOwner>,
    route: Route,
    category: CollectionCategory,
    filter: String,
    sort: LibraryField,
    descending: bool,
    now: i64,
    blocks: Vec<library::HomeBlockKind>,
    history_current_only: bool,
    favorites_only: bool,
    downloaded_only: bool,
    home_variations: (i64, i64),
    home_snapshot: tokio::sync::OnceCell<Vec<(AndroidBrowseRow, TrackSelection)>>,
    display: LibraryListSettings,
    album_tracks: LibraryListSettings,
    artist_tracks: LibraryListSettings,
    source: rufin_core::runtime::SourceHandle,
    settings: rufin_core::SettingsHandle,
    release_section: Option<ArtistReleaseSection>,
    release_counts: tokio::sync::OnceCell<[i64; 6]>,
    folder_projection: tokio::sync::OnceCell<FolderProjection>,
    query: tokio::sync::OnceCell<BrowseQuery>,
}

fn search_rows(rows: library::SearchResults) -> Vec<AndroidBrowseRow> {
    rows.tracks
        .into_iter()
        .map(Into::into)
        .chain(rows.albums.into_iter().map(Into::into))
        .chain(rows.artists.into_iter().map(|row| artist_row(row, false)))
        .collect()
}

impl AndroidBrowseQuery {
    fn selected(&self) -> &SelectedLibrary {
        self.selected
            .as_ref()
            .expect("Source-scoped query has a selected library")
    }

    fn artist_release_identity(&self) -> Option<(&str, bool)> {
        match &self.route {
            Route::ArtistDiscography(uri) => Some((uri, false)),
            Route::AlbumArtistDiscography(uri) => Some((uri, true)),
            Route::ArtistDetail(uri) => Some((uri, false)),
            Route::AlbumArtistDetail(uri) => Some((uri, true)),
            _ => None,
        }
    }

    async fn release_counts(&self) -> Result<[i64; 6], AndroidError> {
        self.release_counts
            .get_or_try_init(|| async {
                let mut request = self.request();
                request.category = CollectionCategory::Albums;
                let BrowseQuery::Releases(query) = request
                    .resolve(&self.database, None, &ReadCancellation::new())
                    .await
                    .map_err(error)?
                else {
                    return Ok([0; 6]);
                };
                query
                    .counts(&self.database, &ReadCancellation::new())
                    .await
                    .map(|counts| counts.map(|count| count as i64))
                    .map_err(error)
            })
            .await
            .copied()
    }

    fn detail_track_settings(&self) -> &LibraryListSettings {
        if self.detail_identity().is_some() && !matches!(self.route, Route::AlbumDetail(_)) {
            &self.artist_tracks
        } else {
            &self.display
        }
    }
    fn detail_identity(&self) -> Option<(&str, bool)> {
        match &self.route {
            Route::AlbumDetail(uri)
            | Route::ArtistDetail(uri)
            | Route::ArtistDiscography(uri)
            | Route::ArtistTracks(uri)
            | Route::ArtistFavoriteTracks(uri) => Some((uri, false)),
            Route::AlbumArtistDetail(uri)
            | Route::AlbumArtistDiscography(uri)
            | Route::AlbumArtistTracks(uri)
            | Route::AlbumArtistFavoriteTracks(uri) => Some((uri, true)),
            _ => None,
        }
    }
    fn album_detail_layout(&self) -> bool {
        self.display.layout == LibraryLayout::Detail
            && library_list_key(&self.route, self.category) == LibraryListKey::Albums
    }
    fn request(&self) -> BrowseRequest<'_> {
        BrowseRequest {
            history_source: self
                .history_current_only
                .then(|| self.selected().source_id.clone()),
            filter: &self.filter,
            sort: self.sort,
            descending: self.descending,
            favorites_only: self.favorites(),
            downloaded_only: self.downloaded(),
            now: self.now,
            inline_tracks: self.album_detail_layout().then_some(0),
            ..BrowseRequest::new(
                &self.route,
                self.category,
                self.selected.as_ref().map(|selected| selected.source_key),
                self.selected
                    .as_ref()
                    .and_then(|selected| selected.music_folder_key),
                &self.display,
                0,
            )
        }
    }

    async fn query(&self) -> library::LibraryResult<&BrowseQuery> {
        self.query
            .get_or_try_init(|| async {
                let folder = if matches!(self.route, Route::Folders { .. }) {
                    Some(self.folder_projection().await?)
                } else {
                    None
                };
                self.request()
                    .resolve(&self.database, folder, &ReadCancellation::new())
                    .await
            })
            .await
    }

    async fn track_query(&self) -> library::LibraryResult<Option<&CatalogQuery>> {
        Ok(self.query().await?.tracks())
    }

    fn downloaded(&self) -> bool {
        self.downloaded_only && self.supports_downloaded_filter()
    }

    fn favorites(&self) -> bool {
        (self.favorites_only && self.supports_favorite_filter())
            || matches!(
                self.route,
                Route::Favorites
                    | Route::ArtistFavoriteTracks(_)
                    | Route::AlbumArtistFavoriteTracks(_)
            )
    }
}

#[uniffi::export]
impl AndroidBrowseQuery {
    pub async fn selection(self: Arc<Self>) -> Result<Arc<TrackSelection>, AndroidError> {
        let runtime = self.runtime.clone();
        runtime
            .spawn(async move {
                self.query()
                    .await
                    .map_err(error)?
                    .collection_selection(matches!(self.route, Route::Folders { .. }))
                    .map(Arc::new)
                    .ok_or_else(|| error("Choose a collection"))
            })
            .await
            .map_err(error)?
    }
}
