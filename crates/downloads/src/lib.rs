//! Download queue ownership, command handling, and bounded transfer execution.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Weak};
use std::task::Poll;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_channel::{Receiver, Sender};
use library::{
    AlbumKey, ArtistKey, Database, FolderKey, GenreKey, MoodKey, PlaylistKey, SmartPlaylistKey,
    SourceKey, TrackKey, TrackSort,
};
use playback::{ResolvedStream, StreamQuality, StreamRequest};
use serde::{Deserialize, Serialize};
use sources::{Source, SourceError, SourceId};
use tracing::warn;

mod track_download;

use track_download::*;

const QUEUE_VERSION: u32 = 2;
const QUEUE_FILE: &str = "queue.json";
const QUEUE_PART_FILE: &str = "queue.json.part";
const RETRY_INTERVAL: Duration = Duration::from_secs(30);
const MAX_ACTIVE_DOWNLOADS: usize = 3;

#[derive(Clone)]
pub struct Downloads {
    root: Arc<PathBuf>,
    commands: Sender<Command>,
}

enum Command {
    Attach {
        source_id: SourceId,
        source_key: SourceKey,
        source: Option<Weak<Source>>,
        folder: Option<FolderKey>,
        response: Sender<Result<(), String>>,
    },
    Download {
        source_id: SourceId,
        subject: DownloadSubject,
        track_keys: Vec<TrackKey>,
    },
    Remove {
        source_id: SourceId,
        track_keys: Vec<TrackKey>,
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
        source_id: SourceId,
        job_id: String,
    },
    ClearJob {
        source_id: SourceId,
        job_id: String,
    },
    SetPaused(bool),
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
    source: Option<Weak<Source>>,
    folder: Option<FolderKey>,
    directory: Option<PathBuf>,
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
    quality: StreamQuality,
    total_tracks: usize,
    #[serde(default)]
    completed: Vec<TrackKey>,
    remaining: Vec<TrackKey>,
    state: DownloadQueueState,
}

#[derive(Debug, Deserialize, Serialize)]
struct QueueFile {
    version: u32,
    source_id: SourceId,
    jobs: Vec<DownloadJob>,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq)]
enum ReleasedDownloadSubject {
    Rule(DownloadRule),
    Track(String),
    Album(String),
    Artist(String),
    Genre(String),
    Mood(String),
    Playlist(String),
    SmartPlaylist(String),
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
    quality: StreamQuality,
    #[serde(rename = "total_tracks")]
    _total_tracks: usize,
    #[serde(default)]
    completed: Vec<String>,
    remaining: Vec<String>,
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
    source_id: &SourceId,
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
        source_id: source_id.clone(),
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

async fn load_queue(
    root: &Path,
    source_id: &SourceId,
    database: &Database,
    source_key: SourceKey,
    custom_directory: Option<&Path>,
) -> Result<Vec<DownloadJob>, String> {
    let directory = source_directory(root, source_id);
    let path = directory.join(QUEUE_FILE);
    let part = directory.join(QUEUE_PART_FILE);
    let (bytes, recovered) = match std::fs::read(&path) {
        Ok(bytes) => (bytes, false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => match std::fs::read(&part) {
            Ok(bytes) => (bytes, true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(format!("could not read {}: {error}", part.display())),
        },
        Err(error) => return Err(format!("could not read {}: {error}", path.display())),
    };
    let jobs = if let Ok(queue) = serde_json::from_slice::<QueueFile>(&bytes) {
        if queue.version != QUEUE_VERSION || queue.source_id != *source_id {
            return Err("the saved download queue does not match this source".to_string());
        }
        queue.jobs
    } else {
        let released = serde_json::from_slice::<ReleasedQueueFile>(&bytes)
            .map_err(|error| format!("could not decode {}: {error}", path.display()))?;
        if released.version != 1 || released.source_id != *source_id {
            return Err("the saved download queue does not match this source".to_string());
        }
        let cancellation = library::ReadCancellation::new();
        let mut rebound = Vec::new();
        for job in released.jobs {
            let Some(subject) =
                rebind_released_subject(database, source_key, job.subject, &cancellation).await?
            else {
                continue;
            };
            let mut completed = Vec::new();
            let mut remaining = Vec::new();
            for object_id in job.completed {
                if let Some(key) = database
                    .track_key_by_object(source_key, &object_id, &cancellation)
                    .await
                    .map_err(|error| error.to_string())?
                {
                    completed.push(key);
                }
            }
            for object_id in job.remaining {
                if let Some(key) = database
                    .track_key_by_object(source_key, &object_id, &cancellation)
                    .await
                    .map_err(|error| error.to_string())?
                {
                    migrate_released_staging(root, source_id, &object_id, key, custom_directory)
                        .await?;
                    remaining.push(key);
                }
            }
            if !remaining.is_empty() {
                rebound.push(DownloadJob {
                    id: job.id,
                    subject,
                    quality: job.quality,
                    total_tracks: completed.len() + remaining.len(),
                    completed,
                    remaining,
                    state: job.state,
                });
            }
        }
        persist_queue(root, source_id, &rebound).await?;
        rebound
    };
    if recovered && let Err(error) = std::fs::rename(&part, &path) {
        warn!(%error, path = %part.display(), "could not finish recovering the download queue");
    }
    Ok(jobs)
}

async fn rebind_released_subject(
    database: &Database,
    source: SourceKey,
    subject: ReleasedDownloadSubject,
    cancellation: &library::ReadCancellation,
) -> Result<Option<DownloadSubject>, String> {
    Ok(match subject {
        ReleasedDownloadSubject::Rule(rule) => Some(DownloadSubject::Rule(rule)),
        ReleasedDownloadSubject::Track(id) => database
            .track_key_by_object(source, &id, cancellation)
            .await
            .map_err(|error| error.to_string())?
            .map(DownloadSubject::Track),
        ReleasedDownloadSubject::Album(id) => database
            .album_key_by_object(source, &id, cancellation)
            .await
            .map_err(|error| error.to_string())?
            .map(DownloadSubject::Album),
        ReleasedDownloadSubject::Artist(id) => database
            .artist_key_by_object(source, &id, cancellation)
            .await
            .map_err(|error| error.to_string())?
            .map(DownloadSubject::Artist),
        ReleasedDownloadSubject::Genre(id) => database
            .genre_key_by_object(source, &id, cancellation)
            .await
            .map_err(|error| error.to_string())?
            .map(DownloadSubject::Genre),
        ReleasedDownloadSubject::Mood(id) => database
            .mood_key_by_object(source, &id, cancellation)
            .await
            .map_err(|error| error.to_string())?
            .map(DownloadSubject::Mood),
        ReleasedDownloadSubject::Playlist(id) => database
            .playlist_key_by_object(source, &id, cancellation)
            .await
            .map_err(|error| error.to_string())?
            .map(DownloadSubject::Playlist),
        ReleasedDownloadSubject::SmartPlaylist(id) => database
            .smart_playlist_key_by_object(source, &id, cancellation)
            .await
            .map_err(|error| error.to_string())?
            .map(DownloadSubject::SmartPlaylist),
        ReleasedDownloadSubject::Prepared { context_id, title } => {
            Some(DownloadSubject::Prepared { context_id, title })
        }
    })
}

async fn migrate_released_staging(
    root: &Path,
    source_id: &SourceId,
    track_object_id: &str,
    track_key: TrackKey,
    custom_directory: Option<&Path>,
) -> Result<(), String> {
    let (old_part, old_checkpoint) =
        released_staging_paths(root, source_id, track_object_id, custom_directory);
    let current = staging_paths(root, source_id, &track_key, custom_directory);
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
    source_id: SourceId,
    job_id: String,
    track_id: TrackKey,
    subject: DownloadSubject,
    paths: DownloadPaths,
    cancellation: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<Result<(), DownloadFailure>>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub enum DownloadRule {
    EntireLibrary,
    Favorites,
    AllPlaylists,
    LatestFiveAlbums,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
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
pub struct SourceDownloadSettings {
    pub source_id: SourceId,
    #[serde(flatten)]
    pub rules: DownloadRules,
    #[serde(default)]
    pub quality: StreamQuality,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<PathBuf>,
}

impl SourceDownloadSettings {
    pub fn for_source(source_id: SourceId) -> Self {
        Self {
            source_id,
            rules: DownloadRules::default(),
            quality: StreamQuality::Original,
            directory: None,
        }
    }

    pub fn is_default(&self) -> bool {
        self.rules.is_empty() && self.quality == StreamQuality::Original && self.directory.is_none()
    }
}

async fn rule_track_ids(
    database: &Database,
    source: SourceKey,
    folder: Option<FolderKey>,
    rule: DownloadRule,
) -> library::LibraryResult<Vec<TrackKey>> {
    let cancellation = library::ReadCancellation::new();
    match rule {
        DownloadRule::EntireLibrary => {
            database
                .track_order(
                    source,
                    folder,
                    false,
                    TrackSort::Title,
                    false,
                    &cancellation,
                )
                .await
        }
        DownloadRule::Favorites => {
            database
                .track_order(source, folder, true, TrackSort::Title, false, &cancellation)
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

type PreparedRules = Result<Vec<(DownloadRule, Vec<TrackKey>)>, String>;

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
            match rule_track_ids(&database, source_key, folder, rule).await {
                Ok(track_keys) => result.push((rule, track_keys)),
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
pub enum DownloadSubject {
    Rule(DownloadRule),
    Track(TrackKey),
    Album(AlbumKey),
    Artist(ArtistKey),
    Genre(GenreKey),
    Mood(MoodKey),
    Playlist(PlaylistKey),
    SmartPlaylist(SmartPlaylistKey),
    Prepared {
        context_id: String,
        title: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DownloadQueueState {
    Queued,
    Downloading,
    WaitingForConnection,
    NeedsAttention,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DownloadQueueItem {
    pub id: String,
    pub source_id: SourceId,
    pub subject: DownloadSubject,
    pub quality: StreamQuality,
    pub completed_tracks: usize,
    pub total_tracks: usize,
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
    pub item_count: usize,
    pub kind: DownloadFeedbackKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DownloadEvent {
    Queue {
        source_id: SourceId,
        snapshot: Arc<DownloadQueueSnapshot>,
    },
    Feedback(DownloadFeedback),
    Notice(String),
}

struct Actor {
    root: Arc<PathBuf>,
    database: Database,
    events: Sender<DownloadEvent>,
    transfers: Arc<TransferClients>,
    prepared_rules: Sender<PreparedRules>,
    attached: HashMap<SourceId, AttachedSource>,
    selected: Option<SourceId>,
    settings: HashMap<SourceId, SourceDownloadSettings>,
    running_rules: Option<RuleIntent>,
    pending_rules: Option<RuleIntent>,
    jobs: HashMap<SourceId, Vec<DownloadJob>>,
    paused: bool,
    next_job: u64,
}

impl Downloads {
    pub fn new(
        root: PathBuf,
        database: Database,
        runtime: tokio::runtime::Handle,
        events: Sender<DownloadEvent>,
        settings: Vec<SourceDownloadSettings>,
    ) -> Self {
        let (commands, receiver) = async_channel::unbounded();
        let (prepared_rules, rule_results) = async_channel::unbounded();
        let downloads = Self {
            root: Arc::new(root),
            commands,
        };
        runtime.spawn(run(
            Actor {
                root: Arc::clone(&downloads.root),
                database,
                events,
                transfers: Arc::new(TransferClients::default()),
                prepared_rules,
                attached: HashMap::new(),
                selected: None,
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
    ) -> Result<(), String> {
        let (response, result) = async_channel::bounded(1);
        self.commands
            .send(Command::Attach {
                source_id,
                source_key,
                source: source.as_ref().map(Arc::downgrade),
                folder,
                response,
            })
            .await
            .map_err(|_| "download operation lane is unavailable".to_string())?;
        result
            .recv()
            .await
            .map_err(|_| "download attachment did not finish".to_string())?
    }

    pub fn download(
        &self,
        source_id: SourceId,
        subject: DownloadSubject,
        track_keys: Vec<TrackKey>,
    ) {
        self.send(Command::Download {
            source_id,
            subject,
            track_keys,
        });
    }

    pub fn remove(&self, source_id: SourceId, track_keys: Vec<TrackKey>, notify: bool) {
        self.send(Command::Remove {
            source_id,
            track_keys,
            notify,
        });
    }

    pub fn library_changed(&self, source_id: SourceId) {
        self.send(Command::LibraryChanged { source_id });
    }

    pub fn settings_changed(&self, settings: Vec<SourceDownloadSettings>) {
        self.send(Command::SettingsChanged(settings));
    }

    pub fn remove_rule(&self, source_id: SourceId, rule: DownloadRule, delete_downloads: bool) {
        self.send(Command::RemoveRule {
            source_id,
            rule,
            delete_downloads,
        });
    }

    pub fn cancel(&self, source_id: SourceId, job_id: String) {
        self.send(Command::Cancel { source_id, job_id });
    }

    pub fn clear_job(&self, source_id: SourceId, job_id: String) {
        self.send(Command::ClearJob { source_id, job_id });
    }

    pub fn set_paused(&self, paused: bool) {
        self.send(Command::SetPaused(paused));
    }

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

    pub fn clear(&self, source_id: SourceId, notify: bool) {
        self.send(Command::Clear { source_id, notify });
    }

    fn send(&self, command: Command) {
        if self.commands.try_send(command).is_err() {
            warn!("download operation lane is unavailable");
        }
    }
}

async fn run(mut actor: Actor, receiver: Receiver<Command>, rule_results: Receiver<PreparedRules>) {
    let mut retry = tokio::time::interval(RETRY_INTERVAL);
    retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut active = Vec::new();
    loop {
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
            Command::Attach {
                source_id,
                source_key,
                source,
                folder,
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
                    download.source_id == source_id && (source_changed || directory_changed)
                })
                .await;
                let result = self
                    .attach(source_id.clone(), source_key, source, folder, directory)
                    .await;
                let ready = result.is_ok();
                self.selected = Some(source_id.clone());
                self.pending_rules = None;
                let _ = response.send(result).await;
                if ready {
                    self.reconcile_all_rules(&source_id);
                }
            }
            Command::Download {
                source_id,
                subject,
                track_keys,
            } => {
                if self.selected.as_ref() != Some(&source_id)
                    || !self.attached.contains_key(&source_id)
                {
                    warn!("ignored a download from an inactive source");
                    return;
                }
                let quality = self.settings_for(&source_id).quality;
                self.enqueue(source_id, subject, quality, track_keys).await;
            }
            Command::Remove {
                source_id,
                track_keys,
                notify,
            } => {
                if !self.attached.contains_key(&source_id) {
                    warn!("ignored download removal from an inactive source");
                    return;
                }
                let remove = track_keys.iter().collect::<HashSet<_>>();
                self.abort_matching(active, false, |download| {
                    download.source_id == source_id && remove.contains(&download.track_id)
                })
                .await;
                self.force_remove(&source_id, track_keys, notify).await;
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
                    download.source_id == source_id
                        && download.subject == DownloadSubject::Rule(rule)
                })
                .await;
                self.remove_rule(&source_id, rule, delete_downloads).await;
            }
            Command::Cancel { source_id, job_id } => {
                self.cancel(&source_id, &job_id, active).await;
            }
            Command::ClearJob { source_id, job_id } => {
                self.clear_job(&source_id, &job_id, active).await;
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
                self.abort_matching(active, false, |download| download.source_id == source_id)
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
        self.settings = settings
            .into_iter()
            .map(|settings| (settings.source_id.clone(), settings))
            .collect::<HashMap<_, _>>();
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
                self.abort_matching(active, false, |download| &download.source_id == source_id)
                    .await;
                self.discard_previous_directory(source_id, &directory).await;
                if let Some(attached) = self.attached.get_mut(source_id) {
                    attached.directory = directory;
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
        if self.selected.as_ref() != Some(source_id) {
            return;
        }
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
        let current = self.selected.as_ref() == Some(&source_id)
            && self.attached[&source_id].folder == intent.folder;
        if !superseded && current {
            match prepared {
                Ok(prepared) => {
                    let quality = self.settings_for(&source_id).quality;
                    for (rule, track_ids) in prepared {
                        self.reconcile_rule(source_id.clone(), rule, quality, track_ids, active)
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
        directory: Option<PathBuf>,
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
                source: source.clone(),
                folder,
                directory: directory.clone(),
            },
        );
        if unchanged && self.jobs.contains_key(&source_id) {
            self.retry_waiting();
            self.publish(&source_id).await;
            return Ok(());
        }
        let mut attachment_error = None;
        match attach_downloaded_files(
            &self.root,
            &self.database,
            source_key,
            &source_id,
            directory.as_deref(),
        )
        .await
        {
            Ok(stale) => {
                for paths in stale {
                    if let Err(error) = remove_download_files(&paths).await {
                        attachment_error.get_or_insert(error);
                    }
                }
            }
            Err(error) => attachment_error = Some(error),
        }
        let source_available = source.as_ref().and_then(Weak::upgrade).is_some();
        let mut jobs = if let Some(jobs) = self.jobs.get(&source_id) {
            jobs.clone()
        } else {
            match load_queue(
                &self.root,
                &source_id,
                &self.database,
                source_key,
                directory.as_deref(),
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
        let cancellation = library::ReadCancellation::new();
        let mut current = HashSet::new();
        let queued = jobs
            .iter()
            .flat_map(|job| job.remaining.iter().copied())
            .collect::<Vec<_>>();
        for page in queued.chunks(256) {
            if let Ok(rows) = self
                .database
                .track_rows(source_key, page, &cancellation)
                .await
            {
                current.extend(rows.into_iter().map(|track| track.track_key));
            }
        }
        jobs.retain_mut(|job| {
            job.remaining
                .retain(|track_key| current.contains(track_key));
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
        if let Err(error) =
            cleanup_staging(&self.root, &source_id, directory.as_deref(), &queued_tracks).await
        {
            attachment_error.get_or_insert_with(|| error.to_string());
        }
        self.jobs.insert(source_id.clone(), jobs);
        self.persist_and_publish(&source_id).await;
        attachment_error.map_or(Ok(()), Err)
    }

    async fn enqueue(
        &mut self,
        source_id: SourceId,
        subject: DownloadSubject,
        quality: StreamQuality,
        track_ids: Vec<TrackKey>,
    ) {
        let Some(attached) = self.attached.get(&source_id) else {
            warn!(%source_id, "ignored a download for an unattached source");
            return;
        };
        let source_available = attached.source.as_ref().and_then(Weak::upgrade).is_some();
        let custom_directory = attached.directory.clone();
        let can_start = !self.paused && source_available;

        let mut seen = HashSet::new();
        let track_ids = track_ids
            .into_iter()
            .filter(|track_id| seen.insert(track_id.clone()))
            .collect::<Vec<_>>();
        if track_ids.is_empty() {
            return;
        }

        let owner = DownloadOwner::Subject(subject.clone());
        let mut completed = Vec::new();
        let mut remaining = Vec::new();
        for track_id in &track_ids {
            match add_owner_to_existing_download(
                &self.root,
                &source_id,
                track_id,
                &owner,
                custom_directory.as_deref(),
            )
            .await
            {
                Ok(true) => completed.push(track_id.clone()),
                Ok(false) => remaining.push(track_id.clone()),
                Err(error) => {
                    warn!(%error, %source_id, %track_id, "could not update download ownership");
                    remaining.push(track_id.clone());
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
                let completed_now = completed.iter().cloned().collect::<HashSet<_>>();
                existing
                    .remaining
                    .retain(|track_id| !completed_now.contains(track_id));
                let mut known = existing
                    .completed
                    .iter()
                    .chain(&existing.remaining)
                    .cloned()
                    .collect::<HashSet<_>>();
                existing.completed.extend(
                    completed
                        .into_iter()
                        .filter(|track_id| known.insert(track_id.clone())),
                );
                let additions = remaining
                    .into_iter()
                    .filter(|track_id| known.insert(track_id.clone()))
                    .collect::<Vec<_>>();
                scheduled_tracks = additions.len();
                existing.remaining.extend(additions);
                existing.total_tracks = existing.completed.len() + existing.remaining.len();
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
                    id: job_id(&source_id, &subject, self.next_job),
                    subject: subject.clone(),
                    quality,
                    total_tracks: track_ids.len(),
                    completed,
                    remaining,
                    state: if source_available {
                        DownloadQueueState::Queued
                    } else {
                        DownloadQueueState::WaitingForConnection
                    },
                });
            }
        }

        self.persist_and_publish(&source_id).await;
        if scheduled_tracks > 0 {
            let _ = self
                .events
                .send(DownloadEvent::Feedback(DownloadFeedback {
                    subject,
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

    async fn reconcile_rule(
        &mut self,
        source_id: SourceId,
        rule: DownloadRule,
        quality: StreamQuality,
        track_ids: Vec<TrackKey>,
        active: &mut Vec<ActiveDownload>,
    ) {
        let Some(attached) = self.attached.get(&source_id).cloned() else {
            warn!(%source_id, "ignored download reconciliation for an unattached source");
            return;
        };
        let source_available = attached.source.as_ref().and_then(Weak::upgrade).is_some();
        let can_start = !self.paused && source_available;

        let mut seen = HashSet::new();
        let track_ids = track_ids
            .into_iter()
            .filter(|track_id| seen.insert(track_id.clone()))
            .collect::<Vec<_>>();
        let desired = track_ids.iter().cloned().collect::<HashSet<_>>();
        let subject = DownloadSubject::Rule(rule);
        let quality_changed = self.jobs.get(&source_id).is_some_and(|jobs| {
            jobs.iter()
                .any(|job| job.subject == subject && job.quality != quality)
        });
        self.abort_matching(active, true, |download| {
            download.source_id == source_id
                && download.subject == subject
                && (quality_changed || !desired.contains(&download.track_id))
        })
        .await;
        let active_job_id = active
            .iter()
            .find(|download| download.source_id == source_id && download.subject == subject)
            .map(|download| download.job_id.clone());
        let owner = DownloadOwner::Subject(subject.clone());

        let custom_directory = self
            .attached
            .get(&source_id)
            .and_then(|attached| attached.directory.as_deref());
        let records = match load_download_records(&self.root, &source_id, custom_directory) {
            Ok(records) => records,
            Err(error) => {
                warn!(%error, %source_id, "could not read rule downloads");
                return;
            }
        };
        for (track_id, mut record) in records {
            if desired.contains(&track_id) || !record.owners.remove(&owner) {
                continue;
            }
            record.owners.extend(self.queued_owners_for_track(
                &source_id,
                &track_id,
                Some(&subject),
            ));
            let Ok(paths) =
                record_download_paths(&self.root, &source_id, &record, custom_directory)
            else {
                continue;
            };
            if record.owners.is_empty() {
                if let Err(error) = remove_download_files(&paths).await {
                    warn!(%error, %source_id, %track_id, "could not remove stale rule download");
                    continue;
                }
                self.remove_download_access(&source_id, track_id, &paths)
                    .await;
            } else if let Err(error) = write_record(&paths, &record).await {
                warn!(%error, %source_id, %track_id, "could not update rule ownership");
            }
        }

        let mut completed = Vec::new();
        let mut remaining = Vec::new();
        for track_id in &track_ids {
            match add_owner_to_existing_download(
                &self.root,
                &source_id,
                track_id,
                &owner,
                custom_directory,
            )
            .await
            {
                Ok(true) => completed.push(track_id.clone()),
                Ok(false) => remaining.push(track_id.clone()),
                Err(error) => {
                    warn!(%error, %source_id, %track_id, "could not update download ownership");
                    remaining.push(track_id.clone());
                }
            }
        }

        let jobs = self.jobs.entry(source_id.clone()).or_default();
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
            .filter(|track_id| quality_changed || !old_remaining.contains(*track_id))
            .count();
        if !remaining.is_empty() {
            let id = existing_id.unwrap_or_else(|| {
                self.next_job = self.next_job.wrapping_add(1);
                job_id(&source_id, &subject, self.next_job)
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
                total_tracks: track_ids.len(),
                completed,
                remaining,
                state,
            };
            jobs.insert(existing_index.unwrap_or(jobs.len()).min(jobs.len()), job);
        }
        self.reconcile_staging(&source_id).await;

        self.persist_and_publish(&source_id).await;
        if scheduled_tracks > 0 {
            let _ = self
                .events
                .send(DownloadEvent::Feedback(DownloadFeedback {
                    subject,
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
            let (source_id, job_id, subject, quality, track_id, state) =
                self.next_candidate(active)?;
            let Some(attached) = self.attached.get(&source_id).cloned() else {
                self.jobs.remove(&source_id);
                self.persist_and_publish(&source_id).await;
                continue;
            };
            let Some(source) = attached.source.as_ref().and_then(Weak::upgrade) else {
                if let Some(job) = self.find_job_mut(&source_id, &job_id) {
                    job.state = DownloadQueueState::WaitingForConnection;
                }
                self.persist_and_publish(&source_id).await;
                continue;
            };
            let cancellation = library::ReadCancellation::new();
            let Some(track) = self
                .database
                .track_rows(attached.source_key, &[track_id], &cancellation)
                .await
                .ok()
                .and_then(|mut rows| rows.pop())
            else {
                self.remove_job_track(&source_id, &job_id, &track_id, false);
                self.persist_and_publish(&source_id).await;
                continue;
            };
            let owner = DownloadOwner::Subject(subject.clone());
            match add_owner_to_existing_download(
                &self.root,
                &source_id,
                &track_id,
                &owner,
                attached.directory.as_deref(),
            )
            .await
            {
                Ok(true) => {
                    self.remove_job_track(&source_id, &job_id, &track_id, true);
                    self.persist_and_publish(&source_id).await;
                    continue;
                }
                Ok(false) => {}
                Err(error) => {
                    warn!(%error, %source_id, %track_id, "could not update download ownership");
                    if let Some(job) = self.find_job_mut(&source_id, &job_id) {
                        job.state = DownloadQueueState::NeedsAttention;
                    }
                    self.persist_and_publish(&source_id).await;
                    continue;
                }
            }
            let media = playback::PlaybackMedia::from(track.clone());
            let request = StreamRequest::for_media(&media, quality);
            let resolved = source.resolve_download(&request);
            let (transcoded_extension, transfer) = match resolved {
                Ok(download) => (download.transcoded_extension(), Ok(download.into_stream())),
                Err(error) => (None, Err(download_source_failure(error))),
            };
            let paths = new_download_paths(
                &self.root,
                &source_id,
                &track,
                attached.directory.as_deref(),
                transcoded_extension,
            );
            let entering_download = state != DownloadQueueState::Downloading;
            if let Some(job) = self.find_job_mut(&source_id, &job_id) {
                job.state = DownloadQueueState::Downloading;
            }
            if entering_download {
                self.persist_and_publish(&source_id).await;
            } else {
                self.publish(&source_id).await;
            }
            let task_paths = paths.clone();
            let transfers = Arc::clone(&self.transfers);
            let (cancellation, cancelled) = tokio::sync::oneshot::channel();
            let task = match transfer {
                Ok(stream) => tokio::spawn(download_track(
                    source_id.clone(),
                    request,
                    stream,
                    task_paths,
                    transfers,
                    cancelled,
                )),
                Err(error) => {
                    drop(cancelled);
                    tokio::spawn(async move { Err(error) })
                }
            };
            return Some(ActiveDownload {
                source_id,
                job_id,
                track_id,
                subject,
                paths,
                cancellation: Some(cancellation),
                task,
            });
        }
    }

    fn next_candidate(
        &self,
        active: &[ActiveDownload],
    ) -> Option<(
        SourceId,
        String,
        DownloadSubject,
        StreamQuality,
        TrackKey,
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
                let track_id = job.remaining.iter().find(|track_id| {
                    !active.iter().any(|download| {
                        download.source_id == *source_id && &download.track_id == *track_id
                    })
                });
                if let Some(track_id) = track_id {
                    return Some((
                        source_id.clone(),
                        job.id.clone(),
                        job.subject.clone(),
                        job.quality,
                        track_id.clone(),
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
            track_id,
            subject,
            paths,
            ..
        } = active;
        let result = match joined {
            Ok(Ok(())) => {
                self.commit_transfer(&source_id, &track_id, &subject, &paths)
                    .await
            }
            Ok(Err(error)) => Err(error),
            Err(error) => Err(DownloadFailure::NeedsAttention(format!(
                "download task failed: {error}"
            ))),
        };
        match result {
            Ok(()) => {
                self.remove_job_track(&source_id, &job_id, &track_id, true);
            }
            Err(DownloadFailure::Item(error)) => {
                warn!(%error, %source_id, %track_id, "could not download track");
                self.remove_job_track(&source_id, &job_id, &track_id, false);
            }
            Err(DownloadFailure::Retry(error)) => {
                warn!(%error, %source_id, "download is waiting for the server");
                self.abort_matching(remaining_active, true, |download| {
                    download.source_id == source_id && download.job_id == job_id
                })
                .await;
                if let Some(job) = self.find_job_mut(&source_id, &job_id) {
                    job.state = DownloadQueueState::WaitingForConnection;
                }
            }
            Err(DownloadFailure::NeedsAttention(error)) => {
                warn!(%error, %source_id, "download needs attention");
                self.abort_matching(remaining_active, true, |download| {
                    download.source_id == source_id && download.job_id == job_id
                })
                .await;
                if let Some(job) = self.find_job_mut(&source_id, &job_id) {
                    job.state = DownloadQueueState::NeedsAttention;
                }
            }
        }
        self.persist_and_publish(&source_id).await;
    }

    async fn commit_transfer(
        &self,
        source_id: &SourceId,
        track_id: &TrackKey,
        subject: &DownloadSubject,
        paths: &DownloadPaths,
    ) -> Result<(), DownloadFailure> {
        finalize_download(
            paths,
            source_id.clone(),
            track_id.clone(),
            DownloadOwner::Subject(subject.clone()),
        )
        .await
        .map_err(DownloadFailure::NeedsAttention)?;
        if let Some(attached) = self.attached.get(source_id) {
            let cancellation = library::ReadCancellation::new();
            if let Some(track) = self
                .database
                .track_rows(attached.source_key, &[*track_id], &cancellation)
                .await
                .map_err(|error| DownloadFailure::NeedsAttention(error.to_string()))?
                .pop()
            {
                let metadata = std::fs::metadata(&paths.audio)
                    .map_err(|error| DownloadFailure::NeedsAttention(error.to_string()))?;
                let (storage_root, relative_path) =
                    local_access_projection(paths).map_err(DownloadFailure::NeedsAttention)?;
                self.database
                    .upsert_local_access(
                        attached.source_key,
                        &library::LocalAccessWrite {
                            track_object_id: Some(track.object_id),
                            origin: library::LocalAccessOrigin::Download,
                            path: paths.audio.to_string_lossy().into_owned(),
                            root: storage_root.to_string_lossy().into_owned(),
                            relative_path: relative_path.to_string_lossy().into_owned(),
                            size_bytes: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
                            mtime_ns: metadata
                                .modified()
                                .ok()
                                .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
                                .map_or(0, |value| {
                                    i64::try_from(value.as_nanos()).unwrap_or(i64::MAX)
                                }),
                            device_id: None,
                            inode: None,
                            parser_version: RECORD_VERSION as i64,
                            title: track.title,
                            album: track.display_album,
                            artist: track.display_artist,
                            disc_number: track.disc_number,
                            track_number: track.track_number,
                            duration_millis: track.duration_millis,
                            media_uri: format!("file://{}", paths.audio.to_string_lossy()),
                            loudness_analysis_key: track.loudness_analysis_key,
                        },
                    )
                    .await
                    .map_err(|error| DownloadFailure::NeedsAttention(error.to_string()))?;
            }
        }
        Ok(())
    }

    async fn abort_matching(
        &mut self,
        active: &mut Vec<ActiveDownload>,
        preserve: bool,
        matches: impl Fn(&ActiveDownload) -> bool,
    ) -> Vec<TrackKey> {
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
    ) -> Vec<TrackKey> {
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
                match self
                    .commit_transfer(
                        &download.source_id,
                        &download.track_id,
                        &download.subject,
                        &download.paths,
                    )
                    .await
                {
                    Ok(()) => {
                        completed_tracks.push(download.track_id.clone());
                        self.remove_job_track(
                            &download.source_id,
                            &download.job_id,
                            &download.track_id,
                            true,
                        );
                    }
                    Err(DownloadFailure::NeedsAttention(error)) => {
                        warn!(
                            %error,
                            source_id = %download.source_id,
                            track_id = %download.track_id,
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
                    source_id = %download.source_id,
                    track_id = %download.track_id,
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

    async fn reconcile_staging(&self, source_id: &SourceId) {
        let queued = self
            .jobs
            .get(source_id)
            .into_iter()
            .flatten()
            .flat_map(|job| job.remaining.iter().cloned())
            .collect::<HashSet<_>>();
        let directory = self
            .attached
            .get(source_id)
            .and_then(|attached| attached.directory.as_deref());
        if let Err(error) = cleanup_staging(&self.root, source_id, directory, &queued).await {
            warn!(%error, %source_id, "could not reconcile download staging");
        }
    }

    async fn discard_previous_directory(&self, source_id: &SourceId, directory: &Option<PathBuf>) {
        let Some(attached) = self.attached.get(source_id) else {
            return;
        };
        if attached.directory == *directory {
            return;
        }
        if let Err(error) = cleanup_staging(
            &self.root,
            source_id,
            attached.directory.as_deref(),
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
        track_id: &TrackKey,
        excluded_subject: Option<&DownloadSubject>,
    ) -> HashSet<DownloadOwner> {
        self.jobs
            .get(source_id)
            .into_iter()
            .flatten()
            .filter(|job| excluded_subject != Some(&job.subject))
            .filter(|job| job.remaining.contains(track_id))
            .map(|job| DownloadOwner::Subject(job.subject.clone()))
            .collect()
    }

    fn find_job_mut(&mut self, source_id: &SourceId, job_id: &str) -> Option<&mut DownloadJob> {
        self.jobs
            .get_mut(source_id)?
            .iter_mut()
            .find(|job| job.id == job_id)
    }

    fn remove_job_track(
        &mut self,
        source_id: &SourceId,
        job_id: &str,
        track_id: &TrackKey,
        completed: bool,
    ) {
        let Some(jobs) = self.jobs.get_mut(source_id) else {
            return;
        };
        let Some(job_index) = jobs.iter().position(|job| job.id == job_id) else {
            return;
        };
        let job = &mut jobs[job_index];
        job.remaining.retain(|candidate| candidate != track_id);
        if completed {
            job.completed.push(track_id.clone());
        }
        if job.remaining.is_empty() {
            jobs.remove(job_index);
        } else {
            job.state = DownloadQueueState::Downloading;
        }
    }

    fn retry_waiting(&mut self) {
        for (source_id, jobs) in &mut self.jobs {
            let attached = self.attached.get(source_id);
            let available = attached.is_some_and(|attached| {
                attached
                    .source
                    .as_ref()
                    .is_some_and(|source| source.strong_count() > 0)
            });
            if let Some(first) = jobs.first_mut()
                && available
                && first.state == DownloadQueueState::WaitingForConnection
            {
                first.state = DownloadQueueState::Queued;
            }
        }
    }

    async fn force_remove(&mut self, source_id: &SourceId, track_ids: Vec<TrackKey>, notify: bool) {
        let remove = track_ids.iter().cloned().collect::<HashSet<_>>();
        for job in self.jobs.entry(source_id.clone()).or_default().iter_mut() {
            job.remaining.retain(|track_id| !remove.contains(track_id));
            job.completed.retain(|track_id| !remove.contains(track_id));
            job.total_tracks = job.remaining.len() + job.completed.len();
        }
        self.jobs
            .entry(source_id.clone())
            .or_default()
            .retain(|job| !job.remaining.is_empty());
        self.reconcile_staging(source_id).await;

        let (removed, failed) = self.delete_downloads(source_id, track_ids).await;
        self.persist_and_publish(source_id).await;
        if notify {
            self.send_removal_notice(removed, failed).await;
        }
    }

    async fn delete_downloads(
        &self,
        source_id: &SourceId,
        track_ids: impl IntoIterator<Item = TrackKey>,
    ) -> (usize, usize) {
        let mut removed = 0usize;
        let mut failed = 0usize;
        let custom_directory = self
            .attached
            .get(source_id)
            .and_then(|attached| attached.directory.as_deref());
        let records =
            load_download_records(&self.root, source_id, custom_directory).unwrap_or_default();
        for track_id in track_ids {
            let paths = records
                .get(&track_id)
                .and_then(|record| {
                    record_download_paths(&self.root, source_id, record, custom_directory).ok()
                })
                .unwrap_or_else(|| download_paths(&self.root, source_id, &track_id));
            match remove_download_files(&paths).await {
                Ok(was_present) => {
                    self.remove_download_access(source_id, track_id, &paths)
                        .await;
                    removed += usize::from(was_present);
                }
                Err(error) => {
                    failed += 1;
                    warn!(%error, %source_id, %track_id, "could not remove downloaded track");
                }
            }
        }
        (removed, failed)
    }

    async fn remove_download_access(
        &self,
        source_id: &SourceId,
        track_key: TrackKey,
        paths: &DownloadPaths,
    ) {
        let Some(attached) = self.attached.get(source_id) else {
            return;
        };
        let cancellation = library::ReadCancellation::new();
        let Some(track) = self
            .database
            .track_rows(attached.source_key, &[track_key], &cancellation)
            .await
            .ok()
            .and_then(|mut rows| rows.pop())
        else {
            return;
        };
        let Ok(Some(access)) = self
            .database
            .resolve_local_access(
                attached.source_key,
                Some(&track.object_id),
                &track.title,
                &track.display_album,
                &track.display_artist,
                track.disc_number,
                track.track_number,
                track.duration_millis,
            )
            .await
        else {
            return;
        };
        if Path::new(&access.path) == paths.audio {
            let _ = self
                .database
                .remove_local_access(attached.source_key, access.local_access_file_key)
                .await;
        }
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
        self.jobs
            .entry(source_id.clone())
            .or_default()
            .retain(|job| job.subject != subject);
        self.reconcile_staging(source_id).await;
        self.release_owner(source_id, &subject, None, !delete_downloads)
            .await;
        self.persist_and_publish(source_id).await;
    }

    async fn cancel(
        &mut self,
        source_id: &SourceId,
        job_id: &str,
        active: &mut Vec<ActiveDownload>,
    ) {
        let exists = self
            .jobs
            .get(source_id)
            .is_some_and(|jobs| jobs.iter().any(|job| job.id == job_id));
        if !exists {
            return;
        }
        self.abort_matching(active, true, |download| {
            download.source_id == *source_id && download.job_id == job_id
        })
        .await;
        self.jobs
            .entry(source_id.clone())
            .or_default()
            .retain(|job| job.id != job_id);
        self.reconcile_staging(source_id).await;
        self.persist_and_publish(source_id).await;
    }

    async fn clear_job(
        &mut self,
        source_id: &SourceId,
        job_id: &str,
        active: &mut Vec<ActiveDownload>,
    ) {
        let Some(job) = self
            .jobs
            .get(source_id)
            .and_then(|jobs| jobs.iter().find(|job| job.id == job_id))
            .cloned()
        else {
            return;
        };
        let completed = job
            .completed
            .into_iter()
            .chain(
                self.abort_matching(active, true, |download| {
                    download.source_id == *source_id && download.job_id == job_id
                })
                .await,
            )
            .collect::<HashSet<_>>();
        let subject = job.subject;
        self.jobs
            .entry(source_id.clone())
            .or_default()
            .retain(|job| job.id != job_id);
        self.reconcile_staging(source_id).await;
        self.release_owner(source_id, &subject, Some(&completed), false)
            .await;
        self.persist_and_publish(source_id).await;
    }

    async fn release_owner(
        &self,
        source_id: &SourceId,
        subject: &DownloadSubject,
        track_ids: Option<&HashSet<TrackKey>>,
        retain: bool,
    ) {
        if track_ids.is_some_and(HashSet::is_empty) {
            return;
        }
        let custom_directory = self
            .attached
            .get(source_id)
            .and_then(|attached| attached.directory.as_deref());
        let records = match load_download_records(&self.root, source_id, custom_directory) {
            Ok(records) => records,
            Err(error) => {
                warn!(%error, %source_id, "could not read download ownership");
                return;
            }
        };
        let owner = DownloadOwner::Subject(subject.clone());
        for (track_id, mut record) in records {
            if track_ids.is_some_and(|track_ids| !track_ids.contains(&track_id)) {
                continue;
            }
            if !record.owners.remove(&owner) {
                continue;
            }
            if retain {
                record.owners.insert(DownloadOwner::Retained);
            }
            record
                .owners
                .extend(self.queued_owners_for_track(source_id, &track_id, None));
            let Ok(paths) = record_download_paths(&self.root, source_id, &record, custom_directory)
            else {
                continue;
            };
            if record.owners.is_empty() {
                if let Err(error) = remove_download_files(&paths).await {
                    warn!(%error, %source_id, %track_id, "could not remove unowned download");
                    continue;
                }
                self.remove_download_access(source_id, track_id, &paths)
                    .await;
            } else if let Err(error) = write_record(&paths, &record).await {
                warn!(%error, %source_id, %track_id, "could not update download ownership");
            }
        }
    }

    async fn move_job(
        &mut self,
        source_id: &SourceId,
        job_id: &str,
        target_job_id: &str,
        after: bool,
    ) {
        let changed = reorder_jobs(
            self.jobs.entry(source_id.clone()).or_default(),
            job_id,
            target_job_id,
            after,
        );
        if changed {
            self.persist_and_publish(source_id).await;
        } else {
            self.publish(source_id).await;
        }
    }

    async fn clear(&mut self, source_id: &SourceId, notify: bool) {
        let staging_directory = self
            .attached
            .get(source_id)
            .and_then(|attached| attached.directory.clone());
        self.jobs.remove(source_id);
        let directory = source_directory(&self.root, source_id);
        let result = async {
            cleanup_staging(
                &self.root,
                source_id,
                staging_directory.as_deref(),
                &HashSet::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
            for record in
                load_download_records(&self.root, source_id, staging_directory.as_deref())?
            {
                let paths = record_download_paths(
                    &self.root,
                    source_id,
                    &record.1,
                    staging_directory.as_deref(),
                )?;
                self.remove_download_access(source_id, record.0, &paths)
                    .await;
                remove_download_files(&paths).await?;
            }
            match tokio::fs::remove_dir_all(&directory).await {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(format!("could not remove {}: {error}", directory.display())),
            }
        }
        .await;
        match result {
            Ok(()) => {
                self.publish(source_id).await;
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

    async fn persist_and_publish(&self, source_id: &SourceId) {
        if let Err(error) = persist_queue(
            &self.root,
            source_id,
            self.jobs.get(source_id).map(Vec::as_slice).unwrap_or(&[]),
        )
        .await
        {
            warn!(%error, %source_id, "could not save the download queue");
        }
        self.publish(source_id).await;
    }

    async fn publish_all(&self) {
        let source_ids = self.jobs.keys().cloned().collect::<Vec<_>>();
        for source_id in source_ids {
            self.publish(&source_id).await;
        }
    }

    async fn publish(&self, source_id: &SourceId) {
        let custom_directory = self
            .attached
            .get(source_id)
            .and_then(|attached| attached.directory.as_deref());
        let downloaded_tracks = load_download_records(&self.root, source_id, custom_directory)
            .map_or(0, |records| records.len());
        let jobs = self
            .jobs
            .get(source_id)
            .into_iter()
            .flatten()
            .map(|job| DownloadQueueItem {
                id: job.id.clone(),
                source_id: source_id.clone(),
                subject: job.subject.clone(),
                quality: job.quality,
                completed_tracks: job.completed.len(),
                total_tracks: job.total_tracks,
                state: job.state,
            })
            .collect::<Vec<_>>();
        let _ = self
            .events
            .send(DownloadEvent::Queue {
                source_id: source_id.clone(),
                snapshot: Arc::new(DownloadQueueSnapshot {
                    jobs: jobs.into(),
                    downloaded_tracks,
                    paused: self.paused,
                }),
            })
            .await;
    }
}

async fn download_track(
    source_id: SourceId,
    request: StreamRequest,
    stream: ResolvedStream,
    paths: DownloadPaths,
    transfers: Arc<TransferClients>,
    cancellation: tokio::sync::oneshot::Receiver<()>,
) -> Result<(), DownloadFailure> {
    run_transfer(
        &source_id,
        &request,
        &stream,
        &paths,
        &transfers,
        cancellation,
    )
    .await
    .map_err(download_source_failure)
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

fn job_id(source_id: &SourceId, subject: &DownloadSubject, sequence: u64) -> String {
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
        let mut scan = library::Scan::begin(&database, source_id, "Source", "source", None)
            .await
            .expect("begin source scan");
        scan.write_track(
            track_object_id,
            None,
            "Track",
            "track artist album",
            "Album",
            "Artist",
            "track",
            180_000,
            1,
            1,
            None,
            None,
            None,
            Some("https://example.invalid/track"),
            Some("FLAC"),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            [7; 32],
        )
        .await
        .expect("stage Track");
        let publication = match scan.finish().await.expect("publish source") {
            library::ScanOutcome::Changed(publication) => publication,
            outcome => panic!("unexpected Scan outcome: {outcome:?}"),
        };
        let track = database
            .track_key_by_object(
                publication.source,
                track_object_id,
                &library::ReadCancellation::new(),
            )
            .await
            .expect("read Track key")
            .expect("Track exists");
        (directory, database, publication.source, track)
    }

    #[test]
    fn queued_track_keys_survive_restart_serialization() {
        let source_id = SourceId::new("source");
        let queue = QueueFile {
            version: QUEUE_VERSION,
            source_id: source_id.clone(),
            jobs: vec![DownloadJob {
                id: "job".to_string(),
                subject: DownloadSubject::Track(TrackKey::from_raw(7)),
                quality: StreamQuality::Original,
                total_tracks: 1,
                completed: Vec::new(),
                remaining: vec![TrackKey::from_raw(7)],
                state: DownloadQueueState::Queued,
            }],
        };
        let restored: QueueFile =
            serde_json::from_slice(&serde_json::to_vec(&queue).expect("encode Queue"))
                .expect("decode Queue");
        assert_eq!(restored.source_id, source_id);
        assert_eq!(restored.jobs[0].remaining, [TrackKey::from_raw(7)]);
    }

    #[tokio::test]
    async fn released_completed_and_partial_downloads_rebind_on_attach() {
        let (_database_directory, database, source_key, track_key) =
            download_fixture("source", "track-object").await;
        let downloads = tempfile::tempdir().expect("temporary Downloads root");
        let source_id = SourceId::new("source");
        let directory = source_directory(downloads.path(), &source_id);
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

        let stale =
            attach_downloaded_files(downloads.path(), &database, source_key, &source_id, None)
                .await
                .expect("attach released download");
        assert!(stale.is_empty());
        let records = load_download_records(downloads.path(), &source_id, None)
            .expect("load migrated records");
        let migrated = records.get(&track_key).expect("migrated record");
        assert_eq!(migrated.completed_size, Some(14));
        assert_eq!(
            migrated.relative_audio_path.as_deref(),
            audio.file_name().map(Path::new)
        );
        assert!(audio.is_file());

        let jobs = load_queue(downloads.path(), &source_id, &database, source_key, None)
            .await
            .expect("migrate released Queue");
        assert_eq!(jobs[0].remaining, [track_key]);
        let current = staging_paths(downloads.path(), &source_id, &track_key, None);
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
        let (_database_directory, database, source_key, track_key) =
            download_fixture("custom-source", "custom-track").await;
        let downloads = tempfile::tempdir().expect("temporary Downloads root");
        let custom = tempfile::tempdir().expect("configured custom root");
        let source_id = SourceId::new("custom-source");
        let directory = source_directory(downloads.path(), &source_id);
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

        let stale = attach_downloaded_files(
            downloads.path(),
            &database,
            source_key,
            &source_id,
            Some(custom.path()),
        )
        .await
        .expect("attach released custom download");
        assert!(stale.is_empty());
        let records = load_download_records(downloads.path(), &source_id, Some(custom.path()))
            .expect("load migrated custom record");
        let migrated = records.get(&track_key).expect("migrated custom record");
        assert!(migrated.custom_storage);
        assert!(
            migrated
                .owners
                .contains(&DownloadOwner::Subject(DownloadSubject::Rule(
                    DownloadRule::Favorites
                )))
        );
        assert_eq!(
            migrated.relative_audio_path.as_deref(),
            Some(relative.as_path())
        );
        assert_eq!(migrated.completed_size, Some(12));
        assert!(custom.path().join(relative).is_file());
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
            root: Arc::new(root.to_path_buf()),
            database,
            events,
            transfers: Arc::new(TransferClients::default()),
            prepared_rules,
            attached: HashMap::from([(
                source_id.clone(),
                AttachedSource {
                    source_key,
                    source: None,
                    folder: None,
                    directory: None,
                },
            )]),
            selected: Some(source_id.clone()),
            settings: HashMap::new(),
            running_rules: None,
            pending_rules: None,
            jobs: HashMap::new(),
            paused: false,
            next_job: 0,
        }
    }

    fn test_job(track: TrackKey) -> DownloadJob {
        DownloadJob {
            id: "job".to_string(),
            subject: DownloadSubject::Track(track),
            quality: StreamQuality::Original,
            total_tracks: 1,
            completed: Vec::new(),
            remaining: vec![track],
            state: DownloadQueueState::Downloading,
        }
    }

    #[tokio::test]
    async fn pausing_cancels_active_work_and_keeps_the_job_queued() {
        let (_library, database, source_key, track) = download_fixture("pause", "track").await;
        let root = tempfile::tempdir().expect("Downloads root");
        let source_id = SourceId::new("pause");
        let mut actor = actor_for_test(root.path(), database, &source_id, source_key);
        actor.jobs.insert(source_id.clone(), vec![test_job(track)]);
        let paths = download_paths(root.path(), &source_id, &track);
        let (cancel, cancelled) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = cancelled.await;
            Err(DownloadFailure::Retry("cancelled".to_string()))
        });
        let mut active = vec![ActiveDownload {
            source_id: source_id.clone(),
            job_id: "job".to_string(),
            track_id: track,
            subject: DownloadSubject::Track(track),
            paths,
            cancellation: Some(cancel),
            task,
        }];

        actor.apply(Command::SetPaused(true), &mut active).await;

        assert!(actor.paused);
        assert!(active.is_empty());
        assert_eq!(actor.jobs[&source_id][0].state, DownloadQueueState::Queued);
    }

    #[tokio::test]
    async fn cancelling_removes_the_job_and_reconciles_its_staging_file() {
        let (_library, database, source_key, track) = download_fixture("cancel", "track").await;
        let root = tempfile::tempdir().expect("Downloads root");
        let source_id = SourceId::new("cancel");
        let mut actor = actor_for_test(root.path(), database, &source_id, source_key);
        actor.jobs.insert(source_id.clone(), vec![test_job(track)]);
        let paths = download_paths(root.path(), &source_id, &track);
        std::fs::create_dir_all(paths.audio_part.parent().expect("staging parent"))
            .expect("create staging");
        std::fs::write(&paths.audio_part, b"partial").expect("write staging");
        let (cancel, cancelled) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = cancelled.await;
            Err(DownloadFailure::Retry("cancelled".to_string()))
        });
        let mut active = vec![ActiveDownload {
            source_id: source_id.clone(),
            job_id: "job".to_string(),
            track_id: track,
            subject: DownloadSubject::Track(track),
            paths: paths.clone(),
            cancellation: Some(cancel),
            task,
        }];

        actor.cancel(&source_id, "job", &mut active).await;

        assert!(actor.jobs[&source_id].is_empty());
        assert!(active.is_empty());
        assert!(!paths.audio_part.exists());
    }

    #[tokio::test]
    async fn stale_completed_transfer_cannot_commit_after_its_job_was_removed() {
        let (_library, database, source_key, track) = download_fixture("stale", "track").await;
        let root = tempfile::tempdir().expect("Downloads root");
        let source_id = SourceId::new("stale");
        let mut actor = actor_for_test(root.path(), database, &source_id, source_key);
        let paths = download_paths(root.path(), &source_id, &track);
        std::fs::create_dir_all(paths.audio_part.parent().expect("staging parent"))
            .expect("create staging");
        std::fs::write(&paths.audio_part, b"complete bytes").expect("write completed staging");
        let active = ActiveDownload {
            source_id: source_id.clone(),
            job_id: "removed".to_string(),
            track_id: track,
            subject: DownloadSubject::Track(track),
            paths: paths.clone(),
            cancellation: None,
            task: tokio::spawn(async { Ok(()) }),
        };

        actor.finish(active, Ok(Ok(())), &mut Vec::new()).await;

        assert!(!paths.audio.exists());
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
