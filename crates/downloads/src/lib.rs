//! Download queue ownership, command handling, and bounded transfer execution.

#[cfg(feature = "bindings")]
uniffi::setup_scaffolding!();

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};
use std::task::Poll;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_channel::{Receiver, Sender};
use audio_processing::ResolvedStream;
use library::{Database, FolderKey, SourceKey, TrackKey};
use serde::{Deserialize, Serialize};
use sources::{DownloadQuality, ResolvedDownload};
use sources::{Source, SourceError, SourceId};
use tracing::warn;

mod quality;
pub use quality::{DOWNLOAD_BITRATES, download_bitrates};
mod storage;
mod track_download;
use storage::DownloadAudio;
pub use storage::DownloadDirectory;

use track_download::*;

pub type DownloadCount = usize;
#[cfg(feature = "bindings")]
uniffi::custom_type!(DownloadCount, u64, {
    remote,
    lower: |value| value as u64,
    try_lift: |value| Ok(usize::try_from(value)?),
});

const QUEUE_VERSION: u32 = 3;
const QUEUE_FILE: &str = "queue.json";
const QUEUE_PART_FILE: &str = "queue.json.part";
const RETRY_INTERVAL: Duration = Duration::from_secs(30);
const MAX_ACTIVE_DOWNLOADS: usize = 3;

#[derive(Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct Downloads {
    root: Arc<PathBuf>,
    commands: Sender<Command>,
    peer: sources::PeerAccess,
    status: tokio::sync::watch::Receiver<DownloadStatus>,
}

/// Holds the existing download actor at a completed command boundary during restore.
pub struct DownloadSuspension(Sender<()>);

impl Drop for DownloadSuspension {
    fn drop(&mut self) {
        let _ = self.0.try_send(());
    }
}

enum Command {
    ClearArtworkCache(Sender<Result<(), String>>),
    ClearLyricsCache(Sender<Result<(), String>>),
    Page {
        offset: u64,
        limit: u32,
        response: Sender<Result<DownloadPage, String>>,
    },
    Attach {
        source_id: SourceId,
        source_key: SourceKey,
        source: Option<Weak<Source>>,
        folder: Option<FolderKey>,
        input_digest: [u8; 32],
        response: Sender<Result<(), String>>,
    },
    Download {
        subject: DownloadSubject,
        media_uris: Vec<String>,
    },
    ConnectChanged,
    Remove {
        media_uris: Vec<String>,
        notify: bool,
    },
    LibraryChanged {
        source_id: SourceId,
    },
    SettingsChanged(Vec<SourceDownloadSettings>),
    RemoveRule {
        source_id: SourceId,
        rule: DownloadRule,
        delete_downloads: bool,
    },
    Cancel {
        source_id: Option<SourceId>,
        job_id: String,
    },
    ClearJob {
        source_id: SourceId,
        job_id: String,
    },
    SetPaused(bool),
    Suspend {
        ready: Sender<()>,
        resume: Receiver<()>,
    },
    Move {
        source_id: SourceId,
        job_id: String,
        target_job_id: String,
        after: bool,
    },
    Clear {
        source_id: SourceId,
        notify: bool,
    },
}

#[derive(Clone)]
struct AttachedSource {
    source_key: SourceKey,
    input_digest: [u8; 32],
    source: Option<Weak<Source>>,
    folder: Option<FolderKey>,
    directory: Option<DownloadDirectory>,
}

#[derive(Clone)]
struct RuleIntent {
    database: Database,
    source_key: SourceKey,
    folder: Option<FolderKey>,
    rules: DownloadRules,
}

impl RuleIntent {
    fn same_context(&self, other: &Self) -> bool {
        self.source_key == other.source_key && self.folder == other.folder
    }
}

fn same_weak_target<T>(left: &Option<Weak<T>>, right: &Option<Weak<T>>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => Weak::ptr_eq(left, right),
        (None, None) => true,
        _ => false,
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
enum DownloadOwner {
    Subject(DownloadSubject),
    Retained,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct DownloadJob {
    id: String,
    subject: DownloadSubject,
    #[serde(deserialize_with = "quality::deserialize")]
    quality: DownloadQuality,
    #[serde(default)]
    replace_unknown: bool,
    #[serde(default)]
    completed: Vec<String>,
    #[serde(default)]
    failed: bool,
    remaining: Vec<String>,
    state: DownloadQueueState,
}

#[derive(Debug, Deserialize, Serialize)]
struct QueueFile {
    version: u32,
    jobs: Vec<DownloadJob>,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq)]
enum ReleasedDownloadSubject {
    Rule(DownloadRule),
    Track(serde_json::Value),
    Album(serde_json::Value),
    Artist(serde_json::Value),
    Genre(serde_json::Value),
    Mood(serde_json::Value),
    Playlist(serde_json::Value),
    SmartPlaylist(serde_json::Value),
    Prepared {
        context_id: String,
        title: Option<String>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq)]
enum ReleasedDownloadOwner {
    Subject(ReleasedDownloadSubject),
    Retained,
}

#[derive(Debug, Deserialize)]
struct ReleasedDownloadJob {
    id: String,
    subject: ReleasedDownloadSubject,
    #[serde(deserialize_with = "quality::deserialize")]
    quality: DownloadQuality,
    #[serde(rename = "total_tracks")]
    _total_tracks: usize,
    #[serde(default)]
    completed: Vec<serde_json::Value>,
    remaining: Vec<serde_json::Value>,
    state: DownloadQueueState,
}

#[derive(Debug, Deserialize)]
struct ReleasedQueueFile {
    version: u32,
    source_id: SourceId,
    jobs: Vec<ReleasedDownloadJob>,
}

async fn persist_queue(
    root: &Path,
    source_id: Option<&SourceId>,
    jobs: &[DownloadJob],
) -> Result<(), String> {
    let directory = source_directory(root, source_id);
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(|error| format!("could not create the download queue directory: {error}"))?;
    let path = directory.join(QUEUE_FILE);
    let part = directory.join(QUEUE_PART_FILE);
    if jobs.is_empty() {
        remove_file_if_present(&part).await?;
        remove_file_if_present(&path).await?;
        return Ok(());
    }
    let encoded = serde_json::to_vec(&QueueFile {
        version: QUEUE_VERSION,
        jobs: jobs.to_vec(),
    })
    .map_err(|error| format!("could not encode the download queue: {error}"))?;
    tokio::fs::write(&part, encoded)
        .await
        .map_err(|error| format!("could not save the download queue: {error}"))?;
    tokio::fs::rename(&part, &path)
        .await
        .map_err(|error| format!("could not finish the download queue: {error}"))
}

fn read_queue_file(
    root: &Path,
    source_id: Option<&SourceId>,
) -> Result<Option<(Vec<u8>, bool, PathBuf, PathBuf)>, String> {
    let directory = source_directory(root, source_id);
    let path = directory.join(QUEUE_FILE);
    let part = directory.join(QUEUE_PART_FILE);
    match std::fs::read(&path) {
        Ok(bytes) => Ok(Some((bytes, false, path, part))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => match std::fs::read(&part) {
            Ok(bytes) => Ok(Some((bytes, true, path, part))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("could not read {}: {error}", part.display())),
        },
        Err(error) => Err(format!("could not read {}: {error}", path.display())),
    }
}

async fn load_queue(
    root: &Path,
    source_id: &SourceId,
    database: &Database,
    source_key: SourceKey,
    custom_directory: Option<&DownloadDirectory>,
) -> Result<Vec<DownloadJob>, String> {
    let Some((bytes, recovered, path, part)) = read_queue_file(root, Some(source_id))? else {
        return Ok(Vec::new());
    };
    let jobs = if let Ok(queue) = serde_json::from_slice::<QueueFile>(&bytes) {
        if queue.version != QUEUE_VERSION {
            return Err("the saved download queue has an unsupported version".to_string());
        }
        queue.jobs
    } else {
        let released = serde_json::from_slice::<ReleasedQueueFile>(&bytes)
            .map_err(|error| format!("could not decode {}: {error}", path.display()))?;
        if !matches!(released.version, 1 | 2) || released.source_id != *source_id {
            return Err("the saved download queue does not match this source".to_string());
        }
        let cancellation = library::ReadCancellation::new();
        let mut rebound = Vec::new();
        for job in released.jobs {
            let mut completed = Vec::new();
            let mut remaining = Vec::new();
            for identity in job.completed {
                if let Some((media_uri, _)) =
                    released_queue_media_uri(database, source_key, &identity, &cancellation).await?
                {
                    completed.push(media_uri);
                }
            }
            for identity in job.remaining {
                if let Some((media_uri, staging_identity)) =
                    released_queue_media_uri(database, source_key, &identity, &cancellation).await?
                {
                    migrate_released_staging(
                        root,
                        source_id,
                        &staging_identity,
                        &media_uri,
                        custom_directory,
                    )
                    .await?;
                    remaining.push(media_uri);
                }
            }
            if !remaining.is_empty() {
                let subject = rebind_released_subject(
                    job.subject,
                    &completed
                        .iter()
                        .chain(&remaining)
                        .cloned()
                        .collect::<Vec<_>>(),
                );
                rebound.push(DownloadJob {
                    id: job.id,
                    subject,
                    quality: job.quality,
                    replace_unknown: false,
                    completed,
                    failed: false,
                    remaining,
                    state: job.state,
                });
            }
        }
        persist_queue(root, Some(source_id), &rebound).await?;
        rebound
    };
    if recovered && let Err(error) = std::fs::rename(&part, &path) {
        warn!(%error, path = %part.display(), "could not finish recovering the download queue");
    }
    Ok(jobs)
}

async fn load_direct_queue(
    root: &Path,
    database: &Database,
    settings: &HashMap<SourceId, SourceDownloadSettings>,
) -> Result<Vec<DownloadJob>, String> {
    let Some((bytes, recovered, path, part)) = read_queue_file(root, None)? else {
        return Ok(Vec::new());
    };
    let mut queue: QueueFile = serde_json::from_slice(&bytes)
        .map_err(|error| format!("could not decode {}: {error}", path.display()))?;
    if queue.version != QUEUE_VERSION {
        return Err("the saved direct download queue has an unsupported version".to_string());
    }
    if recovered && let Err(error) = std::fs::rename(&part, &path) {
        warn!(%error, path=%part.display(), "could not finish recovering the direct download queue");
    }
    queue.jobs.retain(|job| !matches!(&job.subject, DownloadSubject::Prepared { context_id, .. } if context_id=="connect-local"));
    let mut grouped = HashMap::<Option<SourceId>, Vec<DownloadJob>>::new();
    for mut job in queue.jobs {
        let completed = std::mem::take(&mut job.completed);
        let remaining = std::mem::take(&mut job.remaining);
        let mut split = HashMap::<Option<SourceId>, DownloadJob>::new();
        for (completed, uri) in completed
            .into_iter()
            .map(|uri| (true, uri))
            .chain(remaining.into_iter().map(|uri| (false, uri)))
        {
            let source = media_source_id(database, &uri).await;
            let part = split.entry(source).or_insert_with(|| job.clone());
            if completed {
                part.completed.push(uri);
            } else {
                part.remaining.push(uri);
            }
        }
        for (source, job) in split {
            if !job.remaining.is_empty() {
                grouped.entry(source).or_default().push(job);
            }
        }
    }
    let direct = grouped.remove(&None).unwrap_or_default();
    for (source, jobs) in grouped {
        let source = source.unwrap();
        let key = database
            .source_identity_key(&source)
            .await
            .map_err(|error| error.to_string())?
            .ok_or("The download source is unavailable")?;
        let directory = settings
            .get(&source)
            .and_then(|settings| settings.directory.as_ref());
        let mut saved = load_queue(root, &source, database, key, directory).await?;
        for job in jobs {
            if let Some(existing) = saved.iter_mut().find(|existing| existing.id == job.id) {
                let mut completed = existing.completed.iter().cloned().collect::<HashSet<_>>();
                let mut remaining = existing.remaining.iter().cloned().collect::<HashSet<_>>();
                existing.completed.extend(
                    job.completed
                        .into_iter()
                        .filter(|uri| completed.insert(uri.clone())),
                );
                existing.remaining.extend(
                    job.remaining
                        .into_iter()
                        .filter(|uri| !completed.contains(uri) && remaining.insert(uri.clone())),
                );
                existing.remaining.retain(|uri| !completed.contains(uri));
            } else {
                saved.push(job);
            }
        }
        persist_queue(root, Some(&source), &saved).await?;
    }
    persist_queue(root, None, &direct).await?;
    let queued = direct
        .iter()
        .flat_map(|job| job.remaining.iter().cloned())
        .collect();
    cleanup_staging(root, None, None, &queued)
        .await
        .map_err(|error| error.to_string())?;
    Ok(direct)
}

async fn released_queue_media_uri(
    database: &Database,
    source: SourceKey,
    identity: &serde_json::Value,
    cancellation: &library::ReadCancellation,
) -> Result<Option<(String, String)>, String> {
    let (key, staging_identity) = if let Some(object_id) = identity.as_str() {
        let Some(key) = database
            .catalog_keys(
                library::CatalogKind::Track,
                Some(source),
                &[(object_id).to_string()],
                cancellation,
            )
            .await
            .map(|keys| keys.into_iter().next().map(library::TrackKey::from_raw))
            .map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        (key, object_id.to_string())
    } else if let Some(raw) = identity.as_i64() {
        (TrackKey::from_raw(raw), raw.to_string())
    } else {
        return Err("the saved download queue has an invalid media identity".to_string());
    };
    Ok(database
        .selected_track_uris(
            &library::CatalogQuery::keys(
                library::CatalogKind::Track,
                Some(source),
                (&[key]).iter().map(|key| key.raw()).collect(),
            )
            .selection(),
            None,
            cancellation,
        )
        .await
        .map_err(|error| error.to_string())?
        .pop()
        .map(|uri| (uri, staging_identity)))
}

async fn media_source_id(database: &Database, media_uri: &str) -> Option<SourceId> {
    if let Some((source, _, _)) = library::source_entity_parts(media_uri) {
        return Some(source);
    }
    database
        .track_source_by_uri(media_uri, &library::ReadCancellation::new())
        .await
        .ok()
        .flatten()
}

fn rebind_released_subject(
    subject: ReleasedDownloadSubject,
    media_uris: &[String],
) -> DownloadSubject {
    match subject {
        ReleasedDownloadSubject::Rule(rule) => DownloadSubject::Rule(rule),
        ReleasedDownloadSubject::Prepared { context_id, title } => {
            DownloadSubject::Prepared { context_id, title }
        }
        ReleasedDownloadSubject::Track(_) => {
            DownloadSubject::for_media_uris("track", Some("Track"), media_uris)
        }
        ReleasedDownloadSubject::Album(_) => {
            DownloadSubject::for_media_uris("album", Some("Album"), media_uris)
        }
        ReleasedDownloadSubject::Artist(_) => {
            DownloadSubject::for_media_uris("artist", Some("Artist"), media_uris)
        }
        ReleasedDownloadSubject::Genre(_) => {
            DownloadSubject::for_media_uris("genre", Some("Genre"), media_uris)
        }
        ReleasedDownloadSubject::Mood(_) => {
            DownloadSubject::for_media_uris("mood", Some("Mood"), media_uris)
        }
        ReleasedDownloadSubject::Playlist(_) => {
            DownloadSubject::for_media_uris("playlist", Some("Playlist"), media_uris)
        }
        ReleasedDownloadSubject::SmartPlaylist(_) => {
            DownloadSubject::for_media_uris("smart-playlist", Some("Smart Playlist"), media_uris)
        }
    }
}

async fn migrate_released_staging(
    root: &Path,
    source_id: &SourceId,
    track_object_id: &str,
    media_uri: &str,
    custom_directory: Option<&DownloadDirectory>,
) -> Result<(), String> {
    let (old_part, old_checkpoint) =
        released_staging_paths(root, source_id, track_object_id, custom_directory);
    let current = staging_paths(root, Some(source_id), media_uri, custom_directory);
    for (old, new) in [
        (old_part, current.audio_part),
        (old_checkpoint, current.checkpoint),
    ] {
        if old == new || !old.exists() || new.exists() {
            continue;
        }
        if let Some(parent) = new.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| error.to_string())?;
        }
        tokio::fs::rename(&old, &new)
            .await
            .map_err(|error| format!("could not preserve {}: {error}", old.display()))?;
    }
    Ok(())
}

enum DownloadFailure {
    Item(String),
    Retry(String),
    NeedsAttention(String),
}

struct ActiveDownload {
    source_id: Option<SourceId>,
    job_id: String,
    media_uri: String,
    subject: DownloadSubject,
    paths: DownloadPaths,
    quality: DownloadQuality,
    cancellation: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<Result<(), DownloadFailure>>,
    revision: Arc<Mutex<Option<String>>>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum DownloadRule {
    EntireLibrary,
    Favorites,
    AllPlaylists,
    LatestFiveAlbums,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DownloadRules {
    #[serde(default)]
    pub entire_library: bool,
    #[serde(default)]
    pub favorites: bool,
    #[serde(default)]
    pub all_playlists: bool,
    #[serde(default)]
    pub latest_five_albums: bool,
}

impl DownloadRules {
    pub fn is_empty(self) -> bool {
        !self.entire_library && !self.favorites && !self.all_playlists && !self.latest_five_albums
    }

    pub fn contains(self, rule: DownloadRule) -> bool {
        match rule {
            DownloadRule::EntireLibrary => self.entire_library,
            DownloadRule::Favorites => self.favorites,
            DownloadRule::AllPlaylists => self.all_playlists,
            DownloadRule::LatestFiveAlbums => self.latest_five_albums,
        }
    }

    pub fn set(&mut self, rule: DownloadRule, active: bool) {
        match rule {
            DownloadRule::EntireLibrary => self.entire_library = active,
            DownloadRule::Favorites => self.favorites = active,
            DownloadRule::AllPlaylists => self.all_playlists = active,
            DownloadRule::LatestFiveAlbums => self.latest_five_albums = active,
        }
    }

    pub fn active(self) -> impl Iterator<Item = DownloadRule> {
        DownloadRule::ALL
            .into_iter()
            .filter(move |rule| self.contains(*rule))
    }
}

impl DownloadRule {
    pub const ALL: [Self; 4] = [
        Self::EntireLibrary,
        Self::Favorites,
        Self::AllPlaylists,
        Self::LatestFiveAlbums,
    ];
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SourceDownloadSettings {
    pub source_id: SourceId,
    #[serde(flatten)]
    pub rules: DownloadRules,
    #[serde(default)]
    #[serde(deserialize_with = "quality::deserialize")]
    pub quality: DownloadQuality,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<DownloadDirectory>,
}

impl SourceDownloadSettings {
    pub fn for_source(source_id: SourceId) -> Self {
        Self {
            source_id,
            rules: DownloadRules::default(),
            quality: DownloadQuality::Original,
            directory: None,
        }
    }

    pub fn is_default(&self) -> bool {
        self.rules.is_empty()
            && self.quality == DownloadQuality::Original
            && self.directory.is_none()
    }
}

async fn rule_media_uris(
    database: &Database,
    source: SourceKey,
    folder: Option<FolderKey>,
    rule: DownloadRule,
) -> library::LibraryResult<Vec<String>> {
    let cancellation = library::ReadCancellation::new();
    match rule {
        DownloadRule::EntireLibrary | DownloadRule::Favorites => {
            let mut query = library::CatalogQuery::tracks(library::TrackScope::Tracks(source));
            query.folder = folder;
            query.favorites_only = matches!(rule, DownloadRule::Favorites);
            database
                .selected_track_uris(&query.selection(), None, &cancellation)
                .await
        }
        DownloadRule::AllPlaylists => {
            database
                .all_playlist_track_order(source, folder, &cancellation)
                .await
        }
        DownloadRule::LatestFiveAlbums => {
            database
                .latest_album_track_order(source, folder, 5, &cancellation)
                .await
        }
    }
}

type PreparedRules = Result<Vec<(DownloadRule, Vec<String>)>, String>;

fn prepare_rules(intent: RuleIntent, prepared: Sender<PreparedRules>) {
    tokio::spawn(async move {
        let RuleIntent {
            database,
            source_key,
            folder,
            rules,
        } = intent;
        let mut result = Vec::new();
        for rule in rules.active() {
            match rule_media_uris(&database, source_key, folder, rule).await {
                Ok(media_uris) => result.push((rule, media_uris)),
                Err(error) => {
                    drop(prepared.try_send(Err(error.to_string())));
                    return;
                }
            }
        }
        drop(prepared.try_send(Ok(result)));
    });
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum DownloadSubject {
    Rule(DownloadRule),
    Prepared {
        context_id: String,
        title: Option<String>,
    },
}

impl DownloadSubject {
    pub fn for_media_uris(context: &str, title: Option<&str>, media_uris: &[String]) -> Self {
        let encoded = serde_json::to_vec(media_uris).unwrap_or_default();
        Self::Prepared {
            context_id: format!("{context}:{}", hash_id_bytes(&encoded)),
            title: title.map(str::to_string),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum DownloadQueueState {
    Queued,
    Downloading,
    WaitingForConnection,
    NeedsAttention,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DownloadQueueItem {
    pub id: String,
    pub source_id: Option<SourceId>,
    pub subject: DownloadSubject,
    pub preview_uris: Vec<String>,
    pub quality: DownloadQuality,
    pub completed_tracks: DownloadCount,
    pub total_tracks: DownloadCount,
    pub state: DownloadQueueState,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DownloadQueueSnapshot {
    pub jobs: Arc<[DownloadQueueItem]>,
    pub downloaded_tracks: usize,
    pub paused: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DownloadFeedbackKind {
    Started,
    Queued,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DownloadFeedback {
    pub subject: DownloadSubject,
    pub preview_uris: Vec<String>,
    pub item_count: usize,
    pub kind: DownloadFeedbackKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DownloadEvent {
    SubjectChanged {
        subject: DownloadSubject,
        downloaded: bool,
    },
    Changed {
        media_uri: String,
        downloaded: bool,
    },
    Queue {
        source_id: Option<SourceId>,
        snapshot: Arc<DownloadQueueSnapshot>,
    },
    Feedback(DownloadFeedback),
    Notice(String),
}

struct Actor {
    artwork: artwork::Artwork,
    root: Arc<PathBuf>,
    database: Database,
    events: DownloadEvents,
    transfers: Arc<TransferClients>,
    peer: sources::PeerAccess,
    prepared_rules: Sender<PreparedRules>,
    attached: HashMap<SourceId, AttachedSource>,
    settings: HashMap<SourceId, SourceDownloadSettings>,
    running_rules: Option<RuleIntent>,
    pending_rules: Option<RuleIntent>,
    jobs: HashMap<Option<SourceId>, Vec<DownloadJob>>,
    paused: bool,
    next_job: u64,
}

impl Downloads {
    pub fn peer_access(&self) -> sources::PeerAccess {
        self.peer.clone()
    }

    pub fn default_directory(&self) -> &Path {
        &self.root
    }

    pub fn new(
        artwork: artwork::Artwork,
        root: PathBuf,
        database: Database,
        runtime: tokio::runtime::Handle,
        events: Sender<DownloadEvent>,
        settings: Vec<SourceDownloadSettings>,
    ) -> Self {
        let (commands, receiver) = async_channel::unbounded();
        let (prepared_rules, rule_results) = async_channel::unbounded();
        let (status, changes) = tokio::sync::watch::channel(DownloadStatus::default());
        let downloads = Self {
            root: Arc::new(root),
            commands,
            status: changes,
            peer: sources::PeerAccess::default(),
        };
        runtime.spawn(run(
            Actor {
                artwork,
                root: Arc::clone(&downloads.root),
                database,
                events: DownloadEvents {
                    sender: events,
                    status,
                },
                transfers: Arc::new(TransferClients::default()),
                peer: downloads.peer.clone(),
                prepared_rules,
                attached: HashMap::new(),
                settings: settings
                    .into_iter()
                    .map(|settings| (settings.source_id.clone(), settings))
                    .collect(),
                running_rules: None,
                pending_rules: None,
                jobs: HashMap::new(),
                paused: false,
                next_job: 0,
            },
            receiver,
            rule_results,
        ));
        downloads
    }

    pub async fn attach(
        &self,
        source_id: SourceId,
        source_key: SourceKey,
        source: Option<Arc<Source>>,
        folder: Option<FolderKey>,
        input_digest: [u8; 32],
    ) -> Result<(), String> {
        let (response, result) = async_channel::bounded(1);
        self.commands
            .send(Command::Attach {
                source_id,
                source_key,
                source: source.as_ref().map(Arc::downgrade),
                folder,
                input_digest,
                response,
            })
            .await
            .map_err(|_| "download operation lane is unavailable".to_string())?;
        result
            .recv()
            .await
            .map_err(|_| "download attachment did not finish".to_string())?
    }

    pub fn download(&self, subject: DownloadSubject, media_uris: Vec<String>) {
        self.send(Command::Download {
            subject,
            media_uris,
        });
    }

    pub fn remove(&self, media_uris: Vec<String>, notify: bool) {
        self.send(Command::Remove { media_uris, notify });
    }

    pub fn connect_changed(&self) {
        self.send(Command::ConnectChanged);
    }

    pub fn library_changed(&self, source_id: SourceId) {
        self.send(Command::LibraryChanged { source_id });
    }

    pub fn settings_changed(&self, settings: Vec<SourceDownloadSettings>) {
        self.send(Command::SettingsChanged(settings));
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Downloads {
    pub fn remove_rule(&self, source_id: SourceId, rule: DownloadRule, delete_downloads: bool) {
        self.send(Command::RemoveRule {
            source_id,
            rule,
            delete_downloads,
        });
    }
}

impl Downloads {
    pub fn cancel(&self, source_id: SourceId, job_id: String) {
        self.cancel_job(Some(source_id), job_id);
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Downloads {
    pub fn cancel_job(&self, source_id: Option<SourceId>, job_id: String) {
        self.send(Command::Cancel { source_id, job_id });
    }
}

impl Downloads {
    pub fn clear_job(&self, source_id: SourceId, job_id: String) {
        self.send(Command::ClearJob { source_id, job_id });
    }

    pub async fn shutdown(&self) -> Result<(), String> {
        let _suspended = self.suspend().await?;
        self.commands.close();
        Ok(())
    }

    pub async fn suspend(&self) -> Result<DownloadSuspension, String> {
        let (ready, completed) = async_channel::bounded(1);
        let (resume, release) = async_channel::bounded(1);
        self.commands
            .send(Command::Suspend {
                ready,
                resume: release,
            })
            .await
            .map_err(|error| error.to_string())?;
        completed.recv().await.map_err(|error| error.to_string())?;
        Ok(DownloadSuspension(resume))
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Downloads {
    pub fn set_paused(&self, paused: bool) {
        self.send(Command::SetPaused(paused));
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Downloads {
    pub fn move_job(
        &self,
        source_id: SourceId,
        job_id: String,
        target_job_id: String,
        after: bool,
    ) {
        self.send(Command::Move {
            source_id,
            job_id,
            target_job_id,
            after,
        });
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Downloads {
    pub fn clear(&self, source_id: SourceId, notify: bool) {
        self.send(Command::Clear { source_id, notify });
    }
}

impl Downloads {
    fn send(&self, command: Command) {
        if self.commands.try_send(command).is_err() {
            warn!("download operation lane is unavailable");
        }
    }
}

async fn run(mut actor: Actor, receiver: Receiver<Command>, rule_results: Receiver<PreparedRules>) {
    if let Err(error) = migrate_download_records(&actor.root, &actor.database, None, None).await {
        warn!(%error, "could not restore direct download access");
    }
    match load_direct_queue(&actor.root, &actor.database, &actor.settings).await {
        Ok(jobs) if !jobs.is_empty() => {
            actor.jobs.insert(None, jobs);
            actor.publish(None).await;
        }
        Ok(_) => {}
        Err(error) => warn!(%error, "could not load the direct download queue"),
    }
    let mut retry = tokio::time::interval(RETRY_INTERVAL);
    retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut active = Vec::new();
    loop {
        if receiver.is_closed() {
            actor.abort_matching(&mut active, true, |_| true).await;
            break;
        }
        if let Ok(command) = receiver.try_recv() {
            actor.apply(command, &mut active).await;
            continue;
        }
        if let Ok(prepared) = rule_results.try_recv() {
            actor.apply_prepared_rules(prepared, &mut active).await;
            continue;
        }
        actor.fill_slots(&mut active).await;
        if !active.is_empty() {
            tokio::select! {
                command = receiver.recv() => {
                    let Ok(command) = command else {
                        actor.abort_matching(&mut active, true, |_| true).await;
                        break;
                    };
                    actor.apply(command, &mut active).await;
                }
                Ok(prepared) = rule_results.recv() => {
                    actor.apply_prepared_rules(prepared, &mut active).await;
                }
                (index, result) = wait_for_finished(&mut active) => {
                    let current = active.swap_remove(index);
                    actor.finish(current, result, &mut active).await;
                }
                _ = retry.tick() => actor.retry_waiting(),
            }
            continue;
        }
        tokio::select! {
            command = receiver.recv() => {
                let Ok(command) = command else {
                    break;
                };
                actor.apply(command, &mut active).await;
            }
            Ok(prepared) = rule_results.recv() => {
                actor.apply_prepared_rules(prepared, &mut active).await;
            }
            _ = retry.tick() => {
                actor.retry_waiting();
            }
        }
    }
}

async fn wait_for_finished(
    active: &mut [ActiveDownload],
) -> (
    usize,
    Result<Result<(), DownloadFailure>, tokio::task::JoinError>,
) {
    std::future::poll_fn(|context| {
        for (index, download) in active.iter_mut().enumerate() {
            if let Poll::Ready(result) = Pin::new(&mut download.task).poll(context) {
                return Poll::Ready((index, result));
            }
        }
        Poll::Pending
    })
    .await
}

impl Actor {
    async fn apply(&mut self, command: Command, active: &mut Vec<ActiveDownload>) {
        match command {
            Command::ClearArtworkCache(response) => {
                let active_uris: Vec<_> = active
                    .iter()
                    .map(|download| download.media_uri.clone())
                    .collect();
                let result = self
                    .artwork
                    .clear_cache(&self.database, &active_uris)
                    .await
                    .map_err(|error| error.to_string());
                let _ = response.send(result).await;
            }
            Command::ClearLyricsCache(response) => {
                let active_uris: Vec<_> = active
                    .iter()
                    .map(|download| download.media_uri.clone())
                    .collect();
                let result = self
                    .database
                    .clear_lyrics_cache(&active_uris)
                    .await
                    .map_err(|error| error.to_string());
                let _ = response.send(result).await;
            }
            Command::Page {
                offset,
                limit,
                response,
            } => {
                let _ = response.send(self.queue_page(offset, limit).await).await;
            }
            Command::Attach {
                source_id,
                source_key,
                source,
                folder,
                input_digest,
                response,
            } => {
                let directory = self.settings_for(&source_id).directory;
                let directory_changed = self
                    .attached
                    .get(&source_id)
                    .is_some_and(|attached| attached.directory != directory);
                let source_changed = self
                    .attached
                    .get(&source_id)
                    .is_some_and(|attached| !same_weak_target(&attached.source, &source));
                self.discard_matching(active, !directory_changed, |download| {
                    download.source_id.as_ref() == Some(&source_id)
                        && (source_changed || directory_changed)
                })
                .await;
                let result = self
                    .attach(
                        source_id.clone(),
                        source_key,
                        source,
                        folder,
                        directory,
                        input_digest,
                    )
                    .await;
                let ready = result.is_ok();
                self.pending_rules = None;
                let _ = response.send(result).await;
                if ready {
                    self.reconcile_all_rules(&source_id);
                }
            }
            Command::Download {
                subject,
                media_uris,
            } => {
                let mut skipped = false;
                let mut grouped = HashMap::<Option<SourceId>, Vec<String>>::new();
                for media_uri in media_uris {
                    grouped
                        .entry(media_source_id(&self.database, &media_uri).await)
                        .or_default()
                        .push(media_uri);
                }
                for (source_id, media_uris) in grouped {
                    if source_id
                        .as_ref()
                        .is_some_and(|source_id| !self.attached.contains_key(source_id))
                    {
                        warn!(source_id=?source_id, "ignored a download for an unattached source");
                        skipped = true;
                        continue;
                    }
                    let quality = source_id
                        .as_ref()
                        .map_or(DownloadQuality::Original, |source_id| {
                            self.settings_for(source_id).quality
                        });
                    if let Some(feedback) = self
                        .enqueue(source_id, subject.clone(), quality, media_uris, false)
                        .await
                    {
                        let _ = self.events.send(DownloadEvent::Feedback(feedback)).await;
                    }
                }
                if skipped {
                    self.mark_subject_incomplete(&subject).await;
                } else {
                    self.publish_subject_complete(&subject);
                }
            }
            Command::ConnectChanged => {
                self.retry_waiting();
            }
            Command::Remove { media_uris, notify } => {
                let mut grouped = HashMap::<Option<SourceId>, Vec<String>>::new();
                for media_uri in media_uris {
                    grouped
                        .entry(media_source_id(&self.database, &media_uri).await)
                        .or_default()
                        .push(media_uri);
                }
                for (source_id, media_uris) in grouped {
                    let remove = media_uris.iter().collect::<HashSet<_>>();
                    self.abort_matching(active, false, |download| {
                        download.source_id == source_id && remove.contains(&download.media_uri)
                    })
                    .await;
                    self.force_remove(source_id.as_ref(), media_uris, notify)
                        .await;
                }
            }
            Command::LibraryChanged { source_id } => {
                self.reconcile_all_rules(&source_id);
            }
            Command::SettingsChanged(settings) => {
                self.apply_settings(settings, active).await;
            }
            Command::RemoveRule {
                source_id,
                rule,
                delete_downloads,
            } => {
                self.abort_matching(active, true, |download| {
                    download.source_id.as_ref() == Some(&source_id)
                        && download.subject == DownloadSubject::Rule(rule)
                })
                .await;
                self.remove_rule(&source_id, rule, delete_downloads).await;
            }
            Command::Cancel { source_id, job_id } => {
                self.cancel(source_id.as_ref(), &job_id, active).await;
            }
            Command::ClearJob { source_id, job_id } => {
                self.clear_job(&source_id, &job_id, active).await;
            }
            Command::Suspend { ready, resume } => {
                self.abort_matching(active, true, |_| true).await;
                if ready.send(()).await.is_ok() {
                    let _ = resume.recv().await;
                }
            }
            Command::SetPaused(paused) => {
                if self.paused == paused {
                    return;
                }
                self.paused = paused;
                if paused {
                    self.abort_matching(active, true, |_| true).await;
                }
                self.publish_all().await;
            }
            Command::Move {
                source_id,
                job_id,
                target_job_id,
                after,
            } => {
                self.move_job(&source_id, &job_id, &target_job_id, after)
                    .await;
            }
            Command::Clear { source_id, notify } => {
                if let Some(settings) = self.settings.get_mut(&source_id) {
                    settings.rules = DownloadRules::default();
                }
                self.reconcile_all_rules(&source_id);
                self.abort_matching(active, false, |download| {
                    download.source_id.as_ref() == Some(&source_id)
                })
                .await;
                self.clear(&source_id, notify).await;
            }
        }
    }

    fn settings_for(&self, source_id: &SourceId) -> SourceDownloadSettings {
        self.settings
            .get(source_id)
            .cloned()
            .unwrap_or_else(|| SourceDownloadSettings::for_source(source_id.clone()))
    }

    async fn apply_settings(
        &mut self,
        settings: Vec<SourceDownloadSettings>,
        active: &mut Vec<ActiveDownload>,
    ) {
        let settings = settings
            .into_iter()
            .map(|settings| (settings.source_id.clone(), settings))
            .collect::<HashMap<_, _>>();
        let changed_quality = self
            .settings
            .keys()
            .chain(settings.keys())
            .collect::<HashSet<_>>()
            .into_iter()
            .filter_map(|id| {
                let quality = settings
                    .get(id)
                    .map_or(DownloadQuality::Original, |setting| setting.quality);
                (self.settings_for(id).quality != quality).then(|| (id.clone(), quality))
            })
            .collect::<Vec<_>>();
        self.settings = settings;
        let source_ids = self.attached.keys().cloned().collect::<Vec<_>>();
        for source_id in &source_ids {
            let directory = self
                .settings
                .get(source_id)
                .and_then(|settings| settings.directory.clone());
            if self
                .attached
                .get(source_id)
                .is_some_and(|attached| attached.directory != directory)
            {
                self.abort_matching(active, false, |download| {
                    download.source_id.as_ref() == Some(source_id)
                })
                .await;
                self.discard_previous_directory(source_id, &directory).await;
                if let Some(attached) = self.attached.get_mut(source_id) {
                    attached.directory = directory;
                }
            }
        }
        for (source_id, quality) in changed_quality {
            self.abort_matching(active, true, |download| {
                download.source_id.as_ref() == Some(&source_id) && download.quality != quality
            })
            .await;
            if let Some(jobs) = self.jobs.get_mut(&Some(source_id.clone())) {
                for job in jobs {
                    job.quality = quality;
                    job.replace_unknown = true;
                    job.remaining.append(&mut job.completed);
                    job.failed = false;
                    job.state = DownloadQueueState::Queued;
                }
            }
            let mut after = (String::new(), String::new());
            loop {
                let records = match self
                    .database
                    .download_files_page(Some(source_id.as_str()), &after, None)
                    .await
                {
                    Ok(records) if records.is_empty() => break,
                    Ok(records) => records,
                    Err(error) => {
                        warn!(%error, "could not read downloads for a quality change");
                        break;
                    }
                };
                let last = records.last().unwrap();
                after = (last.media_uri.clone(), last.encoding.clone());
                let mut replacements: HashMap<DownloadSubject, Vec<String>> = HashMap::new();
                for record in records {
                    let Ok(owners) = download_owners(&record) else {
                        continue;
                    };
                    for owner in owners {
                        let subject = match owner {
                            DownloadOwner::Subject(subject) => subject,
                            DownloadOwner::Retained => DownloadSubject::Prepared {
                                context_id: "retained-downloads".into(),
                                title: None,
                            },
                        };
                        replacements
                            .entry(subject)
                            .or_default()
                            .push(record.media_uri.clone());
                    }
                }
                for (subject, uris) in replacements {
                    self.enqueue(Some(source_id.clone()), subject, quality, uris, true)
                        .await;
                }
            }
        }
        for source_id in source_ids {
            self.reconcile_all_rules(&source_id);
        }
    }

    fn reconcile_all_rules(&mut self, source_id: &SourceId) {
        let rules = self.settings_for(source_id).rules;
        self.schedule_rules(source_id, rules, true);
    }

    fn schedule_rules(&mut self, source_id: &SourceId, rules: DownloadRules, authoritative: bool) {
        let Some(attached) = self.attached.get(source_id).cloned() else {
            return;
        };
        let mut intent = RuleIntent {
            database: self.database.clone(),
            source_key: attached.source_key,
            folder: attached.folder,
            rules,
        };
        if let Some(pending) = self.pending_rules.as_mut() {
            if authoritative || !pending.same_context(&intent) {
                *pending = intent;
            } else {
                for rule in intent.rules.active() {
                    pending.rules.set(rule, true);
                }
            }
        } else {
            if !authoritative
                && let Some(running) = self.running_rules.as_ref()
                && running.source_key == attached.source_key
                && running.same_context(&intent)
            {
                for rule in running.rules.active() {
                    intent.rules.set(rule, true);
                }
            }
            self.pending_rules = Some(intent);
        }
        self.start_rule_preparation();
    }

    fn start_rule_preparation(&mut self) {
        if self.running_rules.is_some() {
            return;
        }
        let Some(intent) = self
            .pending_rules
            .take()
            .filter(|intent| !intent.rules.is_empty())
        else {
            return;
        };
        self.running_rules = Some(intent.clone());
        prepare_rules(intent, self.prepared_rules.clone());
    }

    async fn apply_prepared_rules(
        &mut self,
        prepared: PreparedRules,
        active: &mut Vec<ActiveDownload>,
    ) {
        let Some(intent) = self.running_rules.take() else {
            return;
        };
        let Some(source_id) = self.attached.iter().find_map(|(id, attached)| {
            (attached.source_key == intent.source_key).then(|| id.clone())
        }) else {
            self.start_rule_preparation();
            return;
        };
        let superseded = self.pending_rules.is_some();
        let same_context = self.attached[&source_id].folder == intent.folder;
        if !superseded && same_context {
            match prepared {
                Ok(prepared) => {
                    let quality = self.settings_for(&source_id).quality;
                    for (rule, media_uris) in prepared {
                        self.reconcile_rule(source_id.clone(), rule, quality, media_uris, active)
                            .await;
                    }
                }
                Err(error) => {
                    warn!(%error, %source_id, "could not prepare automatic downloads");
                }
            }
        }
        self.start_rule_preparation();
    }

    async fn attach(
        &mut self,
        source_id: SourceId,
        source_key: SourceKey,
        source: Option<Weak<Source>>,
        folder: Option<FolderKey>,
        directory: Option<DownloadDirectory>,
        input_digest: [u8; 32],
    ) -> Result<(), String> {
        let unchanged = self.attached.get(&source_id).is_some_and(|attached| {
            attached.directory == directory
                && attached.source_key == source_key
                && same_weak_target(&attached.source, &source)
        });
        self.discard_previous_directory(&source_id, &directory)
            .await;
        self.attached.insert(
            source_id.clone(),
            AttachedSource {
                source_key,
                input_digest,
                source: source.clone(),
                folder,
                directory: directory.clone(),
            },
        );
        let group = Some(source_id.clone());
        if unchanged && self.jobs.contains_key(&group) {
            self.retry_waiting();
            self.publish(Some(&source_id)).await;
            return Ok(());
        }
        let mut attachment_error = None;
        if let Err(error) = migrate_download_records(
            &self.root,
            &self.database,
            Some((source_key, &source_id)),
            directory.as_ref(),
        )
        .await
        {
            attachment_error = Some(error);
        }
        let source_available = source.as_ref().and_then(Weak::upgrade).is_some();
        let mut jobs = if let Some(jobs) = self.jobs.get(&group) {
            jobs.clone()
        } else {
            match load_queue(
                &self.root,
                &source_id,
                &self.database,
                source_key,
                directory.as_ref(),
            )
            .await
            {
                Ok(jobs) => jobs,
                Err(error) => {
                    warn!(%error, %source_id, "could not load the download queue");
                    return Err(error);
                }
            }
        };
        let quality = self.settings_for(&source_id).quality;
        jobs.retain_mut(|job| {
            if job.quality != quality {
                job.quality = quality;
                job.replace_unknown = true;
                job.remaining.append(&mut job.completed);
            }
            job.state = if source_available {
                DownloadQueueState::Queued
            } else {
                DownloadQueueState::WaitingForConnection
            };
            !job.remaining.is_empty()
        });
        let queued_tracks = jobs
            .iter()
            .flat_map(|job| job.remaining.iter().cloned())
            .collect::<HashSet<_>>();
        if let Err(error) = cleanup_staging(
            &self.root,
            Some(&source_id),
            directory.as_ref(),
            &queued_tracks,
        )
        .await
        {
            attachment_error.get_or_insert_with(|| error.to_string());
        }
        self.jobs.insert(group.clone(), jobs);
        self.persist_and_publish(group.as_ref()).await;
        attachment_error.map_or(Ok(()), Err)
    }

    async fn add_download_owner(
        &self,
        source_id: Option<&SourceId>,
        media_uri: &str,
        owner: &DownloadOwner,
        quality: DownloadQuality,
        replace_unknown: bool,
    ) -> Result<bool, String> {
        let automatic = matches!(owner, DownloadOwner::Subject(DownloadSubject::Rule(_)));
        let reference = self
            .database
            .connect_track_reference(media_uri)
            .await
            .map_err(|error| error.to_string())?
            .filter(|reference| {
                reference["local_media"].as_bool() == Some(true)
                    || library::file_media_path(media_uri).is_some()
                    || library::cue_media_parts(media_uri).is_some()
                    || library::document_media_id(media_uri).is_some()
            });
        let encoding = library::media_encoding_key(quality.encoding());
        let previous = self
            .database
            .connect_media_file(media_uri, &encoding)
            .await
            .map_err(|error| error.to_string())?;
        let revision = reference
            .as_ref()
            .map(|reference| reference["revision"].to_string());
        let mut saved = previous
            .as_ref()
            .filter(|file| {
                revision
                    .as_ref()
                    .is_none_or(|revision| *revision == file.revision)
            })
            .cloned();
        if saved.is_none()
            && let Some(revision) = &revision
        {
            saved = self
                .database
                .connect_backing_media_file(media_uri, &encoding, revision)
                .await
                .map_err(|error| error.to_string())?;
        }
        if saved.is_none() && reference.is_none() && !replace_unknown {
            saved = self
                .database
                .connect_media_file(media_uri, "unknown")
                .await
                .map_err(|error| error.to_string())?;
        }
        if saved.is_none()
            && previous.is_none()
            && !automatic
            && quality == DownloadQuality::Original
            && let Some(location) = self
                .peer
                .original_file(&self.database, media_uri)
                .await
                .map_err(|error| error.to_string())?
        {
            let (path, root, relative) = match location {
                library::LocalMediaLocation::File(path) => (
                    reqwest::Url::from_file_path(&path)
                        .map(String::from)
                        .map_err(|_| "Media path must be absolute")?,
                    path.parent()
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .into_owned(),
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                ),
                library::LocalMediaLocation::Document(uri) => (uri.clone(), uri, String::new()),
            };
            saved = Some(library::ConnectMediaFile {
                media_uri: media_uri.into(),
                encoding,
                revision: revision.unwrap_or_default(),
                path,
                storage_root: root,
                relative_path: relative,
                managed: false,
                ..Default::default()
            });
        }
        let saved = match saved {
            Some(file) => {
                let paths = completed_download_paths(&self.root, &file)?;
                paths
                    .audio
                    .inspect()
                    .await?
                    .filter(|metadata| {
                        !metadata.size.is_some_and(|size| {
                            file.completed_size
                                .is_some_and(|expected| size != expected as u64)
                        })
                    })
                    .map(|_| (file, paths))
            }
            None => None,
        };
        if automatic
            && saved.as_ref().is_none_or(|(file, _)| !file.managed)
            && self
                .peer
                .original_file(&self.database, media_uri)
                .await
                .map_err(|error| error.to_string())?
                .is_some()
        {
            return Ok(true);
        }
        let Some((mut file, paths)) = saved else {
            return Ok(false);
        };
        file.media_uri = media_uri.into();
        file.source_id = source_id.map(ToString::to_string);
        let mut owners = download_owners(&file)?;
        if !owners.insert(owner.clone())
            && previous.as_ref().is_some_and(|previous| {
                previous.path == file.path && previous.revision == file.revision
            })
        {
            self.retire_previous_files(&mut file).await?;
            return Ok(true);
        }
        file.owners = serde_json::to_string(&owners).map_err(|error| error.to_string())?;
        let source = source_id
            .and_then(|source| self.attached.get(source))
            .map(|source| source.source_key);
        let access =
            download_access(&self.database, media_uri, &paths, file.completed_size).await?;
        file.storage_root = access.root.clone();
        file.relative_path = access.relative_path.clone();
        file.completed_size = Some(access.size_bytes);
        self.database
            .commit_download(&file, source, &access)
            .await
            .map_err(|error| error.to_string())?;
        if let Some(previous) =
            previous.filter(|old| old.path != file.path || old.revision != file.revision)
        {
            self.retire_receipt(&previous).await?;
        }
        Ok(true)
    }

    async fn enqueue(
        &mut self,
        source_id: Option<SourceId>,
        subject: DownloadSubject,
        quality: DownloadQuality,
        media_uris: Vec<String>,
        replace_unknown: bool,
    ) -> Option<DownloadFeedback> {
        let attached = source_id
            .as_ref()
            .and_then(|source_id| self.attached.get(source_id));
        let source_available = source_id.is_none()
            || attached
                .and_then(|attached| attached.source.as_ref())
                .and_then(Weak::upgrade)
                .is_some();
        let can_start = !self.paused && source_available;

        let mut seen = HashSet::new();
        let media_uris = media_uris
            .into_iter()
            .filter(|media_uri| !media_uri.is_empty() && seen.insert(media_uri.clone()))
            .collect::<Vec<_>>();
        if media_uris.is_empty() {
            return None;
        }

        let owner = DownloadOwner::Subject(subject.clone());
        let mut completed = Vec::new();
        let mut remaining = Vec::new();
        for media_uri in &media_uris {
            match self
                .add_download_owner(
                    source_id.as_ref(),
                    media_uri,
                    &owner,
                    quality,
                    replace_unknown,
                )
                .await
            {
                Ok(true) => completed.push(media_uri.clone()),
                Ok(false) => remaining.push(media_uri.clone()),
                Err(error) => {
                    warn!(%error, source_id=?source_id, %media_uri, "could not update download ownership");
                    remaining.push(media_uri.clone());
                }
            }
        }

        let mut scheduled_tracks = 0usize;
        if !remaining.is_empty() || !completed.is_empty() {
            let jobs = self.jobs.entry(source_id.clone()).or_default();
            if let Some(existing_index) = jobs
                .iter()
                .position(|job| job.subject == subject && job.quality == quality)
            {
                let existing = &mut jobs[existing_index];
                existing.failed = false;
                existing.replace_unknown |= replace_unknown;
                let completed_now = completed.iter().collect::<HashSet<_>>();
                existing
                    .remaining
                    .retain(|media_uri| !completed_now.contains(media_uri));
                let mut known = existing
                    .completed
                    .iter()
                    .chain(&existing.remaining)
                    .cloned()
                    .collect::<HashSet<_>>();
                existing.completed.extend(
                    completed
                        .into_iter()
                        .filter(|media_uri| known.insert(media_uri.clone())),
                );
                let additions = remaining
                    .into_iter()
                    .filter(|media_uri| known.insert(media_uri.clone()))
                    .collect::<Vec<_>>();
                scheduled_tracks = additions.len();
                existing.remaining.extend(additions);
                if existing.state != DownloadQueueState::Downloading {
                    existing.state = if source_available {
                        DownloadQueueState::Queued
                    } else {
                        DownloadQueueState::WaitingForConnection
                    };
                }
                if existing.remaining.is_empty() {
                    jobs.remove(existing_index);
                }
            } else if !remaining.is_empty() {
                scheduled_tracks = remaining.len();
                self.next_job = self.next_job.wrapping_add(1);
                jobs.push(DownloadJob {
                    id: job_id(source_id.as_ref(), &subject, self.next_job),
                    subject: subject.clone(),
                    quality,
                    replace_unknown,
                    completed,
                    failed: false,
                    remaining,
                    state: if source_available {
                        DownloadQueueState::Queued
                    } else {
                        DownloadQueueState::WaitingForConnection
                    },
                });
            }
        }

        self.persist_and_publish(source_id.as_ref()).await;
        (scheduled_tracks > 0).then(|| DownloadFeedback {
            subject,
            preview_uris: media_uris.iter().take(4).cloned().collect(),
            item_count: scheduled_tracks,
            kind: if can_start {
                DownloadFeedbackKind::Started
            } else {
                DownloadFeedbackKind::Queued
            },
        })
    }

    async fn reconcile_rule(
        &mut self,
        source_id: SourceId,
        rule: DownloadRule,
        quality: DownloadQuality,
        media_uris: Vec<String>,
        active: &mut Vec<ActiveDownload>,
    ) {
        let Some(attached) = self.attached.get(&source_id).cloned() else {
            warn!(%source_id, "ignored download reconciliation for an unattached source");
            return;
        };
        let source_available = attached.source.as_ref().and_then(Weak::upgrade).is_some();
        let can_start = !self.paused && source_available;

        let mut seen = HashSet::new();
        let media_uris = media_uris
            .into_iter()
            .filter(|media_uri| seen.insert(media_uri.clone()))
            .collect::<Vec<_>>();
        let desired = media_uris.iter().cloned().collect::<HashSet<_>>();
        let subject = DownloadSubject::Rule(rule);
        let group = Some(source_id.clone());
        let quality_changed = self.jobs.get(&group).is_some_and(|jobs| {
            jobs.iter()
                .any(|job| job.subject == subject && job.quality != quality)
        });
        self.abort_matching(active, true, |download| {
            download.source_id.as_ref() == Some(&source_id)
                && download.subject == subject
                && (quality_changed || !desired.contains(&download.media_uri))
        })
        .await;
        let active_job_id = active
            .iter()
            .find(|download| {
                download.source_id.as_ref() == Some(&source_id) && download.subject == subject
            })
            .map(|download| download.job_id.clone());
        let owner = DownloadOwner::Subject(subject.clone());

        let owner_key = serde_json::to_string(&owner).expect("download ownership can be stored");
        let mut after = (String::new(), String::new());
        loop {
            let records = match self
                .database
                .download_files_page(Some(source_id.as_str()), &after, Some(&owner_key))
                .await
            {
                Ok(records) if records.is_empty() => break,
                Ok(records) => records,
                Err(error) => {
                    warn!(%error, %source_id, "could not read rule downloads");
                    return;
                }
            };
            let last = records.last().unwrap();
            after = (last.media_uri.clone(), last.encoding.clone());
            for mut record in records {
                if desired.contains(&record.media_uri) {
                    continue;
                }
                let Ok(mut owners) = download_owners(&record) else {
                    continue;
                };
                owners.remove(&owner);
                owners.extend(self.queued_owners_for_track(
                    &source_id,
                    &record.media_uri,
                    Some(&subject),
                ));
                if let Err(error) = self.store_owners(&mut record, owners).await {
                    warn!(%error, %source_id, media_uri=%record.media_uri, "could not update rule ownership");
                }
            }
        }

        let replace_unknown = self.jobs.get(&Some(source_id.clone())).is_some_and(|jobs| {
            jobs.iter()
                .any(|job| job.subject == subject && job.replace_unknown)
        });
        let mut completed = Vec::new();
        let mut remaining = Vec::new();
        for media_uri in &media_uris {
            match self
                .add_download_owner(
                    Some(&source_id),
                    media_uri,
                    &owner,
                    quality,
                    replace_unknown,
                )
                .await
            {
                Ok(true) => completed.push(media_uri.clone()),
                Ok(false) => remaining.push(media_uri.clone()),
                Err(error) => {
                    warn!(%error, %source_id, %media_uri, "could not update download ownership");
                    remaining.push(media_uri.clone());
                }
            }
        }

        let jobs = self.jobs.entry(group).or_default();
        let existing_index = jobs.iter().position(|job| job.subject == subject);
        let existing_id = existing_index.map(|index| jobs[index].id.clone());
        let old_remaining = jobs
            .iter()
            .filter(|job| job.subject == subject)
            .flat_map(|job| job.remaining.iter().cloned())
            .collect::<HashSet<_>>();
        jobs.retain(|job| job.subject != subject);

        let scheduled_tracks = remaining
            .iter()
            .filter(|media_uri| quality_changed || !old_remaining.contains(*media_uri))
            .count();
        if !remaining.is_empty() {
            let id = existing_id.unwrap_or_else(|| {
                self.next_job = self.next_job.wrapping_add(1);
                job_id(Some(&source_id), &subject, self.next_job)
            });
            let state = if active_job_id.as_deref() == Some(id.as_str()) {
                DownloadQueueState::Downloading
            } else if source_available {
                DownloadQueueState::Queued
            } else {
                DownloadQueueState::WaitingForConnection
            };
            let job = DownloadJob {
                id,
                subject: subject.clone(),
                quality,
                replace_unknown,
                completed,
                failed: false,
                remaining,
                state,
            };
            jobs.insert(existing_index.unwrap_or(jobs.len()).min(jobs.len()), job);
        }
        self.reconcile_staging(&Some(source_id.clone())).await;

        self.persist_and_publish(Some(&source_id)).await;
        if scheduled_tracks > 0 {
            let _ = self
                .events
                .send(DownloadEvent::Feedback(DownloadFeedback {
                    subject,
                    preview_uris: media_uris.iter().take(4).cloned().collect(),
                    item_count: scheduled_tracks,
                    kind: if can_start {
                        DownloadFeedbackKind::Started
                    } else {
                        DownloadFeedbackKind::Queued
                    },
                }))
                .await;
        }
    }

    async fn fill_slots(&mut self, active: &mut Vec<ActiveDownload>) {
        if self.paused {
            return;
        }
        while active.len() < MAX_ACTIVE_DOWNLOADS {
            let Some(download) = self.start_next(active).await else {
                break;
            };
            active.push(download);
        }
    }

    async fn start_next(&mut self, active: &[ActiveDownload]) -> Option<ActiveDownload> {
        loop {
            let (source_id, job_id, subject, quality, media_uri, state) =
                self.next_candidate(active)?;
            let attached = source_id
                .as_ref()
                .and_then(|source_id| self.attached.get(source_id))
                .cloned();
            let source = attached
                .as_ref()
                .and_then(|attached| attached.source.as_ref())
                .and_then(Weak::upgrade);
            if source_id.is_some() && source.is_none() {
                if let Some(job) = self.find_job_mut(&source_id, &job_id) {
                    job.state = DownloadQueueState::WaitingForConnection;
                }
                self.persist_and_publish(source_id.as_ref()).await;
                continue;
            }
            let replace_unknown = self
                .find_job_mut(&source_id, &job_id)
                .is_some_and(|job| job.replace_unknown);
            let custom_directory = attached
                .as_ref()
                .and_then(|attached| attached.directory.as_ref());
            let owner = DownloadOwner::Subject(subject.clone());
            match self
                .add_download_owner(
                    source_id.as_ref(),
                    &media_uri,
                    &owner,
                    quality,
                    replace_unknown,
                )
                .await
            {
                Ok(true) => {
                    self.remove_job_track(&source_id, &job_id, &media_uri, true);
                    self.persist_and_publish(source_id.as_ref()).await;
                    continue;
                }
                Ok(false) => {}
                Err(error) => {
                    warn!(%error, source_id=?source_id, %media_uri, "could not update download ownership");
                    if let Some(job) = self.find_job_mut(&source_id, &job_id) {
                        job.state = DownloadQueueState::NeedsAttention;
                    }
                    self.persist_and_publish(source_id.as_ref()).await;
                    continue;
                }
            }
            let peer_reference = self
                .database
                .connect_track_reference(&media_uri)
                .await
                .ok()
                .flatten()
                .filter(|reference| {
                    reference["local_media"].as_bool() == Some(true)
                        || library::file_media_path(&media_uri).is_some()
                        || library::cue_media_parts(&media_uri).is_some()
                        || library::document_media_id(&media_uri).is_some()
                });
            let resolved = if peer_reference.is_some() {
                self.peer
                    .download(&self.database, &media_uri, quality)
                    .await
            } else if let Some(source) = source.as_ref() {
                source
                    .resolve_download(&self.database, &media_uri, quality)
                    .await
            } else {
                Some(media_uri.as_str())
                    .filter(|uri| uri.starts_with("http://") || uri.starts_with("https://"))
                    .map(|uri| ResolvedDownload::Ready {
                        stream: ResolvedStream::new(uri),
                        codec: None,
                    })
                    .ok_or(SourceError::InvalidRequest(
                        "Direct media is not downloadable",
                    ))
            };
            let transcoded_extension = match &resolved {
                Ok(ResolvedDownload::Ready { codec, .. }) => codec.map(|codec| codec.extension()),
                Ok(ResolvedDownload::Encode { encoding, .. }) => Some(encoding.codec.extension()),
                Err(_) => None,
            };
            let metadata = self
                .database
                .download_metadata(&media_uri)
                .await
                .ok()
                .flatten();
            let mut paths = new_download_paths(
                &self.root,
                source_id.as_ref(),
                &media_uri,
                metadata.as_ref(),
                custom_directory,
                transcoded_extension,
            );
            if let Some(reference) = &peer_reference {
                let backing = reference["backing_id"].as_str().unwrap_or(&media_uri);
                let revision = reference["revision"].to_string();
                let key = blake3::hash(
                    format!(
                        "{backing}\0{revision}\0{}",
                        library::media_encoding_key(quality.encoding())
                    )
                    .as_bytes(),
                )
                .to_hex()
                .to_string();
                if custom_directory.is_none()
                    && let Ok(Some((root, mut relative))) =
                        self.peer.destination(&self.database, &media_uri).await
                {
                    let previous = self
                        .database
                        .connect_media_file(
                            &media_uri,
                            &library::media_encoding_key(quality.encoding()),
                        )
                        .await
                        .ok()
                        .flatten();
                    if (quality != DownloadQuality::Original
                        || previous.is_some_and(|file| file.revision != revision))
                        && let Some(name) = relative.last_mut()
                    {
                        let extension = quality
                            .extension()
                            .or_else(|| reference["source_format"].as_str())
                            .unwrap_or("audio");
                        *name = format!("{key}.{extension}");
                    }
                    paths.audio = match root {
                        library::LocalMediaLocation::File(root) => DownloadAudio::Native {
                            path: relative
                                .iter()
                                .fold(root.clone(), |path, part| path.join(part)),
                            root: Some(root),
                        },
                        library::LocalMediaLocation::Document(root) => DownloadAudio::Document {
                            root,
                            relative,
                            file: None,
                        },
                    };
                } else if let DownloadAudio::Native { path, .. } = &mut paths.audio {
                    let extension = path.extension().unwrap_or_default().to_string_lossy();
                    *path = path.with_file_name(format!("{key}.{extension}"));
                }
            }
            let entering_download = state != DownloadQueueState::Downloading;
            if let Some(job) = self.find_job_mut(&source_id, &job_id) {
                job.state = DownloadQueueState::Downloading;
            }
            if entering_download {
                self.persist_and_publish(source_id.as_ref()).await;
            } else {
                self.publish(source_id.as_ref()).await;
            }
            let task_paths = paths.clone();
            let transfers = Arc::clone(&self.transfers);
            let (cancellation, cancelled) = tokio::sync::oneshot::channel();
            let revision = Arc::new(Mutex::new(
                peer_reference
                    .as_ref()
                    .map(|reference| reference["revision"].to_string()),
            ));
            let uri = media_uri.clone();
            let database = self.database.clone();
            let artwork = self.artwork.clone();
            let input = attached
                .as_ref()
                .map_or([0; 32], |source| source.input_digest);
            let task_source_id = source_id.clone();
            let peer_assets = peer_reference.is_some().then(|| self.peer.clone());
            let task_revision = Arc::clone(&revision);
            let task = tokio::spawn(async move {
                let mut cancelled = cancelled;
                match resolved.map_err(download_source_failure)? {
                    ResolvedDownload::Encode { stream, encoding } => {
                        encode_download(stream, encoding, &task_paths, &mut cancelled)
                            .await
                            .map_err(download_source_failure)?;
                    }
                    ResolvedDownload::Ready { stream, codec } => {
                        let needs_encoding = match run_transfer(
                            task_source_id.as_ref(),
                            &uri,
                            quality,
                            &stream,
                            &task_paths,
                            &transfers,
                            &mut cancelled,
                        )
                        .await
                        {
                            Ok(revision) => {
                                if let Some(revision) = revision {
                                    *task_revision.lock().unwrap_or_else(|p| p.into_inner()) =
                                        Some(revision);
                                }
                                if let Some(codec) = codec {
                                    let path = task_paths.audio_part.clone();
                                    tokio::task::spawn_blocking(move || {
                                        sources::downloaded_codec(&path)
                                    })
                                    .await
                                    .map_err(|error| {
                                        DownloadFailure::NeedsAttention(error.to_string())
                                    })? != Some(codec)
                                } else {
                                    false
                                }
                            }
                            Err(SourceError::Server {
                                status: 415 | 500 | 501,
                                ..
                            }) if codec.is_some() => true,
                            Err(error) => return Err(download_source_failure(error)),
                        };
                        if needs_encoding {
                            let source = source.as_ref().ok_or_else(|| {
                                DownloadFailure::NeedsAttention(
                                    "The audio format did not match the download profile".into(),
                                )
                            })?;
                            let (ResolvedDownload::Ready {
                                stream: original, ..
                            }
                            | ResolvedDownload::Encode {
                                stream: original, ..
                            }) = source
                                .resolve_download(&database, &uri, DownloadQuality::Original)
                                .await
                                .map_err(download_source_failure)?;
                            let encoding = quality
                                .encoding()
                                .expect("encoded downloads have a profile");
                            encode_download(
                                original.without_window(),
                                encoding,
                                &task_paths,
                                &mut cancelled,
                            )
                            .await
                            .map_err(download_source_failure)?;
                        }
                    }
                }
                // Companion failures do not discard successfully downloaded audio.
                tokio::select! {
                    _ = &mut cancelled => {},
                    _ = cache_track_assets(&database, &artwork, source.as_deref(), peer_assets.as_ref(), &uri, input) => {},
                }
                Ok(())
            });
            return Some(ActiveDownload {
                source_id,
                job_id,
                media_uri,
                subject,
                paths,
                quality,
                cancellation: Some(cancellation),
                task,
                revision,
            });
        }
    }

    fn next_candidate(
        &self,
        active: &[ActiveDownload],
    ) -> Option<(
        Option<SourceId>,
        String,
        DownloadSubject,
        DownloadQuality,
        String,
        DownloadQueueState,
    )> {
        for (source_id, jobs) in &self.jobs {
            for job in jobs {
                if !matches!(
                    job.state,
                    DownloadQueueState::Queued | DownloadQueueState::Downloading
                ) {
                    break;
                }
                let media_uri = job.remaining.iter().find(|media_uri| {
                    !active.iter().any(|download| {
                        download.source_id == *source_id
                            && library::cue_media_parts(&download.media_uri).map_or_else(
                                || download.media_uri.clone(),
                                |(_, backing, _, _)| backing,
                            ) == library::cue_media_parts(media_uri)
                                .map_or_else(|| (*media_uri).clone(), |(_, backing, _, _)| backing)
                    })
                });
                if let Some(media_uri) = media_uri {
                    return Some((
                        source_id.clone(),
                        job.id.clone(),
                        job.subject.clone(),
                        job.quality,
                        media_uri.clone(),
                        job.state,
                    ));
                }
            }
        }
        None
    }

    async fn finish(
        &mut self,
        active: ActiveDownload,
        joined: Result<Result<(), DownloadFailure>, tokio::task::JoinError>,
        remaining_active: &mut Vec<ActiveDownload>,
    ) {
        if self
            .find_job_mut(&active.source_id, &active.job_id)
            .is_none()
        {
            return;
        }
        let ActiveDownload {
            source_id,
            job_id,
            media_uri,
            subject,
            paths,
            quality,
            revision,
            ..
        } = active;
        let result = match joined {
            Ok(Ok(())) => {
                let revision = revision.lock().unwrap_or_else(|p| p.into_inner()).take();
                self.commit_transfer(&source_id, &media_uri, &subject, &paths, quality, revision)
                    .await
                    .map(|_| ())
            }
            Ok(Err(error)) => Err(error),
            Err(error) => Err(DownloadFailure::NeedsAttention(format!(
                "download task failed: {error}"
            ))),
        };
        match result {
            Ok(()) => {
                self.remove_job_track(&source_id, &job_id, &media_uri, true);
            }
            Err(DownloadFailure::Item(error)) => {
                warn!(%error, source_id=?source_id, %media_uri, "could not download track");
                self.mark_subject_incomplete(&subject).await;
                self.remove_job_track(&source_id, &job_id, &media_uri, false);
            }
            Err(DownloadFailure::Retry(error)) => {
                warn!(%error, source_id=?source_id, "download is waiting for the server");
                self.abort_matching(remaining_active, true, |download| {
                    download.source_id == source_id && download.job_id == job_id
                })
                .await;
                if let Some(job) = self.find_job_mut(&source_id, &job_id) {
                    job.state = DownloadQueueState::WaitingForConnection;
                }
            }
            Err(DownloadFailure::NeedsAttention(error)) => {
                warn!(%error, source_id=?source_id, "download needs attention");
                self.abort_matching(remaining_active, true, |download| {
                    download.source_id == source_id && download.job_id == job_id
                })
                .await;
                if let Some(job) = self.find_job_mut(&source_id, &job_id) {
                    job.state = DownloadQueueState::NeedsAttention;
                }
            }
        }
        self.persist_and_publish(source_id.as_ref()).await;
    }

    async fn commit_transfer(
        &self,
        source_id: &Option<SourceId>,
        media_uri: &str,
        subject: &DownloadSubject,
        paths: &DownloadPaths,
        quality: DownloadQuality,
        revision: Option<String>,
    ) -> Result<DownloadPaths, DownloadFailure> {
        let mut file = library::ConnectMediaFile {
            media_uri: media_uri.into(),
            encoding: library::media_encoding_key(quality.encoding()),
            revision: revision.unwrap_or_default(),
            managed: true,
            ..Default::default()
        };
        let previous = self
            .database
            .connect_media_file(media_uri, &file.encoding)
            .await
            .map_err(|error| DownloadFailure::NeedsAttention(error.to_string()))?;
        let finished = finalize_download(paths, previous.as_ref())
            .await
            .map_err(DownloadFailure::NeedsAttention)?;
        let mut owners = previous
            .as_ref()
            .map(download_owners)
            .transpose()
            .map_err(DownloadFailure::NeedsAttention)?
            .unwrap_or_default();
        owners.insert(DownloadOwner::Subject(subject.clone()));
        file.source_id = source_id.as_ref().map(ToString::to_string);
        file.path = finished
            .paths
            .audio
            .access_uri()
            .map_err(DownloadFailure::NeedsAttention)?;
        file.owners = serde_json::to_string(&owners)
            .map_err(|error| DownloadFailure::NeedsAttention(error.to_string()))?;
        file.completed_size = Some(i64::try_from(finished.completed_size).unwrap_or(i64::MAX));
        let mut retired: Vec<String> = previous
            .as_ref()
            .filter(|file| !file.retired_documents.is_empty())
            .map(|file| serde_json::from_str(&file.retired_documents))
            .transpose()
            .map_err(|error| DownloadFailure::NeedsAttention(error.to_string()))?
            .unwrap_or_default();
        if let Some(previous) = previous
            .as_ref()
            .filter(|old| old.managed && old.path != file.path)
        {
            let used = self
                .database
                .connect_media_file_users(previous)
                .await
                .map_err(|error| DownloadFailure::NeedsAttention(error.to_string()))?;
            if !used {
                retired.push(previous.path.clone());
            }
        }
        file.retired_documents = serde_json::to_string(&retired)
            .map_err(|error| DownloadFailure::NeedsAttention(error.to_string()))?;
        let source = source_id
            .as_ref()
            .and_then(|id| self.attached.get(id))
            .map(|source| source.source_key);
        let access = download_access(
            &self.database,
            media_uri,
            &finished.paths,
            file.completed_size,
        )
        .await
        .map_err(DownloadFailure::NeedsAttention)?;
        file.storage_root = access.root.clone();
        file.relative_path = access.relative_path.clone();
        self.database
            .commit_download(&file, source, &access)
            .await
            .map_err(|error| DownloadFailure::NeedsAttention(error.to_string()))?;
        let _ = self
            .events
            .send(DownloadEvent::Changed {
                media_uri: media_uri.into(),
                downloaded: true,
            })
            .await;
        if let Err(error) = self.retire_previous_files(&mut file).await {
            let _ = self.events.send(DownloadEvent::Notice(error)).await;
        }
        Ok(finished.paths)
    }

    async fn retire_previous_files(
        &self,
        file: &mut library::ConnectMediaFile,
    ) -> Result<(), String> {
        let mut retired: Vec<String> = serde_json::from_str(if file.retired_documents.is_empty() {
            "[]"
        } else {
            &file.retired_documents
        })
        .map_err(|error| error.to_string())?;
        while let Some(path) = retired.first() {
            let old = library::ConnectMediaFile {
                path: path.clone(),
                managed: true,
                ..file.clone()
            };
            self.remove_representation_bytes(&old).await?;
            retired.remove(0);
            file.retired_documents =
                serde_json::to_string(&retired).map_err(|error| error.to_string())?;
            self.database
                .connect_save_media_file(file)
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    async fn abort_matching(
        &mut self,
        active: &mut Vec<ActiveDownload>,
        preserve: bool,
        matches: impl Fn(&ActiveDownload) -> bool,
    ) -> Vec<String> {
        self.settle_matching(active, preserve, true, matches).await
    }

    async fn discard_matching(
        &mut self,
        active: &mut Vec<ActiveDownload>,
        preserve: bool,
        matches: impl Fn(&ActiveDownload) -> bool,
    ) {
        self.settle_matching(active, preserve, false, matches).await;
    }

    async fn settle_matching(
        &mut self,
        active: &mut Vec<ActiveDownload>,
        preserve: bool,
        commit_completed: bool,
        matches: impl Fn(&ActiveDownload) -> bool,
    ) -> Vec<String> {
        let mut settling = Vec::new();
        let mut index = 0;
        while index < active.len() {
            if !matches(&active[index]) {
                index += 1;
                continue;
            }
            let mut download = active.swap_remove(index);
            if let Some(cancellation) = download.cancellation.take() {
                let _ = cancellation.send(());
            }
            settling.push(download);
        }

        let mut affected = HashSet::new();
        let mut completed_tracks = Vec::new();
        for download in settling {
            let joined = download.task.await;
            let completed = matches!(joined, Ok(Ok(())));
            if completed && commit_completed {
                let receipt = download
                    .revision
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .take();
                match self
                    .commit_transfer(
                        &download.source_id,
                        &download.media_uri,
                        &download.subject,
                        &download.paths,
                        download.quality,
                        receipt,
                    )
                    .await
                {
                    Ok(_) => {
                        completed_tracks.push(download.media_uri.clone());
                        self.remove_job_track(
                            &download.source_id,
                            &download.job_id,
                            &download.media_uri,
                            true,
                        );
                    }
                    Err(DownloadFailure::NeedsAttention(error)) => {
                        warn!(
                            %error,
                            source_id = ?download.source_id,
                            media_uri = %download.media_uri,
                            "could not finish a completed download"
                        );
                        if let Some(job) = self.find_job_mut(&download.source_id, &download.job_id)
                        {
                            job.state = DownloadQueueState::NeedsAttention;
                        }
                    }
                    Err(_) => unreachable!("download commit failures need attention"),
                }
                continue;
            }
            affected.insert((download.source_id.clone(), download.job_id.clone()));
            if !preserve && let Err(error) = discard_staging(&download.paths).await {
                warn!(
                    %error,
                    source_id = ?download.source_id,
                    media_uri = %download.media_uri,
                    "could not settle an interrupted download"
                );
            }
        }
        for (source_id, job_id) in affected {
            let still_active = active
                .iter()
                .any(|download| download.source_id == source_id && download.job_id == job_id);
            if let Some(job) = self.find_job_mut(&source_id, &job_id) {
                job.state = if still_active {
                    DownloadQueueState::Downloading
                } else {
                    DownloadQueueState::Queued
                };
            }
        }
        completed_tracks
    }

    async fn reconcile_staging(&self, source_id: &Option<SourceId>) {
        let queued = self
            .jobs
            .get(source_id)
            .into_iter()
            .flatten()
            .flat_map(|job| job.remaining.iter().cloned())
            .collect::<HashSet<_>>();
        let directory = source_id
            .as_ref()
            .and_then(|source_id| self.attached.get(source_id))
            .and_then(|attached| attached.directory.as_ref());
        if let Err(error) =
            cleanup_staging(&self.root, source_id.as_ref(), directory, &queued).await
        {
            warn!(%error, source_id=?source_id, "could not reconcile download staging");
        }
    }

    async fn discard_previous_directory(
        &self,
        source_id: &SourceId,
        directory: &Option<DownloadDirectory>,
    ) {
        let Some(attached) = self.attached.get(source_id) else {
            return;
        };
        if attached.directory == *directory {
            return;
        }
        if let Err(error) = cleanup_staging(
            &self.root,
            Some(source_id),
            attached.directory.as_ref(),
            &HashSet::new(),
        )
        .await
        {
            warn!(%error, %source_id, "could not remove download staging from the previous folder");
        }
    }

    fn queued_owners_for_track(
        &self,
        source_id: &SourceId,
        identity: &str,
        excluded_subject: Option<&DownloadSubject>,
    ) -> HashSet<DownloadOwner> {
        self.jobs
            .get(&Some(source_id.clone()))
            .into_iter()
            .flatten()
            .filter(|job| excluded_subject != Some(&job.subject))
            .filter(|job| job.remaining.iter().any(|media_uri| media_uri == identity))
            .map(|job| DownloadOwner::Subject(job.subject.clone()))
            .collect()
    }

    fn find_job_mut(
        &mut self,
        source_id: &Option<SourceId>,
        job_id: &str,
    ) -> Option<&mut DownloadJob> {
        self.jobs
            .get_mut(source_id)?
            .iter_mut()
            .find(|job| job.id == job_id)
    }

    fn remove_job_track(
        &mut self,
        source_id: &Option<SourceId>,
        job_id: &str,
        media_uri: &str,
        completed: bool,
    ) {
        let Some(jobs) = self.jobs.get_mut(source_id) else {
            return;
        };
        let Some(job_index) = jobs.iter().position(|job| job.id == job_id) else {
            return;
        };
        let job = &mut jobs[job_index];
        job.remaining.retain(|candidate| candidate != media_uri);
        if completed {
            job.completed.push(media_uri.to_string());
        }
        if job.remaining.is_empty() {
            let job = jobs.remove(job_index);
            if completed && !job.failed {
                self.publish_subject_complete(&job.subject);
            }
        } else {
            job.state = DownloadQueueState::Downloading;
        }
    }

    fn publish_subject_complete(&self, subject: &DownloadSubject) {
        if !self
            .jobs
            .values()
            .flatten()
            .any(|job| &job.subject == subject)
        {
            let _ = self.events.try_send(DownloadEvent::SubjectChanged {
                subject: subject.clone(),
                downloaded: true,
            });
        }
    }

    async fn mark_subject_incomplete(&mut self, subject: &DownloadSubject) {
        for (source, jobs) in &mut self.jobs {
            let mut changed = false;
            for job in jobs
                .iter_mut()
                .filter(|job| &job.subject == subject && !job.failed)
            {
                job.failed = true;
                changed = true;
            }
            if changed && let Err(error) = persist_queue(&self.root, source.as_ref(), jobs).await {
                warn!(%error, "could not save download progress");
            }
        }
        let _ = self.events.try_send(DownloadEvent::SubjectChanged {
            subject: subject.clone(),
            downloaded: false,
        });
    }

    fn retry_waiting(&mut self) {
        for (source_id, jobs) in &mut self.jobs {
            let available = source_id.as_ref().is_none_or(|source_id| {
                self.attached.get(source_id).is_some_and(|attached| {
                    attached
                        .source
                        .as_ref()
                        .is_some_and(|source| source.strong_count() > 0)
                })
            });
            if let Some(first) = jobs.first_mut()
                && available
                && first.state == DownloadQueueState::WaitingForConnection
            {
                first.state = DownloadQueueState::Queued;
            }
        }
    }

    async fn force_remove(
        &mut self,
        source_id: Option<&SourceId>,
        media_uris: Vec<String>,
        notify: bool,
    ) {
        let group = source_id.cloned();
        let remove = media_uris.iter().collect::<HashSet<_>>();
        let subjects = self
            .jobs
            .get(&group)
            .into_iter()
            .flatten()
            .filter(|job| {
                job.remaining
                    .iter()
                    .chain(&job.completed)
                    .any(|uri| remove.contains(uri))
            })
            .map(|job| job.subject.clone())
            .collect::<HashSet<_>>();
        for subject in subjects {
            self.mark_subject_incomplete(&subject).await;
        }
        for job in self.jobs.entry(group.clone()).or_default().iter_mut() {
            job.remaining
                .retain(|media_uri| !remove.contains(media_uri));
            job.completed
                .retain(|media_uri| !remove.contains(media_uri));
        }
        self.jobs
            .entry(group.clone())
            .or_default()
            .retain(|job| !job.remaining.is_empty());
        self.reconcile_staging(&group).await;

        let (removed, failed) = self.delete_downloads(source_id, media_uris).await;
        self.persist_and_publish(source_id).await;
        if notify {
            self.send_removal_notice(removed, failed).await;
        }
    }

    async fn delete_downloads(
        &self,
        _source_id: Option<&SourceId>,
        media_uris: impl IntoIterator<Item = String>,
    ) -> (usize, usize) {
        let mut removed = 0;
        let mut failed = 0;
        for uri in media_uris {
            match self.database.connect_media_files(&uri).await {
                Ok(files) => {
                    for file in files {
                        match self.retire_receipt(&file).await {
                            Ok(count) => removed += count,
                            Err(error) => {
                                failed += 1;
                                warn!(%error, %uri, "could not remove downloaded media");
                            }
                        }
                    }
                }
                Err(error) => {
                    failed += 1;
                    warn!(%error, %uri, "could not read downloaded media");
                }
            }
        }
        (removed, failed)
    }

    async fn remove_representation_bytes(
        &self,
        file: &library::ConnectMediaFile,
    ) -> Result<usize, String> {
        let used = self
            .database
            .connect_media_file_users(file)
            .await
            .map_err(|error| error.to_string())?;
        if !file.managed || used {
            return Ok(0);
        }
        let paths = completed_download_paths(&self.root, file)?;
        remove_download_files(&paths).await.map(usize::from)
    }

    async fn retire_receipt(&self, file: &library::ConnectMediaFile) -> Result<usize, String> {
        let mut file = file.clone();
        self.retire_previous_files(&mut file).await?;
        let removed = self.remove_representation_bytes(&file).await?;
        self.database
            .connect_forget_media_file(&file)
            .await
            .map_err(|error| error.to_string())?;
        let downloaded = !self
            .database
            .connect_media_files(&file.media_uri)
            .await
            .map_err(|error| error.to_string())?
            .is_empty();
        let _ = self
            .events
            .send(DownloadEvent::Changed {
                media_uri: file.media_uri.clone(),
                downloaded,
            })
            .await;
        for owner in download_owners(&file)? {
            if let DownloadOwner::Subject(subject) = owner {
                let _ = self.events.try_send(DownloadEvent::SubjectChanged {
                    subject,
                    downloaded: false,
                });
            }
        }
        Ok(removed)
    }

    async fn send_removal_notice(&self, removed: usize, failed: usize) {
        let message = match (removed, failed) {
            (0, 0) => "This track is not downloaded".to_string(),
            (1, 0) => "Removed 1 download".to_string(),
            (count, 0) => format!("Removed {count} downloads"),
            (_, failed) => format!("Could not remove {failed} downloads"),
        };
        let _ = self.events.send(DownloadEvent::Notice(message)).await;
    }

    async fn remove_rule(
        &mut self,
        source_id: &SourceId,
        rule: DownloadRule,
        delete_downloads: bool,
    ) {
        let subject = DownloadSubject::Rule(rule);
        let group = Some(source_id.clone());
        self.jobs
            .entry(group.clone())
            .or_default()
            .retain(|job| job.subject != subject);
        self.reconcile_staging(&group).await;
        self.release_owner(source_id, &subject, None, !delete_downloads)
            .await;
        self.persist_and_publish(Some(source_id)).await;
    }

    async fn cancel(
        &mut self,
        source_id: Option<&SourceId>,
        job_id: &str,
        active: &mut Vec<ActiveDownload>,
    ) {
        let group = source_id.cloned();
        let subject = self
            .jobs
            .get(&group)
            .and_then(|jobs| jobs.iter().find(|job| job.id == job_id))
            .map(|job| job.subject.clone());
        let Some(subject) = subject else { return };
        self.mark_subject_incomplete(&subject).await;
        self.abort_matching(active, true, |download| {
            download.source_id.as_ref() == source_id && download.job_id == job_id
        })
        .await;
        self.jobs
            .entry(group.clone())
            .or_default()
            .retain(|job| job.id != job_id);
        self.reconcile_staging(&group).await;
        self.persist_and_publish(source_id).await;
    }

    async fn clear_job(
        &mut self,
        source_id: &SourceId,
        job_id: &str,
        active: &mut Vec<ActiveDownload>,
    ) {
        let group = Some(source_id.clone());
        let Some(job) = self
            .jobs
            .get(&group)
            .and_then(|jobs| jobs.iter().find(|job| job.id == job_id))
            .cloned()
        else {
            return;
        };
        self.mark_subject_incomplete(&job.subject).await;
        let completed = job
            .completed
            .into_iter()
            .chain(
                self.abort_matching(active, true, |download| {
                    download.source_id.as_ref() == Some(source_id) && download.job_id == job_id
                })
                .await,
            )
            .collect::<HashSet<_>>();
        let subject = job.subject;
        self.jobs
            .entry(group.clone())
            .or_default()
            .retain(|job| job.id != job_id);
        self.reconcile_staging(&group).await;
        self.release_owner(source_id, &subject, Some(&completed), false)
            .await;
        self.persist_and_publish(Some(source_id)).await;
    }

    async fn release_owner(
        &self,
        source_id: &SourceId,
        subject: &DownloadSubject,
        media_ids: Option<&HashSet<String>>,
        retain: bool,
    ) {
        if media_ids.is_some_and(HashSet::is_empty) {
            return;
        }
        let owner = DownloadOwner::Subject(subject.clone());
        let owner_key = serde_json::to_string(&owner).expect("download ownership can be stored");
        let mut after = (String::new(), String::new());
        loop {
            let records = match self
                .database
                .download_files_page(Some(source_id.as_str()), &after, Some(&owner_key))
                .await
            {
                Ok(records) if records.is_empty() => break,
                Ok(records) => records,
                Err(error) => {
                    warn!(%error, %source_id, "could not read download ownership");
                    return;
                }
            };
            let last = records.last().unwrap();
            after = (last.media_uri.clone(), last.encoding.clone());
            for mut record in records {
                if media_ids.is_some_and(|ids| !ids.contains(&record.media_uri)) {
                    continue;
                }
                let Ok(mut owners) = download_owners(&record) else {
                    continue;
                };
                owners.remove(&owner);
                if retain {
                    owners.insert(DownloadOwner::Retained);
                }
                owners.extend(self.queued_owners_for_track(source_id, &record.media_uri, None));
                if let Err(error) = self.store_owners(&mut record, owners).await {
                    warn!(%error, %source_id, media_uri=%record.media_uri, "could not update download ownership");
                }
            }
        }
    }

    async fn store_owners(
        &self,
        file: &mut library::ConnectMediaFile,
        owners: HashSet<DownloadOwner>,
    ) -> Result<(), String> {
        if owners.is_empty() {
            self.retire_receipt(file).await.map(|_| ())
        } else {
            file.owners = serde_json::to_string(&owners).map_err(|error| error.to_string())?;
            self.database
                .connect_save_media_file(file)
                .await
                .map_err(|error| error.to_string())
        }
    }

    async fn move_job(
        &mut self,
        source_id: &SourceId,
        job_id: &str,
        target_job_id: &str,
        after: bool,
    ) {
        let group = Some(source_id.clone());
        let changed = reorder_jobs(
            self.jobs.entry(group).or_default(),
            job_id,
            target_job_id,
            after,
        );
        if changed {
            self.persist_and_publish(Some(source_id)).await;
        } else {
            self.publish(Some(source_id)).await;
        }
    }

    async fn clear(&mut self, source_id: &SourceId, notify: bool) {
        let subjects = self
            .jobs
            .get(&Some(source_id.clone()))
            .into_iter()
            .flatten()
            .map(|job| job.subject.clone())
            .collect::<HashSet<_>>();
        for subject in subjects {
            self.mark_subject_incomplete(&subject).await;
        }
        let staging_directory = self
            .attached
            .get(source_id)
            .and_then(|attached| attached.directory.clone());
        self.jobs.remove(&Some(source_id.clone()));
        let directory = source_directory(&self.root, Some(source_id));
        let result = async {
            cleanup_staging(
                &self.root,
                Some(source_id),
                staging_directory.as_ref(),
                &HashSet::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
            let mut after = (String::new(), String::new());
            loop {
                let records = self
                    .database
                    .download_files_page(Some(source_id.as_str()), &after, None)
                    .await
                    .map_err(|error| error.to_string())?;
                let Some(last) = records.last() else {
                    break;
                };
                after = (last.media_uri.clone(), last.encoding.clone());
                for record in records {
                    self.retire_receipt(&record).await?;
                }
            }
            remove_file_if_present(&directory.join(QUEUE_FILE)).await?;
            remove_file_if_present(&directory.join(QUEUE_PART_FILE)).await?;
            match tokio::fs::remove_dir(&directory).await {
                Ok(()) => Ok(()),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                    ) =>
                {
                    Ok(())
                }
                Err(error) => Err(format!("could not remove {}: {error}", directory.display())),
            }
        }
        .await;
        match result {
            Ok(()) => {
                self.publish(Some(source_id)).await;
                if notify {
                    let _ = self
                        .events
                        .send(DownloadEvent::Notice("Removed all downloads".to_string()))
                        .await;
                }
            }
            Err(error) => {
                warn!(%error, %source_id, "could not clear source downloads");
                if notify {
                    let _ = self
                        .events
                        .send(DownloadEvent::Notice(
                            "Could not remove all downloads".to_string(),
                        ))
                        .await;
                }
            }
        }
    }

    async fn persist_and_publish(&self, source_id: Option<&SourceId>) {
        let group = source_id.cloned();
        if let Err(error) = persist_queue(
            &self.root,
            source_id,
            self.jobs.get(&group).map(Vec::as_slice).unwrap_or(&[]),
        )
        .await
        {
            warn!(%error, source_id=?source_id, "could not save the download queue");
        }
        self.publish(source_id).await;
    }

    async fn publish_all(&self) {
        let source_ids = self.jobs.keys().cloned().collect::<Vec<_>>();
        for source_id in source_ids {
            self.publish(source_id.as_ref()).await;
        }
    }

    async fn publish(&self, source_id: Option<&SourceId>) {
        let group = source_id.cloned();
        let downloaded_tracks = self
            .database
            .downloaded_count(source_id.map(SourceId::as_str))
            .await
            .unwrap_or_default();
        let jobs = self
            .jobs
            .get(&group)
            .into_iter()
            .flatten()
            .map(|job| job.queue_item(group.clone()))
            .collect::<Vec<_>>();
        let _ = self
            .events
            .send(DownloadEvent::Queue {
                source_id: group,
                snapshot: Arc::new(DownloadQueueSnapshot {
                    jobs: jobs.into(),
                    downloaded_tracks,
                    paused: self.paused,
                }),
            })
            .await;
    }
}

async fn cache_track_assets(
    database: &Database,
    artwork: &artwork::Artwork,
    source: Option<&Source>,
    peer: Option<&sources::PeerAccess>,
    media_uri: &str,
    input_digest: [u8; 32],
) {
    if let Some(peer) = peer
        && let Err(error) = peer.cache_assets(database, media_uri, input_digest).await
    {
        warn!(%error, %media_uri, "could not download accompanying media");
    }
    let images = async {
        let bindings = database
            .track_artwork_bindings(&[media_uri.to_owned()], &library::ReadCancellation::new())
            .await
            .map_err(|error| error.to_string())?;
        for binding in bindings {
            artwork
                .cache_source(&binding)
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok::<_, String>(())
    };
    let lyrics = async {
        if let Some(source) = source {
            let input = if library::source_entity_parts(media_uri).is_some() {
                input_digest
            } else {
                [0; 32]
            };
            source
                .cached_lyrics(database, media_uri, input)
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok::<_, String>(())
    };
    let (images, lyrics) = tokio::join!(images, lyrics);
    for (kind, result) in [("artwork", images), ("lyrics", lyrics)] {
        if let Err(error) = result {
            warn!(%error, %media_uri, kind, "could not download accompanying media");
        }
    }
}

fn download_source_failure(error: SourceError) -> DownloadFailure {
    match error {
        SourceError::NotFound => DownloadFailure::Item(error.to_string()),
        SourceError::Server { status, .. } if status < 500 && status != 429 => {
            DownloadFailure::Item(error.to_string())
        }
        SourceError::Tls(_)
        | SourceError::Network(_)
        | SourceError::Server { .. }
        | SourceError::Cancelled => DownloadFailure::Retry(error.to_string()),
        SourceError::Auth(_)
        | SourceError::Library(_)
        | SourceError::Json(_)
        | SourceError::InvalidRequest(_)
        | SourceError::InvalidConfig(_)
        | SourceError::Other(_) => DownloadFailure::NeedsAttention(error.to_string()),
    }
}

fn job_id(source_id: Option<&SourceId>, subject: &DownloadSubject, sequence: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let input = serde_json::to_vec(&(source_id, subject, now, sequence)).unwrap_or_default();
    hash_id_bytes(&input)
}

fn reorder_jobs(
    jobs: &mut Vec<DownloadJob>,
    job_id: &str,
    target_job_id: &str,
    after: bool,
) -> bool {
    if job_id == target_job_id {
        return false;
    }
    let Some(source_index) = jobs.iter().position(|job| job.id == job_id) else {
        return false;
    };
    let job = jobs.remove(source_index);
    let Some(target_index) = jobs
        .iter()
        .position(|candidate| candidate.id == target_job_id)
    else {
        jobs.insert(source_index, job);
        return false;
    };
    jobs.insert(target_index + usize::from(after), job);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn download_fixture(
        source_id: &str,
        track_object_id: &str,
    ) -> (tempfile::TempDir, Database, SourceKey, TrackKey) {
        let directory = tempfile::tempdir().expect("temporary download fixture");
        let database = Database::open(&directory.path().join("library.sqlite3"))
            .await
            .expect("open Library");
        let source = database
            .reconcile_source(&library::SourceId::new(source_id), "server", "Source")
            .await
            .unwrap()
            .source;
        database
            .catalog(source_id, None)
            .await
            .unwrap()
            .apply_catalog(
                &library::CatalogBatch {
                    tracks: vec![library::TrackObservation {
                        object_id: track_object_id.to_owned(),
                        metadata: library::TrackMetadata {
                            title: "Track".into(),
                            artist: "Artist".into(),
                            album: "Album".into(),
                            sort_text: "track".into(),
                            duration_millis: 180_000,
                            disc_number: 1,
                            track_number: 1,
                            source_format: Some("FLAC".into()),
                            ..Default::default()
                        },
                        loudness_analysis_key: [7; 32],
                        artwork_binding: None,
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                true,
            )
            .await
            .unwrap();
        let track = database
            .catalog_keys(
                library::CatalogKind::Track,
                Some(source),
                &[(track_object_id).to_string()],
                &library::ReadCancellation::new(),
            )
            .await
            .map(|keys| keys.into_iter().next().map(library::TrackKey::from_raw))
            .expect("read Track key")
            .expect("Track exists");
        (directory, database, source, track)
    }

    #[tokio::test]
    async fn queued_media_survives_restart_serialization() {
        let source_id = SourceId::new("source");
        let (root, database, source, _) = download_fixture("source", "track").await;
        let media_uri = reqwest::Url::from_file_path(root.path().join("track.flac"))
            .unwrap()
            .to_string();
        database
            .catalog(source_id.as_str(), None)
            .await
            .unwrap()
            .apply_catalog(
                &library::CatalogBatch {
                    tracks: vec![library::TrackObservation {
                        object_id: "track".into(),
                        media_uri: Some(media_uri.clone()),
                        metadata: library::TrackMetadata {
                            title: "Track".into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                true,
            )
            .await
            .unwrap();
        let direct = "https://media.example/direct.flac".to_string();
        let queue = QueueFile {
            version: QUEUE_VERSION,
            jobs: vec![DownloadJob {
                id: "job".to_string(),
                subject: DownloadSubject::for_media_uris(
                    "track",
                    Some("Track"),
                    std::slice::from_ref(&media_uri),
                ),
                quality: DownloadQuality::Original,
                replace_unknown: false,
                completed: Vec::new(),
                failed: false,
                remaining: vec![media_uri.clone(), direct.clone()],
                state: DownloadQueueState::Queued,
            }],
        };
        let restored: QueueFile =
            serde_json::from_slice(&serde_json::to_vec(&queue).expect("encode Queue"))
                .expect("decode Queue");
        assert_eq!(
            restored.jobs[0].remaining,
            [media_uri.clone(), direct.clone()]
        );
        persist_queue(root.path(), None, &restored.jobs)
            .await
            .unwrap();
        let mut already_saved = restored.jobs[0].clone();
        already_saved.remaining = vec![media_uri.clone()];
        persist_queue(root.path(), Some(&source_id), &[already_saved])
            .await
            .unwrap();
        let old = staging_paths(root.path(), None, &media_uri, None);
        let http = staging_paths(root.path(), None, &direct, None);
        std::fs::create_dir_all(old.audio_part.parent().unwrap()).unwrap();
        std::fs::write(&old.audio_part, b"old partial").unwrap();
        std::fs::write(&http.audio_part, b"http partial").unwrap();
        let settings = HashMap::new();
        for _ in 0..2 {
            let direct_queue = load_direct_queue(root.path(), &database, &settings)
                .await
                .unwrap();
            assert_eq!(direct_queue[0].remaining, std::slice::from_ref(&direct));
            let ordinary = load_queue(root.path(), &source_id, &database, source, None)
                .await
                .unwrap();
            assert_eq!(ordinary.len(), 1);
            assert_eq!(ordinary[0].remaining, std::slice::from_ref(&media_uri));
        }
        assert!(!old.audio_part.exists());
        assert!(http.audio_part.exists());
        let mut actor = actor_for_test(root.path(), database, &source_id, source);
        let mut settings = SourceDownloadSettings::for_source(source_id.clone());
        settings.quality = DownloadQuality::Transcoded(library::AudioEncoding {
            codec: library::AudioCodec::Opus,
            bitrate_kbps: Some(64),
            channels: library::AudioChannels::Preserve,
        });
        actor.settings.insert(source_id.clone(), settings.clone());
        actor
            .attach(source_id.clone(), source, None, None, None, [0; 32])
            .await
            .unwrap();
        assert_eq!(
            actor.jobs[&Some(source_id.clone())][0].quality,
            settings.quality
        );
        let paths = download_paths(root.path(), None, &media_uri);
        let audio = paths.audio.native().unwrap();
        std::fs::write(audio, b"saved audio").unwrap();
        let record = ReleasedDownloadRecord {
            version: RECORD_VERSION,
            media_uri: media_uri.clone(),
            quality: Some(DownloadQuality::Original),
            owners: HashSet::from([DownloadOwner::Retained]),
            audio: RecordedAudio::Native {
                storage_root: None,
                custom_storage: false,
                relative_audio_path: Some(audio.file_name().unwrap().into()),
            },
            retired_documents: Vec::new(),
            completed_size: Some(11),
        };
        std::fs::write(&paths.record, serde_json::to_vec(&record).unwrap()).unwrap();
        migrate_download_records(root.path(), &actor.database, None, None)
            .await
            .unwrap();
        let receipt = actor
            .database
            .connect_media_file(&media_uri, "original")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(receipt.source_id.as_deref(), Some(source_id.as_str()));
        assert_eq!(std::fs::read(audio).unwrap(), b"saved audio");
        assert_eq!(
            actor
                .database
                .downloaded_count(Some(source_id.as_str()))
                .await
                .unwrap(),
            1
        );
        actor
            .apply(
                Command::Remove {
                    media_uris: vec![media_uri],
                    notify: false,
                },
                &mut Vec::new(),
            )
            .await;
        assert!(actor.jobs[&Some(source_id)].is_empty());
    }

    #[tokio::test]
    async fn direct_http_media_uses_the_application_download_queue_without_a_source() {
        let (_library, database, source_key, _) = download_fixture("unused", "track").await;
        let root = tempfile::Builder::new()
            .prefix("downloads café %")
            .tempdir()
            .expect("Downloads root");
        let source_id = SourceId::new("unused");
        let mut actor = actor_for_test(root.path(), database, &source_id, source_key);
        let media_uri = "https://media.example/direct.flac".to_string();
        let (events, received) = async_channel::unbounded();
        actor.events.sender = events;
        actor
            .apply(
                Command::Download {
                    subject: DownloadSubject::Prepared {
                        context_id: "direct".to_string(),
                        title: Some("Direct".to_string()),
                    },
                    media_uris: vec![media_uri.clone()],
                },
                &mut Vec::new(),
            )
            .await;

        assert_eq!(
            actor.jobs[&None][0].remaining,
            std::slice::from_ref(&media_uri)
        );
        let queue = std::fs::read(source_directory(root.path(), None).join(QUEUE_FILE))
            .expect("read direct Queue");
        let queue: QueueFile = serde_json::from_slice(&queue).expect("decode direct Queue");
        assert_eq!(queue.jobs[0].remaining, std::slice::from_ref(&media_uri));
        let DownloadEvent::Queue {
            source_id,
            snapshot,
        } = received.recv().await.unwrap()
        else {
            panic!("Queue event")
        };
        assert!(source_id.is_none());
        assert_eq!(
            snapshot.jobs[0].preview_uris,
            std::slice::from_ref(&media_uri)
        );
        let DownloadEvent::Feedback(feedback) = received.recv().await.unwrap() else {
            panic!("feedback event")
        };
        assert_eq!(feedback.preview_uris, std::slice::from_ref(&media_uri));

        let paths = download_paths(root.path(), None, &media_uri);
        std::fs::create_dir_all(&paths.directory).expect("create download directory");
        std::fs::write(&paths.audio_part, b"complete bytes").expect("write completed transfer");
        let job = actor.jobs[&None][0].clone();
        let subject = job.subject.clone();
        let active = ActiveDownload {
            source_id: None,
            job_id: job.id,
            media_uri: media_uri.clone(),
            subject: job.subject,
            paths: paths.clone(),
            quality: DownloadQuality::Original,
            cancellation: None,
            revision: Arc::new(Mutex::new(None)),
            task: tokio::spawn(async { Ok(()) }),
        };
        actor.finish(active, Ok(Ok(())), &mut Vec::new()).await;

        assert_eq!(
            received.recv().await.unwrap(),
            DownloadEvent::Changed {
                media_uri: media_uri.clone(),
                downloaded: true,
            }
        );
        let access = actor
            .database
            .playback_access(&media_uri)
            .await
            .unwrap()
            .map(|(uri, _)| uri);
        assert_eq!(
            reqwest::Url::parse(access.as_deref().unwrap())
                .unwrap()
                .to_file_path()
                .unwrap(),
            paths.audio.native().unwrap()
        );
        assert_eq!(
            received.recv().await.unwrap(),
            DownloadEvent::SubjectChanged {
                subject: subject.clone(),
                downloaded: true,
            }
        );
        let DownloadEvent::Queue { snapshot, .. } = received.recv().await.unwrap() else {
            panic!("completed Queue event")
        };
        assert!(snapshot.jobs.is_empty());
        assert_eq!(snapshot.downloaded_tracks, 1);
        for source_id in ["unused", "missing"] {
            let count = actor
                .database
                .downloaded_count(Some(source_id))
                .await
                .unwrap();
            assert_eq!(count, 0);
        }

        actor
            .apply(
                Command::Remove {
                    media_uris: vec![media_uri.clone()],
                    notify: false,
                },
                &mut Vec::new(),
            )
            .await;
        assert_eq!(
            received.recv().await.unwrap(),
            DownloadEvent::Changed {
                media_uri: media_uri.clone(),
                downloaded: false,
            }
        );
        let access = actor
            .database
            .playback_access(&media_uri)
            .await
            .unwrap()
            .map(|(uri, _)| uri);
        assert!(access.is_none());
        assert!(!paths.audio.native().unwrap().exists());
        assert_eq!(
            received.recv().await.unwrap(),
            DownloadEvent::SubjectChanged {
                subject,
                downloaded: false,
            }
        );
        let DownloadEvent::Queue { snapshot, .. } = received.recv().await.unwrap() else {
            panic!("removed Queue event")
        };
        assert_eq!(snapshot.downloaded_tracks, 0);
    }

    #[tokio::test]
    async fn peer_representation_is_recorded_only_after_download_commit() {
        let (_library, database, source_key, track) =
            download_fixture("connect-copy", "track").await;
        let root = tempfile::tempdir().unwrap();
        let source = SourceId::new("connect-copy");
        let uri = test_media(&source, source_key, track);
        let mut actor = actor_for_test(root.path(), database.clone(), &source, source_key);
        let subject =
            DownloadSubject::for_media_uris("connect", Some("Track"), std::slice::from_ref(&uri));
        let _ = actor
            .enqueue(
                Some(source.clone()),
                subject.clone(),
                DownloadQuality::Transcoded(library::AudioEncoding::mp3()),
                vec![uri.clone()],
                false,
            )
            .await;
        let paths = download_paths(root.path(), Some(&source), &uri);
        std::fs::create_dir_all(paths.audio_part.parent().unwrap()).unwrap();
        std::fs::write(&paths.audio_part, b"verified peer bytes").unwrap();
        assert!(
            database
                .connect_media_file(&uri, "mp3")
                .await
                .unwrap()
                .is_none()
        );
        let active = ActiveDownload {
            source_id: Some(source.clone()),
            job_id: actor.jobs[&Some(source.clone())][0].id.clone(),
            media_uri: uri.clone(),
            subject,
            paths: paths.clone(),
            quality: DownloadQuality::Transcoded(library::AudioEncoding::mp3()),
            cancellation: None,
            task: tokio::spawn(async { Ok(()) }),
            revision: Arc::new(Mutex::new(Some("original-revision-2".into()))),
        };
        actor.finish(active, Ok(Ok(())), &mut Vec::new()).await;
        assert_eq!(
            std::fs::read(paths.audio.native().unwrap()).unwrap(),
            b"verified peer bytes"
        );
        assert!(!paths.audio_part.exists());
        let saved = database
            .connect_media_file(&uri, "mp3")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.revision, "original-revision-2");
        assert_eq!(
            library::file_media_path(&saved.path),
            paths.audio.native().map(Path::to_owned)
        );
        assert!(saved.managed);
        assert!(
            database
                .catalog_keys(
                    library::CatalogKind::Track,
                    Some(source_key),
                    &["track".into()],
                    &library::ReadCancellation::new()
                )
                .await
                .unwrap()
                .first()
                .is_some()
        );
    }

    #[tokio::test]
    async fn collection_completion_follows_its_download_jobs_across_sources() {
        for fail_first in [false, true] {
            let (_directory, database, source_key, _) = download_fixture("source", "track").await;
            let root = tempfile::tempdir().unwrap();
            let source = SourceId::new("source");
            let mut actor = actor_for_test(root.path(), database, &source, source_key);
            let (events, received) = async_channel::unbounded();
            actor.events.sender = events;
            let subject = DownloadSubject::Prepared {
                context_id: "playlist:1".into(),
                title: None,
            };
            let remote = library::source_entity_uri(&source, "track", "track");
            let direct = "https://example.org/track.flac".to_string();
            actor
                .apply(
                    Command::Download {
                        subject: subject.clone(),
                        media_uris: vec![remote.clone(), direct.clone()],
                    },
                    &mut Vec::new(),
                )
                .await;
            while received.try_recv().is_ok() {}

            for (index, (group, uri)) in [(Some(source.clone()), remote), (None, direct)]
                .into_iter()
                .enumerate()
            {
                let job = actor.jobs[&group][0].clone();
                let paths = download_paths(root.path(), group.as_ref(), &uri);
                std::fs::create_dir_all(&paths.directory).unwrap();
                std::fs::write(&paths.audio_part, b"completed transfer").unwrap();
                let active = ActiveDownload {
                    source_id: group,
                    job_id: job.id,
                    media_uri: uri,
                    subject: job.subject,
                    paths,
                    quality: DownloadQuality::Original,
                    cancellation: None,
                    revision: Arc::new(Mutex::new(None)),
                    task: tokio::spawn(async { Ok(()) }),
                };
                let result = if index == 0 && fail_first {
                    Err(DownloadFailure::Item("unavailable track".into()))
                } else {
                    Ok(())
                };
                actor.finish(active, Ok(result), &mut Vec::new()).await;
                let mut completed = 0;
                while let Ok(event) = received.try_recv() {
                    if event
                        == (DownloadEvent::SubjectChanged {
                            subject: subject.clone(),
                            downloaded: true,
                        })
                    {
                        completed += 1;
                    }
                }
                assert_eq!(completed, usize::from(index == 1 && !fail_first));
                if index == 0 && fail_first {
                    let saved: QueueFile = serde_json::from_slice(
                        &std::fs::read(source_directory(root.path(), None).join(QUEUE_FILE))
                            .unwrap(),
                    )
                    .unwrap();
                    assert!(
                        saved.jobs[0].failed,
                        "failure remains part of the pending download after restart"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn released_completed_and_partial_downloads_rebind_on_attach() {
        let (_database_directory, database, source_key, _track_key) =
            download_fixture("source", "track-object").await;
        let downloads = tempfile::tempdir().expect("temporary Downloads root");
        let source_id = SourceId::new("source");
        let directory = source_directory(downloads.path(), Some(&source_id));
        std::fs::create_dir_all(&directory).expect("create source Downloads directory");
        let released_stem = hash_id_bytes(b"track-object");
        let audio = directory.join(format!("{released_stem}.{AUDIO_EXTENSION}"));
        std::fs::write(&audio, b"released audio").expect("write released audio");
        let record = directory.join(format!("{released_stem}.{RECORD_EXTENSION}"));
        std::fs::write(
            &record,
            serde_json::to_vec(&serde_json::json!({
                "version": 3,
                "source_id": source_id,
                "track_id": "track-object",
                "owners": ["Retained"]
            }))
            .expect("encode released record"),
        )
        .expect("write released record");

        let (released_part, released_checkpoint) =
            released_staging_paths(downloads.path(), &source_id, "track-object", None);
        std::fs::write(&released_part, b"partial").expect("write released partial");
        std::fs::write(&released_checkpoint, b"checkpoint").expect("write released checkpoint");
        std::fs::write(
            directory.join(QUEUE_FILE),
            serde_json::to_vec(&serde_json::json!({
                "version": 1,
                "source_id": source_id,
                "jobs": [{
                    "id": "job",
                    "subject": {"Track": "track-object"},
                    "quality": "Original",
                    "total_tracks": 1,
                    "completed": [],
                    "remaining": ["track-object"],
                    "state": "Queued"
                }]
            }))
            .expect("encode released Queue"),
        )
        .expect("write released Queue");

        migrate_download_records(
            downloads.path(),
            &database,
            Some((source_key, &source_id)),
            None,
        )
        .await
        .expect("attach released download");
        let records = database
            .download_files_page(
                Some(source_id.as_str()),
                &(String::new(), String::new()),
                None,
            )
            .await
            .expect("load migrated records");
        let migrated = records.first().expect("migrated record");
        assert_eq!(
            migrated.media_uri,
            library::source_entity_uri(&source_id, "track", "track-object")
        );
        assert_eq!(migrated.completed_size, Some(14));
        assert_eq!(
            Path::new(&migrated.relative_path),
            audio.file_name().map(Path::new).unwrap()
        );
        assert!(
            !record.exists(),
            "released record is replaced by the completed registry"
        );
        assert!(audio.is_file());

        let jobs = load_queue(downloads.path(), &source_id, &database, source_key, None)
            .await
            .expect("migrate released Queue");
        assert_eq!(
            jobs[0].remaining[0],
            library::source_entity_uri(&source_id, "track", "track-object")
        );
        let identity = jobs[0].remaining[0].clone();
        let current = staging_paths(downloads.path(), Some(&source_id), &identity, None);
        assert_eq!(
            std::fs::read(current.audio_part).expect("read migrated partial"),
            b"partial"
        );
        assert_eq!(
            std::fs::read(current.checkpoint).expect("read migrated checkpoint"),
            b"checkpoint"
        );
    }

    #[tokio::test]
    async fn released_custom_download_is_authorized_by_current_configuration() {
        let (_database_directory, database, source_key, _track_key) =
            download_fixture("custom-source", "custom-track").await;
        let downloads = tempfile::tempdir().expect("temporary Downloads root");
        let custom = tempfile::tempdir().expect("configured custom root");
        let source_id = SourceId::new("custom-source");
        let directory = source_directory(downloads.path(), Some(&source_id));
        std::fs::create_dir_all(&directory).expect("create source Downloads directory");
        let relative = PathBuf::from("Artist/Album/custom.audio");
        let audio = custom.path().join(&relative);
        std::fs::create_dir_all(audio.parent().expect("custom audio parent"))
            .expect("create custom audio parent");
        std::fs::write(&audio, b"custom audio").expect("write custom audio");
        let record = directory.join(format!(
            "{}.{RECORD_EXTENSION}",
            hash_id_bytes(b"custom-track")
        ));
        std::fs::write(
            record,
            serde_json::to_vec(&serde_json::json!({
                "version": 3,
                "source_id": source_id,
                "track_id": "custom-track",
                "owners": [{"Subject": {"Rule": "Favorites"}}],
                "audio_root": custom.path(),
                "audio_path": audio
            }))
            .expect("encode released custom record"),
        )
        .expect("write released custom record");

        migrate_download_records(
            downloads.path(),
            &database,
            Some((source_key, &source_id)),
            Some(&DownloadDirectory::Native(custom.path().to_owned())),
        )
        .await
        .expect("attach released custom download");
        let records = database
            .download_files_page(
                Some(source_id.as_str()),
                &(String::new(), String::new()),
                None,
            )
            .await
            .expect("load migrated custom record");
        let migrated = records.first().expect("migrated custom record");
        assert_eq!(
            migrated.media_uri,
            library::source_entity_uri(&source_id, "track", "custom-track")
        );
        assert_eq!(Path::new(&migrated.storage_root), custom.path());
        assert!(
            download_owners(migrated)
                .unwrap()
                .contains(&DownloadOwner::Subject(DownloadSubject::Rule(
                    DownloadRule::Favorites
                )))
        );
        assert_eq!(Path::new(&migrated.relative_path), relative.as_path());
        assert_eq!(migrated.completed_size, Some(12));
        assert!(custom.path().join(relative).is_file());
    }

    #[tokio::test]
    async fn cue_backing_file_reuse_and_removal() {
        let root = tempfile::tempdir().unwrap();
        for (case, mapped, original_exists, quality, document) in [
            ("existing", true, true, DownloadQuality::Original, false),
            ("own", false, true, DownloadQuality::Original, false),
            (
                "source-mapped",
                false,
                true,
                DownloadQuality::Transcoded(library::AudioEncoding::mp3()),
                false,
            ),
            ("empty", true, false, DownloadQuality::Original, false),
            ("private", false, false, DownloadQuality::Original, false),
            (
                "smaller",
                true,
                false,
                DownloadQuality::Transcoded(library::AudioEncoding::mp3()),
                false,
            ),
            ("document", true, false, DownloadQuality::Original, true),
        ] {
            let (_db_root, database, source, _) = download_fixture(case, "track").await;
            let folder = root.path().join(case);
            std::fs::create_dir_all(&folder).unwrap();
            let original = folder.join("album.flac");
            if original_exists {
                std::fs::write(&original, b"whole audio").unwrap();
            }
            let mut actor = actor_for_test(root.path(), database, &SourceId::new(case), source);
            let source_id = SourceId::new(case);
            let client = Arc::new(
                Source::open(
                    sources::SourceConfiguration::local(
                        source_id.clone(),
                        "Local",
                        vec![folder.clone()],
                    )
                    .unwrap(),
                    None,
                    None,
                )
                .unwrap(),
            );
            actor.attached.get_mut(&source_id).unwrap().source = Some(Arc::downgrade(&client));
            let mut settings = SourceDownloadSettings::for_source(source_id.clone());
            settings.quality = quality;
            if case == "smaller" {
                settings.directory = Some(DownloadDirectory::Native(root.path().join("custom")));
                actor.attached.get_mut(&source_id).unwrap().directory = settings.directory.clone();
            }
            actor.settings.insert(source_id.clone(), settings);
            if mapped {
                actor.peer.set_folders(std::collections::BTreeMap::from([(
                    case.into(),
                    library::LocalMediaLocation::File(folder.clone()),
                )]));
            }
            let own = original_exists && !mapped;
            let remote = if own {
                reqwest::Url::from_file_path(&original).unwrap().to_string()
            } else if document {
                "content://other-device/album.flac".to_owned()
            } else {
                reqwest::Url::from_file_path(root.path().join(format!("remote-{case}.flac")))
                    .unwrap()
                    .to_string()
            };
            let mut uris = Vec::new();
            for index in 0..3 {
                let uri = library::cue_media_uri(
                    &format!("{case}:{index}"),
                    &remote,
                    index * 1000,
                    (index + 1) * 1000,
                );
                actor.database.connect_apply(&[library::ConnectRecord {
                    kind: "track".into(), key: uri.clone(), value: Some(serde_json::json!({
                        "object_id":format!("{case}:{index}"), "media_uri":uri, "source_id":case,
                        "title":format!("Track {index}"), "display_album":"Album", "display_artist":"Artist", "duration_millis":1000,
                        "disc_number":1, "track_number":index+1, "relative_path":"album.flac", "source_format":"flac", "revision":"v1", "local_media":true
                    })),
                }]).await.unwrap();
                actor
                    .database
                    .create_playlist(
                        None,
                        "Saved",
                        &library::CatalogQuery::uris(
                            library::CatalogKind::Track,
                            vec![uri.clone()],
                        )
                        .selection(),
                    )
                    .await
                    .unwrap();
                if index == 0 {
                    let source_id = SourceId::new(case);
                    let rule = DownloadRule::EntireLibrary;
                    let smaller = DownloadQuality::Transcoded(library::AudioEncoding::mp3());
                    actor
                        .reconcile_rule(
                            source_id.clone(),
                            rule,
                            smaller,
                            vec![uri.clone()],
                            &mut Vec::new(),
                        )
                        .await;
                    assert_eq!(
                        actor.jobs[&Some(source_id.clone())].is_empty(),
                        original_exists && mapped
                    );
                    if own {
                        actor
                            .database
                            .upsert_local_locator(&library::LocalLocatorWrite {
                                source_id: Some(case.into()),
                                media_uri: remote.clone(),
                                origin: if case == "source-mapped" {
                                    "mapping"
                                } else {
                                    "import"
                                }
                                .into(),
                                path: original.to_string_lossy().into_owned(),
                                root: folder.to_string_lossy().into_owned(),
                                relative_path: "album.flac".into(),
                                access_uri: remote.clone(),
                            })
                            .await
                            .unwrap();
                        actor
                            .reconcile_rule(
                                source_id.clone(),
                                rule,
                                smaller,
                                vec![uri.clone()],
                                &mut Vec::new(),
                            )
                            .await;
                        assert!(actor.jobs[&Some(source_id.clone())].is_empty());
                    }
                    assert!(
                        actor
                            .database
                            .connect_media_files(&uri)
                            .await
                            .unwrap()
                            .is_empty()
                    );
                    actor.remove_rule(&source_id, rule, true).await;
                    assert_eq!(original.is_file(), original_exists);
                }
                actor
                    .apply(
                        Command::Download {
                            subject: DownloadSubject::Prepared {
                                context_id: format!("{case}:{index}"),
                                title: None,
                            },
                            media_uris: vec![uri.clone()],
                        },
                        &mut Vec::new(),
                    )
                    .await;
                if let Some(active) = actor.start_next(&[]).await {
                    assert_eq!(active.source_id.as_ref(), Some(&source_id));
                    assert_eq!(active.quality, quality);
                    if case == "smaller" {
                        assert!(
                            active
                                .paths
                                .audio
                                .native()
                                .unwrap()
                                .starts_with(root.path().join("custom"))
                        );
                    }
                    active.task.abort();
                    prepare_download_directories(&active.paths).await.unwrap();
                    std::fs::write(&active.paths.audio_part, b"whole audio").unwrap();
                    actor.finish(active, Ok(Ok(())), &mut Vec::new()).await;
                }
                uris.push(uri);
            }
            let mut paths = Vec::new();
            for uri in &uris {
                let file = actor
                    .database
                    .connect_media_file(uri, &library::media_encoding_key(quality.encoding()))
                    .await
                    .unwrap()
                    .unwrap();
                let managed = !original_exists || quality != DownloadQuality::Original;
                assert_eq!(file.managed, managed);
                if managed {
                    let owner =
                        DownloadOwner::Subject(DownloadSubject::Rule(DownloadRule::Favorites));
                    assert!(
                        actor
                            .add_download_owner(
                                Some(&SourceId::new(case)),
                                uri,
                                &owner,
                                quality,
                                false
                            )
                            .await
                            .unwrap()
                    );
                    let retained = actor
                        .database
                        .connect_media_file(uri, &file.encoding)
                        .await
                        .unwrap()
                        .unwrap();
                    assert!(download_owners(&retained).unwrap().contains(&owner));
                }
                paths.push(file.path);
            }
            assert!(
                paths.iter().all(|path| path == &paths[0]),
                "shared backing for {case}"
            );
            let file = library::file_media_path(&paths[0]).unwrap();
            let managed = !original_exists || quality != DownloadQuality::Original;
            for (index, uri) in uris.into_iter().enumerate() {
                let (removed, failed) = actor.delete_downloads(Some(&source_id), [uri]).await;
                assert_eq!(failed, 0);
                assert_eq!(removed, usize::from(index == 2 && managed));
                assert_eq!(file.is_file(), index != 2 || !managed);
            }
            assert_eq!(original.is_file(), original_exists);
        }
    }

    #[tokio::test]
    async fn cue_concurrent_completion_and_revision_changes() {
        let root = tempfile::tempdir().unwrap();
        for original_exists in [false, true] {
            let (_db_root, database, source, _) = download_fixture("cue", "track").await;
            let mut actor = actor_for_test(root.path(), database, &SourceId::new("cue"), source);
            let folder = root.path().join(format!("mapped-{original_exists}"));
            std::fs::create_dir_all(&folder).unwrap();
            let client = Arc::new(
                Source::open(
                    sources::SourceConfiguration::local(
                        SourceId::new("cue"),
                        "Local",
                        vec![folder.clone()],
                    )
                    .unwrap(),
                    None,
                    None,
                )
                .unwrap(),
            );
            actor
                .attached
                .get_mut(&SourceId::new("cue"))
                .unwrap()
                .source = Some(Arc::downgrade(&client));
            let original = folder.join("album.flac");
            if original_exists {
                std::fs::write(&original, b"old audio").unwrap();
            }
            actor.peer.set_folders(std::collections::BTreeMap::from([(
                "cue".into(),
                library::LocalMediaLocation::File(folder.clone()),
            )]));
            let remote = reqwest::Url::from_file_path(folder.join("remote.flac"))
                .unwrap()
                .to_string();
            let mut uris = Vec::new();
            for index in 0..3 {
                let uri = library::cue_media_uri(
                    &format!("{original_exists}:{index}"),
                    &remote,
                    index * 1000,
                    (index + 1) * 1000,
                );
                actor.database.connect_apply(&[library::ConnectRecord {
                    kind:"track".into(), key:uri.clone(), value:Some(serde_json::json!({
                        "object_id":format!("{original_exists}:{index}"), "media_uri":uri, "source_id":"cue", "title":"Track", "display_album":"Album", "display_artist":"Artist", "duration_millis":1000,
                        "relative_path":"album.flac", "source_format":"flac", "revision":"v1", "local_media":true
                    })),
                }]).await.unwrap();
                uris.push(uri);
            }
            actor
                .database
                .create_playlist(
                    None,
                    "Saved",
                    &library::CatalogQuery::uris(library::CatalogKind::Track, uris.clone())
                        .selection(),
                )
                .await
                .unwrap();
            for (version, bytes) in [("v1", b"old audio"), ("v2", b"new audio")] {
                for (index, uri) in uris.iter().enumerate() {
                    let mut reference = actor
                        .database
                        .connect_track_reference(uri)
                        .await
                        .unwrap()
                        .unwrap();
                    reference["revision"] = version.into();
                    actor
                        .database
                        .connect_apply(&[library::ConnectRecord {
                            kind: "track".into(),
                            key: uri.clone(),
                            value: Some(reference),
                        }])
                        .await
                        .unwrap();
                    if version == "v2" && index == 0 {
                        assert!(
                            !actor
                                .add_download_owner(
                                    Some(&SourceId::new("cue")),
                                    uri,
                                    &DownloadOwner::Subject(DownloadSubject::Rule(
                                        DownloadRule::EntireLibrary
                                    )),
                                    DownloadQuality::Original,
                                    false,
                                )
                                .await
                                .unwrap()
                        );
                        assert!(matches!(
                            actor
                                .peer
                                .download(&actor.database, uri, DownloadQuality::Original)
                                .await,
                            Err(SourceError::Network(_))
                        ));
                    }
                    actor
                        .apply(
                            Command::Download {
                                subject: DownloadSubject::Prepared {
                                    context_id: format!("{version}:{index}"),
                                    title: None,
                                },
                                media_uris: vec![uri.clone()],
                            },
                            &mut Vec::new(),
                        )
                        .await;
                    if let Some(active) = actor.start_next(&[]).await {
                        active.task.abort();
                        prepare_download_directories(&active.paths).await.unwrap();
                        std::fs::write(&active.paths.audio_part, bytes).unwrap();
                        actor.finish(active, Ok(Ok(())), &mut Vec::new()).await;
                    }
                    let file = actor
                        .database
                        .connect_media_file(uri, "original")
                        .await
                        .unwrap()
                        .unwrap();
                    assert_eq!(
                        std::fs::read(library::file_media_path(&file.path).unwrap()).unwrap(),
                        bytes,
                        "original_exists={original_exists}, revision={version}, track={index}"
                    );
                    if version == "v2" && (index == 0 || original_exists) {
                        assert_eq!(std::fs::read(&original).unwrap(), b"old audio");
                    }
                }
                let mut paths = Vec::new();
                for uri in &uris {
                    paths.push(
                        actor
                            .database
                            .connect_media_file(uri, "original")
                            .await
                            .unwrap()
                            .unwrap()
                            .path,
                    );
                }
                assert!(paths.iter().all(|path| path == &paths[0]));
            }
            assert_eq!(original.is_file(), original_exists);
        }
    }

    fn actor_for_test(
        root: &Path,
        database: Database,
        source_id: &SourceId,
        source_key: SourceKey,
    ) -> Actor {
        let (events, _) = async_channel::unbounded();
        let (prepared_rules, _) = async_channel::unbounded();
        Actor {
            artwork: artwork::Artwork::new(root.join("artwork"), tokio::runtime::Handle::current())
                .unwrap(),
            root: Arc::new(root.to_path_buf()),
            database,
            events: DownloadEvents {
                sender: events,
                status: tokio::sync::watch::channel(DownloadStatus::default()).0,
            },
            transfers: Arc::new(TransferClients::default()),
            prepared_rules,
            peer: sources::PeerAccess::default(),
            attached: HashMap::from([(
                source_id.clone(),
                AttachedSource {
                    source_key,
                    input_digest: [0; 32],
                    source: None,
                    folder: None,
                    directory: None,
                },
            )]),
            settings: HashMap::new(),
            running_rules: None,
            pending_rules: None,
            jobs: HashMap::new(),
            paused: false,
            next_job: 0,
        }
    }

    fn test_media(source_id: &SourceId, _source_key: SourceKey, _track: TrackKey) -> String {
        library::source_entity_uri(source_id, "track", "track")
    }

    fn test_job(media_uri: String) -> DownloadJob {
        DownloadJob {
            id: "job".to_string(),
            subject: DownloadSubject::for_media_uris(
                "track",
                Some("Track"),
                std::slice::from_ref(&media_uri),
            ),
            quality: DownloadQuality::Original,
            replace_unknown: false,
            completed: Vec::new(),
            failed: false,
            remaining: vec![media_uri],
            state: DownloadQueueState::Downloading,
        }
    }

    #[tokio::test]
    async fn pausing_cancels_active_work_and_keeps_the_job_queued() {
        let (_library, database, source_key, track) = download_fixture("pause", "track").await;
        let root = tempfile::tempdir().expect("Downloads root");
        let source_id = SourceId::new("pause");
        let mut actor = actor_for_test(root.path(), database, &source_id, source_key);
        let media_uri = test_media(&source_id, source_key, track);
        actor
            .jobs
            .insert(Some(source_id.clone()), vec![test_job(media_uri.clone())]);
        let paths = download_paths(root.path(), Some(&source_id), &media_uri);
        let (cancel, cancelled) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = cancelled.await;
            Err(DownloadFailure::Retry("cancelled".to_string()))
        });
        let mut active = vec![ActiveDownload {
            source_id: Some(source_id.clone()),
            job_id: "job".to_string(),
            media_uri: media_uri.clone(),
            subject: DownloadSubject::for_media_uris(
                "track",
                Some("Track"),
                std::slice::from_ref(&media_uri),
            ),
            paths,
            quality: DownloadQuality::Original,
            cancellation: Some(cancel),
            revision: Arc::new(Mutex::new(None)),
            task,
        }];

        actor.apply(Command::SetPaused(true), &mut active).await;

        assert!(actor.paused);
        assert!(active.is_empty());
        assert_eq!(
            actor.jobs[&Some(source_id)][0].state,
            DownloadQueueState::Queued
        );
    }

    #[tokio::test]
    async fn restore_suspension_drains_active_work_and_holds_following_commands() {
        let (_library, database, source_key, track) = download_fixture("suspend", "track").await;
        let root = tempfile::tempdir().expect("Downloads root");
        let source_id = SourceId::new("suspend");
        let mut actor = actor_for_test(root.path(), database, &source_id, source_key);
        let media_uri = test_media(&source_id, source_key, track);
        actor
            .jobs
            .insert(Some(source_id.clone()), vec![test_job(media_uri.clone())]);
        let paths = download_paths(root.path(), Some(&source_id), &media_uri);
        let (cancel, cancelled) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = cancelled.await;
            Err(DownloadFailure::Retry("cancelled".to_string()))
        });
        let mut active = vec![ActiveDownload {
            source_id: Some(source_id.clone()),
            job_id: "job".to_string(),
            media_uri: media_uri.clone(),
            subject: DownloadSubject::for_media_uris(
                "track",
                Some("Track"),
                std::slice::from_ref(&media_uri),
            ),
            paths,
            quality: DownloadQuality::Original,
            cancellation: Some(cancel),
            revision: Arc::new(Mutex::new(None)),
            task,
        }];

        let (ready, acknowledged) = async_channel::bounded(1);
        let (resume, released) = async_channel::bounded(1);
        let work = tokio::spawn(async move {
            actor
                .apply(
                    Command::Suspend {
                        ready,
                        resume: released,
                    },
                    &mut active,
                )
                .await;
            (actor, active)
        });
        acknowledged.recv().await.unwrap();
        assert!(
            !work.is_finished(),
            "Store replacement retains the command boundary"
        );
        drop(DownloadSuspension(resume));
        let (actor, active) = work.await.unwrap();
        assert!(
            !actor.paused,
            "restoring does not change the user's pause preference"
        );
        assert!(active.is_empty());
        assert_eq!(
            actor.jobs[&Some(source_id)][0].state,
            DownloadQueueState::Queued
        );
    }

    #[tokio::test]
    async fn cancelling_removes_the_job_and_reconciles_its_staging_file() {
        let (_library, database, source_key, track) = download_fixture("cancel", "track").await;
        let root = tempfile::tempdir().expect("Downloads root");
        let source_id = SourceId::new("cancel");
        let mut actor = actor_for_test(root.path(), database, &source_id, source_key);
        let media_uri = test_media(&source_id, source_key, track);
        actor
            .jobs
            .insert(Some(source_id.clone()), vec![test_job(media_uri.clone())]);
        let paths = download_paths(root.path(), Some(&source_id), &media_uri);
        std::fs::create_dir_all(paths.audio_part.parent().expect("staging parent"))
            .expect("create staging");
        std::fs::write(&paths.audio_part, b"partial").expect("write staging");
        let (cancel, cancelled) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = cancelled.await;
            Err(DownloadFailure::Retry("cancelled".to_string()))
        });
        let mut active = vec![ActiveDownload {
            source_id: Some(source_id.clone()),
            job_id: "job".to_string(),
            media_uri: media_uri.clone(),
            subject: DownloadSubject::for_media_uris(
                "track",
                Some("Track"),
                std::slice::from_ref(&media_uri),
            ),
            paths: paths.clone(),
            quality: DownloadQuality::Original,
            cancellation: Some(cancel),
            revision: Arc::new(Mutex::new(None)),
            task,
        }];

        actor.cancel(Some(&source_id), "job", &mut active).await;

        assert!(actor.jobs[&Some(source_id)].is_empty());
        assert!(active.is_empty());
        assert!(!paths.audio_part.exists());
    }

    #[tokio::test]
    async fn stale_completed_transfer_cannot_commit_after_its_job_was_removed() {
        let (_library, database, source_key, track) = download_fixture("stale", "track").await;
        let root = tempfile::tempdir().expect("Downloads root");
        let source_id = SourceId::new("stale");
        let mut actor = actor_for_test(root.path(), database, &source_id, source_key);
        let media_uri = test_media(&source_id, source_key, track);
        let paths = download_paths(root.path(), Some(&source_id), &media_uri);
        std::fs::create_dir_all(paths.audio_part.parent().expect("staging parent"))
            .expect("create staging");
        std::fs::write(&paths.audio_part, b"complete bytes").expect("write completed staging");
        let active = ActiveDownload {
            source_id: Some(source_id.clone()),
            job_id: "removed".to_string(),
            media_uri: media_uri.clone(),
            subject: DownloadSubject::for_media_uris(
                "track",
                Some("Track"),
                std::slice::from_ref(&media_uri),
            ),
            paths: paths.clone(),
            quality: DownloadQuality::Original,
            cancellation: None,
            revision: Arc::new(Mutex::new(None)),
            task: tokio::spawn(async { Ok(()) }),
        };

        actor.finish(active, Ok(Ok(())), &mut Vec::new()).await;

        assert!(!paths.audio.native().unwrap().exists());
        assert!(paths.audio_part.exists());
    }

    #[tokio::test]
    async fn authoritative_rule_change_is_retained_while_prior_rules_prepare() {
        let (_library, database, source_key, _) = download_fixture("rules", "track").await;
        let root = tempfile::tempdir().expect("Downloads root");
        let source_id = SourceId::new("rules");
        let mut actor = actor_for_test(root.path(), database, &source_id, source_key);
        let mut first = DownloadRules::default();
        first.set(DownloadRule::Favorites, true);
        actor.schedule_rules(&source_id, first, true);
        assert!(actor.running_rules.is_some());
        let mut replacement = DownloadRules::default();
        replacement.set(DownloadRule::LatestFiveAlbums, true);
        actor.schedule_rules(&source_id, replacement, true);

        assert_eq!(
            actor.pending_rules.as_ref().map(|intent| intent.rules),
            Some(replacement)
        );
    }
}

#[derive(Clone, Default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DownloadStatus {
    pub revision: u64,
    pub queue_revision: u64,
    pub paused: bool,
    pub queues: Vec<DownloadQueueSummary>,
    pub notice: Option<String>,
}

#[derive(Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DownloadQueueSummary {
    pub source_id: Option<SourceId>,
    pub downloaded_tracks: u64,
    pub total_jobs: u64,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DownloadPage {
    pub total: u64,
    pub jobs: Vec<DownloadQueueItem>,
    pub previews: Vec<library::TrackView>,
}

struct DownloadEvents {
    sender: Sender<DownloadEvent>,
    status: tokio::sync::watch::Sender<DownloadStatus>,
}

impl DownloadEvents {
    fn update(&self, event: &DownloadEvent) {
        self.status.send_if_modified(|state| {
            match event {
                DownloadEvent::Changed { .. } => state.revision += 1,
                DownloadEvent::Notice(message) => state.notice = Some(message.clone()),
                DownloadEvent::Queue {
                    source_id,
                    snapshot,
                } => {
                    state.paused = snapshot.paused;
                    state.queue_revision += 1;
                    let summary = DownloadQueueSummary {
                        source_id: source_id.clone(),
                        downloaded_tracks: snapshot.downloaded_tracks as u64,
                        total_jobs: snapshot.jobs.len() as u64,
                    };
                    if let Some(previous) = state
                        .queues
                        .iter_mut()
                        .find(|queue| &queue.source_id == source_id)
                    {
                        *previous = summary;
                    } else {
                        state.queues.push(summary);
                        state.queues.sort_by(|a, b| a.source_id.cmp(&b.source_id));
                    }
                }
                DownloadEvent::Feedback(_) | DownloadEvent::SubjectChanged { .. } => return false,
            }
            true
        });
    }
    async fn send(
        &self,
        event: DownloadEvent,
    ) -> Result<(), async_channel::SendError<DownloadEvent>> {
        self.update(&event);
        self.sender.send(event).await
    }
    fn try_send(
        &self,
        event: DownloadEvent,
    ) -> Result<(), async_channel::TrySendError<DownloadEvent>> {
        self.update(&event);
        self.sender.try_send(event)
    }
}

impl DownloadJob {
    fn queue_item(&self, source_id: Option<SourceId>) -> DownloadQueueItem {
        DownloadQueueItem {
            id: self.id.clone(),
            source_id,
            subject: self.subject.clone(),
            preview_uris: self
                .completed
                .iter()
                .chain(&self.remaining)
                .take(4)
                .cloned()
                .collect(),
            quality: self.quality,
            completed_tracks: self.completed.len(),
            total_tracks: self.completed.len() + self.remaining.len(),
            state: self.state,
        }
    }
}

impl Actor {
    async fn queue_page(&self, offset: u64, limit: u32) -> Result<DownloadPage, String> {
        let mut groups = self.jobs.iter().collect::<Vec<_>>();
        groups.sort_by(|(a, _), (b, _)| a.cmp(b));
        let total = groups.iter().map(|(_, jobs)| jobs.len() as u64).sum();
        let mut offset = usize::try_from(offset).unwrap_or(usize::MAX);
        let limit = limit.min(128) as usize;
        let mut jobs = Vec::with_capacity(limit);
        for (source, queued) in groups {
            if offset >= queued.len() {
                offset -= queued.len();
                continue;
            }
            jobs.extend(
                queued
                    .iter()
                    .skip(offset)
                    .take(limit - jobs.len())
                    .map(|job| job.queue_item(source.clone())),
            );
            offset = 0;
            if jobs.len() == limit {
                break;
            }
        }
        let uris = jobs
            .iter()
            .filter_map(|job| job.preview_uris.first().cloned())
            .collect::<Vec<_>>();
        let previews = library::CatalogQuery::uris(library::CatalogKind::Track, (&uris).to_vec())
            .rows(
                &self.database,
                0,
                (&uris).len(),
                &library::ReadCancellation::new(),
            )
            .await
            .map(|rows| match rows {
                library::CatalogRows::Tracks(rows) => rows,
                _ => unreachable!("typed catalog projection"),
            })
            .map_err(|error| error.to_string())?;
        Ok(DownloadPage {
            total,
            jobs,
            previews,
        })
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Downloads {
    pub async fn clear_artwork_cache(&self) -> sources::SourceResult<()> {
        let (response, receive) = async_channel::bounded(1);
        self.send(Command::ClearArtworkCache(response));
        receive
            .recv()
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?
            .map_err(SourceError::Other)
    }

    pub async fn clear_lyrics_cache(&self) -> sources::SourceResult<()> {
        let (response, receive) = async_channel::bounded(1);
        self.send(Command::ClearLyricsCache(response));
        receive
            .recv()
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?
            .map_err(SourceError::Other)
    }

    pub fn subscribe(&self) -> Arc<DownloadSubscription> {
        let mut changes = self.status.clone();
        changes.mark_changed();
        Arc::new(DownloadSubscription {
            changes: tokio::sync::Mutex::new(changes),
        })
    }
    pub async fn queue_page(&self, offset: u64, limit: u32) -> sources::SourceResult<DownloadPage> {
        let (response, receive) = async_channel::bounded(1);
        self.send(Command::Page {
            offset,
            limit,
            response,
        });
        receive
            .recv()
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?
            .map_err(SourceError::Other)
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct DownloadSubscription {
    changes: tokio::sync::Mutex<tokio::sync::watch::Receiver<DownloadStatus>>,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl DownloadSubscription {
    pub async fn next(&self) -> sources::SourceResult<DownloadStatus> {
        let mut changes = self.changes.lock().await;
        changes
            .changed()
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?;
        Ok(changes.borrow_and_update().clone())
    }
}
