use crate::route::{CollectionCategory, Route};
use crate::settings::{LibraryField, LibraryLayout, LibraryListKey, LibraryListSettings};
use library::{
    CatalogQuery, CatalogSubject, Database, ReadCancellation, TrackScope, TrackSelection,
};

#[derive(Clone, Debug, PartialEq)]
pub struct ArtistReleasePage {
    pub query: ArtistReleaseQuery,
    pub counts: [usize; 6],
    pub source_counts: [usize; 6],
    pub first_row_position: usize,
    pub first_rows: Vec<library::AlbumView>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArtistReleaseQuery {
    pub source: library::SourceKey,
    pub folder: Option<library::FolderKey>,
    pub artist: library::ArtistKey,
    pub album_artist: bool,
    pub filters: [String; 6],
    pub sort: library::CatalogSort,
    pub descending: bool,
    pub activity: bool,
}

const ARTIST_RELEASE_CLASSES: [library::AlbumReleaseClass; 5] = [
    library::AlbumReleaseClass::Album,
    library::AlbumReleaseClass::Ep,
    library::AlbumReleaseClass::Single,
    library::AlbumReleaseClass::Collection,
    library::AlbumReleaseClass::Other,
];

impl ArtistReleaseQuery {
    pub async fn rows(
        &self,
        database: &Database,
        counts: [usize; 6],
        range: std::ops::Range<usize>,
        cancellation: &ReadCancellation,
    ) -> library::LibraryResult<Vec<library::AlbumView>> {
        if self.filters[..ARTIST_RELEASE_CLASSES.len()]
            .iter()
            .all(|filter| filter == &self.filters[0])
        {
            return Ok(self
                .section_rows(database, None, range.start, range.len(), cancellation)
                .await?
                .into_iter()
                .map(|(_, row)| row)
                .collect());
        }
        let mut rows = Vec::new();
        let mut start = 0;
        for (index, class) in ARTIST_RELEASE_CLASSES.into_iter().enumerate() {
            let end = start + counts[index];
            let first = range.start.max(start);
            let last = range.end.min(end);
            if first < last {
                rows.extend(
                    self.section_rows(
                        database,
                        Some(class),
                        first - start,
                        last - first,
                        cancellation,
                    )
                    .await?
                    .into_iter()
                    .map(|(_, row)| row),
                );
            }
            start = end;
        }
        Ok(rows)
    }

    pub async fn page(
        self,
        database: &Database,
        window: library::RouteSeedWindow,
        cancellation: &ReadCancellation,
    ) -> library::LibraryResult<ArtistReleasePage> {
        let source_counts = database
            .artist_release_counts(
                self.source,
                self.artist,
                self.album_artist,
                self.folder,
                "",
                cancellation,
            )
            .await?
            .map(|count| count as usize);
        let counts = if self.filters.iter().all(String::is_empty) {
            source_counts
        } else {
            self.counts(database, cancellation).await?
        };
        let range = window.range(counts.iter().sum());
        let first_rows = self
            .rows(database, counts, range.clone(), cancellation)
            .await?;
        Ok(ArtistReleasePage {
            query: self,
            counts,
            source_counts,
            first_row_position: range.start,
            first_rows,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
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
        query: &CatalogQuery,
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
        query: &CatalogQuery,
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
        Route::Playlists => LibraryListKey::Playlists,
        Route::SmartPlaylists => LibraryListKey::SmartPlaylists,
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

pub struct BrowseRequest<'a> {
    pub route: &'a Route,
    pub category: CollectionCategory,
    pub source: Option<library::SourceKey>,
    pub folder: Option<library::FolderKey>,
    pub history_source: Option<library::SourceId>,
    pub filter: &'a str,
    pub sort: LibraryField,
    pub descending: bool,
    pub favorites_only: bool,
    pub downloaded_only: bool,
    pub now: i64,
    pub inline_tracks: Option<usize>,
    pub activity: bool,
}

pub enum BrowseQuery {
    Tracks(CatalogQuery),
    Collection(CatalogQuery),
    Releases(ArtistReleaseQuery),
    Empty,
}

pub enum DetailSummary {
    Album(Box<library::AlbumView>),
    Artist(library::ArtistView),
    Playlist(library::PlaylistRow),
    SmartPlaylist(library::SmartPlaylistRow),
    Genre(library::GenreView),
    Mood(library::MoodView),
}

pub enum DetailContent {
    Tracks(library::CatalogPage),
    Releases {
        page: Box<ArtistReleasePage>,
        favorites: Option<library::CatalogPage>,
    },
}

pub struct DetailPage {
    pub summary: DetailSummary,
    pub content: DetailContent,
}

impl<'a> BrowseRequest<'a> {
    pub fn new(
        route: &'a Route,
        category: CollectionCategory,
        source: Option<library::SourceKey>,
        folder: Option<library::FolderKey>,
        settings: &LibraryListSettings,
        inline_tracks: usize,
    ) -> Self {
        Self {
            route,
            category,
            source,
            folder,
            history_source: None,
            filter: "",
            sort: settings.sort_key,
            descending: settings.descending,
            favorites_only: false,
            downloaded_only: false,
            now: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs() as i64),
            inline_tracks: (settings.layout == LibraryLayout::Detail).then_some(inline_tracks),
            activity: match settings.layout {
                LibraryLayout::Grid => &settings.grid_fields,
                _ => &settings.row_fields,
            }
            .iter()
            .chain(if settings.layout == LibraryLayout::Detail {
                settings.detail_track_fields.as_slice()
            } else {
                &[]
            })
            .any(|field| field.catalog_sort().activity_sensitive()),
        }
    }

    pub async fn detail_page(
        &self,
        database: &Database,
        track_settings: &LibraryListSettings,
        window: library::RouteSeedWindow,
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<Option<DetailPage>> {
        use library::{CatalogKind, CatalogRows};
        let album_artist = matches!(
            self.route,
            Route::AlbumArtistDetail(_)
                | Route::AlbumArtistDiscography(_)
                | Route::AlbumArtistTracks(_)
                | Route::AlbumArtistFavoriteTracks(_)
        );
        if matches!(
            self.route,
            Route::ArtistDetail(_)
                | Route::AlbumArtistDetail(_)
                | Route::ArtistDiscography(_)
                | Route::AlbumArtistDiscography(_)
        ) {
            let Some(releases) = self.artist(database, cancel).await? else {
                return Ok(None);
            };
            let favorites = matches!(
                self.route,
                Route::ArtistDetail(_) | Route::AlbumArtistDetail(_)
            )
            .then(|| {
                BrowseRequest {
                    filter: self.filter,
                    favorites_only: true,
                    ..BrowseRequest::new(
                        self.route,
                        CollectionCategory::Tracks,
                        Some(releases.source),
                        self.folder,
                        track_settings,
                        0,
                    )
                }
                .artist_tracks(&releases)
            });
            return releases
                .detail_page(database, favorites, window, cancel)
                .await;
        }
        let mut summary_query = match self.route {
            Route::AlbumDetail(uri) => CatalogQuery::uris(CatalogKind::Album, vec![uri.clone()]),
            Route::ArtistDetail(uri)
            | Route::AlbumArtistDetail(uri)
            | Route::ArtistDiscography(uri)
            | Route::AlbumArtistDiscography(uri)
            | Route::ArtistTracks(uri)
            | Route::AlbumArtistTracks(uri)
            | Route::ArtistFavoriteTracks(uri)
            | Route::AlbumArtistFavoriteTracks(uri) => {
                let mut query = CatalogQuery::uris(CatalogKind::Artist, vec![uri.clone()]);
                if let CatalogSubject::Uris {
                    album_artist: role, ..
                } = &mut query.subject
                {
                    *role = album_artist;
                }
                query
            }
            Route::PlaylistDetail(key) => {
                CatalogQuery::keys(CatalogKind::Playlist, None, vec![key.raw()])
            }
            Route::SmartPlaylistDetail(key) => {
                CatalogQuery::keys(CatalogKind::SmartPlaylist, self.source, vec![key.raw()])
            }
            Route::GenreDetail(key) => {
                CatalogQuery::keys(CatalogKind::Genre, self.source, vec![key.raw()])
            }
            Route::MoodDetail(key) => {
                CatalogQuery::keys(CatalogKind::Mood, self.source, vec![key.raw()])
            }
            _ => return Ok(None),
        };
        summary_query.folder = self.folder;
        let rows = if let Route::SmartPlaylistDetail(key) = self.route {
            CatalogRows::SmartPlaylists(
                database
                    .smart_playlist_rows(self.source, &[*key], self.folder, self.now, cancel)
                    .await?,
            )
        } else {
            summary_query.rows(database, 0, 1, cancel).await?
        };
        let (summary, query, total) = match rows {
            CatalogRows::Albums(mut rows) => {
                let Some(row) = rows.pop() else {
                    return Ok(None);
                };
                let query = self.query_for_scope(TrackScope::Album(row.album_key));
                let total = row.summary.track_count;
                (DetailSummary::Album(Box::new(row)), query, total)
            }
            CatalogRows::Artists(mut rows) => {
                let Some(row) = rows.pop() else {
                    return Ok(None);
                };
                let releases = ArtistReleaseQuery {
                    source: row.source_key,
                    folder: self.folder,
                    artist: row.artist_key,
                    album_artist,
                    filters: std::array::from_fn(|_| self.filter.to_string()),
                    sort: self.sort.catalog_sort(),
                    descending: self.descending,
                    activity: self.activity,
                };
                let query = self.artist_tracks(&releases);
                let total = row.summary.track_count;
                (DetailSummary::Artist(row), query, total)
            }
            CatalogRows::Playlists(mut rows) => {
                let Some(mut row) = rows.pop() else {
                    return Ok(None);
                };
                row.genres = database.playlist_genres(row.playlist_key, cancel).await?;
                let total = row.track_count;
                (
                    DetailSummary::Playlist(row),
                    self.tracks().expect("playlist detail"),
                    total,
                )
            }
            CatalogRows::SmartPlaylists(mut rows) => {
                let Some(row) = rows.pop() else {
                    return Ok(None);
                };
                let total = row.track_count;
                (
                    DetailSummary::SmartPlaylist(row),
                    self.tracks().expect("smart playlist detail"),
                    total,
                )
            }
            CatalogRows::Genres(mut rows) => {
                let Some(row) = rows.pop().filter(|row| row.summary.track_count > 0) else {
                    return Ok(None);
                };
                let total = row.summary.track_count;
                (
                    DetailSummary::Genre(row),
                    self.tracks().expect("genre detail"),
                    total,
                )
            }
            CatalogRows::Moods(mut rows) => {
                let Some(row) = rows.pop().filter(|row| row.summary.track_count > 0) else {
                    return Ok(None);
                };
                let total = row.summary.track_count;
                (
                    DetailSummary::Mood(row),
                    self.tracks().expect("mood detail"),
                    total,
                )
            }
            _ => unreachable!("detail summary"),
        };
        let count = if query.filter.is_empty()
            && !query.favorites_only
            && !query.downloaded_only
            && (query.folder.is_none()
                || matches!(
                    self.route,
                    Route::SmartPlaylistDetail(_) | Route::GenreDetail(_) | Route::MoodDetail(_)
                )) {
            total.max(0) as usize
        } else {
            query.count(database, cancel).await?
        };
        let range = window.range(count);
        let rows = query
            .rows(database, range.start, range.len(), cancel)
            .await?;
        let disc_sections = if matches!(self.route, Route::AlbumDetail(_)) {
            query.disc_sections(database, cancel).await?
        } else {
            Vec::new()
        };
        Ok(Some(DetailPage {
            summary,
            content: DetailContent::Tracks(library::CatalogPage {
                query,
                count,
                first_row_position: range.start,
                rows,
                disc_sections,
            }),
        }))
    }

    pub fn activity_sensitive(&self) -> bool {
        matches!(
            self.route,
            Route::Home | Route::SmartPlaylists | Route::SmartPlaylistDetail(_) | Route::History
        ) || self.activity
            || self.sort.catalog_sort().activity_sensitive()
    }

    pub fn collection(&self) -> Option<CatalogQuery> {
        let source = || self.source.expect("source-scoped collection has a source");
        let kind = match self.route {
            Route::Albums => CatalogSubject::Albums {
                sort: self.sort.catalog_sort(),
                source: source(),
                inline_tracks: self.inline_tracks,
            },
            Route::Favorites if self.category == CollectionCategory::Albums => {
                CatalogSubject::Albums {
                    sort: self.sort.catalog_sort(),
                    source: source(),
                    inline_tracks: self.inline_tracks,
                }
            }
            Route::Favorites if self.category == CollectionCategory::Tracks => return None,
            Route::Artists | Route::AlbumArtists | Route::Favorites => CatalogSubject::Artists {
                sort: self.sort.catalog_sort(),
                source: source(),
                album_artist: matches!(self.route, Route::AlbumArtists),
            },
            Route::Genres => CatalogSubject::Genres {
                source: source(),
                sort: self.sort.catalog_sort(),
            },
            Route::Moods => CatalogSubject::Moods {
                source: source(),
                sort: self.sort.catalog_sort(),
            },
            Route::Playlists => CatalogSubject::Playlists {
                source: self.source,
                sort: self.sort.catalog_sort(),
            },
            Route::SmartPlaylists => CatalogSubject::SmartPlaylists {
                sort: self.sort.catalog_sort(),
                source: self.source,
                now: self.now,
            },
            _ => return None,
        };
        Some(CatalogQuery {
            subject: kind,
            folder: self.folder,
            filter: self.filter.to_string(),
            descending: self.descending,
            favorites_only: self.favorites_only || matches!(self.route, Route::Favorites),
            downloaded_only: false,
            activity: self.activity,
        })
    }

    pub fn tracks(&self) -> Option<CatalogQuery> {
        let source = || self.source.expect("source-scoped song route has a source");
        let scope = match self.route {
            Route::Tracks => Some(TrackScope::Tracks(source())),
            Route::Favorites if self.category == CollectionCategory::Tracks => {
                Some(TrackScope::Tracks(source()))
            }
            Route::GenreDetail(key) => Some(TrackScope::Genre(*key)),
            Route::MoodDetail(key) => Some(TrackScope::Mood(*key)),
            Route::PlaylistDetail(key) => Some(TrackScope::Playlist(*key)),
            Route::SmartPlaylistDetail(key) => Some(TrackScope::Smart {
                key: *key,
                source: self.source,
                now: self.now,
            }),
            Route::History => Some(TrackScope::History {
                source: self.history_source.clone(),
            }),
            _ => None,
        }?;
        Some(self.query_for_scope(scope))
    }

    fn query_for_scope(&self, scope: TrackScope) -> CatalogQuery {
        CatalogQuery {
            subject: library::CatalogSubject::Tracks {
                scope,
                sort: Some(self.sort.catalog_sort()),
            },
            folder: self.folder,
            filter: self.filter.to_string(),
            descending: self.descending,
            favorites_only: self.favorites_only
                || matches!(
                    self.route,
                    Route::Favorites
                        | Route::ArtistFavoriteTracks(_)
                        | Route::AlbumArtistFavoriteTracks(_)
                ),
            downloaded_only: self.downloaded_only,
            activity: self.activity,
        }
    }

    pub async fn artist(
        &self,
        database: &Database,
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<Option<ArtistReleaseQuery>> {
        let uri = match self.route {
            Route::ArtistDetail(uri)
            | Route::AlbumArtistDetail(uri)
            | Route::ArtistDiscography(uri)
            | Route::AlbumArtistDiscography(uri)
            | Route::ArtistTracks(uri)
            | Route::AlbumArtistTracks(uri)
            | Route::ArtistFavoriteTracks(uri)
            | Route::AlbumArtistFavoriteTracks(uri) => uri,
            _ => return Ok(None),
        };
        let Some(identity) = database
            .catalog_identity(library::CatalogKind::Artist, uri, cancel)
            .await?
        else {
            return Ok(None);
        };
        let source = identity.source_key.expect("native artist source");
        let artist = library::ArtistKey::from_raw(identity.key);
        let album_artist = matches!(
            self.route,
            Route::AlbumArtistDetail(_)
                | Route::AlbumArtistDiscography(_)
                | Route::AlbumArtistTracks(_)
                | Route::AlbumArtistFavoriteTracks(_)
        );
        Ok(Some(ArtistReleaseQuery {
            source,
            folder: self.folder,
            artist,
            album_artist,
            filters: std::array::from_fn(|_| self.filter.to_string()),
            sort: self.sort.catalog_sort(),
            descending: self.descending,
            activity: self.activity,
        }))
    }

    pub fn artist_tracks(&self, artist: &ArtistReleaseQuery) -> CatalogQuery {
        self.query_for_scope(TrackScope::Artist {
            key: artist.artist,
            album_artist: artist.album_artist,
        })
    }

    pub async fn resolve(
        &self,
        database: &Database,
        folder_projection: Option<&crate::source::folders::FolderProjection>,
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<BrowseQuery> {
        if let Some(query) = self.collection() {
            return Ok(BrowseQuery::Collection(query));
        }
        let source = || {
            self.source
                .ok_or_else(|| library::LibraryError::InvalidRequest("No source selected".into()))
        };
        if let Some(query) = self.tracks() {
            return Ok(BrowseQuery::Tracks(query));
        }
        if let Route::AlbumDetail(uri) = self.route {
            return Ok(
                match database
                    .catalog_identity(library::CatalogKind::Album, uri, cancel)
                    .await?
                {
                    Some(album) => BrowseQuery::Tracks(self.query_for_scope(TrackScope::Album(
                        library::AlbumKey::from_raw(album.key),
                    ))),
                    None => BrowseQuery::Empty,
                },
            );
        }
        if let Some(artist) = self.artist(database, cancel).await? {
            return Ok(
                if matches!(
                    self.route,
                    Route::ArtistDiscography(_) | Route::AlbumArtistDiscography(_)
                ) || (matches!(
                    self.route,
                    Route::ArtistDetail(_) | Route::AlbumArtistDetail(_)
                ) && self.category == CollectionCategory::Albums)
                {
                    BrowseQuery::Releases(artist)
                } else {
                    BrowseQuery::Tracks(self.artist_tracks(&artist))
                },
            );
        }
        let mut folder = self.folder;
        let scope = match self.route {
            Route::Folders { .. } => {
                let query = folder_projection
                    .expect("resolved folder route")
                    .track_query(source()?);
                folder = query.folder;
                match query.subject {
                    CatalogSubject::Tracks { scope, .. } => Some(scope),
                    _ => unreachable!("folder song query"),
                }
            }
            _ => None,
        };
        if let Some(scope) = scope {
            let mut query = self.query_for_scope(scope);
            query.folder = folder;
            return Ok(BrowseQuery::Tracks(query));
        }
        Ok(BrowseQuery::Empty)
    }
}

impl BrowseQuery {
    pub fn tracks(&self) -> Option<&CatalogQuery> {
        match self {
            Self::Tracks(query) => Some(query),
            _ => None,
        }
    }

    pub fn collection_selection(&self, folder_route: bool) -> Option<TrackSelection> {
        let mut query = match self {
            Self::Tracks(query) => query.clone(),
            Self::Releases(query) => CatalogQuery::tracks(TrackScope::Artist {
                key: query.artist,
                album_artist: query.album_artist,
            }),
            _ => return None,
        };
        if !folder_route {
            if !matches!(
                query.subject,
                library::CatalogSubject::Tracks {
                    scope: library::TrackScope::Smart { .. },
                    ..
                }
            ) {
                query.folder = None;
            }
            if let CatalogSubject::Tracks { sort, .. } = &mut query.subject {
                *sort = None;
            }
            query.descending = false;
        }
        Some(query.selection())
    }
}

impl ArtistReleaseQuery {
    pub async fn detail_page(
        self,
        database: &Database,
        favorites: Option<CatalogQuery>,
        window: library::RouteSeedWindow,
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<Option<DetailPage>> {
        let mut query = CatalogQuery::keys(
            library::CatalogKind::Artist,
            Some(self.source),
            vec![self.artist.raw()],
        );
        query.folder = self.folder;
        if let CatalogSubject::Keys { album_artist, .. } = &mut query.subject {
            *album_artist = self.album_artist;
        }
        let library::CatalogRows::Artists(mut rows) = query.rows(database, 0, 1, cancel).await?
        else {
            unreachable!("artist summary")
        };
        let Some(summary) = rows.pop() else {
            return Ok(None);
        };
        let favorites = match favorites {
            Some(query) => Some(
                query
                    .page(database, library::RouteSeedWindow::top(), cancel)
                    .await?,
            ),
            None => None,
        };
        Ok(Some(DetailPage {
            summary: DetailSummary::Artist(summary),
            content: DetailContent::Releases {
                page: Box::new(self.page(database, window, cancel).await?),
                favorites,
            },
        }))
    }

    pub async fn counts(
        &self,
        database: &Database,
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<[usize; 6]> {
        let mut counts = database
            .artist_release_counts(
                self.source,
                self.artist,
                self.album_artist,
                self.folder,
                &self.filters[0],
                cancel,
            )
            .await?
            .map(|count| count.max(0) as usize);
        for index in 1..ARTIST_RELEASE_CLASSES.len() {
            if self.filters[index] != self.filters[0] {
                counts[index] = database
                    .artist_release_counts(
                        self.source,
                        self.artist,
                        self.album_artist,
                        self.folder,
                        &self.filters[index],
                        cancel,
                    )
                    .await?[index]
                    .max(0) as usize;
            }
        }
        Ok(counts)
    }

    pub async fn section_rows(
        &self,
        database: &Database,
        class: Option<library::AlbumReleaseClass>,
        offset: usize,
        limit: usize,
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<Vec<(library::AlbumReleaseClass, library::AlbumView)>> {
        let index = class
            .and_then(|class| {
                ARTIST_RELEASE_CLASSES
                    .iter()
                    .position(|value| *value == class)
            })
            .unwrap_or(0);
        database
            .artist_release_page(
                self.source,
                self.artist,
                self.album_artist,
                self.folder,
                &self.filters[index],
                self.sort,
                self.descending,
                class,
                offset,
                limit,
                self.activity,
                cancel,
            )
            .await
    }

    pub async fn sections(
        &self,
        database: &Database,
        class: library::AlbumReleaseClass,
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<Vec<library::ScrollSection>> {
        let index = ARTIST_RELEASE_CLASSES
            .iter()
            .position(|value| *value == class)
            .expect("release class");
        database
            .artist_release_scroll_sections(
                self.source,
                self.artist,
                self.album_artist,
                self.folder,
                &self.filters[index],
                self.sort,
                self.descending,
                class,
                cancel,
            )
            .await
    }
}
