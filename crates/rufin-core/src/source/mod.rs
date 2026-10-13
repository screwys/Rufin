//! Source configuration, browsing selection, and source work.
pub mod authorization;
mod availability;
mod download;
pub mod folders;
mod integrations;
mod pins;
pub mod search;
pub use integrations::FileIntegration;

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::runtime::source::{
    ConfiguredSources, DiscoveryStatus, DiscoveryUpdate, EditableSource, LocalAccessStatus,
    LocalFolder, SourceLocalAccess, SourceLocalAccessSummary, SourceOperation, SourceProgress,
    SourceProgressStage, SourceSummary,
};
use crate::runtime::{
    CatalogChange, CatalogPublication, FavoriteSettlement, SelectedLibrary, SourceEvent,
    SourceNotice, SourceNoticeKind,
};
use artwork::Artwork;
use async_channel::{Receiver, Sender};
use downloads::Downloads;
use library::{
    CatalogUpdate, Database, FavoriteTarget, FolderKey, FolderRow, ReadCancellation, SourceKey,
};
use secrets::{SecretStorageMode, SwitchableSecretStore};
use sources::{
    CredentialPreset, LiveFolderPage, SelectedFeed, Source, SourceConfiguration, SourceEntityKind,
    SourceError, SourceId, SourceReadProgress, SourceReadStage, SourceSettingsChange, SourceSetup,
};
use tracing::{info, warn};

use crate::playback::PlaybackOwner;
use crate::settings::{
    ConfiguredSource, SavedLocalAccess, SettingsFile, StoredSettings, delete_provider_secret,
    fresh_credential_ref, fresh_source_id, load_provider_secret, platform_secret_store,
    save_provider_secret,
};

const SOURCE_CHECK_INTERVAL: Duration = Duration::from_secs(5 * 60);

#[derive(Clone)]
pub(crate) struct SourceOutputs {
    pub(crate) events: Sender<SourceEvent>,
    pub(crate) discovery: Sender<DiscoveryUpdate>,
}

pub(crate) struct SourceBootstrap {
    pub(crate) owner: Arc<SourceOwner>,
    pub(crate) configured: ConfiguredSources,
    pub(crate) operation: SourceOperation,
}

#[derive(Clone)]
pub struct SourceOwner {
    pub(crate) shared: Arc<Shared>,
}

struct CachedSource {
    configuration: SourceConfiguration,
    credential_ref: Option<crate::settings::CredentialRef>,
    jellyfin_device_id: String,
    source: Arc<Source>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ArtworkPreparationKey {
    source: SourceKey,
    digest: [u8; 32],
}

struct ActiveArtworkPreparation {
    key: ArtworkPreparationKey,
    token: u64,
    cancelled: tokio_util::sync::CancellationToken,
    abort: Option<tokio::task::AbortHandle>,
}

#[derive(Default)]
struct ArtworkPreparationOwner {
    active: Option<ActiveArtworkPreparation>,
    completed: Option<ArtworkPreparationKey>,
    next_token: u64,
}

impl ArtworkPreparationOwner {
    fn admit(
        &mut self,
        key: ArtworkPreparationKey,
    ) -> Option<(u64, tokio_util::sync::CancellationToken)> {
        if self.completed == Some(key)
            || self.active.as_ref().is_some_and(|active| active.key == key)
        {
            return None;
        }
        self.cancel_active();
        self.next_token = self.next_token.wrapping_add(1).max(1);
        let cancelled = tokio_util::sync::CancellationToken::new();
        self.active = Some(ActiveArtworkPreparation {
            key,
            token: self.next_token,
            cancelled: cancelled.clone(),
            abort: None,
        });
        Some((self.next_token, cancelled))
    }

    fn attach_abort(&mut self, token: u64, abort: tokio::task::AbortHandle) {
        if let Some(active) = self.active.as_mut().filter(|active| active.token == token) {
            active.abort = Some(abort);
        } else {
            abort.abort();
        }
    }

    fn is_current(&self, key: ArtworkPreparationKey, token: u64) -> bool {
        self.active.as_ref().is_some_and(|active| {
            active.key == key && active.token == token && !active.cancelled.is_cancelled()
        })
    }

    fn complete(&mut self, key: ArtworkPreparationKey, token: u64) -> bool {
        if !self.is_current(key, token) {
            return false;
        }
        self.active.take();
        self.completed = Some(key);
        true
    }

    fn fail(&mut self, key: ArtworkPreparationKey, token: u64) -> bool {
        if !self.is_current(key, token) {
            return false;
        }
        self.active.take();
        true
    }

    fn cancel_active(&mut self) {
        if let Some(active) = self.active.take() {
            active.cancelled.cancel();
            if let Some(abort) = active.abort {
                abort.abort();
            }
        }
    }
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
    catalog_changed: tokio::sync::watch::Sender<()>,
    configuration_changed: tokio::sync::watch::Sender<()>,
    selected: Mutex<Option<SelectedLibrary>>,
    catalog_counts: Mutex<HashMap<SourceId, (usize, usize)>>,
    observer: Mutex<Option<Arc<SelectedFeed>>>,
    acquisition: Mutex<Weak<AtomicBool>>,
    connection: Mutex<Option<tokio::task::AbortHandle>>,
    album_release: Mutex<Option<tokio::task::AbortHandle>>,
    artwork_preparation: Mutex<ArtworkPreparationOwner>,
    pub(crate) lane: tokio::sync::Mutex<()>,
    playback: Mutex<Weak<PlaybackOwner>>,
    connect: Mutex<Weak<crate::connect::ConnectOwner>>,
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
        if matches!(&event, SourceEvent::CatalogPublished(publication) if publication.change != CatalogChange::Home)
        {
            self.catalog_changed.send_replace(());
        }
        let _ = self.outputs.events.send(event).await;
    }

    fn publish_operation(&self, operation: SourceOperation) {
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

    fn cancel_observer(&self) {
        if let Some(observer) = self
            .observer
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            observer.cancel();
        }
    }

    fn begin_acquisition(&self) -> Arc<AtomicBool> {
        self.cancel_acquisition();
        let cancelled = Arc::new(AtomicBool::new(false));
        *self
            .acquisition
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::downgrade(&cancelled);
        cancelled
    }

    fn try_begin_acquisition(&self) -> Option<Arc<AtomicBool>> {
        let mut active = self
            .acquisition
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if active
            .upgrade()
            .is_some_and(|cancelled| !cancelled.load(Ordering::Acquire))
        {
            return None;
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        *active = Arc::downgrade(&cancelled);
        Some(cancelled)
    }

    fn acquisition_is_current(&self, cancelled: &Arc<AtomicBool>) -> bool {
        !cancelled.load(Ordering::Acquire)
            && self
                .acquisition
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .upgrade()
                .is_some_and(|active| Arc::ptr_eq(&active, cancelled))
    }

    fn cancel_acquisition(&self) {
        if let Some(cancelled) = self
            .acquisition
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .upgrade()
        {
            cancelled.store(true, Ordering::Release);
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
    ) -> SourceBootstrap {
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
            catalog_changed: tokio::sync::watch::channel(()).0,
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
            operation: tokio::sync::watch::channel(operation.clone()).0,
            selected: Mutex::new(None),
            catalog_counts: Mutex::new(HashMap::new()),
            observer: Mutex::new(None),
            acquisition: Mutex::new(Weak::new()),
            connection: Mutex::new(None),
            album_release: Mutex::new(None),
            artwork_preparation: Mutex::new(ArtworkPreparationOwner::default()),
            lane: tokio::sync::Mutex::new(()),
            playback: Mutex::new(Weak::new()),
            connect: Mutex::new(Weak::new()),
            started: AtomicBool::new(false),
        });
        let weak = Arc::downgrade(&shared);
        shared.database.observe_catalog(move |update| {
            let shared = weak.upgrade();
            async move {
                if let Some(shared) = shared {
                    SourceOwner { shared }.catalog_updated(update).await;
                }
            }
        });
        SourceBootstrap {
            configured: shared.configured_sources(None),
            operation,
            owner: Arc::new(Self { shared }),
        }
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
        self.spawn_serialized(|owner| async move {
            let stored = owner.shared.settings.load();
            if let Err(error) = owner.shared.load_source_counts().await {
                owner.shared.warn_nonfatal(&error);
            }
            let configured = owner.shared.configured_sources(None);
            owner.shared.send(SourceEvent::Configured(configured)).await;
            if let Some(source_id) = stored.sources.selected_source_id {
                owner.select_source(source_id);
            }
        });
        let weak = Arc::downgrade(&self.shared);
        self.shared.runtime.spawn(async move {
            let mut interval = tokio::time::interval(SOURCE_CHECK_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let Some(shared) = weak.upgrade() else {
                    break;
                };
                let owner = SourceOwner { shared };
                let _lane = owner.shared.lane.lock().await;
                crate::playlist_files::refresh_links(&owner, None).await;
            }
        });
        Ok(())
    }

    pub(crate) async fn restore_user_state<F, Work, T>(
        &self,
        setup: bool,
        restore: F,
    ) -> Result<T, String>
    where
        F: FnOnce() -> Work,
        Work: Future<Output = Result<T, String>>,
    {
        let _lane = self.shared.lane.lock().await;
        if setup {
            // Retire acquisition before replacing its settings/catalog so an
            // old source cannot publish another page into the restored profile.
            self.release_selected(true).await;
        }
        let result = restore().await;
        if setup {
            self.shared
                .clients
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clear();
        }
        if result.is_ok()
            && let Err(error) = self.shared.load_source_counts().await
        {
            self.shared.warn_nonfatal(&error);
        }
        let connect_setup = self
            .shared
            .connect
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .upgrade()
            .is_some_and(|connect| connect.status().settings.setup_pending);
        if setup && !connect_setup {
            let current = self.shared.settings.load();
            if let Some(source) = current.sources.selected_source_id.filter(|id| {
                current
                    .sources
                    .configured
                    .iter()
                    .any(|source| source.configuration.source_id == *id)
            }) {
                self.select_source(source);
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
        if sources_changed {
            self.shared
                .clients
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clear();
        }
        let current = self.shared.settings.load();
        self.shared.load_source_counts().await?;
        if sources_changed || self.shared.selected().is_none() {
            self.release_selected(true).await;
            if let Some(source) = current.sources.selected_source_id.or_else(|| {
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
            }) {
                self.select_source(source);
            }
        }
        if let Ok(playback) = self.shared.playback() {
            playback.catalog_changed();
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
                change: CatalogChange::Acquired,
            }))
            .await;
        Ok(())
    }

    async fn remove_source_resources(
        &self,
        source_id: &SourceId,
        credential_ref: Option<crate::settings::CredentialRef>,
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
        if let Ok(playback) = self.shared.playback() {
            if let Err(error) = playback.remove_waveform_cache(source_id) {
                self.shared.warn_nonfatal(&error);
            }
        }
        if let Err(error) = self.shared.artwork.invalidate_source(source_id) {
            self.shared.warn_nonfatal(&error.to_string());
        }
        if let Some(reference) = credential_ref
            && !self
                .shared
                .settings
                .load()
                .sources
                .configured
                .iter()
                .any(|source| {
                    &source.configuration.source_id != source_id
                        && source.credential_ref.as_ref() == Some(&reference)
                })
        {
            let mut logins = self
                .shared
                .plex_logins
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            logins.remove(reference.as_str());
            drop(logins);
            let secrets = Arc::clone(&self.shared.secrets);
            self.shared.runtime.spawn_blocking(move || {
                let _ = delete_provider_secret(&secrets, &reference);
            });
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
    ) -> Result<(), String> {
        if cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        let configured = configured_source(&self.shared.settings.load().sources, &source_id)?;
        let cached_start = self
            .shared
            .database
            .cached_source(source_id.as_str(), &ReadCancellation::new())
            .await
            .map_err(string_error)?
            .is_some_and(|source| source.catalog_complete);
        let publication = self
            .shared
            .database
            .reconcile_source(
                &source_id,
                &configured.configuration.kind,
                &configured.configuration.name,
            )
            .await
            .map_err(string_error)?;
        self.install_selected(
            configured.clone(),
            None,
            publication,
            Arc::clone(&cancelled),
        )
        .await?;
        if !self.shared.acquisition_is_current(&cancelled) {
            return Ok(());
        }
        let Some(selected) = self.selected_library() else {
            return Ok(());
        };
        let owner = self.clone();
        let task = self.shared.runtime.spawn(async move {
            let opened = owner.open_client(source_id.clone()).await;
            let _lane = owner.shared.lane.lock().await;
            if !owner.shared.acquisition_is_current(&cancelled) {
                return;
            }
            let source = match opened {
                Ok(source) => source,
                Err(error) => {
                    owner.shared.warn_nonfatal(&error);
                    return;
                }
            };
            // Folder selection may have changed while credentials were being unlocked.
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
                    Some(source.clone()),
                    selected.music_folder_key,
                )
                .await
            {
                owner.shared.warn_nonfatal(&error);
            }
            owner.start_observer(selected.clone(), source.clone());
            if !cached_start || configured.configuration.is_local() {
                owner.spawn_serialized(move |owner| async move {
                    if !owner.shared.acquisition_is_current(&cancelled) {
                        return;
                    }
                    if !cached_start {
                        let _ = owner
                            .refresh_now(
                                &source_id,
                                &source,
                                &configured.configuration.name,
                                cancelled,
                                true,
                            )
                            .await;
                    } else if configured.configuration.is_local() {
                        let result = source
                            .catch_up_local(
                                &owner.shared.database,
                                selected.source_key,
                                &|_| {},
                                cancelled.clone(),
                            )
                            .await;
                        if owner.shared.acquisition_is_current(&cancelled) {
                            owner.finish_automatic_refresh(&source_id, result).await;
                        }
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

    async fn source_catalog(
        &self,
        configured: &ConfiguredSource,
        source_key: SourceKey,
    ) -> Result<(Arc<[FolderRow]>, Option<FolderKey>, LocalAccessStatus), String> {
        let cancellation = ReadCancellation::new();
        let folder_order = self
            .shared
            .database
            .folder_child_order(source_key, None, &cancellation)
            .await
            .map_err(string_error)?;
        let folders: Arc<[FolderRow]> = self
            .shared
            .database
            .folder_rows(source_key, &folder_order, &cancellation)
            .await
            .map_err(string_error)?
            .into();
        let requested_folder = configured.music_folder_id.clone();
        let folder_key = match requested_folder.as_deref() {
            Some(object_id) => self
                .shared
                .database
                .folder_key_by_object(source_key, object_id, &cancellation)
                .await
                .map_err(string_error)?,
            None => None,
        };
        let (_, track_count) = self
            .shared
            .refresh_source_counts(&configured.configuration.source_id, source_key)
            .await?;
        let formula_match_count = match configured.local_access.as_ref() {
            Some(access) => self
                .shared
                .database
                .mapping_formula_match_count(
                    source_key,
                    access.root_path.to_string_lossy().as_ref(),
                    access.server_prefix.as_deref(),
                    &cancellation,
                )
                .await
                .map_err(string_error)?,
            None => 0,
        };
        let sample_source_path = self
            .shared
            .database
            .mapping_track_page(source_key, None, None, 1, &cancellation)
            .await
            .map_err(string_error)?
            .pop()
            .map(|track| track.source_path);
        Ok((
            folders,
            folder_key,
            LocalAccessStatus {
                total_track_count: track_count,
                matched_track_count: formula_match_count,
                sample_source_path,
            },
        ))
    }

    async fn install_selected(
        &self,
        configured: ConfiguredSource,
        source: Option<Arc<Source>>,
        source_state: library::CachedSource,
        acquisition: Arc<AtomicBool>,
    ) -> Result<(), String> {
        let (music_folders, music_folder_key, local_access_status) = self
            .source_catalog(&configured, source_state.source)
            .await?;
        let selected = SelectedLibrary {
            source_id: configured.configuration.source_id.clone(),
            source_key: source_state.source,
            artwork_digest: source_state
                .artwork_digest
                .as_slice()
                .try_into()
                .map_err(string_error)?,
            music_folder_key,
            music_folder_object_id: configured.music_folder_id,
            music_folders,
            local_access_status,
        };
        if !self.shared.acquisition_is_current(&acquisition) {
            return Ok(());
        }
        self.release_selected(false).await;
        if let Some(source) = &source {
            self.shared
                .clients
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(
                    selected.source_id.clone(),
                    CachedSource {
                        configuration: configured.configuration,
                        credential_ref: configured.credential_ref,
                        jellyfin_device_id: self.shared.settings.load().jellyfin_device_id,
                        source: Arc::clone(source),
                    },
                );
        }
        self.shared
            .downloads
            .attach(
                selected.source_id.clone(),
                selected.source_key,
                source.clone(),
                selected.music_folder_key,
            )
            .await?;
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
        if let Some(source) = source {
            self.start_observer(selected, source);
        }
        Ok(())
    }

    async fn release_selected(&self, cancel_acquisition: bool) {
        if cancel_acquisition {
            self.shared.cancel_acquisition();
        }
        for job in [&self.shared.connection, &self.shared.album_release] {
            if let Some(job) = job.lock().unwrap_or_else(|p| p.into_inner()).take() {
                job.abort();
            }
        }
        self.shared.cancel_observer();
        self.shared
            .artwork_preparation
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .cancel_active();
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

    fn start_observer(&self, selected: SelectedLibrary, source: Arc<Source>) {
        let Some(observer) = source.start_selected_feed(&self.shared.runtime) else {
            return;
        };
        self.shared.cancel_observer();
        *self
            .shared
            .observer
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(observer.clone());
        let owner = self.clone();
        self.shared.runtime.spawn(async move {
            owner
                .consume_selected_feed(selected, source, observer)
                .await;
        });
    }

    async fn consume_selected_feed(
        &self,
        selected: SelectedLibrary,
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
            let _lane = self.shared.lane.lock().await;
            if observer.cancellation().load(Ordering::Acquire) {
                return;
            }
            if !event {
                self.check_remote_freshness(&selected.source_id, &source)
                    .await;
                continue;
            }
            match observer
                .apply_pending(&self.shared.database, selected.source_key)
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
            let acquisition = self.shared.begin_acquisition();
            let Some(configuration) = self.configuration(&selected.source_id) else {
                return;
            };
            let result = source
                .manual_refresh(
                    &self.shared.database,
                    &configuration.name,
                    &|_| {},
                    acquisition.clone(),
                )
                .await;
            if !self.shared.acquisition_is_current(&acquisition) {
                return;
            }
            self.finish_automatic_refresh(&selected.source_id, result)
                .await;
        }
    }

    async fn refresh_now(
        &self,
        source_id: &SourceId,
        source: &Source,
        name: &str,
        acquisition: Arc<AtomicBool>,
        automatic: bool,
    ) -> Result<(), String> {
        if !self.shared.acquisition_is_current(&acquisition) {
            return Err(SourceError::Cancelled.to_string());
        }
        self.publish_operation(SourceOperation::Refreshing {
            source_id: source_id.clone(),
            progress: initial_progress(),
        })
        .await;
        let report = refreshing_progress(Arc::clone(&self.shared), source_id.clone());
        let progress = |value| {
            if self.shared.acquisition_is_current(&acquisition) {
                report(value);
            }
        };
        let outcome = source
            .manual_refresh(
                &self.shared.database,
                name,
                &progress,
                Arc::clone(&acquisition),
            )
            .await;
        let result = outcome
            .as_ref()
            .map(|_| ())
            .map_err(source_refresh_error_text);
        if !self.shared.acquisition_is_current(&acquisition) {
            return Err(SourceError::Cancelled.to_string());
        }
        if automatic {
            self.finish_automatic_refresh(source_id, outcome).await;
        } else {
            self.finish_refresh(source_id, outcome).await;
        }
        result
    }

    async fn finish_automatic_refresh(
        &self,
        source_id: &SourceId,
        result: sources::SourceResult<()>,
    ) {
        if let Err(error) = &result
            && source_error_is_temporary(error)
        {
            warn!(%error, "automatic source refresh unavailable");
            self.publish_operation(SourceOperation::Idle).await;
        } else {
            self.finish_refresh(source_id, result).await;
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
        cancelled: Arc<AtomicBool>,
    ) -> Result<(), String> {
        let (configuration, mut source, mut credential) = (*connected).into_parts();
        let result = async {
            if !self.shared.acquisition_is_current(&cancelled) {
                return Ok(());
            }
            let previous = configured.clone();
            let mut replacement = configured;
            replacement.configuration = configuration;
            if let Some(fresh) = source.plex_login() {
                let existing = replacement.credential_ref.as_ref().and_then(|reference| {
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
            self.persist_connected_source(&replacement, credential)?;
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
            let source = Arc::new(source);
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
            if selected {
                self.install_selected(
                    replacement,
                    Some(Arc::clone(&source)),
                    publication,
                    Arc::clone(&cancelled),
                )
                .await?;
                let id = source.source_id().clone();
                self.spawn_serialized(move |owner| async move {
                    if let Some(configuration) = owner.configuration(&id) {
                        let _ = owner
                            .refresh_now(&id, &source, &configuration.name, cancelled, false)
                            .await;
                    }
                });
            } else {
                if replacement.configuration.is_local() || exclusions_changed {
                    self.refresh_now(
                        &replacement.configuration.source_id,
                        &source,
                        &replacement.configuration.name,
                        Arc::clone(&cancelled),
                        false,
                    )
                    .await?;
                } else {
                    self.publish_operation(SourceOperation::Idle).await;
                }
            }
            Ok(())
        }
        .await;
        if result.is_err() {
            self.publish_operation(SourceOperation::Idle).await;
        }
        result
    }

    pub(crate) fn persist_connected_source(
        &self,
        configured: &ConfiguredSource,
        credential: Option<String>,
    ) -> Result<(), String> {
        if let (Some(reference), Some(secret)) = (&configured.credential_ref, credential) {
            save_provider_secret(&self.shared.secrets, reference, secret)?;
        }
        self.shared.settings.update(|stored| {
            stored
                .sources
                .configured
                .retain(|item| item.configuration.source_id != configured.configuration.source_id);
            stored.sources.configured.push(configured.clone());
            Ok(())
        })?;
        self.shared
            .clients
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&configured.configuration.source_id);
        Ok(())
    }

    async fn edit_configured_source(
        &self,
        input: SourceSettingsChange,
        cancelled: Arc<AtomicBool>,
    ) -> Result<(), String> {
        if cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        let source_id = input.source_id().clone();
        let configured = configured_source(&self.shared.settings.load().sources, &source_id)?;
        let credential = configured
            .credential_ref
            .as_ref()
            .map(|reference| load_provider_secret(&self.shared.secrets, reference))
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
        if cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        match edit {
            sources::SourceEditResult::Unchanged => return Ok(()),
            sources::SourceEditResult::ConfigurationOnly(configuration) => {
                self.shared.settings.update(|stored| {
                    if let Some(item) = stored
                        .sources
                        .configured
                        .iter_mut()
                        .find(|item| item.configuration.source_id == source_id)
                    {
                        item.configuration = configuration;
                    }
                    Ok(())
                })
            }
            sources::SourceEditResult::Connected(connected) => {
                self.install_connected_edit(configured, connected, cancelled)
                    .await
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

    async fn catalog_updated(&self, update: CatalogUpdate) {
        let source_id = &update.source_id;
        crate::playlist_files::discover(self, source_id, update.source).await;
        if !update.song_rekeys.is_empty()
            && let Ok(playback) = self.shared.playback()
        {
            playback.rekey_songs(update.song_rekeys.clone());
        }
        if !update.changed() {
            return;
        }
        let change = if update.metadata {
            if update.inventory {
                CatalogChange::Acquired
            } else {
                CatalogChange::Broad
            }
        } else if update.artwork || (update.playlists && update.home) {
            CatalogChange::Broad
        } else if update.playlists {
            CatalogChange::Playlists(None)
        } else {
            CatalogChange::Home
        };
        let automatic_progress = update.tracks_added
            && matches!(*self.shared.operation.borrow(), SourceOperation::Idle)
            && self
                .shared
                .selected()
                .is_some_and(|selected| selected.source_key == update.source);
        if automatic_progress {
            self.publish_operation(SourceOperation::Refreshing {
                source_id: source_id.clone(),
                progress: source_progress(SourceReadProgress {
                    stage: SourceReadStage::Finalizing,
                    completed: 0,
                    total: None,
                }),
            })
            .await;
        }
        let mut counts_updated = false;
        {
            if let Some(current) = self
                .shared
                .selected
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_mut()
                .filter(|current| current.source_key == update.source)
            {
                current.artwork_digest = update.artwork_digest;
            }
            if update.metadata
                && update.inventory
                && let Some(selected) = self.shared.selected()
                && selected.source_key == update.source
                && let Ok(configured) =
                    configured_source(&self.shared.settings.load().sources, source_id)
            {
                match self.source_catalog(&configured, update.source).await {
                    Ok((folders, folder_key, local_access_status)) => {
                        counts_updated = true;
                        if let Some(current) = self
                            .shared
                            .selected
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .as_mut()
                            .filter(|current| current.source_key == update.source)
                        {
                            current.music_folders = folders;
                            current.music_folder_key = folder_key;
                            current.music_folder_object_id = configured.music_folder_id;
                            current.local_access_status = local_access_status;
                        }
                    }
                    Err(error) => self.shared.warn_nonfatal(&error),
                }
            }
        }
        if update.metadata || update.playlists {
            self.shared.downloads.library_changed(source_id.clone());
        }
        if let Ok(playback) = self.shared.playback() {
            playback.catalog_changed();
        }
        if update.metadata {
            if !counts_updated
                && let Err(error) = self
                    .shared
                    .refresh_source_counts(source_id, update.source)
                    .await
            {
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
                source_key: Some(update.source),
                favorite: None,
                change,
            }))
            .await;
        if let Some(selected) = self
            .shared
            .selected()
            .filter(|selected| selected.source_key == update.source)
        {
            let report_progress = update.tracks_added
                || matches!(
                    &*self.shared.operation.borrow(),
                    SourceOperation::Refreshing { source_id: active, .. } if active == source_id
                );
            self.start_artwork_preparation(selected, report_progress);
        }
        if automatic_progress {
            self.publish_operation(SourceOperation::Idle).await;
        }
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
        if let Ok(playback) = self.shared.playback() {
            playback.catalog_changed();
        }
        self.shared
            .send(SourceEvent::CatalogPublished(CatalogPublication {
                source_key: Some(source),
                favorite: None,
                change,
            }))
            .await;
        if let Some(selected) = self
            .shared
            .selected()
            .filter(|selected| selected.source_key == source)
        {
            {
                let mut preparation = self
                    .shared
                    .artwork_preparation
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                preparation.cancel_active();
                preparation.completed = None;
            }
            self.start_artwork_preparation(selected, false);
        }
    }

    async fn check_remote_freshness(&self, source_id: &SourceId, source: &Source) {
        let Some(configuration) = self.configuration(source_id) else {
            return;
        };
        if configuration.is_local() {
            return;
        }
        let Some(acquisition) = self.shared.try_begin_acquisition() else {
            return;
        };
        let result = source
            .refresh_if_needed(
                &self.shared.database,
                &configuration.name,
                &|_| {},
                acquisition.clone(),
            )
            .await;
        if self.shared.acquisition_is_current(&acquisition) {
            self.finish_automatic_refresh(source_id, result).await;
        }
    }

    async fn publish_mapping_count(
        &self,
        source: SourceKey,
        formula_match_count: usize,
    ) -> Result<(), String> {
        if let Some(current) = self
            .shared
            .selected
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_mut()
            .filter(|current| current.source_key == source)
        {
            current.local_access_status.matched_track_count = formula_match_count;
        }
        self.shared.playback()?.stream_inputs_changed()?;
        self.shared
            .send(SourceEvent::Configured(self.list_sources()))
            .await;
        Ok(())
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

    async fn publish_current_catalog(
        &self,
        favorite: Option<FavoriteSettlement>,
        change: CatalogChange,
    ) {
        if let Some(selected) = self.shared.selected() {
            self.publish_catalog(&selected, favorite, change).await;
        }
    }
}

impl SourceOwner {
    pub fn home_variations(&self) -> (i64, i64) {
        (
            self.shared.home_showcase_variation,
            self.shared.home_explore_variation.load(Ordering::Relaxed),
        )
    }
    pub fn smb_shares(
        &self,
        settings: sources::FileSourceSettings,
        credentials: sources::FileCredentials,
    ) -> Receiver<Result<Vec<(String, String)>, String>> {
        self.reply(move |_, _| async move {
            sources::list_smb_shares(settings, credentials)
                .await
                .map_err(string_error)
        })
    }

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
                result = authorization => { let _ = send.send(result.map(|authorized| project(LoginEvent::Authorized(authorized))).map_err(string_error)).await; }
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
            LoginEvent::Authorized(Authorization::Emby(servers)) => {
                SourceLoginEvent::Servers(servers)
            }
            LoginEvent::Authorized(Authorization::Jellyfin(login)) => {
                SourceLoginEvent::Jellyfin(login)
            }
            LoginEvent::Authorized(Authorization::Plex { login, .. }) => {
                SourceLoginEvent::Plex(Box::new(login))
            }
            LoginEvent::Authorized(Authorization::Files {
                settings,
                credentials,
            }) => SourceLoginEvent::Files {
                settings,
                credentials,
            },
            _ => unreachable!(),
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
                let Some(reference) = configured.credential_ref.as_ref() else {
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
    ) -> Result<Option<crate::settings::CredentialRef>, String> {
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
                return Ok(configured.credential_ref);
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

    pub fn configured_source(
        &self,
        source_id: &SourceId,
    ) -> Result<Option<EditableSource>, String> {
        self.shared
            .settings
            .load()
            .sources
            .configured
            .iter()
            .find(|source| &source.configuration.source_id == source_id)
            .map(|source| editable_source(&source.configuration))
            .transpose()
    }

    pub fn list_sources(&self) -> ConfiguredSources {
        self.shared
            .configured_sources(self.shared.selected().as_ref())
    }

    pub fn operation(&self) -> tokio::sync::watch::Receiver<SourceOperation> {
        self.shared.operation.subscribe()
    }

    pub fn catalog_changes(&self) -> tokio::sync::watch::Receiver<()> {
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

    pub fn discover_servers(&self, provider: sources::DiscoveryProvider) {
        let events = self.shared.outputs.discovery.clone();
        let _ = events.try_send(DiscoveryUpdate {
            provider,
            servers: Arc::from([]),
            status: DiscoveryStatus::Searching,
        });
        self.shared.runtime.spawn(async move {
            let update =
                match sources::discover_servers(provider, Duration::from_millis(1_500)).await {
                    Ok(servers) if servers.is_empty() => DiscoveryUpdate {
                        provider,
                        servers: Arc::from([]),
                        status: DiscoveryStatus::Empty,
                    },
                    Ok(servers) => DiscoveryUpdate {
                        provider,
                        status: DiscoveryStatus::Found(servers.len() as u64),
                        servers: servers.into(),
                    },
                    Err(error) => DiscoveryUpdate {
                        provider,
                        servers: Arc::from([]),
                        status: DiscoveryStatus::Failed(error.to_string()),
                    },
                };
            let _ = events.try_send(update);
        });
    }

    pub fn configure_source(
        &self,
        mut input: SourceSetup,
    ) -> Receiver<Result<SelectedLibrary, String>> {
        let (sender, receiver) = async_channel::bounded(1);
        let cancelled = self.shared.begin_acquisition();
        self.shared.cancel_observer();
        self.spawn_serialized(move |owner| async move {
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
                if !owner.shared.acquisition_is_current(&cancelled) {
                    return Ok(());
                }
                let credential_ref = match reused_credential {
                    Some(reference) => Some(reference),
                    None => credential
                        .as_ref()
                        .map(|_| fresh_credential_ref())
                        .transpose()?,
                };
                let configured = ConfiguredSource {
                    configuration: configuration.clone(),
                    credential_ref,
                    music_folder_id: None,
                    local_access: None,
                    enable_half_stars: false,
                };
                owner.persist_connected_source(&configured, credential)?;
                owner
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
                persisted_source_id = Some(configuration.source_id.clone());
                owner
                    .shared
                    .send(SourceEvent::Configured(
                        owner
                            .shared
                            .configured_sources(owner.shared.selected().as_ref()),
                    ))
                    .await;
                let shared = Arc::clone(&owner.shared);
                let progress_acquisition = Arc::clone(&cancelled);
                let progress = move |value| {
                    if !shared.acquisition_is_current(&progress_acquisition) {
                        return;
                    }
                    shared.publish_operation(SourceOperation::Adding {
                        progress: source_progress(value),
                    });
                };
                let outcome = source
                    .manual_refresh(
                        &owner.shared.database,
                        &configuration.name,
                        &progress,
                        Arc::clone(&cancelled),
                    )
                    .await;
                if !owner.shared.acquisition_is_current(&cancelled) {
                    return Ok(());
                }
                let source_state = owner
                    .shared
                    .database
                    .cached_source(configuration.source_id.as_str(), &ReadCancellation::new())
                    .await
                    .map_err(string_error)?;
                let Some(source_state) = source_state else {
                    return Err(outcome.err().map_or_else(
                        || "The initial library scan did not complete".to_string(),
                        |error| error.to_string(),
                    ));
                };
                owner
                    .install_selected(
                        configured,
                        Some(Arc::clone(&source)),
                        source_state,
                        Arc::clone(&cancelled),
                    )
                    .await?;
                outcome.map_err(string_error)?;
                Ok(())
            }
            .await;
            if !owner.shared.acquisition_is_current(&cancelled) {
                let _ = sender.try_send(Err(SourceError::Cancelled.to_string()));
                return;
            }
            if let Err(error) = &result {
                if let Some(selected) = owner.shared.selected()
                    && let Some(source) = owner.cached_client(&selected.source_id)
                {
                    owner.start_observer(selected, source);
                }
                owner
                    .publish_operation(SourceOperation::Failed {
                        source_id: persisted_source_id,
                        message: error.clone(),
                        add_form: true,
                    })
                    .await;
            }
            let _ = sender.try_send(result.and_then(|()| {
                owner
                    .selected_library()
                    .ok_or_else(|| SourceError::Cancelled.to_string())
            }));
        });
        receiver
    }

    pub fn update_source(&self, input: SourceSettingsChange) -> Receiver<Result<(), String>> {
        let (sender, receiver) = async_channel::bounded(1);
        let cancelled = self.shared.begin_acquisition();
        self.spawn_serialized(move |owner| async move {
            let result = owner.edit_configured_source(input, cancelled).await;
            if let Err(error) = &result {
                owner.shared.warn_nonfatal(&error);
            }
            let _ = sender.try_send(result);
        });
        receiver
    }

    pub fn set_half_stars(&self, source_id: SourceId, enabled: bool) {
        self.spawn_serialized(move |owner| async move {
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

    /// Completes when the source's stored catalog is selected. Connection and refresh
    /// continue in the background, including while credentials are being unlocked.
    pub fn select_source(&self, source_id: SourceId) -> Receiver<Result<SelectedLibrary, String>> {
        let (sender, receiver) = async_channel::bounded(1);
        let cancelled = self.shared.begin_acquisition();
        self.shared.cancel_observer();
        self.spawn_serialized(move |owner| async move {
            if cancelled.load(Ordering::Acquire) {
                let _ = sender.try_send(Err(SourceError::Cancelled.to_string()));
                return;
            }
            owner
                .publish_operation(SourceOperation::Switching {
                    target: source_id.clone(),
                    progress: initial_progress(),
                })
                .await;
            let result = owner
                .select_now(source_id.clone(), Arc::clone(&cancelled))
                .await;
            if cancelled.load(Ordering::Acquire) {
                let _ = sender.try_send(Err(SourceError::Cancelled.to_string()));
                return;
            }
            if let Err(error) = &result {
                if let Some(selected) = owner.shared.selected()
                    && let Some(source) = owner.cached_client(&selected.source_id)
                {
                    owner.start_observer(selected, source);
                }
                owner
                    .publish_operation(SourceOperation::Failed {
                        source_id: Some(source_id),
                        message: error.clone(),
                        add_form: false,
                    })
                    .await;
            }
            let _ = sender.try_send(result.and_then(|()| {
                owner
                    .selected_library()
                    .ok_or_else(|| SourceError::Cancelled.to_string())
            }));
        });
        receiver
    }

    pub fn change_secret_storage(&self, mode: SecretStorageMode) -> Receiver<Result<(), String>> {
        let (sender, receiver) = async_channel::bounded(1);
        let owner = self.clone();
        self.shared.cancel_acquisition();
        self.shared.runtime.spawn(async move {
            let previous = owner.shared.settings.load();
            let changing = previous.ui.secret_storage_mode != mode;
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
                    let scope = fresh_credential_ref()?.as_str().to_string();
                    settings.update(|stored| {
                        stored.ui.secret_storage_mode = mode;
                        // Reset as confirmed in Preferences; never unlock the old keyring.
                        stored.secret_scope_id = scope;
                        stored.ui.lastfm_api_key.clear();
                        stored.scrobbling.lastfm.username.clear();
                        stored.scrobbling.librefm.username.clear();
                        for descriptor in scrobbling::secret_descriptors() {
                            descriptor.value_mut(&mut stored.scrobbling).clear();
                        }
                        stored.scrobbling_secrets_present = false;
                        for source in stored
                            .sources
                            .configured
                            .iter_mut()
                            .chain(&mut stored.sources.integrations)
                        {
                            source.credential_ref = None;
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
                owner
                    .shared
                    .clients
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clear();
                owner
                    .shared
                    .plex_logins
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clear();
                owner.release_selected(true).await;
                if let Some(selected) = owner.shared.settings.load().sources.selected_source_id {
                    owner.select_source(selected);
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
    }

    pub(crate) async fn ensure_local_source(&self) -> Result<Arc<Source>, String> {
        let stored = self.shared.settings.load();
        let local = stored
            .sources
            .configured
            .iter()
            .find(|item| item.configuration.is_local());
        let source = if let Some(local) = local {
            self.client(&local.configuration.source_id)?
        } else {
            let connected = Source::connect(
                fresh_source_id()?,
                SourceSetup::Local { roots: Vec::new() },
                "",
            )
            .await
            .map_err(string_error)?;
            let (configuration, source, credential) = connected.into_parts();
            self.persist_connected_source(
                &ConfiguredSource {
                    configuration: configuration.clone(),
                    credential_ref: None,
                    music_folder_id: None,
                    local_access: None,
                    enable_half_stars: false,
                },
                credential,
            )?;
            library::Scan::begin(
                &self.shared.database,
                configuration.source_id.as_str(),
                &configuration.name,
                "local",
                None,
            )
            .await
            .map_err(string_error)?
            .finish()
            .await
            .map_err(string_error)?;
            self.shared
                .send(SourceEvent::Configured(
                    self.shared
                        .configured_sources(self.shared.selected().as_ref()),
                ))
                .await;
            Arc::new(source)
        };
        Ok(source)
    }

    pub fn add_local_paths(&self, paths: Vec<PathBuf>) -> Receiver<Result<(), String>> {
        let (sender, receiver) = async_channel::bounded(1);
        let cancelled = self.shared.begin_acquisition();
        self.spawn_serialized(move |owner| async move {
            let result = async {
                let source = owner.ensure_local_source().await?;
                let configuration = owner
                    .configuration(source.source_id())
                    .ok_or_else(source_access_unavailable)?;
                let mut roots = local_roots(&configuration)?;
                roots.extend(paths);
                owner
                    .edit_configured_source(
                        SourceSettingsChange::Local {
                            source_id: source.source_id().clone(),
                            roots,
                            excluded_folders: None,
                        },
                        Arc::clone(&cancelled),
                    )
                    .await?;
                if owner.shared.selected().is_none() {
                    owner
                        .select_now(source.source_id().clone(), cancelled)
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
        let cancelled = self.shared.begin_acquisition();
        self.spawn_serialized(move |owner| async move {
            let result = async {
                let source = owner.ensure_local_source().await?;
                let mut roots = owner.document_roots()?;
                for mut root in added {
                    if let Some(current) = roots.iter_mut().find(|current| current.uri == root.uri)
                    {
                        root.id = current.id.clone();
                        *current = root;
                    } else {
                        roots.push(root);
                    }
                }
                owner
                    .edit_configured_source(
                        SourceSettingsChange::LocalDocuments {
                            source_id: source.source_id().clone(),
                            roots,
                        },
                        Arc::clone(&cancelled),
                    )
                    .await?;
                if owner.shared.selected().is_none() {
                    owner
                        .select_now(source.source_id().clone(), cancelled)
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

    pub fn refresh_source(&self, source_id: SourceId) -> Receiver<Result<(), String>> {
        let acquisition = self.shared.begin_acquisition();
        let (sender, receiver) = async_channel::bounded(1);
        let owner = self.clone();
        self.shared.runtime.spawn(async move {
            let result = async {
                if !owner.shared.acquisition_is_current(&acquisition) {
                    return Err(SourceError::Cancelled.to_string());
                }
                owner.open_client(source_id.clone()).await
            }
            .await;
            let _lane = owner.shared.lane.lock().await;
            let result = result.and_then(|source| {
                owner
                    .configuration(&source_id)
                    .map(|configuration| (source, configuration))
                    .ok_or_else(source_access_unavailable)
            });
            let result = match result {
                Ok((source, configuration)) => {
                    owner
                        .refresh_now(&source_id, &source, &configuration.name, acquisition, false)
                        .await
                }
                Err(message) => {
                    if owner.shared.acquisition_is_current(&acquisition) {
                        owner
                            .publish_operation(SourceOperation::Failed {
                                source_id: Some(source_id),
                                message: message.clone(),
                                add_form: false,
                            })
                            .await;
                    }
                    Err(message)
                }
            };
            let _ = sender.try_send(result);
        });
        receiver
    }

    pub fn save_local_access(
        &self,
        mut input: SourceLocalAccess,
        wait_until_mapped: bool,
    ) -> Receiver<Result<(), String>> {
        let (sender, receiver) = async_channel::bounded(1);
        let cancelled = self.shared.begin_acquisition();
        let owner = self.clone();
        self.shared.runtime.spawn(async move {
            let result = async {
                let (source, source_key) = owner.source_target(&input.source_id).await?;
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
                let formula_match_count = owner
                    .shared
                    .database
                    .mapping_formula_match_count(
                        source_key,
                        root_path.to_string_lossy().as_ref(),
                        input.server_prefix.as_deref(),
                        &ReadCancellation::new(),
                    )
                    .await
                    .map_err(string_error)?;
                owner
                    .publish_mapping_count(source_key, formula_match_count)
                    .await?;
                Ok((formula_match_count, root_path, source, source_key))
            }
            .await;
            let (formula_match_count, root_path, source, source_key) = match result {
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
                owner
                    .publish_mapping_count(source_key, formula_match_count)
                    .await
            }
            .await;
            if wait_until_mapped {
                let _ = sender.send(completion).await;
            } else if let Err(error) = completion {
                warn!(%error, "background Local mapping did not complete");
            }
        });
        receiver
    }

    pub fn clear_local_access(&self, source_id: SourceId) {
        self.spawn_serialized(move |owner| async move {
            if let Ok(Some(source)) = owner.shared.database.source_identity_key(&source_id).await {
                let _ = owner.shared.database.clear_mapping_access(source).await;
                if let Some(current) = owner
                    .shared
                    .selected
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .as_mut()
                    .filter(|current| current.source_key == source)
                {
                    current.local_access_status.matched_track_count = 0;
                }
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

    pub fn forget_source(&self, source_id: SourceId) -> Receiver<Result<(), String>> {
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
                .remove_source_resources(
                    &source_id,
                    configured.and_then(|item| item.credential_ref),
                )
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
    }

    pub(crate) fn install_connect(&self, connect: &Arc<crate::connect::ConnectOwner>) {
        *self
            .shared
            .connect
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Arc::downgrade(connect);
        self.shared.downloads.install_connect(Arc::downgrade(
            &(Arc::clone(connect) as Arc<dyn downloads::ConnectDownload>),
        ));
    }

    pub(crate) fn connect_media_changed(&self) {
        self.shared.downloads.connect_changed();
    }

    pub(crate) async fn connect_artwork_changed(&self) {
        if let Ok(playback) = self.shared.playback() {
            playback.catalog_changed();
        }
        self.shared
            .send(SourceEvent::CatalogPublished(CatalogPublication {
                source_key: None,
                favorite: None,
                change: CatalogChange::Broad,
            }))
            .await;
    }

    pub fn download_media(&self, subject: downloads::DownloadSubject, media_uris: Vec<String>) {
        self.spawn_serialized(move |owner| async move {
            let mut source_ids = media_uris
                .iter()
                .filter_map(|media_uri| {
                    library::source_entity_parts(media_uri)
                        .and_then(|(source_id, kind, _)| (kind == "track").then_some(source_id))
                })
                .collect::<Vec<_>>();
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
                    .attach(source_id, source_key, Some(source), None)
                    .await
                {
                    owner.shared.warn_nonfatal(&error);
                }
            }
            owner.shared.downloads.download(subject, media_uris);
        });
    }

    pub fn set_favorite(&self, target: FavoriteTarget, favorite: bool) {
        self.set_favorite_with_result(target, favorite);
    }

    pub fn set_favorite_with_result(
        &self,
        target: FavoriteTarget,
        favorite: bool,
    ) -> Receiver<Result<bool, String>> {
        info!(kind = target.kind(), favorite, "Favorite requested");
        let (sender, receiver) = async_channel::bounded(1);
        self.spawn_serialized(move |owner| async move {
            let result = match owner.apply_favorite(target.clone(), favorite).await {
                Ok(()) => owner
                    .shared
                    .database
                    .favorite(&target)
                    .await
                    .map_err(string_error),
                Err(error) => Err(error),
            };
            let _ = sender.try_send(result);
        });
        receiver
    }

    pub fn set_rating(&self, target: FavoriteTarget, rating: Option<u8>) {
        self.spawn_serialized(move |owner| async move {
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
        self.start_artwork_preparation(selected, true);
    }

    pub fn refresh_home(
        &self,
        source_id: SourceId,
        kind: crate::settings::HomeSectionKind,
    ) -> Receiver<Result<(), String>> {
        use crate::settings::HomeSectionKind;
        if kind == HomeSectionKind::NewlyAdded {
            return self.refresh_source(source_id);
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
            let _lane = owner.shared.lane.lock().await;
            let entries = match source.home_section(section).await {
                Ok(entries) => entries,
                Err(error) => {
                    warn!(%error, "could not refresh Home section");
                    return Ok(());
                }
            };
            let mut scan = library::Scan::begin_items(&database, source_id.as_str())
                .await
                .map_err(string_error)?;
            scan.replace_home_section(section.id(), None, &entries)
                .await
                .map_err(string_error)?;
            scan.finish().await.map_err(string_error)?;
            Ok(())
        })
    }

    pub fn set_music_folder(
        &self,
        source_id: SourceId,
        folder_object_id: Option<String>,
    ) -> Receiver<Result<(), String>> {
        let (sender, receiver) = async_channel::bounded(1);
        self.spawn_serialized(move |owner| async move {
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
                        .folder_key_by_object(source_key, id, &ReadCancellation::new())
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
                    .downloads
                    .attach(
                        source_id.clone(),
                        source_key,
                        owner.cached_client(&source_id),
                        key,
                    )
                    .await?;
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
        if let Some((source_id, source_key, false)) = source {
            self.deliver_favorite(source_id, source_key, target, favorite)
                .await;
        }
        Ok(())
    }

    async fn apply_rating(&self, target: FavoriteTarget, rating: Option<u8>) {
        if !self
            .shared
            .database
            .set_rating(&target, rating)
            .await
            .unwrap_or(false)
        {
            return;
        }
        self.publish_current_catalog(None, CatalogChange::Broad)
            .await;
        let Some((source_id, source_key, local)) = self.favorite_source(&target).await else {
            return;
        };
        let Ok(source) = self.client(&source_id) else {
            return;
        };
        match &target {
            FavoriteTarget::Track(media_uri) if local => {
                if let Err(error) = source
                    .write_file_track_rating(&self.shared.database, source_key, media_uri, rating)
                    .await
                {
                    warn!(%error, "could not write file Track rating");
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
            && cached.configuration == configured.configuration
            && cached.credential_ref == configured.credential_ref
            && cached.jellyfin_device_id == stored.jellyfin_device_id
        {
            return Ok(cached.source.clone());
        }
        let credential = configured
            .credential_ref
            .as_ref()
            .map(|reference| load_provider_secret(&self.shared.secrets, reference))
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
                    item.configuration == configured.configuration
                        && item.credential_ref == configured.credential_ref
                })
        {
            clients.insert(
                source_id.clone(),
                CachedSource {
                    configuration: configured.configuration,
                    credential_ref: configured.credential_ref,
                    jellyfin_device_id: stored.jellyfin_device_id,
                    source: Arc::clone(&source),
                },
            );
        }
        Ok(source)
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
            .credential_ref
            .as_ref()
            .ok_or("Plex credential reference is missing")?;
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
        let saved_reference = reference.clone();
        login
            .try_lock()
            .map_err(string_error)?
            .bind_save(Arc::new(move |secret| {
                let shared = shared.upgrade().ok_or(SourceError::Cancelled)?;
                let _logins = shared
                    .plex_logins
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if !shared
                    .settings
                    .load()
                    .sources
                    .configured
                    .iter()
                    .any(|source| source.credential_ref.as_ref() == Some(&saved_reference))
                {
                    return Err(SourceError::Cancelled);
                }
                save_provider_secret(&shared.secrets, &saved_reference, secret)
                    .map_err(SourceError::Other)
            }));
        logins.insert(reference.as_str().to_owned(), login);
        Ok(())
    }

    pub fn configuration(&self, source_id: &SourceId) -> Option<SourceConfiguration> {
        configured_source(&self.shared.settings.load().sources, source_id)
            .ok()
            .map(|configured| configured.configuration)
    }

    async fn fail_artwork_preparation(
        &self,
        key: ArtworkPreparationKey,
        token: u64,
        revision: u64,
    ) {
        if self
            .shared
            .artwork_preparation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .fail(key, token)
        {
            self.shared
                .send(SourceEvent::ArtworkPreparation {
                    source_key: key.source,
                    revision,
                    progress: None,
                })
                .await;
        }
    }

    fn start_artwork_preparation(&self, selected: SelectedLibrary, report_progress: bool) {
        let key = ArtworkPreparationKey {
            source: selected.source_key,
            digest: selected.artwork_digest,
        };
        let mut preparation = self
            .shared
            .artwork_preparation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = preparation.active.as_ref().map(|active| active.key);
        let Some((token, cancelled)) = preparation.admit(key) else {
            return;
        };
        drop(preparation);
        if let Some(previous) = previous {
            let _ = self
                .shared
                .outputs
                .events
                .try_send(SourceEvent::ArtworkPreparation {
                    source_key: previous.source,
                    revision: artwork_digest_revision(&previous.digest),
                    progress: None,
                });
        }
        let owner = self.clone();
        let task = self.shared.runtime.spawn(async move {
            let initial_revision = artwork_digest_revision(&selected.artwork_digest);
            let current_revision = Arc::new(AtomicU64::new(initial_revision));
            let progress_revision = Arc::clone(&current_revision);
            let progress_owner = owner.clone();
            let events = owner.shared.outputs.events.clone();
            let progress = move |revision, completed| {
                progress_revision.store(revision, Ordering::Release);
                if !report_progress || !artwork_preparation_is_current(&owner, key, token) {
                    return;
                }
                let _ = events.try_send(SourceEvent::ArtworkPreparation {
                    source_key: key.source,
                    revision,
                    progress: Some(artwork_preparation_progress(completed)),
                });
            };
            let result = crate::artwork::prepare_database_source(
                &progress_owner.shared.artwork,
                &progress_owner.shared.database,
                selected.source_key,
                &selected.source_id,
                selected.artwork_digest,
                &progress,
                cancelled,
            )
            .await;
            let revision = current_revision.load(Ordering::Acquire);
            let completed = match result {
                Ok(completed) => completed,
                Err(crate::artwork::PreparationError::Artwork(
                    artwork::ArtworkError::Cancelled,
                )) => return,
                Err(error) => {
                    warn!(%error, "could not prepare selected source artwork");
                    progress_owner
                        .fail_artwork_preparation(key, token, revision)
                        .await;
                    return;
                }
            };
            if !progress_owner
                .shared
                .artwork_preparation
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .complete(key, token)
            {
                return;
            }
            if completed.is_some() {
                progress_owner
                    .shared
                    .send(SourceEvent::ArtworkPreparation {
                        source_key: selected.source_key,
                        revision,
                        progress: None,
                    })
                    .await;
            }
        });
        self.shared
            .artwork_preparation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .attach_abort(token, task.abort_handle());
    }

    async fn deliver_favorite(
        &self,
        source_id: SourceId,
        source_key: SourceKey,
        target: FavoriteTarget,
        favorite: bool,
    ) {
        let Ok(source) = self.client(&source_id) else {
            return;
        };
        let Some((kind, object_id)) = favorite_object(&source_id, &target) else {
            return;
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
            }
            Err(error) if source_error_is_temporary(&error) => {
                info!(favorite, "Favorite delivery deferred");
                let _ = self
                    .shared
                    .database
                    .defer_remote_favorite(&target, favorite, unix_seconds().saturating_add(30))
                    .await;
            }
            Err(_) => {
                info!(favorite, "Favorite delivery rejected");
                if let Ok(Some(previous)) = self
                    .shared
                    .database
                    .reject_remote_favorite(&target, favorite)
                    .await
                {
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
    matches!(error, SourceError::Cancelled)
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
    let cancelled = owner.shared.begin_acquisition();
    owner.spawn_serialized(move |owner| async move {
        if let Err(error) = owner.edit_configured_source(input, cancelled).await {
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
            transcoded_download_bitrate_limit_kbps: configured
                .configuration
                .transcoded_download_bitrate_limit_kbps(),
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
                status: selected
                    .filter(|selected| selected.source_id == configured.configuration.source_id)
                    .map(|selected| selected.local_access_status.clone())
                    .unwrap_or_default(),
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
        local_access: local_access.into(),
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
        source: SourceSummary {
            id: configuration.source_id.clone(),
            kind: configuration.kind.clone(),
            name: configuration.name.clone(),
            transcoded_download_bitrate_limit_kbps: configuration
                .transcoded_download_bitrate_limit_kbps(),
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
            SourceReadStage::Artwork => SourceProgressStage::Artwork,
            SourceReadStage::Files => SourceProgressStage::Files,
            SourceReadStage::Finalizing => SourceProgressStage::Finalizing,
        },
        completed: progress.completed,
        total: progress.total,
    }
}

pub(crate) fn refreshing_progress(
    shared: Arc<Shared>,
    source_id: SourceId,
) -> impl Fn(SourceReadProgress) + Send + Sync {
    move |value| {
        shared.publish_operation(SourceOperation::Refreshing {
            source_id: source_id.clone(),
            progress: source_progress(value),
        });
    }
}

fn artwork_preparation_progress(completed: usize) -> SourceProgress {
    SourceProgress {
        stage: SourceProgressStage::Artwork,
        completed,
        total: None,
    }
}

fn artwork_digest_revision(digest: &[u8; 32]) -> u64 {
    u64::from_le_bytes(digest[..8].try_into().expect("digest prefix"))
}

fn artwork_preparation_is_current(
    owner: &SourceOwner,
    key: ArtworkPreparationKey,
    token: u64,
) -> bool {
    owner
        .shared
        .artwork_preparation
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .is_current(key, token)
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
mod artwork_preparation_tests {
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
        let reference = crate::settings::CredentialRef::new("shared-plex");
        let secret = serde_json::json!({"client_id":"test-device","token":"test-token","account_id":"profile","profiles":{},"resources":{"profile":{"one":"one-token","two":"two-token"}},"home_admin_subscription":false,"download_subscriptions":{}}).to_string();
        let configured:Vec<_> = ["one","two"].into_iter().map(|server|ConfiguredSource {
            configuration: SourceConfiguration {source_id:SourceId::new(server),kind:"plex".into(),name:server.into(),provider_payload:serde_json::json!({"version":1,"server_id":server,"profile_id":"profile","base_url":"http://127.0.0.1:9","address_override":null,"local":true,"relay":false,"owned":true,"trust_invalid_cert":false}).to_string()},
            credential_ref:Some(reference.clone()),music_folder_id:None,local_access:None,enable_half_stars:false,
        }).collect();
        settings
            .update(|stored| {
                stored.sources.configured = configured.clone();
                Ok(())
            })
            .unwrap();
        save_provider_secret(&secrets, &reference, secret.clone()).unwrap();
        let owner = SourceOwner::open_dormant(
            Artwork::new(directory.path().join("artwork"), runtime.clone()).unwrap(),
            Arc::clone(&database),
            Downloads::new(
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
        )
        .owner;
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
            load_provider_secret(&secrets, &reference)
                .unwrap()
                .is_some()
        );
        let snapshot = detached.lock().await.for_setup();
        detached
            .lock()
            .await
            .merge_authorization(&snapshot)
            .unwrap();
        owner
            .shared
            .settings
            .update(|stored| {
                stored.sources.configured.clear();
                Ok(())
            })
            .unwrap();
        owner
            .remove_source_resources(
                &configured[1].configuration.source_id,
                Some(reference.clone()),
            )
            .await;
        tokio::time::timeout(Duration::from_secs(2), async {
            while load_provider_secret(&secrets, &reference)
                .unwrap()
                .is_some()
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("removed credentials are cleaned up in the background");
        assert!(matches!(
            detached.lock().await.merge_authorization(&snapshot),
            Err(SourceError::Cancelled)
        ));
        assert!(
            load_provider_secret(&secrets, &reference)
                .unwrap()
                .is_none()
        );
        assert!(owner.shared.plex_logins.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn automatic_refresh_feedback_requires_new_tracks() {
        let directory = tempfile::tempdir().unwrap();
        let database = Arc::new(
            Database::open(directory.path().join("library.sqlite"))
                .await
                .unwrap(),
        );
        let source_id = SourceId::new("test");
        library::Scan::begin(&database, source_id.as_str(), "Test", "test", None)
            .await
            .unwrap()
            .finish()
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
        )
        .owner;

        for error in [
            SourceError::Network("connection refused".into()),
            SourceError::Cancelled,
            SourceError::Network("connection timed out".into()),
        ] {
            while receiver.try_recv().is_ok() {}
            owner.finish_automatic_refresh(&source_id, Err(error)).await;
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
        owner
            .finish_automatic_refresh(&source_id, Err(SourceError::Auth("Login expired".into())))
            .await;
        assert!(matches!(
            *owner.shared.operation.borrow(),
            SourceOperation::Failed { .. }
        ));

        let image = directory.path().join("cover.png");
        image::RgbaImage::from_pixel(16, 16, image::Rgba([30, 80, 160, 255]))
            .save(&image)
            .unwrap();
        let mut selected_library = SelectedLibrary {
            source_id: source_id.clone(),
            source_key: cached.source,
            artwork_digest: cached.artwork_digest.try_into().unwrap(),
            music_folder_key: None,
            music_folder_object_id: None,
            music_folders: Arc::from([]),
            local_access_status: LocalAccessStatus::default(),
        };
        for (revision, visible) in [(1, true), (2, false), (3, true), (4, false)] {
            let operation = if revision >= 3 {
                SourceOperation::Refreshing {
                    source_id: if revision == 3 {
                        source_id.clone()
                    } else {
                        SourceId::new("new-acquisition")
                    },
                    progress: initial_progress(),
                }
            } else {
                SourceOperation::Idle
            };
            owner.publish_operation(operation).await;
            while receiver.try_recv().is_ok() {}
            let binding = serde_json::to_vec(&sources::LocalImageRef::File {
                source_id: source_id.clone(),
                path: image.to_string_lossy().into_owned(),
                revision: revision.to_string(),
            })
            .unwrap();
            let mut scan = library::Scan::begin_items(&database, source_id.as_str())
                .await
                .unwrap();
            scan.write_artist(&library::ArtistInput {
                object_id: "artist".into(),
                name: "Artist".into(),
                artwork_binding: Some(binding.to_vec()),
                ..Default::default()
            })
            .await
            .unwrap();
            scan.finish().await.unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if matches!(
                        receiver.recv().await.unwrap(),
                        SourceEvent::CatalogPublished(_)
                    ) {
                        break;
                    }
                }
            })
            .await
            .expect("accepted artwork reaches the catalog consumer");
            if revision == 1 {
                let cached = database
                    .cached_source(source_id.as_str(), &ReadCancellation::new())
                    .await
                    .unwrap()
                    .unwrap();
                selected_library.artwork_digest = cached.artwork_digest.try_into().unwrap();
                *owner.shared.selected.lock().unwrap() = Some(selected_library.clone());
                owner.selected_library_revealed(source_id.clone());
            } else if revision == 4 {
                assert!(matches!(&*owner.shared.operation.borrow(),
                    SourceOperation::Refreshing { source_id, .. }
                    if source_id.as_str() == "new-acquisition"));
            }
            let reported = tokio::time::timeout(Duration::from_secs(5), async {
                let mut reported = false;
                loop {
                    if let SourceEvent::ArtworkPreparation { progress, .. } =
                        receiver.recv().await.unwrap()
                    {
                        match progress {
                            Some(progress) => {
                                assert_eq!(progress.completed, 1);
                                reported = true;
                            }
                            None => break reported,
                        }
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(
                reported, visible,
                "initial/manual loads report progress; background updates stay quiet"
            );
            selected_library = owner.shared.selected().unwrap();
            assert!(
                owner
                    .shared
                    .artwork
                    .source_preparation_complete(
                        &source_id,
                        artwork_digest_revision(&selected_library.artwork_digest)
                    )
                    .unwrap()
            );
        }
        for (metadata, artwork, tracks_added, selected, visible) in [
            (false, false, false, true, false),
            (false, true, false, true, false),
            (true, false, false, true, false),
            (true, false, true, true, true),
            (true, false, true, false, false),
        ] {
            owner.publish_operation(SourceOperation::Idle).await;
            *owner.shared.selected.lock().unwrap() = selected.then(|| selected_library.clone());
            while receiver.try_recv().is_ok() {}
            let configuration = owner.configuration_changes();
            let catalog = owner.catalog_changes();
            owner
                .catalog_updated(CatalogUpdate {
                    source_id: source_id.clone(),
                    source: cached.source,
                    artwork_digest: selected_library.artwork_digest,
                    metadata,
                    playlists: false,
                    artwork,
                    home: false,
                    inventory: false,
                    tracks_added,
                    song_rekeys: Arc::from([]),
                })
                .await;
            assert!(
                !configuration.has_changed().unwrap(),
                "catalog data does not change source configuration"
            );
            assert_eq!(catalog.has_changed().unwrap(), metadata || artwork);
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
            assert_eq!(published, metadata || artwork);
        }
        for (artwork, home) in [(false, true), (true, false)] {
            owner
                .catalog_updated(CatalogUpdate {
                    source_id: source_id.clone(),
                    source: cached.source,
                    artwork_digest: [0; 32],
                    metadata: false,
                    playlists: true,
                    artwork,
                    home,
                    inventory: false,
                    tracks_added: false,
                    song_rekeys: Arc::from([]),
                })
                .await;
            assert!(matches!(
                receiver.recv().await.unwrap(),
                SourceEvent::CatalogPublished(CatalogPublication {
                    change: CatalogChange::Broad,
                    ..
                })
            ));
        }
    }

    #[tokio::test]
    async fn adding_a_source_keeps_setup_progress_until_the_catalog_is_ready() {
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
        )
        .owner;
        owner.configure_source(SourceSetup::Local { roots: vec![music] });
        tokio::time::timeout(Duration::from_secs(10), async {
            let mut scanned = false;
            let mut selected = false;
            loop {
                match receiver.recv().await.unwrap() {
                    SourceEvent::Operation(SourceOperation::Adding { progress }) => {
                        scanned |= progress.stage == SourceProgressStage::Finalizing;
                    }
                    SourceEvent::Selected { .. } => {
                        assert!(scanned, "selection must follow the initial scan");
                        selected = true;
                    }
                    SourceEvent::Operation(SourceOperation::Idle) => {
                        assert!(
                            selected,
                            "setup finishes after the ready catalog is selected"
                        );
                        break;
                    }
                    SourceEvent::Operation(SourceOperation::Refreshing { .. }) => {
                        panic!("the initial scan belongs to setup, not background refresh");
                    }
                    SourceEvent::Operation(SourceOperation::Failed { message, .. }) => {
                        panic!("{message}")
                    }
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        owner.shared.cancel_observer();
        owner.shared.cancel_acquisition();
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
            let mut scan =
                library::Scan::begin(&database, source_id.as_str(), "Music", "music", None)
                    .await
                    .unwrap();
            scan.write_album(&library::AlbumInput {
                object_id: "album".into(),
                title: "Cached Album".into(),
                display_artist: "Artist".into(),
                sort_text: "cached album".into(),
                favorite: Some(false),
                ..Default::default()
            })
            .await
            .unwrap();
            let source_key = scan.source();
            scan.finish().await.unwrap();
            let settings = SettingsFile::memory();
            settings.update(|stored| {
            stored.sources.configured.push(ConfiguredSource {
                configuration: SourceConfiguration {
                    source_id: source_id.clone(),
                    kind: "subsonic".into(),
                    name: "Music".into(),
                    provider_payload: serde_json::json!({ "version": 1, "base_url": "http://127.0.0.1:9", "username": "listener", "trust_invalid_cert": false }).to_string(),
                },
                credential_ref: Some(crate::settings::CredentialRef::new("held")),
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
            )
            .owner;
            owner.shared.load_source_counts().await.unwrap();
            assert_eq!(
                owner.shared.configured_sources(None).local_access[0].album_count,
                1
            );
            let completion = owner.select_source(source_id.clone());
            let selected = tokio::time::timeout(Duration::from_secs(2), completion.recv())
                .await
                .expect("selection result must not wait for credentials")
                .unwrap()
                .unwrap();
            assert_eq!(selected.source_id, source_id);
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
                    stored.ui.external_metadata_enabled = false;
                    Ok(())
                })
                .unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                assert!(
                    crate::metadata::identify_metadata(
                        &owner,
                        "file:///music/song.flac".into(),
                        sources::SourceEntityKind::Track,
                        sources::MetadataChanges::new()
                    )
                    .recv()
                    .await
                    .unwrap()
                    .unwrap()
                    .is_none()
                );
                assert!(
                    crate::metadata::identify_metadata(
                        &owner,
                        library::source_entity_uri(&source_id, "album", "album"),
                        sources::SourceEntityKind::Album,
                        sources::MetadataChanges::new()
                    )
                    .recv()
                    .await
                    .unwrap()
                    .unwrap()
                    .is_none()
                );
                assert!(
                    crate::metadata::identify_metadata(
                        &owner,
                        library::source_entity_uri(&source_id, "artist", "artist"),
                        sources::SourceEntityKind::Artist,
                        sources::MetadataChanges::new()
                    )
                    .recv()
                    .await
                    .unwrap()
                    .unwrap()
                    .is_none()
                );
            })
            .await
            .expect("non-native identification does not acquire a provider");
            let mut scan =
                library::Scan::begin(&database, source_id.as_str(), "Music", "music", None)
                    .await
                    .unwrap();
            scan.incomplete();
            scan.write_folder(&library::FolderInput {
                object_id: "new-folder".into(),
                name: Some("New Folder".into()),
                sort_text: Some("new folder".into()),
                ..Default::default()
            })
            .await
            .unwrap();
            scan.write_tracks(&[(
                &library::TrackInput {
                    object_id: "new-track".into(),
                    album_object_id: Some("album".into()),
                    title: "New Track".into(),
                    display_album: "Cached Album".into(),
                    display_artist: "Artist".into(),
                    sort_text: "new track".into(),
                    duration_millis: 1_000,
                    disc_number: 1,
                    track_number: 1,
                    favorite: Some(false),
                    loudness_analysis_key: [0; 32],
                    ..Default::default()
                },
                None,
            )])
            .await
            .unwrap();
            scan.write_track_folders(&[library::ScanLink {
                owner_id: "new-track",
                related_id: "new-folder",
                position: 0,
            }])
            .await
            .unwrap();
            scan.finish().await.unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if let SourceEvent::CatalogPublished(CatalogPublication {
                        change: CatalogChange::Acquired,
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
                            credential_ref: None,
                            music_folder_id: None,
                            local_access: None,
                            enable_half_stars: false,
                        });
                        Ok(())
                    })
                    .unwrap();
                library::Scan::begin(&database, local_id.as_str(), "Local", "local", None)
                    .await
                    .unwrap()
                    .finish()
                    .await
                    .unwrap();
                owner.select_source(local_id.clone());
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
                let configured = owner.shared.configured_sources(None);
                assert_eq!(
                    (
                        configured.local_access[0].album_count,
                        configured.local_access[0].track_count
                    ),
                    (1, 1)
                );
                assert_eq!(configured.local_access[1].track_count, 0);
                let mut scan = library::Scan::begin_items(&database, source_id.as_str())
                    .await
                    .unwrap();
                scan.remove_track("new-track").await.unwrap();
                scan.finish().await.unwrap();
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
                            credential_ref: None,
                            music_folder_id: None,
                            local_access: None,
                            enable_half_stars: false,
                        });
                        Ok(())
                    })
                    .unwrap();
                library::Scan::begin(&database, id.as_str(), id.as_str(), "local", None)
                    .await
                    .unwrap()
                    .finish()
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
            )
            .owner;
            assert_eq!(owner.list_sources().sources.len(), 2);
            assert!(owner.selected_library().is_none());
            let missing = owner.select_source(SourceId::new("missing"));
            let error = tokio::time::timeout(Duration::from_secs(5), missing.recv())
                .await
                .unwrap()
                .unwrap()
                .err()
                .expect("unknown source returns an error to its caller");
            assert!(!error.is_empty());
            assert!(owner.selected_library().is_none());

            // Hold the operation lane so replacement happens before either request starts.
            let lane = owner.shared.lane.lock().await;
            let superseded = owner.select_source(second.clone());
            let initial = owner.select_source(first.clone());
            drop(lane);
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(5), superseded.recv())
                    .await
                    .unwrap()
                    .unwrap()
                    .err(),
                Some(SourceError::Cancelled.to_string()),
            );
            for id in [&first, &second] {
                let selection = if id == &first {
                    initial.clone()
                } else {
                    owner.select_source(id.clone())
                };
                let selected = tokio::time::timeout(Duration::from_secs(5), selection.recv())
                    .await
                    .expect("selection completes without consuming events")
                    .unwrap()
                    .unwrap();
                assert_eq!(&selected.source_id, id);
                assert_eq!(owner.list_sources().selected_source_id.as_ref(), Some(id));
                assert_eq!(owner.selected_library().unwrap().source_id, *id);
            }
            tokio::time::timeout(
                Duration::from_secs(5),
                owner.forget_source(second.clone()).recv(),
            )
            .await
            .expect("removal completes without consuming events")
            .unwrap()
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
            owner.shared.cancel_observer();
            owner.shared.cancel_acquisition();

            owner
                .forget_source(second.clone())
                .recv()
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                owner.list_sources().sources.len(),
                1,
                "removal can be repeated"
            );
            std::fs::remove_file(&settings_path).unwrap();
            std::fs::create_dir(&settings_path).unwrap();
            let error = tokio::time::timeout(
                Duration::from_secs(5),
                owner.forget_source(first.clone()).recv(),
            )
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
            assert!(!error.is_empty(), "failed settings writes reach the caller");
            assert_eq!(owner.list_sources().sources[0].id, first);
        }
    }

    #[tokio::test]
    async fn selected_snapshots_do_not_retain_removed_clients() {
        let directory = tempfile::tempdir().unwrap();
        let database = Arc::new(
            Database::open(directory.path().join("library.sqlite"))
                .await
                .unwrap(),
        );
        let runtime = tokio::runtime::Handle::current();
        let owner = SourceOwner::open_dormant(
            Artwork::new(directory.path().join("artwork"), runtime.clone()).unwrap(),
            Arc::clone(&database),
            Downloads::new(
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
                events: async_channel::unbounded().0,
                discovery: async_channel::unbounded().0,
            },
        )
        .owner;
        let mut snapshots = Vec::new();
        for index in 0..16 {
            let configuration = SourceConfiguration {
                source_id: SourceId::new(format!("instance-{index}")),
                kind: "subsonic".into(),
                name: "Music".into(),
                provider_payload:
                    serde_json::json!({ "version": 1, "base_url": "http://127.0.0.1:9",
                    "username": "listener", "trust_invalid_cert": false })
                    .to_string(),
            };
            let source = Arc::new(
                Source::open(configuration.clone(), Some("salt:token".into()), None).unwrap(),
            );
            let weak_source = Arc::downgrade(&source);
            let source_id = configuration.source_id.clone();
            owner.shared.clients.lock().unwrap().insert(
                source_id.clone(),
                CachedSource {
                    configuration,
                    credential_ref: None,
                    jellyfin_device_id: owner.shared.settings.load().jellyfin_device_id,
                    source,
                },
            );
            let selected = SelectedLibrary {
                source_id: source_id.clone(),
                source_key: SourceKey::from_raw(index + 1),
                artwork_digest: [0; 32],
                music_folder_key: None,
                music_folder_object_id: None,
                music_folders: Arc::from([]),
                local_access_status: LocalAccessStatus::default(),
            };
            *owner.shared.selected.lock().unwrap() = Some(selected.clone());
            snapshots.push(selected);
            owner.release_selected(false).await;
            owner.remove_source_resources(&source_id, None).await;
            assert!(weak_source.upgrade().is_none());
            assert!(owner.selected_library().is_none());
            assert_eq!(snapshots.last().unwrap().source_id, source_id);
        }
    }

    fn key(revision: u64) -> ArtworkPreparationKey {
        let mut digest = [0; 32];
        digest[..8].copy_from_slice(&revision.to_le_bytes());
        ArtworkPreparationKey {
            source: SourceKey::from_raw(1),
            digest,
        }
    }

    #[test]
    fn same_revision_is_deduplicated_and_completion_is_remembered() {
        let mut owner = ArtworkPreparationOwner::default();
        let (token, _) = owner.admit(key(1)).expect("first revision");
        assert!(owner.admit(key(1)).is_none());
        assert!(owner.complete(key(1), token));
        assert!(owner.admit(key(1)).is_none());
    }

    #[test]
    fn replacement_cancels_the_previous_revision_and_keeps_one_active() {
        let mut owner = ArtworkPreparationOwner::default();
        let (stale_token, stale_cancelled) = owner.admit(key(1)).expect("first revision");
        let (current_token, current_cancelled) = owner.admit(key(2)).expect("replacement");
        assert!(stale_cancelled.is_cancelled());
        assert!(!current_cancelled.is_cancelled());
        assert_eq!(
            owner.active.as_ref().map(|active| active.token),
            Some(current_token)
        );
        assert!(!owner.complete(key(1), stale_token));
        assert!(owner.complete(key(2), current_token));
    }

    #[test]
    fn release_cancels_active_artwork_preparation() {
        let mut owner = ArtworkPreparationOwner::default();
        let (_, cancelled) = owner.admit(key(1)).expect("active revision");
        owner.cancel_active();
        assert!(cancelled.is_cancelled());
        assert!(owner.active.is_none());
    }

    #[test]
    fn failed_artwork_revision_is_retryable() {
        let mut owner = ArtworkPreparationOwner::default();
        let (token, _) = owner.admit(key(1)).expect("active revision");
        assert!(owner.fail(key(1), token));
        assert!(owner.admit(key(1)).is_some());
    }

    #[test]
    fn accumulated_progress_is_monotonic_and_has_no_fake_total() {
        let first = artwork_preparation_progress(128);
        let second = artwork_preparation_progress(256);
        assert!(second.completed > first.completed);
        assert_eq!(first.stage, SourceProgressStage::Artwork);
        assert_eq!(second.total, None);
    }
}
