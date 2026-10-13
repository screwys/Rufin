//! Writes acquired catalog facts directly and tracks completed collection scopes.
//! Provider acquisition and visible route construction remain outside this module.

use sqlx::query::Query;
use sqlx::sqlite::{SqliteArguments, SqliteConnection, SqliteQueryResult};
use sqlx::{Connection, FromRow, QueryBuilder, Sqlite, Transaction};
use sqlx_core::transaction::TransactionManager;

mod artwork;
mod entities;
mod files;
mod home;
mod input;
mod loudness;
mod playlists;
mod relations;
pub use input::{
    AlbumInput, AlbumRelationsInput, ArtistInput, FolderInput, GenreInput, LoudnessInput,
    MoodInput, PlaylistEntryInput, PlaylistInput, RecordInput, TrackInput, TrackRelationsInput,
};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{
    Database, HomeEntryInput, LibraryError, LibraryResult, ReadCancellation, SourceId, SourceKey,
    source_entity_uri,
};

const MAX_FRESHNESS_BYTES: usize = 64 * 1024;
const MAX_ROW_BYTES: usize = 8 * 1024 * 1024;

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

/// Stored catalog changes accepted by one source acquisition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogUpdate {
    pub source_id: SourceId,
    pub source: SourceKey,
    pub artwork_digest: [u8; 32],
    pub metadata: bool,
    pub playlists: bool,
    pub artwork: bool,
    pub home: bool,
    pub inventory: bool,
    pub tracks_added: bool,
    pub song_rekeys: std::sync::Arc<[(crate::TrackKey, crate::TrackKey)]>,
}

impl CatalogUpdate {
    pub fn changed(&self) -> bool {
        self.metadata || self.playlists || self.artwork || self.home
    }
}

#[derive(Clone, Debug, FromRow, Eq, PartialEq)]
pub struct CachedSource {
    pub source: SourceKey,
    pub object_id: String,
    pub display_name: String,
    pub artwork_digest: Vec<u8>,
    pub catalog_complete: bool,
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
    song_rekeys: Vec<(crate::TrackKey, crate::TrackKey)>,
    batch_changes: Option<Changes>,
    source_id: String,
    display_name: String,
    normalized_name: String,
    freshness: Option<Freshness>,
    accepted_at: i64,
    batch_writer: Option<tokio::sync::OwnedMutexGuard<Option<sqlx::SqliteConnection>>>,
    point_update: bool,
    local_point_update: bool,
    import_files: bool,
    authoritative: bool,
    distinct_track_covers: bool,
    failed: bool,
}

#[derive(Clone, Copy, Default)]
struct Changes {
    metadata: bool,
    playlists: bool,
    home: bool,
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
    pub fn rekey_songs(&mut self, pairs: Vec<(crate::TrackKey, crate::TrackKey)>) {
        self.song_rekeys.extend(pairs);
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
            "SELECT EXISTS(SELECT 1 FROM temp.scan_seen WHERE kind=?1 AND object_id=?2 AND written=1 UNION ALL SELECT 1 FROM {table} WHERE ?3 AND indexed AND source_key=?4 AND object_id=?2)"
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

    async fn observe_entities(
        &mut self,
        kind: &str,
        object_ids: &[&str],
    ) -> LibraryResult<std::collections::HashSet<String>> {
        if self.failed {
            return Err(LibraryError::ScanFailed);
        }
        if object_ids.is_empty() {
            return Ok(std::collections::HashSet::new());
        }
        let ids = serde_json::to_string(object_ids)?;
        let query = sqlx::query_scalar::<_, String>(
            "INSERT INTO temp.scan_seen(kind,object_id,written)
             SELECT ?1,value,1 FROM json_each(?2) WHERE true
             ON CONFLICT(kind,object_id) DO UPDATE SET written=1 WHERE written=0
             RETURNING object_id",
        )
        .bind(kind)
        .bind(&ids);
        let first = if let Some(writer) = &mut self.batch_writer {
            query
                .fetch_all(writer.as_mut().ok_or(LibraryError::WriterUnavailable)?)
                .await
        } else {
            let mut writer = self.database.writer().await?;
            query
                .fetch_all(writer.as_mut().ok_or(LibraryError::WriterUnavailable)?)
                .await
        };
        if first.is_err() {
            self.failed = true;
        }
        let first = first?;
        let table = match kind {
            "album" => "albums",
            "artist" => "artists",
            "genre" => "genres",
            "mood" => "moods",
            "folder" => "folders",
            "playlist" => "playlists",
            _ => unreachable!(),
        };
        let owner = if kind == "playlist" {
            " AND owner='source'"
        } else {
            ""
        };
        let changed = self
            .execute_result(
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "UPDATE {table} SET indexed=1 WHERE source_key=?1 AND indexed=0{owner}
             AND object_id IN (SELECT value FROM json_each(?2))"
                )))
                .bind(self.source)
                .bind(ids),
            )
            .await?
            .rows_affected()
            > 0;
        if kind == "playlist" {
            self.changes.playlists |= changed;
        } else {
            self.changes.metadata |= changed;
        }
        Ok(first.into_iter().collect())
    }

    async fn observe_tracks(
        &mut self,
        object_ids: &[&str],
    ) -> LibraryResult<std::collections::HashMap<String, bool>> {
        let ids = serde_json::to_string(object_ids)?;
        self.execute(
            sqlx::query(
                "INSERT OR IGNORE INTO temp.scan_affected_albums
             SELECT album_key FROM tracks WHERE source_key=?1
               AND object_id IN (SELECT value FROM json_each(?2)) AND album_key IS NOT NULL",
            )
            .bind(self.source)
            .bind(&ids),
        )
        .await?;
        let first = sqlx::query_as::<_, (String, bool)>(
            "INSERT INTO temp.scan_seen(kind,object_id,written)
             SELECT 'track',value,1 FROM json_each(?2) WHERE true
             ON CONFLICT(kind,object_id) DO UPDATE SET written=1 WHERE written=0
             RETURNING object_id,EXISTS(SELECT 1 FROM tracks
               WHERE source_key=?1 AND tracks.object_id=scan_seen.object_id AND indexed)",
        )
        .bind(self.source)
        .bind(&ids)
        .fetch_all(
            self.batch_writer
                .as_mut()
                .unwrap()
                .as_mut()
                .ok_or(LibraryError::WriterUnavailable)?,
        )
        .await?;
        if !self.point_update && !first.is_empty() {
            let first_ids = serde_json::to_string(
                &first.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            )?;
            self.execute(
                sqlx::query(
                    "INSERT OR IGNORE INTO temp.scan_removals
                 SELECT 'track-folders',value FROM json_each(?1)",
                )
                .bind(first_ids),
            )
            .await?;
        }
        let changed = self
            .execute_result(
                sqlx::query(
                    "UPDATE tracks SET indexed=1 WHERE source_key=?1 AND indexed=0
               AND object_id IN (SELECT value FROM json_each(?2))",
                )
                .bind(self.source)
                .bind(ids),
            )
            .await?
            .rows_affected()
            > 0;
        self.changes.metadata |= changed;
        self.changes.tracks_added |= changed;
        Ok(first.into_iter().collect())
    }

    /// Checks whether the saved catalog matches the cheap freshness fact.
    pub async fn accept_freshness(
        database: &Database,
        source_id: &str,
        freshness: &Freshness,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<bool> {
        validate_id("source", source_id)?;
        let (_permit, mut connection) = database.acquire_general(cancellation).await?;
        Ok(sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM sources
             WHERE object_id=?1 AND freshness=?2 AND distinct_track_covers=?3
               AND catalog_complete)",
        )
        .bind(source_id)
        .bind(freshness.as_bytes())
        .bind(database.distinct_track_covers())
        .fetch_one(&mut *connection)
        .await?)
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
        let source = crate::db::write_source_identity(connection, &source_id).await?;
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
            song_rekeys: Vec::new(),
            batch_changes: None,
            batch_writer: None,
            point_update: false,
            local_point_update: false,
            import_files: false,
            authoritative: true,
            distinct_track_covers: database.distinct_track_covers(),
            failed: false,
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

    pub fn interrupted(&mut self) {
        self.failed = true;
        self.incomplete();
    }

    pub fn incomplete(&mut self) {
        self.authoritative = false;
        self.clear_freshness();
    }

    pub fn clear_freshness(&mut self) {
        self.freshness = None;
    }

    pub async fn retain_genres(&mut self) -> LibraryResult<()> {
        self.execute(sqlx::query("INSERT OR IGNORE INTO temp.scan_seen(kind,object_id,written) SELECT 'genre',object_id,0 FROM genres WHERE source_key=?1 AND indexed").bind(self.source)).await
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
             WHERE track.indexed AND scope.entity_kind='album-members'
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
        if self.failed {
            self.incomplete();
        }
        if let Some(mut writer) = self.batch_writer.take() {
            let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
            if self.failed {
                sqlx::sqlite::SqliteTransactionManager::rollback(connection).await?;
                self.changes = self.batch_changes.take().unwrap();
            } else {
                sqlx::sqlite::SqliteTransactionManager::commit(connection).await?;
                self.batch_changes = None;
            }
        }
        self.failed = false;
        Ok(())
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
                     WHERE track.indexed AND (track.cue_path IS NOT NULL OR track.media_uri LIKE 'file://%')
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

    pub async fn retain_local_cue_paths(&mut self, paths: &[&str]) -> LibraryResult<()> {
        for page in paths.chunks(128) {
            let paths = serde_json::to_string(page)?;
            self.retain_local_tracks_where(
                "track.cue_path IN (SELECT value FROM json_each(?2))",
                Some(&paths),
            )
            .await?;
        }
        Ok(())
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

    async fn retain_local_tracks_where(
        &mut self,
        predicate: &str,
        parameter: Option<&str>,
    ) -> LibraryResult<()> {
        let tracks = format!(
            "SELECT track.track_key FROM tracks track WHERE track.indexed AND track.source_key=?1 AND {predicate}"
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
        for (relation, target_key) in [
            ("album_artists", "artist_key"),
            ("album_genres", "genre_key"),
            ("album_release_types", "release_type"),
        ] {
            self.execute(sqlx::query(sqlx::AssertSqlSafe(format!("INSERT OR IGNORE INTO temp.scan_relation_seen(kind,owner_key,target_key) SELECT '{relation}',album_key,CAST({target_key} AS TEXT) FROM {relation} WHERE album_key IN (SELECT album_key FROM tracks WHERE track_key IN ({tracks}))"))).bind(self.source).bind(parameter)).await?;
        }
        self.execute(sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO temp.scan_seen(kind,object_id,written,supplied) SELECT 'album',album.object_id,0,{} FROM albums album WHERE album.album_key IN (SELECT album_key FROM tracks WHERE track_key IN ({tracks})) ON CONFLICT(kind,object_id) DO UPDATE SET supplied=supplied|excluded.supplied",AlbumInput::SUPPLIED))).bind(self.source).bind(parameter)).await
    }

    pub async fn replace_favorites(
        &mut self,
        tracks: &[String],
        albums: &[String],
        artists: &[String],
    ) -> LibraryResult<()> {
        for (kind, ids) in [("track", tracks), ("album", albums), ("artist", artists)] {
            let ids = serde_json::to_string(ids)?;
            let mut query = crate::favorites::source_user_data_query(kind, |query| {
                query
                    .push("SELECT ")
                    .push_bind(self.source)
                    .push(",object_id,object_id IN (SELECT value FROM json_each(")
                    .push_bind(ids)
                    .push(")),0,NULL,NULL,NULL,NULL FROM ")
                    .push(format!("{kind}s WHERE source_key="))
                    .push_bind(self.source);
            });
            self.changes.user_data |= self.execute_result(query.build()).await?.rows_affected() > 0;
        }
        Ok(())
    }

    pub async fn remove_unstaged_playlists(&mut self) -> LibraryResult<()> {
        self.execute(
            sqlx::query(
                "INSERT OR IGNORE INTO temp.scan_removals(entity_kind,object_id)
             SELECT 'playlist',object_id FROM playlists WHERE source_key=?1 AND owner='source' AND indexed
             AND object_id NOT IN (SELECT object_id FROM temp.scan_seen WHERE kind='playlist')",
            )
            .bind(self.source),
        )
        .await
    }

    /// Updates admitted source entities in bounded batches. Unknown IDs need metadata first.
    pub async fn write_source_user_data(
        &mut self,
        facts: &[(&'static str, String, crate::SourceUserData)],
    ) -> LibraryResult<()> {
        for kind in ["track", "album", "artist"] {
            let mut facts = facts
                .iter()
                .filter(|(entry_kind, _, _)| *entry_kind == kind);
            loop {
                let rows = facts
                    .by_ref()
                    .take(128)
                    .map(|(_, id, fact)| {
                        (
                            id,
                            fact.favorite,
                            fact.rating.is_some(),
                            fact.rating.flatten(),
                            fact.play_count,
                            fact.last_played,
                        )
                    })
                    .collect::<Vec<_>>();
                if rows.is_empty() {
                    break;
                }
                let rows = serde_json::to_string(&rows)?;
                let mut query = crate::favorites::source_user_data_query(kind, |query| {
                    query.push("SELECT ").push_bind(self.source)
                        .push(",owner.object_id,json_extract(fact.value,'$[1]'),json_extract(fact.value,'$[2]'),json_extract(fact.value,'$[3]'),json_extract(fact.value,'$[4]'),NULL,json_extract(fact.value,'$[5]') FROM json_each(")
                        .push_bind(rows).push(format!(") fact CROSS JOIN {kind}s owner ON owner.source_key="))
                        .push_bind(self.source).push(" AND owner.indexed AND owner.object_id=json_extract(fact.value,'$[0]')");
                });
                self.changes.user_data |=
                    self.execute_result(query.build()).await?.rows_affected() > 0;
            }
        }
        Ok(())
    }

    pub async fn finish(mut self) -> LibraryResult<()> {
        self.finish_batch().await?;
        let mut writer = self.database.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let update = self.finalize(connection).await?;
        drop(writer);
        let database = self.database.clone();
        drop(self);
        if let Some(update) = update {
            database.catalog_updated(update).await;
        }
        Ok(())
    }

    async fn finalize(
        &self,
        connection: &mut SqliteConnection,
    ) -> LibraryResult<Option<CatalogUpdate>> {
        let full = self.authoritative && !self.point_update;
        let source = self.source;
        let mut transaction = connection.begin().await?;
        let current = sqlx::query_as::<_, AcceptedSource>("SELECT artwork_digest,catalog_revision,display_name,normalized_name,freshness,distinct_track_covers FROM sources WHERE source_key=?1 AND object_id=?2")
            .bind(source).bind(&self.source_id).fetch_optional(&mut *transaction).await?;
        let Some(current) = current else {
            return Ok(None);
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
        changed.metadata |=
            relations::prune_relations(&mut transaction, source, self.authoritative).await?;
        changed.home |= home::prune_sections(&mut transaction, source, full).await?;
        let home_changed = changed.home;
        if self.local_point_update {
            changed.metadata |= prune_local_orphans(&mut transaction, source).await?;
        }
        changed.metadata |=
            loudness::finish_measurements(&mut transaction, source, self.authoritative).await?;
        changed.artwork |= artwork::update_artwork(
            &mut transaction,
            source,
            self.distinct_track_covers,
            self.point_update,
            self.local_point_update,
            self.authoritative,
        )
        .await?;
        if !self.import_files {
            files::prune_local_files(
                &mut transaction,
                source,
                full,
                self.local_point_update && self.authoritative,
            )
            .await?;
        }
        let catalog_changed = changed.metadata;
        changed.metadata |= changed.user_data;
        let any = changed.metadata || changed.playlists || changed.artwork || home_changed;
        if current.catalog_revision == 0 && !any && !full {
            return Ok(None);
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
        let revision = current.catalog_revision
            + i64::from(changed.metadata || changed.playlists || home_changed);
        let artwork = if full
            || changed.metadata
            || changed.playlists
            || changed.artwork
            || home_changed
            || freshness != current.freshness.as_deref()
            || self.distinct_track_covers != current.distinct_track_covers
        {
            sqlx::query_scalar::<_,Vec<u8>>("UPDATE sources SET display_name=?2,normalized_name=?3,freshness=?4,distinct_track_covers=?5,catalog_revision=?6,artwork_digest=CASE WHEN ?7 THEN randomblob(32) ELSE artwork_digest END,catalog_complete=CASE WHEN ?8 THEN 1 ELSE catalog_complete END WHERE source_key=?1 RETURNING artwork_digest")
                .bind(source).bind(&self.display_name).bind(&self.normalized_name).bind(freshness).bind(self.distinct_track_covers).bind(revision).bind(changed.artwork).bind(full).fetch_one(&mut *transaction).await?
        } else {
            current.artwork_digest
        };
        crate::collections::refresh_facts_on(&mut transaction).await?;
        clear_working_set(&mut transaction).await?;
        if full && catalog_changed {
            sqlx::raw_sql("PRAGMA optimize")
                .execute(&mut *transaction)
                .await?;
        }
        transaction.commit().await?;
        Ok(Some(CatalogUpdate {
            source_id: SourceId::new(self.source_id.clone()),
            source: SourceKey::from_raw(source),
            artwork_digest: artwork.as_slice().try_into().map_err(|_| {
                LibraryError::InvalidStore("source artwork digest is not 32 bytes".to_string())
            })?,
            metadata: changed.metadata,
            playlists: changed.playlists,
            artwork: changed.artwork,
            home: home_changed,
            inventory: !self.point_update || self.local_point_update,
            tracks_added: changed.tracks_added,
            song_rekeys: self.song_rekeys.clone().into(),
        }))
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
    pub async fn reconcile_source(
        &self,
        source_id: &SourceId,
        kind: &str,
        name: &str,
    ) -> LibraryResult<CachedSource> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let source = SourceKey::from_raw(
            crate::db::write_source_identity(&mut transaction, source_id.as_str()).await?,
        );
        sqlx::query("UPDATE sources SET kind=?2,display_name=?3,normalized_name=?4 WHERE source_key=?1 AND (kind,display_name,normalized_name) IS NOT (?2,?3,?4)")
            .bind(source).bind(kind).bind(name).bind(name.to_lowercase()).execute(&mut *transaction).await?;
        let cached = sqlx::query_as::<_, CachedSource>(
            "SELECT source_key source,object_id,display_name,artwork_digest,catalog_complete FROM sources
             WHERE source_key=?1",
        )
        .bind(source)
        .fetch_one(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(cached)
    }

    pub async fn cached_source(
        &self,
        object_id: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<CachedSource>> {
        validate_id("source", object_id)?;
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_as::<_, CachedSource>(
            "SELECT source_key source,object_id,display_name,artwork_digest,catalog_complete
             FROM sources WHERE object_id=?1",
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

fn validate_id(kind: &'static str, value: &str) -> LibraryResult<()> {
    if value.is_empty() {
        return Err(LibraryError::InvalidScan(format!(
            "{kind} identity is empty"
        )));
    }
    Ok(())
}

async fn create_working_set(connection: &mut SqliteConnection) -> LibraryResult<()> {
    sqlx::raw_sql(r#"
    CREATE TEMP TABLE IF NOT EXISTS scan_seen(kind TEXT NOT NULL,object_id TEXT NOT NULL,written INTEGER NOT NULL DEFAULT 0,supplied INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(kind,object_id)) STRICT, WITHOUT ROWID;
    CREATE TEMP TABLE IF NOT EXISTS scan_unresolved_artists(object_id TEXT PRIMARY KEY) STRICT, WITHOUT ROWID;
    CREATE TEMP TABLE IF NOT EXISTS scan_track_albums(track_id TEXT PRIMARY KEY,album_id TEXT NOT NULL) STRICT, WITHOUT ROWID;
    CREATE TEMP TABLE IF NOT EXISTS scan_affected_albums(album_key INTEGER PRIMARY KEY) STRICT;
    CREATE TEMP TABLE IF NOT EXISTS playlist_read(object_id TEXT PRIMARY KEY,track_key INTEGER NOT NULL,position INTEGER NOT NULL) STRICT, WITHOUT ROWID;
    CREATE TEMP TABLE IF NOT EXISTS scan_artwork(kind TEXT NOT NULL,object_id TEXT NOT NULL,source BLOB,binding BLOB,PRIMARY KEY(kind,object_id)) STRICT, WITHOUT ROWID;
    CREATE TEMP TABLE IF NOT EXISTS scan_local_dependency_paths(path TEXT PRIMARY KEY) STRICT;
    CREATE TEMP TABLE IF NOT EXISTS scan_local_seen(path TEXT PRIMARY KEY,root TEXT NOT NULL,kind TEXT NOT NULL,changed INTEGER NOT NULL,pending_directory INTEGER NOT NULL DEFAULT 0,previous_revision TEXT,previous_parse_version INTEGER,previous_accepted INTEGER NOT NULL DEFAULT 0) STRICT, WITHOUT ROWID;
    CREATE INDEX IF NOT EXISTS scan_local_seen_kind_path ON scan_local_seen(kind,path);
    CREATE TEMP TABLE IF NOT EXISTS scan_relation_seen(kind TEXT NOT NULL,owner_key INTEGER NOT NULL,target_key TEXT NOT NULL,PRIMARY KEY(kind,owner_key,target_key)) STRICT, WITHOUT ROWID;
    CREATE TEMP TABLE IF NOT EXISTS scan_local_file_removals(path TEXT PRIMARY KEY) STRICT;
    CREATE TEMP TABLE IF NOT EXISTS scan_retained_paths(path TEXT PRIMARY KEY) STRICT;
    CREATE TEMP TABLE IF NOT EXISTS scan_local_component_paths(path TEXT PRIMARY KEY) STRICT;
    CREATE TEMP TABLE IF NOT EXISTS scan_removals(entity_kind TEXT NOT NULL,object_id TEXT NOT NULL,PRIMARY KEY(entity_kind,object_id)) STRICT;
    "#).execute(&mut *connection).await?;
    clear_working_set(connection).await
}

async fn clear_working_set(connection: &mut SqliteConnection) -> LibraryResult<()> {
    sqlx::raw_sql("DELETE FROM temp.scan_seen; DELETE FROM temp.scan_unresolved_artists; DELETE FROM temp.scan_track_albums; DELETE FROM temp.scan_affected_albums; DELETE FROM temp.scan_artwork;
        DELETE FROM temp.scan_local_dependency_paths; DELETE FROM temp.scan_local_seen; DELETE FROM temp.scan_local_file_removals; DELETE FROM temp.scan_retained_paths; DELETE FROM temp.scan_local_component_paths; DELETE FROM temp.scan_removals;
        DELETE FROM temp.playlist_read;
        DELETE FROM temp.scan_relation_seen;")
        .execute(connection).await?;
    Ok(())
}

async fn prune_local_orphans(
    transaction: &mut Transaction<'_, Sqlite>,
    source_key: i64,
) -> LibraryResult<bool> {
    let mut changed = false;
    for sql in [
        "UPDATE albums SET indexed=0 WHERE source_key=?1 AND indexed AND NOT EXISTS (SELECT 1 FROM tracks WHERE tracks.indexed AND tracks.album_key=albums.album_key)",
        "UPDATE artists SET indexed=0 WHERE source_key=?1 AND indexed AND NOT EXISTS (SELECT 1 FROM track_artists JOIN tracks USING(track_key) WHERE tracks.indexed AND track_artists.artist_key=artists.artist_key) AND NOT EXISTS (SELECT 1 FROM album_artists JOIN albums USING(album_key) WHERE albums.indexed AND album_artists.artist_key=artists.artist_key)",
        "UPDATE genres SET indexed=0 WHERE source_key=?1 AND indexed AND NOT EXISTS (SELECT 1 FROM track_genres JOIN tracks USING(track_key) WHERE tracks.indexed AND track_genres.genre_key=genres.genre_key) AND NOT EXISTS (SELECT 1 FROM album_genres JOIN albums USING(album_key) WHERE albums.indexed AND album_genres.genre_key=genres.genre_key)",
        "UPDATE moods SET indexed=0 WHERE source_key=?1 AND indexed AND NOT EXISTS (SELECT 1 FROM track_moods JOIN tracks USING(track_key) WHERE tracks.indexed AND track_moods.mood_key=moods.mood_key)",
        "UPDATE folders SET indexed=0 WHERE source_key=?1 AND indexed AND NOT EXISTS (SELECT 1 FROM track_folders JOIN tracks USING(track_key) WHERE tracks.indexed AND track_folders.folder_key=folders.folder_key)",
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
        ("playlist", "playlists"),
    ] {
        let sql = format!(
            "UPDATE {table} SET indexed=0 WHERE source_key=?1 AND indexed
             {} {} AND object_id NOT IN (SELECT object_id FROM temp.scan_seen WHERE kind='{kind}')",
            if kind == "playlist" {
                "AND owner='source'"
            } else {
                ""
            },
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
        scan.write_genres(&[&GenreInput {
            object_id: "genre".into(),
            name: "Genre".into(),
            sort_text: Some("genre".into()),
            ..Default::default()
        }])
        .await
        .expect("write before writer failure");
        database.fail_writer().await.expect("fail fixed writer");
        assert!(matches!(
            scan.write_moods(&[&crate::MoodInput {
                object_id: "mood".into(),
                name: "Mood".into(),
                sort_text: "mood".into(),
            }])
            .await,
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
            scan.write_moods(&[&crate::MoodInput {
                object_id: format!("mood-{number}"),
                name: "Mood".into(),
                sort_text: format!("mood-{number}"),
            }])
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
        let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
        database.observe_catalog(move |update| {
            let _ = updates_tx.send(update);
            std::future::ready(())
        });
        scan.finish().await.expect("finish acquisition");
        let update = updates.try_recv().unwrap();
        assert!(update.metadata);
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
