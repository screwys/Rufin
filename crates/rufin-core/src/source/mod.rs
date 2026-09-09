//! The configured sources and the one selected Database-backed source session.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::runtime::source::{
    ConfiguredSources, CredentialInput, CredentialPreset, DiscoveryStatus, DiscoveryUpdate,
    EditableSource, LocalAccessStatus, LocalFolder, OpenSubsonicKind, SourceLocalAccess,
    SourceLocalAccessSummary, SourceOperation, SourceProgress, SourceProgressStage,
    SourceSettingsChange, SourceSetup, SourceSummary,
};
use crate::runtime::{
    CatalogChange, CatalogPublication, FavoriteSettlement, SelectedLibrary, SourceEvent,
    SourceNotice, SourceNoticeKind,
};
use artwork::Artwork;
use async_channel::{Receiver, Sender};
use downloads::Downloads;
use library::{
    Database, FavoriteTarget, FolderKey, FolderRow, ReadCancellation, ScanOutcome, SourceKey,
};
use secrets::{SecretStorageMode, SwitchableSecretStore};
use sources::{
    CredentialHostInput, CredentialSettingsInput, JellyfinEmbySettingsInput,
    JellyfinEmbySetupInput, LiveFolderPage, LocalFolderHostInput, SelectedFeed, Source,
    SourceConfiguration, SourceEntityKind, SourceError, SourceId, SourceReadProgress,
    SourceReadStage, SourceSettingsInput, SourceSetupInput, SubsonicFlavor,
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
pub(crate) struct SelectedSourceState {
    pub(crate) configuration: SourceConfiguration,
    pub(crate) source: Option<Arc<Source>>,
    pub(crate) source_key: SourceKey,
    pub(crate) artwork_digest: [u8; 32],
    pub(crate) database: Arc<Database>,
    pub(crate) runtime: tokio::runtime::Handle,
    pub(crate) music_folder_key: Option<FolderKey>,
    pub(crate) music_folder_object_id: Option<String>,
    pub(crate) music_folders: Arc<[FolderRow]>,
    pub(crate) track_count: usize,
    pub(crate) formula_match_count: usize,
    pub(crate) sample_source_path: Option<String>,
}

impl SelectedSourceState {
    pub(crate) fn source_id(&self) -> &SourceId {
        &self.configuration.source_id
    }
}

pub struct ActiveSource {
    shared: Weak<Shared>,
    current: Mutex<Option<Arc<SelectedSourceState>>>,
    retirement: tokio::sync::watch::Sender<bool>,
}

pub(crate) type WeakActiveSource = Weak<ActiveSource>;

impl ActiveSource {
    fn new(shared: &Arc<Shared>, current: Arc<SelectedSourceState>) -> Arc<Self> {
        let (retirement, _) = tokio::sync::watch::channel(false);
        Arc::new(Self {
            shared: Arc::downgrade(shared),
            current: Mutex::new(Some(current)),
            retirement,
        })
    }

    pub(crate) fn resolve(&self) -> Option<Arc<SelectedSourceState>> {
        if *self.retirement.borrow() {
            return None;
        }
        self.current
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn replace(&self, current: Arc<SelectedSourceState>) {
        let mut slot = self
            .current
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if slot.is_some() {
            *slot = Some(current);
        }
    }

    fn update(
        &self,
        source: SourceKey,
        change: impl FnOnce(&mut SelectedSourceState),
    ) -> Option<Arc<SelectedSourceState>> {
        let mut current = self
            .current
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let selected = current.as_ref()?;
        if selected.source_key != source {
            return None;
        }
        let mut replacement = (**selected).clone();
        change(&mut replacement);
        let replacement = Arc::new(replacement);
        *current = Some(Arc::clone(&replacement));
        Some(replacement)
    }

    pub(crate) fn downgrade(self: &Arc<Self>) -> WeakActiveSource {
        Arc::downgrade(self)
    }

    fn retire(&self) {
        self.retirement.send_replace(true);
        self.current
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
    }

    fn spawn_reply<T, F, Work>(&self, work: F) -> Receiver<T>
    where
        T: Send + 'static,
        F: FnOnce(Arc<SelectedSourceState>) -> Work + Send + 'static,
        Work: Future<Output = T> + Send + 'static,
    {
        let (sender, receiver) = async_channel::bounded(1);
        let Some(shared) = self.shared.upgrade() else {
            return receiver;
        };
        let Some(selected) = self.resolve() else {
            return receiver;
        };
        let mut retired = self.retirement.subscribe();
        let runtime = shared.runtime.clone();
        runtime.spawn(async move {
            if *retired.borrow() {
                return;
            }
            let value = tokio::select! {
                value = work(selected) => value,
                _ = retired.changed() => return,
            };
            if !*retired.borrow() {
                let _ = sender.send(value).await;
            }
        });
        receiver
    }

    fn spawn_selected<F, Work>(&self, work: F)
    where
        F: FnOnce(SourceOwner, Arc<SelectedSourceState>) -> Work + Send + 'static,
        Work: Future<Output = ()> + Send + 'static,
    {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        let Some(selected) = self.resolve() else {
            return;
        };
        let mut retired = self.retirement.subscribe();
        let runtime = shared.runtime.clone();
        runtime.spawn(async move {
            if *retired.borrow() {
                return;
            }
            tokio::select! {
                _ = async move {
                    let lane_owner = Arc::clone(&shared);
                    let _lane = lane_owner.lane.lock().await;
                    work(SourceOwner { shared }, selected).await;
                } => {},
                _ = retired.changed() => {},
            }
        });
    }
}

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ArtworkPreparationKey {
    source: SourceKey,
    digest: [u8; 32],
}

struct ActiveArtworkPreparation {
    key: ArtworkPreparationKey,
    token: u64,
    cancelled: Arc<AtomicBool>,
    abort: Option<tokio::task::AbortHandle>,
}

#[derive(Default)]
struct ArtworkPreparationOwner {
    active: Option<ActiveArtworkPreparation>,
    completed: Option<ArtworkPreparationKey>,
    next_token: u64,
}

impl ArtworkPreparationOwner {
    fn admit(&mut self, key: ArtworkPreparationKey) -> Option<(u64, Arc<AtomicBool>)> {
        if self.completed == Some(key)
            || self.active.as_ref().is_some_and(|active| active.key == key)
        {
            return None;
        }
        self.cancel_active();
        self.next_token = self.next_token.wrapping_add(1).max(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        self.active = Some(ActiveArtworkPreparation {
            key,
            token: self.next_token,
            cancelled: Arc::clone(&cancelled),
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
            active.key == key && active.token == token && !active.cancelled.load(Ordering::Acquire)
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
            active.cancelled.store(true, Ordering::Release);
            if let Some(abort) = active.abort {
                abort.abort();
            }
        }
    }
}

pub(crate) struct Shared {
    artwork: Artwork,
    pub(crate) database: Arc<Database>,
    downloads: Downloads,
    pub(crate) settings: SettingsFile,
    secrets: Arc<SwitchableSecretStore>,
    plex_logins: Mutex<HashMap<String, Arc<tokio::sync::Mutex<sources::PlexLogin>>>>,
    runtime: tokio::runtime::Handle,
    outputs: SourceOutputs,
    selected: Mutex<Option<Arc<ActiveSource>>>,
    catalog_counts: Mutex<HashMap<SourceId, (usize, usize)>>,
    observer: Mutex<Option<Arc<SelectedFeed>>>,
    acquisition: Mutex<Weak<AtomicBool>>,
    artwork_preparation: Mutex<ArtworkPreparationOwner>,
    pub(crate) lane: tokio::sync::Mutex<()>,
    playback: Mutex<Weak<PlaybackOwner>>,
    started: AtomicBool,
}

impl Shared {
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
        selected: Option<&SelectedSourceState>,
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

    pub(crate) fn selected(&self) -> Option<Arc<SelectedSourceState>> {
        self.selected_session()
            .and_then(|session| session.resolve())
    }

    fn selected_session(&self) -> Option<Arc<ActiveSource>> {
        self.selected
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .cloned()
    }

    pub(crate) async fn send(&self, event: SourceEvent) {
        let _ = self.outputs.events.send(event).await;
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
        let shared = Arc::new(Shared {
            artwork,
            database,
            downloads,
            settings,
            secrets,
            plex_logins: Mutex::new(HashMap::new()),
            runtime,
            outputs,
            selected: Mutex::new(None),
            catalog_counts: Mutex::new(HashMap::new()),
            observer: Mutex::new(None),
            acquisition: Mutex::new(Weak::new()),
            artwork_preparation: Mutex::new(ArtworkPreparationOwner::default()),
            lane: tokio::sync::Mutex::new(()),
            playback: Mutex::new(Weak::new()),
            started: AtomicBool::new(false),
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
        let owner = self.clone();
        self.shared.runtime.spawn(async move {
            let mut interval = tokio::time::interval(SOURCE_CHECK_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            interval.tick().await;
            loop {
                interval.tick().await;
                owner.check_remote_freshness().await;
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
        let result = restore().await;
        if result.is_ok()
            && let Err(error) = self.shared.load_source_counts().await
        {
            self.shared.warn_nonfatal(&error);
        }
        if result.is_ok() && setup {
            self.release_selected(true).await;
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
                    .configured_sources(self.shared.selected().as_deref()),
            ))
            .await;
        self.publish_operation(SourceOperation::Idle).await;
        result
    }

    async fn remove_source_resources(
        &self,
        source_id: &SourceId,
        credential_ref: Option<crate::settings::CredentialRef>,
    ) {
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
        self.shared.send(SourceEvent::Operation(operation)).await;
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
        let cached = self
            .shared
            .database
            .cached_source(source_id.as_str(), &ReadCancellation::new())
            .await
            .map_err(string_error)?;
        let cached_start = cached.is_some();
        let publication = self
            .shared
            .database
            .reconcile_source(&source_id)
            .await
            .map_err(string_error)?;
        if cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        self.install_selected(
            configured.clone(),
            None,
            publication,
            false,
            Arc::clone(&cancelled),
        )
        .await?;
        if !self.shared.acquisition_is_current(&cancelled) {
            return Ok(());
        }
        let Some(session) = self.shared.selected_session() else {
            return Ok(());
        };
        let secrets = Arc::clone(&self.shared.secrets);
        let device = self.shared.settings.load().jellyfin_device_id;
        let owner = self.clone();
        self.shared.runtime.spawn(async move {
            let opener = owner.clone();
            let opened = owner
                .shared
                .runtime
                .spawn_blocking(move || {
                    let credential = configured
                        .credential_ref
                        .as_ref()
                        .map(|reference| load_provider_secret(&secrets, reference))
                        .transpose()?
                        .flatten();
                    let mut source =
                        Source::open(configured.configuration.clone(), credential, Some(device))
                            .map_err(string_error)?;
                    opener.bind_plex_login(&configured, &mut source)?;
                    Ok(Arc::new(source))
                })
                .await
                .map_err(string_error)
                .and_then(|opened| opened);
            let _lane = owner.shared.lane.lock().await;
            if !owner.shared.acquisition_is_current(&cancelled) || session.resolve().is_none() {
                return;
            }
            let source = match opened {
                Ok(source) => source,
                Err(error) => {
                    owner.shared.warn_nonfatal(&error);
                    return;
                }
            };
            let Some(selected) = session.update(publication.source, |selected| {
                selected.source = Some(source);
            }) else {
                return;
            };
            if let Err(error) = owner
                .shared
                .downloads
                .attach(
                    selected.source_id().clone(),
                    selected.source_key,
                    selected.source.clone(),
                    selected.music_folder_key,
                )
                .await
            {
                owner.shared.warn_nonfatal(&error);
            }
            owner.start_observer(session, Arc::clone(&selected), cached_start);
            if !cached_start {
                owner
                    .manual_refresh_selected(&selected, "cold-select", cancelled)
                    .await;
            }
        });
        Ok(())
    }

    async fn selected_state(
        &self,
        configured: ConfiguredSource,
        source: Option<Arc<Source>>,
        publication: library::Publication,
    ) -> Result<Arc<SelectedSourceState>, String> {
        let cancellation = ReadCancellation::new();
        let folder_order = self
            .shared
            .database
            .folder_child_order(publication.source, None, &cancellation)
            .await
            .map_err(string_error)?;
        let folders: Arc<[FolderRow]> = self
            .shared
            .database
            .folder_rows(publication.source, &folder_order, &cancellation)
            .await
            .map_err(string_error)?
            .into();
        let requested_folder = configured.music_folder_id.clone();
        let folder_key = match requested_folder.as_deref() {
            Some(object_id) => self
                .shared
                .database
                .folder_key_by_object(publication.source, object_id, &cancellation)
                .await
                .map_err(string_error)?,
            None => None,
        };
        let (_, track_count) = self
            .shared
            .refresh_source_counts(&configured.configuration.source_id, publication.source)
            .await?;
        let formula_match_count = match configured.local_access.as_ref() {
            Some(access) => self
                .shared
                .database
                .mapping_formula_match_count(
                    publication.source,
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
            .mapping_track_page(publication.source, None, None, 1, &cancellation)
            .await
            .map_err(string_error)?
            .pop()
            .map(|track| track.source_path);
        Ok(Arc::new(SelectedSourceState {
            configuration: configured.configuration.clone(),
            source,
            source_key: publication.source,
            artwork_digest: publication.artwork_digest,
            database: Arc::clone(&self.shared.database),
            runtime: self.shared.runtime.clone(),
            music_folder_key: folder_key,
            music_folder_object_id: requested_folder,
            music_folders: folders,
            track_count,
            formula_match_count,
            sample_source_path,
        }))
    }

    async fn install_selected(
        &self,
        configured: ConfiguredSource,
        source: Option<Arc<Source>>,
        publication: library::Publication,
        catch_up: bool,
        acquisition: Arc<AtomicBool>,
    ) -> Result<(), String> {
        let selected = self.selected_state(configured, source, publication).await?;
        let session = ActiveSource::new(&self.shared, Arc::clone(&selected));
        if !self.shared.acquisition_is_current(&acquisition) {
            return Ok(());
        }
        self.release_selected(false).await;
        self.shared
            .downloads
            .attach(
                selected.source_id().clone(),
                selected.source_key,
                selected.source.clone(),
                selected.music_folder_key,
            )
            .await?;
        *self
            .shared
            .selected
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(Arc::clone(&session));
        self.shared.settings.update(|stored| {
            stored.sources.selected_source_id = Some(selected.source_id().clone());
            Ok(())
        })?;
        self.shared
            .send(SourceEvent::Selected {
                configured: self.shared.configured_sources(Some(&selected)),
                selected: ui_selected(Arc::clone(&selected), Arc::clone(&session)),
            })
            .await;
        self.publish_operation(SourceOperation::Idle).await;
        self.start_observer(session, selected, catch_up);
        Ok(())
    }

    async fn release_selected(&self, cancel_acquisition: bool) {
        if cancel_acquisition {
            self.shared.cancel_acquisition();
        }
        self.shared.cancel_observer();
        self.shared
            .artwork_preparation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .cancel_active();
        let slot = self
            .shared
            .selected
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        if let Some(slot) = slot {
            slot.retire();
            self.shared.send(SourceEvent::ReleaseSelected).await;
        }
    }

    fn start_observer(
        &self,
        session: Arc<ActiveSource>,
        selected: Arc<SelectedSourceState>,
        catch_up: bool,
    ) {
        let Some(source) = selected.source.clone() else {
            return;
        };
        let Some(observer) = source.start_selected_feed(&self.shared.runtime) else {
            return;
        };
        self.shared
            .observer
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .replace(Arc::clone(&observer));
        let consumer = self.clone();
        let consumer_session = Arc::clone(&session);
        let consumer_observer = Arc::clone(&observer);
        self.shared.runtime.spawn(async move {
            consumer
                .consume_selected_feed(consumer_session, consumer_observer)
                .await;
        });
        if catch_up && selected.configuration.is_local() {
            let owner = self.clone();
            let cancelled = observer.cancellation();
            self.shared.runtime.spawn(async move {
                owner.catch_up_local(session, cancelled).await;
            });
        }
    }

    async fn consume_selected_feed(&self, session: Arc<ActiveSource>, observer: Arc<SelectedFeed>) {
        while observer.wait_for_change().await {
            let _lane = self.shared.lane.lock().await;
            let Some(selected) = session.resolve() else {
                return;
            };
            match observer
                .apply_pending(&selected.database, selected.source_key)
                .await
            {
                Ok(Some(outcome)) => {
                    self.accept_scan(selected.source_id(), outcome, CatalogChange::Broad)
                        .await;
                    continue;
                }
                Ok(None) => {}
                Err(SourceError::Cancelled) => return,
                Err(error) => {
                    warn!(%error, "bounded selected-source change failed; reacquiring after feed boundary loss");
                }
            }
            observer.cancel();
            let acquisition = self.shared.begin_acquisition();
            self.manual_refresh_selected(&selected, "selected-feed-gap", Arc::clone(&acquisition))
                .await;
            if !self.shared.acquisition_is_current(&acquisition) {
                return;
            }
            if let Some(selected) = session.resolve() {
                self.start_observer(Arc::clone(&session), selected, false);
            }
            return;
        }
    }

    async fn catch_up_local(&self, session: Arc<ActiveSource>, cancelled: Arc<AtomicBool>) {
        let _lane = self.shared.lane.lock().await;
        let Some(selected) = session.resolve() else {
            return;
        };
        let Some(source) = selected.source.as_ref() else {
            return;
        };
        let progress = |_: SourceReadProgress| {};
        let result = source
            .catch_up_local(
                &selected.database,
                selected.source_key,
                &progress,
                Arc::clone(&cancelled),
            )
            .await;
        if !cancelled.load(Ordering::Relaxed) {
            self.finish_refresh(selected.source_id(), result).await;
        }
    }

    async fn manual_refresh_selected(
        &self,
        selected: &SelectedSourceState,
        trigger: &'static str,
        acquisition: Arc<AtomicBool>,
    ) {
        if !self.shared.acquisition_is_current(&acquisition) {
            return;
        }
        info!(trigger, source_key = %selected.source_key, "starting explicit source acquisition");
        let Some(source) = selected.source.as_ref() else {
            self.shared.warn_nonfatal(&source_access_unavailable());
            return;
        };
        self.publish_operation(SourceOperation::Refreshing {
            source_id: selected.source_id().clone(),
            progress: initial_progress(),
        })
        .await;
        let progress = refreshing_progress(
            self.shared.outputs.events.clone(),
            selected.source_id().clone(),
        );
        let outcome = source
            .manual_refresh(
                &selected.database,
                &selected.configuration.name,
                &progress,
                Arc::clone(&acquisition),
            )
            .await;
        if self.shared.acquisition_is_current(&acquisition) {
            self.finish_refresh(selected.source_id(), outcome).await;
        }
    }

    async fn finish_refresh(
        &self,
        source_id: &SourceId,
        result: sources::SourceResult<ScanOutcome>,
    ) {
        if let Ok(outcome) | Err(SourceError::IncompleteScan { outcome, .. }) = &result {
            self.accept_scan(source_id, *outcome, CatalogChange::Acquired)
                .await;
        }
        let operation = match result {
            Ok(_) | Err(SourceError::Cancelled) => SourceOperation::Idle,
            Err(error) => SourceOperation::Failed {
                source_id: Some(source_id.clone()),
                message: error.to_string(),
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
            self.persist_connected_source(&replacement, credential)?;
            self.bind_plex_login(&replacement, &mut source)?;
            let source = Arc::new(source);
            let publication = self
                .shared
                .database
                .reconcile_source(&replacement.configuration.source_id)
                .await
                .map_err(string_error)?;
            let selected = self.shared.selected().is_some_and(|selected| {
                selected.source_id() == &replacement.configuration.source_id
            });
            if selected {
                self.install_selected(
                    replacement,
                    Some(Arc::clone(&source)),
                    publication,
                    false,
                    Arc::clone(&cancelled),
                )
                .await?;
                if let Some(selected) = self.shared.selected() {
                    self.manual_refresh_selected(&selected, "source-edit", Arc::clone(&cancelled))
                        .await;
                }
            } else {
                if replacement.configuration.is_local() {
                    let outcome = source
                        .manual_refresh(
                            &self.shared.database,
                            &replacement.configuration.name,
                            &|_| {},
                            Arc::clone(&cancelled),
                        )
                        .await;
                    if let Err(SourceError::IncompleteScan { outcome, .. }) = &outcome {
                        self.accept_scan(
                            &replacement.configuration.source_id,
                            *outcome,
                            CatalogChange::Broad,
                        )
                        .await;
                    }
                    let outcome = outcome.map_err(string_error)?;
                    self.accept_scan(
                        &replacement.configuration.source_id,
                        outcome,
                        CatalogChange::Broad,
                    )
                    .await;
                }
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
        })
    }

    async fn edit_configured_source(
        &self,
        source_id: SourceId,
        input: SourceSettingsInput,
        cancelled: Arc<AtomicBool>,
    ) -> Result<(), String> {
        if cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
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
                    .configured_sources(self.shared.selected().as_deref()),
            ))
            .await;
        Ok(())
    }

    pub(crate) async fn accept_scan(
        &self,
        source_id: &SourceId,
        outcome: ScanOutcome,
        change: CatalogChange,
    ) {
        tracing::debug!(?outcome, ?change, "accepting source scan result");
        let refresh_counts = matches!(outcome, ScanOutcome::Changed(_));
        let refresh_summary = refresh_counts && change == CatalogChange::Acquired;
        let (publication, catalog_changed, change) = match outcome {
            ScanOutcome::Changed(publication) => (publication, true, change),
            ScanOutcome::PlaylistsChanged(publication) => {
                let change = match change {
                    CatalogChange::Playlists(_) => change,
                    _ => CatalogChange::Playlists(None),
                };
                (publication, true, change)
            }
            ScanOutcome::ArtworkChanged(publication) => (publication, false, change),
            ScanOutcome::Identical(_) | ScanOutcome::Stale | ScanOutcome::Failed => return,
        };
        let mut event = SourceEvent::CatalogPublished(CatalogPublication {
            source_key: Some(publication.source),
            favorite: None,
            change,
        });
        if let Some(session) = self.shared.selected_session() {
            session.update(publication.source, |current| {
                current.artwork_digest = publication.artwork_digest;
            });
            if refresh_summary
                && let Some(selected) = session.resolve()
                && selected.source_key == publication.source
                && let Ok(configured) =
                    configured_source(&self.shared.settings.load().sources, source_id)
            {
                match self
                    .selected_state(configured, selected.source.clone(), publication)
                    .await
                {
                    Ok(selected) => {
                        session.replace(selected);
                        if let Some(selected) = session.resolve() {
                            event = SourceEvent::CatalogReplaced {
                                configured: self.shared.configured_sources(Some(&selected)),
                                selected: ui_selected(selected, session.clone()),
                            };
                        }
                    }
                    Err(error) => self.shared.warn_nonfatal(&error),
                }
            }
        }
        if catalog_changed {
            self.shared.downloads.library_changed(source_id.clone());
        }
        if let Ok(playback) = self.shared.playback() {
            playback.catalog_changed();
        }
        if refresh_counts && matches!(event, SourceEvent::CatalogPublished(_)) {
            if let Err(error) = self
                .shared
                .refresh_source_counts(source_id, publication.source)
                .await
            {
                self.shared.warn_nonfatal(&error);
            }
            let configured = self
                .shared
                .configured_sources(self.shared.selected().as_deref());
            self.shared.send(SourceEvent::Configured(configured)).await;
        }
        self.shared.send(event).await;
        if let Some(session) = self.shared.selected_session()
            && session
                .resolve()
                .is_some_and(|selected| selected.source_key == publication.source)
        {
            self.start_artwork_preparation(session);
        }
    }

    async fn check_remote_freshness(&self) {
        let _lane = self.shared.lane.lock().await;
        let Some(selected) = self.shared.selected() else {
            return;
        };
        if selected.configuration.is_local() {
            return;
        }
        let Some(source) = selected.source.as_ref() else {
            return;
        };
        let Some(acquisition) = self.shared.try_begin_acquisition() else {
            return;
        };
        let progressed = Arc::new(AtomicBool::new(false));
        let progress_started = Arc::clone(&progressed);
        let publish = refreshing_progress(
            self.shared.outputs.events.clone(),
            selected.source_id().clone(),
        );
        let progress = move |value: SourceReadProgress| {
            progress_started.store(true, Ordering::Release);
            publish(value);
        };
        let result = source
            .refresh_if_needed(
                &selected.database,
                &selected.configuration.name,
                &progress,
                Arc::clone(&acquisition),
            )
            .await;
        if self.shared.acquisition_is_current(&acquisition) {
            match result {
                Ok(Some(outcome)) => self.finish_refresh(selected.source_id(), Ok(outcome)).await,
                Err(error) => self.finish_refresh(selected.source_id(), Err(error)).await,
                Ok(None) if progressed.load(Ordering::Acquire) => {
                    self.publish_operation(SourceOperation::Idle).await
                }
                Ok(None) => {}
            }
        }
    }

    async fn publish_mapping_count(
        &self,
        selected: &SelectedSourceState,
        formula_match_count: usize,
    ) -> Result<(), String> {
        let current = {
            let Some(session) = self.shared.selected_session() else {
                return Ok(());
            };
            let Some(current) = session.update(selected.source_key, |current| {
                current.formula_match_count = formula_match_count;
            }) else {
                return Ok(());
            };
            current
        };
        self.shared.playback()?.stream_inputs_changed()?;
        self.shared
            .send(SourceEvent::Configured(
                self.shared.configured_sources(Some(&current)),
            ))
            .await;
        Ok(())
    }

    async fn publish_catalog(
        &self,
        selected: &SelectedSourceState,
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

    pub fn nextcloud_login(
        &self,
        settings: sources::FileSourceSettings,
        credentials: sources::FileCredentials,
    ) -> Receiver<Result<crate::runtime::source::NextcloudLoginEvent, String>> {
        use crate::runtime::source::NextcloudLoginEvent;
        let (send, receive) = async_channel::bounded(2);
        self.shared.runtime.spawn(async move {
            let authorization = sources::authorize_nextcloud(settings, credentials, |url| {
                let _ = send.try_send(Ok(NextcloudLoginEvent::OpenBrowser(url.to_string())));
            });
            tokio::select! {
                _ = send.closed() => {},
                result = authorization => {
                    let _ = send.send(result.map(|(settings, credentials)| NextcloudLoginEvent::Authorized { settings, credentials }).map_err(string_error)).await;
                }
            }
        });
        receive
    }

    pub fn emby_connect_login(
        &self,
        method: crate::runtime::source::EmbyConnectLoginMethod,
    ) -> Receiver<Result<crate::runtime::source::EmbyConnectLoginEvent, String>> {
        use crate::runtime::source::{EmbyConnectLoginEvent, EmbyConnectLoginMethod};
        let (send, receive) = async_channel::bounded(2);
        let device_id = self.shared.settings.load().jellyfin_device_id;
        self.shared.runtime.spawn(async move {
            let authorization = async {
                let login = match method {
                    EmbyConnectLoginMethod::Password { username, password } => sources::EmbyConnectLogin::password(&username, &password).await?,
                    EmbyConnectLoginMethod::Pin => {
                        let pin = sources::EmbyConnectPin::start(device_id).await?;
                        send.send(Ok(EmbyConnectLoginEvent::Code { code: pin.code.clone(), url: pin.approval_url().into() }))
                            .await.map_err(|_| SourceError::Cancelled)?;
                        loop {
                            if let Some(login) = pin.poll().await? { break login; }
                            tokio::time::sleep(Duration::from_secs(2)).await;
                        }
                    }
                };
                login.servers().await
            };
            tokio::select! {
                _ = send.closed() => {},
                result = authorization => {
                    let _ = send.send(result.map(EmbyConnectLoginEvent::Servers).map_err(string_error)).await;
                }
            }
        });
        receive
    }

    pub fn jellyfin_quick_connect(
        &self,
        server_url: String,
        trust_invalid_cert: bool,
    ) -> Receiver<Result<crate::runtime::source::JellyfinQuickConnectEvent, String>> {
        use crate::runtime::source::JellyfinQuickConnectEvent;
        let (send, receive) = async_channel::bounded(2);
        let device_id = self.shared.settings.load().jellyfin_device_id;
        self.shared.runtime.spawn(async move {
            let authorization = async {
                let quick = sources::JellyfinQuickConnect::start(server_url, trust_invalid_cert, device_id).await?;
                send.send(Ok(JellyfinQuickConnectEvent::Code { code: quick.code.clone(), url: quick.approval_url.clone() }))
                    .await.map_err(|_| SourceError::Cancelled)?;
                loop {
                    if let Some(login) = quick.poll().await? { break Ok::<_, SourceError>(login); }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            };
            tokio::select! {
                _ = send.closed() => {},
                result = authorization => {
                    let _ = send.send(result.map(JellyfinQuickConnectEvent::Authorized).map_err(string_error)).await;
                }
            }
        });
        receive
    }

    pub fn plex_login(
        &self,
        method: crate::runtime::source::PlexLoginMethod,
    ) -> Receiver<Result<crate::runtime::source::PlexLoginEvent, String>> {
        use crate::runtime::source::{PlexLoginEvent, PlexLoginMethod};
        let (send, receive) = async_channel::bounded(2);
        self.shared.runtime.spawn(async move {
            let authorization = async {
                let client_id = fresh_source_id().map_err(SourceError::Other)?.as_str().to_owned();
                match method {
                    PlexLoginMethod::Password { username, password, verification_code } => {
                        sources::PlexLogin::password(client_id, &username, &password, verification_code.as_deref()).await
                    }
                    PlexLoginMethod::Browser => {
                        let mut login = sources::PlexLogin::browser(client_id).await?;
                        send.send(Ok(PlexLoginEvent::OpenBrowser(login.url().to_owned())))
                            .await.map_err(|_| SourceError::Cancelled)?;
                        let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
                        loop {
                            if let Some(login) = login.poll().await? { break Ok(login); }
                            if tokio::time::Instant::now() >= deadline {
                                break Err(SourceError::Auth("Plex browser login expired".into()));
                            }
                            tokio::time::sleep(Duration::from_secs(1)).await;
                        }
                    }
                }
            };
            tokio::select! {
                _ = send.closed() => {},
                result = authorization => {
                    let _ = send.send(result.map(|login| PlexLoginEvent::Authorized(Box::new(login))).map_err(string_error)).await;
                }
            }
        });
        receive
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
        input: &mut SourceSetupInput,
    ) -> Result<Option<crate::settings::CredentialRef>, String> {
        let SourceSetupInput::Plex(input) = input else {
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
            owner
                .media_client(&media_uri)
                .await?
                .prepare_collection(&database, &media_uri)
                .await
                .map_err(string_error)
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
                .map_err(string_error)
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

    pub fn configure_source(&self, input: SourceSetup) {
        let cancelled = self.shared.begin_acquisition();
        self.shared.cancel_observer();
        self.spawn_serialized(move |owner| async move {
            if cancelled.load(Ordering::Acquire) {
                return;
            }
            owner
                .publish_operation(SourceOperation::Adding {
                    progress: initial_progress(),
                })
                .await;
            let mut input =
                source_setup_input(input, &owner.shared.settings.load().jellyfin_device_id);
            let mut persisted_source_id = None;
            let result = async {
                let reused_credential = owner.reuse_plex_login(&mut input).await?;
                let connected = Source::connect(fresh_source_id()?, input)
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
                owner.bind_plex_login(&configured, &mut source)?;
                let source = Arc::new(source);
                persisted_source_id = Some(configuration.source_id.clone());
                owner
                    .shared
                    .send(SourceEvent::Configured(
                        owner
                            .shared
                            .configured_sources(owner.shared.selected().as_deref()),
                    ))
                    .await;
                let events = owner.shared.outputs.events.clone();
                let progress = move |value| {
                    let _ = events.try_send(SourceEvent::Operation(SourceOperation::Adding {
                        progress: source_progress(value),
                    }));
                };
                let outcome = source
                    .manual_refresh(
                        &owner.shared.database,
                        &configuration.name,
                        &progress,
                        Arc::clone(&cancelled),
                    )
                    .await;
                let (outcome, scan_error) = match outcome {
                    Ok(outcome) => (outcome, None),
                    Err(SourceError::IncompleteScan { outcome, error }) => {
                        (outcome, Some(error.to_string()))
                    }
                    Err(error) => return Err(error.to_string()),
                };
                if !owner.shared.acquisition_is_current(&cancelled) {
                    return Ok(());
                }
                let publication = match outcome {
                    ScanOutcome::Changed(publication)
                    | ScanOutcome::PlaylistsChanged(publication)
                    | ScanOutcome::ArtworkChanged(publication)
                    | ScanOutcome::Identical(publication) => publication,
                    ScanOutcome::Stale | ScanOutcome::Failed => {
                        return Err(scan_error.unwrap_or_else(|| {
                            "The initial library scan did not complete".to_string()
                        }));
                    }
                };
                owner
                    .install_selected(
                        configured,
                        Some(Arc::clone(&source)),
                        publication,
                        false,
                        Arc::clone(&cancelled),
                    )
                    .await?;
                owner
                    .accept_scan(&configuration.source_id, outcome, CatalogChange::Acquired)
                    .await;
                if let Some(error) = scan_error {
                    return Err(error);
                }
                Ok(())
            }
            .await;
            if let Err(error) = result
                && owner.shared.acquisition_is_current(&cancelled)
            {
                if let Some(session) = owner.shared.selected_session()
                    && let Some(selected) = session.resolve()
                {
                    owner.start_observer(session, selected, false);
                }
                owner
                    .publish_operation(SourceOperation::Failed {
                        source_id: persisted_source_id,
                        message: error,
                        add_form: true,
                    })
                    .await;
            }
        });
    }

    pub fn update_source(&self, input: SourceSettingsChange) {
        let cancelled = self.shared.begin_acquisition();
        let source_id = source_settings_id(&input).clone();
        let input = source_settings_input(input);
        self.spawn_serialized(move |owner| async move {
            if let Err(error) = owner
                .edit_configured_source(source_id, input, cancelled)
                .await
            {
                owner.shared.warn_nonfatal(&error);
            }
        });
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
                        .configured_sources(owner.shared.selected().as_deref()),
                ))
                .await;
        });
    }

    pub fn select_source(&self, source_id: SourceId) {
        let cancelled = self.shared.begin_acquisition();
        self.shared.cancel_observer();
        self.spawn_serialized(move |owner| async move {
            if cancelled.load(Ordering::Acquire) {
                return;
            }
            owner
                .publish_operation(SourceOperation::Switching {
                    target: source_id.clone(),
                    progress: initial_progress(),
                })
                .await;
            if let Err(error) = owner
                .select_now(source_id.clone(), Arc::clone(&cancelled))
                .await
                && !cancelled.load(Ordering::Acquire)
            {
                if let Some(session) = owner.shared.selected_session()
                    && let Some(selected) = session.resolve()
                {
                    owner.start_observer(session, selected, false);
                }
                owner
                    .publish_operation(SourceOperation::Failed {
                        source_id: Some(source_id),
                        message: error,
                        add_form: false,
                    })
                    .await;
            }
        });
    }

    pub fn change_secret_storage(&self, mode: SecretStorageMode) -> Receiver<Result<(), String>> {
        let (sender, receiver) = async_channel::bounded(1);
        let owner = self.clone();
        self.shared.cancel_acquisition();
        self.shared.runtime.spawn(async move {
            let previous = owner.shared.settings.load();
            let changing = previous.ui.secret_storage_mode != mode;
            let result = if previous.ui.secret_storage_mode == mode {
                Ok(())
            } else {
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
                        for source in &mut stored.sources.configured {
                            source.credential_ref = None;
                        }
                        Ok(())
                    })?;
                    secrets.replace(platform_secret_store(&settings));
                    Ok(())
                })
                .await
                .map_err(string_error)
                .and_then(|result| result)
            };
            let changed = result.is_ok() && changing;
            let _ = sender.send(result).await;
            if changed {
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

    pub fn add_local_folder(&self, path: PathBuf) {
        edit_local_roots(self, move |roots| {
            roots.push(path);
        });
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

    pub fn refresh_source(&self, source_id: SourceId) {
        let acquisition = self.shared.begin_acquisition();
        let owner = self.clone();
        self.spawn_serialized(move |_| async move {
            if acquisition.load(Ordering::Acquire) {
                return;
            }
            if let Some(selected) = owner
                .shared
                .selected()
                .filter(|selected| selected.source_id() == &source_id)
            {
                owner
                    .manual_refresh_selected(&selected, "source-preferences", acquisition)
                    .await;
            }
        });
    }

    pub fn save_local_access(
        &self,
        input: SourceLocalAccess,
        wait_until_mapped: bool,
    ) -> Receiver<Result<(), String>> {
        let (sender, receiver) = async_channel::bounded(1);
        let cancelled = self.shared.begin_acquisition();
        let owner = self.clone();
        self.shared.runtime.spawn(async move {
            let result = async {
                let selected = owner
                    .shared
                    .selected()
                    .filter(|selected| selected.source_id() == &input.source_id)
                    .ok_or_else(|| "the mapped source is not selected".to_string())?;
                let source = selected
                    .source
                    .as_ref()
                    .ok_or_else(source_access_unavailable)?;
                let root_path = source
                    .apply_local_mapping(
                        &selected.database,
                        selected.source_key,
                        &input.root_path,
                        input.server_prefix.as_deref(),
                        input.local_prefix.as_deref(),
                        input.sample_source_path.as_deref(),
                        Arc::clone(&cancelled),
                    )
                    .await
                    .map_err(string_error)?;
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
                let formula_match_count = selected
                    .database
                    .mapping_formula_match_count(
                        selected.source_key,
                        root_path.to_string_lossy().as_ref(),
                        input.server_prefix.as_deref(),
                        &ReadCancellation::new(),
                    )
                    .await
                    .map_err(string_error)?;
                owner
                    .publish_mapping_count(&selected, formula_match_count)
                    .await?;
                Ok((formula_match_count, root_path))
            }
            .await;
            let (formula_match_count, root_path) = match result {
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
                let selected = owner
                    .shared
                    .selected()
                    .filter(|selected| selected.source_id() == &input.source_id)
                    .ok_or_else(|| "the mapped source is no longer selected".to_string())?;
                let source = selected
                    .source
                    .as_ref()
                    .ok_or_else(source_access_unavailable)?;
                source
                    .complete_local_mapping(
                        &selected.database,
                        selected.source_key,
                        &root_path,
                        input.server_prefix.as_deref(),
                        input.local_prefix.as_deref(),
                        Arc::clone(&cancelled),
                    )
                    .await
                    .map_err(string_error)?;
                owner
                    .publish_mapping_count(&selected, formula_match_count)
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
            if let Some(selected) = owner
                .shared
                .selected()
                .filter(|selected| selected.source_id() == &source_id)
            {
                let _ = selected
                    .database
                    .clear_mapping_access(selected.source_key)
                    .await;
                if let Some(session) = owner.shared.selected_session() {
                    session.update(selected.source_key, |current| {
                        current.formula_match_count = 0;
                    });
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
                        .configured_sources(owner.shared.selected().as_deref()),
                ))
                .await;
        });
    }

    pub fn forget_source(&self, source_id: SourceId) {
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
                .filter(|selected| selected.source_id() == &source_id);
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
                        return;
                    }
                }
            }
            if let Err(error) = owner.shared.database.remove_source(&source_id).await {
                owner.shared.warn_nonfatal(&error.to_string());
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
        });
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
        self.spawn_serialized(move |owner| async move {
            owner.apply_favorite(target, favorite).await;
        });
    }

    pub fn set_rating(&self, target: FavoriteTarget, rating: Option<u8>) {
        self.spawn_serialized(move |owner| async move {
            owner.apply_rating(target, rating).await;
        });
    }
}

impl ActiveSource {
    pub fn selected_library_revealed(&self) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        let Some(session) = shared.selected_session() else {
            return;
        };
        let Some(selected) = session.resolve() else {
            return;
        };
        let settings = shared.settings.clone();
        let events = shared.outputs.events.clone();
        let source_key = selected.source_key;
        let weak = session.downgrade();
        shared.runtime.spawn(async move {
            crate::album_release::run_selected_album_release_lookup(
                settings, events, source_key, weak,
            )
            .await;
        });
        let owner = SourceOwner { shared };
        owner.start_artwork_preparation(session);
    }

    pub fn refresh_library(&self, trigger: crate::runtime::LibraryRefreshTrigger) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        let acquisition = shared.begin_acquisition();
        self.spawn_selected(move |owner, selected| async move {
            if acquisition.load(Ordering::Acquire) {
                return;
            }
            let label = match trigger {
                crate::runtime::LibraryRefreshTrigger::GlobalAction => "global-action",
                crate::runtime::LibraryRefreshTrigger::NewlyAdded => "home-newly-added",
            };
            owner
                .manual_refresh_selected(&selected, label, acquisition)
                .await;
        });
    }

    pub fn refresh_home(&self, kind: crate::settings::HomeSectionKind) {
        if kind == crate::settings::HomeSectionKind::NewlyAdded {
            self.refresh_library(crate::runtime::LibraryRefreshTrigger::NewlyAdded);
        } else {
            self.spawn_selected(move |owner, selected| async move {
                if kind != crate::settings::HomeSectionKind::Explore {
                    let Some(source) = selected.source.as_ref() else {
                        return;
                    };
                    let section = match kind {
                        crate::settings::HomeSectionKind::MostPlayed => {
                            sources::SourceHomeSection::MostPlayed
                        }
                        crate::settings::HomeSectionKind::RecentlyPlayed => {
                            sources::SourceHomeSection::RecentlyPlayed
                        }
                        crate::settings::HomeSectionKind::RecentlyReleased => {
                            sources::SourceHomeSection::RecentlyReleased
                        }
                        crate::settings::HomeSectionKind::Explore
                        | crate::settings::HomeSectionKind::NewlyAdded => return,
                    };
                    match source.home_section(section).await {
                        Ok(entries) => {
                            let section_id = entries
                                .first()
                                .map(|entry| entry.section_id.as_str())
                                .unwrap_or(match section {
                                    sources::SourceHomeSection::MostPlayed => "most-played",
                                    sources::SourceHomeSection::NewlyAdded => "newly-added",
                                    sources::SourceHomeSection::RecentlyPlayed => "recently-played",
                                    sources::SourceHomeSection::RecentlyReleased => {
                                        "recently-released"
                                    }
                                });
                            if let Err(error) = selected
                                .database
                                .replace_home_section(selected.source_key, section_id, &entries)
                                .await
                            {
                                warn!(%error,"could not replace Home section");
                                return;
                            }
                        }
                        Err(error) => {
                            warn!(%error,"could not refresh Home section");
                            return;
                        }
                    }
                }
                owner
                    .publish_catalog(&selected, None, CatalogChange::Home)
                    .await;
            });
        }
    }

    pub fn set_music_folder(&self, folder_object_id: Option<String>) {
        self.spawn_selected(move |owner, selected| async move {
            let key = match folder_object_id.as_deref() {
                Some(object_id) => selected
                    .database
                    .folder_key_by_object(selected.source_key, object_id, &ReadCancellation::new())
                    .await
                    .ok()
                    .flatten(),
                None => None,
            };
            if folder_object_id.is_some() && key.is_none() {
                return;
            }
            let mut replacement = (*selected).clone();
            replacement.music_folder_key = key;
            replacement.music_folder_object_id = folder_object_id.clone();
            let configured = replacement.configuration.clone();
            let _ = owner.shared.settings.update(|stored| {
                if let Some(item) = stored
                    .sources
                    .configured
                    .iter_mut()
                    .find(|item| item.configuration.source_id == configured.source_id)
                {
                    item.music_folder_id = folder_object_id.clone();
                }
                Ok(())
            });
            let replacement = Arc::new(replacement);
            if let Some(session) = owner.shared.selected_session() {
                session.replace(Arc::clone(&replacement));
            }
            let _ = owner
                .shared
                .downloads
                .attach(
                    replacement.source_id().clone(),
                    replacement.source_key,
                    replacement.source.clone(),
                    replacement.music_folder_key,
                )
                .await;
            let session = owner.shared.selected_session().expect("selected session");
            owner
                .shared
                .send(SourceEvent::CatalogReplaced {
                    configured: owner.shared.configured_sources(Some(&replacement)),
                    selected: ui_selected(replacement, session),
                })
                .await;
        });
    }

    pub fn folder(
        &self,
        folder_object_id: Option<String>,
        music_folder_object_id: Option<String>,
    ) -> Receiver<Result<LiveFolderPage, String>> {
        self.spawn_reply(move |selected| async move {
            let source = selected
                .source
                .clone()
                .ok_or_else(source_access_unavailable)?;
            source
                .browse_folder(
                    folder_object_id.as_deref(),
                    music_folder_object_id.as_deref(),
                )
                .await
                .map_err(string_error)
        })
    }

    pub fn search(
        &self,
        query: String,
        limit: usize,
    ) -> Receiver<Result<library::SearchResults, String>> {
        let shared = self.shared.clone();
        self.spawn_reply(move |selected| async move {
            let source = selected
                .source
                .clone()
                .ok_or_else(source_access_unavailable)?;
            let (rows, outcome) = source
                .live_search(&selected.database, &query, limit)
                .await
                .map_err(string_error)?;
            if let Some(shared) = shared.upgrade()
                && let Some(outcome) = outcome
            {
                SourceOwner { shared }
                    .accept_scan(source.source_id(), outcome, CatalogChange::Broad)
                    .await;
            }
            Ok(rows)
        })
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
                FavoriteTarget::Track(media_uri) => SourceId::new(
                    self.shared
                        .database
                        .track_row_by_uri(media_uri, &ReadCancellation::new())
                        .await
                        .ok()
                        .flatten()?
                        .source_id,
                ),
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

    async fn apply_favorite(&self, target: FavoriteTarget, favorite: bool) {
        let source = self.favorite_source(&target).await;
        let changed = if source.as_ref().is_some_and(|(_, _, local)| !local) {
            self.shared
                .database
                .queue_remote_favorite(&target, favorite, unix_seconds())
                .await
        } else {
            self.shared.database.set_favorite(&target, favorite).await
        }
        .unwrap_or(false);
        if !changed {
            return;
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
                .track_row_by_uri(media_uri, &ReadCancellation::new())
                .await
                .map_err(string_error)?
                .map(|row| SourceId::new(row.source_id)),
        }
        .ok_or_else(source_access_unavailable)?;
        self.client(&source_id)
    }

    pub(crate) fn client(&self, source_id: &SourceId) -> Result<Arc<Source>, String> {
        if let Some(source) = self
            .shared
            .selected()
            .filter(|selected| selected.source_id() == source_id)
            .and_then(|selected| selected.source.clone())
        {
            return Ok(source);
        }
        let configured = configured_source(&self.shared.settings.load().sources, source_id)?;
        let credential = configured
            .credential_ref
            .as_ref()
            .map(|reference| load_provider_secret(&self.shared.secrets, reference))
            .transpose()?
            .flatten();
        let mut source = Source::open(
            configured.configuration.clone(),
            credential,
            Some(self.shared.settings.load().jellyfin_device_id),
        )
        .map_err(string_error)?;
        self.bind_plex_login(&configured, &mut source)?;
        Ok(Arc::new(source))
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

    pub(crate) fn current_session(&self) -> Option<Arc<ActiveSource>> {
        self.shared.selected_session()
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

    fn start_artwork_preparation(&self, session: Arc<ActiveSource>) {
        let Some(selected) = session.resolve() else {
            return;
        };
        let key = ArtworkPreparationKey {
            source: selected.source_key,
            digest: selected.artwork_digest,
        };
        let Some((token, cancelled)) = self
            .shared
            .artwork_preparation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .admit(key)
        else {
            return;
        };
        let owner = self.clone();
        let task = self.shared.runtime.spawn(async move {
            let initial_revision = artwork_digest_revision(&selected.artwork_digest);
            let current_revision = Arc::new(AtomicU64::new(initial_revision));
            let progress_revision = Arc::clone(&current_revision);
            let progress_owner = owner.clone();
            let events = owner.shared.outputs.events.clone();
            let progress = move |revision, completed| {
                progress_revision.store(revision, Ordering::Release);
                if !artwork_preparation_is_current(&owner, key, token) {
                    return;
                }
                let _ = events.try_send(SourceEvent::ArtworkPreparation {
                    source_key: key.source,
                    revision,
                    progress: Some(artwork_preparation_progress(completed)),
                });
            };
            let result = async {
                crate::artwork::prepare_database_source(
                    &progress_owner.shared.artwork,
                    &selected.database,
                    selected.source_key,
                    selected.source_id(),
                    selected.artwork_digest,
                    &progress,
                    Arc::clone(&cancelled),
                )
                .await
            }
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
                let _ = self
                    .shared
                    .database
                    .acknowledge_remote_favorite(&target, favorite)
                    .await;
            }
            Err(error) if source_error_is_temporary(&error) => {
                let _ = self
                    .shared
                    .database
                    .defer_remote_favorite(&target, favorite, unix_seconds().saturating_add(30))
                    .await;
            }
            Err(_) => {
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
    let input = SourceSettingsInput::Local { roots };
    let source_id = local.configuration.source_id;
    let cancelled = owner.shared.begin_acquisition();
    owner.spawn_serialized(move |owner| async move {
        if let Err(error) = owner
            .edit_configured_source(source_id, input, cancelled)
            .await
        {
            owner.shared.warn_nonfatal(&error);
        }
    });
}

fn configured_sources(
    stored: &StoredSettings,
    selected: Option<&SelectedSourceState>,
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
                    .filter(|selected| selected.source_id() == &configured.configuration.source_id)
                    .map(|selected| LocalAccessStatus {
                        total_track_count: selected.track_count,
                        matched_track_count: selected.formula_match_count,
                        sample_source_path: selected.sample_source_path.clone(),
                    })
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
        local_access: local_access.into(),
    }
}

fn ui_selected(
    selected: Arc<SelectedSourceState>,
    operations: Arc<ActiveSource>,
) -> SelectedLibrary {
    SelectedLibrary {
        source_id: selected.source_id().clone(),
        source_key: selected.source_key,
        music_folder_key: selected.music_folder_key,
        music_folder_object_id: selected.music_folder_object_id.clone(),
        music_folders: Arc::clone(&selected.music_folders),
        database: Arc::clone(&selected.database),
        runtime: selected.runtime.clone(),
        operations,
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

fn editable_source(configuration: &SourceConfiguration) -> Result<EditableSource, String> {
    match configuration.editable().map_err(string_error)? {
        sources::EditableSource::Plex { settings, .. } => Ok(EditableSource {
            file_settings: None,
            source: SourceSummary {
                id: configuration.source_id.clone(),
                kind: configuration.kind.clone(),
                name: configuration.name.clone(),
                transcoded_download_bitrate_limit_kbps: configuration
                    .transcoded_download_bitrate_limit_kbps(),
                half_stars_enabled: true,
            },
            credentials: CredentialPreset {
                source_name: settings.name.clone(),
                server_url: settings.address_override.clone().unwrap_or_default(),
                username: String::new(),
                trust_invalid_cert: settings.trust_invalid_cert,
                open_subsonic_authentication: None,
            },
            use_instant_mix: None,
            emby_connect: false,
            plex_settings: Some(settings),
        }),
        sources::EditableSource::Credentials {
            credentials,
            use_instant_mix,
            emby_connect,
            subsonic_authentication,
            ..
        } => Ok(EditableSource {
            plex_settings: None,
            file_settings: None,
            source: SourceSummary {
                id: configuration.source_id.clone(),
                kind: configuration.kind.clone(),
                name: configuration.name.clone(),
                transcoded_download_bitrate_limit_kbps: configuration
                    .transcoded_download_bitrate_limit_kbps(),
                half_stars_enabled: matches!(configuration.kind.as_str(), "jellyfin" | "emby"),
            },
            credentials: CredentialPreset {
                source_name: credentials.server_name,
                server_url: credentials.server_url,
                username: credentials.username,
                trust_invalid_cert: credentials.trust_invalid_cert,
                open_subsonic_authentication: subsonic_authentication,
            },
            use_instant_mix,
            emby_connect,
        }),
        sources::EditableSource::Files { settings, .. } => Ok(EditableSource {
            plex_settings: None,
            source: SourceSummary {
                id: configuration.source_id.clone(),
                kind: configuration.kind.clone(),
                name: configuration.name.clone(),
                transcoded_download_bitrate_limit_kbps: None,
                half_stars_enabled: false,
            },
            credentials: CredentialPreset {
                source_name: configuration.name.clone(),
                server_url: settings.url.clone(),
                username: settings.username.clone(),
                trust_invalid_cert: settings.trust_invalid_certificate,
                open_subsonic_authentication: None,
            },
            use_instant_mix: None,
            emby_connect: false,
            file_settings: Some(settings),
        }),
        sources::EditableSource::Local { .. } => {
            Err("Local folders are edited from the Local source panel".to_string())
        }
    }
}

fn source_setup_input(input: SourceSetup, jellyfin_device_id: &str) -> SourceSetupInput {
    match input {
        SourceSetup::EmbyConnect {
            server,
            source_name,
            use_instant_mix,
        } => SourceSetupInput::EmbyConnect {
            server,
            source_name,
            use_instant_mix,
            device_id: jellyfin_device_id.to_owned(),
        },
        SourceSetup::JellyfinQuickConnect {
            login,
            source_name,
            use_instant_mix,
        } => SourceSetupInput::JellyfinQuickConnect {
            login,
            source_name,
            use_instant_mix,
        },
        SourceSetup::Plex(input) => SourceSetupInput::Plex(input),
        SourceSetup::WebDav {
            name,
            settings,
            credentials,
        } => SourceSetupInput::WebDav {
            name,
            settings,
            credentials,
        },
        SourceSetup::Smb {
            name,
            settings,
            credentials,
        } => SourceSetupInput::Smb {
            name,
            settings,
            credentials,
        },
        SourceSetup::JellyfinEmby {
            kind,
            credentials,
            use_instant_mix,
        } => SourceSetupInput::JellyfinEmby(JellyfinEmbySetupInput {
            kind,
            credentials: credential_host_input(credentials),
            use_instant_mix,
            device_id: jellyfin_device_id.to_string(),
        }),
        SourceSetup::OpenSubsonic {
            kind,
            authentication,
            credentials,
        } => SourceSetupInput::Subsonic {
            flavor: subsonic_flavor(kind),
            authentication,
            credentials: credential_host_input(credentials),
        },
        SourceSetup::Local { roots } => SourceSetupInput::Local(LocalFolderHostInput { roots }),
    }
}

fn source_settings_input(input: SourceSettingsChange) -> SourceSettingsInput {
    match input {
        SourceSettingsChange::EmbyConnect {
            server,
            source_name,
            use_instant_mix,
            ..
        } => SourceSettingsInput::EmbyConnect {
            server,
            source_name,
            use_instant_mix,
        },
        SourceSettingsChange::JellyfinQuickConnect {
            login,
            source_name,
            use_instant_mix,
            ..
        } => SourceSettingsInput::JellyfinQuickConnect {
            login,
            source_name,
            use_instant_mix,
        },
        SourceSettingsChange::Plex { settings, .. } => SourceSettingsInput::Plex(settings),
        SourceSettingsChange::Files {
            name,
            settings,
            credentials,
            ..
        } => SourceSettingsInput::Files {
            name,
            settings,
            credentials,
        },
        SourceSettingsChange::JellyfinEmby {
            connect_manually,
            source_id: _,
            credentials,
            use_instant_mix,
        } => SourceSettingsInput::JellyfinEmby(JellyfinEmbySettingsInput {
            connect_manually,
            credentials: credential_settings_input(credentials),
            use_instant_mix,
        }),
        SourceSettingsChange::OpenSubsonic {
            source_id: _,
            kind: _,
            authentication,
            credentials,
        } => SourceSettingsInput::Subsonic {
            authentication,
            credentials: credential_settings_input(credentials),
        },
    }
}

fn source_settings_id(input: &SourceSettingsChange) -> &SourceId {
    match input {
        SourceSettingsChange::EmbyConnect { source_id, .. }
        | SourceSettingsChange::JellyfinQuickConnect { source_id, .. }
        | SourceSettingsChange::Plex { source_id, .. }
        | SourceSettingsChange::Files { source_id, .. }
        | SourceSettingsChange::JellyfinEmby { source_id, .. }
        | SourceSettingsChange::OpenSubsonic { source_id, .. } => source_id,
    }
}

fn credential_host_input(input: CredentialInput) -> CredentialHostInput {
    CredentialHostInput {
        server_name: input.source_name,
        server_url: input.server_url,
        username: input.username,
        password: input.secret,
        trust_invalid_cert: input.trust_invalid_cert,
    }
}

fn credential_settings_input(input: CredentialInput) -> CredentialSettingsInput {
    CredentialSettingsInput {
        name: input.source_name.unwrap_or_default(),
        base_url: input.server_url,
        username: input.username,
        password: input.secret,
        trust_invalid_cert: input.trust_invalid_cert,
    }
}

fn subsonic_flavor(kind: OpenSubsonicKind) -> SubsonicFlavor {
    match kind {
        OpenSubsonicKind::Navidrome => SubsonicFlavor::Navidrome,
        OpenSubsonicKind::OpenSubsonic => SubsonicFlavor::Subsonic,
    }
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

fn refreshing_progress(
    events: Sender<SourceEvent>,
    source_id: SourceId,
) -> impl Fn(SourceReadProgress) + Send + Sync {
    move |value| {
        let _ = events.try_send(SourceEvent::Operation(SourceOperation::Refreshing {
            source_id: source_id.clone(),
            progress: source_progress(value),
        }));
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
            scan.write_album(
                "album",
                "Cached Album",
                "cached album",
                "Artist",
                "cached album",
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
            )
            .await
            .unwrap();
            let ScanOutcome::Changed(publication) = scan.finish().await.unwrap() else {
                panic!("initial catalog");
            };
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
            owner.select_source(source_id.clone());
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
            assert_eq!(published.source_key, publication.source);
            tokio::time::timeout(Duration::from_secs(2), started_receiver.recv())
                .await
                .unwrap()
                .unwrap();
            let session = owner.shared.selected_session().unwrap();
            assert!(session.resolve().unwrap().source.is_none());
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
                    crate::metadata::identify_track_metadata(
                        &owner,
                        "file:///music/song.flac".into(),
                        sources::TrackMetadataValues::default()
                    )
                    .recv()
                    .await
                    .unwrap()
                    .unwrap()
                    .is_none()
                );
                assert!(
                    crate::metadata::identify_album_metadata(
                        &owner,
                        library::source_entity_uri(&source_id, "album", "album"),
                        sources::AlbumMetadataValues::default()
                    )
                    .recv()
                    .await
                    .unwrap()
                    .unwrap()
                    .is_none()
                );
                assert!(
                    crate::metadata::identify_artist_metadata(
                        &owner,
                        library::source_entity_uri(&source_id, "artist", "artist"),
                        sources::ArtistMetadataValues::default()
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
            let mut scan = library::Scan::begin_items(&database, source_id.as_str())
                .await
                .unwrap();
            scan.write_folder("new-folder", "New Folder", "new folder", "new folder", None)
                .await
                .unwrap();
            scan.write_track(
                "new-track",
                Some("album"),
                "New Track",
                "new track",
                "Cached Album",
                "Artist",
                "new track",
                1_000,
                1,
                1,
                None,
                None,
                None,
                None,
                None,
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
                [0; 32],
            )
            .await
            .unwrap();
            scan.write_track_folders(&[library::ScanLink {
                owner_id: "new-track",
                related_id: "new-folder",
                position: 0,
            }])
            .await
            .unwrap();
            let outcome = scan.finish().await.unwrap();
            let ScanOutcome::Changed(updated_publication) = outcome else {
                panic!("new folder publication")
            };
            owner
                .accept_scan(&source_id, outcome, CatalogChange::Acquired)
                .await;
            assert!(Arc::ptr_eq(
                &session,
                &owner.shared.selected_session().unwrap()
            ));
            assert!(
                session.resolve().unwrap().source.is_none(),
                "summary refresh does not obtain credentials"
            );
            assert_eq!(
                session.resolve().unwrap().music_folders[0].object_id,
                "new-folder"
            );
            let mut summary_published = false;
            while let Ok(event) = receiver.try_recv() {
                if let SourceEvent::CatalogReplaced { selected, .. } = event {
                    assert_eq!(selected.music_folders[0].object_id, "new-folder");
                    summary_published = true;
                }
            }
            assert!(
                summary_published,
                "the UI receives the updated folder snapshot"
            );
            if switch_while_held {
                let old_state = Arc::downgrade(&session.resolve().unwrap());
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
                owner
                    .accept_scan(
                        &source_id,
                        scan.finish().await.unwrap(),
                        CatalogChange::Broad,
                    )
                    .await;
                assert_eq!(
                    owner.shared.configured_sources(None).local_access[0].track_count,
                    0
                );
                assert!(session.resolve().is_none());
                assert!(old_state.upgrade().is_none());
                release.send(()).unwrap();
                tokio::time::timeout(Duration::from_secs(2), async {
                    while Arc::strong_count(&session) > 1 {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("retired credential continuation releases its session");
                assert!(session.resolve().is_none());
                assert_eq!(owner.shared.selected().unwrap().source_id(), &local_id);
                continue;
            }
            release.send(()).unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while session.resolve().unwrap().source.is_none() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("provider attaches after unlock");
            assert!(Arc::ptr_eq(
                &session,
                &owner.shared.selected_session().unwrap()
            ));
            assert_eq!(
                database
                    .cached_source(source_id.as_str(), &ReadCancellation::new())
                    .await
                    .unwrap()
                    .unwrap()
                    .catalog_revision as u64,
                updated_publication.catalog_revision
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
            let settings = SettingsFile::memory();
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
            let mut previous = None;
            for id in [&first, &second] {
                owner.select_source(id.clone());
                tokio::time::timeout(Duration::from_secs(5), async {
                    while settings.load().sources.selected_source_id.as_ref() != Some(id) {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("selection completes without consuming events");
                if let Some(previous) = previous.take() {
                    let previous: Arc<ActiveSource> = previous;
                    assert!(previous.resolve().is_none());
                }
                previous = owner.shared.selected_session();
            }
            owner.forget_source(second.clone());
            tokio::time::timeout(Duration::from_secs(5), async {
                while settings
                    .load()
                    .sources
                    .configured
                    .iter()
                    .any(|item| item.configuration.source_id == second)
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("removal completes without consuming events");
            assert!(previous.unwrap().resolve().is_none());
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
                assert_eq!(assignments, vec![Some(first), None, Some(second), None]);
            }
            owner.shared.cancel_observer();
            owner.shared.cancel_acquisition();
        }
    }

    #[tokio::test]
    async fn retired_source_handles_release_their_client_graph_before_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let database = Arc::new(
            Database::open(directory.path().join("library.sqlite"))
                .await
                .unwrap(),
        );
        let mut stale_handles = Vec::new();
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
            let selected = Arc::new(SelectedSourceState {
                configuration,
                source: Some(source),
                source_key: SourceKey::from_raw(index + 1),
                artwork_digest: [0; 32],
                database: Arc::clone(&database),
                runtime: tokio::runtime::Handle::current(),
                music_folder_key: None,
                music_folder_object_id: None,
                music_folders: Arc::from([]),
                track_count: 0,
                formula_match_count: 0,
                sample_source_path: None,
            });
            let weak_selected = Arc::downgrade(&selected);
            let (retirement, _) = tokio::sync::watch::channel(false);
            let session = Arc::new(ActiveSource {
                shared: Weak::new(),
                current: Mutex::new(Some(selected)),
                retirement,
            });
            assert!(session.resolve().is_some());
            session.retire();
            stale_handles.push(session);
            assert!(weak_source.upgrade().is_none());
            assert!(weak_selected.upgrade().is_none());
            assert!(
                stale_handles
                    .iter()
                    .all(|session| session.resolve().is_none())
            );
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
        assert!(stale_cancelled.load(Ordering::Acquire));
        assert!(!current_cancelled.load(Ordering::Acquire));
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
        assert!(cancelled.load(Ordering::Acquire));
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
