//! Owns bounded cached Search and complete route-filter orders over normalized facts.
//! Callers receive final rows and never merge related Store results.

use sqlx::{Connection, QueryBuilder, Sqlite, SqliteConnection};

use crate::{
    AlbumKey, AlbumRow, ArtistKey, ArtistRow, Database, FolderKey, LibraryResult, ReadCancellation,
    SourceKey, TrackKey, TrackRow,
    collections::{load_album_rows, load_artist_rows},
    tracks::load_track_rows,
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
    pub tracks: Vec<TrackRow>,
    pub albums: Vec<AlbumRow>,
    pub artists: Vec<ArtistRow>,
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
                "Search identity page exceeds 100 rows".to_string(),
            ));
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let track_keys = object_track_keys(&mut transaction, source, track_objects).await?;
        let album_keys = object_album_keys(&mut transaction, source, album_objects).await?;
        let artist_keys = object_artist_keys(&mut transaction, source, artist_objects).await?;
        let tracks = load_track_rows(&mut transaction, source, &track_keys).await?;
        let albums = load_album_rows(&mut transaction, source, &album_keys, folder).await?;
        let artists = load_artist_rows(
            &mut transaction,
            source,
            &artist_keys,
            album_artists,
            folder,
        )
        .await?;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(SearchResults {
            tracks,
            albums,
            artists,
        })
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
        let terms_json = serde_json::to_string(&terms)?;
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let track_keys = sqlx::query_scalar::<_, TrackKey>(
            "SELECT track_key FROM tracks
             WHERE source_key=?1 AND NOT EXISTS (
               SELECT 1 FROM json_each(?2) term
               WHERE CASE WHEN length(term.value)=1
                     THEN instr(' '||normalized_search||' ',' '||term.value||' ')
                     ELSE instr(normalized_search,term.value) END=0
                 AND CAST(year AS TEXT)<>term.value
             )
               AND (?4 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=tracks.track_key AND scope.folder_key=?4))
             ORDER BY sort_text, track_key LIMIT ?3",
        )
        .bind(source)
        .bind(&terms_json)
        .bind(request.limit as i64)
        .bind(folder)
        .fetch_all(&mut *transaction)
        .await?;
        let tracks = load_track_rows(&mut transaction, source, &track_keys).await?;
        let album_keys = sqlx::query_scalar::<_, AlbumKey>(
            "SELECT album.album_key FROM albums AS album
             WHERE album.source_key=?1 AND NOT EXISTS (
               SELECT 1 FROM json_each(?2) term WHERE
                 CASE WHEN length(term.value)=1
                   THEN instr(' '||album.normalized_title || ' ' || lower(album.display_artist)||' ',' '||term.value||' ')
                   ELSE instr(album.normalized_title || ' ' || lower(album.display_artist),term.value) END=0
                 AND CAST(album.year AS TEXT)<>term.value
                 AND NOT EXISTS (SELECT 1 FROM album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key=album.album_key AND CASE WHEN length(term.value)=1 THEN instr(' '||artist.normalized_name||' ',' '||term.value||' ') ELSE instr(artist.normalized_name,term.value) END>0)
                 AND NOT EXISTS (SELECT 1 FROM album_genres credit JOIN genres genre USING(genre_key) WHERE credit.album_key=album.album_key AND CASE WHEN length(term.value)=1 THEN instr(' '||genre.normalized_name||' ',' '||term.value||' ') ELSE instr(genre.normalized_name,term.value) END>0)
             )
               AND (?4 IS NULL OR EXISTS (SELECT 1 FROM tracks item JOIN track_folders scope USING(track_key) WHERE item.album_key=album.album_key AND scope.folder_key=?4))
             ORDER BY album.sort_text, album.album_key LIMIT ?3",
        )
        .bind(source)
        .bind(&terms_json)
        .bind(request.limit as i64)
        .bind(folder)
        .fetch_all(&mut *transaction)
        .await?;
        let albums = load_album_rows(&mut transaction, source, &album_keys, folder).await?;
        let artist_keys = sqlx::query_scalar::<_, ArtistKey>(
            "SELECT artist.artist_key FROM artists artist
             WHERE artist.source_key=?1 AND NOT EXISTS (
               SELECT 1 FROM json_each(?2) term WHERE CASE WHEN length(term.value)=1
                 THEN instr(' '||artist.normalized_name||' ',' '||term.value||' ')
                 ELSE instr(artist.normalized_name,term.value) END=0
             )
               AND ((?5=0 AND EXISTS (SELECT 1 FROM track_artists credit WHERE credit.artist_key=artist.artist_key)) OR (?5=1 AND EXISTS (SELECT 1 FROM album_artists credit WHERE credit.artist_key=artist.artist_key)))
               AND (?4 IS NULL OR (?5=0 AND EXISTS (SELECT 1 FROM track_artists credit JOIN track_folders scope USING(track_key) WHERE credit.artist_key=artist.artist_key AND scope.folder_key=?4)) OR (?5=1 AND EXISTS (SELECT 1 FROM album_artists credit JOIN tracks track USING(album_key) JOIN track_folders scope USING(track_key) WHERE credit.artist_key=artist.artist_key AND scope.folder_key=?4)))
             ORDER BY artist.sort_text,artist.artist_key LIMIT ?3",
        )
        .bind(source)
        .bind(&terms_json)
        .bind(request.limit as i64)
        .bind(folder)
        .bind(album_artists)
        .fetch_all(&mut *transaction)
        .await?;
        let artists = load_artist_rows(
            &mut transaction,
            source,
            &artist_keys,
            album_artists,
            folder,
        )
        .await?;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(SearchResults {
            tracks,
            albums,
            artists,
        })
    }
}

async fn object_track_keys(
    connection: &mut SqliteConnection,
    source: SourceKey,
    objects: &[String],
) -> LibraryResult<Vec<TrackKey>> {
    object_keys(connection, source, objects, "tracks", "track_key").await
}

async fn object_album_keys(
    connection: &mut SqliteConnection,
    source: SourceKey,
    objects: &[String],
) -> LibraryResult<Vec<AlbumKey>> {
    if objects.is_empty() {
        return Ok(Vec::new());
    }
    let mut query = object_key_query(objects, "albums", "album_key");
    query.push_bind(source).push(" ORDER BY requested.position");
    Ok(query
        .build_query_scalar()
        .persistent(false)
        .fetch_all(connection)
        .await?)
}

async fn object_artist_keys(
    connection: &mut SqliteConnection,
    source: SourceKey,
    objects: &[String],
) -> LibraryResult<Vec<ArtistKey>> {
    if objects.is_empty() {
        return Ok(Vec::new());
    }
    let mut query = object_key_query(objects, "artists", "artist_key");
    query.push_bind(source).push(" ORDER BY requested.position");
    Ok(query
        .build_query_scalar()
        .persistent(false)
        .fetch_all(connection)
        .await?)
}

async fn object_keys(
    connection: &mut SqliteConnection,
    source: SourceKey,
    objects: &[String],
    table: &'static str,
    key: &'static str,
) -> LibraryResult<Vec<TrackKey>> {
    if objects.is_empty() {
        return Ok(Vec::new());
    }
    let mut query = object_key_query(objects, table, key);
    query.push_bind(source).push(" ORDER BY requested.position");
    Ok(query
        .build_query_scalar()
        .persistent(false)
        .fetch_all(connection)
        .await?)
}

fn object_key_query(
    objects: &[String],
    table: &'static str,
    key: &'static str,
) -> QueryBuilder<Sqlite> {
    let mut query = QueryBuilder::<Sqlite>::new("WITH requested(object_id,position) AS (");
    query.push_values(
        objects.iter().cloned().enumerate(),
        |mut row, (position, object)| {
            row.push_bind(object).push_bind(position as i64);
        },
    );
    query
        .push(") SELECT entity.")
        .push(key)
        .push(" FROM requested JOIN ")
        .push(table)
        .push(" entity ON entity.object_id=requested.object_id WHERE entity.source_key=");
    query
}
