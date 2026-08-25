//! Stores opaque accepted artwork bindings and pages distinct bindings for preparation.
//! Fetching, disk caching, decoding, and fallback choice remain outside Library.

use sqlx::{Connection, FromRow};

use crate::{
    AlbumKey, ArtistKey, Database, FolderKey, GenreKey, LibraryError, LibraryResult, PlaylistKey,
    ReadCancellation, SourceKey, TrackKey,
};

const ARTWORK_PAGE_LIMIT: usize = 128;
const ARTWORK_BINDING_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct ArtworkPreparationPage {
    pub artwork_digest: [u8; 32],
    pub bindings: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct LocalAlbumArtworkCandidate {
    pub album_key: crate::AlbumKey,
    pub media_uri: String,
}

impl Database {
    pub async fn local_album_artwork_candidates(
        &self,
        source: SourceKey,
        after: Option<crate::AlbumKey>,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<LocalAlbumArtworkCandidate>> {
        let limit = limit.clamp(1, ARTWORK_PAGE_LIMIT) as i64;
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let rows = sqlx::query_as::<_, LocalAlbumArtworkCandidate>(
            "SELECT album.album_key,
                    (SELECT track.media_uri FROM tracks track
                     WHERE track.album_key=album.album_key
                       AND track.media_uri LIKE 'file://%'
                     ORDER BY track.disc_number,track.track_number,track.track_key LIMIT 1) media_uri
             FROM albums album
             WHERE album.source_key=?1 AND album.album_key>?2
               AND album.artwork_binding IS NULL
               AND EXISTS (SELECT 1 FROM tracks track WHERE track.album_key=album.album_key AND track.media_uri LIKE 'file://%')
             ORDER BY album.album_key LIMIT ?3",
        )
        .bind(source)
        .bind(after.map_or(0, crate::AlbumKey::raw))
        .bind(limit)
        .fetch_all(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(rows?)
    }

    pub async fn track_artwork_binding(
        &self,
        source: SourceKey,
        key: TrackKey,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<Vec<u8>>> {
        read_binding(
            self,
            "SELECT artwork_binding FROM tracks WHERE source_key=?1 AND track_key=?2",
            source,
            key.raw(),
            cancellation,
        )
        .await
    }
    pub async fn album_artwork_binding(
        &self,
        source: SourceKey,
        key: AlbumKey,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<Vec<u8>>> {
        read_binding(
            self,
            "SELECT artwork_binding FROM albums WHERE source_key=?1 AND album_key=?2",
            source,
            key.raw(),
            cancellation,
        )
        .await
    }
    pub async fn artist_artwork_binding(
        &self,
        source: SourceKey,
        key: ArtistKey,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<Vec<u8>>> {
        read_binding(
            self,
            "SELECT artwork_binding FROM artists WHERE source_key=?1 AND artist_key=?2",
            source,
            key.raw(),
            cancellation,
        )
        .await
    }
    pub async fn genre_artwork_binding(
        &self,
        source: SourceKey,
        key: GenreKey,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<Vec<u8>>> {
        read_binding(
            self,
            "SELECT artwork_binding FROM genres WHERE source_key=?1 AND genre_key=?2",
            source,
            key.raw(),
            cancellation,
        )
        .await
    }
    pub async fn folder_artwork_binding(
        &self,
        source: SourceKey,
        key: FolderKey,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<Vec<u8>>> {
        read_binding(
            self,
            "SELECT artwork_binding FROM folders WHERE source_key=?1 AND folder_key=?2",
            source,
            key.raw(),
            cancellation,
        )
        .await
    }
    pub async fn playlist_artwork_binding(
        &self,
        source: SourceKey,
        key: PlaylistKey,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<Vec<u8>>> {
        read_binding(
            self,
            "SELECT artwork_binding FROM playlists WHERE source_key=?1 AND playlist_key=?2",
            source,
            key.raw(),
            cancellation,
        )
        .await
    }

    pub async fn write_track_artwork_binding(
        &self,
        source: SourceKey,
        key: TrackKey,
        binding: Option<&[u8]>,
        digest: [u8; 32],
    ) -> LibraryResult<bool> {
        write_binding(self, "UPDATE tracks SET artwork_binding=?3 WHERE source_key=?1 AND track_key=?2 AND artwork_binding IS NOT ?3", source, key.raw(), binding, digest).await
    }
    pub async fn write_album_artwork_binding(
        &self,
        source: SourceKey,
        key: AlbumKey,
        binding: Option<&[u8]>,
        digest: [u8; 32],
    ) -> LibraryResult<bool> {
        write_binding(self, "UPDATE albums SET artwork_binding=?3 WHERE source_key=?1 AND album_key=?2 AND artwork_binding IS NOT ?3", source, key.raw(), binding, digest).await
    }
    pub async fn write_artist_artwork_binding(
        &self,
        source: SourceKey,
        key: ArtistKey,
        binding: Option<&[u8]>,
        digest: [u8; 32],
    ) -> LibraryResult<bool> {
        write_binding(self, "UPDATE artists SET artwork_binding=?3 WHERE source_key=?1 AND artist_key=?2 AND artwork_binding IS NOT ?3", source, key.raw(), binding, digest).await
    }
    pub async fn write_genre_artwork_binding(
        &self,
        source: SourceKey,
        key: GenreKey,
        binding: Option<&[u8]>,
        digest: [u8; 32],
    ) -> LibraryResult<bool> {
        write_binding(self, "UPDATE genres SET artwork_binding=?3 WHERE source_key=?1 AND genre_key=?2 AND artwork_binding IS NOT ?3", source, key.raw(), binding, digest).await
    }
    pub async fn write_folder_artwork_binding(
        &self,
        source: SourceKey,
        key: FolderKey,
        binding: Option<&[u8]>,
        digest: [u8; 32],
    ) -> LibraryResult<bool> {
        write_binding(self, "UPDATE folders SET artwork_binding=?3 WHERE source_key=?1 AND folder_key=?2 AND artwork_binding IS NOT ?3", source, key.raw(), binding, digest).await
    }
    pub async fn write_playlist_artwork_binding(
        &self,
        source: SourceKey,
        key: PlaylistKey,
        binding: Option<&[u8]>,
        digest: [u8; 32],
    ) -> LibraryResult<bool> {
        write_binding(self, "UPDATE playlists SET artwork_binding=?3 WHERE source_key=?1 AND playlist_key=?2 AND artwork_binding IS NOT ?3", source, key.raw(), binding, digest).await
    }

    pub async fn artwork_preparation_page(
        &self,
        source: SourceKey,
        after_binding: Option<&[u8]>,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<ArtworkPreparationPage> {
        let limit = limit.clamp(1, ARTWORK_PAGE_LIMIT) as i64;
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let digest = sqlx::query_scalar::<_, Vec<u8>>(
            "SELECT artwork_digest FROM sources WHERE source_key=?1",
        )
        .bind(source)
        .fetch_one(&mut *transaction)
        .await?;
        let bindings = sqlx::query_scalar::<_, Vec<u8>>("SELECT binding FROM (SELECT artwork_binding binding FROM tracks WHERE source_key=?1 AND artwork_binding IS NOT NULL AND (?2 IS NULL OR artwork_binding>?2) UNION SELECT artwork_binding FROM albums WHERE source_key=?1 AND artwork_binding IS NOT NULL AND (?2 IS NULL OR artwork_binding>?2) UNION SELECT artwork_binding FROM artists WHERE source_key=?1 AND artwork_binding IS NOT NULL AND (?2 IS NULL OR artwork_binding>?2) UNION SELECT artwork_binding FROM genres WHERE source_key=?1 AND artwork_binding IS NOT NULL AND (?2 IS NULL OR artwork_binding>?2) UNION SELECT artwork_binding FROM folders WHERE source_key=?1 AND artwork_binding IS NOT NULL AND (?2 IS NULL OR artwork_binding>?2) UNION SELECT artwork_binding FROM playlists WHERE source_key=?1 AND artwork_binding IS NOT NULL AND (?2 IS NULL OR artwork_binding>?2) UNION SELECT artwork_binding FROM home_entries WHERE source_key=?1 AND artwork_binding IS NOT NULL AND (?2 IS NULL OR artwork_binding>?2) UNION SELECT fallback_artwork_binding FROM queue_occurrences WHERE source_key=?1 AND fallback_artwork_binding IS NOT NULL AND (?2 IS NULL OR fallback_artwork_binding>?2)) ORDER BY binding LIMIT ?3")
            .bind(source).bind(after_binding).bind(limit).fetch_all(&mut *transaction).await?;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(ArtworkPreparationPage {
            artwork_digest: digest.try_into().map_err(|_| {
                LibraryError::InvalidStore("source artwork digest is not 32 bytes".to_string())
            })?,
            bindings,
        })
    }
}

async fn read_binding(
    database: &Database,
    sql: &'static str,
    source: SourceKey,
    key: i64,
    cancellation: &ReadCancellation,
) -> LibraryResult<Option<Vec<u8>>> {
    let (_permit, mut connection) = database.acquire_general(cancellation).await?;
    let result = sqlx::query_scalar(sql)
        .bind(source)
        .bind(key)
        .fetch_optional(&mut *connection)
        .await?;
    Database::clear_progress(&mut connection).await?;
    Ok(result.flatten())
}

async fn write_binding(
    database: &Database,
    sql: &'static str,
    source: SourceKey,
    key: i64,
    binding: Option<&[u8]>,
    digest: [u8; 32],
) -> LibraryResult<bool> {
    if binding.is_some_and(|value| value.len() > ARTWORK_BINDING_BYTES) {
        return Err(LibraryError::InvalidRequest(
            "artwork binding is too large".to_string(),
        ));
    }
    let mut writer = database.writer().await?;
    let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
    let mut transaction = connection.begin().await?;
    let changed = sqlx::query(sql)
        .bind(source)
        .bind(key)
        .bind(binding)
        .execute(&mut *transaction)
        .await?
        .rows_affected()
        == 1;
    if changed {
        sqlx::query("UPDATE sources SET artwork_digest=?2 WHERE source_key=?1")
            .bind(source)
            .bind(digest.as_slice())
            .execute(&mut *transaction)
            .await?;
    }
    transaction.commit().await?;
    Ok(changed)
}
