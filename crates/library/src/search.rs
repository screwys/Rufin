//! Search over the resident Catalog, and the rows of server search results by object id.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::catalog::views::Context;
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
    pub async fn search_rows_by_objects(
        &self,
        source: SourceKey,
        _folder: Option<FolderKey>,
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
            [
                tracks,
                keys(crate::CatalogKind::Album, album_objects),
                keys(crate::CatalogKind::Artist, artist_objects),
            ]
        };
        self.search_rows(&mut connection, &keys, album_artists)
            .await
    }

    /// The rows of search results, in one statement.
    pub(crate) async fn search_rows(
        &self,
        connection: &mut sqlx::SqliteConnection,
        keys: &[Vec<i64>; 3],
        album_artists: bool,
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
        let rows = self
            .rows_of_on(connection, &items, &crate::read::Shape::of(None), true)
            .await?;
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
        let terms = request.query.split_whitespace().collect::<Vec<_>>();
        let matches = |text: &str| terms.iter().all(|term| term_match(text, term));
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
        let keys = |order: &crate::Order| {
            order
                .items(0..order.len())
                .into_iter()
                .filter_map(|item| match item {
                    Item::Track { key, .. } => Some(key.raw()),
                    Item::Album(key) => Some(key.raw()),
                    Item::Artist { key, .. } => Some(key.raw()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let (tracks, albums, artists) = (keys(&tracks), keys(&albums), keys(&artists));
        let keys = {
            let snapshot = self.snapshot();
            let context = Context::new(&snapshot);
            let name = |id| snapshot.text(id).to_lowercase();
            [
                tracks
                    .iter()
                    .filter_map(|key| snapshot.tracks.get(*key))
                    .filter(|record| matches(&context.track_search(record)))
                    .map(|record| i64::from(record.key))
                    .take(request.limit)
                    .collect::<Vec<_>>(),
                albums
                    .iter()
                    .filter_map(|key| snapshot.albums.get(*key))
                    .filter(|album| {
                        let caption = format!(
                            "{} {}",
                            snapshot.text(album.title),
                            snapshot.text(album.artist)
                        )
                        .to_lowercase();
                        let year = (album.year != i16::MIN).then(|| album.year.to_string());
                        let runs = snapshot.album_runs(album);
                        terms.iter().all(|term| {
                            term_match(&caption, term)
                                || year.as_deref() == Some(*term)
                                || runs[0].iter().any(|slot| {
                                    snapshot
                                        .artists
                                        .live(*slot)
                                        .is_some_and(|artist| term_match(&name(artist.name), term))
                                })
                                || runs[1].iter().any(|slot| {
                                    snapshot
                                        .tags
                                        .live(*slot)
                                        .is_some_and(|tag| term_match(&name(tag.name), term))
                                })
                        })
                    })
                    .map(|album| i64::from(album.key))
                    .take(request.limit)
                    .collect(),
                artists
                    .iter()
                    .filter_map(|key| snapshot.artists.get(*key))
                    .filter(|artist| matches(&name(artist.name)))
                    .map(|artist| i64::from(artist.key))
                    .take(request.limit)
                    .collect(),
            ]
        };
        let mut connection = self.acquire_loaded(cancellation).await?;
        self.search_rows(&mut connection, &keys, album_artists)
            .await
    }
}

/// A one-letter term matches a whole word; a longer one matches anywhere.
fn term_match(text: &str, term: &str) -> bool {
    if term.chars().count() == 1 {
        format!(" {text} ").contains(&format!(" {term} "))
    } else {
        text.contains(term)
    }
}
