//! Persists bounded Local observations, dependency paths, and point playback access.
//! Walking, parsing, retries, and filesystem policy remain owned by Sources.

use std::collections::BTreeMap;

use sqlx::{Connection, FromRow, QueryBuilder, Sqlite};

use crate::{
    Database, LibraryError, LibraryResult, LocalAccessFileKey, LocalFileKey, ReadCancellation,
    SourceKey, TrackKey, loudness::recompute_album_loudness_key,
};

const LOCAL_FILE_PAGE_LIMIT: usize = 128;

#[derive(Debug, FromRow)]
pub struct DownloadMetadata {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub disc_number: i64,
    pub track_number: i64,
    pub duration_millis: i64,
    pub source_format: Option<String>,
    pub loudness_analysis_key: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalFileKind {
    Media,
    Cue,
    Image,
    Directory,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalFileState {
    Accepted,
    Rejected,
    Unreadable,
    Observed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LocalFileWrite {
    pub path: String,
    pub root: String,
    pub relative_path: String,
    pub kind: LocalFileKind,
    pub size_bytes: Option<i64>,
    pub mtime_ns: i64,
    pub device_id: Option<i64>,
    pub inode: Option<i64>,
    pub parse_version: Option<i64>,
    pub state: LocalFileState,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LocalFileRow {
    pub local_file_key: LocalFileKey,
    pub path: String,
    pub root: String,
    pub relative_path: String,
    pub kind: LocalFileKind,
    pub size_bytes: Option<i64>,
    pub mtime_ns: i64,
    pub device_id: Option<i64>,
    pub inode: Option<i64>,
    pub parse_version: Option<i64>,
    pub state: LocalFileState,
    pub dependencies: Vec<String>,
    pub track_object_id: Option<String>,
}

#[derive(Clone, Debug, FromRow)]
struct LocalFileScalar {
    local_file_key: LocalFileKey,
    path: String,
    root: String,
    relative_path: String,
    kind: String,
    size_bytes: Option<i64>,
    mtime_ns: i64,
    device_id: Option<i64>,
    inode: Option<i64>,
    parse_version: Option<i64>,
    state: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, FromRow)]
pub struct LocalLocatorWrite {
    pub source_id: Option<String>,
    pub media_uri: String,
    pub origin: String,
    pub path: String,
    pub root: String,
    pub relative_path: String,
    pub access_uri: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LocalAccessWrite {
    pub media_uri: String,
    pub origin: LocalAccessOrigin,
    pub path: String,
    pub root: String,
    pub relative_path: String,
    pub size_bytes: i64,
    pub mtime_ns: i64,
    pub device_id: Option<i64>,
    pub inode: Option<i64>,
    pub parser_version: i64,
    pub title: String,
    pub album: String,
    pub artist: String,
    pub disc_number: i64,
    pub track_number: i64,
    pub duration_millis: i64,
    pub access_uri: String,
    pub loudness_analysis_key: Option<[u8; 32]>,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct LocalAccessRow {
    pub local_access_file_key: LocalAccessFileKey,
    pub media_uri: String,
    pub origin: LocalAccessOrigin,
    pub path: String,
    pub root: String,
    pub relative_path: String,
    pub size_bytes: i64,
    pub mtime_ns: i64,
    pub device_id: Option<i64>,
    pub inode: Option<i64>,
    pub parser_version: i64,
    pub title: String,
    pub album: String,
    pub artist: String,
    pub disc_number: i64,
    pub track_number: i64,
    pub duration_millis: i64,
    pub access_uri: String,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct MappingTrackRow {
    pub track_key: TrackKey,
    pub object_id: String,
    pub media_uri: String,
    pub source_path: String,
    pub title: String,
    pub album: String,
    pub artist: String,
    pub disc_number: i64,
    pub track_number: i64,
    pub duration_millis: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
pub enum LocalAccessOrigin {
    Local,
    Mapping,
    Download,
    Import,
}

impl LocalAccessOrigin {
    fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Mapping => "mapping",
            Self::Download => "download",
            Self::Import => "import",
        }
    }
}

impl LocalFileKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Media => "media",
            Self::Cue => "cue",
            Self::Image => "image",
            Self::Directory => "directory",
        }
    }
    fn parse(value: &str) -> LibraryResult<Self> {
        match value {
            "media" => Ok(Self::Media),
            "cue" => Ok(Self::Cue),
            "image" => Ok(Self::Image),
            "directory" => Ok(Self::Directory),
            _ => Err(LibraryError::InvalidStore(
                "invalid Local file kind".to_string(),
            )),
        }
    }
}

impl LocalFileState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Unreadable => "unreadable",
            Self::Observed => "observed",
        }
    }
    fn parse(value: &str) -> LibraryResult<Self> {
        match value {
            "accepted" => Ok(Self::Accepted),
            "rejected" => Ok(Self::Rejected),
            "unreadable" => Ok(Self::Unreadable),
            "observed" => Ok(Self::Observed),
            _ => Err(LibraryError::InvalidStore(
                "invalid Local file state".to_string(),
            )),
        }
    }
}

impl Database {
    pub async fn downloaded_count(&self, source_id: Option<&str>) -> LibraryResult<usize> {
        let (_permit, mut connection) = self.acquire_general(&ReadCancellation::new()).await?;
        let count = if let Some(source_id) = source_id {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM main.local_locators
                 WHERE source_key=(SELECT source_key FROM main.source_ids WHERE object_id=?1)
                   AND origin='download'",
            )
            .bind(source_id)
            .fetch_one(&mut *connection)
            .await
        } else {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM main.local_locators
                 WHERE source_key IS NULL AND origin='download'",
            )
            .fetch_one(&mut *connection)
            .await
        };
        Database::clear_progress(&mut connection).await?;
        Ok(usize::try_from(count?).unwrap_or_default())
    }

    pub async fn download_metadata(
        &self,
        media_uri: &str,
    ) -> LibraryResult<Option<DownloadMetadata>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_as(
            "SELECT title,display_artist artist,display_album album,disc_number,track_number,
                    duration_millis,source_format,loudness_analysis_key
             FROM tracks WHERE media_uri=?1",
        )
        .bind(media_uri)
        .fetch_optional(&mut *connection)
        .await?)
    }

    pub async fn playback_access_uri(&self, media_uri: &str) -> LibraryResult<Option<String>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar(
            "SELECT access_uri FROM local_access_files WHERE media_uri=?1
             ORDER BY CASE origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END,
                      local_access_file_key LIMIT 1",
        )
        .bind(media_uri)
        .fetch_optional(&mut *connection)
        .await?)
    }

    pub async fn source_counts(
        &self,
        source: SourceKey,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<(usize, usize)> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let(album,track)=sqlx::query_as::<_,(i64,i64)>("SELECT (SELECT count(*) FROM albums WHERE source_key=?1),(SELECT count(*) FROM tracks WHERE source_key=?1)").bind(source).fetch_one(&mut *connection).await?;
        Database::clear_progress(&mut connection).await?;
        Ok((
            usize::try_from(album).unwrap_or_default(),
            usize::try_from(track).unwrap_or_default(),
        ))
    }

    pub async fn mapping_formula_match_count(
        &self,
        source: SourceKey,
        root: &str,
        server_prefix: Option<&str>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<usize> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let count = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM tracks
             WHERE source_key=?1 AND source_path IS NOT NULL AND (
               (?2 IS NOT NULL AND substr(source_path,1,length(?2))=?2)
               OR (?2 IS NULL AND (
                 (substr(source_path,1,1)<>'/' AND substr(source_path,2,1)<>':' AND substr(source_path,1,2)<>char(92)||char(92))
                 OR substr(source_path,1,length(?3))=?3
               ))
             )",
        )
        .bind(source)
        .bind(server_prefix)
        .bind(root)
        .fetch_one(&mut *connection)
        .await?;
        Database::clear_progress(&mut connection).await?;
        Ok(usize::try_from(count).unwrap_or_default())
    }

    pub async fn clear_mapping_access(&self, source: SourceKey) -> LibraryResult<u64> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        sqlx::query("DELETE FROM catalog.local_access_metadata WHERE access_uri IN (SELECT access_uri FROM local_access_files WHERE source_key=?1 AND origin='mapping')").bind(source).execute(&mut *connection).await?;
        let removed =
            sqlx::query("DELETE FROM main.local_locators WHERE local_access_file_key IN (SELECT local_access_file_key FROM local_access_files WHERE source_key=?1 AND origin='mapping')")
                .bind(source)
                .execute(&mut *connection)
                .await?
                .rows_affected();
        sqlx::query("UPDATE tracks SET loudness_analysis_key=COALESCE((SELECT access.loudness_analysis_key FROM local_access_files access WHERE access.media_uri=tracks.media_uri ORDER BY CASE access.origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END,access.local_access_file_key LIMIT 1),source_loudness_analysis_key) WHERE source_key=?1")
            .bind(source).execute(&mut *connection).await?;
        Ok(removed)
    }

    pub async fn mapping_track_page(
        &self,
        source: SourceKey,
        after: Option<TrackKey>,
        source_path: Option<&str>,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<MappingTrackRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let rows = sqlx::query_as::<_, MappingTrackRow>(
            "SELECT track_key,object_id,media_uri,source_path,title,display_album album,display_artist artist,disc_number,track_number,duration_millis
             FROM tracks WHERE source_key=?1 AND source_path IS NOT NULL
               AND ((?3 IS NULL AND track_key>?2) OR source_path=?3)
             ORDER BY track_key LIMIT ?4",
        ).bind(source).bind(after.map_or(0, TrackKey::raw)).bind(source_path).bind(limit.clamp(1,128) as i64)
        .fetch_all(&mut *connection).await;
        Database::clear_progress(&mut connection).await?;
        Ok(rows?)
    }

    pub async fn mapping_track_source_path(
        &self,
        source: SourceKey,
        track: TrackKey,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<String>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let path = sqlx::query_scalar(
            "SELECT source_path FROM tracks
             WHERE source_key=?1 AND track_key=?2 AND source_path IS NOT NULL",
        )
        .bind(source)
        .bind(track)
        .fetch_optional(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(path?)
    }

    pub async fn local_file_page(
        &self,
        source: SourceKey,
        after: Option<LocalFileKey>,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<LocalFileRow>> {
        let limit = limit.clamp(1, LOCAL_FILE_PAGE_LIMIT) as i64;
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let scalars = sqlx::query_as::<_, LocalFileScalar>("SELECT local_file_key,path,root,relative_path,kind,size_bytes,mtime_ns,device_id,inode,parse_version,state FROM local_files WHERE source_key=?1 AND local_file_key>?2 ORDER BY local_file_key LIMIT ?3")
            .bind(source).bind(after.map_or(0, LocalFileKey::raw)).bind(limit)
            .fetch_all(&mut *transaction).await?;
        let rows = load_local_file_rows(&mut transaction, scalars).await?;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(rows)
    }

    pub async fn local_file_reuse_candidates(
        &self,
        source: SourceKey,
        observations: &[LocalFileWrite],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<LocalFileRow>> {
        if observations.len() > LOCAL_FILE_PAGE_LIMIT {
            return Err(LibraryError::InvalidRequest(
                "Local reuse batch exceeds 128 files".to_string(),
            ));
        }
        if observations.is_empty() {
            return Ok(Vec::new());
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let mut query =
            QueryBuilder::<Sqlite>::new("WITH requested(path,device_id,inode,position) AS (");
        query.push_values(
            observations.iter().enumerate(),
            |mut row, (position, observation)| {
                row.push_bind(&observation.path)
                    .push_bind(observation.device_id)
                    .push_bind(observation.inode)
                    .push_bind(position as i64);
            },
        );
        query.push(") SELECT file.local_file_key,file.path,file.root,file.relative_path,file.kind,file.size_bytes,file.mtime_ns,file.device_id,file.inode,file.parse_version,file.state FROM requested JOIN local_files file ON file.source_key=")
            .push_bind(source)
            .push(" AND (file.path=requested.path OR (requested.device_id IS NOT NULL AND requested.inode IS NOT NULL AND file.device_id=requested.device_id AND file.inode=requested.inode)) ORDER BY requested.position,CASE WHEN file.path=requested.path THEN 0 ELSE 1 END,file.local_file_key");
        let scalars = query
            .build_query_as::<LocalFileScalar>()
            .persistent(false)
            .fetch_all(&mut *transaction)
            .await?;
        let rows = load_local_file_rows(&mut transaction, scalars).await?;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(rows)
    }

    pub async fn upsert_local_access(
        &self,
        source: Option<SourceKey>,
        access: &LocalAccessWrite,
    ) -> LibraryResult<LocalAccessFileKey> {
        validate_local_access(access)?;
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        sqlx::query("DELETE FROM catalog.local_access_metadata WHERE access_uri IN (SELECT access_uri FROM main.local_locators WHERE media_uri=?1 AND origin=?2 AND path<>?3)").bind(&access.media_uri).bind(access.origin.as_str()).bind(&access.path).execute(&mut *transaction).await?;
        sqlx::query(
            "DELETE FROM main.local_locators WHERE media_uri=?1 AND origin=?2 AND path<>?3",
        )
        .bind(&access.media_uri)
        .bind(access.origin.as_str())
        .bind(&access.path)
        .execute(&mut *transaction)
        .await?;
        let source_id = if let Some(source) = source {
            sqlx::query_scalar::<_, String>(
                "SELECT object_id FROM catalog.sources WHERE source_key=?1",
            )
            .bind(source)
            .fetch_optional(&mut *transaction)
            .await?
        } else {
            None
        };
        let key = write_local_locator(
            &mut transaction,
            &LocalLocatorWrite {
                source_id,
                media_uri: access.media_uri.clone(),
                origin: access.origin.as_str().to_owned(),
                path: access.path.clone(),
                root: access.root.clone(),
                relative_path: access.relative_path.clone(),
                access_uri: access.access_uri.clone(),
            },
        )
        .await?;
        transaction.commit().await?;
        let mut transaction = connection.begin().await?;
        sqlx::query("INSERT INTO catalog.local_access_metadata(access_uri,size_bytes,mtime_ns,
            device_id,inode,parser_version,title,normalized_title,album,normalized_album,artist,normalized_artist,
            disc_number,track_number,duration_millis,loudness_analysis_key)
            VALUES(?1,?2,?3,?4,?5,?6,?7,lower(?7),?8,lower(?8),?9,lower(?9),?10,?11,?12,?13)
            ON CONFLICT(access_uri) DO UPDATE SET size_bytes=excluded.size_bytes,mtime_ns=excluded.mtime_ns,
            device_id=excluded.device_id,inode=excluded.inode,parser_version=excluded.parser_version,
            title=excluded.title,normalized_title=excluded.normalized_title,album=excluded.album,
            normalized_album=excluded.normalized_album,artist=excluded.artist,normalized_artist=excluded.normalized_artist,
            disc_number=excluded.disc_number,track_number=excluded.track_number,duration_millis=excluded.duration_millis,
            loudness_analysis_key=excluded.loudness_analysis_key")
            .bind(&access.access_uri).bind(access.size_bytes).bind(access.mtime_ns).bind(access.device_id).bind(access.inode)
            .bind(access.parser_version).bind(&access.title).bind(&access.album).bind(&access.artist)
            .bind(access.disc_number).bind(access.track_number).bind(access.duration_millis)
            .bind(access.loudness_analysis_key.as_ref().map(|key|key.as_slice()))
            .execute(&mut *transaction).await?;
        if let Some((track, album)) =
            sqlx::query_as::<_, (crate::TrackKey, Option<crate::AlbumKey>)>(
                "SELECT track_key,album_key FROM tracks WHERE media_uri=?1",
            )
            .bind(&access.media_uri)
            .fetch_optional(&mut *transaction)
            .await?
        {
            sqlx::query("UPDATE tracks SET loudness_analysis_key=COALESCE(?2,loudness_analysis_key) WHERE track_key=?1")
                .bind(track)
                .bind(access.loudness_analysis_key.as_ref().map(|key| key.as_slice()))
                .execute(&mut *transaction)
                .await?;
            if let Some(album) = album {
                recompute_album_loudness_key(&mut transaction, album).await?;
            }
        }
        transaction.commit().await?;
        Ok(key)
    }

    pub async fn original_file_path(&self, media_uri: &str) -> LibraryResult<Option<String>> {
        let (_permit, mut connection) = self.acquire_general(&ReadCancellation::new()).await?;
        Ok(sqlx::query_scalar("SELECT path FROM main.local_locators WHERE media_uri=?1 AND origin<>'download' ORDER BY CASE origin WHEN 'mapping' THEN 0 ELSE 1 END,local_access_file_key LIMIT 1")
            .bind(media_uri).fetch_optional(&mut *connection).await?)
    }

    pub async fn resolve_local_access(
        &self,
        source: SourceKey,
        media_uri: &str,
        title: &str,
        album: &str,
        artist: &str,
        disc_number: i64,
        track_number: i64,
        duration_millis: i64,
    ) -> LibraryResult<Option<LocalAccessRow>> {
        let mut connection = self.acquire_reader().await?;
        if let Some(row) = sqlx::query_as::<_, LocalAccessRow>("SELECT local_access_file_key,media_uri,origin,path,root,relative_path,size_bytes,mtime_ns,device_id,inode,parser_version,title,album,artist,disc_number,track_number,duration_millis,access_uri FROM local_access_files WHERE media_uri=?1 ORDER BY CASE origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END,local_access_file_key LIMIT 1")
            .bind(media_uri).fetch_optional(&mut *connection).await? {
            return Ok(Some(row));
        }
        Ok(sqlx::query_as::<_, LocalAccessRow>("SELECT locator.local_access_file_key,locator.media_uri,locator.origin,locator.path,locator.root,locator.relative_path,metadata.size_bytes,metadata.mtime_ns,metadata.device_id,metadata.inode,metadata.parser_version,metadata.title,metadata.album,metadata.artist,metadata.disc_number,metadata.track_number,metadata.duration_millis,locator.access_uri FROM catalog.local_access_metadata metadata JOIN local_access_files locator USING(access_uri) WHERE locator.source_key=?1 AND metadata.normalized_title=lower(?2) AND metadata.normalized_album=lower(?3) AND metadata.normalized_artist=lower(?4) AND metadata.disc_number=?5 AND metadata.track_number=?6 AND metadata.duration_millis=?7 ORDER BY CASE locator.origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END,locator.local_access_file_key LIMIT 1")
            .bind(source).bind(title).bind(album).bind(artist).bind(disc_number)
            .bind(track_number).bind(duration_millis).fetch_optional(&mut *connection).await?)
    }

    pub async fn retaining_download_rows(
        &self,
        media_uris: &[String],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<LocalAccessRow>> {
        if media_uris.len() > LOCAL_FILE_PAGE_LIMIT {
            return Err(LibraryError::InvalidRequest(format!(
                "Local access reads are limited to {LOCAL_FILE_PAGE_LIMIT} media URIs"
            )));
        }
        if media_uris.is_empty() {
            return Ok(Vec::new());
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut query = QueryBuilder::<Sqlite>::new("WITH requested(media_uri,ordinal) AS (");
        query.push_values(media_uris.iter().enumerate(), |mut row, (ordinal, uri)| {
            row.push_bind(uri).push_bind(ordinal as i64);
        });
        query.push(
            ") SELECT access.local_access_file_key,access.media_uri,access.origin,
                            access.path,access.root,access.relative_path,access.size_bytes,
                            access.mtime_ns,access.device_id,access.inode,access.parser_version,
                            access.title,access.album,access.artist,access.disc_number,
                            access.track_number,access.duration_millis,access.access_uri
                     FROM requested JOIN local_access_files access ON access.local_access_file_key=(
                       SELECT min(candidate.local_access_file_key)
                       FROM local_access_files candidate
                       WHERE candidate.media_uri=requested.media_uri
                         AND candidate.origin='download'
                     ) ORDER BY requested.ordinal",
        );
        let result = query
            .build_query_as::<LocalAccessRow>()
            .persistent(false)
            .fetch_all(&mut *connection)
            .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn remove_local_access(&self, key: LocalAccessFileKey) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let removed_access = sqlx::query_scalar::<_, String>(
            "SELECT media_uri FROM local_access_files WHERE local_access_file_key=?1",
        )
        .bind(key)
        .fetch_optional(&mut *transaction)
        .await?;
        sqlx::query("DELETE FROM catalog.local_access_metadata WHERE access_uri=(SELECT access_uri FROM main.local_locators WHERE local_access_file_key=?1)")
            .bind(key)
            .execute(&mut *transaction)
            .await?;
        let removed = sqlx::query("DELETE FROM main.local_locators WHERE local_access_file_key=?1")
            .bind(key)
            .execute(&mut *transaction)
            .await?
            .rows_affected()
            == 1;
        if let Some(media_uri) = removed_access.as_deref()
            && let Some((track, album)) =
                sqlx::query_as::<_, (crate::TrackKey, Option<crate::AlbumKey>)>(
                    "SELECT track_key,album_key FROM tracks WHERE media_uri=?1",
                )
                .bind(media_uri)
                .fetch_optional(&mut *transaction)
                .await?
        {
            sqlx::query("UPDATE tracks SET loudness_analysis_key=COALESCE((SELECT access.loudness_analysis_key FROM local_access_files access WHERE access.media_uri=?2 ORDER BY CASE access.origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END,access.local_access_file_key LIMIT 1),source_loudness_analysis_key) WHERE track_key=?1")
                .bind(track).bind(media_uri).execute(&mut *transaction).await?;
            if let Some(album) = album {
                recompute_album_loudness_key(&mut transaction, album).await?;
            }
        }
        transaction.commit().await?;
        Ok(removed)
    }
}

async fn load_local_file_rows(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    scalars: Vec<LocalFileScalar>,
) -> LibraryResult<Vec<LocalFileRow>> {
    let mut dependencies = BTreeMap::<LocalFileKey, Vec<String>>::new();
    let mut track_objects = BTreeMap::<LocalFileKey, String>::new();
    if !scalars.is_empty() {
        let mut query = QueryBuilder::<Sqlite>::new("WITH requested(local_file_key,position) AS (");
        query.push_values(scalars.iter().enumerate(), |mut row, (position, file)| {
            row.push_bind(file.local_file_key)
                .push_bind(position as i64);
        });
        query.push(") SELECT dependency.local_file_key,dependency.dependency_path FROM requested JOIN local_file_dependencies dependency USING(local_file_key) ORDER BY requested.position,dependency.position");
        for (file, dependency) in query
            .build_query_as::<(LocalFileKey, String)>()
            .persistent(false)
            .fetch_all(&mut **transaction)
            .await?
        {
            dependencies.entry(file).or_default().push(dependency);
        }
        let mut query = QueryBuilder::<Sqlite>::new("WITH requested(local_file_key,path) AS (");
        query.push_values(&scalars, |mut row, file| {
            row.push_bind(file.local_file_key).push_bind(&file.path);
        });
        query.push(") SELECT requested.local_file_key,min(track.object_id) FROM requested JOIN tracks track ON track.media_uri='file://'||requested.path OR track.cue_path=requested.path GROUP BY requested.local_file_key");
        for (file, object_id) in query
            .build_query_as::<(LocalFileKey, String)>()
            .persistent(false)
            .fetch_all(&mut **transaction)
            .await?
        {
            track_objects.insert(file, object_id);
        }
    }
    scalars
        .into_iter()
        .map(|scalar| {
            Ok(LocalFileRow {
                local_file_key: scalar.local_file_key,
                path: scalar.path,
                root: scalar.root,
                relative_path: scalar.relative_path,
                kind: LocalFileKind::parse(&scalar.kind)?,
                size_bytes: scalar.size_bytes,
                mtime_ns: scalar.mtime_ns,
                device_id: scalar.device_id,
                inode: scalar.inode,
                parse_version: scalar.parse_version,
                state: LocalFileState::parse(&scalar.state)?,
                dependencies: dependencies
                    .remove(&scalar.local_file_key)
                    .unwrap_or_default(),
                track_object_id: track_objects.remove(&scalar.local_file_key),
            })
        })
        .collect()
}

fn validate_local_access(access: &LocalAccessWrite) -> LibraryResult<()> {
    if access.path.is_empty()
        || access.root.is_empty()
        || access.media_uri.is_empty()
        || access.access_uri.is_empty()
        || access.size_bytes < 0
        || access.parser_version < 1
        || access.disc_number < 0
        || access.track_number < 0
        || access.duration_millis < 0
    {
        return Err(LibraryError::InvalidRequest(
            "invalid Local access file".to_string(),
        ));
    }
    Ok(())
}

impl Database {
    pub async fn import_local_locators_jsonl(
        &self,
        input: impl std::io::BufRead,
    ) -> LibraryResult<u64> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let count = import_local_locators_jsonl_on(&mut transaction, input).await?;
        transaction.commit().await?;
        Ok(count)
    }
}

pub(crate) async fn write_local_locator(
    connection: &mut sqlx::SqliteConnection,
    locator: &LocalLocatorWrite,
) -> LibraryResult<LocalAccessFileKey> {
    let source = match &locator.source_id {
        Some(id) => Some(crate::db::write_source_identity(connection, id).await?),
        None => None,
    };
    Ok(sqlx::query_scalar("INSERT INTO main.local_locators(source_key,media_uri,origin,path,root,relative_path,access_uri) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(source_key,path) DO UPDATE SET media_uri=excluded.media_uri,origin=excluded.origin,root=excluded.root,relative_path=excluded.relative_path,access_uri=excluded.access_uri ON CONFLICT(media_uri,origin) DO UPDATE SET source_key=excluded.source_key,path=excluded.path,root=excluded.root,relative_path=excluded.relative_path,access_uri=excluded.access_uri RETURNING local_access_file_key")
        .bind(source).bind(&locator.media_uri).bind(&locator.origin).bind(&locator.path).bind(&locator.root).bind(&locator.relative_path).bind(&locator.access_uri).fetch_one(connection).await?)
}

pub(crate) async fn export_local_locators_jsonl_on(
    connection: &mut sqlx::SqliteConnection,
    mut output: impl std::io::Write,
) -> LibraryResult<u64> {
    use futures_util::TryStreamExt;
    output.write_all(b"{\"version\":1}\n")?;
    let mut rows = sqlx::query_as::<_, LocalLocatorWrite>("SELECT source.object_id source_id,locator.media_uri,locator.origin,locator.path,locator.root,locator.relative_path,locator.access_uri FROM main.local_locators locator LEFT JOIN main.source_ids source USING(source_key) WHERE locator.origin<>'download' ORDER BY locator.local_access_file_key").fetch(&mut *connection);
    let mut count = 0;
    while let Some(row) = rows.try_next().await? {
        serde_json::to_writer(&mut output, &row)?;
        output.write_all(b"\n")?;
        count += 1;
    }
    Ok(count)
}
pub(crate) async fn import_local_locators_jsonl_on(
    connection: &mut sqlx::SqliteConnection,
    input: impl std::io::BufRead,
) -> LibraryResult<u64> {
    read_local_locators(connection, input, false).await
}

pub(crate) async fn import_local_locators_jsonl_preserving_downloads_on(
    connection: &mut sqlx::SqliteConnection,
    input: impl std::io::BufRead,
) -> LibraryResult<u64> {
    read_local_locators(connection, input, true).await
}

async fn read_local_locators(
    connection: &mut sqlx::SqliteConnection,
    input: impl std::io::BufRead,
    preserve_downloads: bool,
) -> LibraryResult<u64> {
    let mut lines = input.lines();
    let header = lines
        .next()
        .transpose()?
        .ok_or_else(|| LibraryError::InvalidRequest("missing locator format version".into()))?;
    if serde_json::from_str::<serde_json::Value>(&header)?
        .get("version")
        .and_then(serde_json::Value::as_u64)
        != Some(1)
    {
        return Err(LibraryError::InvalidRequest(
            "unsupported locator format version".into(),
        ));
    }
    let mut count = 0;
    for line in lines {
        let locator = serde_json::from_str::<LocalLocatorWrite>(&line?)?;
        if preserve_downloads && sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM main.local_locators WHERE origin='download' AND (media_uri=?1 OR path=?2))").bind(&locator.media_uri).bind(&locator.path).fetch_one(&mut *connection).await? { continue; }
        write_local_locator(connection, &locator).await?;
        count += 1;
    }
    Ok(count)
}

impl Database {
    pub async fn playlist_file_uri_page(
        &self,
        playlist: crate::PlaylistKey,
        after: i64,
    ) -> LibraryResult<Vec<(i64, String)>> {
        let (_permit, mut connection) = self.acquire_general(&ReadCancellation::new()).await?;
        Ok(sqlx::query_as("SELECT position,media_uri FROM playlist_entries WHERE playlist_key=?1 AND position>?2 AND media_uri LIKE 'file:%' ORDER BY position LIMIT 128").bind(playlist).bind(after).fetch_all(&mut *connection).await?)
    }
    pub async fn import_local_playlist_paths(
        &self,
        source_id: &str,
        playlist: crate::PlaylistKey,
    ) -> LibraryResult<()> {
        let mut after = -1;
        loop {
            let page = self.playlist_file_uri_page(playlist, after).await?;
            if page.is_empty() {
                break;
            }
            after = page.last().unwrap().0;
            let mut writer = self.writer().await?;
            let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
            let mut transaction = connection.begin().await?;
            for (_, uri) in page {
                let Some(path) = crate::file_media_path(&uri).filter(|path| path.is_file()) else {
                    continue;
                };
                let watched: bool =
                    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM tracks WHERE media_uri=?1)")
                        .bind(&uri)
                        .fetch_one(&mut *transaction)
                        .await?;
                if watched {
                    continue;
                }
                write_local_locator(
                    &mut transaction,
                    &LocalLocatorWrite {
                        source_id: Some(source_id.to_owned()),
                        media_uri: uri.clone(),
                        origin: "import".into(),
                        path: path.to_string_lossy().into_owned(),
                        root: String::new(),
                        relative_path: String::new(),
                        access_uri: uri,
                    },
                )
                .await?;
            }
            transaction.commit().await?;
        }
        Ok(())
    }
    pub async fn imported_local_path_page(
        &self,
        source_id: &str,
        after: &str,
    ) -> LibraryResult<Vec<String>> {
        let (_permit, mut connection) = self.acquire_general(&ReadCancellation::new()).await?;
        Ok(sqlx::query_scalar("SELECT locator.path FROM main.local_locators locator JOIN main.source_ids source USING(source_key) WHERE source.object_id=?1 AND locator.origin='import' AND locator.path>?2 AND EXISTS(SELECT 1 FROM playlist_entries entry WHERE entry.media_uri=locator.media_uri) ORDER BY locator.path LIMIT 128").bind(source_id).bind(after).fetch_all(&mut *connection).await?)
    }
}

impl Database {
    pub async fn unreferenced_import_page(
        &self,
        source_id: &str,
    ) -> LibraryResult<Vec<(LocalAccessFileKey, Option<String>, bool)>> {
        let (_permit, mut connection) = self.acquire_general(&ReadCancellation::new()).await?;
        Ok(sqlx::query_as("SELECT locator.local_access_file_key,track.object_id,EXISTS(SELECT 1 FROM local_files file WHERE file.source_key=(SELECT source_key FROM catalog.sources WHERE object_id=source.object_id) AND file.path=locator.path) FROM main.local_locators locator JOIN main.source_ids source USING(source_key) LEFT JOIN tracks track USING(media_uri) WHERE source.object_id=?1 AND locator.origin='import' AND NOT EXISTS(SELECT 1 FROM playlist_entries entry WHERE entry.media_uri=locator.media_uri) AND NOT EXISTS(SELECT 1 FROM user_media_state state WHERE state.media_uri=locator.media_uri AND (state.favorite=1 OR state.rating IS NOT NULL)) ORDER BY locator.local_access_file_key LIMIT 128").bind(source_id).fetch_all(&mut *connection).await?)
    }
}
