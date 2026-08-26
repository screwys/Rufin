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

#[derive(FromRow)]
struct LocalDependency {
    local_file_key: LocalFileKey,
    dependency_path: String,
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
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<MappingTrackRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let rows = sqlx::query_as::<_, MappingTrackRow>(
            "SELECT track_key,object_id,source_path,title,display_album album,display_artist artist,disc_number,track_number,duration_millis
             FROM tracks WHERE source_key=?1 AND track_key>?2 AND source_path IS NOT NULL
             ORDER BY track_key LIMIT ?3",
        ).bind(source).bind(after.map_or(0, TrackKey::raw)).bind(limit.clamp(1,128) as i64)
        .fetch_all(&mut *connection).await;
        Database::clear_progress(&mut connection).await?;
        Ok(rows?)
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

    pub async fn local_directory_page(
        &self,
        source: SourceKey,
        after: Option<&str>,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<LocalFileRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let scalars = sqlx::query_as::<_, LocalFileScalar>(
            "SELECT local_file_key,path,root,relative_path,kind,size_bytes,mtime_ns,device_id,inode,parse_version,state
             FROM local_files WHERE source_key=?1 AND kind='directory' AND path>?2
             ORDER BY path LIMIT ?3",
        )
        .bind(source)
        .bind(after.unwrap_or(""))
        .bind(limit.clamp(1, LOCAL_FILE_PAGE_LIMIT) as i64)
        .fetch_all(&mut *transaction)
        .await?;
        let rows = load_local_file_rows(&mut transaction, scalars).await?;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(rows)
    }

    /// Returns the bounded persisted Local component touched by exact watcher paths.
    /// Direct observations, CUE owners of changed dependencies, and directory siblings of
    /// changed artwork are resolved from the accepted observation ledger without walking roots.
    pub async fn local_component_file_page(
        &self,
        source: SourceKey,
        paths: &[String],
        image_directories: &[String],
        after: Option<LocalFileKey>,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<LocalFileRow>> {
        if paths.len() > LOCAL_FILE_PAGE_LIMIT {
            return Err(LibraryError::InvalidRequest(
                "Local watcher batch exceeds 128 paths".to_string(),
            ));
        }
        if paths.is_empty() && image_directories.is_empty() {
            return Ok(Vec::new());
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        if image_directories.len() > LOCAL_FILE_PAGE_LIMIT {
            return Err(LibraryError::InvalidRequest(
                "Local artwork directory batch exceeds 128 paths".to_string(),
            ));
        }
        let mut query = QueryBuilder::<Sqlite>::new("WITH requested(path,position) AS (");
        if paths.is_empty() {
            query.push("SELECT NULL,NULL WHERE 0");
        } else {
            query.push_values(paths.iter().enumerate(), |mut row, (position, path)| {
                row.push_bind(path).push_bind(position as i64);
            });
        }
        query.push("), artwork_directory(prefix) AS (");
        if image_directories.is_empty() {
            query.push("SELECT NULL WHERE 0");
        } else {
            query.push_values(image_directories, |mut row, directory| {
                row.push_bind(directory);
            });
        }
        query.push(") SELECT DISTINCT file.local_file_key,file.path,file.root,file.relative_path,file.kind,file.size_bytes,file.mtime_ns,file.device_id,file.inode,file.parse_version,file.state FROM local_files file WHERE file.source_key=")
            .push_bind(source)
            .push(" AND file.local_file_key>")
            .push_bind(after.map_or(0, LocalFileKey::raw))
            .push(" AND (EXISTS(SELECT 1 FROM requested WHERE requested.path=file.path) OR EXISTS(SELECT 1 FROM local_file_dependencies dependency JOIN requested ON requested.path=dependency.dependency_path WHERE dependency.local_file_key=file.local_file_key) OR EXISTS(SELECT 1 FROM artwork_directory WHERE file.path>=artwork_directory.prefix AND file.path<artwork_directory.prefix||char(1114111))) ORDER BY file.local_file_key LIMIT ")
            .push_bind(limit.clamp(1, LOCAL_FILE_PAGE_LIMIT) as i64);
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

    pub async fn local_track_objects_for_paths(
        &self,
        source: SourceKey,
        paths: &[String],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<String>> {
        if paths.len() > LOCAL_FILE_PAGE_LIMIT {
            return Err(LibraryError::InvalidRequest(
                "Local component batch exceeds 128 paths".to_string(),
            ));
        }
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut query = QueryBuilder::<Sqlite>::new("WITH requested(path,position) AS (");
        query.push_values(paths.iter().enumerate(), |mut row, (position, path)| {
            row.push_bind(path).push_bind(position as i64);
        });
        query.push(") SELECT DISTINCT track.object_id FROM requested JOIN tracks track ON track.source_key=")
            .push_bind(source)
            .push(" AND (track.media_uri='file://'||requested.path OR track.cue_path=requested.path) ORDER BY track.object_id");
        let result = query
            .build_query_scalar()
            .persistent(false)
            .fetch_all(&mut *connection)
            .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn local_accepted_paths(
        &self,
        source: SourceKey,
        paths: &[String],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<String>> {
        if paths.len() > LOCAL_FILE_PAGE_LIMIT {
            return Err(LibraryError::InvalidRequest(
                "Local accepted-path batch exceeds 128 files".to_string(),
            ));
        }
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut query = QueryBuilder::<Sqlite>::new("WITH requested(path,position) AS (");
        query.push_values(paths.iter().enumerate(), |mut row, (position, path)| {
            row.push_bind(path).push_bind(position as i64);
        });
        query.push(") SELECT requested.path FROM requested WHERE EXISTS (SELECT 1 FROM tracks track WHERE track.source_key=")
            .push_bind(source)
            .push(" AND (track.media_uri=('file://'||requested.path) OR track.cue_path=requested.path)) ORDER BY requested.position");
        let result = query
            .build_query_scalar()
            .persistent(false)
            .fetch_all(&mut *connection)
            .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn upsert_local_file(
        &self,
        source: SourceKey,
        file: &LocalFileWrite,
        dependencies: &[String],
    ) -> LibraryResult<LocalFileKey> {
        let mut keys = self
            .upsert_local_files(source, &[(file.clone(), dependencies.to_vec())])
            .await?;
        Ok(keys.remove(0))
    }

    pub async fn upsert_local_files(
        &self,
        source: SourceKey,
        files: &[(LocalFileWrite, Vec<String>)],
    ) -> LibraryResult<Vec<LocalFileKey>> {
        if files.len() > LOCAL_FILE_PAGE_LIMIT {
            return Err(LibraryError::InvalidRequest(
                "Local observation batch exceeds 128 files".to_string(),
            ));
        }
        for (file, dependencies) in files {
            validate_local_file(file, dependencies)?;
        }
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let mut keys = Vec::with_capacity(files.len());
        for (file, dependencies) in files {
            let key = sqlx::query_scalar::<_, LocalFileKey>("INSERT INTO local_files(source_key,path,root,relative_path,kind,size_bytes,mtime_ns,device_id,inode,parse_version,state) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11) ON CONFLICT(source_key,path) DO UPDATE SET root=excluded.root,relative_path=excluded.relative_path,kind=excluded.kind,size_bytes=excluded.size_bytes,mtime_ns=excluded.mtime_ns,device_id=excluded.device_id,inode=excluded.inode,parse_version=excluded.parse_version,state=excluded.state RETURNING local_file_key")
                .bind(source).bind(&file.path).bind(&file.root).bind(&file.relative_path)
                .bind(file.kind.as_str()).bind(file.size_bytes).bind(file.mtime_ns)
                .bind(file.device_id).bind(file.inode).bind(file.parse_version)
                .bind(file.state.as_str()).fetch_one(&mut *transaction).await?;
            sqlx::query("DELETE FROM local_file_dependencies WHERE local_file_key=?1")
                .bind(key)
                .execute(&mut *transaction)
                .await?;
            for (position, dependency) in dependencies.iter().enumerate() {
                sqlx::query("INSERT INTO local_file_dependencies(local_file_key,dependency_path,position) VALUES (?1,?2,?3)")
                    .bind(key).bind(dependency).bind(position as i64).execute(&mut *transaction).await?;
            }
            keys.push(key);
        }
        transaction.commit().await?;
        Ok(keys)
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
        let mut dependencies = BTreeMap::<LocalFileKey, Vec<String>>::new();
        if !scalars.is_empty() {
            let mut query =
                QueryBuilder::<Sqlite>::new("WITH requested(local_file_key,position) AS (");
            query.push_values(scalars.iter().enumerate(), |mut row, (position, file)| {
                row.push_bind(file.local_file_key)
                    .push_bind(position as i64);
            });
            query.push(") SELECT dependency.local_file_key,dependency.dependency_path FROM requested JOIN local_file_dependencies dependency USING(local_file_key) ORDER BY requested.position,dependency.position");
            for dependency in query
                .build_query_as::<LocalDependency>()
                .persistent(false)
                .fetch_all(&mut *transaction)
                .await?
            {
                dependencies
                    .entry(dependency.local_file_key)
                    .or_default()
                    .push(dependency.dependency_path);
            }
        }
        let mut rows = Vec::with_capacity(scalars.len());
        for scalar in scalars {
            rows.push(LocalFileRow {
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
            });
        }
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(rows)
    }

    pub async fn local_file_identities(
        &self,
        source: SourceKey,
        identities: &[(i64, i64)],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<LocalFileRow>> {
        if identities.len() > LOCAL_FILE_PAGE_LIMIT {
            return Err(LibraryError::InvalidRequest(
                "Local identity batch exceeds 128 files".to_string(),
            ));
        }
        if identities.is_empty() {
            return Ok(Vec::new());
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let mut query =
            QueryBuilder::<Sqlite>::new("WITH requested(device_id,inode,position) AS (");
        query.push_values(
            identities.iter().enumerate(),
            |mut row, (position, identity)| {
                row.push_bind(identity.0)
                    .push_bind(identity.1)
                    .push_bind(position as i64);
            },
        );
        query.push(") SELECT file.local_file_key,file.path,file.root,file.relative_path,file.kind,file.size_bytes,file.mtime_ns,file.device_id,file.inode,file.parse_version,file.state FROM requested JOIN local_files file ON file.source_key=")
            .push_bind(source)
            .push(" AND file.device_id=requested.device_id AND file.inode=requested.inode ORDER BY requested.position,file.local_file_key");
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

    pub async fn remove_local_file(
        &self,
        source: SourceKey,
        key: LocalFileKey,
    ) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(
            sqlx::query("DELETE FROM local_files WHERE source_key=?1 AND local_file_key=?2")
                .bind(source)
                .bind(key)
                .execute(connection)
                .await?
                .rows_affected()
                == 1,
        )
    }

    pub async fn local_file_identity(
        &self,
        source: SourceKey,
        device_id: i64,
        inode: i64,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<LocalFileRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let scalar = sqlx::query_as::<_, LocalFileScalar>("SELECT local_file_key,path,root,relative_path,kind,size_bytes,mtime_ns,device_id,inode,parse_version,state FROM local_files WHERE source_key=?1 AND device_id=?2 AND inode=?3 ORDER BY local_file_key LIMIT 1")
            .bind(source).bind(device_id).bind(inode).fetch_optional(&mut *connection).await?;
        let result = if let Some(scalar) = scalar {
            let dependencies = sqlx::query_scalar("SELECT dependency_path FROM local_file_dependencies WHERE local_file_key=?1 ORDER BY position")
                .bind(scalar.local_file_key).fetch_all(&mut *connection).await?;
            Some(LocalFileRow {
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
                dependencies,
            })
        } else {
            None
        };
        Database::clear_progress(&mut connection).await?;
        Ok(result)
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
        let track_object_id=sqlx::query_scalar::<_,Option<String>>("SELECT track_object_id FROM local_access_files WHERE source_key=?1 AND local_access_file_key=?2")
            .bind(source).bind(key).fetch_optional(&mut *transaction).await?.flatten();
        let removed = sqlx::query(
            "DELETE FROM local_access_files WHERE source_key=?1 AND local_access_file_key=?2",
        )
        .bind(source)
        .bind(key)
        .execute(&mut *transaction)
        .await?
        .rows_affected()
            == 1;
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
    if !scalars.is_empty() {
        let mut query = QueryBuilder::<Sqlite>::new("WITH requested(local_file_key,position) AS (");
        query.push_values(scalars.iter().enumerate(), |mut row, (position, file)| {
            row.push_bind(file.local_file_key)
                .push_bind(position as i64);
        });
        query.push(") SELECT dependency.local_file_key,dependency.dependency_path FROM requested JOIN local_file_dependencies dependency USING(local_file_key) ORDER BY requested.position,dependency.position");
        for dependency in query
            .build_query_as::<LocalDependency>()
            .persistent(false)
            .fetch_all(&mut **transaction)
            .await?
        {
            dependencies
                .entry(dependency.local_file_key)
                .or_default()
                .push(dependency.dependency_path);
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
            })
        })
        .collect()
}

fn validate_local_file(file: &LocalFileWrite, dependencies: &[String]) -> LibraryResult<()> {
    if file.path.is_empty()
        || file.root.is_empty()
        || file.size_bytes.is_some_and(|value| value < 0)
        || dependencies.iter().any(String::is_empty)
    {
        return Err(LibraryError::InvalidRequest(
            "invalid Local file observation".to_string(),
        ));
    }
    Ok(())
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
