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

use library::{Collection, ListRead, MemberRead, MemberRow, MemberSelection, ReadCancellation};
use rufin_core::browse::{DetailContent, OpenOptions, OpenedCollection, library_list_key};
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
    database: Arc<library::Database>,
    runtime: tokio::runtime::Handle,
    playback: std::sync::Arc<rufin_core::playback::PlaybackOwner>,
    route: Route,
    category: CollectionCategory,
    filter: String,
    sort: LibraryField,
    descending: bool,
    blocks: Vec<library::HomeBlockKind>,
    history_current_only: bool,
    favorites_only: bool,
    downloaded_only: bool,
    home_snapshot: tokio::sync::OnceCell<Vec<(AndroidBrowseRow, MemberSelection)>>,
    display: LibraryListSettings,
    album_tracks: LibraryListSettings,
    artist_tracks: LibraryListSettings,
    source: rufin_core::runtime::SourceHandle,
    settings: rufin_core::SettingsHandle,
    release_section: Option<ArtistReleaseSection>,
    opened: tokio::sync::OnceCell<OpenedCollection>,
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
    async fn opened(&self, cancel: &ReadCancellation) -> library::LibraryResult<&OpenedCollection> {
        self.opened
            .get_or_try_init(|| async {
                self.source
                    .open_collection(
                        &self.route,
                        self.selected.as_ref(),
                        &OpenOptions {
                            category: self.category,
                            filter: self.filter.clone(),
                            sort: Some(self.sort),
                            descending: Some(self.descending),
                            favorites_only: self.favorites_only,
                            downloaded_only: self.downloaded_only,
                            history_current_only: self.history_current_only,
                            ..Default::default()
                        },
                        library::RouteSeedWindow::top(),
                        cancel,
                    )
                    .await
            })
            .await
    }

    async fn members(
        &self,
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<Option<&MemberRead>> {
        Ok(match self.opened(&cancel).await? {
            OpenedCollection::Tracks(page)
            | OpenedCollection::Detail(Some(rufin_core::browse::DetailPage {
                content: DetailContent::Tracks(page),
                ..
            })) => Some(&page.read),
            OpenedCollection::Folders(page) => Some(&page.read),
            _ => None,
        })
    }

    async fn list(&self, cancel: &ReadCancellation) -> library::LibraryResult<Option<ListRead>> {
        Ok(match self.opened(&cancel).await? {
            OpenedCollection::Albums(page) => Some(page.read.clone()),
            OpenedCollection::Artists { read, .. }
            | OpenedCollection::Genres { read, .. }
            | OpenedCollection::Moods { read, .. }
            | OpenedCollection::Playlists { read, .. }
            | OpenedCollection::SmartPlaylists { read, .. } => Some(read.clone()),
            OpenedCollection::Detail(Some(rufin_core::browse::DetailPage {
                content: DetailContent::Releases { page, .. },
                ..
            })) => Some(page.query.clone()),
            _ => None,
        })
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
    pub async fn release_query(
        self: Arc<Self>,
        section_id: String,
        filter: String,
        sort: Option<LibraryField>,
        descending: bool,
    ) -> Result<Arc<Self>, AndroidError> {
        let section = release_section(&section_id)?;
        let runtime = self.runtime.clone();
        run_read(&runtime, move |cancel| async move {
            let OpenedCollection::Detail(Some(detail)) =
                self.opened(&cancel).await.map_err(error)?
            else {
                return Err(error("Choose an artist"));
            };
            let DetailContent::Releases { page, .. } = &detail.content else {
                return Err(error("Choose an artist"));
            };
            let display = self
                .settings
                .load()
                .library_list(LibraryListKey::ArtistAlbums);
            let (sort, descending) = sort.map_or((display.sort_key, display.descending), |sort| {
                (sort, descending)
            });
            let mut read = page.query.clone();
            let mut selected = library::Page {
                count: 0,
                first_row_position: 0,
                rows: Vec::new(),
            };
            if let ArtistReleaseSection::Class(class) = section {
                let index = release_index(class);
                let library::ListKind::ArtistAlbums {
                    release_class,
                    filters,
                    ..
                } = &mut read.kind
                else {
                    unreachable!("Artist albums")
                };
                let same_filter = filters
                    .as_ref()
                    .map_or(read.filter.as_str(), |filters| &filters[index])
                    .trim()
                    == filter.trim();
                *release_class = Some(class);
                *filters = None;
                let same_order = read.sort == sort.catalog_sort() && read.descending == descending;
                read.filter.clone_from(&filter);
                read.sort = sort.catalog_sort();
                read.descending = descending;
                if same_filter {
                    selected.count = page.counts[index];
                    if same_order {
                        let start: usize = page.counts[..index].iter().sum();
                        let first = page.page.first_row_position.max(start);
                        let last = (page.page.first_row_position + page.page.rows.len())
                            .min(start + selected.count);
                        if first < last {
                            selected.rows = page.page.rows[first - page.page.first_row_position
                                ..last - page.page.first_row_position]
                                .to_vec();
                            selected.first_row_position = first - start;
                        }
                    } else {
                        let range = library::RouteSeedWindow::top().range(selected.count);
                        selected.rows = read
                            .albums(&self.database, range.start, range.len(), &cancel)
                            .await
                            .map_err(error)?;
                    }
                } else {
                    selected = read
                        .page_albums(&self.database, library::RouteSeedWindow::top(), &cancel)
                        .await
                        .map_err(error)?;
                }
            }
            let opened = OpenedCollection::Albums(rufin_core::browse::AlbumListPage {
                read,
                content: rufin_core::browse::AlbumListContent::Rows(selected),
            });
            Ok(Arc::new(Self {
                selected: self.selected.clone(),
                database: self.database.clone(),
                runtime: self.runtime.clone(),
                playback: self.playback.clone(),
                route: match &self.route {
                    Route::ArtistDetail(uri) | Route::ArtistDiscography(uri) => {
                        Route::ArtistDiscography(uri.clone())
                    }
                    Route::AlbumArtistDetail(uri) | Route::AlbumArtistDiscography(uri) => {
                        Route::AlbumArtistDiscography(uri.clone())
                    }
                    _ => unreachable!("Artist releases"),
                },
                category: CollectionCategory::Albums,
                filter,
                sort,
                descending,
                blocks: self.blocks.clone(),
                history_current_only: false,
                favorites_only: false,
                downloaded_only: false,
                home_snapshot: tokio::sync::OnceCell::new(),
                display,
                album_tracks: self.album_tracks.clone(),
                artist_tracks: self.artist_tracks.clone(),
                source: self.source.clone(),
                settings: self.settings.clone(),
                release_section: Some(section),
                opened: tokio::sync::OnceCell::new_with(Some(opened)),
            }))
        })
        .await
    }

    pub async fn selection(self: Arc<Self>) -> Result<Arc<MemberSelection>, AndroidError> {
        let runtime = self.runtime.clone();
        run_read(&runtime, move |cancel| async move {
            if let Some(read) = self.members(&cancel).await.map_err(error)? {
                let mut read = read.clone();
                if !matches!(self.route, Route::Folders { .. }) {
                    if !matches!(read.collection, Collection::Smart { .. }) {
                        read.folder = None;
                    }
                    read.sort = None;
                    read.descending = false;
                }
                return Ok(Arc::new(read.selection()));
            }
            if let OpenedCollection::Detail(Some(rufin_core::browse::DetailPage {
                summary: rufin_core::browse::DetailSummary::Artist(artist),
                ..
            })) = self.opened(&cancel).await.map_err(error)?
            {
                if self.release_section.is_none() {
                    return Ok(Arc::new(MemberSelection::Collections(vec![
                        Collection::Artist {
                            key: artist.artist_key,
                            album_artist: self
                                .artist_release_identity()
                                .is_some_and(|(_, role)| role),
                        },
                    ])));
                }
            }
            self.list(&cancel)
                .await
                .map_err(error)?
                .map(|read| Arc::new(read.selection()))
                .ok_or_else(|| error("Choose a collection"))
        })
        .await
    }
}
