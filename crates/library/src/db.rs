//! Owns the fixed SQLite writer, bounded read pool, and read cancellation.
//! It creates no runtime and contains no product query policy.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use sqlx::pool::PoolConnection;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection, SqlitePool, SqlitePoolOptions};
use sqlx::{Connection, Sqlite};
use tokio::sync::{Mutex, Notify, OwnedSemaphorePermit, Semaphore};

use crate::{LibraryError, LibraryResult, schema};

const PAGE_SIZE_BYTES: u32 = 4 * 1024;
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
    export_readers: SqlitePool,
    export_read: tokio::sync::Mutex<()>,
    general_read: Arc<Semaphore>,
    acquisition: Arc<Mutex<()>>,
    distinct_track_covers: AtomicBool,
}

/// Final ownership of one fixed writer and the bounded read-only pool.
#[derive(Clone)]
pub struct Database {
    inner: Arc<DatabaseInner>,
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
        Self::open_with_legacy_catalog(
            path,
            path.with_extension("catalog.sqlite"),
            configured,
            selected,
        )
        .await
    }

    pub async fn open_with_legacy_catalog(
        path: impl AsRef<Path>,
        catalog: impl AsRef<Path>,
        configured: &[crate::SourceId],
        selected: Option<&crate::SourceId>,
    ) -> LibraryResult<Self> {
        let path = path.as_ref();
        let catalog = catalog.as_ref();
        if path.exists() {
            Self::migrate_released(path, path, catalog, configured, selected).await?;
        }
        Self::open_final(path).await
    }

    pub async fn open_installation(
        path: &Path,
        legacy: &Path,
        catalog: &Path,
        configured: &[crate::SourceId],
        selected: Option<&crate::SourceId>,
    ) -> LibraryResult<Self> {
        if !path.exists() && legacy.exists() {
            Self::migrate_released(legacy, path, catalog, configured, selected).await?;
        }
        Self::open_with_legacy_catalog(path, catalog, configured, selected).await
    }

    async fn migrate_released(
        input: &Path,
        destination: &Path,
        catalog: &Path,
        configured: &[crate::SourceId],
        selected: Option<&crate::SourceId>,
    ) -> LibraryResult<()> {
        let mut reader =
            SqliteConnection::connect_with(&base_options(input).read_only(true)).await?;
        let version = schema::pragma(&mut reader, "user_version").await;
        reader.close().await?;
        match version? {
            1..=43 => {
                crate::migration::import_released(input, destination, catalog, configured, selected)
                    .await?
            }
            44..=47 => crate::migration::import_current(input, destination, catalog).await?,
            0 | 56 | 57 => {
                if input != destination {
                    Self::relocate(input, destination).await?;
                }
            }
            version => {
                return Err(LibraryError::Migration(format!(
                    "unsupported library schema {version}"
                )));
            }
        }
        Ok(())
    }

    pub async fn close(self) -> LibraryResult<()> {
        tokio::join!(
            self.inner.export_readers.close(),
            self.inner.readers.close()
        );
        if let Some(writer) = self.inner.writer.lock().await.take() {
            writer.close().await?;
        }
        Ok(())
    }

    pub(crate) async fn open_final(path: &Path) -> LibraryResult<Self> {
        let mut writer = open_writer(path).await?;
        let initialized = async {
            schema::initialize_durable(&mut writer).await?;
            crate::connect::initialize(&mut writer).await?;
            crate::collections::refresh_facts_on(&mut writer).await?;
            sqlx::raw_sql("PRAGMA optimize=0x10002")
                .execute(&mut writer)
                .await?;
            LibraryResult::Ok(())
        }
        .await;
        if let Err(error) = initialized {
            writer.close().await?;
            return Err(error);
        }
        let readers = open_readers(path, MIN_READERS, MAX_READERS).await?;
        let export_readers = open_readers(path, 0, 1).await?;
        Ok(Self {
            inner: Arc::new(DatabaseInner {
                writer: Arc::new(Mutex::new(Some(writer))),
                readers,
                export_readers,
                export_read: tokio::sync::Mutex::new(()),
                general_read: Arc::new(Semaphore::new(1)),
                acquisition: Arc::new(Mutex::new(())),
                distinct_track_covers: AtomicBool::new(false),
            }),
        })
    }

    pub async fn source_identity_key(
        &self,
        source_id: &crate::SourceId,
    ) -> LibraryResult<Option<crate::SourceKey>> {
        let (_permit, mut connection) = self.acquire_general(&ReadCancellation::new()).await?;
        Ok(
            sqlx::query_scalar("SELECT source_key FROM sources WHERE object_id=?1")
                .bind(source_id.as_str())
                .fetch_optional(&mut *connection)
                .await?,
        )
    }

    pub async fn remove_source(&self, source_id: &crate::SourceId) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let source =
            sqlx::query_scalar::<_, i64>("SELECT source_key FROM sources WHERE object_id=?1")
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
        sqlx::query("DELETE FROM local_access_metadata WHERE access_uri IN (SELECT access_uri FROM main.local_locators locator JOIN sources identity USING(source_key) WHERE identity.object_id=?1) AND NOT EXISTS(SELECT 1 FROM local_locators other WHERE other.access_uri=local_access_metadata.access_uri AND other.source_key IS NOT ?2)").bind(source_id.as_str()).bind(source).execute(&mut *transaction).await?;
        // Referenced metadata survives source removal.
        sqlx::query("UPDATE tracks SET indexed=0 WHERE source_key=?1")
            .bind(source)
            .execute(&mut *transaction)
            .await?;
        sqlx::query("UPDATE albums SET indexed=0 WHERE source_key=?1")
            .bind(source)
            .execute(&mut *transaction)
            .await?;
        sqlx::query("UPDATE artists SET indexed=0 WHERE source_key=?1")
            .bind(source)
            .execute(&mut *transaction)
            .await?;
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

    // Playback point reads bypass the route-read permit.
    pub(crate) async fn acquire_reader(&self) -> LibraryResult<PoolConnection<Sqlite>> {
        Ok(self.inner.readers.acquire().await?)
    }

    pub(crate) async fn acquire_export(
        &self,
    ) -> LibraryResult<(tokio::sync::MutexGuard<'_, ()>, PoolConnection<Sqlite>)> {
        let permit = self.inner.export_read.lock().await;
        Ok((permit, self.inner.export_readers.acquire().await?))
    }

    pub(crate) fn acquire_scan(&self) -> LibraryResult<tokio::sync::OwnedMutexGuard<()>> {
        self.inner
            .acquisition
            .clone()
            .try_lock_owned()
            .map_err(|_| LibraryError::ScanInProgress)
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
         PRAGMA synchronous=NORMAL;
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

async fn open_readers(path: &Path, minimum: u32, maximum: u32) -> LibraryResult<SqlitePool> {
    let options = SqlitePoolOptions::new()
        .min_connections(minimum)
        .max_connections(maximum)
        .acquire_timeout(ACQUIRE_TIMEOUT)
        .idle_timeout(READER_IDLE_TIMEOUT)
        .max_lifetime(None)
        .test_before_acquire(true)
        .after_connect(move |connection, _| {
            Box::pin(async move {
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
        });
    if minimum == 0 {
        Ok(options.connect_lazy_with(reader_options(path)))
    } else {
        Ok(options.connect_with(reader_options(path)).await?)
    }
}

pub(crate) fn preserve_store(path: &Path) -> std::io::Result<()> {
    let destination = path.with_extension(format!(
        "before-migration-{}",
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
    async fn close_waits_for_exports_and_closes_both_reader_pools() {
        use futures_util::FutureExt;

        let (_directory, database) = database().await;
        let export = database.acquire_export().await.unwrap();
        let mut closing = Box::pin(database.clone().close());
        assert!(closing.as_mut().now_or_never().is_none());
        assert!(database.inner.readers.is_closed());
        assert!(database.inner.export_readers.is_closed());
        drop(export);
        timeout(Duration::from_secs(1), closing)
            .await
            .unwrap()
            .unwrap();
        assert!(database.acquire_export().await.is_err());
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
                 (SELECT temp_store FROM pragma_temp_store),
                 (SELECT page_size FROM pragma_page_size)",
        )
        .fetch_one(&mut *reader)
        .await
        .expect("read fixed reader options");
        assert_eq!(row.get::<String, _>(0), "wal");
        assert_eq!(row.get::<i64, _>(1), 1);
        assert_eq!(row.get::<i64, _>(2), 1);
        assert_eq!(row.get::<i64, _>(3), i64::from(PAGE_SIZE_BYTES));
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

        let mut writer = database.writer().await.expect("acquire fixed writer");
        let writer = writer.as_mut().expect("writer remains available");
        let writer_options = sqlx::query_as::<_, (String, i64)>(
            "SELECT
                 (SELECT journal_mode FROM pragma_journal_mode),
                 (SELECT temp_store FROM pragma_temp_store)",
        )
        .fetch_one(&mut *writer)
        .await
        .expect("read fixed writer options");
        assert_eq!(writer_options, ("wal".to_string(), 1));
        let synchronous = sqlx::query_scalar::<_, i64>("PRAGMA synchronous")
            .fetch_one(&mut *writer)
            .await
            .expect("read writer synchronous mode");
        assert_eq!(synchronous, 1);
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
        let (permit, general) = database
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

        drop(general);
        drop(permit);
        let (_permit, _admitted) = timeout(Duration::from_secs(1), waiting)
            .await
            .expect("third general read admitted after permit release")
            .expect("general read task joined")
            .expect("general read acquired");
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
            sqlx::query_scalar::<_, i64>(
                "WITH RECURSIVE count(value) AS (
                     VALUES(0) UNION ALL SELECT value + 1 FROM count
                 ) SELECT sum(value) FROM count",
            )
            .fetch_one(&mut *connection)
            .await
            .map_err(LibraryError::from)
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
    }

    #[tokio::test]
    async fn dynamic_reader_skips_journal_mode_and_is_idle_reapable() {
        let (directory, database) = database().await;
        let cancellation = ReadCancellation::new();
        let (_permit, general) = database
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
            sqlx::query("INSERT INTO sources(object_id,display_name,normalized_name,artwork_digest) VALUES ('source','Source','source',zeroblob(32))")
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
    sqlx::query("INSERT INTO sources(object_id) VALUES(?1) ON CONFLICT(object_id) DO NOTHING")
        .bind(source_id)
        .execute(&mut *connection)
        .await?;
    Ok(
        sqlx::query_scalar("SELECT source_key FROM sources WHERE object_id=?1")
            .bind(source_id)
            .fetch_one(connection)
            .await?,
    )
}
