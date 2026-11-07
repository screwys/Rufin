//! The sync folder carries records between devices that are never online together. Each
//! device writes its own rows as segments `<first>-<last>.rcs` into
//! `Rufin Connect/<profile>/<device>/` and reads the directories of the other trusted
//! devices. A segment is encrypted whole with the profile key, because the folder leaves the
//! device; one written halfway does not decrypt and is read again later.
use std::{
    io::{Read as _, Write as _},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use rufin_connect::store::{Cursor, Row};

use super::{ConnectOwner, Receiving, Session, SyncFolder};
use crate::string_error;

const ROOT: &str = "Rufin Connect";
const SUFFIX: &str = ".rcs";
const PAGE: usize = 512;
/// More segments than this in a device's directory are compacted into one.
const SEGMENTS: usize = 64;

/// What one session's sync folder work shares: segments are written one pass at a time,
/// and a folder on this device is watched.
#[derive(Default)]
pub(super) struct State {
    lock: tokio::sync::Mutex<()>,
    watcher: Mutex<Option<notify::RecommendedWatcher>>,
}

enum Folder {
    Path(PathBuf),
    Document(String),
    Connection(Arc<sources::Source>, String),
}

struct Segment {
    first: i64,
    last: i64,
    handle: String,
}

/// Writes this device's new records to the folder.
pub(super) async fn write(owner: &Arc<ConnectOwner>, session: &Arc<Session>) {
    let Some(folder) = owner.status.borrow().settings.folder.clone() else {
        return;
    };
    let _lock = session.folder.lock.lock().await;
    let result = async {
        let folder = Folder::open(owner, &folder)?;
        write_own(owner, session, &folder).await
    }
    .await;
    if let Err(error) = result {
        tracing::warn!(%error, "Connect could not write the sync folder");
    }
}

/// Writes this device's new records, then applies what the other devices wrote.
pub(super) async fn sync(owner: &Arc<ConnectOwner>, session: &Arc<Session>) {
    let Some(folder) = owner.status.borrow().settings.folder.clone() else {
        return;
    };
    let _lock = session.folder.lock.lock().await;
    let result = async {
        let folder = Folder::open(owner, &folder)?;
        write_own(owner, session, &folder).await?;
        read_others(session, &folder).await
    }
    .await;
    match result {
        Ok(0) => {}
        Ok(_) => owner.relay(session).await,
        Err(error) => tracing::warn!(%error, "Connect could not sync the folder"),
    }
}

/// Reads the folder soon after a file changes in it, when it is on this device.
pub(super) fn watch(owner: &Arc<ConnectOwner>, session: &Arc<Session>) {
    use notify::Watcher as _;
    let folder = owner.status.borrow().settings.folder.clone();
    let mut watcher = session
        .folder
        .watcher
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    *watcher = None;
    let Some(SyncFolder::Path { path }) = folder else {
        return;
    };
    let changed = Arc::new(tokio::sync::Notify::new());
    let wake = Arc::clone(&changed);
    // This device's own writes are no news to it.
    let own = path
        .join(ROOT)
        .join(&session.profile)
        .join(&session.records.device);
    let created = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if event.is_ok_and(|event| {
            !event.kind.is_access() && event.paths.iter().any(|path| !path.starts_with(&own))
        }) {
            wake.notify_one();
        }
    })
    .and_then(|mut watching| {
        let root = path.join(ROOT);
        std::fs::create_dir_all(&root)?;
        watching.watch(&root, notify::RecursiveMode::Recursive)?;
        Ok(watching)
    });
    match created {
        Ok(watching) => *watcher = Some(watching),
        Err(error) => {
            tracing::warn!(%error, "Connect cannot watch the sync folder");
            return;
        }
    }
    let weak = Arc::downgrade(owner);
    let session_of = Arc::downgrade(session);
    owner
        .runtime
        .spawn(session.stop.clone().run_until_cancelled_owned(async move {
            loop {
                changed.notified().await;
                // A sync tool writes several files in a row.
                tokio::time::sleep(Duration::from_secs(2)).await;
                let (Some(owner), Some(session)) = (weak.upgrade(), session_of.upgrade()) else {
                    return;
                };
                sync(&owner, &session).await;
            }
        }));
}

async fn write_own(
    owner: &Arc<ConnectOwner>,
    session: &Arc<Session>,
    folder: &Folder,
) -> Result<(), String> {
    let records = &session.records;
    let written = records
        .store
        .profile()
        .await
        .map_err(string_error)?
        .ok_or("Connect is not enabled")?
        .written;
    let directory = [ROOT, &session.profile, &records.device].map(str::to_owned);
    let (rows, last) = own_rows(session, written).await?;
    if last == written {
        return Ok(());
    }
    let name = format!("{:019}-{last:019}{SUFFIX}", written + 1);
    folder
        .write(owner, &directory, &name, seal(session, &rows)?)
        .await?;
    records
        .store
        .set_written(last)
        .await
        .map_err(string_error)?;
    let segments = folder.list(&directory).await?;
    // The segments after the oldest merge into one; read in order, its rows supersede the
    // oldest one's.
    if segments.len() > SEGMENTS
        && let Some(oldest) = segments.iter().min_by_key(|segment| segment.first)
    {
        let (rows, _) = own_rows(session, oldest.last).await?;
        let first = oldest.last + 1;
        let name = format!("{first:019}-{last:019}{SUFFIX}");
        folder
            .write(owner, &directory, &name, seal(session, &rows)?)
            .await?;
        for segment in segments.iter().filter(|segment| {
            segment.handle != oldest.handle && (segment.first, segment.last) != (first, last)
        }) {
            folder.delete(&segment.handle).await?;
        }
    }
    Ok(())
}

/// This device's own rows after `after`, with their values, and the last sequence examined.
async fn own_rows(session: &Arc<Session>, mut after: i64) -> Result<(Vec<Row>, i64), String> {
    let records = &session.records;
    let mut rows = Vec::new();
    loop {
        let mut page = records
            .store
            .rows_after(after, PAGE, Some(&records.device))
            .await
            .map_err(string_error)?;
        let Some(last) = page.last().map(|row| row.sequence) else {
            break;
        };
        after = last;
        rufin_connect::sync::Values::encode(records.as_ref(), &mut page)
            .await
            .map_err(string_error)?;
        rows.extend(
            page.into_iter()
                .filter(|row| row.deleted || row.value.is_some()),
        );
    }
    Ok((rows, after))
}

/// Applies the segments other devices wrote; returns how many records applied.
async fn read_others(session: &Arc<Session>, folder: &Folder) -> Result<u64, String> {
    let records = &session.records;
    let cursors = records
        .store
        .peers()
        .await
        .map_err(string_error)?
        .into_iter()
        .map(|peer| (peer.device, peer.read))
        .collect::<std::collections::HashMap<_, _>>();
    let mut receiving = Receiving::default();
    let mut ends = Vec::new();
    let mut applied = 0;
    for device in records.peers() {
        let read = cursors.get(&device).copied().unwrap_or(0);
        let directory = [ROOT, &session.profile, &device].map(str::to_owned);
        let mut segments = folder
            .list(&directory)
            .await?
            .into_iter()
            .filter(|segment| segment.last > read)
            .collect::<Vec<_>>();
        segments.sort_by_key(|segment| (segment.first, segment.last));
        let mut end = read;
        for segment in segments {
            // The cursor stays below a segment not delivered yet, as segments are named after
            // the one written before them, and below one that is not written completely yet.
            if segment.first > end + 1 {
                break;
            }
            let rows = match open(session, &folder.read(&segment.handle).await?) {
                Ok(rows) => rows,
                Err(error) => {
                    tracing::debug!(%error, segment.handle, "Connect sync folder segment is incomplete");
                    break;
                }
            };
            let rows = rows
                .into_iter()
                .filter(|row| row.sequence > read && row.device == device)
                .collect::<Vec<_>>();
            end = end.max(segment.last);
            if rows.is_empty() {
                continue;
            }
            applied += records
                .receive(&device, rows, &mut receiving, None, |device, sequence| {
                    Cursor::Read { device, sequence }
                })
                .await?;
        }
        if end > read {
            ends.push((device, end));
        }
    }
    for (device, end) in ends {
        applied += records
            .receive(
                &device,
                Vec::new(),
                &mut receiving,
                Some(end),
                |device, sequence| Cursor::Read { device, sequence },
            )
            .await?;
    }
    Ok(applied)
}

fn seal(session: &Session, rows: &[Row]) -> Result<Vec<u8>, String> {
    let mut plain = Vec::new();
    for row in rows {
        serde_json::to_writer(&mut plain, row).map_err(string_error)?;
        plain.push(b'\n');
    }
    session.records.encrypt(&plain)
}

fn open(session: &Session, bytes: &[u8]) -> Result<Vec<Row>, String> {
    String::from_utf8(session.records.decrypt(bytes)?)
        .map_err(string_error)?
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).map_err(string_error))
        .collect()
}

fn segment(name: String, handle: String) -> Option<Segment> {
    let (first, last) = name.strip_suffix(SUFFIX)?.split_once('-')?;
    Some(Segment {
        first: first.parse().ok()?,
        last: last.parse().ok()?,
        handle,
    })
}

impl Folder {
    fn open(owner: &ConnectOwner, folder: &SyncFolder) -> Result<Self, String> {
        Ok(match folder {
            SyncFolder::Path { path } => Self::Path(path.clone()),
            SyncFolder::Document { document } => Self::Document(document.clone()),
            SyncFolder::Connection { connection, path } => {
                Self::Connection(owner.source.client(connection)?, path.clone())
            }
        })
    }

    /// The segments in a directory; a missing directory holds none.
    async fn list(&self, directory: &[String]) -> Result<Vec<Segment>, String> {
        let files = match self {
            Self::Path(root) => {
                let path = directory
                    .iter()
                    .fold(root.clone(), |path, part| path.join(part));
                tokio::task::spawn_blocking(move || match std::fs::read_dir(&path) {
                    Ok(entries) => entries
                        .map(|entry| {
                            let entry = entry.map_err(string_error)?;
                            Ok((
                                entry.file_name().to_string_lossy().into_owned(),
                                entry.path().to_string_lossy().into_owned(),
                            ))
                        })
                        .collect::<Result<Vec<_>, String>>(),
                    Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
                    Err(cause) => Err(string_error(cause)),
                })
                .await
                .map_err(string_error)??
            }
            Self::Document(root) => {
                let (root, directory) = (root.clone(), directory.to_vec());
                tokio::task::spawn_blocking(move || {
                    let mut current = sources::stat_document(&root).map_err(string_error)?;
                    for part in &directory {
                        match sources::find_document_child(&current.uri, part)
                            .map_err(string_error)?
                        {
                            Some(child) => current = child,
                            None => return Ok(Vec::new()),
                        }
                    }
                    let mut files = Vec::new();
                    let mut offset = 0;
                    loop {
                        let page =
                            sources::list_documents(&current.uri, offset).map_err(string_error)?;
                        files.extend(
                            page.entries
                                .into_iter()
                                .filter(|entry| !entry.directory)
                                .map(|entry| (entry.name, entry.uri)),
                        );
                        match page.next {
                            Some(next) => offset = next,
                            None => return Ok::<_, String>(files),
                        }
                    }
                })
                .await
                .map_err(string_error)??
            }
            Self::Connection(source, root) => source
                .file_connection()
                .map_err(string_error)?
                .list_folder_files(&relative(root, directory), SUFFIX)
                .await
                .map_err(string_error)?,
        };
        Ok(files
            .into_iter()
            .filter_map(|(name, handle)| segment(name, handle))
            .collect())
    }

    async fn read(&self, handle: &str) -> Result<Vec<u8>, String> {
        match self {
            Self::Path(_) => tokio::fs::read(handle).await.map_err(string_error),
            Self::Document(_) => {
                let handle = handle.to_owned();
                tokio::task::spawn_blocking(move || {
                    let mut bytes = Vec::new();
                    sources::open_document_input(&handle)
                        .map_err(string_error)?
                        .read_to_end(&mut bytes)
                        .map_err(string_error)?;
                    Ok(bytes)
                })
                .await
                .map_err(string_error)?
            }
            Self::Connection(source, _) => {
                let file = tempfile::NamedTempFile::new().map_err(string_error)?;
                source
                    .file_connection()
                    .map_err(string_error)?
                    .copy_file(handle, file.path())
                    .await
                    .map_err(string_error)?
                    .ok_or("The sync folder file is gone")?;
                tokio::fs::read(file.path()).await.map_err(string_error)
            }
        }
    }

    async fn write(
        &self,
        owner: &ConnectOwner,
        directory: &[String],
        name: &str,
        bytes: Vec<u8>,
    ) -> Result<(), String> {
        match self {
            Self::Path(root) => {
                let path = directory
                    .iter()
                    .fold(root.clone(), |path, part| path.join(part));
                tokio::fs::create_dir_all(&path)
                    .await
                    .map_err(string_error)?;
                tokio::fs::write(path.join(name), bytes)
                    .await
                    .map_err(string_error)
            }
            Self::Document(root) => {
                let file = staged(owner, &bytes)?;
                let (root, directory, name) = (root.clone(), directory.to_vec(), name.to_owned());
                tokio::task::spawn_blocking(move || {
                    let parent = sources::create_document_directories(&root, &directory)
                        .map_err(string_error)?;
                    match sources::find_document_child(&parent.uri, &name).map_err(string_error)? {
                        Some(existing) => sources::save_document(&existing.uri, file.path(), None)
                            .map(|_| ())
                            .map_err(string_error),
                        None => sources::create_document(
                            &parent.uri,
                            &name,
                            "application/octet-stream",
                            file.path(),
                        )
                        .map(|_| ())
                        .map_err(string_error),
                    }
                })
                .await
                .map_err(string_error)?
            }
            Self::Connection(source, root) => {
                let file = staged(owner, &bytes)?;
                let files = source.file_connection().map_err(string_error)?;
                files
                    .write_file(
                        &format!("{}/{name}", relative(root, directory)),
                        file.path(),
                    )
                    .await
                    .map_err(string_error)
            }
        }
    }

    async fn delete(&self, handle: &str) -> Result<(), String> {
        match self {
            Self::Path(_) => tokio::fs::remove_file(handle).await.map_err(string_error),
            Self::Document(_) => {
                let handle = handle.to_owned();
                tokio::task::spawn_blocking(move || sources::delete_document(&handle))
                    .await
                    .map_err(string_error)?
                    .map_err(string_error)
            }
            Self::Connection(source, _) => source
                .file_connection()
                .map_err(string_error)?
                .remove_file(handle)
                .await
                .map_err(string_error),
        }
    }
}

/// A segment in a file the document host and file connections upload from.
fn staged(owner: &ConnectOwner, bytes: &[u8]) -> Result<tempfile::NamedTempFile, String> {
    let mut file = tempfile::NamedTempFile::new_in(&owner.directory).map_err(string_error)?;
    file.write_all(bytes).map_err(string_error)?;
    Ok(file)
}

fn relative(root: &str, directory: &[String]) -> String {
    std::iter::once(root.trim_matches('/'))
        .chain(directory.iter().map(String::as_str))
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}
