//! Owns the fixed SQLite writer, bounded read pool, and read cancellation.
//! It creates no runtime and contains no product query policy.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use sqlx::pool::PoolConnection;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection, SqlitePool, SqlitePoolOptions};
use sqlx::{Connection, Sqlite};
use tokio::sync::{Mutex, Notify, OwnedSemaphorePermit, Semaphore};

use crate::{LibraryError, LibraryResult, schema};

const PAGE_SIZE_BYTES: u32 = 4 * 1024;
const PAGE_CACHE_KIB: i32 = 1024;
const WRITER_STATEMENTS: usize = 64;
const READER_STATEMENTS: usize = 32;
const COMMAND_BUFFER: usize = 1;
const ROW_BUFFER: usize = 32;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(2);
const READER_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const PROGRESS_INTERVAL: i32 = 1_000;
const MIN_READERS: u32 = 1;
const MAX_READERS: u32 = 2;

/// Request-local cooperative cancellation for one non-Playback read.
#[derive(Debug, Default)]
struct CancellationInner {
    cancelled: AtomicBool,
    notify: Notify,
}

#[derive(Clone, Debug, Default)]
pub struct ReadCancellation(Arc<CancellationInner>);

impl ReadCancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
        self.0.notify.notify_waiters();
    }

    fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }

    pub(crate) async fn cancelled(&self) {
        loop {
            let notified = self.0.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

struct DatabaseInner {
    writer: Arc<Mutex<Option<SqliteConnection>>>,
    readers: SqlitePool,
    general_read: Arc<Semaphore>,
    active_scan: AtomicU64,
    next_scan: AtomicU64,
    distinct_track_covers: AtomicBool,
    _temporary_catalog: Option<tempfile::TempDir>,
}

/// Final ownership of one fixed writer and the bounded read-only pool.
#[derive(Clone)]
pub struct Database {
    inner: Arc<DatabaseInner>,
    fresh_start: bool,
}

impl Database {
    pub fn set_distinct_track_covers(&self, enabled: bool) {
        self.inner
            .distinct_track_covers
            .store(enabled, Ordering::Release);
    }

    pub fn distinct_track_covers(&self) -> bool {
        self.inner.distinct_track_covers.load(Ordering::Acquire)
    }
    pub async fn relocate(source: &Path, destination: &Path) -> LibraryResult<()> {
        if destination.exists() || !source.exists() {
            return Ok(());
        }
        let parent = destination.parent().ok_or_else(|| {
            LibraryError::InvalidRequest("the Store destination has no parent".to_string())
        })?;
        std::fs::create_dir_all(parent)?;
        let pending = destination.with_extension(format!("relocating-{}", std::process::id()));
        if pending.exists() {
            std::fs::remove_file(&pending)?;
        }
        let options = SqliteConnectOptions::new()
            .filename(source)
            .read_only(true)
            .create_if_missing(false)
            .busy_timeout(BUSY_TIMEOUT);
        let mut connection = SqliteConnection::connect_with(&options).await?;
        let result = sqlx::query("VACUUM INTO ?1")
            .bind(pending.to_string_lossy().as_ref())
            .execute(&mut connection)
            .await;
        connection.close().await?;
        result?;
        std::fs::OpenOptions::new()
            .write(true)
            .open(&pending)?
            .sync_all()?;
        std::fs::rename(&pending, destination)?;
        #[cfg(unix)]
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    }

    pub async fn open(path: impl AsRef<Path>) -> LibraryResult<Self> {
        Self::open_configured(path, &[], None).await
    }

    pub async fn open_configured(
        path: impl AsRef<Path>,
        configured: &[crate::SourceId],
        selected: Option<&crate::SourceId>,
    ) -> LibraryResult<Self> {
        let path = path.as_ref();
        Self::open_with_catalog(
            path,
            path.with_extension("catalog.sqlite"),
            configured,
            selected,
        )
        .await
    }

    pub async fn open_with_catalog(
        path: impl AsRef<Path>,
        catalog: impl AsRef<Path>,
        configured: &[crate::SourceId],
        selected: Option<&crate::SourceId>,
    ) -> LibraryResult<Self> {
        let path = path.as_ref();
        let catalog = catalog.as_ref();
        if path.exists() {
            let migrated = Self::migrate_released(path, path, configured, selected).await;
            if let Err(error) = migrated {
                if !is_store_content_failure(&error) {
                    return Err(error);
                }
                tracing::warn!(%error, "could not migrate released Store; opening fresh state");
                preserve_store(path)?;
                let mut database = Self::open_final(path, catalog).await?;
                database.fresh_start = true;
                return Ok(database);
            }
        }
        match Self::open_final(path, catalog).await {
            Ok(database) => Ok(database),
            Err(error) if is_store_content_failure(&error) => {
                tracing::warn!(%error, "could not use Store; opening fresh state");
                preserve_store(path)?;
                let mut database = Self::open_final(path, catalog).await?;
                database.fresh_start = true;
                Ok(database)
            }
            Err(error) => Err(error),
        }
    }

    pub async fn open_installation(
        path: &Path,
        legacy: &Path,
        catalog: &Path,
        configured: &[crate::SourceId],
        selected: Option<&crate::SourceId>,
    ) -> LibraryResult<Self> {
        if !path.exists() && legacy.exists() {
            if let Err(error) = Self::migrate_released(legacy, path, configured, selected).await {
                tracing::warn!(%error, "could not migrate old Store; original retained");
                let mut database = Self::open_final(path, catalog).await?;
                database.fresh_start = true;
                return Ok(database);
            }
        }
        Self::open_with_catalog(path, catalog, configured, selected).await
    }

    async fn migrate_released(
        input: &Path,
        destination: &Path,
        configured: &[crate::SourceId],
        selected: Option<&crate::SourceId>,
    ) -> LibraryResult<()> {
        let mut reader =
            SqliteConnection::connect_with(&base_options(input).read_only(true)).await?;
        let version = schema::pragma(&mut reader, "user_version").await;
        reader.close().await?;
        let version = version?;
        if (1..=43).contains(&version) {
            crate::migration::import_released(input, destination, configured, selected).await?;
        } else if input != destination {
            Self::relocate(input, destination).await?;
        }
        Ok(())
    }

    pub fn fresh_start(&self) -> bool {
        self.fresh_start
    }

    pub async fn close(self) -> LibraryResult<()> {
        self.inner.readers.close().await;
        if let Some(writer) = self.inner.writer.lock().await.take() {
            writer.close().await?;
        }
        Ok(())
    }

    pub(crate) async fn open_final(path: &Path, catalog: &Path) -> LibraryResult<Self> {
        let mut writer = open_writer(path).await?;
        if let Err(error) = schema::initialize_durable(&mut writer).await {
            writer.close().await?;
            return Err(error);
        }
        let (catalog, temporary_catalog) = prepare_catalog(catalog).await?;
        schema::attach_catalog(&mut writer, &catalog).await?;
        schema::initialize_local_activity(&mut writer).await?;
        let readers = open_readers(path, &catalog).await?;
        Ok(Self {
            inner: Arc::new(DatabaseInner {
                writer: Arc::new(Mutex::new(Some(writer))),
                readers,
                general_read: Arc::new(Semaphore::new(1)),
                active_scan: AtomicU64::new(0),
                next_scan: AtomicU64::new(1),
                distinct_track_covers: AtomicBool::new(false),
                _temporary_catalog: temporary_catalog,
            }),
            fresh_start: false,
        })
    }

    pub async fn source_identity_key(
        &self,
        source_id: &crate::SourceId,
    ) -> LibraryResult<Option<crate::SourceKey>> {
        let (_permit, mut connection) = self.acquire_general(&ReadCancellation::new()).await?;
        Ok(
            sqlx::query_scalar("SELECT source_key FROM catalog.sources WHERE object_id=?1")
                .bind(source_id.as_str())
                .fetch_optional(&mut *connection)
                .await?,
        )
    }

    pub async fn remove_source(&self, source_id: &crate::SourceId) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let source = sqlx::query_scalar::<_, i64>(
            "SELECT source_key FROM catalog.sources WHERE object_id=?1",
        )
        .bind(source_id.as_str())
        .fetch_optional(&mut *transaction)
        .await?;
        for kind in ["track", "album", "artist"] {
            let prefix = crate::keys::source_entity_prefix(source_id, kind);
            for query in [
                "DELETE FROM lyrics_cache WHERE substr(media_uri,1,length(?1))=?1 OR media_uri IN (SELECT media_uri FROM tracks WHERE source_key=?2)",
                "DELETE FROM user_media_state WHERE substr(media_uri,1,length(?1))=?1 OR media_uri IN (SELECT media_uri FROM tracks WHERE source_key=?2)",
                "DELETE FROM favorite_outbox WHERE substr(media_uri,1,length(?1))=?1 OR media_uri IN (SELECT media_uri FROM tracks WHERE source_key=?2)",
            ] {
                sqlx::query(query)
                    .bind(&prefix)
                    .bind(source)
                    .execute(&mut *transaction)
                    .await?;
            }
        }
        sqlx::query("DELETE FROM catalog.local_access_metadata WHERE access_uri IN (SELECT access_uri FROM main.local_locators locator JOIN main.source_ids identity USING(source_key) WHERE identity.object_id=?1)").bind(source_id.as_str()).execute(&mut *transaction).await?;
        // Accepted listens and global Playlist snapshots have no source foreign key.
        let removed = sqlx::query("DELETE FROM sources WHERE source_key=?1")
            .bind(source)
            .execute(&mut *transaction)
            .await?
            .rows_affected()
            == 1;
        let durable_removed = sqlx::query("DELETE FROM main.source_ids WHERE object_id=?1")
            .bind(source_id.as_str())
            .execute(&mut *transaction)
            .await?
            .rows_affected()
            == 1;
        transaction.commit().await?;
        Ok(removed || durable_removed)
    }

    pub(crate) async fn writer(
        &self,
    ) -> LibraryResult<tokio::sync::MutexGuard<'_, Option<SqliteConnection>>> {
        let writer = self.inner.writer.lock().await;
        if writer.is_none() {
            return Err(LibraryError::WriterUnavailable);
        }
        Ok(writer)
    }

    pub(crate) async fn writer_owned(
        &self,
    ) -> LibraryResult<tokio::sync::OwnedMutexGuard<Option<SqliteConnection>>> {
        let writer = Arc::clone(&self.inner.writer).lock_owned().await;
        if writer.is_none() {
            return Err(LibraryError::WriterUnavailable);
        }
        Ok(writer)
    }

    pub(crate) async fn acquire_general(
        &self,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<(OwnedSemaphorePermit, PoolConnection<Sqlite>)> {
        let permit = tokio::select! {
            permit = Arc::clone(&self.inner.general_read).acquire_owned() => {
                permit.expect("Database retains the general-read semaphore")
            }
            () = cancellation.cancelled() => return Err(LibraryError::ReadCancelled),
        };
        if cancellation.is_cancelled() {
            return Err(LibraryError::ReadCancelled);
        }
        let mut connection = tokio::select! {
            connection = self.inner.readers.acquire() => connection?,
            () = cancellation.cancelled() => return Err(LibraryError::ReadCancelled),
        };
        let cancellation = cancellation.clone();
        connection
            .lock_handle()
            .await?
            .set_progress_handler(PROGRESS_INTERVAL, move || !cancellation.is_cancelled());
        Ok((permit, connection))
    }

    // Playback and bulk exports bypass the route-read permit, sharing the bounded pool.
    pub(crate) async fn acquire_reader(&self) -> LibraryResult<PoolConnection<Sqlite>> {
        Ok(self.inner.readers.acquire().await?)
    }

    pub(crate) async fn clear_progress(
        connection: &mut PoolConnection<Sqlite>,
    ) -> LibraryResult<()> {
        connection.lock_handle().await?.remove_progress_handler();
        Ok(())
    }

    pub(crate) fn begin_scan(&self) -> LibraryResult<u64> {
        let token = self.inner.next_scan.fetch_add(1, Ordering::Relaxed);
        self.inner
            .active_scan
            .compare_exchange(0, token, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| LibraryError::ScanInProgress)?;
        Ok(token)
    }

    pub(crate) fn scan_is_current(&self, token: u64) -> bool {
        self.inner.active_scan.load(Ordering::Acquire) == token
    }

    pub(crate) fn release_scan(&self, token: u64) {
        let _ =
            self.inner
                .active_scan
                .compare_exchange(token, 0, Ordering::AcqRel, Ordering::Acquire);
    }

    pub(crate) fn writer_failed(&self) {
        self.inner.active_scan.store(0, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn reader_pool(&self) -> &SqlitePool {
        &self.inner.readers
    }

    #[cfg(test)]
    pub(crate) async fn fail_writer(&self) -> LibraryResult<()> {
        let mut writer = self.inner.writer.lock().await;
        if let Some(connection) = writer.take() {
            connection.close().await?;
        }
        self.writer_failed();
        Ok(())
    }
}

pub(crate) async fn open_writer(path: &Path) -> LibraryResult<SqliteConnection> {
    let options = base_options(path)
        .create_if_missing(true)
        .statement_cache_capacity(WRITER_STATEMENTS)
        .thread_name(|id| format!("rufin-library-writer-{id}"));
    let mut connection = SqliteConnection::connect_with(&options).await?;
    // Own the handle before configuring it so failures can await its release before replacement.
    let result = sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "PRAGMA page_size={PAGE_SIZE_BYTES}; PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL; PRAGMA cache_size=-{PAGE_CACHE_KIB};
         PRAGMA temp_store=FILE; PRAGMA mmap_size=0;"
    )))
    .execute(&mut connection)
    .await;
    if let Err(error) = result {
        connection.close().await?;
        return Err(error.into());
    }
    Ok(connection)
}

fn reader_options(path: &Path) -> SqliteConnectOptions {
    base_options(path)
        .pragma("cache_size", (-PAGE_CACHE_KIB).to_string())
        .pragma("temp_store", "FILE")
        .pragma("mmap_size", "0")
        .with_regexp()
        .read_only(true)
        .create_if_missing(false)
        .statement_cache_capacity(READER_STATEMENTS)
        .thread_name(|id| format!("rufin-library-reader-{id}"))
}

fn base_options(path: &Path) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(path)
        .shared_cache(false)
        .busy_timeout(BUSY_TIMEOUT)
        .command_buffer_size(COMMAND_BUFFER)
        .row_buffer_size(ROW_BUFFER)
}

async fn open_readers(path: &Path, catalog: &Path) -> LibraryResult<SqlitePool> {
    let catalog = catalog.to_path_buf();
    Ok(SqlitePoolOptions::new()
        .min_connections(MIN_READERS)
        .max_connections(MAX_READERS)
        .acquire_timeout(ACQUIRE_TIMEOUT)
        .idle_timeout(READER_IDLE_TIMEOUT)
        .max_lifetime(None)
        .test_before_acquire(true)
        .after_connect(move |connection, _| {
            let catalog = catalog.clone();
            Box::pin(async move {
                schema::attach_catalog(connection, &catalog)
                    .await
                    .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
                sqlx::query("PRAGMA query_only=ON")
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .after_release(|connection, _| {
            Box::pin(async move {
                connection.lock_handle().await?.remove_progress_handler();
                Ok(true)
            })
        })
        .connect_with(reader_options(path))
        .await?)
}

async fn prepare_catalog(
    path: &Path,
) -> LibraryResult<(std::path::PathBuf, Option<tempfile::TempDir>)> {
    async fn open(path: &Path) -> LibraryResult<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut connection = open_writer(path).await?;
        let result = schema::initialize_catalog(&mut connection).await;
        connection.close().await?;
        result
    }
    match open(path).await {
        Ok(()) => return Ok((path.to_path_buf(), None)),
        Err(error) if is_store_content_failure(&error) => {
            for suffix in ["", "-wal", "-shm"] {
                let mut name = path.as_os_str().to_os_string();
                name.push(suffix);
                match std::fs::remove_file(name) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => break,
                }
            }
            if open(path).await.is_ok() {
                return Ok((path.to_path_buf(), None));
            }
        }
        Err(_) => {}
    }
    let directory = tempfile::Builder::new()
        .prefix("rufin-catalog-")
        .tempdir()?;
    let path = directory.path().join("catalog.sqlite");
    open(&path).await?;
    Ok((path, Some(directory)))
}

pub(crate) fn preserve_store(path: &Path) -> std::io::Result<()> {
    let destination = path.with_extension(format!(
        "unusable-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    for suffix in ["", "-wal", "-shm"] {
        let mut from = path.as_os_str().to_os_string();
        from.push(suffix);
        let mut to = destination.as_os_str().to_os_string();
        to.push(suffix);
        if Path::new(&from).exists() {
            std::fs::rename(from, to)?;
        }
    }
    Ok(())
}

fn is_store_content_failure(error: &LibraryError) -> bool {
    match error {
        LibraryError::InvalidStore(_) | LibraryError::Migration(_) | LibraryError::Json(_) => true,
        LibraryError::Sqlite(sqlx::Error::Database(error)) => error
            .code()
            .as_deref()
            .and_then(|code| code.parse::<i32>().ok())
            .is_some_and(|code| matches!(code & 0xff, 1 | 11 | 19 | 20 | 26)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Row;
    use tokio::time::{Instant, sleep, timeout};

    async fn database() -> (tempfile::TempDir, Database) {
        let directory = tempfile::tempdir().expect("temporary Store directory");
        let database = Database::open(directory.path().join("library.sqlite3"))
            .await
            .expect("open SQLx Library");
        (directory, database)
    }

    #[tokio::test]
    async fn fixed_writer_and_steady_reader_have_explicit_limits() {
        let (_directory, database) = database().await;
        assert_eq!(database.reader_pool().size(), MIN_READERS);
        assert_eq!(
            database.reader_pool().options().get_min_connections(),
            MIN_READERS
        );
        assert_eq!(
            database.reader_pool().options().get_max_connections(),
            MAX_READERS
        );
        assert_eq!(
            database.reader_pool().options().get_acquire_timeout(),
            ACQUIRE_TIMEOUT
        );
        assert_eq!(
            database.reader_pool().options().get_idle_timeout(),
            Some(READER_IDLE_TIMEOUT)
        );

        let cancellation = ReadCancellation::new();
        let (_permit, mut reader) = database
            .acquire_general(&cancellation)
            .await
            .expect("acquire general reader");
        let row = sqlx::query(
            "SELECT
                 (SELECT journal_mode FROM pragma_journal_mode),
                 (SELECT query_only FROM pragma_query_only),
                 (SELECT cache_size FROM pragma_cache_size),
                 (SELECT temp_store FROM pragma_temp_store),
                 (SELECT page_size FROM pragma_page_size)",
        )
        .fetch_one(&mut *reader)
        .await
        .expect("read fixed reader options");
        assert_eq!(row.get::<String, _>(0), "wal");
        assert_eq!(row.get::<i64, _>(1), 1);
        assert_eq!(row.get::<i64, _>(2), i64::from(-PAGE_CACHE_KIB));
        assert_eq!(row.get::<i64, _>(3), 1);
        assert_eq!(row.get::<i64, _>(4), i64::from(PAGE_SIZE_BYTES));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("PRAGMA mmap_size")
                .fetch_one(&mut *reader)
                .await
                .expect("read reader mmap bound"),
            0
        );
        assert!(
            sqlx::query("CREATE TABLE reader_write(value INTEGER)")
                .execute(&mut *reader)
                .await
                .is_err(),
            "pooled reader accepted a write"
        );
        Database::clear_progress(&mut reader)
            .await
            .expect("clear reader progress handler");

        let mut writer = database.writer().await.expect("acquire fixed writer");
        let writer = writer.as_mut().expect("writer remains available");
        let writer_options = sqlx::query_as::<_, (String, i64, i64)>(
            "SELECT
                 (SELECT journal_mode FROM pragma_journal_mode),
                 (SELECT cache_size FROM pragma_cache_size),
                 (SELECT temp_store FROM pragma_temp_store)",
        )
        .fetch_one(&mut *writer)
        .await
        .expect("read fixed writer options");
        assert_eq!(writer_options, ("wal".to_string(), -1024, 1));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("PRAGMA mmap_size")
                .fetch_one(&mut *writer)
                .await
                .expect("read writer mmap bound"),
            0
        );
    }

    #[tokio::test]
    async fn playback_bypass_uses_second_slot_but_third_general_read_waits_fairly() {
        let (_directory, database) = database().await;
        let cancellation = ReadCancellation::new();
        let (permit, mut general) = database
            .acquire_general(&cancellation)
            .await
            .expect("acquire suspended general read");

        let mut playback = database
            .acquire_reader()
            .await
            .expect("acquire Playback bypass");
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT 1")
                .fetch_one(&mut *playback)
                .await
                .expect("run Playback point read"),
            1
        );
        assert_eq!(database.reader_pool().size(), MAX_READERS);

        let waiting_database = database.clone();
        let waiting = tokio::spawn(async move {
            waiting_database
                .acquire_general(&ReadCancellation::new())
                .await
        });
        sleep(Duration::from_millis(50)).await;
        assert!(
            !waiting.is_finished(),
            "a third general read bypassed the permit"
        );
        drop(playback);
        sleep(Duration::from_millis(50)).await;
        assert!(
            !waiting.is_finished(),
            "a free pool slot bypassed the permit"
        );

        Database::clear_progress(&mut general)
            .await
            .expect("clear suspended read handler");
        drop(general);
        drop(permit);
        let (_permit, mut admitted) = timeout(Duration::from_secs(1), waiting)
            .await
            .expect("third general read admitted after permit release")
            .expect("general read task joined")
            .expect("general read acquired");
        Database::clear_progress(&mut admitted)
            .await
            .expect("clear admitted handler");
    }

    #[tokio::test]
    async fn cancelled_general_read_returns_a_clean_usable_connection() {
        let (_directory, database) = database().await;
        let cancellation = ReadCancellation::new();
        let task_database = database.clone();
        let task_cancellation = cancellation.clone();
        let read = tokio::spawn(async move {
            let (_permit, mut connection) =
                task_database.acquire_general(&task_cancellation).await?;
            let result = sqlx::query_scalar::<_, i64>(
                "WITH RECURSIVE count(value) AS (
                     VALUES(0) UNION ALL SELECT value + 1 FROM count
                 ) SELECT sum(value) FROM count",
            )
            .fetch_one(&mut *connection)
            .await;
            Database::clear_progress(&mut connection).await?;
            result.map_err(LibraryError::from)
        });
        sleep(Duration::from_millis(20)).await;
        cancellation.cancel();
        let error = timeout(Duration::from_secs(2), read)
            .await
            .expect("cancelled SQLite read completed")
            .expect("cancelled read task joined")
            .expect_err("progress handler interrupted the read");
        assert!(error.to_string().contains("interrupted"), "{error}");

        let (_permit, mut connection) = database
            .acquire_general(&ReadCancellation::new())
            .await
            .expect("reacquire cleaned reader");
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT 7")
                .fetch_one(&mut *connection)
                .await
                .expect("reader remains usable"),
            7
        );
        Database::clear_progress(&mut connection)
            .await
            .expect("clear replacement read handler");
    }

    #[tokio::test]
    async fn dynamic_reader_skips_journal_mode_and_is_idle_reapable() {
        let (directory, database) = database().await;
        let cancellation = ReadCancellation::new();
        let (_permit, mut general) = database
            .acquire_general(&cancellation)
            .await
            .expect("hold steady reader");
        let mut writer = database.writer().await.expect("acquire writer");
        sqlx::raw_sql("BEGIN IMMEDIATE")
            .execute(writer.as_mut().expect("writer available"))
            .await
            .expect("hold writer transaction");
        let mut optional = database
            .acquire_reader()
            .await
            .expect("open optional reader without journal write");
        assert_eq!(
            sqlx::query_scalar::<_, String>("PRAGMA journal_mode")
                .fetch_one(&mut *optional)
                .await
                .expect("optional reader observes WAL"),
            "wal"
        );
        sqlx::raw_sql("ROLLBACK")
            .execute(writer.as_mut().expect("writer available"))
            .await
            .expect("release writer transaction");
        drop(writer);
        drop(optional);
        Database::clear_progress(&mut general)
            .await
            .expect("clear steady reader handler");
        drop(general);
        assert_eq!(
            database.reader_pool().options().get_idle_timeout(),
            Some(READER_IDLE_TIMEOUT)
        );

        let idle_timeout = Duration::from_millis(100);
        let pool = SqlitePoolOptions::new()
            .min_connections(MIN_READERS)
            .max_connections(MAX_READERS)
            .acquire_timeout(ACQUIRE_TIMEOUT)
            .idle_timeout(idle_timeout)
            .max_lifetime(None)
            .connect_with(reader_options(&directory.path().join("library.sqlite3")))
            .await
            .expect("open short-deadline reader pool");
        let first = pool.acquire().await.expect("acquire steady reader");
        let second = pool.acquire().await.expect("acquire optional reader");
        drop(first);
        drop(second);
        sleep(idle_timeout * 3).await;
        assert_eq!(pool.size(), MIN_READERS);
        pool.close().await;
    }

    #[tokio::test]
    async fn reader_replacement_preserves_idempotent_reads() {
        let (_directory, database) = database().await;
        {
            let mut writer = database.writer().await.expect("acquire writer");
            sqlx::query("INSERT INTO sources(object_id,display_name,normalized_name,catalog_digest,artwork_digest) VALUES ('source','Source','source',zeroblob(32),zeroblob(32))")
                .execute(writer.as_mut().expect("writer available"))
                .await
                .expect("insert idempotent read row");
        }
        let mut reader = database
            .acquire_reader()
            .await
            .expect("acquire reader to replace");
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM sources")
                .fetch_one(&mut *reader)
                .await
                .expect("first idempotent read"),
            1
        );
        reader.close_on_drop();
        drop(reader);

        let deadline = Instant::now() + Duration::from_secs(2);
        while database.reader_pool().size() < MIN_READERS && Instant::now() < deadline {
            sleep(Duration::from_millis(20)).await;
        }
        let mut replacement = database
            .acquire_reader()
            .await
            .expect("acquire replacement reader");
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM sources")
                .fetch_one(&mut *replacement)
                .await
                .expect("replacement idempotent read"),
            1
        );
    }
}

pub(crate) async fn write_source_identity(
    connection: &mut SqliteConnection,
    source_id: &str,
) -> LibraryResult<i64> {
    if source_id.is_empty() {
        return Err(LibraryError::InvalidRequest(
            "source identity cannot be empty".into(),
        ));
    }
    sqlx::query(
        "INSERT INTO main.source_ids(object_id) VALUES(?1) ON CONFLICT(object_id) DO NOTHING",
    )
    .bind(source_id)
    .execute(&mut *connection)
    .await?;
    Ok(
        sqlx::query_scalar("SELECT source_key FROM main.source_ids WHERE object_id=?1")
            .bind(source_id)
            .fetch_one(connection)
            .await?,
    )
}
