//! Search over the resident Catalog, and the rows of server search results by object id.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::catalog::views::{Context, holds};
use crate::read::Members;
use crate::{
    AlbumKey, AlbumRow, ArtistKey, ArtistRow, Catalog, Collection, FolderKey, Item, LibraryField,
    LibraryResult, ListKind, ListRead, MemberRead, Read, ReadCancellation, Row, SourceKey,
    TrackKey, TrackRow,
};

const SEARCH_LIMIT: usize = 100;
const SEARCH_TEXT_LIMIT: usize = 256;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SearchMode {
    #[default]
    Title,
    Lyrics,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct LyricsSnippet {
    pub parts: Vec<LyricsSnippetPart>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct LyricsSnippetPart {
    pub text: String,
    pub highlighted: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchRequest {
    query: String,
    limit: usize,
}

impl SearchRequest {
    pub fn new(query: impl Into<String>) -> Self {
        Self::with_limit(query, 20)
    }

    pub fn with_limit(query: impl Into<String>, limit: usize) -> Self {
        let query = query
            .into()
            .trim()
            .to_lowercase()
            .chars()
            .take(SEARCH_TEXT_LIMIT)
            .collect();
        Self {
            query,
            limit: limit.clamp(1, SEARCH_LIMIT),
        }
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub const fn limit(&self) -> usize {
        self.limit
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchResults {
    pub tracks: Vec<TrackRow>,
    pub albums: Vec<AlbumRow>,
    pub artists: Vec<ArtistRow>,
    pub lyrics: BTreeMap<crate::TrackKey, LyricsSnippet>,
}

impl Catalog {
    /// The rows of a server's search results by object id in music folder `folder`, kept to it
    /// as the lists of that folder hold them unless the server `kept` its results to it already
    /// (its Catalog may not hold every result's folder yet).
    pub async fn search_rows_by_objects(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        kept: bool,
        album_artists: bool,
        track_objects: &[String],
        album_objects: &[String],
        artist_objects: &[String],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<SearchResults> {
        if [
            track_objects.len(),
            album_objects.len(),
            artist_objects.len(),
        ]
        .into_iter()
        .any(|len| len > SEARCH_LIMIT)
        {
            return Err(crate::LibraryError::InvalidRequest(
                "Search identity page exceeds 100 rows".into(),
            ));
        }
        let mut connection = self.acquire_loaded(cancellation).await?;
        let tracks = self
            .track_keys_by_objects(&mut connection, Some(source), track_objects)
            .await?;
        let keys = {
            let snapshot = self.snapshot();
            let owner = snapshot
                .source_slot(source)
                .unwrap_or(crate::catalog::snapshot::NO_SOURCE);
            let keys = |kind, objects: &[String]| {
                let objects = objects.iter().map(String::as_str).collect::<Vec<_>>();
                snapshot
                    .object_keys(kind, owner, &objects)
                    .into_iter()
                    .flatten()
                    .collect()
            };
            let mut keys: [Vec<i64>; 3] = [
                tracks,
                keys(crate::CatalogKind::Album, album_objects),
                keys(crate::CatalogKind::Artist, artist_objects),
            ];
            let context = Context::new(&snapshot);
            match context.folder_slot(folder, Some(owner)) {
                None => keys = Default::default(),
                Some(None) => {}
                Some(Some(_)) if kept => {}
                Some(Some(folder)) => {
                    let derived = self.derived(&context, Some(folder));
                    keys[0].retain(|key| {
                        snapshot
                            .tracks
                            .get(*key)
                            .is_some_and(|record| context.in_folder(record, Some(folder)))
                    });
                    keys[1].retain(|key| {
                        snapshot
                            .albums
                            .slot(*key)
                            .is_some_and(|slot| derived.albums[slot as usize].tracks > 0)
                    });
                    keys[2].retain(|key| {
                        snapshot.artists.slot(*key).is_some_and(|slot| {
                            derived.artists[usize::from(album_artists)][slot as usize].tracks > 0
                        })
                    });
                }
            }
            keys
        };
        self.search_rows(&mut connection, &keys, album_artists, folder)
            .await
    }

    /// The rows of search results, in one statement.
    pub(crate) async fn search_rows(
        &self,
        connection: &mut sqlx::SqliteConnection,
        keys: &[Vec<i64>; 3],
        album_artists: bool,
        folder: Option<FolderKey>,
    ) -> LibraryResult<SearchResults> {
        let items = keys[0]
            .iter()
            .map(|key| Item::Track {
                key: TrackKey::from_raw(*key),
                entry: None,
            })
            .chain(
                keys[1]
                    .iter()
                    .map(|key| Item::Album(AlbumKey::from_raw(*key))),
            )
            .chain(keys[2].iter().map(|key| Item::Artist {
                key: ArtistKey::from_raw(*key),
                album_artist: album_artists,
            }))
            .collect::<Vec<_>>();
        // Each row counts in music folder `folder` as the one rule narrows its kind.
        let shape = crate::read::Shape {
            folder,
            ..crate::read::Shape::of(None)
        };
        let rows = self.rows_of_on(connection, &items, &shape, true).await?;
        let mut results = SearchResults::default();
        for row in rows {
            match row {
                Row::Track(row) => results.tracks.push(row),
                Row::Album(row) => results.albums.push(row),
                Row::Artist(row) => results.artists.push(row),
                _ => {}
            }
        }
        Ok(results)
    }

    /// `results` read again from the current Catalog: the same rows with their current
    /// totals, favorites, ratings and downloads.
    pub async fn reread_search(
        &self,
        results: &SearchResults,
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<SearchResults> {
        let mut connection = self.acquire_loaded(cancellation).await?;
        let keys = [
            results
                .tracks
                .iter()
                .map(|row| row.track_key.raw())
                .collect(),
            results
                .albums
                .iter()
                .map(|row| row.album_key.raw())
                .collect(),
            results
                .artists
                .iter()
                .map(|row| row.artist_key.raw())
                .collect(),
        ];
        let album_artists = results.artists.iter().any(|row| row.album_artist);
        let mut reread = self
            .search_rows(&mut connection, &keys, album_artists, folder)
            .await?;
        reread.lyrics.clone_from(&results.lyrics);
        Ok(reread)
    }

    /// Tracks, albums and artists of a source whose texts hold every search term.
    pub async fn search(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        album_artists: bool,
        request: &SearchRequest,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<SearchResults> {
        if request.query.is_empty() {
            return Ok(SearchResults::default());
        }
        // A one-letter term matches a whole word; a longer one matches anywhere.
        let terms = request
            .query
            .split_whitespace()
            .map(|term| (term, term.chars().count() == 1))
            .collect::<Vec<_>>();
        let tracks = MemberRead {
            folder,
            ..MemberRead::new(Collection::Source(source))
        };
        let tracks = self.order(&Read::Members(tracks), cancellation).await?;
        let albums = ListRead {
            folder,
            ..ListRead::new(ListKind::Albums, Some(source), LibraryField::Title)
        };
        let albums = self.order(&Read::List(albums), cancellation).await?;
        let artists = ListRead {
            folder,
            ..ListRead::new(
                ListKind::Artists {
                    album_artist: album_artists,
                },
                Some(source),
                LibraryField::Title,
            )
        };
        let artists = self.order(&Read::List(artists), cancellation).await?;
        let keys = {
            let snapshot = self.snapshot();
            let context = Context::new(&snapshot);
            let all = |holds: &dyn Fn(&str, bool) -> bool| {
                terms.iter().all(|(term, word)| holds(term, *word))
            };
            let found = |order: &crate::Order, kept: &dyn Fn(u32) -> bool| match &order.0.members {
                Members::Tracks(keys) | Members::Keys(keys) => keys
                    .iter()
                    .filter(|key| kept(**key))
                    .map(|key| i64::from(*key))
                    .take(request.limit)
                    .collect(),
                _ => Vec::new(),
            };
            [
                found(&tracks, &|key| {
                    snapshot.tracks.get(key.into()).is_some_and(|record| {
                        all(&|term, word| context.track_holds(record, term, word))
                    })
                }),
                found(&albums, &|key| {
                    snapshot.albums.get(key.into()).is_some_and(|record| {
                        all(&|term, word| context.album_holds(record, term, word))
                    })
                }),
                found(&artists, &|key| {
                    snapshot.artists.get(key.into()).is_some_and(|record| {
                        all(&|term, word| holds(snapshot.text(record.name), term, word))
                    })
                }),
            ]
        };
        let mut connection = self.acquire_loaded(cancellation).await?;
        self.search_rows(&mut connection, &keys, album_artists, folder)
            .await
    }
}
