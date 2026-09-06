//! Stores opaque accepted artwork bindings and pages distinct bindings for preparation.
//! Fetching, disk caching, decoding, and fallback choice remain outside Library.

use sqlx::{Connection, QueryBuilder, Sqlite};

use crate::{Database, FolderKey, LibraryError, LibraryResult, ReadCancellation, SourceKey};

const ARTWORK_PAGE_LIMIT: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepresentativeArtworkScope {
    AllTracks,
    FavoriteTracks,
    PlaylistTracks,
    LatestAlbums(usize),
}

impl Database {
    pub async fn track_artwork_bindings(
        &self,
        media_uris: &[String],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<Vec<u8>>> {
        if media_uris.len() > ARTWORK_PAGE_LIMIT {
            return Err(LibraryError::InvalidRequest(
                "artwork URI window exceeds 128".into(),
            ));
        }
        let mut connection = tokio::select! {
            result = self.acquire_reader() => result?,
            () = cancellation.cancelled() => return Err(LibraryError::ReadCancelled),
        };
        Ok(sqlx::query_scalar(
            "SELECT track.artwork_binding FROM json_each(?1) requested
             JOIN tracks track ON track.media_uri=requested.value
             WHERE track.artwork_binding IS NOT NULL ORDER BY requested.key",
        )
        .bind(serde_json::to_string(media_uris)?)
        .fetch_all(&mut *connection)
        .await?)
    }

    pub async fn representative_artwork_page(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        scope: RepresentativeArtworkScope,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<Vec<u8>>> {
        let limit = limit.clamp(1, ARTWORK_PAGE_LIMIT) as i64;
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT track.artwork_binding binding
             FROM tracks track
             WHERE track.source_key=",
        );
        query.push_bind(source);
        match scope {
            RepresentativeArtworkScope::AllTracks => {}
            RepresentativeArtworkScope::FavoriteTracks => {
                query.push(" AND COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=track.media_uri),track.source_favorite)=1");
            }
            RepresentativeArtworkScope::PlaylistTracks => {
                query.push(" AND EXISTS (SELECT 1 FROM playlist_entries entry WHERE entry.media_uri=track.media_uri)");
            }
            RepresentativeArtworkScope::LatestAlbums(album_limit) => {
                query.push(" AND track.album_key IN (SELECT latest.album_key FROM albums latest WHERE latest.source_key=").push_bind(source).push(" ORDER BY latest.date_added DESC NULLS LAST,latest.album_key LIMIT ").push_bind(album_limit.clamp(1, 100) as i64).push(")");
            }
        };
        query.push(" AND (").push_bind(folder).push(" IS NULL OR EXISTS (SELECT 1 FROM track_folders folder_scope WHERE folder_scope.track_key=track.track_key AND folder_scope.folder_key=").push_bind(folder).push(")) AND track.artwork_binding IS NOT NULL GROUP BY binding ORDER BY min(track.sort_text),min(track.track_key) LIMIT ").push_bind(limit);
        let bindings = query
            .build_query_scalar::<Vec<u8>>()
            .persistent(false)
            .fetch_all(&mut *connection)
            .await?;
        Database::clear_progress(&mut connection).await?;
        Ok(bindings)
    }

    pub async fn artwork_preparation_page(
        &self,
        source: SourceKey,
        after_binding: Option<&[u8]>,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<Vec<u8>>> {
        let limit = limit.clamp(1, ARTWORK_PAGE_LIMIT) as i64;
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let bindings = sqlx::query_scalar::<_, Vec<u8>>("SELECT binding FROM (SELECT artwork_binding binding FROM tracks WHERE source_key=?1 AND artwork_binding IS NOT NULL AND (?2 IS NULL OR artwork_binding>?2) UNION SELECT artwork_binding FROM albums WHERE source_key=?1 AND artwork_binding IS NOT NULL AND (?2 IS NULL OR artwork_binding>?2) UNION SELECT artwork_binding FROM artists WHERE source_key=?1 AND artwork_binding IS NOT NULL AND (?2 IS NULL OR artwork_binding>?2) UNION SELECT artwork_binding FROM genres WHERE source_key=?1 AND artwork_binding IS NOT NULL AND (?2 IS NULL OR artwork_binding>?2) UNION SELECT artwork_binding FROM folders WHERE source_key=?1 AND artwork_binding IS NOT NULL AND (?2 IS NULL OR artwork_binding>?2) UNION SELECT artwork_binding FROM playlists WHERE source_key=?1 AND artwork_binding IS NOT NULL AND (?2 IS NULL OR artwork_binding>?2)) ORDER BY binding LIMIT ?3")
            .bind(source).bind(after_binding).bind(limit).fetch_all(&mut *transaction).await?;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(bindings)
    }
}
