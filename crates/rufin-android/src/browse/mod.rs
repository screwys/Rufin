use std::ops::Range;
use std::sync::Arc;
mod commands;
mod details;
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

use library::{Collection, MemberSelection, ReadCancellation, Row};
use rufin_core::view::{OpenOptions, Opened, View, library_list_key};
use rufin_core::{
    route::{CollectionCategory, Route, sidebar_route_descriptor},
    runtime::SelectedLibrary,
    settings::{
        LibraryField, LibraryLayout, LibraryListKey, LibraryListSettings, SidebarPin,
        SidebarRouteItem, layout::DisplaySize,
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

/// What a Home row plays.
fn home_selection(row: &AndroidBrowseRow) -> Option<MemberSelection> {
    let AndroidBrowseItem::Library { row } = &row.item else {
        return None;
    };
    Some(match row {
        Row::Album(album) => MemberSelection::Collections(vec![Collection::Album(album.album_key)]),
        Row::Track(track) => MemberSelection::Keys {
            source: None,
            keys: vec![track.track_key].into(),
        },
        Row::Tag(genre) => MemberSelection::Collections(vec![Collection::Genre(
            library::GenreKey::from_raw(genre.key),
        )]),
        _ => return None,
    })
}

#[derive(uniffi::Object)]
pub struct AndroidBrowseQuery {
    selected: Option<SelectedLibrary>,
    catalog: Arc<library::Catalog>,
    runtime: tokio::runtime::Handle,
    playback: Arc<rufin_core::playback::PlaybackOwner>,
    source: rufin_core::runtime::SourceHandle,
    settings: rufin_core::SettingsHandle,
    route: Route,
    options: OpenOptions,
    sort: LibraryField,
    descending: bool,
    blocks: Vec<library::HomeBlockKind>,
    display: LibraryListSettings,
    album_tracks: LibraryListSettings,
    artist_tracks: LibraryListSettings,
    view: tokio::sync::OnceCell<View>,
    home: std::sync::OnceLock<Vec<AndroidBrowseRow>>,
}

fn search_rows(mut rows: library::SearchResults) -> Vec<AndroidBrowseRow> {
    rows.tracks
        .into_iter()
        .map(|track| AndroidBrowseRow {
            lyrics_snippet: rows.lyrics.remove(&track.track_key),
            ..Row::Track(track).into()
        })
        .chain(rows.albums.into_iter().map(|row| Row::Album(row).into()))
        .chain(rows.artists.into_iter().map(|row| Row::Artist(row).into()))
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
            && library_list_key(&self.route, self.options.category) == LibraryListKey::Albums
    }

    async fn open(&self, cancel: &ReadCancellation) -> library::LibraryResult<Opened> {
        self.source
            .open(
                self.route.clone(),
                self.selected.clone(),
                self.options.clone(),
                library::RouteSeedWindow::top(),
                true,
                cancel,
            )
            .await
    }

    async fn view(&self, cancel: &ReadCancellation) -> library::LibraryResult<&View> {
        self.view
            .get_or_try_init(|| async { Ok(self.open(cancel).await?.view) })
            .await
    }

    fn downloaded(&self) -> bool {
        self.options.downloaded_only && self.supports_downloaded_filter()
    }

    fn favorites(&self) -> bool {
        (self.options.favorites_only && self.supports_favorite_filter())
            || matches!(
                self.route,
                Route::Favorites
                    | Route::ArtistFavoriteTracks(_)
                    | Route::AlbumArtistFavoriteTracks(_)
            )
    }

    fn selected_track_fields(&self) -> &[LibraryField] {
        if self.album_detail_layout() {
            &self.display.detail_track_fields
        } else {
            &self.detail_track_settings().row_fields
        }
    }

    /// The configured columns of a track row; `index` is its place among the page's tracks.
    fn track_fields(&self, track: &library::TrackRow, index: usize) -> Vec<AndroidFieldValue> {
        self.selected_track_fields()
            .iter()
            .map(|field| AndroidFieldValue {
                id: *field,
                text: if *field == LibraryField::RowIndex {
                    (index + 1).to_string()
                } else {
                    rufin_core::settings::presentation::track_row_field(track, *field)
                },
            })
            .collect()
    }

    /// The rows of `view` that start at `first`.
    fn browse_rows(&self, view: &View, rows: Vec<Row>, first: usize) -> Vec<AndroidBrowseRow> {
        let folders = view.folder_len().unwrap_or(0);
        let releases = view.release_counts().map(|(counts, _)| counts);
        rows.into_iter()
            .zip(first..)
            .map(|(row, position)| match row {
                Row::Track(ref track) => AndroidBrowseRow {
                    fields: self.track_fields(track, position.saturating_sub(folders)),
                    ..row.into()
                },
                Row::AlbumTrack { ref track, index } => AndroidBrowseRow {
                    fields: self.track_fields(track, index as usize),
                    ..row.into()
                },
                Row::Folder(folder) => {
                    let Route::Folders { path } = &self.route else {
                        unreachable!("Folder rows belong to a folder route")
                    };
                    let mut path = path.clone();
                    path.push(rufin_core::route::FolderPathItem {
                        id: folder.object_id,
                        name: folder.name,
                    });
                    AndroidBrowseRow::new(AndroidBrowseItem::Folder {
                        route: Route::Folders { path },
                        artwork: folder.artwork_binding,
                    })
                }
                Row::Album(_) if releases.is_some() => {
                    let mut end = 0;
                    let class = releases
                        .unwrap_or_default()
                        .iter()
                        .position(|count| {
                            end += count;
                            position < end
                        })
                        .unwrap_or(ARTIST_RELEASE_GROUPS.len() - 1);
                    let (id, title) = ARTIST_RELEASE_GROUPS[class];
                    AndroidBrowseRow {
                        section_id: id.into(),
                        section: localization::tr(title),
                        section_kind: "artist_release".into(),
                        ..row.into()
                    }
                }
                row => row.into(),
            })
            .collect()
    }

    async fn rows(
        &self,
        range: Range<usize>,
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<Vec<AndroidBrowseRow>> {
        let view = self.view(cancel).await?;
        if view.home().is_some() {
            let rows = self.home_rows(view);
            let range = range.start.min(rows.len())..range.end.min(rows.len());
            return Ok(rows[range].to_vec());
        }
        let first = range.start;
        Ok(self.browse_rows(view, view.rows(range, cancel).await?, first))
    }

    fn home_rows(&self, view: &View) -> &[AndroidBrowseRow] {
        self.home.get_or_init(|| {
            let home = view.home().expect("Home rows").clone();
            let mut rows = Vec::new();
            let block = |row: Row, block| {
                let mut row: AndroidBrowseRow = row.into();
                home_section(&mut row, block);
                row
            };
            let album = |album: library::HomeAlbumRow, kind| AndroidBrowseRow {
                display_title: Some(album.title),
                ..block(Row::Album(album.album), kind)
            };
            let track = |track: library::HomeTrackRow, kind| AndroidBrowseRow {
                display_title: Some(track.title),
                ..block(Row::Track(track.track), kind)
            };
            rows.extend(
                home.showcase
                    .map(|row| album(row, library::HomeBlockKind::Showcase)),
            );
            rows.extend(
                home.explore
                    .into_iter()
                    .map(|row| album(row, library::HomeBlockKind::Explore)),
            );
            for (kind, section) in [
                (library::HomeBlockKind::MostPlayed, home.most_played),
                (library::HomeBlockKind::NewlyAdded, home.newly_added),
                (library::HomeBlockKind::RecentlyPlayed, home.recently_played),
                (
                    library::HomeBlockKind::RecentlyReleased,
                    home.recently_released,
                ),
            ] {
                rows.extend(section.tracks.into_iter().map(|row| track(row, kind)));
                rows.extend(section.albums.into_iter().map(|row| album(row, kind)));
            }
            for section in home.provider_sections {
                let id = format!("provider:{}", section.section_id);
                let title = rufin_core::settings::presentation::home_provider_title(
                    &section.section_id,
                    section.title.as_deref(),
                );
                let provider = |row: AndroidBrowseRow| AndroidBrowseRow {
                    section: title.clone(),
                    section_id: id.clone(),
                    section_kind: "section".into(),
                    ..row
                };
                rows.extend(section.rows.tracks.into_iter().map(|row| {
                    provider(AndroidBrowseRow {
                        display_title: Some(row.title),
                        ..Row::Track(row.track).into()
                    })
                }));
                rows.extend(section.rows.albums.into_iter().map(|row| {
                    provider(AndroidBrowseRow {
                        display_title: Some(row.title),
                        ..Row::Album(row.album).into()
                    })
                }));
            }
            rows.extend(home.genres.into_iter().map(|row| AndroidBrowseRow {
                section: localization::tr("Featured genres"),
                section_id: "Genres".into(),
                section_kind: "genres".into(),
                ..AndroidBrowseRow::new(AndroidBrowseItem::Library { row: Row::Tag(row) })
            }));
            let provider = rows
                .iter()
                .any(|row| row.section_id.starts_with("provider:"));
            rows.sort_by_key(|row| {
                if provider && row.section_id == "Showcase" {
                    return 0;
                }
                if row.section_id.starts_with("provider:") {
                    return 1;
                }
                self.blocks
                    .iter()
                    .position(|block| {
                        serde_json::to_value(block)
                            .expect("Home block name")
                            .as_str()
                            == Some(row.section_id.as_str())
                    })
                    .map_or(usize::MAX, |index| index + 2)
            });
            let filter = self.options.filter.to_lowercase();
            rows.retain(|row| row.matches_filter(&filter));
            rows
        })
    }
}

#[uniffi::export]
impl AndroidBrowseQuery {
    pub async fn selection(self: Arc<Self>) -> Result<Arc<MemberSelection>, AndroidError> {
        let runtime = self.runtime.clone();
        run_read(&runtime, move |cancel| async move {
            self.source
                .selection(&self.route, self.selected.as_ref(), &self.options, &cancel)
                .await
                .map_err(error)?
                .map(Arc::new)
                .ok_or_else(|| error("Choose a collection"))
        })
        .await
    }
}
