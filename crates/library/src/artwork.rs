//! Stores opaque accepted artwork bindings and pages distinct bindings for preparation.
//! Fetching, disk caching, decoding, and fallback choice remain outside Library.

use futures_util::TryStreamExt;
use sqlx::{Connection, FromRow, QueryBuilder, Sqlite};
use std::path::{Path, PathBuf};

use crate::{AlbumKey, Database, LibraryError, LibraryResult, ReadCancellation, SourceKey};

const ARTWORK_PAGE_LIMIT: usize = 128;
const ARTWORK_BINDING_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct LocalAlbumArtworkInput {
    pub album_key: AlbumKey,
    pub media_uri: String,
    pub media_size_bytes: Option<i64>,
    pub media_mtime_ns: i64,
    pub sidecar_path: Option<String>,
    pub sidecar_size_bytes: Option<i64>,
    pub sidecar_mtime_ns: Option<i64>,
}

#[derive(FromRow)]
struct LocalAlbumArtworkScalar {
    album_key: AlbumKey,
    media_uri: String,
    media_size_bytes: Option<i64>,
    media_mtime_ns: Option<i64>,
    sidecar_path: Option<String>,
    sidecar_size_bytes: Option<i64>,
    sidecar_mtime_ns: Option<i64>,
}

impl Database {
    pub async fn write_album_artwork_bindings(
        &self,
        source: SourceKey,
        bindings: &[(AlbumKey, Vec<u8>)],
    ) -> LibraryResult<usize> {
        if bindings.len() > ARTWORK_PAGE_LIMIT
            || bindings
                .iter()
                .any(|(_, binding)| binding.len() > ARTWORK_BINDING_BYTES)
        {
            return Err(LibraryError::InvalidRequest(
                "invalid Album artwork batch".to_string(),
            ));
        }
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let mut changed = 0;
        for (album, binding) in bindings {
            changed += sqlx::query("UPDATE albums SET artwork_binding=?3 WHERE source_key=?1 AND album_key=?2 AND artwork_binding IS NOT ?3")
                .bind(source).bind(album).bind(binding).execute(&mut *transaction).await?.rows_affected() as usize;
        }
        transaction.commit().await?;
        Ok(changed)
    }

    pub async fn finalize_artwork_digest(&self, source: SourceKey) -> LibraryResult<[u8; 32]> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut hasher = blake3::Hasher::new();
        {
            let mut rows = sqlx::query_scalar::<_, Vec<u8>>("SELECT binding FROM (SELECT artwork_binding binding FROM tracks WHERE source_key=?1 AND artwork_binding IS NOT NULL UNION SELECT artwork_binding FROM albums WHERE source_key=?1 AND artwork_binding IS NOT NULL UNION SELECT artwork_binding FROM artists WHERE source_key=?1 AND artwork_binding IS NOT NULL UNION SELECT artwork_binding FROM genres WHERE source_key=?1 AND artwork_binding IS NOT NULL UNION SELECT artwork_binding FROM folders WHERE source_key=?1 AND artwork_binding IS NOT NULL UNION SELECT artwork_binding FROM playlists WHERE source_key=?1 AND artwork_binding IS NOT NULL UNION SELECT artwork_binding FROM home_entries WHERE source_key=?1 AND artwork_binding IS NOT NULL) ORDER BY binding").bind(source).fetch(&mut *connection);
            while let Some(binding) = rows.try_next().await? {
                hasher.update(&(binding.len() as u64).to_le_bytes());
                hasher.update(&binding);
            }
        }
        let digest = *hasher.finalize().as_bytes();
        sqlx::query("UPDATE sources SET artwork_digest=?2 WHERE source_key=?1")
            .bind(source)
            .bind(digest.as_slice())
            .execute(connection)
            .await?;
        Ok(digest)
    }

    pub async fn local_album_artwork_page(
        &self,
        source: SourceKey,
        after: Option<crate::AlbumKey>,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<LocalAlbumArtworkInput>> {
        let limit = limit.clamp(1, ARTWORK_PAGE_LIMIT) as i64;
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let candidates = sqlx::query_as::<_, (AlbumKey, String)>(
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
        .fetch_all(&mut *transaction)
        .await?;
        if candidates.is_empty() {
            transaction.commit().await?;
            Database::clear_progress(&mut connection).await?;
            return Ok(Vec::new());
        }
        let mut requested = Vec::with_capacity(candidates.len() * 2);
        for (ordinal, (album, media_uri)) in candidates.iter().enumerate() {
            let path = PathBuf::from(media_uri.strip_prefix("file://").unwrap_or(media_uri));
            let directory = path.parent().map(directory_prefix);
            let parent = path.parent().and_then(Path::parent).map(directory_prefix);
            for (priority, prefix) in directory.into_iter().chain(parent).enumerate() {
                requested.push((
                    *album,
                    media_uri,
                    path.to_string_lossy().into_owned(),
                    prefix,
                    priority as i64,
                    ordinal as i64,
                ));
            }
        }
        let mut query = QueryBuilder::<Sqlite>::new(
            "WITH requested(album_key,media_uri,media_path,prefix,priority,ordinal) AS (",
        );
        query.push_values(
            requested,
            |mut row, (album, uri, path, prefix, priority, ordinal)| {
                row.push_bind(album)
                    .push_bind(uri)
                    .push_bind(path)
                    .push_bind(prefix)
                    .push_bind(priority)
                    .push_bind(ordinal);
            },
        );
        query.push(
            "), scoped AS (
               SELECT requested.*,
                 (SELECT count(DISTINCT track.album_key) FROM tracks track
                  WHERE track.source_key=",
        ).push_bind(source).push(
            " AND track.album_key IS NOT NULL AND track.media_uri>=('file://'||requested.prefix)
                    AND track.media_uri<('file://'||requested.prefix||char(1114111))) album_count
               FROM requested
             ), images AS (
               SELECT scoped.*,image.path sidecar_path,image.size_bytes sidecar_size_bytes,
                 image.mtime_ns sidecar_mtime_ns,
                 CASE
                   WHEN lower(image.path)=lower(prefix||'cover.jpg') THEN 0 WHEN lower(image.path)=lower(prefix||'cover.jpeg') THEN 1
                   WHEN lower(image.path)=lower(prefix||'cover.png') THEN 2 WHEN lower(image.path)=lower(prefix||'cover.webp') THEN 3
                   WHEN lower(image.path)=lower(prefix||'folder.jpg') THEN 4 WHEN lower(image.path)=lower(prefix||'folder.jpeg') THEN 5
                   WHEN lower(image.path)=lower(prefix||'folder.png') THEN 6 WHEN lower(image.path)=lower(prefix||'folder.webp') THEN 7
                   WHEN lower(image.path)=lower(prefix||'front.jpg') THEN 8 WHEN lower(image.path)=lower(prefix||'front.jpeg') THEN 9
                   WHEN lower(image.path)=lower(prefix||'front.png') THEN 10 WHEN lower(image.path)=lower(prefix||'front.webp') THEN 11
                   WHEN lower(image.path)=lower(prefix||'album.jpg') THEN 12 WHEN lower(image.path)=lower(prefix||'album.jpeg') THEN 13
                   WHEN lower(image.path)=lower(prefix||'album.png') THEN 14 WHEN lower(image.path)=lower(prefix||'album.webp') THEN 15
                 END rank,
                 count(image.path) OVER(PARTITION BY scoped.album_key,scoped.priority) image_count
               FROM scoped LEFT JOIN local_files image ON image.source_key=",
        ).push_bind(source).push(
            " AND image.kind='image' AND image.path>=scoped.prefix
                 AND image.path<scoped.prefix||char(1114111)
                 AND instr(substr(image.path,length(scoped.prefix)+1),'/')=0
               WHERE scoped.album_count=1
             ), eligible AS (
               SELECT *,row_number() OVER(PARTITION BY album_key ORDER BY priority,rank NULLS LAST,sidecar_path) choice
               FROM images WHERE sidecar_path IS NOT NULL AND (rank IS NOT NULL OR image_count=1)
             ), albums AS (
               SELECT album_key,media_uri,media_path,min(ordinal) ordinal FROM requested GROUP BY album_key
             )
             SELECT albums.album_key,albums.media_uri,media.size_bytes media_size_bytes,
               media.mtime_ns media_mtime_ns,eligible.sidecar_path,eligible.sidecar_size_bytes,
               eligible.sidecar_mtime_ns
             FROM albums LEFT JOIN local_files media ON media.source_key=",
        ).push_bind(source).push(
            " AND media.path=albums.media_path
             LEFT JOIN eligible ON eligible.album_key=albums.album_key AND eligible.choice=1
             ORDER BY albums.ordinal",
        );
        let rows = query
            .build_query_as::<LocalAlbumArtworkScalar>()
            .persistent(false)
            .fetch_all(&mut *transaction)
            .await?
            .into_iter()
            .map(|row| LocalAlbumArtworkInput {
                album_key: row.album_key,
                media_uri: row.media_uri,
                media_size_bytes: row.media_size_bytes,
                media_mtime_ns: row.media_mtime_ns.unwrap_or_default(),
                sidecar_path: row.sidecar_path,
                sidecar_size_bytes: row.sidecar_size_bytes,
                sidecar_mtime_ns: row.sidecar_mtime_ns,
            })
            .collect();
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(rows)
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
        let bindings = sqlx::query_scalar::<_, Vec<u8>>("SELECT binding FROM (SELECT artwork_binding binding FROM tracks WHERE source_key=?1 AND artwork_binding IS NOT NULL AND (?2 IS NULL OR artwork_binding>?2) UNION SELECT artwork_binding FROM albums WHERE source_key=?1 AND artwork_binding IS NOT NULL AND (?2 IS NULL OR artwork_binding>?2) UNION SELECT artwork_binding FROM artists WHERE source_key=?1 AND artwork_binding IS NOT NULL AND (?2 IS NULL OR artwork_binding>?2) UNION SELECT artwork_binding FROM genres WHERE source_key=?1 AND artwork_binding IS NOT NULL AND (?2 IS NULL OR artwork_binding>?2) UNION SELECT artwork_binding FROM folders WHERE source_key=?1 AND artwork_binding IS NOT NULL AND (?2 IS NULL OR artwork_binding>?2) UNION SELECT artwork_binding FROM playlists WHERE source_key=?1 AND artwork_binding IS NOT NULL AND (?2 IS NULL OR artwork_binding>?2) UNION SELECT artwork_binding FROM home_entries WHERE source_key=?1 AND artwork_binding IS NOT NULL AND (?2 IS NULL OR artwork_binding>?2) UNION SELECT fallback_artwork_binding FROM queue_occurrences WHERE source_key=?1 AND fallback_artwork_binding IS NOT NULL AND (?2 IS NULL OR fallback_artwork_binding>?2)) ORDER BY binding LIMIT ?3")
            .bind(source).bind(after_binding).bind(limit).fetch_all(&mut *transaction).await?;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(bindings)
    }
}

fn directory_prefix(path: &Path) -> String {
    let mut prefix = path
        .to_string_lossy()
        .trim_end_matches(['/', '\\'])
        .to_string();
    prefix.push(std::path::MAIN_SEPARATOR);
    prefix
}
