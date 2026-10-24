//! Owns bounded cached Search and complete route-filter orders over normalized facts.
//! Callers receive final rows and never merge related Store results.

use serde::{Deserialize, Serialize};
use sqlx::Connection;
use std::collections::BTreeMap;

use crate::query::{OrderedQuery, RequestedKeys};
use crate::{
    AlbumView, ArtistView, CatalogSort, Collection, Database, FolderKey, LibraryResult, ListKind,
    ListRead, MemberRead, ReadCancellation, SourceKey, TrackView,
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

impl Database {
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
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let request = |kind: crate::CatalogKind,
                       objects: &[String]|
         -> LibraryResult<RequestedKeys> {
            let (table, name) = kind.sql();
            let batch = RequestedKeys::batch(objects)?;
            Ok(RequestedKeys {
                sql: format!(
                    "SELECT entity.{name}_key value,request.ordinal FROM ({}) request JOIN {table} entity ON entity.source_key={} AND entity.object_id=request.value",
                    batch.sql,
                    source.raw()
                ),
                arguments: batch.arguments,
            })
        };
        let result = SearchResults {
            lyrics: BTreeMap::new(),
            tracks: crate::tracks::load_track_requested(
                &mut transaction,
                request(crate::CatalogKind::Track, track_objects)?,
            )
            .await?,
            albums: crate::collections::load_album_requested(
                &mut transaction,
                request(crate::CatalogKind::Album, album_objects)?,
                false,
            )
            .await?,
            artists: crate::collections::load_artist_requested(
                &mut transaction,
                request(crate::CatalogKind::Artist, artist_objects)?,
                album_artists,
                false,
            )
            .await?,
        };
        transaction.commit().await?;
        Ok(result)
    }

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
        let terms = request
            .query
            .split_whitespace()
            .filter(|term| !term.is_empty())
            .collect::<Vec<_>>();
        let terms = crate::query::quote(&serde_json::to_string(&terms)?);
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let mut tracks = MemberRead::new(Collection::Source(source));
        tracks.folder = folder;
        let tracks = crate::query::member_query_on(&mut transaction, &tracks).await?;
        let mut albums = ListRead::new(ListKind::Albums, Some(source), CatalogSort::Title);
        albums.folder = folder;
        let albums = crate::collections::collection_query(&albums);
        let mut artists = ListRead::new(
            ListKind::Artists {
                album_artist: album_artists,
            },
            Some(source),
            CatalogSort::Title,
        );
        artists.folder = folder;
        let artists = crate::collections::collection_query(&artists);
        let album_condition = format!(
            "({} OR CAST(album.year_sort AS TEXT)=term.value OR EXISTS(SELECT 1 FROM album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key=album.album_key AND {}) OR EXISTS(SELECT 1 FROM album_genres credit JOIN genres genre USING(genre_key) WHERE credit.album_key=album.album_key AND {}))",
            term_match("album.title_search||' '||COALESCE(album.artist_search,'')"),
            term_match("artist.normalized_name"),
            term_match("genre.normalized_name")
        );
        let request = |mut relation: OrderedQuery, condition: String| {
            relation.predicate.push_str(&format!(" AND NOT EXISTS(SELECT 1 FROM json_each({terms}) term WHERE NOT COALESCE({condition},0))"));
            relation.requested_keys(0, request.limit)
        };
        let result = SearchResults {
            lyrics: BTreeMap::new(),
            tracks: crate::tracks::load_track_requested(
                &mut transaction,
                request(tracks, term_match("track.normalized_search")),
            )
            .await?,
            albums: crate::collections::load_album_requested(
                &mut transaction,
                request(albums, album_condition),
                false,
            )
            .await?,
            artists: crate::collections::load_artist_requested(
                &mut transaction,
                request(artists, term_match("artist.normalized_name")),
                album_artists,
                false,
            )
            .await?,
        };
        transaction.commit().await?;
        Ok(result)
    }
}

fn term_match(text: &str) -> String {
    format!(
        "CASE WHEN length(term.value)=1 THEN instr(' '||{text}||' ',' '||term.value||' ') ELSE instr({text},term.value) END>0"
    )
}
