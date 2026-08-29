//! Persists bounded Local observations, dependency paths, and point playback access.
//! Walking, parsing, retries, and filesystem policy remain owned by Sources.

use std::collections::BTreeMap;

use sqlx::{Connection, FromRow, QueryBuilder, Sqlite};

use crate::{
    Database, LibraryError, LibraryResult, LocalAccessFileKey, LocalFileKey, ReadCancellation,
    SourceKey, TrackKey, loudness::recompute_album_loudness_key,
};

const LOCAL_FILE_PAGE_LIMIT: usize = 128;

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

#[derive(Clone, Debug, PartialEq)]
pub struct LocalAccessWrite {
    pub track_object_id: Option<String>,
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
    pub media_uri: String,
    pub loudness_analysis_key: [u8; 32],
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct LocalAccessRow {
    pub local_access_file_key: LocalAccessFileKey,
    pub track_object_id: Option<String>,
    pub origin: LocalAccessOrigin,
    pub path: String,
    pub root: String,
    pub relative_path: String,
    pub size_bytes: i64,
    pub mtime_ns: i64,
    pub device_id: Option<i64>,
    pub inode: Option<i64>,
    pub parser_version: i64,
    pub media_uri: String,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct MappingTrackRow {
    pub track_key: TrackKey,
    pub object_id: String,
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
}

impl LocalAccessOrigin {
    fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Mapping => "mapping",
            Self::Download => "download",
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

    pub async fn mapping_access_count(
        &self,
        source: SourceKey,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<usize> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let count=sqlx::query_scalar::<_,i64>("SELECT count(DISTINCT track_object_id) FROM local_access_files WHERE source_key=?1 AND origin='mapping' AND track_object_id IS NOT NULL").bind(source).fetch_one(&mut *connection).await?;
        Database::clear_progress(&mut connection).await?;
        Ok(usize::try_from(count).unwrap_or_default())
    }

    pub async fn clear_mapping_access(&self, source: SourceKey) -> LibraryResult<u64> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let removed =
            sqlx::query("DELETE FROM local_access_files WHERE source_key=?1 AND origin='mapping'")
                .bind(source)
                .execute(&mut *connection)
                .await?
                .rows_affected();
        sqlx::query("UPDATE tracks SET loudness_analysis_key=COALESCE((SELECT access.loudness_analysis_key FROM local_access_files access WHERE access.source_key=tracks.source_key AND access.track_object_id=tracks.object_id ORDER BY CASE access.origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END,access.local_access_file_key LIMIT 1),source_loudness_analysis_key) WHERE source_key=?1")
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
            "SELECT track_key,object_id,source_path,title,display_album album,display_artist artist,disc_number,track_number,duration_millis
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

    pub async fn mapping_track_object(
        &self,
        source: SourceKey,
        title: &str,
        album: &str,
        artist: &str,
        disc_number: i64,
        track_number: i64,
        duration_millis: i64,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<String>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result=sqlx::query_scalar("SELECT object_id FROM tracks WHERE source_key=?1 AND normalized_search IS NOT NULL AND lower(title)=lower(?2) AND lower(display_album)=lower(?3) AND lower(display_artist)=lower(?4) AND disc_number=?5 AND track_number=?6 AND abs(duration_millis-?7)<=2000 ORDER BY track_key LIMIT 1")
            .bind(source).bind(title).bind(album).bind(artist).bind(disc_number).bind(track_number).bind(duration_millis).fetch_optional(&mut *connection).await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
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
        source: SourceKey,
        access: &LocalAccessWrite,
    ) -> LibraryResult<LocalAccessFileKey> {
        validate_local_access(access)?;
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        if let Some(track_object_id) = access.track_object_id.as_deref() {
            sqlx::query("DELETE FROM local_access_files WHERE source_key=?1 AND track_object_id=?2 AND origin=?3 AND path<>?4")
                .bind(source).bind(track_object_id).bind(access.origin.as_str()).bind(&access.path).execute(&mut *transaction).await?;
        }
        let key=sqlx::query_scalar::<_, LocalAccessFileKey>("INSERT INTO local_access_files(source_key,track_object_id,origin,path,root,relative_path,size_bytes,mtime_ns,device_id,inode,parser_version,title,normalized_title,album,normalized_album,artist,normalized_artist,disc_number,track_number,duration_millis,media_uri,loudness_analysis_key) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,lower(?12),?13,lower(?13),?14,lower(?14),?15,?16,?17,?18,?19) ON CONFLICT(source_key,path) DO UPDATE SET track_object_id=excluded.track_object_id,origin=excluded.origin,root=excluded.root,relative_path=excluded.relative_path,size_bytes=excluded.size_bytes,mtime_ns=excluded.mtime_ns,device_id=excluded.device_id,inode=excluded.inode,parser_version=excluded.parser_version,title=excluded.title,normalized_title=excluded.normalized_title,album=excluded.album,normalized_album=excluded.normalized_album,artist=excluded.artist,normalized_artist=excluded.normalized_artist,disc_number=excluded.disc_number,track_number=excluded.track_number,duration_millis=excluded.duration_millis,media_uri=excluded.media_uri,loudness_analysis_key=excluded.loudness_analysis_key RETURNING local_access_file_key")
            .bind(source).bind(access.track_object_id.as_deref()).bind(access.origin.as_str()).bind(&access.path).bind(&access.root)
            .bind(&access.relative_path).bind(access.size_bytes).bind(access.mtime_ns)
            .bind(access.device_id).bind(access.inode).bind(access.parser_version)
            .bind(&access.title).bind(&access.album).bind(&access.artist).bind(access.disc_number)
            .bind(access.track_number).bind(access.duration_millis).bind(&access.media_uri)
            .bind(access.loudness_analysis_key.as_slice())
            .fetch_one(&mut *transaction).await?;
        if let Some(track_object_id) = access.track_object_id.as_deref()
            && let Some((track, album)) =
                sqlx::query_as::<_, (crate::TrackKey, Option<crate::AlbumKey>)>(
                    "SELECT track_key,album_key FROM tracks WHERE source_key=?1 AND object_id=?2",
                )
                .bind(source)
                .bind(&track_object_id)
                .fetch_optional(&mut *transaction)
                .await?
        {
            sqlx::query("UPDATE tracks SET loudness_analysis_key=?2 WHERE track_key=?1")
                .bind(track)
                .bind(access.loudness_analysis_key.as_slice())
                .execute(&mut *transaction)
                .await?;
            if let Some(album) = album {
                recompute_album_loudness_key(&mut transaction, album).await?;
            }
        }
        transaction.commit().await?;
        Ok(key)
    }

    pub async fn resolve_local_access(
        &self,
        source: SourceKey,
        track_object_id: Option<&str>,
        title: &str,
        album: &str,
        artist: &str,
        disc_number: i64,
        track_number: i64,
        duration_millis: i64,
    ) -> LibraryResult<Option<LocalAccessRow>> {
        let mut connection = self.acquire_playback().await?;
        if let Some(track_object_id) = track_object_id {
            if let Some(row) = sqlx::query_as::<_, LocalAccessRow>("SELECT local_access_file_key,track_object_id,origin,path,root,relative_path,size_bytes,mtime_ns,device_id,inode,parser_version,media_uri FROM local_access_files WHERE source_key=?1 AND track_object_id=?2 ORDER BY CASE origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END,local_access_file_key LIMIT 1")
                .bind(source).bind(track_object_id).fetch_optional(&mut *connection).await? {
                return Ok(Some(row));
            }
        }
        Ok(sqlx::query_as::<_, LocalAccessRow>("SELECT local_access_file_key,track_object_id,origin,path,root,relative_path,size_bytes,mtime_ns,device_id,inode,parser_version,media_uri FROM local_access_files WHERE source_key=?1 AND normalized_title=lower(?2) AND normalized_album=lower(?3) AND normalized_artist=lower(?4) AND disc_number=?5 AND track_number=?6 AND duration_millis=?7 ORDER BY CASE origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END,local_access_file_key LIMIT 1")
            .bind(source).bind(title).bind(album).bind(artist).bind(disc_number)
            .bind(track_number).bind(duration_millis).fetch_optional(&mut *connection).await?)
    }

    pub async fn remove_local_access(
        &self,
        source: SourceKey,
        key: LocalAccessFileKey,
    ) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let removed_access=sqlx::query_as::<_,(Option<String>,String)>("SELECT track_object_id,media_uri FROM local_access_files WHERE source_key=?1 AND local_access_file_key=?2")
            .bind(source).bind(key).fetch_optional(&mut *transaction).await?;
        let removed = sqlx::query(
            "DELETE FROM local_access_files WHERE source_key=?1 AND local_access_file_key=?2",
        )
        .bind(source)
        .bind(key)
        .execute(&mut *transaction)
        .await?
        .rows_affected()
            == 1;
        let track_object_id = removed_access
            .as_ref()
            .and_then(|(track_object_id, _)| track_object_id.as_deref());
        if removed
            && let Some((track_object_id, removed_media_uri)) =
                removed_access
                    .as_ref()
                    .and_then(|(track_object_id, media_uri)| {
                        track_object_id.as_deref().map(|track| (track, media_uri))
                    })
        {
            let replacement=sqlx::query_scalar::<_,Option<String>>("SELECT COALESCE((SELECT access.media_uri FROM local_access_files access WHERE access.source_key=?1 AND access.track_object_id=?2 ORDER BY CASE access.origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END,access.local_access_file_key LIMIT 1),(SELECT track.media_uri FROM tracks track WHERE track.source_key=?1 AND track.object_id=?2))")
                .bind(source).bind(track_object_id).fetch_one(&mut *transaction).await?;
            sqlx::query("UPDATE queue_occurrences SET fallback_media_uri=?4 WHERE source_key=?1 AND track_object_id=?2 AND fallback_media_uri=?3")
                .bind(source).bind(track_object_id).bind(removed_media_uri).bind(replacement)
                .execute(&mut *transaction).await?;
        }
        if let Some(track_object_id) = track_object_id
            && let Some((track, album)) =
                sqlx::query_as::<_, (crate::TrackKey, Option<crate::AlbumKey>)>(
                    "SELECT track_key,album_key FROM tracks WHERE source_key=?1 AND object_id=?2",
                )
                .bind(source)
                .bind(&track_object_id)
                .fetch_optional(&mut *transaction)
                .await?
        {
            sqlx::query("UPDATE tracks SET loudness_analysis_key=COALESCE((SELECT access.loudness_analysis_key FROM local_access_files access WHERE access.source_key=?2 AND access.track_object_id=?3 ORDER BY CASE access.origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END,access.local_access_file_key LIMIT 1),source_loudness_analysis_key) WHERE track_key=?1")
                .bind(track).bind(source).bind(&track_object_id).execute(&mut *transaction).await?;
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
