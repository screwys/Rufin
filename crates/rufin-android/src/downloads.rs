use std::{collections::BTreeMap, sync::Arc};

use downloads::{DownloadEvent, DownloadQueueSnapshot, DownloadRule};
use rufin_core::{
    SettingsHandle,
    runtime::{ProductHandles, SourceHandle},
};

use crate::host::{AndroidError, error};

#[derive(uniffi::Record)]
pub struct AndroidTargetDownloadStatus {
    pub total: u64,
    pub downloaded: u64,
}

#[derive(Clone, uniffi::Record)]
pub struct AndroidDownloadJob {
    pub id: String,
    pub source_id: Option<String>,
    pub title: String,
    pub completed: u64,
    pub total: u64,
    pub state: String,
    pub artwork_identity: Option<Vec<u8>>,
    pub subtitle: String,
}

#[derive(Clone, uniffi::Record)]
pub struct AndroidDownloadQueue {
    pub source_id: Option<String>,
    pub downloaded_tracks: u64,
    pub total_jobs: u64,
}

#[derive(uniffi::Record)]
pub struct AndroidDownloadPage {
    pub total: u64,
    pub jobs: Vec<AndroidDownloadJob>,
}

#[derive(Clone, Default, uniffi::Record)]
pub struct AndroidDownloadsState {
    pub revision: u64,
    pub queue_revision: u64,
    pub paused: bool,
    pub queues: Vec<AndroidDownloadQueue>,
    pub notice: Option<String>,
}

#[derive(uniffi::Record)]
pub struct AndroidDownloadRule {
    pub id: String,
    pub title: String,
    pub enabled: bool,
}

#[derive(uniffi::Record)]
pub struct AndroidDownloadSource {
    pub id: String,
    pub name: String,
    pub directory: Option<String>,
    pub bitrate: Option<u32>,
    pub bitrate_limit: Option<u32>,
    pub rules: Vec<AndroidDownloadRule>,
}

#[derive(uniffi::Object)]
pub struct AndroidDownloads {
    downloads: downloads::Downloads,
    settings: SettingsHandle,
    source: SourceHandle,
    runtime: tokio::runtime::Handle,
    changes: tokio::sync::watch::Receiver<AndroidDownloadsState>,
    task: tokio::task::AbortHandle,
    queues: Arc<std::sync::Mutex<BTreeMap<Option<sources::SourceId>, Arc<DownloadQueueSnapshot>>>>,
    database: Arc<library::Database>,
}

fn rule_title(rule: DownloadRule) -> &'static str {
    match rule {
        DownloadRule::EntireLibrary => "Entire Library",
        DownloadRule::Favorites => "Favorites",
        DownloadRule::AllPlaylists => "All Playlists",
        DownloadRule::LatestFiveAlbums => "5 Latest Albums",
    }
}

fn rule_id(rule: DownloadRule) -> String {
    serde_json::to_value(rule)
        .expect("Download rule serialization")
        .as_str()
        .expect("Download rule name")
        .to_owned()
}

impl AndroidDownloads {
    pub(crate) fn new(
        products: &ProductHandles,
        events: async_channel::Receiver<DownloadEvent>,
    ) -> Self {
        let (updates, changes) = tokio::sync::watch::channel(AndroidDownloadsState::default());
        let queues = Arc::new(std::sync::Mutex::new(BTreeMap::<
            Option<sources::SourceId>,
            Arc<DownloadQueueSnapshot>,
        >::new()));
        let recorded = queues.clone();
        let task = products
            .runtime
            .spawn(async move {
                let mut state = AndroidDownloadsState::default();
                while let Ok(event) = events.recv().await {
                    match event {
                        DownloadEvent::Changed { .. } => state.revision += 1,
                        DownloadEvent::Notice(notice) => state.notice = Some(notice),
                        DownloadEvent::Feedback(_) | DownloadEvent::SubjectChanged { .. } => {
                            continue;
                        }
                        DownloadEvent::Queue {
                            source_id,
                            snapshot,
                        } => {
                            state.paused = snapshot.paused;
                            state.queue_revision += 1;
                            recorded
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner())
                                .insert(source_id, snapshot);
                        }
                    }
                    state.queues = recorded
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .iter()
                        .map(|(source, snapshot)| AndroidDownloadQueue {
                            source_id: source.as_ref().map(ToString::to_string),
                            downloaded_tracks: snapshot.downloaded_tracks as u64,
                            total_jobs: snapshot.jobs.len() as u64,
                        })
                        .collect();
                    updates.send_replace(state.clone());
                }
            })
            .abort_handle();
        Self {
            downloads: products.downloads.clone(),
            settings: products.settings.clone(),
            source: products.source.clone(),
            runtime: products.runtime.clone(),
            changes,
            task,
            queues,
            database: products.library.clone(),
        }
    }
}

impl Drop for AndroidDownloads {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[uniffi::export]
impl AndroidDownloads {
    pub async fn queue_page(
        &self,
        offset: u64,
        limit: u32,
    ) -> Result<AndroidDownloadPage, AndroidError> {
        let snapshots = self
            .queues
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let database = self.database.clone();
        self.runtime
            .spawn(async move {
                let total = snapshots
                    .iter()
                    .map(|snapshot| snapshot.jobs.len() as u64)
                    .sum();
                let mut offset = usize::try_from(offset).map_err(error)?;
                let mut captured = Vec::new();
                let limit = limit.min(128) as usize;
                for snapshot in snapshots {
                    if offset >= snapshot.jobs.len() {
                        offset -= snapshot.jobs.len();
                        continue;
                    }
                    captured.extend(
                        snapshot
                            .jobs
                            .iter()
                            .skip(offset)
                            .take(limit - captured.len())
                            .cloned(),
                    );
                    offset = 0;
                    if captured.len() >= limit {
                        break;
                    }
                }
                let uris = captured
                    .iter()
                    .filter_map(|job| job.preview_uris.first().cloned())
                    .collect::<Vec<_>>();
                let rows = database
                    .track_rows_by_uri(&uris, &library::ReadCancellation::new())
                    .await
                    .map_err(error)?;
                let jobs = captured
                    .into_iter()
                    .map(|job| {
                        let preview = job
                            .preview_uris
                            .first()
                            .and_then(|uri| rows.iter().find(|row| &row.media_uri == uri));
                        AndroidDownloadJob {
                            id: job.id,
                            source_id: job.source_id.map(|source| source.to_string()),
                            title: match job.subject {
                                downloads::DownloadSubject::Rule(rule) => rule_title(rule).into(),
                                downloads::DownloadSubject::Prepared { title, .. } => {
                                    title.unwrap_or_else(|| "Downloads".into())
                                }
                            },
                            subtitle: preview
                                .map(|row| format!("{} · {}", row.album, row.artist))
                                .unwrap_or_default(),
                            completed: job.completed_tracks as u64,
                            total: job.total_tracks as u64,
                            state: match job.state {
                                downloads::DownloadQueueState::Queued => "Queued",
                                downloads::DownloadQueueState::Downloading => "Downloading",
                                downloads::DownloadQueueState::WaitingForConnection => {
                                    "Waiting for connection"
                                }
                                downloads::DownloadQueueState::NeedsAttention => "Needs attention",
                            }
                            .into(),
                            artwork_identity: preview.and_then(|row| row.artwork_binding.clone()),
                        }
                    })
                    .collect();
                Ok(AndroidDownloadPage { total, jobs })
            })
            .await
            .map_err(error)?
    }

    pub fn subscribe(&self) -> Arc<AndroidDownloadsSubscription> {
        let mut changes = self.changes.clone();
        changes.mark_changed();
        Arc::new(AndroidDownloadsSubscription {
            changes: tokio::sync::Mutex::new(changes),
        })
    }

    pub fn set_paused(&self, paused: bool) {
        self.downloads.set_paused(paused);
    }
    pub fn cancel(&self, source_id: Option<String>, id: String) {
        self.downloads
            .cancel_job(source_id.map(sources::SourceId::new), id);
    }
    pub fn move_job(&self, source_id: String, id: String, target: String, after: bool) {
        self.downloads
            .move_job(sources::SourceId::new(source_id), id, target, after);
    }

    pub async fn sources(&self) -> Result<Vec<AndroidDownloadSource>, AndroidError> {
        let stored = self.settings.load();
        Ok(self
            .source
            .list_sources()
            .sources
            .iter()
            .filter(|source| source.kind != "local")
            .map(|source| {
                let configured = stored.download_settings(&source.id);
                AndroidDownloadSource {
                    id: source.id.to_string(),
                    name: source.name.clone(),
                    directory: configured.directory.map(|directory| directory.to_string()),
                    bitrate: configured.quality.max_bitrate_kbps(),
                    bitrate_limit: source.transcoded_download_bitrate_limit_kbps,
                    rules: DownloadRule::ALL
                        .into_iter()
                        .map(|rule| AndroidDownloadRule {
                            id: rule_id(rule),
                            title: localization::tr(rule_title(rule)),
                            enabled: configured.rules.contains(rule),
                        })
                        .collect(),
                }
            })
            .collect())
    }

    pub async fn set_rule(
        &self,
        source: String,
        rule: String,
        enabled: bool,
        delete_downloads: bool,
    ) -> Result<(), AndroidError> {
        let rule: DownloadRule =
            serde_json::from_value(serde_json::Value::String(rule)).map_err(error)?;
        let source = sources::SourceId::new(source);
        let settings = self.settings.clone();
        let configured = source.clone();
        self.runtime
            .spawn_blocking(move || settings.set_download_rule(configured, rule, enabled))
            .await
            .map_err(error)?
            .map_err(error)?;
        if !enabled {
            self.downloads.remove_rule(source, rule, delete_downloads);
        }
        Ok(())
    }

    pub async fn set_quality(
        &self,
        source: String,
        bitrate: Option<u32>,
    ) -> Result<(), AndroidError> {
        let settings = self.settings.clone();
        self.runtime
            .spawn_blocking(move || {
                settings.set_download_quality(
                    sources::SourceId::new(source),
                    bitrate.map_or(
                        playback::StreamQuality::Original,
                        playback::StreamQuality::MaxBitrateKbps,
                    ),
                )
            })
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub async fn clear_source(&self, source: String) -> Result<(), AndroidError> {
        let source = sources::SourceId::new(source);
        let settings = self.settings.clone();
        let configured = source.clone();
        self.runtime
            .spawn_blocking(move || settings.clear_download_rules(configured))
            .await
            .map_err(error)?
            .map_err(error)?;
        self.downloads.clear(source, true);
        Ok(())
    }
}

#[derive(uniffi::Object)]
pub struct AndroidDownloadsSubscription {
    changes: tokio::sync::Mutex<tokio::sync::watch::Receiver<AndroidDownloadsState>>,
}
#[uniffi::export]
impl AndroidDownloadsSubscription {
    pub async fn next(&self) -> Result<AndroidDownloadsState, AndroidError> {
        let mut changes = self.changes.lock().await;
        changes.changed().await.map_err(error)?;
        Ok(changes.borrow_and_update().clone())
    }
}
