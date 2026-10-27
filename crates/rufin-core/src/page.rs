//! An open route: what it opened with, its rows as data, and what a committed change means for
//! it. GTK, Android and the web draw it their own way.

use library::{
    AlbumDetailRow, Collection, MemberRead, MemberRow, MemberSelection, ReadCancellation,
};

use crate::browse::{
    AlbumListContent, AlbumListPage, DetailContent, DetailPage, FolderEntry, OpenOptions,
    OpenedCollection, library_list_key, uses_activity,
};
use crate::route::{CollectionCategory, Route};
use crate::runtime::{CatalogChange, CatalogPublication, SelectedLibrary};
use crate::settings::app::Settings;
use crate::settings::{LibraryLayout, LibraryListKey, LibraryListSettings};
use crate::source::SourceOwner;

/// What a committed change means for an open page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refresh {
    Nothing,
    /// Read the page again where it is; its rows may have moved, come or gone.
    Reread,
    /// The page's source or music folder changed; open the route again.
    Reopen,
}

/// One row of a page, as the catalog reads it.
#[derive(Clone, Debug)]
pub enum Row {
    Track(library::TrackView),
    Entry(library::PlaylistEntryView),
    Album(library::AlbumView),
    /// An album heading its tracks in the album list's Detail layout.
    AlbumHeading(library::AlbumView),
    /// A track under its album heading; `index` counts within the album.
    AlbumTrack {
        track: library::TrackView,
        index: usize,
    },
    /// A release on an artist page and its release group.
    Release {
        album: library::AlbumView,
        group: usize,
    },
    Artist {
        artist: library::ArtistView,
        album_artist: bool,
    },
    Genre(library::GenreView),
    Mood(library::MoodView),
    PlaylistItem(library::PlaylistListRow),
    Folder(FolderEntry),
}

pub struct Page {
    owner: SourceOwner,
    route: Route,
    selected: Option<SelectedLibrary>,
    options: OpenOptions,
    /// The settings the page read with.
    settings: Settings,
    opened: OpenedCollection,
}

impl SourceOwner {
    pub async fn open_page(
        &self,
        route: Route,
        selected: Option<SelectedLibrary>,
        mut options: OpenOptions,
        window: library::RouteSeedWindow,
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<Page> {
        let settings = self.shared.settings.load().ui;
        let opened = self
            .open_collection(&route, selected.as_ref(), &options, window, cancel)
            .await?;
        // Smart Playlists keep the members they opened with when the page reads again.
        options.smart_opened = match &opened {
            OpenedCollection::SmartPlaylists { read, .. } => match read.kind {
                library::ListKind::SmartPlaylists { now } => Some(now),
                _ => options.smart_opened,
            },
            OpenedCollection::Detail(Some(DetailPage {
                content: DetailContent::Tracks(page),
                ..
            })) => match page.read.collection {
                Collection::Smart { now, .. } => Some(now),
                _ => options.smart_opened,
            },
            _ => options.smart_opened,
        };
        Ok(Page {
            owner: self.clone(),
            route,
            selected,
            options,
            settings,
            opened,
        })
    }
}

impl Page {
    pub fn opened(&self) -> &OpenedCollection {
        &self.opened
    }

    pub fn route(&self) -> &Route {
        &self.route
    }

    pub fn options(&self) -> &OpenOptions {
        &self.options
    }

    /// The list settings of the route, as the page read with them.
    pub fn list_settings(&self) -> LibraryListSettings {
        self.settings
            .library_list(library_list_key(&self.route, self.options.category))
    }

    /// Reads the page again with `options` and current settings. A folder keeps the folder it
    /// opened, so new child folders show when the route opens again.
    pub async fn reread(
        &self,
        options: OpenOptions,
        window: library::RouteSeedWindow,
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<Page> {
        let (OpenedCollection::Folders(folders), Some(selected)) = (&self.opened, &self.selected)
        else {
            return self
                .owner
                .open_page(
                    self.route.clone(),
                    self.selected.clone(),
                    options,
                    window,
                    cancel,
                )
                .await;
        };
        let settings = self.owner.shared.settings.load().ui;
        let opened = OpenedCollection::Folders(
            self.owner
                .folder_page(
                    selected,
                    folders.projection.clone(),
                    &options,
                    window,
                    cancel,
                )
                .await?,
        );
        Ok(Page {
            owner: self.owner.clone(),
            route: self.route.clone(),
            selected: self.selected.clone(),
            options,
            settings,
            opened,
        })
    }

    /// Whether `current` settings change what the page reads. Named lists read again on every
    /// settings change.
    pub fn settings_changed(&self, current: &Settings) -> bool {
        let lists = |key| (self.settings.library_list(key), current.library_list(key));
        let order = |key| {
            let (read, current) = lists(key);
            read.sort_key != current.sort_key || read.descending != current.descending
        };
        match &self.opened {
            OpenedCollection::Home(_) | OpenedCollection::Search(_) => false,
            OpenedCollection::Albums(_) => {
                let (read, current) = lists(LibraryListKey::Albums);
                (read.layout == LibraryLayout::Detail) != (current.layout == LibraryLayout::Detail)
                    || read.sort_key != current.sort_key
                    || read.descending != current.descending
                    || uses_activity(&Route::Albums, &read)
                        != uses_activity(&Route::Albums, &current)
            }
            OpenedCollection::Artists { .. }
            | OpenedCollection::Genres { .. }
            | OpenedCollection::Moods { .. }
            | OpenedCollection::Playlists { .. }
            | OpenedCollection::SmartPlaylists { .. } => true,
            _ if matches!(
                self.route,
                Route::ArtistDetail(_) | Route::AlbumArtistDetail(_)
            ) =>
            {
                order(LibraryListKey::ArtistTracks) || order(LibraryListKey::ArtistAlbums)
            }
            _ => order(library_list_key(&self.route, self.options.category)),
        }
    }

    /// Whether a favorite change can move or drop rows: the page lists only favorites or sorts
    /// by them.
    pub fn favorites_reorder(&self) -> bool {
        if !matches!(
            self.route,
            Route::Tracks | Route::Favorites | Route::Albums | Route::Artists | Route::AlbumArtists
        ) {
            return false;
        }
        match &self.opened {
            OpenedCollection::Tracks(page) => {
                page.read.favorites_only || page.read.sort == Some(library::CatalogSort::Favorite)
            }
            OpenedCollection::Albums(AlbumListPage { read, .. })
            | OpenedCollection::Artists { read, .. } => {
                read.favorites_only || read.sort == library::CatalogSort::Favorite
            }
            _ => false,
        }
    }

    pub fn len(&self) -> usize {
        match &self.opened {
            OpenedCollection::Tracks(page)
            | OpenedCollection::Detail(Some(DetailPage {
                content: DetailContent::Tracks(page),
                ..
            })) => page.page.count,
            OpenedCollection::Albums(page) => match &page.content {
                AlbumListContent::Rows(page) => page.count,
                AlbumListContent::Detail { page, .. } => page.count,
            },
            OpenedCollection::Artists { page, .. } => page.count,
            OpenedCollection::Genres { page, .. } => page.count,
            OpenedCollection::Moods { page, .. } => page.count,
            OpenedCollection::Playlists { page, .. }
            | OpenedCollection::SmartPlaylists { page, .. } => page.count,
            OpenedCollection::Folders(page) => page.folder_count + page.track_count,
            OpenedCollection::Detail(Some(DetailPage {
                content: DetailContent::Releases { page, .. },
                ..
            })) => page.page.count,
            OpenedCollection::Home(_)
            | OpenedCollection::Search(_)
            | OpenedCollection::Detail(None) => 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The member read behind a track page, for selections and plays.
    pub fn members(&self) -> Option<&MemberRead> {
        match &self.opened {
            OpenedCollection::Tracks(page)
            | OpenedCollection::Detail(Some(DetailPage {
                content: DetailContent::Tracks(page),
                ..
            })) => Some(&page.read),
            OpenedCollection::Folders(page) => Some(&page.read),
            _ => None,
        }
    }

    /// Everything the page lists, for playing or queueing it whole.
    pub fn selection(&self) -> Option<MemberSelection> {
        if let Some(read) = self.members() {
            let mut read = read.clone();
            if !matches!(self.route, Route::Folders { .. }) {
                if !matches!(read.collection, Collection::Smart { .. }) {
                    read.folder = None;
                }
                read.sort = None;
                read.descending = false;
            }
            return Some(read.selection());
        }
        match &self.opened {
            OpenedCollection::Albums(page) => Some(page.read.clone().selection()),
            OpenedCollection::Artists { read, .. }
            | OpenedCollection::Genres { read, .. }
            | OpenedCollection::Moods { read, .. }
            | OpenedCollection::Playlists { read, .. }
            | OpenedCollection::SmartPlaylists { read, .. } => Some(read.clone().selection()),
            OpenedCollection::Detail(Some(DetailPage {
                content: DetailContent::Releases { page, .. },
                ..
            })) => Some(page.query.clone().selection()),
            _ => None,
        }
    }

    /// Letter-index positions, for pages listed by name.
    pub async fn sections(
        &self,
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<Vec<library::ScrollSection>> {
        let catalog = &self.owner.shared.catalog;
        match &self.opened {
            OpenedCollection::Folders(page) => {
                let source = self
                    .selected
                    .as_ref()
                    .map(|selected| selected.source_key)
                    .ok_or_else(|| {
                        library::LibraryError::InvalidRequest("No source selected".into())
                    })?;
                page.projection
                    .sections(catalog, source, &page.read, cancel)
                    .await
            }
            _ if self.route == Route::History => Ok(Vec::new()),
            OpenedCollection::Albums(page) => match page.content {
                AlbumListContent::Rows(_) => page.read.sections(catalog, cancel).await,
                AlbumListContent::Detail { .. } => Ok(Vec::new()),
            },
            OpenedCollection::Artists { read, .. }
            | OpenedCollection::Genres { read, .. }
            | OpenedCollection::Moods { read, .. }
            | OpenedCollection::Playlists { read, .. }
            | OpenedCollection::SmartPlaylists { read, .. } => read.sections(catalog, cancel).await,
            _ => match self.members() {
                Some(read) => read.sections(catalog, cancel).await,
                None => Ok(Vec::new()),
            },
        }
    }

    /// Rows `offset..offset + limit`, from the opened window when it holds them.
    pub async fn rows(
        &self,
        offset: usize,
        limit: usize,
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<Vec<Row>> {
        let catalog = &self.owner.shared.catalog;
        Ok(match &self.opened {
            OpenedCollection::Tracks(page)
            | OpenedCollection::Detail(Some(DetailPage {
                content: DetailContent::Tracks(page),
                ..
            })) => window(
                &page.page,
                offset,
                limit,
                page.read.rows(catalog, offset, limit, cancel),
            )
            .await?
            .into_iter()
            .map(member)
            .collect(),
            OpenedCollection::Albums(opened) => match &opened.content {
                AlbumListContent::Detail { index, page } => window(
                    page,
                    offset,
                    limit,
                    index.rows(catalog, offset, limit, cancel),
                )
                .await?
                .into_iter()
                .map(|row| match row {
                    AlbumDetailRow::Album { album, .. } => Row::AlbumHeading(album),
                    AlbumDetailRow::Track { track, index, .. } => Row::AlbumTrack { track, index },
                })
                .collect(),
                AlbumListContent::Rows(page) => window(
                    page,
                    offset,
                    limit,
                    opened.read.albums(catalog, offset, limit, cancel),
                )
                .await?
                .into_iter()
                .map(Row::Album)
                .collect(),
            },
            OpenedCollection::Artists { read, page } => {
                let album_artist = matches!(self.route, Route::AlbumArtists);
                window(
                    page,
                    offset,
                    limit,
                    read.artists(catalog, offset, limit, cancel),
                )
                .await?
                .into_iter()
                .map(|artist| Row::Artist {
                    artist,
                    album_artist,
                })
                .collect()
            }
            OpenedCollection::Genres { read, page } => window(
                page,
                offset,
                limit,
                read.genres(catalog, offset, limit, cancel),
            )
            .await?
            .into_iter()
            .map(Row::Genre)
            .collect(),
            OpenedCollection::Moods { read, page } => window(
                page,
                offset,
                limit,
                read.moods(catalog, offset, limit, cancel),
            )
            .await?
            .into_iter()
            .map(Row::Mood)
            .collect(),
            OpenedCollection::Playlists { read, page }
            | OpenedCollection::SmartPlaylists { read, page } => window(
                page,
                offset,
                limit,
                read.playlist_items(catalog, offset, limit, cancel),
            )
            .await?
            .into_iter()
            .map(Row::PlaylistItem)
            .collect(),
            OpenedCollection::Detail(Some(DetailPage {
                content: DetailContent::Releases { page, .. },
                ..
            })) => window(
                &page.page,
                offset,
                limit,
                page.query.albums(catalog, offset, limit, cancel),
            )
            .await?
            .into_iter()
            .enumerate()
            .map(|(index, album)| {
                let mut end = 0;
                let group = page
                    .counts
                    .iter()
                    .position(|count| {
                        end += count;
                        offset + index < end
                    })
                    .unwrap_or(4);
                Row::Release { album, group }
            })
            .collect(),
            OpenedCollection::Folders(page) => {
                let end = offset
                    .saturating_add(limit)
                    .min(page.folder_count + page.track_count);
                if offset == page.first_row_position
                    && end - offset == page.folders.len() + page.tracks.len()
                {
                    page.folders
                        .iter()
                        .cloned()
                        .map(Row::Folder)
                        .chain(page.tracks.iter().cloned().map(member))
                        .collect()
                } else {
                    let source = self
                        .selected
                        .as_ref()
                        .map(|selected| selected.source_key)
                        .ok_or_else(|| {
                            library::LibraryError::InvalidRequest("No source selected".into())
                        })?;
                    let folders = if offset < page.folder_count {
                        page.projection
                            .child_page(catalog, source, &page.read, offset..end, cancel)
                            .await?
                            .1
                    } else {
                        Vec::new()
                    };
                    let tracks = page
                        .read
                        .rows(
                            catalog,
                            offset.saturating_sub(page.folder_count),
                            (end - offset).saturating_sub(folders.len()),
                            cancel,
                        )
                        .await?;
                    folders
                        .into_iter()
                        .map(Row::Folder)
                        .chain(tracks.into_iter().map(member))
                        .collect()
                }
            }
            OpenedCollection::Home(_)
            | OpenedCollection::Search(_)
            | OpenedCollection::Detail(None) => Vec::new(),
        })
    }
}

fn member(row: MemberRow) -> Row {
    match row {
        MemberRow::Track(track) => Row::Track(track),
        MemberRow::Entry(entry) => Row::Entry(entry),
    }
}

async fn window<T: Clone>(
    page: &library::Page<T>,
    offset: usize,
    limit: usize,
    load: impl std::future::Future<Output = library::LibraryResult<Vec<T>>>,
) -> library::LibraryResult<Vec<T>> {
    if offset >= page.count {
        return Ok(Vec::new());
    }
    let end = offset.saturating_add(limit).min(page.count);
    let first = page.first_row_position;
    if offset >= first && end <= first + page.rows.len() {
        Ok(page.rows[offset - first..end - first].to_vec())
    } else {
        load.await
    }
}

/// What a committed change means for a page open on `route`.
pub fn refresh(
    owner: &SourceOwner,
    route: &Route,
    opened_with: Option<&SelectedLibrary>,
    options: &OpenOptions,
    publication: &CatalogPublication,
) -> Refresh {
    let current = owner.shared.selected();
    let affects = publication.source_key.is_none()
        || current.as_ref().map(|selected| selected.source_key) == publication.source_key;
    let uses_source = route.uses_selected_source()
        && (route != &Route::Search || options.search_mode == library::SearchMode::Title);
    let unaffected = !affects && route.requires_selected_source() && uses_source;
    match &publication.change {
        CatalogChange::Broad => {
            let scope = |selected: &SelectedLibrary| {
                (
                    selected.source_key,
                    selected.music_folder_key,
                    selected.music_folder_object_id.clone(),
                )
            };
            if unaffected {
                Refresh::Nothing
            } else if uses_source && opened_with.map(scope) != current.as_ref().map(scope) {
                Refresh::Reopen
            } else {
                Refresh::Reread
            }
        }
        CatalogChange::Activity => {
            let settings = owner.shared.settings.load();
            let categories: &[CollectionCategory] = match route {
                Route::Favorites => &[options.category],
                Route::ArtistDetail(_) | Route::AlbumArtistDetail(_) => {
                    &[CollectionCategory::Tracks, CollectionCategory::Albums]
                }
                _ => &[CollectionCategory::Tracks],
            };
            let sensitive = options
                .sort
                .is_some_and(|sort| sort.catalog_sort().activity_sensitive())
                || categories.iter().any(|category| {
                    uses_activity(
                        route,
                        &settings.ui.library_list(library_list_key(route, *category)),
                    )
                });
            if unaffected || !sensitive {
                Refresh::Nothing
            } else {
                Refresh::Reread
            }
        }
        CatalogChange::Scope => {
            if affects && uses_source {
                Refresh::Reopen
            } else {
                Refresh::Nothing
            }
        }
        // Rows show favorites from the publication itself.
        CatalogChange::Favorites => Refresh::Nothing,
        CatalogChange::Home => {
            if affects && route == &Route::Home {
                Refresh::Reread
            } else {
                Refresh::Nothing
            }
        }
        CatalogChange::Playlists(playlist) => {
            if matches!(
                route,
                Route::Playlists | Route::SmartPlaylists | Route::PlaylistFolder { .. }
            ) || (playlist.is_none() && matches!(route, Route::SmartPlaylistDetail(_)))
                || matches!(route, Route::PlaylistDetail(key) if playlist.is_none_or(|playlist| playlist == *key))
            {
                Refresh::Reread
            } else {
                Refresh::Nothing
            }
        }
        CatalogChange::Album(album) => {
            if matches!(route, Route::AlbumDetail(uri) if uri == album)
                || matches!(
                    route,
                    Route::ArtistDetail(_)
                        | Route::AlbumArtistDetail(_)
                        | Route::ArtistDiscography(_)
                        | Route::AlbumArtistDiscography(_)
                )
            {
                Refresh::Reread
            } else {
                Refresh::Nothing
            }
        }
    }
}

/// Whether a committed change can change the sidebar pins.
pub fn pins_refresh(owner: &SourceOwner, publication: &CatalogPublication) -> bool {
    match publication.change {
        CatalogChange::Broad | CatalogChange::Playlists(_) | CatalogChange::Album(_) => true,
        CatalogChange::Activity => owner.shared.settings.load().ui.sidebar.pins_use_activity(),
        CatalogChange::Scope | CatalogChange::Favorites | CatalogChange::Home => false,
    }
}
