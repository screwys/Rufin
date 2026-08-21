//! Private SQLite connection lane.
//!
//! One worker thread owns one connection. Public library operations send typed
//! work to this lane; callers never receive a connection, SQL closure, Store
//! handle, or route query interface.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params, types::ValueRef};
use thiserror::Error;

use crate::{
    ActivityItem, ActivityItemId, ActivityPeriod, ActivitySummary, ActivityWrite, Album, AlbumId,
    AlbumRelations, AlbumReleaseCandidate, AlbumReleaseResult, Artist, ArtistId, CandidateBatch,
    CandidateChange, CandidateFinish, CandidateHeader, CueSegment, FavoriteItemId, Genre, GenreId,
    HomeFacts, ImageRef, LibraryInput, LocalAccessFile, LocalArtworkRef, LocalFile, LocalFileKind,
    LocalFileState, LocalImport, LoudnessItemId, LoudnessMeasurement, LoudnessMeasurementWrite,
    LyricsCacheAuthority, LyricsCacheInput, LyricsCacheKey, LyricsCacheTrim, LyricsCacheWrite,
    MusicFolder, MusicFolderId, NewScrobble, PendingFavorite, PendingScrobble, PendingScrobbleId,
    PlaybackCheckpoint, PlaybackLoad, PlaybackOccurrenceId, PlaybackProgressUpdate,
    PlaybackQueueRowsSnapshot, PlaybackState, PlaybackStateUpdate, PlaybackTraversalUpdate,
    PlaybackWriteOutcome, Playlist, PlaylistEntry, PlaylistId, PlaylistSnapshot, ProviderFreshness,
    RecentPlay, ScrobbleService, SmartPlaylistBuiltin, SmartPlaylistId, SmartPlaylistRecord,
    SourceId, Track, TrackActivity, TrackData, TrackId, TrackRelations,
    favorites::FavoriteValue,
    items::color_seed,
    loaded::{ItemReplacement, LocalFavoriteTransfer, LocalFavoriteUpdate},
    refresh::STORE_BYTE_BATCH_LIMIT,
    refresh::STORE_ROW_BATCH_LIMIT,
};

pub(crate) mod repair;
mod schema;

pub use repair::StoreRepairReport;

#[derive(Debug, Error)]
pub(crate) enum StoreError {
    #[error("SQLite failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Store I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("the Store worker stopped")]
    WorkerStopped,
    #[error("the Store worker panicked while opening")]
    WorkerPanicked,
    #[error("unsupported Store (application ID {application_id}, schema {user_version})")]
    UnsupportedSchema {
        application_id: i64,
        user_version: i64,
    },
    #[error("invalid final Store schema: {0}")]
    InvalidFinalSchema(String),
    #[error("source {0} still has an unfinished candidate being cleaned")]
    CandidateCleanupPending(SourceId),
    #[error("candidate {0} is no longer available")]
    CandidateMissing(i64),
    #[error("library {library_id} is not current for source {source_id}")]
    WrongCurrentLibrary {
        source_id: SourceId,
        library_id: i64,
    },
    #[error("one Store row exceeds the 8 MiB write bound")]
    RowTooLarge,
    #[error("provider freshness markers may not exceed 64 KiB")]
    FreshnessTooLarge,
    #[error("integer value is outside SQLite's signed range")]
    IntegerRange,
    #[error("invalid stored {kind}: {value}")]
    InvalidValue { kind: &'static str, value: String },
}

pub(crate) type StoreResult<T> = Result<T, StoreError>;

#[derive(Debug)]
pub(crate) struct StoreCommit {
    pub freshness: Option<ProviderFreshness>,
    pub home: HomeFacts,
    pub activity: Vec<TrackActivity>,
    pub recent_plays: Vec<RecentPlay>,
}

pub(crate) struct PreparedStoreCandidate {
    candidate_library_id: i64,
    current_library_id: Option<i64>,
    change: CandidateChange,
    content_digest: [u8; 32],
    home_digest: [u8; 32],
    freshness: Option<ProviderFreshness>,
    home: HomeFacts,
    home_json: String,
    accepted_at: i64,
}

impl PreparedStoreCandidate {
    pub(crate) const fn current_library_id(&self) -> Option<i64> {
        self.current_library_id
    }

    pub(crate) const fn change(&self) -> CandidateChange {
        self.change
    }
}

pub(crate) struct StoreCandidatePreparation {
    pub prepared: PreparedStoreCandidate,
    pub input: Option<LibraryInput>,
}

pub(crate) struct StoredSourceUpdate {
    pub replacement: ItemReplacement,
    pub playlists: Vec<PlaylistSnapshot>,
    pub removed_playlists: Vec<PlaylistId>,
}

pub(crate) struct StoredLocalComponent {
    pub files: Vec<LocalFile>,
    pub removed_paths: Vec<String>,
    pub replacement: ItemReplacement,
    pub imports: Vec<LocalImport>,
    pub favorites: Vec<FavoriteItemId>,
    pub activity: Vec<TrackActivity>,
}

type StoreJob = Box<dyn FnOnce(&mut Worker) + Send>;

#[derive(Clone)]
pub(crate) struct StoreLane {
    inner: Arc<StoreLaneInner>,
}

struct StoreLaneInner {
    sender: Option<SyncSender<StoreJob>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Drop for StoreLaneInner {
    fn drop(&mut self) {
        drop(self.sender.take());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl StoreLane {
    pub(crate) fn same_lane(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    pub(crate) fn open(path: impl Into<PathBuf>) -> StoreResult<Self> {
        Self::spawn(StoreLocation::Path(path.into()))
    }

    pub(crate) fn memory() -> StoreResult<Self> {
        Self::spawn(StoreLocation::Memory)
    }

    pub(crate) fn open_with_repair(
        path: impl Into<PathBuf>,
    ) -> StoreResult<(Self, Option<StoreRepairReport>)> {
        let path = path.into();
        match Self::open(path.clone()) {
            Ok(store) => Ok((store, None)),
            Err(error) if path.exists() && repair::caused_by_store_contents(&error) => {
                let report = repair::repair(&path)?;
                Ok((Self::open(path)?, Some(report)))
            }
            Err(error) => Err(error),
        }
    }

    fn spawn(location: StoreLocation) -> StoreResult<Self> {
        let (sender, receiver) = mpsc::sync_channel(64);
        let (opened_sender, opened_receiver) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("rufin-store".to_string())
            .spawn(move || {
                let opened = Worker::open(location);
                match opened {
                    Ok(mut worker) => {
                        if opened_sender.send(Ok(())).is_ok() {
                            worker.run(receiver);
                        }
                    }
                    Err(error) => {
                        let _ = opened_sender.send(Err(error));
                    }
                }
            })
            .map_err(StoreError::Io)?;
        match opened_receiver.recv() {
            Ok(Ok(())) => Ok(Self {
                inner: Arc::new(StoreLaneInner {
                    sender: Some(sender),
                    worker: Some(worker),
                }),
            }),
            Ok(Err(error)) => {
                drop(sender);
                let _ = worker.join();
                Err(error)
            }
            Err(_) => {
                drop(sender);
                let _ = worker.join();
                Err(StoreError::WorkerPanicked)
            }
        }
    }

    pub(crate) fn begin_candidate(&self, header: CandidateHeader) -> StoreResult<i64> {
        self.execute(move |worker| worker.begin_candidate(header))
    }

    pub(crate) fn write_candidate(
        &self,
        library_id: i64,
        batch: CandidateBatch,
    ) -> StoreResult<()> {
        self.execute(move |worker| worker.write_candidate(library_id, batch))
    }

    pub(crate) fn prepare_candidate(
        &self,
        library_id: i64,
        finish: CandidateFinish,
    ) -> StoreResult<StoreCandidatePreparation> {
        self.execute(move |worker| worker.prepare_candidate(library_id, finish))
    }

    pub(crate) fn accept_candidate(
        &self,
        prepared: PreparedStoreCandidate,
    ) -> StoreResult<StoreCommit> {
        self.execute(move |worker| worker.accept_candidate(prepared))
    }

    pub(crate) fn schedule_cleanup(&self, library_id: i64) {
        self.schedule(move |worker| worker.queue_cleanup(library_id));
    }

    pub(crate) fn load_current(&self, source_id: SourceId) -> StoreResult<Option<LibraryInput>> {
        self.execute(move |worker| {
            worker
                .current_library_id(&source_id)
                .and_then(|library_id| {
                    library_id
                        .map(|library_id| worker.load_library(library_id))
                        .transpose()
                })
        })
    }

    pub(crate) fn remove_source_data(&self, source_id: SourceId) -> StoreResult<()> {
        self.execute(move |worker| worker.remove_source_data(&source_id))
    }

    pub(crate) fn replace_loudness(
        &self,
        source_id: SourceId,
        library_id: i64,
        writes: Vec<LoudnessMeasurementWrite>,
    ) -> StoreResult<()> {
        self.execute(move |worker| worker.replace_loudness(&source_id, library_id, &writes))
    }

    pub(crate) fn replace_source_update(
        &self,
        source_id: SourceId,
        library_id: i64,
        replacement: ItemReplacement,
        playlists: Vec<PlaylistSnapshot>,
        removed_playlists: Vec<PlaylistId>,
    ) -> StoreResult<StoredSourceUpdate> {
        self.execute(move |worker| {
            worker.replace_source_update(
                &source_id,
                library_id,
                replacement,
                playlists,
                removed_playlists,
            )
        })
    }

    pub(crate) fn replace_home(
        &self,
        source_id: SourceId,
        library_id: i64,
        home: HomeFacts,
    ) -> StoreResult<()> {
        self.execute(move |worker| worker.replace_home(&source_id, library_id, &home))
    }

    pub(crate) fn replace_local_component(
        &self,
        source_id: SourceId,
        library_id: i64,
        observed_at: i64,
        files: Vec<LocalFile>,
        removed_paths: Vec<String>,
        replacement: ItemReplacement,
        favorite_update: LocalFavoriteUpdate,
    ) -> StoreResult<StoredLocalComponent> {
        self.execute(move |worker| {
            worker.replace_local_component(
                &source_id,
                library_id,
                observed_at,
                files,
                removed_paths,
                replacement,
                favorite_update,
            )
        })
    }

    pub(crate) fn replace_local_access(
        &self,
        source_id: SourceId,
        files: Vec<LocalAccessFile>,
    ) -> StoreResult<bool> {
        self.execute(move |worker| worker.replace_local_access(&source_id, &files))
    }

    pub(crate) fn clear_local_access(&self, source_id: SourceId) -> StoreResult<()> {
        self.execute(move |worker| worker.clear_local_access(&source_id))
    }

    pub(crate) fn set_favorite(
        &self,
        source_id: SourceId,
        item_id: FavoriteItemId,
        favorite: bool,
        local: bool,
        fallback: Option<FavoriteValue>,
    ) -> StoreResult<()> {
        self.execute(move |worker| {
            worker.set_favorite(&source_id, &item_id, favorite, local, fallback)
        })
    }

    pub(crate) fn set_rating(
        &self,
        source_id: SourceId,
        item_id: FavoriteItemId,
        rating: Option<u8>,
    ) -> StoreResult<()> {
        self.execute(move |worker| worker.set_rating(&source_id, &item_id, rating))
    }

    pub(crate) fn queue_remote_favorite(
        &self,
        source_id: SourceId,
        item_id: FavoriteItemId,
        favorite: bool,
        previous: bool,
        next_attempt_at: i64,
        fallback: Option<FavoriteValue>,
    ) -> StoreResult<()> {
        self.execute(move |worker| {
            worker.queue_remote_favorite(
                &source_id,
                &item_id,
                favorite,
                previous,
                next_attempt_at,
                fallback,
            )
        })
    }

    pub(crate) fn due_remote_favorites(
        &self,
        source_id: SourceId,
        now: i64,
        limit: usize,
    ) -> StoreResult<Vec<PendingFavorite>> {
        self.execute(move |worker| worker.due_remote_favorites(&source_id, now, limit))
    }

    pub(crate) fn complete_remote_favorite(
        &self,
        source_id: SourceId,
        item_id: FavoriteItemId,
        favorite: bool,
    ) -> StoreResult<()> {
        self.execute(move |worker| worker.complete_remote_favorite(&source_id, &item_id, favorite))
    }

    pub(crate) fn defer_remote_favorite(
        &self,
        source_id: SourceId,
        item_id: FavoriteItemId,
        favorite: bool,
        next_attempt_at: i64,
    ) -> StoreResult<()> {
        self.execute(move |worker| {
            worker.defer_remote_favorite(&source_id, &item_id, favorite, next_attempt_at)
        })
    }

    pub(crate) fn reject_remote_favorite(
        &self,
        source_id: SourceId,
        item_id: FavoriteItemId,
        favorite: bool,
    ) -> StoreResult<Option<bool>> {
        self.execute(move |worker| worker.reject_remote_favorite(&source_id, &item_id, favorite))
    }

    pub(crate) fn replace_local_playlist(
        &self,
        source_id: SourceId,
        snapshot: PlaylistSnapshot,
    ) -> StoreResult<()> {
        self.execute(move |worker| worker.replace_local_playlist(&source_id, snapshot))
    }

    pub(crate) fn remove_local_playlist(
        &self,
        source_id: SourceId,
        playlist_id: PlaylistId,
    ) -> StoreResult<()> {
        self.execute(move |worker| worker.remove_local_playlist(&source_id, &playlist_id))
    }

    pub(crate) fn put_smart_playlist(
        &self,
        source_id: SourceId,
        record: SmartPlaylistRecord,
    ) -> StoreResult<()> {
        self.execute(move |worker| worker.put_smart_playlist(&source_id, &record))
    }

    pub(crate) fn remove_smart_playlist(
        &self,
        source_id: SourceId,
        smart_playlist_id: SmartPlaylistId,
    ) -> StoreResult<()> {
        self.execute(move |worker| worker.remove_smart_playlist(&source_id, &smart_playlist_id))
    }

    pub(crate) fn replace_smart_playlist_order(
        &self,
        source_id: SourceId,
        ordered_ids: Vec<SmartPlaylistId>,
    ) -> StoreResult<()> {
        self.execute(move |worker| worker.replace_smart_playlist_order(&source_id, &ordered_ids))
    }

    pub(crate) fn record_activity(
        &self,
        source_id: SourceId,
        activity: ActivityWrite,
    ) -> StoreResult<Option<TrackActivity>> {
        self.execute(move |worker| worker.record_activity(&source_id, &activity))
    }

    pub(crate) fn activity_summary(
        &self,
        source_id: SourceId,
        period: ActivityPeriod,
    ) -> StoreResult<ActivitySummary> {
        self.execute(move |worker| worker.activity_summary(&source_id, &period))
    }

    pub(crate) fn load_playback(&self, source_id: SourceId) -> StoreResult<PlaybackLoad> {
        self.execute(move |worker| worker.load_playback(&source_id))
    }

    pub(crate) fn replace_playback(
        &self,
        checkpoint: PlaybackCheckpoint,
    ) -> StoreResult<PlaybackWriteOutcome> {
        self.execute(move |worker| worker.replace_playback(&checkpoint))
    }

    pub(crate) fn update_playback_state(
        &self,
        update: PlaybackStateUpdate,
    ) -> StoreResult<PlaybackWriteOutcome> {
        self.execute(move |worker| worker.update_playback_state(&update))
    }

    pub(crate) fn replace_playback_traversal(
        &self,
        update: PlaybackTraversalUpdate,
    ) -> StoreResult<PlaybackWriteOutcome> {
        self.execute(move |worker| worker.replace_playback_traversal(&update))
    }

    pub(crate) fn update_playback_progress(
        &self,
        update: PlaybackProgressUpdate,
    ) -> StoreResult<PlaybackWriteOutcome> {
        self.execute(move |worker| worker.update_playback_progress(&update))
    }

    pub(crate) fn remove_playback(&self, source_id: SourceId) -> StoreResult<bool> {
        self.execute(move |worker| worker.remove_playback(&source_id))
    }

    pub(crate) fn cached_lyrics(
        &self,
        key: LyricsCacheKey,
        expected_input: LyricsCacheInput,
    ) -> StoreResult<Option<crate::CachedLyrics>> {
        self.execute(move |worker| worker.cached_lyrics(&key, &expected_input))
    }

    pub(crate) fn store_lyrics(&self, write: LyricsCacheWrite) -> StoreResult<LyricsCacheTrim> {
        self.execute(move |worker| worker.store_lyrics(&write))
    }

    pub(crate) fn remove_lyrics_if_authority(
        &self,
        key: LyricsCacheKey,
        authority: LyricsCacheAuthority,
    ) -> StoreResult<bool> {
        self.execute(move |worker| worker.remove_lyrics_if_authority(&key, authority))
    }

    pub(crate) fn remove_track_lyrics_by_authority(
        &self,
        source_id: SourceId,
        track_id: TrackId,
        authority: LyricsCacheAuthority,
    ) -> StoreResult<u64> {
        self.execute(move |worker| {
            worker.remove_track_lyrics_by_authority(&source_id, &track_id, authority)
        })
    }

    pub(crate) fn accept_album_release(
        &self,
        candidate: AlbumReleaseCandidate,
        result: AlbumReleaseResult,
    ) -> StoreResult<bool> {
        self.execute(move |worker| worker.accept_album_release(&candidate, &result))
    }

    pub(crate) fn album_release_candidates(
        &self,
        source_id: SourceId,
        library_id: i64,
        limit: usize,
    ) -> StoreResult<Vec<AlbumReleaseCandidate>> {
        self.execute(move |worker| worker.album_release_candidates(&source_id, library_id, limit))
    }

    pub(crate) fn queue_scrobbles(&self, scrobbles: Vec<NewScrobble>) -> StoreResult<usize> {
        self.execute(move |worker| worker.queue_scrobbles(&scrobbles))
    }

    pub(crate) fn due_scrobbles(
        &self,
        service: ScrobbleService,
        account_id: String,
        now: i64,
        limit: usize,
    ) -> StoreResult<Vec<PendingScrobble>> {
        self.execute(move |worker| worker.due_scrobbles(service, &account_id, now, limit))
    }

    pub(crate) fn complete_scrobble(&self, id: PendingScrobbleId) -> StoreResult<()> {
        self.execute(move |worker| worker.complete_scrobble(&id))
    }

    pub(crate) fn discard_scrobbles(
        &self,
        service: ScrobbleService,
        account_id: String,
    ) -> StoreResult<usize> {
        self.execute(move |worker| worker.discard_scrobbles(service, &account_id))
    }

    pub(crate) fn defer_scrobble(
        &self,
        id: PendingScrobbleId,
        next_attempt_at: i64,
    ) -> StoreResult<()> {
        self.execute(move |worker| worker.defer_scrobble(&id, next_attempt_at))
    }

    pub(crate) fn block_scrobbles(
        &self,
        service: ScrobbleService,
        account_id: String,
        error: String,
    ) -> StoreResult<usize> {
        self.execute(move |worker| worker.block_scrobbles(service, &account_id, &error))
    }

    pub(crate) fn wake_scrobbles(
        &self,
        service: ScrobbleService,
        account_id: String,
        now: i64,
    ) -> StoreResult<usize> {
        self.execute(move |worker| worker.wake_scrobbles(service, &account_id, now))
    }

    fn execute<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Worker) -> StoreResult<T> + Send + 'static,
    ) -> StoreResult<T> {
        let (reply, receive) = reply_channel();
        self.send(Box::new(move |worker| respond(reply, operation(worker))))?;
        receive_reply(receive)
    }

    fn schedule(&self, operation: impl FnOnce(&mut Worker) + Send + 'static) {
        if let Some(sender) = self.inner.sender.as_ref() {
            let _ = sender.try_send(Box::new(operation));
        }
    }

    fn send(&self, job: StoreJob) -> StoreResult<()> {
        self.inner
            .sender
            .as_ref()
            .ok_or(StoreError::WorkerStopped)?
            .send(job)
            .map_err(|_| StoreError::WorkerStopped)
    }
}

enum StoreLocation {
    Path(PathBuf),
    Memory,
}

type Reply<T> = SyncSender<StoreResult<T>>;

struct Worker {
    connection: Connection,
    cleanup: VecDeque<i64>,
    cleanup_set: HashSet<i64>,
}

impl Worker {
    fn open(location: StoreLocation) -> StoreResult<Self> {
        let connection = match location {
            StoreLocation::Path(path) => open_path(&path)?,
            StoreLocation::Memory => Connection::open_in_memory()?,
        };
        connection.busy_timeout(Duration::from_secs(5))?;
        schema::initialize(&connection)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        let abandoned = cleanup_targets(&connection)?;
        let mut worker = Self {
            connection,
            cleanup: VecDeque::new(),
            cleanup_set: HashSet::new(),
        };
        for library_id in abandoned {
            worker.queue_cleanup(library_id);
        }
        Ok(worker)
    }

    fn run(&mut self, receiver: Receiver<StoreJob>) {
        loop {
            let job = if self.cleanup.is_empty() {
                match receiver.recv() {
                    Ok(job) => job,
                    Err(_) => break,
                }
            } else {
                match receiver.recv_timeout(Duration::from_millis(5)) {
                    Ok(job) => job,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        self.clean_one_batch();
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            };
            job(self);
            self.clean_one_batch();
        }
    }

    fn begin_candidate(&mut self, header: CandidateHeader) -> StoreResult<i64> {
        let unfinished = self
            .connection
            .query_row(
                "SELECT library_id FROM source_libraries
                 WHERE source_id = ?1 AND accepted_at IS NULL",
                [header.source_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if let Some(library_id) = unfinished {
            self.queue_cleanup(library_id);
            return Err(StoreError::CandidateCleanupPending(header.source_id));
        }

        self.connection.execute(
            "INSERT INTO source_libraries(
                source_id, input_version, input_digest
             ) VALUES (?1, 1, ?2)",
            params![header.source_id.as_str(), header.input_digest.as_slice()],
        )?;
        Ok(self.connection.last_insert_rowid())
    }

    fn write_candidate(&mut self, library_id: i64, batch: CandidateBatch) -> StoreResult<()> {
        self.require_unaccepted(library_id)?;
        match &batch {
            CandidateBatch::Albums(values) => write_bounded(
                &mut self.connection,
                values,
                estimate_album,
                |transaction, album| write_album(transaction, library_id, album),
            ),
            CandidateBatch::Tracks(values) => write_bounded(
                &mut self.connection,
                values,
                estimate_track,
                |transaction, track| write_track(transaction, library_id, track),
            ),
            CandidateBatch::Artists(values) => write_bounded(
                &mut self.connection,
                values,
                estimate_artist,
                |transaction, artist| write_artist(transaction, library_id, artist),
            ),
            CandidateBatch::Genres(values) => write_bounded(
                &mut self.connection,
                values,
                estimate_genre,
                |transaction, genre| write_genre(transaction, library_id, genre),
            ),
            CandidateBatch::MusicFolders(values) => write_bounded(
                &mut self.connection,
                values,
                estimate_music_folder,
                |transaction, folder| write_music_folder(transaction, library_id, folder),
            ),
            CandidateBatch::Playlists(values) => {
                for snapshot in values {
                    write_playlist(&mut self.connection, library_id, snapshot)?;
                }
                Ok(())
            }
            CandidateBatch::LocalFiles(values) => write_bounded(
                &mut self.connection,
                values,
                estimate_local_file,
                |transaction, file| write_local_file(transaction, library_id, file),
            ),
        }?;
        Ok(())
    }

    fn prepare_candidate(
        &mut self,
        library_id: i64,
        finish: CandidateFinish,
    ) -> StoreResult<StoreCandidatePreparation> {
        let source_id = self.require_unaccepted(library_id)?;
        let CandidateFinish {
            freshness,
            home,
            accepted_at,
        } = finish;
        let candidate_input_digest = self.connection.query_row(
            "SELECT input_digest FROM source_libraries WHERE library_id = ?1",
            [library_id],
            |row| row.get::<_, Vec<u8>>(0),
        )?;
        if freshness
            .as_ref()
            .is_some_and(|freshness| freshness.marker.len() > 65_536)
        {
            return Err(StoreError::FreshnessTooLarge);
        }
        let (home_json, home_digest) = persisted_home(&home)?;
        let mut current = self
            .connection
            .query_row(
                "SELECT
                    library_id, input_digest, content_digest, home_digest
                 FROM source_libraries
                 WHERE source_id = ?1 AND accepted_at IS NOT NULL
                 ORDER BY library_id DESC
                 LIMIT 1",
                [source_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, Option<Vec<u8>>>(2)?,
                        row.get::<_, Option<Vec<u8>>>(3)?,
                    ))
                },
            )
            .optional()?;
        let content_digest = candidate_content_digest(&self.connection, library_id)?;
        if let Some((current_id, current_input_digest, current_digest, _)) = current.as_mut()
            && *current_input_digest == candidate_input_digest
            && current_digest.is_none()
        {
            *current_digest = Some(
                candidate_content_digest(&self.connection, *current_id)?
                    .as_slice()
                    .to_vec(),
            );
        }

        let current_library_id = current.as_ref().map(|(library_id, ..)| *library_id);
        if let Some((_, current_input_digest, current_digest, current_home_digest)) = current
            && current_input_digest == candidate_input_digest
            && current_digest.as_deref() == Some(content_digest.as_slice())
        {
            let change = if current_home_digest.as_deref() == Some(home_digest.as_slice()) {
                CandidateChange::None
            } else {
                CandidateChange::Home
            };
            return Ok(StoreCandidatePreparation {
                prepared: PreparedStoreCandidate {
                    candidate_library_id: library_id,
                    current_library_id,
                    change,
                    content_digest,
                    home_digest,
                    freshness,
                    home,
                    home_json,
                    accepted_at,
                },
                input: None,
            });
        }

        let input = self.load_candidate_library(
            library_id,
            &source_id,
            candidate_input_digest,
            freshness.clone(),
            home.clone(),
            accepted_at,
        )?;
        Ok(StoreCandidatePreparation {
            prepared: PreparedStoreCandidate {
                candidate_library_id: library_id,
                current_library_id,
                change: CandidateChange::Library,
                content_digest,
                home_digest,
                freshness,
                home,
                home_json,
                accepted_at,
            },
            input: Some(input),
        })
    }

    fn accept_candidate(&mut self, prepared: PreparedStoreCandidate) -> StoreResult<StoreCommit> {
        let PreparedStoreCandidate {
            candidate_library_id,
            current_library_id,
            change,
            content_digest,
            home_digest,
            freshness,
            home,
            home_json,
            accepted_at,
        } = prepared;
        let source_id = self.require_unaccepted(candidate_library_id)?;
        if self.current_library_id(&source_id)? != current_library_id {
            return Err(StoreError::CandidateMissing(candidate_library_id));
        }

        if change != CandidateChange::Library {
            let current_id =
                current_library_id.ok_or(StoreError::CandidateMissing(candidate_library_id))?;
            let transaction = self.connection.transaction()?;
            update_accepted_metadata(
                &transaction,
                current_id,
                &content_digest,
                &home_digest,
                freshness.as_ref(),
                &home_json,
                change == CandidateChange::Home,
            )?;
            transaction.commit()?;
            self.queue_cleanup(candidate_library_id);
            return Ok(StoreCommit {
                freshness,
                home,
                activity: Vec::new(),
                recent_plays: Vec::new(),
            });
        }

        let old_ids = self
            .connection
            .prepare(
                "SELECT library_id FROM source_libraries
                 WHERE source_id = ?1
                   AND accepted_at IS NOT NULL
                   AND library_id <> ?2
                 ORDER BY library_id",
            )?
            .query_map(params![source_id.as_str(), candidate_library_id], |row| {
                row.get(0)
            })?
            .collect::<Result<Vec<i64>, _>>()?;
        let transaction = self.connection.transaction()?;
        update_candidate_acceptance(
            &transaction,
            candidate_library_id,
            &content_digest,
            &home_digest,
            freshness.as_ref(),
            accepted_at,
            &home_json,
        )?;
        if home.is_rufin_defined() {
            insert_local_imports(&transaction, &source_id, candidate_library_id, accepted_at)?;
        }
        prune_album_release_info(&transaction, &source_id, candidate_library_id)?;
        let activity = load_track_activity(&transaction, &source_id)?;
        let recent_plays = load_recent_plays(&transaction, &source_id)?;
        transaction.commit()?;
        for old_id in old_ids {
            self.queue_cleanup(old_id);
        }
        Ok(StoreCommit {
            freshness,
            home,
            activity,
            recent_plays,
        })
    }

    fn require_unaccepted(&self, library_id: i64) -> StoreResult<SourceId> {
        self.connection
            .query_row(
                "SELECT source_id FROM source_libraries
                 WHERE library_id = ?1 AND accepted_at IS NULL",
                [library_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(SourceId::new)
            .ok_or(StoreError::CandidateMissing(library_id))
    }

    fn current_library_id(&self, source_id: &SourceId) -> StoreResult<Option<i64>> {
        Ok(self
            .connection
            .query_row(
                "SELECT library_id FROM source_libraries
                 WHERE source_id = ?1 AND accepted_at IS NOT NULL
                 ORDER BY library_id DESC LIMIT 1",
                [source_id.as_str()],
                |row| row.get(0),
            )
            .optional()?)
    }

    fn remove_source_data(&mut self, source_id: &SourceId) -> StoreResult<()> {
        let library_ids = self
            .connection
            .prepare(
                "SELECT library_id FROM source_libraries
                 WHERE source_id = ?1 ORDER BY library_id",
            )?
            .query_map([source_id.as_str()], |row| row.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for library_id in &library_ids {
            while !cleanup_library_batch(&mut self.connection, *library_id)? {}
        }

        let transaction = self.connection.transaction()?;
        transaction.execute(
            "DELETE FROM local_access_files WHERE source_id = ?1",
            [source_id.as_str()],
        )?;
        transaction.execute(
            "DELETE FROM local_playlist_entries WHERE source_id = ?1",
            [source_id.as_str()],
        )?;
        transaction.execute(
            "DELETE FROM local_playlists WHERE source_id = ?1",
            [source_id.as_str()],
        )?;
        transaction.execute(
            "DELETE FROM playback_state WHERE source_id = ?1",
            [source_id.as_str()],
        )?;
        transaction.execute(
            "DELETE FROM playback_queues WHERE source_id = ?1",
            [source_id.as_str()],
        )?;
        for table in [
            "local_favorites",
            "user_ratings",
            "pending_favorites",
            "loudness_measurements",
            "smart_playlists",
            "local_imports",
            "listening_aggregates",
            "recent_plays",
            "lyrics_cache",
            "album_release_info",
        ] {
            transaction.execute(
                &format!("DELETE FROM {table} WHERE source_id = ?1"),
                [source_id.as_str()],
            )?;
        }
        transaction.commit()?;

        let removed = library_ids.into_iter().collect::<HashSet<_>>();
        self.cleanup.retain(|target| !removed.contains(target));
        self.cleanup_set.retain(|target| !removed.contains(target));
        Ok(())
    }

    fn replace_loudness(
        &mut self,
        source_id: &SourceId,
        library_id: i64,
        writes: &[LoudnessMeasurementWrite],
    ) -> StoreResult<()> {
        if self.current_library_id(source_id)? != Some(library_id) {
            return Err(StoreError::WrongCurrentLibrary {
                source_id: source_id.clone(),
                library_id,
            });
        }
        let transaction = self.connection.transaction()?;
        for write in writes {
            transaction.execute(
                "INSERT INTO loudness_measurements(
                     source_id, scope, item_id, analysis_key,
                     integrated_lufs, true_peak
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(source_id, scope, item_id) DO UPDATE SET
                     analysis_key = excluded.analysis_key,
                     integrated_lufs = excluded.integrated_lufs,
                     true_peak = excluded.true_peak",
                params![
                    source_id.as_str(),
                    write.item.scope(),
                    write.item.as_str(),
                    write.analysis_key.as_slice(),
                    write.measurement.integrated_lufs,
                    write.measurement.true_peak_ratio,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    fn replace_source_update(
        &mut self,
        source_id: &SourceId,
        library_id: i64,
        mut replacement: ItemReplacement,
        playlists: Vec<PlaylistSnapshot>,
        removed_playlists: Vec<PlaylistId>,
    ) -> StoreResult<StoredSourceUpdate> {
        if self.current_library_id(source_id)? != Some(library_id) {
            return Err(StoreError::WrongCurrentLibrary {
                source_id: source_id.clone(),
                library_id,
            });
        }
        let transaction = self.connection.transaction()?;
        let item_changed = !replacement.is_empty();
        write_item_replacement(&transaction, source_id, library_id, &mut replacement, None)?;
        let mut changed_removed_playlists = Vec::new();
        for playlist_id in removed_playlists {
            if source_playlist_exists(&transaction, library_id, &playlist_id)? {
                changed_removed_playlists.push(playlist_id);
            }
        }
        let mut changed_playlists = Vec::new();
        for snapshot in playlists {
            if !source_playlist_matches(&transaction, library_id, &snapshot)? {
                changed_playlists.push(snapshot);
            }
        }
        for playlist_id in &changed_removed_playlists {
            remove_source_playlist(&transaction, library_id, playlist_id)?;
        }
        for snapshot in &changed_playlists {
            remove_source_playlist(&transaction, library_id, &snapshot.playlist.id)?;
        }
        for snapshot in &changed_playlists {
            insert_source_playlist(&transaction, library_id, snapshot)?;
        }
        if item_changed || !changed_playlists.is_empty() || !changed_removed_playlists.is_empty() {
            invalidate_content_digest(&transaction, library_id)?;
        }
        transaction.commit()?;
        Ok(StoredSourceUpdate {
            replacement,
            playlists: changed_playlists,
            removed_playlists: changed_removed_playlists,
        })
    }

    fn replace_home(
        &mut self,
        source_id: &SourceId,
        library_id: i64,
        home: &HomeFacts,
    ) -> StoreResult<()> {
        if self.current_library_id(source_id)? != Some(library_id) {
            return Err(StoreError::WrongCurrentLibrary {
                source_id: source_id.clone(),
                library_id,
            });
        }
        let (home_json, home_digest) = persisted_home(home)?;
        self.connection.execute(
            "UPDATE source_libraries
             SET home_digest = ?2, home_json = ?3
             WHERE library_id = ?1 AND accepted_at IS NOT NULL",
            params![library_id, home_digest.as_slice(), home_json],
        )?;
        Ok(())
    }

    fn replace_local_component(
        &mut self,
        source_id: &SourceId,
        library_id: i64,
        observed_at: i64,
        files: Vec<LocalFile>,
        removed_paths: Vec<String>,
        mut replacement: ItemReplacement,
        favorite_update: LocalFavoriteUpdate,
    ) -> StoreResult<StoredLocalComponent> {
        if self.current_library_id(source_id)? != Some(library_id) {
            return Err(StoreError::WrongCurrentLibrary {
                source_id: source_id.clone(),
                library_id,
            });
        }
        let transaction = self.connection.transaction()?;
        for path in &removed_paths {
            transaction.execute(
                "DELETE FROM local_files WHERE library_id = ?1 AND path = ?2",
                params![library_id, path],
            )?;
        }
        for file in &files {
            transaction.execute(
                "DELETE FROM local_files WHERE library_id = ?1 AND path = ?2",
                params![library_id, file.path],
            )?;
        }
        for file in &files {
            write_local_file(&transaction, library_id, file)?;
        }
        let imports = write_item_replacement(
            &transaction,
            source_id,
            library_id,
            &mut replacement,
            Some(observed_at),
        )?;
        transfer_local_favorites(&transaction, source_id, &favorite_update.transfers)?;
        if !files.is_empty() || !removed_paths.is_empty() || !replacement.is_empty() {
            invalidate_content_digest(&transaction, library_id)?;
        }
        let favorites = load_local_favorites_for(&transaction, source_id, favorite_update.targets)?;
        let activity = replacement
            .tracks
            .iter()
            .filter_map(|track| {
                load_optional_track_activity(&transaction, source_id, &track.id).transpose()
            })
            .collect::<StoreResult<Vec<_>>>()?;
        transaction.commit()?;
        Ok(StoredLocalComponent {
            files,
            removed_paths,
            replacement,
            imports,
            favorites,
            activity,
        })
    }

    fn load_library(&self, library_id: i64) -> StoreResult<LibraryInput> {
        let (source_id, input_digest, freshness_version, freshness_marker, home_json) = self
            .connection
            .query_row(
                "SELECT
                    source_id, input_digest, freshness_version, freshness_marker, home_json
                 FROM source_libraries
                 WHERE library_id = ?1 AND accepted_at IS NOT NULL",
                [library_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<Vec<u8>>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .optional()?
            .ok_or(StoreError::CandidateMissing(library_id))?;
        let source_id = SourceId::new(source_id);
        let input_digest =
            <[u8; 32]>::try_from(input_digest).map_err(|value| StoreError::InvalidValue {
                kind: "source input digest",
                value: format!("{} bytes", value.len()),
            })?;
        let freshness = match (freshness_version, freshness_marker) {
            (None, None) => None,
            (Some(version), Some(marker)) => Some(ProviderFreshness {
                version: u32::try_from(version).map_err(|_| StoreError::InvalidValue {
                    kind: "provider freshness version",
                    value: version.to_string(),
                })?,
                marker,
            }),
            _ => {
                return Err(StoreError::InvalidValue {
                    kind: "provider freshness",
                    value: "incomplete columns".to_string(),
                });
            }
        };
        let mut input = LibraryInput::new(source_id.clone(), library_id, input_digest, freshness);
        input.albums = load_albums(&self.connection, library_id)?;
        input.tracks = load_tracks(&self.connection, library_id)?;
        input.artists = load_artists(&self.connection, library_id)?;
        input.genres = load_genres(&self.connection, library_id)?;
        input.music_folders = load_music_folders(&self.connection, library_id)?;
        input.local_files = load_local_files(&self.connection, library_id)?;
        input.playlists = load_source_playlists(&self.connection, library_id)?;
        let home = home_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?
            .ok_or_else(|| StoreError::InvalidValue {
                kind: "Home facts",
                value: "missing".to_string(),
            })?;
        complete_loaded_input(&self.connection, input, &source_id, home)
    }

    fn load_candidate_library(
        &self,
        library_id: i64,
        source_id: &SourceId,
        input_digest: Vec<u8>,
        freshness: Option<ProviderFreshness>,
        home: HomeFacts,
        accepted_at: i64,
    ) -> StoreResult<LibraryInput> {
        self.require_unaccepted(library_id)?;
        let input_digest =
            <[u8; 32]>::try_from(input_digest).map_err(|value| StoreError::InvalidValue {
                kind: "source input digest",
                value: format!("{} bytes", value.len()),
            })?;
        let rufin_defined_home = home.is_rufin_defined();
        let mut input = LibraryInput::new(source_id.clone(), library_id, input_digest, freshness);
        input.albums = load_albums(&self.connection, library_id)?;
        input.tracks = load_tracks(&self.connection, library_id)?;
        input.artists = load_artists(&self.connection, library_id)?;
        input.genres = load_genres(&self.connection, library_id)?;
        input.music_folders = load_music_folders(&self.connection, library_id)?;
        input.local_files = load_local_files(&self.connection, library_id)?;
        input.playlists = load_source_playlists(&self.connection, library_id)?;
        let mut input = complete_loaded_input(&self.connection, input, source_id, home)?;
        if rufin_defined_home {
            input.local_imports =
                load_candidate_local_imports(&self.connection, source_id, library_id, accepted_at)?;
        }
        Ok(input)
    }

    fn replace_local_access(
        &mut self,
        source_id: &SourceId,
        files: &[LocalAccessFile],
    ) -> StoreResult<bool> {
        let current = load_current_local_access(&self.connection, source_id)?;
        if current == files {
            return Ok(false);
        }
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "DELETE FROM local_access_files WHERE source_id = ?1",
            [source_id.as_str()],
        )?;
        for file in files {
            if estimate_local_access_file(file)? > STORE_BYTE_BATCH_LIMIT {
                return Err(StoreError::RowTooLarge);
            }
            write_local_access_file(&transaction, source_id, file)?;
        }
        transaction.commit()?;
        Ok(true)
    }

    fn clear_local_access(&mut self, source_id: &SourceId) -> StoreResult<()> {
        self.connection.execute(
            "DELETE FROM local_access_files WHERE source_id = ?1",
            [source_id.as_str()],
        )?;
        Ok(())
    }

    fn set_favorite(
        &mut self,
        source_id: &SourceId,
        item_id: &FavoriteItemId,
        favorite: bool,
        local: bool,
        fallback: Option<FavoriteValue>,
    ) -> StoreResult<()> {
        if local {
            let transaction = self.connection.transaction()?;
            if favorite {
                transaction.execute(
                    "INSERT OR IGNORE INTO local_favorites(
                        source_id, item_kind, item_id
                     ) VALUES (?1, ?2, ?3)",
                    params![
                        source_id.as_str(),
                        item_id.kind().as_str(),
                        item_id.as_str()
                    ],
                )?;
            } else {
                transaction.execute(
                    "DELETE FROM local_favorites
                     WHERE source_id = ?1 AND item_kind = ?2 AND item_id = ?3",
                    params![
                        source_id.as_str(),
                        item_id.kind().as_str(),
                        item_id.as_str()
                    ],
                )?;
            }
            transaction.commit()?;
            return Ok(());
        }
        let library_id =
            self.current_library_id(source_id)?
                .ok_or_else(|| StoreError::InvalidValue {
                    kind: "favorite source",
                    value: source_id.to_string(),
                })?;
        let transaction = self.connection.transaction()?;
        if persist_favorite(&transaction, library_id, item_id, favorite, fallback)? {
            invalidate_content_digest(&transaction, library_id)?;
        }
        transaction.commit()?;
        Ok(())
    }

    fn set_rating(
        &mut self,
        source_id: &SourceId,
        item_id: &FavoriteItemId,
        rating: Option<u8>,
    ) -> StoreResult<()> {
        let rating = rating.unwrap_or(0);
        if rating > 10 {
            return Err(StoreError::InvalidValue {
                kind: "rating",
                value: rating.to_string(),
            });
        }
        self.connection.execute(
            "INSERT INTO user_ratings(source_id, item_kind, item_id, rating)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(source_id, item_kind, item_id)
             DO UPDATE SET rating = excluded.rating",
            params![
                source_id.as_str(),
                item_id.kind().as_str(),
                item_id.as_str(),
                i64::from(rating)
            ],
        )?;
        Ok(())
    }

    fn queue_remote_favorite(
        &mut self,
        source_id: &SourceId,
        item_id: &FavoriteItemId,
        favorite: bool,
        previous: bool,
        next_attempt_at: i64,
        fallback: Option<FavoriteValue>,
    ) -> StoreResult<()> {
        let library_id =
            self.current_library_id(source_id)?
                .ok_or_else(|| StoreError::InvalidValue {
                    kind: "favorite source",
                    value: source_id.to_string(),
                })?;
        let transaction = self.connection.transaction()?;
        if persist_favorite(&transaction, library_id, item_id, favorite, fallback)? {
            invalidate_content_digest(&transaction, library_id)?;
        }
        transaction.execute(
            "INSERT INTO pending_favorites(
                 source_id, item_kind, item_id, favorite, previous_favorite,
                 attempts, next_attempt_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6)
             ON CONFLICT(source_id, item_kind, item_id)
             DO UPDATE SET favorite = excluded.favorite,
                           next_attempt_at = excluded.next_attempt_at",
            params![
                source_id.as_str(),
                item_id.kind().as_str(),
                item_id.as_str(),
                i64::from(favorite),
                i64::from(previous),
                next_attempt_at,
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn due_remote_favorites(
        &mut self,
        source_id: &SourceId,
        now: i64,
        limit: usize,
    ) -> StoreResult<Vec<PendingFavorite>> {
        let mut statement = self.connection.prepare(
            "SELECT item_kind, item_id, favorite, attempts
             FROM pending_favorites
             WHERE source_id = ?1 AND next_attempt_at <= ?2
             ORDER BY next_attempt_at, item_kind, item_id
             LIMIT ?3",
        )?;
        statement
            .query_map(
                params![
                    source_id.as_str(),
                    now,
                    i64::try_from(limit).map_err(|_| StoreError::IntegerRange)?,
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )?
            .map(|row| {
                let (kind, id, favorite, attempts) = row?;
                Ok(PendingFavorite {
                    item: favorite_item_id(&kind, id)?,
                    favorite: favorite != 0,
                    attempts: checked_u32(attempts)?,
                })
            })
            .collect()
    }

    fn complete_remote_favorite(
        &mut self,
        source_id: &SourceId,
        item_id: &FavoriteItemId,
        favorite: bool,
    ) -> StoreResult<()> {
        let library_id =
            self.current_library_id(source_id)?
                .ok_or_else(|| StoreError::InvalidValue {
                    kind: "favorite source",
                    value: source_id.to_string(),
                })?;
        let transaction = self.connection.transaction()?;
        let removed = transaction.execute(
            "DELETE FROM pending_favorites
             WHERE source_id = ?1 AND item_kind = ?2 AND item_id = ?3
               AND favorite = ?4",
            params![
                source_id.as_str(),
                item_id.kind().as_str(),
                item_id.as_str(),
                i64::from(favorite),
            ],
        )?;
        if removed > 0
            && favorite_row_exists(&transaction, library_id, item_id)?
            && persist_favorite(&transaction, library_id, item_id, favorite, None)?
        {
            invalidate_content_digest(&transaction, library_id)?;
        }
        transaction.commit()?;
        Ok(())
    }

    fn defer_remote_favorite(
        &mut self,
        source_id: &SourceId,
        item_id: &FavoriteItemId,
        favorite: bool,
        next_attempt_at: i64,
    ) -> StoreResult<()> {
        self.connection.execute(
            "UPDATE pending_favorites
             SET attempts = attempts + 1, next_attempt_at = ?5
             WHERE source_id = ?1 AND item_kind = ?2 AND item_id = ?3
               AND favorite = ?4",
            params![
                source_id.as_str(),
                item_id.kind().as_str(),
                item_id.as_str(),
                i64::from(favorite),
                next_attempt_at,
            ],
        )?;
        Ok(())
    }

    fn reject_remote_favorite(
        &mut self,
        source_id: &SourceId,
        item_id: &FavoriteItemId,
        favorite: bool,
    ) -> StoreResult<Option<bool>> {
        let library_id =
            self.current_library_id(source_id)?
                .ok_or_else(|| StoreError::InvalidValue {
                    kind: "favorite source",
                    value: source_id.to_string(),
                })?;
        let transaction = self.connection.transaction()?;
        let previous = transaction
            .query_row(
                "SELECT previous_favorite FROM pending_favorites
                 WHERE source_id = ?1 AND item_kind = ?2 AND item_id = ?3
                   AND favorite = ?4",
                params![
                    source_id.as_str(),
                    item_id.kind().as_str(),
                    item_id.as_str(),
                    i64::from(favorite),
                ],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let Some(previous) = previous else {
            return Ok(None);
        };
        transaction.execute(
            "DELETE FROM pending_favorites
             WHERE source_id = ?1 AND item_kind = ?2 AND item_id = ?3
               AND favorite = ?4",
            params![
                source_id.as_str(),
                item_id.kind().as_str(),
                item_id.as_str(),
                i64::from(favorite),
            ],
        )?;
        let previous = previous != 0;
        if favorite_row_exists(&transaction, library_id, item_id)?
            && persist_favorite(&transaction, library_id, item_id, previous, None)?
        {
            invalidate_content_digest(&transaction, library_id)?;
        }
        transaction.commit()?;
        Ok(Some(previous))
    }

    fn replace_local_playlist(
        &mut self,
        source_id: &SourceId,
        snapshot: PlaylistSnapshot,
    ) -> StoreResult<()> {
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "DELETE FROM local_playlist_entries
             WHERE source_id = ?1 AND playlist_id = ?2",
            params![source_id.as_str(), snapshot.playlist.id.as_str()],
        )?;
        transaction.execute(
            "INSERT INTO local_playlists(source_id, playlist_id, name)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(source_id, playlist_id)
             DO UPDATE SET name = excluded.name",
            params![
                source_id.as_str(),
                snapshot.playlist.id.as_str(),
                snapshot.playlist.name
            ],
        )?;
        for (position, entry) in snapshot.entries.iter().enumerate() {
            transaction.execute(
                "INSERT INTO local_playlist_entries(
                    source_id, playlist_id, position, occurrence_id, track_id
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    source_id.as_str(),
                    snapshot.playlist.id.as_str(),
                    i64::try_from(position).map_err(|_| StoreError::IntegerRange)?,
                    entry.occurrence_id,
                    entry.track_id.as_str(),
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    fn remove_local_playlist(
        &mut self,
        source_id: &SourceId,
        playlist_id: &PlaylistId,
    ) -> StoreResult<()> {
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "DELETE FROM local_playlist_entries
             WHERE source_id = ?1 AND playlist_id = ?2",
            params![source_id.as_str(), playlist_id.as_str()],
        )?;
        transaction.execute(
            "DELETE FROM local_playlists
             WHERE source_id = ?1 AND playlist_id = ?2",
            params![source_id.as_str(), playlist_id.as_str()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn put_smart_playlist(
        &mut self,
        source_id: &SourceId,
        record: &SmartPlaylistRecord,
    ) -> StoreResult<()> {
        let definition_json = crate::smart_playlists::validated_smart_playlist_json(
            &record.definition,
        )
        .map_err(|value| StoreError::InvalidValue {
            kind: "smart playlist definition",
            value,
        })?;
        self.connection.execute(
            "INSERT INTO smart_playlists(
                source_id, smart_playlist_id, name, builtin_key,
                definition_json, position
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(source_id, smart_playlist_id)
             DO UPDATE SET
                name = excluded.name,
                builtin_key = excluded.builtin_key,
                definition_json = excluded.definition_json,
                position = excluded.position",
            params![
                source_id.as_str(),
                record.id.as_str(),
                record.name,
                record.builtin.map(SmartPlaylistBuiltin::key),
                definition_json,
                i64::from(record.position),
            ],
        )?;
        Ok(())
    }

    fn remove_smart_playlist(
        &mut self,
        source_id: &SourceId,
        id: &SmartPlaylistId,
    ) -> StoreResult<()> {
        self.connection.execute(
            "DELETE FROM smart_playlists
             WHERE source_id = ?1 AND smart_playlist_id = ?2",
            params![source_id.as_str(), id.as_str()],
        )?;
        Ok(())
    }

    fn replace_smart_playlist_order(
        &mut self,
        source_id: &SourceId,
        ordered_ids: &[SmartPlaylistId],
    ) -> StoreResult<()> {
        let transaction = self.connection.transaction()?;
        let stored_count = transaction.query_row(
            "SELECT COUNT(*) FROM smart_playlists WHERE source_id = ?1",
            [source_id.as_str()],
            |row| row.get::<_, i64>(0),
        )?;
        if stored_count != i64::try_from(ordered_ids.len()).map_err(|_| StoreError::IntegerRange)? {
            return Err(StoreError::InvalidValue {
                kind: "smart playlist order",
                value: "the order does not contain every saved smart playlist".to_string(),
            });
        }
        let shift = transaction.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1
             FROM smart_playlists
             WHERE source_id = ?1",
            [source_id.as_str()],
            |row| row.get::<_, i64>(0),
        )?;
        transaction.execute(
            "UPDATE smart_playlists
             SET position = position + ?1
             WHERE source_id = ?2",
            params![shift, source_id.as_str()],
        )?;
        for (position, id) in ordered_ids.iter().enumerate() {
            let changed = transaction.execute(
                "UPDATE smart_playlists
                 SET position = ?1
                 WHERE source_id = ?2 AND smart_playlist_id = ?3",
                params![
                    i64::try_from(position).map_err(|_| StoreError::IntegerRange)?,
                    source_id.as_str(),
                    id.as_str(),
                ],
            )?;
            if changed != 1 {
                return Err(StoreError::InvalidValue {
                    kind: "smart playlist order",
                    value: format!("{} is not saved for this source", id.as_str()),
                });
            }
        }
        transaction.commit()?;
        Ok(())
    }

    fn record_activity(
        &mut self,
        source_id: &SourceId,
        activity: &ActivityWrite,
    ) -> StoreResult<Option<TrackActivity>> {
        let transaction = self.connection.transaction()?;
        if activity.skipped {
            transaction.execute(
                "INSERT INTO listening_aggregates(
                    source_id, period, item_kind, item_id, display_name,
                    display_context, play_count, skip_count, last_played_at
                 ) VALUES (?1, 'lifetime', 'track', ?2, ?3, ?4, 0, 1, NULL)
                 ON CONFLICT(source_id, period, item_kind, item_id)
                 DO UPDATE SET skip_count =
                    COALESCE(listening_aggregates.skip_count, 0) + 1",
                params![
                    source_id.as_str(),
                    activity.track_id.as_str(),
                    activity.track_title,
                    activity.artist_name,
                ],
            )?;
            let replacement = load_one_track_activity(&transaction, source_id, &activity.track_id)?;
            transaction.commit()?;
            return Ok(Some(replacement));
        }

        let played_at = activity.played_at.ok_or_else(|| StoreError::InvalidValue {
            kind: "accepted play time",
            value: "missing".to_string(),
        })?;
        let month = activity
            .month
            .as_deref()
            .ok_or_else(|| StoreError::InvalidValue {
                kind: "accepted play month",
                value: "missing".to_string(),
            })?;
        let play_id = activity
            .play_id
            .as_deref()
            .ok_or_else(|| StoreError::InvalidValue {
                kind: "accepted play ID",
                value: "missing".to_string(),
            })?;
        let inserted = transaction.execute(
            "INSERT OR IGNORE INTO recent_plays(
                play_id, source_id, track_id, track_title, artist_name,
                album_title, played_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                play_id,
                source_id.as_str(),
                activity.track_id.as_str(),
                activity.track_title,
                activity.artist_name,
                activity.album_title,
                played_at,
            ],
        )?;
        if inserted == 0 {
            transaction.commit()?;
            return Ok(None);
        }
        for credit in &activity.credits {
            transaction.execute(
                "INSERT INTO listening_aggregates(
                    source_id, period, item_kind, item_id, display_name,
                    display_context, play_count, skip_count, last_played_at
                 ) VALUES (
                    ?1, 'lifetime', ?2, ?3, ?4, ?5, 1,
                    CASE WHEN ?2 = 'track' THEN 0 ELSE NULL END,
                    ?6
                 )
                 ON CONFLICT(source_id, period, item_kind, item_id)
                 DO UPDATE SET
                    display_name = excluded.display_name,
                    display_context = excluded.display_context,
                    play_count = listening_aggregates.play_count + 1,
                    last_played_at = MAX(
                        COALESCE(listening_aggregates.last_played_at, 0),
                        excluded.last_played_at
                    )",
                params![
                    source_id.as_str(),
                    credit.kind,
                    credit.id,
                    credit.name,
                    credit.context,
                    played_at,
                ],
            )?;
            transaction.execute(
                "INSERT INTO listening_aggregates(
                    source_id, period, item_kind, item_id, display_name,
                    display_context, play_count, skip_count, last_played_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, NULL, NULL)
                 ON CONFLICT(source_id, period, item_kind, item_id)
                 DO UPDATE SET
                    display_name = excluded.display_name,
                    display_context = excluded.display_context,
                    play_count = listening_aggregates.play_count + 1",
                params![
                    source_id.as_str(),
                    month,
                    credit.kind,
                    credit.id,
                    credit.name,
                    credit.context,
                ],
            )?;
        }
        transaction.execute(
            "DELETE FROM recent_plays
             WHERE play_id IN (
                SELECT play_id FROM recent_plays
                WHERE source_id = ?1
                ORDER BY played_at DESC, play_id DESC
                LIMIT -1 OFFSET 100
             )",
            [source_id.as_str()],
        )?;
        let replacement = load_one_track_activity(&transaction, source_id, &activity.track_id)?;
        transaction.commit()?;
        Ok(Some(replacement))
    }

    fn activity_summary(
        &self,
        source_id: &SourceId,
        period: &ActivityPeriod,
    ) -> StoreResult<ActivitySummary> {
        Ok(ActivitySummary {
            tracks: load_activity_items(&self.connection, source_id, period, "track")?,
            artists: load_activity_items(&self.connection, source_id, period, "artist")?,
            genres: load_activity_items(&self.connection, source_id, period, "genre")?,
        })
    }

    fn load_playback(&mut self, source_id: &SourceId) -> StoreResult<PlaybackLoad> {
        let queue = self
            .connection
            .query_row(
                "SELECT revision, rows_json, traversal_json
                 FROM playback_queues WHERE source_id = ?1",
                [source_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        let state = self
            .connection
            .query_row(
                "SELECT
                    revision, selected_occurrence_id, progress_millis
                 FROM playback_state WHERE source_id = ?1",
                [source_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?;
        let ((revision, rows, traversal), state) = match (queue, state) {
            (Some(queue), Some(state)) => (queue, state),
            (None, None) => return Ok(PlaybackLoad::Missing),
            _ => {
                self.remove_playback(source_id)?;
                return Ok(PlaybackLoad::DiscardedCorrupt);
            }
        };
        let (state_revision, selected, progress_millis) = state;
        let parsed = (|| {
            if revision != state_revision {
                return None;
            }
            let rows = serde_json::from_str::<PlaybackQueueRowsSnapshot>(&rows).ok()?;
            let traversal = serde_json::from_str::<Vec<PlaybackOccurrenceId>>(&traversal).ok()?;
            let checkpoint = PlaybackCheckpoint {
                source_id: source_id.clone(),
                revision: u64::try_from(revision).ok()?,
                queue: rows.with_traversal(traversal),
                state: PlaybackState {
                    selected: selected.map(PlaybackOccurrenceId::new),
                    progress_millis: u64::try_from(progress_millis).ok()?,
                },
            };
            crate::playback_state::validate_checkpoint(&checkpoint)
                .ok()
                .map(|()| checkpoint)
        })();
        match parsed {
            Some(checkpoint) => Ok(PlaybackLoad::Ready(checkpoint)),
            None => {
                self.remove_playback(source_id)?;
                Ok(PlaybackLoad::DiscardedCorrupt)
            }
        }
    }

    fn replace_playback(
        &mut self,
        checkpoint: &PlaybackCheckpoint,
    ) -> StoreResult<PlaybackWriteOutcome> {
        let revision = i64::try_from(checkpoint.revision).map_err(|_| StoreError::IntegerRange)?;
        let current = self
            .connection
            .query_row(
                "SELECT revision FROM playback_queues WHERE source_id = ?1",
                [checkpoint.source_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if current.is_some_and(|current| current >= revision) {
            return Ok(PlaybackWriteOutcome::Stale);
        }
        let rows = serde_json::to_string(&checkpoint.queue.rows())?;
        let traversal = serde_json::to_string(&checkpoint.queue.traversal)?;
        let selected = checkpoint
            .state
            .selected
            .as_ref()
            .map(PlaybackOccurrenceId::as_str);
        let progress = i64::try_from(checkpoint.state.progress_millis)
            .map_err(|_| StoreError::IntegerRange)?;
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO playback_queues(
                source_id, revision, rows_json, traversal_json
             ) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(source_id)
             DO UPDATE SET
                revision = excluded.revision,
                rows_json = excluded.rows_json,
                traversal_json = excluded.traversal_json",
            params![checkpoint.source_id.as_str(), revision, rows, traversal,],
        )?;
        transaction.execute(
            "INSERT INTO playback_state(
                source_id, revision, selected_occurrence_id, progress_millis
             ) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(source_id)
             DO UPDATE SET
                revision = excluded.revision,
                selected_occurrence_id = excluded.selected_occurrence_id,
                progress_millis = excluded.progress_millis",
            params![checkpoint.source_id.as_str(), revision, selected, progress,],
        )?;
        transaction.commit()?;
        Ok(PlaybackWriteOutcome::Applied)
    }

    fn replace_playback_traversal(
        &mut self,
        update: &PlaybackTraversalUpdate,
    ) -> StoreResult<PlaybackWriteOutcome> {
        if update.revision <= update.expected_revision {
            return Ok(PlaybackWriteOutcome::Stale);
        }
        let expected_revision =
            i64::try_from(update.expected_revision).map_err(|_| StoreError::IntegerRange)?;
        let revision = i64::try_from(update.revision).map_err(|_| StoreError::IntegerRange)?;
        let progress =
            i64::try_from(update.state.progress_millis).map_err(|_| StoreError::IntegerRange)?;
        let traversal_json = serde_json::to_string(&update.traversal)?;
        let transaction = self.connection.transaction()?;
        let queue_changed = transaction.execute(
            "UPDATE playback_queues
             SET revision = ?3, traversal_json = ?4
             WHERE source_id = ?1 AND revision = ?2",
            params![
                update.source_id.as_str(),
                expected_revision,
                revision,
                traversal_json,
            ],
        )?;
        let state_changed = transaction.execute(
            "UPDATE playback_state
             SET revision = ?3,
                 selected_occurrence_id = ?4,
                 progress_millis = ?5
             WHERE source_id = ?1 AND revision = ?2",
            params![
                update.source_id.as_str(),
                expected_revision,
                revision,
                update
                    .state
                    .selected
                    .as_ref()
                    .map(PlaybackOccurrenceId::as_str),
                progress,
            ],
        )?;
        if (queue_changed, state_changed) != (1, 1) {
            transaction.rollback()?;
            return Ok(PlaybackWriteOutcome::Stale);
        }
        transaction.commit()?;
        Ok(PlaybackWriteOutcome::Applied)
    }

    fn update_playback_state(
        &mut self,
        update: &PlaybackStateUpdate,
    ) -> StoreResult<PlaybackWriteOutcome> {
        let changed = self.connection.execute(
            "UPDATE playback_state
             SET selected_occurrence_id = ?3,
                 progress_millis = ?4
             WHERE source_id = ?1 AND revision = ?2",
            params![
                update.source_id.as_str(),
                i64::try_from(update.revision).map_err(|_| StoreError::IntegerRange)?,
                update.selected.as_ref().map(PlaybackOccurrenceId::as_str),
                i64::try_from(update.progress_millis).map_err(|_| StoreError::IntegerRange)?,
            ],
        )?;
        Ok(if changed == 1 {
            PlaybackWriteOutcome::Applied
        } else {
            PlaybackWriteOutcome::Stale
        })
    }

    fn update_playback_progress(
        &mut self,
        update: &PlaybackProgressUpdate,
    ) -> StoreResult<PlaybackWriteOutcome> {
        let changed = self.connection.execute(
            "UPDATE playback_state
             SET progress_millis = ?4
             WHERE source_id = ?1
               AND revision = ?2
               AND selected_occurrence_id = ?3",
            params![
                update.source_id.as_str(),
                i64::try_from(update.revision).map_err(|_| StoreError::IntegerRange)?,
                update.occurrence.as_str(),
                i64::try_from(update.progress_millis).map_err(|_| StoreError::IntegerRange)?,
            ],
        )?;
        Ok(if changed == 1 {
            PlaybackWriteOutcome::Applied
        } else {
            PlaybackWriteOutcome::Stale
        })
    }

    fn remove_playback(&mut self, source_id: &SourceId) -> StoreResult<bool> {
        let transaction = self.connection.transaction()?;
        let state = transaction.execute(
            "DELETE FROM playback_state WHERE source_id = ?1",
            [source_id.as_str()],
        )?;
        let queue = transaction.execute(
            "DELETE FROM playback_queues WHERE source_id = ?1",
            [source_id.as_str()],
        )?;
        transaction.commit()?;
        Ok(state != 0 || queue != 0)
    }

    fn cached_lyrics(
        &mut self,
        key: &LyricsCacheKey,
        expected_input: &LyricsCacheInput,
    ) -> StoreResult<Option<crate::CachedLyrics>> {
        let stored = self
            .connection
            .query_row(
                "SELECT
                    origin, input_digest, payload, cached_at
                 FROM lyrics_cache
                 WHERE source_id = ?1
                   AND track_id = ?2
                   AND role = ?3
                   AND language = ?4
                   AND script = ?5",
                params![
                    key.source_id.as_str(),
                    key.track_id.as_str(),
                    key.role.as_str(),
                    key.language,
                    key.script,
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((origin, input_digest, payload, cached_at)) = stored else {
            return Ok(None);
        };
        let decoded = (|| {
            let digest = <[u8; 32]>::try_from(input_digest).ok()?;
            if digest != expected_input.digest {
                return None;
            }
            Some(crate::CachedLyrics {
                key: key.clone(),
                authority: LyricsCacheAuthority::from_stored(&origin)?,
                input: LyricsCacheInput { digest },
                payload,
                cached_at,
            })
        })();
        if decoded.is_none() {
            self.remove_lyrics(key)?;
        }
        Ok(decoded)
    }

    fn store_lyrics(&mut self, write: &LyricsCacheWrite) -> StoreResult<LyricsCacheTrim> {
        if write.payload.len() > 8 * 1024 * 1024 {
            return Err(StoreError::RowTooLarge);
        }
        self.connection.execute(
            "INSERT INTO lyrics_cache(
                source_id, track_id, role, language, script, origin,
                input_version, input_digest, payload,
                cached_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?8, ?9)
             ON CONFLICT(source_id, track_id, role, language, script)
             DO UPDATE SET
                origin = excluded.origin,
                input_digest = excluded.input_digest,
                payload = excluded.payload,
                cached_at = excluded.cached_at",
            params![
                write.key.source_id.as_str(),
                write.key.track_id.as_str(),
                write.key.role.as_str(),
                write.key.language,
                write.key.script,
                write.authority.as_str(),
                write.input.digest.as_slice(),
                write.payload,
                write.cached_at,
            ],
        )?;

        let mut trimmed = LyricsCacheTrim::default();
        loop {
            let (rows, bytes) = lyrics_cache_usage(&self.connection)?;
            if rows <= 10_000 && bytes <= 64 * 1024 * 1024 {
                break;
            }
            let victims = {
                let mut statement = self.connection.prepare(
                    "SELECT rowid, length(CAST(payload AS BLOB))
                     FROM lyrics_cache
                     ORDER BY cached_at, rowid
                     LIMIT 500",
                )?;
                statement
                    .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?
                    .collect::<Result<Vec<_>, _>>()?
            };
            if victims.is_empty() {
                break;
            }
            let transaction = self.connection.transaction()?;
            for (rowid, bytes) in &victims {
                transaction.execute("DELETE FROM lyrics_cache WHERE rowid = ?1", [rowid])?;
                trimmed.rows_removed = trimmed.rows_removed.saturating_add(1);
                trimmed.bytes_removed = trimmed
                    .bytes_removed
                    .saturating_add(u64::try_from(*bytes).unwrap_or(0));
            }
            transaction.commit()?;
        }
        Ok(trimmed)
    }

    fn remove_lyrics_if_authority(
        &mut self,
        key: &LyricsCacheKey,
        authority: LyricsCacheAuthority,
    ) -> StoreResult<bool> {
        Ok(self.connection.execute(
            "DELETE FROM lyrics_cache
             WHERE source_id = ?1
               AND track_id = ?2
               AND role = ?3
               AND language = ?4
               AND script = ?5
               AND origin = ?6",
            params![
                key.source_id.as_str(),
                key.track_id.as_str(),
                key.role.as_str(),
                key.language,
                key.script,
                authority.as_str(),
            ],
        )? == 1)
    }

    fn remove_track_lyrics_by_authority(
        &mut self,
        source_id: &SourceId,
        track_id: &TrackId,
        authority: LyricsCacheAuthority,
    ) -> StoreResult<u64> {
        self.connection
            .execute(
                "DELETE FROM lyrics_cache
                 WHERE source_id = ?1
                   AND track_id = ?2
                   AND origin = ?3",
                params![source_id.as_str(), track_id.as_str(), authority.as_str()],
            )
            .map(|removed| removed as u64)
            .map_err(Into::into)
    }

    fn remove_lyrics(&mut self, key: &LyricsCacheKey) -> StoreResult<()> {
        self.connection.execute(
            "DELETE FROM lyrics_cache
             WHERE source_id = ?1
               AND track_id = ?2
               AND role = ?3
               AND language = ?4
               AND script = ?5",
            params![
                key.source_id.as_str(),
                key.track_id.as_str(),
                key.role.as_str(),
                key.language,
                key.script,
            ],
        )?;
        Ok(())
    }

    fn album_release_candidates(
        &self,
        source_id: &SourceId,
        library_id: i64,
        limit: usize,
    ) -> StoreResult<Vec<AlbumReleaseCandidate>> {
        if self.current_library_id(source_id)? != Some(library_id) || limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit.min(500)).map_err(|_| StoreError::IntegerRange)?;
        let mut statement = self.connection.prepare(
            "SELECT album.album_id,
                    album.musicbrainz_release_group_id,
                    album.musicbrainz_release_id
             FROM albums AS album
             LEFT JOIN album_release_info AS release
               ON release.source_id = ?1 AND release.album_id = album.album_id
             WHERE album.library_id = ?2
               AND json_array_length(album.release_types_json) = 0
               AND (
                   NULLIF(album.musicbrainz_release_group_id, '') IS NOT NULL
                   OR NULLIF(album.musicbrainz_release_id, '') IS NOT NULL
               )
               AND (
                   release.exact_identity_key IS NULL
                   OR release.exact_identity_key <> CASE
                       WHEN NULLIF(album.musicbrainz_release_group_id, '') IS NOT NULL
                       THEN 'release-group:' || album.musicbrainz_release_group_id
                       ELSE 'release:' || album.musicbrainz_release_id
                   END
               )
             ORDER BY album.album_id
             LIMIT ?3",
        )?;
        statement
            .query_map(params![source_id.as_str(), library_id, limit], |row| {
                let release_group_id = row.get::<_, Option<String>>(1)?;
                let release_id = row.get::<_, Option<String>>(2)?;
                let identity = release_group_id
                    .filter(|id| !id.is_empty())
                    .map(crate::AlbumReleaseIdentity::ReleaseGroup)
                    .or_else(|| {
                        release_id
                            .filter(|id| !id.is_empty())
                            .map(crate::AlbumReleaseIdentity::Release)
                    })
                    .expect("Album release candidate query requires one exact identity");
                Ok(AlbumReleaseCandidate {
                    source_id: source_id.clone(),
                    album_id: AlbumId::new(row.get::<_, String>(0)?),
                    identity,
                    library_id,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    fn accept_album_release(
        &mut self,
        candidate: &AlbumReleaseCandidate,
        result: &AlbumReleaseResult,
    ) -> StoreResult<bool> {
        if self.current_library_id(&candidate.source_id)? != Some(candidate.library_id) {
            return Ok(false);
        }
        let current_identity = self
            .connection
            .query_row(
                "SELECT
                    release_types_json,
                    musicbrainz_release_id,
                    musicbrainz_release_group_id
                 FROM albums
                 WHERE library_id = ?1 AND album_id = ?2",
                params![candidate.library_id, candidate.album_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((release_types, release_id, release_group_id)) = current_identity else {
            return Ok(false);
        };
        if !serde_json::from_str::<Vec<String>>(&release_types)?.is_empty() {
            return Ok(false);
        }
        let current_key = release_group_id
            .map(|id| format!("release-group:{id}"))
            .or_else(|| release_id.map(|id| format!("release:{id}")));
        let expected_key = candidate.identity.stored_key();
        if current_key.as_deref() != Some(expected_key.as_str()) {
            return Ok(false);
        }
        let (lookup_state, release_types, is_compilation) = match result {
            AlbumReleaseResult::Found { release_types } => (
                "found",
                Some(serde_json::to_string(release_types)?),
                Some(i64::from(
                    release_types.iter().any(|kind| kind == "compilation"),
                )),
            ),
            AlbumReleaseResult::Missing => ("missing", None, None),
        };
        self.connection.execute(
            "INSERT INTO album_release_info(
                source_id, album_id, exact_identity_key, lookup_state,
                release_types_json, is_compilation
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(source_id, album_id)
             DO UPDATE SET
                exact_identity_key = excluded.exact_identity_key,
                lookup_state = excluded.lookup_state,
                release_types_json = excluded.release_types_json,
                is_compilation = excluded.is_compilation",
            params![
                candidate.source_id.as_str(),
                candidate.album_id.as_str(),
                expected_key,
                lookup_state,
                release_types,
                is_compilation,
            ],
        )?;
        Ok(true)
    }

    fn queue_scrobbles(&mut self, scrobbles: &[NewScrobble]) -> StoreResult<usize> {
        let transaction = self.connection.transaction()?;
        let mut inserted = 0;
        {
            let mut statement = transaction.prepare(
                "INSERT OR IGNORE INTO pending_scrobbles(
                    service, account_id, play_id, track_title, artist_name,
                    album_title, duration_millis, started_at, attempts,
                    next_attempt_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?8)",
            )?;
            for scrobble in scrobbles {
                inserted += statement.execute(params![
                    scrobble.id.service.as_str(),
                    scrobble.id.account_id,
                    scrobble.id.play_id,
                    scrobble.track_title,
                    scrobble.artist_name,
                    scrobble.album_title,
                    i64::try_from(scrobble.duration_millis)
                        .map_err(|_| StoreError::IntegerRange)?,
                    scrobble.started_at,
                ])?;
            }
        }
        transaction.commit()?;
        Ok(inserted)
    }

    fn due_scrobbles(
        &mut self,
        service: ScrobbleService,
        account_id: &str,
        now: i64,
        limit: usize,
    ) -> StoreResult<Vec<PendingScrobble>> {
        let mut statement = self.connection.prepare(
            "SELECT
                service, account_id, play_id, track_title, artist_name,
                album_title, duration_millis, started_at, attempts,
                next_attempt_at
             FROM pending_scrobbles
             WHERE service = ?1
               AND account_id = ?2
               AND next_attempt_at IS NOT NULL
               AND next_attempt_at <= ?3
             ORDER BY next_attempt_at, started_at, play_id
             LIMIT ?4",
        )?;
        let rows = statement.query_map(
            params![
                service.as_str(),
                account_id,
                now,
                i64::try_from(limit).map_err(|_| StoreError::IntegerRange)?
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                ))
            },
        )?;
        rows.map(|row| {
            let (
                service,
                account_id,
                play_id,
                track_title,
                artist_name,
                album_title,
                duration_millis,
                started_at,
                attempts,
                next_attempt_at,
            ) = row?;
            Ok(PendingScrobble {
                id: PendingScrobbleId {
                    service: ScrobbleService::from_stored(&service).ok_or_else(|| {
                        StoreError::InvalidValue {
                            kind: "scrobble service",
                            value: service,
                        }
                    })?,
                    account_id,
                    play_id,
                },
                track_title,
                artist_name,
                album_title,
                duration_millis: checked_u64(duration_millis)?,
                started_at,
                attempts: checked_u32(attempts)?,
                next_attempt_at,
            })
        })
        .collect()
    }

    fn complete_scrobble(&mut self, id: &PendingScrobbleId) -> StoreResult<()> {
        self.connection.execute(
            "DELETE FROM pending_scrobbles
             WHERE service = ?1 AND account_id = ?2 AND play_id = ?3",
            params![id.service.as_str(), id.account_id, id.play_id],
        )?;
        Ok(())
    }

    fn discard_scrobbles(
        &mut self,
        service: ScrobbleService,
        account_id: &str,
    ) -> StoreResult<usize> {
        Ok(self.connection.execute(
            "DELETE FROM pending_scrobbles
             WHERE service = ?1 AND account_id = ?2",
            params![service.as_str(), account_id],
        )?)
    }

    fn defer_scrobble(&mut self, id: &PendingScrobbleId, next_attempt_at: i64) -> StoreResult<()> {
        self.connection.execute(
            "UPDATE pending_scrobbles
             SET attempts = attempts + 1,
                 next_attempt_at = ?4,
                 last_error = NULL
             WHERE service = ?1 AND account_id = ?2 AND play_id = ?3",
            params![
                id.service.as_str(),
                id.account_id,
                id.play_id,
                next_attempt_at,
            ],
        )?;
        Ok(())
    }

    fn block_scrobbles(
        &mut self,
        service: ScrobbleService,
        account_id: &str,
        error: &str,
    ) -> StoreResult<usize> {
        Ok(self.connection.execute(
            "UPDATE pending_scrobbles
             SET next_attempt_at = NULL,
                 last_error = ?3
             WHERE service = ?1 AND account_id = ?2",
            params![service.as_str(), account_id, error],
        )?)
    }

    fn wake_scrobbles(
        &mut self,
        service: ScrobbleService,
        account_id: &str,
        now: i64,
    ) -> StoreResult<usize> {
        Ok(self.connection.execute(
            "UPDATE pending_scrobbles
             SET next_attempt_at = ?3,
                 last_error = NULL
             WHERE service = ?1
               AND account_id = ?2
               AND next_attempt_at IS NULL",
            params![service.as_str(), account_id, now],
        )?)
    }

    fn queue_cleanup(&mut self, library_id: i64) {
        if self.cleanup_set.insert(library_id) {
            self.cleanup.push_back(library_id);
        }
    }

    fn clean_one_batch(&mut self) {
        let Some(target) = self.cleanup.pop_front() else {
            return;
        };
        let cleaned = cleanup_library_batch(&mut self.connection, target);
        match cleaned {
            Ok(true) => {
                self.cleanup_set.remove(&target);
            }
            Ok(false) | Err(_) => self.cleanup.push_back(target),
        }
    }
}

fn cleanup_targets(connection: &Connection) -> StoreResult<Vec<i64>> {
    let mut statement = connection.prepare(
        "SELECT candidate.library_id
         FROM source_libraries AS candidate
         WHERE candidate.accepted_at IS NULL
            OR EXISTS (
                SELECT 1
                FROM source_libraries AS newer
                WHERE newer.source_id = candidate.source_id
                  AND newer.accepted_at IS NOT NULL
                  AND newer.library_id > candidate.library_id
            )
         ORDER BY candidate.library_id",
    )?;
    Ok(statement
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?)
}

fn lyrics_cache_usage(connection: &Connection) -> StoreResult<(u64, u64)> {
    let (rows, bytes) = connection.query_row(
        "SELECT
            count(*),
            COALESCE(sum(length(CAST(payload AS BLOB))), 0)
         FROM lyrics_cache",
        [],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    )?;
    Ok((checked_u64(rows)?, checked_u64(bytes)?))
}

fn load_activity_items(
    connection: &Connection,
    source_id: &SourceId,
    period: &ActivityPeriod,
    item_kind: &'static str,
) -> StoreResult<Vec<ActivityItem>> {
    type StoredActivityRow = (
        String,
        String,
        Option<String>,
        i64,
        Option<i64>,
        Option<i64>,
    );

    let rows = match period {
        ActivityPeriod::Lifetime => {
            let mut statement = connection.prepare(
                "SELECT
                    item_id, display_name, display_context,
                    play_count, skip_count, last_played_at
                 FROM listening_aggregates
                 WHERE source_id = ?1 AND period = 'lifetime' AND item_kind = ?2
                 ORDER BY play_count DESC, display_name COLLATE NOCASE, item_id
                 LIMIT 5",
            )?;
            statement
                .query_map(params![source_id.as_str(), item_kind], activity_row)?
                .collect::<Result<Vec<_>, _>>()?
        }
        ActivityPeriod::Month(month) => {
            let mut statement = connection.prepare(
                "SELECT
                    item_id, display_name, display_context,
                    play_count, skip_count, last_played_at
                 FROM listening_aggregates
                 WHERE source_id = ?1 AND period = ?2 AND item_kind = ?3
                 ORDER BY play_count DESC, display_name COLLATE NOCASE, item_id
                 LIMIT 5",
            )?;
            statement
                .query_map(params![source_id.as_str(), month, item_kind], activity_row)?
                .collect::<Result<Vec<_>, _>>()?
        }
        ActivityPeriod::Year(year) => {
            let first = format!("{year:04}-01");
            let last = format!("{year:04}-12");
            let mut statement = connection.prepare(
                "SELECT
                    monthly.item_id,
                    COALESCE(lifetime.display_name, MAX(monthly.display_name)),
                    COALESCE(lifetime.display_context, MAX(monthly.display_context)),
                    SUM(monthly.play_count),
                    NULL,
                    NULL
                 FROM listening_aggregates AS monthly
                 LEFT JOIN listening_aggregates AS lifetime
                   ON lifetime.source_id = monthly.source_id
                  AND lifetime.period = 'lifetime'
                  AND lifetime.item_kind = monthly.item_kind
                  AND lifetime.item_id = monthly.item_id
                 WHERE monthly.source_id = ?1
                   AND monthly.period BETWEEN ?2 AND ?3
                   AND monthly.item_kind = ?4
                 GROUP BY monthly.item_id
                 ORDER BY SUM(monthly.play_count) DESC,
                          COALESCE(lifetime.display_name, MAX(monthly.display_name))
                            COLLATE NOCASE,
                          monthly.item_id
                 LIMIT 5",
            )?;
            statement
                .query_map(
                    params![source_id.as_str(), first, last, item_kind],
                    activity_row,
                )?
                .collect::<Result<Vec<_>, _>>()?
        }
    };

    rows.into_iter()
        .map(
            |(id, name, context, play_count, skip_count, last_played_at): StoredActivityRow| {
                let id = match item_kind {
                    "track" => ActivityItemId::Track(TrackId::new(id)),
                    "artist" => ActivityItemId::Artist(ArtistId::new(id)),
                    "genre" => ActivityItemId::Genre(GenreId::new(id)),
                    _ => unreachable!("activity item kind is fixed by the caller"),
                };
                Ok(ActivityItem {
                    id,
                    name,
                    context,
                    play_count: checked_u64(play_count)?,
                    skip_count: skip_count.map(checked_u64).transpose()?,
                    last_played_at,
                })
            },
        )
        .collect()
}

fn activity_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<(
    String,
    String,
    Option<String>,
    i64,
    Option<i64>,
    Option<i64>,
)> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
    ))
}

fn open_path(path: &Path) -> StoreResult<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(Connection::open(path)?)
}

fn reply_channel<T>() -> (Reply<T>, Receiver<StoreResult<T>>) {
    mpsc::sync_channel(1)
}

fn receive_reply<T>(receiver: Receiver<StoreResult<T>>) -> StoreResult<T> {
    receiver.recv().map_err(|_| StoreError::WorkerStopped)?
}

fn respond<T>(reply: Reply<T>, result: StoreResult<T>) {
    let _ = reply.send(result);
}

fn write_bounded<T>(
    connection: &mut Connection,
    values: &[T],
    estimate: impl Fn(&T) -> StoreResult<usize>,
    mut write: impl FnMut(&Transaction<'_>, &T) -> StoreResult<()>,
) -> StoreResult<()> {
    let mut start = 0;
    while start < values.len() {
        let mut end = start;
        let mut bytes = 0_usize;
        while end < values.len() && end - start < STORE_ROW_BATCH_LIMIT {
            let row_bytes = estimate(&values[end])?;
            if row_bytes > STORE_BYTE_BATCH_LIMIT {
                return Err(StoreError::RowTooLarge);
            }
            if end > start && bytes.saturating_add(row_bytes) > STORE_BYTE_BATCH_LIMIT {
                break;
            }
            bytes = bytes.saturating_add(row_bytes);
            end += 1;
        }
        let transaction = connection.transaction()?;
        for value in &values[start..end] {
            write(&transaction, value)?;
        }
        transaction.commit()?;
        start = end;
    }
    Ok(())
}

fn write_album(transaction: &Transaction<'_>, library_id: i64, album: &Album) -> StoreResult<()> {
    let relations_json = serde_json::to_string(&album.relations)?;
    let release_types_json = serde_json::to_string(&album.release_types)?;
    let (image_item_id, image_tag) = image_parts(album.image_ref.as_ref());
    let artwork = artwork_parts(album.local_artwork.as_ref());
    transaction.execute(
        "INSERT INTO albums(
            library_id, album_id, title, display_artist, year, release_date,
            date_added, last_played, play_count, user_rating, favorite,
            image_item_id, image_tag, release_types_json, is_compilation,
            musicbrainz_release_id, musicbrainz_release_group_id,
            local_artwork_kind, local_artwork_path,
            local_artwork_picture_index, local_artwork_revision,
            relations_json
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
            ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22
         )",
        params![
            library_id,
            album.id.as_str(),
            album.title,
            album.artist,
            i64::from(album.year),
            album.release_date,
            album.date_added,
            album.last_played,
            album.play_count.map(i64::from),
            album.user_rating.map(i64::from),
            i64::from(album.favorite),
            image_item_id,
            image_tag,
            release_types_json,
            album.is_compilation.map(i64::from),
            album.musicbrainz_album_id,
            album.musicbrainz_release_group_id,
            artwork.kind,
            artwork.path,
            artwork.picture_index,
            artwork.revision,
            relations_json,
        ],
    )?;
    Ok(())
}

fn write_track(transaction: &Transaction<'_>, library_id: i64, track: &Track) -> StoreResult<()> {
    let relations_json = serde_json::to_string(&track.relations)?;
    let (image_item_id, image_tag) = image_parts(track.image_ref.as_ref());
    let artwork = artwork_parts(track.local_artwork.as_ref());
    let (cue_path, cue_start, cue_end) = track.cue.as_ref().map_or((None, None, None), |cue| {
        (
            Some(cue.cue_path.as_str()),
            i64::try_from(cue.start_millis).ok(),
            i64::try_from(cue.end_millis).ok(),
        )
    });
    if track.cue.is_some() && (cue_start.is_none() || cue_end.is_none()) {
        return Err(StoreError::IntegerRange);
    }
    transaction.execute(
        "INSERT INTO tracks(
            library_id, track_id, album_id, title, display_album,
            display_artist, year, release_date, date_added, last_played,
            play_count, skip_count, user_rating, duration_seconds, favorite,
            disc_number, track_number, image_item_id, image_tag, source_format,
            comment, bpm, musicbrainz_recording_id,
            musicbrainz_release_track_id, source_path, cue_path,
            cue_start_millis, cue_end_millis, local_artwork_kind,
            local_artwork_path, local_artwork_picture_index,
            local_artwork_revision, relations_json
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
            ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24,
            ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32, ?33
         )",
        params![
            library_id,
            track.id.as_str(),
            track.album_id.as_ref().map(AlbumId::as_str),
            track.title,
            track.album,
            track.artist,
            i64::from(track.year),
            track.release_date,
            track.date_added,
            track.last_played,
            track.play_count.map(i64::from),
            track.skip_count.map(i64::from),
            track.user_rating.map(i64::from),
            i64::from(track.duration_seconds),
            i64::from(track.favorite),
            i64::from(track.disc_number),
            i64::from(track.track_number),
            image_item_id,
            image_tag,
            track.source_format,
            track.comment,
            track.bpm.map(i64::from),
            track.musicbrainz_recording_id,
            track.musicbrainz_release_track_id,
            track.source_path,
            cue_path,
            cue_start,
            cue_end,
            artwork.kind,
            artwork.path,
            artwork.picture_index,
            artwork.revision,
            relations_json,
        ],
    )?;
    Ok(())
}

fn write_artist(
    transaction: &Transaction<'_>,
    library_id: i64,
    artist: &Artist,
) -> StoreResult<()> {
    let (image_item_id, image_tag) = image_parts(artist.image_ref.as_ref());
    let artwork = artwork_parts(artist.local_artwork.as_ref());
    transaction.execute(
        "INSERT INTO artists(
            library_id, artist_id, name, last_played, play_count, user_rating,
            favorite, image_item_id, image_tag, musicbrainz_artist_id,
            local_artwork_kind, local_artwork_path,
            local_artwork_picture_index, local_artwork_revision
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14
         )",
        params![
            library_id,
            artist.id.as_str(),
            artist.name,
            artist.last_played,
            artist.play_count.map(i64::from),
            artist.user_rating.map(i64::from),
            i64::from(artist.favorite),
            image_item_id,
            image_tag,
            artist.musicbrainz_artist_id,
            artwork.kind,
            artwork.path,
            artwork.picture_index,
            artwork.revision,
        ],
    )?;
    Ok(())
}

fn write_genre(transaction: &Transaction<'_>, library_id: i64, genre: &Genre) -> StoreResult<()> {
    let (image_item_id, image_tag) = image_parts(genre.image_ref.as_ref());
    transaction.execute(
        "INSERT INTO genres(
            library_id, genre_id, name, image_item_id, image_tag
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            library_id,
            genre.id.as_str(),
            genre.name,
            image_item_id,
            image_tag
        ],
    )?;
    Ok(())
}

fn write_music_folder(
    transaction: &Transaction<'_>,
    library_id: i64,
    folder: &MusicFolder,
) -> StoreResult<()> {
    let (image_item_id, image_tag) = image_parts(folder.image_ref.as_ref());
    transaction.execute(
        "INSERT INTO music_folders(
            library_id, folder_id, name, image_item_id, image_tag
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            library_id,
            folder.id.as_str(),
            folder.name,
            image_item_id,
            image_tag,
        ],
    )?;
    Ok(())
}

fn write_playlist(
    connection: &mut Connection,
    library_id: i64,
    snapshot: &PlaylistSnapshot,
) -> StoreResult<()> {
    let (image_item_id, image_tag) = image_parts(snapshot.playlist.image_ref.as_ref());
    let transaction = connection.transaction()?;
    transaction.execute(
        "INSERT INTO source_playlists(
            library_id, playlist_id, name, image_item_id, image_tag
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            library_id,
            snapshot.playlist.id.as_str(),
            snapshot.playlist.name,
            image_item_id,
            image_tag,
        ],
    )?;
    transaction.commit()?;

    let playlist_id = &snapshot.playlist.id;
    let entries = snapshot.entries.iter().enumerate().collect::<Vec<_>>();
    write_bounded(
        connection,
        &entries,
        estimate_playlist_entry,
        |transaction, indexed| {
            let (position, entry) = indexed;
            transaction.execute(
                "INSERT INTO source_playlist_entries(
                    library_id, playlist_id, position, occurrence_id, track_id
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    library_id,
                    playlist_id.as_str(),
                    i64::try_from(*position).map_err(|_| StoreError::IntegerRange)?,
                    entry.occurrence_id,
                    entry.track_id.as_str(),
                ],
            )?;
            Ok(())
        },
    )
}

fn write_local_file(
    transaction: &Transaction<'_>,
    library_id: i64,
    file: &LocalFile,
) -> StoreResult<()> {
    let dependencies_json = serde_json::to_string(&file.dependencies)?;
    transaction.execute(
        "INSERT INTO local_files(
            library_id, path, root, relative_path, kind, size_bytes, mtime_ns,
            device_id, inode, parse_version, state,
            dependencies_json
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12
         )",
        params![
            library_id,
            file.path,
            file.root,
            file.relative_path,
            file.kind.as_str(),
            optional_sqlite_u64(file.size_bytes)?,
            file.mtime_ns,
            file.device_id.map(sqlite_filesystem_identity),
            file.inode.map(sqlite_filesystem_identity),
            file.parse_version.map(i64::from),
            file.state.as_str(),
            dependencies_json,
        ],
    )?;
    Ok(())
}

fn write_local_access_file(
    transaction: &Transaction<'_>,
    source_id: &SourceId,
    file: &LocalAccessFile,
) -> StoreResult<()> {
    transaction.execute(
        "INSERT INTO local_access_files(
            source_id, path, root, relative_path, size_bytes, mtime_ns,
            device_id, inode, parser_version, title, album, artist,
            disc_number, track_number, duration_seconds
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15
         )",
        params![
            source_id.as_str(),
            file.path,
            file.root,
            file.relative_path,
            i64::try_from(file.size_bytes).map_err(|_| StoreError::IntegerRange)?,
            file.mtime_ns,
            file.device_id.map(sqlite_filesystem_identity),
            file.inode.map(sqlite_filesystem_identity),
            i64::from(file.parser_version),
            file.title,
            file.album,
            file.artist,
            i64::from(file.disc_number),
            i64::from(file.track_number),
            i64::from(file.duration_seconds),
        ],
    )?;
    Ok(())
}

fn insert_source_playlist(
    transaction: &Transaction<'_>,
    library_id: i64,
    snapshot: &PlaylistSnapshot,
) -> StoreResult<()> {
    let (image_item_id, image_tag) = image_parts(snapshot.playlist.image_ref.as_ref());
    transaction.execute(
        "INSERT INTO source_playlists(
            library_id, playlist_id, name, image_item_id, image_tag
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            library_id,
            snapshot.playlist.id.as_str(),
            snapshot.playlist.name,
            image_item_id,
            image_tag
        ],
    )?;
    for (position, entry) in snapshot.entries.iter().enumerate() {
        transaction.execute(
            "INSERT INTO source_playlist_entries(
                library_id, playlist_id, position, occurrence_id, track_id
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                library_id,
                snapshot.playlist.id.as_str(),
                i64::try_from(position).map_err(|_| StoreError::IntegerRange)?,
                entry.occurrence_id,
                entry.track_id.as_str(),
            ],
        )?;
    }
    Ok(())
}

fn source_playlist_exists(
    connection: &Connection,
    library_id: i64,
    playlist_id: &PlaylistId,
) -> StoreResult<bool> {
    Ok(connection
        .query_row(
            "SELECT 1 FROM source_playlists
             WHERE library_id = ?1 AND playlist_id = ?2",
            params![library_id, playlist_id.as_str()],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn source_playlist_matches(
    connection: &Connection,
    library_id: i64,
    snapshot: &PlaylistSnapshot,
) -> StoreResult<bool> {
    let header = connection
        .query_row(
            "SELECT name, image_item_id, image_tag
             FROM source_playlists
             WHERE library_id = ?1 AND playlist_id = ?2",
            params![library_id, snapshot.playlist.id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((name, image_item_id, image_tag)) = header else {
        return Ok(false);
    };
    let expected_image = image_parts(snapshot.playlist.image_ref.as_ref());
    if name != snapshot.playlist.name
        || image_item_id.as_deref() != expected_image.0
        || image_tag.as_deref() != expected_image.1
    {
        return Ok(false);
    }

    let mut statement = connection.prepare(
        "SELECT position, occurrence_id, track_id
         FROM source_playlist_entries
         WHERE library_id = ?1 AND playlist_id = ?2
         ORDER BY position",
    )?;
    let mut rows = statement.query(params![library_id, snapshot.playlist.id.as_str()])?;
    for (position, expected) in snapshot.entries.iter().enumerate() {
        let Some(row) = rows.next()? else {
            return Ok(false);
        };
        if row.get::<_, i64>(0)? != i64::try_from(position).map_err(|_| StoreError::IntegerRange)?
            || row.get::<_, String>(1)? != expected.occurrence_id
            || row.get::<_, String>(2)? != expected.track_id.as_str()
        {
            return Ok(false);
        }
    }
    Ok(rows.next()?.is_none())
}

fn remove_source_playlist(
    transaction: &Transaction<'_>,
    library_id: i64,
    playlist_id: &PlaylistId,
) -> StoreResult<()> {
    transaction.execute(
        "DELETE FROM source_playlist_entries
         WHERE library_id = ?1 AND playlist_id = ?2",
        params![library_id, playlist_id.as_str()],
    )?;
    transaction.execute(
        "DELETE FROM source_playlists
         WHERE library_id = ?1 AND playlist_id = ?2",
        params![library_id, playlist_id.as_str()],
    )?;
    Ok(())
}

fn write_item_replacement(
    transaction: &Transaction<'_>,
    source_id: &SourceId,
    library_id: i64,
    replacement: &mut ItemReplacement,
    local_observed_at: Option<i64>,
) -> StoreResult<Vec<LocalImport>> {
    apply_user_ratings_to_replacement(transaction, source_id, replacement)?;
    if local_observed_at.is_some() {
        for album in &mut replacement.albums {
            album.favorite = false;
            album.play_count = None;
            album.last_played = None;
            album.release_types.clear();
            album.is_compilation = None;
        }
        for track in &mut replacement.tracks {
            track.favorite = false;
            track.play_count = None;
            track.skip_count = None;
            track.last_played = None;
        }
        for artist in &mut replacement.artists {
            artist.favorite = false;
            artist.play_count = None;
            artist.last_played = None;
        }
    }
    for album_id in &replacement.removed_albums {
        transaction.execute(
            "DELETE FROM albums WHERE library_id = ?1 AND album_id = ?2",
            params![library_id, album_id.as_str()],
        )?;
        transaction.execute(
            "DELETE FROM album_release_info WHERE source_id = ?1 AND album_id = ?2",
            params![source_id.as_str(), album_id.as_str()],
        )?;
    }
    for track_id in &replacement.removed_tracks {
        transaction.execute(
            "DELETE FROM tracks WHERE library_id = ?1 AND track_id = ?2",
            params![library_id, track_id.as_str()],
        )?;
    }
    for artist_id in &replacement.removed_artists {
        transaction.execute(
            "DELETE FROM artists WHERE library_id = ?1 AND artist_id = ?2",
            params![library_id, artist_id.as_str()],
        )?;
    }
    for genre_id in &replacement.removed_genres {
        transaction.execute(
            "DELETE FROM genres WHERE library_id = ?1 AND genre_id = ?2",
            params![library_id, genre_id.as_str()],
        )?;
    }
    for album in &replacement.albums {
        transaction.execute(
            "DELETE FROM albums WHERE library_id = ?1 AND album_id = ?2",
            params![library_id, album.id.as_str()],
        )?;
    }
    for album in &replacement.albums {
        write_album(transaction, library_id, album)?;
    }
    for track in &replacement.tracks {
        transaction.execute(
            "DELETE FROM tracks WHERE library_id = ?1 AND track_id = ?2",
            params![library_id, track.id.as_str()],
        )?;
    }
    for track in &replacement.tracks {
        write_track(transaction, library_id, track)?;
    }
    for artist in &replacement.artists {
        transaction.execute(
            "DELETE FROM artists WHERE library_id = ?1 AND artist_id = ?2",
            params![library_id, artist.id.as_str()],
        )?;
    }
    for artist in &replacement.artists {
        write_artist(transaction, library_id, artist)?;
    }
    for genre in &replacement.genres {
        transaction.execute(
            "DELETE FROM genres WHERE library_id = ?1 AND genre_id = ?2",
            params![library_id, genre.id.as_str()],
        )?;
    }
    for genre in &replacement.genres {
        write_genre(transaction, library_id, genre)?;
    }

    for album in &mut replacement.albums {
        apply_exact_album_release_info(transaction, source_id, album)?;
    }
    let mut imports = Vec::new();
    if let Some(observed_at) = local_observed_at {
        for track in &replacement.tracks {
            transaction.execute(
                "INSERT OR IGNORE INTO local_imports(source_id, track_id, first_seen_at)
                 VALUES (?1, ?2, ?3)",
                params![source_id.as_str(), track.id.as_str(), observed_at],
            )?;
            let first_seen_at = transaction.query_row(
                "SELECT first_seen_at FROM local_imports
                 WHERE source_id = ?1 AND track_id = ?2",
                params![source_id.as_str(), track.id.as_str()],
                |row| row.get(0),
            )?;
            imports.push(LocalImport {
                track_id: track.id.clone(),
                first_seen_at,
            });
        }
    }
    Ok(imports)
}

fn persist_favorite(
    transaction: &Transaction<'_>,
    library_id: i64,
    item_id: &FavoriteItemId,
    favorite: bool,
    fallback: Option<FavoriteValue>,
) -> StoreResult<bool> {
    let (table, id_column) = match item_id {
        FavoriteItemId::Album(_) => ("albums", "album_id"),
        FavoriteItemId::Track(_) => ("tracks", "track_id"),
        FavoriteItemId::Artist(_) => ("artists", "artist_id"),
    };
    let current = transaction
        .query_row(
            &format!(
                "SELECT favorite FROM {table}
                 WHERE library_id = ?1 AND {id_column} = ?2"
            ),
            params![library_id, item_id.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let Some(current) = current else {
        let Some(mut fallback) = fallback else {
            return Err(StoreError::InvalidValue {
                kind: "favorite item",
                value: item_id.as_str().to_string(),
            });
        };
        match (&mut fallback, item_id) {
            (FavoriteValue::Album(album), FavoriteItemId::Album(id)) if &album.id == id => {
                album.favorite = favorite;
                write_album(transaction, library_id, album)?;
            }
            (FavoriteValue::Artist(artist), FavoriteItemId::Artist(id)) if &artist.id == id => {
                artist.favorite = favorite;
                write_artist(transaction, library_id, artist)?;
            }
            _ => {
                return Err(StoreError::InvalidValue {
                    kind: "favorite fallback",
                    value: item_id.as_str().to_string(),
                });
            }
        }
        return Ok(true);
    };
    if (current != 0) == favorite {
        return Ok(false);
    }
    transaction.execute(
        &format!(
            "UPDATE {table} SET favorite = ?3
             WHERE library_id = ?1 AND {id_column} = ?2"
        ),
        params![library_id, item_id.as_str(), i64::from(favorite)],
    )?;
    Ok(true)
}

fn favorite_row_exists(
    transaction: &Transaction<'_>,
    library_id: i64,
    item_id: &FavoriteItemId,
) -> StoreResult<bool> {
    let (table, id_column) = match item_id {
        FavoriteItemId::Album(_) => ("albums", "album_id"),
        FavoriteItemId::Track(_) => ("tracks", "track_id"),
        FavoriteItemId::Artist(_) => ("artists", "artist_id"),
    };
    Ok(transaction
        .query_row(
            &format!(
                "SELECT 1 FROM {table}
                 WHERE library_id = ?1 AND {id_column} = ?2"
            ),
            params![library_id, item_id.as_str()],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn favorite_item_id(kind: &str, id: String) -> StoreResult<FavoriteItemId> {
    Ok(match kind {
        "album" => FavoriteItemId::Album(AlbumId::new(id)),
        "track" => FavoriteItemId::Track(TrackId::new(id)),
        "artist" => FavoriteItemId::Artist(ArtistId::new(id)),
        _ => {
            return Err(StoreError::InvalidValue {
                kind: "favorite item kind",
                value: kind.to_string(),
            });
        }
    })
}

fn invalidate_content_digest(transaction: &Transaction<'_>, library_id: i64) -> StoreResult<()> {
    transaction.execute(
        "UPDATE source_libraries
         SET content_digest = NULL
         WHERE library_id = ?1 AND accepted_at IS NOT NULL",
        [library_id],
    )?;
    Ok(())
}

struct CandidateDigestTable {
    name: &'static str,
    order_by: &'static str,
}

const CANDIDATE_DIGEST_TABLES: &[CandidateDigestTable] = &[
    CandidateDigestTable {
        name: "albums",
        order_by: "album_id",
    },
    CandidateDigestTable {
        name: "tracks",
        order_by: "track_id",
    },
    CandidateDigestTable {
        name: "artists",
        order_by: "artist_id",
    },
    CandidateDigestTable {
        name: "genres",
        order_by: "genre_id",
    },
    CandidateDigestTable {
        name: "music_folders",
        order_by: "folder_id",
    },
    CandidateDigestTable {
        name: "source_playlists",
        order_by: "playlist_id",
    },
    CandidateDigestTable {
        name: "source_playlist_entries",
        order_by: "playlist_id, position",
    },
    CandidateDigestTable {
        name: "local_files",
        order_by: "path",
    },
];

/// Hashes the exact invisible candidate rows in their durable canonical order.
///
/// Provider batches may arrive in any order. SQLite already owns the complete
/// candidate and its primary-key order, so closing it needs only one current
/// row and one hasher rather than a second source-sized collection of leaves.
fn candidate_content_digest(connection: &Connection, library_id: i64) -> StoreResult<[u8; 32]> {
    let mut digest = blake3::Hasher::new();
    digest.update(b"rufin-source-store-digest");
    digest.update(&1_u32.to_le_bytes());

    for table in CANDIDATE_DIGEST_TABLES {
        let sql = format!(
            "SELECT * FROM {} WHERE library_id = ?1 ORDER BY {}",
            table.name, table.order_by
        );
        let mut statement = connection.prepare(&sql)?;
        let column_names = statement
            .column_names()
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let mut library_id_columns = column_names
            .iter()
            .enumerate()
            .filter_map(|(index, name)| (name == "library_id").then_some(index));
        let Some(library_id_column) = library_id_columns.next() else {
            return Err(StoreError::InvalidFinalSchema(format!(
                "{} has no library_id column",
                table.name
            )));
        };
        if library_id_columns.next().is_some() {
            return Err(StoreError::InvalidFinalSchema(format!(
                "{} has more than one library_id column",
                table.name
            )));
        }
        let kept_columns = column_names
            .iter()
            .enumerate()
            .filter_map(|(index, name)| (index != library_id_column).then_some((index, name)))
            .collect::<Vec<_>>();
        update_digest_bytes(&mut digest, table.name.as_bytes());
        digest.update(&(kept_columns.len() as u32).to_le_bytes());
        for (_, name) in &kept_columns {
            update_digest_bytes(&mut digest, name.as_bytes());
        }

        let mut rows = statement.query([library_id])?;
        let mut row_count = 0_u64;
        while let Some(row) = rows.next()? {
            digest.update(b"row");
            for (column, _) in &kept_columns {
                update_digest_value(&mut digest, row.get_ref(*column)?);
            }
            row_count = row_count.checked_add(1).ok_or(StoreError::IntegerRange)?;
        }
        digest.update(b"rows");
        digest.update(&row_count.to_le_bytes());
    }

    Ok(*digest.finalize().as_bytes())
}

fn persisted_home(home: &HomeFacts) -> StoreResult<(String, [u8; 32])> {
    let json = serde_json::to_string(home)?;
    let mut digest = blake3::Hasher::new();
    digest.update(b"rufin-home-digest");
    digest.update(&1_u32.to_le_bytes());
    update_digest_bytes(&mut digest, json.as_bytes());
    Ok((json, *digest.finalize().as_bytes()))
}

fn update_digest_bytes(digest: &mut blake3::Hasher, value: &[u8]) {
    digest.update(&(value.len() as u64).to_le_bytes());
    digest.update(value);
}

fn update_digest_value(digest: &mut blake3::Hasher, value: ValueRef<'_>) {
    match value {
        ValueRef::Null => {
            digest.update(&[0]);
        }
        ValueRef::Integer(value) => {
            digest.update(&[1]);
            digest.update(&value.to_le_bytes());
        }
        ValueRef::Real(value) => {
            digest.update(&[2]);
            digest.update(&value.to_bits().to_le_bytes());
        }
        ValueRef::Text(value) => {
            digest.update(&[3]);
            update_digest_bytes(digest, value);
        }
        ValueRef::Blob(value) => {
            digest.update(&[4]);
            update_digest_bytes(digest, value);
        }
    }
}

fn update_candidate_acceptance(
    transaction: &Transaction<'_>,
    library_id: i64,
    content_digest: &[u8; 32],
    home_digest: &[u8; 32],
    freshness: Option<&ProviderFreshness>,
    accepted_at: i64,
    home_json: &str,
) -> StoreResult<()> {
    let freshness = freshness_parts(freshness);
    transaction.execute(
        "UPDATE source_libraries
         SET content_digest = ?2,
             freshness_version = ?3,
             freshness_marker = ?4,
             home_digest = ?5,
             home_json = ?6,
             accepted_at = ?7
         WHERE library_id = ?1 AND accepted_at IS NULL",
        params![
            library_id,
            content_digest.as_slice(),
            freshness.version,
            freshness.marker,
            home_digest.as_slice(),
            home_json,
            accepted_at,
        ],
    )?;
    Ok(())
}

fn insert_local_imports(
    transaction: &Transaction<'_>,
    source_id: &SourceId,
    library_id: i64,
    accepted_at: i64,
) -> StoreResult<()> {
    transaction.execute(
        "INSERT OR IGNORE INTO local_imports(source_id, track_id, first_seen_at)
         SELECT ?1, track_id, ?3
         FROM tracks
         WHERE library_id = ?2",
        params![source_id.as_str(), library_id, accepted_at],
    )?;
    Ok(())
}

fn prune_album_release_info(
    transaction: &Transaction<'_>,
    source_id: &SourceId,
    library_id: i64,
) -> StoreResult<()> {
    transaction.execute(
        "DELETE FROM album_release_info
         WHERE source_id = ?1
           AND NOT EXISTS (
               SELECT 1 FROM albums
               WHERE albums.library_id = ?2
                 AND albums.album_id = album_release_info.album_id
           )",
        params![source_id.as_str(), library_id],
    )?;
    Ok(())
}

fn update_accepted_metadata(
    transaction: &Transaction<'_>,
    library_id: i64,
    content_digest: &[u8; 32],
    home_digest: &[u8; 32],
    freshness: Option<&ProviderFreshness>,
    home_json: &str,
    home_changed: bool,
) -> StoreResult<()> {
    let freshness = freshness_parts(freshness);
    if home_changed {
        transaction.execute(
            "UPDATE source_libraries
             SET content_digest = ?2,
                 freshness_version = ?3,
                 freshness_marker = ?4,
                 home_digest = ?5,
                 home_json = ?6
             WHERE library_id = ?1 AND accepted_at IS NOT NULL",
            params![
                library_id,
                content_digest.as_slice(),
                freshness.version,
                freshness.marker,
                home_digest.as_slice(),
                home_json,
            ],
        )?;
    } else {
        transaction.execute(
            "UPDATE source_libraries
             SET content_digest = ?2,
                 freshness_version = ?3,
                 freshness_marker = ?4
             WHERE library_id = ?1 AND accepted_at IS NOT NULL",
            params![
                library_id,
                content_digest.as_slice(),
                freshness.version,
                freshness.marker
            ],
        )?;
    }
    Ok(())
}

fn load_track_activity(
    connection: &Connection,
    source_id: &SourceId,
) -> StoreResult<Vec<TrackActivity>> {
    let mut statement = connection.prepare(
        "SELECT
            item_id,
            play_count,
            COALESCE(skip_count, 0),
            CASE
                WHEN last_played_at IS NULL THEN NULL
                ELSE datetime(last_played_at, 'unixepoch')
            END
         FROM listening_aggregates
         WHERE source_id = ?1
           AND period = 'lifetime'
           AND item_kind = 'track'",
    )?;
    statement
        .query_map([source_id.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?
        .map(|row| {
            let (track_id, play_count, skip_count, last_played) = row?;
            track_activity_from_values(track_id, play_count, skip_count, last_played)
        })
        .collect()
}

fn load_one_track_activity(
    connection: &Connection,
    source_id: &SourceId,
    track_id: &TrackId,
) -> StoreResult<TrackActivity> {
    load_optional_track_activity(connection, source_id, track_id)?.ok_or_else(|| {
        StoreError::InvalidValue {
            kind: "Track activity",
            value: track_id.to_string(),
        }
    })
}

fn load_optional_track_activity(
    connection: &Connection,
    source_id: &SourceId,
    track_id: &TrackId,
) -> StoreResult<Option<TrackActivity>> {
    let values = connection
        .query_row(
            "SELECT
            play_count,
            COALESCE(skip_count, 0),
            CASE
                WHEN last_played_at IS NULL THEN NULL
                ELSE datetime(last_played_at, 'unixepoch')
            END
         FROM listening_aggregates
         WHERE source_id = ?1
           AND period = 'lifetime'
           AND item_kind = 'track'
           AND item_id = ?2",
            params![source_id.as_str(), track_id.as_str()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()?;
    values
        .map(|(play_count, skip_count, last_played)| {
            track_activity_from_values(
                track_id.as_str().to_string(),
                play_count,
                skip_count,
                last_played,
            )
        })
        .transpose()
}

fn track_activity_from_values(
    track_id: String,
    play_count: i64,
    skip_count: i64,
    last_played: Option<String>,
) -> StoreResult<TrackActivity> {
    Ok(TrackActivity {
        track_id: TrackId::new(track_id),
        play_count: checked_u64(play_count)?
            .min(u64::from(u32::MAX))
            .try_into()
            .unwrap_or(u32::MAX),
        skip_count: checked_u64(skip_count)?
            .min(u64::from(u32::MAX))
            .try_into()
            .unwrap_or(u32::MAX),
        last_played,
    })
}

fn load_recent_plays(
    connection: &Connection,
    source_id: &SourceId,
) -> StoreResult<Vec<RecentPlay>> {
    let mut statement = connection.prepare(
        "SELECT play_id, track_id, track_title, artist_name, album_title, played_at
         FROM recent_plays
         WHERE source_id = ?1
         ORDER BY played_at DESC, play_id DESC
         LIMIT 100",
    )?;
    Ok(statement
        .query_map([source_id.as_str()], |row| {
            Ok(RecentPlay {
                play_id: row.get(0)?,
                track_id: TrackId::new(row.get::<_, String>(1)?),
                track_title: row.get(2)?,
                artist_name: row.get(3)?,
                album_title: row.get(4)?,
                played_at: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?)
}

fn load_local_imports(
    connection: &Connection,
    source_id: &SourceId,
    library_id: i64,
) -> StoreResult<Vec<LocalImport>> {
    let mut statement = connection.prepare(
        "SELECT local_imports.track_id, local_imports.first_seen_at
         FROM local_imports
         JOIN tracks
           ON tracks.library_id = ?2
          AND tracks.track_id = local_imports.track_id
         WHERE local_imports.source_id = ?1",
    )?;
    Ok(statement
        .query_map(params![source_id.as_str(), library_id], |row| {
            Ok(LocalImport {
                track_id: TrackId::new(row.get::<_, String>(0)?),
                first_seen_at: row.get(1)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?)
}

fn load_candidate_local_imports(
    connection: &Connection,
    source_id: &SourceId,
    library_id: i64,
    accepted_at: i64,
) -> StoreResult<Vec<LocalImport>> {
    let mut imports = load_local_imports(connection, source_id, library_id)?;
    let known = imports
        .iter()
        .map(|import| import.track_id.clone())
        .collect::<HashSet<_>>();
    let mut statement = connection.prepare(
        "SELECT track_id
         FROM tracks
         WHERE library_id = ?1
         ORDER BY track_id",
    )?;
    imports.extend(
        statement
            .query_map([library_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(TrackId::new)
            .filter(|track_id| !known.contains(track_id))
            .map(|track_id| LocalImport {
                track_id,
                first_seen_at: accepted_at,
            }),
    );
    Ok(imports)
}

fn complete_loaded_input(
    connection: &Connection,
    mut input: LibraryInput,
    source_id: &SourceId,
    home: HomeFacts,
) -> StoreResult<LibraryInput> {
    input.local_access = load_current_local_access(connection, source_id)?;
    input
        .playlists
        .extend(load_local_playlists(connection, source_id)?);
    input.smart_playlists = load_smart_playlists(connection, source_id)?;
    input.local_favorites = load_local_favorites(connection, source_id)?;
    apply_pending_favorites(connection, source_id, &mut input)?;
    apply_user_ratings(connection, source_id, &mut input)?;
    apply_album_release_info(connection, source_id, &mut input.albums)?;
    input.activity = load_track_activity(connection, source_id)?;
    input.recent_plays = load_recent_plays(connection, source_id)?;
    input.loudness = load_loudness(connection, source_id)?;
    if matches!(home, HomeFacts::RufinDefined) {
        input.local_imports = load_local_imports(connection, source_id, input.library_id)?;
    }
    input.home = Some(home);
    Ok(input)
}

fn apply_user_ratings(
    connection: &Connection,
    source_id: &SourceId,
    input: &mut LibraryInput,
) -> StoreResult<()> {
    let ratings = load_user_ratings(connection, source_id)?;
    apply_ratings_to_items(
        &ratings,
        &mut input.albums,
        &mut input.tracks,
        &mut input.artists,
    );
    Ok(())
}

fn apply_ratings_to_items(
    ratings: &HashMap<(String, String), Option<u8>>,
    albums: &mut [Album],
    tracks: &mut [Track],
    artists: &mut [Artist],
) {
    for album in albums {
        apply_rating(&ratings, "album", album.id.as_str(), &mut album.user_rating);
    }
    for track in tracks {
        let id = track.id.as_str().to_string();
        if let Some(rating) = ratings.get(&("track".to_string(), id)) {
            track.make_mut().user_rating = *rating;
        }
    }
    for artist in artists {
        apply_rating(
            &ratings,
            "artist",
            artist.id.as_str(),
            &mut artist.user_rating,
        );
    }
}

fn apply_user_ratings_to_replacement(
    connection: &Connection,
    source_id: &SourceId,
    replacement: &mut ItemReplacement,
) -> StoreResult<()> {
    let ratings = load_user_ratings(connection, source_id)?;
    apply_ratings_to_items(
        &ratings,
        &mut replacement.albums,
        &mut replacement.tracks,
        &mut replacement.artists,
    );
    Ok(())
}

fn load_user_ratings(
    connection: &Connection,
    source_id: &SourceId,
) -> StoreResult<HashMap<(String, String), Option<u8>>> {
    let mut statement = connection
        .prepare("SELECT item_kind, item_id, rating FROM user_ratings WHERE source_id = ?1")?;
    statement
        .query_map([source_id.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .map(|row| {
            let (kind, id, rating) = row?;
            let rating = u8::try_from(rating).map_err(|_| StoreError::IntegerRange)?;
            Ok(((kind, id), (rating > 0).then_some(rating)))
        })
        .collect()
}

fn apply_rating(
    ratings: &HashMap<(String, String), Option<u8>>,
    kind: &str,
    id: &str,
    rating: &mut Option<u8>,
) {
    if let Some(value) = ratings.get(&(kind.to_string(), id.to_string())) {
        *rating = *value;
    }
}

fn load_loudness(
    connection: &Connection,
    source_id: &SourceId,
) -> StoreResult<Vec<LoudnessMeasurementWrite>> {
    let mut statement = connection.prepare(
        "SELECT scope, item_id, analysis_key, integrated_lufs, true_peak
         FROM loudness_measurements
         WHERE source_id = ?1
         ORDER BY scope, item_id",
    )?;
    let rows = statement
        .query_map([source_id.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Vec<u8>>(2)?,
                row.get::<_, Option<f64>>(3)?,
                row.get::<_, f64>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(
            |(scope, item_id, analysis_key, integrated_lufs, true_peak_ratio)| {
                let item = match scope.as_str() {
                    "track" => LoudnessItemId::Track(TrackId::new(item_id)),
                    "album" => LoudnessItemId::Album(AlbumId::new(item_id)),
                    _ => {
                        return Err(StoreError::InvalidValue {
                            kind: "loudness scope",
                            value: scope,
                        });
                    }
                };
                let analysis_key = <[u8; 32]>::try_from(analysis_key).map_err(|value| {
                    StoreError::InvalidValue {
                        kind: "loudness analysis key",
                        value: format!("{} bytes", value.len()),
                    }
                })?;
                let measurement = LoudnessMeasurement::new(integrated_lufs, true_peak_ratio)
                    .map_err(|value| StoreError::InvalidValue {
                        kind: "loudness measurement",
                        value,
                    })?;
                Ok(LoudnessMeasurementWrite {
                    item,
                    analysis_key,
                    measurement,
                })
            },
        )
        .collect()
}

fn apply_pending_favorites(
    connection: &Connection,
    source_id: &SourceId,
    input: &mut LibraryInput,
) -> StoreResult<()> {
    let mut statement = connection.prepare(
        "SELECT item_kind, item_id, favorite
         FROM pending_favorites
         WHERE source_id = ?1",
    )?;
    let pending = statement
        .query_map([source_id.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)? != 0,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut albums = input
        .albums
        .iter_mut()
        .map(|album| (album.id.as_str().to_string(), album))
        .collect::<HashMap<_, _>>();
    let mut tracks = input
        .tracks
        .iter_mut()
        .map(|track| (track.id.as_str().to_string(), track))
        .collect::<HashMap<_, _>>();
    let mut artists = input
        .artists
        .iter_mut()
        .map(|artist| (artist.id.as_str().to_string(), artist))
        .collect::<HashMap<_, _>>();
    for (kind, id, favorite) in pending {
        match kind.as_str() {
            "album" => {
                if let Some(album) = albums.get_mut(&id) {
                    album.favorite = favorite;
                }
            }
            "track" => {
                if let Some(track) = tracks.get_mut(&id) {
                    track.favorite = favorite;
                }
            }
            "artist" => {
                if let Some(artist) = artists.get_mut(&id) {
                    artist.favorite = favorite;
                }
            }
            _ => {
                return Err(StoreError::InvalidValue {
                    kind: "favorite item kind",
                    value: kind,
                });
            }
        }
    }
    Ok(())
}

fn load_current_local_access(
    connection: &Connection,
    source_id: &SourceId,
) -> StoreResult<Vec<LocalAccessFile>> {
    load_local_access_files(connection, source_id)
}

fn load_albums(connection: &Connection, library_id: i64) -> StoreResult<Vec<Album>> {
    let mut statement = connection.prepare(
        "SELECT album_id, title, display_artist, year, release_date, date_added,
                last_played, play_count, user_rating, favorite, image_item_id,
                image_tag, release_types_json, is_compilation,
                musicbrainz_release_id, musicbrainz_release_group_id,
                local_artwork_kind, local_artwork_path,
                local_artwork_picture_index, local_artwork_revision,
                relations_json
         FROM albums WHERE library_id = ?1",
    )?;
    let mut rows = statement.query([library_id])?;
    let mut albums = Vec::new();
    while let Some(row) = rows.next()? {
        let id = AlbumId::new(row.get::<_, String>(0)?);
        albums.push(Album {
            color_seed: color_seed(id.as_str()),
            id,
            title: row.get(1)?,
            artist: row.get(2)?,
            year: checked_u16(row.get(3)?)?,
            release_date: row.get(4)?,
            date_added: row.get(5)?,
            last_played: row.get(6)?,
            play_count: optional_u32(row.get(7)?)?,
            user_rating: optional_u8(row.get(8)?)?,
            favorite: row.get::<_, i64>(9)? != 0,
            image_ref: image_from_parts(row.get(10)?, row.get(11)?),
            release_types: serde_json::from_str(&row.get::<_, String>(12)?)?,
            is_compilation: row.get::<_, Option<i64>>(13)?.map(|value| value != 0),
            musicbrainz_album_id: row.get(14)?,
            musicbrainz_release_group_id: row.get(15)?,
            local_artwork: artwork_from_parts(
                row.get(16)?,
                row.get(17)?,
                row.get(18)?,
                row.get(19)?,
            )?,
            relations: serde_json::from_str::<AlbumRelations>(&row.get::<_, String>(20)?)?,
        });
    }
    Ok(albums)
}

fn load_tracks(connection: &Connection, library_id: i64) -> StoreResult<Vec<Track>> {
    let mut statement = connection.prepare(
        "SELECT track_id, album_id, title, display_album, display_artist, year,
                release_date, date_added, last_played, play_count, skip_count,
                user_rating, duration_seconds, favorite, disc_number,
                track_number, image_item_id, image_tag, source_format, comment,
                bpm, musicbrainz_recording_id, musicbrainz_release_track_id,
                source_path, cue_path, cue_start_millis, cue_end_millis,
                local_artwork_kind, local_artwork_path,
                local_artwork_picture_index, local_artwork_revision,
                relations_json
         FROM tracks WHERE library_id = ?1",
    )?;
    let mut rows = statement.query([library_id])?;
    let mut tracks = Vec::new();
    while let Some(row) = rows.next()? {
        tracks.push(track_from_row(row)?);
    }
    Ok(tracks)
}

fn track_from_row(row: &Row<'_>) -> StoreResult<Track> {
    let cue_path = row.get::<_, Option<String>>(24)?;
    let cue_start = row.get::<_, Option<i64>>(25)?;
    let cue_end = row.get::<_, Option<i64>>(26)?;
    let cue = match (cue_path, cue_start, cue_end) {
        (Some(cue_path), Some(start), Some(end)) => Some(CueSegment {
            cue_path,
            start_millis: checked_u64(start)?,
            end_millis: checked_u64(end)?,
        }),
        (None, None, None) => None,
        _ => {
            return Err(StoreError::InvalidValue {
                kind: "CUE segment",
                value: "incomplete columns".to_string(),
            });
        }
    };
    Ok(Track::new(TrackData {
        id: TrackId::new(row.get::<_, String>(0)?),
        album_id: row.get::<_, Option<String>>(1)?.map(AlbumId::new),
        title: row.get(2)?,
        album: row.get(3)?,
        album_artwork: None,
        artist: row.get(4)?,
        year: checked_u16(row.get(5)?)?,
        release_date: row.get(6)?,
        date_added: row.get(7)?,
        last_played: row.get(8)?,
        play_count: optional_u32(row.get(9)?)?,
        skip_count: optional_u32(row.get(10)?)?,
        user_rating: optional_u8(row.get(11)?)?,
        duration_seconds: checked_u32(row.get(12)?)?,
        favorite: row.get::<_, i64>(13)? != 0,
        disc_number: checked_u16(row.get(14)?)?,
        track_number: checked_u16(row.get(15)?)?,
        image_ref: image_from_parts(row.get(16)?, row.get(17)?),
        source_format: row.get(18)?,
        comment: row.get(19)?,
        bpm: optional_u16(row.get(20)?)?,
        musicbrainz_recording_id: row.get(21)?,
        musicbrainz_release_track_id: row.get(22)?,
        source_path: row.get(23)?,
        cue,
        local_artwork: artwork_from_parts(row.get(27)?, row.get(28)?, row.get(29)?, row.get(30)?)?,
        relations: serde_json::from_str::<TrackRelations>(&row.get::<_, String>(31)?)?,
    }))
}

fn load_artists(connection: &Connection, library_id: i64) -> StoreResult<Vec<Artist>> {
    let mut statement = connection.prepare(
        "SELECT artist_id, name, last_played, play_count, user_rating, favorite,
                image_item_id, image_tag, musicbrainz_artist_id,
                local_artwork_kind, local_artwork_path,
                local_artwork_picture_index, local_artwork_revision
         FROM artists WHERE library_id = ?1",
    )?;
    let mut rows = statement.query([library_id])?;
    let mut artists = Vec::new();
    while let Some(row) = rows.next()? {
        artists.push(Artist {
            id: ArtistId::new(row.get::<_, String>(0)?),
            name: row.get(1)?,
            last_played: row.get(2)?,
            play_count: optional_u32(row.get(3)?)?,
            user_rating: optional_u8(row.get(4)?)?,
            favorite: row.get::<_, i64>(5)? != 0,
            image_ref: image_from_parts(row.get(6)?, row.get(7)?),
            musicbrainz_artist_id: row.get(8)?,
            local_artwork: artwork_from_parts(
                row.get(9)?,
                row.get(10)?,
                row.get(11)?,
                row.get(12)?,
            )?,
        });
    }
    Ok(artists)
}

fn load_genres(connection: &Connection, library_id: i64) -> StoreResult<Vec<Genre>> {
    let mut statement = connection.prepare(
        "SELECT genre_id, name, image_item_id, image_tag
         FROM genres WHERE library_id = ?1",
    )?;
    let mut rows = statement.query([library_id])?;
    let mut genres = Vec::new();
    while let Some(row) = rows.next()? {
        genres.push(Genre {
            id: GenreId::new(row.get::<_, String>(0)?),
            name: row.get(1)?,
            image_ref: image_from_parts(row.get(2)?, row.get(3)?),
        });
    }
    Ok(genres)
}

fn load_music_folders(connection: &Connection, library_id: i64) -> StoreResult<Vec<MusicFolder>> {
    let mut statement = connection.prepare(
        "SELECT folder_id, name, image_item_id, image_tag
         FROM music_folders
         WHERE library_id = ?1",
    )?;
    Ok(statement
        .query_map([library_id], |row| {
            Ok(MusicFolder {
                id: MusicFolderId::new(row.get::<_, String>(0)?),
                name: row.get(1)?,
                image_ref: image_from_parts(row.get(2)?, row.get(3)?),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?)
}

fn load_local_files(connection: &Connection, library_id: i64) -> StoreResult<Vec<LocalFile>> {
    let mut statement = connection.prepare(
        "SELECT
            path, root, relative_path, kind, size_bytes, mtime_ns,
            device_id, inode, parse_version, state, dependencies_json
         FROM local_files
         WHERE library_id = ?1
         ORDER BY path",
    )?;
    let mut rows = statement.query([library_id])?;
    let mut files = Vec::new();
    while let Some(row) = rows.next()? {
        let kind = row.get::<_, String>(3)?;
        let state = row.get::<_, String>(9)?;
        files.push(LocalFile {
            path: row.get(0)?,
            root: row.get(1)?,
            relative_path: row.get(2)?,
            kind: LocalFileKind::from_stored(&kind).ok_or_else(|| StoreError::InvalidValue {
                kind: "Local file kind",
                value: kind,
            })?,
            size_bytes: row.get::<_, Option<i64>>(4)?.map(checked_u64).transpose()?,
            mtime_ns: row.get(5)?,
            device_id: row
                .get::<_, Option<i64>>(6)?
                .map(filesystem_identity_from_sqlite),
            inode: row
                .get::<_, Option<i64>>(7)?
                .map(filesystem_identity_from_sqlite),
            parse_version: row.get::<_, Option<i64>>(8)?.map(checked_u32).transpose()?,
            state: LocalFileState::from_stored(&state).ok_or_else(|| StoreError::InvalidValue {
                kind: "Local file state",
                value: state,
            })?,
            dependencies: serde_json::from_str(&row.get::<_, String>(10)?)?,
        });
    }
    Ok(files)
}

fn load_local_access_files(
    connection: &Connection,
    source_id: &SourceId,
) -> StoreResult<Vec<LocalAccessFile>> {
    let mut statement = connection.prepare(
        "SELECT
            path, root, relative_path, size_bytes, mtime_ns, device_id, inode,
            parser_version, title, album, artist, disc_number, track_number,
            duration_seconds
         FROM local_access_files
         WHERE source_id = ?1
         ORDER BY path",
    )?;
    Ok(statement
        .query_map([source_id.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, String>(10)?,
                row.get::<_, i64>(11)?,
                row.get::<_, i64>(12)?,
                row.get::<_, i64>(13)?,
            ))
        })?
        .map(|row| {
            let (
                path,
                root,
                relative_path,
                size_bytes,
                mtime_ns,
                device_id,
                inode,
                parser_version,
                title,
                album,
                artist,
                disc_number,
                track_number,
                duration_seconds,
            ) = row?;
            Ok(LocalAccessFile {
                path,
                root,
                relative_path,
                size_bytes: checked_u64(size_bytes)?,
                mtime_ns,
                device_id: device_id.map(filesystem_identity_from_sqlite),
                inode: inode.map(filesystem_identity_from_sqlite),
                parser_version: checked_u32(parser_version)?,
                title,
                album,
                artist,
                disc_number: checked_u16(disc_number)?,
                track_number: checked_u16(track_number)?,
                duration_seconds: checked_u32(duration_seconds)?,
            })
        })
        .collect::<StoreResult<Vec<_>>>()?)
}

fn load_source_playlists(
    connection: &Connection,
    library_id: i64,
) -> StoreResult<Vec<PlaylistSnapshot>> {
    let mut entries = load_source_playlist_entries(connection, library_id)?;
    let mut statement = connection.prepare(
        "SELECT playlist_id, name, image_item_id, image_tag
         FROM source_playlists
         WHERE library_id = ?1
         ORDER BY name COLLATE NOCASE, playlist_id",
    )?;
    let headers = statement
        .query_map([library_id], |row| {
            Ok((
                PlaylistId::new(row.get::<_, String>(0)?),
                row.get::<_, String>(1)?,
                image_from_parts(row.get(2)?, row.get(3)?),
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut playlists = Vec::with_capacity(headers.len());
    for (id, name, image_ref) in headers {
        playlists.push(PlaylistSnapshot {
            playlist: Playlist {
                id: id.clone(),
                name,
                image_ref,
            },
            entries: entries.remove(&id).unwrap_or_default(),
        });
    }
    Ok(playlists)
}

fn load_local_playlists(
    connection: &Connection,
    source_id: &SourceId,
) -> StoreResult<Vec<PlaylistSnapshot>> {
    let mut entries = load_local_playlist_entries(connection, source_id)?;
    let mut statement = connection.prepare(
        "SELECT playlist_id, name
         FROM local_playlists
         WHERE source_id = ?1
         ORDER BY name COLLATE NOCASE, playlist_id",
    )?;
    let headers = statement
        .query_map([source_id.as_str()], |row| {
            Ok((
                PlaylistId::new(row.get::<_, String>(0)?),
                row.get::<_, String>(1)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut playlists = Vec::with_capacity(headers.len());
    for (id, name) in headers {
        playlists.push(PlaylistSnapshot {
            playlist: Playlist {
                id: id.clone(),
                name,
                image_ref: None,
            },
            entries: entries.remove(&id).unwrap_or_default(),
        });
    }
    Ok(playlists)
}

fn load_smart_playlists(
    connection: &Connection,
    source_id: &SourceId,
) -> StoreResult<Vec<SmartPlaylistRecord>> {
    let mut statement = connection.prepare(
        "SELECT smart_playlist_id, name, builtin_key, definition_json, position
         FROM smart_playlists
         WHERE source_id = ?1
         ORDER BY position, smart_playlist_id",
    )?;
    let mut rows = statement.query([source_id.as_str()])?;
    let mut records = Vec::new();
    while let Some(row) = rows.next()? {
        let id = SmartPlaylistId::new(row.get::<_, String>(0)?);
        let builtin_key = row.get::<_, Option<String>>(2)?;
        let builtin = builtin_key
            .as_deref()
            .map(|key| {
                SmartPlaylistBuiltin::from_key(key).ok_or_else(|| StoreError::InvalidValue {
                    kind: "smart playlist builtin",
                    value: key.to_string(),
                })
            })
            .transpose()?;
        let definition_json = row.get::<_, String>(3)?;
        let definition =
            match serde_json::from_str::<crate::SmartPlaylistDefinition>(&definition_json) {
                Ok(definition) => definition,
                Err(error) => {
                    tracing::warn!(
                        %source_id,
                        smart_playlist_id = %id,
                        %error,
                        "ignored an unreadable smart playlist definition"
                    );
                    continue;
                }
            };
        if let Err(error) = crate::smart_playlists::validated_smart_playlist_json(&definition) {
            tracing::warn!(
                %source_id,
                smart_playlist_id = %id,
                %error,
                "ignored an invalid smart playlist definition"
            );
            continue;
        }
        records.push(SmartPlaylistRecord {
            id,
            name: row.get(1)?,
            builtin,
            definition,
            position: checked_u32(row.get(4)?)?,
        });
    }
    Ok(records)
}

fn load_source_playlist_entries(
    connection: &Connection,
    library_id: i64,
) -> StoreResult<std::collections::HashMap<PlaylistId, Vec<PlaylistEntry>>> {
    let mut statement = connection.prepare(
        "SELECT playlist_id, occurrence_id, track_id
         FROM source_playlist_entries
         WHERE library_id = ?1
         ORDER BY playlist_id, position",
    )?;
    group_playlist_entries(statement.query_map([library_id], |row| {
        Ok((
            PlaylistId::new(row.get::<_, String>(0)?),
            PlaylistEntry {
                occurrence_id: row.get(1)?,
                track_id: TrackId::new(row.get::<_, String>(2)?),
            },
        ))
    })?)
}

fn load_local_playlist_entries(
    connection: &Connection,
    source_id: &SourceId,
) -> StoreResult<std::collections::HashMap<PlaylistId, Vec<PlaylistEntry>>> {
    let mut statement = connection.prepare(
        "SELECT playlist_id, occurrence_id, track_id
         FROM local_playlist_entries
         WHERE source_id = ?1
         ORDER BY playlist_id, position",
    )?;
    group_playlist_entries(statement.query_map([source_id.as_str()], |row| {
        Ok((
            PlaylistId::new(row.get::<_, String>(0)?),
            PlaylistEntry {
                occurrence_id: row.get(1)?,
                track_id: TrackId::new(row.get::<_, String>(2)?),
            },
        ))
    })?)
}

fn group_playlist_entries(
    rows: rusqlite::MappedRows<
        '_,
        impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<(PlaylistId, PlaylistEntry)>,
    >,
) -> StoreResult<std::collections::HashMap<PlaylistId, Vec<PlaylistEntry>>> {
    let mut grouped = std::collections::HashMap::new();
    for row in rows {
        let (playlist_id, entry) = row?;
        grouped
            .entry(playlist_id)
            .or_insert_with(Vec::new)
            .push(entry);
    }
    Ok(grouped)
}

fn load_local_favorites(
    connection: &Connection,
    source_id: &SourceId,
) -> StoreResult<Vec<FavoriteItemId>> {
    let mut statement = connection
        .prepare("SELECT item_kind, item_id FROM local_favorites WHERE source_id = ?1")?;
    let mut favorites = Vec::new();
    for row in statement.query_map([source_id.as_str()], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })? {
        let (kind, id) = row?;
        favorites.push(match kind.as_str() {
            "album" => FavoriteItemId::Album(AlbumId::new(id)),
            "track" => FavoriteItemId::Track(TrackId::new(id)),
            "artist" => FavoriteItemId::Artist(ArtistId::new(id)),
            _ => {
                return Err(StoreError::InvalidValue {
                    kind: "Local favorite kind",
                    value: kind,
                });
            }
        });
    }
    Ok(favorites)
}

fn transfer_local_favorites(
    transaction: &Transaction<'_>,
    source_id: &SourceId,
    transfers: &[LocalFavoriteTransfer],
) -> StoreResult<()> {
    for transfer in transfers {
        transaction.execute(
            "INSERT OR IGNORE INTO local_favorites(source_id, item_kind, item_id)
             SELECT source_id, ?4, ?5
             FROM local_favorites
             WHERE source_id = ?1 AND item_kind = ?2 AND item_id = ?3",
            params![
                source_id.as_str(),
                transfer.removed.kind().as_str(),
                transfer.removed.as_str(),
                transfer.replacement.kind().as_str(),
                transfer.replacement.as_str(),
            ],
        )?;
        transaction.execute(
            "DELETE FROM local_favorites
             WHERE source_id = ?1 AND item_kind = ?2 AND item_id = ?3",
            params![
                source_id.as_str(),
                transfer.removed.kind().as_str(),
                transfer.removed.as_str(),
            ],
        )?;
    }
    Ok(())
}

fn load_local_favorites_for(
    connection: &Connection,
    source_id: &SourceId,
    targets: Vec<FavoriteItemId>,
) -> StoreResult<Vec<FavoriteItemId>> {
    let mut statement = connection.prepare(
        "SELECT 1 FROM local_favorites
         WHERE source_id = ?1 AND item_kind = ?2 AND item_id = ?3
         LIMIT 1",
    )?;
    let mut favorites = Vec::new();
    for target in targets {
        if statement
            .query_row(
                params![source_id.as_str(), target.kind().as_str(), target.as_str()],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            favorites.push(target);
        }
    }
    Ok(favorites)
}

fn apply_album_release_info(
    connection: &Connection,
    source_id: &SourceId,
    albums: &mut [Album],
) -> StoreResult<()> {
    let mut statement = connection.prepare(
        "SELECT
            album_id, exact_identity_key, lookup_state,
            release_types_json, is_compilation
         FROM album_release_info WHERE source_id = ?1",
    )?;
    let rows = statement
        .query_map([source_id.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                ),
            ))
        })?
        .collect::<Result<HashMap<_, _>, _>>()?;
    for album in albums {
        if !album.release_types.is_empty() {
            continue;
        }
        let Some(identity) = crate::album_release::release_identity(album) else {
            continue;
        };
        let identity_key = identity.stored_key();
        let Some((stored_identity, state, release_types, is_compilation)) =
            rows.get(album.id.as_str())
        else {
            continue;
        };
        if stored_identity != &identity_key {
            continue;
        }
        if state == "missing" {
            continue;
        }
        if state == "found"
            && let Some(release_types) = release_types
        {
            album.release_types = serde_json::from_str(release_types)?;
            album.is_compilation = is_compilation.map(|value| value != 0);
            continue;
        }
    }
    Ok(())
}

fn apply_exact_album_release_info(
    connection: &Connection,
    source_id: &SourceId,
    album: &mut Album,
) -> StoreResult<()> {
    let current_identity =
        crate::album_release::release_identity(album).map(|identity| identity.stored_key());
    let stored = connection
        .query_row(
            "SELECT
                exact_identity_key, lookup_state,
                release_types_json, is_compilation
             FROM album_release_info
             WHERE source_id = ?1 AND album_id = ?2",
            params![source_id.as_str(), album.id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((stored_identity, state, release_types, is_compilation)) = stored else {
        return Ok(());
    };
    if current_identity.as_deref() != Some(stored_identity.as_str()) {
        connection.execute(
            "DELETE FROM album_release_info
             WHERE source_id = ?1 AND album_id = ?2",
            params![source_id.as_str(), album.id.as_str()],
        )?;
        return Ok(());
    }
    if !album.release_types.is_empty() {
        return Ok(());
    }
    match (state.as_str(), release_types) {
        ("missing", None) => Ok(()),
        ("found", Some(release_types)) => {
            album.release_types = serde_json::from_str(&release_types)?;
            album.is_compilation = is_compilation.map(|value| value != 0);
            Ok(())
        }
        _ => Err(StoreError::InvalidValue {
            kind: "Album release lookup",
            value: format!("{state} for {}", album.id),
        }),
    }
}

fn cleanup_library_batch(connection: &mut Connection, library_id: i64) -> StoreResult<bool> {
    const TABLES: &[&str] = &[
        "source_playlist_entries",
        "source_playlists",
        "local_files",
        "albums",
        "tracks",
        "artists",
        "genres",
        "music_folders",
    ];
    for table in TABLES {
        let transaction = connection.transaction()?;
        let sql = format!(
            "DELETE FROM {table}
             WHERE rowid IN (
                 SELECT rowid FROM {table} WHERE library_id = ?1 LIMIT ?2
             )"
        );
        let removed = transaction.execute(
            &sql,
            params![
                library_id,
                i64::try_from(STORE_ROW_BATCH_LIMIT).unwrap_or(500)
            ],
        )?;
        transaction.commit()?;
        if removed > 0 {
            return Ok(false);
        }
    }
    connection.execute(
        "DELETE FROM source_libraries WHERE library_id = ?1",
        [library_id],
    )?;
    Ok(true)
}

struct ArtworkParts<'a> {
    kind: Option<&'static str>,
    path: Option<&'a str>,
    picture_index: Option<i64>,
    revision: Option<&'a str>,
}

fn artwork_parts(artwork: Option<&LocalArtworkRef>) -> ArtworkParts<'_> {
    match artwork {
        None => ArtworkParts {
            kind: None,
            path: None,
            picture_index: None,
            revision: None,
        },
        Some(LocalArtworkRef::File { path, revision }) => ArtworkParts {
            kind: Some("file"),
            path: Some(path),
            picture_index: None,
            revision: Some(revision),
        },
        Some(LocalArtworkRef::Embedded {
            path,
            picture_index,
            revision,
        }) => ArtworkParts {
            kind: Some("embedded"),
            path: Some(path),
            picture_index: Some(i64::from(*picture_index)),
            revision: Some(revision),
        },
    }
}

fn artwork_from_parts(
    kind: Option<String>,
    path: Option<String>,
    picture_index: Option<i64>,
    revision: Option<String>,
) -> StoreResult<Option<LocalArtworkRef>> {
    match (kind.as_deref(), path, picture_index, revision) {
        (None, None, None, None) => Ok(None),
        (Some("file"), Some(path), None, Some(revision)) => {
            Ok(Some(LocalArtworkRef::File { path, revision }))
        }
        (Some("embedded"), Some(path), Some(index), Some(revision)) => {
            Ok(Some(LocalArtworkRef::Embedded {
                path,
                picture_index: u32::try_from(index).map_err(|_| StoreError::IntegerRange)?,
                revision,
            }))
        }
        _ => Err(StoreError::InvalidValue {
            kind: "local artwork",
            value: "incomplete columns".to_string(),
        }),
    }
}

fn image_parts(image: Option<&ImageRef>) -> (Option<&str>, Option<&str>) {
    image.map_or((None, None), |image| {
        (Some(image.item_id.as_str()), image.tag.as_deref())
    })
}

fn image_from_parts(item_id: Option<String>, tag: Option<String>) -> Option<ImageRef> {
    item_id.map(|item_id| ImageRef { item_id, tag })
}

struct FreshnessParts<'a> {
    version: Option<i64>,
    marker: Option<&'a [u8]>,
}

fn freshness_parts(freshness: Option<&ProviderFreshness>) -> FreshnessParts<'_> {
    freshness.map_or(
        FreshnessParts {
            version: None,
            marker: None,
        },
        |freshness| FreshnessParts {
            version: Some(i64::from(freshness.version)),
            marker: Some(&freshness.marker),
        },
    )
}

fn estimate_album(album: &Album) -> StoreResult<usize> {
    Ok(256
        + album.title.len()
        + album.artist.len()
        + serde_json::to_vec(&album.relations)?.len()
        + serde_json::to_vec(&album.release_types)?.len())
}

fn estimate_track(track: &Track) -> StoreResult<usize> {
    Ok(384
        + track.title.len()
        + track.artist.len()
        + track.album.len()
        + serde_json::to_vec(&track.relations)?.len())
}

fn estimate_artist(artist: &Artist) -> StoreResult<usize> {
    Ok(128 + artist.name.len())
}

fn estimate_genre(genre: &Genre) -> StoreResult<usize> {
    Ok(96 + genre.name.len())
}

fn estimate_music_folder(folder: &MusicFolder) -> StoreResult<usize> {
    Ok(64 + folder.name.len())
}

fn estimate_playlist_entry(indexed: &(usize, &PlaylistEntry)) -> StoreResult<usize> {
    Ok(64 + indexed.1.occurrence_id.len() + indexed.1.track_id.as_str().len())
}

fn estimate_local_file(file: &LocalFile) -> StoreResult<usize> {
    Ok(192
        + file.path.len()
        + file.root.len()
        + file.relative_path.len()
        + serde_json::to_vec(&file.dependencies)?.len())
}

fn estimate_local_access_file(file: &LocalAccessFile) -> StoreResult<usize> {
    Ok(256
        + file.path.len()
        + file.root.len()
        + file.relative_path.len()
        + file.title.len()
        + file.album.len()
        + file.artist.len())
}

fn optional_sqlite_u64(value: Option<u64>) -> StoreResult<Option<i64>> {
    value
        .map(|value| i64::try_from(value).map_err(|_| StoreError::IntegerRange))
        .transpose()
}

fn sqlite_filesystem_identity(value: u64) -> i64 {
    i64::from_ne_bytes(value.to_ne_bytes())
}

fn filesystem_identity_from_sqlite(value: i64) -> u64 {
    u64::from_ne_bytes(value.to_ne_bytes())
}

fn checked_u64(value: i64) -> StoreResult<u64> {
    u64::try_from(value).map_err(|_| StoreError::IntegerRange)
}

fn checked_u32(value: i64) -> StoreResult<u32> {
    u32::try_from(value).map_err(|_| StoreError::IntegerRange)
}

fn checked_u16(value: i64) -> StoreResult<u16> {
    u16::try_from(value).map_err(|_| StoreError::IntegerRange)
}

fn optional_u32(value: Option<i64>) -> StoreResult<Option<u32>> {
    value.map(checked_u32).transpose()
}

fn optional_u16(value: Option<i64>) -> StoreResult<Option<u16>> {
    value.map(checked_u16).transpose()
}

fn optional_u8(value: Option<i64>) -> StoreResult<Option<u8>> {
    value
        .map(|value| u8::try_from(value).map_err(|_| StoreError::IntegerRange))
        .transpose()
}
