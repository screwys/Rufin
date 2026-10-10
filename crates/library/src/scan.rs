//! Writes acquired catalog facts directly and tracks completed collection scopes.
//! Provider acquisition and visible route construction remain outside this module.

use blake3::Hasher;
use sqlx::query::Query;
use sqlx::sqlite::{SqliteArguments, SqliteConnection, SqliteQueryResult};
use sqlx::{Connection, FromRow, QueryBuilder, Row, Sqlite, Transaction};
use sqlx_core::transaction::TransactionManager;

mod artwork;
mod entities;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{
    Database, HomeEntryInput, LibraryError, LibraryResult, ReadCancellation, SourceId, SourceKey,
    source_entity_uri,
};

const MAX_FRESHNESS_BYTES: usize = 64 * 1024;
const MAX_ROW_BYTES: usize = 8 * 1024 * 1024;
const LOUDNESS_PAGE_ROWS: i64 = 256;

/// A source-owned cheap fact used to accept an already-current catalog.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Freshness(Vec<u8>);

impl Freshness {
    pub fn new(marker: impl Into<Vec<u8>>) -> LibraryResult<Self> {
        let marker = marker.into();
        if marker.len() > MAX_FRESHNESS_BYTES {
            return Err(LibraryError::InvalidScan(
                "freshness markers may not exceed 64 KiB".to_string(),
            ));
        }
        Ok(Self(marker))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// The only facts emitted by catalog publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Publication {
    pub source: SourceKey,
    pub catalog_revision: u64,
    pub artwork_digest: [u8; 32],
    /// This publication inserted tracks that were not in the source catalog.
    pub tracks_added: bool,
}

#[derive(Clone, Debug, FromRow, Eq, PartialEq)]
pub struct CachedSource {
    pub source: SourceKey,
    pub object_id: String,
    pub display_name: String,
    pub catalog_revision: i64,
    pub artwork_digest: Vec<u8>,
}

/// Changes accepted by a source acquisition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScanOutcome {
    Changed(Publication),
    PlaylistsChanged(Publication),
    ArtworkChanged(Publication),
    Identical(Publication),
    Stale,
    Failed,
}

/// One relation fact in a bounded Scan page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScanLink<'a> {
    pub owner_id: &'a str,
    pub related_id: &'a str,
    pub position: i64,
}

impl<'a> ScanLink<'a> {
    pub const fn new(owner_id: &'a str, related_id: &'a str, position: i64) -> Self {
        Self {
            owner_id,
            related_id,
            position,
        }
    }
}

/// One acquisition and its identifier and inventory working set.
pub struct Scan {
    database: Database,
    _permit: tokio::sync::OwnedMutexGuard<()>,
    source: i64,
    changes: Changes,
    batch_changes: Option<Changes>,
    source_id: String,
    display_name: String,
    normalized_name: String,
    freshness: Option<Freshness>,
    accepted_at: i64,
    batch_writer: Option<tokio::sync::OwnedMutexGuard<Option<sqlx::SqliteConnection>>>,
    point_update: bool,
    local_point_update: bool,
    authoritative: bool,
    distinct_track_covers: bool,
    failed: bool,
    favorites: Option<[String; 3]>,
    user_data: Vec<(&'static str, String, crate::SourceUserData)>,
}

#[derive(Clone, Copy, Default)]
struct Changes {
    metadata: bool,
    playlists: bool,
    artwork: bool,
    tracks_added: bool,
    user_data: bool,
}

#[derive(FromRow)]
struct AcceptedSource {
    display_name: String,
    normalized_name: String,
    freshness: Option<Vec<u8>>,
    distinct_track_covers: bool,
    artwork_digest: Vec<u8>,
    catalog_revision: i64,
}

#[derive(FromRow)]
struct AlbumAudioKey {
    album_object_id: String,
    disc_number: i64,
    track_number: i64,
    sort_text: String,
    object_id: String,
    source_key: Vec<u8>,
    current_key: Vec<u8>,
}

#[derive(Clone, Debug, FromRow)]
pub struct LocalArtworkCandidate {
    pub object_id: String,
    pub path: String,
    pub root: Option<String>,
    pub picture_index: Option<i64>,
    pub artist_pictures: Option<String>,
    pub primary_artist: bool,
    pub revision: Option<String>,
    pub disc_number: i64,
    pub track_number: i64,
    pub sort_text: String,
}

impl Scan {
    /// Save the selected files as playlist entries without retaining a folder inventory.
    pub async fn create_local_playlist(&mut self, name: &str) -> LibraryResult<crate::PlaylistKey> {
        let mut writer = self.database.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let (playlist, _) =
            crate::playlists::create_playlist_identity(&mut transaction, None, name).await?;
        sqlx::query("INSERT INTO main.playlist_entries(
            playlist_key,object_id,media_uri,title,artist,album,album_display_artist,
            duration_millis,disc_number,track_number,year,release_date,source_format,
            musicbrainz_recording_id,musicbrainz_release_track_id,position)
            SELECT ?1,'rufin:entry:'||lower(hex(randomblob(16))),track.media_uri,
                track.title,track.display_artist,track.display_album,album.display_artist,
                track.duration_millis,track.disc_number,track.track_number,track.year,
                track.release_date,track.source_format,track.musicbrainz_recording_id,
                track.musicbrainz_release_track_id,
                row_number() OVER (ORDER BY coalesce(track.cue_path,track.source_path),track.disc_number,track.track_number,track.object_id)-1
            FROM tracks track LEFT JOIN albums album USING(album_key)
            WHERE track.source_key=?2 AND EXISTS(SELECT 1 FROM temp.scan_local_files file WHERE file.path=coalesce(track.cue_path,track.source_path))")
            .bind(playlist).bind(self.source).execute(&mut *transaction).await?;
        sqlx::raw_sql("DELETE FROM temp.scan_local_files; DELETE FROM temp.scan_local_file_dependencies; DELETE FROM temp.scan_local_dependency_paths;")
            .execute(&mut *transaction).await?;
        transaction.commit().await?;
        Ok(playlist)
    }

    pub fn source_id(&self) -> &str {
        &self.source_id
    }

    pub async fn contains_entity(
        &mut self,
        kind: &str,
        object_id: &str,
        include_cached: bool,
    ) -> LibraryResult<bool> {
        let table = match kind {
            "album" => "albums",
            "track" => "tracks",
            "artist" => "artists",
            _ => return Ok(false),
        };
        let sql = format!(
            "SELECT EXISTS(SELECT 1 FROM temp.scan_seen WHERE kind=?1 AND object_id=?2 AND written=1 UNION ALL SELECT 1 FROM {table} WHERE ?3 AND source_key=?4 AND object_id=?2)"
        );
        let query = || {
            sqlx::query_scalar::<_, bool>(sqlx::AssertSqlSafe(sql.as_str()))
                .bind(kind)
                .bind(object_id)
                .bind(include_cached)
                .bind(self.source)
        };
        if let Some(writer) = &mut self.batch_writer {
            Ok(query()
                .fetch_one(writer.as_mut().ok_or(LibraryError::WriterUnavailable)?)
                .await?)
        } else {
            let mut writer = self.database.writer().await?;
            Ok(query()
                .fetch_one(writer.as_mut().ok_or(LibraryError::WriterUnavailable)?)
                .await?)
        }
    }

    async fn observe(&mut self, kind: &str, object_id: &str) -> LibraryResult<bool> {
        if kind == "track" {
            self.execute(sqlx::query("INSERT OR IGNORE INTO temp.scan_affected_albums SELECT album_key FROM tracks WHERE source_key=?1 AND object_id=?2 AND album_key IS NOT NULL").bind(self.source).bind(object_id)).await?;
        }
        let first = self.execute_result(sqlx::query("INSERT INTO temp.scan_seen(kind,object_id,written) VALUES(?1,?2,1) ON CONFLICT(kind,object_id) DO UPDATE SET written=1 WHERE written=0").bind(kind).bind(object_id)).await?.rows_affected()>0;
        if first && kind == "track" && !self.point_update {
            self.execute(
                sqlx::query("INSERT OR IGNORE INTO temp.scan_removals VALUES('track-folders',?1)")
                    .bind(object_id),
            )
            .await?;
        }
        Ok(first)
    }

    /// Returns the current publication when the cheap freshness fact is accepted.
    pub async fn accept_freshness(
        database: &Database,
        source_id: &str,
        freshness: &Freshness,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<Publication>> {
        validate_id("source", source_id)?;
        let (_permit, mut connection) = database.acquire_general(cancellation).await?;
        let result = sqlx::query_as::<_, (i64, i64, Vec<u8>)>(
            "SELECT source_key,catalog_revision,artwork_digest FROM sources
             WHERE object_id=?1 AND freshness=?2 AND distinct_track_covers=?3
               AND catalog_revision>0",
        )
        .bind(source_id)
        .bind(freshness.as_bytes())
        .bind(database.distinct_track_covers())
        .fetch_optional(&mut *connection)
        .await;
        result?
            .map(|(source_key, revision, artwork)| publication(source_key, revision, &artwork))
            .transpose()
    }

    pub async fn begin(
        database: &Database,
        source_id: impl Into<String>,
        display_name: impl Into<String>,
        normalized_name: impl Into<String>,
        freshness: Option<Freshness>,
    ) -> LibraryResult<Self> {
        let source_id = source_id.into();
        validate_id("source", &source_id)?;
        let permit = database.acquire_scan()?;
        let mut writer = database.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let source = ensure_source_identity(connection, &source_id).await?;
        create_working_set(connection).await?;
        Ok(Self {
            database: database.clone(),
            _permit: permit,
            source,
            source_id,
            display_name: display_name.into(),
            normalized_name: normalized_name.into(),
            freshness,
            accepted_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |time| time.as_secs() as i64),
            changes: Changes::default(),
            batch_changes: None,
            batch_writer: None,
            point_update: false,
            local_point_update: false,
            authoritative: true,
            distinct_track_covers: database.distinct_track_covers(),
            failed: false,
            favorites: None,
            user_data: Vec::new(),
        })
    }

    pub async fn begin_items(database: &Database, source_id: &str) -> LibraryResult<Self> {
        let cached = database
            .cached_source(source_id, &ReadCancellation::new())
            .await?
            .ok_or_else(|| LibraryError::InvalidRequest("live source is not cached".to_string()))?;
        let mut scan = Self::begin(
            database,
            source_id,
            cached.display_name.clone(),
            cached.display_name.to_lowercase(),
            None,
        )
        .await?;
        scan.point_update = true;
        Ok(scan)
    }

    pub async fn begin_local_items(database: &Database, source_id: &str) -> LibraryResult<Self> {
        let mut scan = Self::begin_items(database, source_id).await?;
        scan.local_point_update = true;
        Ok(scan)
    }

    pub fn incomplete(&mut self) {
        self.authoritative = false;
        self.clear_freshness();
    }

    pub fn clear_freshness(&mut self) {
        self.freshness = None;
    }

    pub async fn retain_genres(&mut self) -> LibraryResult<()> {
        self.execute(sqlx::query("INSERT OR IGNORE INTO temp.scan_seen(kind,object_id,written) SELECT 'genre',object_id,0 FROM genres WHERE source_key=?1").bind(self.source)).await
    }

    pub async fn retain_playlists(&mut self) -> LibraryResult<()> {
        self.execute(sqlx::query("INSERT OR IGNORE INTO temp.scan_seen(kind,object_id,written) SELECT 'playlist',object_id,0 FROM catalog.native_playlists WHERE source_key=?1").bind(self.source)).await?;
        self.execute(sqlx::query("DELETE FROM temp.scan_playlist_entries"))
            .await?;
        self.execute(sqlx::query("DELETE FROM temp.scan_playlist_scopes"))
            .await
    }

    pub async fn retain_home_section(&mut self, section: &str) -> LibraryResult<()> {
        self.execute(
            sqlx::query("INSERT OR IGNORE INTO temp.scan_retained_home VALUES(?1)").bind(section),
        )
        .await
    }

    pub async fn observed_media_uris(&self, uris: &[&str]) -> LibraryResult<Vec<String>> {
        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT media_uri FROM tracks JOIN temp.scan_seen seen USING(object_id) WHERE seen.kind='track' AND source_key=",
        );
        query.push_bind(self.source).push(" AND media_uri IN (");
        let mut values = query.separated(",");
        for uri in uris {
            values.push_bind(uri);
        }
        values.push_unseparated(")");
        let mut writer = self.database.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(query.build_query_scalar().fetch_all(connection).await?)
    }

    pub async fn remove_track(&mut self, object_id: &str) -> LibraryResult<()> {
        self.write_removal("track", object_id).await
    }

    /// The album's complete track listing was read. Previously known members
    /// absent from that listing must be resolved before publishing: they may
    /// have moved to another album rather than disappeared from the source.
    pub async fn replace_album_membership(&mut self, object_id: &str) -> LibraryResult<()> {
        self.write_removal("album-members", object_id).await
    }

    pub async fn unobserved_album_tracks(
        &mut self,
        after: Option<&str>,
    ) -> LibraryResult<Vec<String>> {
        let mut writer = self.database.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(sqlx::query_scalar(
            "SELECT track.object_id FROM temp.scan_removals scope
             JOIN albums album ON album.source_key=?1 AND album.object_id=scope.object_id
             JOIN tracks track USING(album_key)
             WHERE scope.entity_kind='album-members'
               AND (?2 IS NULL OR track.object_id>?2)
               AND NOT EXISTS(SELECT 1 FROM temp.scan_seen seen WHERE seen.kind='track' AND seen.object_id=track.object_id)
               AND NOT EXISTS(SELECT 1 FROM temp.scan_removals removed WHERE removed.entity_kind='track' AND removed.object_id=track.object_id)
             ORDER BY track.object_id LIMIT 100",
        ).bind(self.source).bind(after).fetch_all(connection).await?)
    }

    pub async fn remove_album(&mut self, object_id: &str) -> LibraryResult<()> {
        self.write_removal("album", object_id).await
    }

    pub async fn remove_artist(&mut self, object_id: &str) -> LibraryResult<()> {
        self.write_removal("artist", object_id).await
    }

    pub async fn remove_genre(&mut self, object_id: &str) -> LibraryResult<()> {
        self.write_removal("genre", object_id).await
    }

    pub async fn remove_playlist(&mut self, object_id: &str) -> LibraryResult<()> {
        self.write_removal("playlist", object_id).await
    }

    async fn write_removal(&mut self, kind: &'static str, object_id: &str) -> LibraryResult<()> {
        self.require_id(kind, object_id)?;
        self.execute(
            sqlx::query("INSERT OR IGNORE INTO temp.scan_removals VALUES (?1,?2)")
                .bind(kind)
                .bind(object_id),
        )
        .await
    }

    pub const fn accepted_at(&self) -> i64 {
        self.accepted_at
    }

    pub fn source(&self) -> SourceKey {
        SourceKey::from_raw(self.source)
    }

    pub async fn begin_batch(&mut self) -> LibraryResult<()> {
        if self.batch_writer.is_some() {
            return Err(LibraryError::InvalidScan(
                "an acquisition batch is already active".into(),
            ));
        }
        let mut writer = self.database.writer_owned().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        if !source_exists(connection, self.source, &self.source_id).await? {
            return Err(LibraryError::InvalidScan(
                "source was removed during acquisition".into(),
            ));
        }
        sqlx::sqlite::SqliteTransactionManager::begin(connection, None).await?;
        self.batch_changes = Some(self.changes);
        self.batch_writer = Some(writer);
        Ok(())
    }

    pub async fn finish_batch(&mut self) -> LibraryResult<()> {
        let Some(mut writer) = self.batch_writer.take() else {
            return Err(LibraryError::InvalidScan(
                "no acquisition batch is active".into(),
            ));
        };
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        if self.failed {
            sqlx::sqlite::SqliteTransactionManager::rollback(connection).await?;
            self.changes = self.batch_changes.take().unwrap();
        } else {
            sqlx::sqlite::SqliteTransactionManager::commit(connection).await?;
            self.batch_changes = None;
        }
        Ok(())
    }

    pub async fn write_local_dependency_paths(&mut self, paths: &[String]) -> LibraryResult<()> {
        if paths.len() > 128 {
            self.failed = true;
            return Err(LibraryError::InvalidScan(
                "Local dependency batch exceeds 128 paths".to_string(),
            ));
        }
        if paths.is_empty() {
            return Ok(());
        }
        for path in paths {
            self.require_id("Local dependency path", path)?;
            self.require_row_bytes(&[path.as_bytes()])?;
        }
        let mut query = QueryBuilder::<Sqlite>::new(
            "INSERT OR IGNORE INTO temp.scan_local_dependency_paths(path) ",
        );
        query.push_values(paths, |mut row, path| {
            row.push_bind(path);
        });
        self.execute(query.build()).await?;
        let documents = paths
            .iter()
            .filter(|path| path.starts_with("rufin-document://"))
            .collect::<Vec<_>>();
        if !documents.is_empty() {
            let mut query = QueryBuilder::<Sqlite>::new(
                "INSERT OR IGNORE INTO temp.scan_local_dependency_paths(path)
                 SELECT alias.path FROM temp.scan_local_files original
                 JOIN temp.scan_local_files alias ON alias.native_id=original.native_id
                 WHERE original.native_id IS NOT NULL AND original.path IN (",
            );
            let mut values = query.separated(",");
            for path in documents {
                values.push_bind(path);
            }
            values.push_unseparated(")");
            self.execute(query.build()).await?;
        }
        Ok(())
    }

    pub async fn write_local_files(
        &mut self,
        files: &[(crate::LocalFileWrite, Vec<String>)],
    ) -> LibraryResult<()> {
        if files.len() > 128 {
            self.failed = true;
            return Err(LibraryError::InvalidScan(
                "Local observation batch exceeds 128 files".to_string(),
            ));
        }
        if files.is_empty() {
            return Ok(());
        }
        self.local_point_update = self.point_update;
        for (file, dependencies) in files {
            if file.path.is_empty()
                || file.root.is_empty()
                || !file.path.starts_with(&file.root)
                || file.mtime_ns < 0
                || dependencies.iter().any(String::is_empty)
            {
                self.failed = true;
                return Err(LibraryError::InvalidScan(
                    "invalid Local observation".to_string(),
                ));
            }
            self.require_row_bytes(&[
                file.path.as_bytes(),
                file.root.as_bytes(),
                file.relative_path.as_bytes(),
                file.native_id.as_deref().unwrap_or_default().as_bytes(),
                file.revision.as_deref().unwrap_or_default().as_bytes(),
            ])?;
        }
        let mut observations = QueryBuilder::<Sqlite>::new(
            "INSERT INTO temp.scan_local_files(path,root,relative_path,kind,size_bytes,mtime_ns,device_id,inode,native_id,revision,picture_index,artist_pictures,parse_version,state) ",
        );
        observations.push_values(files, |mut row, (file, _)| {
            row.push_bind(&file.path)
                .push_bind(&file.root)
                .push_bind(&file.relative_path)
                .push_bind(file.kind.as_str())
                .push_bind(file.size_bytes)
                .push_bind(file.mtime_ns)
                .push_bind(file.device_id)
                .push_bind(file.inode)
                .push_bind(&file.native_id)
                .push_bind(&file.revision)
                .push_bind(file.picture_index)
                .push_bind(&file.artist_pictures)
                .push_bind(file.parse_version)
                .push_bind(file.state.as_str());
        });
        observations.push(" ON CONFLICT(path) DO UPDATE SET root=excluded.root,relative_path=excluded.relative_path,kind=excluded.kind,size_bytes=excluded.size_bytes,mtime_ns=excluded.mtime_ns,device_id=excluded.device_id,inode=excluded.inode,native_id=excluded.native_id,revision=excluded.revision,picture_index=excluded.picture_index,artist_pictures=excluded.artist_pictures,parse_version=excluded.parse_version,state=excluded.state");
        self.execute(observations.build()).await?;

        let mut clear = QueryBuilder::<Sqlite>::new(
            "DELETE FROM temp.scan_local_file_dependencies WHERE path IN (",
        );
        let mut separated = clear.separated(",");
        for (file, _) in files {
            separated.push_bind(&file.path);
        }
        separated.push_unseparated(")");
        self.execute(clear.build()).await?;

        let dependency_count = files
            .iter()
            .map(|(_, dependencies)| dependencies.len())
            .sum::<usize>();
        if dependency_count > 0 {
            let mut dependencies = QueryBuilder::<Sqlite>::new(
                "INSERT INTO temp.scan_local_file_dependencies(path,dependency_path,position) ",
            );
            dependencies.push_values(
                files.iter().flat_map(|(file, values)| {
                    values
                        .iter()
                        .enumerate()
                        .map(move |(position, dependency)| (&file.path, dependency, position))
                }),
                |mut row, (path, dependency, position)| {
                    row.push_bind(path)
                        .push_bind(dependency)
                        .push_bind(position as i64);
                },
            );
            self.execute(dependencies.build()).await?;
        }
        Ok(())
    }

    pub async fn remove_local_file_paths(&mut self, paths: &[String]) -> LibraryResult<()> {
        if paths.len() > 128 {
            self.failed = true;
            return Err(LibraryError::InvalidScan(
                "Local removal batch exceeds 128 paths".to_string(),
            ));
        }
        if paths.is_empty() {
            return Ok(());
        }
        if paths.iter().any(String::is_empty) {
            self.failed = true;
            return Err(LibraryError::InvalidScan(
                "invalid Local removal path".to_string(),
            ));
        }
        let mut query = QueryBuilder::<Sqlite>::new(
            "INSERT OR IGNORE INTO temp.scan_local_file_removals(path) ",
        );
        query.push_values(paths, |mut row, path| {
            row.push_bind(path);
        });
        self.execute(query.build()).await
    }

    pub async fn local_dependency_paths(&mut self, paths: &[String]) -> LibraryResult<Vec<String>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        if paths.len() > 128 {
            return Err(LibraryError::InvalidScan(
                "Local dependency lookup exceeds 128 paths".to_string(),
            ));
        }
        if let Some(writer) = self.batch_writer.as_mut() {
            let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
            return fetch_local_dependency_paths(connection, paths).await;
        }
        let mut writer = self.database.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        fetch_local_dependency_paths(connection, paths).await
    }

    /// Reuse an unchanged remote directory's observations after the source has
    /// established that its revision covers descendants (for example Nextcloud).
    pub async fn retain_file_tree(
        &mut self,
        directory: &crate::LocalFileWrite,
    ) -> LibraryResult<bool> {
        let source = self.source;
        let prefix = format!("{}/", directory.path.trim_end_matches('/'));
        let mut writer = self.database.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let unchanged: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM local_files file JOIN sources USING(source_key)
             WHERE source_key=?1 AND file.path=?2 AND file.kind='directory'
               AND file.revision=?3 AND file.parse_version=?4 AND sources.freshness IS NOT NULL)",
        )
        .bind(source)
        .bind(&directory.path)
        .bind(&directory.revision)
        .bind(directory.parse_version)
        .fetch_one(&mut *connection)
        .await?;
        if !unchanged {
            return Ok(false);
        }
        sqlx::query(
            "INSERT OR IGNORE INTO temp.scan_local_files
             SELECT path,root,relative_path,kind,size_bytes,mtime_ns,device_id,inode,
                    native_id,revision,picture_index,artist_pictures,parse_version,
                    CASE WHEN kind='directory' THEN 'accepted' ELSE state END
             FROM local_files WHERE source_key=?1 AND path>=?2 AND path<?2||char(1114111)",
        )
        .bind(source)
        .bind(prefix)
        .execute(&mut *connection)
        .await?;
        sqlx::query("UPDATE temp.scan_local_files SET state='accepted' WHERE path=?1")
            .bind(&directory.path)
            .execute(&mut *connection)
            .await?;
        Ok(true)
    }

    /// Retain a collection listing proven unchanged by a depth-one DAV sync.
    /// Child directories remain pending: their own tokens cover their children.
    pub async fn retain_file_directory_children(&mut self, path: &str) -> LibraryResult<()> {
        let prefix = format!("{}/", path.trim_end_matches('/'));
        self.execute(
            sqlx::query(
                "INSERT OR IGNORE INTO temp.scan_local_files
             SELECT path,root,relative_path,kind,size_bytes,mtime_ns,device_id,inode,
                    native_id,revision,picture_index,artist_pictures,parse_version,
                    CASE WHEN kind='directory' THEN 'observed' ELSE state END
             FROM local_files WHERE source_key=?1 AND path>=?2 AND path<?2||char(1114111)
               AND instr(substr(rtrim(path,'/'),length(?2)+1),'/')=0",
            )
            .bind(self.source)
            .bind(prefix),
        )
        .await?;
        self.execute(
            sqlx::query("UPDATE temp.scan_local_files SET state='accepted' WHERE path=?1")
                .bind(path),
        )
        .await
    }

    pub async fn local_inventory_path_page(
        &mut self,
        kind: crate::LocalFileKind,
        after: Option<&str>,
        exclude_cue_dependencies: bool,
        limit: usize,
    ) -> LibraryResult<Vec<String>> {
        self.local_inventory_path_page_in_root(kind, after, exclude_cue_dependencies, limit, None)
            .await
    }

    pub async fn local_inventory_path_page_in_root(
        &mut self,
        kind: crate::LocalFileKind,
        after: Option<&str>,
        exclude_cue_dependencies: bool,
        limit: usize,
        root: Option<&str>,
    ) -> LibraryResult<Vec<String>> {
        let query = || {
            sqlx::query_scalar::<_, String>(
                "SELECT file.path FROM temp.scan_local_files file
             WHERE file.kind=?1 AND file.path>?2 AND (?5 IS NULL OR file.root=?5)
               AND (file.kind<>'directory' OR file.state='observed')
               AND (?3=0 OR NOT EXISTS (
                 SELECT 1 FROM temp.scan_local_dependency_paths dependency
                 WHERE dependency.path=file.path
               ))
             ORDER BY file.path LIMIT ?4",
            )
            .bind(kind.as_str())
            .bind(after.unwrap_or(""))
            .bind(exclude_cue_dependencies)
            .bind(limit.clamp(1, 128) as i64)
            .bind(root)
        };
        if let Some(writer) = self.batch_writer.as_mut() {
            let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
            return Ok(query().fetch_all(&mut *connection).await?);
        }
        let mut writer = self.database.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(query().fetch_all(&mut *connection).await?)
    }

    pub async fn local_inventory_file_page(
        &mut self,
        kind: crate::LocalFileKind,
        after: Option<&str>,
    ) -> LibraryResult<Vec<crate::LocalFileWrite>> {
        self.local_inventory_file_page_in_root(kind, after, None)
            .await
    }

    pub async fn local_inventory_file_page_in_root(
        &mut self,
        kind: crate::LocalFileKind,
        after: Option<&str>,
        root: Option<&str>,
    ) -> LibraryResult<Vec<crate::LocalFileWrite>> {
        let query = || {
            sqlx::query("SELECT path,root,relative_path,kind,size_bytes,mtime_ns,device_id,inode,native_id,revision,picture_index,artist_pictures,parse_version,state FROM temp.scan_local_files WHERE kind=?1 AND path>?2 AND (?3 IS NULL OR root=?3) ORDER BY path LIMIT 128")
            .bind(kind.as_str()).bind(after.unwrap_or("")).bind(root)
        };
        let rows = if let Some(writer) = self.batch_writer.as_mut() {
            query()
                .fetch_all(writer.as_mut().ok_or(LibraryError::WriterUnavailable)?)
                .await?
        } else {
            let mut writer = self.database.writer().await?;
            query()
                .fetch_all(writer.as_mut().ok_or(LibraryError::WriterUnavailable)?)
                .await?
        };
        rows.into_iter().map(local_inventory_row).collect()
    }

    pub async fn local_inventory_files(
        &mut self,
        paths: &[String],
    ) -> LibraryResult<Vec<crate::LocalFileWrite>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        if paths.len() > 128 {
            return Err(LibraryError::InvalidScan(
                "File inventory lookup exceeds 128 paths".into(),
            ));
        }
        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT path,root,relative_path,kind,size_bytes,mtime_ns,device_id,inode,native_id,revision,picture_index,artist_pictures,parse_version,state FROM temp.scan_local_files WHERE path IN (",
        );
        let mut values = query.separated(",");
        for path in paths {
            values.push_bind(path);
        }
        values.push_unseparated(")");
        let rows = if let Some(writer) = self.batch_writer.as_mut() {
            query
                .build()
                .fetch_all(writer.as_mut().ok_or(LibraryError::WriterUnavailable)?)
                .await?
        } else {
            let mut writer = self.database.writer().await?;
            query
                .build()
                .fetch_all(writer.as_mut().ok_or(LibraryError::WriterUnavailable)?)
                .await?
        };
        rows.into_iter().map(local_inventory_row).collect()
    }

    /// At most two paths are needed to distinguish a unique native ID from aliases.
    pub async fn local_inventory_native_paths(
        &mut self,
        native_id: &str,
    ) -> LibraryResult<Vec<String>> {
        let query = || {
            sqlx::query_scalar(
                "SELECT path FROM temp.scan_local_files WHERE native_id=?1 ORDER BY path LIMIT 2",
            )
            .bind(native_id)
        };
        if let Some(writer) = self.batch_writer.as_mut() {
            return Ok(query()
                .fetch_all(writer.as_mut().ok_or(LibraryError::WriterUnavailable)?)
                .await?);
        }
        let mut writer = self.database.writer().await?;
        Ok(query()
            .fetch_all(writer.as_mut().ok_or(LibraryError::WriterUnavailable)?)
            .await?)
    }

    pub async fn write_local_component_paths(&mut self, paths: &[String]) -> LibraryResult<()> {
        if paths.is_empty() {
            return Ok(());
        }
        if paths.len() > 128 {
            self.failed = true;
            return Err(LibraryError::InvalidScan(
                "Local component staging batch exceeds 128 paths".to_string(),
            ));
        }
        let mut query = QueryBuilder::<Sqlite>::new(
            "INSERT OR IGNORE INTO temp.scan_local_component_paths(path) ",
        );
        query.push_values(paths, |mut row, path| {
            row.push_bind(path);
        });
        self.execute(query.build()).await
    }

    pub async fn expand_local_artwork_prefixes(
        &mut self,
        source: SourceKey,
        directories: &[String],
        image_directories: &[String],
    ) -> LibraryResult<()> {
        if directories.is_empty() && image_directories.is_empty() {
            return Ok(());
        }
        if directories.len().saturating_add(image_directories.len()) > 128 {
            self.failed = true;
            return Err(LibraryError::InvalidScan(
                "Local component prefix batch exceeds 128 paths".to_string(),
            ));
        }
        let prefixes = directories
            .iter()
            .map(|prefix| (prefix, true))
            .chain(image_directories.iter().map(|prefix| (prefix, false)))
            .collect::<Vec<_>>();
        let mut query = QueryBuilder::<Sqlite>::new("WITH requested(prefix,directory) AS (");
        query.push_values(&prefixes, |mut row, (prefix, directory)| {
            row.push_bind(*prefix).push_bind(*directory);
        });
        query.push("), eligible AS (SELECT prefix,directory OR 1=(SELECT count(DISTINCT track.album_key) FROM tracks track WHERE track.source_key=")
            .push_bind(source)
            .push(" AND track.album_key IS NOT NULL AND track.source_path>=requested.prefix AND track.source_path<requested.prefix||char(1114111)) AS descendants FROM requested) INSERT OR IGNORE INTO temp.scan_local_component_paths(path) SELECT file.path FROM eligible JOIN local_files file ON file.source_key=")
            .push_bind(source)
            .push(" AND file.path>=eligible.prefix AND file.path<eligible.prefix||char(1114111) AND (eligible.descendants OR (file.kind<>'directory' AND instr(substr(file.path,length(eligible.prefix)+1),substr(eligible.prefix,-1))=0))");
        self.execute(query.build()).await
    }

    pub async fn expand_local_component(&mut self, source: SourceKey) -> LibraryResult<()> {
        self.execute(
            sqlx::query(
                "INSERT OR IGNORE INTO temp.scan_local_component_paths(path)
                 SELECT file.path FROM temp.scan_local_component_paths seed
                 JOIN local_files directory
                   ON directory.source_key=?1 AND directory.path=seed.path
                      AND directory.kind='directory'
                 JOIN local_files file
                   ON file.source_key=?1 AND (
                       file.path LIKE directory.path||char(47)||'%'
                       OR file.path LIKE directory.path||char(92)||'%'
                   )",
            )
            .bind(source),
        )
        .await?;
        self.execute(
            sqlx::query(
                "WITH RECURSIVE component(path) AS (
                     SELECT path FROM temp.scan_local_component_paths
                     UNION
                     SELECT dependency.dependency_path
                     FROM component current
                     JOIN local_files file
                       ON file.source_key=?1 AND file.path=current.path
                     JOIN local_file_dependencies dependency USING(local_file_key)
                     UNION
                     SELECT file.path
                     FROM component current
                     CROSS JOIN local_file_dependencies dependency
                       ON dependency.dependency_path=current.path
                     CROSS JOIN local_files file
                       ON file.source_key=?1
                      AND file.local_file_key=dependency.local_file_key
                 )
                 INSERT OR IGNORE INTO temp.scan_local_component_paths(path)
                 SELECT path FROM component",
            )
            .bind(source),
        )
        .await
    }

    pub async fn local_component_path_page(
        &mut self,
        after: Option<&str>,
        limit: usize,
    ) -> LibraryResult<Vec<String>> {
        let query = || {
            sqlx::query_scalar::<_, String>(
                "SELECT path FROM temp.scan_local_component_paths
                 WHERE path>?1 ORDER BY path LIMIT ?2",
            )
            .bind(after.unwrap_or(""))
            .bind(limit.clamp(1, 128) as i64)
        };
        if let Some(writer) = self.batch_writer.as_mut() {
            let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
            return Ok(query().fetch_all(&mut *connection).await?);
        }
        let mut writer = self.database.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(query().fetch_all(&mut *connection).await?)
    }

    pub async fn local_affected_album_path_page(
        &mut self,
        source: SourceKey,
        after: Option<&str>,
        limit: usize,
    ) -> LibraryResult<Vec<String>> {
        let query = || {
            sqlx::query_scalar::<_, String>(
                "WITH affected_album(object_id) AS (
                     SELECT object_id FROM temp.scan_seen WHERE kind='album'
                     UNION
                     SELECT album.object_id FROM temp.scan_removals removal
                     JOIN tracks track ON track.source_key=?1
                       AND removal.entity_kind='track' AND removal.object_id=track.object_id
                     JOIN albums album USING(album_key)
                 ), affected_path(path) AS (
                     SELECT DISTINCT COALESCE(track.cue_path,track.source_path)
                     FROM affected_album affected
                     JOIN albums album ON album.source_key=?1 AND album.object_id=affected.object_id
                     JOIN tracks track USING(album_key)
                     WHERE (track.cue_path IS NOT NULL OR track.media_uri LIKE 'file://%')
                       AND NOT EXISTS (
                           SELECT 1 FROM temp.scan_local_component_paths component
                           WHERE component.path=COALESCE(track.cue_path,track.source_path)
                       )
                 ) SELECT path FROM affected_path WHERE path>?2 ORDER BY path LIMIT ?3",
            )
            .bind(source)
            .bind(after.unwrap_or(""))
            .bind(limit.clamp(1, 128) as i64)
        };
        if let Some(writer) = self.batch_writer.as_mut() {
            let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
            return Ok(query().fetch_all(&mut *connection).await?);
        }
        let mut writer = self.database.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(query().fetch_all(&mut *connection).await?)
    }

    pub async fn remove_local_component_tracks(&mut self, source: SourceKey) -> LibraryResult<()> {
        self.execute(
            sqlx::query(
                "INSERT OR IGNORE INTO temp.scan_removals(entity_kind,object_id)
                 SELECT 'track',track.object_id FROM tracks track
                 JOIN temp.scan_local_component_paths component
                   ON track.source_path=component.path
                      OR track.cue_path=component.path
                 WHERE track.source_key=?1",
            )
            .bind(source),
        )
        .await
    }

    pub async fn retain_local_media_paths(&mut self, paths: &[String]) -> LibraryResult<()> {
        if paths.len() > 128 {
            self.failed = true;
            return Err(LibraryError::InvalidScan(
                "retained Local media batch exceeds 128 paths".to_string(),
            ));
        }
        if paths.is_empty() {
            return Ok(());
        }
        self.execute(sqlx::query("DELETE FROM temp.scan_retained_paths"))
            .await?;
        let mut requested =
            QueryBuilder::<Sqlite>::new("INSERT INTO temp.scan_retained_paths(path) ");
        requested.push_values(paths, |mut row, path| {
            row.push_bind(path);
        });
        self.execute(requested.build()).await?;
        self.retain_local_tracks_where(
            "track.source_path IN (SELECT path FROM temp.scan_retained_paths) AND track.cue_path IS NULL",
            None,
        )
        .await
    }

    pub async fn retain_local_cue_path(&mut self, path: &str) -> LibraryResult<()> {
        self.retain_local_tracks("track.cue_path=?2", path).await
    }

    pub async fn retain_connected_file_tracks(&mut self) -> LibraryResult<()> {
        self.retain_local_tracks_where(
            "track.source_path IS NULL AND EXISTS(
                SELECT 1 FROM main.connect_collection received WHERE received.media_uri=track.media_uri)
             AND NOT EXISTS(SELECT 1 FROM temp.scan_seen seen WHERE seen.kind='track' AND seen.object_id=track.object_id)",
            None,
        )
        .await
    }

    async fn retain_local_tracks(
        &mut self,
        predicate: &'static str,
        value: &str,
    ) -> LibraryResult<()> {
        self.retain_local_tracks_where(predicate, Some(value)).await
    }

    async fn retain_local_tracks_where(
        &mut self,
        predicate: &str,
        parameter: Option<&str>,
    ) -> LibraryResult<()> {
        let tracks = format!(
            "SELECT track.track_key FROM tracks track WHERE track.source_key=?1 AND {predicate}"
        );
        for (kind, query) in [
            (
                "track",
                format!("SELECT object_id FROM tracks WHERE track_key IN ({tracks})"),
            ),
            (
                "artist",
                format!(
                    "SELECT artist.object_id FROM artists artist JOIN track_artists relation USING(artist_key) WHERE relation.track_key IN ({tracks}) UNION SELECT artist.object_id FROM artists artist JOIN album_artists relation USING(artist_key) WHERE relation.album_key IN (SELECT album_key FROM tracks WHERE track_key IN ({tracks}))"
                ),
            ),
            (
                "genre",
                format!(
                    "SELECT genre.object_id FROM genres genre JOIN track_genres relation USING(genre_key) WHERE relation.track_key IN ({tracks}) UNION SELECT genre.object_id FROM genres genre JOIN album_genres relation USING(genre_key) WHERE relation.album_key IN (SELECT album_key FROM tracks WHERE track_key IN ({tracks}))"
                ),
            ),
            (
                "folder",
                format!(
                    "SELECT folder.object_id FROM folders folder JOIN track_folders relation USING(folder_key) WHERE relation.track_key IN ({tracks})"
                ),
            ),
            (
                "mood",
                format!(
                    "SELECT mood.object_id FROM moods mood JOIN track_moods relation USING(mood_key) WHERE relation.track_key IN ({tracks})"
                ),
            ),
        ] {
            self.execute(sqlx::query(sqlx::AssertSqlSafe(format!("INSERT OR IGNORE INTO temp.scan_seen(kind,object_id,written) SELECT '{kind}',object_id,0 FROM ({query})"))).bind(self.source).bind(parameter)).await?;
        }
        for (relation, target, key) in [
            ("album_artists", "artists", "artist_key"),
            ("album_genres", "genres", "genre_key"),
        ] {
            self.execute(sqlx::query(sqlx::AssertSqlSafe(format!("INSERT OR IGNORE INTO temp.scan_{relation} SELECT album.object_id,target.object_id,relation.position FROM {relation} relation JOIN albums album USING(album_key) JOIN {target} target USING({key}) WHERE album.album_key IN (SELECT album_key FROM tracks WHERE track_key IN ({tracks}))"))).bind(self.source).bind(parameter)).await?;
        }
        self.execute(sqlx::query(sqlx::AssertSqlSafe(format!("INSERT OR IGNORE INTO temp.scan_album_release_types SELECT album.object_id,relation.release_type,relation.position FROM album_release_types relation JOIN albums album USING(album_key) WHERE album.album_key IN (SELECT album_key FROM tracks WHERE track_key IN ({tracks}))"))).bind(self.source).bind(parameter)).await?;
        self.execute(sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO temp.scan_seen(kind,object_id,written,supplied) SELECT 'album',album.object_id,0,{} FROM albums album WHERE album.album_key IN (SELECT album_key FROM tracks WHERE track_key IN ({tracks})) ON CONFLICT(kind,object_id) DO UPDATE SET supplied=supplied|excluded.supplied",entities::ALBUM_SUPPLIED))).bind(self.source).bind(parameter)).await
    }

    pub async fn write_track_relations(
        &mut self,
        artists: &[(&str, &str)],
        genres: &[(&str, &str)],
        moods: &[(&str, &str)],
    ) -> LibraryResult<()> {
        self.write_ordered_relation_batch("temp.scan_track_artists", "Track", artists)
            .await?;
        self.write_ordered_relation_batch("temp.scan_track_genres", "Track", genres)
            .await?;
        self.write_ordered_relation_batch("temp.scan_track_moods", "Track", moods)
            .await
    }

    pub async fn write_album_relations(
        &mut self,
        artists: &[(&str, &str)],
        genres: &[(&str, &str)],
        release_types: &[(&str, &str)],
    ) -> LibraryResult<()> {
        self.write_ordered_relation_batch("temp.scan_album_artists", "Album", artists)
            .await?;
        self.write_ordered_relation_batch("temp.scan_album_genres", "Album", genres)
            .await?;
        self.write_ordered_relation_batch("temp.scan_album_release_types", "Album", release_types)
            .await
    }

    pub async fn write_track_folders(&mut self, links: &[ScanLink<'_>]) -> LibraryResult<()> {
        self.write_links_batch(
            "INSERT OR IGNORE INTO temp.scan_track_folders(owner_id,related_id,position) ",
            links,
        )
        .await
    }

    pub async fn replace_track_folders(
        &mut self,
        object_id: &str,
        links: &[ScanLink<'_>],
    ) -> LibraryResult<()> {
        self.write_removal("track-folders", object_id).await?;
        self.execute(
            sqlx::query("DELETE FROM temp.scan_track_folders WHERE owner_id=?1").bind(object_id),
        )
        .await?;
        self.write_track_folders(links).await
    }

    pub fn replace_favorites(&mut self, tracks: &[String], albums: &[String], artists: &[String]) {
        self.favorites = Some(
            [tracks, albums, artists]
                .map(|ids| serde_json::to_string(ids).expect("favorite IDs serialize")),
        );
    }

    pub async fn remove_unstaged_playlists(&mut self) -> LibraryResult<()> {
        self.execute(
            sqlx::query(
                "INSERT OR IGNORE INTO temp.scan_removals(entity_kind,object_id)
             SELECT 'playlist',object_id FROM catalog.native_playlists WHERE source_key=?1
             AND object_id NOT IN (SELECT object_id FROM temp.scan_seen WHERE kind='playlist')",
            )
            .bind(self.source),
        )
        .await
    }

    pub async fn retain_playlist_entries_if_fresh(
        &mut self,
        object_id: &str,
        revision: &str,
        now: i64,
    ) -> LibraryResult<bool> {
        let retained = self.execute_result(sqlx::query("UPDATE temp.scan_playlist_scopes SET entries_supplied=0 WHERE object_id=?2 AND EXISTS(SELECT 1 FROM catalog.native_playlists cached WHERE cached.source_key=?1 AND cached.object_id=?2 AND cached.provider_revision=?3 AND cached.valid_until>?4)").bind(self.source).bind(object_id).bind(revision).bind(now)).await?.rows_affected()>0;
        if retained {
            self.execute(sqlx::query("UPDATE temp.scan_artwork AS image SET source=COALESCE(image.source,cached.artwork_binding),binding=COALESCE(image.binding,cached.artwork_binding) FROM catalog.native_playlists cached WHERE image.kind='playlist' AND image.object_id=?2 AND cached.object_id=image.object_id AND cached.source_key=?1").bind(self.source).bind(object_id)).await?;
        }
        Ok(retained)
    }

    pub async fn write_playlist_freshness(
        &mut self,
        object_id: &str,
        revision: Option<&str>,
        valid_until: Option<i64>,
    ) -> LibraryResult<()> {
        self.execute(sqlx::query("UPDATE catalog.native_playlists SET provider_revision=?3,valid_until=?4 WHERE source_key=?1 AND object_id=?2 AND (provider_revision,valid_until) IS NOT (?3,?4)").bind(self.source).bind(object_id).bind(revision).bind(valid_until)).await
    }

    pub async fn write_playlist(
        &mut self,
        object_id: &str,
        name: &str,
        normalized_name: &str,
        sort_text: &str,
        artwork_binding: Option<&[u8]>,
    ) -> LibraryResult<()> {
        self.require_id("playlist", object_id)?;
        let first = self.observe("playlist", object_id).await?;
        self.execute(
            sqlx::query("INSERT OR IGNORE INTO temp.scan_playlist_scopes(object_id) VALUES(?1)")
                .bind(object_id),
        )
        .await?;
        self.execute(sqlx::query("INSERT INTO main.playlists(source_key,object_id,position) SELECT durable.source_key,?2,COALESCE((SELECT max(position)+1 FROM main.playlists),0) FROM main.source_ids durable JOIN catalog.sources source USING(object_id) WHERE source.source_key=?1 ON CONFLICT(source_key,object_id) DO NOTHING").bind(self.source).bind(object_id)).await?;
        self.changes.playlists |= self.execute_result(sqlx::query("INSERT INTO catalog.native_playlists(source_key,object_id,name,normalized_name,sort_text) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(source_key,object_id) DO UPDATE SET name=excluded.name,normalized_name=excluded.normalized_name,sort_text=excluded.sort_text WHERE (name,normalized_name,sort_text) IS NOT (excluded.name,excluded.normalized_name,excluded.sort_text)").bind(self.source).bind(object_id).bind(name).bind(normalized_name).bind(sort_text)).await?.rows_affected()>0;
        self.remember_artwork("playlist", object_id, artwork_binding, first)
            .await
    }

    pub async fn write_playlist_permissions(
        &mut self,
        object_id: &str,
        writable: bool,
        metadata_writable: bool,
    ) -> LibraryResult<()> {
        self.changes.playlists |= self.execute_result(sqlx::query("UPDATE catalog.native_playlists SET writable=?3,metadata_writable=?4 WHERE source_key=?1 AND object_id=?2 AND (writable,metadata_writable) IS NOT (?3,?4)").bind(self.source).bind(object_id).bind(writable).bind(metadata_writable)).await?.rows_affected()>0;
        Ok(())
    }

    pub async fn write_playlist_entry(
        &mut self,
        playlist_id: &str,
        object_id: &str,
        track_id: &str,
        position: i64,
    ) -> LibraryResult<()> {
        self.require_id("playlist", playlist_id)?;
        self.require_id("playlist entry", object_id)?;
        self.require_id("track", track_id)?;
        if position < 0 {
            self.failed = true;
            return Err(LibraryError::InvalidScan(
                "playlist position cannot be negative".to_string(),
            ));
        }
        self.require_row_bytes(&[
            playlist_id.as_bytes(),
            object_id.as_bytes(),
            track_id.as_bytes(),
        ])?;
        let media_uri = source_entity_uri(&SourceId::new(&self.source_id), "track", track_id);
        self.execute(
            sqlx::query("INSERT INTO temp.scan_playlist_entries VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT DO NOTHING")
                .bind(playlist_id)
                .bind(object_id)
                .bind(track_id)
                .bind(media_uri)
                .bind(position),
        )
        .await
    }

    pub async fn write_home_entry(&mut self, input: &HomeEntryInput) -> LibraryResult<()> {
        self.require_id("Home section", &input.section_id)?;
        self.require_id("Home entity", &input.entity_object_id)?;
        if input.position < 0 {
            self.failed = true;
            return Err(LibraryError::InvalidScan(
                "Home position cannot be negative".to_string(),
            ));
        }
        self.require_row_bytes(&[
            input.section_id.as_bytes(),
            input.entity_object_id.as_bytes(),
            input.title.as_bytes(),
            input.subtitle.as_bytes(),
        ])?;
        self.execute(
            sqlx::query(
                "INSERT INTO temp.scan_home_entries SELECT ?1, ?2, ?3, ?4, ?5, ?6, NULL
                WHERE CASE ?3
                  WHEN 'track' THEN EXISTS(SELECT 1 FROM tracks WHERE source_key=?7 AND object_id=?4)
                  WHEN 'album' THEN EXISTS(SELECT 1 FROM albums WHERE source_key=?7 AND object_id=?4)
                  WHEN 'artist' THEN EXISTS(SELECT 1 FROM artists WHERE source_key=?7 AND object_id=?4)
                  WHEN 'playlist' THEN EXISTS(SELECT 1 FROM catalog.native_playlists WHERE source_key=?7 AND object_id=?4)
                END",
            )
            .bind(&input.section_id)
            .bind(input.position)
            .bind(input.kind.as_str())
            .bind(&input.entity_object_id)
            .bind(&input.title)
            .bind(&input.subtitle)
            .bind(self.source),
        )
        .await
    }

    pub async fn write_home_section_title(
        &mut self,
        section_id: &str,
        title: &str,
    ) -> LibraryResult<()> {
        self.require_row_bytes(&[section_id.as_bytes(), title.as_bytes()])?;
        self.execute(
            sqlx::query("UPDATE temp.scan_home_entries SET section_title=?2 WHERE owner_id=?1")
                .bind(section_id)
                .bind(title),
        )
        .await
    }

    pub async fn replace_home_section(
        &mut self,
        section: &str,
        entries: &[HomeEntryInput],
    ) -> LibraryResult<()> {
        self.execute(
            sqlx::query("INSERT OR IGNORE INTO temp.scan_removals VALUES ('home',?1)")
                .bind(section),
        )
        .await?;
        self.execute(
            sqlx::query("DELETE FROM temp.scan_home_entries WHERE owner_id=?1").bind(section),
        )
        .await?;
        for entry in entries {
            self.write_home_entry(entry).await?;
        }
        Ok(())
    }

    /// Stages sparse facts only for an admitted entity. Unknown IDs need metadata first.
    pub async fn write_source_user_data(
        &mut self,
        kind: &'static str,
        object_id: String,
        facts: crate::SourceUserData,
    ) -> LibraryResult<bool> {
        if !self.contains_entity(kind, &object_id, true).await? {
            return Ok(false);
        }
        self.user_data.push((kind, object_id, facts));
        Ok(true)
    }

    pub async fn finish(mut self) -> LibraryResult<ScanOutcome> {
        if self.batch_writer.is_some() {
            self.finish_batch().await?;
        }
        if self.failed {
            self.incomplete();
        }
        let mut writer = self.database.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let result = self.publish(connection).await;
        clear_working_set(connection).await?;
        result
    }

    async fn publish(&self, connection: &mut SqliteConnection) -> LibraryResult<ScanOutcome> {
        let full = self.authoritative && !self.point_update;
        let source = self.source;
        let mut transaction = connection.begin().await?;
        let current = sqlx::query_as::<_, AcceptedSource>("SELECT artwork_digest,catalog_revision,display_name,normalized_name,freshness,distinct_track_covers FROM sources WHERE source_key=?1 AND object_id=?2")
            .bind(source).bind(&self.source_id).fetch_optional(&mut *transaction).await?;
        let Some(current) = current else {
            return Ok(ScanOutcome::Stale);
        };
        let mut changed = self.changes;
        if self.authoritative {
            let (album_metadata, album_user_data) =
                entities::finish_albums(&mut transaction, source).await?;
            changed.metadata |= album_metadata;
            changed.user_data |= album_user_data;
        }
        changed.metadata |= sqlx::query("UPDATE tracks SET album_key=album.album_key FROM temp.scan_track_albums pending JOIN albums album ON album.source_key=?1 AND album.object_id=pending.album_id WHERE tracks.source_key=?1 AND tracks.object_id=pending.track_id AND tracks.album_key IS NOT album.album_key")
            .bind(source).execute(&mut *transaction).await?.rows_affected()>0;
        let (removed, removed_playlists) = publish_removals(&mut transaction, source, full).await?;
        changed.metadata |= removed;
        changed.playlists |= removed_playlists;
        changed.metadata |= publish_links(&mut transaction, source, self.authoritative).await?;
        changed.playlists |= publish_playlist_entries(&mut transaction, source).await?;
        let home_changed = publish_home(&mut transaction, source, full).await?;
        if self.local_point_update {
            changed.metadata |= prune_local_orphans(&mut transaction, source).await?;
        }
        changed.metadata |= update_album_loudness(&mut transaction, source).await?;
        changed.metadata |=
            publish_source_loudness(&mut transaction, source, self.authoritative).await?;
        changed.artwork |= artwork::update_artwork(
            &mut transaction,
            source,
            self.distinct_track_covers,
            self.point_update,
            self.local_point_update,
        )
        .await?;
        publish_local_files(&mut transaction, source, full).await?;
        let catalog_changed = changed.metadata;
        changed.metadata |= changed.user_data;
        changed.metadata |= self.publish_user_data(&mut transaction, source).await?;
        changed.metadata |= home_changed;
        let any = changed.metadata || changed.playlists || changed.artwork;
        if current.catalog_revision == 0 && !any && !full {
            return Ok(ScanOutcome::Failed);
        }
        changed.metadata |= current.catalog_revision == 0
            || current.display_name != self.display_name
            || current.normalized_name != self.normalized_name;
        let freshness = if !self.point_update {
            self.freshness.as_ref().map(Freshness::as_bytes)
        } else if catalog_changed {
            None
        } else {
            current.freshness.as_deref()
        };
        let revision = current.catalog_revision + i64::from(changed.metadata || changed.playlists);
        let artwork = if full
            || changed.metadata
            || changed.playlists
            || changed.artwork
            || freshness != current.freshness.as_deref()
            || self.distinct_track_covers != current.distinct_track_covers
        {
            sqlx::query_scalar::<_,Vec<u8>>("UPDATE sources SET display_name=?2,normalized_name=?3,freshness=?4,distinct_track_covers=?5,catalog_revision=?6,artwork_digest=CASE WHEN ?7 THEN randomblob(32) ELSE artwork_digest END,catalog_complete=CASE WHEN ?8 THEN 1 ELSE catalog_complete END WHERE source_key=?1 RETURNING artwork_digest")
                .bind(source).bind(&self.display_name).bind(&self.normalized_name).bind(freshness).bind(self.distinct_track_covers).bind(revision).bind(changed.artwork).bind(full).fetch_one(&mut *transaction).await?
        } else {
            current.artwork_digest
        };
        transaction.commit().await?;
        if full {
            sqlx::raw_sql("PRAGMA optimize=0x10002")
                .execute(&mut *connection)
                .await?;
        }
        let publication = Publication {
            tracks_added: changed.tracks_added,
            ..publication(source, revision, &artwork)?
        };
        Ok(if changed.metadata {
            ScanOutcome::Changed(publication)
        } else if changed.playlists {
            ScanOutcome::PlaylistsChanged(publication)
        } else if changed.artwork {
            ScanOutcome::ArtworkChanged(publication)
        } else {
            ScanOutcome::Identical(publication)
        })
    }

    async fn publish_user_data(
        &self,
        connection: &mut SqliteConnection,
        source: i64,
    ) -> LibraryResult<bool> {
        let mut changed = false;
        for (index, kind) in ["track", "album", "artist"].into_iter().enumerate() {
            if let Some(favorites) = &self.favorites {
                changed |= crate::favorites::publish_source_user_data(connection, kind, |query| {
                    query
                        .push("SELECT ")
                        .push_bind(source)
                        .push(",object_id,object_id IN (SELECT value FROM json_each(")
                        .push_bind(&favorites[index])
                        .push(")),0,NULL,NULL,NULL,NULL FROM ")
                        .push(format!("{kind}s WHERE source_key="))
                        .push_bind(source);
                })
                .await?;
            }
            let facts = self
                .user_data
                .iter()
                .filter(|(item_kind, _, _)| *item_kind == kind);
            if facts.clone().next().is_some() {
                changed |= crate::favorites::publish_source_user_data(connection, kind, |query| {
                    query.push_values(facts.clone(), |mut row, (_, id, fact)| {
                        row.push_bind(source)
                            .push_bind(id)
                            .push_bind(fact.favorite)
                            .push_bind(fact.rating.is_some())
                            .push_bind(fact.rating.flatten())
                            .push_bind(fact.play_count)
                            .push("NULL")
                            .push_bind(fact.last_played);
                    });
                })
                .await?;
            }
        }
        Ok(changed)
    }

    async fn write_links_batch(
        &mut self,
        prefix: &'static str,
        links: &[ScanLink<'_>],
    ) -> LibraryResult<()> {
        if links.is_empty() {
            return Ok(());
        }
        for link in links {
            self.require_id("link owner", link.owner_id)?;
            self.require_id("link target", link.related_id)?;
            if link.position < 0 {
                self.failed = true;
                return Err(LibraryError::InvalidScan(
                    "link position cannot be negative".to_string(),
                ));
            }
            self.require_row_bytes(&[link.owner_id.as_bytes(), link.related_id.as_bytes()])?;
        }
        for page in links.chunks(128) {
            let mut query = QueryBuilder::<Sqlite>::new(prefix);
            query.push_values(page, |mut row, link| {
                row.push_bind(link.owner_id)
                    .push_bind(link.related_id)
                    .push_bind(link.position);
            });
            self.execute(query.build()).await?;
        }
        Ok(())
    }

    async fn write_ordered_relation_batch(
        &mut self,
        table: &'static str,
        owner_kind: &'static str,
        links: &[(&str, &str)],
    ) -> LibraryResult<()> {
        if links.is_empty() {
            return Ok(());
        }
        for (owner, related) in links {
            self.require_id(owner_kind, owner)?;
            self.require_id("relation", related)?;
            self.require_row_bytes(&[owner.as_bytes(), related.as_bytes()])?;
        }
        for page in links.chunks(128) {
            let mut query =
                QueryBuilder::<Sqlite>::new("WITH input(owner_id,related_id,ordinal) AS (");
            query.push_values(
                page.iter().enumerate(),
                |mut row, (ordinal, (owner, related))| {
                    row.push_bind(*owner)
                        .push_bind(*related)
                        .push_bind(ordinal as i64);
                },
            );
            query.push(
                "), deduplicated AS (
                 SELECT owner_id,related_id,min(ordinal) AS ordinal
                 FROM input GROUP BY owner_id,related_id
             ), ranked AS (
                 SELECT owner_id,related_id,
                        row_number() OVER (PARTITION BY owner_id ORDER BY ordinal)-1 AS offset
                 FROM deduplicated
             ) INSERT OR IGNORE INTO ",
            );
            query.push(table);
            query.push(
                "(owner_id,related_id,position)
             SELECT owner_id,related_id,
                    COALESCE((SELECT max(position)+1 FROM ",
            );
            query.push(table);
            query.push(
                " existing WHERE existing.owner_id=ranked.owner_id),0)+offset
             FROM ranked",
            );
            self.execute(query.build()).await?;
        }
        Ok(())
    }

    fn require_id(&mut self, kind: &'static str, value: &str) -> LibraryResult<()> {
        if let Err(error) = validate_id(kind, value) {
            self.failed = true;
            return Err(error);
        }
        Ok(())
    }

    fn require_row_bytes(&mut self, values: &[&[u8]]) -> LibraryResult<()> {
        let bytes = values
            .iter()
            .try_fold(0_usize, |total, value| total.checked_add(value.len()));
        if bytes.is_none_or(|bytes| bytes > MAX_ROW_BYTES) {
            self.failed = true;
            return Err(LibraryError::InvalidScan(
                "one acquired row may not exceed 8 MiB".to_string(),
            ));
        }
        Ok(())
    }

    async fn execute<'query>(
        &mut self,
        query: Query<'query, Sqlite, SqliteArguments>,
    ) -> LibraryResult<()> {
        self.execute_result(query).await.map(|_| ())
    }

    async fn execute_result<'query>(
        &mut self,
        query: Query<'query, Sqlite, SqliteArguments>,
    ) -> LibraryResult<SqliteQueryResult> {
        if self.failed {
            return Err(LibraryError::ScanFailed);
        }
        let result = if let Some(writer) = &mut self.batch_writer {
            query
                .execute(writer.as_mut().ok_or(LibraryError::WriterUnavailable)?)
                .await
        } else {
            let mut writer = self.database.writer().await?;
            let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
            if !source_exists(connection, self.source, &self.source_id).await? {
                self.failed = true;
                return Err(LibraryError::InvalidScan(
                    "source was removed during acquisition".into(),
                ));
            }
            query.execute(connection).await
        };
        if result.is_err() {
            self.failed = true;
        }
        Ok(result?)
    }
}

impl Database {
    pub async fn reconcile_source(&self, source_id: &SourceId) -> LibraryResult<Publication> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let source = SourceKey::from_raw(
            ensure_source_identity(&mut transaction, source_id.as_str()).await?,
        );
        let observation = sqlx::query_as::<_, (i64, Vec<u8>)>(
            "SELECT catalog_revision,artwork_digest FROM sources
             WHERE source_key=?1 AND catalog_revision>0",
        )
        .bind(source)
        .fetch_optional(&mut *transaction)
        .await?;
        transaction.commit().await?;
        match observation {
            Some((revision, artwork)) => publication(source.raw(), revision, &artwork),
            None => Ok(Publication {
                source,
                catalog_revision: 0,
                artwork_digest: [0; 32],
                tracks_added: false,
            }),
        }
    }

    pub async fn cached_source(
        &self,
        object_id: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<CachedSource>> {
        validate_id("source", object_id)?;
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_as::<_, CachedSource>(
            "SELECT source_key source,object_id,display_name,catalog_revision,artwork_digest
             FROM sources WHERE object_id=?1 AND catalog_revision>0",
        )
        .bind(object_id)
        .fetch_optional(&mut *connection)
        .await?)
    }
}

impl Drop for Scan {
    fn drop(&mut self) {
        if let Some(writer) = self.batch_writer.as_mut() {
            if let Some(connection) = writer.as_mut() {
                sqlx::sqlite::SqliteTransactionManager::start_rollback(connection);
            }
        }
    }
}

async fn fetch_local_dependency_paths(
    connection: &mut sqlx::SqliteConnection,
    paths: &[String],
) -> LibraryResult<Vec<String>> {
    let mut query = QueryBuilder::<Sqlite>::new(
        "SELECT path FROM temp.scan_local_dependency_paths WHERE path IN (",
    );
    let mut separated = query.separated(",");
    for path in paths {
        separated.push_bind(path);
    }
    separated.push_unseparated(")");
    Ok(query
        .build_query_scalar()
        .persistent(false)
        .fetch_all(connection)
        .await?)
}

fn validate_id(kind: &'static str, value: &str) -> LibraryResult<()> {
    if value.is_empty() {
        return Err(LibraryError::InvalidScan(format!(
            "{kind} identity is empty"
        )));
    }
    Ok(())
}

fn publication(
    source_key: i64,
    revision: i64,
    artwork_digest: &[u8],
) -> LibraryResult<Publication> {
    let revision = u64::try_from(revision)
        .map_err(|_| LibraryError::InvalidStore("negative catalog revision".to_string()))?;
    Ok(Publication {
        source: SourceKey::from_raw(source_key),
        catalog_revision: revision,
        tracks_added: false,
        artwork_digest: artwork_digest.try_into().map_err(|_| {
            LibraryError::InvalidStore("source artwork digest is not 32 bytes".to_string())
        })?,
    })
}

async fn create_working_set(connection: &mut SqliteConnection) -> LibraryResult<()> {
    sqlx::raw_sql(r#"
    CREATE TEMP TABLE IF NOT EXISTS scan_seen(kind TEXT NOT NULL,object_id TEXT NOT NULL,written INTEGER NOT NULL DEFAULT 0,supplied INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(kind,object_id)) STRICT, WITHOUT ROWID;
    CREATE TEMP TABLE IF NOT EXISTS scan_unresolved_artists(object_id TEXT PRIMARY KEY) STRICT, WITHOUT ROWID;
    CREATE TEMP TABLE IF NOT EXISTS scan_track_albums(track_id TEXT PRIMARY KEY,album_id TEXT NOT NULL) STRICT, WITHOUT ROWID;
    CREATE TEMP TABLE IF NOT EXISTS scan_affected_albums(album_key INTEGER PRIMARY KEY) STRICT;
    CREATE TEMP TABLE IF NOT EXISTS scan_loudness(kind TEXT NOT NULL,object_id TEXT NOT NULL,integrated_lufs REAL,true_peak REAL,replay_gain_db REAL,replay_gain_peak REAL,PRIMARY KEY(kind,object_id)) STRICT, WITHOUT ROWID;
    CREATE TEMP TABLE IF NOT EXISTS scan_artwork(kind TEXT NOT NULL,object_id TEXT NOT NULL,source BLOB,binding BLOB,PRIMARY KEY(kind,object_id)) STRICT, WITHOUT ROWID;
    CREATE TEMP TABLE IF NOT EXISTS scan_local_dependency_paths(path TEXT PRIMARY KEY) STRICT;
    CREATE TEMP TABLE IF NOT EXISTS scan_local_files(path TEXT PRIMARY KEY,root TEXT NOT NULL,relative_path TEXT NOT NULL,kind TEXT NOT NULL,size_bytes INTEGER,mtime_ns INTEGER NOT NULL,device_id INTEGER,inode INTEGER,native_id TEXT,revision TEXT,picture_index INTEGER,artist_pictures TEXT,parse_version INTEGER,state TEXT NOT NULL) STRICT;
    CREATE INDEX IF NOT EXISTS scan_local_files_kind_path ON scan_local_files(kind,path);
    CREATE INDEX IF NOT EXISTS scan_local_files_native_id ON scan_local_files(native_id,path) WHERE native_id IS NOT NULL;
    CREATE TEMP TABLE IF NOT EXISTS scan_local_file_dependencies(path TEXT NOT NULL,dependency_path TEXT NOT NULL,position INTEGER NOT NULL,PRIMARY KEY(path,dependency_path)) STRICT;
    CREATE TEMP TABLE IF NOT EXISTS scan_local_file_removals(path TEXT PRIMARY KEY) STRICT;
    CREATE TEMP TABLE IF NOT EXISTS scan_retained_paths(path TEXT PRIMARY KEY) STRICT;
    CREATE TEMP TABLE IF NOT EXISTS scan_local_component_paths(path TEXT PRIMARY KEY) STRICT;
    CREATE TEMP TABLE IF NOT EXISTS scan_removals(entity_kind TEXT NOT NULL,object_id TEXT NOT NULL,PRIMARY KEY(entity_kind,object_id)) STRICT;
    CREATE TEMP TABLE IF NOT EXISTS scan_playlist_scopes(object_id TEXT PRIMARY KEY,entries_supplied INTEGER NOT NULL DEFAULT 1) STRICT;
    CREATE TEMP TABLE IF NOT EXISTS scan_playlist_entries(playlist_id TEXT NOT NULL,object_id TEXT NOT NULL,track_id TEXT NOT NULL,media_uri TEXT NOT NULL,position INTEGER NOT NULL,PRIMARY KEY(playlist_id,object_id)) STRICT;
    CREATE TEMP TABLE IF NOT EXISTS scan_retained_home(owner_id TEXT PRIMARY KEY) STRICT;
    CREATE TEMP TABLE IF NOT EXISTS scan_home_entries(owner_id TEXT NOT NULL,position INTEGER NOT NULL,entity_kind TEXT NOT NULL,entity_object_id TEXT NOT NULL,title TEXT NOT NULL,subtitle TEXT NOT NULL,section_title TEXT,PRIMARY KEY(owner_id,position)) STRICT;
    "#).execute(&mut *connection).await?;
    for relation in [
        "album_artists",
        "track_artists",
        "album_genres",
        "track_genres",
        "track_moods",
        "track_folders",
        "album_release_types",
    ] {
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE TEMP TABLE IF NOT EXISTS scan_{relation}(owner_id TEXT NOT NULL,related_id TEXT NOT NULL,position INTEGER NOT NULL,PRIMARY KEY(owner_id,related_id)) STRICT, WITHOUT ROWID; CREATE INDEX IF NOT EXISTS scan_{relation}_related ON scan_{relation}(related_id,owner_id);"))).execute(&mut *connection).await?;
    }
    clear_working_set(connection).await
}

async fn clear_working_set(connection: &mut SqliteConnection) -> LibraryResult<()> {
    sqlx::raw_sql("DELETE FROM temp.scan_seen; DELETE FROM temp.scan_unresolved_artists; DELETE FROM temp.scan_track_albums; DELETE FROM temp.scan_affected_albums; DELETE FROM temp.scan_loudness; DELETE FROM temp.scan_artwork;
        DELETE FROM temp.scan_local_dependency_paths; DELETE FROM temp.scan_local_file_dependencies; DELETE FROM temp.scan_local_file_removals; DELETE FROM temp.scan_local_files; DELETE FROM temp.scan_retained_paths; DELETE FROM temp.scan_local_component_paths; DELETE FROM temp.scan_removals;
        DELETE FROM temp.scan_home_entries; DELETE FROM temp.scan_retained_home; DELETE FROM temp.scan_playlist_entries; DELETE FROM temp.scan_playlist_scopes;
        DELETE FROM temp.scan_album_release_types; DELETE FROM temp.scan_track_folders; DELETE FROM temp.scan_track_moods; DELETE FROM temp.scan_track_genres; DELETE FROM temp.scan_album_genres; DELETE FROM temp.scan_track_artists; DELETE FROM temp.scan_album_artists;")
        .execute(connection).await?;
    Ok(())
}

async fn update_album_loudness(
    connection: &mut SqliteConnection,
    source: i64,
) -> LibraryResult<bool> {
    sqlx::query("INSERT OR IGNORE INTO temp.scan_affected_albums SELECT album.album_key FROM albums album WHERE album.source_key=?1 AND (EXISTS(SELECT 1 FROM temp.scan_seen WHERE kind='album' AND written=1 AND object_id=album.object_id) OR EXISTS(SELECT 1 FROM tracks track JOIN temp.scan_seen seen ON seen.kind='track' AND seen.written=1 AND seen.object_id=track.object_id WHERE track.source_key=?1 AND track.album_key=album.album_key))").bind(source).execute(&mut *connection).await?;
    let empty = *album_loudness_hasher().finalize().as_bytes();
    let mut changed=sqlx::query("UPDATE albums SET source_loudness_analysis_key=?2,loudness_analysis_key=?2 WHERE source_key=?1 AND album_key IN (SELECT album_key FROM temp.scan_affected_albums) AND NOT EXISTS(SELECT 1 FROM tracks WHERE tracks.album_key=albums.album_key) AND (source_loudness_analysis_key,loudness_analysis_key) IS NOT (?2,?2)").bind(source).bind(empty.as_slice()).execute(&mut *connection).await?.rows_affected()>0;
    let mut last = (String::new(), -1_i64, -1_i64, String::new(), String::new());
    let mut current_album: Option<String> = None;
    let mut source_hasher = album_loudness_hasher();
    let mut current_hasher = album_loudness_hasher();
    loop {
        let page=sqlx::query_as::<_,AlbumAudioKey>("SELECT album.object_id album_object_id,track.disc_number,track.track_number,track.sort_text,track.object_id,track.source_loudness_analysis_key source_key,track.loudness_analysis_key current_key FROM tracks track JOIN albums album USING(album_key) WHERE track.source_key=?1 AND album.album_key IN (SELECT album_key FROM temp.scan_affected_albums) AND (album.object_id,track.disc_number,track.track_number,track.sort_text,track.object_id)>(?2,?3,?4,?5,?6) ORDER BY album.object_id,track.disc_number,track.track_number,track.sort_text,track.object_id LIMIT ?7").bind(source).bind(&last.0).bind(last.1).bind(last.2).bind(&last.3).bind(&last.4).bind(LOUDNESS_PAGE_ROWS).fetch_all(&mut *connection).await?;
        let mut updates = Vec::new();
        if page.is_empty() {
            if let Some(album) = current_album.take() {
                updates.push((
                    album,
                    *source_hasher.finalize().as_bytes(),
                    *current_hasher.finalize().as_bytes(),
                ));
            }
            changed |= write_album_loudness_keys(connection, source, &updates).await?;
            break;
        }
        for row in &page {
            if current_album.as_deref() != Some(row.album_object_id.as_str()) {
                if let Some(album) = current_album.replace(row.album_object_id.clone()) {
                    updates.push((
                        album,
                        *source_hasher.finalize().as_bytes(),
                        *current_hasher.finalize().as_bytes(),
                    ));
                }
                source_hasher = album_loudness_hasher();
                current_hasher = album_loudness_hasher();
            }
            source_hasher.update(&row.source_key);
            current_hasher.update(&row.current_key);
        }
        let row = page.last().unwrap();
        last = (
            row.album_object_id.clone(),
            row.disc_number,
            row.track_number,
            row.sort_text.clone(),
            row.object_id.clone(),
        );
        changed |= write_album_loudness_keys(connection, source, &updates).await?;
    }
    Ok(changed)
}

fn album_loudness_hasher() -> Hasher {
    let mut hasher = Hasher::new();
    hasher.update(b"rufin-album-loudness-v1\0");
    hasher
}

async fn write_album_loudness_keys(
    connection: &mut SqliteConnection,
    source: i64,
    updates: &[(String, [u8; 32], [u8; 32])],
) -> LibraryResult<bool> {
    if updates.is_empty() {
        return Ok(false);
    }
    let mut query = QueryBuilder::<Sqlite>::new("WITH next(object_id,source_key,current_key) AS (");
    query.push_values(updates, |mut row, (id, source, current)| {
        row.push_bind(id)
            .push_bind(source.as_slice())
            .push_bind(current.as_slice());
    });
    query.push(") UPDATE albums SET source_loudness_analysis_key=next.source_key,loudness_analysis_key=next.current_key FROM next WHERE albums.source_key=").push_bind(source).push(" AND albums.object_id=next.object_id AND (albums.source_loudness_analysis_key,albums.loudness_analysis_key) IS NOT (next.source_key,next.current_key)");
    Ok(query
        .build()
        .persistent(false)
        .execute(connection)
        .await?
        .rows_affected()
        > 0)
}

async fn publish_source_loudness(
    transaction: &mut Transaction<'_, Sqlite>,
    source: i64,
    authoritative: bool,
) -> LibraryResult<bool> {
    let mut changed = false;
    for kind in ["track", "album"] {
        for (table, value, peak, origin) in [
            (
                "loudness_measurements",
                "integrated_lufs",
                "true_peak",
                true,
            ),
            (
                "replay_gain_measurements",
                "replay_gain_db",
                "replay_gain_peak",
                false,
            ),
        ] {
            let key = format!("{kind}_key");
            let origin_filter = if origin { "AND origin='source'" } else { "" };
            if authoritative || kind == "track" {
                changed |= sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {table} WHERE source_key=?1 AND entity_kind=?2 {origin_filter} AND entity_key IN (SELECT item.{key} FROM {kind}s item JOIN temp.scan_seen seen ON seen.kind=?2 AND seen.written=1 AND seen.object_id=item.object_id WHERE item.source_key=?1) AND NOT EXISTS(SELECT 1 FROM temp.scan_loudness supplied JOIN {kind}s item ON item.source_key=?1 AND item.object_id=supplied.object_id WHERE supplied.kind=?2 AND supplied.{value} IS NOT NULL AND item.source_loudness_analysis_key=item.loudness_analysis_key AND item.{key}={table}.entity_key)"))).bind(source).bind(kind).execute(&mut **transaction).await?.rows_affected()>0;
            }
            let (target_value, target_peak) = if origin {
                ("integrated_lufs", "true_peak")
            } else {
                ("gain_db", "peak")
            };
            let origin_columns = if origin { ",origin" } else { "" };
            let origin_value = if origin { ",'source'" } else { "" };
            let origin_update = if origin { ",origin='source'" } else { "" };
            let origin_comparison = if origin { ",origin" } else { "" };
            changed |= sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO {table}(source_key,entity_kind,entity_key,analysis_key,{target_value},{target_peak}{origin_columns}) SELECT ?1,?2,item.{key},item.loudness_analysis_key,supplied.{value},supplied.{peak}{origin_value} FROM temp.scan_loudness supplied JOIN {kind}s item ON item.source_key=?1 AND item.object_id=supplied.object_id WHERE supplied.kind=?2 AND supplied.{value} IS NOT NULL AND item.source_loudness_analysis_key=item.loudness_analysis_key ON CONFLICT(source_key,entity_kind,entity_key) DO UPDATE SET analysis_key=excluded.analysis_key,{target_value}=excluded.{target_value},{target_peak}=excluded.{target_peak}{origin_update} WHERE (analysis_key,{target_value},{target_peak}{origin_comparison}) IS NOT (excluded.analysis_key,excluded.{target_value},excluded.{target_peak}{origin_value})"))).bind(source).bind(kind).execute(&mut **transaction).await?.rows_affected()>0;
        }
    }
    Ok(changed)
}

async fn publish_local_files(
    transaction: &mut Transaction<'_, Sqlite>,
    source_key: i64,
    full: bool,
) -> LibraryResult<()> {
    if full {
        sqlx::query(
            "DELETE FROM local_files WHERE source_key=?1
             AND path NOT IN (SELECT path FROM temp.scan_local_files)",
        )
        .bind(source_key)
        .execute(&mut **transaction)
        .await?;
    } else {
        sqlx::query(
            "DELETE FROM local_files WHERE source_key=?1
             AND path IN (SELECT path FROM temp.scan_local_file_removals)",
        )
        .bind(source_key)
        .execute(&mut **transaction)
        .await?;
    }
    sqlx::query(
        "INSERT INTO local_files(
             source_key,path,root,relative_path,kind,size_bytes,mtime_ns,
             device_id,inode,native_id,revision,picture_index,artist_pictures,parse_version,state
         )
         SELECT ?1,path,root,relative_path,kind,size_bytes,mtime_ns,
                device_id,inode,native_id,CASE WHEN kind='directory' AND ?2=0 THEN NULL ELSE revision END,picture_index,artist_pictures,parse_version,state
         FROM temp.scan_local_files WHERE true
         ON CONFLICT(source_key,path) DO UPDATE SET
             root=excluded.root,relative_path=excluded.relative_path,
             kind=excluded.kind,size_bytes=excluded.size_bytes,
             mtime_ns=excluded.mtime_ns,device_id=excluded.device_id,
             inode=excluded.inode,parse_version=excluded.parse_version,
             native_id=excluded.native_id,revision=excluded.revision,picture_index=excluded.picture_index,artist_pictures=CASE WHEN local_files.size_bytes IS excluded.size_bytes AND local_files.mtime_ns=excluded.mtime_ns AND local_files.revision IS excluded.revision THEN COALESCE(excluded.artist_pictures,local_files.artist_pictures) ELSE excluded.artist_pictures END,
             state=excluded.state",
    )
    .bind(source_key)
    .bind(full)
    .execute(&mut **transaction)
    .await?;
    sqlx::query(
        "DELETE FROM local_file_dependencies WHERE local_file_key IN (
             SELECT file.local_file_key FROM local_files file
             JOIN temp.scan_local_files staged USING(path)
             WHERE file.source_key=?1
         )",
    )
    .bind(source_key)
    .execute(&mut **transaction)
    .await?;
    sqlx::query(
        "INSERT INTO local_file_dependencies(local_file_key,dependency_path,position)
         SELECT file.local_file_key,dependency.dependency_path,dependency.position
         FROM temp.scan_local_file_dependencies dependency
         JOIN local_files file ON file.source_key=?1 AND file.path=dependency.path",
    )
    .bind(source_key)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn prune_local_orphans(
    transaction: &mut Transaction<'_, Sqlite>,
    source_key: i64,
) -> LibraryResult<bool> {
    let mut changed = false;
    for sql in [
        "DELETE FROM albums WHERE source_key=?1 AND NOT EXISTS (SELECT 1 FROM tracks WHERE tracks.album_key=albums.album_key)",
        "DELETE FROM artists WHERE source_key=?1 AND NOT EXISTS (SELECT 1 FROM track_artists WHERE track_artists.artist_key=artists.artist_key) AND NOT EXISTS (SELECT 1 FROM album_artists WHERE album_artists.artist_key=artists.artist_key)",
        "DELETE FROM genres WHERE source_key=?1 AND NOT EXISTS (SELECT 1 FROM track_genres WHERE track_genres.genre_key=genres.genre_key) AND NOT EXISTS (SELECT 1 FROM album_genres WHERE album_genres.genre_key=genres.genre_key)",
        "DELETE FROM moods WHERE source_key=?1 AND NOT EXISTS (SELECT 1 FROM track_moods WHERE track_moods.mood_key=moods.mood_key)",
        "DELETE FROM folders WHERE source_key=?1 AND NOT EXISTS (SELECT 1 FROM track_folders WHERE track_folders.folder_key=folders.folder_key)",
    ] {
        changed |= sqlx::query(sql)
            .bind(source_key)
            .execute(&mut **transaction)
            .await?
            .rows_affected()
            > 0;
    }
    Ok(changed)
}

async fn publish_removals(
    transaction: &mut Transaction<'_, Sqlite>,
    source_key: i64,
    full: bool,
) -> LibraryResult<(bool, bool)> {
    let mut metadata = false;
    let mut playlists = false;
    for (kind, table) in [
        ("track", "tracks"),
        ("album", "albums"),
        ("artist", "artists"),
        ("genre", "genres"),
        ("mood", "moods"),
        ("folder", "folders"),
        ("playlist", "catalog.native_playlists"),
    ] {
        let sql = format!(
            "DELETE FROM {table} WHERE source_key=?1
             {} AND object_id NOT IN (SELECT object_id FROM temp.scan_seen WHERE kind='{kind}')",
            if full {
                ""
            } else {
                "AND object_id IN (SELECT object_id FROM temp.scan_removals WHERE entity_kind=?2)"
            }
        );
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(source_key);
        if !full {
            query = query.bind(kind);
        }
        let changed = query.execute(&mut **transaction).await?.rows_affected() > 0;
        if kind == "playlist" {
            playlists |= changed;
        } else {
            metadata |= changed;
        }
    }
    Ok((metadata, playlists))
}

async fn publish_links(
    transaction: &mut Transaction<'_, Sqlite>,
    source: i64,
    authoritative: bool,
) -> LibraryResult<bool> {
    let mut changed = false;
    for (relation, owners, owner_key, targets, target_key) in [
        (
            "album_artists",
            "albums",
            "album_key",
            "artists",
            "artist_key",
        ),
        (
            "track_artists",
            "tracks",
            "track_key",
            "artists",
            "artist_key",
        ),
        ("album_genres", "albums", "album_key", "genres", "genre_key"),
        ("track_genres", "tracks", "track_key", "genres", "genre_key"),
        ("track_moods", "tracks", "track_key", "moods", "mood_key"),
        (
            "track_folders",
            "tracks",
            "track_key",
            "folders",
            "folder_key",
        ),
        (
            "album_release_types",
            "albums",
            "album_key",
            "",
            "release_type",
        ),
    ] {
        let (target, join) = if targets.is_empty() {
            ("link.related_id".to_string(), String::new())
        } else {
            (
                format!("target.{target_key}"),
                format!(
                    "CROSS JOIN {targets} target ON target.source_key=?1 AND target.object_id=link.related_id"
                ),
            )
        };
        let rows = format!(
            "SELECT owner.{owner_key} AS owner,{target} AS target,
             row_number() OVER(PARTITION BY link.owner_id ORDER BY link.position,link.related_id)-1 AS position
             FROM temp.scan_{relation} link CROSS JOIN {owners} owner ON owner.source_key=?1 AND owner.object_id=link.owner_id {join}"
        );
        if !authoritative && owners == "albums" {
            let append = format!(
                "WITH incoming AS ({rows}), additions AS MATERIALIZED (
                    SELECT incoming.*,COALESCE((SELECT max(position) FROM {relation}
                        WHERE {owner_key}=incoming.owner),-1) AS previous
                    FROM incoming WHERE NOT EXISTS(SELECT 1 FROM {relation}
                        WHERE {owner_key}=incoming.owner AND {target_key}=incoming.target))
                 INSERT INTO {relation}({owner_key},{target_key},position)
                 SELECT owner,target,previous+row_number() OVER(PARTITION BY owner ORDER BY position)
                 FROM additions"
            );
            changed |= sqlx::query(sqlx::AssertSqlSafe(append))
                .bind(source)
                .execute(&mut **transaction)
                .await?
                .rows_affected()
                > 0;
            continue;
        }
        let delete = format!(
            "WITH expected AS MATERIALIZED ({rows}) DELETE FROM {relation} AS current
             WHERE {owner_key} IN (SELECT {owner_key} FROM {owners} WHERE source_key=?1 {})
             AND NOT EXISTS(SELECT 1 FROM expected WHERE expected.owner=current.{owner_key}
                 AND expected.target=current.{target_key} AND expected.position=current.position)",
            if relation == "track_folders" {
                "AND object_id IN (SELECT owner_id FROM temp.scan_track_folders
                 UNION SELECT object_id FROM temp.scan_removals WHERE entity_kind='track-folders')"
                    .to_string()
            } else {
                format!(
                    "AND object_id IN (SELECT object_id FROM temp.scan_seen WHERE kind='{}' AND written=1)",
                    owners.trim_end_matches('s')
                )
            }
        );
        changed |= sqlx::query(sqlx::AssertSqlSafe(delete))
            .bind(source)
            .execute(&mut **transaction)
            .await?
            .rows_affected()
            > 0;
        let insert =
            format!("INSERT OR IGNORE INTO {relation}({owner_key},{target_key},position) {rows}");
        changed |= sqlx::query(sqlx::AssertSqlSafe(insert))
            .bind(source)
            .execute(&mut **transaction)
            .await?
            .rows_affected()
            > 0;
    }
    Ok(changed)
}

async fn publish_home(
    transaction: &mut Transaction<'_, Sqlite>,
    source: i64,
    full: bool,
) -> LibraryResult<bool> {
    let rows = "SELECT ?1 AS source_key,entry.owner_id AS section_id,entry.position,entry.entity_kind,
        CASE entry.entity_kind
          WHEN 'track' THEN (SELECT track_key FROM tracks WHERE source_key=?1 AND object_id=entry.entity_object_id)
          WHEN 'album' THEN (SELECT album_key FROM albums WHERE source_key=?1 AND object_id=entry.entity_object_id)
          WHEN 'artist' THEN (SELECT artist_key FROM artists WHERE source_key=?1 AND object_id=entry.entity_object_id)
          WHEN 'playlist' THEN (SELECT playlist_key FROM playlists WHERE source_key=?1 AND object_id=entry.entity_object_id)
        END AS entity_key,entry.title,entry.subtitle,entry.section_title
        FROM temp.scan_home_entries entry";
    let delete = format!("WITH expected AS MATERIALIZED ({rows}) DELETE FROM home_entries AS current WHERE source_key=?1
        AND ((?2 AND section_id NOT IN (SELECT owner_id FROM temp.scan_retained_home)) OR section_id IN (SELECT object_id FROM temp.scan_removals WHERE entity_kind='home'))
        AND NOT EXISTS(SELECT 1 FROM expected WHERE expected.section_id=current.section_id AND expected.position=current.position AND expected.entity_key IS NOT NULL)");
    let mut changed = sqlx::query(sqlx::AssertSqlSafe(delete))
        .bind(source)
        .bind(full)
        .execute(&mut **transaction)
        .await?
        .rows_affected()
        > 0;
    let insert = format!("WITH expected AS ({rows}) INSERT INTO home_entries(source_key,section_id,position,entity_kind,entity_key,title,subtitle,section_title)
        SELECT * FROM expected WHERE entity_key IS NOT NULL ON CONFLICT(source_key,section_id,position) DO UPDATE SET
        entity_kind=excluded.entity_kind,entity_key=excluded.entity_key,title=excluded.title,subtitle=excluded.subtitle,section_title=excluded.section_title
        WHERE (home_entries.entity_kind,home_entries.entity_key,home_entries.title,home_entries.subtitle,home_entries.section_title)
        IS NOT (excluded.entity_kind,excluded.entity_key,excluded.title,excluded.subtitle,excluded.section_title)");
    changed |= sqlx::query(sqlx::AssertSqlSafe(insert))
        .bind(source)
        .execute(&mut **transaction)
        .await?
        .rows_affected()
        > 0;
    Ok(changed)
}

async fn publish_playlist_entries(
    transaction: &mut Transaction<'_, Sqlite>,
    source_key: i64,
) -> LibraryResult<bool> {
    sqlx::query("WITH ranked AS (
        SELECT playlist_id,object_id,row_number() OVER(PARTITION BY playlist_id ORDER BY position,object_id)-1 AS position
        FROM temp.scan_playlist_entries)
        UPDATE temp.scan_playlist_entries AS entry SET position=ranked.position FROM ranked
        WHERE entry.playlist_id=ranked.playlist_id AND entry.object_id=ranked.object_id AND entry.position IS NOT ranked.position")
        .execute(&mut **transaction).await?;
    let mut changed = sqlx::query(
        "WITH moved AS MATERIALIZED (
             SELECT playlist.playlist_key,max(
                 (SELECT coalesce(max(position),0) FROM catalog.native_playlist_entries existing WHERE existing.playlist_key=playlist.playlist_key),
                 (SELECT coalesce(max(position),0) FROM temp.scan_playlist_entries entry WHERE entry.playlist_id=playlist.object_id)
             )+1 AS offset
             FROM temp.scan_playlist_scopes staged CROSS JOIN catalog.native_playlists playlist ON playlist.object_id=staged.object_id
             WHERE playlist.source_key=?1 AND staged.entries_supplied AND EXISTS(
                 SELECT 1 FROM temp.scan_playlist_entries entry LEFT JOIN catalog.native_playlist_entries current
                 ON current.playlist_key=playlist.playlist_key AND current.object_id=entry.object_id
                 WHERE entry.playlist_id=playlist.object_id AND entry.position IS NOT current.position)
         ) UPDATE catalog.native_playlist_entries SET position=position + (SELECT offset FROM moved WHERE moved.playlist_key=native_playlist_entries.playlist_key)
         WHERE playlist_entry_key IN (SELECT entry.playlist_entry_key FROM moved CROSS JOIN catalog.native_playlist_entries entry ON entry.playlist_key=moved.playlist_key)",
    ).bind(source_key).execute(&mut **transaction).await?.rows_affected() > 0;
    changed |= sqlx::query(
        "DELETE FROM catalog.native_playlist_entries WHERE playlist_entry_key IN (
             SELECT entry.playlist_entry_key FROM temp.scan_playlist_scopes staged CROSS JOIN catalog.native_playlists playlist
             ON playlist.source_key=?1 AND playlist.object_id=staged.object_id
             CROSS JOIN catalog.native_playlist_entries entry ON entry.playlist_key=playlist.playlist_key
             WHERE staged.entries_supplied AND NOT EXISTS(SELECT 1 FROM temp.scan_playlist_entries supplied
                 WHERE supplied.playlist_id=staged.object_id AND supplied.object_id=entry.object_id))",
    ).bind(source_key).execute(&mut **transaction).await?.rows_affected() > 0;
    changed |= sqlx::query("INSERT INTO catalog.native_playlist_entries(playlist_key, object_id, media_uri, title, artist, album, album_display_artist, duration_millis, disc_number, track_number, year, release_date, source_format, musicbrainz_recording_id, musicbrainz_release_track_id, position) SELECT playlist.playlist_key, entry.object_id, entry.media_uri, track.title, track.display_artist, track.display_album, album.display_artist, track.duration_millis, track.disc_number, track.track_number, track.year, track.release_date, track.source_format, track.musicbrainz_recording_id, track.musicbrainz_release_track_id, row_number() OVER (
                    PARTITION BY entry.playlist_id ORDER BY entry.position,entry.object_id
                  )-1
           FROM temp.scan_playlist_entries AS entry
           CROSS JOIN catalog.native_playlists AS playlist
             ON playlist.source_key=?1
            AND playlist.object_id=entry.playlist_id
           LEFT JOIN tracks AS track
             ON track.media_uri=entry.media_uri
           LEFT JOIN albums AS album USING(album_key)
           WHERE true
         ON CONFLICT(playlist_key, object_id) DO UPDATE SET
             media_uri=excluded.media_uri,
             title=excluded.title,
             artist=excluded.artist,
             album=excluded.album,
             album_display_artist=excluded.album_display_artist,
             duration_millis=excluded.duration_millis,
             disc_number=excluded.disc_number,
             track_number=excluded.track_number,
             year=excluded.year,
             release_date=excluded.release_date,
             source_format=excluded.source_format,
             musicbrainz_recording_id=excluded.musicbrainz_recording_id,
             musicbrainz_release_track_id=excluded.musicbrainz_release_track_id,
             position=excluded.position
         WHERE (native_playlist_entries.media_uri,native_playlist_entries.title,native_playlist_entries.artist,native_playlist_entries.album,native_playlist_entries.album_display_artist,native_playlist_entries.duration_millis,native_playlist_entries.disc_number,native_playlist_entries.track_number,native_playlist_entries.year,native_playlist_entries.release_date,native_playlist_entries.source_format,native_playlist_entries.musicbrainz_recording_id,native_playlist_entries.musicbrainz_release_track_id,native_playlist_entries.position) IS NOT (excluded.media_uri,excluded.title,excluded.artist,excluded.album,excluded.album_display_artist,excluded.duration_millis,excluded.disc_number,excluded.track_number,excluded.year,excluded.release_date,excluded.source_format,excluded.musicbrainz_recording_id,excluded.musicbrainz_release_track_id,excluded.position)").bind(source_key).execute(&mut **transaction).await?.rows_affected() > 0;
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn writer_failure_and_begin_query_failure_release_active_scan() {
        let directory = tempfile::tempdir().expect("temporary Store directory");
        let database = Database::open(directory.path().join("library.sqlite3"))
            .await
            .expect("open Library");
        {
            let mut writer = database.writer().await.expect("acquire writer");
            sqlx::raw_sql("ALTER TABLE sources RENAME TO hidden_sources")
                .execute(writer.as_mut().expect("writer available"))
                .await
                .expect("hide sources before source lookup");
        }
        let construction_error = Scan::begin(&database, "source", "Source", "source", None)
            .await
            .err()
            .expect("source lookup fails during acquisition construction");
        assert!(matches!(construction_error, LibraryError::Sqlite(_)));
        {
            let mut writer = database.writer().await.expect("reacquire writer");
            sqlx::raw_sql("ALTER TABLE hidden_sources RENAME TO sources")
                .execute(writer.as_mut().expect("writer available"))
                .await
                .expect("restore sources after construction failure");
        }
        let mut scan = Scan::begin(&database, "source", "Source", "source", None)
            .await
            .expect("next acquisition gets the released permit");
        scan.write_genre("genre", "Genre", "genre", Some("genre"), None)
            .await
            .expect("write before writer failure");
        database.fail_writer().await.expect("fail fixed writer");
        assert!(matches!(
            scan.write_mood("mood", "Mood", "mood", "mood").await,
            Err(LibraryError::WriterUnavailable)
        ));
        assert!(matches!(
            scan.finish().await,
            Err(LibraryError::WriterUnavailable)
        ));
        drop(
            database
                .acquire_scan()
                .expect("failed completion releases its permit"),
        );
    }

    #[tokio::test]
    async fn scan_releases_writer_for_interleaved_point_write() {
        let directory = tempfile::tempdir().expect("temporary Store directory");
        let database = Database::open(directory.path().join("library.sqlite3"))
            .await
            .expect("open Library");
        let mut scan = Scan::begin(&database, "source", "Source", "source", None)
            .await
            .expect("begin acquisition");
        scan.begin_batch().await.expect("begin bounded Scan batch");
        for number in 0..128 {
            scan.write_mood(
                &format!("mood-{number}"),
                "Mood",
                "mood",
                &format!("mood-{number}"),
            )
            .await
            .expect("write one row in the bounded transaction");
        }
        let point_database = database.clone();
        let (started_send, started_receive) = tokio::sync::oneshot::channel();
        let (acquired_send, mut acquired_receive) = tokio::sync::oneshot::channel();
        let point_write = tokio::spawn(async move {
            let _ = started_send.send(());
            let mut writer = point_database
                .writer()
                .await
                .expect("acquire released writer");
            let _ = acquired_send.send(());
            sqlx::query("INSERT INTO sources(object_id,display_name,normalized_name,artwork_digest) VALUES ('batch-point','Batch point','batch point',zeroblob(32))")
                .execute(writer.as_mut().expect("writer available"))
                .await
                .expect("interleave after bounded batch");
        });
        started_receive.await.expect("point writer started");
        tokio::task::yield_now().await;
        assert!(
            matches!(
                acquired_receive.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ),
            "a point write waits only while the bounded page transaction is active"
        );
        scan.finish_batch()
            .await
            .expect("release bounded Scan batch");
        acquired_receive
            .await
            .expect("point writer acquired after batch");
        point_write.await.expect("join interleaved point write");
        {
            let mut writer = database.writer().await.expect("scan released writer");
            let writer = writer.as_mut().expect("writer available");
            let moods: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM moods WHERE source_key=(SELECT source_key FROM sources WHERE object_id='source')",
            )
            .fetch_one(&mut *writer)
            .await
            .expect("read committed acquisition rows");
            assert_eq!(moods, 128);
            let point_sources: i64 =
                sqlx::query_scalar("SELECT count(*) FROM sources WHERE object_id='batch-point'")
                    .fetch_one(&mut *writer)
                    .await
                    .expect("read the interleaved point write");
            assert_eq!(point_sources, 1);
        }
        assert!(matches!(
            scan.finish().await.expect("finish acquisition"),
            ScanOutcome::Changed(_)
        ));
    }
}

async fn source_exists(
    connection: &mut SqliteConnection,
    source: i64,
    source_id: &str,
) -> LibraryResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sources WHERE source_key=?1 AND object_id=?2)",
    )
    .bind(source)
    .bind(source_id)
    .fetch_one(connection)
    .await?)
}

async fn ensure_source_identity(
    connection: &mut sqlx::SqliteConnection,
    source_id: &str,
) -> LibraryResult<i64> {
    crate::db::write_source_identity(connection, source_id).await?;
    sqlx::query("INSERT INTO catalog.sources(object_id,display_name,normalized_name,artwork_digest) VALUES (?1,?1,lower(?1),zeroblob(32)) ON CONFLICT(object_id) DO NOTHING")
        .bind(source_id).execute(&mut *connection).await?;
    Ok(
        sqlx::query_scalar("SELECT source_key FROM catalog.sources WHERE object_id=?1")
            .bind(source_id)
            .fetch_one(connection)
            .await?,
    )
}

fn local_inventory_row(row: sqlx::sqlite::SqliteRow) -> LibraryResult<crate::LocalFileWrite> {
    Ok(crate::LocalFileWrite {
        path: row.try_get("path")?,
        root: row.try_get("root")?,
        relative_path: row.try_get("relative_path")?,
        kind: crate::LocalFileKind::parse(row.try_get("kind")?)?,
        size_bytes: row.try_get("size_bytes")?,
        mtime_ns: row.try_get("mtime_ns")?,
        device_id: row.try_get("device_id")?,
        inode: row.try_get("inode")?,
        native_id: row.try_get("native_id")?,
        revision: row.try_get("revision")?,
        picture_index: row.try_get("picture_index")?,
        artist_pictures: row.try_get("artist_pictures")?,
        parse_version: row.try_get("parse_version")?,
        state: crate::LocalFileState::parse(row.try_get("state")?)?,
    })
}
