//! The configured sources and the one selected Database-backed source session.

use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use artwork::Artwork;
use async_channel::{Receiver, Sender};
use downloads::Downloads;
use library::{
    AlbumKey, ArtistKey, Database, FavoriteTarget, FolderKey, FolderRow, PlaylistEntryKey,
    PlaylistKey, ReadCancellation, ScanOutcome, SourceKey, TrackKey,
};
use playback::{QueuePlacement, SourceSessionEpoch};
use scrobbling::Scrobbler;
use secrets::{SecretStorageMode, SwitchableSecretStore};
use sources::{
    AlbumMetadata, AlbumMetadataEdit, AlbumMetadataValues, ArtistMetadata, ArtistMetadataEdit,
    ArtistMetadataValues, CredentialHostInput, CredentialSettingsInput, JellyfinSettingsInput,
    JellyfinSetupInput, LiveFolderPage, LiveSearchResults, LocalFolderHostInput, SelectedFeed,
    Source, SourceConfiguration, SourceEntityKind, SourceError, SourceId, SourceMetadataError,
    SourceReadProgress, SourceReadStage, SourceSettingsInput, SourceSetupInput,
    SubsonicAuthentication, SubsonicFlavor, TrackMetadata, TrackMetadataEdit, TrackMetadataValues,
};
use tracing::{info, warn};
use ui::runtime::source::{
    ConfiguredSources, CredentialInput, CredentialPreset, DiscoveredServer, DiscoveryStatus,
    DiscoveryUpdate, EditableSource, LiveSearchCollectionTarget, LocalAccessStatus, LocalFolder,
    OpenSubsonicAuthentication, OpenSubsonicKind, SelectedSourcePort, SourceLocalAccess,
    SourceLocalAccessSummary, SourceOperation, SourcePort, SourceProgress, SourceProgressStage,
    SourceSettingsChange, SourceSetup, SourceSummary,
};
use ui::runtime::{
    CatalogChange, CatalogPublication, FavoriteSettlement, SelectedLibrary, SourceEvent,
    SourceNotice, SourceNoticeKind,
};

use crate::playback::PlaybackOwner;
use crate::settings::{
    ConfiguredSource, SavedLocalAccess, SettingsFile, StoredSettings, all_secret_keys,
    delete_provider_secret, fresh_credential_ref, fresh_secret_scope_id, load_provider_secret,
    load_scrobbling_settings, platform_secret_store, save_provider_secret,
};

const SOURCE_CHECK_INTERVAL: Duration = Duration::from_secs(5 * 60);

#[derive(Clone)]
pub(crate) struct SelectedSourceState {
    pub(crate) configuration: SourceConfiguration,
    pub(crate) source: Option<Arc<Source>>,
    pub(crate) source_key: SourceKey,
    pub(crate) artwork_digest: [u8; 32],
    pub(crate) source_session_epoch: SourceSessionEpoch,
    pub(crate) database: Arc<Database>,
    pub(crate) runtime: tokio::runtime::Handle,
    pub(crate) music_folder_key: Option<FolderKey>,
    pub(crate) music_folder_object_id: Option<String>,
    pub(crate) music_folders: Arc<[FolderRow]>,
    pub(crate) album_count: usize,
    pub(crate) track_count: usize,
    pub(crate) mapped_count: usize,
}

impl SelectedSourceState {
    pub(crate) fn source_id(&self) -> &SourceId {
        &self.configuration.source_id
    }
}

pub(crate) struct ActiveSource {
    shared: Weak<Shared>,
    source_id: SourceId,
    source_session_epoch: SourceSessionEpoch,
    retirement: tokio::sync::watch::Sender<bool>,
}

pub(crate) type WeakActiveSource = Weak<ActiveSource>;

impl ActiveSource {
    fn new(shared: &Arc<Shared>, state: &SelectedSourceState) -> Arc<Self> {
        let (retirement, _) = tokio::sync::watch::channel(false);
        Arc::new(Self {
            shared: Arc::downgrade(shared),
            source_id: state.source_id().clone(),
            source_session_epoch: state.source_session_epoch,
            retirement,
        })
    }

    pub(crate) fn resolve(&self) -> Option<Arc<SelectedSourceState>> {
        self.shared
            .upgrade()?
            .resolve_selected(&self.source_id, self.source_session_epoch)
    }

    pub(crate) fn downgrade(self: &Arc<Self>) -> WeakActiveSource {
        Arc::downgrade(self)
    }

    fn retire(&self) {
        self.retirement.send_replace(true);
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
        let source_id = self.source_id.clone();
        let epoch = self.source_session_epoch;
        let runtime = shared.runtime.clone();
        runtime.spawn(async move {
            let Some(selected) = shared.resolve_selected(&source_id, epoch) else {
                return;
            };
            let value = work(selected).await;
            if shared.resolve_selected(&source_id, epoch).is_some() {
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
        let source_id = self.source_id.clone();
        let epoch = self.source_session_epoch;
        let runtime = shared.runtime.clone();
        runtime.spawn(async move {
            let lane_owner = Arc::clone(&shared);
            let _lane = lane_owner.lane.lock().await;
            let Some(selected) = shared.resolve_selected(&source_id, epoch) else {
                return;
            };
            work(SourceOwner { shared }, selected).await;
        });
    }

    fn playlist_change<F, Work>(&self, change: F)
    where
        F: FnOnce(Arc<Source>, Arc<SelectedSourceState>) -> Work + Send + 'static,
        Work: Future<Output = Result<(bool, Option<ScanOutcome>), SourceError>> + Send + 'static,
    {
        self.spawn_selected(move |owner, selected| async move {
            let result = match selected.source.as_ref().cloned() {
                Some(source) => change(source, Arc::clone(&selected)).await,
                None => Err(SourceError::InvalidRequest("Source is unavailable")),
            };
            owner.accept_playlist_result(&selected, result).await;
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
pub(crate) struct SourceOwner {
    shared: Arc<Shared>,
}

struct SelectedSlot {
    session: Arc<ActiveSource>,
    current: Arc<SelectedSourceState>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ArtworkPreparationKey {
    source: SourceKey,
    epoch: SourceSessionEpoch,
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

struct Shared {
    artwork: Artwork,
    database: Arc<Database>,
    downloads: Downloads,
    settings: SettingsFile,
    secrets: Arc<SwitchableSecretStore>,
    scrobbler: Arc<Scrobbler>,
    runtime: tokio::runtime::Handle,
    outputs: SourceOutputs,
    selected: Mutex<Option<SelectedSlot>>,
    observer: Mutex<Option<Arc<SelectedFeed>>>,
    acquisition: Mutex<Weak<AtomicBool>>,
    artwork_preparation: Mutex<ArtworkPreparationOwner>,
    lane: tokio::sync::Mutex<()>,
    playback: Mutex<Weak<PlaybackOwner>>,
    next_epoch: AtomicU64,
    started: AtomicBool,
}

impl Shared {
    fn resolve_selected(
        &self,
        source_id: &SourceId,
        epoch: SourceSessionEpoch,
    ) -> Option<Arc<SelectedSourceState>> {
        self.selected
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .filter(|slot| {
                slot.current.source_id() == source_id && slot.current.source_session_epoch == epoch
            })
            .map(|slot| Arc::clone(&slot.current))
    }

    fn selected(&self) -> Option<Arc<SelectedSourceState>> {
        self.selected
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(|slot| Arc::clone(&slot.current))
    }

    fn selected_session(&self) -> Option<Arc<ActiveSource>> {
        self.selected
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(|slot| Arc::clone(&slot.session))
    }

    async fn send(&self, event: SourceEvent) {
        let _ = self.outputs.events.send(event).await;
    }

    fn warn_nonfatal(&self, message: &str) {
        warn!(message, "source operation was ignored");
    }

    fn playback(&self) -> Result<Arc<PlaybackOwner>, String> {
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
        scrobbler: Arc<Scrobbler>,
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
            scrobbler,
            runtime,
            outputs,
            selected: Mutex::new(None),
            observer: Mutex::new(None),
            acquisition: Mutex::new(Weak::new()),
            artwork_preparation: Mutex::new(ArtworkPreparationOwner::default()),
            lane: tokio::sync::Mutex::new(()),
            playback: Mutex::new(Weak::new()),
            next_epoch: AtomicU64::new(1),
            started: AtomicBool::new(false),
        });
        SourceBootstrap {
            configured: configured_sources(&stored, None),
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
        if let Some(source_id) = self.shared.settings.load().sources.selected_source_id {
            SourcePort::select_source(self, source_id);
        }
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

    fn spawn_serialized<F, Work>(&self, work: F)
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
        let credential = configured
            .credential_ref
            .as_ref()
            .map(|reference| load_provider_secret(&self.shared.secrets, reference))
            .transpose()?
            .flatten();
        let cached = self
            .shared
            .database
            .cached_source(source_id.as_str(), &ReadCancellation::new())
            .await
            .map_err(string_error)?;
        let cached_start = cached.is_some();
        let source = match Source::open(
            configured.configuration.clone(),
            credential,
            Some(self.shared.settings.load().jellyfin_device_id),
        ) {
            Ok(source) => Some(Arc::new(source)),
            Err(error) if cached.is_some() && source_error_allows_cache(&error) => None,
            Err(error) => return Err(error.to_string()),
        };
        let publication = if let Some(cached) = cached {
            library::Publication {
                source: cached.source,
                catalog_revision: u64::try_from(cached.catalog_revision)
                    .map_err(|_| "cached catalog revision is invalid".to_string())?,
                artwork_digest: cached
                    .artwork_digest
                    .try_into()
                    .map_err(|_| "cached artwork digest is invalid".to_string())?,
            }
        } else {
            let source = source.as_ref().ok_or_else(source_access_unavailable)?;
            let events = self.shared.outputs.events.clone();
            let target = source_id.clone();
            let progress = move |value: SourceReadProgress| {
                let _ = events.try_send(SourceEvent::Operation(SourceOperation::Switching {
                    target: target.clone(),
                    progress: source_progress(value),
                }));
            };
            acquire_required_catalog(
                source,
                &self.shared.database,
                &configured.configuration.name,
                &progress,
                Arc::clone(&cancelled),
            )
            .await?
        };
        if cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        self.install_selected(
            configured,
            source,
            publication,
            cached_start,
            cancelled,
            || Ok(()),
        )
        .await
    }

    async fn install_selected<Commit>(
        &self,
        configured: ConfiguredSource,
        source: Option<Arc<Source>>,
        publication: library::Publication,
        catch_up: bool,
        acquisition: Arc<AtomicBool>,
        commit: Commit,
    ) -> Result<(), String>
    where
        Commit: FnOnce() -> Result<(), String>,
    {
        self.shared
            .database
            .ensure_default_smart_playlists(publication.source)
            .await
            .map_err(string_error)?;
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
        let (album_count, track_count) = self
            .shared
            .database
            .source_counts(publication.source, &cancellation)
            .await
            .map_err(string_error)?;
        let mapped_count = self
            .shared
            .database
            .mapping_access_count(publication.source, &cancellation)
            .await
            .map_err(string_error)?;
        let selected = Arc::new(SelectedSourceState {
            configuration: configured.configuration.clone(),
            source,
            source_key: publication.source,
            artwork_digest: publication.artwork_digest,
            source_session_epoch: SourceSessionEpoch::new(
                self.shared.next_epoch.fetch_add(1, Ordering::AcqRel),
            ),
            database: Arc::clone(&self.shared.database),
            runtime: self.shared.runtime.clone(),
            music_folder_key: folder_key,
            music_folder_object_id: requested_folder,
            music_folders: folders,
            album_count,
            track_count,
            mapped_count,
        });
        let session = ActiveSource::new(&self.shared, &selected);
        let playback = self.shared.playback()?;
        let prepared = playback
            .prepare_selected(Arc::clone(&session), Arc::clone(&selected))
            .await?;
        if !self.shared.acquisition_is_current(&acquisition) {
            return Ok(());
        }
        commit()?;
        self.release_selected(false).await;
        let cutover = playback.stop_for_source_switch();
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
            .unwrap_or_else(|p| p.into_inner()) = Some(SelectedSlot {
            session: Arc::clone(&session),
            current: Arc::clone(&selected),
        });
        let projection = playback.install_prepared(prepared, cutover);
        self.shared.settings.update(|stored| {
            stored.sources.selected_source_id = Some(selected.source_id().clone());
            Ok(())
        })?;
        let stored = self.shared.settings.load();
        self.shared
            .send(SourceEvent::Selected {
                configured: configured_sources(&stored, Some(&selected)),
                selected: ui_selected(Arc::clone(&selected), Arc::clone(&session)),
                playback: Box::new(projection),
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
            slot.session.retire();
            let (acknowledged, receiver) = async_channel::bounded(1);
            self.shared
                .send(SourceEvent::ReleaseSelected { acknowledged })
                .await;
            let _ = receiver.recv().await;
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
        let Some(observer) = source.start_selected_feed(
            &self.shared.runtime,
            Arc::clone(&selected.database),
            selected.source_key,
        ) else {
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
            match observer.apply_pending().await {
                Ok(Some(outcome)) => {
                    self.accept_scan(&selected, outcome, CatalogChange::Broad)
                        .await;
                    continue;
                }
                Ok(None) => {}
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
        self.publish_operation(SourceOperation::Refreshing {
            source_id: selected.source_id().clone(),
            progress: SourceProgress {
                stage: SourceProgressStage::Files,
                completed: 0,
                total: None,
            },
        })
        .await;
        let progress = refreshing_progress(
            self.shared.outputs.events.clone(),
            selected.source_id().clone(),
        );
        match source
            .catch_up_local(
                &selected.database,
                selected.source_key,
                &progress,
                cancelled,
            )
            .await
        {
            Ok(outcome) => {
                self.accept_scan(&selected, outcome, CatalogChange::Broad)
                    .await
            }
            Err(error) => warn!(%error, "Local startup catch-up failed"),
        }
        self.publish_operation(SourceOperation::Idle).await;
    }

    async fn manual_refresh_selected(
        &self,
        selected: &SelectedSourceState,
        trigger: &'static str,
        acquisition: Arc<AtomicBool>,
    ) {
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
        match outcome {
            Ok(outcome) => {
                self.accept_scan(selected, outcome, CatalogChange::Broad)
                    .await
            }
            Err(SourceError::Cancelled) => {}
            Err(error) => self.shared.warn_nonfatal(&error.to_string()),
        }
        if self.shared.acquisition_is_current(&acquisition) {
            self.publish_operation(SourceOperation::Idle).await;
        }
    }

    async fn install_connected_edit(
        &self,
        configured: ConfiguredSource,
        connected: Box<sources::ConnectedSource>,
        cancelled: Arc<AtomicBool>,
    ) -> Result<(), String> {
        let (configuration, source, credential) = (*connected).into_parts();
        let source = Arc::new(source);
        self.publish_operation(SourceOperation::Refreshing {
            source_id: configuration.source_id.clone(),
            progress: initial_progress(),
        })
        .await;
        let progress = refreshing_progress(
            self.shared.outputs.events.clone(),
            configuration.source_id.clone(),
        );
        let result = async {
            let publication = acquire_required_catalog(
                &source,
                &self.shared.database,
                &configuration.name,
                &progress,
                Arc::clone(&cancelled),
            )
            .await?;
            if !self.shared.acquisition_is_current(&cancelled) {
                return Ok(());
            }
            let mut replacement = configured;
            replacement.configuration = configuration;
            let selected = self.shared.selected().is_some_and(|selected| {
                selected.source_id() == &replacement.configuration.source_id
            });
            if selected {
                let committed = replacement.clone();
                self.install_selected(
                    replacement,
                    Some(source),
                    publication,
                    false,
                    cancelled,
                    move || self.persist_connected_source(&committed, credential),
                )
                .await?;
            } else {
                self.persist_connected_source(&replacement, credential)?;
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

    fn persist_connected_source(
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
            sources::SourceEditResult::Unchanged => Ok(()),
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
        }
    }

    async fn accept_scan(
        &self,
        selected: &SelectedSourceState,
        outcome: ScanOutcome,
        change: CatalogChange,
    ) {
        let (publication, catalog_changed) = match outcome {
            ScanOutcome::Changed(publication) => (publication, true),
            ScanOutcome::ArtworkChanged(publication) => (publication, false),
            ScanOutcome::Identical(_) | ScanOutcome::Stale | ScanOutcome::Failed => return,
        };
        if let Some(slot) = self
            .shared
            .selected
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_mut()
            && slot.current.source_session_epoch == selected.source_session_epoch
        {
            let mut replacement = (*slot.current).clone();
            replacement.artwork_digest = publication.artwork_digest;
            slot.current = Arc::new(replacement);
        }
        if catalog_changed {
            self.shared
                .downloads
                .library_changed(selected.source_id().clone());
            if let Ok(playback) = self.shared.playback() {
                playback.catalog_changed();
            }
            self.shared
                .send(SourceEvent::CatalogPublished(CatalogPublication {
                    source_key: publication.source,
                    source_session_epoch: selected.source_session_epoch,
                    favorite: None,
                    change,
                }))
                .await;
        }
        if let Some(session) = self.shared.selected_session() {
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
        if let Ok(Some(outcome)) = source
            .refresh_if_needed(
                &selected.database,
                &selected.configuration.name,
                &progress,
                Arc::clone(&acquisition),
            )
            .await
        {
            self.accept_scan(&selected, outcome, CatalogChange::Broad)
                .await;
        }
        if progressed.load(Ordering::Acquire) {
            self.publish_operation(SourceOperation::Idle).await;
        }
    }

    async fn accept_playlist_result(
        &self,
        selected: &SelectedSourceState,
        result: Result<(bool, Option<ScanOutcome>), SourceError>,
    ) {
        match result {
            Ok((true, Some(outcome))) => {
                self.accept_scan(selected, outcome, CatalogChange::Playlists)
                    .await
            }
            Ok((true, None)) => {
                self.publish_catalog(selected, None, CatalogChange::Playlists)
                    .await
            }
            Ok((false, _)) => {}
            Err(error) => self.shared.warn_nonfatal(&error.to_string()),
        }
    }

    async fn publish_mapping_count(
        &self,
        selected: &SelectedSourceState,
        mapped_count: usize,
    ) -> Result<(), String> {
        let current = {
            let mut slots = self
                .shared
                .selected
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(slot) = slots
                .as_mut()
                .filter(|slot| slot.current.source_session_epoch == selected.source_session_epoch)
            else {
                return Ok(());
            };
            let mut replacement = (*slot.current).clone();
            replacement.mapped_count = mapped_count;
            slot.current = Arc::new(replacement);
            Arc::clone(&slot.current)
        };
        self.shared
            .playback()?
            .stream_inputs_changed(current.source_key, current.source_session_epoch)?;
        self.shared
            .send(SourceEvent::Configured(configured_sources(
                &self.shared.settings.load(),
                Some(&current),
            )))
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
                source_key: selected.source_key,
                source_session_epoch: selected.source_session_epoch,
                favorite,
                change,
            }))
            .await;
    }
}

impl SourcePort for SourceOwner {
    fn configured_source(&self, source_id: &SourceId) -> Result<Option<EditableSource>, String> {
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

    fn discover_servers(&self) {
        let events = self.shared.outputs.discovery.clone();
        let _ = events.try_send(DiscoveryUpdate {
            servers: Arc::from([]),
            status: DiscoveryStatus::Searching,
        });
        self.shared.runtime.spawn(async move {
            let update =
                match sources::discover_jellyfin_servers(Duration::from_millis(1_500)).await {
                    Ok(servers) if servers.is_empty() => DiscoveryUpdate {
                        servers: Arc::from([]),
                        status: DiscoveryStatus::Empty,
                    },
                    Ok(servers) => {
                        let servers = servers
                            .into_iter()
                            .map(|server| DiscoveredServer {
                                name: server.name,
                                address: server.address,
                                id: server.id,
                            })
                            .collect::<Vec<_>>();
                        DiscoveryUpdate {
                            status: DiscoveryStatus::Found(servers.len() as u64),
                            servers: servers.into(),
                        }
                    }
                    Err(error) => DiscoveryUpdate {
                        servers: Arc::from([]),
                        status: DiscoveryStatus::Failed(error.to_string()),
                    },
                };
            let _ = events.try_send(update);
        });
    }

    fn configure_source(&self, input: SourceSetup) {
        let cancelled = self.shared.begin_acquisition();
        self.spawn_serialized(move |owner| async move {
            if cancelled.load(Ordering::Acquire) {
                return;
            }
            owner
                .publish_operation(SourceOperation::Adding {
                    progress: initial_progress(),
                })
                .await;
            let input = source_setup_input(input, &owner.shared.settings.load().jellyfin_device_id);
            let result = async {
                let connected = Source::connect(input).await.map_err(string_error)?;
                let (configuration, source, credential) = connected.into_parts();
                let source = Arc::new(source);
                let events = owner.shared.outputs.events.clone();
                let progress = move |value: SourceReadProgress| {
                    let _ = events.try_send(SourceEvent::Operation(SourceOperation::Adding {
                        progress: source_progress(value),
                    }));
                };
                let publication = acquire_required_catalog(
                    &source,
                    &owner.shared.database,
                    &configuration.name,
                    &progress,
                    Arc::clone(&cancelled),
                )
                .await?;
                if !owner.shared.acquisition_is_current(&cancelled) {
                    return Ok(());
                }
                let credential_ref = credential
                    .as_ref()
                    .map(|_| fresh_credential_ref())
                    .transpose()?;
                let configured = ConfiguredSource {
                    configuration: configuration.clone(),
                    credential_ref,
                    music_folder_id: None,
                    local_access: None,
                };
                let commit_owner = owner.clone();
                let committed = configured.clone();
                owner
                    .install_selected(
                        configured,
                        Some(source),
                        publication,
                        false,
                        Arc::clone(&cancelled),
                        move || commit_owner.persist_connected_source(&committed, credential),
                    )
                    .await
            }
            .await;
            if let Err(error) = result
                && owner.shared.acquisition_is_current(&cancelled)
            {
                owner
                    .publish_operation(SourceOperation::Failed {
                        source_id: None,
                        message: error,
                        add_form: true,
                    })
                    .await;
            }
        });
    }

    fn update_source(&self, input: SourceSettingsChange) {
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

    fn select_source(&self, source_id: SourceId) {
        let cancelled = self.shared.begin_acquisition();
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

    fn change_secret_storage(&self, mode: SecretStorageMode) -> Receiver<Result<(), String>> {
        let (sender, receiver) = async_channel::bounded(1);
        self.spawn_serialized(move |owner| async move {
            let previous = owner.shared.settings.load();
            let result = if previous.ui.secret_storage_mode == mode {
                Ok(())
            } else {
                let keys = all_secret_keys(&previous);
                let update = owner.shared.settings.update(|stored| {
                    stored.ui.secret_storage_mode = mode;
                    stored.secret_scope_id = fresh_secret_scope_id()?;
                    for descriptor in scrobbling::secret_descriptors() {
                        descriptor.value_mut(&mut stored.scrobbling).clear();
                    }
                    Ok(stored.clone())
                });
                match update {
                    Ok(changed) => {
                        let old = owner
                            .shared
                            .secrets
                            .replace(platform_secret_store(&changed));
                        for key in keys {
                            let _ = old.delete_secret(&key);
                        }
                        let scrobbling =
                            load_scrobbling_settings(&owner.shared.settings, &owner.shared.secrets);
                        owner
                            .shared
                            .scrobbler
                            .update_settings(scrobbling, changed.ui.private_mode)
                    }
                    Err(error) => Err(error),
                }
            };
            let _ = sender.send(result).await;
        });
        receiver
    }

    fn add_local_folder(&self, path: PathBuf) {
        edit_local_roots(self, move |roots| {
            roots.push(path);
        });
    }

    fn replace_local_folder(&self, current: String, replacement: PathBuf) {
        edit_local_roots(self, move |roots| {
            if let Some(root) = roots
                .iter_mut()
                .find(|root| root.to_string_lossy() == current)
            {
                *root = replacement;
            }
        });
    }

    fn remove_local_folder(&self, path: String) {
        edit_local_roots(self, move |roots| {
            roots.retain(|root| root.to_string_lossy() != path)
        });
    }

    fn refresh_source(&self, source_id: SourceId) {
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

    fn check_for_source_changes(&self) {
        let owner = self.clone();
        self.shared.runtime.spawn(async move {
            owner.check_remote_freshness().await;
        });
    }

    fn save_local_access(&self, input: SourceLocalAccess) -> Receiver<Result<(), String>> {
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
                source
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
                        root_path: input.root_path.clone(),
                        server_prefix: input.server_prefix.clone(),
                        local_prefix: input.local_prefix.clone(),
                    });
                    Ok(())
                })?;
                let mapped_count = selected
                    .database
                    .mapping_access_count(selected.source_key, &ReadCancellation::new())
                    .await
                    .map_err(string_error)?;
                owner.publish_mapping_count(&selected, mapped_count).await?;
                Ok(())
            }
            .await;
            let succeeded = result.is_ok();
            let _ = sender.send(result).await;
            if !succeeded || cancelled.load(Ordering::Acquire) {
                return;
            }
            let Some(selected) = owner.shared.selected().filter(|selected| {
                selected.source_id() == &input.source_id && !cancelled.load(Ordering::Acquire)
            }) else {
                return;
            };
            let Some(source) = selected.source.as_ref() else {
                return;
            };
            match source
                .complete_local_mapping(
                    &selected.database,
                    selected.source_key,
                    &input.root_path,
                    input.server_prefix.as_deref(),
                    input.local_prefix.as_deref(),
                    Arc::clone(&cancelled),
                )
                .await
            {
                Ok(mapped_count) if !cancelled.load(Ordering::Acquire) => {
                    if let Err(error) = owner.publish_mapping_count(&selected, mapped_count).await {
                        warn!(%error, "could not publish completed Local mapping");
                    }
                }
                Ok(_) | Err(SourceError::Cancelled) => {}
                Err(error) => warn!(%error, "background Local mapping did not complete"),
            }
        });
        receiver
    }

    fn clear_local_access(&self, source_id: SourceId) {
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
                if let Some(slot) = owner
                    .shared
                    .selected
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .as_mut()
                {
                    let mut replacement = (*slot.current).clone();
                    replacement.mapped_count = 0;
                    slot.current = Arc::new(replacement);
                }
                let _ = owner.shared.playback().and_then(|playback| {
                    playback
                        .stream_inputs_changed(selected.source_key, selected.source_session_epoch)
                });
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
                .send(SourceEvent::Configured(configured_sources(
                    &owner.shared.settings.load(),
                    owner.shared.selected().as_deref(),
                )))
                .await;
        });
    }

    fn forget_source(&self, source_id: SourceId) {
        self.spawn_serialized(move |owner| async move {
            let stored = owner.shared.settings.load();
            let configured = stored
                .sources
                .configured
                .iter()
                .find(|item| item.configuration.source_id == source_id)
                .cloned();
            if owner
                .shared
                .selected()
                .is_some_and(|selected| selected.source_id() == &source_id)
            {
                owner.release_selected(true).await;
            }
            if let Some(cached) = owner
                .shared
                .database
                .cached_source(source_id.as_str(), &ReadCancellation::new())
                .await
                .ok()
                .flatten()
            {
                owner.shared.downloads.clear(source_id.clone(), false);
                let _ = owner.shared.database.remove_source(cached.source).await;
            }
            if let Some(reference) = configured.and_then(|item| item.credential_ref) {
                let _ = delete_provider_secret(&owner.shared.secrets, &reference);
            }
            let _ = owner.shared.settings.update(|stored| {
                stored
                    .sources
                    .configured
                    .retain(|item| item.configuration.source_id != source_id);
                if stored.sources.selected_source_id.as_ref() == Some(&source_id) {
                    stored.sources.selected_source_id = None;
                }
                Ok(())
            });
            owner
                .shared
                .send(SourceEvent::Configured(configured_sources(
                    &owner.shared.settings.load(),
                    None,
                )))
                .await;
        });
    }
}

impl SelectedSourcePort for ActiveSource {
    fn selected_library_revealed(&self) {
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
        let epoch = selected.source_session_epoch;
        let weak = session.downgrade();
        shared.runtime.spawn(async move {
            crate::album_release::run_selected_album_release_lookup(
                settings,
                events,
                source_key,
                epoch,
                weak,
                Arc::new(AtomicBool::new(false)),
            )
            .await;
        });
        let owner = SourceOwner { shared };
        owner.start_artwork_preparation(session);
    }

    fn refresh_library(&self, trigger: ui::runtime::LibraryRefreshTrigger) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        let acquisition = shared.begin_acquisition();
        self.spawn_selected(move |owner, selected| async move {
            if acquisition.load(Ordering::Acquire) {
                return;
            }
            let label = match trigger {
                ui::runtime::LibraryRefreshTrigger::GlobalAction => "global-action",
                ui::runtime::LibraryRefreshTrigger::NewlyAdded => "home-newly-added",
            };
            owner
                .manual_refresh_selected(&selected, label, acquisition)
                .await;
        });
    }

    fn refresh_home(&self, kind: ui::HomeSectionKind) {
        if kind == ui::HomeSectionKind::NewlyAdded {
            self.refresh_library(ui::runtime::LibraryRefreshTrigger::NewlyAdded);
        } else {
            self.spawn_selected(move |owner, selected| async move {
                if kind != ui::HomeSectionKind::Explore {
                    let Some(source) = selected.source.as_ref() else {
                        return;
                    };
                    let section = match kind {
                        ui::HomeSectionKind::MostPlayed => sources::SourceHomeSection::MostPlayed,
                        ui::HomeSectionKind::RecentlyPlayed => {
                            sources::SourceHomeSection::RecentlyPlayed
                        }
                        ui::HomeSectionKind::RecentlyReleased => {
                            sources::SourceHomeSection::RecentlyReleased
                        }
                        ui::HomeSectionKind::Explore | ui::HomeSectionKind::NewlyAdded => return,
                    };
                    match source.home_section(section).await {
                        Ok(entries) => {
                            let section_id = entries
                                .first()
                                .map(|entry| entry.section_id.as_str())
                                .unwrap_or(match section {
                                    sources::SourceHomeSection::MostPlayed => "most-played",
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

    fn set_music_folder(&self, folder_object_id: Option<String>) {
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
            if let Some(slot) = owner
                .shared
                .selected
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_mut()
            {
                slot.current = Arc::clone(&replacement);
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
                    configured: configured_sources(
                        &owner.shared.settings.load(),
                        Some(&replacement),
                    ),
                    selected: ui_selected(replacement, session),
                })
                .await;
        });
    }

    fn set_favorite(&self, target: FavoriteTarget, favorite: bool) {
        self.spawn_selected(move |owner, selected| async move {
            let remote = selected.source.is_some() && !selected.configuration.is_local();
            let changed = if remote {
                selected
                    .database
                    .queue_remote_favorite(selected.source_key, target, favorite, unix_seconds())
                    .await
            } else {
                selected
                    .database
                    .set_favorite(selected.source_key, target, favorite)
                    .await
            }
            .unwrap_or(false);
            if changed {
                owner
                    .publish_catalog(
                        &selected,
                        Some(FavoriteSettlement {
                            target,
                            requested: favorite,
                            effective: favorite,
                        }),
                        CatalogChange::Broad,
                    )
                    .await;
                if remote {
                    owner.deliver_favorite(selected, target, favorite).await;
                }
            }
        });
    }

    fn set_rating(&self, target: FavoriteTarget, rating: Option<u8>) {
        self.spawn_selected(move |owner, selected| async move {
            let changed = selected
                .database
                .set_rating(selected.source_key, target, rating)
                .await
                .unwrap_or(false);
            if changed {
                owner
                    .publish_catalog(&selected, None, CatalogChange::Broad)
                    .await;
                if let Some(source) = selected.source.as_ref() {
                    match target {
                        FavoriteTarget::Track(key) if selected.configuration.is_local() => {
                            if let Err(error) = source
                                .write_local_track_rating(
                                    &selected.database,
                                    selected.source_key,
                                    key,
                                    rating,
                                )
                                .await
                            {
                                warn!(%error, "could not write Local Track rating");
                            }
                        }
                        _ if !selected.configuration.is_local() => {
                            if let Some((_, object_id)) = favorite_object(&selected, target).await {
                                if let Err(error) = source.set_rating(&object_id, rating).await {
                                    warn!(%error, "could not write source rating");
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        });
    }

    fn create_playlist(&self, name: String, tracks: Vec<TrackKey>) {
        self.playlist_change(move |source, selected| async move {
            source
                .create_playlist(&selected.database, selected.source_key, &name, &tracks)
                .await
        });
    }

    fn rename_playlist(&self, playlist: PlaylistKey, name: String) {
        self.playlist_change(move |source, selected| async move {
            source
                .rename_playlist(&selected.database, selected.source_key, playlist, &name)
                .await
        });
    }

    fn delete_playlist(&self, playlist: PlaylistKey) {
        self.playlist_change(move |source, selected| async move {
            source
                .delete_playlist(&selected.database, selected.source_key, playlist)
                .await
        });
    }

    fn add_playlist_tracks(
        &self,
        playlist: PlaylistKey,
        tracks: Vec<TrackKey>,
        skip_duplicates: bool,
    ) -> usize {
        let count = tracks.len();
        self.playlist_change(move |source, selected| async move {
            source
                .add_playlist_tracks(
                    &selected.database,
                    selected.source_key,
                    playlist,
                    &tracks,
                    skip_duplicates,
                )
                .await
        });
        count
    }

    fn remove_playlist_entries(&self, playlist: PlaylistKey, entries: Vec<PlaylistEntryKey>) {
        self.playlist_change(move |source, selected| async move {
            source
                .remove_playlist_entries(
                    &selected.database,
                    selected.source_key,
                    playlist,
                    &entries,
                )
                .await
        });
    }

    fn move_playlist_entry(&self, playlist: PlaylistKey, entry: PlaylistEntryKey, position: usize) {
        self.playlist_change(move |source, selected| async move {
            source
                .move_playlist_entry(
                    &selected.database,
                    selected.source_key,
                    playlist,
                    entry,
                    position,
                )
                .await
        });
    }

    fn folder(
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

    fn search(&self, query: String, limit: usize) -> Receiver<Result<LiveSearchResults, String>> {
        self.spawn_reply(move |selected| async move {
            let source = selected
                .source
                .clone()
                .ok_or_else(source_access_unavailable)?;
            source
                .live_search(&query, limit)
                .await
                .map_err(string_error)
        })
    }

    fn play_live_search_collection(
        &self,
        target: LiveSearchCollectionTarget,
        placement: QueuePlacement,
    ) {
        self.spawn_selected(move |owner, selected| async move {
            let (source_collection, context_id) = match &target {
                LiveSearchCollectionTarget::Album(id) => (
                    sources::SourceCollection::Album(id.clone()),
                    format!("search:album:{id}"),
                ),
                LiveSearchCollectionTarget::Artist(id) => (
                    sources::SourceCollection::Artist(id.clone()),
                    format!("search:artist:{id}"),
                ),
            };
            let mut order = if let Some(source) = selected.source.as_ref() {
                match source
                    .collection_track_object_ids(&source_collection, 500)
                    .await
                {
                    Ok(ids) => selected
                        .database
                        .track_keys_by_objects(selected.source_key, &ids, &ReadCancellation::new())
                        .await
                        .unwrap_or_default(),
                    Err(_) => Vec::new(),
                }
            } else {
                Vec::new()
            };
            if order.is_empty() {
                order = match target {
                    LiveSearchCollectionTarget::Album(id) => match selected
                        .database
                        .album_key_by_object(selected.source_key, &id, &ReadCancellation::new())
                        .await
                        .ok()
                        .flatten()
                    {
                        Some(album) => selected
                            .database
                            .album_track_order(
                                selected.source_key,
                                album,
                                selected.music_folder_key,
                                "",
                                library::TrackSort::Title,
                                false,
                                &ReadCancellation::new(),
                            )
                            .await
                            .unwrap_or_default(),
                        None => Vec::new(),
                    },
                    LiveSearchCollectionTarget::Artist(id) => match selected
                        .database
                        .artist_key_by_object(selected.source_key, &id, &ReadCancellation::new())
                        .await
                        .ok()
                        .flatten()
                    {
                        Some(artist) => selected
                            .database
                            .artist_track_order(
                                selected.source_key,
                                artist,
                                false,
                                selected.music_folder_key,
                                "",
                                library::TrackSort::Title,
                                false,
                                &ReadCancellation::new(),
                            )
                            .await
                            .unwrap_or_default(),
                        None => Vec::new(),
                    },
                };
            }
            let Some(first) = order.first().copied() else {
                return;
            };
            let Some(anchor) = selected
                .database
                .track_rows(selected.source_key, &[first], &ReadCancellation::new())
                .await
                .ok()
                .and_then(|mut rows| rows.pop())
            else {
                return;
            };
            let request = playback::LoadedPlayRequest::context(
                selected.source_key,
                selected.source_session_epoch,
                order.into(),
                anchor.into(),
                0,
                placement,
                context_id,
                false,
            );
            if let (Some(request), Ok(playback)) = (request, owner.shared.playback()) {
                playback::QueueCommandPort::play_loaded(&*playback, request);
            }
        });
    }

    fn track_metadata(
        &self,
        track: TrackKey,
    ) -> Receiver<Result<TrackMetadata, SourceMetadataError>> {
        self.spawn_reply(move |selected| async move {
            let source = selected
                .source
                .clone()
                .ok_or(SourceMetadataError::Unavailable)?;
            source
                .read_track_metadata(&selected.database, selected.source_key, track)
                .await
        })
    }

    fn album_metadata(
        &self,
        album: AlbumKey,
    ) -> Receiver<Result<AlbumMetadata, SourceMetadataError>> {
        self.spawn_reply(move |selected| async move {
            let source = selected
                .source
                .clone()
                .ok_or(SourceMetadataError::Unavailable)?;
            source
                .read_album_metadata(&selected.database, selected.source_key, album)
                .await
        })
    }

    fn artist_metadata(
        &self,
        artist: ArtistKey,
    ) -> Receiver<Result<ArtistMetadata, SourceMetadataError>> {
        self.spawn_reply(move |selected| async move {
            let source = selected
                .source
                .clone()
                .ok_or(SourceMetadataError::Unavailable)?;
            source
                .read_artist_metadata(&selected.database, selected.source_key, artist)
                .await
        })
    }

    fn write_reviewed_track_metadata(
        &self,
        track: TrackKey,
        revision: Option<String>,
        application_token: Option<String>,
        edit: TrackMetadataEdit,
    ) -> Receiver<Result<(), SourceMetadataError>> {
        self.spawn_reply(move |selected| async move {
            let source = selected
                .source
                .clone()
                .ok_or(SourceMetadataError::Unavailable)?;
            source
                .write_track_metadata(
                    &selected.database,
                    selected.source_key,
                    track,
                    revision.as_deref().unwrap_or_default(),
                    application_token.as_deref(),
                    edit,
                )
                .await
                .map(|_| ())
        })
    }

    fn write_reviewed_album_metadata(
        &self,
        album: AlbumKey,
        revision: Option<String>,
        application_token: Option<String>,
        edit: AlbumMetadataEdit,
    ) -> Receiver<Result<(), SourceMetadataError>> {
        self.spawn_reply(move |selected| async move {
            let source = selected
                .source
                .clone()
                .ok_or(SourceMetadataError::Unavailable)?;
            source
                .write_album_metadata(
                    &selected.database,
                    selected.source_key,
                    album,
                    revision.as_deref().unwrap_or_default(),
                    application_token.as_deref(),
                    edit,
                )
                .await
                .map(|_| ())
        })
    }

    fn write_reviewed_artist_metadata(
        &self,
        artist: ArtistKey,
        revision: Option<String>,
        application_token: Option<String>,
        edit: ArtistMetadataEdit,
    ) -> Receiver<Result<(), SourceMetadataError>> {
        self.spawn_reply(move |selected| async move {
            let source = selected
                .source
                .clone()
                .ok_or(SourceMetadataError::Unavailable)?;
            source
                .write_artist_metadata(
                    &selected.database,
                    selected.source_key,
                    artist,
                    revision.as_deref().unwrap_or_default(),
                    application_token.as_deref(),
                    edit,
                )
                .await
                .map(|_| ())
        })
    }

    fn identify_track_metadata(
        &self,
        track: TrackKey,
        values: TrackMetadataValues,
    ) -> Receiver<Result<Option<(TrackMetadataValues, Option<String>)>, String>> {
        let external = self
            .shared
            .upgrade()
            .is_some_and(|shared| shared.settings.load().ui.allows_external_metadata_lookup());
        self.spawn_reply(move |selected| async move {
            if let Some(source) = selected.source.clone()
                && let Some((values, token)) = source
                    .identify_track_metadata(
                        &selected.database,
                        selected.source_key,
                        track,
                        &values,
                    )
                    .await?
            {
                return Ok(Some((values, Some(token))));
            }
            if !external {
                return Ok(None);
            }
            tokio::task::spawn_blocking(move || metadata_lookup::identify_track_metadata(&values))
                .await
                .map_err(string_error)?
                .map(|identified| identified.map(|values| (values, None)))
        })
    }

    fn identify_album_metadata(
        &self,
        album: AlbumKey,
        values: AlbumMetadataValues,
    ) -> Receiver<Result<Option<(AlbumMetadataValues, Option<String>)>, String>> {
        let external = self
            .shared
            .upgrade()
            .is_some_and(|shared| shared.settings.load().ui.allows_external_metadata_lookup());
        self.spawn_reply(move |selected| async move {
            if let Some(source) = selected.source.clone()
                && let Some((values, token)) = source
                    .identify_album_metadata(
                        &selected.database,
                        selected.source_key,
                        album,
                        &values,
                    )
                    .await?
            {
                return Ok(Some((values, Some(token))));
            }
            if !external {
                return Ok(None);
            }
            tokio::task::spawn_blocking(move || metadata_lookup::identify_album_metadata(&values))
                .await
                .map_err(string_error)?
                .map(|identified| identified.map(|values| (values, None)))
        })
    }

    fn identify_artist_metadata(
        &self,
        artist: ArtistKey,
        values: ArtistMetadataValues,
    ) -> Receiver<Result<Option<(ArtistMetadataValues, Option<String>)>, String>> {
        let external = self
            .shared
            .upgrade()
            .is_some_and(|shared| shared.settings.load().ui.allows_external_metadata_lookup());
        self.spawn_reply(move |selected| async move {
            if let Some(source) = selected.source.clone()
                && let Some((values, token)) = source
                    .identify_artist_metadata(
                        &selected.database,
                        selected.source_key,
                        artist,
                        &values,
                    )
                    .await?
            {
                return Ok(Some((values, Some(token))));
            }
            if !external {
                return Ok(None);
            }
            tokio::task::spawn_blocking(move || metadata_lookup::identify_artist_metadata(&values))
                .await
                .map_err(string_error)?
                .map(|identified| identified.map(|values| (values, None)))
        })
    }
}

impl SourceOwner {
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
            epoch: selected.source_session_epoch,
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
            let images = selected.source.as_ref().map_or_else(
                || artwork::SourceImages::cache_only(selected.source_id().clone()),
                |source| artwork::SourceImages::new(Arc::clone(source)),
            );
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
            let result = progress_owner
                .shared
                .artwork
                .prepare_database_source(
                    &selected.database,
                    selected.source_key,
                    images,
                    selected.artwork_digest,
                    &progress,
                    Arc::clone(&cancelled),
                )
                .await;
            let revision = current_revision.load(Ordering::Acquire);
            let completed = match result {
                Ok(completed) => completed,
                Err(artwork::ArtworkError::Cancelled) => return,
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
        selected: Arc<SelectedSourceState>,
        target: FavoriteTarget,
        favorite: bool,
    ) {
        let Some(source) = selected.source.as_ref() else {
            return;
        };
        let Some((kind, object_id)) = favorite_object(&selected, target).await else {
            return;
        };
        match source.set_favorite(kind, &object_id, favorite).await {
            Ok(()) => {
                let _ = selected
                    .database
                    .acknowledge_remote_favorite(selected.source_key, target, favorite)
                    .await;
            }
            Err(error) if source_error_is_temporary(&error) => {
                let _ = selected
                    .database
                    .defer_remote_favorite(
                        selected.source_key,
                        target,
                        favorite,
                        unix_seconds().saturating_add(30),
                    )
                    .await;
            }
            Err(_) => {
                if let Ok(Some(previous)) = selected
                    .database
                    .reject_remote_favorite(selected.source_key, target, favorite)
                    .await
                {
                    self.publish_catalog(
                        &selected,
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
                            source_key: selected.source_key,
                            source_session_epoch: selected.source_session_epoch,
                            kind: SourceNoticeKind::FavoriteRejected,
                        }))
                        .await;
                }
            }
        }
    }
}

async fn favorite_object(
    selected: &SelectedSourceState,
    target: FavoriteTarget,
) -> Option<(SourceEntityKind, String)> {
    let cancel = ReadCancellation::new();
    match target {
        FavoriteTarget::Track(key) => selected
            .database
            .track_rows(selected.source_key, &[key], &cancel)
            .await
            .ok()?
            .pop()
            .map(|row| (SourceEntityKind::Track, row.object_id)),
        FavoriteTarget::Album(key) => selected
            .database
            .album_rows(selected.source_key, &[key], None, &cancel)
            .await
            .ok()?
            .pop()
            .map(|row| (SourceEntityKind::Album, row.object_id)),
        FavoriteTarget::Artist(key) => selected
            .database
            .artist_rows(selected.source_key, &[key], false, None, &cancel)
            .await
            .ok()?
            .pop()
            .map(|row| (SourceEntityKind::Artist, row.object_id)),
    }
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

async fn acquire_required_catalog(
    source: &Source,
    database: &Database,
    display_name: &str,
    progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
    cancelled: Arc<AtomicBool>,
) -> Result<library::Publication, String> {
    match source
        .manual_refresh(database, display_name, progress, cancelled)
        .await
        .map_err(string_error)?
    {
        ScanOutcome::Changed(publication)
        | ScanOutcome::ArtworkChanged(publication)
        | ScanOutcome::Identical(publication) => Ok(publication),
        ScanOutcome::Stale | ScanOutcome::Failed => {
            Err("the source catalog was not published".to_string())
        }
    }
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
    if roots.is_empty() {
        SourcePort::forget_source(owner, local.configuration.source_id);
        return;
    }
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
                    .map(|selected| {
                        let metadata = configured.local_access.as_ref().is_some_and(|access| {
                            access.server_prefix.is_none() && access.local_prefix.is_none()
                        });
                        LocalAccessStatus {
                            total_track_count: selected.track_count,
                            direct_match_count: 0,
                            prefix_match_count: if metadata { 0 } else { selected.mapped_count },
                            metadata_match_count: if metadata { selected.mapped_count } else { 0 },
                            unmatched_count: selected
                                .track_count
                                .saturating_sub(selected.mapped_count),
                            sample_source_path: None,
                            sample_local_path: None,
                        }
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
                album_count: selected
                    .filter(|selected| selected.source_id() == &configured.configuration.source_id)
                    .map_or(0, |selected| selected.album_count),
                track_count: selected
                    .filter(|selected| selected.source_id() == &configured.configuration.source_id)
                    .map_or(0, |selected| selected.track_count),
            }
        })
        .collect::<Vec<_>>();
    ConfiguredSources {
        sources: sources.into(),
        selected_source_id: stored.sources.selected_source_id.clone(),
        local_folders: local_folders.into(),
        local_access: local_access.into(),
        first_run: stored.sources.configured.is_empty()
            || stored.sources.selected_source_id.is_none(),
    }
}

fn ui_selected(
    selected: Arc<SelectedSourceState>,
    operations: Arc<ActiveSource>,
) -> SelectedLibrary {
    let artwork = selected.source.as_ref().map_or_else(
        || artwork::SourceImages::cache_only(selected.source_id().clone()),
        |source| artwork::SourceImages::new(Arc::clone(source)),
    );
    SelectedLibrary {
        source_key: selected.source_key,
        source_session_epoch: selected.source_session_epoch,
        music_folder_key: selected.music_folder_key,
        music_folder_object_id: selected.music_folder_object_id.clone(),
        music_folders: Arc::clone(&selected.music_folders),
        playlist_tracks_can_repeat: selected.configuration.playlist_tracks_can_repeat(),
        artwork,
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
        sources::EditableSource::Credentials {
            credentials,
            jellyfin_use_instant_mix,
            subsonic_authentication,
            ..
        } => Ok(EditableSource {
            source: SourceSummary {
                id: configuration.source_id.clone(),
                kind: configuration.kind.clone(),
                name: configuration.name.clone(),
                transcoded_download_bitrate_limit_kbps: configuration
                    .transcoded_download_bitrate_limit_kbps(),
            },
            credentials: CredentialPreset {
                source_name: credentials.server_name,
                server_url: credentials.server_url,
                username: credentials.username,
                trust_invalid_cert: credentials.trust_invalid_cert,
                open_subsonic_authentication: subsonic_authentication
                    .map(open_subsonic_authentication),
            },
            jellyfin_use_instant_mix,
        }),
        sources::EditableSource::Local { .. } => {
            Err("Local folders are edited from the Local source panel".to_string())
        }
    }
}

fn source_setup_input(input: SourceSetup, jellyfin_device_id: &str) -> SourceSetupInput {
    match input {
        SourceSetup::Jellyfin {
            credentials,
            use_instant_mix,
        } => SourceSetupInput::Jellyfin(JellyfinSetupInput {
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
            authentication: subsonic_authentication(authentication),
            credentials: credential_host_input(credentials),
        },
        SourceSetup::Local { roots } => SourceSetupInput::Local(LocalFolderHostInput { roots }),
    }
}

fn source_settings_input(input: SourceSettingsChange) -> SourceSettingsInput {
    match input {
        SourceSettingsChange::Jellyfin {
            source_id: _,
            credentials,
            use_instant_mix,
        } => SourceSettingsInput::Jellyfin(JellyfinSettingsInput {
            credentials: credential_settings_input(credentials),
            use_instant_mix,
        }),
        SourceSettingsChange::OpenSubsonic {
            source_id: _,
            kind: _,
            authentication,
            credentials,
        } => SourceSettingsInput::Subsonic {
            authentication: subsonic_authentication(authentication),
            credentials: credential_settings_input(credentials),
        },
    }
}

fn source_settings_id(input: &SourceSettingsChange) -> &SourceId {
    match input {
        SourceSettingsChange::Jellyfin { source_id, .. }
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

fn subsonic_authentication(authentication: OpenSubsonicAuthentication) -> SubsonicAuthentication {
    match authentication {
        OpenSubsonicAuthentication::Password => SubsonicAuthentication::Password,
        OpenSubsonicAuthentication::ApiKey => SubsonicAuthentication::ApiKey,
    }
}

fn open_subsonic_authentication(
    authentication: SubsonicAuthentication,
) -> OpenSubsonicAuthentication {
    match authentication {
        SubsonicAuthentication::Password => OpenSubsonicAuthentication::Password,
        SubsonicAuthentication::ApiKey => OpenSubsonicAuthentication::ApiKey,
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

fn string_error(error: impl std::fmt::Display) -> String {
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

    fn key(revision: u64) -> ArtworkPreparationKey {
        let mut digest = [0; 32];
        digest[..8].copy_from_slice(&revision.to_le_bytes());
        ArtworkPreparationKey {
            source: SourceKey::from_raw(1),
            epoch: SourceSessionEpoch::new(1),
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
