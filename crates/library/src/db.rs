//! Owns the fixed SQLite writer, bounded read pool, and read cancellation.
//! It creates no runtime and contains no product query policy.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use sqlx::pool::PoolConnection;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteConnection, SqliteJournalMode, SqlitePool, SqlitePoolOptions,
    SqliteSynchronous,
};
use sqlx::{Connection, Sqlite};
use tokio::sync::{Mutex, Notify, OwnedSemaphorePermit, Semaphore};

use crate::{LibraryError, LibraryResult, recovery, schema};

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

    async fn cancelled(&self) {
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
}

/// Final ownership of one fixed writer and the bounded read-only pool.
#[derive(Clone)]
pub struct Database {
    inner: Arc<DatabaseInner>,
}

impl Database {
    pub async fn open(path: impl AsRef<Path>) -> LibraryResult<Self> {
        let path = path.as_ref().to_path_buf();
        match Self::open_final(&path).await {
            Ok(database) => return Ok(database),
            Err(error) if recovery::is_store_content_failure(&error) => {}
            Err(error) => return Err(error),
        }
        if matches!(recovery::is_migratable_legacy(&path).await, Ok(true)) {
            if let Err(error) = recovery::migrate_legacy(&path).await {
                if recovery::is_store_content_failure(&error) {
                    recovery::repair_legacy(&path).await?;
                } else {
                    return Err(error);
                }
            }
        } else if matches!(recovery::is_repairable_legacy(&path).await, Ok(true)) {
            recovery::repair_legacy(&path).await?;
        } else {
            recovery::rebuild_unusable(&path).await?;
        }
        Self::open_final(&path).await
    }

    async fn open_final(path: &Path) -> LibraryResult<Self> {
        let mut writer = open_writer(&path).await?;
        if let Err(error) = schema::initialize(&mut writer).await {
            writer.close().await?;
            return Err(error);
        }
        let readers = open_readers(&path).await?;
        Ok(Self {
            inner: Arc::new(DatabaseInner {
                writer: Arc::new(Mutex::new(Some(writer))),
                readers,
                general_read: Arc::new(Semaphore::new(1)),
                active_scan: AtomicU64::new(0),
                next_scan: AtomicU64::new(1),
            }),
        })
    }

    pub async fn remove_source(&self, source: crate::SourceKey) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        // Foreign keys preserve accepted listens while cascading source-owned state.
        let removed = sqlx::query("DELETE FROM sources WHERE source_key=?1")
            .bind(source)
            .execute(&mut *transaction)
            .await?
            .rows_affected()
            == 1;
        transaction.commit().await?;
        Ok(removed)
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

    // Playback resolution bypasses the fair route-read permit but still uses the bounded pool.
    #[allow(dead_code)]
    pub(crate) async fn acquire_playback(&self) -> LibraryResult<PoolConnection<Sqlite>> {
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
    Ok(SqliteConnection::connect_with(&writer_options(path)).await?)
}

fn writer_options(path: &Path) -> SqliteConnectOptions {
    base_options(path)
        .create_if_missing(true)
        .page_size(PAGE_SIZE_BYTES)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .statement_cache_capacity(WRITER_STATEMENTS)
        .thread_name(|id| format!("rufin-library-writer-{id}"))
}

fn reader_options(path: &Path) -> SqliteConnectOptions {
    base_options(path)
        .read_only(true)
        .create_if_missing(false)
        .pragma("query_only", "ON")
        .statement_cache_capacity(READER_STATEMENTS)
        .thread_name(|id| format!("rufin-library-reader-{id}"))
}

fn base_options(path: &Path) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(path)
        .shared_cache(false)
        .busy_timeout(BUSY_TIMEOUT)
        .pragma("cache_size", (-PAGE_CACHE_KIB).to_string())
        .pragma("temp_store", "FILE")
        .pragma("mmap_size", "0")
        .command_buffer_size(COMMAND_BUFFER)
        .row_buffer_size(ROW_BUFFER)
}

async fn open_readers(path: &Path) -> LibraryResult<SqlitePool> {
    Ok(SqlitePoolOptions::new()
        .min_connections(MIN_READERS)
        .max_connections(MAX_READERS)
        .acquire_timeout(ACQUIRE_TIMEOUT)
        .idle_timeout(READER_IDLE_TIMEOUT)
        .max_lifetime(None)
        .test_before_acquire(true)
        .after_release(|connection, _| {
            Box::pin(async move {
                connection.lock_handle().await?.remove_progress_handler();
                Ok(true)
            })
        })
        .connect_with(reader_options(path))
        .await?)
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
            .acquire_playback()
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
            .acquire_playback()
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
            sqlx::query(
                "INSERT INTO sources(
                     object_id, display_name, normalized_name,
                     catalog_digest, artwork_digest
                 ) VALUES ('source', 'Source', 'source', zeroblob(32), zeroblob(32))",
            )
            .execute(writer.as_mut().expect("writer available"))
            .await
            .expect("insert idempotent read row");
        }
        let mut reader = database
            .acquire_playback()
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
            .acquire_playback()
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
