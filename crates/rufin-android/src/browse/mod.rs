use std::sync::Arc;
mod commands;
mod details;
mod records;
mod search;
pub use records::{
    AndroidArtistLink, AndroidArtistReleaseGroup, AndroidBreadcrumb, AndroidBrowseDisplay,
    AndroidBrowseRow, AndroidCollectionCategory, AndroidDetailLink, AndroidDetailSummary,
    AndroidFieldChoice, AndroidFieldValue, AndroidHomeSection, AndroidHomeSectionKind,
    AndroidMusicFolder, AndroidMusicFolderState, AndroidOpened, AndroidPinPage,
    AndroidPlaylistImport, AndroidReading, AndroidRouteDescriptor, AndroidRouteSetting,
    AndroidRows, AndroidScrollSection, AndroidSortField,
};

use library::{MemberSelection, ReadCancellation, Row};
use rufin_core::collection_context::CollectionTarget;
use rufin_core::playback::{PlayRequest, QueuePlacement};
use rufin_core::view::{OpenOptions, PlayTarget, SelectionScope, View, library_list_key};
use rufin_core::{
    route::{CollectionCategory, Route, sidebar_route_descriptor},
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

fn browse_display(key: LibraryListKey, settings: &LibraryListSettings) -> AndroidBrowseDisplay {
    use rufin_core::settings::presentation::{library_field_title, library_fields_title};
    let layout = settings.layout;
    let shown = settings.fields(layout);
    let hidden = rufin_core::settings::layout::available_fields(key, layout)
        .iter()
        .filter(|field| !shown.contains(field));
    AndroidBrowseDisplay {
        fields_title: localization::tr(library_fields_title(layout)),
        field_choices: shown
            .iter()
            .chain(hidden)
            .map(|field| AndroidFieldChoice {
                id: *field,
                title: localization::tr(library_field_title(*field)),
                shown: shown.contains(field),
                can_toggle: settings.can_toggle_field(layout, *field),
            })
            .collect(),
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

/// What a play names in the queue: `title`, of the collection `target` is.
fn caption(
    target: Option<CollectionTarget>,
    title: Option<String>,
) -> Option<Arc<library::QueueContextTitle>> {
    title.map(|title| {
        Arc::new(library::QueueContextTitle {
            kind: target.map(|target| target.kind().into()),
            title: title.into(),
        })
    })
}

/// The caption of a play of `row`: a collection by its name, a track of the Albums list in
/// Detail layout by its album, another track by the page `view` shows, titled `page_title`.
fn row_caption(
    row: &AndroidBrowseRow,
    view: Option<&AndroidView>,
    page_title: Option<String>,
) -> Option<Arc<library::QueueContextTitle>> {
    use rufin_core::settings::presentation::row_field;
    match &row.row {
        Row::AlbumTrack { track, .. } => caption(
            Some(CollectionTarget::Album(String::new())),
            Some(track.album.clone()),
        ),
        Row::Track(_) => caption(
            view.and_then(|view| CollectionTarget::for_route(view.view.route())),
            page_title,
        ),
        library => caption(
            row.route.as_ref().and_then(CollectionTarget::for_route),
            row.display_title.clone().or_else(|| match library {
                Row::Folder(_) => None,
                library => Some(row_field(library, LibraryField::Title)),
            }),
        ),
    }
}

/// The album or artist a detail route shows, and whether it is read as an album artist.
fn detail_identity(route: &Route) -> Option<(&str, bool)> {
    match route {
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

/// `row` showing `fields`; `index` is its place among the page's rows of its kind.
fn shown_row(row: Row, fields: &[LibraryField], index: usize) -> AndroidBrowseRow {
    let fields = if matches!(row, Row::AlbumHeading(_)) {
        Vec::new()
    } else {
        fields
            .iter()
            .map(|field| AndroidFieldValue {
                id: *field,
                text: if *field == LibraryField::RowIndex {
                    (index + 1).to_string()
                } else {
                    rufin_core::settings::presentation::row_field(&row, *field)
                },
            })
            .collect()
    };
    AndroidBrowseRow {
        fields,
        ..row.into()
    }
}

/// A route a client shows, open: its rows on demand, its plays and what a change means for it.
#[derive(uniffi::Object)]
pub struct AndroidView {
    view: Arc<View>,
    runtime: tokio::runtime::Handle,
    playback: Arc<rufin_core::playback::PlaybackOwner>,
    settings: rufin_core::SettingsHandle,
    source: rufin_core::runtime::SourceHandle,
    /// The fields its rows show, and the fields an artist's favorite tracks show.
    fields: Vec<LibraryField>,
    favorite_fields: Vec<LibraryField>,
    /// The committed changes its rows show.
    changes: rufin_core::shown::ShownChanges,
}

impl AndroidView {
    fn key(&self) -> LibraryListKey {
        library_list_key(self.view.route(), self.view.options().category)
    }

    /// The rows of the view that start at `first`.
    fn rows_from(&self, rows: Vec<Row>, first: usize) -> Vec<AndroidBrowseRow> {
        let folders = self.view.folder_len().unwrap_or(0);
        rows.into_iter()
            .zip(first..)
            .map(|(row, position)| match row {
                Row::Track(_) => shown_row(row, &self.fields, position.saturating_sub(folders)),
                Row::AlbumTrack { index, .. } => shown_row(row, &self.fields, index as usize),
                // A child folder opens the folder core names, and shows no fields.
                row @ Row::Folder(_) => match self.view.row_route(&row) {
                    Some(route) => AndroidBrowseRow::opening(row, route),
                    None => AndroidBrowseRow::new(row),
                },
                row => shown_row(row, &self.fields, position),
            })
            .collect()
    }

    /// Home's sections as rows, in core's order and with its titles.
    fn home_sections(&self) -> Vec<AndroidHomeSection> {
        use library::HomeBlockKind;
        use rufin_core::settings::layout::{HOME_ALBUM_FIELDS, HOME_TRACK_FIELDS};
        use rufin_core::view::{HomeItem, HomeSection};
        // Home cards show GTK's Home labels.
        let album_card = |album: library::HomeAlbumRow| AndroidBrowseRow {
            display_title: Some(album.title),
            ..shown_row(Row::Album(album.album), &HOME_ALBUM_FIELDS, 0)
        };
        let track_card = |track: library::HomeTrackRow| AndroidBrowseRow {
            display_title: Some(track.title),
            ..shown_row(Row::Track(track.track), &HOME_TRACK_FIELDS, 0)
        };
        let refreshable = |block: HomeBlockKind| block.section_kind().map(|_| block);
        self.view
            .home_sections()
            .unwrap_or_default()
            .into_iter()
            .map(|section| {
                let title = section.title();
                let items = || {
                    section
                        .items()
                        .into_iter()
                        .map(|item| match item {
                            HomeItem::Track(row) => track_card(*row),
                            HomeItem::Album(row) => album_card(*row),
                        })
                        .collect()
                };
                let (kind, refresh, rows) = match &section {
                    HomeSection::Showcase(row) => (
                        AndroidHomeSectionKind::Showcase,
                        refreshable(HomeBlockKind::Showcase),
                        vec![album_card(row.as_ref().clone())],
                    ),
                    HomeSection::Rows { block, .. } => {
                        (AndroidHomeSectionKind::Rows, refreshable(*block), items())
                    }
                    HomeSection::Provider(_) => (AndroidHomeSectionKind::Rows, None, items()),
                    HomeSection::Genres(genres) => (
                        AndroidHomeSectionKind::Genres,
                        refreshable(HomeBlockKind::Genres),
                        genres
                            .iter()
                            .cloned()
                            .map(|row| Row::Tag(row).into())
                            .collect(),
                    ),
                };
                AndroidHomeSection {
                    title,
                    kind,
                    refresh,
                    rows,
                }
            })
            .collect()
    }

    fn play_request(&self, request: Option<PlayRequest>) -> Result<(), AndroidError> {
        self.playback
            .play_selection(request.ok_or_else(|| error("Track is no longer in this route"))?);
        Ok(())
    }
}

#[uniffi::export]
impl AndroidView {
    pub fn route(&self) -> Route {
        self.view.route().clone()
    }

    /// Rows `offset` onwards, at most `limit` and 256, and what committed while they read.
    pub async fn rows(
        self: Arc<Self>,
        offset: u64,
        limit: u32,
    ) -> Result<AndroidRows, AndroidError> {
        let runtime = self.runtime.clone();
        let reading = AndroidReading::new(self.source.reading());
        run_read(&runtime, move |cancel| async move {
            let offset = usize::try_from(offset).map_err(error)?;
            let rows = self
                .view
                .rows(offset..offset + limit.min(256) as usize, &cancel)
                .await
                .map_err(error)?;
            Ok(AndroidRows {
                rows: self.rows_from(rows, offset),
                reading,
            })
        })
        .await
    }

    /// What a committed change means for the page, in the library selected now. Rows show
    /// favorites, ratings and downloads through `AndroidLibrary::show_changes`.
    pub fn refresh(&self, changed: rufin_core::source::feed::Changed) -> rufin_core::view::Refresh {
        self.view
            .refresh(self.source.selected_library().as_ref(), &changed)
    }

    /// The row `current` shows at among the page's rows, as core places the playing track.
    pub fn playing_position(&self, current: Arc<library::QueueRow>) -> Option<u64> {
        let context = current.context_id();
        self.view
            .playing_position(
                current.track.track_key,
                context.as_deref().zip(current.source_rank()),
            )
            .map(|position| position as u64)
    }

    /// Whether the committed settings make the page read again: those its order, rows or Home
    /// blocks were read with changed.
    pub fn settings_reread(&self) -> bool {
        self.view.settings_reread(&self.settings.load()).is_some()
    }

    /// Plays or queues the page whole, as its header does.
    pub fn play_all(
        &self,
        placement: QueuePlacement,
        shuffled: bool,
        context_title: Option<String>,
    ) -> Result<(), AndroidError> {
        let target = CollectionTarget::for_route(self.view.route());
        self.playback.play_selection(
            self.view
                .play_all(placement, shuffled)
                .ok_or_else(|| error("Choose a collection"))?
                .with_context_title(caption(target, context_title)),
        );
        Ok(())
    }

    /// Plays Home's section `index`, which `context_title` names.
    pub fn play_section(
        &self,
        index: u64,
        shuffled: bool,
        context_title: Option<String>,
    ) -> Result<(), AndroidError> {
        let request = self.view.play_section(
            usize::try_from(index).map_err(error)?,
            QueuePlacement::Now,
            shuffled,
        );
        self.play_request(
            request.map(|request| request.with_context_title(caption(None, context_title))),
        )
    }

    /// Home's section titles, in their order.
    pub fn home_titles(&self) -> Vec<String> {
        self.view
            .home_sections()
            .unwrap_or_default()
            .iter()
            .map(rufin_core::view::HomeSection::title)
            .collect()
    }

    /// The folder an album or artist page's folder link opens, resolved when it is chosen.
    pub async fn folder_uri(&self) -> Result<String, AndroidError> {
        let (uri, album_artist) =
            detail_identity(self.view.route()).ok_or_else(|| error("Choose an album or artist"))?;
        self.source
            .collection_folder_uri(uri.to_owned(), album_artist)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub fn has_letter_sections(&self) -> bool {
        self.view.has_letter_sections()
    }

    pub async fn sections(self: Arc<Self>) -> Result<Vec<AndroidScrollSection>, AndroidError> {
        let runtime = self.runtime.clone();
        run_read(&runtime, move |cancel| async move {
            Ok(self
                .view
                .sections(&cancel)
                .await
                .map_err(error)?
                .into_iter()
                .map(|section| AndroidScrollSection {
                    title: section.title,
                    index: section.index,
                })
                .collect())
        })
        .await
    }

    pub fn supports_downloaded_filter(&self) -> bool {
        rufin_core::view::supports_downloaded_filter(
            self.view.route(),
            self.view.options().category,
        )
    }

    pub fn supports_favorite_filter(&self) -> bool {
        rufin_core::view::supports_favorite_filter(self.view.route(), self.view.options().category)
    }

    pub fn breadcrumbs(&self) -> Vec<AndroidBreadcrumb> {
        let Route::Folders { path } = self.view.route() else {
            return Vec::new();
        };
        let mut items = vec![AndroidBreadcrumb {
            title: localization::tr("Folders"),
            route: Route::Folders { path: Vec::new() },
        }];
        items.extend(
            path.iter()
                .enumerate()
                .map(|(index, item)| AndroidBreadcrumb {
                    title: item.name.clone(),
                    route: Route::Folders {
                        path: path[..=index].to_vec(),
                    },
                }),
        );
        items
    }

    pub fn sort_fields(&self) -> Vec<AndroidSortField> {
        rufin_core::settings::available_sort_fields(self.key())
            .iter()
            .map(|field| AndroidSortField::of(*field))
            .collect()
    }

    /// The list's display settings, in the order the page reads.
    pub fn display_settings(&self) -> AndroidBrowseDisplay {
        let key = self.key();
        let mut display = self.settings.load().library_list(key);
        (display.sort_key, display.descending) = self.view.order();
        browse_display(key, &display)
    }

    /// The fields the page's rows show, by title.
    pub fn fields(&self) -> Vec<AndroidSortField> {
        self.fields
            .iter()
            .map(|field| AndroidSortField::of(*field))
            .collect()
    }
}

impl AndroidLibrary {
    /// The fields a page on `route` shows in its layout. An artist's favorite tracks show as
    /// rows, on the artist's page and their own.
    fn shown_fields(&self, route: &Route, key: LibraryListKey) -> Vec<LibraryField> {
        if matches!(
            route,
            Route::ArtistFavoriteTracks(_) | Route::AlbumArtistFavoriteTracks(_)
        ) {
            return self.favorite_fields();
        }
        let display = self.settings.load().library_list(key);
        display.fields(display.layout).to_vec()
    }

    /// `view` as the page Kotlin shows.
    fn view_of(&self, view: Arc<View>) -> Arc<AndroidView> {
        let key = library_list_key(view.route(), view.options().category);
        Arc::new(AndroidView {
            fields: self.shown_fields(view.route(), key),
            favorite_fields: self.favorite_fields(),
            view,
            runtime: self.runtime.clone(),
            playback: self.playback.clone(),
            settings: self.settings.clone(),
            source: self.source.clone(),
            changes: Default::default(),
        })
    }

    fn favorite_fields(&self) -> Vec<LibraryField> {
        self.settings
            .load()
            .library_list(LibraryListKey::ArtistTracks)
            .row_fields
    }
}

#[uniffi::export]
impl AndroidLibrary {
    /// Opens `route` with these options and reads its first rows, its header unless it is
    /// `embedded`, an artist's release groups and favorite tracks, and Home's sections.
    /// `previous`, the view this one replaces, reads again in place when it lists the same
    /// route in the same library, so an open Smart Playlist keeps its members. History lists
    /// `history_source`'s plays, or every source's. What committed while it read comes with it,
    /// for the page to handle once it shows the rows.
    #[allow(clippy::too_many_arguments)]
    pub async fn open(
        self: Arc<Self>,
        route: Route,
        category: CollectionCategory,
        filter: String,
        sort: Option<LibraryField>,
        descending: bool,
        favorites_only: bool,
        downloaded_only: bool,
        history_source: Option<library::SourceId>,
        previous: Option<Arc<AndroidView>>,
        embedded: bool,
    ) -> Result<AndroidOpened, AndroidError> {
        let options = OpenOptions {
            category,
            filter,
            sort,
            descending: sort.map(|_| descending),
            favorites_only,
            downloaded_only,
            history_source,
            ..Default::default()
        };
        let runtime = self.runtime.clone();
        let reading = AndroidReading::new(self.source.reading());
        run_read(&runtime, move |cancel| async move {
            let seed = library::RouteSeedWindow::top();
            let selected = self.source.selected_library();
            let previous = previous.filter(|previous| {
                *previous.view.route() == route && previous.view.reads_same_in(selected.as_ref())
            });
            let reread = previous.is_some();
            let opened = match previous {
                Some(previous) => previous.view.reread(options, seed, &cancel).await,
                None => {
                    self.source
                        .open(route, selected, options, seed, true, &cancel)
                        .await
                }
            }
            .map_err(error)?;
            let view = self.view_of(opened.view);
            let rows = view.rows_from(opened.rows, opened.first);
            let groups = view
                .view
                .release_counts()
                .map(|(counts, _)| {
                    let mut index = 0;
                    rufin_core::settings::presentation::ARTIST_RELEASE_TITLES
                        .iter()
                        .zip(counts)
                        .enumerate()
                        .map(|(class, (title, total))| {
                            let group = AndroidArtistReleaseGroup {
                                id: class.to_string(),
                                title: (*title).into(),
                                total: total as u64,
                                index,
                            };
                            index += total as u64;
                            group
                        })
                        .collect()
                })
                .unwrap_or_default();
            let favorite_rows = opened
                .favorites
                .unwrap_or_default()
                .into_iter()
                .enumerate()
                .map(|(index, row)| shown_row(row, &view.favorite_fields, index))
                .collect();
            let shown = !view.view.is_missing() && !embedded;
            let header = opened.header.filter(|_| shown);
            let summary = header
                .map(|header| self.detail_summary(&view, header))
                .transpose()?;
            Ok(AndroidOpened {
                count: view.view.len() as u64,
                rows,
                summary,
                keeps_summary: reread && shown,
                groups,
                favorite_rows,
                home: view.home_sections(),
                view,
                reading,
            })
        })
        .await
    }

    /// Plays `row` at `placement` as core's row rule decides (decisions.md, "What a row plays
    /// is one rule"): shown at `index` on `view`'s page, or outside a page when `view` is None.
    /// A play of the page is captioned `page_title`.
    pub async fn play(
        &self,
        row: AndroidBrowseRow,
        view: Option<Arc<AndroidView>>,
        index: u64,
        placement: QueuePlacement,
        shuffled: bool,
        page_title: Option<String>,
    ) -> Result<(), AndroidError> {
        let index = usize::try_from(index).map_err(error)?;
        let caption = row_caption(&row, view.as_deref(), page_title);
        let source = self.source.clone();
        let selected = source.selected_library();
        let request = run_read(&self.runtime, move |cancel| async move {
            let selected = selected.as_ref();
            match (&view, PlayTarget::of_row(&row.row)) {
                (Some(view), _) => {
                    view.view
                        .play(&row.row, index, placement, shuffled, &cancel)
                        .await
                }
                (None, Some(target)) => source
                    .play_target(&target, selected, placement, shuffled, &cancel)
                    .await
                    .map(Some),
                (None, None) => Ok(None),
            }
            .map_err(error)
        })
        .await?
        .ok_or_else(|| error("Nothing to play here"))?;
        self.playback
            .play_selection(request.with_context_title(caption));
        Ok(())
    }

    /// Plays the track, album or artist `media_uri` of a listening overview, captioned `title`.
    pub async fn play_uri(
        &self,
        media_uri: String,
        title: Option<String>,
    ) -> Result<(), AndroidError> {
        let source = self.source.clone();
        let selected = source.selected_library();
        let request = run_read(&self.runtime, move |cancel| async move {
            source
                .play_uri(
                    &media_uri,
                    selected.as_ref(),
                    QueuePlacement::Now,
                    false,
                    &cancel,
                )
                .await
                .map_err(error)
        })
        .await?;
        self.playback
            .play_selection(request.with_context_title(caption(None, title)));
        Ok(())
    }
}
