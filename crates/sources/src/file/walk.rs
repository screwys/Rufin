//! One walker for file sources. It merge-joins each name-sorted directory listing with the
//! source's stored rows, read in walk order, and stages only new, changed and vanished files.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{StreamExt, stream};
use library::{FileBatch, LocalFileKind, LocalFileState, LocalFileWrite, StoredFile, walk_key};

use crate::file::media::{self, MediaRead, Worker};
use crate::{SourceError, SourceReadProgress, SourceReadStage, SourceResult};

/// Stored rows read per SELECT: one SELECT covers most libraries, and memory stays bounded.
const PAGE: usize = 8192;
/// Staged files parsed and written per Write.
const BATCH: usize = 128;
/// Identities kept per walk to follow renamed files.
const IDENTITIES: usize = 65_536;

/// One listed directory entry, or a stat of one path.
#[derive(Clone, Debug, Default)]
pub(crate) struct Entry {
    /// The name the walk orders by and stores in `relative_path`.
    pub name: String,
    /// The segment the source locates the entry by; the name, except for documents.
    pub native: String,
    pub directory: bool,
    pub size: Option<i64>,
    pub mtime_ns: i64,
    pub device_id: Option<i64>,
    pub inode: Option<i64>,
    pub native_id: Option<String>,
    /// WebDAV ETag, SMB change time, document revision.
    pub revision: Option<String>,
}

/// A root a full walk lists, from the given (relative, native) starts.
pub(crate) struct Root {
    pub root: String,
    pub starts: Vec<(String, String)>,
}

pub(crate) enum CueRead {
    Accepted {
        inputs: Vec<String>,
        tracks: Vec<library::TrackObservation>,
    },
    Rejected,
    Unreadable,
}

pub(crate) trait FileSource: Sync {
    fn roots(&self) -> SourceResult<Vec<Root>>;
    fn excluded(&self, root: &str, relative: &str) -> bool;
    /// The row of an entry: its location, kind and parser version.
    fn row(
        &self,
        root: &str,
        relative: &str,
        native: &str,
        entry: &Entry,
    ) -> SourceResult<LocalFileWrite>;
    /// Where a path sits: its root, relative path and native path.
    fn place(&self, path: &str) -> Option<(String, String, String)>;
    fn list(
        &self,
        root: &str,
        native: &str,
    ) -> impl Future<Output = SourceResult<Vec<Entry>>> + Send;
    fn stat(&self, root: &str, native: &str) -> impl Future<Output = SourceResult<Entry>> + Send;
    /// `siblings` are the names in the file's directory, when the walk listed it.
    fn read_media(
        &self,
        file: &mut LocalFileWrite,
        native: &str,
        worker: Arc<Mutex<Worker>>,
        siblings: Option<&BTreeSet<String>>,
    ) -> impl Future<Output = SourceResult<MediaRead>> + Send;
    fn read_cue(
        &self,
        file: &mut LocalFileWrite,
        native: &str,
        worker: Arc<Mutex<Worker>>,
        siblings: Option<&BTreeSet<String>>,
    ) -> impl Future<Output = SourceResult<CueRead>> + Send;
    /// Who reads the folder artwork of the walked files.
    fn artwork(&self) -> crate::file::artwork::ArtworkFiles<'_>;
    /// Nextcloud: a directory whose recursive ETag is unchanged needs no listing below it.
    fn unchanged_subtree(&self, _stored: &LocalFileWrite, _listed: &LocalFileWrite) -> bool {
        false
    }
}

#[derive(Clone, Copy)]
pub(crate) enum WalkScope<'a> {
    /// Every root. `resync` also reads rejected files again (manual Resync Library).
    All { resync: bool },
    /// Live changes and imports: these paths, a directory with its subtree.
    Paths {
        paths: &'a [String],
        rename: Option<&'a (String, String)>,
    },
}

/// Walks `files` and writes what changed, then reads the folder artwork of the directories
/// it changed. `configured` are every root of the source, so rows of roots another walker of
/// the same source lists stay untouched.
pub(crate) async fn walk<F: FileSource>(
    files: &F,
    observation: &mut library::Observation<'_>,
    configured: &[String],
    scope: WalkScope<'_>,
    artwork: &ArtworkState,
    progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> SourceResult<()> {
    let roots = files.roots()?;
    let workers = (0..std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .clamp(1, 4))
        .map(|_| Arc::new(Mutex::new(Worker::default())))
        .collect();
    let mut walker = Walker {
        files,
        observation,
        walked: roots.iter().map(|root| root.root.clone()).collect(),
        configured,
        resync: matches!(scope, WalkScope::All { resync: true }),
        stored: VecDeque::new(),
        cursor: None,
        inclusive: false,
        exhausted: false,
        page: PAGE,
        staged: Vec::new(),
        batch: FileBatch::default(),
        forced: BTreeSet::new(),
        directories: BTreeSet::new(),
        fresh: HashMap::new(),
        gone: HashMap::new(),
        lyrics: BTreeSet::new(),
        imports: Vec::new(),
        failure: None,
        accepted: 0,
        reported: None,
        workers,
        progress,
        cancelled,
    };
    let walked = match scope {
        WalkScope::All { .. } => walker.all(roots).await,
        WalkScope::Paths { paths, rename } => walker.paths(paths, rename).await,
    };
    let walked = match walked {
        Ok(()) => walker.finish().await,
        failed => failed,
    };
    let (directories, resync, failure) = (walker.directories, walker.resync, walker.failure);
    // What was written gets its folder artwork whether or not the walk listed everything; a
    // cancelled walk leaves its directories to the next pass.
    if matches!(walked, Err(SourceError::Cancelled)) {
        artwork.defer(directories);
        return walked;
    }
    let pass = artwork
        .pass(files.artwork(), observation, directories, resync, cancelled)
        .await;
    walked?;
    pass?;
    // A root or directory the walk could not list keeps its rows, and the walk fails, so it
    // completes nothing and expires nothing.
    match failure {
        Some(failure) => Err(SourceError::Other(failure)),
        None => Ok(()),
    }
}

#[derive(Clone, Eq, Hash, PartialEq)]
enum Identity {
    Inode(i64, i64),
    Native(String),
}

fn identity(file: &LocalFileWrite) -> Option<Identity> {
    if !parseable(file.kind) {
        return None;
    }
    match (file.device_id, file.inode, &file.native_id) {
        (Some(device), Some(inode), _) => Some(Identity::Inode(device, inode)),
        (_, _, Some(id)) => Some(Identity::Native(id.clone())),
        _ => None,
    }
}

struct Staged {
    row: LocalFileWrite,
    stored: Option<LocalFileWrite>,
    native: String,
    siblings: Option<Arc<BTreeSet<String>>>,
}

struct Walker<'w, 'o, F: FileSource> {
    files: &'w F,
    observation: &'w mut library::Observation<'o>,
    walked: Vec<String>,
    configured: &'w [String],
    resync: bool,
    stored: VecDeque<StoredFile>,
    /// Where the next page of stored rows starts.
    cursor: Option<(String, String)>,
    inclusive: bool,
    exhausted: bool,
    page: usize,
    staged: Vec<Staged>,
    batch: FileBatch,
    /// Paths that are read again even when their facts are unchanged.
    forced: BTreeSet<String>,
    directories: BTreeSet<String>,
    fresh: HashMap<Identity, String>,
    gone: HashMap<Identity, String>,
    /// Directories a changed lyrics sidecar of a live change stages again.
    lyrics: BTreeSet<(String, String, String)>,
    /// Imported files no listing held.
    imports: Vec<StoredFile>,
    /// The first root or directory the walk could not list.
    failure: Option<String>,
    accepted: usize,
    reported: Option<Instant>,
    workers: Vec<Arc<Mutex<Worker>>>,
    progress: &'w (dyn Fn(SourceReadProgress) + Send + Sync),
    cancelled: &'w (dyn Fn() -> bool + Send + Sync),
}

fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}/{name}")
    }
}

fn within(row: &StoredFile, root: &str, key: &str) -> bool {
    if row.file.root != root {
        return false;
    }
    let row = walk_key(&row.file.relative_path);
    key.is_empty()
        || row == key
        || row
            .strip_prefix(key)
            .is_some_and(|rest| rest.starts_with('\u{1}'))
}

/// The directory of a file path or location, as the artwork pass matches track paths.
fn parent_directory(path: &str) -> Option<String> {
    if let Ok(mut uri) = url::Url::parse(path)
        && uri.path_segments().is_some()
    {
        uri.set_fragment(None);
        uri.path_segments_mut().ok()?.pop_if_empty().pop().push("");
        return Some(uri.into());
    }
    let parent = std::path::Path::new(path).parent()?.to_string_lossy();
    Some(format!(
        "{}{}",
        parent.trim_end_matches(std::path::MAIN_SEPARATOR),
        std::path::MAIN_SEPARATOR
    ))
}

fn parseable(kind: LocalFileKind) -> bool {
    matches!(kind, LocalFileKind::Media | LocalFileKind::Cue)
}

impl<F: FileSource> Walker<'_, '_, F> {
    fn fail(&mut self, failure: String) {
        tracing::warn!(failure, "File source walk is incomplete");
        self.failure.get_or_insert(failure);
    }

    fn check(&self) -> SourceResult<()> {
        crate::file::check_cancelled(self.cancelled)
    }

    async fn peek(&mut self) -> SourceResult<Option<&StoredFile>> {
        if self.stored.is_empty() && !self.exhausted {
            let start = self
                .cursor
                .as_ref()
                .map(|(root, relative)| (root.as_str(), relative.as_str()));
            let rows = self
                .observation
                .file_rows(
                    &self.walked,
                    self.configured,
                    start,
                    self.inclusive,
                    self.page,
                )
                .await?;
            self.inclusive = false;
            self.exhausted = rows.len() < self.page;
            if let Some(last) = rows.last() {
                self.cursor = Some((last.file.root.clone(), last.file.relative_path.clone()));
            }
            self.stored.extend(rows);
        }
        Ok(self.stored.front())
    }

    /// Stored rows from this path on, for a walk of one subtree.
    fn seek(&mut self, root: &str, relative: &str) {
        self.stored.clear();
        self.cursor = Some((root.to_owned(), relative.to_owned()));
        self.inclusive = true;
        self.exhausted = false;
        self.page = 256;
    }

    /// The stored row at exactly this position.
    async fn take(&mut self, root: &str, key: &str) -> SourceResult<Option<StoredFile>> {
        Ok(match self.peek().await? {
            Some(row) if row.file.root == root && walk_key(&row.file.relative_path) == key => {
                self.stored.pop_front()
            }
            _ => None,
        })
    }

    /// Vanishes stored rows inside `subtree` that sort before `key`. Returns whether a lyrics
    /// sidecar was among them.
    async fn vanish_before(&mut self, root: &str, subtree: &str, key: &str) -> SourceResult<bool> {
        let mut lyrics = false;
        while let Some(row) = self.peek().await? {
            if !within(row, root, subtree) || walk_key(&row.file.relative_path).as_str() >= key {
                break;
            }
            let row = self.stored.pop_front().expect("peeked row");
            lyrics |= self.vanish(row);
            self.write_removed().await?;
        }
        Ok(lyrics)
    }

    async fn vanish_subtree(&mut self, root: &str, key: &str) -> SourceResult<bool> {
        let mut lyrics = false;
        while let Some(row) = self.peek().await? {
            if !within(row, root, key) {
                break;
            }
            let row = self.stored.pop_front().expect("peeked row");
            lyrics |= self.vanish(row);
            self.write_removed().await?;
        }
        Ok(lyrics)
    }

    /// A deleted subtree's rows go in batches, apart from the files staged around them: those
    /// wait for their directory's CUE sheets. Renames wait for the file they name.
    async fn write_removed(&mut self) -> SourceResult<()> {
        if self.batch.removed.len() < BATCH {
            return Ok(());
        }
        self.report(true);
        self.check()?;
        let renames = std::mem::take(&mut self.batch.renames);
        let batch = std::mem::replace(
            &mut self.batch,
            FileBatch {
                renames,
                ..FileBatch::default()
            },
        );
        let released = self.observation.write_files(batch).await?;
        self.forced.extend(released);
        Ok(())
    }

    /// A subtree the walk could not list keeps its rows.
    async fn keep_subtree(&mut self, root: &str, key: &str) -> SourceResult<()> {
        while let Some(row) = self.peek().await? {
            if !within(row, root, key) {
                break;
            }
            self.stored.pop_front();
        }
        Ok(())
    }

    fn vanish(&mut self, row: StoredFile) -> bool {
        // An imported file is checked by itself, wherever its folder went.
        if row.imported {
            self.imports.push(row);
            return false;
        }
        let file = row.file;
        self.directories.extend(parent_directory(&file.path));
        if let Some(id) = identity(&file) {
            if let Some(new) = self.fresh.remove(&id) {
                self.batch.renames.push((file.path.clone(), new));
            } else if self.gone.len() < IDENTITIES {
                self.gone.insert(id, file.path.clone());
            }
        }
        let lyrics = crate::file::lyrics::is_sidecar(&file.path);
        self.batch.removed.push(file);
        lyrics
    }

    fn unchanged(&mut self, stored: Option<&StoredFile>, row: &LocalFileWrite) -> bool {
        let forced = self.forced.remove(&row.path);
        let Some(stored) = stored.map(|stored| &stored.file).filter(|_| !forced) else {
            return false;
        };
        let unchanged = stored.kind == row.kind
            && stored.size_bytes == row.size_bytes
            && stored.mtime_ns == row.mtime_ns
            && stored.revision == row.revision
            && stored.parse_version == row.parse_version
            && match stored.state {
                LocalFileState::Accepted => true,
                LocalFileState::Rejected => !self.resync || !parseable(stored.kind),
                LocalFileState::Unreadable => false,
            };
        if !unchanged
            || (stored.device_id, stored.inode, &stored.native_id)
                == (row.device_id, row.inode, &row.native_id)
        {
            return unchanged;
        }
        // A provider that names the file in this place anew moves its tracks there (stage).
        if stored.path != row.path {
            return false;
        }
        // A remount that numbers the device again changes no content: the row takes the new
        // identity, which later renames are matched by, without a parse.
        self.batch.identities.push(row.clone());
        true
    }

    fn stage(
        &mut self,
        stored: Option<StoredFile>,
        mut row: LocalFileWrite,
        native: String,
        siblings: Option<Arc<BTreeSet<String>>>,
        new: bool,
    ) {
        if new
            && stored.is_none()
            && let Some(id) = identity(&row)
        {
            if let Some(old) = self.gone.remove(&id) {
                self.batch.renames.push((old, row.path.clone()));
            } else if self.fresh.len() < IDENTITIES {
                self.fresh.insert(id, row.path.clone());
            }
        }
        if let Some(stored) = &stored {
            if stored.file.kind != row.kind {
                // A file that became a directory, or the reverse, leaves its tracks behind.
                self.batch.removed.push(stored.file.clone());
            } else if stored.file.path != row.path {
                // The provider gave the file in this place a new identity, as Android does
                // once MediaStore indexes a file; its tracks and user data move to the new path.
                self.batch
                    .renames
                    .push((stored.file.path.clone(), row.path.clone()));
                self.batch.removed.push(stored.file.clone());
            }
        }
        if parseable(row.kind) {
            self.staged.push(Staged {
                row,
                stored: stored.map(|stored| stored.file),
                native,
                siblings,
            });
        } else {
            row.state = LocalFileState::Accepted;
            self.directories.extend(parent_directory(&row.path));
            self.batch.files.push(row);
        }
    }

    /// Progress, once something is read: an unchanged walk reports none.
    fn report(&mut self, force: bool) {
        if self.reported.is_none() {
            (self.progress)(SourceReadProgress {
                stage: SourceReadStage::Tracks,
                completed: 0,
                total: None,
            });
        }
        if force
            || self
                .reported
                .is_none_or(|reported| reported.elapsed() >= Duration::from_millis(100))
        {
            (self.progress)(SourceReadProgress {
                stage: SourceReadStage::Tracks,
                completed: self.accepted,
                total: None,
            });
            self.reported = Some(Instant::now());
        }
    }

    async fn all(&mut self, mut roots: Vec<Root>) -> SourceResult<()> {
        roots.sort_by(|a, b| a.root.cmp(&b.root));
        for Root { root, mut starts } in roots {
            self.unwalked_roots(Some(&root)).await?;
            starts.sort_by_key(|(relative, _)| walk_key(relative));
            starts.dedup_by(|later, earlier| {
                later.0 == earlier.0 || later.0.starts_with(&format!("{}/", earlier.0))
            });
            let entry = match self.files.stat(&root, "").await {
                Ok(entry) => entry,
                Err(SourceError::Cancelled) => return Err(SourceError::Cancelled),
                Err(error) => {
                    // An unplugged drive or an unreachable share keeps every row.
                    self.fail(format!("File source root {root} is unavailable: {error}"));
                    self.keep_subtree(&root, "").await?;
                    continue;
                }
            };
            for (relative, native) in starts {
                let key = walk_key(&relative);
                self.vanish_before(&root, "", &key).await?;
                let entry = if relative.is_empty() {
                    Ok(entry.clone())
                } else {
                    self.files.stat(&root, &native).await
                };
                match entry {
                    Ok(entry) => self.visit(&root, &relative, &native, entry, None).await?,
                    Err(SourceError::NotFound) => {
                        self.vanish_subtree(&root, &key).await?;
                    }
                    Err(SourceError::Cancelled) => return Err(SourceError::Cancelled),
                    Err(error) => {
                        self.fail(format!("Could not inspect {relative}: {error}"));
                        self.keep_subtree(&root, &key).await?;
                    }
                }
            }
            self.vanish_subtree(&root, "").await?;
            self.flush(false).await?;
        }
        self.unwalked_roots(None).await
    }

    /// Rows of roots no longer configured, before `limit`, vanish.
    async fn unwalked_roots(&mut self, limit: Option<&str>) -> SourceResult<()> {
        while let Some(row) = self.peek().await? {
            if limit.is_some_and(|limit| row.file.root.as_str() >= limit) {
                break;
            }
            let row = self.stored.pop_front().expect("peeked row");
            // An import root inside a listed root left its rows to that root's walk.
            if self
                .files
                .place(&row.file.path)
                .is_some_and(|(root, _, _)| root != row.file.root && self.walked.contains(&root))
            {
                continue;
            }
            self.vanish(row);
            self.flush(false).await?;
        }
        Ok(())
    }

    async fn paths(
        &mut self,
        paths: &[String],
        rename: Option<&(String, String)>,
    ) -> SourceResult<()> {
        let mut places = paths
            .iter()
            .filter_map(|path| self.files.place(path))
            .collect::<Vec<_>>();
        places.sort_by(|a, b| (&a.0, walk_key(&a.1)).cmp(&(&b.0, walk_key(&b.1))));
        places.dedup_by(|later, earlier| {
            later.0 == earlier.0
                && (earlier.1.is_empty()
                    || later.1 == earlier.1
                    || later.1.starts_with(&format!("{}/", earlier.1)))
        });
        // A rename the source reported moves the old track's identity and user data even
        // without a file identity to match the paths by.
        if let Some(((old_root, old, old_native), (new_root, new, new_native))) =
            rename.and_then(|(old, new)| Some((self.files.place(old)?, self.files.place(new)?)))
        {
            let old = self
                .files
                .row(&old_root, &old, &old_native, &Entry::default())?;
            let new = self
                .files
                .row(&new_root, &new, &new_native, &Entry::default())?;
            self.batch.renames.push((old.path, new.path));
        }
        let mut available = HashMap::new();
        for (root, relative, native) in places {
            self.check()?;
            // A change under a root that is gone (an unplugged drive) keeps every row.
            if !available.contains_key(&root) {
                let found = self.files.stat(&root, "").await.is_ok();
                available.insert(root.clone(), found);
            }
            if !available[&root] {
                self.fail(format!("File source root {root} is unavailable"));
                continue;
            }
            self.seek(&root, &relative);
            let key = walk_key(&relative);
            let entry = if self.files.excluded(&root, &relative) {
                Err(SourceError::NotFound)
            } else {
                self.files.stat(&root, &native).await
            };
            match entry {
                Ok(entry) => {
                    let directory = entry.directory;
                    self.visit(&root, &relative, &native, entry, None).await?;
                    if !directory && crate::file::lyrics::is_sidecar(&relative) {
                        self.lyrics_directory(&root, &relative, &native);
                    }
                }
                Err(SourceError::NotFound) => {}
                Err(SourceError::Cancelled) => return Err(SourceError::Cancelled),
                Err(error) => {
                    self.fail(format!("Could not inspect {relative}: {error}"));
                    self.keep_subtree(&root, &key).await?;
                    continue;
                }
            }
            if self.vanish_subtree(&root, &key).await? {
                self.lyrics_directory(&root, &relative, &native);
            }
        }
        self.flush(true).await
    }

    fn lyrics_directory(&mut self, root: &str, relative: &str, native: &str) {
        let parent = |path: &str| {
            path.rsplit_once('/')
                .map_or(String::new(), |(parent, _)| parent.to_owned())
        };
        self.lyrics
            .insert((root.to_owned(), parent(relative), parent(native)));
    }

    fn visit<'s>(
        &'s mut self,
        root: &'s str,
        relative: &'s str,
        native: &'s str,
        entry: Entry,
        siblings: Option<Arc<BTreeSet<String>>>,
    ) -> Pin<Box<dyn Future<Output = SourceResult<()>> + Send + 's>> {
        Box::pin(async move {
            self.check()?;
            let key = walk_key(relative);
            let stored = self.take(root, &key).await?;
            let row = self.files.row(root, relative, native, &entry)?;
            if !entry.directory {
                if !self.unchanged(stored.as_ref(), &row) {
                    self.stage(stored, row, native.to_owned(), siblings, true);
                } else if row.kind == LocalFileKind::Media
                    && stored.is_some_and(|stored| stored.file.state == LocalFileState::Accepted)
                {
                    self.accepted += 1;
                }
                return Ok(());
            }
            if let Some(stored) = &stored
                && stored.file.kind == LocalFileKind::Directory
                && self.files.unchanged_subtree(&stored.file, &row)
            {
                return self.keep_subtree(root, &key).await;
            }
            if !self.unchanged(stored.as_ref(), &row) {
                self.stage(stored, row, native.to_owned(), None, true);
            }
            let entries = match self.files.list(root, native).await {
                Ok(entries) => entries,
                Err(SourceError::Cancelled) => return Err(SourceError::Cancelled),
                Err(error) => {
                    // A directory that fails to list keeps its rows.
                    self.fail(format!("Could not list {relative}: {error}"));
                    return self.keep_subtree(root, &key).await;
                }
            };
            // An empty root with stored rows is an unmounted mount point, as unavailable as an
            // unplugged drive.
            if relative.is_empty()
                && entries.is_empty()
                && self.peek().await?.is_some_and(|row| row.file.root == root)
            {
                self.fail(format!("File source root {root} is empty"));
                return self.keep_subtree(root, &key).await;
            }
            let mut entries = entries
                .into_iter()
                .filter(|entry| !self.files.excluded(root, &join(relative, &entry.name)))
                .collect::<Vec<_>>();
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            entries.dedup_by(|later, earlier| later.name == earlier.name);
            let siblings = Arc::new(
                entries
                    .iter()
                    .filter(|entry| !entry.directory)
                    .map(|entry| entry.name.clone())
                    .collect::<BTreeSet<_>>(),
            );
            let mut lyrics = false;
            // Unchanged files are kept for a read only when a listed sidecar can change.
            let sidecars = siblings
                .iter()
                .any(|name| crate::file::lyrics::is_sidecar(name));
            let mut kept = Vec::new();
            for entry in entries {
                let child = join(relative, &entry.name);
                let child_native = join(native, &entry.native);
                let child_key = walk_key(&child);
                lyrics |= self.vanish_before(root, &key, &child_key).await?;
                if entry.directory {
                    self.visit(root, &child, &child_native, entry, None).await?;
                    continue;
                }
                let stored = self.take(root, &child_key).await?;
                let row = self.files.row(root, &child, &child_native, &entry)?;
                if self.unchanged(stored.as_ref(), &row) {
                    if parseable(row.kind) {
                        if row.kind == LocalFileKind::Media
                            && stored
                                .as_ref()
                                .is_some_and(|stored| stored.file.state == LocalFileState::Accepted)
                        {
                            self.accepted += 1;
                        }
                        if sidecars {
                            kept.push((stored, row, child_native));
                        }
                    }
                    continue;
                }
                // A sidecar read again with the same facts (a resync, a parser change) changed
                // no lyrics.
                lyrics |= crate::file::lyrics::is_sidecar(&row.path)
                    && stored.as_ref().is_none_or(|stored| {
                        (
                            stored.file.size_bytes,
                            stored.file.mtime_ns,
                            &stored.file.revision,
                        ) != (row.size_bytes, row.mtime_ns, &row.revision)
                    });
                self.stage(stored, row, child_native, Some(Arc::clone(&siblings)), true);
            }
            lyrics |= self.vanish_subtree(root, &key).await?;
            // A changed lyrics sidecar reads its directory's media again. Without a listed
            // sidecar the last one went, and the directory is listed again at the end.
            if lyrics && sidecars {
                for (stored, row, native) in kept {
                    self.stage(stored, row, native, Some(Arc::clone(&siblings)), false);
                }
            } else if lyrics {
                self.lyrics
                    .insert((root.to_owned(), relative.to_owned(), native.to_owned()));
            }
            self.flush(false).await
        })
    }

    /// Parses and writes what is staged, at a directory's end once a batch is full: the CUE
    /// sheets first, then the media files in batches, one Write each.
    async fn flush(&mut self, now: bool) -> SourceResult<()> {
        if !now
            && self.staged.len()
                + self.batch.files.len()
                + self.batch.removed.len()
                + self.batch.identities.len()
                < BATCH
        {
            return Ok(());
        }
        if self.staged.is_empty() && self.batch.is_empty() {
            return Ok(());
        }
        // Progress starts with the first work, so an unchanged walk reports nothing, and a
        // cancellation from it stops the walk before this write.
        self.report(true);
        self.check()?;
        let staged = std::mem::take(&mut self.staged);
        let (cues, media): (Vec<_>, Vec<_>) = staged
            .into_iter()
            .partition(|staged| staged.row.kind == LocalFileKind::Cue);
        // CUE sheets first: a media file an accepted sheet reads is not a track of its own.
        let mut sheets = Vec::new();
        let mut taken = BTreeSet::new();
        for mut staged in cues {
            self.check()?;
            let read = self
                .files
                .read_cue(
                    &mut staged.row,
                    &staged.native,
                    Arc::clone(&self.workers[0]),
                    staged.siblings.as_deref(),
                )
                .await;
            staged.row.state = match read {
                Ok(CueRead::Accepted { inputs, tracks }) => {
                    taken.extend(inputs.iter().cloned());
                    sheets.push((staged.row.path.clone(), inputs, tracks));
                    LocalFileState::Accepted
                }
                Ok(CueRead::Rejected) => LocalFileState::Rejected,
                Ok(CueRead::Unreadable) => LocalFileState::Unreadable,
                Err(SourceError::Cancelled) => return Err(SourceError::Cancelled),
                Err(error) => {
                    tracing::warn!(%error, "Could not read a CUE sheet");
                    LocalFileState::Unreadable
                }
            };
            self.push_read(staged);
        }
        let (inputs, mut media): (Vec<_>, Vec<_>) = media
            .into_iter()
            .partition(|staged| taken.contains(&staged.row.path));
        for staged in inputs {
            self.push_read(staged);
        }
        let mut renames = std::mem::take(&mut self.batch.renames);
        loop {
            let rest = media.split_off(media.len().min(BATCH));
            // A rename resolves once its new path has a track, so it waits for that chunk.
            let later = rest
                .iter()
                .map(|staged| staged.row.path.as_str())
                .collect::<HashSet<_>>();
            let (wait, now) = renames
                .into_iter()
                .partition(|(_, new)| later.contains(new.as_str()));
            (self.batch.renames, renames) = (now, wait);
            self.write_chunk(media, std::mem::take(&mut sheets)).await?;
            media = rest;
            if media.is_empty() {
                return Ok(());
            }
        }
    }

    /// Parses a batch of media files and writes them with the rest of the batch.
    async fn write_chunk(
        &mut self,
        media: Vec<Staged>,
        sheets: Vec<(String, Vec<String>, Vec<library::TrackObservation>)>,
    ) -> SourceResult<()> {
        let catalog = self.observation.catalog;
        let source = self.observation.source;
        let files = self.files;
        let workers = self.workers.clone();
        let mut reads = stream::iter(media.into_iter().enumerate().map(|(index, mut staged)| {
            let worker = Arc::clone(&workers[index % workers.len()]);
            async move {
                let read = files
                    .read_media(
                        &mut staged.row,
                        &staged.native,
                        worker,
                        staged.siblings.as_deref(),
                    )
                    .await;
                (staged, read)
            }
        }))
        .buffered(self.workers.len());
        let mut accepted = Vec::new();
        while let Some((mut staged, read)) = reads.next().await {
            self.check()?;
            staged.row.state = match read {
                Ok(MediaRead::Accepted(track)) => {
                    track.observe_images(&mut staged.row)?;
                    accepted.push((staged, track.into_observation(media::accepted_at())));
                    continue;
                }
                Ok(MediaRead::Rejected) => LocalFileState::Rejected,
                Ok(MediaRead::Unreadable) => LocalFileState::Unreadable,
                Err(SourceError::Cancelled) => return Err(SourceError::Cancelled),
                Err(error) => {
                    tracing::warn!(%error, "Could not read a media file");
                    LocalFileState::Unreadable
                }
            };
            self.push_read(staged);
            self.report(false);
        }
        drop(reads);
        // A changed input of an accepted sheet the Store holds reads that sheet again.
        let paths = accepted
            .iter()
            .map(|(staged, _)| staged.row.path.as_str())
            .collect::<Vec<_>>();
        let mut owned = BTreeSet::new();
        if !paths.is_empty() {
            for (input, cue) in catalog.cue_owners(source, &paths).await? {
                if !sheets.iter().any(|(sheet, _, _)| *sheet == cue) {
                    owned.insert(input);
                    self.forced.insert(cue);
                }
            }
        }
        // A file read again keeps its tracks' object ids, which server-style URIs derive from,
        // and a renamed file takes the ids of the path it left.
        let renamed = self
            .batch
            .renames
            .iter()
            .map(|(old, new)| (new.clone(), old.clone()))
            .collect::<HashMap<_, _>>();
        let paths = accepted
            .iter()
            .map(|(staged, _)| staged.row.path.as_str())
            .filter(|path| !owned.contains(*path))
            .chain(sheets.iter().map(|(sheet, _, _)| sheet.as_str()))
            .chain(renamed.values().map(String::as_str))
            .collect::<Vec<_>>();
        let mut objects: HashMap<String, String> = if paths.is_empty() {
            HashMap::new()
        } else {
            catalog
                .file_track_objects(source, &paths)
                .await?
                .into_iter()
                .rev()
                .collect()
        };
        for (new, old) in &renamed {
            if let Some(id) = objects.get(old).cloned() {
                objects.entry(new.clone()).or_insert(id);
            }
        }
        for (path, inputs, mut tracks) in sheets {
            if let Some(prefix) = objects
                .get(&path)
                .and_then(|id| id.rsplit_once(':'))
                .map(|(prefix, _)| prefix)
            {
                for track in &mut tracks {
                    if let Some((_, number)) = track.object_id.rsplit_once(':') {
                        track.object_id = format!("{prefix}:{number}");
                    }
                }
            }
            self.batch.hidden.extend(inputs.iter().cloned());
            self.batch.inputs.push((path, inputs));
            self.accepted += tracks.len();
            self.batch.tracks.extend(tracks);
        }
        for (mut staged, mut track) in accepted {
            staged.row.state = LocalFileState::Accepted;
            if !owned.contains(&staged.row.path) {
                if let Some(id) = objects.get(&staged.row.path) {
                    track.object_id.clone_from(id);
                }
                self.batch.tracks.push(track);
                self.accepted += 1;
            }
            self.push_read(staged);
        }
        self.check()?;
        let batch = std::mem::take(&mut self.batch);
        let released = self.observation.write_files(batch).await?;
        self.forced.extend(released);
        self.report(true);
        Ok(())
    }

    /// A read file's row, unless it reads as the Store holds it: an unreadable or rejected
    /// file read again writes nothing.
    fn push_read(&mut self, staged: Staged) {
        if staged.stored.as_ref() != Some(&staged.row) {
            self.directories.extend(parent_directory(&staged.row.path));
            self.batch.files.push(staged.row);
        }
    }

    /// An imported file that its folder's listing did not hold: it stays while it exists, and
    /// a missing one keeps its row, unreadable, so it returns with its file.
    async fn recheck(&mut self, row: StoredFile) -> SourceResult<()> {
        self.check()?;
        let (root, relative) = (row.file.root.clone(), row.file.relative_path.clone());
        let native = self
            .files
            .place(&row.file.path)
            .map_or_else(|| relative.clone(), |(_, _, native)| native);
        match self.files.stat(&root, &native).await {
            Ok(entry) if !entry.directory => {
                let file = self.files.row(&root, &relative, &native, &entry)?;
                if !self.unchanged(Some(&row), &file) {
                    self.stage(Some(row), file, native, None, false);
                }
            }
            Ok(_) => {}
            Err(SourceError::NotFound) => {
                if row.file.state != LocalFileState::Unreadable {
                    let mut file = row.file;
                    file.state = LocalFileState::Unreadable;
                    self.directories.extend(parent_directory(&file.path));
                    self.batch.files.push(file);
                }
            }
            Err(SourceError::Cancelled) => return Err(SourceError::Cancelled),
            Err(error) => self.fail(format!("Could not inspect {relative}: {error}")),
        }
        Ok(())
    }

    async fn finish(&mut self) -> SourceResult<()> {
        for row in std::mem::take(&mut self.imports) {
            self.recheck(row).await?;
        }
        // Paths the walk passed before it learned they need a read: CUE sheets whose inputs
        // changed, and inputs a sheet released.
        self.flush(true).await?;
        for _ in 0..4 {
            if self.forced.is_empty() {
                break;
            }
            for path in std::mem::take(&mut self.forced) {
                self.check()?;
                let Some((root, relative, native)) = self.files.place(&path) else {
                    continue;
                };
                match self.files.stat(&root, &native).await {
                    Ok(entry) if !entry.directory => {
                        let row = self.files.row(&root, &relative, &native, &entry)?;
                        self.stage(None, row, native, None, false);
                    }
                    Ok(_) | Err(SourceError::NotFound) => {}
                    Err(SourceError::Cancelled) => return Err(SourceError::Cancelled),
                    Err(error) => tracing::warn!(%error, "Could not inspect a file"),
                }
            }
            self.flush(true).await?;
        }
        for (root, relative, native) in std::mem::take(&mut self.lyrics) {
            self.check()?;
            let Ok(entries) = self.files.list(&root, &native).await else {
                continue;
            };
            let siblings = Arc::new(
                entries
                    .iter()
                    .filter(|entry| !entry.directory)
                    .map(|entry| entry.name.clone())
                    .collect::<BTreeSet<_>>(),
            );
            for entry in entries.iter().filter(|entry| !entry.directory) {
                let child = join(&relative, &entry.name);
                if self.files.excluded(&root, &child) {
                    continue;
                }
                let child_native = join(&native, &entry.native);
                let row = self.files.row(&root, &child, &child_native, entry)?;
                if parseable(row.kind) && !crate::file::lyrics::is_sidecar(&row.path) {
                    self.stage(None, row, child_native, Some(Arc::clone(&siblings)), false);
                }
            }
        }
        self.flush(true).await
    }
}

/// The directories a cancelled walk left without their folder artwork, for the next pass.
#[derive(Default)]
pub(crate) struct ArtworkState(Mutex<BTreeSet<String>>);

impl ArtworkState {
    fn defer(&self, mut directories: BTreeSet<String>) {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .append(&mut directories);
    }

    /// The folder-artwork pass for the directories a walk changed and those still owed. A
    /// resync reads every album's artwork again.
    async fn pass(
        &self,
        files: crate::file::artwork::ArtworkFiles<'_>,
        observation: &mut library::Observation<'_>,
        mut directories: BTreeSet<String>,
        resync: bool,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        directories.append(&mut self.0.lock().unwrap_or_else(|error| error.into_inner()));
        if !resync && directories.is_empty() {
            return Ok(());
        }
        let scope = (!resync).then(|| directories.iter().cloned().collect::<Vec<_>>());
        let read = files
            .acquire(observation, scope.as_deref(), cancelled)
            .await;
        if matches!(read, Err(SourceError::Cancelled)) {
            self.defer(directories);
        }
        read
    }
}
