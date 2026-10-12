use crate::route::{CollectionCategory, Route};
use crate::settings::{LibraryField, LibraryLayout, LibraryListKey, LibraryListSettings};
use library::{
    CollectionKind, CollectionQuery, Database, ReadCancellation, SongQuery, SongScope,
    SongSelection,
};

#[derive(Clone, Debug, PartialEq)]
pub struct ArtistReleasePage {
    pub query: ArtistReleaseQuery,
    pub counts: [usize; 6],
    pub source_counts: [usize; 6],
    pub first_row_position: usize,
    pub first_rows: Vec<library::AlbumRow>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArtistReleaseQuery {
    pub source: library::SourceKey,
    pub folder: Option<library::FolderKey>,
    pub artist: library::ArtistKey,
    pub album_artist: bool,
    pub filters: [String; 6],
    pub sort: library::AlbumSort,
    pub descending: bool,
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
    ) -> library::LibraryResult<Vec<library::AlbumRow>> {
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
        query: &SongQuery,
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
                            name: row.name,
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
        query: &SongQuery,
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
        for mut section in database.song_sections(query, cancel).await? {
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
}

pub enum BrowseQuery {
    Songs(SongQuery),
    Collection(CollectionQuery),
    Releases(ArtistReleaseQuery),
    Empty,
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
        }
    }

    pub fn collection(&self) -> Option<CollectionQuery> {
        let source = || self.source.expect("source-scoped collection has a source");
        let kind = match self.route {
            Route::Albums => CollectionKind::Albums {
                sort: self.sort.album_sort(),
                source: source(),
                inline_tracks: self.inline_tracks,
            },
            Route::Favorites if self.category == CollectionCategory::Albums => {
                CollectionKind::Albums {
                    sort: self.sort.album_sort(),
                    source: source(),
                    inline_tracks: self.inline_tracks,
                }
            }
            Route::Favorites if self.category == CollectionCategory::Tracks => return None,
            Route::Artists | Route::AlbumArtists | Route::Favorites => CollectionKind::Artists {
                sort: self.sort.artist_sort(),
                source: source(),
                album_artist: matches!(self.route, Route::AlbumArtists),
            },
            Route::Genres => CollectionKind::Genres {
                source: source(),
                sort: self.sort.genre_sort(),
            },
            Route::Moods => CollectionKind::Moods {
                source: source(),
                sort: self.sort.mood_sort(),
            },
            Route::Playlists => CollectionKind::Playlists {
                source: self.source,
                sort: self.sort.playlist_sort(),
            },
            Route::SmartPlaylists => CollectionKind::SmartPlaylists {
                sort: self.sort.smart_playlist_sort(),
                source: self.source,
                now: self.now,
            },
            _ => return None,
        };
        Some(CollectionQuery {
            kind,
            folder: self.folder,
            filter: self.filter.to_string(),
            descending: self.descending,
            favorites_only: self.favorites_only || matches!(self.route, Route::Favorites),
        })
    }

    pub fn songs(&self) -> Option<SongQuery> {
        let source = || self.source.expect("source-scoped song route has a source");
        let scope = match self.route {
            Route::Tracks => Some(SongScope::Tracks(source())),
            Route::Favorites if self.category == CollectionCategory::Tracks => {
                Some(SongScope::Tracks(source()))
            }
            Route::GenreDetail(key) => Some(SongScope::Genre(*key)),
            Route::MoodDetail(key) => Some(SongScope::Mood(*key)),
            Route::PlaylistDetail(key) => Some(SongScope::Playlist(*key)),
            Route::SmartPlaylistDetail(key) => Some(SongScope::Smart {
                key: *key,
                source: self.source,
                now: self.now,
            }),
            Route::History => Some(SongScope::History {
                source: self.history_source.clone(),
            }),
            _ => None,
        }?;
        Some(self.query_for_scope(scope))
    }

    fn query_for_scope(&self, scope: SongScope) -> SongQuery {
        SongQuery {
            folder: self.folder,
            filter: self.filter.to_string(),
            sort: Some(self.sort.track_sort()),
            descending: self.descending,
            favorites_only: self.favorites_only
                || matches!(
                    self.route,
                    Route::Favorites
                        | Route::ArtistFavoriteTracks(_)
                        | Route::AlbumArtistFavoriteTracks(_)
                ),
            downloaded_only: self.downloaded_only,
            ..SongQuery::new(scope)
        }
    }

    pub async fn album(
        &self,
        database: &Database,
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<Option<(library::AlbumRow, SongQuery)>> {
        let Route::AlbumDetail(uri) = self.route else {
            return Ok(None);
        };
        Ok(database
            .album_row_by_media_uri(uri, cancel)
            .await?
            .map(|album| {
                let query = self.query_for_scope(SongScope::Album(album.album_key));
                (album, query)
            }))
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
        let Some((source, artist)) = database.artist_identity_by_media_uri(uri, cancel).await?
        else {
            return Ok(None);
        };
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
            sort: self.sort.album_sort(),
            descending: self.descending,
        }))
    }

    pub fn artist_songs(&self, artist: &ArtistReleaseQuery) -> SongQuery {
        self.query_for_scope(SongScope::Artist {
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
        if let Some(query) = self.songs() {
            return Ok(BrowseQuery::Songs(query));
        }
        if let Some((_, query)) = self.album(database, cancel).await? {
            return Ok(BrowseQuery::Songs(query));
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
                    BrowseQuery::Songs(self.artist_songs(&artist))
                },
            );
        }
        let mut folder = self.folder;
        let scope = match self.route {
            Route::Folders { .. } => {
                let query = folder_projection
                    .expect("resolved folder route")
                    .song_query(source()?);
                folder = query.folder;
                Some(query.scope)
            }
            _ => None,
        };
        if let Some(scope) = scope {
            let mut query = self.query_for_scope(scope);
            query.folder = folder;
            return Ok(BrowseQuery::Songs(query));
        }
        Ok(BrowseQuery::Empty)
    }
}

impl BrowseQuery {
    pub fn songs(&self) -> Option<&SongQuery> {
        match self {
            Self::Songs(query) => Some(query),
            _ => None,
        }
    }

    pub fn collection_selection(&self, folder_route: bool) -> Option<SongSelection> {
        let mut query = match self {
            Self::Songs(query) => query.clone(),
            Self::Releases(query) => SongQuery::new(SongScope::Artist {
                key: query.artist,
                album_artist: query.album_artist,
            }),
            _ => return None,
        };
        if !folder_route {
            if !matches!(query.scope, SongScope::Smart { .. }) {
                query.folder = None;
            }
            query.sort = None;
            query.descending = false;
        }
        Some(query.selection())
    }
}

impl ArtistReleaseQuery {
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
    ) -> library::LibraryResult<Vec<(library::AlbumReleaseClass, library::AlbumRow)>> {
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
