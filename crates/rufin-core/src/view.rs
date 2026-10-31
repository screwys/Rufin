//! An open route: the read behind it, its rows on demand, and what a change means for it.
//! GTK, Android and the web draw it their own way. A view holds its reads and counts; orders
//! live in the Catalog's cache, so every window reads the current one.

use std::ops::Range;
use std::sync::Arc;

use library::{
    AlbumSpan, Catalog, Collection, Fetch, FolderRead, FolderRow, Header, HeaderKey, HomePage,
    Item, LibraryError, LibraryResult, ListKind, ListRead, MemberRead, MemberSelection, Read,
    ReadCancellation, RouteSeedWindow, Row, ScrollSection, SourceKey,
};

use crate::route::{CollectionCategory, Route};
use crate::runtime::SelectedLibrary;
use crate::settings::app::Settings;
use crate::settings::{LibraryField, LibraryLayout, LibraryListKey, LibraryListSettings};
use crate::source::SourceOwner;
use crate::source::feed::Changed;
use crate::source::folders::FolderProjection;

/// What a committed change means for an open page, weakest first.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Refresh {
    Nothing,
    /// Read the page again where it is; its rows may have moved, come or gone.
    Reread,
    /// The page's source or music folder changed; open the route again.
    Reopen,
}

/// What a page reads again.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reread {
    /// The page, as [`View::reread`] reads it.
    Page,
    /// An artist page's releases, as [`View::reread_releases`] reads them.
    Releases,
}

#[derive(Clone, PartialEq)]
pub struct OpenOptions {
    pub category: CollectionCategory,
    pub filter: String,
    pub search_mode: library::SearchMode,
    pub sort: Option<LibraryField>,
    pub descending: Option<bool>,
    pub favorites_only: bool,
    pub downloaded_only: bool,
    /// The one source History lists; every source when `None`.
    pub history_source: Option<sources::SourceId>,
    pub release_filters: Option<[String; 6]>,
    /// When a Smart Playlist page first opened. Reopening the page with it keeps the members
    /// the page opened with; plays and syncs change them on the next visit.
    pub smart_opened: Option<i64>,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            category: CollectionCategory::Tracks,
            filter: String::new(),
            search_mode: library::SearchMode::Title,
            sort: None,
            descending: None,
            favorites_only: false,
            downloaded_only: false,
            history_source: None,
            release_filters: None,
            smart_opened: None,
        }
    }
}

pub struct View {
    owner: SourceOwner,
    route: Route,
    selected: Option<SelectedLibrary>,
    options: OpenOptions,
    /// The settings the view read with.
    settings: Settings,
    art: bool,
    body: Body,
}

enum Body {
    /// Lists, detail tracks, History, playlist entries and albums with their tracks. An album
    /// page holds where each disc starts, the Albums list in Detail layout where each album does.
    Rows {
        read: Read,
        len: usize,
        discs: Vec<(u32, i64)>,
        spans: Option<Arc<[AlbumSpan]>>,
    },
    /// An artist's releases, counted per release group as filtered and unfiltered, and the
    /// artist's favorite tracks when the page shows them.
    Releases {
        read: ListRead,
        counts: [usize; 6],
        source_counts: [usize; 6],
        favorites: Option<(MemberRead, usize)>,
    },
    /// Child folders, then the folder's tracks. Cached child folders have a read; live ones
    /// are the projection's.
    Folders {
        projection: FolderProjection,
        folders: Option<FolderRead>,
        folder_len: usize,
        tracks: MemberRead,
        track_len: usize,
    },
    Home(Box<HomePage>),
    /// The collection the route names is gone or empty, or the route lists nothing itself.
    Missing,
}

/// A view and the rows it read when it opened: its header, the seed window starting at
/// `first`, and an artist's favorite tracks. `header` and `favorites` are None when this read
/// did not read them; a reread of detail tracks or of releases keeps what the page shows.
pub struct Opened {
    pub view: Arc<View>,
    pub header: Option<Header>,
    pub first: usize,
    pub rows: Vec<Row>,
    pub favorites: Option<Vec<Row>>,
}

/// What a view reads before its body is known.
enum Plan {
    Rows(Read),
    Releases {
        read: ListRead,
        favorites: Option<MemberRead>,
        /// The favorite tracks a reread of the releases keeps.
        kept: Option<(MemberRead, usize)>,
    },
    Folders {
        projection: FolderProjection,
        tracks: MemberRead,
    },
    Home(Box<HomePage>),
    Missing,
}

impl SourceOwner {
    pub async fn open(
        &self,
        route: Route,
        selected: Option<SelectedLibrary>,
        options: OpenOptions,
        seed: RouteSeedWindow,
        art: bool,
        cancel: &ReadCancellation,
    ) -> LibraryResult<Opened> {
        let settings = self.shared.settings.load().ui;
        let (header, plan) = self
            .plan(&route, selected.as_ref(), &options, &settings, cancel)
            .await?;
        read(
            View {
                owner: self.clone(),
                route,
                selected,
                options,
                settings,
                art,
                body: Body::Missing,
            },
            plan,
            header,
            seed,
            cancel,
        )
        .await
    }

    /// Everything `route` lists, in the order and filter `options` ask, for playing, queueing or
    /// downloading it whole, without reading its rows or header. None when the route lists nothing
    /// or its collection is gone or empty.
    pub async fn selection(
        &self,
        route: &Route,
        selected: Option<&SelectedLibrary>,
        options: &OpenOptions,
        cancel: &ReadCancellation,
    ) -> LibraryResult<Option<MemberSelection>> {
        let settings = self.shared.settings.load().ui;
        let (header, plan) = self
            .plan(route, selected, options, &settings, cancel)
            .await?;
        Ok(Some(match plan {
            Plan::Rows(Read::Members(read)) | Plan::Folders { tracks: read, .. } => {
                // A tag page with no tracks reads as missing.
                if matches!(header, Some(HeaderKey::Genre(_) | HeaderKey::Mood(_)))
                    && self
                        .shared
                        .catalog
                        .order(&Read::Members(read.clone()), cancel)
                        .await?
                        .is_empty()
                {
                    return Ok(None);
                }
                read.selection()
            }
            Plan::Rows(Read::List(read) | Read::AlbumTracks(read))
            | Plan::Releases { read, .. } => read.selection(),
            _ => return Ok(None),
        }))
    }

    /// What `route` reads and the header it shows, from the resident Catalog: no rows.
    async fn plan(
        &self,
        route: &Route,
        selected: Option<&SelectedLibrary>,
        options: &OpenOptions,
        settings: &Settings,
        cancel: &ReadCancellation,
    ) -> LibraryResult<(Option<HeaderKey>, Plan)> {
        let catalog = &self.shared.catalog;
        tokio::select! {
            () = catalog.loaded() => {}
            () = cancel.cancelled() => return Err(LibraryError::ReadCancelled),
        }
        let (sort, descending, activity) = read_order(route, options, settings);
        let source = selected.map(|selected| selected.source_key);
        let require_source =
            || source.ok_or_else(|| LibraryError::InvalidRequest("No source selected".into()));
        let now = options.smart_opened.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs() as i64)
        });
        let member = |collection| member_read(route, selected, options, settings, collection);
        let list = |kind| {
            let mut read = ListRead::new(kind, source, sort);
            read.folder = selected.and_then(|selected| selected.music_folder_key);
            read.filter.clone_from(&options.filter);
            read.descending = descending;
            read.favorites_only = favorites_only(route, options);
            read.activity = activity;
            read
        };
        let playlists = |kind| {
            let mut read = list(kind);
            read.playlist_scope = match route {
                Route::PlaylistFolder { key, .. } => library::PlaylistScope::Folder { key: *key },
                _ => library::PlaylistScope::Root,
            };
            Plan::Rows(Read::List(read))
        };
        // Favorites lists the category it shows.
        let shown = match (route, options.category) {
            (Route::Favorites, CollectionCategory::Tracks) => Route::Tracks,
            (Route::Favorites, CollectionCategory::Albums) => Route::Albums,
            (Route::Favorites, CollectionCategory::Artists) => Route::Artists,
            (route, _) => route.clone(),
        };
        let (header, plan) = match &shown {
            Route::AlbumDetail(uri) => match catalog.item(uri) {
                Some(Item::Album(key)) => (
                    Some(HeaderKey::Album { key, activity }),
                    Plan::Rows(Read::Members(member(Collection::Album(key)))),
                ),
                _ => (None, Plan::Missing),
            },
            Route::ArtistDetail(uri)
            | Route::AlbumArtistDetail(uri)
            | Route::ArtistDiscography(uri)
            | Route::AlbumArtistDiscography(uri)
            | Route::ArtistTracks(uri)
            | Route::AlbumArtistTracks(uri)
            | Route::ArtistFavoriteTracks(uri)
            | Route::AlbumArtistFavoriteTracks(uri) => {
                let album_artist = matches!(
                    route,
                    Route::AlbumArtistDetail(_)
                        | Route::AlbumArtistDiscography(_)
                        | Route::AlbumArtistTracks(_)
                        | Route::AlbumArtistFavoriteTracks(_)
                );
                let artist = catalog.item(uri).and_then(|item| match item {
                    Item::Artist { key, .. } => Some(key),
                    _ => None,
                });
                let artist_source = library::source_entity_parts(uri)
                    .and_then(|(source, _, _)| catalog.source_key(&source));
                match artist.zip(artist_source) {
                    None => (None, Plan::Missing),
                    Some((key, artist_source)) => {
                        let tracks = member(Collection::Artist { key, album_artist });
                        let plan = if matches!(
                            route,
                            Route::ArtistDetail(_)
                                | Route::AlbumArtistDetail(_)
                                | Route::ArtistDiscography(_)
                                | Route::AlbumArtistDiscography(_)
                        ) {
                            let mut read = list(ListKind::ArtistAlbums {
                                artist: key,
                                album_artist,
                                release_class: None,
                                filters: None,
                            });
                            read.source = Some(artist_source);
                            let favorites = matches!(
                                route,
                                Route::ArtistDetail(_) | Route::AlbumArtistDetail(_)
                            )
                            .then(|| {
                                let list = settings.library_list(LibraryListKey::ArtistTracks);
                                let mut read = tracks;
                                read.sort = Some(list.sort_key);
                                read.descending = list.descending;
                                read.favorites_only = true;
                                read
                            });
                            Plan::Releases {
                                read: release_query(read, &options, &settings),
                                favorites,
                                kept: None,
                            }
                        } else {
                            Plan::Rows(Read::Members(tracks))
                        };
                        (
                            Some(HeaderKey::Artist {
                                key,
                                album_artist,
                                activity,
                            }),
                            plan,
                        )
                    }
                }
            }
            Route::PlaylistDetail(key) => (
                Some(HeaderKey::Playlist(*key)),
                Plan::Rows(Read::Members(member(Collection::Playlist(*key)))),
            ),
            Route::SmartPlaylistDetail(key) => (
                Some(HeaderKey::Smart { key: *key, now }),
                Plan::Rows(Read::Members(member(Collection::Smart { key: *key, now }))),
            ),
            Route::GenreDetail(key) => (
                Some(HeaderKey::Genre(*key)),
                Plan::Rows(Read::Members(member(Collection::Genre(*key)))),
            ),
            Route::MoodDetail(key) => (
                Some(HeaderKey::Mood(*key)),
                Plan::Rows(Read::Members(member(Collection::Mood(*key)))),
            ),
            Route::Tracks => (
                None,
                Plan::Rows(Read::Members(member(Collection::Source(require_source()?)))),
            ),
            Route::History => (
                None,
                Plan::Rows(Read::Members(member(Collection::History {
                    source: options.history_source.clone(),
                }))),
            ),
            Route::Albums => {
                require_source()?;
                let read = list(ListKind::Albums);
                let detail =
                    settings.library_list(LibraryListKey::Albums).layout == LibraryLayout::Detail;
                (
                    None,
                    Plan::Rows(if detail {
                        Read::AlbumTracks(read)
                    } else {
                        Read::List(read)
                    }),
                )
            }
            Route::Artists | Route::AlbumArtists | Route::Favorites => {
                require_source()?;
                let album_artist = shown == Route::AlbumArtists;
                (
                    None,
                    Plan::Rows(Read::List(list(ListKind::Artists { album_artist }))),
                )
            }
            Route::Genres => {
                require_source()?;
                (None, Plan::Rows(Read::List(list(ListKind::Genres))))
            }
            Route::Moods => {
                require_source()?;
                (None, Plan::Rows(Read::List(list(ListKind::Moods))))
            }
            Route::Playlists | Route::PlaylistFolder { smart: false, .. } => {
                (None, playlists(ListKind::Playlists))
            }
            Route::SmartPlaylists | Route::PlaylistFolder { smart: true, .. } => {
                (None, playlists(ListKind::SmartPlaylists { now }))
            }
            Route::Folders { path } => {
                let selected = selected
                    .ok_or_else(|| LibraryError::InvalidRequest("No source selected".into()))?;
                let projection = crate::source::folders::resolve_folder(
                    self,
                    catalog,
                    selected,
                    path.last().map(|item| item.id.as_str()),
                    cancel,
                )
                .await?;
                let tracks = folder_tracks(route, selected, &projection, options, settings);
                (None, Plan::Folders { projection, tracks })
            }
            Route::Home => {
                let (showcase, explore) = self.home_variations();
                let home = catalog
                    .home_page(
                        require_source()?,
                        selected.and_then(|selected| selected.music_folder_key),
                        showcase,
                        explore,
                        &settings.application.home_blocks,
                        cancel,
                    )
                    .await?;
                (None, Plan::Home(Box::new(home)))
            }
            // Search reads its results itself.
            Route::Search => (None, Plan::Missing),
        };
        Ok((header, plan))
    }
}

/// Reads `plan` into `view`: its orders, then one statement for its header and seed window.
/// A gone or empty collection reads as missing.
async fn read(
    mut view: View,
    plan: Plan,
    header: Option<HeaderKey>,
    seed: RouteSeedWindow,
    cancel: &ReadCancellation,
) -> LibraryResult<Opened> {
    let catalog = Arc::clone(&view.owner.shared.catalog);
    let named = header.is_some();
    let (header, first, rows, favorites) = match plan {
        Plan::Rows(read) => {
            let order = catalog.order(&read, cancel).await?;
            let range = seed.range(order.len());
            let fetched = catalog
                .fetch(
                    Fetch {
                        rows: Some((&order, range.clone())),
                        header,
                        art: view.art,
                        ..Fetch::default()
                    },
                    cancel,
                )
                .await?;
            let discs = match &read {
                Read::Members(MemberRead {
                    collection: Collection::Album(_),
                    ..
                }) => catalog.disc_sections(&order),
                _ => Vec::new(),
            };
            view.body = Body::Rows {
                len: order.len(),
                discs,
                spans: order.album_spans().cloned(),
                read,
            };
            (fetched.header, range.start, fetched.rows, None)
        }
        Plan::Releases {
            read,
            favorites,
            kept,
        } => {
            let mut unfiltered = read.clone();
            unfiltered.filter.clear();
            if let ListKind::ArtistAlbums { filters, .. } = &mut unfiltered.kind {
                *filters = None;
            }
            let source_counts = catalog
                .order(&Read::List(unfiltered), cancel)
                .await?
                .release_counts()
                .unwrap_or_default();
            let filtered = match &read.kind {
                ListKind::ArtistAlbums {
                    filters: Some(filters),
                    ..
                } => filters.iter().any(|filter| !filter.trim().is_empty()),
                _ => !read.filter.trim().is_empty(),
            };
            let order = catalog.order(&Read::List(read.clone()), cancel).await?;
            let counts = if filtered {
                order.release_counts().unwrap_or_default()
            } else {
                source_counts
            };
            let range = seed.range(counts.iter().sum());
            let favorite_order = match &favorites {
                Some(favorites) => Some(
                    catalog
                        .order(&Read::Members(favorites.clone()), cancel)
                        .await?,
                ),
                None => None,
            };
            let fetched = catalog
                .fetch(
                    Fetch {
                        rows: Some((&order, range.clone())),
                        favorites: favorite_order
                            .as_ref()
                            .map(|order| (order, RouteSeedWindow::top().range(order.len()))),
                        header,
                        art: view.art,
                    },
                    cancel,
                )
                .await?;
            view.body = Body::Releases {
                read,
                counts,
                source_counts,
                favorites: favorites
                    .zip(favorite_order.as_ref().map(library::Order::len))
                    .or(kept),
            };
            (
                fetched.header,
                range.start,
                fetched.rows,
                favorite_order.is_some().then_some(fetched.favorites),
            )
        }
        Plan::Folders { projection, tracks } => {
            let folders = match &projection {
                FolderProjection::Live { .. } => None,
                FolderProjection::Cached(parent) => Some(FolderRead {
                    source: view
                        .selected
                        .as_ref()
                        .map(|selected| selected.source_key)
                        .ok_or_else(|| LibraryError::InvalidRequest("No source selected".into()))?,
                    parent: *parent,
                    filter: tracks.filter.clone(),
                    descending: tracks.descending,
                }),
            };
            let folder_len = match &folders {
                Some(folders) => catalog
                    .order(&Read::Folders(folders.clone()), cancel)
                    .await?
                    .len(),
                None => projection
                    .live_folders(&tracks.filter, tracks.descending)
                    .count(),
            };
            let track_len = catalog
                .order(&Read::Members(tracks.clone()), cancel)
                .await?
                .len();
            view.body = Body::Folders {
                projection,
                folders,
                folder_len,
                tracks,
                track_len,
            };
            let range = seed.range(folder_len + track_len);
            let rows = view.rows(range.clone(), cancel).await?;
            (None, range.start, rows, None)
        }
        Plan::Home(home) => {
            view.body = Body::Home(home);
            (None, 0, Vec::new(), None)
        }
        Plan::Missing => (None, 0, Vec::new(), None),
    };
    let found = match &header {
        Some(Header::Tag(tag)) => tag.summary.track_count > 0,
        Some(_) => true,
        None => !named,
    };
    if !found {
        view.body = Body::Missing;
        return Ok(Opened {
            view: Arc::new(view),
            header,
            first: 0,
            rows: Vec::new(),
            favorites,
        });
    }
    // Smart Playlists keep the members they opened with when the page reads again.
    if let Body::Rows {
        read:
            Read::List(ListRead {
                kind: ListKind::SmartPlaylists { now },
                ..
            })
            | Read::Members(MemberRead {
                collection: Collection::Smart { now, .. },
                ..
            }),
        ..
    } = &view.body
    {
        view.options.smart_opened = Some(*now);
    }
    Ok(Opened {
        view: Arc::new(view),
        header,
        first,
        rows,
        favorites,
    })
}

impl View {
    pub fn route(&self) -> &Route {
        &self.route
    }

    pub fn selected(&self) -> Option<&SelectedLibrary> {
        self.selected.as_ref()
    }

    pub fn options(&self) -> &OpenOptions {
        &self.options
    }

    /// The list settings of the route, as the view read with them.
    pub fn list_settings(&self) -> LibraryListSettings {
        self.settings
            .library_list(library_list_key(&self.route, self.options.category))
    }

    /// How many rows it lists; folders count their child folders and then their tracks.
    pub fn len(&self) -> usize {
        match &self.body {
            Body::Rows { len, .. } => *len,
            Body::Releases { counts, .. } => counts.iter().sum(),
            Body::Folders {
                folder_len,
                track_len,
                ..
            } => folder_len + track_len,
            Body::Home(_) | Body::Missing => 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether the collection the route names is gone or empty.
    pub fn is_missing(&self) -> bool {
        matches!(self.body, Body::Missing)
    }

    pub fn home(&self) -> Option<&HomePage> {
        match &self.body {
            Body::Home(home) => Some(home),
            _ => None,
        }
    }

    /// How many child folders come before a folder's tracks.
    pub fn folder_len(&self) -> Option<usize> {
        match &self.body {
            Body::Folders { folder_len, .. } => Some(*folder_len),
            _ => None,
        }
    }

    /// Rows `range`, at most 256, 128 of an artist's releases. Folders give child folder rows,
    /// then track rows.
    pub async fn rows(
        &self,
        range: Range<usize>,
        cancel: &ReadCancellation,
    ) -> LibraryResult<Vec<Row>> {
        let catalog = self.catalog();
        match &self.body {
            Body::Rows { read, .. } => {
                let order = catalog.order(read, cancel).await?;
                catalog.rows(&order, range, self.art, cancel).await
            }
            Body::Releases { read, .. } => {
                let order = catalog.order(&Read::List(read.clone()), cancel).await?;
                catalog.rows(&order, range, self.art, cancel).await
            }
            Body::Folders {
                projection,
                folders,
                folder_len,
                tracks,
                ..
            } => {
                let mut rows = Vec::new();
                if range.start < *folder_len {
                    let end = range.end.min(*folder_len);
                    match folders {
                        Some(folders) => {
                            let order = catalog
                                .order(&Read::Folders(folders.clone()), cancel)
                                .await?;
                            rows = catalog
                                .rows(&order, range.start..end, self.art, cancel)
                                .await?;
                        }
                        None => rows.extend(
                            projection
                                .live_folders(&tracks.filter, tracks.descending)
                                .skip(range.start)
                                .take(end - range.start)
                                .map(|folder| {
                                    Row::Folder(FolderRow {
                                        folder_key: None,
                                        object_id: folder.object_id.clone(),
                                        name: folder.name.clone(),
                                        summary: Default::default(),
                                        artwork_binding: None,
                                    })
                                }),
                        ),
                    }
                }
                if range.end > *folder_len {
                    let order = catalog
                        .order(&Read::Members(tracks.clone()), cancel)
                        .await?;
                    let start = range.start.max(*folder_len) - folder_len;
                    rows.extend(
                        catalog
                            .rows(&order, start..range.end - folder_len, self.art, cancel)
                            .await?,
                    );
                }
                Ok(rows)
            }
            Body::Home(_) | Body::Missing => Ok(Vec::new()),
        }
    }

    /// An artist page's favorite tracks `range`.
    pub async fn favorite_rows(
        &self,
        range: Range<usize>,
        cancel: &ReadCancellation,
    ) -> LibraryResult<Vec<Row>> {
        let Body::Releases {
            favorites: Some((read, _)),
            ..
        } = &self.body
        else {
            return Ok(Vec::new());
        };
        let catalog = self.catalog();
        let order = catalog.order(&Read::Members(read.clone()), cancel).await?;
        catalog.rows(&order, range, self.art, cancel).await
    }

    /// The member read behind an artist page's favorite tracks, for selections and plays.
    pub fn favorites(&self) -> Option<&MemberRead> {
        match &self.body {
            Body::Releases {
                favorites: Some((read, _)),
                ..
            } => Some(read),
            _ => None,
        }
    }

    /// How many favorite tracks an artist page lists.
    pub fn favorites_len(&self) -> Option<usize> {
        match &self.body {
            Body::Releases {
                favorites: Some((_, len)),
                ..
            } => Some(*len),
            _ => None,
        }
    }

    /// Letter-index positions, for pages listed by name.
    pub async fn sections(&self, cancel: &ReadCancellation) -> LibraryResult<Vec<ScrollSection>> {
        let catalog = self.catalog();
        Ok(match &self.body {
            Body::Folders {
                projection,
                folders,
                folder_len,
                tracks,
                ..
            } => {
                let mut sections = match folders {
                    Some(folders) => catalog.sections(
                        &catalog
                            .order(&Read::Folders(folders.clone()), cancel)
                            .await?,
                    ),
                    None => {
                        catalog
                            .ordered_name_sections(
                                projection
                                    .live_folders(&tracks.filter, tracks.descending)
                                    .map(|folder| folder.name.as_str()),
                                cancel,
                            )
                            .await?
                    }
                };
                let order = catalog
                    .order(&Read::Members(tracks.clone()), cancel)
                    .await?;
                for mut section in catalog.sections(&order) {
                    section.index += *folder_len as u64;
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
                sections
            }
            _ if self.route == Route::History => Vec::new(),
            Body::Rows {
                read: Read::AlbumTracks(_),
                ..
            } => Vec::new(),
            Body::Rows { read, .. } => catalog.sections(&catalog.order(read, cancel).await?),
            _ => Vec::new(),
        })
    }

    /// Where each disc of an album page starts, when it has several.
    pub fn disc_sections(&self) -> &[(u32, i64)] {
        match &self.body {
            Body::Rows { discs, .. } => discs,
            _ => &[],
        }
    }

    /// An artist page's albums per release group: filtered, then unfiltered.
    pub fn release_counts(&self) -> Option<([usize; 6], [usize; 6])> {
        match &self.body {
            Body::Releases {
                counts,
                source_counts,
                ..
            } => Some((*counts, *source_counts)),
            _ => None,
        }
    }

    /// Where each album of the Albums list in Detail layout starts.
    pub fn album_spans(&self) -> Option<&Arc<[AlbumSpan]>> {
        match &self.body {
            Body::Rows { spans, .. } => spans.as_ref(),
            _ => None,
        }
    }

    /// The item at `index`, for anchors and plays by index. A live folder has none.
    pub async fn item(
        &self,
        index: usize,
        cancel: &ReadCancellation,
    ) -> LibraryResult<Option<Item>> {
        let catalog = self.catalog();
        let (read, index) = match &self.body {
            Body::Rows { read, .. } => (read.clone(), index),
            Body::Releases { read, .. } => (Read::List(read.clone()), index),
            Body::Folders {
                folders,
                folder_len,
                tracks,
                ..
            } => match (index < *folder_len, folders) {
                (true, Some(folders)) => (Read::Folders(folders.clone()), index),
                (true, None) => return Ok(None),
                (false, _) => (Read::Members(tracks.clone()), index - folder_len),
            },
            Body::Home(_) | Body::Missing => return Ok(None),
        };
        Ok(catalog
            .order(&read, cancel)
            .await?
            .items(index..index + 1)
            .pop())
    }

    /// The member read behind a track page, for selections and plays.
    pub fn members(&self) -> Option<&MemberRead> {
        match &self.body {
            Body::Rows {
                read: Read::Members(read),
                ..
            } => Some(read),
            Body::Folders { tracks, .. } => Some(tracks),
            _ => None,
        }
    }

    /// Everything the page lists, in the order and filter it shows, for playing or queueing it
    /// whole.
    pub fn selection(&self) -> Option<MemberSelection> {
        if let Some(read) = self.members() {
            return Some(read.clone().selection());
        }
        match &self.body {
            Body::Rows {
                read: Read::List(read) | Read::AlbumTracks(read),
                ..
            }
            | Body::Releases { read, .. } => Some(read.clone().selection()),
            _ => None,
        }
    }

    /// What `current` settings make the page read again. Named lists read again on every
    /// settings change.
    pub fn settings_reread(&self, current: &Settings) -> Option<Reread> {
        let lists = |key| (self.settings.library_list(key), current.library_list(key));
        let order = |key| {
            let (read, current) = lists(key);
            read.sort_key != current.sort_key || read.descending != current.descending
        };
        let changed = match &self.body {
            Body::Rows {
                read:
                    Read::AlbumTracks(_)
                    | Read::List(ListRead {
                        kind: ListKind::Albums,
                        ..
                    }),
                ..
            } => {
                let (read, current) = lists(LibraryListKey::Albums);
                (read.layout == LibraryLayout::Detail) != (current.layout == LibraryLayout::Detail)
                    || read.sort_key != current.sort_key
                    || read.descending != current.descending
                    || uses_activity(&Route::Albums, &read)
                        != uses_activity(&Route::Albums, &current)
            }
            Body::Rows {
                read: Read::List(_),
                ..
            } => true,
            _ if matches!(
                self.route,
                Route::ArtistDetail(_) | Route::AlbumArtistDetail(_)
            ) =>
            {
                if order(LibraryListKey::ArtistTracks) {
                    return Some(Reread::Page);
                }
                return order(LibraryListKey::ArtistAlbums).then_some(Reread::Releases);
            }
            _ => order(library_list_key(&self.route, self.options.category)),
        };
        changed.then_some(Reread::Page)
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
        match &self.body {
            Body::Rows {
                read: Read::Members(read),
                ..
            } => read.favorites_only || read.sort == Some(LibraryField::Favorite),
            Body::Rows {
                read: Read::List(read) | Read::AlbumTracks(read),
                ..
            } => read.favorites_only || read.sort == LibraryField::Favorite,
            _ => false,
        }
    }

    /// Reads the page again with `options` and current settings. A folder keeps the folder it
    /// opened, so new child folders show when the route opens again. Album, artist track, genre
    /// and mood pages and Discography read their rows again and keep their header, so a
    /// collection that is gone reads as empty.
    pub async fn reread(
        &self,
        options: OpenOptions,
        seed: RouteSeedWindow,
        cancel: &ReadCancellation,
    ) -> LibraryResult<Opened> {
        let settings = self.owner.shared.settings.load().ui;
        match (&self.body, &self.selected) {
            (Body::Folders { projection, .. }, Some(selected)) => {
                let tracks = folder_tracks(&self.route, selected, projection, &options, &settings);
                let plan = Plan::Folders {
                    projection: projection.clone(),
                    tracks,
                };
                read(self.again(options, settings), plan, None, seed, cancel).await
            }
            (
                Body::Rows {
                    read: Read::Members(members),
                    ..
                },
                _,
            ) if matches!(
                self.route,
                Route::AlbumDetail(_)
                    | Route::ArtistTracks(_)
                    | Route::AlbumArtistTracks(_)
                    | Route::ArtistFavoriteTracks(_)
                    | Route::AlbumArtistFavoriteTracks(_)
                    | Route::GenreDetail(_)
                    | Route::MoodDetail(_)
            ) =>
            {
                let members = member_read(
                    &self.route,
                    self.selected.as_ref(),
                    &options,
                    &settings,
                    members.collection.clone(),
                );
                let plan = Plan::Rows(Read::Members(members));
                read(self.again(options, settings), plan, None, seed, cancel).await
            }
            (Body::Releases { .. }, _)
                if matches!(
                    self.route,
                    Route::ArtistDiscography(_) | Route::AlbumArtistDiscography(_)
                ) =>
            {
                self.releases(options, settings, seed, cancel).await
            }
            _ => {
                self.owner
                    .open(
                        self.route.clone(),
                        self.selected.clone(),
                        options,
                        seed,
                        self.art,
                        cancel,
                    )
                    .await
            }
        }
    }

    /// Reads an artist page's releases again with `options` and current settings. The header
    /// and favorite tracks are not read again; a page whose track order moved since it read
    /// reads again whole.
    pub async fn reread_releases(
        &self,
        options: OpenOptions,
        seed: RouteSeedWindow,
        cancel: &ReadCancellation,
    ) -> LibraryResult<Opened> {
        let settings = self.owner.shared.settings.load().ui;
        if matches!(self.body, Body::Releases { .. })
            && self.settings_reread(&settings) != Some(Reread::Page)
        {
            return self.releases(options, settings, seed, cancel).await;
        }
        self.reread(options, seed, cancel).await
    }

    async fn releases(
        &self,
        options: OpenOptions,
        settings: Settings,
        seed: RouteSeedWindow,
        cancel: &ReadCancellation,
    ) -> LibraryResult<Opened> {
        let Body::Releases {
            read: releases,
            favorites,
            ..
        } = &self.body
        else {
            unreachable!("releases reread an artist page");
        };
        let releases = release_query(releases.clone(), &options, &settings);
        read(
            self.again(options, settings),
            Plan::Releases {
                read: releases,
                favorites: None,
                kept: favorites.clone(),
            },
            None,
            seed,
            cancel,
        )
        .await
    }

    /// This view's route with new options and settings, before its body is read.
    fn again(&self, options: OpenOptions, settings: Settings) -> View {
        View {
            owner: self.owner.clone(),
            route: self.route.clone(),
            selected: self.selected.clone(),
            options,
            settings,
            art: self.art,
            body: Body::Missing,
        }
    }

    fn catalog(&self) -> &Catalog {
        &self.owner.shared.catalog
    }
}

/// What a committed change means for a page open on `route` with `options`, opened while
/// `opened_with` was selected.
pub fn refresh(
    owner: &SourceOwner,
    route: &Route,
    opened_with: Option<&SelectedLibrary>,
    options: &OpenOptions,
    changed: &Changed,
) -> Refresh {
    let current = owner.shared.selected();
    let selected = current.as_ref().map(|selected| selected.source_key);
    let affects = |source: Option<SourceKey>| source.is_none() || source == selected;
    let uses_source = route.uses_selected_source()
        && (route != &Route::Search || options.search_mode == library::SearchMode::Title);
    let unaffected = |source| !affects(source) && route.requires_selected_source() && uses_source;
    let scope = |selected: &SelectedLibrary| {
        (
            selected.source_key,
            selected.music_folder_key,
            selected.music_folder_object_id.clone(),
        )
    };
    let mut refresh = Refresh::Nothing;
    // A rating shows on every page of the source, so it reads like a catalog change.
    let catalog = changed
        .all
        .then_some(None)
        .into_iter()
        .chain(changed.sources.iter().map(|source| Some(*source)))
        .chain(
            changed
                .items
                .iter()
                .any(|item| item.rating)
                .then_some(selected),
        );
    for source in catalog {
        refresh = refresh.max(if unaffected(source) {
            Refresh::Nothing
        } else if uses_source && opened_with.map(scope) != current.as_ref().map(scope) {
            Refresh::Reopen
        } else {
            Refresh::Reread
        });
    }
    if changed.listened {
        let settings = owner.shared.settings.load();
        let categories: &[CollectionCategory] = match route {
            Route::Favorites => &[options.category],
            Route::ArtistDetail(_) | Route::AlbumArtistDetail(_) => {
                &[CollectionCategory::Tracks, CollectionCategory::Albums]
            }
            _ => &[CollectionCategory::Tracks],
        };
        let sensitive = options.sort.is_some_and(LibraryField::activity_sensitive)
            || categories.iter().any(|category| {
                uses_activity(
                    route,
                    &settings.ui.library_list(library_list_key(route, *category)),
                )
            });
        if sensitive {
            refresh = refresh.max(Refresh::Reread);
        }
    }
    if changed
        .scope
        .is_some_and(|source| affects(Some(source)) && uses_source)
    {
        refresh = Refresh::Reopen;
    }
    if route == &Route::Home && changed.home.iter().any(|source| affects(Some(*source))) {
        refresh = refresh.max(Refresh::Reread);
    }
    // The playlist lists changing reads like every playlist changing.
    let playlist = |playlist: Option<library::PlaylistKey>| {
        matches!(
            route,
            Route::Playlists | Route::SmartPlaylists | Route::PlaylistFolder { .. }
        ) || (playlist.is_none() && matches!(route, Route::SmartPlaylistDetail(_)))
            || matches!(route, Route::PlaylistDetail(key) if playlist.is_none_or(|playlist| playlist == *key))
    };
    if changed
        .playlist_lists
        .then_some(None)
        .into_iter()
        .chain(changed.playlists.iter().map(|key| Some(*key)))
        .any(playlist)
    {
        refresh = refresh.max(Refresh::Reread);
    }
    refresh
}

/// Whether a committed change can change the sidebar pins.
pub fn pins_refresh(owner: &SourceOwner, changed: &Changed) -> bool {
    changed.all
        || changed.pins
        || changed.playlist_lists
        || !changed.sources.is_empty()
        || !changed.playlists.is_empty()
        || changed.items.iter().any(|item| item.rating)
        || changed.listened && owner.shared.settings.load().ui.sidebar.pins_use_activity()
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

/// Whether plays change what an open route shows. An open Smart Playlist keeps the list it
/// opened with; plays change its members on the next visit.
fn uses_activity(route: &Route, settings: &LibraryListSettings) -> bool {
    if matches!(route, Route::SmartPlaylistDetail(_)) {
        return false;
    }
    matches!(
        route,
        Route::Home
            | Route::SmartPlaylists
            | Route::PlaylistFolder { smart: true, .. }
            | Route::History
    ) || settings.sort_key.activity_sensitive()
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
        .any(|field| field.activity_sensitive())
}

/// The order a route reads in, and whether it reads activity.
fn read_order(
    route: &Route,
    options: &OpenOptions,
    settings: &Settings,
) -> (LibraryField, bool, bool) {
    let display = settings.library_list(library_list_key(route, options.category));
    let sort = options.sort.unwrap_or(display.sort_key);
    let descending = options.descending.unwrap_or(display.descending);
    (
        sort,
        descending,
        uses_activity(route, &display) || sort.activity_sensitive(),
    )
}

fn favorites_only(route: &Route, options: &OpenOptions) -> bool {
    options.favorites_only && supports_favorite_filter(route, options.category)
        || matches!(
            route,
            Route::Favorites | Route::ArtistFavoriteTracks(_) | Route::AlbumArtistFavoriteTracks(_)
        )
}

/// How `route` reads the members of `collection`.
fn member_read(
    route: &Route,
    selected: Option<&SelectedLibrary>,
    options: &OpenOptions,
    settings: &Settings,
    collection: Collection,
) -> MemberRead {
    let (sort, descending, activity) = read_order(route, options, settings);
    let mut read = MemberRead::new(collection);
    read.sort = Some(sort);
    read.descending = descending;
    read.folder = selected.and_then(|selected| selected.music_folder_key);
    read.filter.clone_from(&options.filter);
    read.favorites_only = favorites_only(route, options);
    read.downloaded_only =
        options.downloaded_only && supports_downloaded_filter(route, options.category);
    read.activity = activity;
    read
}

/// The tracks of a resolved folder.
fn folder_tracks(
    route: &Route,
    selected: &SelectedLibrary,
    projection: &FolderProjection,
    options: &OpenOptions,
    settings: &Settings,
) -> MemberRead {
    let (sort, descending, activity) = read_order(route, options, settings);
    let mut read = projection.members(selected.source_key);
    read.sort = Some(sort);
    read.descending = descending;
    read.filter.clone_from(&options.filter);
    read.activity = activity;
    read
}

/// An artist's releases, searched and ordered as `options` and the release list settings ask.
fn release_query(mut query: ListRead, options: &OpenOptions, settings: &Settings) -> ListRead {
    if let ListKind::ArtistAlbums { filters, .. } = &mut query.kind {
        *filters = options
            .release_filters
            .as_ref()
            .map(|filters| Arc::new(std::array::from_fn(|index| filters[index].clone())));
    }
    if options.sort.is_none() {
        let albums = settings.library_list(LibraryListKey::ArtistAlbums);
        query.sort = albums.sort_key;
        query.descending = options.descending.unwrap_or(albums.descending);
    }
    query
}
