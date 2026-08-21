//! The configured sources and the one selected source session.
//!
//! Rufin owns selection and operation ordering here. Concrete sources acquire
//! facts and perform provider operations; Library accepts and queries them.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use artwork::{Artwork, SourceImages};
use async_channel::{Receiver, Sender};
use library::{
    AcceptedHomeChange, AcceptedLibraryChange, CandidateChange, FavoriteAcceptance, FavoriteItemId,
    FolderContents, HomeSectionKind, HomeSnapshot, Libraries, Library, LocalAccessTarget,
    MetadataDraft, MetadataEdit, MetadataEditing, MetadataError, MetadataItemId, MusicFolderId,
    PlaylistEdit, PlaylistTrackAdd, PreparedSourceCandidate, RecordedActivity,
    SmartPlaylistBuiltin, SmartPlaylistDefinition, SmartPlaylistId, SourceId, Track, TrackSort,
};
use playback::{PlaybackProjection, SourceSessionEpoch};
use scrobbling::Scrobbler;
use secrets::{SecretStorageMode, SwitchableSecretStore};
use sources::{
    CredentialHostInput, CredentialSettingsInput, JellyfinSettingsInput, JellyfinSetupInput,
    LocalFolderHostInput, NativeSourceResult, ObservedSourceChange, PreparedSourceChange, Source,
    SourceCacheMatch, SourceConfiguration, SourceEditResult, SourceFreshness, SourceInputIdentity,
    SourceReadProgress, SourceReadStage, SourceSettingsInput, SourceSetupInput,
    SubsonicAuthentication, SubsonicFlavor,
};
use tracing::warn;
use ui::runtime::source::{
    ConfiguredSources, CredentialInput, CredentialPreset, DiscoveredServer, DiscoveryStatus,
    DiscoveryUpdate, EditableSource, LocalFolder, OpenSubsonicAuthentication, OpenSubsonicKind,
    SelectedSourcePort, SourceLocalAccess, SourceLocalAccessSummary, SourceOperation, SourcePort,
    SourceProgress, SourceProgressStage, SourceSettingsChange, SourceSetup, SourceSummary,
};
use ui::runtime::{
    HomePublication, SelectedLibrary, SelectedLibraryUpdate, SourceEvent, SourceNotice,
    SourceNoticeKind,
};

use crate::album_release::run_selected_album_release_lookup;
use crate::playback::PlaybackOwner;
use crate::settings::{
    ConfiguredSource, CredentialRef, SettingsFile, SourceSettings, StoredSettings, all_secret_keys,
    delete_provider_secret, fresh_credential_ref, fresh_secret_scope_id, load_provider_secret,
    load_scrobbling_settings, platform_secret_store, save_provider_secret,
};
use downloads::Downloads;

const SOURCE_CHECK_INTERVAL: Duration = Duration::from_secs(5 * 60);
const FAVORITE_RETRY_BASE: u64 = 30;
const FAVORITE_RETRY_MAX: u64 = 5 * 60;
const FAVORITE_RETRY_BATCH: usize = 100;

mod connection;
mod local_access;
mod observer;

use connection::{
    add_source, configured_source, prepare_configured_refresh_candidate, prepare_refresh_candidate,
    select_source,
};
use local_access::ActiveLocalAccess;
#[cfg(test)]
use local_access::accept_metadata_local_access_mapping;
#[cfg(test)]
use observer::ObservedChangeRun;
use observer::{
    ActiveObserver, ConfiguredJellyfinFeed, FreshnessAdmission, PendingChanges, RefreshRequest,
};

#[cfg(test)]
mod tests;

/// The current immutable facts for one selected-source session.
///
/// Rufin replaces this value atomically when the source executor, accepted
/// Library, Home, or music-folder scope changes. Consumers resolve it through
/// [`ActiveSource`] instead of retaining a second mutable mirror.
#[derive(Clone)]
pub(crate) struct SelectedSourceState {
    pub(crate) configuration: SourceConfiguration,
    pub(crate) source: Option<Arc<Source>>,
    pub(crate) source_session_epoch: SourceSessionEpoch,
    pub(crate) library: Arc<Library>,
    pub(crate) home: Arc<HomeSnapshot>,
    pub(crate) music_folder_id: Option<MusicFolderId>,
}

impl SelectedSourceState {
    pub(crate) fn source_id(&self) -> &SourceId {
        &self.configuration.source_id
    }

    fn qualifier(&self) -> SourceQualifier {
        SourceQualifier {
            source_id: self.source_id().clone(),
            epoch: self.source_session_epoch,
        }
    }

    fn metadata_context(
        &self,
        item_id: &MetadataItemId,
    ) -> Result<Option<MetadataContext>, library::LibraryQueryError> {
        let Some(source) = self.source.as_ref().cloned() else {
            return Ok(None);
        };
        let Some(subject) = self.library.metadata_subject(item_id)? else {
            return Ok(None);
        };
        Ok(Some(MetadataContext {
            source,
            subject,
            local_access: None,
        }))
    }

    fn metadata_editing_available(&self, item_id: &MetadataItemId) -> bool {
        let Some(source) = self.source.as_ref() else {
            return false;
        };
        self.library
            .metadata_item(item_id)
            .ok()
            .flatten()
            .is_some_and(|item| source.metadata_editing_available(&item))
    }

    fn metadata_access_context(
        &self,
        item_id: &MetadataItemId,
    ) -> Result<Option<MetadataContext>, MetadataError> {
        let Some(source) = self.source.as_ref().cloned() else {
            return Ok(None);
        };
        if source.needs_metadata_local_access() {
            let Some((subject, local_access)) = self
                .library
                .metadata_subject_with_local_access(item_id, None)?
            else {
                return Ok(None);
            };
            return Ok(Some(MetadataContext {
                source,
                subject,
                local_access: Some(local_access),
            }));
        }
        let Some(subject) = self
            .library
            .metadata_subject(item_id)
            .map_err(|error| MetadataError::Write(error.to_string()))?
        else {
            return Ok(None);
        };
        Ok(Some(MetadataContext {
            source,
            subject,
            local_access: None,
        }))
    }
}

struct MetadataContext {
    source: Arc<Source>,
    subject: library::MetadataSubject,
    local_access: Option<Vec<LocalAccessTarget>>,
}

/// A stable selected-session identity and fence.
///
/// The handle owns no selected facts. Resolving consults SourceOwner's one
/// authoritative slot, and returns `None` as soon as that session is retired.
pub(crate) struct ActiveSource {
    shared: Weak<Shared>,
    source_id: SourceId,
    source_session_epoch: SourceSessionEpoch,
    retirement: tokio::sync::watch::Sender<bool>,
    #[cfg(test)]
    fixed: Mutex<Option<Arc<SelectedSourceState>>>,
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
            #[cfg(test)]
            fixed: Mutex::new(None),
        })
    }

    pub(crate) fn resolve(&self) -> Option<Arc<SelectedSourceState>> {
        #[cfg(test)]
        if let Some(selected) = self
            .fixed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .cloned()
        {
            return Some(selected);
        }
        self.shared
            .upgrade()?
            .resolve_selected(&self.source_id, self.source_session_epoch)
    }

    pub(crate) fn downgrade(self: &Arc<Self>) -> WeakActiveSource {
        Arc::downgrade(self)
    }

    #[cfg(test)]
    pub(crate) fn fixed_for_test(state: SelectedSourceState) -> Arc<Self> {
        let source_id = state.source_id().clone();
        let source_session_epoch = state.source_session_epoch;
        let (retirement, _) = tokio::sync::watch::channel(false);
        Arc::new(Self {
            shared: Weak::new(),
            source_id,
            source_session_epoch,
            retirement,
            fixed: Mutex::new(Some(Arc::new(state))),
        })
    }

    #[cfg(test)]
    pub(crate) fn replace_for_test(&self, state: SelectedSourceState) {
        *self
            .fixed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::new(state));
    }

    fn retire(&self) {
        self.retirement.send_replace(true);
    }
}

fn resolve_observer_session(
    cancelled: &AtomicBool,
    session: &ActiveSource,
) -> Option<Arc<SelectedSourceState>> {
    (!cancelled.load(Ordering::Acquire))
        .then(|| session.resolve())
        .flatten()
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

pub(crate) type SourceAcceptanceSender = SourceOwner;

struct Shared {
    artwork: Artwork,
    library: Libraries,
    downloads: Downloads,
    settings: SettingsFile,
    secrets: Arc<SwitchableSecretStore>,
    scrobbler: Arc<Scrobbler>,
    runtime: tokio::runtime::Handle,
    outputs: SourceOutputs,
    state: Mutex<OwnerState>,
    lane: tokio::sync::Mutex<()>,
    acceptance_lane: tokio::sync::Mutex<()>,
    interruptible: Mutex<Vec<InterruptibleTask>>,
    playback: Mutex<Weak<PlaybackOwner>>,
    next_epoch: AtomicU64,
    next_token: AtomicU64,
    started: AtomicBool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceQualifier {
    source_id: SourceId,
    epoch: SourceSessionEpoch,
}

struct MetadataReply {
    sender: Option<Sender<Result<(), MetadataError>>>,
    write_started: bool,
}

impl MetadataReply {
    fn new(sender: Sender<Result<(), MetadataError>>) -> Self {
        Self {
            sender: Some(sender),
            write_started: false,
        }
    }

    fn mark_write_started(&mut self) {
        self.write_started = true;
    }

    fn finish(mut self, result: Result<(), MetadataError>) {
        if let Some(sender) = self.sender.take() {
            let _ = sender.try_send(result);
        }
    }
}

impl Drop for MetadataReply {
    fn drop(&mut self) {
        let Some(sender) = self.sender.take() else {
            return;
        };
        let error = if self.write_started {
            MetadataError::SavedRefreshFailed(
                "Metadata editing was interrupted before the written metadata was accepted."
                    .to_string(),
            )
        } else {
            MetadataError::Unavailable
        };
        let _ = sender.try_send(Err(error));
    }
}

enum SmartPlaylistOperation {
    Create {
        name: String,
        definition: SmartPlaylistDefinition,
    },
    Update {
        id: SmartPlaylistId,
        name: String,
        definition: SmartPlaylistDefinition,
    },
    Delete(SmartPlaylistId),
    Restore(SmartPlaylistBuiltin),
    Move {
        dragged: SmartPlaylistId,
        target: SmartPlaylistId,
        after: bool,
    },
}

struct SelectedSlot {
    session: Arc<ActiveSource>,
    current: Arc<SelectedSourceState>,
}

struct InterruptibleTask {
    token: u64,
    cancelled: Arc<AtomicBool>,
    handle: Option<tokio::task::AbortHandle>,
}

#[derive(Clone, Copy)]
struct InterruptibleRegistration {
    token: u64,
}

struct ActiveInterruptible {
    shared: Arc<Shared>,
    registration: InterruptibleRegistration,
}

impl Drop for ActiveInterruptible {
    fn drop(&mut self) {
        self.shared
            .unregister_interruptible(self.registration.token);
    }
}

struct OwnerState {
    selected: Option<SelectedSlot>,
    observer: Option<ActiveObserver>,
    jellyfin_feeds: BTreeMap<SourceId, ConfiguredJellyfinFeed>,
    local_access: Option<ActiveLocalAccess>,
    selected_revealed: bool,
    active_album_release: Option<Arc<AtomicBool>>,
    refresh: Option<Arc<RefreshRequest>>,
    freshness: FreshnessAdmission,
}

impl SourceOwner {
    pub(crate) fn open_dormant(
        artwork: Artwork,
        library: Libraries,
        downloads: Downloads,
        settings: SettingsFile,
        secrets: Arc<SwitchableSecretStore>,
        scrobbler: Arc<Scrobbler>,
        runtime: tokio::runtime::Handle,
        outputs: SourceOutputs,
    ) -> SourceBootstrap {
        let stored = settings.load();
        let operation = match stored.sources.selected_source_id.clone() {
            Some(target) => SourceOperation::Switching {
                target,
                progress: initial_progress(),
            },
            None => SourceOperation::Idle,
        };
        let shared = Arc::new(Shared {
            artwork,
            library,
            downloads,
            settings,
            secrets,
            scrobbler,
            runtime,
            outputs,
            state: Mutex::new(OwnerState {
                selected: None,
                observer: None,
                jellyfin_feeds: BTreeMap::new(),
                local_access: None,
                selected_revealed: false,
                active_album_release: None,
                refresh: None,
                freshness: FreshnessAdmission::new(tokio::time::Instant::now()),
            }),
            lane: tokio::sync::Mutex::new(()),
            acceptance_lane: tokio::sync::Mutex::new(()),
            interruptible: Mutex::new(Vec::new()),
            playback: Mutex::new(Weak::new()),
            next_epoch: AtomicU64::new(1),
            next_token: AtomicU64::new(1),
            started: AtomicBool::new(false),
        });
        let owner = Arc::new(Self { shared });
        SourceBootstrap {
            configured: configured_sources(&stored, None),
            operation,
            owner,
        }
    }

    pub(crate) fn attach_playback(&self, playback: &Arc<PlaybackOwner>) {
        *self
            .shared
            .playback
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::downgrade(playback);
    }

    pub(crate) fn acceptance_sender(&self) -> SourceAcceptanceSender {
        self.clone()
    }

    pub(crate) fn start(&self) -> Result<(), String> {
        if self.shared.started.swap(true, Ordering::AcqRel) {
            return Err("the source owner is already running".to_string());
        }
        let periodic = self.clone();
        self.shared.runtime.spawn(async move {
            let mut interval = tokio::time::interval(SOURCE_CHECK_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            interval.tick().await;
            loop {
                interval.tick().await;
                periodic.request_favorite_retry();
                periodic.request_freshness_check(false);
            }
        });
        if let Some(source_id) = self.shared.settings.load().sources.selected_source_id {
            SourcePort::select_source(self, source_id);
        }
        Ok(())
    }

    pub(crate) fn album_release_settings_changed(&self, enabled: bool) {
        self.spawn_serialized(false, move |mut operations, _| async move {
            if enabled {
                operations.start_album_release_lookup();
            } else {
                operations.cancel_album_release_lookup(false);
            }
        });
    }

    fn request_favorite_retry(&self) {
        self.spawn_serialized(false, |mut operations, _| async move {
            let Some(selected) = operations
                .shared
                .selected()
                .filter(|selected| selected.source.is_some())
            else {
                return;
            };
            operations.retry_remote_favorites(selected).await;
        });
    }

    fn spawn_serialized<F, Work>(&self, interruptible: bool, work: F)
    where
        F: FnOnce(SourceOwner, Arc<AtomicBool>) -> Work + Send + 'static,
        Work: Future<Output = ()> + Send + 'static,
    {
        self.spawn_serialized_with_cancel(interruptible, Arc::new(AtomicBool::new(false)), work);
    }

    fn spawn_serialized_with_cancel<F, Work>(
        &self,
        interruptible: bool,
        cancelled: Arc<AtomicBool>,
        work: F,
    ) where
        F: FnOnce(SourceOwner, Arc<AtomicBool>) -> Work + Send + 'static,
        Work: Future<Output = ()> + Send + 'static,
    {
        let registration =
            interruptible.then(|| self.shared.register_interruptible(Arc::clone(&cancelled)));
        self.spawn_registered(registration, cancelled, work);
    }

    fn spawn_registered<F, Work>(
        &self,
        registration: Option<InterruptibleRegistration>,
        cancelled: Arc<AtomicBool>,
        work: F,
    ) where
        F: FnOnce(SourceOwner, Arc<AtomicBool>) -> Work + Send + 'static,
        Work: Future<Output = ()> + Send + 'static,
    {
        let shared = Arc::clone(&self.shared);
        let Some(registration) = registration else {
            self.shared.runtime.spawn(async move {
                let lane_owner = Arc::clone(&shared);
                let _lane = lane_owner.lane.lock().await;
                if cancelled.load(Ordering::Acquire) {
                    return;
                }
                work(
                    SourceOwner {
                        shared: Arc::clone(&shared),
                    },
                    cancelled,
                )
                .await;
            });
            return;
        };
        let active = ActiveInterruptible {
            shared: Arc::clone(&shared),
            registration,
        };
        let (start, started) = tokio::sync::oneshot::channel();
        let handle = self.shared.runtime.spawn(async move {
            let _active = active;
            if started.await.is_err() {
                return;
            }
            let lane_owner = Arc::clone(&shared);
            let _lane = lane_owner.lane.lock().await;
            if cancelled.load(Ordering::Acquire) {
                return;
            }
            work(
                SourceOwner {
                    shared: Arc::clone(&shared),
                },
                cancelled,
            )
            .await;
        });
        let attached = self
            .shared
            .attach_interruptible(registration.token, handle.abort_handle());
        if attached {
            let _ = start.send(());
        }
    }

    fn spawn_transition<F, Work>(
        &self,
        operation: SourceOperation,
        failure_source: Option<SourceId>,
        add_form: bool,
        work: F,
    ) where
        F: FnOnce(SourceOwner, Arc<AtomicBool>) -> Work + Send + 'static,
        Work: Future<Output = Result<(), String>> + Send + 'static,
    {
        self.shared.cancel_interruptible();
        self.spawn_serialized(true, move |mut operations, cancelled| async move {
            operations
                .shared
                .send_event(SourceEvent::Operation(operation))
                .await;
            if let Err(error) = work(operations.clone(), Arc::clone(&cancelled)).await
                && !cancelled.load(Ordering::Acquire)
            {
                operations
                    .fail_transition(failure_source, error, add_form)
                    .await;
            }
        });
    }
}

impl SourceOwner {
    pub(crate) fn publish_activity(
        &self,
        source_id: SourceId,
        source_session_epoch: SourceSessionEpoch,
        update: RecordedActivity,
    ) {
        let shared = Arc::clone(&self.shared);
        self.shared.runtime.spawn(async move {
            SourceOwner { shared }
                .accept_activity(
                    SourceQualifier {
                        source_id,
                        epoch: source_session_epoch,
                    },
                    update,
                )
                .await;
        });
    }
}

impl SourceOwner {
    async fn start_selected_access(&mut self, catch_up: bool) {
        let Some(session) = self.shared.selected_session() else {
            return;
        };
        let Some(selected) = session.resolve() else {
            return;
        };
        let qualifier = selected.qualifier();
        let local = selected.configuration.is_local();
        {
            let mut state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if local
                && state
                    .observer
                    .as_ref()
                    .is_some_and(|observer| observer.qualifier == qualifier)
            {
                return;
            }
            if !catch_up {
                state.freshness.defer(tokio::time::Instant::now());
            }
        }
        self.stop_observer().await;
        self.start_local_access_refresh(&selected).await;
        if !local {
            if selected.configuration.kind == "jellyfin" {
                self.resume_configured_feed(selected.source_id());
            } else if catch_up {
                SourceOwner {
                    shared: Arc::clone(&self.shared),
                }
                .request_freshness_check(true);
            }
            return;
        }
        let Some(source) = selected.source.as_ref().cloned() else {
            return;
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let changes = Arc::new(Mutex::new(PendingChanges::new(None, true)));
        let change_cancelled = Arc::clone(&cancelled);
        let stop_cancelled = Arc::clone(&cancelled);
        let change_owner = Arc::downgrade(&self.shared);
        let change_state = Arc::clone(&changes);
        let change_session = Arc::clone(&session);
        let stop_session = Arc::clone(&session);
        let (completed, completion) = tokio::sync::oneshot::channel();
        let handle = match std::thread::Builder::new()
            .name("rufin-local-watcher".to_string())
            .spawn(move || {
                let result = source.listen_local_changes(
                    catch_up,
                    move |change| {
                        change_owner.upgrade().is_some_and(|shared| {
                            SourceOwner { shared }.queue_observed_change(
                                &change_state,
                                &change_session,
                                &change_cancelled,
                                change,
                            )
                        })
                    },
                    move || {
                        stop_cancelled.load(Ordering::Acquire) || stop_session.resolve().is_none()
                    },
                );
                if let Err(error) = result {
                    warn!(%error, "selected Local source change feed stopped");
                }
                let _ = completed.send(());
            }) {
            Ok(handle) => handle,
            Err(error) => {
                warn!(%error, "could not start the Local library watcher thread");
                return;
            }
        };
        let observer = ActiveObserver {
            qualifier: qualifier.clone(),
            cancelled: Arc::clone(&cancelled),
            completion: Some(completion),
            handle: Some(handle),
        };
        let stale = {
            let mut state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state
                .selected
                .as_ref()
                .is_some_and(|slot| Arc::ptr_eq(&slot.session, &session))
            {
                state.observer = Some(observer);
                None
            } else {
                Some(observer)
            }
        };
        if let Some(observer) = stale {
            observer.stop().await;
        }
    }

    async fn retire_selected_access(&self) {
        let (observer, local_access) = {
            let mut state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (state.observer.take(), state.local_access.take())
        };
        drop(local_access);
        if let Some(observer) = observer {
            observer.stop().await;
        }
    }

    async fn begin_transition(&mut self) {
        self.retire_selected_session().await;
        self.shared.release_selected().await;
    }

    async fn retire_selected_session(&mut self) {
        self.cancel_album_release_lookup(true);
        self.retire_selected_access().await;
        self.shared.stop_playback().await;
    }

    fn start_album_release_lookup(&mut self) {
        let Some(session) = self.shared.selected_session() else {
            return;
        };
        let Some(selected) = session.resolve() else {
            return;
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        {
            let mut state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !state.selected_revealed
                || state.active_album_release.is_some()
                || !self
                    .shared
                    .settings
                    .load()
                    .ui
                    .allows_external_metadata_lookup()
            {
                return;
            }
            state.active_album_release = Some(Arc::clone(&cancelled));
        }
        let settings = self.shared.settings.clone();
        let events = self.shared.outputs.events.clone();
        let source_id = selected.source_id().clone();
        let source_session_epoch = selected.source_session_epoch;
        let selected = session.downgrade();
        let shared = Arc::clone(&self.shared);
        let active = Arc::clone(&cancelled);
        drop(self.shared.runtime.spawn_blocking(move || {
            run_selected_album_release_lookup(
                settings,
                events,
                source_id,
                source_session_epoch,
                selected,
                cancelled,
            );
            let mut state = shared
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state
                .active_album_release
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &active))
            {
                state.active_album_release = None;
            }
        }));
    }

    fn cancel_album_release_lookup(&mut self, reset_reveal: bool) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(active) = state.active_album_release.take() {
            active.store(true, Ordering::Release);
        }
        if reset_reveal {
            state.selected_revealed = false;
        }
    }

    fn progress<F>(
        &self,
        cancelled: Arc<AtomicBool>,
        operation: F,
    ) -> Arc<dyn Fn(SourceReadProgress) + Send + Sync>
    where
        F: Fn(SourceProgress) -> Option<SourceOperation> + Send + Sync + 'static,
    {
        let events = self.shared.outputs.events.clone();
        Arc::new(move |progress| {
            if !cancelled.load(Ordering::Acquire)
                && let Some(operation) = operation(source_progress(progress))
            {
                let _ = events.try_send(SourceEvent::Operation(operation));
            }
        })
    }

    async fn accept_activity(&mut self, qualifier: SourceQualifier, update: RecordedActivity) {
        let acceptance_owner = Arc::clone(&self.shared);
        let _acceptance = acceptance_owner.acceptance_lane.lock().await;
        let Some(selected) = self
            .shared
            .selected()
            .filter(|selected| selected.qualifier() == qualifier)
        else {
            return;
        };
        let library = Arc::clone(&selected.library);
        match blocking(move || {
            library
                .apply_recorded_activity(&update)
                .map_err(string_error)
        })
        .await
        {
            Ok(Some(change)) => self.publish_accepted_change(&selected, change).await,
            Ok(None) => {}
            Err(error) => warn!(%error, "could not apply accepted playback activity"),
        }
    }

    async fn selected_update_failed(&mut self, error: String) {
        self.shared.warn_nonfatal(&error);
        self.shared
            .send_event(SourceEvent::Operation(SourceOperation::Idle))
            .await;
        self.start_selected_access(true).await;
    }

    async fn accept_prepared_change(
        &mut self,
        selected: Arc<SelectedSourceState>,
        prepared: PreparedSourceChange,
    ) -> Result<(), String> {
        let change = match prepared {
            PreparedSourceChange::SourceUpdate(update) => {
                let library = Arc::clone(&selected.library);
                blocking(move || library.accept_source_update(update).map_err(string_error)).await?
            }
            PreparedSourceChange::LocalReplacement(replacement) => {
                let library = Arc::clone(&selected.library);
                blocking(move || {
                    library
                        .accept_local_component(replacement)
                        .map_err(string_error)
                })
                .await?
            }
            PreparedSourceChange::Full | PreparedSourceChange::Ignored => {
                return Err("the source change was not prepared for exact acceptance".to_string());
            }
        };
        if let Some(change) = change {
            self.publish_accepted_change(&selected, change).await;
        }
        Ok(())
    }

    async fn refresh_home(&mut self, selected: Arc<SelectedSourceState>, kind: HomeSectionKind) {
        let source_section = match selected.source.as_ref() {
            Some(source) => match source.home_section(kind).await.map_err(string_error) {
                Ok(NativeSourceResult::Available(section)) => Some(section),
                Ok(NativeSourceResult::Unavailable) => None,
                Err(error) => {
                    self.shared.warn_nonfatal(&error);
                    return;
                }
            },
            None => None,
        };
        if !self.shared.matches_selected(&selected.qualifier()) {
            return;
        }
        let library = Arc::clone(&selected.library);
        let folder = selected.music_folder_id.clone();
        let current = Arc::clone(&selected.home);
        let home = blocking(move || {
            match source_section {
                Some(section) => library.accept_home_section(folder.as_ref(), &current, section),
                None => library.refresh_rufin_home_section(folder.as_ref(), &current, kind),
            }
            .map_err(string_error)
        })
        .await;
        match home {
            Ok(home) => {
                let mut replacement = (*selected).clone();
                replacement.home = Arc::clone(&home);
                if self.shared.replace_selected(replacement) {
                    self.shared
                        .send_event(SourceEvent::Home(HomePublication {
                            source_id: selected.source_id().clone(),
                            source_session_epoch: selected.source_session_epoch,
                            kind,
                            home,
                        }))
                        .await;
                }
            }
            Err(error) => self.shared.warn_nonfatal(&error),
        }
    }

    async fn set_favorite(
        &mut self,
        selected: Arc<SelectedSourceState>,
        item: FavoriteItemId,
        favorite: bool,
    ) {
        if selected.configuration.is_local() {
            let library = Arc::clone(&selected.library);
            let acceptance = FavoriteAcceptance::RufinOwned { item, favorite };
            match blocking(move || library.accept_favorite(acceptance).map_err(string_error)).await
            {
                Ok(change) => self.publish_accepted_change(&selected, change).await,
                Err(error) => self.shared.warn_nonfatal(&error),
            }
            return;
        }

        let library = Arc::clone(&selected.library);
        let queued_item = item.clone();
        let queued = blocking(move || {
            library
                .queue_remote_favorite(queued_item, favorite, unix_seconds())
                .map_err(string_error)
        })
        .await;
        let change = match queued {
            Ok(change) => change,
            Err(error) => {
                self.shared.warn_nonfatal(&error);
                return;
            }
        };
        self.publish_accepted_change(&selected, change).await;
        let pending = library::PendingFavorite {
            item,
            favorite,
            attempts: 0,
        };
        match selected.source.clone() {
            Some(source) => {
                self.deliver_remote_favorite(&selected, source, pending, true)
                    .await;
            }
            None => {
                self.defer_remote_favorite(&selected, &pending).await;
                self.send_source_notice(&selected, SourceNoticeKind::ServerUnreachable)
                    .await;
            }
        }
    }

    async fn set_rating(
        &mut self,
        selected: Arc<SelectedSourceState>,
        item: FavoriteItemId,
        rating: Option<u8>,
    ) {
        let library = Arc::clone(&selected.library);
        let accepted_item = item.clone();
        match blocking(move || {
            library
                .set_rating(accepted_item, rating)
                .map_err(string_error)
        })
        .await
        {
            Ok(change) => self.publish_accepted_change(&selected, change).await,
            Err(error) => {
                self.shared.warn_nonfatal(&error);
                return;
            }
        }
        if let Some(source) = selected.source.as_ref() {
            let result = if selected.configuration.is_local() {
                let library = Arc::clone(&selected.library);
                let source = Arc::clone(source);
                let track_id = match item {
                    FavoriteItemId::Track(id) => Some(id),
                    FavoriteItemId::Album(_) | FavoriteItemId::Artist(_) => None,
                };
                blocking(move || {
                    let Some(track_id) = track_id else {
                        return Ok(NativeSourceResult::Unavailable);
                    };
                    let track = library
                        .track(&track_id)
                        .map_err(string_error)?
                        .ok_or_else(|| "the rated Track is no longer in the Library".to_string())?;
                    source
                        .set_local_rating(&track, rating)
                        .map_err(string_error)
                })
                .await
            } else {
                source.set_rating(item, rating).await.map_err(string_error)
            };
            if let Err(error) = result {
                self.shared.warn_nonfatal(&error);
            }
        }
    }

    async fn retry_remote_favorites(&mut self, selected: Arc<SelectedSourceState>) {
        let Some(source) = selected.source.clone() else {
            return;
        };
        if selected.configuration.is_local() {
            return;
        }
        let library = Arc::clone(&selected.library);
        let due = blocking(move || {
            library
                .due_remote_favorites(unix_seconds(), FAVORITE_RETRY_BATCH)
                .map_err(string_error)
        })
        .await;
        let due = match due {
            Ok(due) => due,
            Err(error) => {
                self.shared.warn_nonfatal(&error);
                return;
            }
        };
        for pending in due {
            if !self
                .deliver_remote_favorite(&selected, Arc::clone(&source), pending, false)
                .await
            {
                break;
            }
        }
    }

    async fn deliver_remote_favorite(
        &mut self,
        selected: &SelectedSourceState,
        source: Arc<Source>,
        pending: library::PendingFavorite,
        notify: bool,
    ) -> bool {
        match source
            .set_favorite(pending.item.clone(), pending.favorite)
            .await
        {
            Ok(_) => {
                let library = Arc::clone(&selected.library);
                let item = pending.item;
                if let Err(error) = blocking(move || {
                    library
                        .complete_remote_favorite(item, pending.favorite)
                        .map_err(string_error)
                })
                .await
                {
                    self.shared.warn_nonfatal(&error);
                }
                true
            }
            Err(error) if source_error_is_temporary(&error) => {
                warn!(%error, "favorite delivery will be retried");
                self.defer_remote_favorite(selected, &pending).await;
                if notify {
                    self.send_source_notice(selected, SourceNoticeKind::ServerUnreachable)
                        .await;
                }
                false
            }
            Err(error) => {
                warn!(%error, "favorite delivery was rejected");
                let library = Arc::clone(&selected.library);
                let item = pending.item;
                let rejected = blocking(move || {
                    library
                        .reject_remote_favorite(item, pending.favorite)
                        .map_err(string_error)
                })
                .await;
                match rejected {
                    Ok(Some(change)) => self.publish_accepted_change(selected, change).await,
                    Ok(None) => {}
                    Err(error) => self.shared.warn_nonfatal(&error),
                }
                if notify {
                    self.send_source_notice(selected, SourceNoticeKind::FavoriteRejected)
                        .await;
                }
                true
            }
        }
    }

    async fn defer_remote_favorite(
        &self,
        selected: &SelectedSourceState,
        pending: &library::PendingFavorite,
    ) {
        let delay = FAVORITE_RETRY_BASE
            .saturating_mul(1_u64 << pending.attempts.min(3))
            .min(FAVORITE_RETRY_MAX);
        let next_attempt_at = unix_seconds().saturating_add(delay as i64);
        let library = Arc::clone(&selected.library);
        let item = pending.item.clone();
        let favorite = pending.favorite;
        if let Err(error) = blocking(move || {
            library
                .defer_remote_favorite(item, favorite, next_attempt_at)
                .map_err(string_error)
        })
        .await
        {
            self.shared.warn_nonfatal(&error);
        }
    }

    async fn send_source_notice(&self, selected: &SelectedSourceState, kind: SourceNoticeKind) {
        self.shared
            .send_event(SourceEvent::Notice(SourceNotice {
                source_id: selected.source_id().clone(),
                source_session_epoch: selected.source_session_epoch,
                kind,
            }))
            .await;
    }

    async fn edit_playlist(&mut self, selected: Arc<SelectedSourceState>, edit: PlaylistEdit) {
        let acceptance = match selected.source.as_ref() {
            Some(source) => source.edit_playlist(edit).await.map_err(string_error),
            None => Err(source_access_unavailable()),
        };
        let acceptance = match acceptance {
            Ok(acceptance) => acceptance,
            Err(error) => {
                self.shared.warn_nonfatal(&error);
                return;
            }
        };
        if !self.shared.matches_selected(&selected.qualifier()) {
            return;
        }
        let library = Arc::clone(&selected.library);
        match blocking(move || library.accept_playlist(acceptance).map_err(string_error)).await {
            Ok(Some(change)) => self.publish_accepted_change(&selected, change).await,
            Ok(None) => {}
            Err(error) => self.shared.warn_nonfatal(&error),
        }
    }

    async fn smart_playlist(
        &mut self,
        selected: Arc<SelectedSourceState>,
        operation: SmartPlaylistOperation,
    ) {
        let library = Arc::clone(&selected.library);
        let change = blocking(move || match operation {
            SmartPlaylistOperation::Create { name, definition } => library
                .create_smart_playlist(name, definition)
                .map_err(string_error),
            SmartPlaylistOperation::Update {
                id,
                name,
                definition,
            } => library
                .update_smart_playlist(id, name, definition)
                .map_err(string_error),
            SmartPlaylistOperation::Delete(id) => {
                library.delete_smart_playlist(&id).map_err(string_error)
            }
            SmartPlaylistOperation::Restore(builtin) => library
                .restore_builtin_smart_playlist(builtin)
                .map_err(string_error),
            SmartPlaylistOperation::Move {
                dragged,
                target,
                after,
            } => library
                .move_smart_playlist_relative(dragged, target, after)
                .map_err(string_error),
        })
        .await;
        match change {
            Ok(Some(change)) => self.publish_accepted_change(&selected, change).await,
            Ok(None) => {}
            Err(error) => self.shared.warn_nonfatal(&error),
        }
    }

    async fn edit_metadata(
        &mut self,
        selected: Arc<SelectedSourceState>,
        edit: MetadataEdit,
        cancelled: Arc<AtomicBool>,
        mut reply: MetadataReply,
    ) {
        let progress = self.progress(Arc::clone(&cancelled), |_| None);
        let prepared = match selected.metadata_access_context(&edit.item_id) {
            Err(error) => Err(error),
            Ok(None) => Err(MetadataError::Unavailable),
            Ok(Some(context)) => {
                reply.mark_write_started();
                context
                    .source
                    .write_metadata(
                        Arc::clone(&selected.library),
                        context.subject,
                        edit,
                        context.local_access,
                        progress,
                        Arc::clone(&cancelled),
                    )
                    .await
            }
        };
        let accepted = match prepared {
            Err(error) => Err(error),
            Ok(PreparedSourceChange::Full) => {
                let candidate = prepare_refresh_candidate(
                    Arc::clone(&self.shared),
                    (*selected).clone(),
                    self.progress(Arc::clone(&cancelled), |_| None),
                    Arc::clone(&cancelled),
                )
                .await
                .map_err(MetadataError::SavedRefreshFailed);
                match candidate {
                    Err(error) => Err(error),
                    Ok(candidate) => {
                        let acceptance_owner = Arc::clone(&self.shared);
                        let _acceptance = acceptance_owner.acceptance_lane.lock().await;
                        self.commit_refresh(selected, candidate)
                            .await
                            .map_err(MetadataError::SavedRefreshFailed)
                    }
                }
            }
            Ok(PreparedSourceChange::Ignored) => Err(MetadataError::SavedRefreshFailed(
                "the source did not return the written item".to_string(),
            )),
            Ok(prepared) => {
                let acceptance_owner = Arc::clone(&self.shared);
                let _acceptance = acceptance_owner.acceptance_lane.lock().await;
                self.accept_prepared_change(selected, prepared)
                    .await
                    .map_err(MetadataError::SavedRefreshFailed)
            }
        };
        reply.finish(accepted);
    }

    async fn publish_accepted_change(
        &mut self,
        selected: &SelectedSourceState,
        change: AcceptedLibraryChange,
    ) {
        if !self.shared.matches_selected(&selected.qualifier()) {
            return;
        }
        let downloads_changed = change.download_coverage_changed;
        let album_release_candidates_changed = change.album_release_candidates_changed;
        if album_release_candidates_changed {
            self.cancel_album_release_lookup(false);
        }
        let tracks = change
            .tracks
            .iter()
            .filter_map(|replacement| replacement.track.clone())
            .collect::<Vec<_>>();
        if !tracks.is_empty() {
            let refreshed = match self.shared.playback() {
                Ok(playback) => {
                    let source_id = selected.source_id().clone();
                    let epoch = selected.source_session_epoch;
                    blocking(move || playback.refresh_accepted_tracks(&source_id, epoch, tracks))
                        .await
                }
                Err(error) => Err(error),
            };
            match refreshed {
                Ok(()) => {}
                Err(error) => {
                    self.shared.warn_nonfatal(&error);
                }
            }
        }
        let home = if change.home == AcceptedHomeChange::Keep {
            None
        } else {
            let library = Arc::clone(&selected.library);
            let current = Arc::clone(&selected.home);
            let folder = selected.music_folder_id.clone();
            let home_change = change.home.clone();
            match blocking(move || {
                library
                    .home_after_accepted_change(folder.as_ref(), &current, &home_change)
                    .map_err(string_error)
            })
            .await
            {
                Ok(home) => home,
                Err(error) => {
                    warn!(%error, source_id = %selected.source_id(), "could not prepare Home after an accepted Library change");
                    None
                }
            }
        };
        if let Some(home) = &home {
            let Some(active) = self
                .shared
                .selected()
                .filter(|active| active.qualifier() == selected.qualifier())
            else {
                return;
            };
            let mut replacement = (*active).clone();
            replacement.home = Arc::clone(&home);
            self.shared.replace_selected(replacement);
        }
        if downloads_changed {
            self.shared
                .downloads
                .library_changed(Arc::clone(&selected.library), change.clone());
        }
        self.shared
            .send_event(SourceEvent::LibraryUpdate(SelectedLibraryUpdate {
                source_id: selected.source_id().clone(),
                source_session_epoch: selected.source_session_epoch,
                change,
                home,
            }))
            .await;
        if album_release_candidates_changed {
            self.start_album_release_lookup();
        }
    }

    async fn fail_transition(
        &mut self,
        source_id: Option<SourceId>,
        message: String,
        add_form: bool,
    ) {
        if let Some(selected) = self
            .shared
            .selected()
            .filter(|selected| selected.configuration.kind == "jellyfin")
        {
            self.resume_configured_feed(selected.source_id());
        }
        self.shared
            .send_event(SourceEvent::Operation(SourceOperation::Failed {
                source_id,
                message,
                add_form,
            }))
            .await;
    }
}

impl Shared {
    fn selected_session(&self) -> Option<Arc<ActiveSource>> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .selected
            .as_ref()
            .map(|slot| Arc::clone(&slot.session))
    }

    fn selected(&self) -> Option<Arc<SelectedSourceState>> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .selected
            .as_ref()
            .map(|slot| Arc::clone(&slot.current))
    }

    fn resolve_selected(
        &self,
        source_id: &SourceId,
        epoch: SourceSessionEpoch,
    ) -> Option<Arc<SelectedSourceState>> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .selected
            .as_ref()
            .filter(|slot| {
                slot.current.source_id() == source_id && slot.current.source_session_epoch == epoch
            })
            .map(|slot| Arc::clone(&slot.current))
    }

    fn replace_selected(&self, selected: SelectedSourceState) -> bool {
        let qualifier = selected.qualifier();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(slot) = state
            .selected
            .as_mut()
            .filter(|slot| slot.current.qualifier() == qualifier)
        else {
            return false;
        };
        slot.current = Arc::new(selected);
        true
    }

    fn install_selected_slot(
        &self,
        session: Arc<ActiveSource>,
        selected: Arc<SelectedSourceState>,
    ) {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .selected = Some(SelectedSlot {
            session,
            current: selected,
        });
    }

    fn matches_selected(&self, qualifier: &SourceQualifier) -> bool {
        self.selected()
            .is_some_and(|selected| selected.qualifier() == *qualifier)
    }

    fn register_interruptible(&self, cancelled: Arc<AtomicBool>) -> InterruptibleRegistration {
        let registration = self.reserve_interruptible();
        self.register_reserved_interruptible(registration, cancelled);
        registration
    }

    fn reserve_interruptible(&self) -> InterruptibleRegistration {
        InterruptibleRegistration {
            token: self.next_token.fetch_add(1, Ordering::AcqRel),
        }
    }

    fn register_reserved_interruptible(
        &self,
        registration: InterruptibleRegistration,
        cancelled: Arc<AtomicBool>,
    ) {
        self.interruptible
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(InterruptibleTask {
                token: registration.token,
                cancelled,
                handle: None,
            });
    }

    fn attach_interruptible(&self, token: u64, handle: tokio::task::AbortHandle) -> bool {
        let mut handle = Some(handle);
        let attached = {
            let mut active = self
                .interruptible
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(entry) = active.iter_mut().find(|entry| entry.token == token)
                && !entry.cancelled.load(Ordering::Acquire)
            {
                entry.handle = handle.take();
                true
            } else {
                active.retain(|entry| entry.token != token);
                false
            }
        };
        if let Some(handle) = handle {
            handle.abort();
        }
        attached
    }

    fn unregister_interruptible(&self, token: u64) {
        self.interruptible
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|entry| entry.token != token);
    }

    fn protect_interruptible_commit(&self, cancelled: &Arc<AtomicBool>) -> bool {
        let mut active = self
            .interruptible
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(entry) = active
            .iter_mut()
            .find(|entry| Arc::ptr_eq(&entry.cancelled, cancelled))
        else {
            return false;
        };
        if entry.cancelled.load(Ordering::Acquire) {
            return false;
        }
        entry.handle = None;
        true
    }

    fn cancel_interruptible(&self) {
        let (refresh, active) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.freshness.cancel();
            let refresh = state.refresh.take();
            let active = std::mem::take(
                &mut *self
                    .interruptible
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
            );
            if let Some(refresh) = &refresh {
                refresh.cancelled.store(true, Ordering::Release);
            }
            for entry in &active {
                entry.cancelled.store(true, Ordering::Release);
            }
            (refresh, active)
        };
        drop(refresh);
        for mut entry in active {
            if let Some(handle) = entry.handle.take() {
                handle.abort();
            }
        }
    }

    fn finish_refresh(&self, request: &Arc<RefreshRequest>) -> Option<bool> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state
            .refresh
            .as_ref()
            .is_some_and(|active| Arc::ptr_eq(active, request))
        {
            state.refresh = None;
            return Some(request.visible.load(Ordering::Acquire));
        }
        None
    }

    fn finish_freshness_check(&self, token: u64) {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .freshness
            .finish(token);
    }

    fn playback(&self) -> Result<Arc<PlaybackOwner>, String> {
        self.playback
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .upgrade()
            .ok_or_else(|| "Playback is not attached to the selected source owner".to_string())
    }

    async fn publish_selected(
        &self,
        session: Arc<ActiveSource>,
        selected: Arc<SelectedSourceState>,
        playback: PlaybackProjection,
    ) {
        if let Ok(playback_owner) = self.playback() {
            playback_owner.publish_selected_products(&playback);
        }
        let stored = self.settings.load();
        let selected = ui_selected(selected, session);
        self.send_event(SourceEvent::Selected {
            configured: configured_sources(&stored, Some(&selected)),
            selected,
            playback: Box::new(playback),
        })
        .await;
    }

    async fn publish_library_replacement(&self, selected: SelectedSourceState) {
        if !self.replace_selected_runtime(selected).await {
            return;
        }
        let Some(session) = self.selected_session() else {
            return;
        };
        let Some(selected) = session.resolve() else {
            return;
        };
        let selected = ui_selected(selected, session);
        self.send_event(SourceEvent::LibraryReplaced {
            configured: configured_sources(&self.settings.load(), Some(&selected)),
            selected,
        })
        .await;
    }

    async fn publish_home_replacement(&self, selected: SelectedSourceState) {
        let source_id = selected.source_id().clone();
        let source_session_epoch = selected.source_session_epoch;
        let home = Arc::clone(&selected.home);
        if !self.replace_selected(selected) {
            return;
        }
        self.send_event(SourceEvent::HomeReplaced {
            source_id,
            source_session_epoch,
            home,
        })
        .await;
    }

    async fn replace_selected_runtime(&self, selected: SelectedSourceState) -> bool {
        if !self.replace_selected(selected) {
            return false;
        }
        if let Some(selected) = self.selected() {
            self.attach_selected_downloads(&selected).await;
        }
        true
    }

    async fn attach_selected_downloads(&self, selected: &SelectedSourceState) {
        if let Err(error) = self
            .downloads
            .attach(
                selected.source.clone(),
                &selected.library,
                selected.music_folder_id.clone(),
            )
            .await
        {
            self.warn_nonfatal(&error);
        }
    }

    async fn publish_configured(&self) {
        let selected = self.selected_session().and_then(|session| {
            let selected = session.resolve()?;
            Some(ui_selected(selected, session))
        });
        self.send_event(SourceEvent::Configured(configured_sources(
            &self.settings.load(),
            selected.as_ref(),
        )))
        .await;
    }

    async fn stop_playback(&self) {
        let Ok(playback) = self.playback() else {
            return;
        };
        if let Err(error) = blocking(move || Ok(playback.stop_for_source_switch())).await {
            warn!(%error, "could not stop Playback for a source transition");
        }
    }

    async fn release_selected(&self) {
        if self.selected().is_none() {
            return;
        }
        let (acknowledged, acknowledgement) = async_channel::bounded(1);
        if self
            .outputs
            .events
            .send(SourceEvent::ReleaseSelected { acknowledged })
            .await
            .is_ok()
        {
            let _ = acknowledgement.recv().await;
        }
        let (selected, observer, local_access) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (
                state.selected.take(),
                state.observer.take(),
                state.local_access.take(),
            )
        };
        if let Some(selected) = &selected {
            selected.session.retire();
        }
        drop(selected);
        drop(local_access);
        if let Some(observer) = observer {
            observer.stop().await;
        }
    }

    fn warn_nonfatal(&self, error: &str) {
        warn!(%error, "operation was not available");
    }

    async fn send_event(&self, event: SourceEvent) {
        if self.outputs.events.send(event).await.is_err() {
            warn!("source event lane is unavailable");
        }
    }
}

async fn blocking<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(string_error)?
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
        self.spawn_transition(
            SourceOperation::Adding {
                progress: initial_progress(),
            },
            None,
            true,
            move |mut operations, cancelled| async move {
                let progress = operations.progress(Arc::clone(&cancelled), |progress| {
                    Some(SourceOperation::Adding { progress })
                });
                add_source(&mut operations, input, progress, cancelled).await
            },
        );
    }

    fn update_source(&self, input: SourceSettingsChange) {
        let source_id = source_settings_id(&input).clone();
        let input = source_settings_input(input);
        self.spawn_serialized(true, move |mut operations, cancelled| async move {
            operations
                .apply_source_update(source_id, input, false, cancelled)
                .await;
        });
    }

    fn select_source(&self, source_id: SourceId) {
        if self
            .shared
            .selected()
            .is_some_and(|selected| selected.source_id() == &source_id)
        {
            self.spawn_serialized(false, |operations, _| async move {
                operations
                    .shared
                    .send_event(SourceEvent::Operation(SourceOperation::Idle))
                    .await;
            });
            return;
        }
        let target = source_id.clone();
        self.spawn_transition(
            SourceOperation::Switching {
                target: source_id.clone(),
                progress: initial_progress(),
            },
            Some(source_id.clone()),
            false,
            move |mut operations, cancelled| async move {
                operations.begin_transition().await;
                let configured =
                    configured_source(&operations.shared.settings.load().sources, &source_id)?;
                let progress_target = target.clone();
                let progress = operations.progress(Arc::clone(&cancelled), move |progress| {
                    Some(SourceOperation::Switching {
                        target: progress_target.clone(),
                        progress,
                    })
                });
                select_source(&mut operations, configured, progress, cancelled, true).await
            },
        );
    }

    fn change_secret_storage(&self, mode: SecretStorageMode) -> Receiver<Result<(), String>> {
        let (result, receiver) = async_channel::bounded(1);
        let stored = self.shared.settings.load();
        let reconnects_selected = stored.ui.secret_storage_mode != mode
            && self.shared.selected().is_some_and(|selected| {
                matches!(
                    selected.configuration.editable(),
                    Ok(sources::EditableSource::Credentials { .. })
                )
            });
        if reconnects_selected {
            self.shared.cancel_interruptible();
        }
        self.spawn_serialized(false, move |mut operations, _| async move {
            let changed = operations.apply_secret_storage_change(mode).await;
            let _ = result.send(changed).await;
        });
        receiver
    }

    fn add_local_folder(&self, path: PathBuf) {
        let local = self
            .shared
            .settings
            .load()
            .sources
            .configured
            .iter()
            .find(|source| {
                matches!(
                    source.configuration.editable(),
                    Ok(sources::EditableSource::Local { .. })
                )
            })
            .cloned();
        let Some(local) = local else {
            self.configure_source(SourceSetup::Local { roots: vec![path] });
            return;
        };
        let mut roots = match local_roots(&local.configuration) {
            Ok(roots) => roots,
            Err(error) => {
                self.shared.warn_nonfatal(&error);
                return;
            }
        };
        if roots.contains(&path) {
            return;
        }
        roots.push(path);
        let source_id = local.configuration.source_id;
        self.spawn_serialized(true, move |mut operations, cancelled| async move {
            operations
                .apply_source_update(
                    source_id,
                    SourceSettingsInput::Local { roots },
                    true,
                    cancelled,
                )
                .await;
        });
    }

    fn replace_local_folder(&self, current: String, replacement: PathBuf) {
        let local = self
            .shared
            .settings
            .load()
            .sources
            .configured
            .iter()
            .find(|source| {
                matches!(
                    source.configuration.editable(),
                    Ok(sources::EditableSource::Local { .. })
                )
            })
            .cloned();
        let Some(local) = local else {
            self.shared.warn_nonfatal("Local is not configured");
            return;
        };
        let mut roots = match local_roots(&local.configuration) {
            Ok(roots) => roots,
            Err(error) => {
                self.shared.warn_nonfatal(&error);
                return;
            }
        };
        let Some(root) = roots
            .iter_mut()
            .find(|root| root.to_string_lossy() == current)
        else {
            self.shared
                .warn_nonfatal("The Local folder is no longer configured");
            return;
        };
        if root == &replacement {
            return;
        }
        *root = replacement;
        let source_id = local.configuration.source_id;
        self.spawn_serialized(true, move |mut operations, cancelled| async move {
            operations
                .apply_source_update(
                    source_id,
                    SourceSettingsInput::Local { roots },
                    true,
                    cancelled,
                )
                .await;
        });
    }

    fn remove_local_folder(&self, path: String) {
        let local = self
            .shared
            .settings
            .load()
            .sources
            .configured
            .iter()
            .find(|source| {
                matches!(
                    source.configuration.editable(),
                    Ok(sources::EditableSource::Local { .. })
                )
            })
            .cloned();
        let Some(local) = local else {
            self.shared.warn_nonfatal("Local is not configured");
            return;
        };
        let mut roots = match local_roots(&local.configuration) {
            Ok(roots) => roots,
            Err(error) => {
                self.shared.warn_nonfatal(&error);
                return;
            }
        };
        roots.retain(|root| root.to_string_lossy() != path);
        let source_id = local.configuration.source_id;
        if roots.is_empty() {
            self.forget_source(source_id);
            return;
        }
        self.spawn_serialized(true, move |mut operations, cancelled| async move {
            operations
                .apply_source_update(
                    source_id,
                    SourceSettingsInput::Local { roots },
                    true,
                    cancelled,
                )
                .await;
        });
    }

    fn refresh_source(&self, source_id: SourceId) {
        self.request_manual_refresh(source_id);
    }

    fn check_for_source_changes(&self) {
        self.request_freshness_check(false);
    }

    fn save_local_access(&self, input: SourceLocalAccess) -> Receiver<Result<(), String>> {
        let (result, receiver) = async_channel::bounded(1);
        self.spawn_serialized(false, move |mut operations, _| async move {
            operations.apply_local_access(input, result).await;
        });
        receiver
    }

    fn clear_local_access(&self, source_id: SourceId) {
        self.spawn_serialized(false, move |mut operations, _| async move {
            operations.remove_local_access(source_id).await;
        });
    }

    fn forget_source(&self, source_id: SourceId) {
        if self
            .shared
            .selected()
            .is_some_and(|selected| selected.source_id() == &source_id)
        {
            self.shared.cancel_interruptible();
        }
        self.spawn_serialized(false, move |mut operations, _| async move {
            operations.forget_now(source_id).await;
        });
    }
}

impl ActiveSource {
    fn owner(&self) -> Option<SourceOwner> {
        Some(SourceOwner {
            shared: self.shared.upgrade()?,
        })
    }

    fn spawn_selected<F, Work>(&self, interruptible: bool, work: F)
    where
        F: FnOnce(SourceOwner, Arc<SelectedSourceState>, Arc<AtomicBool>) -> Work + Send + 'static,
        Work: Future<Output = ()> + Send + 'static,
    {
        let Some(owner) = self.owner() else {
            return;
        };
        let source_id = self.source_id.clone();
        let epoch = self.source_session_epoch;
        owner.spawn_serialized(interruptible, move |operations, cancelled| async move {
            let Some(selected) = operations.shared.resolve_selected(&source_id, epoch) else {
                return;
            };
            work(operations, selected, cancelled).await;
        });
    }

    fn spawn_reply<T, F, Work>(&self, work: F) -> Receiver<T>
    where
        T: Send + 'static,
        F: FnOnce(Arc<Shared>, SourceQualifier) -> Work + Send + 'static,
        Work: Future<Output = T> + Send + 'static,
    {
        let (sender, receiver) = async_channel::bounded(1);
        let Some(shared) = self.shared.upgrade() else {
            return receiver;
        };
        let qualifier = SourceQualifier {
            source_id: self.source_id.clone(),
            epoch: self.source_session_epoch,
        };
        if !shared.matches_selected(&qualifier) {
            return receiver;
        }
        let mut retirement = self.retirement.subscribe();
        let runtime = shared.runtime.clone();
        runtime.spawn(async move {
            let value = tokio::select! {
                biased;
                _ = retirement.wait_for(|retired| *retired) => return,
                _ = sender.closed() => return,
                value = work(Arc::clone(&shared), qualifier.clone()) => value,
            };
            if shared.matches_selected(&qualifier) {
                let _ = sender.send(value).await;
            }
        });
        receiver
    }
}

impl SelectedSourcePort for ActiveSource {
    fn selected_library_revealed(&self) {
        self.spawn_selected(false, |mut operations, selected, _| async move {
            operations
                .shared
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .selected_revealed = true;
            operations.resume_configured_feed(selected.source_id());
            operations.start_album_release_lookup();
        });
    }

    fn refresh_library(&self) {
        self.spawn_selected(false, |operations, selected, _| async move {
            SourceOwner {
                shared: Arc::clone(&operations.shared),
            }
            .request_refresh(selected.source_id().clone(), true);
        });
    }

    fn refresh_home(&self, kind: HomeSectionKind) {
        self.spawn_selected(false, move |mut operations, selected, _| async move {
            operations.refresh_home(selected, kind).await;
        });
    }

    fn set_music_folder(&self, folder_id: Option<MusicFolderId>) {
        self.spawn_selected(false, move |mut operations, selected, _| async move {
            operations.set_music_folder(selected, folder_id).await;
        });
    }

    fn set_favorite(&self, item: FavoriteItemId, favorite: bool) {
        self.spawn_selected(false, move |mut operations, selected, _| async move {
            operations.set_favorite(selected, item, favorite).await;
        });
    }

    fn set_rating(&self, item: FavoriteItemId, rating: Option<u8>) {
        self.spawn_selected(false, move |mut operations, selected, _| async move {
            operations.set_rating(selected, item, rating).await;
        });
    }

    fn add_playlist_tracks(&self, request: PlaylistTrackAdd) -> usize {
        let Some(selected) = self.resolve() else {
            return 0;
        };
        let edit = match selected.library.prepare_playlist_add(request) {
            Ok(Some(edit)) => edit,
            Ok(None) => return 0,
            Err(error) => {
                warn!(%error, "could not prepare playlist tracks");
                return 0;
            }
        };
        let count = match &edit {
            PlaylistEdit::AddTracks { track_ids, .. } => track_ids.len(),
            _ => 0,
        };
        self.edit_playlist(edit);
        count
    }

    fn edit_playlist(&self, edit: PlaylistEdit) {
        self.spawn_selected(false, move |mut operations, selected, _| async move {
            operations.edit_playlist(selected, edit).await;
        });
    }

    fn folder(
        &self,
        folder_id: Option<library::FolderId>,
        music_folder_id: Option<MusicFolderId>,
    ) -> Receiver<Result<FolderContents, String>> {
        let source = self.resolve().and_then(|selected| selected.source.clone());
        self.spawn_reply(move |shared, qualifier| async move {
            let provider = match source {
                Some(source) => Some(
                    source
                        .folder(folder_id.as_ref(), music_folder_id.as_ref())
                        .await,
                ),
                None => None,
            };
            let library = selected_reply_library(&shared, &qualifier)?;
            route_folder_result(
                library,
                folder_id.as_ref(),
                music_folder_id.as_ref(),
                provider,
            )
        })
    }

    fn search(
        &self,
        request: library::SearchRequest,
    ) -> Receiver<Result<library::SearchResults, String>> {
        let source = self.resolve().and_then(|selected| selected.source.clone());
        self.spawn_reply(move |shared, qualifier| async move {
            let provider = match source {
                Some(source) => Some(source.search(&request).await),
                None => None,
            };
            let library = selected_reply_library(&shared, &qualifier)?;
            route_search_result(library, request, provider).await
        })
    }

    fn metadata_editing_available(&self, item_id: &MetadataItemId) -> bool {
        self.resolve()
            .is_some_and(|selected| selected.metadata_editing_available(item_id))
    }

    fn metadata(&self, item_id: MetadataItemId) -> Receiver<Result<MetadataDraft, MetadataError>> {
        self.spawn_reply(move |shared, qualifier| async move {
            let context = {
                let Some(selected) = shared.resolve_selected(&qualifier.source_id, qualifier.epoch)
                else {
                    return Err(MetadataError::Unavailable);
                };
                selected.metadata_access_context(&item_id)
            };
            match context {
                Ok(Some(context)) => {
                    context
                        .source
                        .read_metadata(context.subject, context.local_access)
                        .await
                }
                Ok(None) => Err(MetadataError::Unavailable),
                Err(error) => Err(error),
            }
        })
    }

    fn edit_metadata(&self, edit: MetadataEdit) -> Receiver<Result<(), MetadataError>> {
        let (result, receiver) = async_channel::bounded(1);
        let reply = MetadataReply::new(result);
        if edit.changes.is_empty() && edit.application.is_none() {
            reply.finish(Ok(()));
            return receiver;
        }
        self.spawn_selected(
            false,
            move |mut operations, selected, cancelled| async move {
                operations
                    .edit_metadata(selected, edit, cancelled, reply)
                    .await;
            },
        );
        receiver
    }

    fn identify_metadata(
        &self,
        item_id: MetadataItemId,
        editing: MetadataEditing,
        values: library::MetadataValues,
    ) -> Receiver<Result<Option<library::MetadataIdentification>, String>> {
        let external_lookup_allowed = self
            .shared
            .upgrade()
            .is_some_and(|shared| shared.settings.load().ui.allows_external_metadata_lookup());
        self.spawn_reply(move |shared, qualifier| async move {
            let context = {
                let Some(selected) = shared.resolve_selected(&qualifier.source_id, qualifier.epoch)
                else {
                    return Ok(None);
                };
                selected.metadata_context(&item_id).ok().flatten()
            };
            let Some(context) = context else {
                return Ok(None);
            };
            let direct_applicable =
                external_lookup_allowed && item_id.has_exact_musicbrainz_identity(&values);
            let source_search_applicable = context.source.metadata_source_search(&context.subject)
                && !values.title.trim().is_empty();
            if !direct_applicable && !source_search_applicable {
                Ok(None)
            } else {
                identify_metadata_with_fallback(
                    context.source,
                    context.subject,
                    item_id,
                    editing,
                    values,
                    direct_applicable,
                    source_search_applicable,
                )
                .await
            }
        })
    }

    fn save_metadata_local_access(
        &self,
        input: SourceLocalAccess,
        item_id: MetadataItemId,
    ) -> Receiver<Result<(), String>> {
        let (result, receiver) = async_channel::bounded(1);
        self.spawn_selected(false, move |mut operations, selected, _| async move {
            operations
                .save_metadata_local_access(selected, input, item_id, result)
                .await;
        });
        receiver
    }

    fn create_smart_playlist(&self, name: String, definition: SmartPlaylistDefinition) {
        self.spawn_selected(false, move |mut operations, selected, _| async move {
            operations
                .smart_playlist(
                    selected,
                    SmartPlaylistOperation::Create { name, definition },
                )
                .await;
        });
    }

    fn update_smart_playlist(
        &self,
        id: SmartPlaylistId,
        name: String,
        definition: SmartPlaylistDefinition,
    ) {
        self.spawn_selected(false, move |mut operations, selected, _| async move {
            operations
                .smart_playlist(
                    selected,
                    SmartPlaylistOperation::Update {
                        id,
                        name,
                        definition,
                    },
                )
                .await;
        });
    }

    fn delete_smart_playlist(&self, id: SmartPlaylistId) {
        self.spawn_selected(false, move |mut operations, selected, _| async move {
            operations
                .smart_playlist(selected, SmartPlaylistOperation::Delete(id))
                .await;
        });
    }

    fn restore_builtin_smart_playlist(&self, builtin: SmartPlaylistBuiltin) {
        self.spawn_selected(false, move |mut operations, selected, _| async move {
            operations
                .smart_playlist(selected, SmartPlaylistOperation::Restore(builtin))
                .await;
        });
    }

    fn move_smart_playlist(&self, dragged: SmartPlaylistId, target: SmartPlaylistId, after: bool) {
        self.spawn_selected(false, move |mut operations, selected, _| async move {
            operations
                .smart_playlist(
                    selected,
                    SmartPlaylistOperation::Move {
                        dragged,
                        target,
                        after,
                    },
                )
                .await;
        });
    }
}

fn selected_reply_library(
    shared: &Shared,
    qualifier: &SourceQualifier,
) -> Result<Arc<Library>, String> {
    shared
        .resolve_selected(&qualifier.source_id, qualifier.epoch)
        .map(|selected| Arc::clone(&selected.library))
        .ok_or_else(|| "the selected source changed".to_string())
}

async fn identify_metadata_with_fallback(
    source: Arc<Source>,
    subject: library::MetadataSubject,
    item_id: MetadataItemId,
    editing: MetadataEditing,
    current: library::MetadataValues,
    direct_applicable: bool,
    source_search_applicable: bool,
) -> Result<Option<library::MetadataIdentification>, String> {
    let direct_item_id = item_id;
    let direct_values = current.clone();
    let direct = async move {
        blocking(move || metadata_lookup::identify_metadata(&direct_item_id, &direct_values))
            .await
            .map(|candidate| candidate.map(library::MetadataIdentification::values))
    };
    let source_search_values = current.clone();
    let source_search = async move {
        source
            .identify_metadata(&subject, &source_search_values)
            .await
    };
    resolve_identification(
        direct_applicable,
        source_search_applicable,
        &editing,
        &current,
        direct,
        source_search,
    )
    .await
}

async fn resolve_identification<Direct, SourceSearch>(
    direct_applicable: bool,
    source_search_applicable: bool,
    editing: &MetadataEditing,
    current: &library::MetadataValues,
    direct: Direct,
    source_search: SourceSearch,
) -> Result<Option<library::MetadataIdentification>, String>
where
    Direct: Future<Output = Result<Option<library::MetadataIdentification>, String>>,
    SourceSearch: Future<Output = Result<Option<library::MetadataIdentification>, String>>,
{
    let mut source_failure = None;
    if source_search_applicable {
        match source_search.await {
            Ok(Some(candidate)) if editing.identification_changes(current, &candidate.values) => {
                return Ok(Some(candidate));
            }
            Ok(_) => {}
            Err(error) => source_failure = Some(error),
        }
    }
    if direct_applicable {
        return match direct.await {
            Ok(Some(candidate)) if editing.identification_changes(current, &candidate.values) => {
                Ok(Some(candidate))
            }
            Ok(_) => source_failure.map_or(Ok(None), Err),
            Err(error) => match source_failure {
                Some(source_error) => Err(source_error),
                None if source_search_applicable => Ok(None),
                None => Err(error),
            },
        };
    }
    match source_failure {
        Some(error) => Err(error),
        None => Ok(None),
    }
}

fn route_folder_result(
    loaded: Arc<Library>,
    folder_id: Option<&library::FolderId>,
    music_folder_id: Option<&MusicFolderId>,
    provider: Option<sources::SourceResult<NativeSourceResult<FolderContents>>>,
) -> Result<FolderContents, String> {
    match live_source_result(provider)? {
        Some(contents) => reconcile_folder_contents(&loaded, contents),
        None => cached_folder_contents(loaded, folder_id, music_folder_id),
    }
}

async fn route_search_result(
    loaded: Arc<Library>,
    request: library::SearchRequest,
    provider: Option<sources::SourceResult<NativeSourceResult<library::SearchResults>>>,
) -> Result<library::SearchResults, String> {
    match live_source_result(provider)? {
        Some(results) => hydrate_search_tracks(&loaded, results),
        None => cached_search(loaded, request).await,
    }
}

fn live_source_result<T>(
    result: Option<sources::SourceResult<NativeSourceResult<T>>>,
) -> Result<Option<T>, String> {
    match result {
        Some(Ok(NativeSourceResult::Available(value))) => Ok(Some(value)),
        Some(Ok(NativeSourceResult::Unavailable)) | None => Ok(None),
        Some(Err(error)) if source_error_allows_cache(&error) => Ok(None),
        Some(Err(error)) => Err(error.to_string()),
    }
}

fn cached_folder_contents(
    loaded: Arc<Library>,
    folder_id: Option<&library::FolderId>,
    music_folder_id: Option<&MusicFolderId>,
) -> Result<FolderContents, String> {
    let local = loaded
        .local_folder_contents(folder_id)
        .map_err(string_error)?;
    if folder_id.is_some() && local.is_some() {
        return Ok(local.unwrap_or_default());
    }
    if let Some(local) = local
        && (!local.folders.is_empty() || !local.tracks.is_empty())
    {
        return Ok(local);
    }
    let tracks = loaded
        .track_list(music_folder_id, TrackSort::Title, false)
        .and_then(|tracks| tracks.materialize_owned())
        .map_err(string_error)?;
    Ok(FolderContents {
        folders: Arc::from([]),
        tracks: tracks.into(),
    })
}

pub(crate) fn source_error_allows_cache(error: &sources::SourceError) -> bool {
    matches!(
        error,
        sources::SourceError::Network(_)
            | sources::SourceError::Server {
                status: 500..=599,
                ..
            }
    )
}

fn source_error_is_temporary(error: &sources::SourceError) -> bool {
    source_error_allows_cache(error)
        || matches!(
            error,
            sources::SourceError::Server {
                status: 408 | 425 | 429,
                ..
            }
        )
}

fn reconcile_folder_contents(
    loaded: &Library,
    mut contents: FolderContents,
) -> Result<FolderContents, String> {
    contents.tracks = contents
        .tracks
        .iter()
        .cloned()
        .map(|track| accepted_track_or(loaded, track))
        .collect::<Result<Vec<_>, _>>()?
        .into();
    Ok(contents)
}

fn hydrate_search_tracks(
    loaded: &Library,
    mut results: library::SearchResults,
) -> Result<library::SearchResults, String> {
    for track in &mut results.tracks {
        *track = accepted_track_or(loaded, track.clone())?;
    }
    Ok(results)
}

fn accepted_track_or(loaded: &Library, track: Track) -> Result<Track, String> {
    loaded
        .track(&track.id)
        .map(|accepted| accepted.unwrap_or(track))
        .map_err(string_error)
}

async fn cached_search(
    loaded: Arc<Library>,
    request: library::SearchRequest,
) -> Result<library::SearchResults, String> {
    blocking(move || loaded.search(&request).map_err(string_error)).await
}

fn normalize_music_folder(
    loaded: &Library,
    folder_id: Option<MusicFolderId>,
) -> Result<Option<MusicFolderId>, String> {
    let Some(folder_id) = folder_id else {
        return Ok(None);
    };
    loaded
        .contains_music_folder(&folder_id)
        .map(|present| present.then_some(folder_id))
        .map_err(string_error)
}

fn configured_sources(
    stored: &StoredSettings,
    selected: Option<&SelectedLibrary>,
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
            path: path.to_string_lossy().to_string(),
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
                });
            let (album_count, track_count) = selected
                .filter(|selected| selected.source_id == configured.configuration.source_id)
                .and_then(|selected| selected.library.counts().ok())
                .map(|counts| (counts.albums, counts.tracks))
                .unwrap_or_default();
            let status = access
                .as_ref()
                .and_then(|_| {
                    selected
                        .filter(|selected| selected.source_id == configured.configuration.source_id)
                        .and_then(|selected| selected.library.local_access_status().ok())
                })
                .unwrap_or_default();
            SourceLocalAccessSummary {
                source_id: configured.configuration.source_id.clone(),
                access,
                status,
                selected_music_folder_name: selected
                    .filter(|selected| selected.source_id == configured.configuration.source_id)
                    .and_then(|selected| {
                        let wanted = selected.music_folder_id.as_ref()?;
                        selected
                            .library
                            .music_folders()
                            .ok()?
                            .iter()
                            .find(|folder| &folder.id == wanted)
                            .map(|folder| folder.name.clone())
                    }),
                album_count,
                track_count,
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
    let playlist_tracks_can_repeat = selected.configuration.playlist_tracks_can_repeat();
    let artwork = selected.source.as_ref().map_or_else(
        || artwork::SourceImages::cache_only(selected.source_id().clone()),
        |source| artwork::SourceImages::new(Arc::clone(source)),
    );
    SelectedLibrary {
        source_id: selected.source_id().clone(),
        source_session_epoch: selected.source_session_epoch,
        music_folder_id: selected.music_folder_id.clone(),
        playlist_tracks_can_repeat,
        artwork,
        library: Arc::clone(&selected.library),
        home: Arc::clone(&selected.home),
        operations,
    }
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
