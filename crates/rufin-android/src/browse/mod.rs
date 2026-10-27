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
    AndroidDetailLink, AndroidDetailSummary, AndroidFieldValue, AndroidMusicFolder,
    AndroidMusicFolderState, AndroidPinPage, AndroidPlaylistImport, AndroidRouteDescriptor,
    AndroidRouteSetting, AndroidScrollSection, AndroidSortField,
};

use library::{Collection, MemberRead, MemberRow, MemberSelection, ReadCancellation};
use rufin_core::browse::{OpenOptions, OpenedCollection, library_list_key};
use rufin_core::page::{Page, Row};
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

struct CancelReadOnDrop {
    cancel: ReadCancellation,
    task: tokio::task::AbortHandle,
}

impl Drop for CancelReadOnDrop {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.task.abort();
    }
}

async fn run_read<T, F>(
    runtime: &tokio::runtime::Handle,
    read: impl FnOnce(ReadCancellation) -> F,
) -> Result<T, AndroidError>
where
    T: Send + 'static,
    F: std::future::Future<Output = Result<T, AndroidError>> + Send + 'static,
{
    let cancel = ReadCancellation::new();
    let task = runtime.spawn(read(cancel.clone()));
    let _guard = CancelReadOnDrop {
        cancel,
        task: task.abort_handle(),
    };
    task.await.map_err(error)?
}

const ARTIST_RELEASE_GROUPS: [(&str, &str); 6] = [
    ("Album", "Albums"),
    ("Ep", "EPs"),
    ("Single", "Singles"),
    ("Collection", "Collections"),
    ("Other", "Other releases"),
    ("AppearsOn", "Appears On"),
];

fn release_row(album: library::AlbumView, class: usize) -> AndroidBrowseRow {
    let mut row: AndroidBrowseRow = album.into();
    let (id, title) = ARTIST_RELEASE_GROUPS[class];
    row.section_id = id.into();
    row.section = localization::tr(title);
    row.section_kind = "artist_release".into();
    row
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
    catalog: Arc<library::Catalog>,
    runtime: tokio::runtime::Handle,
    playback: std::sync::Arc<rufin_core::playback::PlaybackOwner>,
    route: Route,
    category: CollectionCategory,
    filter: String,
    sort: LibraryField,
    descending: bool,
    blocks: Vec<library::HomeBlockKind>,
    history_current_only: bool,
    smart_opened: Option<i64>,
    favorites_only: bool,
    downloaded_only: bool,
    home_snapshot: tokio::sync::OnceCell<Vec<(AndroidBrowseRow, MemberSelection)>>,
    display: LibraryListSettings,
    album_tracks: LibraryListSettings,
    artist_tracks: LibraryListSettings,
    source: rufin_core::runtime::SourceHandle,
    settings: rufin_core::SettingsHandle,
    page: tokio::sync::OnceCell<Page>,
}

fn search_rows(mut rows: library::SearchResults) -> Vec<AndroidBrowseRow> {
    rows.tracks
        .into_iter()
        .map(|track| {
            let snippet = rows.lyrics.remove(&track.track_key);
            AndroidBrowseRow {
                lyrics_snippet: snippet,
                ..track.into()
            }
        })
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
    fn open_options(&self) -> OpenOptions {
        OpenOptions {
            category: self.category,
            filter: self.filter.clone(),
            sort: Some(self.sort),
            descending: Some(self.descending),
            favorites_only: self.favorites_only,
            downloaded_only: self.downloaded_only,
            history_source: self
                .selected
                .as_ref()
                .filter(|_| self.history_current_only)
                .map(|selected| selected.source_id.clone()),
            smart_opened: self.smart_opened,
            ..Default::default()
        }
    }

    async fn opened_page(&self, cancel: &ReadCancellation) -> library::LibraryResult<&Page> {
        self.page
            .get_or_try_init(|| {
                self.source.open_page(
                    self.route.clone(),
                    self.selected.clone(),
                    self.open_options(),
                    library::RouteSeedWindow::top(),
                    cancel,
                )
            })
            .await
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
    pub async fn selection(self: Arc<Self>) -> Result<Arc<MemberSelection>, AndroidError> {
        let runtime = self.runtime.clone();
        run_read(&runtime, move |cancel| async move {
            self.opened_page(&cancel)
                .await
                .map_err(error)?
                .selection()
                .map(Arc::new)
                .ok_or_else(|| error("Choose a collection"))
        })
        .await
    }
}
