//! Owns the Store (the fixed SQLite writer, bounded read pools and read cancellation) and the
//! resident Catalog state. It creates no runtime and contains no product query policy.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};

use sqlx::pool::PoolConnection;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection, SqlitePool, SqlitePoolOptions};
use sqlx::{Connection, Sqlite};
use tokio::sync::{Mutex, Notify, broadcast, watch};

use crate::catalog::image;
use crate::catalog::snapshot::Snapshot;
use crate::catalog::write::{Change, Family};
use crate::{LibraryError, LibraryResult, SourceKey, schema};

const PAGE_SIZE_BYTES: u32 = 4 * 1024;
const WRITER_STATEMENTS: usize = 128;
const READER_STATEMENTS: usize = 32;
const ROW_BUFFER: usize = 32;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(2);
const READER_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const PROGRESS_INTERVAL: i32 = 1_000;
const MIN_READERS: u32 = 1;
const MAX_READERS: u32 = 2;
/// Changes a subscriber may fall behind by before it hears it lagged.
const CHANGE_BUFFER: usize = 1_024;
/// The Catalog image is written once no Write changed the catalog for this long after one did.
const IMAGE_QUIET: Duration = Duration::from_secs(10);
/// A Catalog with more than one removed slot in this many is loaded again before its image is
/// written, so neither memory nor the image keeps them.
const REMOVED_SLOTS: usize = 16;

/// Request-local cooperative cancellation for one read.
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

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }

    pub async fn cancelled(&self) {
        loop {
            let notified = self.0.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

/// The SQLite file and its connections.
struct Store {
    path: PathBuf,
    writer: Arc<Mutex<Option<SqliteConnection>>>,
    readers: SqlitePool,
    /// Playback's own reader, so a track change never waits behind general reads.
    playable_readers: SqlitePool,
    export_readers: SqlitePool,
    export_read: tokio::sync::Mutex<()>,
}

/// What a row is to an open walk: the walk listed it, or a write outside the walk changed it
/// while the walk ran, so the walk leaves it alone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Mark {
    Walk,
    Live,
}

/// The rows each open walk saw, by family and key.
#[derive(Default)]
struct Walks {
    open: HashMap<u64, (SourceKey, HashMap<(Family, i64), Mark>)>,
    last: u64,
    /// When this process last forgot each source's expired tracks.
    expired: HashMap<SourceKey, i64>,
}

/// When the Catalog image is written: at quiet after the catalog's facts change, and at close
/// after any change, so listens alone do not rewrite it.
#[derive(Default)]
struct ImageState {
    /// The facts changed since the image was written, and when they last did.
    changed: Option<Instant>,
    /// Something changed since the image was written.
    dirty: bool,
    closed: bool,
}

struct CatalogInner {
    store: Store,
    snapshot: RwLock<Snapshot>,
    /// True once the first load finished, even when it failed.
    loaded: watch::Sender<bool>,
    /// Memory is behind the Store; the next Write reloads before anything else.
    reload: AtomicBool,
    changes: broadcast::Sender<Arc<Change>>,
    walks: std::sync::Mutex<Walks>,
    /// An image matching the Store's commit counter may exist, so the next commit raises it.
    image_current: AtomicBool,
    /// Shared with the image thread, which holds no strong handle while it waits.
    image: Arc<(std::sync::Mutex<ImageState>, Condvar)>,
    views: std::sync::Mutex<crate::catalog::views::Views>,
    /// Held while totals are built, so readers that need the same ones wait for one build.
    deriving: std::sync::Mutex<()>,
    /// The saved folder mappings by source id.
    local_access: RwLock<HashMap<String, crate::LocalAccess>>,
}

/// The one public handle: the Store's connections and the resident catalog loaded from it.
#[derive(Clone)]
pub struct Catalog {
    inner: Arc<CatalogInner>,
}

/// A pooled reader with its read's progress handler. The handler stays on the connection after
/// release and turns inert when this drops, before the connection goes back to the pool.
pub(crate) struct Reader {
    live: Arc<AtomicBool>,
    connection: PoolConnection<Sqlite>,
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.live.store(false, Ordering::Release);
    }
}

impl std::ops::Deref for Reader {
    type Target = SqliteConnection;

    fn deref(&self) -> &SqliteConnection {
        &self.connection
    }
}

impl std::ops::DerefMut for Reader {
    fn deref_mut(&mut self) -> &mut SqliteConnection {
        &mut self.connection
    }
}

/// Holds the writer lock. `Catalog::writer` creates it only while the writer is open.
pub(crate) struct WriterGuard<'a>(tokio::sync::MutexGuard<'a, Option<SqliteConnection>>);

impl std::ops::Deref for WriterGuard<'_> {
    type Target = SqliteConnection;

    fn deref(&self) -> &SqliteConnection {
        self.0.as_ref().expect("the writer stays open while locked")
    }
}

impl std::ops::DerefMut for WriterGuard<'_> {
    fn deref_mut(&mut self) -> &mut SqliteConnection {
        self.0.as_mut().expect("the writer stays open while locked")
    }
}

impl Catalog {
    pub fn path(&self) -> &Path {
        &self.inner.store.path
    }

    /// The identity of a source the Catalog holds.
    pub fn source_id(&self, source: SourceKey) -> Option<crate::SourceId> {
        self.snapshot()
            .source(source)
            .map(|source| crate::SourceId::new(source.object.clone()))
    }

    /// The key of a source the Catalog holds.
    pub fn source_key(&self, source_id: &crate::SourceId) -> Option<SourceKey> {
        self.snapshot()
            .sources
            .values()
            .find(|source| source.object == source_id.as_str())
            .map(|source| source.key)
    }

    /// The server time of the source's last completed listing or catch-up.
    pub fn synced_at(&self, source: SourceKey) -> Option<i64> {
        self.snapshot()
            .source(source)
            .and_then(|source| source.synced_at)
    }

    /// How many of the source's tracks are listed.
    pub fn indexed_tracks(&self, source: SourceKey) -> usize {
        use crate::catalog::snapshot::Packed;
        let snapshot = self.snapshot();
        let Some(slot) = snapshot.source_slot(source) else {
            return 0;
        };
        snapshot
            .tracks
            .iter()
            .filter(|(_, record)| {
                record.source() == slot && record.flags() & crate::catalog::snapshot::INDEXED != 0
            })
            .count()
    }

    /// The sources of the tracks, albums and artists these state changes name.
    pub fn state_sources(&self, items: &[crate::StateChange]) -> BTreeSet<SourceKey> {
        use crate::catalog::snapshot::Packed;
        let snapshot = self.snapshot();
        items
            .iter()
            .filter_map(|item| {
                let slot = if let Some(track) = item.track() {
                    snapshot.tracks.get(track.raw())?.source()
                } else if let Some(album) = item.album() {
                    snapshot.albums.get(album.raw())?.source
                } else {
                    snapshot.artists.get(item.artist()?.raw())?.source
                };
                snapshot.source_key(slot)
            })
            .collect()
    }

    /// Every committed Write, in commit order. A receiver that falls behind gets a Lagged
    /// error, which means everything may have changed.
    pub fn changes(&self) -> broadcast::Receiver<Arc<Change>> {
        self.inner.changes.subscribe()
    }

    /// Runs `pass` once the Catalog loaded: a full pass (`None`) first and after the feed
    /// lagged, then one per batch of Changes heard since the previous pass. It ends when
    /// `pass` answers false or the feed closes, and does not keep the Catalog open.
    pub fn follow<Pass, Done>(&self, mut pass: Pass) -> impl Future<Output = ()> + Send + 'static
    where
        Pass: FnMut(Option<Vec<Arc<Change>>>) -> Done + Send + 'static,
        Done: Future<Output = bool> + Send,
    {
        use broadcast::error::{RecvError, TryRecvError};
        let mut changes = self.changes();
        let catalog = self.clone();
        async move {
            catalog.loaded().await;
            drop(catalog);
            let (mut full, mut heard) = (true, Vec::new());
            loop {
                loop {
                    match changes.try_recv() {
                        Ok(change) => heard.push(change),
                        Err(TryRecvError::Lagged(_)) => full = true,
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Closed) => return,
                    }
                }
                if full || !heard.is_empty() {
                    let heard = std::mem::take(&mut heard);
                    if !pass((!std::mem::take(&mut full)).then_some(heard)).await {
                        return;
                    }
                    continue;
                }
                match changes.recv().await {
                    Ok(change) => heard.push(change),
                    Err(RecvError::Lagged(_)) => full = true,
                    Err(RecvError::Closed) => return,
                }
            }
        }
    }

    /// Starts a walk of a source: a listing whose completion hides what it did not see.
    pub(crate) fn open_walk(&self, source: SourceKey) -> u64 {
        let mut walks = self.walks();
        walks.last += 1;
        let walk = walks.last;
        walks.open.insert(walk, (source, HashMap::new()));
        walk
    }

    pub(crate) fn walk_open(&self, walk: u64) -> bool {
        self.walks().open.contains_key(&walk)
    }

    pub(crate) fn close_walk(&self, walk: u64) {
        self.walks().open.remove(&walk);
    }

    /// Whether a completed listing forgets expired tracks now: once a day per source, and on
    /// the first completion after launch.
    pub(crate) fn expiry_due(&self, source: SourceKey, now: i64) -> bool {
        let mut walks = self.walks();
        if walks
            .expired
            .get(&source)
            .is_some_and(|at| now - at < 24 * 60 * 60)
        {
            return false;
        }
        walks.expired.insert(source, now);
        true
    }

    fn walks(&self) -> std::sync::MutexGuard<'_, Walks> {
        self.inner
            .walks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn mark(&self, walk: u64, family: Family, key: i64) -> Option<Mark> {
        self.walks()
            .open
            .get(&walk)
            .and_then(|(_, marks)| marks.get(&(family, key)).copied())
    }

    /// Stops every open walk of a source, so none of them completes. A source whose
    /// configuration changed or that is forgotten starts over with its next walk.
    pub(crate) fn close_walks(&self, source: SourceKey) {
        self.walks().open.retain(|_, (owner, _)| *owner != source);
    }

    /// The keys a walk did not see. A walk that is no longer open saw everything, so its
    /// completion hides nothing.
    pub(crate) fn unseen(&self, walk: u64, family: Family, mut keys: Vec<i64>) -> Vec<i64> {
        match self.walks().open.get(&walk) {
            Some((_, marks)) => keys.retain(|key| !marks.contains_key(&(family, *key))),
            None => keys.clear(),
        }
        keys
    }

    /// Rows a Write saw. A walk marks them as its own. Every other open walk of their source
    /// marks them live, since a write outside it is newer than what it read.
    pub(crate) fn record_seen(&self, walk: Option<u64>, seen: Vec<(Family, i64, SourceKey)>) {
        if seen.is_empty() {
            return;
        }
        let mut walks = self.walks();
        for (open, (owner, marks)) in &mut walks.open {
            for (family, key, source) in &seen {
                if source != owner {
                    continue;
                }
                if walk == Some(*open) {
                    marks.entry((*family, *key)).or_insert(Mark::Walk);
                } else {
                    marks.insert((*family, *key), Mark::Live);
                }
            }
        }
    }
}

impl Catalog {
    pub(crate) fn publish(&self, change: Arc<Change>) {
        let _ = self.inner.changes.send(change);
    }

    /// The revision of a Change that leaves the Catalog's records as they are.
    pub(crate) fn next_revision(&self) -> u64 {
        let mut snapshot = self.snapshot_mut();
        snapshot.revision += 1;
        snapshot.revision
    }

    /// A Write committed, so the image is written again at close, or once the Catalog is quiet
    /// when it changed the facts.
    pub(crate) fn committed(&self, facts: bool) {
        self.inner.changed(facts);
    }

    pub(crate) fn views_lock(&self) -> &std::sync::Mutex<crate::catalog::views::Views> {
        &self.inner.views
    }

    pub(crate) fn deriving(&self) -> std::sync::MutexGuard<'_, ()> {
        self.inner
            .deriving
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn snapshot(&self) -> RwLockReadGuard<'_, Snapshot> {
        self.inner
            .snapshot
            .read()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn snapshot_mut(&self) -> RwLockWriteGuard<'_, Snapshot> {
        self.inner
            .snapshot
            .write()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The revision of the last committed Change.
    pub fn revision(&self) -> u64 {
        self.snapshot().revision
    }

    /// Waits until the first load finished.
    pub async fn loaded(&self) {
        let mut loaded = self.inner.loaded.subscribe();
        let _ = loaded.wait_for(|loaded| *loaded).await;
    }

    pub(crate) fn reload_pending(&self) -> bool {
        self.inner.reload.load(Ordering::Acquire)
    }

    pub(crate) fn require_reload(&self) {
        self.inner.reload.store(true, Ordering::Release);
        self.inner.image_current.store(true, Ordering::Release);
    }

    /// Raises the Store's commit counter in the open transaction when an image of the current
    /// state may exist.
    pub(crate) async fn count_commit(&self, writer: &mut SqliteConnection) -> LibraryResult<()> {
        if self.inner.image_current.load(Ordering::Acquire) {
            image::count_commit(writer).await?;
            self.inner.image_current.store(false, Ordering::Release);
        }
        Ok(())
    }

    /// A commit that may have landed without its counter is followed by one that raises it.
    pub(crate) fn image_may_be_current(&self) {
        self.inner.image_current.store(true, Ordering::Release);
    }

    /// Builds a fresh Snapshot while the old one keeps serving readers, then swaps them.
    /// Holding the writer means no commit lands between the read and the swap.
    pub(crate) async fn reload_under(&self, _writer: &mut WriterGuard<'_>) -> LibraryResult<u64> {
        let snapshot = load(&self.inner.store.path).await?;
        let (revision, previous) = self.snapshot_mut().succeed(snapshot);
        drop(previous);
        process_memory::request_reclaim();
        self.inner.reload.store(false, Ordering::Release);
        Ok(revision)
    }
}

impl Catalog {
    /// The saved folder mappings by source id; reads resolve tracks through them.
    pub fn set_local_access(&self, rules: HashMap<String, crate::LocalAccess>) {
        *self
            .inner
            .local_access
            .write()
            .unwrap_or_else(PoisonError::into_inner) = rules;
    }

    /// Where a source's track would be under the source's mapping.
    pub(crate) fn mapped_file(
        &self,
        source_id: &str,
        source_path: &str,
        size: Option<u64>,
    ) -> Option<crate::MappedFile> {
        self.inner
            .local_access
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(source_id)?
            .mapped(source_path, size)
    }

    /// Read when artwork falls back to a track picture; nothing stored depends on it. Clients
    /// redraw every cover when it changes.
    pub fn set_distinct_track_covers(&self, enabled: bool) {
        self.set_display(|snapshot| &mut snapshot.distinct_track_covers, enabled);
    }

    pub fn distinct_track_covers(&self) -> bool {
        self.snapshot().distinct_track_covers
    }

    /// Whether albums without a picture look their cover up, from the external metadata
    /// setting. Nothing stored depends on it; clients redraw every cover when it changes.
    pub fn set_cover_lookup(&self, enabled: bool) {
        self.set_display(|snapshot| &mut snapshot.cover_lookup, enabled);
    }

    /// Whether artists and albums sort by their sort tags. Nothing stored depends on it; the
    /// orders built since the last change are dropped and clients reopen their pages.
    pub fn set_use_sort_tags(&self, enabled: bool) {
        self.set_display(|snapshot| &mut snapshot.sort_tags, enabled);
    }

    /// Sets a flag the Snapshot shows rows by. A load copies it from the Snapshot it replaces.
    fn set_display(&self, flag: fn(&mut Snapshot) -> &mut bool, enabled: bool) {
        let mut snapshot = self.snapshot_mut();
        if std::mem::replace(flag(&mut snapshot), enabled) == enabled {
            return;
        }
        let revision = snapshot.raise([true; 5]);
        drop(snapshot);
        if *self.inner.loaded.borrow() {
            self.publish(Arc::new(Change {
                revision,
                scopes: vec![crate::Scope::Display],
            }));
        }
    }

    /// Opens the installed Store. Without one, it imports the Store a release wrote at
    /// `released` and leaves that file as it was, so the release still opens it after a
    /// downgrade. A Store imported from a released one gets its albums resolved once, since the
    /// import copies only the facts albums are resolved from.
    pub async fn open_installation(
        path: &Path,
        released: &Path,
        catalog: &Path,
    ) -> LibraryResult<Self> {
        // A Store without a version is new, or holds an import that stopped and rolled back.
        let current = path.exists()
            && Self::version(path)
                .await
                .map_or(true, |version| version != 0);
        let mut imported = false;
        if !current && released.exists() {
            let mut result = Self::import_released(released, path, catalog).await;
            if result
                .as_ref()
                .is_err_and(|error| !error.is_store_unavailable())
                && catalog.exists()
            {
                // A damaged catalog file fails the import; favorites, playlists, history and the
                // queue import without it.
                tracing::warn!("could not import the released catalog file; importing without it");
                move_store(catalog, None)?;
                result = Self::import_released(released, path, catalog).await;
            }
            match result {
                Ok(()) => imported = true,
                Err(error) if error.is_store_unavailable() => return Err(error),
                Err(error) => {
                    tracing::warn!(%error, "could not import the released Store; starting a new one");
                }
            }
        }
        // The release's catalog file is a cache it rebuilds after a downgrade.
        let _ = move_store(catalog, None);
        let opened = async {
            if path.exists() {
                Self::check_version(path).await?;
            }
            Self::open(path).await
        }
        .await;
        let (opened, imported) = match opened {
            Err(error) if path.exists() && !error.is_store_unavailable() => {
                tracing::warn!(%error, "could not open the Store; keeping its compatible columns");
                image::remove(path);
                if let Err(error) = crate::migration::current::recover_columns(path).await {
                    if error.is_store_unavailable() {
                        return Err(error);
                    }
                    tracing::warn!(%error, "could not read the Store; keeping it aside");
                    move_store(path, Some(&path.with_extension("unreadable")))?;
                }
                (Self::open(path).await?, true)
            }
            opened => (opened?, imported),
        };
        if imported {
            crate::catalog::albums::resolve_all(&opened).await?;
        }
        Ok(opened)
    }

    async fn version(path: &Path) -> LibraryResult<i64> {
        let mut reader =
            SqliteConnection::connect_with(&base_options(path).read_only(true)).await?;
        let version = schema::pragma(&mut reader, "user_version").await;
        reader.close().await?;
        version
    }

    async fn import_released(released: &Path, path: &Path, catalog: &Path) -> LibraryResult<()> {
        match Self::version(released).await? {
            44..=46 => {
                image::remove(path);
                crate::migration::import_current(released, path, catalog).await
            }
            version => Err(LibraryError::Migration(format!(
                "unsupported released library schema {version}"
            ))),
        }
    }

    /// Fails for a Store another version wrote, which is then recovered.
    async fn check_version(path: &Path) -> LibraryResult<()> {
        match Self::version(path).await? {
            0 | schema::STORE_VERSION => Ok(()),
            version => Err(LibraryError::Migration(format!(
                "unsupported library schema {version}"
            ))),
        }
    }

    pub async fn close(self) -> LibraryResult<()> {
        let store = &self.inner.store;
        {
            let (state, wake) = &*self.inner.image;
            state.lock().unwrap_or_else(PoisonError::into_inner).closed = true;
            wake.notify_all();
        }
        tokio::join!(
            store.export_readers.close(),
            store.playable_readers.close(),
            store.readers.close(),
            self.loaded()
        );
        // Changes since the image, listens among them, so the next start reads the image.
        let catalog = Self {
            inner: Arc::clone(&self.inner),
        };
        let _ = tokio::task::spawn_blocking(move || catalog.write_image()).await;
        if let Some(writer) = store.writer.lock().await.take() {
            writer.close().await?;
        }
        Ok(())
    }

    pub async fn open(path: impl AsRef<Path>) -> LibraryResult<Self> {
        let path = path.as_ref();
        let store = Store::open(path).await?;
        let inner = Arc::new(CatalogInner {
            store,
            snapshot: RwLock::new(Snapshot::default()),
            loaded: watch::channel(false).0,
            reload: AtomicBool::new(false),
            changes: broadcast::channel(CHANGE_BUFFER).0,
            walks: std::sync::Mutex::new(Walks::default()),
            // An image written by an earlier run may match the counter.
            image_current: AtomicBool::new(true),
            image: Arc::new((std::sync::Mutex::new(ImageState::default()), Condvar::new())),
            views: Default::default(),
            deriving: std::sync::Mutex::new(()),
            local_access: RwLock::new(HashMap::new()),
        });
        // The load reads on connections and threads of its own, beside the clients' first
        // frames. Writes wait for it, so nothing it reads changes under it.
        let weak = Arc::downgrade(&inner);
        let store = path.to_owned();
        std::thread::Builder::new()
            .name("rufin-catalog-load".into())
            .spawn(move || {
                let started = Instant::now();
                let loaded = start(&store);
                if let Some(inner) = weak.upgrade() {
                    inner.finish_load(loaded, started.elapsed());
                }
            })?;
        let weak = Arc::downgrade(&inner);
        let image = Arc::clone(&inner.image);
        std::thread::Builder::new()
            .name("rufin-catalog-image".into())
            .spawn(move || write_images(&weak, &image))?;
        Ok(Self { inner })
    }

    pub(crate) async fn writer(&self) -> LibraryResult<WriterGuard<'_>> {
        let writer = self.inner.store.writer.lock().await;
        if writer.is_none() {
            return Err(LibraryError::WriterUnavailable);
        }
        Ok(WriterGuard(writer))
    }

    /// A pooled reader whose statements stop once `cancellation` fires.
    pub(crate) async fn acquire_cancellable(
        &self,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Reader> {
        let mut connection = tokio::select! {
            connection = self.inner.store.readers.acquire() => connection?,
            () = cancellation.cancelled() => return Err(LibraryError::ReadCancelled),
        };
        let live = Arc::new(AtomicBool::new(true));
        let (read, cancellation) = (Arc::clone(&live), cancellation.clone());
        connection
            .lock_handle()
            .await?
            .set_progress_handler(PROGRESS_INTERVAL, move || {
                !(read.load(Ordering::Acquire) && cancellation.is_cancelled())
            });
        Ok(Reader { live, connection })
    }

    /// `acquire_cancellable` once the first load finished, for reads that need the Catalog.
    pub(crate) async fn acquire_loaded(
        &self,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Reader> {
        tokio::select! {
            () = self.loaded() => {}
            () = cancellation.cancelled() => return Err(LibraryError::ReadCancelled),
        }
        self.acquire_cancellable(cancellation).await
    }

    /// A pooled reader for a read nothing cancels. A handler a cancellable read left on the
    /// connection is inert, so this takes no handle lock.
    pub(crate) async fn acquire_reader(&self) -> LibraryResult<PoolConnection<Sqlite>> {
        Ok(self.inner.store.readers.acquire().await?)
    }

    pub(crate) async fn acquire_playable(&self) -> LibraryResult<PoolConnection<Sqlite>> {
        Ok(self.inner.store.playable_readers.acquire().await?)
    }

    pub(crate) async fn acquire_export(
        &self,
    ) -> LibraryResult<(tokio::sync::MutexGuard<'_, ()>, PoolConnection<Sqlite>)> {
        let store = &self.inner.store;
        let permit = store.export_read.lock().await;
        Ok((permit, store.export_readers.acquire().await?))
    }

    #[cfg(test)]
    pub(crate) fn reader_pool(&self) -> &SqlitePool {
        &self.inner.store.readers
    }
}

impl CatalogInner {
    fn changed(&self, facts: bool) {
        let (state, wake) = &*self.image;
        let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
        state.dirty = true;
        if facts {
            state.changed = Some(Instant::now());
            wake.notify_one();
        }
    }

    fn finish_load(&self, loaded: LibraryResult<(Snapshot, bool)>, elapsed: Duration) {
        let revision = match loaded {
            Ok((snapshot, from_image)) => {
                tracing::debug!(
                    tracks = snapshot.track_count(),
                    bytes = snapshot.bytes(),
                    elapsed_ms = elapsed.as_millis() as u64,
                    from_image,
                    "loaded the Catalog"
                );
                let (revision, previous) = self
                    .snapshot
                    .write()
                    .unwrap_or_else(PoisonError::into_inner)
                    .succeed(snapshot);
                drop(previous);
                process_memory::request_reclaim();
                if !from_image {
                    self.changed(true);
                }
                revision
            }
            Err(error) => {
                tracing::warn!(%error, "could not load the Catalog; the next write reloads it");
                self.reload.store(true, Ordering::Release);
                self.snapshot
                    .read()
                    .unwrap_or_else(PoisonError::into_inner)
                    .revision
            }
        };
        // Rows read by key before the load, such as now playing, showed no state or covers.
        // The Reload goes out first, so a Write that waited for the load publishes after it.
        let _ = self.changes.send(Arc::new(Change {
            revision,
            scopes: vec![crate::Scope::Reload],
        }));
        self.loaded.send_replace(true);
    }
}

impl Store {
    async fn open(path: &Path) -> LibraryResult<Self> {
        let mut writer = open_writer(path).await?;
        let initialized = async {
            schema::initialize_durable(&mut writer).await?;
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
        let readers = open_readers(path, MIN_READERS, MAX_READERS, ROW_BUFFER).await?;
        let export_readers = open_readers(path, 0, 1, 1).await?;
        let playable_readers = open_readers(path, 0, 1, ROW_BUFFER).await?;
        Ok(Self {
            path: path.to_owned(),
            writer: Arc::new(Mutex::new(Some(writer))),
            readers,
            playable_readers,
            export_readers,
            export_read: tokio::sync::Mutex::new(()),
        })
    }
}

/// The first Catalog: the image when it matches the Store, else a load. Returns whether it came
/// from the image.
fn start(path: &Path) -> LibraryResult<(Snapshot, bool)> {
    let stamp = futures_executor::block_on(async {
        let mut connection = SqliteConnection::connect_with(&reader_options(path)).await?;
        let stamp = image::stamp(&mut connection).await;
        connection.close().await?;
        stamp
    })?;
    if let Some(snapshot) = stamp.as_ref().and_then(|stamp| image::read(path, stamp)) {
        return Ok((snapshot, true));
    }
    Ok((crate::catalog::load::snapshot(path)?, false))
}

/// Loads on a thread of its own, so no runtime worker waits for it.
async fn load(path: &Path) -> LibraryResult<Snapshot> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let path = path.to_owned();
    std::thread::Builder::new()
        .name("rufin-catalog-load".into())
        .spawn(move || {
            let _ = sender.send(crate::catalog::load::snapshot(&path));
        })?;
    receiver
        .await
        .map_err(|_| LibraryError::InvalidStore("the Catalog load stopped".into()))?
}

/// Writes the Catalog image once the Catalog has been quiet for a while after a change.
fn write_images(
    weak: &std::sync::Weak<CatalogInner>,
    image: &(std::sync::Mutex<ImageState>, Condvar),
) {
    let (state, wake) = image;
    let mut guard = state.lock().unwrap_or_else(PoisonError::into_inner);
    loop {
        if guard.closed {
            return;
        }
        let Some(changed) = guard.changed else {
            guard = wake.wait(guard).unwrap_or_else(PoisonError::into_inner);
            continue;
        };
        let due = changed + IMAGE_QUIET;
        let now = Instant::now();
        if now < due {
            guard = wake
                .wait_timeout(guard, due - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
            continue;
        }
        guard.changed = None;
        // A Write that commits locks this state while it holds the writer.
        drop(guard);
        let Some(inner) = weak.upgrade() else {
            return;
        };
        Catalog { inner }.write_image();
        guard = state.lock().unwrap_or_else(PoisonError::into_inner);
    }
}

impl Catalog {
    /// Writes the image of the current Catalog. The writer is held while it is stamped and the
    /// Catalog is taken for reading, so the stamp is the Catalog's, and the next commit raises
    /// the counter; the encode runs once the writer is released. A Catalog with many removed
    /// slots or replaced strings is loaded again first, so the image holds none of them.
    fn write_image(&self) {
        let writer = futures_executor::block_on(self.inner.store.writer.lock());
        let (state, _) = &*self.inner.image;
        let dirty = std::mem::take(&mut state.lock().unwrap_or_else(PoisonError::into_inner).dirty);
        // A Write whose apply failed left memory behind the Store.
        if writer.is_none() || self.reload_pending() || !dirty {
            return;
        }
        let mut writer = WriterGuard(writer);
        let sealed = futures_executor::block_on(async {
            let compact = {
                let snapshot = self.snapshot();
                snapshot.removed_slots() * REMOVED_SLOTS > snapshot.record_slots()
                    || snapshot.strings.wasteful()
            };
            if compact {
                self.reload_under(&mut writer).await?;
            }
            image::seal(&mut writer).await
        });
        if sealed.is_ok() {
            self.inner.image_current.store(true, Ordering::Release);
        }
        let snapshot = self.snapshot();
        drop(writer);
        match sealed {
            Ok(stamp) => {
                if let Err(error) = image::write(&self.inner.store.path, &snapshot, &stamp) {
                    tracing::debug!(%error, "could not write the Catalog image");
                }
            }
            Err(error) => tracing::debug!(%error, "could not stamp the Catalog image"),
        }
    }
}

impl Drop for CatalogInner {
    fn drop(&mut self) {
        let (state, wake) = &*self.image;
        state.lock().unwrap_or_else(PoisonError::into_inner).closed = true;
        wake.notify_all();
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

pub(crate) fn reader_options(path: &Path) -> SqliteConnectOptions {
    base_options(path)
        .pragma("temp_store", "FILE")
        .pragma("mmap_size", "0")
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
        .row_buffer_size(ROW_BUFFER)
}

async fn open_readers(
    path: &Path,
    minimum: u32,
    maximum: u32,
    row_buffer: usize,
) -> LibraryResult<SqlitePool> {
    let options = SqlitePoolOptions::new()
        .min_connections(minimum)
        .max_connections(maximum)
        .acquire_timeout(ACQUIRE_TIMEOUT)
        .idle_timeout(READER_IDLE_TIMEOUT)
        .max_lifetime(None)
        .test_before_acquire(true)
        .after_connect(move |connection, _| {
            Box::pin(async move {
                crate::lyrics_tokenizer::register(connection)
                    .await
                    .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
                sqlx::query("PRAGMA query_only=ON")
                    .execute(connection)
                    .await?;
                Ok(())
            })
        });
    if minimum == 0 {
        Ok(options.connect_lazy_with(reader_options(path).row_buffer_size(row_buffer)))
    } else {
        Ok(options
            .connect_with(reader_options(path).row_buffer_size(row_buffer))
            .await?)
    }
}

/// Removes a Store with its journal files, or moves them to `aside`.
pub(crate) fn move_store(path: &Path, aside: Option<&Path>) -> std::io::Result<()> {
    for suffix in ["", "-wal", "-shm"] {
        let file = |path: &Path| {
            let mut file = path.as_os_str().to_os_string();
            file.push(suffix);
            file
        };
        let moved = match aside {
            Some(aside) => std::fs::rename(file(path), file(aside)),
            None => std::fs::remove_file(file(path)),
        };
        match moved {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Row;
    use tokio::time::{Instant, sleep, timeout};

    async fn catalog() -> (tempfile::TempDir, Catalog) {
        let directory = tempfile::tempdir().expect("temporary Store directory");
        let catalog = Catalog::open(directory.path().join("library.sqlite3"))
            .await
            .expect("open SQLx Library");
        // The timed steps below measure the Store, not the first load.
        catalog.loaded().await;
        (directory, catalog)
    }

    /// A stale image would show an older library, so a commit after an image is written must
    /// keep it from being read.
    #[tokio::test(flavor = "multi_thread")]
    async fn image_is_read_only_while_the_store_is_unchanged() {
        let (_directory, catalog) = catalog().await;
        let source = crate::SourceId::new("source");
        catalog
            .reconcile_source(&source, "local", "Source")
            .await
            .unwrap();
        let writing = catalog.clone();
        tokio::task::spawn_blocking(move || writing.write_image())
            .await
            .unwrap();
        let path = catalog.path().to_owned();
        let read = path.clone();
        let (snapshot, from_image) = tokio::task::spawn_blocking(move || start(&read))
            .await
            .unwrap()
            .unwrap();
        assert!(from_image);
        assert_eq!(snapshot.export(), catalog.snapshot().export());
        catalog
            .reconcile_source(&source, "local", "Renamed")
            .await
            .unwrap();
        let (_, from_image) = tokio::task::spawn_blocking(move || start(&path))
            .await
            .unwrap()
            .unwrap();
        assert!(!from_image);
    }

    #[tokio::test]
    async fn close_waits_for_exports_and_closes_both_reader_pools() {
        use futures_util::FutureExt;

        let (_directory, catalog) = catalog().await;
        let export = catalog.acquire_export().await.unwrap();
        let mut closing = Box::pin(catalog.clone().close());
        assert!(closing.as_mut().now_or_never().is_none());
        assert!(catalog.inner.store.readers.is_closed());
        assert!(catalog.inner.store.export_readers.is_closed());
        drop(export);
        timeout(Duration::from_secs(1), closing)
            .await
            .unwrap()
            .unwrap();
        assert!(catalog.acquire_export().await.is_err());
    }

    #[tokio::test]
    async fn fixed_writer_and_steady_reader_have_explicit_limits() {
        let (_directory, catalog) = catalog().await;
        assert_eq!(catalog.reader_pool().size(), MIN_READERS);
        assert_eq!(
            catalog.reader_pool().options().get_min_connections(),
            MIN_READERS
        );
        assert_eq!(
            catalog.reader_pool().options().get_max_connections(),
            MAX_READERS
        );
        assert_eq!(
            catalog.reader_pool().options().get_acquire_timeout(),
            ACQUIRE_TIMEOUT
        );
        assert_eq!(
            catalog.reader_pool().options().get_idle_timeout(),
            Some(READER_IDLE_TIMEOUT)
        );

        let cancellation = ReadCancellation::new();
        let mut reader = catalog
            .acquire_cancellable(&cancellation)
            .await
            .expect("acquire a reader");
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

        let mut writer = catalog.writer().await.expect("acquire fixed writer");
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
    async fn cancelled_read_returns_a_usable_connection() {
        let (_directory, catalog) = catalog().await;
        let cancellation = ReadCancellation::new();
        let task_catalog = catalog.clone();
        let task_cancellation = cancellation.clone();
        let read = tokio::spawn(async move {
            let mut connection = task_catalog.acquire_cancellable(&task_cancellation).await?;
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

        // The cancelled read's handler is still on the connection, inert, so a read long enough
        // to reach it runs to the end.
        let mut connection = catalog
            .acquire_reader()
            .await
            .expect("reacquire the reader");
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "WITH RECURSIVE count(value) AS (
                     VALUES(0) UNION ALL SELECT value + 1 FROM count WHERE value < 99999
                 ) SELECT count(*) FROM count",
            )
            .fetch_one(&mut *connection)
            .await
            .expect("reader remains usable"),
            100_000
        );
    }

    #[tokio::test]
    async fn dynamic_reader_skips_journal_mode_and_is_idle_reapable() {
        let (directory, catalog) = catalog().await;
        let cancellation = ReadCancellation::new();
        let general = catalog
            .acquire_cancellable(&cancellation)
            .await
            .expect("hold steady reader");
        let mut writer = catalog.writer().await.expect("acquire writer");
        sqlx::raw_sql("BEGIN IMMEDIATE")
            .execute(&mut *writer)
            .await
            .expect("hold writer transaction");
        let mut optional = catalog
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
            .execute(&mut *writer)
            .await
            .expect("release writer transaction");
        drop(writer);
        drop(optional);
        drop(general);
        assert_eq!(
            catalog.reader_pool().options().get_idle_timeout(),
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
        let (_directory, catalog) = catalog().await;
        {
            let mut writer = catalog.writer().await.expect("acquire writer");
            sqlx::query("INSERT INTO sources(object_id,display_name) VALUES ('source','Source')")
                .execute(&mut *writer)
                .await
                .expect("insert idempotent read row");
        }
        let mut reader = catalog
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
        while catalog.reader_pool().size() < MIN_READERS && Instant::now() < deadline {
            sleep(Duration::from_millis(20)).await;
        }
        let mut replacement = catalog
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
