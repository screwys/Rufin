//! Owns bounded cached Search and complete route-filter orders over normalized facts.
//! Callers receive final rows and never merge related Store results.

use sqlx::Connection;

use crate::{
    AlbumView, ArtistView, CatalogQuery, CatalogRows, CatalogSort, CatalogSubject, Database,
    FolderKey, LibraryResult, ReadCancellation, SourceKey, TrackScope, TrackView,
};

const SEARCH_LIMIT: usize = 100;
const SEARCH_TEXT_LIMIT: usize = 256;

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
}

impl Database {
    pub async fn search_rows_by_objects(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
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
        let mut result = SearchResults::default();
        for (kind, objects) in [
            (crate::CatalogKind::Track, track_objects),
            (crate::CatalogKind::Album, album_objects),
            (crate::CatalogKind::Artist, artist_objects),
        ] {
            let (table, name) = kind.sql();
            let keys=sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT entity.{name}_key FROM json_each(?1) requested JOIN {table} entity ON entity.object_id=requested.value WHERE entity.source_key=?2 ORDER BY CAST(requested.key AS INTEGER)"))).bind(serde_json::to_string(objects)?).bind(source).fetch_all(&mut *transaction).await?;
            let mut query = CatalogQuery::keys(kind, Some(source), Vec::new());
            query.folder = folder;
            if let CatalogSubject::Keys { album_artist, .. } = &mut query.subject {
                *album_artist = album_artists;
            }
            add_rows(
                &mut result,
                crate::query::load_keys_on(&mut transaction, &query, keys).await?,
            );
        }
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
        let mut result = SearchResults::default();
        for subject in [
            CatalogSubject::Tracks {
                scope: TrackScope::Tracks(source),
                sort: Some(CatalogSort::Title),
            },
            CatalogSubject::Albums {
                source,
                sort: CatalogSort::Title,
                inline_tracks: None,
            },
            CatalogSubject::Artists {
                source,
                sort: CatalogSort::Title,
                album_artist: album_artists,
            },
        ] {
            let query = CatalogQuery {
                subject,
                folder,
                filter: String::new(),
                descending: false,
                favorites_only: false,
                downloaded_only: false,
                activity: false,
            };
            let mut relation = crate::query::compile_on(&mut transaction, &query).await?;
            let text = match query.subject {
                CatalogSubject::Tracks { .. } => "track.normalized_search",
                CatalogSubject::Albums { .. } => {
                    "ordering.title_search||' '||COALESCE(ordering.artist_search,'')"
                }
                _ => "artist.normalized_name",
            };
            let mut condition = term_match(text);
            if matches!(query.subject, CatalogSubject::Albums { .. }) {
                condition = format!(
                    "({condition} OR CAST(ordering.year_sort AS TEXT)=term.value OR EXISTS(SELECT 1 FROM effective_album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key=album.album_key AND {}) OR EXISTS(SELECT 1 FROM effective_album_genres credit JOIN genres genre USING(genre_key) WHERE credit.album_key=album.album_key AND {}))",
                    term_match("artist.normalized_name"),
                    term_match("genre.normalized_name")
                );
            }
            relation.predicate.push_str(&format!(" AND NOT EXISTS(SELECT 1 FROM json_each({terms}) term WHERE NOT COALESCE({condition},0))"));
            let keys = crate::query::selected_values_on(
                &mut transaction,
                &relation,
                &relation.key,
                std::slice::from_ref(&(0..request.limit)),
            )
            .await?;
            add_rows(
                &mut result,
                crate::query::load_keys_on(&mut transaction, &query, keys).await?,
            );
        }
        transaction.commit().await?;
        Ok(result)
    }
}

fn term_match(text: &str) -> String {
    format!(
        "CASE WHEN length(term.value)=1 THEN instr(' '||{text}||' ',' '||term.value||' ') ELSE instr({text},term.value) END>0"
    )
}

fn add_rows(result: &mut SearchResults, rows: CatalogRows) {
    match rows {
        CatalogRows::Tracks(rows) => result.tracks = rows,
        CatalogRows::Albums(rows) => result.albums = rows,
        CatalogRows::Artists(rows) => result.artists = rows,
        _ => unreachable!(),
    }
}
