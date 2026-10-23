use std::sync::Arc;

use crate::route::{CollectionCategory, Route};
use crate::runtime::SelectedLibrary;
use crate::settings::{LibraryField, LibraryLayout, LibraryListKey, LibraryListSettings};
use library::{
    Collection, Database, ListKind, ListRead, MemberRead, MemberRow, Page, ReadCancellation,
};

#[derive(Clone, Debug)]
pub struct ArtistReleasePage {
    pub query: ListRead,
    pub counts: [usize; 6],
    pub source_counts: [usize; 6],
    pub page: Page<library::AlbumView>,
}

impl ArtistReleasePage {
    pub async fn load(
        database: &Database,
        query: ListRead,
        window: library::RouteSeedWindow,
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<Self> {
        let mut unfiltered = query.clone();
        unfiltered.filter.clear();
        if let ListKind::ArtistAlbums { filters, .. } = &mut unfiltered.kind {
            *filters = None;
        }
        let source_counts = unfiltered.artist_release_counts(database, cancel).await?;
        let filtered = match &query.kind {
            ListKind::ArtistAlbums {
                filters: Some(filters),
                ..
            } => filters.iter().any(|filter| !filter.trim().is_empty()),
            _ => !query.filter.trim().is_empty(),
        };
        let counts = if filtered {
            query.artist_release_counts(database, cancel).await?
        } else {
            source_counts
        };
        let range = window.range(counts.iter().sum());
        let first_rows = query
            .albums(database, range.start, range.len(), cancel)
            .await?;
        Ok(Self {
            query,
            counts,
            source_counts,
            page: Page {
                count: counts.iter().sum(),
                first_row_position: range.start,
                rows: first_rows,
            },
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FolderEntry {
    pub object_id: String,
    pub name: String,
    pub artwork_binding: Option<Vec<u8>>,
}

impl crate::source::folders::FolderProjection {
    pub async fn child_page(
        &self,
        database: &Database,
        source: library::SourceKey,
        query: &MemberRead,
        range: std::ops::Range<usize>,
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<(usize, Vec<FolderEntry>)> {
        Ok(match self {
            Self::Live { .. } => (
                self.live_folders(&query.filter, query.descending).count(),
                self.live_folders(&query.filter, query.descending)
                    .skip(range.start)
                    .take(range.len())
                    .map(|folder| FolderEntry {
                        object_id: folder.object_id.clone(),
                        name: folder.name.clone(),
                        artwork_binding: None,
                    })
                    .collect(),
            ),
            Self::Cached(folder) => {
                let (count, rows) = database
                    .filtered_folder_page(
                        source,
                        *folder,
                        &query.filter,
                        query.descending,
                        range.start,
                        range.len(),
                        cancel,
                    )
                    .await?;
                (
                    count,
                    rows.into_iter()
                        .map(|row| FolderEntry {
                            object_id: row.object_id,
                            name: row.metadata.name.unwrap_or_default(),
                            artwork_binding: row.artwork_binding,
                        })
                        .collect(),
                )
            }
        })
    }

    pub async fn sections(
        &self,
        database: &Database,
        source: library::SourceKey,
        query: &MemberRead,
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<Vec<library::ScrollSection>> {
        let mut sections = match self {
            Self::Live { .. } => {
                database
                    .ordered_name_sections(
                        self.live_folders(&query.filter, query.descending)
                            .map(|folder| folder.name.as_str()),
                        cancel,
                    )
                    .await?
            }
            Self::Cached(folder) => {
                database
                    .folder_section_positions(
                        source,
                        *folder,
                        &query.filter,
                        query.descending,
                        cancel,
                    )
                    .await?
            }
        };
        let folders = self
            .child_page(database, source, query, 0..0, cancel)
            .await?
            .0;
        for mut section in query.sections(database, cancel).await? {
            section.index += folders as u64;
            if let Some(existing) = sections
                .iter_mut()
                .find(|existing| existing.title == section.title)
            {
                existing.index = existing.index.min(section.index);
            } else {
                sections.push(section);
            }
        }
        sections.sort_by_key(|section| section.index);
        Ok(sections)
    }
}

pub fn library_list_key(route: &Route, category: CollectionCategory) -> LibraryListKey {
    match route {
        Route::Albums => LibraryListKey::Albums,
        Route::Artists => LibraryListKey::Artists,
        Route::AlbumArtists => LibraryListKey::AlbumArtists,
        Route::Genres => LibraryListKey::Genres,
        Route::Moods => LibraryListKey::Moods,
        Route::Playlists | Route::PlaylistFolder { smart: false, .. } => LibraryListKey::Playlists,
        Route::SmartPlaylists | Route::PlaylistFolder { smart: true, .. } => {
            LibraryListKey::SmartPlaylists
        }
        Route::PlaylistDetail(_) => LibraryListKey::PlaylistTracks,
        Route::SmartPlaylistDetail(_) => LibraryListKey::SmartPlaylistTracks,
        Route::AlbumDetail(_) => LibraryListKey::AlbumDetailTracks,
        Route::ArtistDiscography(_) | Route::AlbumArtistDiscography(_) => {
            LibraryListKey::ArtistAlbums
        }
        Route::ArtistDetail(_) | Route::AlbumArtistDetail(_)
            if category == CollectionCategory::Albums =>
        {
            LibraryListKey::ArtistAlbums
        }
        Route::ArtistDetail(_)
        | Route::AlbumArtistDetail(_)
        | Route::ArtistTracks(_)
        | Route::AlbumArtistTracks(_)
        | Route::ArtistFavoriteTracks(_)
        | Route::AlbumArtistFavoriteTracks(_) => LibraryListKey::ArtistTracks,
        Route::GenreDetail(_) => LibraryListKey::GenreTracks,
        Route::MoodDetail(_) => LibraryListKey::MoodTracks,
        Route::History => LibraryListKey::History,
        Route::Folders { .. } => LibraryListKey::Folders,
        Route::Favorites if category == CollectionCategory::Tracks => {
            LibraryListKey::FavoriteTracks
        }
        Route::Favorites => category.key(),
        _ => LibraryListKey::Tracks,
    }
}

#[derive(Clone)]
pub struct OpenOptions {
    pub category: CollectionCategory,
    pub filter: String,
    pub sort: Option<LibraryField>,
    pub descending: Option<bool>,
    pub favorites_only: bool,
    pub downloaded_only: bool,
    pub history_current_only: bool,
    pub release_filters: Option<[String; 6]>,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            category: CollectionCategory::Tracks,
            filter: String::new(),
            sort: None,
            descending: None,
            favorites_only: false,
            downloaded_only: false,
            history_current_only: false,
            release_filters: None,
        }
    }
}

#[derive(Clone)]
pub struct MemberPage {
    pub read: MemberRead,
    pub page: Page<MemberRow>,
    pub disc_sections: Vec<(u32, i64)>,
}

impl MemberPage {
    pub async fn load(
        database: &Database,
        read: MemberRead,
        window: library::RouteSeedWindow,
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<Self> {
        let page = read.page(database, window, cancel).await?;
        let disc_sections = if matches!(read.collection, Collection::Album(_)) {
            read.disc_sections(database, cancel).await?
        } else {
            Vec::new()
        };
        Ok(Self {
            read,
            page,
            disc_sections,
        })
    }
}

#[derive(Clone)]
pub enum DetailSummary {
    Album(Box<library::AlbumView>),
    Artist(Box<library::ArtistView>),
    Playlist(Box<library::PlaylistRow>),
    SmartPlaylist(Box<library::SmartPlaylistRow>),
    Genre(Box<library::GenreView>),
    Mood(Box<library::MoodView>),
}

#[derive(Clone)]
pub enum DetailContent {
    Tracks(MemberPage),
    Releases {
        page: Box<ArtistReleasePage>,
        favorites: Option<MemberPage>,
    },
}

#[derive(Clone)]
pub struct DetailPage {
    pub summary: DetailSummary,
    pub content: DetailContent,
}

#[derive(Clone)]
pub struct FolderPage {
    pub projection: crate::source::folders::FolderProjection,
    pub read: MemberRead,
    pub folder_count: usize,
    pub track_count: usize,
    pub first_row_position: usize,
    pub folders: Vec<FolderEntry>,
    pub tracks: Vec<MemberRow>,
}

impl FolderPage {
    pub async fn load(
        database: &Database,
        source: library::SourceKey,
        projection: crate::source::folders::FolderProjection,
        read: MemberRead,
        window: library::RouteSeedWindow,
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<Self> {
        let folder_count = projection
            .child_page(database, source, &read, 0..0, cancel)
            .await?
            .0;
        let track_count = read.count(database, cancel).await?;
        let range = window.range(folder_count + track_count);
        let folders = if range.start < folder_count {
            projection
                .child_page(database, source, &read, range.clone(), cancel)
                .await?
                .1
        } else {
            Vec::new()
        };
        let tracks = read
            .rows(
                database,
                range.start.saturating_sub(folder_count),
                range.len().saturating_sub(folders.len()),
                cancel,
            )
            .await?;
        Ok(Self {
            projection,
            read,
            folder_count,
            track_count,
            first_row_position: range.start,
            folders,
            tracks,
        })
    }
}

#[derive(Clone)]
pub enum AlbumListContent {
    Rows(Page<library::AlbumView>),
    Detail {
        index: Arc<library::AlbumDetailIndex>,
        page: Page<library::AlbumDetailRow>,
    },
}
#[derive(Clone)]
pub struct AlbumListPage {
    pub read: ListRead,
    pub content: AlbumListContent,
}

#[derive(Clone)]
pub enum OpenedCollection {
    Tracks(MemberPage),
    Detail(Option<DetailPage>),
    Albums(AlbumListPage),
    Artists {
        read: ListRead,
        page: Page<library::ArtistView>,
    },
    Genres {
        read: ListRead,
        page: Page<library::GenreView>,
    },
    Moods {
        read: ListRead,
        page: Page<library::MoodView>,
    },
    Playlists {
        read: ListRead,
        page: Page<library::PlaylistListRow>,
    },
    SmartPlaylists {
        read: ListRead,
        page: Page<library::PlaylistListRow>,
    },
    Folders(FolderPage),
    Home(Box<library::HomePage>),
    Search(library::SearchResults),
}

pub(crate) struct PreparedAlbums {
    revision: crate::runtime::CatalogRevision,
    configuration: tokio::sync::watch::Receiver<()>,
    index: Arc<library::AlbumDetailIndex>,
}

pub fn list_display(captured: &ListRead, filter: &str, settings: &LibraryListSettings) -> ListRead {
    let mut read = captured.clone();
    read.filter = filter.trim().to_owned();
    read.sort = settings.sort_key.catalog_sort();
    read.descending = settings.descending;
    read.activity = match settings.layout {
        LibraryLayout::Grid => &settings.grid_fields,
        _ => &settings.row_fields,
    }
    .iter()
    .chain(if settings.layout == LibraryLayout::Detail {
        settings.detail_track_fields.as_slice()
    } else {
        &[]
    })
    .any(|field| field.catalog_sort().activity_sensitive());
    read
}

pub fn supports_favorite_filter(route: &Route, category: CollectionCategory) -> bool {
    matches!(
        route,
        Route::Tracks
            | Route::Albums
            | Route::Artists
            | Route::AlbumArtists
            | Route::AlbumDetail(_)
            | Route::ArtistTracks(_)
            | Route::AlbumArtistTracks(_)
    ) || matches!(route, Route::GenreDetail(_) | Route::MoodDetail(_))
        && category == CollectionCategory::Tracks
}

pub fn supports_downloaded_filter(route: &Route, category: CollectionCategory) -> bool {
    matches!(
        route,
        Route::Tracks
            | Route::AlbumDetail(_)
            | Route::ArtistTracks(_)
            | Route::AlbumArtistTracks(_)
            | Route::ArtistFavoriteTracks(_)
            | Route::AlbumArtistFavoriteTracks(_)
    ) || matches!(
        route,
        Route::Favorites | Route::GenreDetail(_) | Route::MoodDetail(_)
    ) && category == CollectionCategory::Tracks
}

pub fn uses_activity(route: &Route, settings: &LibraryListSettings) -> bool {
    matches!(
        route,
        Route::Home
            | Route::SmartPlaylists
            | Route::SmartPlaylistDetail(_)
            | Route::PlaylistFolder { smart: true, .. }
            | Route::History
    ) || settings.sort_key.catalog_sort().activity_sensitive()
        || match settings.layout {
            LibraryLayout::Grid => &settings.grid_fields,
            _ => &settings.row_fields,
        }
        .iter()
        .chain(if settings.layout == LibraryLayout::Detail {
            settings.detail_track_fields.as_slice()
        } else {
            &[]
        })
        .any(|field| field.catalog_sort().activity_sensitive())
}

impl crate::source::SourceOwner {
    pub async fn album_list_page(
        &self,
        read: ListRead,
        window: library::RouteSeedWindow,
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<AlbumListPage> {
        if self
            .shared
            .settings
            .load()
            .ui
            .library_list(LibraryListKey::Albums)
            .layout
            != LibraryLayout::Detail
        {
            return Ok(AlbumListPage {
                content: AlbumListContent::Rows(
                    read.page_albums(&self.shared.database, window, cancel)
                        .await?,
                ),
                read,
            });
        }
        let mut retained = tokio::select! {
            retained = self.shared.prepared_albums.lock() => retained,
            () = cancel.cancelled() => return Err(library::LibraryError::ReadCancelled),
        };
        let revision = self.catalog_changes().borrow().clone();
        let index = if let Some(prepared) = retained.as_ref().filter(|prepared| {
            prepared.index.matches(&read)
                && !prepared.configuration.has_changed().unwrap_or(true)
                && prepared.revision.catalog == revision.catalog
                && (!read.sort.activity_sensitive()
                    || prepared.revision.activity == revision.activity)
        }) {
            Arc::clone(&prepared.index)
        } else {
            let index = read
                .album_detail_index(&self.shared.database, cancel)
                .await?;
            *retained = Some(PreparedAlbums {
                revision,
                configuration: self.configuration_changes(),
                index: Arc::clone(&index),
            });
            index
        };
        drop(retained);
        let page = index.page(&self.shared.database, window, cancel).await?;
        Ok(AlbumListPage {
            read,
            content: AlbumListContent::Detail { index, page },
        })
    }

    pub async fn open_collection(
        &self,
        route: &Route,
        selected: Option<&SelectedLibrary>,
        options: &OpenOptions,
        window: library::RouteSeedWindow,
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<OpenedCollection> {
        let database = &self.shared.database;
        let settings = self.shared.settings.load();
        let key = library_list_key(route, options.category);
        let display = settings.ui.library_list(key);
        let sort = options.sort.unwrap_or(display.sort_key).catalog_sort();
        let descending = options.descending.unwrap_or(display.descending);
        let activity = uses_activity(route, &display) || sort.activity_sensitive();
        let folder = selected.and_then(|selected| selected.music_folder_key);
        let source = selected.map(|selected| selected.source_key);
        let require_source = || {
            source.ok_or_else(|| library::LibraryError::InvalidRequest("No source selected".into()))
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs() as i64);
        let favorites = options.favorites_only && supports_favorite_filter(route, options.category)
            || matches!(
                route,
                Route::Favorites
                    | Route::ArtistFavoriteTracks(_)
                    | Route::AlbumArtistFavoriteTracks(_)
            );
        let member = |collection| {
            let mut read = MemberRead::new(collection);
            read.sort = Some(sort);
            read.descending = descending;
            read.folder = folder;
            read.filter.clone_from(&options.filter);
            read.favorites_only = favorites;
            read.downloaded_only =
                options.downloaded_only && supports_downloaded_filter(route, options.category);
            read.activity = activity;
            read
        };
        let list = |kind| {
            let mut read = ListRead::new(kind, source, sort);
            read.folder = folder;
            read.filter.clone_from(&options.filter);
            read.descending = descending;
            read.favorites_only = favorites;
            read.activity = activity;
            read
        };
        let summary_and_members = match route {
            Route::AlbumDetail(uri) => database.album(uri, activity, cancel).await?.map(|row| {
                let read = member(Collection::Album(row.album_key));
                (DetailSummary::Album(Box::new(row)), read)
            }),
            Route::ArtistDetail(uri)
            | Route::AlbumArtistDetail(uri)
            | Route::ArtistDiscography(uri)
            | Route::AlbumArtistDiscography(uri)
            | Route::ArtistTracks(uri)
            | Route::AlbumArtistTracks(uri)
            | Route::ArtistFavoriteTracks(uri)
            | Route::AlbumArtistFavoriteTracks(uri) => {
                let role = matches!(
                    route,
                    Route::AlbumArtistDetail(_)
                        | Route::AlbumArtistDiscography(_)
                        | Route::AlbumArtistTracks(_)
                        | Route::AlbumArtistFavoriteTracks(_)
                );
                let Some(row) = database.artist(uri, role, activity, cancel).await? else {
                    return Ok(OpenedCollection::Detail(None));
                };
                let read = member(Collection::Artist {
                    key: row.artist_key,
                    album_artist: role,
                });
                if matches!(
                    route,
                    Route::ArtistDetail(_)
                        | Route::AlbumArtistDetail(_)
                        | Route::ArtistDiscography(_)
                        | Route::AlbumArtistDiscography(_)
                ) {
                    let mut query = list(ListKind::ArtistAlbums {
                        artist: row.artist_key,
                        album_artist: role,
                        release_class: None,
                        filters: options.release_filters.as_ref().map(|filters| {
                            Arc::new(std::array::from_fn(|index| filters[index].clone()))
                        }),
                    });
                    query.source = Some(row.source_key);
                    if options.sort.is_none() {
                        let albums = settings.ui.library_list(LibraryListKey::ArtistAlbums);
                        query.sort = albums.sort_key.catalog_sort();
                        query.descending = options.descending.unwrap_or(albums.descending);
                    }
                    let page =
                        Box::new(ArtistReleasePage::load(database, query, window, cancel).await?);
                    let favorites =
                        if matches!(route, Route::ArtistDetail(_) | Route::AlbumArtistDetail(_)) {
                            let tracks = settings.ui.library_list(LibraryListKey::ArtistTracks);
                            let mut read = read;
                            read.sort = Some(tracks.sort_key.catalog_sort());
                            read.descending = tracks.descending;
                            read.favorites_only = true;
                            Some(
                                MemberPage::load(
                                    database,
                                    read,
                                    library::RouteSeedWindow::top(),
                                    cancel,
                                )
                                .await?,
                            )
                        } else {
                            None
                        };
                    return Ok(OpenedCollection::Detail(Some(DetailPage {
                        summary: DetailSummary::Artist(Box::new(row)),
                        content: DetailContent::Releases { page, favorites },
                    })));
                }
                Some((DetailSummary::Artist(Box::new(row)), read))
            }
            Route::PlaylistDetail(key) => {
                database.playlists(&[*key], cancel).await?.pop().map(|row| {
                    (
                        DetailSummary::Playlist(Box::new(row)),
                        member(Collection::Playlist(*key)),
                    )
                })
            }
            Route::SmartPlaylistDetail(key) => database
                .smart_playlist_rows(&[*key], now, cancel)
                .await?
                .pop()
                .map(|row| {
                    (
                        DetailSummary::SmartPlaylist(Box::new(row)),
                        member(Collection::Smart { key: *key, now }),
                    )
                }),
            Route::GenreDetail(key) => database
                .genres(&[*key], folder, cancel)
                .await?
                .pop()
                .filter(|row| row.summary.track_count > 0)
                .map(|row| {
                    (
                        DetailSummary::Genre(Box::new(row)),
                        member(Collection::Genre(*key)),
                    )
                }),
            Route::MoodDetail(key) => database
                .moods(&[*key], folder, cancel)
                .await?
                .pop()
                .filter(|row| row.summary.track_count > 0)
                .map(|row| {
                    (
                        DetailSummary::Mood(Box::new(row)),
                        member(Collection::Mood(*key)),
                    )
                }),
            _ => None,
        };
        if let Some((summary, read)) = summary_and_members {
            let page = if read.filter.trim().is_empty()
                && !read.favorites_only
                && !read.downloaded_only
                && (read.folder.is_none()
                    || matches!(read.collection, Collection::Smart { .. })
                    || matches!(&summary, DetailSummary::Playlist(row) if row.source_key.is_none()))
            {
                let count = match &summary {
                    DetailSummary::Album(row) => row.summary.track_count,
                    DetailSummary::Artist(row) => row.summary.track_count,
                    DetailSummary::Genre(row) => row.summary.track_count,
                    DetailSummary::Mood(row) => row.summary.track_count,
                    DetailSummary::Playlist(row) => row.track_count,
                    DetailSummary::SmartPlaylist(row) => row.track_count,
                }
                .max(0) as usize;
                let range = window.range(count);
                let rows = read
                    .rows(database, range.start, range.len(), cancel)
                    .await?;
                let disc_sections = if matches!(read.collection, Collection::Album(_)) {
                    read.disc_sections(database, cancel).await?
                } else {
                    Vec::new()
                };
                MemberPage {
                    read,
                    page: Page {
                        count,
                        first_row_position: range.start,
                        rows,
                    },
                    disc_sections,
                }
            } else {
                MemberPage::load(database, read, window, cancel).await?
            };
            return Ok(OpenedCollection::Detail(Some(DetailPage {
                summary,
                content: DetailContent::Tracks(page),
            })));
        }
        Ok(match route {
            Route::AlbumDetail(_)
            | Route::PlaylistDetail(_)
            | Route::SmartPlaylistDetail(_)
            | Route::GenreDetail(_)
            | Route::MoodDetail(_) => OpenedCollection::Detail(None),
            Route::Tracks | Route::Favorites
                if !matches!(route, Route::Favorites)
                    || options.category == CollectionCategory::Tracks =>
            {
                OpenedCollection::Tracks(
                    MemberPage::load(
                        database,
                        member(Collection::Source(require_source()?)),
                        window,
                        cancel,
                    )
                    .await?,
                )
            }
            Route::History => OpenedCollection::Tracks(
                MemberPage::load(
                    database,
                    member(Collection::History {
                        source: options
                            .history_current_only
                            .then(|| selected.map(|selected| selected.source_id.clone()))
                            .flatten(),
                    }),
                    window,
                    cancel,
                )
                .await?,
            ),
            Route::Albums | Route::Favorites
                if !matches!(route, Route::Favorites)
                    || options.category == CollectionCategory::Albums =>
            {
                require_source()?;
                OpenedCollection::Albums(
                    self.album_list_page(list(ListKind::Albums), window, cancel)
                        .await?,
                )
            }
            Route::Artists | Route::AlbumArtists | Route::Favorites => {
                require_source()?;
                let role = matches!(route, Route::AlbumArtists);
                let read = list(ListKind::Artists { album_artist: role });
                let page = read.page_artists(database, window, cancel).await?;
                OpenedCollection::Artists { read, page }
            }
            Route::Genres => {
                require_source()?;
                let read = list(ListKind::Genres);
                let page = read.page_genres(database, window, cancel).await?;
                OpenedCollection::Genres { read, page }
            }
            Route::Moods => {
                require_source()?;
                let read = list(ListKind::Moods);
                let page = read.page_moods(database, window, cancel).await?;
                OpenedCollection::Moods { read, page }
            }
            Route::Playlists | Route::PlaylistFolder { smart: false, .. } => {
                let mut read = list(ListKind::Playlists);
                read.playlist_scope = match route {
                    Route::PlaylistFolder { key, .. } => {
                        library::PlaylistScope::Folder { key: *key }
                    }
                    _ => library::PlaylistScope::Root,
                };
                let page = read.page_playlist_items(database, window, cancel).await?;
                OpenedCollection::Playlists { read, page }
            }
            Route::SmartPlaylists | Route::PlaylistFolder { smart: true, .. } => {
                let mut read = list(ListKind::SmartPlaylists { now });
                read.playlist_scope = match route {
                    Route::PlaylistFolder { key, .. } => {
                        library::PlaylistScope::Folder { key: *key }
                    }
                    _ => library::PlaylistScope::Root,
                };
                let page = read.page_playlist_items(database, window, cancel).await?;
                OpenedCollection::SmartPlaylists { read, page }
            }
            Route::Folders { path } => {
                let selected = selected.ok_or_else(|| {
                    library::LibraryError::InvalidRequest("No source selected".into())
                })?;
                let projection = crate::source::folders::resolve_folder(
                    self,
                    database,
                    selected,
                    path.last().map(|item| item.id.as_str()),
                    cancel,
                )
                .await?;
                let mut read = projection.members(selected.source_key);
                read.sort = Some(sort);
                read.descending = descending;
                read.filter.clone_from(&options.filter);
                read.activity = activity;
                OpenedCollection::Folders(
                    FolderPage::load(
                        database,
                        selected.source_key,
                        projection,
                        read,
                        window,
                        cancel,
                    )
                    .await?,
                )
            }
            Route::Home => {
                let (showcase, explore) = self.home_variations();
                OpenedCollection::Home(Box::new(
                    database
                        .home_page(
                            require_source()?,
                            folder,
                            showcase,
                            explore,
                            &settings.ui.application.home_blocks,
                            cancel,
                        )
                        .await?,
                ))
            }
            Route::Search => {
                let selected = selected.ok_or_else(|| {
                    library::LibraryError::InvalidRequest("No source selected".into())
                })?;
                OpenedCollection::Search(
                    crate::source::search::acquire_search(
                        self,
                        database,
                        selected,
                        options.filter.clone(),
                        60,
                        cancel.clone(),
                    )
                    .await
                    .map_err(library::LibraryError::InvalidRequest)?,
                )
            }
            _ => unreachable!("collection route was handled above"),
        })
    }
}
