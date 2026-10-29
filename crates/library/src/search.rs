//! Search over the resident Catalog, and the rows of server search results by object id.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::catalog::views::Context;
use crate::{
    AlbumKey, AlbumView, ArtistKey, ArtistView, Catalog, CatalogSort, Collection, FolderKey,
    LibraryResult, ListKind, ListRead, MemberRead, ReadCancellation, SourceKey, TrackKey,
    TrackView,
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
    pub tracks: Vec<TrackView>,
    pub albums: Vec<AlbumView>,
    pub artists: Vec<ArtistView>,
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
        let (_permit, mut connection) = self.acquire_loaded(cancellation).await?;
        let mut keys = Vec::new();
        for (table, key, objects) in [
            ("tracks", "track_key", track_objects),
            ("albums", "album_key", album_objects),
            ("artists", "artist_key", artist_objects),
        ] {
            keys.push(sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!(
                "SELECT entity.{key} FROM json_each(?1) requested JOIN {table} entity ON entity.source_key=?2 AND entity.object_id=requested.value ORDER BY CAST(requested.key AS INTEGER)"
            )))
            .bind(serde_json::to_string(objects)?)
            .bind(source)
            .fetch_all(&mut *connection)
            .await?);
        }
        self.search_rows(&mut connection, &keys, album_artists)
            .await
    }

    async fn search_rows(
        &self,
        connection: &mut sqlx::SqliteConnection,
        keys: &[Vec<i64>],
        album_artists: bool,
    ) -> LibraryResult<SearchResults> {
        Ok(SearchResults {
            lyrics: BTreeMap::new(),
            tracks: crate::tracks::load_track_rows(
                self,
                connection,
                &keys[0]
                    .iter()
                    .copied()
                    .map(TrackKey::from_raw)
                    .collect::<Vec<_>>(),
            )
            .await?,
            albums: crate::collections::load_album_rows(
                self,
                connection,
                &keys[1]
                    .iter()
                    .copied()
                    .map(AlbumKey::from_raw)
                    .collect::<Vec<_>>(),
                false,
            )
            .await?,
            artists: crate::collections::load_artist_rows(
                self,
                connection,
                &keys[2]
                    .iter()
                    .copied()
                    .map(ArtistKey::from_raw)
                    .collect::<Vec<_>>(),
                album_artists,
                false,
            )
            .await?,
        })
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
        let (_permit, mut connection) = self.acquire_loaded(cancellation).await?;
        let tracks = MemberRead {
            folder,
            ..MemberRead::new(Collection::Source(source))
        };
        let tracks = self.member_order(&mut connection, &tracks).await?;
        let albums = ListRead {
            folder,
            ..ListRead::new(ListKind::Albums, Some(source), CatalogSort::Title)
        };
        let albums = self.list_order(&mut connection, &albums).await?;
        let artists = ListRead {
            folder,
            ..ListRead::new(
                ListKind::Artists {
                    album_artist: album_artists,
                },
                Some(source),
                CatalogSort::Title,
            )
        };
        let artists = self.list_order(&mut connection, &artists).await?;
        let keys = {
            let snapshot = self.snapshot();
            let context = Context::new(&snapshot, false);
            let name = |id| snapshot.text(id).to_lowercase();
            vec![
                tracks
                    .iter()
                    .map(|member| &snapshot.tracks.records[member.track as usize])
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
