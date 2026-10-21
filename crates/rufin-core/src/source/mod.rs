//! Source configuration, browsing selection, and source work.
pub mod authorization;
mod availability;
mod download;
pub mod folders;
mod integrations;
mod pins;
pub mod search;
pub use integrations::FileIntegration;

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::runtime::source::{
    ConfiguredSources, DiscoveryStatus, DiscoveryUpdate, EditableSource, LocalAccessStatus,
    LocalFolder, SourceLocalAccess, SourceLocalAccessSummary, SourceOperation, SourceProgress,
    SourceProgressStage, SourceSummary,
};
use crate::runtime::{
    CatalogChange, CatalogPublication, CatalogRevision, FavoriteSettlement, SelectedLibrary,
    SourceEvent, SourceNotice, SourceNoticeKind,
};
use artwork::Artwork;
use async_channel::{Receiver, Sender};
use downloads::Downloads;
use library::{
    CatalogUpdate, Database, FavoriteTarget, FolderChoice, FolderKey, ReadCancellation, SourceKey,
};
use secrets::{SecretStorageMode, SwitchableSecretStore};
use sources::{
    CredentialPreset, LiveFolderPage, SelectedFeed, Source, SourceConfiguration, SourceEntityKind,
    SourceError, SourceId, SourceReadProgress, SourceReadStage, SourceSettingsChange, SourceSetup,
};
use tracing::{info, warn};

use crate::playback::PlaybackOwner;
use crate::settings::{
    ConfiguredSource, SavedLocalAccess, SettingsFile, StoredSettings, fresh_source_id,
    load_provider_secret, platform_secret_store, seal_credential,
};

const SOURCE_CHECK_INTERVAL: Duration = Duration::from_secs(5 * 60);

#[derive(Clone)]
pub(crate) struct SourceOutputs {
    pub(crate) events: Sender<SourceEvent>,
    pub(crate) discovery: Sender<DiscoveryUpdate>,
}

#[derive(Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct SourceOwner {
    pub(crate) shared: Arc<Shared>,
}

struct CachedSource {
    configuration: SourceConfiguration,
    credential: Option<crate::settings::Credential>,
    jellyfin_device_id: String,
    source: Arc<Source>,
    refresh: Option<Refresh>,
    observer: Option<(Arc<SelectedFeed>, tokio::task::AbortHandle)>,
    mapping: Option<Arc<AtomicBool>>,
}

struct Refresh {
    cancelled: Arc<AtomicBool>,
    progress: SourceProgress,
    visible: bool,
}

impl Drop for CachedSource {
    fn drop(&mut self) {
        self.source.retire();
        if let Some((observer, task)) = &self.observer {
            observer.cancel();
            task.abort();
        }
        if let Some(refresh) = &self.refresh {
            refresh.cancelled.store(true, Ordering::Release);
        }
        if let Some(mapping) = &self.mapping {
            mapping.store(true, Ordering::Release);
        }
    }
}

impl CachedSource {
    fn matches(&self, configured: &ConfiguredSource, device: &str) -> bool {
        same_native_input(&self.configuration, &configured.configuration)
            && self.credential == configured.credential
            && self.jellyfin_device_id == device
    }
}

fn same_native_input(left: &SourceConfiguration, right: &SourceConfiguration) -> bool {
    left.source_id == right.source_id
        && left.kind == right.kind
        && left.provider_payload == right.provider_payload
}

pub(crate) struct Shared {
    home_showcase_variation: i64,
    home_explore_variation: std::sync::atomic::AtomicI64,
    pub(crate) artwork: Artwork,
    pub(crate) database: Arc<Database>,
    downloads: Downloads,
    pub(crate) settings: SettingsFile,
    pub(crate) secrets: Arc<SwitchableSecretStore>,
    clients: Mutex<HashMap<SourceId, CachedSource>>,
    plex_logins: Mutex<HashMap<String, Arc<tokio::sync::Mutex<sources::PlexLogin>>>>,
    pub(crate) runtime: tokio::runtime::Handle,
    outputs: SourceOutputs,
    operation: tokio::sync::watch::Sender<SourceOperation>,
    catalog_changed: tokio::sync::watch::Sender<CatalogRevision>,
    configuration_changed: tokio::sync::watch::Sender<()>,
    discovery: tokio::sync::watch::Sender<Option<DiscoveryUpdate>>,
    selected: Mutex<Option<SelectedLibrary>>,
    catalog_counts: Mutex<HashMap<SourceId, (usize, usize)>>,
    pub(crate) prepared_albums: tokio::sync::Mutex<Option<crate::browse::PreparedAlbums>>,
    selection_intent: Mutex<Weak<AtomicBool>>,
    connection: Mutex<Option<tokio::task::AbortHandle>>,
    album_release: Mutex<Option<tokio::task::AbortHandle>>,
    asset_download: Mutex<Option<(SourceId, Weak<tokio_util::sync::CancellationToken>)>>,
    pub(crate) lane: tokio::sync::Mutex<()>,
    playback: Mutex<Weak<PlaybackOwner>>,
    connect: Mutex<Weak<crate::connect::ConnectOwner>>,
    maintenance: Arc<tokio::sync::Notify>,
    started: AtomicBool,
}

impl Shared {
    pub(crate) fn cached_source_counts(&self, source: &SourceId) -> Option<(usize, usize)> {
        self.catalog_counts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(source)
            .copied()
    }

    async fn refresh_source_counts(
        &self,
        source_id: &SourceId,
        source: SourceKey,
    ) -> Result<(usize, usize), String> {
        let counts = self
            .database
            .source_counts(source, &ReadCancellation::new())
            .await
            .map_err(string_error)?;
        self.catalog_counts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(source_id.clone(), counts);
        Ok(counts)
    }

    async fn load_source_counts(&self) -> Result<(), String> {
        let counts = self
            .database
            .all_source_counts()
            .await
            .map_err(string_error)?;
        *self
            .catalog_counts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = counts;
        Ok(())
    }

    pub(crate) fn configured_sources(
        &self,
        selected: Option<&SelectedLibrary>,
    ) -> ConfiguredSources {
        configured_sources(
            &self.settings.load(),
            selected,
            &self
                .catalog_counts
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    pub(crate) fn selected(&self) -> Option<SelectedLibrary> {
        self.selected
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub(crate) async fn send(&self, event: SourceEvent) {
        if matches!(
            &event,
            SourceEvent::Configured(_)
                | SourceEvent::Selected { .. }
                | SourceEvent::ReleaseSelected
        ) {
            self.configuration_changed.send_replace(());
        }
        if let SourceEvent::CatalogPublished(publication) = &event
            && publication.change != CatalogChange::Home
        {
            self.catalog_changed.send_modify(|revision| {
                if publication.change == CatalogChange::Activity {
                    revision.activity += 1;
                } else {
                    revision.catalog += 1;
                    revision.change = Some(publication.change.clone());
                }
            });
            if matches!(
                publication.change,
                CatalogChange::Broad | CatalogChange::Album(_)
            ) && let Ok(playback) = self.playback()
            {
                playback.catalog_changed();
            }
        }
        let _ = self.outputs.events.send(event).await;
    }

    fn publish_operation(&self, operation: SourceOperation) {
        let active = self
            .asset_download
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if matches!(
            operation,
            SourceOperation::Idle | SourceOperation::Refreshing { .. }
        ) && active
            .as_ref()
            .and_then(|(_, token)| token.upgrade())
            .is_some()
            && matches!(
                *self.operation.borrow(),
                SourceOperation::Downloading { .. }
            )
        {
            return;
        }
        self.operation.send_replace(operation.clone());
        let _ = self
            .outputs
            .events
            .try_send(SourceEvent::Operation(operation));
    }

    pub(crate) fn warn_nonfatal(&self, message: &str) {
        warn!(message, "source operation was ignored");
    }

    pub(crate) fn playback(&self) -> Result<Arc<PlaybackOwner>, String> {
        self.playback
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .upgrade()
            .ok_or_else(|| "Playback is unavailable".to_string())
    }

    async fn retire_changed_clients(&self, credentials_replaced: bool) -> Result<(), String> {
        let current = self.settings.load();
        let retired = {
            let mut clients = self.clients.lock().unwrap_or_else(|p| p.into_inner());
            let ids = clients
                .iter()
                .filter(|(id, cached)| {
                    (credentials_replaced && cached.credential.is_some())
                        || !current
                            .sources
                            .configured
                            .iter()
                            .chain(&current.sources.integrations)
                            .any(|configured| {
                                &configured.configuration.source_id == *id
                                    && cached.matches(configured, &current.jellyfin_device_id)
                            })
                })
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            for id in &ids {
                clients.remove(id);
            }
            ids
        };
        for id in retired {
            self.database
                .invalidate_source_observation(&id)
                .await
                .map_err(string_error)?;
        }
        Ok(())
    }

    fn begin_selection(&self) -> Arc<AtomicBool> {
        self.cancel_selection();
        let cancelled = Arc::new(AtomicBool::new(false));
        *self
            .selection_intent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::downgrade(&cancelled);
        cancelled
    }

    fn selection_is_current(&self, cancelled: &Arc<AtomicBool>) -> bool {
        !cancelled.load(Ordering::Acquire)
            && self
                .selection_intent
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .upgrade()
                .is_some_and(|active| Arc::ptr_eq(&active, cancelled))
    }

    fn cancel_selection(&self) {
        if let Some(cancelled) = self
            .selection_intent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .upgrade()
        {
            cancelled.store(true, Ordering::Release);
        }
    }

    pub(crate) fn begin_refresh(
        &self,
        source_id: &SourceId,
        replace: bool,
        visible: bool,
    ) -> Option<Arc<AtomicBool>> {
        let mut clients = self.clients.lock().unwrap_or_else(|p| p.into_inner());
        let cached = clients.get_mut(source_id)?;
        if let Some(previous) = &cached.refresh {
            if !replace && !previous.cancelled.load(Ordering::Acquire) {
                return None;
            }
            previous.cancelled.store(true, Ordering::Release);
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        cached.refresh = Some(Refresh {
            cancelled: cancelled.clone(),
            progress: initial_progress(),
            visible,
        });
        Some(cancelled)
    }

    fn refresh_is_current(&self, source_id: &SourceId, token: &Arc<AtomicBool>) -> bool {
        !token.load(Ordering::Acquire)
            && self
                .clients
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(source_id)
                .and_then(|cached| cached.refresh.as_ref())
                .is_some_and(|refresh| Arc::ptr_eq(&refresh.cancelled, token))
    }

    fn is_refreshing(&self, source_id: &SourceId) -> bool {
        self.clients
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(source_id)
            .and_then(|cached| cached.refresh.as_ref())
            .is_some_and(|refresh| !refresh.cancelled.load(Ordering::Acquire))
    }

    pub(crate) fn present_refresh(&self, source_id: &SourceId) {
        let progress = self
            .clients
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(source_id)
            .and_then(|cached| cached.refresh.as_ref())
            .filter(|refresh| refresh.visible)
            .map(|refresh| refresh.progress.clone());
        if let Some(progress) = progress
            && !self.operation.borrow().blocks_library()
        {
            self.publish_operation(SourceOperation::Refreshing {
                source_id: source_id.clone(),
                progress,
            });
        }
    }

    pub(crate) fn refresh_progress(
        &self,
        source_id: &SourceId,
        token: &Arc<AtomicBool>,
        value: SourceReadProgress,
    ) {
        let progress = source_progress(value);
        {
            let mut clients = self.clients.lock().unwrap_or_else(|p| p.into_inner());
            let Some(refresh) = clients
                .get_mut(source_id)
                .and_then(|cached| cached.refresh.as_mut())
            else {
                return;
            };
            if !Arc::ptr_eq(&refresh.cancelled, token) || token.load(Ordering::Acquire) {
                return;
            }
            if refresh.progress == progress {
                return;
            }
            refresh.progress = progress.clone();
            if !refresh.visible {
                return;
            }
        }
        if matches!(&*self.operation.borrow(), SourceOperation::Refreshing { source_id: current, .. } if current == source_id)
        {
            self.publish_operation(SourceOperation::Refreshing {
                source_id: source_id.clone(),
                progress,
            });
        }
    }
}

impl SourceOwner {
    pub(crate) fn open_dormant(
        artwork: Artwork,
        database: Arc<Database>,
        downloads: Downloads,
        settings: SettingsFile,
        secrets: Arc<SwitchableSecretStore>,
        runtime: tokio::runtime::Handle,
        outputs: SourceOutputs,
    ) -> Arc<Self> {
        let stored = settings.load();
        let operation =
            stored
                .sources
                .selected_source_id
                .clone()
                .map_or(SourceOperation::Idle, |target| SourceOperation::Switching {
                    target,
                    progress: initial_progress(),
                });
        let home_variation = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos() as i64);
        let shared = Arc::new(Shared {
            catalog_changed: tokio::sync::watch::channel(CatalogRevision::default()).0,
            configuration_changed: tokio::sync::watch::channel(()).0,
            home_showcase_variation: home_variation,
            home_explore_variation: std::sync::atomic::AtomicI64::new(home_variation),
            artwork,
            database,
            downloads,
            settings,
            secrets,
            clients: Mutex::new(HashMap::new()),
            plex_logins: Mutex::new(HashMap::new()),
            runtime,
            outputs,
            operation: tokio::sync::watch::channel(operation).0,
            discovery: tokio::sync::watch::channel(None).0,
            selected: Mutex::new(None),
            catalog_counts: Mutex::new(HashMap::new()),
            prepared_albums: tokio::sync::Mutex::new(None),
            selection_intent: Mutex::new(Weak::new()),
            connection: Mutex::new(None),
            album_release: Mutex::new(None),
            asset_download: Mutex::new(None),
            lane: tokio::sync::Mutex::new(()),
            playback: Mutex::new(Weak::new()),
            connect: Mutex::new(Weak::new()),
            maintenance: Arc::new(tokio::sync::Notify::new()),
            started: AtomicBool::new(false),
        });
        let weak = Arc::downgrade(&shared);
        shared.database.observe_catalog(move |source, update| {
            let shared = weak.upgrade();
            async move {
                if let Some(shared) = shared {
                    if let Some((id, key)) = source {
                        SourceOwner { shared }
                            .catalog_updated(id, key, update)
                            .await;
                    } else if update.activity {
                        shared
                            .send(SourceEvent::CatalogPublished(CatalogPublication {
                                source_key: None,
                                favorite: None,
                                change: CatalogChange::Activity,
                            }))
                            .await;
                    }
                }
            }
        });
        Arc::new(Self { shared })
    }

    pub(crate) fn attach_playback(&self, playback: &Arc<PlaybackOwner>) {
        *self
            .shared
            .playback
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Arc::downgrade(playback);
    }

    pub(crate) fn start(&self) -> Result<(), String> {
        if self.shared.started.swap(true, Ordering::AcqRel) {
            return Err("the source owner is already running".to_string());
        }
        if let Some(source_id) = self.shared.settings.load().sources.selected_source_id {
            let owner = self.clone();
            self.shared.runtime.spawn(async move {
                let _ = owner.select_source(source_id).await;
            });
        }
        let owner = self.clone();
        self.shared.runtime.spawn(async move {
            if let Err(error) = owner.shared.load_source_counts().await {
                owner.shared.warn_nonfatal(&error);
            }
            owner
                .shared
                .send(SourceEvent::Configured(owner.list_sources()))
                .await;
        });
        let weak = Arc::downgrade(&self.shared);
        let maintenance = Arc::clone(&self.shared.maintenance);
        self.shared.runtime.spawn(async move {
            let mut interval = tokio::time::interval(SOURCE_CHECK_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                let check_links = tokio::select! {
                    _ = interval.tick() => true,
                    _ = maintenance.notified() => false,
                };
                let Some(shared) = weak.upgrade() else {
                    break;
                };
                let owner = SourceOwner { shared };
                if check_links {
                    let _lane = owner.shared.lane.lock().await;
                    crate::playlist_files::refresh_links(&owner, None).await;
                }
                owner.deliver_pending_favorites().await;
            }
        });
        Ok(())
    }

    pub(crate) async fn close_storage(&self) {
        self.shared.started.store(false, Ordering::Release);
        self.release_selected(true).await;
        self.shared
            .clients
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
    }

    pub(crate) async fn restore_user_state<F, Work, T>(
        &self,
        setup: bool,
        credentials_replaced: bool,
        restore: F,
    ) -> Result<T, String>
    where
        F: FnOnce() -> Work,
        Work: Future<Output = Result<T, String>>,
    {
        let _lane = self.shared.lane.lock().await;
        if setup {
            self.release_selected(true).await;
        }
        let result = restore().await;
        if setup
            && let Err(error) = self
                .shared
                .retire_changed_clients(credentials_replaced)
                .await
        {
            self.shared.warn_nonfatal(&error);
        }
        if result.is_ok()
            && let Err(error) = self.shared.load_source_counts().await
        {
            self.shared.warn_nonfatal(&error);
        }
        if setup && self.shared.started.load(Ordering::Acquire) {
            let current = self.shared.settings.load();
            if let Some(source) = current.sources.selected_source_id.filter(|id| {
                current
                    .sources
                    .configured
                    .iter()
                    .any(|source| source.configuration.source_id == *id)
            }) {
                let owner = self.clone();
                self.shared.runtime.spawn(async move {
                    let _ = owner.select_source(source).await;
                });
            }
        }
        self.shared
            .send(SourceEvent::Configured(
                self.shared
                    .configured_sources(self.shared.selected().as_ref()),
            ))
            .await;
        self.publish_operation(SourceOperation::Idle).await;
        result
    }

    /// Called only for an applied Connect page. Empty synchronization passes do not
    /// publish catalog events or rebuild the selected source.
    pub(crate) async fn connect_changed(&self, sources_changed: bool) -> Result<(), String> {
        let _lane = self.shared.lane.lock().await;
        let current = self.shared.settings.load();
        if sources_changed {
            self.shared.retire_changed_clients(false).await?;
        }
        self.shared.load_source_counts().await?;
        let reconnect = self
            .shared
            .selected()
            .is_none_or(|selected| self.cached_client(&selected.source_id).is_none());
        if reconnect {
            self.release_selected(true).await;
            if let Some(source) = current
                .sources
                .selected_source_id
                .filter(|id| {
                    current
                        .sources
                        .configured
                        .iter()
                        .any(|source| source.configuration.source_id == *id)
                })
                .or_else(|| {
                    let counts = self
                        .shared
                        .catalog_counts
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    current
                        .sources
                        .configured
                        .iter()
                        .find(|source| {
                            counts
                                .get(&source.configuration.source_id)
                                .is_some_and(|(_, tracks)| *tracks > 0)
                        })
                        .map(|source| source.configuration.source_id.clone())
                })
            {
                let owner = self.clone();
                self.shared.runtime.spawn(async move {
                    let _ = owner.select_source(source).await;
                });
            }
        }
        self.shared
            .send(SourceEvent::Configured(
                self.shared
                    .configured_sources(self.shared.selected().as_ref()),
            ))
            .await;
        self.shared
            .send(SourceEvent::CatalogPublished(CatalogPublication {
                source_key: None,
                favorite: None,
                change: CatalogChange::Broad,
            }))
            .await;
        Ok(())
    }

    async fn remove_source_resources(
        &self,
        source_id: &SourceId,
        credential: Option<crate::settings::Credential>,
    ) {
        self.shared
            .clients
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(source_id);
        self.shared
            .catalog_counts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(source_id);
        self.shared.downloads.clear(source_id.clone(), false);
        if let Err(error) = self
            .shared
            .database
            .clear_source_waveform_cache(source_id)
            .await
        {
            self.shared.warn_nonfatal(&error.to_string());
        }
        if let Ok(playback) = self.shared.playback() {
            if let Err(error) = playback.remove_waveform_cache(source_id) {
                self.shared.warn_nonfatal(&error);
            }
        }
        if let Err(error) = self.shared.artwork.invalidate_source(source_id) {
            self.shared.warn_nonfatal(&error.to_string());
        }
        if let Some(reference) = credential
            && !self
                .shared
                .settings
                .load()
                .sources
                .configured
                .iter()
                .any(|source| {
                    &source.configuration.source_id != source_id
                        && source.credential.as_ref() == Some(&reference)
                })
        {
            let mut logins = self
                .shared
                .plex_logins
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            logins.remove(reference.as_str());
            drop(logins);
            if let crate::settings::Credential::Legacy(reference) = reference {
                let secrets = Arc::clone(&self.shared.secrets);
                self.shared.runtime.spawn_blocking(move || {
                    let _ = secrets::SecretStore::delete_secret(
                        secrets.as_ref(),
                        &secrets::SecretKey::provider_token(&reference),
                    );
                });
            }
        }
    }

    pub(crate) fn spawn_serialized<F, Work>(&self, work: F)
    where
        F: FnOnce(SourceOwner) -> Work + Send + 'static,
        Work: Future<Output = ()> + Send + 'static,
    {
        let shared = Arc::clone(&self.shared);
        self.shared.runtime.spawn(async move {
            let lane_owner = Arc::clone(&shared);
            let _lane = lane_owner.lane.lock().await;
            work(SourceOwner { shared }).await;
        });
    }

    async fn publish_operation(&self, operation: SourceOperation) {
        self.shared.publish_operation(operation);
    }

    async fn select_now(
        &self,
        source_id: SourceId,
        cancelled: Arc<AtomicBool>,
        refresh: bool,
    ) -> Result<(), String> {
        if cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        let configured = configured_source(&self.shared.settings.load().sources, &source_id)?;
        let publication = match self
            .shared
            .database
            .cached_source(source_id.as_str(), &ReadCancellation::new())
            .await
            .map_err(string_error)?
        {
            Some(cached) => cached,
            None => self
                .shared
                .database
                .reconcile_source(
                    &source_id,
                    &configured.configuration.kind,
                    &configured.configuration.name,
                )
                .await
                .map_err(string_error)?,
        };
        let cached_start = publication.catalog_complete;
        self.install_selected(
            configured.clone(),
            None,
            publication,
            Arc::clone(&cancelled),
        )
        .await?;
        if !self.shared.selection_is_current(&cancelled) {
            return Ok(());
        }
        if self
            .shared
            .clients
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&source_id)
            .is_some_and(|cached| cached.observer.is_some())
        {
            return Ok(());
        }
        let Some(selected) = self.selected_library() else {
            return Ok(());
        };
        let owner = self.clone();
        let task = self.shared.runtime.spawn(async move {
            let source = owner.open_client(source_id.clone()).await;
            if !owner.shared.selection_is_current(&cancelled) {
                return;
            }
            let Some(selected) = owner
                .selected_library()
                .filter(|current| current.source_key == selected.source_key)
            else {
                return;
            };
            if let Err(error) = owner
                .shared
                .downloads
                .attach(
                    source_id.clone(),
                    selected.source_key,
                    source.as_ref().ok().cloned(),
                    selected.music_folder_key,
                    owner.source_input_digest(&source_id),
                )
                .await
            {
                owner.shared.warn_nonfatal(&error);
            }
            if !owner.shared.selection_is_current(&cancelled) {
                return;
            }
            let source = match source {
                Ok(source) => source,
                Err(error) => {
                    owner.shared.warn_nonfatal(&error);
                    return;
                }
            };
            let activated =
                owner.start_observer(selected.clone(), source.clone(), refresh && !cached_start);
            owner.shared.present_refresh(&source_id);
            if refresh && activated && (!cached_start || configured.configuration.is_local()) {
                let refresh_owner = owner.clone();
                let refresh_source = source.clone();
                owner.shared.runtime.spawn(async move {
                    if !cached_start {
                        let _ = refresh_owner
                            .refresh_now(&source_id, &refresh_source, false, true)
                            .await;
                    } else if let Some(token) =
                        refresh_owner.shared.begin_refresh(&source_id, false, false)
                    {
                        let progress = |value| {
                            refresh_owner
                                .shared
                                .refresh_progress(&source_id, &token, value)
                        };
                        let result = refresh_source
                            .catch_up_local(
                                &refresh_owner.shared.database,
                                &progress,
                                token.clone(),
                            )
                            .await;
                        refresh_owner
                            .finish_source_refresh(&source_id, &token, result)
                            .await;
                    }
                });
            }
            if let Ok(playback) = owner.shared.playback() {
                playback.catalog_changed();
            }
        });
        *self
            .shared
            .connection
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(task.abort_handle());
        Ok(())
    }

    async fn selected_folders(
        &self,
        configured: &ConfiguredSource,
        source_key: SourceKey,
    ) -> Result<(Arc<[FolderChoice]>, Option<FolderKey>), String> {
        let folders = self
            .shared
            .database
            .music_folders(source_key)
            .await
            .map_err(string_error)?;
        let selected = match configured.music_folder_id.as_deref() {
            Some(object_id) => self
                .shared
                .database
                .folder_identity_key(source_key, object_id)
                .await
                .map_err(string_error)?,
            None => None,
        };
        Ok((folders.into(), selected))
    }

    async fn install_selected(
        &self,
        configured: ConfiguredSource,
        mut source: Option<Arc<Source>>,
        source_state: library::CachedSource,
        acquisition: Arc<AtomicBool>,
    ) -> Result<(), String> {
        let (music_folders, music_folder_key) = self
            .selected_folders(&configured, source_state.source)
            .await?;
        let selected = SelectedLibrary {
            source_id: configured.configuration.source_id.clone(),
            source_key: source_state.source,
            music_folder_key,
            music_folder_object_id: configured.music_folder_id.clone(),
            music_folders,
        };
        if !self.shared.selection_is_current(&acquisition) {
            return Ok(());
        }
        self.release_selected(false).await;
        source = source.map(|source| self.cache_connected(&configured, source));
        *self
            .shared
            .selected
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(selected.clone());
        self.shared.settings.update(|stored| {
            stored.sources.selected_source_id = Some(selected.source_id.clone());
            Ok(())
        })?;
        self.shared
            .send(SourceEvent::Selected {
                configured: self.shared.configured_sources(Some(&selected)),
                selected: selected.clone(),
            })
            .await;
        self.publish_operation(SourceOperation::Idle).await;
        self.shared.present_refresh(&selected.source_id);
        if let Some(source) = source {
            self.start_observer(selected.clone(), source.clone(), true);
            let owner = self.clone();
            self.shared.runtime.spawn(async move {
                if let Err(error) = owner
                    .shared
                    .downloads
                    .attach(
                        selected.source_id.clone(),
                        selected.source_key,
                        Some(source),
                        selected.music_folder_key,
                        owner.source_input_digest(&selected.source_id),
                    )
                    .await
                {
                    owner.shared.warn_nonfatal(&error);
                }
            });
        }
        Ok(())
    }

    async fn release_selected(&self, cancel_selection: bool) {
        if cancel_selection {
            self.shared.cancel_selection();
        }
        for job in [&self.shared.connection, &self.shared.album_release] {
            if let Some(job) = job.lock().unwrap_or_else(|p| p.into_inner()).take() {
                job.abort();
            }
        }
        let released = self
            .shared
            .selected
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
            .is_some();
        if released {
            if let Ok(playback) = self.shared.playback() {
                playback.cancel_loudness_analysis();
            }
            self.shared.send(SourceEvent::ReleaseSelected).await;
        }
    }

    fn start_observer(
        &self,
        selected: SelectedLibrary,
        source: Arc<Source>,
        initial_scan_pending: bool,
    ) -> bool {
        let mut clients = self
            .shared
            .clients
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(cached) = clients.get_mut(&selected.source_id) else {
            return false;
        };
        if cached.observer.is_some() {
            return false;
        }
        let observer = source.start_selected_feed(&self.shared.runtime, initial_scan_pending);
        let shared = Arc::downgrade(&self.shared);
        let task = self.shared.runtime.spawn(Self::consume_selected_feed(
            shared,
            selected.source_id,
            selected.source_key,
            source,
            observer.clone(),
        ));
        cached.observer = Some((observer, task.abort_handle()));
        true
    }

    async fn consume_selected_feed(
        shared: Weak<Shared>,
        source_id: SourceId,
        source_key: SourceKey,
        source: Arc<Source>,
        observer: Arc<SelectedFeed>,
    ) {
        let mut interval = tokio::time::interval(SOURCE_CHECK_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        interval.tick().await;
        loop {
            let event = tokio::select! {
                changed = observer.wait_for_change() => { if !changed { return; } true },
                _ = interval.tick() => false,
            };
            if observer.cancellation().load(Ordering::Acquire) {
                return;
            }
            let Some(shared) = shared.upgrade() else {
                observer.cancel();
                return;
            };
            let owner = Self { shared };
            if owner.shared.is_refreshing(&source_id) {
                continue;
            }
            if !event {
                owner.check_remote_freshness(&source_id, &source).await;
                continue;
            }
            match observer
                .apply_pending(&owner.shared.database, source_key)
                .await
            {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => {
                    if source_error_is_cancelled(&error) {
                        return;
                    }
                    warn!(%error, "bounded source change failed; refreshing after feed boundary loss");
                }
            }
            let _ = owner
                .refresh_now(&source_id, &source, true, !source.needs_periodic_refresh())
                .await;
        }
    }

    async fn refresh_now(
        &self,
        source_id: &SourceId,
        source: &Arc<Source>,
        automatic: bool,
        force: bool,
    ) -> Result<(), String> {
        if source.is_retired() {
            return Err(SourceError::Cancelled.to_string());
        }
        let Some(token) = self.shared.begin_refresh(source_id, !automatic, !automatic) else {
            return Ok(());
        };
        if !automatic {
            self.shared.present_refresh(source_id);
        }
        let progress = |value| self.shared.refresh_progress(source_id, &token, value);
        let outcome = source
            .acquire_catalog(&self.shared.database, &progress, token.clone(), force)
            .await;
        let outcome = outcome.map(|_| ());
        let result = outcome
            .as_ref()
            .map(|_| ())
            .map_err(source_refresh_error_text);
        self.finish_source_refresh(source_id, &token, outcome).await;
        result
    }

    pub(crate) async fn finish_source_refresh(
        &self,
        source_id: &SourceId,
        token: &Arc<AtomicBool>,
        result: sources::SourceResult<()>,
    ) {
        if !self.shared.refresh_is_current(source_id, token) {
            return;
        }
        if result.is_ok() {
            self.shared.maintenance.notify_one();
        }
        {
            let mut clients = self
                .shared
                .clients
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if let Some(cached) = clients.get_mut(source_id)
                && cached
                    .refresh
                    .as_ref()
                    .is_some_and(|refresh| Arc::ptr_eq(&refresh.cancelled, token))
            {
                cached.refresh = None;
            }
        }
        let observer = self
            .shared
            .clients
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(source_id)
            .and_then(|cached| {
                cached
                    .observer
                    .as_ref()
                    .map(|(observer, _)| observer.clone())
            });
        if let Some(observer) = observer {
            observer.wake_pending();
        }
        let presented = matches!(&*self.shared.operation.borrow(),
            SourceOperation::Refreshing { source_id: current, .. } if current == source_id);
        if presented {
            self.finish_refresh(source_id, result).await;
        } else if let Err(error) = result
            && !source_error_is_cancelled(&error)
        {
            warn!(%error, "background source refresh failed");
        }
    }

    pub(crate) async fn finish_refresh(
        &self,
        source_id: &SourceId,
        result: sources::SourceResult<()>,
    ) {
        let operation = match result {
            Ok(_) => SourceOperation::Idle,
            Err(error) if source_error_is_cancelled(&error) => SourceOperation::Idle,
            Err(error) => SourceOperation::Failed {
                source_id: Some(source_id.clone()),
                message: {
                    warn!(%error, "source refresh failed");
                    source_refresh_error_text(&error)
                },
                add_form: false,
            },
        };
        self.publish_operation(operation).await;
    }

    async fn install_connected_edit(
        &self,
        configured: ConfiguredSource,
        connected: Box<sources::ConnectedSource>,
    ) -> Result<(), String> {
        let (configuration, mut source, mut credential) = (*connected).into_parts();
        let result = async {
            let current = configured_source(
                &self.shared.settings.load().sources,
                &configured.configuration.source_id,
            )?;
            if current.configuration != configured.configuration
                || current.credential != configured.credential
            {
                return Err(SourceError::Cancelled.to_string());
            }
            let previous = configured.clone();
            let mut replacement = configured;
            replacement.configuration = configuration;
            if let Some(fresh) = source.plex_login() {
                let existing = replacement.credential.as_ref().and_then(|reference| {
                    self.shared
                        .plex_logins
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .get(reference.as_str())
                        .cloned()
                });
                if let Some(existing) = existing {
                    let fresh = fresh.lock().await.for_setup();
                    let mut existing = existing.lock().await;
                    existing.merge_authorization(&fresh).map_err(string_error)?;
                    credential = Some(existing.encode().map_err(string_error)?);
                }
            }
            let exclusions_changed = match (
                previous.configuration.editable().map_err(string_error)?,
                replacement.configuration.editable().map_err(string_error)?,
            ) {
                (
                    sources::EditableSource::Local {
                        excluded_folders: old,
                        ..
                    },
                    sources::EditableSource::Local {
                        excluded_folders: new,
                        ..
                    },
                ) => old != new,
                (
                    sources::EditableSource::Files { settings: old, .. },
                    sources::EditableSource::Files { settings: new, .. },
                ) => old.excluded_folders != new.excluded_folders,
                _ => false,
            };
            if !same_native_input(&previous.configuration, &replacement.configuration)
                || credential.is_some()
            {
                self.shared
                    .database
                    .invalidate_source_observation(&replacement.configuration.source_id)
                    .await
                    .map_err(string_error)?;
            }
            replacement = self.persist_connected_source(&replacement, credential)?;
            if let (Ok(old), Ok(new)) = (
                local_roots(&previous.configuration),
                local_roots(&replacement.configuration),
            ) && new.len() < old.len()
                && new.iter().all(|root| old.contains(root))
            {
                let removed = old
                    .into_iter()
                    .enumerate()
                    .filter(|(_, root)| !new.contains(root))
                    .collect::<Vec<_>>();
                if let Err(error) = self
                    .shared
                    .database
                    .remove_playlist_file_roots(&replacement.configuration.source_id, &removed)
                    .await
                {
                    self.persist_connected_source(&previous, None)?;
                    return Err(string_error(error));
                }
            }
            self.bind_plex_login(&replacement, &mut source)?;
            let source = self.cache_connected(&replacement, Arc::new(source));
            let publication = self
                .shared
                .database
                .reconcile_source(
                    &replacement.configuration.source_id,
                    &replacement.configuration.kind,
                    &replacement.configuration.name,
                )
                .await
                .map_err(string_error)?;
            let selected = self
                .shared
                .selected()
                .is_some_and(|selected| selected.source_id == replacement.configuration.source_id);
            let refresh = selected || replacement.configuration.is_local() || exclusions_changed;
            if selected && !self.shared.operation.borrow().blocks_library() {
                let intent = self.shared.begin_selection();
                self.install_selected(replacement, Some(Arc::clone(&source)), publication, intent)
                    .await?;
            }
            if refresh {
                let id = source.source_id().clone();
                let owner = self.clone();
                self.shared.runtime.spawn(async move {
                    if owner.configuration(&id).is_some() {
                        let _ = owner.refresh_now(&id, &source, false, true).await;
                    }
                });
            } else {
                self.publish_operation(SourceOperation::Idle).await;
            }
            Ok(())
        }
        .await;
        if result.is_err() {
            self.publish_operation(SourceOperation::Idle).await;
        }
        result
    }

    fn cache_connected(&self, configured: &ConfiguredSource, source: Arc<Source>) -> Arc<Source> {
        let device = self.shared.settings.load().jellyfin_device_id;
        let mut clients = self
            .shared
            .clients
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(cached) = clients.get(&configured.configuration.source_id)
            && cached.matches(configured, &device)
        {
            return cached.source.clone();
        }
        clients.insert(
            configured.configuration.source_id.clone(),
            CachedSource {
                configuration: configured.configuration.clone(),
                credential: configured.credential.clone(),
                jellyfin_device_id: device,
                source: source.clone(),
                refresh: None,
                observer: None,
                mapping: None,
            },
        );
        source
    }

    pub(crate) fn persist_connected_source(
        &self,
        configured: &ConfiguredSource,
        credential: Option<String>,
    ) -> Result<ConfiguredSource, String> {
        let mut configured = configured.clone();
        if let Some(secret) = credential {
            if configured
                .credential
                .as_ref()
                .map(|credential| load_provider_secret(self.shared.secrets.as_ref(), credential))
                .transpose()?
                .flatten()
                .as_deref()
                != Some(secret.as_str())
            {
                configured.credential =
                    Some(seal_credential(self.shared.secrets.as_ref(), &secret)?);
            }
        }
        self.shared.settings.update(|stored| {
            stored
                .sources
                .configured
                .retain(|item| item.configuration.source_id != configured.configuration.source_id);
            stored.sources.configured.push(configured.clone());
            Ok(())
        })?;
        let device = self.shared.settings.load().jellyfin_device_id;
        self.shared
            .clients
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|id, cached| {
                id != &configured.configuration.source_id || cached.matches(&configured, &device)
            });
        Ok(configured)
    }

    async fn edit_configured_source(&self, input: SourceSettingsChange) -> Result<(), String> {
        let source_id = input.source_id().clone();
        let configured = configured_source(&self.shared.settings.load().sources, &source_id)?;
        let credential = configured
            .credential
            .as_ref()
            .map(|reference| load_provider_secret(self.shared.secrets.as_ref(), reference))
            .transpose()?
            .flatten();
        let edit = Source::edit(
            configured.configuration.clone(),
            credential,
            input,
            Some(self.shared.settings.load().jellyfin_device_id),
        )
        .await
        .map_err(string_error)?;
        match edit {
            sources::SourceEditResult::Unchanged => return Ok(()),
            sources::SourceEditResult::ConfigurationOnly(configuration) => {
                let current = configured_source(&self.shared.settings.load().sources, &source_id)?;
                if current.configuration != configured.configuration
                    || current.credential != configured.credential
                {
                    return Err(SourceError::Cancelled.to_string());
                }
                if !same_native_input(&configured.configuration, &configuration) {
                    self.shared
                        .database
                        .invalidate_source_observation(&source_id)
                        .await
                        .map_err(string_error)?;
                    self.shared
                        .clients
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .remove(&source_id);
                }
                self.shared.settings.update(|stored| {
                    if let Some(item) = stored
                        .sources
                        .configured
                        .iter_mut()
                        .find(|item| item.configuration.source_id == source_id)
                    {
                        item.configuration = configuration.clone();
                    }
                    Ok(())
                })?;
                self.shared
                    .database
                    .reconcile_source(&source_id, &configuration.kind, &configuration.name)
                    .await
                    .map_err(string_error)?;
                Ok(())
            }
            sources::SourceEditResult::Connected(connected) => {
                self.install_connected_edit(configured, connected).await
            }
        }?;
        self.shared
            .send(SourceEvent::Configured(
                self.shared
                    .configured_sources(self.shared.selected().as_ref()),
            ))
            .await;
        Ok(())
    }

    async fn catalog_updated(
        &self,
        source_id: SourceId,
        source: library::SourceKey,
        update: CatalogUpdate,
    ) {
        let source_id = &source_id;
        if !update.track_rekeys.is_empty()
            && let Ok(playback) = self.shared.playback()
        {
            playback.rekey_tracks(update.track_rekeys.clone());
        }
        if !update.changed() {
            return;
        }
        if update.metadata || update.playlists {
            crate::playlist_files::discover(self, source_id, source).await;
        }
        let change = if update.metadata
            || update.artwork
            || (update.playlists && (update.home || update.activity))
        {
            CatalogChange::Broad
        } else if update.playlists {
            CatalogChange::Playlists(None)
        } else if update.activity {
            CatalogChange::Activity
        } else {
            CatalogChange::Home
        };
        {
            if update.metadata
                && let Some(selected) = self.shared.selected()
                && selected.source_key == source
                && let Ok(configured) =
                    configured_source(&self.shared.settings.load().sources, source_id)
            {
                match self.selected_folders(&configured, source).await {
                    Ok((folders, folder_key)) => {
                        if let Some(current) = self
                            .shared
                            .selected
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .as_mut()
                            .filter(|current| current.source_key == source)
                        {
                            current.music_folders = folders;
                            current.music_folder_key = folder_key;
                            current.music_folder_object_id = configured.music_folder_id;
                        }
                    }
                    Err(error) => self.shared.warn_nonfatal(&error),
                }
            }
        }
        if update.metadata || update.playlists {
            self.shared.downloads.library_changed(source_id.clone());
        }
        if update.metadata {
            if let Err(error) = self.shared.refresh_source_counts(source_id, source).await {
                self.shared.warn_nonfatal(&error);
            }
            let _ = self
                .shared
                .outputs
                .events
                .send(SourceEvent::Configured(
                    self.shared
                        .configured_sources(self.shared.selected().as_ref()),
                ))
                .await;
        }
        self.shared
            .send(SourceEvent::CatalogPublished(CatalogPublication {
                source_key: Some(source),
                favorite: None,
                change,
            }))
            .await;
    }

    pub(crate) async fn artwork_invalidated(&self, source_id: &SourceId, change: CatalogChange) {
        let source = match self
            .shared
            .database
            .cached_source(source_id.as_str(), &ReadCancellation::new())
            .await
        {
            Ok(Some(source)) => source.source,
            Ok(None) => return,
            Err(error) => {
                self.shared.warn_nonfatal(&error.to_string());
                return;
            }
        };
        self.shared
            .send(SourceEvent::CatalogPublished(CatalogPublication {
                source_key: Some(source),
                favorite: None,
                change,
            }))
            .await;
    }

    async fn check_remote_freshness(&self, source_id: &SourceId, source: &Arc<Source>) {
        if source.needs_periodic_refresh() {
            let _ = self.refresh_now(source_id, source, true, false).await;
        }
    }

    async fn publish_catalog(
        &self,
        selected: &SelectedLibrary,
        favorite: Option<FavoriteSettlement>,
        change: CatalogChange,
    ) {
        self.shared
            .send(SourceEvent::CatalogPublished(CatalogPublication {
                source_key: Some(selected.source_key),
                favorite,
                change,
            }))
            .await;
    }

    pub(crate) async fn publish_current_catalog(
        &self,
        favorite: Option<FavoriteSettlement>,
        change: CatalogChange,
    ) {
        self.shared
            .send(SourceEvent::CatalogPublished(CatalogPublication {
                source_key: self.shared.selected().map(|selected| selected.source_key),
                favorite,
                change,
            }))
            .await;
    }
}

impl SourceOwner {
    pub fn home_variations(&self) -> (i64, i64) {
        (
            self.shared.home_showcase_variation,
            self.shared.home_explore_variation.load(Ordering::Relaxed),
        )
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl SourceOwner {
    pub async fn smb_shares(
        &self,
        settings: sources::FileSourceSettings,
        credentials: sources::FileCredentials,
    ) -> sources::SourceResult<Vec<authorization::SourceChoice>> {
        self.shared
            .runtime
            .spawn(async move {
                sources::list_smb_shares(settings, credentials)
                    .await
                    .map(|shares| {
                        shares
                            .into_iter()
                            .enumerate()
                            .map(|(index, (title, address))| authorization::SourceChoice {
                                index: index as u32,
                                title,
                                address: Some(address),
                                pin_required: false,
                            })
                            .collect()
                    })
            })
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?
    }
}

impl SourceOwner {
    fn login_events<E: Send + 'static>(
        &self,
        method: authorization::SourceAuthorizationMethod,
        project: impl Fn(authorization::LoginEvent) -> E + Send + Sync + 'static,
    ) -> Receiver<Result<E, String>> {
        use crate::runtime::source::{EmbyConnectLoginMethod, PlexLoginMethod};
        use authorization::{Authorization, LoginEvent, SourceAuthorizationMethod};
        let (send, receive) = async_channel::bounded(2);
        let device_id = self.shared.settings.load().jellyfin_device_id;
        self.shared.runtime.spawn(async move {
            let authorization = async {
                Ok::<_, SourceError>(match method {
                    SourceAuthorizationMethod::Nextcloud { settings, credentials } => {
                        let (settings, credentials) = sources::authorize_nextcloud(settings, credentials, |url| {
                            let _ = send.try_send(Ok(project(LoginEvent::OpenBrowser(url.to_owned()))));
                        }).await?;
                        Authorization::Files { settings, credentials }
                    }
                    SourceAuthorizationMethod::Emby { method } => {
                        let login = match method {
                            EmbyConnectLoginMethod::Password { username, password } => sources::EmbyConnectLogin::password(&username, &password).await?,
                            EmbyConnectLoginMethod::Pin => {
                                let pin = sources::EmbyConnectPin::start(device_id).await?;
                                send.send(Ok(project(LoginEvent::Code { code: pin.code.clone(), url: pin.approval_url().into() }))).await.map_err(|_| SourceError::Cancelled)?;
                                loop {
                                    if let Some(login) = pin.poll().await? { break login; }
                                    tokio::time::sleep(Duration::from_secs(2)).await;
                                }
                            }
                        };
                        Authorization::Emby(login.servers().await?)
                    }
                    SourceAuthorizationMethod::Jellyfin { url, trust_invalid_cert } => {
                        let quick = sources::JellyfinQuickConnect::start(url, trust_invalid_cert, device_id).await?;
                        send.send(Ok(project(LoginEvent::Code { code: quick.code.clone(), url: quick.approval_url.clone() }))).await.map_err(|_| SourceError::Cancelled)?;
                        let login = loop {
                            if let Some(login) = quick.poll().await? { break login; }
                            tokio::time::sleep(Duration::from_secs(2)).await;
                        };
                        Authorization::Jellyfin(login)
                    }
                    SourceAuthorizationMethod::Plex { method } => {
                        let client_id = fresh_source_id().map_err(SourceError::Other)?.as_str().to_owned();
                        let login = match method {
                            PlexLoginMethod::Password { username, password, verification_code } => sources::PlexLogin::password(client_id, &username, &password, verification_code.as_deref()).await?,
                            PlexLoginMethod::Browser => {
                                let mut login = sources::PlexLogin::browser(client_id).await?;
                                send.send(Ok(project(LoginEvent::OpenBrowser(login.url().to_owned())))).await.map_err(|_| SourceError::Cancelled)?;
                                let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
                                loop {
                                    if let Some(login) = login.poll().await? { break login; }
                                    if tokio::time::Instant::now() >= deadline { return Err(SourceError::Auth("Plex browser login expired".into())); }
                                    tokio::time::sleep(Duration::from_secs(1)).await;
                                }
                            }
                        };
                        Authorization::Plex { login, profiles: Vec::new(), profile: None, servers: Vec::new() }
                    }
                })
            };
            tokio::select! {
                _ = send.closed() => {},
                result = authorization => { let _ = send.send(result.map(|authorized| project(LoginEvent::Authorized(Box::new(authorized)))).map_err(string_error)).await; }
            }
        });
        receive
    }

    pub fn login(
        &self,
        method: authorization::SourceAuthorizationMethod,
    ) -> Receiver<Result<crate::runtime::source::SourceLoginEvent, String>> {
        use crate::runtime::source::SourceLoginEvent;
        use authorization::{Authorization, LoginEvent};
        self.login_events(method, |event| match event {
            LoginEvent::Code { code, url } => SourceLoginEvent::Code { code, url },
            LoginEvent::OpenBrowser(url) => SourceLoginEvent::OpenBrowser(url),
            LoginEvent::Authorized(authorization) => match *authorization {
                Authorization::Emby(servers) => SourceLoginEvent::Servers(servers),
                Authorization::Jellyfin(login) => SourceLoginEvent::Jellyfin(login),
                Authorization::Plex { login, .. } => SourceLoginEvent::Plex(Box::new(login)),
                Authorization::Files {
                    settings,
                    credentials,
                } => SourceLoginEvent::Files {
                    settings,
                    credentials,
                },
                _ => unreachable!(),
            },
        })
    }

    pub fn plex_profiles(
        &self,
        mut login: sources::PlexLogin,
    ) -> Receiver<Result<(sources::PlexLogin, Vec<sources::PlexProfile>), String>> {
        let (send, receive) = async_channel::bounded(1);
        self.shared.runtime.spawn(async move {
            tokio::select! {
                _ = send.closed() => {},
                result = login.profiles() => { let _ = send.send(result.map(|profiles| (login, profiles)).map_err(string_error)).await; }
            }
        });
        receive
    }

    pub fn plex_servers(
        &self,
        mut login: sources::PlexLogin,
        profile: sources::PlexProfile,
        pin: Option<String>,
        lan: Vec<sources::DiscoveredServer>,
    ) -> Receiver<Result<(sources::PlexLogin, Vec<sources::PlexServer>), String>> {
        let (send, receive) = async_channel::bounded(1);
        self.shared.runtime.spawn(async move {
            let operation = async {
                login.authorize_profile(&profile, pin.as_deref()).await?;
                login.servers(&profile.id, &lan).await
            };
            tokio::select! {
                _ = send.closed() => {},
                result = operation => { let _ = send.send(result.map(|servers| (login, servers)).map_err(string_error)).await; }
            }
        });
        receive
    }

    pub fn plex_saved_logins(&self) -> Receiver<Result<Vec<(String, sources::PlexLogin)>, String>> {
        self.reply(move |owner, _| async move {
            let mut seen = std::collections::HashSet::new();
            let mut logins = Vec::new();
            for configured in owner.shared.settings.load().sources.configured {
                if configured.configuration.kind != "plex" {
                    continue;
                }
                let Some(reference) = configured.credential.as_ref() else {
                    continue;
                };
                if !seen.insert(reference.as_str().to_owned()) {
                    continue;
                }
                let source = owner.client(&configured.configuration.source_id)?;
                if let Some(login) = source.plex_login() {
                    logins.push((
                        configured.configuration.name,
                        login.lock().await.for_setup(),
                    ));
                }
            }
            Ok(logins)
        })
    }

    async fn reuse_plex_login(
        &self,
        input: &mut SourceSetup,
    ) -> Result<Option<crate::settings::Credential>, String> {
        let SourceSetup::Plex(input) = input else {
            return Ok(None);
        };
        for configured in self.shared.settings.load().sources.configured {
            if configured.configuration.kind != "plex" {
                continue;
            }
            let source = self.client(&configured.configuration.source_id)?;
            let Some(login) = source.plex_login() else {
                continue;
            };
            let mut login = login.lock().await;
            if login.same_device(&input.login) {
                login
                    .merge_authorization(&input.login)
                    .map_err(string_error)?;
                input.login = login.for_setup();
                return Ok(configured.credential);
            }
        }
        Ok(None)
    }

    pub fn prepare_collection(&self, media_uri: String) -> Receiver<Result<(), String>> {
        self.reply(move |owner, database| async move {
            let source = owner.media_client(&media_uri).await?;
            let result = source.prepare_collection(&database, &media_uri).await;
            result.map_err(string_error)
        })
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl SourceOwner {
    pub fn list_sources(&self) -> ConfiguredSources {
        self.shared
            .configured_sources(self.shared.selected().as_ref())
    }
}

impl SourceOwner {
    pub fn operation(&self) -> tokio::sync::watch::Receiver<SourceOperation> {
        self.shared.operation.subscribe()
    }

    pub fn catalog_changes(&self) -> tokio::sync::watch::Receiver<CatalogRevision> {
        self.shared.catalog_changed.subscribe()
    }

    pub fn configuration_changes(&self) -> tokio::sync::watch::Receiver<()> {
        self.shared.configuration_changed.subscribe()
    }

    pub fn cached_source_counts(&self, source: &SourceId) -> Option<(usize, usize)> {
        self.shared.cached_source_counts(source)
    }

    /// Browse a configured source without changing the selected library.
    pub async fn browse_folder(
        &self,
        source_id: SourceId,
        folder_object_id: Option<String>,
        music_folder_object_id: Option<String>,
    ) -> Result<LiveFolderPage, String> {
        self.open_client(source_id)
            .await?
            .browse_folder(
                &self.shared.database,
                folder_object_id.as_deref(),
                music_folder_object_id.as_deref(),
            )
            .await
            .map_err(string_error)
    }

    pub fn selected_source_summary(&self) -> Option<(SourceId, String)> {
        let source_id = self
            .shared
            .selected
            .lock()
            .unwrap()
            .as_ref()?
            .source_id
            .clone();
        let name = self.shared.settings.source_name(&source_id);
        Some((source_id, name))
    }

    pub fn selected_library(&self) -> Option<SelectedLibrary> {
        self.shared.selected()
    }

    /// Browsing uses the requested source and folder without changing desktop selection.
    #[allow(clippy::too_many_arguments)]
    pub fn collection_folder_uri(
        &self,
        media_uri: String,
        album_artist: bool,
        folder: Option<library::FolderKey>,
    ) -> Receiver<Result<String, String>> {
        self.reply(move |owner, database| async move {
            let (source_id, _, _) =
                library::source_entity_parts(&media_uri).ok_or_else(source_access_unavailable)?;
            let configuration = owner
                .configuration(&source_id)
                .ok_or_else(source_access_unavailable)?;
            let (first, last) = database
                .collection_source_path_bounds(&media_uri, album_artist, folder)
                .await
                .map_err(string_error)?
                .ok_or_else(source_access_unavailable)?;
            configuration
                .detail_folder_uri(&first, &last)
                .map_err(string_error)?
                .ok_or_else(|| SourceError::NotFound.to_string())
        })
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl SourceOwner {
    pub fn discover_servers(&self, provider: sources::DiscoveryProvider) {
        let owner = self.clone();
        let publish = move |update: DiscoveryUpdate| {
            owner.shared.discovery.send_replace(Some(update.clone()));
            let _ = owner.shared.outputs.discovery.try_send(update);
        };
        publish(DiscoveryUpdate {
            provider,
            servers: Vec::new(),
            status: DiscoveryStatus::Searching,
        });
        self.shared.runtime.spawn(async move {
            let update =
                match sources::discover_servers(provider, Duration::from_millis(1_500)).await {
                    Ok(servers) if servers.is_empty() => DiscoveryUpdate {
                        provider,
                        servers: Vec::new(),
                        status: DiscoveryStatus::Empty,
                    },
                    Ok(servers) => DiscoveryUpdate {
                        provider,
                        status: DiscoveryStatus::Found(servers.len() as u64),
                        servers: servers.into(),
                    },
                    Err(error) => DiscoveryUpdate {
                        provider,
                        servers: Vec::new(),
                        status: DiscoveryStatus::Failed(error.to_string()),
                    },
                };
            publish(update);
        });
    }
}

impl SourceOwner {
    pub fn configure_source(
        &self,
        mut input: SourceSetup,
    ) -> Receiver<Result<SelectedLibrary, String>> {
        let (sender, receiver) = async_channel::bounded(1);
        let cancelled = self.shared.begin_selection();
        let owner = self.clone();
        self.shared.runtime.spawn(async move {
            if cancelled.load(Ordering::Acquire) {
                let _ = sender.try_send(Err(SourceError::Cancelled.to_string()));
                return;
            }
            owner
                .publish_operation(SourceOperation::Adding {
                    progress: initial_progress(),
                })
                .await;
            let mut persisted_source_id = None;
            let result = async {
                let reused_credential = owner.reuse_plex_login(&mut input).await?;
                let device_id = owner.shared.settings.load().jellyfin_device_id;
                let connected = Source::connect(fresh_source_id()?, input, &device_id)
                    .await
                    .map_err(string_error)?;
                let (configuration, mut source, credential) = connected.into_parts();
                if !owner.shared.selection_is_current(&cancelled) {
                    return Err(SourceError::Cancelled.to_string());
                }
                let configured = ConfiguredSource {
                    configuration: configuration.clone(),
                    credential: reused_credential,
                    music_folder_id: None,
                    local_access: None,
                    enable_half_stars: false,
                };
                let configured = owner.persist_connected_source(&configured, credential)?;
                persisted_source_id = Some(configuration.source_id.clone());
                let source_state = owner
                    .shared
                    .database
                    .reconcile_source(
                        &configuration.source_id,
                        &configuration.kind,
                        &configuration.name,
                    )
                    .await
                    .map_err(string_error)?;
                owner.bind_plex_login(&configured, &mut source)?;
                let source = Arc::new(source);
                owner
                    .install_selected(
                        configured,
                        Some(Arc::clone(&source)),
                        source_state,
                        Arc::clone(&cancelled),
                    )
                    .await?;
                Ok(source)
            }
            .await;
            if !owner.shared.selection_is_current(&cancelled) {
                let _ = sender.try_send(Err(SourceError::Cancelled.to_string()));
                return;
            }
            match result {
                Ok(source) => {
                    let Some(selected) = owner.selected_library() else {
                        let _ = sender.try_send(Err(SourceError::Cancelled.to_string()));
                        return;
                    };
                    let _ = sender.try_send(Ok(selected));
                    let _ = owner
                        .refresh_now(source.source_id(), &source, false, true)
                        .await;
                }
                Err(error) => {
                    if let Some(selected) = owner.shared.selected()
                        && let Some(source) = owner.cached_client(&selected.source_id)
                    {
                        owner.start_observer(selected, source, false);
                    }
                    owner
                        .publish_operation(SourceOperation::Failed {
                            source_id: persisted_source_id,
                            message: error.clone(),
                            add_form: true,
                        })
                        .await;
                    let _ = sender.try_send(Err(error));
                }
            }
        });
        receiver
    }

    pub fn update_source(&self, input: SourceSettingsChange) -> Receiver<Result<(), String>> {
        let (sender, receiver) = async_channel::bounded(1);
        let owner = self.clone();
        self.shared.runtime.spawn(async move {
            let result = owner.edit_configured_source(input).await;
            if let Err(error) = &result {
                owner.shared.warn_nonfatal(&error);
            }
            let _ = sender.try_send(result);
        });
        receiver
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl SourceOwner {
    pub fn set_half_stars(&self, source_id: SourceId, enabled: bool) {
        let owner = self.clone();
        self.shared.runtime.spawn(async move {
            if let Err(error) = owner.shared.settings.update(|stored| {
                let configured = stored
                    .sources
                    .configured
                    .iter_mut()
                    .find(|configured| configured.configuration.source_id == source_id)
                    .ok_or_else(|| "the configured source no longer exists".to_string())?;
                configured.enable_half_stars = enabled;
                Ok(())
            }) {
                owner.shared.warn_nonfatal(&error);
                return;
            }
            owner
                .shared
                .send(SourceEvent::Configured(
                    owner
                        .shared
                        .configured_sources(owner.shared.selected().as_ref()),
                ))
                .await;
        });
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl SourceOwner {
    /// Completes when the source's stored catalog is selected. Connection and refresh
    /// continue in the background, including while credentials are being unlocked.
    pub async fn select_source(&self, source_id: SourceId) -> sources::SourceResult<SourceId> {
        let cancelled = self.shared.begin_selection();
        let owner = self.clone();
        self.shared
            .runtime
            .spawn(async move {
                if cancelled.load(Ordering::Acquire) {
                    return Err(SourceError::Cancelled.to_string());
                }
                owner.shared.publish_operation(SourceOperation::Switching {
                    target: source_id.clone(),
                    progress: initial_progress(),
                });
                let result = owner
                    .select_now(source_id.clone(), Arc::clone(&cancelled), true)
                    .await;
                if cancelled.load(Ordering::Acquire) {
                    return Err(SourceError::Cancelled.to_string());
                }
                if let Err(error) = &result {
                    if let Some(selected) = owner.shared.selected()
                        && let Some(source) = owner.cached_client(&selected.source_id)
                    {
                        owner.start_observer(selected, source, false);
                    }
                    owner.shared.publish_operation(SourceOperation::Failed {
                        source_id: Some(source_id),
                        message: error.clone(),
                        add_form: false,
                    });
                }
                result?;
                owner
                    .selected_library()
                    .map(|selected| selected.source_id)
                    .ok_or_else(|| SourceError::Cancelled.to_string())
            })
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?
            .map_err(SourceError::Other)
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl SourceOwner {
    pub async fn change_secret_storage(
        &self,
        mode: SecretStorageMode,
    ) -> sources::SourceResult<()> {
        let (sender, receiver) = async_channel::bounded(1);
        let owner = self.clone();
        self.shared.runtime.spawn(async move {
            let previous = owner.shared.settings.load();
            let changing = previous.ui.application.secret_storage_mode != mode;
            let result = async {
                if !changing {
                    return Ok(());
                }
                let connect = owner
                    .shared
                    .connect
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .upgrade();
                if let Some(connect) = connect {
                    connect.reset_secret_storage().await?;
                }
                let settings = owner.shared.settings.clone();
                let secrets = Arc::clone(&owner.shared.secrets);
                tokio::task::spawn_blocking(move || {
                    for name in ["secrets.json", "backup-password.json"] {
                        if let Err(error) = std::fs::remove_file(settings.config_dir().join(name))
                            && error.kind() != std::io::ErrorKind::NotFound
                        {
                            warn!(%error, "could not remove old credential file");
                        }
                    }
                    let scope = fresh_source_id()?.to_string();
                    settings.update(|stored| {
                        stored.ui.application.secret_storage_mode = mode;
                        // Reset as confirmed in Preferences; never unlock the old keyring.
                        stored.secret_scope_id = scope;
                        stored.ui.lastfm_api_key.clear();
                        stored.scrobbling.lastfm.username.clear();
                        stored.scrobbling.librefm.username.clear();
                        for descriptor in scrobbling::secret_descriptors() {
                            descriptor.value_mut(&mut stored.scrobbling).clear();
                        }
                        stored.scrobbling_secrets_present = false;
                        stored.scrobbling_credentials.clear();
                        for source in stored
                            .sources
                            .configured
                            .iter_mut()
                            .chain(&mut stored.sources.integrations)
                        {
                            source.credential = None;
                        }
                        Ok(())
                    })?;
                    secrets.replace(platform_secret_store(&settings));
                    settings.web_controller_credentials_changed();
                    Ok(())
                })
                .await
                .map_err(string_error)
                .and_then(|result| result)
            }
            .await;
            let changed = result.is_ok() && changing;
            let _ = sender.send(result).await;
            if changed {
                if let Err(error) = owner.shared.retire_changed_clients(true).await {
                    owner.shared.warn_nonfatal(&error);
                }
                owner
                    .shared
                    .plex_logins
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clear();
                owner.release_selected(true).await;
                if let Some(selected) = owner.shared.settings.load().sources.selected_source_id {
                    let selected_owner = owner.clone();
                    owner.shared.runtime.spawn(async move {
                        let _ = selected_owner.select_source(selected).await;
                    });
                }
                owner
                    .shared
                    .send(SourceEvent::Configured(
                        owner.shared.configured_sources(None),
                    ))
                    .await;
            }
        });
        receiver
            .recv()
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?
            .map_err(SourceError::Other)
    }
}

impl SourceOwner {
    pub(crate) async fn ensure_local_source(&self) -> Result<Arc<Source>, String> {
        let (configuration, created) = if let Some(configuration) =
            self.shared.settings.local_configuration()
        {
            (configuration, false)
        } else {
            let configuration = SourceConfiguration::local(fresh_source_id()?, "Local", Vec::new())
                .map_err(string_error)?;
            self.shared.settings.update(|stored| {
                if let Some(local) = stored
                    .sources
                    .configured
                    .iter()
                    .find(|item| item.configuration.is_local())
                {
                    return Ok((local.configuration.clone(), false));
                }
                stored.sources.configured.push(ConfiguredSource {
                    configuration: configuration.clone(),
                    credential: None,
                    music_folder_id: None,
                    local_access: None,
                    enable_half_stars: false,
                });
                stored.ui.set_download_quality(
                    configuration.source_id.clone(),
                    sources::DownloadQuality::Transcoded(library::AudioEncoding {
                        codec: library::AudioCodec::Opus,
                        bitrate_kbps: Some(128),
                        channels: library::AudioChannels::Preserve,
                    }),
                );
                Ok((configuration.clone(), true))
            })?
        };
        self.shared
            .database
            .reconcile_source(
                &configuration.source_id,
                &configuration.kind,
                &configuration.name,
            )
            .await
            .map_err(string_error)?;
        if created {
            self.shared
                .downloads
                .settings_changed(self.shared.settings.load().ui.downloads);
            self.shared
                .send(SourceEvent::Configured(
                    self.shared
                        .configured_sources(self.shared.selected().as_ref()),
                ))
                .await;
        }
        self.client(&configuration.source_id)
    }

    pub fn add_local_paths(&self, paths: Vec<PathBuf>) -> Receiver<Result<(), String>> {
        let (sender, receiver) = async_channel::bounded(1);
        let owner = self.clone();
        self.shared.runtime.spawn(async move {
            let result = async {
                let source = owner.ensure_local_source().await?;
                let configuration = owner
                    .configuration(source.source_id())
                    .ok_or_else(source_access_unavailable)?;
                let mut roots = local_roots(&configuration)?;
                roots.extend(paths);
                owner
                    .edit_configured_source(SourceSettingsChange::Local {
                        source_id: source.source_id().clone(),
                        roots,
                        excluded_folders: None,
                    })
                    .await?;
                if owner.shared.selected().is_none() {
                    let unchanged =
                        owner.configuration(source.source_id()).as_ref() == Some(&configuration);
                    owner
                        .select_now(
                            source.source_id().clone(),
                            owner.shared.begin_selection(),
                            unchanged,
                        )
                        .await?;
                }
                Ok(())
            }
            .await;
            let _ = sender.send(result).await;
        });
        receiver
    }

    pub fn document_roots(&self) -> Result<Vec<sources::DocumentRoot>, String> {
        let stored = self.shared.settings.load();
        let Some(local) = stored
            .sources
            .configured
            .iter()
            .find(|source| source.configuration.is_local())
        else {
            return Ok(Vec::new());
        };
        match local.configuration.editable().map_err(string_error)? {
            sources::EditableSource::Local { document_roots, .. } => Ok(document_roots),
            _ => unreachable!(),
        }
    }

    pub fn add_document_roots(
        &self,
        added: Vec<sources::DocumentRoot>,
    ) -> Receiver<Result<(), String>> {
        let (sender, receiver) = async_channel::bounded(1);
        let owner = self.clone();
        self.shared.runtime.spawn(async move {
            let result = async {
                let source = owner.ensure_local_source().await?;
                let previous = owner.document_roots()?;
                let mut roots = previous.clone();
                for mut root in added {
                    if let Some(current) = roots.iter_mut().find(|current| current.uri == root.uri)
                    {
                        root.id = current.id.clone();
                        *current = root;
                    } else {
                        roots.push(root);
                    }
                }
                let unchanged = roots == previous;
                owner
                    .edit_configured_source(SourceSettingsChange::LocalDocuments {
                        source_id: source.source_id().clone(),
                        roots,
                    })
                    .await?;
                if owner
                    .shared
                    .selected()
                    .is_none_or(|selected| selected.source_id != *source.source_id())
                {
                    owner
                        .select_now(
                            source.source_id().clone(),
                            owner.shared.begin_selection(),
                            unchanged,
                        )
                        .await?;
                }
                Ok(())
            }
            .await;
            let _ = sender.send(result).await;
        });
        receiver
    }

    pub fn replace_local_folder(&self, current: String, replacement: PathBuf) {
        edit_local_roots(self, move |roots| {
            if let Some(root) = roots
                .iter_mut()
                .find(|root| root.to_string_lossy() == current)
            {
                *root = replacement;
            }
        });
    }

    pub fn remove_local_folder(&self, path: String) {
        edit_local_roots(self, move |roots| {
            roots.retain(|root| root.to_string_lossy() != path)
        });
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl SourceOwner {
    pub async fn refresh_source(
        &self,
        source_id: SourceId,
        automatic: bool,
    ) -> sources::SourceResult<()> {
        let source = self
            .open_client(source_id.clone())
            .await
            .map_err(SourceError::Other)?;
        self.refresh_now(&source_id, &source, automatic, true)
            .await
            .map_err(SourceError::Other)
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl SourceOwner {
    pub async fn local_access_status(
        &self,
        source_id: SourceId,
    ) -> sources::SourceResult<LocalAccessStatus> {
        self.reply(move |owner, database| async move {
            let configured = configured_source(&owner.shared.settings.load().sources, &source_id)?;
            let Some(source) = database
                .source_identity_key(&source_id)
                .await
                .map_err(string_error)?
            else {
                return Ok(LocalAccessStatus::default());
            };
            let cancellation = ReadCancellation::new();
            let (_, total_track_count) = database
                .source_counts(source, &cancellation)
                .await
                .map_err(string_error)?;
            let matched_track_count = match configured.local_access {
                Some(access) => database
                    .mapping_formula_match_count(
                        source,
                        access.root_path.to_string_lossy().as_ref(),
                        access.server_prefix.as_deref(),
                        &cancellation,
                    )
                    .await
                    .map_err(string_error)?,
                None => 0,
            };
            let sample_source_path = database
                .mapping_track_page(source, None, None, 1, &cancellation)
                .await
                .map_err(string_error)?
                .pop()
                .map(|track| track.source_path);
            Ok(LocalAccessStatus {
                total_track_count,
                matched_track_count,
                sample_source_path,
            })
        })
        .recv()
        .await
        .map_err(|error| SourceError::Other(error.to_string()))?
        .map_err(SourceError::Other)
    }

    pub async fn save_local_access(
        &self,
        mut input: SourceLocalAccess,
        wait_until_mapped: bool,
    ) -> sources::SourceResult<()> {
        let (sender, receiver) = async_channel::bounded(1);
        let owner = self.clone();
        self.shared.runtime.spawn(async move {
            let cancelled = Arc::new(AtomicBool::new(false));
            let result = async {
                let (source, source_key) = owner.source_target(&input.source_id).await?;
                {
                    let mut clients = owner
                        .shared
                        .clients
                        .lock()
                        .unwrap_or_else(|p| p.into_inner());
                    if let Some(cached) = clients.get_mut(&input.source_id) {
                        if let Some(previous) = cached.mapping.replace(cancelled.clone()) {
                            previous.store(true, Ordering::Release);
                        }
                    }
                }
                let (root_path, server_prefix) = source
                    .apply_local_mapping(
                        &owner.shared.database,
                        source_key,
                        &input.root_path,
                        input.server_prefix.as_deref(),
                        input.local_prefix.as_deref(),
                        input.sample_source_path.as_deref(),
                        Arc::clone(&cancelled),
                    )
                    .await
                    .map_err(string_error)?;
                input.server_prefix = server_prefix;
                if source.is_retired() || cancelled.load(Ordering::Acquire) {
                    return Err(SourceError::Cancelled.to_string());
                }
                owner.shared.settings.update(|stored| {
                    let configured = stored
                        .sources
                        .configured
                        .iter_mut()
                        .find(|configured| configured.configuration.source_id == input.source_id)
                        .ok_or_else(|| "the mapped source is no longer configured".to_string())?;
                    configured.local_access = Some(SavedLocalAccess {
                        root_path: root_path.clone(),
                        server_prefix: input.server_prefix.clone(),
                        local_prefix: input.local_prefix.clone(),
                    });
                    Ok(())
                })?;
                owner.shared.playback()?.stream_inputs_changed()?;
                owner
                    .shared
                    .send(SourceEvent::Configured(owner.list_sources()))
                    .await;
                Ok((root_path, source, source_key))
            }
            .await;
            let (root_path, source, source_key) = match result {
                Ok(completed) => completed,
                Err(error) => {
                    let _ = sender.send(Err(error)).await;
                    return;
                }
            };
            if !wait_until_mapped {
                let _ = sender.send(Ok(())).await;
            }
            let completion: Result<(), String> = async {
                if cancelled.load(Ordering::Acquire) {
                    return Err("Local file mapping was cancelled".to_string());
                }
                source
                    .complete_local_mapping(
                        &owner.shared.database,
                        source_key,
                        &root_path,
                        input.server_prefix.as_deref(),
                        input.local_prefix.as_deref(),
                        Arc::clone(&cancelled),
                    )
                    .await
                    .map_err(string_error)?;
                owner.shared.playback()?.stream_inputs_changed()?;
                owner
                    .shared
                    .send(SourceEvent::Configured(owner.list_sources()))
                    .await;
                Ok(())
            }
            .await;
            if wait_until_mapped {
                let _ = sender.send(completion).await;
            } else if let Err(error) = completion {
                warn!(%error, "background Local mapping did not complete");
            }
        });
        receiver
            .recv()
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?
            .map_err(SourceError::Other)
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl SourceOwner {
    pub fn clear_local_access(&self, source_id: SourceId) {
        let owner = self.clone();
        self.shared.runtime.spawn(async move {
            if let Some(cached) = owner
                .shared
                .clients
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get_mut(&source_id)
                && let Some(token) = cached.mapping.take()
            {
                token.store(true, Ordering::Release);
            }

            if let Ok(Some(source)) = owner.shared.database.source_identity_key(&source_id).await {
                let _ = owner.shared.database.clear_mapping_access(source).await;
                let _ = owner
                    .shared
                    .playback()
                    .and_then(|playback| playback.stream_inputs_changed());
            }
            let _ = owner.shared.settings.update(|stored| {
                if let Some(configured) = stored
                    .sources
                    .configured
                    .iter_mut()
                    .find(|configured| configured.configuration.source_id == source_id)
                {
                    configured.local_access = None;
                }
                Ok(())
            });
            owner
                .shared
                .send(SourceEvent::Configured(
                    owner
                        .shared
                        .configured_sources(owner.shared.selected().as_ref()),
                ))
                .await;
        });
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl SourceOwner {
    pub async fn forget_source(&self, source_id: SourceId) -> sources::SourceResult<()> {
        let selected = self
            .shared
            .selected()
            .is_some_and(|selected| selected.source_id == source_id);
        self.shared
            .clients
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&source_id);
        if selected {
            self.shared.cancel_selection();
        }
        if matches!(&*self.shared.operation.borrow(), SourceOperation::Refreshing { source_id: active, .. } if active == &source_id)
        {
            self.shared.publish_operation(SourceOperation::Idle);
        }
        let (sender, receiver) = async_channel::bounded(1);
        self.spawn_serialized(move |owner| async move {
            let stored = owner.shared.settings.load();
            let configured = stored
                .sources
                .configured
                .iter()
                .find(|item| item.configuration.source_id == source_id)
                .cloned();
            let selected = owner
                .shared
                .selected()
                .filter(|selected| selected.source_id == source_id);
            if selected.is_some() {
                owner.release_selected(true).await;
            }
            let playback = owner.shared.playback().ok();
            let identity = owner
                .shared
                .database
                .source_identity_key(&source_id)
                .await
                .ok()
                .flatten();
            if let Some(source_key) = identity {
                if let Some(playback) = playback.as_ref() {
                    if let Err(error) = playback.forget_source(source_key).await {
                        owner.shared.warn_nonfatal(&error);
                        let _ = sender.try_send(Err(error));
                        return;
                    }
                }
            }
            let removed = owner
                .shared
                .database
                .remove_source(&source_id)
                .await
                .map(|_| ())
                .map_err(string_error);
            if let Err(error) = &removed {
                owner.shared.warn_nonfatal(error);
            }
            if let Err(error) = owner.shared.settings.update(|stored| {
                stored
                    .sources
                    .configured
                    .retain(|item| item.configuration.source_id != source_id);
                if stored.sources.selected_source_id.as_ref() == Some(&source_id) {
                    stored.sources.selected_source_id = None;
                }
                Ok(())
            }) {
                owner.shared.warn_nonfatal(&error);
                let _ = sender.try_send(Err(error));
                return;
            }
            owner
                .remove_source_resources(&source_id, configured.and_then(|item| item.credential))
                .await;
            owner
                .shared
                .send(SourceEvent::Configured(
                    owner.shared.configured_sources(None),
                ))
                .await;
            let _ = sender.try_send(removed);
        });
        receiver
            .recv()
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?
            .map_err(SourceError::Other)
    }
}

impl SourceOwner {
    pub(crate) fn install_connect(&self, connect: &Arc<crate::connect::ConnectOwner>) {
        *self
            .shared
            .connect
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Arc::downgrade(connect);
    }

    pub(crate) fn peer_access(&self) -> sources::PeerAccess {
        self.shared.downloads.peer_access()
    }

    pub(crate) fn connect_media_changed(&self) {
        self.shared.downloads.connect_changed();
    }

    pub fn download_media(&self, subject: downloads::DownloadSubject, media_uris: Vec<String>) {
        let owner = self.clone();
        self.shared.runtime.spawn(async move {
            let mut source_ids = Vec::new();
            for media_uri in &media_uris {
                let source = match library::source_entity_parts(media_uri) {
                    Some((source, kind, _)) if kind == "track" => Some(source),
                    _ => owner
                        .shared
                        .database
                        .track_source_by_uri(media_uri, &ReadCancellation::new())
                        .await
                        .ok()
                        .flatten(),
                };
                if let Some(source) = source {
                    source_ids.push(source);
                }
            }
            source_ids.sort();
            source_ids.dedup();
            for source_id in source_ids {
                let (source, source_key) = match owner.source_target(&source_id).await {
                    Ok(target) => target,
                    Err(error) => {
                        owner.shared.warn_nonfatal(&error);
                        continue;
                    }
                };
                if let Err(error) = owner
                    .shared
                    .downloads
                    .attach(
                        source_id.clone(),
                        source_key,
                        Some(source),
                        None,
                        owner.source_input_digest(&source_id),
                    )
                    .await
                {
                    owner.shared.warn_nonfatal(&error);
                }
            }
            owner.shared.downloads.download(subject, media_uris);
        });
    }

    pub fn set_favorite(&self, target: FavoriteTarget, favorite: bool) {
        let owner = self.clone();
        self.shared.runtime.spawn(async move {
            let _ = owner.set_favorite_with_result(target, favorite).await;
        });
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl SourceOwner {
    pub async fn set_favorite_with_result(
        &self,
        target: FavoriteTarget,
        favorite: bool,
    ) -> sources::SourceResult<bool> {
        info!(kind = target.kind(), favorite, "Favorite requested");
        let (sender, receiver) = async_channel::bounded(1);
        self.spawn_serialized(move |owner| async move {
            let result = match owner.apply_favorite(target, favorite).await {
                Ok(()) => Ok(favorite),
                Err(error) => Err(error),
            };
            let _ = sender.try_send(result);
        });
        receiver
            .recv()
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?
            .map_err(SourceError::Other)
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl SourceOwner {
    pub fn set_rating(&self, target: FavoriteTarget, rating: Option<u8>) {
        let owner = self.clone();
        self.shared.runtime.spawn(async move {
            owner.apply_rating(target, rating).await;
        });
    }
}

impl SourceOwner {
    pub fn selected_library_revealed(&self, source_id: SourceId) {
        let Some(selected) = self
            .selected_library()
            .filter(|selected| selected.source_id == source_id)
        else {
            return;
        };
        let mut active = self
            .shared
            .album_release
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(task) = active.take() {
            task.abort();
        }
        let task =
            self.shared
                .runtime
                .spawn(crate::album_release::run_selected_album_release_lookup(
                    self.shared.database.clone(),
                    self.shared.settings.clone(),
                    self.shared.outputs.events.clone(),
                    selected.source_key,
                ));
        *active = Some(task.abort_handle());
        drop(active);
    }

    pub fn refresh_home(
        &self,
        source_id: SourceId,
        kind: crate::settings::HomeSectionKind,
    ) -> Receiver<Result<(), String>> {
        use crate::settings::HomeSectionKind;
        if kind == HomeSectionKind::NewlyAdded {
            return self.reply(move |owner, _| async move {
                owner
                    .refresh_source(source_id, false)
                    .await
                    .map_err(string_error)
            });
        }
        self.reply(move |owner, database| async move {
            if kind == HomeSectionKind::Explore {
                owner
                    .shared
                    .home_explore_variation
                    .fetch_add(1, Ordering::Relaxed);
                if let Some(selected) = owner
                    .selected_library()
                    .filter(|selected| selected.source_id == source_id)
                {
                    owner
                        .publish_catalog(&selected, None, CatalogChange::Home)
                        .await;
                }
                return Ok(());
            }
            let source = owner.open_client(source_id.clone()).await?;
            let section = match kind {
                HomeSectionKind::MostPlayed => sources::SourceHomeSection::MostPlayed,
                HomeSectionKind::RecentlyPlayed => sources::SourceHomeSection::RecentlyPlayed,
                HomeSectionKind::RecentlyReleased => sources::SourceHomeSection::RecentlyReleased,
                HomeSectionKind::Explore | HomeSectionKind::NewlyAdded => unreachable!(),
            };
            if let Err(error) = source.refresh_home_section(&database, section).await {
                warn!(%error,"could not refresh Home section");
            }
            Ok(())
        })
    }

    pub fn set_music_folder(
        &self,
        source_id: SourceId,
        folder_object_id: Option<String>,
    ) -> Receiver<Result<(), String>> {
        let (sender, receiver) = async_channel::bounded(1);
        let owner = self.clone();
        self.shared.runtime.spawn(async move {
            let result = async {
                let source_key = owner
                    .shared
                    .database
                    .source_identity_key(&source_id)
                    .await
                    .map_err(string_error)?
                    .ok_or_else(source_access_unavailable)?;
                let key = match folder_object_id.as_deref() {
                    Some(id) => owner
                        .shared
                        .database
                        .folder_identity_key(source_key, id)
                        .await
                        .map_err(string_error)?,
                    None => None,
                };
                if folder_object_id.is_some() && key.is_none() {
                    return Ok(());
                }
                owner.shared.settings.update(|stored| {
                    if let Some(item) = stored
                        .sources
                        .configured
                        .iter_mut()
                        .find(|item| item.configuration.source_id == source_id)
                    {
                        item.music_folder_id = folder_object_id.clone();
                    }
                    Ok(())
                })?;
                if let Some(current) = owner
                    .shared
                    .selected
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .as_mut()
                    .filter(|selected| selected.source_key == source_key)
                {
                    current.music_folder_key = key;
                    current.music_folder_object_id = folder_object_id;
                }
                owner
                    .shared
                    .send(SourceEvent::Configured(owner.list_sources()))
                    .await;
                owner
                    .shared
                    .send(SourceEvent::CatalogPublished(CatalogPublication {
                        source_key: Some(source_key),
                        favorite: None,
                        change: CatalogChange::Scope,
                    }))
                    .await;
                let downloads_owner = owner.clone();
                owner.shared.runtime.spawn(async move {
                    if let Err(error) = downloads_owner
                        .shared
                        .downloads
                        .attach(
                            source_id.clone(),
                            source_key,
                            downloads_owner.cached_client(&source_id),
                            key,
                            downloads_owner.source_input_digest(&source_id),
                        )
                        .await
                    {
                        downloads_owner.shared.warn_nonfatal(&error);
                    }
                });
                Ok(())
            }
            .await;
            let _ = sender.try_send(result);
        });
        receiver
    }
}

impl SourceOwner {
    pub(crate) async fn source_target(
        &self,
        source_id: &SourceId,
    ) -> Result<(Arc<Source>, SourceKey), String> {
        let source = self.client(source_id)?;
        let publication = self
            .shared
            .database
            .cached_source(source_id.as_str(), &ReadCancellation::new())
            .await
            .map_err(string_error)?
            .ok_or_else(source_access_unavailable)?;
        Ok((source, publication.source))
    }

    async fn favorite_source(
        &self,
        target: &FavoriteTarget,
    ) -> Option<(SourceId, SourceKey, bool)> {
        let source_id = match library::source_entity_parts(target.media_uri()) {
            Some((source_id, kind, _)) if kind == target.kind() => source_id,
            _ => match target {
                FavoriteTarget::Track(media_uri) => self
                    .shared
                    .database
                    .track_source_by_uri(media_uri, &ReadCancellation::new())
                    .await
                    .ok()
                    .flatten()?,
                FavoriteTarget::Album(_) | FavoriteTarget::Artist(_) => return None,
            },
        };
        let source_key = self
            .shared
            .database
            .cached_source(source_id.as_str(), &ReadCancellation::new())
            .await
            .ok()
            .flatten()?
            .source;
        let configuration = self.configuration(&source_id)?;
        let local = configuration.is_file_library() || configuration.kind == "plex";
        Some((source_id, source_key, local))
    }

    async fn apply_favorite(&self, target: FavoriteTarget, favorite: bool) -> Result<(), String> {
        let source = self.favorite_source(&target).await;
        let changed = if source.as_ref().is_some_and(|(_, _, local)| !local) {
            self.shared
                .database
                .queue_remote_favorite(&target, favorite, unix_seconds())
                .await
        } else {
            self.shared.database.set_favorite(&target, favorite).await
        }
        .map_err(string_error)?;
        if !changed {
            return Ok(());
        }
        self.publish_current_catalog(
            Some(FavoriteSettlement {
                target: target.clone(),
                requested: favorite,
                effective: favorite,
            }),
            CatalogChange::Broad,
        )
        .await;
        if source.is_some_and(|(_, _, local)| !local) {
            self.shared.maintenance.notify_one();
        }
        Ok(())
    }

    async fn apply_rating(&self, target: FavoriteTarget, rating: Option<u8>) {
        let lane = self.shared.lane.lock().await;
        if !self
            .shared
            .database
            .set_rating(&target, rating)
            .await
            .unwrap_or(false)
        {
            return;
        }
        let source = self
            .favorite_source(&target)
            .await
            .and_then(|(id, key, local)| {
                self.client(&id).ok().map(|source| (source, id, key, local))
            });
        drop(lane);
        self.publish_current_catalog(None, CatalogChange::Broad)
            .await;
        if let Some((source, source_id, source_key, local)) = source {
            match &target {
                FavoriteTarget::Track(uri) if local => {
                    if let Err(error) = source
                        .write_file_track_rating(&self.shared.database, source_key, uri, rating)
                        .await
                    {
                        warn!(%error,"could not write file Track rating");
                    }
                }
                _ if !local => {
                    if let Some((_, object_id)) = favorite_object(&source_id, &target)
                        && let Err(error) = source.set_rating(&object_id, rating).await
                    {
                        warn!(%error, "could not write source rating");
                    }
                }
                _ => {}
            }
        }
    }

    pub(crate) fn reply<T, F, Work>(&self, work: F) -> Receiver<T>
    where
        T: Send + 'static,
        F: FnOnce(SourceOwner, Arc<Database>) -> Work + Send + 'static,
        Work: Future<Output = T> + Send + 'static,
    {
        let (sender, receiver) = async_channel::bounded(1);
        let owner = self.clone();
        let database = Arc::clone(&self.shared.database);
        self.shared.runtime.spawn(async move {
            let _ = sender.send(work(owner, database).await).await;
        });
        receiver
    }

    pub(crate) async fn media_client(&self, media_uri: &str) -> Result<Arc<Source>, String> {
        let source_id = match library::source_entity_parts(media_uri) {
            Some((source_id, _, _)) => Some(source_id),
            None => self
                .shared
                .database
                .track_source_by_uri(media_uri, &ReadCancellation::new())
                .await
                .map_err(string_error)?,
        }
        .ok_or_else(source_access_unavailable)?;
        self.client(&source_id)
    }

    pub(crate) fn client(&self, source_id: &SourceId) -> Result<Arc<Source>, String> {
        let stored = self.shared.settings.load();
        let configured = stored
            .sources
            .configured
            .iter()
            .chain(&stored.sources.integrations)
            .find(|s| &s.configuration.source_id == source_id)
            .cloned()
            .ok_or("The connection no longer exists")?;
        if let Some(cached) = self
            .shared
            .clients
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(source_id)
            && cached.matches(&configured, &stored.jellyfin_device_id)
        {
            return Ok(cached.source.clone());
        }
        let credential = configured
            .credential
            .as_ref()
            .map(|reference| load_provider_secret(self.shared.secrets.as_ref(), reference))
            .transpose()?
            .flatten();

        let mut source = Source::open(
            configured.configuration.clone(),
            credential.clone(),
            Some(stored.jellyfin_device_id.clone()),
        )
        .map_err(string_error)?;
        self.bind_plex_login(&configured, &mut source)?;
        let source = Arc::new(source);
        let mut clients = self
            .shared
            .clients
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let current = self.shared.settings.load();
        if current.jellyfin_device_id == stored.jellyfin_device_id
            && current
                .sources
                .configured
                .iter()
                .chain(&current.sources.integrations)
                .any(|item| {
                    same_native_input(&item.configuration, &configured.configuration)
                        && item.credential == configured.credential
                })
        {
            if let Some(cached) = clients.get(source_id)
                && cached.matches(&configured, &stored.jellyfin_device_id)
            {
                return Ok(cached.source.clone());
            }
            clients.insert(
                source_id.clone(),
                CachedSource {
                    configuration: configured.configuration,
                    credential: configured.credential,
                    jellyfin_device_id: stored.jellyfin_device_id,
                    source: Arc::clone(&source),
                    refresh: None,
                    observer: None,
                    mapping: None,
                },
            );
            return Ok(source);
        }
        Err(SourceError::Cancelled.to_string())
    }

    pub(crate) async fn open_client(&self, source_id: SourceId) -> Result<Arc<Source>, String> {
        let owner = self.clone();
        tokio::task::spawn_blocking(move || owner.client(&source_id))
            .await
            .map_err(string_error)?
    }

    pub(crate) fn cached_client(&self, source_id: &SourceId) -> Option<Arc<Source>> {
        self.shared
            .clients
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(source_id)
            .map(|cached| cached.source.clone())
    }

    fn bind_plex_login(
        &self,
        configured: &ConfiguredSource,
        source: &mut Source,
    ) -> Result<(), String> {
        let Some(login) = source.plex_login() else {
            return Ok(());
        };
        let reference = configured
            .credential
            .as_ref()
            .ok_or("Plex credentials are missing")?;
        let mut logins = self
            .shared
            .plex_logins
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(existing) = logins.get(reference.as_str()) {
            source.bind_plex_login(Arc::clone(existing));
            return Ok(());
        }
        let shared = Arc::downgrade(&self.shared);
        let reference = reference.clone();
        let saved_reference = Arc::new(Mutex::new(reference.clone()));
        login
            .try_lock()
            .map_err(string_error)?
            .bind_save(Arc::new(move |secret| {
                let shared = shared.upgrade().ok_or(SourceError::Cancelled)?;
                let mut logins = shared
                    .plex_logins
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let mut previous = saved_reference
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let next = seal_credential(shared.secrets.as_ref(), &secret)
                    .map_err(SourceError::Other)?;
                let changed = shared
                    .settings
                    .update(|stored| {
                        let mut changed = false;
                        for source in stored
                            .sources
                            .configured
                            .iter_mut()
                            .filter(|source| source.credential.as_ref() == Some(&previous))
                        {
                            source.credential = Some(next.clone());
                            changed = true;
                        }
                        Ok(changed)
                    })
                    .map_err(SourceError::Other)?;
                if !changed {
                    return Err(SourceError::Cancelled);
                }
                if let Some(login) = logins.remove(previous.as_str()) {
                    logins.insert(next.as_str().to_owned(), login);
                }
                *previous = next;
                Ok(())
            }));
        logins.insert(reference.as_str().to_owned(), login);
        Ok(())
    }

    pub fn configuration(&self, source_id: &SourceId) -> Option<SourceConfiguration> {
        configured_source(&self.shared.settings.load().sources, source_id)
            .ok()
            .map(|configured| configured.configuration)
    }

    async fn deliver_favorite(
        &self,
        source_id: SourceId,
        source_key: SourceKey,
        target: FavoriteTarget,
        favorite: bool,
    ) -> bool {
        let Ok(source) = self.open_client(source_id.clone()).await else {
            return true;
        };
        let Some((kind, object_id)) = favorite_object(&source_id, &target) else {
            return false;
        };
        match source.set_favorite(kind, &object_id, favorite).await {
            Ok(()) => {
                let acknowledgement = self
                    .shared
                    .database
                    .acknowledge_remote_favorite(&target, favorite)
                    .await;
                info!(
                    favorite,
                    acknowledged = matches!(acknowledgement, Ok(true)),
                    "Favorite delivered"
                );
                if acknowledgement.is_err() {
                    return true;
                }
            }
            Err(error) if source_error_is_temporary(&error) => {
                info!(favorite, "Favorite delivery deferred");
                let _ = self
                    .shared
                    .database
                    .defer_remote_favorite(&target, favorite, unix_seconds().saturating_add(30))
                    .await;
                return true;
            }
            Err(_) => {
                info!(favorite, "Favorite delivery rejected");
                let previous = match self
                    .shared
                    .database
                    .reject_remote_favorite(&target, favorite)
                    .await
                {
                    Ok(previous) => previous,
                    Err(_) => return true,
                };
                if let Some(previous) = previous {
                    self.publish_current_catalog(
                        Some(FavoriteSettlement {
                            target,
                            requested: favorite,
                            effective: previous,
                        }),
                        CatalogChange::Broad,
                    )
                    .await;
                    self.shared
                        .send(SourceEvent::Notice(SourceNotice {
                            source_key: self
                                .shared
                                .selected()
                                .map_or(source_key, |selected| selected.source_key),
                            kind: SourceNoticeKind::FavoriteRejected,
                        }))
                        .await;
                }
            }
        }
        false
    }

    async fn deliver_pending_favorites(&self) {
        let now = unix_seconds();
        let pending = match self
            .shared
            .database
            .due_remote_favorites(now, 64, &ReadCancellation::new())
            .await
        {
            Ok(pending) => pending,
            Err(error) => {
                self.shared.warn_nonfatal(&error.to_string());
                return;
            }
        };
        let full = pending.len() == 64;
        let mut completed = true;
        let mut unavailable = HashSet::new();
        for (media_uri, favorite) in pending {
            let Some((source_id, kind, _)) = library::source_entity_parts(&media_uri) else {
                completed = false;
                continue;
            };
            let target = match kind.as_str() {
                "track" => FavoriteTarget::Track(media_uri),
                "album" => FavoriteTarget::Album(media_uri),
                "artist" => FavoriteTarget::Artist(media_uri),
                _ => {
                    completed = false;
                    continue;
                }
            };
            if unavailable.contains(&source_id) {
                let _ = self
                    .shared
                    .database
                    .defer_remote_favorite(&target, favorite, unix_seconds().saturating_add(30))
                    .await;
                completed = false;
                continue;
            }
            let Some((_, source_key, false)) = self.favorite_source(&target).await else {
                completed = false;
                continue;
            };
            if self
                .deliver_favorite(source_id.clone(), source_key, target, favorite)
                .await
            {
                unavailable.insert(source_id);
                completed = false;
            }
        }
        if full && completed {
            self.shared.maintenance.notify_one();
        }
    }
}

fn favorite_object(
    source: &SourceId,
    target: &FavoriteTarget,
) -> Option<(SourceEntityKind, String)> {
    let (source_id, kind, object_id) = library::source_entity_parts(target.media_uri())?;
    if &source_id != source || kind != target.kind() {
        return None;
    }
    let kind = match target {
        FavoriteTarget::Track(_) => SourceEntityKind::Track,
        FavoriteTarget::Album(_) => SourceEntityKind::Album,
        FavoriteTarget::Artist(_) => SourceEntityKind::Artist,
    };
    Some((kind, object_id))
}

fn source_error_is_cancelled(error: &SourceError) -> bool {
    matches!(
        error,
        SourceError::Cancelled | SourceError::Library(library::LibraryError::ReadCancelled)
    )
}

fn source_error_is_temporary(error: &SourceError) -> bool {
    source_error_allows_cache(error)
        || matches!(
            error,
            SourceError::Server {
                status: 408 | 425 | 429,
                ..
            }
        )
}

fn configured_source(
    settings: &crate::settings::SourceSettings,
    source_id: &SourceId,
) -> Result<ConfiguredSource, String> {
    settings
        .configured
        .iter()
        .find(|item| &item.configuration.source_id == source_id)
        .cloned()
        .ok_or_else(|| "the configured source no longer exists".to_string())
}

fn half_stars_enabled(configured: &ConfiguredSource) -> bool {
    matches!(
        configured.configuration.kind.as_str(),
        "jellyfin" | "emby" | "plex"
    ) || configured.enable_half_stars
}

fn edit_local_roots(owner: &SourceOwner, edit: impl FnOnce(&mut Vec<PathBuf>) + Send + 'static) {
    let stored = owner.shared.settings.load();
    let Some(local) = stored
        .sources
        .configured
        .iter()
        .find(|item| item.configuration.is_local())
        .cloned()
    else {
        owner.shared.warn_nonfatal("Local is not configured");
        return;
    };
    let mut roots = local_roots(&local.configuration).unwrap_or_default();
    edit(&mut roots);
    let input = SourceSettingsChange::Local {
        source_id: local.configuration.source_id,
        roots,
        excluded_folders: None,
    };
    let task_owner = owner.clone();
    owner.shared.runtime.spawn(async move {
        let owner = task_owner;
        if let Err(error) = owner.edit_configured_source(input).await {
            owner.shared.warn_nonfatal(&error);
        }
    });
}

fn configured_sources(
    stored: &StoredSettings,
    selected: Option<&SelectedLibrary>,
    counts: &HashMap<SourceId, (usize, usize)>,
) -> ConfiguredSources {
    let sources = stored
        .sources
        .configured
        .iter()
        .map(|configured| SourceSummary {
            id: configured.configuration.source_id.clone(),
            kind: configured.configuration.kind.clone(),
            name: configured.configuration.name.clone(),
            half_stars_enabled: half_stars_enabled(configured),
            supports_playlist_public: configured.configuration.supports_playlist_public(),
        })
        .collect::<Vec<_>>();
    let local_folders = stored
        .sources
        .configured
        .iter()
        .flat_map(|configured| local_roots(&configured.configuration).unwrap_or_default())
        .map(|path| LocalFolder {
            path: path.to_string_lossy().into_owned(),
        })
        .collect::<Vec<_>>();
    let local_access = stored
        .sources
        .configured
        .iter()
        .map(|configured| {
            let access = configured
                .local_access
                .as_ref()
                .map(|access| SourceLocalAccess {
                    source_id: configured.configuration.source_id.clone(),
                    root_path: access.root_path.clone(),
                    server_prefix: access.server_prefix.clone(),
                    local_prefix: access.local_prefix.clone(),
                    sample_source_path: None,
                });
            SourceLocalAccessSummary {
                source_id: configured.configuration.source_id.clone(),
                access,
                selected_music_folder_name: selected
                    .and_then(|selected| {
                        let wanted = selected.music_folder_object_id.as_ref()?;
                        selected
                            .music_folders
                            .iter()
                            .find(|folder| &folder.object_id == wanted)
                    })
                    .map(|folder| folder.name.clone()),
                album_count: counts
                    .get(&configured.configuration.source_id)
                    .map_or(0, |counts| counts.0),
                track_count: counts
                    .get(&configured.configuration.source_id)
                    .map_or(0, |counts| counts.1),
            }
        })
        .collect::<Vec<_>>();
    ConfiguredSources {
        sources: sources.into(),
        selected_source_id: stored.sources.selected_source_id.clone(),
        local_folders: local_folders.into(),
        local_excluded_folders: stored
            .sources
            .configured
            .iter()
            .flat_map(|configured| match configured.configuration.editable() {
                Ok(sources::EditableSource::Local {
                    excluded_folders, ..
                }) => excluded_folders,
                _ => Vec::new(),
            })
            .collect::<Vec<_>>()
            .into(),
        local_access,
        document_roots: stored
            .sources
            .configured
            .iter()
            .filter(|source| {
                Some(&source.configuration.source_id) == stored.sources.selected_source_id.as_ref()
            })
            .flat_map(|source| match source.configuration.editable() {
                Ok(sources::EditableSource::Local { document_roots, .. }) => document_roots,
                _ => Vec::new(),
            })
            .collect(),
    }
}

pub(crate) fn source_error_allows_cache(error: &SourceError) -> bool {
    matches!(
        error,
        SourceError::Network(_)
            | SourceError::Server {
                status: 500..=599,
                ..
            }
    )
}

fn source_refresh_error_text(error: &SourceError) -> String {
    if source_error_allows_cache(error) {
        localization::tr("Server is unreachable")
    } else {
        error.to_string()
    }
}

fn editable_source(configuration: &SourceConfiguration) -> Result<EditableSource, String> {
    let mut editable = EditableSource {
        integration: false,
        source: SourceSummary {
            id: configuration.source_id.clone(),
            kind: configuration.kind.clone(),
            name: configuration.name.clone(),
            half_stars_enabled: matches!(configuration.kind.as_str(), "jellyfin" | "emby" | "plex"),
            supports_playlist_public: configuration.supports_playlist_public(),
        },
        credentials: CredentialPreset {
            source_name: configuration.name.clone(),
            server_url: String::new(),
            username: String::new(),
            trust_invalid_cert: false,
            open_subsonic_authentication: None,
        },
        plex_settings: None,
        file_settings: None,
        use_instant_mix: None,
        emby_connect: false,
    };
    match configuration.editable().map_err(string_error)? {
        sources::EditableSource::Plex { settings, .. } => {
            editable.credentials.server_url = settings.address_override.clone().unwrap_or_default();
            editable.credentials.trust_invalid_cert = settings.trust_invalid_cert;
            editable.plex_settings = Some(settings);
        }
        sources::EditableSource::Credentials {
            credentials,
            use_instant_mix,
            emby_connect,
            ..
        } => {
            editable.credentials = credentials;
            editable.use_instant_mix = use_instant_mix;
            editable.emby_connect = emby_connect;
        }
        sources::EditableSource::Files { settings, .. } => {
            editable.credentials.server_url = settings.url.clone();
            editable.credentials.username = settings.username.clone();
            editable.credentials.trust_invalid_cert = settings.trust_invalid_certificate;
            editable.file_settings = Some(settings);
        }
        sources::EditableSource::Local { .. } => {}
    }
    Ok(editable)
}

fn local_roots(configuration: &SourceConfiguration) -> Result<Vec<PathBuf>, String> {
    match configuration.editable().map_err(string_error)? {
        sources::EditableSource::Local { roots, .. } => Ok(roots),
        _ => Err("the configured source is not Local".to_string()),
    }
}

fn source_progress(progress: SourceReadProgress) -> SourceProgress {
    SourceProgress {
        stage: match progress.stage {
            SourceReadStage::Albums => SourceProgressStage::Albums,
            SourceReadStage::Tracks => SourceProgressStage::Tracks,
            SourceReadStage::Artists => SourceProgressStage::Artists,
            SourceReadStage::Genres => SourceProgressStage::Genres,
            SourceReadStage::Playlists => SourceProgressStage::Playlists,
            SourceReadStage::Home => SourceProgressStage::Home,
            SourceReadStage::Finalizing => SourceProgressStage::Finalizing,
        },
        completed: progress.completed,
        total: progress.total,
    }
}

fn initial_progress() -> SourceProgress {
    SourceProgress {
        stage: SourceProgressStage::Connecting,
        completed: 0,
        total: None,
    }
}

pub(crate) fn source_access_unavailable() -> String {
    "Live source access is unavailable. Check the saved credentials and refresh.".to_string()
}

pub(crate) fn string_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod source_tests {
    use super::*;
    use secrets::SecretStore;

    #[tokio::test]
    async fn plex_sources_share_login_until_last_removal_and_cannot_resurrect_secrets() {
        let directory = tempfile::tempdir().unwrap();
        let database = Arc::new(
            Database::open(directory.path().join("library.sqlite"))
                .await
                .unwrap(),
        );
        let runtime = tokio::runtime::Handle::current();
        let settings = SettingsFile::memory();
        let secrets = Arc::new(SwitchableSecretStore::new(Arc::new(
            secrets::MemorySecretStore::new(),
        )));
        let secret = serde_json::json!({"client_id":"test-device","token":"test-token","account_id":"profile","profiles":{},"resources":{"profile":{"one":"one-token","two":"two-token"}},"home_admin_subscription":false,"download_subscriptions":{}}).to_string();
        let reference = seal_credential(secrets.as_ref(), &secret).unwrap();
        let configured:Vec<_> = ["one","two"].into_iter().map(|server|ConfiguredSource {
            configuration: SourceConfiguration {source_id:SourceId::new(server),kind:"plex".into(),name:server.into(),provider_payload:serde_json::json!({"version":1,"server_id":server,"profile_id":"profile","base_url":"http://127.0.0.1:9","address_override":null,"local":true,"relay":false,"owned":true,"trust_invalid_cert":false}).to_string()},
            credential:Some(reference.clone()),music_folder_id:None,local_access:None,enable_half_stars:false,
        }).collect();
        settings
            .update(|stored| {
                stored.sources.configured = configured.clone();
                Ok(())
            })
            .unwrap();
        let owner = SourceOwner::open_dormant(
            Artwork::new(directory.path().join("artwork"), runtime.clone()).unwrap(),
            Arc::clone(&database),
            Downloads::new(
                artwork::Artwork::new(directory.path().join("downloads/artwork"), runtime.clone())
                    .unwrap(),
                directory.path().join("downloads"),
                database.as_ref().clone(),
                runtime.clone(),
                async_channel::unbounded().0,
                Vec::new(),
            ),
            settings,
            Arc::clone(&secrets),
            runtime,
            SourceOutputs {
                events: async_channel::unbounded().0,
                discovery: async_channel::unbounded().0,
            },
        );
        let mut first = Source::open(
            configured[0].configuration.clone(),
            Some(secret.clone()),
            None,
        )
        .unwrap();
        let mut second =
            Source::open(configured[1].configuration.clone(), Some(secret), None).unwrap();
        owner.bind_plex_login(&configured[0], &mut first).unwrap();
        owner.bind_plex_login(&configured[1], &mut second).unwrap();
        let detached = first.plex_login().unwrap();
        assert!(Arc::ptr_eq(&detached, &second.plex_login().unwrap()));
        owner
            .shared
            .settings
            .update(|stored| {
                stored.sources.configured.remove(0);
                Ok(())
            })
            .unwrap();
        owner
            .remove_source_resources(
                &configured[0].configuration.source_id,
                Some(reference.clone()),
            )
            .await;
        assert!(
            load_provider_secret(secrets.as_ref(), &reference)
                .unwrap()
                .is_some()
        );
        let snapshot = detached.lock().await.for_setup();
        detached
            .lock()
            .await
            .merge_authorization(&snapshot)
            .unwrap();
        let current = owner.shared.settings.load().sources.configured[0]
            .credential
            .clone()
            .unwrap();
        assert!(
            load_provider_secret(secrets.as_ref(), &current)
                .unwrap()
                .is_some()
        );
        owner
            .shared
            .settings
            .update(|stored| {
                stored.sources.configured.clear();
                Ok(())
            })
            .unwrap();
        owner
            .remove_source_resources(&configured[1].configuration.source_id, Some(current))
            .await;
        assert!(matches!(
            detached.lock().await.merge_authorization(&snapshot),
            Err(SourceError::Cancelled)
        ));
        assert!(owner.shared.settings.load().sources.configured.is_empty());
        assert!(owner.shared.plex_logins.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn acquisition_progress_belongs_to_its_source() {
        let directory = tempfile::tempdir().unwrap();
        let database = Arc::new(
            Database::open(directory.path().join("library.sqlite"))
                .await
                .unwrap(),
        );
        let source_id = SourceId::new("test");
        database
            .reconcile_source(&source_id, "local", "Test")
            .await
            .unwrap();
        let cached = database
            .cached_source(source_id.as_str(), &ReadCancellation::new())
            .await
            .unwrap()
            .unwrap();
        let runtime = tokio::runtime::Handle::current();
        let (events, receiver) = async_channel::unbounded();
        let owner = SourceOwner::open_dormant(
            Artwork::new(directory.path().join("artwork"), runtime.clone()).unwrap(),
            Arc::clone(&database),
            Downloads::new(
                artwork::Artwork::new(directory.path().join("downloads/artwork"), runtime.clone())
                    .unwrap(),
                directory.path().join("downloads"),
                database.as_ref().clone(),
                runtime.clone(),
                async_channel::unbounded().0,
                Vec::new(),
            ),
            SettingsFile::memory(),
            Arc::new(SwitchableSecretStore::new(Arc::new(
                secrets::MemorySecretStore::new(),
            ))),
            runtime,
            SourceOutputs {
                events,
                discovery: async_channel::unbounded().0,
            },
        );

        let music = directory.path().join("music");
        std::fs::create_dir(&music).unwrap();
        let configuration =
            SourceConfiguration::local(source_id.clone(), "Test", vec![music]).unwrap();
        let configured = ConfiguredSource {
            configuration: configuration.clone(),
            credential: None,
            music_folder_id: None,
            local_access: None,
            enable_half_stars: false,
        };
        owner
            .shared
            .settings
            .update(|stored| {
                stored.sources.configured.push(configured.clone());
                Ok(())
            })
            .unwrap();
        owner.cache_connected(
            &configured,
            Arc::new(Source::open(configuration, None, None).unwrap()),
        );
        let other_id = SourceId::new("other");
        database
            .reconcile_source(&other_id, "local", "Other")
            .await
            .unwrap();
        let mut other = configured.clone();
        other.configuration =
            SourceConfiguration::local(other_id.clone(), "Other", Vec::new()).unwrap();
        owner
            .shared
            .settings
            .update(|stored| {
                stored.sources.configured.push(other.clone());
                Ok(())
            })
            .unwrap();
        owner.cache_connected(
            &other,
            Arc::new(Source::open(other.configuration.clone(), None, None).unwrap()),
        );
        let first = owner.shared.begin_refresh(&source_id, false, true).unwrap();
        let second = owner.shared.begin_refresh(&other_id, false, true).unwrap();
        owner.shared.present_refresh(&source_id);
        owner.shared.begin_selection();
        assert!(owner.shared.refresh_is_current(&source_id, &first));
        assert!(owner.shared.refresh_is_current(&other_id, &second));
        owner.shared.refresh_progress(
            &other_id,
            &second,
            SourceReadProgress {
                stage: SourceReadStage::Tracks,
                completed: 128,
                total: None,
            },
        );
        assert!(
            matches!(&*owner.shared.operation.borrow(), SourceOperation::Refreshing {source_id: current,..} if current==&source_id)
        );
        owner
            .finish_source_refresh(&other_id, &second, Ok(()))
            .await;
        assert!(
            matches!(&*owner.shared.operation.borrow(), SourceOperation::Refreshing {source_id: current,..} if current==&source_id)
        );
        owner
            .shared
            .settings
            .update(|stored| {
                stored.sources.configured[0].configuration.name = "Renamed".into();
                Ok(())
            })
            .unwrap();
        owner.shared.retire_changed_clients(false).await.unwrap();
        assert!(owner.shared.refresh_is_current(&source_id, &first));
        owner
            .finish_source_refresh(&source_id, &first, Ok(()))
            .await;
        for error in [
            SourceError::Network("connection refused".into()),
            SourceError::Cancelled,
            SourceError::Auth("Login expired".into()),
            SourceError::Network("connection timed out".into()),
        ] {
            while receiver.try_recv().is_ok() {}
            owner.publish_operation(SourceOperation::Idle).await;
            let token = owner
                .shared
                .begin_refresh(&source_id, false, false)
                .unwrap();
            owner.shared.present_refresh(&source_id);
            owner.shared.refresh_progress(
                &source_id,
                &token,
                SourceReadProgress {
                    stage: SourceReadStage::Tracks,
                    completed: 128,
                    total: None,
                },
            );
            assert!(matches!(
                *owner.shared.operation.borrow(),
                SourceOperation::Idle
            ));
            owner
                .finish_source_refresh(&source_id, &token, Err(error))
                .await;
            assert!(matches!(
                *owner.shared.operation.borrow(),
                SourceOperation::Idle
            ));
            while let Ok(event) = receiver.try_recv() {
                assert!(!matches!(
                    event,
                    SourceEvent::Operation(SourceOperation::Failed { .. })
                ));
            }
        }
        owner
            .finish_refresh(
                &source_id,
                Err(SourceError::Network("connection refused".into())),
            )
            .await;
        assert!(matches!(&*owner.shared.operation.borrow(),
            SourceOperation::Failed { message, .. } if message == "Server is unreachable"));
        let token = owner.shared.begin_refresh(&source_id, false, true).unwrap();
        owner.shared.present_refresh(&source_id);
        owner
            .finish_source_refresh(
                &source_id,
                &token,
                Err(SourceError::Auth("Login expired".into())),
            )
            .await;
        assert!(matches!(
            *owner.shared.operation.borrow(),
            SourceOperation::Failed { .. }
        ));

        let selected_library = SelectedLibrary {
            source_id: source_id.clone(),
            source_key: cached.source,
            music_folder_key: None,
            music_folder_object_id: None,
            music_folders: Arc::from([]),
        };
        let client = owner.client(&source_id).unwrap();
        assert!(owner.start_observer(selected_library.clone(), client.clone(), false));
        let observer = owner.shared.clients.lock().unwrap()[&source_id]
            .observer
            .as_ref()
            .unwrap()
            .0
            .clone();
        *owner.shared.selected.lock().unwrap() = Some(selected_library.clone());
        owner.release_selected(false).await;
        assert!(!observer.cancellation().load(Ordering::Acquire));
        assert!(!owner.start_observer(selected_library.clone(), client, false));
        for (metadata, activity, artwork, selected, visible) in [
            (false, false, false, true, false),
            (false, false, true, true, false),
            (true, false, false, true, false),
            (true, false, false, false, false),
            (false, true, false, true, false),
            (false, true, false, false, false),
        ] {
            owner.publish_operation(SourceOperation::Idle).await;
            *owner.shared.selected.lock().unwrap() = selected.then(|| selected_library.clone());
            while receiver.try_recv().is_ok() {}
            let configuration = owner.configuration_changes();
            let catalog = owner.catalog_changes();
            owner
                .catalog_updated(
                    source_id.clone(),
                    cached.source,
                    CatalogUpdate {
                        metadata,
                        activity,
                        playlists: false,
                        artwork,
                        home: false,
                        track_rekeys: Arc::from([]),
                    },
                )
                .await;
            assert!(
                !configuration.has_changed().unwrap(),
                "catalog data does not change source configuration"
            );
            assert_eq!(
                catalog.has_changed().unwrap(),
                metadata || artwork || activity
            );
            let mut refreshing = false;
            let mut published = false;
            while let Ok(event) = receiver.try_recv() {
                refreshing |= matches!(
                    event,
                    SourceEvent::Operation(SourceOperation::Refreshing { .. })
                );
                published |= matches!(event, SourceEvent::CatalogPublished(_));
            }
            assert_eq!(refreshing, visible);
            assert_eq!(published, metadata || artwork || activity);
        }
        let mut changes = owner.catalog_changes();
        let before = changes.borrow().clone();
        for (metadata, activity) in [(true, false), (false, true)] {
            owner
                .catalog_updated(
                    source_id.clone(),
                    cached.source,
                    CatalogUpdate {
                        metadata,
                        activity,
                        ..Default::default()
                    },
                )
                .await;
        }
        let revision = changes.borrow_and_update().clone();
        assert_eq!(revision.catalog, before.catalog + 1);
        assert_eq!(revision.activity, before.activity + 1);
        assert_eq!(revision.change, Some(CatalogChange::Broad));
        while receiver.try_recv().is_ok() {}
        for (artwork, home, expected) in [
            (false, true, CatalogChange::Broad),
            (true, false, CatalogChange::Broad),
            (false, false, CatalogChange::Playlists(None)),
        ] {
            owner
                .catalog_updated(
                    source_id.clone(),
                    cached.source,
                    CatalogUpdate {
                        metadata: false,
                        activity: false,
                        playlists: true,
                        artwork,
                        home,
                        track_rekeys: Arc::from([]),
                    },
                )
                .await;
            let SourceEvent::CatalogPublished(publication) = receiver.recv().await.unwrap() else {
                panic!("scan changes publish catalog content");
            };
            assert_eq!(publication.change, expected);
        }
        let mut catalog = owner.catalog_changes();
        let before = catalog.borrow().clone();
        let listen: library::ListenWrite = serde_json::from_value(serde_json::json!({
            "external_id":"direct", "media_uri":"https://audio.invalid/direct.flac",
            "title":"Direct", "artist":"", "album":"", "duration_millis":1000,
            "started_at":1_700_000_000,"local_period":"2023-11","listened_millis":1000,"skipped":false
        })).unwrap();
        owner
            .shared
            .database
            .record_listen(&listen, None, &[])
            .await
            .unwrap();
        let SourceEvent::CatalogPublished(publication) = receiver.recv().await.unwrap() else {
            panic!("accepted direct listen publishes activity");
        };
        assert_eq!(publication.source_key, None);
        assert_eq!(publication.change, CatalogChange::Activity);
        let revision = catalog.borrow_and_update().clone();
        assert_eq!(revision.catalog, before.catalog);
        assert_eq!(revision.activity, before.activity + 1);
        owner
            .shared
            .database
            .record_listen(&listen, None, &[])
            .await
            .unwrap();
        assert!(!catalog.has_changed().unwrap());
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn adding_a_source_connects_before_its_background_scan() {
        for already_configured in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let music = directory.path().join("music");
            std::fs::create_dir(&music).unwrap();
            let database = Arc::new(
                Database::open(directory.path().join("library.sqlite"))
                    .await
                    .unwrap(),
            );
            let runtime = tokio::runtime::Handle::current();
            let (events, receiver) = async_channel::unbounded();
            let owner = SourceOwner::open_dormant(
                Artwork::new(directory.path().join("artwork"), runtime.clone()).unwrap(),
                Arc::clone(&database),
                Downloads::new(
                    artwork::Artwork::new(
                        directory.path().join("downloads/artwork"),
                        runtime.clone(),
                    )
                    .unwrap(),
                    directory.path().join("downloads"),
                    database.as_ref().clone(),
                    runtime.clone(),
                    async_channel::unbounded().0,
                    Vec::new(),
                ),
                SettingsFile::memory(),
                Arc::new(SwitchableSecretStore::new(Arc::new(
                    secrets::MemorySecretStore::new(),
                ))),
                runtime,
                SourceOutputs {
                    events,
                    discovery: async_channel::unbounded().0,
                },
            );
            let mut connected = if already_configured {
                let source_id = SourceId::new("restored-source");
                owner
                    .shared
                    .settings
                    .update(|stored| {
                        stored.sources.configured.push(ConfiguredSource {
                            configuration: SourceConfiguration::local(
                                source_id.clone(),
                                "Restored",
                                vec![music],
                            )
                            .unwrap(),
                            credential: None,
                            music_folder_id: None,
                            local_access: None,
                            enable_half_stars: false,
                        });
                        Ok(())
                    })
                    .unwrap();
                assert!(
                    database
                        .cached_source(source_id.as_str(), &ReadCancellation::new())
                        .await
                        .unwrap()
                        .is_none()
                );
                assert_eq!(
                    owner.select_source(source_id.clone()).await.unwrap(),
                    source_id
                );
                None
            } else {
                Some(owner.configure_source(SourceSetup::Local { roots: vec![music] }))
            };
            tokio::time::timeout(Duration::from_secs(10), async {
                let mut scanned = false;
                let mut selected = None;
                let mut refreshing = false;
                loop {
                    match receiver.recv().await.unwrap() {
                        SourceEvent::Operation(SourceOperation::Adding { progress }) => {
                            assert_eq!(progress.stage, SourceProgressStage::Connecting);
                            assert!(selected.is_none());
                        }
                        SourceEvent::Selected {
                            selected: library, ..
                        } => {
                            assert!(!refreshing, "selection must precede the initial scan");
                            selected = Some(library.source_id);
                        }
                        SourceEvent::Operation(SourceOperation::Idle) => {
                            assert!(selected.is_some());
                            if refreshing {
                                assert!(scanned, "the empty scan reports its track count");
                                break;
                            }
                        }
                        SourceEvent::Operation(SourceOperation::Refreshing {
                            source_id,
                            progress,
                        }) => {
                            assert_eq!(selected.as_ref(), Some(&source_id));
                            if let Some(connected) = connected.take() {
                                let library = connected.try_recv().unwrap().unwrap();
                                assert_eq!(library.source_id, source_id);
                                assert_eq!(
                                    owner.list_sources().selected_source_id,
                                    Some(source_id)
                                );
                            }
                            refreshing = true;
                            scanned |= progress.stage == SourceProgressStage::Tracks
                                && progress.completed == 0;
                        }
                        SourceEvent::Operation(SourceOperation::Failed { message, .. }) => {
                            panic!("{message}")
                        }
                        _ => {}
                    }
                }
            })
            .await
            .unwrap_or_else(|_| {
                panic!("initial scan did not finish, already_configured={already_configured}")
            });
            assert!(matches!(
                *owner.shared.operation.borrow(),
                SourceOperation::Idle
            ));
            let source_id = owner.selected_library().unwrap().source_id;
            assert!(
                database
                    .cached_source(source_id.as_str(), &ReadCancellation::new())
                    .await
                    .unwrap()
                    .unwrap()
                    .catalog_complete
            );
            owner.shared.clients.lock().unwrap().clear();
            owner.shared.cancel_selection();
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cached_selection_is_published_while_credentials_are_held() {
        struct HeldSecrets {
            started: Sender<()>,
            release: Mutex<std::sync::mpsc::Receiver<()>>,
        }
        impl SecretStore for HeldSecrets {
            fn load_secret(&self, _: &secrets::SecretKey) -> secrets::SecretResult<Option<String>> {
                let _ = self.started.try_send(());
                let _ = self.release.lock().unwrap().recv();
                Ok(Some("salt:token".into()))
            }
            fn save_secret(&self, _: &secrets::SecretKey, _: &str) -> secrets::SecretResult<()> {
                Ok(())
            }
            fn delete_secret(&self, _: &secrets::SecretKey) -> secrets::SecretResult<()> {
                Ok(())
            }
        }
        for switch_while_held in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let database = Arc::new(
                Database::open(directory.path().join("library.sqlite"))
                    .await
                    .unwrap(),
            );
            let source_id = SourceId::new("held-credential-source");
            database
                .reconcile_source(&source_id, "subsonic", "Music")
                .await
                .unwrap();
            let source_key = database
                .source_identity_key(&source_id)
                .await
                .unwrap()
                .unwrap();
            let update = {
                let mut catalog = database.catalog(source_id.as_str(), None).await.unwrap();
                catalog
                    .apply_catalog(
                        &library::CatalogBatch {
                            folders: vec![library::FolderObservation {
                                object_id: "new-folder".into(),
                                metadata: library::FolderMetadata {
                                    name: Some("New Folder".into()),
                                    ..Default::default()
                                },
                                ..Default::default()
                            }],
                            albums: vec![library::AlbumObservation {
                                object_id: "album".into(),
                                metadata: library::AlbumMetadata {
                                    title: "Cached Album".into(),
                                    artist: "Artist".into(),
                                    sort_text: "cached album".into(),
                                    ..Default::default()
                                },
                                user_data: library::NativeUserState {
                                    favorite: Some(false),
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
                catalog.into_changes()
            };
            database
                .publish_catalog_update(source_id.as_str(), update)
                .await;
            let settings = SettingsFile::memory();
            settings.update(|stored| {
            stored.sources.configured.push(ConfiguredSource {
                configuration: SourceConfiguration {
                    source_id: source_id.clone(),
                    kind: "subsonic".into(),
                    name: "Music".into(),
                    provider_payload: serde_json::json!({ "version": 1, "base_url": "http://127.0.0.1:9", "username": "listener", "trust_invalid_cert": false }).to_string(),
                },
                credential: Some(crate::settings::Credential::new("held")),
                music_folder_id: None,
                local_access: None,
                enable_half_stars: false,
            });
            Ok(())
        }).unwrap();
            let runtime = tokio::runtime::Handle::current();
            let (started, started_receiver) = async_channel::bounded(1);
            let (release, release_receiver) = std::sync::mpsc::channel();
            let secrets = Arc::new(SwitchableSecretStore::new(Arc::new(HeldSecrets {
                started,
                release: Mutex::new(release_receiver),
            })));
            let (events, receiver) = async_channel::unbounded();
            let owner = SourceOwner::open_dormant(
                Artwork::new(directory.path().join("artwork"), runtime.clone()).unwrap(),
                Arc::clone(&database),
                Downloads::new(
                    artwork::Artwork::new(
                        directory.path().join("downloads/artwork"),
                        runtime.clone(),
                    )
                    .unwrap(),
                    directory.path().join("downloads"),
                    database.as_ref().clone(),
                    runtime.clone(),
                    async_channel::unbounded().0,
                    Vec::new(),
                ),
                settings,
                secrets,
                runtime,
                SourceOutputs {
                    events,
                    discovery: async_channel::unbounded().0,
                },
            );
            owner.shared.load_source_counts().await.unwrap();
            assert_eq!(
                owner.shared.configured_sources(None).local_access[0].album_count,
                1
            );
            let completion = owner.select_source(source_id.clone());
            let selected = tokio::time::timeout(Duration::from_secs(2), completion)
                .await
                .expect("selection result must not wait for credentials")
                .unwrap();
            assert_eq!(selected, source_id);
            drop(selected);
            let published = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if let SourceEvent::Selected {
                        configured,
                        selected,
                    } = receiver.recv().await.unwrap()
                    {
                        assert_eq!(configured.local_access[0].album_count, 1);
                        break selected;
                    }
                }
            })
            .await
            .expect("cached selection must not wait for credentials");
            assert_eq!(published.source_id, source_id);
            assert_eq!(published.source_key, source_key);
            assert_eq!(published.music_folders[0].name, "New Folder");
            tokio::time::timeout(
                Duration::from_secs(2),
                owner
                    .set_music_folder(source_id.clone(), Some("new-folder".into()))
                    .recv(),
            )
            .await
            .expect("an empty cached folder can be selected while credentials are held")
            .unwrap()
            .unwrap();
            assert_eq!(
                owner.selected_library().unwrap().music_folder_key,
                Some(published.music_folders[0].folder_key)
            );
            tokio::time::timeout(Duration::from_secs(2), started_receiver.recv())
                .await
                .unwrap()
                .unwrap();
            assert!(owner.cached_client(&source_id).is_none());
            let connection = owner
                .shared
                .connection
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .clone();
            let favorite =
                FavoriteTarget::Album(library::source_entity_uri(&source_id, "album", "album"));
            for value in [true, false] {
                assert_eq!(
                    tokio::time::timeout(
                        Duration::from_secs(2),
                        owner.set_favorite_with_result(favorite.clone(), value),
                    )
                    .await
                    .expect("favorite completion must not wait for live access")
                    .unwrap(),
                    value
                );
                assert_eq!(database.favorite(&favorite).await.unwrap(), value);
            }
            while receiver.try_recv().is_ok() {}
            let local_uri = url::Url::from_file_path(directory.path().join("local.flac"))
                .unwrap()
                .to_string();
            let local_stream = tokio::time::timeout(
                Duration::from_secs(2),
                crate::playback::prepare_stream(
                    &database,
                    playback::StreamRequest::new(&local_uri, playback::StreamQuality::Original),
                    None,
                    None,
                    |source_id| owner.open_client(source_id),
                ),
            )
            .await
            .expect("local streams do not wait for credentials")
            .unwrap();
            assert_eq!(local_stream.uri(), local_uri);
            owner
                .shared
                .settings
                .update(|stored| {
                    stored.ui.application.external_metadata_enabled = false;
                    Ok(())
                })
                .unwrap();
            let update = {
                let mut catalog = database.catalog(source_id.as_str(), None).await.unwrap();
                catalog
                    .apply_catalog(
                        &library::CatalogBatch {
                            folders: vec![library::FolderObservation {
                                object_id: "new-folder".into(),
                                metadata: library::FolderMetadata {
                                    name: Some("New Folder".into()),
                                    sort_text: Some("new folder".into()),
                                },
                                ..Default::default()
                            }],
                            tracks: vec![library::TrackObservation {
                                object_id: "new-track".into(),
                                album_object_id: Some("album".into()),
                                metadata: library::TrackMetadata {
                                    title: "New Track".into(),
                                    album: "Cached Album".into(),
                                    artist: "Artist".into(),
                                    sort_text: "new track".into(),
                                    duration_millis: 1_000,
                                    disc_number: 1,
                                    track_number: 1,
                                    ..Default::default()
                                },
                                user_data: library::NativeUserState {
                                    favorite: Some(false),
                                    ..Default::default()
                                },
                                folders: Some(vec!["new-folder".into()]),
                                ..Default::default()
                            }],
                            ..Default::default()
                        },
                        true,
                    )
                    .await
                    .unwrap();
                catalog.into_changes()
            };
            database
                .publish_catalog_update(source_id.as_str(), update)
                .await;
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if let SourceEvent::CatalogPublished(CatalogPublication {
                        change: CatalogChange::Broad,
                        ..
                    }) = receiver.recv().await.unwrap()
                    {
                        assert_eq!(
                            owner.selected_library().unwrap().music_folders[0].object_id,
                            "new-folder"
                        );
                        break;
                    }
                }
            })
            .await
            .expect("the UI receives the updated folder snapshot");
            assert!(
                owner.cached_client(&source_id).is_none(),
                "summary refresh does not obtain credentials"
            );
            assert_eq!(
                owner.selected_library().unwrap().music_folders[0].object_id,
                "new-folder"
            );
            if switch_while_held {
                drop(published);
                let local_id = SourceId::new("local-replacement");
                owner
                    .shared
                    .settings
                    .update(|stored| {
                        stored.sources.configured.push(ConfiguredSource {
                            configuration: SourceConfiguration::local(
                                local_id.clone(),
                                "Local",
                                Vec::new(),
                            )
                            .unwrap(),
                            credential: None,
                            music_folder_id: None,
                            local_access: None,
                            enable_half_stars: false,
                        });
                        Ok(())
                    })
                    .unwrap();
                database
                    .reconcile_source(&local_id, "local", "Local")
                    .await
                    .unwrap();
                let selecting = owner.clone();
                let selected_id = local_id.clone();
                let selection =
                    tokio::spawn(async move { selecting.select_source(selected_id).await });
                let replacement = tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        match receiver.recv().await.unwrap() {
                            SourceEvent::Selected { selected, .. } => break selected,
                            _ => {}
                        }
                    }
                })
                .await
                .expect("another cached source can be selected during unlock");
                assert_eq!(replacement.source_id, local_id);
                assert_eq!(selection.await.unwrap().unwrap(), local_id);
                let configured = owner.shared.configured_sources(None);
                assert_eq!(
                    (
                        configured.local_access[0].album_count,
                        configured.local_access[0].track_count
                    ),
                    (1, 1)
                );
                assert_eq!(configured.local_access[1].track_count, 0);
                let update = {
                    let mut catalog = database.catalog(source_id.as_str(), None).await.unwrap();
                    catalog
                        .remove_source_items("track", &["new-track".to_string()])
                        .await
                        .unwrap();
                    catalog.into_changes()
                };
                database
                    .publish_catalog_update(source_id.as_str(), update)
                    .await;
                tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        if matches!(receiver.recv().await.unwrap(),
                            SourceEvent::CatalogPublished(CatalogPublication { source_key: Some(key), .. })
                            if key == source_key)
                        {
                            break;
                        }
                    }
                })
                .await
                .expect("the unselected source deletion reaches the catalog consumer");
                assert_eq!(
                    owner.shared.configured_sources(None).local_access[0].track_count,
                    0
                );
                release.send(()).unwrap();
                tokio::time::timeout(Duration::from_secs(2), async {
                    while owner.cached_client(&source_id).is_none() || !connection.is_finished() {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("replaced connection stops after the held credential read finishes");
                assert_eq!(owner.selected_library().unwrap().source_id, local_id);
                assert!(owner.selected_library().unwrap().music_folders.is_empty());
                continue;
            }
            release.send(()).unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while owner.cached_client(&source_id).is_none() || !connection.is_finished() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("live access resumes after credential initialization");
            assert_eq!(owner.selected_library().unwrap().source_id, source_id);
            assert_eq!(
                owner.selected_library().unwrap().music_folders[0].object_id,
                "new-folder"
            );
            assert_eq!(
                database
                    .cached_source(source_id.as_str(), &ReadCancellation::new())
                    .await
                    .unwrap()
                    .unwrap()
                    .source,
                source_key
            );
            assert_eq!(
                (
                    owner.shared.configured_sources(None).local_access[0].album_count,
                    owner.shared.configured_sources(None).local_access[0].track_count,
                ),
                (1, 1)
            );
            owner.deliver_pending_favorites().await;
            assert!(!database.favorite(&favorite).await.unwrap());
            assert!(
                database
                    .due_remote_favorites(unix_seconds(), 64, &ReadCancellation::new())
                    .await
                    .unwrap()
                    .is_empty(),
                "offline delivery is deferred without losing the local edit"
            );
            assert_eq!(
                database
                    .due_remote_favorites(unix_seconds() + 60, 64, &ReadCancellation::new())
                    .await
                    .unwrap(),
                vec![(favorite.media_uri().to_owned(), false)]
            );
        }
    }

    #[tokio::test]
    async fn source_switch_and_removal_complete_without_an_event_consumer() {
        for keep_receiver in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let database = Arc::new(
                Database::open(directory.path().join("library.sqlite"))
                    .await
                    .unwrap(),
            );
            let settings_path = directory.path().join("settings.json");
            let settings = SettingsFile::open(settings_path.clone()).unwrap();
            let first = SourceId::new("first");
            let second = SourceId::new("second");
            for id in [&first, &second] {
                settings
                    .update(|stored| {
                        stored.sources.configured.push(ConfiguredSource {
                            configuration: SourceConfiguration::local(
                                id.clone(),
                                id.as_str(),
                                Vec::new(),
                            )
                            .unwrap(),
                            credential: None,
                            music_folder_id: None,
                            local_access: None,
                            enable_half_stars: false,
                        });
                        Ok(())
                    })
                    .unwrap();
                database
                    .reconcile_source(&id, "local", id.as_str())
                    .await
                    .unwrap();
            }
            let runtime = tokio::runtime::Handle::current();
            let (events, receiver) = async_channel::unbounded();
            let receiver = keep_receiver.then_some(receiver);
            let owner = SourceOwner::open_dormant(
                Artwork::new(directory.path().join("artwork"), runtime.clone()).unwrap(),
                Arc::clone(&database),
                Downloads::new(
                    artwork::Artwork::new(
                        directory.path().join("downloads/artwork"),
                        runtime.clone(),
                    )
                    .unwrap(),
                    directory.path().join("downloads"),
                    database.as_ref().clone(),
                    runtime.clone(),
                    async_channel::unbounded().0,
                    Vec::new(),
                ),
                settings.clone(),
                Arc::new(SwitchableSecretStore::new(Arc::new(
                    secrets::MemorySecretStore::new(),
                ))),
                runtime,
                SourceOutputs {
                    events,
                    discovery: async_channel::unbounded().0,
                },
            );
            assert_eq!(owner.list_sources().sources.len(), 2);
            assert!(owner.selected_library().is_none());
            let missing = owner.select_source(SourceId::new("missing"));
            let error = tokio::time::timeout(Duration::from_secs(5), missing)
                .await
                .unwrap()
                .err()
                .expect("unknown source returns an error to its caller");
            assert!(!error.to_string().is_empty());
            assert!(owner.selected_library().is_none());

            // A pending playlist edit must not hold up source selection.
            let lane = owner.shared.lane.lock().await;
            let mut superseded = Box::pin(owner.select_source(second.clone()));
            let mut initial = Box::pin(owner.select_source(first.clone()));
            assert!(futures_util::poll!(superseded.as_mut()).is_pending());
            assert!(futures_util::poll!(initial.as_mut()).is_pending());
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(5), superseded)
                    .await
                    .unwrap()
                    .err()
                    .map(|error| error.to_string()),
                Some(SourceError::Other(SourceError::Cancelled.to_string()).to_string()),
            );
            let mut initial = Some(initial);
            for id in [&first, &second] {
                let selection = if id == &first {
                    initial.take().unwrap()
                } else {
                    Box::pin(owner.select_source(id.clone()))
                };
                let selected = tokio::time::timeout(Duration::from_secs(5), selection)
                    .await
                    .expect("selection completes without consuming events")
                    .unwrap();
                assert_eq!(&selected, id);
                assert_eq!(owner.list_sources().selected_source_id.as_ref(), Some(id));
                assert_eq!(owner.selected_library().unwrap().source_id, *id);
            }
            drop(lane);
            tokio::time::timeout(Duration::from_secs(5), owner.forget_source(second.clone()))
                .await
                .expect("removal completes without consuming events")
                .unwrap();
            assert_eq!(owner.list_sources().sources.len(), 1);
            assert!(owner.shared.selected().is_none());
            assert!(
                database
                    .source_identity_key(&second)
                    .await
                    .unwrap()
                    .is_none()
            );
            if let Some(receiver) = receiver {
                let mut assignments = Vec::new();
                while let Ok(event) = receiver.try_recv() {
                    match event {
                        SourceEvent::Selected { selected, .. } => {
                            assignments.push(Some(selected.source_id))
                        }
                        SourceEvent::ReleaseSelected => assignments.push(None),
                        _ => {}
                    }
                }
                assert_eq!(
                    assignments,
                    vec![Some(first.clone()), None, Some(second.clone()), None]
                );
            }
            owner.shared.clients.lock().unwrap().clear();
            owner.shared.cancel_selection();

            owner.forget_source(second.clone()).await.unwrap();
            assert_eq!(
                owner.list_sources().sources.len(),
                1,
                "removal can be repeated"
            );
            std::fs::remove_file(&settings_path).unwrap();
            std::fs::create_dir(&settings_path).unwrap();
            let error =
                tokio::time::timeout(Duration::from_secs(5), owner.forget_source(first.clone()))
                    .await
                    .unwrap()
                    .unwrap_err();
            assert!(
                !error.to_string().is_empty(),
                "failed settings writes reach the caller"
            );
            assert_eq!(owner.list_sources().sources[0].id, first);
        }
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct SourceSubscription {
    source: SourceOwner,
    changes: tokio::sync::Mutex<tokio::sync::watch::Receiver<()>>,
}
#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct SourceOperationSubscription {
    changes: tokio::sync::Mutex<tokio::sync::watch::Receiver<SourceOperation>>,
}
#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct SourceDiscoverySubscription {
    changes: tokio::sync::Mutex<tokio::sync::watch::Receiver<Option<DiscoveryUpdate>>>,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl SourceOwner {
    pub fn subscribe(&self) -> Arc<SourceSubscription> {
        let mut changes = self.configuration_changes();
        changes.mark_changed();
        Arc::new(SourceSubscription {
            source: self.clone(),
            changes: tokio::sync::Mutex::new(changes),
        })
    }
    pub fn subscribe_operation(&self) -> Arc<SourceOperationSubscription> {
        let mut changes = self.operation();
        changes.mark_changed();
        Arc::new(SourceOperationSubscription {
            changes: tokio::sync::Mutex::new(changes),
        })
    }
    pub fn subscribe_discovery(&self) -> Arc<SourceDiscoverySubscription> {
        let mut changes = self.shared.discovery.subscribe();
        changes.mark_changed();
        Arc::new(SourceDiscoverySubscription {
            changes: tokio::sync::Mutex::new(changes),
        })
    }
    pub fn authorize(
        self: Arc<Self>,
        method: authorization::SourceAuthorizationMethod,
    ) -> Arc<authorization::SourceAuthorization> {
        authorization::SourceAuthorization::new(self, method)
    }
    pub async fn saved_plex_logins(
        self: Arc<Self>,
    ) -> sources::SourceResult<Arc<authorization::SourceAuthorization>> {
        authorization::SourceAuthorization::saved_plex(self)
            .await
            .map_err(|error| SourceError::Other(error.to_string()))
    }
    pub async fn save_jellyfin_emby(
        &self,
        source_id: Option<SourceId>,
        kind: sources::ServerKind,
        credentials: sources::CredentialInput,
        use_instant_mix: bool,
        connect_manually: bool,
    ) -> sources::SourceResult<()> {
        if let Some(source_id) = source_id {
            self.update_source(SourceSettingsChange::JellyfinEmby {
                source_id,
                credentials,
                use_instant_mix,
                connect_manually,
            })
            .recv()
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?
            .map_err(SourceError::Other)
        } else {
            self.configure_source(SourceSetup::JellyfinEmby {
                kind,
                credentials,
                use_instant_mix,
            })
            .recv()
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?
            .map(|_| ())
            .map_err(SourceError::Other)
        }
    }
    pub async fn save_subsonic(
        &self,
        source_id: Option<SourceId>,
        kind: sources::SubsonicFlavor,
        authentication: sources::SubsonicAuthentication,
        credentials: sources::CredentialInput,
    ) -> sources::SourceResult<()> {
        if let Some(source_id) = source_id {
            self.update_source(SourceSettingsChange::OpenSubsonic {
                source_id,
                kind,
                authentication,
                credentials,
            })
            .recv()
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?
            .map_err(SourceError::Other)
        } else {
            self.configure_source(SourceSetup::OpenSubsonic {
                kind,
                authentication,
                credentials,
            })
            .recv()
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?
            .map(|_| ())
            .map_err(SourceError::Other)
        }
    }
    pub async fn save_plex(
        &self,
        source_id: SourceId,
        settings: sources::PlexSettingsInput,
    ) -> sources::SourceResult<()> {
        self.update_source(SourceSettingsChange::Plex {
            source_id,
            settings,
        })
        .recv()
        .await
        .map_err(|error| SourceError::Other(error.to_string()))?
        .map_err(SourceError::Other)
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl SourceSubscription {
    pub async fn next(&self) -> sources::SourceResult<ConfiguredSources> {
        self.changes
            .lock()
            .await
            .changed()
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?;
        Ok(self.source.list_sources())
    }
}
#[cfg_attr(feature = "bindings", uniffi::export)]
impl SourceOperationSubscription {
    pub async fn next(&self) -> sources::SourceResult<SourceOperation> {
        let mut changes = self.changes.lock().await;
        changes
            .changed()
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?;
        Ok(changes.borrow_and_update().clone())
    }
}
#[cfg_attr(feature = "bindings", uniffi::export)]
impl SourceDiscoverySubscription {
    pub async fn next(&self) -> sources::SourceResult<DiscoveryUpdate> {
        let mut changes = self.changes.lock().await;
        loop {
            changes
                .changed()
                .await
                .map_err(|error| SourceError::Other(error.to_string()))?;
            if let Some(update) = changes.borrow_and_update().clone() {
                return Ok(update);
            }
        }
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl SourceOwner {
    pub fn editable(&self, source_id: SourceId) -> sources::SourceResult<Option<EditableSource>> {
        let stored = self.shared.settings.load();
        stored
            .sources
            .configured
            .iter()
            .map(|source| (source, false))
            .chain(
                stored
                    .sources
                    .integrations
                    .iter()
                    .map(|source| (source, true)),
            )
            .find(|(source, _)| source.configuration.source_id == source_id)
            .map(|(source, integration)| {
                let mut form =
                    editable_source(&source.configuration).map_err(SourceError::Other)?;
                form.integration = integration;
                Ok(form)
            })
            .transpose()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct SourceCatalogSubscription {
    changes: tokio::sync::Mutex<tokio::sync::watch::Receiver<CatalogRevision>>,
}
#[cfg_attr(feature = "bindings", uniffi::export)]
impl SourceOwner {
    pub fn subscribe_catalog(&self) -> Arc<SourceCatalogSubscription> {
        let mut changes = self.catalog_changes();
        changes.mark_changed();
        Arc::new(SourceCatalogSubscription {
            changes: tokio::sync::Mutex::new(changes),
        })
    }
}
#[cfg_attr(feature = "bindings", uniffi::export)]
impl SourceCatalogSubscription {
    pub async fn next(&self) -> sources::SourceResult<CatalogRevision> {
        let mut changes = self.changes.lock().await;
        changes
            .changed()
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?;
        Ok(changes.borrow_and_update().clone())
    }
}
