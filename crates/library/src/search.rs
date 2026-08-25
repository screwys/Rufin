//! Owns bounded cached Search and complete route-filter orders over normalized facts.
//! Callers receive final rows and never merge related Store results.

use sqlx::Connection;

use crate::{
    AlbumKey, AlbumRow, ArtistKey, ArtistRow, Database, FolderKey, GenreKey, LibraryResult,
    MoodKey, PlaylistKey, ReadCancellation, SourceKey, TrackKey, TrackRow,
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
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let track_keys = sqlx::query_scalar::<_, TrackKey>(
            "SELECT track_key FROM tracks
             WHERE source_key=?1 AND (instr(normalized_search, ?2)>0 OR CAST(year AS TEXT)=?2)
               AND (?4 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=tracks.track_key AND scope.folder_key=?4))
             ORDER BY sort_text, track_key LIMIT ?3",
        )
        .bind(source)
        .bind(&request.query)
        .bind(request.limit as i64)
        .bind(folder)
        .fetch_all(&mut *transaction)
        .await?;
        let tracks = load_track_rows(&mut transaction, source, &track_keys).await?;
        let album_keys = sqlx::query_scalar::<_, AlbumKey>(
            "SELECT album.album_key FROM albums AS album
             WHERE album.source_key=?1 AND (instr(album.normalized_title || ' ' || lower(album.display_artist), ?2)>0 OR CAST(album.year AS TEXT)=?2 OR EXISTS (SELECT 1 FROM album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key=album.album_key AND instr(artist.normalized_name,?2)>0) OR EXISTS (SELECT 1 FROM album_genres credit JOIN genres genre USING(genre_key) WHERE credit.album_key=album.album_key AND instr(genre.normalized_name,?2)>0))
               AND (?4 IS NULL OR EXISTS (SELECT 1 FROM tracks item JOIN track_folders scope USING(track_key) WHERE item.album_key=album.album_key AND scope.folder_key=?4))
             ORDER BY album.sort_text, album.album_key LIMIT ?3",
        )
        .bind(source)
        .bind(&request.query)
        .bind(request.limit as i64)
        .bind(folder)
        .fetch_all(&mut *transaction)
        .await?;
        let albums = load_album_rows(&mut transaction, source, &album_keys, folder).await?;
        let artist_keys = sqlx::query_scalar::<_, ArtistKey>(
            "SELECT artist.artist_key FROM artists artist
             WHERE artist.source_key=?1 AND instr(artist.normalized_name,?2)>0
               AND ((?5=0 AND EXISTS (SELECT 1 FROM track_artists credit WHERE credit.artist_key=artist.artist_key)) OR (?5=1 AND EXISTS (SELECT 1 FROM album_artists credit WHERE credit.artist_key=artist.artist_key)))
               AND (?4 IS NULL OR (?5=0 AND EXISTS (SELECT 1 FROM track_artists credit JOIN track_folders scope USING(track_key) WHERE credit.artist_key=artist.artist_key AND scope.folder_key=?4)) OR (?5=1 AND EXISTS (SELECT 1 FROM album_artists credit JOIN tracks track USING(album_key) JOIN track_folders scope USING(track_key) WHERE credit.artist_key=artist.artist_key AND scope.folder_key=?4)))
             ORDER BY artist.sort_text,artist.artist_key LIMIT ?3",
        )
        .bind(source)
        .bind(&request.query)
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

    pub async fn track_filter_order(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        query: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackKey>> {
        let query = normalized_filter(query);
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_scalar::<_, TrackKey>(
            "SELECT track_key FROM tracks
             WHERE source_key=?1 AND (instr(normalized_search, ?2)>0 OR CAST(year AS TEXT)=?2)
               AND (?3 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=tracks.track_key AND scope.folder_key=?3))
             ORDER BY sort_text, track_key",
        )
        .bind(source)
        .bind(query)
        .bind(folder)
        .fetch_all(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn album_filter_order(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        query: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<AlbumKey>> {
        let query = normalized_filter(query);
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_scalar::<_, AlbumKey>(
            "SELECT album.album_key FROM albums album
             WHERE album.source_key=?1 AND (instr(album.normalized_title || ' ' || lower(album.display_artist), ?2)>0 OR CAST(album.year AS TEXT)=?2 OR EXISTS (SELECT 1 FROM album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key=album.album_key AND instr(artist.normalized_name,?2)>0) OR EXISTS (SELECT 1 FROM album_genres credit JOIN genres genre USING(genre_key) WHERE credit.album_key=album.album_key AND instr(genre.normalized_name,?2)>0))
               AND (?3 IS NULL OR EXISTS (SELECT 1 FROM tracks item JOIN track_folders scope USING(track_key) WHERE item.album_key=album.album_key AND scope.folder_key=?3))
             ORDER BY sort_text, album_key",
        )
        .bind(source)
        .bind(query)
        .bind(folder)
        .fetch_all(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn artist_filter_order(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        album_artists: bool,
        query: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<ArtistKey>> {
        let query = normalized_filter(query);
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_scalar::<_, ArtistKey>(
            "SELECT artist.artist_key FROM artists artist
             WHERE artist.source_key=?1 AND instr(artist.normalized_name, ?2)>0
               AND ((?4=0 AND EXISTS (SELECT 1 FROM track_artists credit WHERE credit.artist_key=artist.artist_key)) OR (?4=1 AND EXISTS (SELECT 1 FROM album_artists credit WHERE credit.artist_key=artist.artist_key)))
               AND (?3 IS NULL OR (?4=0 AND EXISTS (SELECT 1 FROM track_artists credit JOIN track_folders scope USING(track_key) WHERE credit.artist_key=artist.artist_key AND scope.folder_key=?3)) OR (?4=1 AND EXISTS (SELECT 1 FROM album_artists credit JOIN tracks track USING(album_key) JOIN track_folders scope USING(track_key) WHERE credit.artist_key=artist.artist_key AND scope.folder_key=?3)))
             ORDER BY sort_text, artist_key",
        )
        .bind(source)
        .bind(query)
        .bind(folder)
        .bind(album_artists)
        .fetch_all(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn genre_filter_order(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        query: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<GenreKey>> {
        let query = normalized_filter(query);
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_scalar::<_, GenreKey>(
            "SELECT genre.genre_key FROM genres genre
             WHERE genre.source_key=?1 AND instr(genre.normalized_name, ?2)>0
               AND (?3 IS NULL OR EXISTS (SELECT 1 FROM track_genres credit JOIN track_folders scope USING(track_key) WHERE credit.genre_key=genre.genre_key AND scope.folder_key=?3))
             ORDER BY sort_text, genre_key",
        )
        .bind(source)
        .bind(query)
        .bind(folder)
        .fetch_all(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn mood_filter_order(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        query: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<MoodKey>> {
        let query = normalized_filter(query);
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_scalar::<_, MoodKey>(
            "SELECT mood.mood_key FROM moods mood
             WHERE mood.source_key=?1 AND instr(mood.normalized_name, ?2)>0
               AND (?3 IS NULL OR EXISTS (SELECT 1 FROM track_moods credit JOIN track_folders scope USING(track_key) WHERE credit.mood_key=mood.mood_key AND scope.folder_key=?3))
             ORDER BY sort_text, mood_key",
        )
        .bind(source)
        .bind(query)
        .bind(folder)
        .fetch_all(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn playlist_filter_order(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        query: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<PlaylistKey>> {
        let query = normalized_filter(query);
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_scalar::<_, PlaylistKey>(
            "SELECT playlist.playlist_key FROM playlists playlist
             WHERE playlist.source_key=?1 AND instr(playlist.normalized_name, ?2)>0
               AND (?3 IS NULL OR EXISTS (SELECT 1 FROM playlist_entries entry JOIN track_folders scope USING(track_key) WHERE entry.playlist_key=playlist.playlist_key AND scope.folder_key=?3))
             ORDER BY sort_text, playlist_key",
        )
        .bind(source)
        .bind(query)
        .bind(folder)
        .fetch_all(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }
}

fn normalized_filter(query: &str) -> String {
    query
        .trim()
        .to_lowercase()
        .chars()
        .take(SEARCH_TEXT_LIMIT)
        .collect()
}
