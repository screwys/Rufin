//! Streams bounded source catalog facts into file-backed staging and publishes them atomically.
//! Provider acquisition and visible route construction remain outside this module.

use blake3::Hasher;
use sqlx::query::Query;
use sqlx::sqlite::{SqliteArguments, SqliteRow};
use sqlx::{Connection, FromRow, QueryBuilder, Row, Sqlite, Transaction, TypeInfo, ValueRef};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{Database, HomeEntryInput, LibraryError, LibraryResult, ReadCancellation, SourceKey};

const MAX_FRESHNESS_BYTES: usize = 64 * 1024;
const MAX_STAGED_ROW_BYTES: usize = 8 * 1024 * 1024;
const DIGEST_PAGE_ROWS: i64 = 256;

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
}

#[derive(Clone, Debug, FromRow, Eq, PartialEq)]
pub struct CachedSource {
    pub source: SourceKey,
    pub object_id: String,
    pub display_name: String,
    pub catalog_revision: i64,
    pub artwork_digest: Vec<u8>,
}

/// The atomic result of finishing a source scan.
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

/// One bounded, connection-owned source scan.
pub struct Scan {
    database: Database,
    token: u64,
    source_id: String,
    display_name: String,
    normalized_name: String,
    freshness: Option<Freshness>,
    expected_revision: Option<i64>,
    existing_source_key: Option<i64>,
    accepted_at: i64,
    batch_writer: Option<tokio::sync::OwnedMutexGuard<Option<sqlx::SqliteConnection>>>,
    point_update: bool,
    local_point_update: bool,
    failed: bool,
}

#[derive(FromRow)]
struct AcceptedSource {
    source_key: i64,
    catalog_digest: Vec<u8>,
    artwork_digest: Vec<u8>,
    catalog_revision: i64,
}

#[derive(FromRow)]
struct StagedAlbumAudioKey {
    album_object_id: String,
    disc_number: i64,
    track_number: i64,
    sort_text: String,
    object_id: String,
    source_key: Vec<u8>,
    current_key: Vec<u8>,
}

impl Scan {
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
            "SELECT source_key, catalog_revision, artwork_digest
             FROM sources
             WHERE object_id=?1 AND freshness=?2",
        )
        .bind(source_id)
        .bind(freshness.as_bytes())
        .fetch_optional(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        result?
            .map(|(source_key, revision, artwork)| publication(source_key, revision, &artwork))
            .transpose()
    }

    /// Starts staging a source without holding a publication transaction.
    pub async fn begin(
        database: &Database,
        source_id: impl Into<String>,
        display_name: impl Into<String>,
        normalized_name: impl Into<String>,
        freshness: Option<Freshness>,
    ) -> LibraryResult<Self> {
        let source_id = source_id.into();
        let display_name = display_name.into();
        let normalized_name = normalized_name.into();
        validate_id("source", &source_id)?;
        validate_row_bytes(&[
            source_id.as_bytes(),
            display_name.as_bytes(),
            normalized_name.as_bytes(),
        ])?;
        let token = database.begin_scan()?;
        let construction = async {
            let mut writer = database.writer().await?;
            let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
            create_staging(connection).await?;
            sqlx::query_as::<_, (i64, i64)>(
                "SELECT source_key,catalog_revision FROM sources WHERE object_id=?1",
            )
            .bind(&source_id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(LibraryError::from)
        }
        .await;
        let current = match construction {
            Ok(current) => current,
            Err(error) => {
                database.release_scan(token);
                return Err(error);
            }
        };
        Ok(Self {
            database: database.clone(),
            token,
            source_id,
            display_name,
            normalized_name,
            freshness,
            expected_revision: current.map(|(_, revision)| revision),
            existing_source_key: current.map(|(source_key, _)| source_key),
            accepted_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|duration| i64::try_from(duration.as_secs()).ok())
                .unwrap_or_default(),
            batch_writer: None,
            point_update: false,
            local_point_update: false,
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
        scan.local_point_update = source_id == "local:server:library";
        Ok(scan)
    }

    pub async fn remove_track(&mut self, object_id: &str) -> LibraryResult<()> {
        self.write_removal("track", object_id).await
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
        self.stage(
            sqlx::query("INSERT OR IGNORE INTO temp.scan_removals VALUES (?1,?2)")
                .bind(kind)
                .bind(object_id),
        )
        .await
    }

    pub const fn accepted_at(&self) -> i64 {
        self.accepted_at
    }

    pub fn existing_source(&self) -> Option<SourceKey> {
        self.existing_source_key.map(SourceKey::from_raw)
    }

    /// Holds the writer for one bounded provider page and one SQLite transaction.
    pub async fn begin_batch(&mut self) -> LibraryResult<()> {
        if self.batch_writer.is_some() {
            return Err(LibraryError::InvalidScan(
                "a Scan batch is already active".to_string(),
            ));
        }
        let mut writer = self.database.writer_owned().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *connection)
            .await?;
        self.batch_writer = Some(writer);
        Ok(())
    }

    pub async fn finish_batch(&mut self) -> LibraryResult<()> {
        let Some(mut writer) = self.batch_writer.take() else {
            return Err(LibraryError::InvalidScan(
                "no Scan batch is active".to_string(),
            ));
        };
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let sql = if self.failed { "ROLLBACK" } else { "COMMIT" };
        sqlx::query(sql).execute(&mut *connection).await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn write_album(
        &mut self,
        object_id: &str,
        title: &str,
        normalized_title: &str,
        display_artist: &str,
        sort_text: &str,
        year: Option<i64>,
        release_date: Option<&str>,
        date_added: Option<&str>,
        musicbrainz_release_id: Option<&str>,
        musicbrainz_release_group_id: Option<&str>,
        is_compilation: Option<bool>,
        artwork_binding: Option<&[u8]>,
        favorite: bool,
        rating: Option<i64>,
        first_seen_at: Option<i64>,
    ) -> LibraryResult<()> {
        self.require_id("album", object_id)?;
        let rating = self.public_rating(rating)?;
        self.require_row_bytes(&[
            object_id.as_bytes(),
            title.as_bytes(),
            normalized_title.as_bytes(),
            display_artist.as_bytes(),
            sort_text.as_bytes(),
            release_date.unwrap_or_default().as_bytes(),
            date_added.unwrap_or_default().as_bytes(),
            musicbrainz_release_id.unwrap_or_default().as_bytes(),
            musicbrainz_release_group_id.unwrap_or_default().as_bytes(),
            artwork_binding.unwrap_or_default(),
        ])?;
        self.stage(
            sqlx::query(
                "INSERT INTO temp.scan_albums(
                    object_id, title, normalized_title, display_artist, sort_text,
                    year, release_date, date_added, musicbrainz_release_id,
                    musicbrainz_release_group_id, is_compilation, artwork_binding,
                    favorite, rating, first_seen_at
                 ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                ?11, ?12, ?13, ?14, ?15
             ) ON CONFLICT(object_id) DO UPDATE SET
                year=COALESCE(scan_albums.year,excluded.year),
                release_date=COALESCE(scan_albums.release_date,excluded.release_date),
                date_added=COALESCE(scan_albums.date_added,excluded.date_added),
                musicbrainz_release_id=COALESCE(scan_albums.musicbrainz_release_id,excluded.musicbrainz_release_id),
                musicbrainz_release_group_id=COALESCE(scan_albums.musicbrainz_release_group_id,excluded.musicbrainz_release_group_id),
                is_compilation=CASE
                    WHEN scan_albums.is_compilation=1 OR excluded.is_compilation=1 THEN 1
                    WHEN scan_albums.is_compilation=0 OR excluded.is_compilation=0 THEN 0
                    ELSE NULL END,
                artwork_binding=COALESCE(scan_albums.artwork_binding,excluded.artwork_binding),
                favorite=max(scan_albums.favorite,excluded.favorite),
                rating=COALESCE(scan_albums.rating,excluded.rating),
                first_seen_at=COALESCE(scan_albums.first_seen_at,excluded.first_seen_at)",
            )
            .bind(object_id)
            .bind(title)
            .bind(normalized_title)
            .bind(display_artist)
            .bind(sort_text)
            .bind(year)
            .bind(release_date)
            .bind(date_added)
            .bind(musicbrainz_release_id)
            .bind(musicbrainz_release_group_id)
            .bind(is_compilation)
            .bind(artwork_binding)
            .bind(favorite)
            .bind(rating)
            .bind(first_seen_at),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn write_track(
        &mut self,
        object_id: &str,
        album_object_id: Option<&str>,
        title: &str,
        normalized_search: &str,
        display_album: &str,
        display_artist: &str,
        sort_text: &str,
        duration_millis: i64,
        disc_number: i64,
        track_number: i64,
        year: Option<i64>,
        release_date: Option<&str>,
        date_added: Option<&str>,
        media_uri: Option<&str>,
        source_format: Option<&str>,
        comment: Option<&str>,
        bpm: Option<i64>,
        musicbrainz_recording_id: Option<&str>,
        musicbrainz_release_track_id: Option<&str>,
        cue_path: Option<&str>,
        cue_start_millis: Option<i64>,
        cue_end_millis: Option<i64>,
        artwork_binding: Option<&[u8]>,
        favorite: bool,
        rating: Option<i64>,
        first_seen_at: Option<i64>,
        baseline_play_count: Option<i64>,
        baseline_skip_count: Option<i64>,
        baseline_last_played: Option<i64>,
        source_path: Option<&str>,
        loudness_analysis_key: [u8; 32],
    ) -> LibraryResult<()> {
        self.require_id("track", object_id)?;
        let rating = self.public_rating(rating)?;
        if let Some(album_id) = album_object_id {
            self.require_id("album", album_id)?;
        }
        if baseline_play_count.is_some_and(|value| value < 0)
            || baseline_skip_count.is_some_and(|value| value < 0)
            || baseline_last_played.is_some_and(|value| value < 0)
        {
            self.failed = true;
            return Err(LibraryError::InvalidScan(
                "provider Activity baseline cannot be negative".to_string(),
            ));
        }
        self.require_row_bytes(&[
            object_id.as_bytes(),
            album_object_id.unwrap_or_default().as_bytes(),
            title.as_bytes(),
            normalized_search.as_bytes(),
            display_album.as_bytes(),
            display_artist.as_bytes(),
            sort_text.as_bytes(),
            release_date.unwrap_or_default().as_bytes(),
            date_added.unwrap_or_default().as_bytes(),
            media_uri.unwrap_or_default().as_bytes(),
            source_path.unwrap_or_default().as_bytes(),
            source_format.unwrap_or_default().as_bytes(),
            comment.unwrap_or_default().as_bytes(),
            musicbrainz_recording_id.unwrap_or_default().as_bytes(),
            musicbrainz_release_track_id.unwrap_or_default().as_bytes(),
            cue_path.unwrap_or_default().as_bytes(),
            artwork_binding.unwrap_or_default(),
            loudness_analysis_key.as_slice(),
        ])?;
        self.stage(
            sqlx::query(
                "INSERT INTO temp.scan_tracks(
                    object_id, album_object_id, title, normalized_search,
                    display_album, display_artist, sort_text, duration_millis,
                    disc_number, track_number, year, release_date, date_added,
                    media_uri, source_path, source_format, comment, bpm,
                    musicbrainz_recording_id, musicbrainz_release_track_id,
                    cue_path, cue_start_millis, cue_end_millis, artwork_binding,
                    favorite, rating, first_seen_at, baseline_play_count,
                    baseline_skip_count, baseline_last_played,
                    source_loudness_analysis_key
                 ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
                ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27,
                ?28, ?29, ?30, ?31
             ) ON CONFLICT(object_id) DO NOTHING",
            )
            .bind(object_id)
            .bind(album_object_id)
            .bind(title)
            .bind(normalized_search)
            .bind(display_album)
            .bind(display_artist)
            .bind(sort_text)
            .bind(duration_millis)
            .bind(disc_number)
            .bind(track_number)
            .bind(year)
            .bind(release_date)
            .bind(date_added)
            .bind(media_uri)
            .bind(source_path)
            .bind(source_format)
            .bind(comment)
            .bind(bpm)
            .bind(musicbrainz_recording_id)
            .bind(musicbrainz_release_track_id)
            .bind(cue_path)
            .bind(cue_start_millis)
            .bind(cue_end_millis)
            .bind(artwork_binding)
            .bind(favorite)
            .bind(rating)
            .bind(first_seen_at)
            .bind(baseline_play_count)
            .bind(baseline_skip_count)
            .bind(baseline_last_played)
            .bind(loudness_analysis_key.as_slice()),
        )
        .await
    }

    pub async fn write_track_source_loudness(
        &mut self,
        track_object_id: &str,
        integrated_lufs: Option<f64>,
        true_peak: Option<f64>,
        replay_gain_db: Option<f64>,
        replay_gain_peak: Option<f64>,
    ) -> LibraryResult<()> {
        self.write_source_loudness(
            "UPDATE temp.scan_tracks
             SET source_integrated_lufs=?2, source_true_peak=?3,
                 source_replay_gain_db=?4, source_replay_gain_peak=?5
             WHERE object_id=?1",
            track_object_id,
            integrated_lufs,
            true_peak,
            replay_gain_db,
            replay_gain_peak,
        )
        .await
    }

    pub async fn write_album_source_loudness(
        &mut self,
        album_object_id: &str,
        integrated_lufs: Option<f64>,
        true_peak: Option<f64>,
        replay_gain_db: Option<f64>,
        replay_gain_peak: Option<f64>,
    ) -> LibraryResult<()> {
        self.write_source_loudness(
            "UPDATE temp.scan_albums
             SET source_integrated_lufs=?2, source_true_peak=?3,
                 source_replay_gain_db=?4, source_replay_gain_peak=?5
             WHERE object_id=?1",
            album_object_id,
            integrated_lufs,
            true_peak,
            replay_gain_db,
            replay_gain_peak,
        )
        .await
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
        self.stage(query.build()).await
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
            ])?;
        }
        let mut observations = QueryBuilder::<Sqlite>::new(
            "INSERT INTO temp.scan_local_files(path,root,relative_path,kind,size_bytes,mtime_ns,device_id,inode,parse_version,state) ",
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
                .push_bind(file.parse_version)
                .push_bind(file.state.as_str());
        });
        observations.push(" ON CONFLICT(path) DO UPDATE SET root=excluded.root,relative_path=excluded.relative_path,kind=excluded.kind,size_bytes=excluded.size_bytes,mtime_ns=excluded.mtime_ns,device_id=excluded.device_id,inode=excluded.inode,parse_version=excluded.parse_version,state=excluded.state");
        self.stage(observations.build()).await?;

        let mut clear = QueryBuilder::<Sqlite>::new(
            "DELETE FROM temp.scan_local_file_dependencies WHERE path IN (",
        );
        let mut separated = clear.separated(",");
        for (file, _) in files {
            separated.push_bind(&file.path);
        }
        separated.push_unseparated(")");
        self.stage(clear.build()).await?;

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
            self.stage(dependencies.build()).await?;
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
        self.stage(query.build()).await
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

    pub async fn local_inventory_path_page(
        &mut self,
        kind: crate::LocalFileKind,
        after: Option<&str>,
        exclude_cue_dependencies: bool,
        limit: usize,
    ) -> LibraryResult<Vec<String>> {
        let query = || {
            sqlx::query_scalar::<_, String>(
                "SELECT file.path FROM temp.scan_local_files file
             WHERE file.kind=?1 AND file.path>?2
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
        };
        if let Some(writer) = self.batch_writer.as_mut() {
            let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
            return Ok(query().fetch_all(&mut *connection).await?);
        }
        let mut writer = self.database.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(query().fetch_all(&mut *connection).await?)
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
        self.stage(query.build()).await
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
        query.push("), eligible AS (SELECT prefix FROM requested WHERE directory OR 1=(SELECT count(DISTINCT track.album_key) FROM tracks track WHERE track.source_key=")
            .push_bind(source)
            .push(" AND track.album_key IS NOT NULL AND track.media_uri>=('file://'||requested.prefix) AND track.media_uri<('file://'||requested.prefix||char(1114111)))) INSERT OR IGNORE INTO temp.scan_local_component_paths(path) SELECT file.path FROM eligible JOIN local_files file ON file.source_key=")
            .push_bind(source)
            .push(" AND file.path LIKE eligible.prefix||'%'");
        self.stage(query.build()).await?;

        let mut query = QueryBuilder::<Sqlite>::new("WITH requested(prefix,directory) AS (");
        query.push_values(prefixes, |mut row, (prefix, directory)| {
            row.push_bind(prefix).push_bind(directory);
        });
        query.push("), eligible AS (SELECT prefix FROM requested WHERE directory OR 1=(SELECT count(DISTINCT track.album_key) FROM tracks track WHERE track.source_key=")
            .push_bind(source)
            .push(" AND track.album_key IS NOT NULL AND track.media_uri>=('file://'||requested.prefix) AND track.media_uri<('file://'||requested.prefix||char(1114111)))) INSERT OR IGNORE INTO temp.scan_artwork_invalidations(album_object_id) SELECT DISTINCT album.object_id FROM eligible JOIN tracks track ON track.source_path>=eligible.prefix AND track.source_path<(eligible.prefix||char(1114111)) JOIN albums album USING(album_key) WHERE track.source_key=")
            .push_bind(source);
        self.stage(query.build()).await
    }

    pub async fn expand_local_component(&mut self, source: SourceKey) -> LibraryResult<()> {
        self.stage(
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
        self.stage(
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
                     JOIN local_file_dependencies dependency
                       ON dependency.dependency_path=current.path
                     JOIN local_files file
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
                     SELECT object_id FROM temp.scan_albums
                     UNION
                     SELECT album.object_id FROM temp.scan_removals removal
                     JOIN tracks track ON track.source_key=?1
                       AND removal.entity_kind='track' AND removal.object_id=track.object_id
                     JOIN albums album USING(album_key)
                 ), affected_path(path) AS (
                     SELECT DISTINCT COALESCE(track.cue_path,substr(track.media_uri,8))
                     FROM affected_album affected
                     JOIN albums album ON album.source_key=?1 AND album.object_id=affected.object_id
                     JOIN tracks track USING(album_key)
                     WHERE (track.cue_path IS NOT NULL OR track.media_uri LIKE 'file://%')
                       AND NOT EXISTS (
                           SELECT 1 FROM temp.scan_local_component_paths component
                           WHERE component.path=COALESCE(track.cue_path,substr(track.media_uri,8))
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
        self.stage(
            sqlx::query(
                "INSERT OR IGNORE INTO temp.scan_removals(entity_kind,object_id)
                 SELECT 'track',track.object_id FROM tracks track
                 JOIN temp.scan_local_component_paths component
                   ON track.media_uri='file://'||component.path
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
        self.stage(sqlx::query("DELETE FROM temp.scan_retained_paths"))
            .await?;
        let mut requested =
            QueryBuilder::<Sqlite>::new("INSERT INTO temp.scan_retained_paths(path) ");
        requested.push_values(paths, |mut row, path| {
            row.push_bind(path);
        });
        self.stage(requested.build()).await?;
        self.retain_local_tracks_where(
            "track.media_uri IN (SELECT 'file://'||path FROM temp.scan_retained_paths)",
            None,
        )
        .await
    }

    pub async fn retain_local_cue_path(&mut self, path: &str) -> LibraryResult<()> {
        self.retain_local_tracks("track.cue_path=?2", path).await
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
        predicate: &'static str,
        value: Option<&str>,
    ) -> LibraryResult<()> {
        let Some(source_key) = self.existing_source_key else {
            return Ok(());
        };
        let statements = [
            format!(
                "INSERT OR IGNORE INTO temp.scan_albums(object_id,title,normalized_title,display_artist,sort_text,year,release_date,date_added,musicbrainz_release_id,musicbrainz_release_group_id,is_compilation,artwork_binding,favorite,rating,first_seen_at,source_loudness_analysis_key,loudness_analysis_key) SELECT DISTINCT album.object_id,album.title,album.normalized_title,album.display_artist,album.sort_text,album.year,album.release_date,album.date_added,album.musicbrainz_release_id,album.musicbrainz_release_group_id,album.is_compilation,album.artwork_binding,album.source_favorite,album.source_rating,album.first_seen_at,album.source_loudness_analysis_key,album.loudness_analysis_key FROM albums album JOIN tracks track USING(album_key) WHERE track.source_key=?1 AND {predicate}"
            ),
            format!(
                "INSERT OR IGNORE INTO temp.scan_tracks(object_id,album_object_id,title,normalized_search,display_album,display_artist,sort_text,duration_millis,disc_number,track_number,year,release_date,date_added,media_uri,source_path,source_format,comment,bpm,musicbrainz_recording_id,musicbrainz_release_track_id,cue_path,cue_start_millis,cue_end_millis,artwork_binding,favorite,rating,first_seen_at,baseline_play_count,baseline_skip_count,baseline_last_played,source_loudness_analysis_key) SELECT track.object_id,album.object_id,track.title,track.normalized_search,track.display_album,track.display_artist,track.sort_text,track.duration_millis,track.disc_number,track.track_number,track.year,track.release_date,track.date_added,track.media_uri,track.source_path,track.source_format,track.comment,track.bpm,track.musicbrainz_recording_id,track.musicbrainz_release_track_id,track.cue_path,track.cue_start_millis,track.cue_end_millis,track.artwork_binding,track.source_favorite,track.source_rating,track.first_seen_at,baseline.play_count,baseline.skip_count,baseline.last_played_at,track.source_loudness_analysis_key FROM tracks track LEFT JOIN albums album USING(album_key) LEFT JOIN activity_baseline baseline ON baseline.source_key=track.source_key AND baseline.track_object_id=track.object_id AND baseline.period='lifetime' AND baseline.item_kind='track' WHERE track.source_key=?1 AND {predicate}"
            ),
            format!(
                "INSERT OR IGNORE INTO temp.scan_artists SELECT DISTINCT artist.object_id,artist.name,artist.normalized_name,artist.sort_text,artist.musicbrainz_artist_id,artist.artwork_binding,artist.source_favorite,artist.source_rating FROM artists artist WHERE artist.source_key=?1 AND (EXISTS(SELECT 1 FROM track_artists relation JOIN tracks track USING(track_key) WHERE relation.artist_key=artist.artist_key AND {predicate}) OR EXISTS(SELECT 1 FROM album_artists relation JOIN albums album USING(album_key) JOIN tracks track USING(album_key) WHERE relation.artist_key=artist.artist_key AND {predicate}))"
            ),
            format!(
                "INSERT OR IGNORE INTO temp.scan_genres SELECT DISTINCT genre.object_id,genre.name,genre.normalized_name,genre.sort_text,genre.artwork_binding FROM genres genre WHERE genre.source_key=?1 AND (EXISTS(SELECT 1 FROM track_genres relation JOIN tracks track USING(track_key) WHERE relation.genre_key=genre.genre_key AND {predicate}) OR EXISTS(SELECT 1 FROM album_genres relation JOIN albums album USING(album_key) JOIN tracks track USING(album_key) WHERE relation.genre_key=genre.genre_key AND {predicate}))"
            ),
            format!(
                "INSERT OR IGNORE INTO temp.scan_moods SELECT DISTINCT mood.object_id,mood.name,mood.normalized_name,mood.sort_text FROM moods mood WHERE mood.source_key=?1 AND EXISTS(SELECT 1 FROM track_moods relation JOIN tracks track USING(track_key) WHERE relation.mood_key=mood.mood_key AND {predicate})"
            ),
            format!(
                "INSERT OR IGNORE INTO temp.scan_folders SELECT DISTINCT folder.object_id,folder.name,folder.normalized_name,folder.sort_text,folder.artwork_binding FROM folders folder WHERE folder.source_key=?1 AND EXISTS(SELECT 1 FROM track_folders relation JOIN tracks track USING(track_key) WHERE relation.folder_key=folder.folder_key AND {predicate})"
            ),
            format!(
                "INSERT OR IGNORE INTO temp.scan_track_artists SELECT track.object_id,artist.object_id,relation.position FROM track_artists relation JOIN tracks track USING(track_key) JOIN artists artist USING(artist_key) WHERE track.source_key=?1 AND {predicate}"
            ),
            format!(
                "INSERT OR IGNORE INTO temp.scan_track_genres SELECT track.object_id,genre.object_id,relation.position FROM track_genres relation JOIN tracks track USING(track_key) JOIN genres genre USING(genre_key) WHERE track.source_key=?1 AND {predicate}"
            ),
            format!(
                "INSERT OR IGNORE INTO temp.scan_track_moods SELECT track.object_id,mood.object_id,relation.position FROM track_moods relation JOIN tracks track USING(track_key) JOIN moods mood USING(mood_key) WHERE track.source_key=?1 AND {predicate}"
            ),
            format!(
                "INSERT OR IGNORE INTO temp.scan_track_folders SELECT track.object_id,folder.object_id,relation.position FROM track_folders relation JOIN tracks track USING(track_key) JOIN folders folder USING(folder_key) WHERE track.source_key=?1 AND {predicate}"
            ),
            format!(
                "INSERT OR IGNORE INTO temp.scan_album_artists SELECT DISTINCT album.object_id,artist.object_id,relation.position FROM album_artists relation JOIN albums album USING(album_key) JOIN artists artist USING(artist_key) JOIN tracks track USING(album_key) WHERE track.source_key=?1 AND {predicate}"
            ),
            format!(
                "INSERT OR IGNORE INTO temp.scan_album_genres SELECT DISTINCT album.object_id,genre.object_id,relation.position FROM album_genres relation JOIN albums album USING(album_key) JOIN genres genre USING(genre_key) JOIN tracks track USING(album_key) WHERE track.source_key=?1 AND {predicate}"
            ),
            format!(
                "INSERT OR IGNORE INTO temp.scan_album_release_types SELECT DISTINCT album.object_id,relation.release_type,relation.position FROM album_release_types relation JOIN albums album USING(album_key) JOIN tracks track USING(album_key) WHERE track.source_key=?1 AND {predicate}"
            ),
        ];
        for statement in statements {
            let query = sqlx::query(sqlx::AssertSqlSafe(statement)).bind(source_key);
            if let Some(value) = value {
                self.stage(query.bind(value)).await?;
            } else {
                self.stage(query).await?;
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn write_artist(
        &mut self,
        object_id: &str,
        name: &str,
        normalized_name: &str,
        sort_text: &str,
        musicbrainz_artist_id: Option<&str>,
        artwork_binding: Option<&[u8]>,
        favorite: bool,
        rating: Option<i64>,
    ) -> LibraryResult<()> {
        self.require_id("artist", object_id)?;
        let rating = self.public_rating(rating)?;
        self.require_row_bytes(&[
            object_id.as_bytes(),
            name.as_bytes(),
            normalized_name.as_bytes(),
            sort_text.as_bytes(),
            musicbrainz_artist_id.unwrap_or_default().as_bytes(),
            artwork_binding.unwrap_or_default(),
        ])?;
        self.stage(
            sqlx::query("INSERT INTO temp.scan_artists VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) ON CONFLICT(object_id) DO UPDATE SET name=excluded.name,normalized_name=excluded.normalized_name,sort_text=excluded.sort_text,musicbrainz_artist_id=COALESCE(excluded.musicbrainz_artist_id,scan_artists.musicbrainz_artist_id),artwork_binding=COALESCE(excluded.artwork_binding,scan_artists.artwork_binding),favorite=max(scan_artists.favorite,excluded.favorite),rating=COALESCE(excluded.rating,scan_artists.rating)")
                .bind(object_id)
                .bind(name)
                .bind(normalized_name)
                .bind(sort_text)
                .bind(musicbrainz_artist_id)
                .bind(artwork_binding)
                .bind(favorite)
                .bind(rating),
        )
        .await
    }

    pub async fn write_genre(
        &mut self,
        object_id: &str,
        name: &str,
        normalized_name: &str,
        sort_text: &str,
        artwork_binding: Option<&[u8]>,
    ) -> LibraryResult<()> {
        self.require_id("genre", object_id)?;
        self.require_row_bytes(&[
            object_id.as_bytes(),
            name.as_bytes(),
            normalized_name.as_bytes(),
            sort_text.as_bytes(),
            artwork_binding.unwrap_or_default(),
        ])?;
        self.stage(
            sqlx::query("INSERT INTO temp.scan_genres VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(object_id) DO UPDATE SET name=excluded.name,normalized_name=excluded.normalized_name,sort_text=excluded.sort_text,artwork_binding=COALESCE(excluded.artwork_binding,scan_genres.artwork_binding)")
                .bind(object_id)
                .bind(name)
                .bind(normalized_name)
                .bind(sort_text)
                .bind(artwork_binding),
        )
        .await
    }

    pub async fn write_mood(
        &mut self,
        object_id: &str,
        name: &str,
        normalized_name: &str,
        sort_text: &str,
    ) -> LibraryResult<()> {
        self.require_id("mood", object_id)?;
        self.require_row_bytes(&[
            object_id.as_bytes(),
            name.as_bytes(),
            normalized_name.as_bytes(),
            sort_text.as_bytes(),
        ])?;
        self.stage(
            sqlx::query("INSERT INTO temp.scan_moods VALUES (?1, ?2, ?3, ?4) ON CONFLICT(object_id) DO UPDATE SET name=excluded.name,normalized_name=excluded.normalized_name,sort_text=excluded.sort_text")
                .bind(object_id)
                .bind(name)
                .bind(normalized_name)
                .bind(sort_text),
        )
        .await
    }

    pub async fn write_folder(
        &mut self,
        object_id: &str,
        name: &str,
        normalized_name: &str,
        sort_text: &str,
        artwork_binding: Option<&[u8]>,
    ) -> LibraryResult<()> {
        self.require_id("folder", object_id)?;
        self.require_row_bytes(&[
            object_id.as_bytes(),
            name.as_bytes(),
            normalized_name.as_bytes(),
            sort_text.as_bytes(),
            artwork_binding.unwrap_or_default(),
        ])?;
        self.stage(
            sqlx::query("INSERT INTO temp.scan_folders VALUES (?1, ?2, ?3, ?4, ?5)")
                .bind(object_id)
                .bind(name)
                .bind(normalized_name)
                .bind(sort_text)
                .bind(artwork_binding),
        )
        .await
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
            "INSERT INTO temp.scan_track_folders(owner_id,related_id,position) ",
            links,
        )
        .await
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
        self.require_row_bytes(&[
            object_id.as_bytes(),
            name.as_bytes(),
            normalized_name.as_bytes(),
            sort_text.as_bytes(),
            artwork_binding.unwrap_or_default(),
        ])?;
        self.stage(
            sqlx::query("INSERT INTO temp.scan_playlists VALUES (?1, ?2, ?3, ?4, ?5)")
                .bind(object_id)
                .bind(name)
                .bind(normalized_name)
                .bind(sort_text)
                .bind(artwork_binding),
        )
        .await
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
        self.stage(
            sqlx::query("INSERT INTO temp.scan_playlist_entries VALUES (?1, ?2, ?3, ?4)")
                .bind(playlist_id)
                .bind(object_id)
                .bind(track_id)
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
            input.artwork_binding.as_deref().unwrap_or_default(),
        ])?;
        self.stage(
            sqlx::query("INSERT INTO temp.scan_home_entries VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)")
                .bind(&input.section_id)
                .bind(input.position)
                .bind(input.kind.as_str())
                .bind(&input.entity_object_id)
                .bind(&input.title)
                .bind(&input.subtitle)
                .bind(input.artwork_binding.as_deref()),
        )
        .await
    }

    /// Canonicalizes staging and atomically accepts, ignores, or rejects it.
    pub async fn finish(mut self) -> LibraryResult<ScanOutcome> {
        if self.batch_writer.is_some() {
            self.finish_batch().await?;
        }
        if self.failed {
            let cleanup = self.cleanup().await;
            self.database.release_scan(self.token);
            cleanup?;
            return Ok(ScanOutcome::Failed);
        }
        if !self.database.scan_is_current(self.token) {
            self.database.release_scan(self.token);
            return Ok(ScanOutcome::Failed);
        }
        if self.point_update {
            let mut writer = self.database.writer().await?;
            let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
            let result = self.publish_items(connection).await;
            let cleanup = drop_staging(connection).await;
            drop(writer);
            self.database.release_scan(self.token);
            cleanup?;
            return result;
        }
        prepare_album_loudness_keys(&self.database, self.token, &self.source_id).await?;
        let staged_digest = canonical_catalog_digest(&self.database, self.token).await?;
        let catalog_digest =
            digest_catalog_header(staged_digest, &self.display_name, &self.normalized_name);
        let artwork_digest = canonical_artwork_digest(&self.database, self.token).await?;
        let mut writer = self.database.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let result = self
            .publish_staged(connection, catalog_digest, artwork_digest)
            .await;
        if matches!(
            &result,
            Err(LibraryError::Sqlite(
                sqlx::Error::WorkerCrashed | sqlx::Error::Io(_)
            ))
        ) {
            *writer = None;
            self.database.writer_failed();
            return result;
        }
        let cleanup = drop_staging(connection).await;
        drop(writer);
        self.database.release_scan(self.token);
        cleanup?;
        result
    }

    async fn publish_staged(
        &self,
        connection: &mut sqlx::SqliteConnection,
        catalog_digest: [u8; 32],
        artwork_digest: [u8; 32],
    ) -> LibraryResult<ScanOutcome> {
        if !self.database.scan_is_current(self.token) {
            return Ok(ScanOutcome::Failed);
        }
        let mut transaction = connection.begin().await?;
        let current = sqlx::query_as::<_, AcceptedSource>(
            "SELECT source_key, catalog_digest, artwork_digest, catalog_revision
             FROM sources WHERE object_id=?1",
        )
        .bind(&self.source_id)
        .fetch_optional(&mut *transaction)
        .await?;
        if current.as_ref().map(|row| row.catalog_revision) != self.expected_revision {
            transaction.rollback().await?;
            return Ok(ScanOutcome::Stale);
        }
        if let Some(current) = &current {
            if current.catalog_digest.as_slice() == catalog_digest {
                if current.artwork_digest.as_slice() != artwork_digest {
                    publish_artwork_bindings(&mut transaction, current.source_key).await?;
                    publish_local_files(&mut transaction, current.source_key, true).await?;
                    sqlx::query(
                        "UPDATE sources SET display_name=?2,normalized_name=?3,freshness=?4,artwork_digest=?5 WHERE source_key=?1",
                    )
                    .bind(current.source_key)
                    .bind(&self.display_name)
                    .bind(&self.normalized_name)
                    .bind(self.freshness.as_ref().map(Freshness::as_bytes))
                    .bind(artwork_digest.as_slice())
                    .execute(&mut *transaction)
                    .await?;
                    transaction.commit().await?;
                    return Ok(ScanOutcome::ArtworkChanged(publication(
                        current.source_key,
                        current.catalog_revision,
                        &artwork_digest,
                    )?));
                }
                sqlx::query(
                    "UPDATE sources
                     SET display_name=?2, normalized_name=?3, freshness=?4
                     WHERE source_key=?1",
                )
                .bind(current.source_key)
                .bind(&self.display_name)
                .bind(&self.normalized_name)
                .bind(self.freshness.as_ref().map(Freshness::as_bytes))
                .execute(&mut *transaction)
                .await?;
                publish_local_files(&mut transaction, current.source_key, true).await?;
                transaction.commit().await?;
                return Ok(ScanOutcome::Identical(publication(
                    current.source_key,
                    current.catalog_revision,
                    &current.artwork_digest,
                )?));
            }
        }

        let source_key = if let Some(current) = current {
            current.source_key
        } else {
            sqlx::query(
                "INSERT INTO sources(
                     object_id, display_name, normalized_name, freshness,
                     catalog_digest, artwork_digest, catalog_revision
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
            )
            .bind(&self.source_id)
            .bind(&self.display_name)
            .bind(&self.normalized_name)
            .bind(self.freshness.as_ref().map(Freshness::as_bytes))
            .bind(catalog_digest.as_slice())
            .bind(artwork_digest.as_slice())
            .execute(&mut *transaction)
            .await?
            .last_insert_rowid()
        };
        validate_references(&mut transaction).await?;
        publish_entities(&mut transaction, source_key, true).await?;
        publish_activity_baseline(&mut transaction, source_key).await?;
        publish_source_loudness(&mut transaction, source_key, true).await?;
        publish_links(&mut transaction, source_key, true).await?;
        publish_local_files(&mut transaction, source_key, true).await?;
        let revision = self.expected_revision.unwrap_or(0) + 1;
        sqlx::query(
            "UPDATE sources
             SET display_name=?2, normalized_name=?3, freshness=?4,
                 catalog_digest=?5, artwork_digest=?6, catalog_revision=?7
             WHERE source_key=?1",
        )
        .bind(source_key)
        .bind(&self.display_name)
        .bind(&self.normalized_name)
        .bind(self.freshness.as_ref().map(Freshness::as_bytes))
        .bind(catalog_digest.as_slice())
        .bind(artwork_digest.as_slice())
        .bind(revision)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(ScanOutcome::Changed(publication(
            source_key,
            revision,
            &artwork_digest,
        )?))
    }

    async fn publish_items(
        &self,
        connection: &mut sqlx::SqliteConnection,
    ) -> LibraryResult<ScanOutcome> {
        let mut transaction = connection.begin().await?;
        let Some((source_key, revision, mut artwork_digest)) =
            sqlx::query_as::<_, (i64, i64, Vec<u8>)>(
                "SELECT source_key,catalog_revision,artwork_digest FROM sources WHERE object_id=?1",
            )
            .bind(&self.source_id)
            .fetch_optional(&mut *transaction)
            .await?
        else {
            transaction.rollback().await?;
            return Err(LibraryError::InvalidRequest(
                "live source is not cached".to_string(),
            ));
        };
        if Some(revision) != self.expected_revision {
            transaction.rollback().await?;
            return Ok(ScanOutcome::Stale);
        }
        let artwork_changed = staged_artwork_changed(&mut transaction, source_key).await?;
        sqlx::query(
            "UPDATE temp.scan_albums AS staged SET
                 source_loudness_analysis_key=(SELECT album.source_loudness_analysis_key FROM albums album WHERE album.source_key=?1 AND album.object_id=staged.object_id),
                 loudness_analysis_key=(SELECT album.loudness_analysis_key FROM albums album WHERE album.source_key=?1 AND album.object_id=staged.object_id)
             WHERE EXISTS (SELECT 1 FROM albums album WHERE album.source_key=?1 AND album.object_id=staged.object_id)",
        )
        .bind(source_key)
        .execute(&mut *transaction)
        .await?;
        let playlists_changed = staged_playlists_changed(&mut transaction, source_key).await?;
        let non_playlist_changed =
            staged_non_playlist_catalog_changed(&mut transaction, source_key).await?;
        if !playlists_changed && !non_playlist_changed {
            publish_local_files(&mut transaction, source_key, false).await?;
            let artwork_changed =
                publish_artwork_invalidations(&mut transaction, source_key).await? > 0;
            if artwork_changed {
                sqlx::query("UPDATE sources SET artwork_digest=randomblob(32) WHERE source_key=?1")
                    .bind(source_key)
                    .execute(&mut *transaction)
                    .await?;
                artwork_digest =
                    sqlx::query_scalar("SELECT artwork_digest FROM sources WHERE source_key=?1")
                        .bind(source_key)
                        .fetch_one(&mut *transaction)
                        .await?;
            }
            transaction.commit().await?;
            return Ok(if artwork_changed {
                ScanOutcome::ArtworkChanged(publication(source_key, revision, &artwork_digest)?)
            } else {
                ScanOutcome::Identical(publication(source_key, revision, &artwork_digest)?)
            });
        }
        validate_references(&mut transaction).await?;
        publish_entities(&mut transaction, source_key, false).await?;
        let affected_albums = sqlx::query_scalar::<_, crate::AlbumKey>(
            "SELECT DISTINCT track.album_key FROM tracks track
             JOIN temp.scan_tracks staged ON staged.object_id=track.object_id
             WHERE track.source_key=?1 AND track.album_key IS NOT NULL",
        )
        .bind(source_key)
        .fetch_all(&mut *transaction)
        .await?;
        for album in affected_albums {
            crate::loudness::recompute_album_source_and_current_keys(&mut transaction, album)
                .await?;
        }
        publish_activity_baseline(&mut transaction, source_key).await?;
        publish_source_loudness(&mut transaction, source_key, false).await?;
        publish_links(&mut transaction, source_key, false).await?;
        publish_removals(&mut transaction, source_key).await?;
        let artwork_changed = publish_artwork_invalidations(&mut transaction, source_key).await?
            > 0
            || artwork_changed;
        if self.local_point_update {
            prune_local_orphans(&mut transaction, source_key).await?;
        }
        publish_local_files(&mut transaction, source_key, false).await?;
        let revision = revision + 1;
        sqlx::query("UPDATE sources SET freshness=NULL,catalog_revision=?2,artwork_digest=CASE WHEN ?3 THEN randomblob(32) ELSE artwork_digest END WHERE source_key=?1")
            .bind(source_key)
            .bind(revision)
            .bind(artwork_changed)
            .execute(&mut *transaction)
            .await?;
        if artwork_changed {
            artwork_digest =
                sqlx::query_scalar("SELECT artwork_digest FROM sources WHERE source_key=?1")
                    .bind(source_key)
                    .fetch_one(&mut *transaction)
                    .await?;
        }
        transaction.commit().await?;
        let publication = publication(source_key, revision, &artwork_digest)?;
        Ok(if playlists_changed && !non_playlist_changed {
            ScanOutcome::PlaylistsChanged(publication)
        } else {
            ScanOutcome::Changed(publication)
        })
    }

    async fn cleanup(&self) -> LibraryResult<()> {
        if let Ok(mut writer) = self.database.writer().await {
            if let Some(connection) = writer.as_mut() {
                drop_staging(connection).await?;
            }
        }
        Ok(())
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
            self.stage(query.build()).await?;
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
            self.stage(query.build()).await?;
        }
        Ok(())
    }

    async fn write_source_loudness(
        &mut self,
        sql: &'static str,
        object_id: &str,
        integrated_lufs: Option<f64>,
        true_peak: Option<f64>,
        replay_gain_db: Option<f64>,
        replay_gain_peak: Option<f64>,
    ) -> LibraryResult<()> {
        self.require_id("loudness entity", object_id)?;
        if (integrated_lufs.is_none() && replay_gain_db.is_none())
            || integrated_lufs.is_some_and(|value| !value.is_finite())
            || true_peak.is_some_and(|value| !value.is_finite() || value < 0.0)
            || replay_gain_db.is_some_and(|value| !value.is_finite())
            || replay_gain_peak.is_some_and(|value| !value.is_finite() || value < 0.0)
        {
            self.failed = true;
            return Err(LibraryError::InvalidScan(
                "invalid source loudness fact".to_string(),
            ));
        }
        let query = || {
            sqlx::query(sql)
                .bind(object_id)
                .bind(integrated_lufs)
                .bind(true_peak)
                .bind(replay_gain_db)
                .bind(replay_gain_peak)
        };
        let result = if let Some(writer) = self.batch_writer.as_mut() {
            let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
            query().execute(&mut *connection).await
        } else {
            let mut writer = self.database.writer().await?;
            let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
            query().execute(&mut *connection).await
        };
        match result {
            Ok(result) if result.rows_affected() == 1 => Ok(()),
            Ok(_) => {
                self.failed = true;
                Err(LibraryError::InvalidScan(
                    "source loudness entity has not been staged".to_string(),
                ))
            }
            Err(error) => {
                if matches!(error, sqlx::Error::WorkerCrashed | sqlx::Error::Io(_)) {
                    if let Some(writer) = self.batch_writer.as_mut() {
                        **writer = None;
                    }
                    self.database.writer_failed();
                }
                self.failed = true;
                Err(error.into())
            }
        }
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
        if bytes.is_none_or(|bytes| bytes > MAX_STAGED_ROW_BYTES) {
            self.failed = true;
            return Err(LibraryError::InvalidScan(
                "one staged row may not exceed 8 MiB".to_string(),
            ));
        }
        Ok(())
    }

    fn public_rating(&mut self, rating: Option<i64>) -> LibraryResult<Option<i64>> {
        if rating.is_some_and(|rating| !(0..=10).contains(&rating)) {
            self.failed = true;
            return Err(LibraryError::InvalidScan(
                "source rating must be in Rufin's 0..=10 scale".to_string(),
            ));
        }
        Ok(rating.map(|rating| rating * 10))
    }

    async fn stage<'query>(
        &mut self,
        query: Query<'query, Sqlite, SqliteArguments>,
    ) -> LibraryResult<()> {
        if self.failed {
            return Err(LibraryError::ScanFailed);
        }
        if !self.database.scan_is_current(self.token) {
            self.failed = true;
            return Err(LibraryError::ScanFailed);
        }
        if let Some(writer) = self.batch_writer.as_mut() {
            let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
            return match query.execute(&mut *connection).await {
                Ok(_) => Ok(()),
                Err(error) => {
                    if matches!(error, sqlx::Error::WorkerCrashed | sqlx::Error::Io(_)) {
                        **writer = None;
                        self.database.writer_failed();
                    }
                    self.failed = true;
                    Err(error.into())
                }
            };
        }
        let mut writer = self.database.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        match query.execute(&mut *connection).await {
            Ok(_) => Ok(()),
            Err(error) => {
                if matches!(error, sqlx::Error::WorkerCrashed | sqlx::Error::Io(_)) {
                    *writer = None;
                    self.database.writer_failed();
                }
                self.failed = true;
                Err(error.into())
            }
        }
    }
}

async fn publish_artwork_bindings(
    transaction: &mut Transaction<'_, Sqlite>,
    source_key: i64,
) -> LibraryResult<()> {
    for sql in [
        "UPDATE tracks SET artwork_binding=(SELECT staged.artwork_binding FROM temp.scan_tracks staged WHERE staged.object_id=tracks.object_id) WHERE source_key=?1 AND EXISTS(SELECT 1 FROM temp.scan_tracks staged WHERE staged.object_id=tracks.object_id)",
        "UPDATE albums SET artwork_binding=(SELECT staged.artwork_binding FROM temp.scan_albums staged WHERE staged.object_id=albums.object_id) WHERE source_key=?1 AND EXISTS(SELECT 1 FROM temp.scan_albums staged WHERE staged.object_id=albums.object_id)",
        "UPDATE artists SET artwork_binding=(SELECT staged.artwork_binding FROM temp.scan_artists staged WHERE staged.object_id=artists.object_id) WHERE source_key=?1 AND EXISTS(SELECT 1 FROM temp.scan_artists staged WHERE staged.object_id=artists.object_id)",
        "UPDATE genres SET artwork_binding=(SELECT staged.artwork_binding FROM temp.scan_genres staged WHERE staged.object_id=genres.object_id) WHERE source_key=?1 AND EXISTS(SELECT 1 FROM temp.scan_genres staged WHERE staged.object_id=genres.object_id)",
        "UPDATE folders SET artwork_binding=(SELECT staged.artwork_binding FROM temp.scan_folders staged WHERE staged.object_id=folders.object_id) WHERE source_key=?1 AND EXISTS(SELECT 1 FROM temp.scan_folders staged WHERE staged.object_id=folders.object_id)",
        "UPDATE playlists SET artwork_binding=(SELECT staged.artwork_binding FROM temp.scan_playlists staged WHERE staged.object_id=playlists.object_id) WHERE source_key=?1 AND ownership='source' AND EXISTS(SELECT 1 FROM temp.scan_playlists staged WHERE staged.object_id=playlists.object_id)",
    ] {
        sqlx::query(sql)
            .bind(source_key)
            .execute(&mut **transaction)
            .await?;
    }
    Ok(())
}

impl Database {
    pub async fn cached_source(
        &self,
        object_id: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<CachedSource>> {
        validate_id("source", object_id)?;
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_as::<_, CachedSource>(
            "SELECT source_key source,object_id,display_name,catalog_revision,artwork_digest
             FROM sources WHERE object_id=?1",
        )
        .bind(object_id)
        .fetch_optional(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }
}

impl Drop for Scan {
    fn drop(&mut self) {
        if let Some(mut writer) = self.batch_writer.take() {
            let database = self.database.clone();
            let token = self.token;
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    if let Some(connection) = writer.as_mut() {
                        let _ = sqlx::query("ROLLBACK").execute(&mut *connection).await;
                        let _ = drop_staging(connection).await;
                    }
                    database.release_scan(token);
                });
            } else {
                self.database.release_scan(self.token);
            }
            return;
        }
        if !self.database.scan_is_current(self.token) {
            self.database.release_scan(self.token);
            return;
        }
        let database = self.database.clone();
        let token = self.token;
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if let Ok(mut writer) = database.writer().await {
                    if let Some(connection) = writer.as_mut() {
                        let _ = drop_staging(connection).await;
                    }
                }
                database.release_scan(token);
            });
        } else {
            self.database.release_scan(self.token);
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

fn validate_row_bytes(values: &[&[u8]]) -> LibraryResult<()> {
    let bytes = values
        .iter()
        .try_fold(0_usize, |total, value| total.checked_add(value.len()));
    if bytes.is_none_or(|bytes| bytes > MAX_STAGED_ROW_BYTES) {
        return Err(LibraryError::InvalidScan(
            "one staged row may not exceed 8 MiB".to_string(),
        ));
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
        artwork_digest: artwork_digest.try_into().map_err(|_| {
            LibraryError::InvalidStore("source artwork digest is not 32 bytes".to_string())
        })?,
    })
}

async fn create_staging(connection: &mut sqlx::SqliteConnection) -> LibraryResult<()> {
    drop_staging(connection).await?;
    sqlx::raw_sql(
        "CREATE TEMP TABLE scan_albums(
             object_id TEXT PRIMARY KEY, title TEXT NOT NULL,
             normalized_title TEXT NOT NULL, display_artist TEXT NOT NULL,
             sort_text TEXT NOT NULL, year INTEGER, release_date TEXT,
             date_added TEXT, musicbrainz_release_id TEXT,
             musicbrainz_release_group_id TEXT, is_compilation INTEGER,
             artwork_binding BLOB,
             favorite INTEGER NOT NULL,
             rating INTEGER, first_seen_at INTEGER,
             source_loudness_analysis_key BLOB NOT NULL DEFAULT X'0000000000000000000000000000000000000000000000000000000000000000',
             loudness_analysis_key BLOB NOT NULL DEFAULT X'0000000000000000000000000000000000000000000000000000000000000000',
             source_integrated_lufs REAL, source_true_peak REAL,
             source_replay_gain_db REAL, source_replay_gain_peak REAL
         ) STRICT;
         CREATE TEMP TABLE scan_tracks(
             object_id TEXT PRIMARY KEY, album_object_id TEXT,
             title TEXT NOT NULL, normalized_search TEXT NOT NULL,
             display_album TEXT NOT NULL, display_artist TEXT NOT NULL,
             sort_text TEXT NOT NULL, duration_millis INTEGER NOT NULL,
             disc_number INTEGER NOT NULL, track_number INTEGER NOT NULL,
             year INTEGER, release_date TEXT, date_added TEXT, media_uri TEXT,
             source_path TEXT, source_format TEXT, comment TEXT, bpm INTEGER,
             musicbrainz_recording_id TEXT, musicbrainz_release_track_id TEXT,
             cue_path TEXT, cue_start_millis INTEGER, cue_end_millis INTEGER,
             artwork_binding BLOB, favorite INTEGER NOT NULL,
             rating INTEGER, first_seen_at INTEGER,
             baseline_play_count INTEGER, baseline_skip_count INTEGER,
             baseline_last_played INTEGER,
             source_loudness_analysis_key BLOB NOT NULL,
             source_integrated_lufs REAL, source_true_peak REAL,
             source_replay_gain_db REAL, source_replay_gain_peak REAL
         ) STRICT;
         CREATE INDEX scan_tracks_album_loudness_idx ON scan_tracks(
             album_object_id, disc_number, track_number, sort_text, object_id
         );
         CREATE TEMP TABLE scan_local_dependency_paths(
             path TEXT PRIMARY KEY
         ) STRICT;
         CREATE TEMP TABLE scan_local_files(
             path TEXT PRIMARY KEY, root TEXT NOT NULL, relative_path TEXT NOT NULL,
             kind TEXT NOT NULL, size_bytes INTEGER, mtime_ns INTEGER NOT NULL,
             device_id INTEGER, inode INTEGER, parse_version INTEGER, state TEXT NOT NULL
         ) STRICT;
         CREATE TEMP TABLE scan_local_file_dependencies(
             path TEXT NOT NULL, dependency_path TEXT NOT NULL, position INTEGER NOT NULL,
             PRIMARY KEY(path,dependency_path)
         ) STRICT;
         CREATE TEMP TABLE scan_local_file_removals(path TEXT PRIMARY KEY) STRICT;
         CREATE TEMP TABLE scan_retained_paths(path TEXT PRIMARY KEY) STRICT;
         CREATE TEMP TABLE scan_local_component_paths(path TEXT PRIMARY KEY) STRICT;
         CREATE TEMP TABLE scan_removals(
             entity_kind TEXT NOT NULL,
             object_id TEXT NOT NULL,
             PRIMARY KEY(entity_kind,object_id)
         ) STRICT;
         CREATE TEMP TABLE scan_artwork_invalidations(album_object_id TEXT PRIMARY KEY) STRICT;
         CREATE TEMP TABLE scan_artists(
             object_id TEXT PRIMARY KEY, name TEXT NOT NULL,
             normalized_name TEXT NOT NULL, sort_text TEXT NOT NULL,
             musicbrainz_artist_id TEXT, artwork_binding BLOB,
             favorite INTEGER NOT NULL, rating INTEGER
         ) STRICT;
         CREATE TEMP TABLE scan_genres(
             object_id TEXT PRIMARY KEY, name TEXT NOT NULL,
             normalized_name TEXT NOT NULL, sort_text TEXT NOT NULL,
             artwork_binding BLOB
         ) STRICT;
         CREATE TEMP TABLE scan_moods(
             object_id TEXT PRIMARY KEY, name TEXT NOT NULL,
             normalized_name TEXT NOT NULL, sort_text TEXT NOT NULL
         ) STRICT;
         CREATE TEMP TABLE scan_folders(
             object_id TEXT PRIMARY KEY, name TEXT NOT NULL,
             normalized_name TEXT NOT NULL, sort_text TEXT NOT NULL,
             artwork_binding BLOB
         ) STRICT;
         CREATE TEMP TABLE scan_album_artists(
             owner_id TEXT NOT NULL, related_id TEXT NOT NULL,
             position INTEGER NOT NULL,
             PRIMARY KEY(owner_id, position), UNIQUE(owner_id, related_id)
         ) STRICT;
         CREATE TEMP TABLE scan_track_artists(
             owner_id TEXT NOT NULL, related_id TEXT NOT NULL,
             position INTEGER NOT NULL,
             PRIMARY KEY(owner_id, position), UNIQUE(owner_id, related_id)
         ) STRICT;
         CREATE TEMP TABLE scan_album_genres(
             owner_id TEXT NOT NULL, related_id TEXT NOT NULL,
             position INTEGER NOT NULL,
             PRIMARY KEY(owner_id, position), UNIQUE(owner_id, related_id)
         ) STRICT;
         CREATE TEMP TABLE scan_track_genres(
             owner_id TEXT NOT NULL, related_id TEXT NOT NULL,
             position INTEGER NOT NULL,
             PRIMARY KEY(owner_id, position), UNIQUE(owner_id, related_id)
         ) STRICT;
         CREATE TEMP TABLE scan_track_moods(
             owner_id TEXT NOT NULL, related_id TEXT NOT NULL,
             position INTEGER NOT NULL,
             PRIMARY KEY(owner_id, position), UNIQUE(owner_id, related_id)
         ) STRICT;
         CREATE TEMP TABLE scan_track_folders(
             owner_id TEXT NOT NULL, related_id TEXT NOT NULL,
             position INTEGER NOT NULL,
             PRIMARY KEY(owner_id, position), UNIQUE(owner_id, related_id)
         ) STRICT;
         CREATE TEMP TABLE scan_album_release_types(
             owner_id TEXT NOT NULL, related_id TEXT NOT NULL,
             position INTEGER NOT NULL,
             PRIMARY KEY(owner_id, position), UNIQUE(owner_id, related_id)
         ) STRICT;
         CREATE TEMP TABLE scan_playlists(
             object_id TEXT PRIMARY KEY, name TEXT NOT NULL,
             normalized_name TEXT NOT NULL, sort_text TEXT NOT NULL,
             artwork_binding BLOB
         ) STRICT;
         CREATE TEMP TABLE scan_playlist_entries(
             playlist_id TEXT NOT NULL, object_id TEXT NOT NULL,
             track_id TEXT NOT NULL, position INTEGER NOT NULL,
             PRIMARY KEY(playlist_id, position), UNIQUE(playlist_id, object_id)
         ) STRICT;
         CREATE TEMP TABLE scan_home_entries(
             owner_id TEXT NOT NULL, position INTEGER NOT NULL,
             entity_kind TEXT NOT NULL CHECK (
                 entity_kind IN ('track', 'album', 'artist', 'playlist')
             ), entity_object_id TEXT NOT NULL,
             title TEXT NOT NULL, subtitle TEXT NOT NULL, artwork_binding BLOB,
             PRIMARY KEY(owner_id, position)
         ) STRICT;",
    )
    .execute(connection)
    .await?;
    Ok(())
}

async fn drop_staging(connection: &mut sqlx::SqliteConnection) -> LibraryResult<()> {
    sqlx::raw_sql(
        "DROP TABLE IF EXISTS temp.scan_artwork_invalidations;
         DROP TABLE IF EXISTS temp.scan_removals;
         DROP TABLE IF EXISTS temp.scan_local_dependency_paths;
         DROP TABLE IF EXISTS temp.scan_local_file_dependencies;
         DROP TABLE IF EXISTS temp.scan_local_file_removals;
         DROP TABLE IF EXISTS temp.scan_local_files;
         DROP TABLE IF EXISTS temp.scan_retained_paths;
         DROP TABLE IF EXISTS temp.scan_local_component_paths;
         DROP TABLE IF EXISTS temp.scan_home_entries;
         DROP TABLE IF EXISTS temp.scan_playlist_entries;
         DROP TABLE IF EXISTS temp.scan_playlists;
         DROP TABLE IF EXISTS temp.scan_album_release_types;
         DROP TABLE IF EXISTS temp.scan_track_folders;
         DROP TABLE IF EXISTS temp.scan_track_moods;
         DROP TABLE IF EXISTS temp.scan_track_genres;
         DROP TABLE IF EXISTS temp.scan_album_genres;
         DROP TABLE IF EXISTS temp.scan_track_artists;
         DROP TABLE IF EXISTS temp.scan_album_artists;
         DROP TABLE IF EXISTS temp.scan_folders;
         DROP TABLE IF EXISTS temp.scan_moods;
         DROP TABLE IF EXISTS temp.scan_genres;
         DROP TABLE IF EXISTS temp.scan_artists;
         DROP TABLE IF EXISTS temp.scan_tracks;
         DROP TABLE IF EXISTS temp.scan_albums;",
    )
    .execute(connection)
    .await?;
    Ok(())
}

async fn prepare_album_loudness_keys(
    database: &Database,
    token: u64,
    source_id: &str,
) -> LibraryResult<()> {
    let empty = *album_loudness_hasher().finalize().as_bytes();
    let source_key = {
        let mut writer = database.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        sqlx::query(
            "UPDATE temp.scan_albums SET source_loudness_analysis_key=?1,loudness_analysis_key=?1",
        )
        .bind(empty.as_slice())
        .execute(&mut *connection)
        .await?;
        sqlx::query_scalar::<_, i64>("SELECT source_key FROM sources WHERE object_id=?1")
            .bind(source_id)
            .fetch_optional(&mut *connection)
            .await?
    };
    let mut last = (String::new(), -1_i64, -1_i64, String::new(), String::new());
    let mut current_album: Option<String> = None;
    let mut source_hasher = album_loudness_hasher();
    let mut current_hasher = album_loudness_hasher();
    loop {
        if !database.scan_is_current(token) {
            return Err(LibraryError::ScanFailed);
        }
        let page = {
            let mut writer = database.writer().await?;
            let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
            sqlx::query_as::<_,StagedAlbumAudioKey>("SELECT track.album_object_id,track.disc_number,track.track_number,track.sort_text,track.object_id,track.source_loudness_analysis_key source_key,COALESCE((SELECT access.loudness_analysis_key FROM local_access_files access WHERE access.source_key=?6 AND access.track_object_id=track.object_id ORDER BY CASE access.origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END,access.local_access_file_key LIMIT 1),track.source_loudness_analysis_key) current_key FROM temp.scan_tracks track WHERE track.album_object_id IS NOT NULL AND (track.album_object_id,track.disc_number,track.track_number,track.sort_text,track.object_id)>(?1,?2,?3,?4,?5) ORDER BY track.album_object_id,track.disc_number,track.track_number,track.sort_text,track.object_id LIMIT ?7")
                .bind(&last.0).bind(last.1).bind(last.2).bind(&last.3).bind(&last.4)
                .bind(source_key).bind(DIGEST_PAGE_ROWS).fetch_all(&mut *connection).await?
        };
        let mut updates = Vec::new();
        if page.is_empty() {
            if let Some(album) = current_album.take() {
                updates.push((
                    album,
                    *source_hasher.finalize().as_bytes(),
                    *current_hasher.finalize().as_bytes(),
                ));
            }
            write_staged_album_loudness_keys(database, token, &updates).await?;
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
        let row = page.last().expect("nonempty staged audio page");
        last = (
            row.album_object_id.clone(),
            row.disc_number,
            row.track_number,
            row.sort_text.clone(),
            row.object_id.clone(),
        );
        write_staged_album_loudness_keys(database, token, &updates).await?;
    }
    Ok(())
}

fn album_loudness_hasher() -> Hasher {
    let mut hasher = Hasher::new();
    hasher.update(b"rufin-album-loudness-v1\0");
    hasher
}

async fn write_staged_album_loudness_keys(
    database: &Database,
    token: u64,
    updates: &[(String, [u8; 32], [u8; 32])],
) -> LibraryResult<()> {
    if updates.is_empty() {
        return Ok(());
    }
    if !database.scan_is_current(token) {
        return Err(LibraryError::ScanFailed);
    }
    let mut writer = database.writer().await?;
    let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
    let mut query = QueryBuilder::<Sqlite>::new("WITH next(object_id,source_key,current_key) AS (");
    query.push_values(updates, |mut row, (object_id, source, current)| {
        row.push_bind(object_id)
            .push_bind(source.as_slice())
            .push_bind(current.as_slice());
    });
    query.push(") UPDATE temp.scan_albums SET source_loudness_analysis_key=(SELECT source_key FROM next WHERE next.object_id=scan_albums.object_id),loudness_analysis_key=(SELECT current_key FROM next WHERE next.object_id=scan_albums.object_id) WHERE object_id IN (SELECT object_id FROM next)");
    query
        .build()
        .persistent(false)
        .execute(&mut *connection)
        .await?;
    Ok(())
}

const OBJECT_DIGEST_QUERIES: &[(&str, &str)] = &[
    (
        "albums",
        "SELECT * FROM temp.scan_albums
         WHERE object_id > ?1 ORDER BY object_id LIMIT ?2",
    ),
    (
        "tracks",
        "SELECT * FROM temp.scan_tracks
         WHERE object_id > ?1 ORDER BY object_id LIMIT ?2",
    ),
    (
        "artists",
        "SELECT * FROM temp.scan_artists
         WHERE object_id > ?1 ORDER BY object_id LIMIT ?2",
    ),
    (
        "genres",
        "SELECT * FROM temp.scan_genres
         WHERE object_id > ?1 ORDER BY object_id LIMIT ?2",
    ),
    (
        "moods",
        "SELECT * FROM temp.scan_moods
         WHERE object_id > ?1 ORDER BY object_id LIMIT ?2",
    ),
    (
        "folders",
        "SELECT * FROM temp.scan_folders
         WHERE object_id > ?1 ORDER BY object_id LIMIT ?2",
    ),
    (
        "playlists",
        "SELECT * FROM temp.scan_playlists
         WHERE object_id > ?1 ORDER BY object_id LIMIT ?2",
    ),
];

const RELATION_DIGEST_QUERIES: &[(&str, &str)] = &[
    (
        "album_artists",
        "SELECT * FROM temp.scan_album_artists
         WHERE (owner_id, position) > (?1, ?2)
         ORDER BY owner_id, position LIMIT ?3",
    ),
    (
        "track_artists",
        "SELECT * FROM temp.scan_track_artists
         WHERE (owner_id, position) > (?1, ?2)
         ORDER BY owner_id, position LIMIT ?3",
    ),
    (
        "album_genres",
        "SELECT * FROM temp.scan_album_genres
         WHERE (owner_id, position) > (?1, ?2)
         ORDER BY owner_id, position LIMIT ?3",
    ),
    (
        "track_genres",
        "SELECT * FROM temp.scan_track_genres
         WHERE (owner_id, position) > (?1, ?2)
         ORDER BY owner_id, position LIMIT ?3",
    ),
    (
        "track_moods",
        "SELECT * FROM temp.scan_track_moods
         WHERE (owner_id, position) > (?1, ?2)
         ORDER BY owner_id, position LIMIT ?3",
    ),
    (
        "track_folders",
        "SELECT * FROM temp.scan_track_folders
         WHERE (owner_id, position) > (?1, ?2)
         ORDER BY owner_id, position LIMIT ?3",
    ),
    (
        "album_release_types",
        "SELECT * FROM temp.scan_album_release_types
         WHERE (owner_id, position) > (?1, ?2)
         ORDER BY owner_id, position LIMIT ?3",
    ),
    (
        "home_entries",
        "SELECT * FROM temp.scan_home_entries
         WHERE (owner_id, position) > (?1, ?2)
         ORDER BY owner_id, position LIMIT ?3",
    ),
];

const PLAYLIST_ENTRY_DIGEST_QUERY: &str = "SELECT * FROM temp.scan_playlist_entries
     WHERE (playlist_id, position) > (?1, ?2)
     ORDER BY playlist_id, position LIMIT ?3";

const ARTWORK_DIGEST_QUERIES: &[(&str, &str)] = &[
    (
        "albums",
        "SELECT object_id, artwork_binding FROM temp.scan_albums
         WHERE object_id > ?1 ORDER BY object_id LIMIT ?2",
    ),
    (
        "tracks",
        "SELECT object_id, artwork_binding FROM temp.scan_tracks
         WHERE object_id > ?1 ORDER BY object_id LIMIT ?2",
    ),
    (
        "artists",
        "SELECT object_id, artwork_binding FROM temp.scan_artists
         WHERE object_id > ?1 ORDER BY object_id LIMIT ?2",
    ),
    (
        "genres",
        "SELECT object_id, artwork_binding FROM temp.scan_genres
         WHERE object_id > ?1 ORDER BY object_id LIMIT ?2",
    ),
    (
        "folders",
        "SELECT object_id, artwork_binding FROM temp.scan_folders
         WHERE object_id > ?1 ORDER BY object_id LIMIT ?2",
    ),
    (
        "playlists",
        "SELECT object_id, artwork_binding FROM temp.scan_playlists
         WHERE object_id > ?1 ORDER BY object_id LIMIT ?2",
    ),
];

const HOME_ARTWORK_DIGEST_QUERIES: &[(&str, &str)] = &[(
    "home_entries",
    "SELECT owner_id, position, artwork_binding FROM temp.scan_home_entries
     WHERE (owner_id, position) > (?1, ?2)
     ORDER BY owner_id, position LIMIT ?3",
)];

async fn canonical_catalog_digest(database: &Database, token: u64) -> LibraryResult<[u8; 32]> {
    let mut hasher = Hasher::new();
    hash_object_pages(database, token, &mut hasher, OBJECT_DIGEST_QUERIES).await?;
    hash_relation_pages(database, token, &mut hasher, RELATION_DIGEST_QUERIES).await?;
    hash_bytes(&mut hasher, b"playlist_entries");
    hash_playlist_entry_pages(database, token, &mut hasher).await?;
    Ok(*hasher.finalize().as_bytes())
}

async fn canonical_artwork_digest(database: &Database, token: u64) -> LibraryResult<[u8; 32]> {
    let mut hasher = Hasher::new();
    hash_object_pages(database, token, &mut hasher, ARTWORK_DIGEST_QUERIES).await?;
    hash_relation_pages(database, token, &mut hasher, HOME_ARTWORK_DIGEST_QUERIES).await?;
    Ok(*hasher.finalize().as_bytes())
}

async fn hash_object_pages(
    database: &Database,
    token: u64,
    hasher: &mut Hasher,
    queries: &'static [(&'static str, &'static str)],
) -> LibraryResult<()> {
    for (table, sql) in queries {
        hash_bytes(hasher, table.as_bytes());
        let mut last_object_id = String::new();
        loop {
            let rows = digest_page(
                database,
                token,
                sqlx::query(*sql)
                    .bind(&last_object_id)
                    .bind(DIGEST_PAGE_ROWS),
            )
            .await?;
            for row in &rows {
                hash_row(hasher, row)?;
            }
            let Some(row) = rows.last() else {
                break;
            };
            last_object_id = row.try_get("object_id")?;
            if rows.len() < DIGEST_PAGE_ROWS as usize {
                break;
            }
        }
    }
    Ok(())
}

async fn hash_relation_pages(
    database: &Database,
    token: u64,
    hasher: &mut Hasher,
    queries: &'static [(&'static str, &'static str)],
) -> LibraryResult<()> {
    for (table, sql) in queries {
        hash_bytes(hasher, table.as_bytes());
        let mut last_owner_id = String::new();
        let mut last_position = -1_i64;
        loop {
            let rows = digest_page(
                database,
                token,
                sqlx::query(*sql)
                    .bind(&last_owner_id)
                    .bind(last_position)
                    .bind(DIGEST_PAGE_ROWS),
            )
            .await?;
            for row in &rows {
                hash_row(hasher, row)?;
            }
            let Some(row) = rows.last() else {
                break;
            };
            last_owner_id = row.try_get("owner_id")?;
            last_position = row.try_get("position")?;
            if rows.len() < DIGEST_PAGE_ROWS as usize {
                break;
            }
        }
    }
    Ok(())
}

async fn hash_playlist_entry_pages(
    database: &Database,
    token: u64,
    hasher: &mut Hasher,
) -> LibraryResult<()> {
    let mut last_playlist_id = String::new();
    let mut last_position = -1_i64;
    loop {
        let rows = digest_page(
            database,
            token,
            sqlx::query(PLAYLIST_ENTRY_DIGEST_QUERY)
                .bind(&last_playlist_id)
                .bind(last_position)
                .bind(DIGEST_PAGE_ROWS),
        )
        .await?;
        for row in &rows {
            hash_row(hasher, row)?;
        }
        let Some(row) = rows.last() else {
            break;
        };
        last_playlist_id = row.try_get("playlist_id")?;
        last_position = row.try_get("position")?;
        if rows.len() < DIGEST_PAGE_ROWS as usize {
            break;
        }
    }
    Ok(())
}

async fn digest_page<'query>(
    database: &Database,
    token: u64,
    query: Query<'query, Sqlite, SqliteArguments>,
) -> LibraryResult<Vec<SqliteRow>> {
    if !database.scan_is_current(token) {
        return Err(LibraryError::ScanFailed);
    }
    let mut writer = database.writer().await?;
    let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
    let rows = query.persistent(false).fetch_all(&mut *connection).await;
    if matches!(&rows, Err(sqlx::Error::WorkerCrashed | sqlx::Error::Io(_))) {
        *writer = None;
        database.writer_failed();
    }
    Ok(rows?)
}

fn hash_row(hasher: &mut Hasher, row: &SqliteRow) -> LibraryResult<()> {
    hasher.update(&[0xff]);
    for index in 0..row.columns().len() {
        let value = row.try_get_raw(index)?;
        if value.is_null() {
            hasher.update(&[0]);
            continue;
        }
        match value.type_info().name() {
            "INTEGER" => {
                hasher.update(&[1]);
                hasher.update(&row.try_get::<i64, _>(index)?.to_be_bytes());
            }
            "REAL" => {
                hasher.update(&[2]);
                hasher.update(&row.try_get::<f64, _>(index)?.to_bits().to_be_bytes());
            }
            "TEXT" => {
                hasher.update(&[3]);
                hash_bytes(hasher, row.try_get::<String, _>(index)?.as_bytes());
            }
            "BLOB" => {
                hasher.update(&[4]);
                hash_bytes(hasher, &row.try_get::<Vec<u8>, _>(index)?);
            }
            kind => {
                return Err(LibraryError::InvalidStore(format!(
                    "unsupported canonical SQLite value {kind}"
                )));
            }
        }
    }
    Ok(())
}

fn hash_bytes(hasher: &mut Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value);
}

async fn validate_references(transaction: &mut Transaction<'_, Sqlite>) -> LibraryResult<()> {
    let checks = [
        "SELECT 1 FROM temp.scan_tracks AS child
         WHERE child.album_object_id IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM temp.scan_albums WHERE object_id = child.album_object_id)",
        "SELECT 1 FROM temp.scan_playlist_entries AS child
         WHERE NOT EXISTS (SELECT 1 FROM temp.scan_playlists WHERE object_id = child.playlist_id)",
        "SELECT 1 FROM temp.scan_album_artists AS link
         WHERE NOT EXISTS (SELECT 1 FROM temp.scan_albums WHERE object_id = link.owner_id)
            OR NOT EXISTS (SELECT 1 FROM temp.scan_artists WHERE object_id = link.related_id)",
        "SELECT 1 FROM temp.scan_track_artists AS link
         WHERE NOT EXISTS (SELECT 1 FROM temp.scan_tracks WHERE object_id = link.owner_id)
            OR NOT EXISTS (SELECT 1 FROM temp.scan_artists WHERE object_id = link.related_id)",
        "SELECT 1 FROM temp.scan_album_genres AS link
         WHERE NOT EXISTS (SELECT 1 FROM temp.scan_albums WHERE object_id = link.owner_id)
            OR NOT EXISTS (SELECT 1 FROM temp.scan_genres WHERE object_id = link.related_id)",
        "SELECT 1 FROM temp.scan_track_genres AS link
         WHERE NOT EXISTS (SELECT 1 FROM temp.scan_tracks WHERE object_id = link.owner_id)
            OR NOT EXISTS (SELECT 1 FROM temp.scan_genres WHERE object_id = link.related_id)",
        "SELECT 1 FROM temp.scan_track_moods AS link
         WHERE NOT EXISTS (SELECT 1 FROM temp.scan_tracks WHERE object_id = link.owner_id)
            OR NOT EXISTS (SELECT 1 FROM temp.scan_moods WHERE object_id = link.related_id)",
        "SELECT 1 FROM temp.scan_track_folders AS link
         WHERE NOT EXISTS (SELECT 1 FROM temp.scan_tracks WHERE object_id = link.owner_id)
            OR NOT EXISTS (SELECT 1 FROM temp.scan_folders WHERE object_id = link.related_id)",
        "SELECT 1 FROM temp.scan_album_release_types AS link
         WHERE NOT EXISTS (SELECT 1 FROM temp.scan_albums WHERE object_id = link.owner_id)",
        "SELECT 1 FROM temp.scan_home_entries AS entry WHERE
           (entry.entity_kind='track' AND NOT EXISTS (
              SELECT 1 FROM temp.scan_tracks WHERE object_id=entry.entity_object_id))
           OR (entry.entity_kind='album' AND NOT EXISTS (
              SELECT 1 FROM temp.scan_albums WHERE object_id=entry.entity_object_id))
           OR (entry.entity_kind='artist' AND NOT EXISTS (
              SELECT 1 FROM temp.scan_artists WHERE object_id=entry.entity_object_id))
           OR (entry.entity_kind='playlist' AND NOT EXISTS (
              SELECT 1 FROM temp.scan_playlists WHERE object_id=entry.entity_object_id))",
    ];
    for sql in checks {
        if sqlx::query_scalar::<_, i64>(sql)
            .fetch_optional(&mut **transaction)
            .await?
            .is_some()
        {
            return Err(LibraryError::InvalidScan(
                "scan contains a relationship to a missing staged object".to_string(),
            ));
        }
    }
    Ok(())
}

async fn symmetric_point_change(
    transaction: &mut Transaction<'_, Sqlite>,
    source_key: i64,
    staged: &'static str,
    current: &'static str,
) -> LibraryResult<bool> {
    let sql = format!(
        "SELECT EXISTS(
           SELECT * FROM (SELECT * FROM ({staged}) EXCEPT SELECT * FROM ({current}))
           UNION ALL
           SELECT * FROM (SELECT * FROM ({current}) EXCEPT SELECT * FROM ({staged}))
         )"
    );
    Ok(sqlx::query_scalar::<_, bool>(sqlx::AssertSqlSafe(sql))
        .bind(source_key)
        .fetch_one(&mut **transaction)
        .await?)
}

async fn staged_playlists_changed(
    transaction: &mut Transaction<'_, Sqlite>,
    source_key: i64,
) -> LibraryResult<bool> {
    for (staged, current) in [
        (
            "SELECT object_id,name,normalized_name,sort_text,artwork_binding FROM temp.scan_playlists",
            "SELECT object_id,name,normalized_name,sort_text,artwork_binding FROM playlists WHERE source_key=?1 AND ownership='source' AND (object_id IN (SELECT object_id FROM temp.scan_playlists) OR object_id IN (SELECT object_id FROM temp.scan_removals WHERE entity_kind='playlist'))",
        ),
        (
            "SELECT playlist_id,object_id,track_id,position FROM temp.scan_playlist_entries",
            "SELECT playlist.object_id,entry.object_id,entry.track_object_id,entry.position FROM playlist_entries entry JOIN playlists playlist USING(playlist_key) WHERE playlist.source_key=?1 AND playlist.ownership='source' AND (playlist.object_id IN (SELECT object_id FROM temp.scan_playlists) OR playlist.object_id IN (SELECT object_id FROM temp.scan_removals WHERE entity_kind='playlist'))",
        ),
    ] {
        if symmetric_point_change(transaction, source_key, staged, current).await? {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn staged_non_playlist_catalog_changed(
    transaction: &mut Transaction<'_, Sqlite>,
    source_key: i64,
) -> LibraryResult<bool> {
    let entities = [
        (
            "SELECT staged.object_id,staged.title,staged.normalized_title,staged.display_artist,staged.sort_text,staged.year,staged.release_date,staged.date_added,staged.musicbrainz_release_id,staged.musicbrainz_release_group_id,staged.is_compilation,staged.artwork_binding,staged.favorite,staged.rating,COALESCE(current.first_seen_at,staged.first_seen_at),staged.source_loudness_analysis_key FROM temp.scan_albums staged LEFT JOIN albums current ON current.source_key=?1 AND current.object_id=staged.object_id",
            "SELECT object_id,title,normalized_title,display_artist,sort_text,year,release_date,date_added,musicbrainz_release_id,musicbrainz_release_group_id,is_compilation,artwork_binding,source_favorite,source_rating,first_seen_at,source_loudness_analysis_key FROM albums WHERE source_key=?1 AND (object_id IN (SELECT object_id FROM temp.scan_albums) OR object_id IN (SELECT object_id FROM temp.scan_removals WHERE entity_kind='album'))",
        ),
        (
            "SELECT staged.object_id,staged.album_object_id,staged.title,staged.normalized_search,staged.display_album,staged.display_artist,staged.sort_text,staged.duration_millis,staged.disc_number,staged.track_number,staged.year,staged.release_date,staged.date_added,staged.media_uri,staged.source_path,staged.source_format,staged.comment,staged.bpm,staged.musicbrainz_recording_id,staged.musicbrainz_release_track_id,staged.cue_path,staged.cue_start_millis,staged.cue_end_millis,staged.artwork_binding,staged.favorite,staged.rating,COALESCE(current.first_seen_at,staged.first_seen_at),staged.source_loudness_analysis_key FROM temp.scan_tracks staged LEFT JOIN tracks current ON current.source_key=?1 AND current.object_id=staged.object_id",
            "SELECT track.object_id,album.object_id,track.title,track.normalized_search,track.display_album,track.display_artist,track.sort_text,track.duration_millis,track.disc_number,track.track_number,track.year,track.release_date,track.date_added,track.media_uri,track.source_path,track.source_format,track.comment,track.bpm,track.musicbrainz_recording_id,track.musicbrainz_release_track_id,track.cue_path,track.cue_start_millis,track.cue_end_millis,track.artwork_binding,track.source_favorite,track.source_rating,track.first_seen_at,track.source_loudness_analysis_key FROM tracks track LEFT JOIN albums album USING(album_key) WHERE track.source_key=?1 AND (track.object_id IN (SELECT object_id FROM temp.scan_tracks) OR track.object_id IN (SELECT object_id FROM temp.scan_removals WHERE entity_kind='track'))",
        ),
        (
            "SELECT object_id,name,normalized_name,sort_text,musicbrainz_artist_id,artwork_binding,favorite,rating FROM temp.scan_artists",
            "SELECT object_id,name,normalized_name,sort_text,musicbrainz_artist_id,artwork_binding,source_favorite,source_rating FROM artists WHERE source_key=?1 AND (object_id IN (SELECT object_id FROM temp.scan_artists) OR object_id IN (SELECT object_id FROM temp.scan_removals WHERE entity_kind='artist'))",
        ),
        (
            "SELECT object_id,name,normalized_name,sort_text,artwork_binding FROM temp.scan_genres",
            "SELECT object_id,name,normalized_name,sort_text,artwork_binding FROM genres WHERE source_key=?1 AND (object_id IN (SELECT object_id FROM temp.scan_genres) OR object_id IN (SELECT object_id FROM temp.scan_removals WHERE entity_kind='genre'))",
        ),
        (
            "SELECT object_id,name,normalized_name,sort_text FROM temp.scan_moods",
            "SELECT object_id,name,normalized_name,sort_text FROM moods WHERE source_key=?1 AND object_id IN (SELECT object_id FROM temp.scan_moods)",
        ),
        (
            "SELECT object_id,name,normalized_name,sort_text,artwork_binding FROM temp.scan_folders",
            "SELECT object_id,name,normalized_name,sort_text,artwork_binding FROM folders WHERE source_key=?1 AND object_id IN (SELECT object_id FROM temp.scan_folders)",
        ),
    ];
    for (staged, current) in entities {
        if symmetric_point_change(transaction, source_key, staged, current).await? {
            return Ok(true);
        }
    }

    let relations = [
        (
            "SELECT owner_id,related_id,position FROM temp.scan_album_artists",
            "SELECT album.object_id,artist.object_id,link.position FROM album_artists link JOIN albums album USING(album_key) JOIN artists artist USING(artist_key) WHERE album.source_key=?1 AND (album.object_id IN (SELECT object_id FROM temp.scan_albums) OR album.object_id IN (SELECT object_id FROM temp.scan_removals WHERE entity_kind='album'))",
        ),
        (
            "SELECT owner_id,related_id,position FROM temp.scan_track_artists",
            "SELECT track.object_id,artist.object_id,link.position FROM track_artists link JOIN tracks track USING(track_key) JOIN artists artist USING(artist_key) WHERE track.source_key=?1 AND (track.object_id IN (SELECT object_id FROM temp.scan_tracks) OR track.object_id IN (SELECT object_id FROM temp.scan_removals WHERE entity_kind='track'))",
        ),
        (
            "SELECT owner_id,related_id,position FROM temp.scan_album_genres",
            "SELECT album.object_id,genre.object_id,link.position FROM album_genres link JOIN albums album USING(album_key) JOIN genres genre USING(genre_key) WHERE album.source_key=?1 AND (album.object_id IN (SELECT object_id FROM temp.scan_albums) OR album.object_id IN (SELECT object_id FROM temp.scan_removals WHERE entity_kind='album'))",
        ),
        (
            "SELECT owner_id,related_id,position FROM temp.scan_track_genres",
            "SELECT track.object_id,genre.object_id,link.position FROM track_genres link JOIN tracks track USING(track_key) JOIN genres genre USING(genre_key) WHERE track.source_key=?1 AND (track.object_id IN (SELECT object_id FROM temp.scan_tracks) OR track.object_id IN (SELECT object_id FROM temp.scan_removals WHERE entity_kind='track'))",
        ),
        (
            "SELECT owner_id,related_id,position FROM temp.scan_track_moods",
            "SELECT track.object_id,mood.object_id,link.position FROM track_moods link JOIN tracks track USING(track_key) JOIN moods mood USING(mood_key) WHERE track.source_key=?1 AND (track.object_id IN (SELECT object_id FROM temp.scan_tracks) OR track.object_id IN (SELECT object_id FROM temp.scan_removals WHERE entity_kind='track'))",
        ),
        (
            "SELECT owner_id,related_id,position FROM temp.scan_track_folders",
            "SELECT track.object_id,folder.object_id,link.position FROM track_folders link JOIN tracks track USING(track_key) JOIN folders folder USING(folder_key) WHERE track.source_key=?1 AND (track.object_id IN (SELECT object_id FROM temp.scan_tracks) OR track.object_id IN (SELECT object_id FROM temp.scan_removals WHERE entity_kind='track'))",
        ),
        (
            "SELECT owner_id,related_id,position FROM temp.scan_album_release_types",
            "SELECT album.object_id,link.release_type,link.position FROM album_release_types link JOIN albums album USING(album_key) WHERE album.source_key=?1 AND (album.object_id IN (SELECT object_id FROM temp.scan_albums) OR album.object_id IN (SELECT object_id FROM temp.scan_removals WHERE entity_kind='album'))",
        ),
    ];
    for (staged, current) in relations {
        if symmetric_point_change(transaction, source_key, staged, current).await? {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn publish_entities(
    transaction: &mut Transaction<'_, Sqlite>,
    source_key: i64,
    full: bool,
) -> LibraryResult<()> {
    sqlx::query(
        "INSERT INTO albums(
             source_key, object_id, title, normalized_title, display_artist,
             sort_text, year, release_date, date_added,
             musicbrainz_release_id, musicbrainz_release_group_id,
             is_compilation,artwork_binding, source_favorite, source_rating, first_seen_at,
             source_loudness_analysis_key,loudness_analysis_key
         ) SELECT ?1, object_id, title, normalized_title, display_artist,
                  sort_text, year, release_date, date_added,
                  musicbrainz_release_id, musicbrainz_release_group_id,
                  is_compilation,artwork_binding, favorite, rating, first_seen_at,
                  source_loudness_analysis_key,loudness_analysis_key
           FROM temp.scan_albums WHERE true
         ON CONFLICT(source_key, object_id) DO UPDATE SET
             title = excluded.title, normalized_title = excluded.normalized_title,
             display_artist = excluded.display_artist, sort_text = excluded.sort_text,
             year = excluded.year, release_date = excluded.release_date,
             date_added = excluded.date_added,
             musicbrainz_release_id = excluded.musicbrainz_release_id,
             musicbrainz_release_group_id = excluded.musicbrainz_release_group_id,
             is_compilation=excluded.is_compilation,
             artwork_binding = excluded.artwork_binding,
             source_favorite = excluded.source_favorite,
             source_rating = excluded.source_rating,
             first_seen_at = COALESCE(albums.first_seen_at, excluded.first_seen_at),
             source_loudness_analysis_key=excluded.source_loudness_analysis_key,
             loudness_analysis_key=excluded.loudness_analysis_key",
    )
    .bind(source_key)
    .execute(&mut **transaction)
    .await?;
    for sql in [
        "INSERT INTO artists(
             source_key, object_id, name, normalized_name, sort_text,
             musicbrainz_artist_id, artwork_binding, source_favorite, source_rating
         ) SELECT ?1, object_id, name, normalized_name, sort_text,
                  musicbrainz_artist_id, artwork_binding, favorite, rating
           FROM temp.scan_artists WHERE true
         ON CONFLICT(source_key, object_id) DO UPDATE SET
             name=excluded.name, normalized_name=excluded.normalized_name,
             sort_text=excluded.sort_text,
             musicbrainz_artist_id=excluded.musicbrainz_artist_id,
             artwork_binding=excluded.artwork_binding,
             source_favorite=excluded.source_favorite, source_rating=excluded.source_rating",
        "INSERT INTO genres(
             source_key, object_id, name, normalized_name, sort_text, artwork_binding
         ) SELECT ?1, object_id, name, normalized_name, sort_text, artwork_binding
           FROM temp.scan_genres WHERE true
         ON CONFLICT(source_key, object_id) DO UPDATE SET
             name=excluded.name, normalized_name=excluded.normalized_name,
             sort_text=excluded.sort_text, artwork_binding=excluded.artwork_binding",
        "INSERT INTO moods(source_key, object_id, name, normalized_name, sort_text)
         SELECT ?1, object_id, name, normalized_name, sort_text FROM temp.scan_moods WHERE true
         ON CONFLICT(source_key, object_id) DO UPDATE SET
             name=excluded.name, normalized_name=excluded.normalized_name,
             sort_text=excluded.sort_text",
        "INSERT INTO folders(
             source_key, object_id, name, normalized_name, sort_text, artwork_binding
         ) SELECT ?1, object_id, name, normalized_name, sort_text, artwork_binding
           FROM temp.scan_folders WHERE true
         ON CONFLICT(source_key, object_id) DO UPDATE SET
             name=excluded.name, normalized_name=excluded.normalized_name,
             sort_text=excluded.sort_text, artwork_binding=excluded.artwork_binding",
        "INSERT INTO playlists(
             source_key, ownership, object_id, name, normalized_name, sort_text, artwork_binding
         ) SELECT ?1, 'source', object_id, name, normalized_name, sort_text, artwork_binding
           FROM temp.scan_playlists WHERE true
         ON CONFLICT(source_key, ownership, object_id) DO UPDATE SET
             name=excluded.name, normalized_name=excluded.normalized_name,
             sort_text=excluded.sort_text, artwork_binding=excluded.artwork_binding",
    ] {
        sqlx::query(sql)
            .bind(source_key)
            .execute(&mut **transaction)
            .await?;
    }
    sqlx::query(
        "INSERT INTO tracks(
             source_key, object_id, album_key, title, normalized_search,
             display_album, display_artist, sort_text, duration_millis,
             disc_number, track_number, year, release_date, date_added, media_uri, source_path,
             source_format, comment, bpm, musicbrainz_recording_id,
             musicbrainz_release_track_id, cue_path, cue_start_millis, cue_end_millis,
             artwork_binding, source_favorite, source_rating, first_seen_at,
             source_loudness_analysis_key,loudness_analysis_key
         ) SELECT ?1, item.object_id, album.album_key, item.title,
                  item.normalized_search, item.display_album, item.display_artist,
                  item.sort_text, item.duration_millis, item.disc_number,
                  item.track_number, item.year, item.release_date, item.date_added,
                  item.media_uri, item.source_path, item.source_format, item.comment, item.bpm,
                  item.musicbrainz_recording_id, item.musicbrainz_release_track_id,
                  item.cue_path, item.cue_start_millis, item.cue_end_millis,
                  item.artwork_binding, item.favorite, item.rating, item.first_seen_at,
                  item.source_loudness_analysis_key,
                  COALESCE((SELECT access.loudness_analysis_key
                            FROM local_access_files AS access
                            WHERE access.source_key=?1 AND access.track_object_id=item.object_id
                            ORDER BY CASE access.origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END,
                                     access.local_access_file_key LIMIT 1),
                           item.source_loudness_analysis_key)
           FROM temp.scan_tracks AS item
           LEFT JOIN albums AS album
             ON album.source_key = ?1 AND album.object_id = item.album_object_id
           WHERE true
         ON CONFLICT(source_key, object_id) DO UPDATE SET
             album_key=excluded.album_key, title=excluded.title,
             normalized_search=excluded.normalized_search,
             display_album=excluded.display_album,
             display_artist=excluded.display_artist, sort_text=excluded.sort_text,
             duration_millis=excluded.duration_millis,
             disc_number=excluded.disc_number, track_number=excluded.track_number,
             year=excluded.year, release_date=excluded.release_date,
             date_added=excluded.date_added, media_uri=excluded.media_uri, source_path=excluded.source_path,
             source_format=excluded.source_format, comment=excluded.comment,
             bpm=excluded.bpm, musicbrainz_recording_id=excluded.musicbrainz_recording_id,
             musicbrainz_release_track_id=excluded.musicbrainz_release_track_id,
             cue_path=excluded.cue_path, cue_start_millis=excluded.cue_start_millis,
             cue_end_millis=excluded.cue_end_millis,
             artwork_binding=excluded.artwork_binding,
             source_favorite=excluded.source_favorite, source_rating=excluded.source_rating,
             first_seen_at=COALESCE(tracks.first_seen_at, excluded.first_seen_at),
             source_loudness_analysis_key=excluded.source_loudness_analysis_key,
             loudness_analysis_key=excluded.loudness_analysis_key",
    )
    .bind(source_key)
    .execute(&mut **transaction)
    .await?;
    if full {
        sqlx::query("DELETE FROM home_entries WHERE source_key=?1")
            .bind(source_key)
            .execute(&mut **transaction)
            .await?;
        sqlx::query(
            "INSERT INTO home_entries(
             source_key, section_id, position, entity_kind, entity_key,
             title, subtitle, artwork_binding
         ) SELECT ?1, entry.owner_id, entry.position, entry.entity_kind,
                  CASE entry.entity_kind
                    WHEN 'track' THEN (SELECT track_key FROM tracks
                      WHERE source_key=?1 AND object_id=entry.entity_object_id)
                    WHEN 'album' THEN (SELECT album_key FROM albums
                      WHERE source_key=?1 AND object_id=entry.entity_object_id)
                    WHEN 'artist' THEN (SELECT artist_key FROM artists
                      WHERE source_key=?1 AND object_id=entry.entity_object_id)
                    WHEN 'playlist' THEN (SELECT playlist_key FROM playlists
                      WHERE source_key=?1 AND ownership='source'
                        AND object_id=entry.entity_object_id)
                  END,
                  entry.title, entry.subtitle, entry.artwork_binding
           FROM temp.scan_home_entries AS entry",
        )
        .bind(source_key)
        .execute(&mut **transaction)
        .await?;
    }
    if !full {
        return Ok(());
    }
    for sql in [
        "DELETE FROM tracks WHERE source_key=?1 AND NOT EXISTS (
             SELECT 1 FROM temp.scan_tracks WHERE scan_tracks.object_id=tracks.object_id
         )",
        "DELETE FROM albums WHERE source_key=?1 AND NOT EXISTS (
             SELECT 1 FROM temp.scan_albums WHERE scan_albums.object_id=albums.object_id
         )",
        "DELETE FROM artists WHERE source_key=?1 AND NOT EXISTS (
             SELECT 1 FROM temp.scan_artists WHERE scan_artists.object_id=artists.object_id
         )",
        "DELETE FROM genres WHERE source_key=?1 AND NOT EXISTS (
             SELECT 1 FROM temp.scan_genres WHERE scan_genres.object_id=genres.object_id
         )",
        "DELETE FROM moods WHERE source_key=?1 AND NOT EXISTS (
             SELECT 1 FROM temp.scan_moods WHERE scan_moods.object_id=moods.object_id
         )",
        "DELETE FROM folders WHERE source_key=?1 AND NOT EXISTS (
             SELECT 1 FROM temp.scan_folders WHERE scan_folders.object_id=folders.object_id
         )",
    ] {
        sqlx::query(sql)
            .bind(source_key)
            .execute(&mut **transaction)
            .await?;
    }
    sqlx::query(
        "DELETE FROM playlists
         WHERE source_key=?1 AND ownership='source'
           AND NOT EXISTS (
               SELECT 1 FROM temp.scan_playlists
               WHERE scan_playlists.object_id=playlists.object_id
           )",
    )
    .bind(source_key)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn publish_source_loudness(
    transaction: &mut Transaction<'_, Sqlite>,
    source_key: i64,
    full: bool,
) -> LibraryResult<()> {
    let track_delete = if full {
        "DELETE FROM loudness_measurements WHERE source_key=?1 AND entity_kind='track' AND origin='source' AND NOT EXISTS (SELECT 1 FROM temp.scan_tracks staged JOIN tracks track ON track.source_key=?1 AND track.object_id=staged.object_id AND track.source_loudness_analysis_key=track.loudness_analysis_key WHERE staged.source_integrated_lufs IS NOT NULL AND track.track_key=loudness_measurements.entity_key)"
    } else {
        "DELETE FROM loudness_measurements WHERE source_key=?1 AND entity_kind='track' AND origin='source' AND entity_key IN (SELECT track.track_key FROM tracks track JOIN temp.scan_tracks staged ON staged.object_id=track.object_id WHERE track.source_key=?1 AND staged.source_integrated_lufs IS NULL)"
    };
    let album_delete = if full {
        "DELETE FROM loudness_measurements WHERE source_key=?1 AND entity_kind='album' AND origin='source' AND NOT EXISTS (SELECT 1 FROM temp.scan_albums staged JOIN albums album ON album.source_key=?1 AND album.object_id=staged.object_id AND album.source_loudness_analysis_key=album.loudness_analysis_key WHERE staged.source_integrated_lufs IS NOT NULL AND album.album_key=loudness_measurements.entity_key)"
    } else {
        "DELETE FROM loudness_measurements WHERE source_key=?1 AND entity_kind='album' AND origin='source' AND entity_key IN (SELECT album.album_key FROM albums album JOIN temp.scan_albums staged ON staged.object_id=album.object_id WHERE album.source_key=?1 AND staged.source_integrated_lufs IS NULL)"
    };
    for sql in [
        track_delete,
        album_delete,
        "INSERT INTO loudness_measurements(source_key,entity_kind,entity_key,analysis_key,integrated_lufs,true_peak,origin) SELECT ?1,'track',track.track_key,track.loudness_analysis_key,staged.source_integrated_lufs,staged.source_true_peak,'source' FROM temp.scan_tracks staged JOIN tracks track ON track.source_key=?1 AND track.object_id=staged.object_id AND track.source_loudness_analysis_key=track.loudness_analysis_key WHERE staged.source_integrated_lufs IS NOT NULL ON CONFLICT(source_key,entity_kind,entity_key) DO UPDATE SET analysis_key=excluded.analysis_key,integrated_lufs=excluded.integrated_lufs,true_peak=excluded.true_peak,origin='source'",
        "INSERT INTO loudness_measurements(source_key,entity_kind,entity_key,analysis_key,integrated_lufs,true_peak,origin) SELECT ?1,'album',album.album_key,album.loudness_analysis_key,staged.source_integrated_lufs,staged.source_true_peak,'source' FROM temp.scan_albums staged JOIN albums album ON album.source_key=?1 AND album.object_id=staged.object_id AND album.source_loudness_analysis_key=album.loudness_analysis_key WHERE staged.source_integrated_lufs IS NOT NULL ON CONFLICT(source_key,entity_kind,entity_key) DO UPDATE SET analysis_key=excluded.analysis_key,integrated_lufs=excluded.integrated_lufs,true_peak=excluded.true_peak,origin='source'",
    ] {
        sqlx::query(sql)
            .bind(source_key)
            .execute(&mut **transaction)
            .await?;
    }
    let replay_track_delete = if full {
        "DELETE FROM replay_gain_measurements WHERE source_key=?1 AND entity_kind='track' AND NOT EXISTS (SELECT 1 FROM temp.scan_tracks staged JOIN tracks track ON track.source_key=?1 AND track.object_id=staged.object_id AND track.source_loudness_analysis_key=track.loudness_analysis_key WHERE staged.source_replay_gain_db IS NOT NULL AND track.track_key=replay_gain_measurements.entity_key)"
    } else {
        "DELETE FROM replay_gain_measurements WHERE source_key=?1 AND entity_kind='track' AND entity_key IN (SELECT track.track_key FROM tracks track JOIN temp.scan_tracks staged ON staged.object_id=track.object_id WHERE track.source_key=?1 AND staged.source_replay_gain_db IS NULL)"
    };
    let replay_album_delete = if full {
        "DELETE FROM replay_gain_measurements WHERE source_key=?1 AND entity_kind='album' AND NOT EXISTS (SELECT 1 FROM temp.scan_albums staged JOIN albums album ON album.source_key=?1 AND album.object_id=staged.object_id AND album.source_loudness_analysis_key=album.loudness_analysis_key WHERE staged.source_replay_gain_db IS NOT NULL AND album.album_key=replay_gain_measurements.entity_key)"
    } else {
        "DELETE FROM replay_gain_measurements WHERE source_key=?1 AND entity_kind='album' AND entity_key IN (SELECT album.album_key FROM albums album JOIN temp.scan_albums staged ON staged.object_id=album.object_id WHERE album.source_key=?1 AND staged.source_replay_gain_db IS NULL)"
    };
    for sql in [
        replay_track_delete,
        replay_album_delete,
        "INSERT INTO replay_gain_measurements(source_key,entity_kind,entity_key,analysis_key,gain_db,peak) SELECT ?1,'track',track.track_key,track.loudness_analysis_key,staged.source_replay_gain_db,staged.source_replay_gain_peak FROM temp.scan_tracks staged JOIN tracks track ON track.source_key=?1 AND track.object_id=staged.object_id AND track.source_loudness_analysis_key=track.loudness_analysis_key WHERE staged.source_replay_gain_db IS NOT NULL ON CONFLICT(source_key,entity_kind,entity_key) DO UPDATE SET analysis_key=excluded.analysis_key,gain_db=excluded.gain_db,peak=excluded.peak",
        "INSERT INTO replay_gain_measurements(source_key,entity_kind,entity_key,analysis_key,gain_db,peak) SELECT ?1,'album',album.album_key,album.loudness_analysis_key,staged.source_replay_gain_db,staged.source_replay_gain_peak FROM temp.scan_albums staged JOIN albums album ON album.source_key=?1 AND album.object_id=staged.object_id AND album.source_loudness_analysis_key=album.loudness_analysis_key WHERE staged.source_replay_gain_db IS NOT NULL ON CONFLICT(source_key,entity_kind,entity_key) DO UPDATE SET analysis_key=excluded.analysis_key,gain_db=excluded.gain_db,peak=excluded.peak",
    ] {
        sqlx::query(sql)
            .bind(source_key)
            .execute(&mut **transaction)
            .await?;
    }
    Ok(())
}

async fn publish_activity_baseline(
    transaction: &mut Transaction<'_, Sqlite>,
    source_key: i64,
) -> LibraryResult<()> {
    sqlx::query(
        "INSERT INTO activity_baseline(
             source_key,track_object_id,play_count,skip_count,last_played_at
         )
         SELECT ?1,staged.object_id,COALESCE(staged.baseline_play_count,0),
                COALESCE(staged.baseline_skip_count,0),staged.baseline_last_played
         FROM temp.scan_tracks staged
         WHERE (staged.baseline_play_count IS NOT NULL
             OR staged.baseline_skip_count IS NOT NULL
             OR staged.baseline_last_played IS NOT NULL)
           AND NOT EXISTS (
               SELECT 1 FROM activity_baseline current
               WHERE current.source_key=?1
                 AND current.period='lifetime' AND current.item_kind='track'
                 AND current.track_object_id=staged.object_id
           )",
    )
    .bind(source_key)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn publish_removals(
    transaction: &mut Transaction<'_, Sqlite>,
    source_key: i64,
) -> LibraryResult<()> {
    for (kind, table, staged, ownership) in [
        ("track", "tracks", "scan_tracks", ""),
        ("album", "albums", "scan_albums", ""),
        ("artist", "artists", "scan_artists", ""),
        ("genre", "genres", "scan_genres", ""),
        (
            "playlist",
            "playlists",
            "scan_playlists",
            " AND ownership='source'",
        ),
    ] {
        let sql = format!(
            "DELETE FROM {table} WHERE source_key=?1{ownership} AND object_id IN (
                 SELECT object_id FROM temp.scan_removals WHERE entity_kind=?2
             ) AND NOT EXISTS (SELECT 1 FROM temp.{staged} staged WHERE staged.object_id={table}.object_id)"
        );
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(source_key)
            .bind(kind)
            .execute(&mut **transaction)
            .await?;
    }
    Ok(())
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
             device_id,inode,parse_version,state
         )
         SELECT ?1,path,root,relative_path,kind,size_bytes,mtime_ns,
                device_id,inode,parse_version,state
         FROM temp.scan_local_files WHERE true
         ON CONFLICT(source_key,path) DO UPDATE SET
             root=excluded.root,relative_path=excluded.relative_path,
             kind=excluded.kind,size_bytes=excluded.size_bytes,
             mtime_ns=excluded.mtime_ns,device_id=excluded.device_id,
             inode=excluded.inode,parse_version=excluded.parse_version,
             state=excluded.state",
    )
    .bind(source_key)
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

async fn publish_artwork_invalidations(
    transaction: &mut Transaction<'_, Sqlite>,
    source_key: i64,
) -> LibraryResult<u64> {
    Ok(sqlx::query("UPDATE albums SET artwork_binding=NULL WHERE source_key=?1 AND object_id IN (SELECT album_object_id FROM temp.scan_artwork_invalidations) AND artwork_binding IS NOT NULL").bind(source_key).execute(&mut **transaction).await?.rows_affected())
}

async fn staged_artwork_changed(
    transaction: &mut Transaction<'_, Sqlite>,
    source_key: i64,
) -> LibraryResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(
           SELECT 1 FROM temp.scan_tracks staged
           LEFT JOIN tracks current ON current.source_key=?1 AND current.object_id=staged.object_id
           WHERE staged.artwork_binding IS NOT current.artwork_binding
           UNION ALL
           SELECT 1 FROM temp.scan_albums staged
           LEFT JOIN albums current ON current.source_key=?1 AND current.object_id=staged.object_id
           WHERE staged.artwork_binding IS NOT current.artwork_binding
           UNION ALL
           SELECT 1 FROM temp.scan_artists staged
           LEFT JOIN artists current ON current.source_key=?1 AND current.object_id=staged.object_id
           WHERE staged.artwork_binding IS NOT current.artwork_binding
           UNION ALL
           SELECT 1 FROM temp.scan_genres staged
           LEFT JOIN genres current ON current.source_key=?1 AND current.object_id=staged.object_id
           WHERE staged.artwork_binding IS NOT current.artwork_binding
           UNION ALL
           SELECT 1 FROM temp.scan_folders staged
           LEFT JOIN folders current ON current.source_key=?1 AND current.object_id=staged.object_id
           WHERE staged.artwork_binding IS NOT current.artwork_binding
           UNION ALL
           SELECT 1 FROM temp.scan_playlists staged
           LEFT JOIN playlists current ON current.source_key=?1 AND current.ownership='source' AND current.object_id=staged.object_id
           WHERE staged.artwork_binding IS NOT current.artwork_binding
           UNION ALL
           SELECT 1 FROM temp.scan_removals removal JOIN tracks current
             ON removal.entity_kind='track' AND current.source_key=?1 AND current.object_id=removal.object_id
           WHERE current.artwork_binding IS NOT NULL
           UNION ALL
           SELECT 1 FROM temp.scan_removals removal JOIN albums current
             ON removal.entity_kind='album' AND current.source_key=?1 AND current.object_id=removal.object_id
           WHERE current.artwork_binding IS NOT NULL
           UNION ALL
           SELECT 1 FROM temp.scan_removals removal JOIN artists current
             ON removal.entity_kind='artist' AND current.source_key=?1 AND current.object_id=removal.object_id
           WHERE current.artwork_binding IS NOT NULL
           UNION ALL
           SELECT 1 FROM temp.scan_removals removal JOIN genres current
             ON removal.entity_kind='genre' AND current.source_key=?1 AND current.object_id=removal.object_id
           WHERE current.artwork_binding IS NOT NULL
           UNION ALL
           SELECT 1 FROM temp.scan_removals removal JOIN playlists current
             ON removal.entity_kind='playlist' AND current.source_key=?1 AND current.ownership='source' AND current.object_id=removal.object_id
           WHERE current.artwork_binding IS NOT NULL
         )",
    )
    .bind(source_key)
    .fetch_one(&mut **transaction)
    .await?)
}

async fn prune_local_orphans(
    transaction: &mut Transaction<'_, Sqlite>,
    source_key: i64,
) -> LibraryResult<()> {
    for sql in [
        "DELETE FROM albums WHERE source_key=?1 AND NOT EXISTS (SELECT 1 FROM tracks WHERE tracks.album_key=albums.album_key)",
        "DELETE FROM artists WHERE source_key=?1 AND NOT EXISTS (SELECT 1 FROM track_artists WHERE track_artists.artist_key=artists.artist_key) AND NOT EXISTS (SELECT 1 FROM album_artists WHERE album_artists.artist_key=artists.artist_key)",
        "DELETE FROM genres WHERE source_key=?1 AND NOT EXISTS (SELECT 1 FROM track_genres WHERE track_genres.genre_key=genres.genre_key) AND NOT EXISTS (SELECT 1 FROM album_genres WHERE album_genres.genre_key=genres.genre_key)",
        "DELETE FROM moods WHERE source_key=?1 AND NOT EXISTS (SELECT 1 FROM track_moods WHERE track_moods.mood_key=moods.mood_key)",
        "DELETE FROM folders WHERE source_key=?1 AND NOT EXISTS (SELECT 1 FROM track_folders WHERE track_folders.folder_key=folders.folder_key)",
    ] {
        sqlx::query(sql)
            .bind(source_key)
            .execute(&mut **transaction)
            .await?;
    }
    Ok(())
}

async fn publish_links(
    transaction: &mut Transaction<'_, Sqlite>,
    source_key: i64,
    full: bool,
) -> LibraryResult<()> {
    let full_deletes = [
        "DELETE FROM album_artists WHERE album_key IN (
             SELECT album_key FROM albums WHERE source_key=?1
         )",
        "DELETE FROM track_artists WHERE track_key IN (
             SELECT track_key FROM tracks WHERE source_key=?1
         )",
        "DELETE FROM album_genres WHERE album_key IN (
             SELECT album_key FROM albums WHERE source_key=?1
         )",
        "DELETE FROM track_genres WHERE track_key IN (
             SELECT track_key FROM tracks WHERE source_key=?1
         )",
        "DELETE FROM track_moods WHERE track_key IN (
             SELECT track_key FROM tracks WHERE source_key=?1
         )",
        "DELETE FROM track_folders WHERE track_key IN (
             SELECT track_key FROM tracks WHERE source_key=?1
         )",
        "DELETE FROM album_release_types WHERE album_key IN (
             SELECT album_key FROM albums WHERE source_key=?1
         )",
    ];
    let point_deletes = [
        "DELETE FROM album_artists WHERE album_key IN (SELECT album.album_key FROM albums album JOIN temp.scan_albums staged ON staged.object_id=album.object_id WHERE album.source_key=?1)",
        "DELETE FROM track_artists WHERE track_key IN (SELECT track.track_key FROM tracks track JOIN temp.scan_tracks staged ON staged.object_id=track.object_id WHERE track.source_key=?1)",
        "DELETE FROM album_genres WHERE album_key IN (SELECT album.album_key FROM albums album JOIN temp.scan_albums staged ON staged.object_id=album.object_id WHERE album.source_key=?1)",
        "DELETE FROM track_genres WHERE track_key IN (SELECT track.track_key FROM tracks track JOIN temp.scan_tracks staged ON staged.object_id=track.object_id WHERE track.source_key=?1)",
        "DELETE FROM track_moods WHERE track_key IN (SELECT track.track_key FROM tracks track JOIN temp.scan_tracks staged ON staged.object_id=track.object_id WHERE track.source_key=?1)",
        "DELETE FROM track_folders WHERE track_key IN (SELECT track.track_key FROM tracks track JOIN temp.scan_tracks staged ON staged.object_id=track.object_id WHERE track.source_key=?1)",
        "DELETE FROM album_release_types WHERE album_key IN (SELECT album.album_key FROM albums album JOIN temp.scan_albums staged ON staged.object_id=album.object_id WHERE album.source_key=?1)",
    ];
    for sql in if full { &full_deletes } else { &point_deletes } {
        sqlx::query(*sql)
            .bind(source_key)
            .execute(&mut **transaction)
            .await?;
    }
    let playlist_position_offset = sqlx::query_scalar::<_, i64>(
        "SELECT MAX(
             COALESCE((
                 SELECT MAX(entry.position)
                 FROM playlist_entries AS entry
                 JOIN playlists AS playlist USING (playlist_key)
                 WHERE playlist.source_key=?1 AND playlist.ownership='source'
             ), 0),
             COALESCE((SELECT MAX(position) FROM temp.scan_playlist_entries), 0)
         ) + 1",
    )
    .bind(source_key)
    .fetch_one(&mut **transaction)
    .await?;
    sqlx::query(if full {
        "UPDATE playlist_entries
         SET position=position + ?2
         WHERE playlist_key IN (
             SELECT playlist_key FROM playlists
             WHERE source_key=?1 AND ownership='source'
         )"
    } else {
        "UPDATE playlist_entries
         SET position=position + ?2
         WHERE playlist_key IN (
             SELECT playlist.playlist_key FROM playlists playlist
             JOIN temp.scan_playlists staged ON staged.object_id=playlist.object_id
             WHERE playlist.source_key=?1 AND playlist.ownership='source'
         )"
    })
    .bind(source_key)
    .bind(playlist_position_offset)
    .execute(&mut **transaction)
    .await?;
    for sql in [
        "INSERT INTO album_artists(album_key, artist_key, position)
         SELECT album.album_key, artist.artist_key, link.position
         FROM temp.scan_album_artists AS link
         JOIN albums AS album ON album.source_key=?1 AND album.object_id=link.owner_id
         JOIN artists AS artist ON artist.source_key=?1 AND artist.object_id=link.related_id",
        "INSERT INTO track_artists(track_key, artist_key, position)
         SELECT track.track_key, artist.artist_key, link.position
         FROM temp.scan_track_artists AS link
         JOIN tracks AS track ON track.source_key=?1 AND track.object_id=link.owner_id
         JOIN artists AS artist ON artist.source_key=?1 AND artist.object_id=link.related_id",
        "INSERT INTO album_genres(album_key, genre_key, position)
         SELECT album.album_key, genre.genre_key, link.position
         FROM temp.scan_album_genres AS link
         JOIN albums AS album ON album.source_key=?1 AND album.object_id=link.owner_id
         JOIN genres AS genre ON genre.source_key=?1 AND genre.object_id=link.related_id",
        "INSERT INTO track_genres(track_key, genre_key, position)
         SELECT track.track_key, genre.genre_key, link.position
         FROM temp.scan_track_genres AS link
         JOIN tracks AS track ON track.source_key=?1 AND track.object_id=link.owner_id
         JOIN genres AS genre ON genre.source_key=?1 AND genre.object_id=link.related_id",
        "INSERT INTO track_moods(track_key, mood_key, position)
         SELECT track.track_key, mood.mood_key, link.position
         FROM temp.scan_track_moods AS link
         JOIN tracks AS track ON track.source_key=?1 AND track.object_id=link.owner_id
         JOIN moods AS mood ON mood.source_key=?1 AND mood.object_id=link.related_id",
        "INSERT INTO track_folders(track_key, folder_key, position)
         SELECT track.track_key, folder.folder_key, link.position
         FROM temp.scan_track_folders AS link
         JOIN tracks AS track ON track.source_key=?1 AND track.object_id=link.owner_id
         JOIN folders AS folder ON folder.source_key=?1 AND folder.object_id=link.related_id",
        "INSERT INTO album_release_types(album_key, release_type, position)
         SELECT album.album_key, link.related_id, link.position
         FROM temp.scan_album_release_types AS link
         JOIN albums AS album ON album.source_key=?1 AND album.object_id=link.owner_id",
        "INSERT INTO playlist_entries(
             playlist_key, object_id, track_key, track_object_id, position
         ) SELECT playlist.playlist_key, entry.object_id, track.track_key,
                  entry.track_id, entry.position
           FROM temp.scan_playlist_entries AS entry
           JOIN playlists AS playlist
             ON playlist.source_key=?1 AND playlist.ownership='source'
            AND playlist.object_id=entry.playlist_id
           LEFT JOIN tracks AS track
             ON track.source_key=?1 AND track.object_id=entry.track_id
           WHERE true
         ON CONFLICT(playlist_key, object_id) DO UPDATE SET
             track_key=excluded.track_key,
             track_object_id=excluded.track_object_id,
             position=excluded.position",
    ] {
        sqlx::query(sql)
            .bind(source_key)
            .execute(&mut **transaction)
            .await?;
    }
    sqlx::query(if full {
        "DELETE FROM playlist_entries
         WHERE playlist_key IN (
             SELECT playlist_key FROM playlists
             WHERE source_key=?1 AND ownership='source'
         )
           AND NOT EXISTS (
               SELECT 1
               FROM playlists AS playlist
               JOIN temp.scan_playlist_entries AS staged
                 ON playlist.ownership='source'
                AND staged.playlist_id=playlist.object_id
                AND staged.object_id=playlist_entries.object_id
               WHERE playlist.playlist_key=playlist_entries.playlist_key
           )"
    } else {
        "DELETE FROM playlist_entries
         WHERE playlist_key IN (
             SELECT playlist.playlist_key FROM playlists playlist
             JOIN temp.scan_playlists staged ON staged.object_id=playlist.object_id
             WHERE playlist.source_key=?1 AND playlist.ownership='source'
         )
           AND NOT EXISTS (
               SELECT 1
               FROM playlists AS playlist
               JOIN temp.scan_playlist_entries AS staged
                 ON playlist.ownership='source'
                AND staged.playlist_id=playlist.object_id
                AND staged.object_id=playlist_entries.object_id
               WHERE playlist.playlist_key=playlist_entries.playlist_key
           )"
    })
    .bind(source_key)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn digest_catalog_header(
    staged_digest: [u8; 32],
    display_name: &str,
    normalized_name: &str,
) -> [u8; 32] {
    let mut hasher = Hasher::new();
    hash_bytes(&mut hasher, b"source");
    hash_bytes(&mut hasher, display_name.as_bytes());
    hash_bytes(&mut hasher, normalized_name.as_bytes());
    hash_bytes(&mut hasher, &staged_digest);
    *hasher.finalize().as_bytes()
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
                .expect("hide sources before expected-revision query");
        }
        let construction_error = Scan::begin(&database, "source", "Source", "source", None)
            .await
            .err()
            .expect("expected-revision query fails after staging creation");
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
            .expect("next Scan reuses released token");
        scan.write_genre("genre", "Genre", "genre", "genre", None)
            .await
            .expect("stage before writer failure");
        database.fail_writer().await.expect("fail fixed writer");
        assert!(matches!(
            scan.write_mood("mood", "Mood", "mood", "mood").await,
            Err(LibraryError::ScanFailed)
        ));
        assert_eq!(
            scan.finish().await.expect("finish invalidated scan"),
            ScanOutcome::Failed
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
            .expect("begin TEMP scan");
        scan.begin_batch().await.expect("begin bounded Scan batch");
        for number in 0..128 {
            scan.write_mood(
                &format!("mood-{number}"),
                "Mood",
                "mood",
                &format!("mood-{number}"),
            )
            .await
            .expect("stage one row in the bounded transaction");
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
            sqlx::query("INSERT INTO sources(object_id,display_name,normalized_name,catalog_digest,artwork_digest) VALUES ('batch-point','Point','point',zeroblob(32),zeroblob(32))")
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
            let object_plan = sqlx::query_as::<_, (i64, i64, i64, String)>(
                "EXPLAIN QUERY PLAN SELECT * FROM temp.scan_tracks
                 WHERE object_id > ?1 ORDER BY object_id LIMIT ?2",
            )
            .bind("")
            .bind(DIGEST_PAGE_ROWS)
            .fetch_one(&mut *writer)
            .await
            .expect("read object keyset plan")
            .3;
            assert!(
                object_plan.contains("SEARCH")
                    && object_plan.contains("sqlite_autoindex_scan_tracks_1")
                    && object_plan.contains("object_id>?")
                    && !object_plan.contains("SCAN"),
                "{object_plan}"
            );
            let album_audio_plan = sqlx::query_as::<_, (i64, i64, i64, String)>(
                "EXPLAIN QUERY PLAN SELECT track.album_object_id,
                         track.disc_number,track.track_number,track.sort_text,
                         track.object_id,track.source_loudness_analysis_key,
                         COALESCE((SELECT access.loudness_analysis_key
                                   FROM local_access_files access
                                   WHERE access.source_key=?6
                                     AND access.track_object_id=track.object_id
                                   ORDER BY CASE access.origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END,
                                            access.local_access_file_key LIMIT 1),
                                  track.source_loudness_analysis_key)
                     FROM temp.scan_tracks track
                     WHERE track.album_object_id IS NOT NULL
                       AND (track.album_object_id,track.disc_number,track.track_number,
                            track.sort_text,track.object_id)>(?1,?2,?3,?4,?5)
                     ORDER BY track.album_object_id,track.disc_number,
                              track.track_number,track.sort_text,track.object_id
                     LIMIT ?7",
            )
            .bind("")
            .bind(-1_i64)
            .bind(-1_i64)
            .bind("")
            .bind("")
            .bind(Option::<i64>::None)
            .bind(DIGEST_PAGE_ROWS)
            .fetch_all(&mut *writer)
            .await
            .expect("read Album loudness keyset plan")
            .into_iter()
            .map(|row| row.3)
            .collect::<Vec<_>>()
            .join(" | ");
            assert!(
                album_audio_plan.contains("scan_tracks_album_loudness_idx")
                    && !album_audio_plan.contains("USE TEMP B-TREE FOR ORDER BY"),
                "{album_audio_plan}"
            );
            let relation_plan = sqlx::query_as::<_, (i64, i64, i64, String)>(
                "EXPLAIN QUERY PLAN SELECT * FROM temp.scan_track_artists
                 WHERE (owner_id, position) > (?1, ?2)
                 ORDER BY owner_id, position LIMIT ?3",
            )
            .bind("")
            .bind(-1_i64)
            .bind(DIGEST_PAGE_ROWS)
            .fetch_one(&mut *writer)
            .await
            .expect("read relationship keyset plan")
            .3;
            assert!(
                relation_plan.contains("SEARCH")
                    && relation_plan.contains("sqlite_autoindex_scan_track_artists_1")
                    && !relation_plan.contains("SCAN"),
                "{relation_plan}"
            );
            let playlist_plan = sqlx::query_as::<_, (i64, i64, i64, String)>(
                "EXPLAIN QUERY PLAN SELECT * FROM temp.scan_playlist_entries
                 WHERE (playlist_id, position) > (?1, ?2)
                 ORDER BY playlist_id, position LIMIT ?3",
            )
            .bind("")
            .bind(-1_i64)
            .bind(DIGEST_PAGE_ROWS)
            .fetch_one(&mut *writer)
            .await
            .expect("read playlist keyset plan")
            .3;
            assert!(
                playlist_plan.contains("SEARCH")
                    && playlist_plan.contains("sqlite_autoindex_scan_playlist_entries_1")
                    && !playlist_plan.contains("SCAN"),
                "{playlist_plan}"
            );
            sqlx::query(
                "INSERT INTO sources(
                     object_id, display_name, normalized_name,
                     catalog_digest, artwork_digest
                 ) VALUES ('point-source', 'Point', 'point', zeroblob(32), zeroblob(32))",
            )
            .execute(&mut *writer)
            .await
            .expect("interleave point write");
        }
        assert!(matches!(
            scan.finish().await.expect("publish staged scan"),
            ScanOutcome::Changed(_)
        ));
    }
}
