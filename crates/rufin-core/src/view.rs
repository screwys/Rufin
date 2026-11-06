//! An open route: the read behind it, its rows on demand, and what a change means for it.
//! GTK, Android and the web draw it their own way. A view holds its reads and counts; orders
//! live in the Catalog's cache, so every window reads the current one.

use std::ops::Range;
use std::sync::Arc;

use library::{
    AlbumSpan, Catalog, Collection, Fetch, FolderRead, FolderRow, Header, HeaderKey, HomeAlbumRow,
    HomeBlockKind, HomePage, HomeProviderSection, HomeSectionRows, HomeTrackRow, Item,
    LibraryError, LibraryResult, ListKind, ListRead, MemberRead, MemberSelection, Read,
    ReadCancellation, RouteSeedWindow, Row, ScrollSection, SearchResults, SourceKey, TagRow,
};

use crate::playback::{PlayRequest, QueuePlacement};
use crate::route::{CollectionCategory, Route};
use crate::runtime::SelectedLibrary;
use crate::settings::app::Settings;
use crate::settings::{LibraryField, LibraryLayout, LibraryListKey, LibraryListSettings};
use crate::source::SourceOwner;
use crate::source::feed::Changed;
use crate::source::folders::FolderProjection;

/// What a committed change means for an open page, weakest first.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
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

/// How a route opens. Its serde form, with the route, names a web page.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
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
    /// The layout the page shows when not the saved one: the web shows lists as cards.
    pub layout: Option<LibraryLayout>,
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
            layout: None,
        }
    }
}

pub struct View {
    owner: SourceOwner,
    route: Route,
    selected: Option<SelectedLibrary>,
    options: OpenOptions,
    /// The settings the view read with.
    settings: crate::settings::Committed,
    art: bool,
    /// The header it shows; a reread that keeps the header keeps it.
    header: Option<HeaderKey>,
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
    /// What a search found, each kind in its list's sort.
    Search(Box<SearchResults>),
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
    Search(Box<SearchResults>),
    Missing,
}

/// The most rows one Catalog read returns.
const ROWS_WINDOW: usize = 256;

/// What of the selected library a page reads, by the one music folder rule (decisions.md,
/// 2026-11-06), as Feishin: clients pass the selection and core applies it by route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Neither the source nor its music folder: album and playlist pages, which name their
    /// members, and the play history.
    Whole,
    /// The source: the playlist lists, which a music folder does not filter.
    Source,
    /// The source and its music folder, which narrows only that source's tracks.
    Folder,
}

impl Scope {
    /// A collection page reads in the scope [`library::CatalogKind::follows_folder`] gives its
    /// kind; every list, Home and Search read in the music folder.
    pub fn of(route: &Route) -> Self {
        use library::CatalogKind as Kind;
        let kind = match route {
            Route::History => return Self::Whole,
            Route::Playlists | Route::PlaylistFolder { smart: false, .. } => return Self::Source,
            Route::AlbumDetail(_) => Kind::Album,
            Route::PlaylistDetail(_) => Kind::Playlist,
            Route::SmartPlaylistDetail(_) => Kind::SmartPlaylist,
            Route::GenreDetail(_) => Kind::Genre,
            Route::MoodDetail(_) => Kind::Mood,
            Route::ArtistDetail(_)
            | Route::AlbumArtistDetail(_)
            | Route::ArtistDiscography(_)
            | Route::AlbumArtistDiscography(_)
            | Route::ArtistTracks(_)
            | Route::AlbumArtistTracks(_)
            | Route::ArtistFavoriteTracks(_)
            | Route::AlbumArtistFavoriteTracks(_) => Kind::Artist,
            _ => return Self::Folder,
        };
        if kind.follows_folder() {
            Self::Folder
        } else {
            Self::Whole
        }
    }

    /// `selected` as a page in this scope reads it.
    pub fn apply(self, selected: Option<&SelectedLibrary>) -> Option<SelectedLibrary> {
        match self {
            Self::Whole => None,
            Self::Source => selected.map(|selected| SelectedLibrary {
                music_folder_key: None,
                music_folder_object_id: None,
                ..selected.clone()
            }),
            Self::Folder => selected.cloned(),
        }
    }

    /// The music folder a page in this scope reads in `selected`.
    pub fn folder(self, selected: Option<&SelectedLibrary>) -> Option<library::FolderKey> {
        selected
            .and_then(|selected| selected.music_folder_key)
            .filter(|_| self == Self::Folder)
    }

    /// Whether a page in this scope reads the same in library `a` as in `b`.
    pub fn same(self, a: Option<&SelectedLibrary>, b: Option<&SelectedLibrary>) -> bool {
        self == Self::Whole || SelectedLibrary::same(a, b, self == Self::Folder)
    }
}

/// What a route's tracks are taken for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionScope {
    /// Queueing it or adding it to a playlist: what its page lists, as Play queues it.
    Page,
    /// Downloading it or removing its downloads: whole collections, except a smart playlist's
    /// members and a folder's tracks, in the music folder they are read in (decided
    /// 2026-11-05).
    Downloads,
}

impl SelectionScope {
    /// Whether it takes the tracks of a page on `route` in the music folder the page reads.
    fn keeps_folder(self, route: &Route) -> bool {
        self == Self::Page
            || matches!(
                route,
                Route::Folders { .. }
                    | Route::SmartPlaylists
                    | Route::PlaylistFolder { smart: true, .. }
                    | Route::SmartPlaylistDetail(_)
            )
    }
}

/// What activating a row outside a page plays, by the one rule (decisions.md, "What a row
/// plays is one rule", 2026-11-06): a collection row plays its page whole, as that page's header
/// Play does; a track plays only itself. A track row on a page plays through [`View::play`].
#[derive(Clone, Debug, PartialEq)]
pub enum PlayTarget {
    Route(Route),
    Track(library::TrackKey),
}

impl PlayTarget {
    /// What `row` plays outside a page; None for a playlist folder, which its menu offers no
    /// Play for, and for a child folder, which plays what its page's
    /// [`View::row_target`] gives.
    pub fn of_row(row: &Row) -> Option<Self> {
        match row {
            Row::Track(track) | Row::AlbumTrack { track, .. } => Some(Self::of_track(track)),
            Row::Album(album) | Row::AlbumHeading(album) => Some(Self::of_album(album)),
            Row::Artist(artist) => Some(Self::of_artist(artist)),
            Row::Tag(tag) => Some(Self::of_tag(tag)),
            Row::Playlist(playlist) => Self::of_playlist(playlist),
            Row::Folder(_) => None,
        }
    }

    /// Whether `row` offers Play: every row [`PlayTarget::of_row`] gives a target, and a child
    /// folder, which plays what its page's [`View::row_target`] gives.
    pub fn plays(row: &Row) -> bool {
        matches!(row, Row::Folder(_)) || Self::of_row(row).is_some()
    }

    /// What a track row plays outside a page: only itself.
    pub fn of_track(track: &library::TrackRow) -> Self {
        Self::Track(track.track_key)
    }

    /// What an album row plays: its page.
    pub fn of_album(album: &library::AlbumRow) -> Self {
        Self::Route(Route::of_album(album))
    }

    /// What an artist row plays: its page, in the role it was read with.
    pub fn of_artist(artist: &library::ArtistRow) -> Self {
        Self::Route(Route::of_artist(artist))
    }

    /// What a genre or mood row plays: its page.
    pub fn of_tag(tag: &library::TagRow) -> Self {
        Self::Route(Route::of_tag(tag))
    }

    /// What a playlist or smart playlist row plays: its page; None for a playlist folder.
    pub fn of_playlist(row: &library::PlaylistListRow) -> Option<Self> {
        match row {
            library::PlaylistListRow::Folder(_) => None,
            row => Some(Self::Route(Route::of_playlist(row))),
        }
    }

    /// The context id its plays carry before a page's filter and order; None for a track.
    pub fn context(&self) -> Option<String> {
        match self {
            Self::Route(route) => Some(context_base(route)),
            Self::Track(_) => None,
        }
    }
}

impl SourceOwner {
    /// What playing `target` at `placement` queues, read in `selected`. None when its page
    /// lists nothing.
    pub async fn play_target(
        &self,
        target: &PlayTarget,
        selected: Option<&SelectedLibrary>,
        placement: QueuePlacement,
        shuffled: bool,
        cancel: &ReadCancellation,
    ) -> LibraryResult<Option<PlayRequest>> {
        match target {
            PlayTarget::Track(key) => {
                Ok(Some(PlayRequest::track(*key, placement).shuffled(shuffled)))
            }
            PlayTarget::Route(route) => {
                self.play_route(
                    route,
                    selected,
                    &OpenOptions::default(),
                    placement,
                    shuffled,
                    cancel,
                )
                .await
            }
        }
    }

    /// What playing the track, album or artist `uri` outside a page queues, read in `selected`:
    /// a remembered search result or a listening overview row. An album or artist plays its
    /// page whole; a track plays alone.
    pub async fn play_uri(
        &self,
        uri: &str,
        selected: Option<&SelectedLibrary>,
        placement: QueuePlacement,
        shuffled: bool,
        cancel: &ReadCancellation,
    ) -> LibraryResult<Option<PlayRequest>> {
        match Route::of_uri(uri) {
            Some(route) => {
                self.play_target(
                    &PlayTarget::Route(route),
                    selected,
                    placement,
                    shuffled,
                    cancel,
                )
                .await
            }
            None => Ok(Some(
                PlayRequest::uris(vec![uri.to_owned()], placement).shuffled(shuffled),
            )),
        }
    }

    /// The tracks `target` holds for `scope`, read in `selected`. None when it holds nothing.
    pub async fn target_selection(
        &self,
        target: &PlayTarget,
        selected: Option<&SelectedLibrary>,
        scope: SelectionScope,
        cancel: &ReadCancellation,
    ) -> LibraryResult<Option<MemberSelection>> {
        match target {
            PlayTarget::Track(key) => Ok(Some(MemberSelection::Keys {
                source: None,
                keys: vec![*key].into(),
            })),
            PlayTarget::Route(route) => self.route_selection(route, selected, scope, cancel).await,
        }
    }

    pub async fn open(
        &self,
        route: Route,
        selected: Option<SelectedLibrary>,
        options: OpenOptions,
        seed: RouteSeedWindow,
        art: bool,
        cancel: &ReadCancellation,
    ) -> LibraryResult<Opened> {
        let settings = self.shared.settings.committed();
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
                header: None,
                body: Body::Missing,
            },
            plan,
            header,
            seed,
            cancel,
        )
        .await
    }

    /// Everything `route` holds for `scope`, read in `selected`. None when the route lists
    /// nothing.
    pub async fn route_selection(
        &self,
        route: &Route,
        selected: Option<&SelectedLibrary>,
        scope: SelectionScope,
        cancel: &ReadCancellation,
    ) -> LibraryResult<Option<MemberSelection>> {
        let mut selected = selected.cloned();
        if !scope.keeps_folder(route)
            && let Some(selected) = &mut selected
        {
            selected.music_folder_key = None;
        }
        self.selection(route, selected.as_ref(), &OpenOptions::default(), cancel)
            .await
    }

    /// The `ranges` of a list's rows for `scope`, each row as route_selection reads its page.
    pub fn list_selection(
        &self,
        read: ListRead,
        ranges: Vec<Range<usize>>,
        scope: SelectionScope,
    ) -> MemberSelection {
        rows_selection(read, Some(ranges), &self.shared.settings.committed(), scope)
    }

    /// What each of the `ranges` of a list's rows plays, by [`PlayTarget::of_row`]: a playlist
    /// folder plays nothing.
    pub async fn list_targets(
        &self,
        read: ListRead,
        ranges: &[Range<usize>],
        cancel: &ReadCancellation,
    ) -> LibraryResult<Vec<PlayTarget>> {
        let catalog = &self.shared.catalog;
        let order = catalog.order(&Read::List(read), cancel).await?;
        let mut targets = Vec::new();
        for range in ranges {
            for start in range.clone().step_by(ROWS_WINDOW) {
                let end = (start + ROWS_WINDOW).min(range.end);
                let rows = catalog.rows(&order, start..end, false, cancel).await?;
                targets.extend(rows.iter().filter_map(PlayTarget::of_row));
            }
        }
        Ok(targets)
    }

    /// Everything `route` lists, in the order and filter `options` ask, without reading its rows
    /// or header. None when the route lists nothing or its collection is gone or empty.
    async fn selection(
        &self,
        route: &Route,
        selected: Option<&SelectedLibrary>,
        options: &OpenOptions,
        cancel: &ReadCancellation,
    ) -> LibraryResult<Option<MemberSelection>> {
        let settings = self.shared.settings.committed();
        let (header, plan) = self
            .plan(route, selected, options, &settings, cancel)
            .await?;
        // An album, artist or tag page with no tracks reads as missing, as its header counts
        // them.
        let empty = |folder| {
            header.as_ref().is_some_and(|header| {
                !matches!(header, HeaderKey::Playlist(_) | HeaderKey::Smart { .. })
                    && self.shared.catalog.track_count(header, folder) == 0
            })
        };
        Ok(Some(match plan {
            Plan::Rows(Read::Members(read)) | Plan::Folders { tracks: read, .. } => {
                if empty(read.folder) {
                    return Ok(None);
                }
                read.selection()
            }
            Plan::Rows(Read::List(read) | Read::AlbumTracks(read)) => {
                rows_selection(read, None, &settings, SelectionScope::Page)
            }
            Plan::Releases { read, .. } => match artist_tracks(route, &read, &settings) {
                Some(_) if empty(read.folder) => return Ok(None),
                Some(tracks) => tracks.selection(),
                None => return Ok(None),
            },
            _ => return Ok(None),
        }))
    }

    /// What playing `route` whole queues, as its page's header Play does: everything it lists
    /// in its saved list sort, or the order and filter `options` ask, from a random track when
    /// shuffled. A collection row's card, pin and menu Play play the page the row opens. None
    /// when the route lists nothing or its collection is gone or empty.
    pub async fn play_route(
        &self,
        route: &Route,
        selected: Option<&SelectedLibrary>,
        options: &OpenOptions,
        placement: QueuePlacement,
        shuffled: bool,
        cancel: &ReadCancellation,
    ) -> LibraryResult<Option<PlayRequest>> {
        let Some(selection) = self.selection(route, selected, options, cancel).await? else {
            return Ok(None);
        };
        let (sort, descending, _) = read_order(route, options, &self.shared.settings.committed());
        Ok(Some(play_whole(
            selection,
            context_id(route, selected, options, sort, descending).into(),
            placement,
            shuffled,
        )))
    }

    /// Fetches the album or artist `route` shows whole first when a live search admitted only
    /// part of it. Offline it shows that part.
    async fn complete_collection(&self, route: &Route) {
        let (Route::AlbumDetail(uri)
        | Route::ArtistDetail(uri)
        | Route::AlbumArtistDetail(uri)
        | Route::ArtistDiscography(uri)
        | Route::AlbumArtistDiscography(uri)
        | Route::ArtistTracks(uri)
        | Route::AlbumArtistTracks(uri)
        | Route::ArtistFavoriteTracks(uri)
        | Route::AlbumArtistFavoriteTracks(uri)) = route
        else {
            return;
        };
        let Some(source) = library::source_entity_parts(uri)
            .and_then(|(source_id, _, _)| self.cached_client(&source_id))
        else {
            return;
        };
        if let Err(error) = source.complete_collection(&self.shared.catalog, uri).await {
            tracing::warn!(%error, "could not fetch the collection");
        }
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
        self.complete_collection(route).await;
        let (sort, descending, activity) = read_order(route, options, settings);
        let selected = Scope::of(route).apply(selected);
        let selected = selected.as_ref();
        let source = selected.map(|selected| selected.source_key);
        let require_source =
            || source.ok_or_else(|| LibraryError::InvalidRequest("No source selected".into()));
        let now = options.smart_opened.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs() as i64)
        });
        let folder = selected.and_then(|selected| selected.music_folder_key);
        let member = |collection| member_read(route, folder, options, settings, collection);
        let list = |kind| {
            let mut read = ListRead::new(kind, source, sort);
            read.folder = folder;
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
                            // The favorite tracks keep the page's search; its order is the
                            // releases'.
                            let favorites = matches!(
                                route,
                                Route::ArtistDetail(_) | Route::AlbumArtistDetail(_)
                            )
                            .then(|| {
                                let page = if album_artist {
                                    Route::AlbumArtistFavoriteTracks(uri.clone())
                                } else {
                                    Route::ArtistFavoriteTracks(uri.clone())
                                };
                                let options = OpenOptions {
                                    filter: options.filter.clone(),
                                    ..OpenOptions::default()
                                };
                                member_read(&page, folder, &options, settings, tracks.collection)
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
                let detail = options
                    .layout
                    .unwrap_or(settings.library_list(LibraryListKey::Albums).layout)
                    == LibraryLayout::Detail;
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
            Route::Search => {
                let mut results = crate::source::search::search(
                    self,
                    selected,
                    options.search_mode,
                    options.filter.clone(),
                    cancel,
                )
                .await?;
                let folder = selected.and_then(|selected| selected.music_folder_key);
                catalog.sort_search(&mut results, folder, search_sorts(settings));
                (None, Plan::Search(Box::new(results)))
            }
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
    view.header = header.or(view.header);
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
        Plan::Search(results) => {
            view.body = Body::Search(results);
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
    // A smart playlist keeps the members it opened with when the page reads again; the Smart
    // Playlists list counts as of each read.
    if let Body::Rows {
        read:
            Read::Members(MemberRead {
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

    /// The source owner it reads from.
    pub fn owner(&self) -> &SourceOwner {
        &self.owner
    }

    pub fn selected(&self) -> Option<&SelectedLibrary> {
        self.selected.as_ref()
    }

    pub fn options(&self) -> &OpenOptions {
        &self.options
    }

    /// Whether this view lists what `previous` listed: read again for a committed change,
    /// not for another search, filter, order or list setting.
    pub fn same_list(&self, previous: &View) -> bool {
        self.route == previous.route
            && self.options == previous.options
            && previous.settings_reread(&self.settings).is_none()
    }

    /// The context id a play of this page carries, in the order it reads; `result` ties it to
    /// the order a client shows, as `|result=`.
    fn play_context(&self, result: Option<&str>) -> Arc<str> {
        with_result(self.context_id(), result)
    }

    /// The context id a play of an artist page's favorite tracks carries, in their order.
    pub fn favorites_context_id(&self) -> Option<String> {
        let read = self.favorites()?;
        let album_artist = matches!(self.route, Route::AlbumArtistDetail(_));
        let uri = match &self.route {
            Route::ArtistDetail(uri) | Route::AlbumArtistDetail(uri) => uri.clone(),
            _ => return None,
        };
        Some(format!(
            "{}|favorites|query={}|sort={:?}|descending={}",
            crate::collection_context::CollectionTarget::Artist {
                id: uri,
                album_artist,
            }
            .context_id(),
            read.filter,
            read.sort,
            read.descending,
        ))
    }

    /// The context id a play of this page carries, in the order it reads.
    pub fn context_id(&self) -> String {
        let (sort, descending, _) = read_order(&self.route, &self.options, &self.settings);
        context_id(
            &self.route,
            self.selected.as_ref(),
            &self.options,
            sort,
            descending,
        )
    }

    /// The order it reads in: the field and whether it is descending.
    pub fn order(&self) -> (LibraryField, bool) {
        let (sort, descending, _) = read_order(&self.route, &self.options, &self.settings);
        (sort, descending)
    }

    /// Whether plays change what it shows.
    pub fn reads_activity(&self) -> bool {
        read_order(&self.route, &self.options, &self.settings).2
    }

    /// Whether it reads the same in `selected` as in the library it opened in.
    pub fn reads_same_in(&self, selected: Option<&SelectedLibrary>) -> bool {
        Scope::of(&self.route).same(self.selected.as_ref(), selected)
    }

    /// What a committed change means for this page now that the client shows `current`, the
    /// selection it passes: the runtime's on GTK and Android, the page's own on the web. A page
    /// that reads another source or music folder in it opens again. Favorites, ratings and
    /// downloads read it again only where they order or filter its rows; the rows and header
    /// that show them show them through [`crate::shown::ShownChanges`].
    pub fn refresh(&self, current: Option<&SelectedLibrary>, changed: &Changed) -> Refresh {
        let refresh = changes(
            &self.owner,
            &self.route,
            self.selected.as_ref(),
            current,
            &self.options,
            changed,
        );
        if !changed.items.is_empty() && self.states_reorder() {
            return refresh.max(Refresh::Reread);
        }
        if !changed.downloads.is_empty() && self.members().is_some_and(|read| read.downloaded_only)
        {
            return refresh.max(Refresh::Reread);
        }
        refresh
    }

    /// When and in which music folder its rows count their totals and downloads: smart
    /// playlists as of when the Smart Playlists list was read, or the Smart Playlist page
    /// opened; every row in the music folder the page reads.
    pub(crate) fn counted(&self) -> (Option<i64>, Option<library::FolderKey>) {
        let now = match &self.body {
            Body::Rows {
                read:
                    Read::List(ListRead {
                        kind: ListKind::SmartPlaylists { now },
                        ..
                    }),
                ..
            } => Some(*now),
            _ => self.options.smart_opened,
        };
        (now, self.music_folder())
    }

    /// What playing the page whole queues, as its header Play does: everything it lists in
    /// its order and filter, from a random track when shuffle is on; an artist page plays the
    /// artist's tracks. None when it lists no tracks of its own.
    pub fn play_all(
        &self,
        placement: QueuePlacement,
        shuffled: bool,
        result: Option<&str>,
    ) -> Option<PlayRequest> {
        let selection = match &self.body {
            Body::Releases { read, .. } => {
                artist_tracks(&self.route, read, &self.settings)?.selection()
            }
            _ => self.selection()?,
        };
        Some(play_whole(
            selection,
            self.play_context(result),
            placement,
            shuffled,
        ))
    }

    /// What playing the page from its track `key` at `index` among its tracks queues; `entry`
    /// names a playlist entry. Play Next and Play Later queue only that track, and so does a
    /// track on Home or Search, which list rows outside a page.
    pub fn play_track(
        &self,
        key: library::TrackKey,
        entry: Option<library::PlaylistEntryKey>,
        index: usize,
        placement: QueuePlacement,
        shuffled: bool,
        result: Option<&str>,
    ) -> Option<PlayRequest> {
        let page = self
            .members()
            .filter(|_| matches!(placement, QueuePlacement::Replace { .. }));
        let Some(page) = page else {
            return Some(PlayRequest::track(key, placement).shuffled(shuffled));
        };
        Some(
            PlayRequest::captured(page.clone().selection(), index, placement, false)
                .with_context(self.play_context(result))
                .with_anchor(key, entry)
                .shuffled(shuffled),
        )
    }

    /// What playing an artist page's favorite track `key` at `index` among them queues, as
    /// [`View::play_track`] does for the page's tracks. None when the page shows no favorite
    /// tracks.
    pub fn play_favorite(
        &self,
        key: library::TrackKey,
        index: usize,
        placement: QueuePlacement,
        shuffled: bool,
        result: Option<&str>,
    ) -> Option<PlayRequest> {
        let read = self.favorites()?;
        if !matches!(placement, QueuePlacement::Replace { .. }) {
            return Some(PlayRequest::track(key, placement));
        }
        self.favorites_context_id().map(|context| {
            PlayRequest::captured(read.clone().selection(), index, placement, false)
                .with_context(with_result(context, result))
                .with_anchor(key, None)
                .shuffled(shuffled)
        })
    }

    /// What activating `row`, shown at `index` on this page, queues, by the one rule
    /// (decisions.md, "What a row plays is one rule", 2026-11-06): a track plays the page from
    /// it ([`View::play_track`]), a track of the Albums list in Detail layout its album from it,
    /// and any other row what [`View::row_target`] gives. The row's track and playlist entry
    /// anchor the play, so a row that moved since it showed still plays. None when it plays
    /// nothing.
    pub async fn play(
        &self,
        row: &Row,
        index: usize,
        placement: QueuePlacement,
        shuffled: bool,
        result: Option<&str>,
        cancel: &ReadCancellation,
    ) -> LibraryResult<Option<PlayRequest>> {
        let tracks = index.saturating_sub(self.folder_len().unwrap_or(0));
        Ok(match row {
            Row::Track(track) => self.play_track(
                track.track_key,
                track.entry.as_ref().map(|entry| entry.key),
                tracks,
                placement,
                shuffled,
                result,
            ),
            Row::AlbumTrack { track, .. } => {
                self.play_album_track(track, placement, shuffled, result)
            }
            row => match self.row_target(row) {
                Some(target) => {
                    return self
                        .owner
                        .play_target(&target, self.selected.as_ref(), placement, shuffled, cancel)
                        .await;
                }
                None => None,
            },
        })
    }

    /// The page `row` of this page opens: a child folder the folder it names under this one,
    /// any other row what [`Route::of_row`] gives.
    pub fn row_route(&self, row: &Row) -> Option<Route> {
        match (row, &self.route) {
            (Row::Folder(folder), Route::Folders { path }) => {
                let mut path = path.clone();
                path.push(crate::route::FolderPathItem {
                    id: folder.object_id.clone(),
                    name: folder.name.clone(),
                });
                Some(Route::Folders { path })
            }
            _ => Route::of_row(row),
        }
    }

    /// What `row` of this page plays outside the page's order, as its menu, card and drag
    /// take it: a child folder plays the folder it opens (decisions.md, "Round nine
    /// follow-ups"), any other row what [`PlayTarget::of_row`] gives.
    pub fn row_target(&self, row: &Row) -> Option<PlayTarget> {
        match row {
            Row::Folder(_) => self.row_route(row).map(PlayTarget::Route),
            row => PlayTarget::of_row(row),
        }
    }

    /// What playing a track of the Albums list in Detail layout queues: its album from it.
    fn play_album_track(
        &self,
        track: &library::TrackRow,
        placement: QueuePlacement,
        shuffled: bool,
        result: Option<&str>,
    ) -> Option<PlayRequest> {
        if !matches!(placement, QueuePlacement::Replace { .. }) {
            return Some(PlayRequest::track(track.track_key, placement));
        }
        let route = Route::AlbumDetail(track.links.album_media_uri.clone()?);
        let read = self.page_members(&route, Collection::Album(track.links.album_key?));
        let context = context_id(
            &route,
            self.selected.as_ref(),
            &OpenOptions::default(),
            read.sort?,
            read.descending,
        );
        Some(
            PlayRequest::captured(read.selection(), 0, placement, false)
                .with_context(with_result(context, result))
                .with_anchor(track.track_key, None)
                .shuffled(shuffled),
        )
    }

    /// What the page of `collection` on `route` lists unfiltered, in its saved order and this
    /// page's music folder: the read its card Play queues.
    fn page_members(&self, route: &Route, collection: Collection) -> MemberRead {
        member_read(
            route,
            self.music_folder(),
            &OpenOptions::default(),
            &self.settings,
            collection,
        )
    }

    /// The music folder selected when it opened.
    fn music_folder(&self) -> Option<library::FolderKey> {
        self.selected
            .as_ref()
            .and_then(|selected| selected.music_folder_key)
    }

    /// What playing Home's section `index` queues: its rows in the order it shows them, from
    /// its row `from`. None when the page is not Home or has no such section.
    pub fn play_section(
        &self,
        index: usize,
        from: usize,
        placement: QueuePlacement,
        shuffled: bool,
        result: Option<&str>,
    ) -> Option<PlayRequest> {
        let section = self.home_sections()?.into_iter().nth(index)?;
        // Each album and genre as its page lists it: an album whole, a genre in the folder.
        let album = |album: &library::AlbumRow| {
            self.page_members(&Route::of_album(album), Collection::Album(album.album_key))
                .selection()
        };
        let selections = match &section {
            HomeSection::Showcase(row) => vec![album(&row.album)],
            HomeSection::Genres(genres) => genres
                .iter()
                .map(|genre| {
                    let key = library::GenreKey::from_raw(genre.key);
                    self.page_members(&Route::GenreDetail(key), Collection::Genre(key))
                        .selection()
                })
                .collect(),
            HomeSection::Rows { .. } | HomeSection::Provider(_) => section
                .items()
                .into_iter()
                .map(|item| match item {
                    HomeItem::Track(row) => MemberSelection::Keys {
                        source: None,
                        keys: vec![row.track.track_key].into(),
                    },
                    HomeItem::Album(row) => album(&row.album),
                })
                .collect(),
        };
        let id = match &section {
            HomeSection::Provider(provider) => format!("provider:{}", provider.section_id),
            _ => section.block().map(block_id).unwrap_or_default(),
        };
        let source = self.selected.as_ref()?.source_id.as_str();
        Some(
            PlayRequest::captured(MemberSelection::Concat(selections), from, placement, false)
                .with_context(with_result(format!("home:{source}:{id}"), result))
                .shuffled(shuffled),
        )
    }

    /// Whether the page offers a letter index: it lists by a name its letter sections follow.
    pub fn has_letter_sections(&self) -> bool {
        let named = matches!(
            self.order().0,
            LibraryField::Title
                | LibraryField::Artist
                | LibraryField::AlbumArtist
                | LibraryField::Album
                | LibraryField::Genre
        );
        named
            && self.route != Route::History
            && match &self.body {
                Body::Rows {
                    read: Read::AlbumTracks(_),
                    ..
                }
                | Body::Home(_)
                | Body::Search(_)
                | Body::Missing => false,
                Body::Rows { .. } | Body::Releases { .. } | Body::Folders { .. } => true,
            }
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
            Body::Home(_) | Body::Search(_) | Body::Missing => 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether the collection the route names is gone or empty.
    pub fn is_missing(&self) -> bool {
        matches!(self.body, Body::Missing)
    }

    /// Home's sections as clients show them, from the blocks and filter it opened with.
    pub fn home_sections(&self) -> Option<Vec<HomeSection>> {
        let Body::Home(home) = &self.body else {
            return None;
        };
        Some(home_sections(
            home,
            &self.settings.application.home_blocks,
            &self.options.filter,
        ))
    }

    /// What a search found, each kind in its list's sort.
    pub fn search_results(&self) -> Option<&SearchResults> {
        match &self.body {
            Body::Search(results) => Some(results),
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
            Body::Home(_) | Body::Search(_) | Body::Missing => Ok(Vec::new()),
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

    /// The list read behind a page of collections, for selections of its rows.
    pub fn list(&self) -> Option<&ListRead> {
        match &self.body {
            Body::Rows {
                read: Read::List(read) | Read::AlbumTracks(read),
                ..
            }
            | Body::Releases { read, .. } => Some(read),
            _ => None,
        }
    }

    /// Everything the page lists, in the order and filter it shows, for playing or queueing it
    /// whole.
    pub fn selection(&self) -> Option<MemberSelection> {
        if let Some(read) = self.members() {
            return Some(read.clone().selection());
        }
        self.list()
            .map(|read| rows_selection(read.clone(), None, &self.settings, SelectionScope::Page))
    }

    /// What `current` settings make the page read again: the settings its order, rows and
    /// home blocks were read with.
    pub fn settings_reread(&self, current: &Settings) -> Option<Reread> {
        let lists = |key| (self.settings.library_list(key), current.library_list(key));
        let order = |key| {
            let (read, current) = lists(key);
            read.sort_key != current.sort_key || read.descending != current.descending
        };
        let key = library_list_key(&self.route, self.options.category);
        let changed = match &self.body {
            Body::Rows {
                read: Read::List(_) | Read::AlbumTracks(_),
                ..
            } => {
                let (read, current) = lists(key);
                order(key)
                    || uses_activity(&self.route, &read) != uses_activity(&self.route, &current)
                    || key == LibraryListKey::Albums
                        && (read.layout == LibraryLayout::Detail)
                            != (current.layout == LibraryLayout::Detail)
            }
            Body::Home(_) => {
                self.settings.application.home_blocks != current.application.home_blocks
            }
            Body::Search(_) => CollectionCategory::ALL
                .into_iter()
                .any(|category| order(category.key())),
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
            _ => order(key),
        };
        changed.then_some(Reread::Page)
    }

    /// Whether a favorite or rating change can move or drop rows: the page lists only favorites
    /// or sorts by favorites or ratings, Search by the sorts of the lists it orders by. Other
    /// pages patch the shown values into their rows.
    pub fn states_reorder(&self) -> bool {
        let orders = |field: LibraryField| {
            matches!(field, LibraryField::Favorite | LibraryField::UserRating)
        };
        match &self.body {
            Body::Rows {
                read: Read::Members(read),
                ..
            }
            | Body::Folders { tracks: read, .. } => {
                read.favorites_only || orders(read.effective_sort())
            }
            Body::Rows {
                read: Read::List(read) | Read::AlbumTracks(read),
                ..
            }
            | Body::Releases { read, .. } => read.favorites_only || orders(read.sort),
            Body::Search(_) => search_sorts(&self.settings)
                .into_iter()
                .any(|(field, _)| orders(field)),
            _ => false,
        }
    }

    /// Reads the page again with `options` and current settings. A folder keeps the folder it
    /// opened, so new child folders show when the route opens again. Album, artist track, genre
    /// and mood pages and Discography read their rows again and keep their header, so a
    /// collection that is gone reads as empty. A search runs its query again.
    pub async fn reread(
        &self,
        options: OpenOptions,
        seed: RouteSeedWindow,
        cancel: &ReadCancellation,
    ) -> LibraryResult<Opened> {
        // A Smart Playlist page keeps the members it opened with.
        let options = OpenOptions {
            smart_opened: options.smart_opened.or(self.options.smart_opened),
            ..options
        };
        let settings = self.owner.shared.settings.committed();
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
                    self.music_folder(),
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
        let settings = self.owner.shared.settings.committed();
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
        settings: crate::settings::Committed,
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
    fn again(&self, options: OpenOptions, settings: crate::settings::Committed) -> View {
        View {
            owner: self.owner.clone(),
            route: self.route.clone(),
            selected: self.selected.clone(),
            options,
            settings,
            art: self.art,
            header: self.header,
            body: Body::Missing,
        }
    }

    fn catalog(&self) -> &Catalog {
        &self.owner.shared.catalog
    }
}

/// One Home section, as every client shows it.
#[derive(Clone, Debug, PartialEq)]
pub enum HomeSection {
    Showcase(Box<HomeAlbumRow>),
    /// Explore, Most played, Newly added, Recently played or Recently released.
    Rows {
        block: HomeBlockKind,
        rows: HomeSectionRows,
    },
    /// A section the source's server lists.
    Provider(HomeProviderSection),
    Genres(Vec<TagRow>),
}

/// One row of a Home section.
#[derive(Clone, Debug, PartialEq)]
pub enum HomeItem {
    Track(Box<HomeTrackRow>),
    Album(Box<HomeAlbumRow>),
}

impl library::ShowsState for HomeItem {
    fn state(&self, change: &library::StateChange) -> Option<&library::TrackState> {
        match self {
            Self::Track(row) => row.track.state(change),
            Self::Album(row) => row.album.state(change),
        }
    }
    fn state_mut(&mut self, change: &library::StateChange) -> Option<&mut library::TrackState> {
        match self {
            Self::Track(row) => row.track.state_mut(change),
            Self::Album(row) => row.album.state_mut(change),
        }
    }
    fn downloads(&self) -> Option<library::Downloads> {
        match self {
            Self::Track(row) => row.track.downloads(),
            Self::Album(row) => row.album.downloads(),
        }
    }
    fn show_downloads(&mut self, read: library::Downloads) -> bool {
        match self {
            Self::Track(row) => row.track.show_downloads(read),
            Self::Album(row) => row.album.show_downloads(read),
        }
    }
}

impl HomeSection {
    /// The block it shows; None for the server's own sections.
    pub fn block(&self) -> Option<HomeBlockKind> {
        match self {
            Self::Showcase(_) => Some(HomeBlockKind::Showcase),
            Self::Genres(_) => Some(HomeBlockKind::Genres),
            Self::Rows { block, .. } => Some(*block),
            Self::Provider(_) => None,
        }
    }

    /// Its title as the page shows it.
    pub fn title(&self) -> String {
        use crate::settings::presentation::{home_block_title, home_provider_title};
        match self {
            Self::Showcase(_) => localization::tr(home_block_title(HomeBlockKind::Showcase)),
            Self::Genres(_) => localization::tr(home_block_title(HomeBlockKind::Genres)),
            Self::Rows { block, .. } => localization::tr(home_block_title(*block)),
            Self::Provider(provider) => {
                home_provider_title(&provider.section_id, provider.title.as_deref())
            }
        }
    }

    /// Its rows in the order the page shows them: tracks, then albums.
    pub fn items(&self) -> Vec<HomeItem> {
        let rows = match self {
            Self::Rows { rows, .. } => rows,
            Self::Provider(provider) => &provider.rows,
            Self::Showcase(_) | Self::Genres(_) => return Vec::new(),
        };
        rows.tracks
            .iter()
            .map(|row| HomeItem::Track(Box::new(row.clone())))
            .chain(
                rows.albums
                    .iter()
                    .map(|row| HomeItem::Album(Box::new(row.clone()))),
            )
            .collect()
    }
}

/// Home's sections in the order clients show them: the visible `blocks` in their order, except
/// that the showcase leads the server's own sections when there are any. Sections with nothing
/// to show are left out, and so are rows whose title, artist and album do not contain `filter`.
pub fn home_sections(home: &HomePage, blocks: &[HomeBlockKind], filter: &str) -> Vec<HomeSection> {
    let filter = filter.trim().to_lowercase();
    let shown = |text: String| text.to_lowercase().contains(&filter);
    let album = |row: &HomeAlbumRow| {
        filter.is_empty() || shown(format!("{} {}", row.title, row.album.artist))
    };
    let rows = |tracks: &[HomeTrackRow], albums: &[HomeAlbumRow]| HomeSectionRows {
        tracks: tracks
            .iter()
            .filter(|row| {
                filter.is_empty()
                    || shown(format!(
                        "{} {} · {}",
                        row.title, row.track.artist, row.track.album
                    ))
            })
            .cloned()
            .collect(),
        albums: albums.iter().filter(|row| album(row)).cloned().collect(),
    };
    let section_rows = |section: &HomeSectionRows| rows(&section.tracks, &section.albums);
    let block = |block: HomeBlockKind| {
        let section = match block {
            HomeBlockKind::Showcase => {
                return home
                    .showcase
                    .as_ref()
                    .filter(|row| album(row))
                    .map(|row| HomeSection::Showcase(Box::new(row.clone())));
            }
            HomeBlockKind::Genres => {
                return (!home.genres.is_empty()).then(|| HomeSection::Genres(home.genres.clone()));
            }
            HomeBlockKind::Explore => rows(&[], &home.explore),
            HomeBlockKind::MostPlayed => section_rows(&home.most_played),
            HomeBlockKind::NewlyAdded => section_rows(&home.newly_added),
            HomeBlockKind::RecentlyPlayed => section_rows(&home.recently_played),
            HomeBlockKind::RecentlyReleased => section_rows(&home.recently_released),
        };
        (!section.tracks.is_empty() || !section.albums.is_empty()).then_some(HomeSection::Rows {
            block,
            rows: section,
        })
    };
    let providers = home
        .provider_sections
        .iter()
        .map(|section| HomeProviderSection {
            section_id: section.section_id.clone(),
            title: section.title.clone(),
            rows: section_rows(&section.rows),
        })
        .filter(|section| !section.rows.tracks.is_empty() || !section.rows.albums.is_empty())
        .map(HomeSection::Provider)
        .collect::<Vec<_>>();
    let mut sections = Vec::new();
    let mut blocks = blocks.iter().copied().collect::<Vec<_>>();
    if !providers.is_empty() {
        if let Some(index) = blocks
            .iter()
            .position(|block| *block == HomeBlockKind::Showcase)
        {
            sections.extend(block(blocks.remove(index)));
        }
        sections.extend(providers);
    }
    sections.extend(blocks.into_iter().filter_map(block));
    sections
}

/// A Home block's name in the context id of a section play.
fn block_id(block: HomeBlockKind) -> String {
    serde_json::to_value(block)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// What playing `selection` whole queues, with `context`, from a random track when shuffled.
fn play_whole(
    selection: MemberSelection,
    context: Arc<str>,
    placement: QueuePlacement,
    shuffled: bool,
) -> PlayRequest {
    PlayRequest::ordered(selection, 0, placement, true)
        .with_context(context)
        .shuffled(shuffled)
}

/// An artist page's own tracks on the releases it shows, in the Artist tracks list's saved
/// order: what the page plays and downloads whole, rather than whole albums of its releases,
/// which queue other artists of a compilation.
fn artist_tracks(route: &Route, releases: &ListRead, settings: &Settings) -> Option<MemberRead> {
    let ListKind::ArtistAlbums {
        artist,
        album_artist,
        release_class,
        filters,
    } = &releases.kind
    else {
        return None;
    };
    let page = match route {
        Route::ArtistDetail(uri)
        | Route::AlbumArtistDetail(uri)
        | Route::ArtistDiscography(uri)
        | Route::AlbumArtistDiscography(uri)
            if *album_artist =>
        {
            Route::AlbumArtistTracks(uri.clone())
        }
        Route::ArtistDetail(uri)
        | Route::AlbumArtistDetail(uri)
        | Route::ArtistDiscography(uri)
        | Route::AlbumArtistDiscography(uri) => Route::ArtistTracks(uri.clone()),
        _ => return None,
    };
    let mut read = member_read(
        &page,
        releases.folder,
        &OpenOptions::default(),
        settings,
        Collection::Artist {
            key: *artist,
            album_artist: *album_artist,
        },
    );
    let filtered = !releases.filter.trim().is_empty()
        || releases.favorites_only
        || release_class.is_some()
        || filters
            .as_deref()
            .is_some_and(|filters| filters.iter().any(|filter| !filter.trim().is_empty()));
    read.within = filtered.then(|| Box::new(releases.clone()));
    Some(read)
}

/// The `ranges` of a list's rows for `scope`, all of them when None, in the list's order:
/// each row's collection as route_selection reads its page, in that page's saved order and
/// the list's music folder as `scope` keeps it.
fn rows_selection(
    read: ListRead,
    ranges: Option<Vec<Range<usize>>>,
    settings: &Settings,
    scope: SelectionScope,
) -> MemberSelection {
    let page = match read.kind {
        ListKind::Albums | ListKind::ArtistAlbums { .. } => Route::AlbumDetail(String::new()),
        ListKind::Artists { .. } => Route::ArtistTracks(String::new()),
        ListKind::Genres => Route::GenreDetail(library::GenreKey::from_raw(0)),
        ListKind::Moods => Route::MoodDetail(library::MoodKey::from_raw(0)),
        ListKind::Playlists => Route::PlaylistDetail(library::PlaylistKey::from_raw(0)),
        ListKind::SmartPlaylists { .. } => {
            Route::SmartPlaylistDetail(library::SmartPlaylistKey::from_raw(0))
        }
    };
    // Each row's page reads its collection, which takes the place of this one.
    let members = member_read(
        &page,
        read.folder.filter(|_| scope.keeps_folder(&page)),
        &OpenOptions::default(),
        settings,
        Collection::Album(library::AlbumKey::from_raw(0)),
    );
    MemberSelection::Parents {
        read,
        ranges,
        members: Box::new(members),
    }
}

/// `context` tied to the order a client shows: `|result=` and its id.
fn with_result(context: String, result: Option<&str>) -> Arc<str> {
    match result {
        Some(result) => format!("{context}|result={result}").into(),
        None => context.into(),
    }
}

/// The collection or route a page's context id starts with; a play of the collection from
/// elsewhere carries it bare.
pub fn context_base(route: &Route) -> String {
    crate::collection_context::CollectionTarget::for_route(route).map_or_else(
        || {
            format!(
                "route:{}",
                serde_json::to_string(route).expect("Route serialization")
            )
        },
        |target| target.context_id(),
    )
}

/// The context id a play of a page on `route` carries: its collection's id or its route, with
/// the filter, order, and the source and music folder its [`Scope`] reads, so a whole album
/// or playlist has one id in every folder.
pub fn context_id(
    route: &Route,
    selected: Option<&SelectedLibrary>,
    options: &OpenOptions,
    sort: LibraryField,
    descending: bool,
) -> String {
    // History lists every source's plays or one source's; each is its own context.
    let history = match (route, &options.history_source) {
        (Route::History, Some(source)) => format!("|history={}", source.as_str()),
        _ => String::new(),
    };
    let selected = Scope::of(route).apply(selected);
    format!(
        "{}|query={}|source={}|folder={:?}|sort={sort:?}|descending={descending}|favorites={}|downloaded={}{history}",
        context_base(route),
        options.filter,
        selected
            .as_ref()
            .map_or("", |selected| selected.source_id.as_str()),
        selected
            .as_ref()
            .and_then(|selected| selected.music_folder_key),
        favorites_only(route, options),
        options.downloaded_only && supports_downloaded_filter(route, options.category),
    )
}

/// What a committed change means for a page on `route`, opened while `opened_with` was
/// selected, now that `current` is.
fn changes(
    owner: &SourceOwner,
    route: &Route,
    opened_with: Option<&SelectedLibrary>,
    current: Option<&SelectedLibrary>,
    options: &OpenOptions,
    changed: &Changed,
) -> Refresh {
    let selected = current.map(|selected| selected.source_key);
    let affects = |source: Option<SourceKey>| source.is_none() || source == selected;
    let unaffected =
        |source| !affects(source) && route.requires_selected_source(options.search_mode);
    let same = Scope::of(route).same(opened_with, current);
    let mut refresh = Refresh::Nothing;
    // Favorites and ratings are patched into the rows that show them; a page whose rows they
    // order or filter reads again through `View::states_reorder`.
    let catalog = changed
        .all
        .then_some(None)
        .into_iter()
        .chain(changed.sources.iter().map(|source| Some(*source)));
    for source in catalog {
        refresh = refresh.max(if unaffected(source) {
            Refresh::Nothing
        } else if !same {
            Refresh::Reopen
        } else {
            Refresh::Reread
        });
    }
    // Plays change what a page shows where its order reads them; an artist page reads its
    // tracks' order and its releases', Search the orders of the Tracks, Albums and Artists
    // lists.
    if changed.listened {
        let settings = owner.shared.settings.committed();
        let sensitive = match route {
            Route::Search => CollectionCategory::ALL
                .into_iter()
                .any(|category| uses_activity(route, &settings.library_list(category.key()))),
            Route::ArtistDetail(_) | Route::AlbumArtistDetail(_) => {
                [CollectionCategory::Tracks, CollectionCategory::Albums]
                    .into_iter()
                    .any(|category| {
                        let options = OpenOptions {
                            category,
                            ..options.clone()
                        };
                        read_order(route, &options, &settings).2
                    })
            }
            _ => read_order(route, options, &settings).2,
        };
        if sensitive {
            refresh = refresh.max(Refresh::Reread);
        }
    }
    if changed.lyrics
        && route == &Route::Search
        && options.search_mode == library::SearchMode::Lyrics
    {
        refresh = refresh.max(Refresh::Reread);
    }
    if refresh_unread(route, opened_with, current, changed) == Refresh::Reopen {
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

/// What a committed change means for a page on `route` that reads nothing itself, opened while
/// `opened_with` was selected, now that `current` is: a page waiting for a source, or the Search
/// page, whose search reads the results. It opens again when the selection changed so that it
/// reads another source or music folder, as a page that reads does.
pub fn refresh_unread(
    route: &Route,
    opened_with: Option<&SelectedLibrary>,
    current: Option<&SelectedLibrary>,
    changed: &Changed,
) -> Refresh {
    if changed.scope && !Scope::of(route).same(opened_with, current) {
        Refresh::Reopen
    } else {
        Refresh::Nothing
    }
}

/// Whether a committed change can change the sidebar pins. Plays, ratings and the music
/// folder change only what smart playlist pins count.
pub fn pins_refresh(owner: &SourceOwner, changed: &Changed) -> bool {
    changed.all
        || changed.pins
        || changed.scope
        || changed.playlist_lists
        || !changed.sources.is_empty()
        || !changed.playlists.is_empty()
        || (changed.listened || changed.items.iter().any(|item| item.rating_changed))
            && owner.shared.settings.load().ui.sidebar.pins_use_activity()
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

/// How a page on `route` reads the members of `collection` with `options`, with `folder`
/// selected: the one track read of every page, play and selection.
fn member_read(
    route: &Route,
    folder: Option<library::FolderKey>,
    options: &OpenOptions,
    settings: &Settings,
    collection: Collection,
) -> MemberRead {
    let (sort, descending, activity) = read_order(route, options, settings);
    let mut read = MemberRead::new(collection);
    read.sort = Some(sort);
    read.descending = descending;
    read.folder = folder.filter(|_| Scope::of(route) == Scope::Folder);
    read.filter.clone_from(&options.filter);
    read.favorites_only = favorites_only(route, options);
    read.downloaded_only =
        options.downloaded_only && supports_downloaded_filter(route, options.category);
    read.activity = activity;
    read
}

/// The tracks of a resolved folder: those the projection names, in the folder it names.
fn folder_tracks(
    route: &Route,
    selected: &SelectedLibrary,
    projection: &FolderProjection,
    options: &OpenOptions,
    settings: &Settings,
) -> MemberRead {
    member_read(
        route,
        projection.folder(),
        options,
        settings,
        projection.collection(selected.source_key),
    )
}

/// The sorts of the Tracks, Albums and Artists lists, which order search results.
fn search_sorts(settings: &Settings) -> [(LibraryField, bool); 3] {
    CollectionCategory::ALL.map(|category| {
        let list = settings.library_list(category.key());
        (list.sort_key, list.descending)
    })
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
