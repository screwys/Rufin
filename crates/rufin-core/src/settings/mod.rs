mod activity;
mod player;
mod preferences;
pub use app::{
    AppearancePreferences, ApplicationPreferences, ArtistImageProvider, CoverProvider,
    PlayerDisplayPreferences,
};
pub use player::{
    ExternalLyricsProvider, PlayerPreferenceOptions, PlayerPreferences, SettingsError,
};
pub use preferences::{CommittedPreferences, PreferenceSubscription};
pub mod app;
pub mod context_menu;
pub mod layout;
mod library_lists;
pub mod presentation;
pub mod right_panel;
mod storage_reset;
pub(crate) use storage_reset::device_secrets;
pub use storage_reset::{StorageReset, StorageResetRequest};
mod secret_storage;
pub use secret_storage::{KeyringSecretStore, SecretStorageMode};
pub mod sidebar;
pub mod visualizer;
mod web_controller;
pub use web_controller::ControllerSettings;

pub use app::{
    ExternalSiteLinkSettings, HomeBlockKind, HomeSectionKind, RandomPlayGenreSelection,
    RandomPlaySettings, Settings, SettingsHandle, default_home_blocks,
};
pub use context_menu::{ContextMenuItem, ContextMenuItemSettings, ContextMenuSettings};
pub use layout::{
    AccentPreference, DEFAULT_LEFT_SIDEBAR_WIDTH, DEFAULT_RIGHT_SIDEBAR_WIDTH,
    DEFAULT_WINDOW_HEIGHT, DEFAULT_WINDOW_WIDTH, LayoutProfile, LayoutSettings, LeftSidebarMode,
    LibraryField, LibraryLayout, LibraryListKey, LibraryListSettings, LibraryListSettingsEntry,
    MAX_LEFT_SIDEBAR_WIDTH, MAX_NARROW_LAYOUT_THRESHOLD, MAX_RESTORED_WINDOW_HEIGHT,
    MAX_RESTORED_WINDOW_WIDTH, MAX_RIGHT_SIDEBAR_WIDTH, MAX_TABLE_COLUMN_WIDTH,
    MIN_LEFT_SIDEBAR_WIDTH, MIN_NARROW_LAYOUT_THRESHOLD, MIN_RIGHT_SIDEBAR_WIDTH,
    MIN_TABLE_COLUMN_WIDTH, RightSidebarMode, ThemePreference, available_detail_track_fields,
    available_grid_fields, available_row_fields, available_sort_fields,
    default_library_list_settings, sanitized_window_size,
};
pub use sidebar::{
    HomeBlockRow, SidebarPin, SidebarRouteItem, SidebarRouteItemSettings, SidebarSettings,
};

use std::fs;
use std::io::{ErrorKind, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, mpsc};

use crate::scrobbling::Settings as ScrobblingSettings;
use app::Settings as UiSettings;
use playback::StreamQuality;
use secrets::{
    CachedSecretStore, ConfigSecretStore, SecretKey, SecretStore, SwitchableSecretStore,
};
use serde::{Deserialize, Serialize};
use sources::{SourceConfiguration, SourceId};
use tracing::warn;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum Credential {
    Sealed {
        encrypted: String,
        #[serde(skip)]
        session: bool,
    },
    Legacy(String),
}

impl Credential {
    #[cfg(test)]
    pub(crate) fn new(value: impl Into<String>) -> Self {
        Self::Legacy(value.into())
    }

    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::Sealed { encrypted, .. } => encrypted,
            Self::Legacy(reference) => reference,
        }
    }

    fn session(&self) -> bool {
        matches!(self, Self::Sealed { session: true, .. })
    }
}

pub(crate) fn fresh_source_id() -> Result<sources::SourceId, String> {
    random_identity("rufin-source-").map(sources::SourceId::new)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct ConfiguredSource {
    #[serde(flatten)]
    pub(crate) configuration: SourceConfiguration,
    #[serde(
        default,
        alias = "credential_ref",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) credential: Option<Credential>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) music_folder_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) local_access: Option<SavedLocalAccess>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) enable_half_stars: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct SavedLocalAccess {
    pub(crate) root_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) server_prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) local_prefix: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct SourceSettings {
    #[serde(default)]
    pub(crate) configured: Vec<ConfiguredSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) integrations: Vec<ConfiguredSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) selected_source_id: Option<SourceId>,
}

impl SourceSettings {
    pub(crate) fn connections(&self) -> impl Iterator<Item = &ConfiguredSource> {
        self.configured.iter().chain(&self.integrations)
    }
    pub(crate) fn connections_mut(&mut self) -> impl Iterator<Item = &mut ConfiguredSource> {
        self.configured.iter_mut().chain(&mut self.integrations)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct StoredSettings {
    #[serde(flatten)]
    pub ui: UiSettings,
    #[serde(default)]
    pub(crate) equalizer: crate::playback::EqualizerPreferences,
    #[serde(default)]
    pub(crate) scrobbling: ScrobblingSettings,
    #[serde(default)]
    pub(crate) scrobbling_secrets_present: bool,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub(crate) scrobbling_credentials: std::collections::BTreeMap<String, Credential>,
    #[serde(default)]
    pub(crate) sources: SourceSettings,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) secret_scope_id: String,
    #[serde(default)]
    pub(crate) jellyfin_device_id: String,
    #[serde(default)]
    visualizer_preset_bar_counts_updated: bool,
    /// Rises when a restore replaces the credentials kept outside the settings, so the feed
    /// tells it.
    #[serde(skip)]
    pub secrets_revision: u64,
    /// Rises when the web controller's token is replaced or the secret store switches, so the
    /// controller starts again with its token.
    #[serde(skip)]
    pub web_controller_token_revision: u64,
}

impl Default for StoredSettings {
    fn default() -> Self {
        Self {
            ui: UiSettings::default(),
            equalizer: Default::default(),
            scrobbling: ScrobblingSettings::default(),
            scrobbling_secrets_present: false,
            scrobbling_credentials: Default::default(),
            sources: SourceSettings::default(),
            secret_scope_id: String::new(),
            jellyfin_device_id: String::new(),
            visualizer_preset_bar_counts_updated: true,
            secrets_revision: 0,
            web_controller_token_revision: 0,
        }
    }
}

impl StoredSettings {
    /// Whether artwork and lyrics download automatically.
    pub(crate) fn cache_downloads(&self) -> (bool, bool) {
        (
            self.ui.application.download_artwork_automatically,
            self.ui.application.download_lyrics_automatically,
        )
    }

    pub(crate) fn migrate_defaults(&mut self) {
        self.scrobbling.sanitize();
        if !self.visualizer_preset_bar_counts_updated {
            self.ui.visualizer.migrate_preset_bar_counts();
            self.visualizer_preset_bar_counts_updated = true;
        }
        self.ui.visualizer.migrate_named_presets();
        app::migrate_library_lists(&mut self.ui.library_lists);
        self.ui.sanitize();
    }

    /// Whether `other` names the same scrobbling accounts and secrets; the switches may
    /// differ.
    pub(crate) fn same_scrobbling_accounts(&self, other: &Self) -> bool {
        self.scrobbling.same_accounts(&other.scrobbling)
            && other.scrobbling_credentials == self.scrobbling_credentials
            && other.scrobbling_secrets_present == self.scrobbling_secrets_present
            && other.ui.lastfm_api_key == self.ui.lastfm_api_key
            && other.secret_scope_id == self.secret_scope_id
            && other.secrets_revision == self.secrets_revision
    }

    pub(crate) fn scrobbling_runtime_settings(&self) -> ScrobblingSettings {
        let mut settings = self.scrobbling.clone();
        settings.lastfm.api_key = self.ui.lastfm_api_key.clone();
        settings
    }
}

/// Runs `pass` with the previous and the committed settings on every change while its owner
/// lives, as `features::follow` does for the Catalog's changes.
pub(crate) fn follow<T, Pass, Done>(
    runtime: &tokio::runtime::Handle,
    settings: &SettingsFile,
    owner: std::sync::Weak<T>,
    mut pass: Pass,
) -> tokio::task::AbortHandle
where
    T: Send + Sync + 'static,
    Pass: FnMut(Arc<T>, &StoredSettings, &StoredSettings) -> Done + Send + 'static,
    Done: std::future::Future<Output = ()> + Send,
{
    let mut committed = settings.changes();
    runtime
        .spawn(async move {
            let mut previous = Arc::clone(&committed.borrow_and_update());
            while committed.changed().await.is_ok() {
                let current = Arc::clone(&committed.borrow_and_update());
                let Some(owner) = owner.upgrade() else {
                    return;
                };
                pass(owner, &previous, &current).await;
                previous = current;
            }
        })
        .abort_handle()
}

#[derive(Clone)]
pub struct SettingsFile {
    /// The committed settings and every change, whichever writer made it: the one settings
    /// feed.
    committed: tokio::sync::watch::Sender<Arc<StoredSettings>>,
    path: Option<PathBuf>,
    config_dir: PathBuf,
    /// Held while a writer turns the committed settings into the next.
    writing: Arc<Mutex<()>>,
    persistence: Arc<Mutex<StoredSettings>>,
    edits: Arc<OnceLock<Result<mpsc::SyncSender<Self>, String>>>,
    persistence_error: tokio::sync::watch::Sender<Option<String>>,
    pub(crate) secret_storage_fallbacks: (
        async_channel::Sender<KeyringSecretStore>,
        async_channel::Receiver<KeyringSecretStore>,
    ),
}

impl SettingsFile {
    /// The committed settings, now and after every change.
    pub(crate) fn changes(&self) -> tokio::sync::watch::Receiver<Arc<StoredSettings>> {
        self.committed.subscribe()
    }

    pub(crate) fn config_dir(&self) -> &Path {
        &self.config_dir
    }
    pub(crate) fn open(path: PathBuf) -> Result<Self, String> {
        let (mut value, mut changed) = read_startup_settings(&path)?;
        value.migrate_defaults();
        for recent in &mut value.ui.recent_search_results {
            if let Some(binding) = &mut recent.artwork_binding {
                let normalized = library::normalize_artwork_binding(binding)
                    .map_err(|error| error.to_string())?;
                if *binding != normalized {
                    *binding = normalized;
                    changed = true;
                }
            }
        }
        if value.jellyfin_device_id.trim().is_empty() {
            match random_identity("rufin-") {
                Ok(identity) => {
                    value.jellyfin_device_id = identity;
                    changed = true;
                }
                Err(error) => warn!(%error, "could not create a Jellyfin device identity"),
            }
        }
        if changed {
            if let Err(error) = write_settings(&path, &value) {
                warn!(%error, "could not save startup settings");
            }
        }
        Ok(Self {
            committed: tokio::sync::watch::channel(Arc::new(value.clone())).0,
            config_dir: path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf(),
            path: Some(path),
            writing: Arc::default(),
            persistence: Arc::new(Mutex::new(value)),
            edits: Arc::new(OnceLock::new()),
            persistence_error: tokio::sync::watch::channel(None).0,
            secret_storage_fallbacks: async_channel::unbounded(),
        })
    }

    #[cfg(test)]
    pub(crate) fn memory() -> Self {
        Self::memory_at(PathBuf::from("."))
    }

    pub(crate) fn memory_at(config_dir: PathBuf) -> Self {
        let mut value = StoredSettings::default();
        value.migrate_defaults();
        value.jellyfin_device_id = random_identity("rufin-").unwrap_or_default();
        Self {
            path: None,
            committed: tokio::sync::watch::channel(Arc::new(value.clone())).0,
            config_dir,
            writing: Arc::default(),
            persistence: Arc::new(Mutex::new(value)),
            edits: Arc::new(OnceLock::new()),
            persistence_error: tokio::sync::watch::channel(None).0,
            secret_storage_fallbacks: async_channel::unbounded(),
        }
    }

    /// The committed settings.
    pub fn load(&self) -> Arc<StoredSettings> {
        self.committed.borrow().clone()
    }

    /// The committed settings as a reader holds them, without a copy.
    pub(crate) fn committed(&self) -> Committed {
        Committed(self.load())
    }

    pub(crate) fn equalizer_preset(
        &self,
        id: &str,
    ) -> Option<crate::playback::equalizer::SavedPreset> {
        self.committed
            .borrow()
            .equalizer
            .presets
            .iter()
            .find(|p| p.id == id)
            .cloned()
    }

    pub(crate) fn playback_stream_quality(&self) -> StreamQuality {
        self.committed.borrow().ui.playback.stream_quality
    }

    pub(crate) fn local_configuration(&self) -> Option<SourceConfiguration> {
        self.committed
            .borrow()
            .sources
            .configured
            .iter()
            .find(|source| source.configuration.is_local())
            .map(|source| source.configuration.clone())
    }

    /// Apply preferences immediately. The shared writer reports durable failures separately.
    pub(crate) fn edit<T>(
        &self,
        operation: impl FnOnce(&mut StoredSettings) -> Result<T, String>,
    ) -> Result<T, String> {
        let writer = if self.path.is_some() {
            Some(
                self.edits
                    .get_or_init(|| {
                        let (sender, receiver) = mpsc::sync_channel::<Self>(1);
                        std::thread::Builder::new()
                            .name("rufin-settings".into())
                            .spawn(move || {
                                while let Ok(file) = receiver.recv() {
                                    if let Err(error) = file.flush() {
                                        warn!(%error, "could not save settings");
                                    }
                                }
                            })
                            .map_err(|error| error.to_string())?;
                        Ok(sender)
                    })
                    .as_ref()
                    .map_err(Clone::clone)?,
            )
        } else {
            None
        };
        let _writing = self.writing.lock().unwrap_or_else(|p| p.into_inner());
        let current = self.load();
        let mut next = StoredSettings::clone(&current);
        let output = operation(&mut next)?;
        next.prepare()?;
        if next != *current {
            // The writer saves what is committed, so it hears of the edit after it.
            self.committed.send_replace(Arc::new(next));
            if let Some(writer) = writer {
                match writer.try_send(self.clone()) {
                    Ok(()) | Err(mpsc::TrySendError::Full(_)) => {}
                    Err(mpsc::TrySendError::Disconnected(_)) => {
                        return Err("Settings writer stopped".into());
                    }
                }
            }
        }
        Ok(output)
    }

    /// Commits an edit once it is saved, or returns the error. The edit sees the committed
    /// settings and nothing commits between it and its publication.
    pub(crate) fn update<T>(
        &self,
        operation: impl FnOnce(&mut StoredSettings) -> Result<T, String>,
    ) -> Result<T, String> {
        let _writing = self.writing.lock().unwrap_or_else(|p| p.into_inner());
        let current = self.load();
        let mut next = StoredSettings::clone(&current);
        let output = operation(&mut next)?;
        next.prepare()?;
        {
            let mut saved = self.persistence.lock().unwrap_or_else(|p| p.into_inner());
            if next != *saved {
                self.persist(&next)?;
                *saved = next.clone();
            }
        }
        if next != *current {
            self.committed.send_replace(Arc::new(next));
        }
        Ok(output)
    }

    pub(crate) fn flush(&self) -> Result<(), String> {
        let mut saved = self.persistence.lock().unwrap_or_else(|p| p.into_inner());
        let result = (|| {
            let next = self.load();
            if *next != *saved {
                self.persist(&next)?;
                *saved = StoredSettings::clone(&next);
            }
            Ok(())
        })();
        self.persistence_error
            .send_replace(result.as_ref().err().cloned());
        result
    }

    fn persist(&self, value: &StoredSettings) -> Result<(), String> {
        self.path
            .as_ref()
            .map_or(Ok(()), |path| write_settings(path, value))
    }
}

impl StoredSettings {
    fn prepare(&mut self) -> Result<(), String> {
        self.ui.sanitize();
        let backup = &mut self.ui.application.backup;
        if backup.enabled
            && backup.schedule.frequency == crate::backup::format::BackupFrequency::Off
        {
            backup.schedule.frequency = crate::backup::format::BackupFrequency::Daily;
        }
        if backup.schedule.frequency != crate::backup::format::BackupFrequency::Off
            && backup.schedule.schedule_id.is_empty()
        {
            backup.schedule.schedule_id = random_identity("schedule-")?;
        }
        backup.schedule.retention = 2;
        backup.schedule.hour = backup.schedule.hour.min(23);
        backup.schedule.weekday = backup.schedule.weekday.min(6);
        Ok(())
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
#[derive(Clone)]
pub struct SettingsOwner {
    file: SettingsFile,
    secrets: Arc<SwitchableSecretStore>,
}

impl SettingsOwner {
    pub fn local_configuration(&self) -> Option<sources::SourceConfiguration> {
        self.file.local_configuration()
    }

    pub(crate) fn new(file: SettingsFile, secrets: Arc<SwitchableSecretStore>) -> Arc<Self> {
        Arc::new(Self { file, secrets })
    }

    pub(crate) fn file(&self) -> &SettingsFile {
        &self.file
    }

    pub(crate) fn secrets(&self) -> &Arc<SwitchableSecretStore> {
        &self.secrets
    }

    /// Writes restored settings. `credentials_changed` says the secrets they name were
    /// replaced, which the feed tells by a new `secrets_revision`. The revisions count on
    /// whatever the restore puts in their place.
    pub(crate) fn restore(
        &self,
        restore: impl FnOnce(&mut StoredSettings) -> Result<(), String>,
        credentials_changed: bool,
    ) -> Result<(), String> {
        self.file.update(|stored| {
            let revisions = (
                stored.secrets_revision,
                stored.web_controller_token_revision,
            );
            restore(stored)?;
            stored.secrets_revision = revisions.0 + u64::from(credentials_changed);
            stored.web_controller_token_revision = revisions.1;
            Ok(())
        })
    }
}

impl SettingsFile {
    /// Tells the feed that the web controller's token was replaced.
    pub(crate) fn web_controller_token_replaced(&self) -> Result<(), String> {
        self.edit(|stored| {
            stored.web_controller_token_revision += 1;
            Ok(())
        })
    }
}

impl SettingsOwner {
    pub fn persistence_errors(&self) -> tokio::sync::watch::Receiver<Option<String>> {
        self.file.persistence_error.subscribe()
    }

    /// Finish pending preference saves before shutdown or export.
    pub fn flush(&self) -> Result<(), String> {
        self.file.flush()
    }

    /// The committed settings, now and after every change, whichever writer made it.
    pub fn changes(&self) -> tokio::sync::watch::Receiver<Arc<StoredSettings>> {
        self.file.changes()
    }

    /// The committed settings, shared rather than copied.
    pub fn load(&self) -> Committed {
        self.file.committed()
    }

    pub fn visualizer_render_settings(&self) -> (u32, visualizer::VisualizerAppearance) {
        let stored = self.file.load();
        (
            stored.ui.visualizer.fps_limit,
            stored.ui.visualizer.appearance.clone(),
        )
    }

    pub fn set_pinned(&self, pin: SidebarPin, pinned: bool) -> Result<bool, String> {
        self.file
            .update(|stored| Ok(stored.ui.sidebar.set_pinned(pin, pinned)))
    }

    pub fn set_route_visible(&self, item: SidebarRouteItem, visible: bool) -> Result<bool, String> {
        self.file
            .update(|stored| Ok(stored.ui.sidebar.set_route_visible(item, visible)))
    }

    pub fn move_route(&self, item: SidebarRouteItem, delta: isize) -> Result<bool, String> {
        self.file
            .update(|stored| Ok(stored.ui.sidebar.move_route(item, delta)))
    }

    pub fn remember_search_result(&self, result: app::RecentSearchResult) -> Result<bool, String> {
        if self.file.load().ui.recent_search_results.first() == Some(&result) {
            return Ok(false);
        }
        self.file
            .update(|stored| Ok(stored.ui.remember_search_result(result)))
    }

    pub fn remove_search_result(
        &self,
        kind: app::RecentSearchKind,
        media_uri: &str,
    ) -> Result<bool, String> {
        self.file.update(|stored| {
            let previous = stored.ui.recent_search_results.len();
            stored
                .ui
                .recent_search_results
                .retain(|result| result.kind != kind || result.media_uri != media_uri);
            Ok(previous != stored.ui.recent_search_results.len())
        })
    }

    pub fn clear_search_results(&self) -> Result<bool, String> {
        self.file.update(|stored| {
            let changed = !stored.ui.recent_search_results.is_empty();
            stored.ui.recent_search_results.clear();
            Ok(changed)
        })
    }

    pub fn reorder_pin(&self, moved: &SidebarPin, target: &SidebarPin) -> Result<bool, String> {
        self.file.update(|stored| {
            let Some(after) = stored.ui.sidebar.pin_drop_after(moved, target) else {
                return Ok(false);
            };
            Ok(stored.ui.sidebar.reorder_pin(moved, target, after))
        })
    }

    /// Apply an edit now; durable failures are published through `persistence_errors`.
    pub fn update_preferences(
        &self,
        edit: impl FnOnce(&mut UiSettings) -> Result<(), String>,
    ) -> Result<(), String> {
        self.file.edit(move |stored| edit(&mut stored.ui))
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl SettingsOwner {
    /// The Home blocks as the block editor lists them.
    pub fn home_block_rows(&self) -> Vec<sidebar::HomeBlockRow> {
        sidebar::home_block_rows(&self.file.load().ui.application.home_blocks)
    }

    pub fn set_home_block_visible(
        &self,
        block: library::HomeBlockKind,
        visible: bool,
    ) -> Result<bool, SettingsError> {
        self.file
            .update(|stored| {
                Ok(sidebar::set_home_block_visible(
                    &mut stored.ui.application.home_blocks,
                    block,
                    visible,
                ))
            })
            .map_err(Into::into)
    }

    pub fn move_home_block(
        &self,
        block: library::HomeBlockKind,
        delta: i32,
    ) -> Result<bool, SettingsError> {
        self.file
            .update(|stored| {
                Ok(sidebar::move_home_block(
                    &mut stored.ui.application.home_blocks,
                    block,
                    delta as isize,
                ))
            })
            .map_err(Into::into)
    }

    pub fn reorder_home_block(
        &self,
        source: library::HomeBlockKind,
        target: library::HomeBlockKind,
        after: bool,
    ) -> Result<bool, SettingsError> {
        self.file
            .update(|stored| {
                Ok(sidebar::reorder_home_block(
                    &mut stored.ui.application.home_blocks,
                    source,
                    target,
                    after,
                ))
            })
            .map_err(Into::into)
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl SettingsOwner {
    pub fn set_download_location(
        &self,
        source_id: SourceId,
        location: Option<crate::downloads::DownloadDirectory>,
    ) -> Result<(), SettingsError> {
        self.update_preferences(move |ui| {
            ui.set_download_location(source_id, location);
            Ok(())
        })
        .map_err(Into::into)
    }
}

impl SettingsOwner {}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl SettingsOwner {
    pub fn set_download_quality(
        &self,
        source_id: SourceId,
        quality: sources::DownloadQuality,
    ) -> Result<(), SettingsError> {
        self.update_preferences(move |ui| {
            ui.set_download_quality(source_id, quality);
            Ok(())
        })
        .map_err(Into::into)
    }
}

pub(crate) fn platform_secret_store(file: &SettingsFile) -> Arc<dyn SecretStore> {
    let settings = file.load();
    let config_dir = file.config_dir();
    match settings.ui.application.secret_storage_mode {
        SecretStorageMode::ConfigFile => Arc::new(CachedSecretStore::new(Arc::new(
            ConfigSecretStore::with_scope(
                config_dir.join("secrets.json"),
                settings.secret_scope_id.clone(),
            ),
        ))),
        SecretStorageMode::SystemKeyring => {
            Arc::new(secret_storage::KeyringSecretStore::new(file.clone()))
        }
    }
}

fn system_keyring_secret_store(scope_id: &str) -> Arc<dyn SecretStore> {
    match secrets::SystemKeyringStore::new(scope_id.to_string()) {
        Ok(store) => Arc::new(store),
        Err(error) => Arc::new(secrets::UnavailableSecretStore::new(error.to_string())),
    }
}

pub(crate) fn provider_secret_key(reference: &Credential) -> SecretKey {
    SecretKey::provider_token(reference.as_str())
}

pub(crate) fn backup_password_key() -> SecretKey {
    SecretKey::namespaced("backup", "password", "Rufin scheduled backup password")
}

pub(crate) fn backup_password_store(
    settings: &StoredSettings,
    config_dir: &Path,
) -> Arc<dyn SecretStore> {
    match settings.ui.application.secret_storage_mode {
        SecretStorageMode::ConfigFile => Arc::new(ConfigSecretStore::with_scope(
            config_dir.join("backup-password.json"),
            settings.secret_scope_id.clone(),
        )),
        SecretStorageMode::SystemKeyring => system_keyring_secret_store(&settings.secret_scope_id),
    }
}

fn settings_encryption_key() -> SecretKey {
    SecretKey::namespaced(
        "settings",
        "encryption-key",
        "Rufin settings encryption key",
    )
}

fn settings_key(secrets: &dyn SecretStore, create: bool) -> Result<age::x25519::Identity, String> {
    use age::secrecy::ExposeSecret as _;
    let key = settings_encryption_key();
    if let Some(value) = secrets
        .load_secret(&key)
        .map_err(|error| error.to_string())?
    {
        return value.parse().map_err(|error: &str| error.to_string());
    }
    if !create {
        return Err("Saved credentials are locked or unavailable".into());
    }
    static CREATION: Mutex<()> = Mutex::new(());
    let _creation = CREATION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(value) = secrets
        .load_secret(&key)
        .map_err(|error| error.to_string())?
    {
        return value.parse().map_err(|error: &str| error.to_string());
    }
    let identity = age::x25519::Identity::generate();
    secrets
        .save_secret(&key, identity.to_string().expose_secret())
        .map_err(|error| error.to_string())?;
    Ok(identity)
}

pub(crate) fn seal_credential(
    secrets: &dyn SecretStore,
    value: &str,
) -> Result<Credential, String> {
    use base64::Engine as _;
    let identity = settings_key(secrets, true)?;
    let recipient = identity.to_public();
    let mut output =
        age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))
            .map_err(|error| error.to_string())?
            .wrap_output(Vec::new())
            .map_err(|error| error.to_string())?;
    output
        .write_all(value.as_bytes())
        .map_err(|error| error.to_string())?;
    Ok(Credential::Sealed {
        encrypted: base64::engine::general_purpose::STANDARD
            .encode(output.finish().map_err(|error| error.to_string())?),
        session: !secrets.is_persistent(),
    })
}

pub(crate) fn load_provider_secret(
    secrets: &dyn SecretStore,
    credential: &Credential,
) -> Result<Option<String>, String> {
    use base64::Engine as _;
    use std::io::Read as _;
    let Credential::Sealed { encrypted, .. } = credential else {
        return secrets
            .load_secret(&provider_secret_key(credential))
            .map_err(|error| error.to_string());
    };
    let identity = settings_key(secrets, false)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encrypted)
        .map_err(|error| error.to_string())?;
    let mut input = age::Decryptor::new(bytes.as_slice())
        .map_err(|error| error.to_string())?
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .map_err(|error| error.to_string())?;
    let mut value = String::new();
    input
        .read_to_string(&mut value)
        .map_err(|error| error.to_string())?;
    Ok(Some(value))
}

fn scrobbling_secrets_present(settings: &ScrobblingSettings) -> bool {
    crate::scrobbling::secret_descriptors(settings)
        .iter()
        .any(|descriptor| !descriptor.value(settings).trim().is_empty())
}

pub(crate) fn scrobbling_secret_key(descriptor: &crate::scrobbling::SecretDescriptor) -> SecretKey {
    SecretKey::namespaced(
        descriptor.namespace(),
        &descriptor.kind(),
        descriptor.label(),
    )
}

pub(crate) fn read_scrobbling(
    stored: &StoredSettings,
    secrets: &dyn SecretStore,
) -> Result<ScrobblingSettings, String> {
    let mut settings = stored.scrobbling_runtime_settings();
    for descriptor in crate::scrobbling::secret_descriptors(&settings) {
        if let Some(credential) = stored.scrobbling_credentials.get(&descriptor.kind()) {
            let value = load_provider_secret(secrets, credential)?.unwrap_or_default();
            descriptor.set(&mut settings, value);
        } else if stored.scrobbling_secrets_present && descriptor.value(&settings).is_empty() {
            let value = secrets
                .load_secret(&scrobbling_secret_key(&descriptor))
                .map_err(|error| error.to_string())?
                .unwrap_or_default();
            descriptor.set(&mut settings, value);
        }
    }
    settings.sanitize();
    Ok(settings)
}

pub(crate) fn store_scrobbling(
    stored: &mut StoredSettings,
    secrets: &dyn SecretStore,
    input: &ScrobblingSettings,
) -> Result<(), String> {
    let mut input = input.clone();
    input.sanitize();
    let descriptors = crate::scrobbling::secret_descriptors(&input);
    // The secrets of servers that are gone go with them.
    stored.scrobbling_credentials.retain(|kind, _| {
        descriptors
            .iter()
            .any(|descriptor| descriptor.kind() == *kind)
    });
    for descriptor in descriptors {
        let kind = descriptor.kind();
        let value = descriptor.value(&input);
        if value.is_empty() {
            stored.scrobbling_credentials.remove(&kind);
        } else if stored
            .scrobbling_credentials
            .get(&kind)
            .map(|credential| load_provider_secret(secrets, credential))
            .transpose()?
            .flatten()
            .as_deref()
            != Some(value)
        {
            let credential = seal_credential(secrets, value)?;
            stored.scrobbling_credentials.insert(kind, credential);
        }
        descriptor.set(&mut input, String::new());
    }
    stored.ui.lastfm_api_key = input.lastfm.api_key.clone();
    input.lastfm.api_key.clear();
    stored.scrobbling = input;
    stored.scrobbling_secrets_present = false;
    Ok(())
}

pub(crate) fn persist_scrobbling_settings(
    file: &SettingsFile,
    secrets: &Arc<SwitchableSecretStore>,
    input: &ScrobblingSettings,
    scope: &str,
) -> Result<(), String> {
    let mut prepared = StoredSettings::clone(&file.load());
    store_scrobbling(&mut prepared, secrets.as_ref(), input)?;
    file.update(|stored| {
        if stored.secret_scope_id != scope {
            return Err("credential storage was reset".into());
        }
        // Switches commit through their own edit, which may have landed meanwhile.
        let mut scrobbling = prepared.scrobbling;
        let kept = &stored.scrobbling;
        for (account, kept) in [
            (&mut scrobbling.lastfm, &kept.lastfm),
            (&mut scrobbling.librefm, &kept.librefm),
        ] {
            account.enabled = kept.enabled;
            account.now_playing_enabled = kept.now_playing_enabled;
        }
        scrobbling.listenbrainz.enabled = kept.listenbrainz.enabled;
        scrobbling.listenbrainz.now_playing_enabled = kept.listenbrainz.now_playing_enabled;
        for server in &mut scrobbling.servers {
            if let Some(kept) = kept.server(&server.id) {
                server.enabled = kept.enabled;
                server.now_playing_enabled = kept.now_playing_enabled;
            }
        }
        stored.scrobbling = scrobbling;
        stored.scrobbling_credentials = prepared.scrobbling_credentials;
        stored.scrobbling_secrets_present = false;
        stored.ui.lastfm_api_key = prepared.ui.lastfm_api_key;
        Ok(())
    })
}

pub(crate) fn load_scrobbling_settings(
    file: &SettingsFile,
    secrets: &Arc<SwitchableSecretStore>,
) -> ScrobblingSettings {
    let current = file.load();
    match read_scrobbling(&current, secrets.as_ref()) {
        Ok(settings) => {
            if current.scrobbling_secrets_present || scrobbling_secrets_present(&current.scrobbling)
            {
                let mut migrated = StoredSettings::clone(&current);
                if let Err(error) = store_scrobbling(&mut migrated, secrets.as_ref(), &settings)
                    .and_then(|()| {
                        file.update(|stored| {
                            if stored.scrobbling == current.scrobbling
                                && stored.scrobbling_credentials == current.scrobbling_credentials
                            {
                                stored.scrobbling_credentials = migrated.scrobbling_credentials;
                                stored.scrobbling = migrated.scrobbling;
                                stored.scrobbling_secrets_present = false;
                            }
                            Ok(())
                        })
                    })
                {
                    warn!(%error, "could not migrate saved scrobbling credentials");
                }
            }
            settings
        }
        Err(error) => {
            warn!(%error, "could not load saved scrobbling credentials");
            current.scrobbling_runtime_settings()
        }
    }
}

/// Seals source credentials saved before they were sealed.
pub(crate) fn migrate_source_credentials(
    file: &SettingsFile,
    secrets: &dyn SecretStore,
) -> Result<(), String> {
    let mut replacements = std::collections::BTreeMap::new();
    for source in file.load().sources.connections() {
        if let Some(Credential::Legacy(reference)) = &source.credential {
            if !replacements.contains_key(reference) {
                let value = secrets
                    .load_secret(&SecretKey::provider_token(reference))
                    .map_err(|error| error.to_string())?;
                let credential = value
                    .as_deref()
                    .map(|value| seal_credential(secrets, value))
                    .transpose()?;
                replacements.insert(reference.clone(), credential);
            }
        }
    }
    if replacements.is_empty() {
        return Ok(());
    }
    file.update(|stored| {
        for source in stored.sources.connections_mut() {
            if let Some(Credential::Legacy(reference)) = &source.credential {
                if let Some(credential) = replacements.get(reference) {
                    source.credential = credential.clone();
                }
            }
        }
        Ok(())
    })?;
    if secrets.is_persistent() {
        for reference in replacements.keys() {
            if let Err(error) = secrets.delete_secret(&SecretKey::provider_token(reference)) {
                warn!(%error, "could not remove migrated source credential");
            }
        }
    }
    Ok(())
}

pub(crate) fn random_identity(prefix: &str) -> Result<String, String> {
    use std::fmt::Write as _;

    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| error.to_string())?;
    let mut value = String::with_capacity(prefix.len() + bytes.len() * 2);
    value.push_str(prefix);
    for byte in bytes {
        write!(&mut value, "{byte:02x}").map_err(|error| error.to_string())?;
    }
    Ok(value)
}
fn read_settings_json(path: &Path) -> Result<Option<serde_json::Value>, String> {
    match fs::read_to_string(path) {
        Ok(raw) if raw.trim().is_empty() => Ok(None),
        Ok(raw) => serde_json::from_str(&raw)
            .map(Some)
            .map_err(|error| error.to_string()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn replace_setting(
    root: &mut serde_json::Value,
    path: &[String],
    value: Option<serde_json::Value>,
) -> Option<serde_json::Value> {
    let (key, parents) = path.split_last().unwrap();
    let parent = parents.iter().fold(root, |parent, key| &mut parent[key]);
    let fields = parent.as_object_mut().unwrap();
    match value {
        Some(value) => fields.insert(key.clone(), value),
        None => fields.remove(key),
    }
}

fn recover_setting(
    accepted: &mut serde_json::Value,
    path: Vec<String>,
    value: &serde_json::Value,
    unsupported: &mut Vec<Vec<String>>,
) {
    let previous = replace_setting(accepted, &path, Some(value.clone()));
    if serde_json::from_value::<StoredSettings>(accepted.clone()).is_ok() {
        return;
    }
    let recover_children = previous.as_ref().is_some_and(serde_json::Value::is_object);
    replace_setting(accepted, &path, previous);
    if recover_children && let Some(fields) = value.as_object() {
        for (key, value) in fields {
            let mut child = path.clone();
            child.push(key.clone());
            recover_setting(accepted, child, value, unsupported);
        }
    } else {
        unsupported.push(path);
    }
}

pub(crate) fn decode_settings(
    raw: &serde_json::Value,
) -> Result<(StoredSettings, Vec<Vec<String>>), String> {
    let mut migrated = raw.clone();
    if migrated.get("equalizer").is_none()
        && let Some(legacy) = raw.get("playback").and_then(|p| p.get("equalizer"))
    {
        migrated["equalizer"] =
            serde_json::to_value(crate::playback::EqualizerPreferences::from_legacy(legacy))
                .map_err(|e| e.to_string())?;
    }
    let raw = &migrated;
    if let Ok(stored) = serde_json::from_value::<StoredSettings>(raw.clone()) {
        stored.equalizer.validate()?;
        return Ok((stored, Vec::new()));
    }
    let fields = raw.as_object().ok_or("settings must be a JSON object")?;
    let mut accepted =
        serde_json::to_value(StoredSettings::default()).map_err(|error| error.to_string())?;
    let mut unsupported = Vec::new();
    for (key, value) in fields {
        recover_setting(&mut accepted, vec![key.clone()], value, &mut unsupported);
    }
    serde_json::from_value(accepted)
        .map(|stored| (stored, unsupported))
        .map_err(|error| error.to_string())
}

fn read_startup_settings(path: &Path) -> Result<(StoredSettings, bool), String> {
    let Some(raw) = read_settings_json(path)? else {
        return Ok((StoredSettings::default(), false));
    };
    let (stored, unsupported) = decode_settings(&raw)?;
    if !unsupported.is_empty() {
        warn!(?unsupported, path = %path.display(),
            "using defaults for incompatible preferences; saved values remain intact");
    }
    let migrated = raw.get("equalizer").is_none()
        && raw
            .get("playback")
            .and_then(|p| p.get("equalizer"))
            .is_some();
    Ok((stored, migrated))
}

// Apply typed changes without dropping fields this version does not understand.
// Arrays are whole preferences: replacing their contents is an explicit change.
fn merge_settings(
    saved: &mut serde_json::Value,
    previous: &serde_json::Value,
    next: &serde_json::Value,
) {
    if previous == next {
        return;
    }
    if let (Some(saved), Some(previous), Some(next)) = (
        saved.as_object_mut(),
        previous.as_object(),
        next.as_object(),
    ) {
        for key in previous.keys() {
            if !next.contains_key(key) {
                saved.remove(key);
            }
        }
        for (key, value) in next {
            match (saved.get_mut(key), previous.get(key)) {
                (Some(saved), Some(previous)) => merge_settings(saved, previous, value),
                _ => {
                    saved.insert(key.clone(), value.clone());
                }
            }
        }
    } else {
        *saved = next.clone();
    }
}

fn persistent_settings(value: &StoredSettings) -> StoredSettings {
    let mut persistent = value.clone();
    for source in persistent.sources.connections_mut() {
        if source.credential.as_ref().is_some_and(Credential::session) {
            source.credential = None;
        }
    }
    persistent
        .scrobbling_credentials
        .retain(|_, credential| !credential.session());
    persistent
}

pub(crate) fn write_settings(path: &Path, value: &StoredSettings) -> Result<(), String> {
    let next =
        serde_json::to_value(persistent_settings(value)).map_err(|error| error.to_string())?;
    let mut merged = match read_settings_json(path)? {
        None => next,
        Some(mut saved) => {
            let (mut previous, unsupported) = decode_settings(&saved)?;
            let mut before = serde_json::to_value(&previous).map_err(|error| error.to_string())?;
            if !unsupported.is_empty() {
                previous.migrate_defaults();
                let defaults =
                    serde_json::to_value(&previous).map_err(|error| error.to_string())?;
                // Sanitizing a runtime default must not overwrite an unsupported saved value.
                for path in unsupported {
                    let default = path
                        .iter()
                        .try_fold(&defaults, |parent, key| parent.get(key));
                    replace_setting(&mut before, &path, default.cloned());
                }
            }
            merge_settings(&mut saved, &before, &next);
            saved
        }
    };
    // The decoded legacy value can compare equal before and after migration.
    // Install its new key even when the ordinary merge has no changed fields.
    if merged.get("equalizer").is_none() {
        merged["equalizer"] = serde_json::to_value(&value.equalizer).map_err(|e| e.to_string())?;
    }
    if let Some(playback) = merged
        .get_mut("playback")
        .and_then(serde_json::Value::as_object_mut)
    {
        playback.remove("equalizer");
    }
    let mut json = serde_json::to_vec_pretty(&merged).map_err(|error| error.to_string())?;
    json.push(b'\n');
    write_private(path, &json)
}

pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| error.to_string())?;
    temporary.persist(path).map_err(|error| error.to_string())?;
    if let Err(error) = sync_settings_directory(parent) {
        warn!(%error,path=%path.display(),"could not sync the settings directory after saving");
    }
    Ok(())
}
#[cfg(unix)]
fn sync_settings_directory(parent: &Path) -> std::io::Result<()> {
    fs::File::open(parent).and_then(|directory| directory.sync_all())
}
#[cfg(not(unix))]
fn sync_settings_directory(_parent: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrets::{SecretError, SecretResult};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn pending_settings_update_leaves_committed_values_readable_and_serializes_writers() {
        use std::sync::mpsc;
        use std::time::Duration;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let file = SettingsFile {
            path: Some(path.clone()),
            ..SettingsFile::memory_at(directory.path().to_path_buf())
        };
        let initial = file.load();
        let (entered, pending) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let writer_file = file.clone();
        let writer = std::thread::spawn(move || {
            writer_file.update(|stored| {
                stored.ui.shuffle_enabled = true;
                entered.send(()).unwrap();
                resume.recv().unwrap();
                Ok(())
            })
        });
        pending.recv().unwrap();
        let (read, result) = mpsc::channel();
        let reader_file = file.clone();
        let reader = std::thread::spawn(move || read.send(reader_file.load()).unwrap());
        let observed = result.recv_timeout(Duration::from_secs(5));
        // A writer that starts meanwhile sees the pending one's result; so does an edit.
        let next_file = file.clone();
        let next_writer = std::thread::spawn(move || {
            next_file.update(|stored| {
                let previous_shuffle = stored.ui.shuffle_enabled;
                stored.ui.application.private_mode = true;
                Ok(previous_shuffle)
            })
        });
        let edit_file = file.clone();
        let editor = std::thread::spawn(move || {
            edit_file.edit(|stored| {
                stored.ui.window_width = Some(1234);
                stored.ui.window_height = Some(800);
                Ok(stored.ui.shuffle_enabled)
            })
        });
        // Always release the writer, including when a blocked reader times out.
        release.send(()).unwrap();
        writer.join().unwrap().unwrap();
        reader.join().unwrap();
        assert!(next_writer.join().unwrap().unwrap());
        assert!(editor.join().unwrap().unwrap());
        let edited_width = file.load().ui.window_width;
        assert_eq!(observed.unwrap(), initial);
        assert_eq!(edited_width, Some(1234));
        let committed = file.load();
        assert!(committed.ui.shuffle_enabled);
        assert!(committed.ui.application.private_mode);
        assert_eq!(committed.ui.window_width, Some(1234));
        file.flush().unwrap();
        assert_eq!(read_startup_settings(&path).unwrap().0, *file.load());
    }

    #[test]
    fn failed_settings_save_does_not_publish_and_later_saves_succeed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        // A directory at the target path makes the atomic replacement fail.
        fs::create_dir(&path).unwrap();
        let file = SettingsFile {
            path: Some(path.clone()),
            ..SettingsFile::memory_at(directory.path().to_path_buf())
        };
        let initial = file.load();
        let changes = file.changes();
        assert!(
            file.update(|stored| {
                stored.ui.web_controller.enabled = !stored.ui.web_controller.enabled;
                Ok(())
            })
            .is_err()
        );
        assert_eq!(file.load(), initial);
        assert!(!changes.has_changed().unwrap());

        fs::remove_dir(&path).unwrap();
        file.update(|stored| {
            stored.ui.web_controller.enabled = !stored.ui.web_controller.enabled;
            Ok(())
        })
        .unwrap();
        let committed = file.load();
        assert_ne!(committed.ui.web_controller, initial.ui.web_controller);
        assert!(changes.has_changed().unwrap());
        assert_eq!(*changes.borrow(), committed);
        assert_eq!(read_startup_settings(&path).unwrap().0, *committed);
    }

    #[derive(Default)]
    struct CountingSecretStore {
        loads: AtomicUsize,
        saves: AtomicUsize,
        deletes: AtomicUsize,
    }

    impl SecretStore for CountingSecretStore {
        fn save_secret(&self, _key: &SecretKey, _secret: &str) -> SecretResult<()> {
            self.saves.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn load_secret(&self, _key: &SecretKey) -> SecretResult<Option<String>> {
            self.loads.fetch_add(1, Ordering::Relaxed);
            Ok(None)
        }

        fn delete_secret(&self, _key: &SecretKey) -> SecretResult<()> {
            self.deletes.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    fn counting_secrets() -> (Arc<CountingSecretStore>, Arc<SwitchableSecretStore>) {
        let store = Arc::new(CountingSecretStore::default());
        let backend: Arc<dyn SecretStore> = store.clone();
        (store, Arc::new(SwitchableSecretStore::new(backend)))
    }

    #[test]
    fn backup_schedule_identity_and_private_settings_survive_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let file = SettingsFile {
            path: Some(path.clone()),
            ..SettingsFile::memory_at(directory.path().to_path_buf())
        };
        assert!(!file.load().ui.application.backup.enabled);
        file.update(|stored| {
            stored.ui.application.backup.schedule.frequency =
                crate::backup::format::BackupFrequency::Daily;
            Ok(())
        })
        .unwrap();
        let identity = file
            .load()
            .ui
            .application
            .backup
            .schedule
            .schedule_id
            .clone();
        assert!(!identity.is_empty());
        file.update(|stored| {
            stored.ui.application.backup.contents.saved_logins = true;
            stored.ui.application.backup.schedule.retention = 0;
            Ok(())
        })
        .unwrap();
        let restored = read_startup_settings(&path).unwrap().0;
        assert_eq!(
            restored.ui.application.backup.schedule.schedule_id,
            identity
        );
        assert_eq!(restored.ui.application.backup.schedule.retention, 2);
        assert!(restored.ui.application.backup.contents.saved_logins);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
        let secrets = directory.path().join("secrets.json");
        fs::write(&secrets, b"old").unwrap();
        write_private(&secrets, br#"{"restored": "secret"}"#).unwrap();
        assert_eq!(fs::read(&secrets).unwrap(), br#"{"restored": "secret"}"#);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&secrets).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn fresh_scrobbling_settings_do_not_open_secret_storage() {
        let file = SettingsFile::memory();
        let (store, secrets) = counting_secrets();

        let settings = load_scrobbling_settings(&file, &secrets);
        persist_scrobbling_settings(&file, &secrets, &settings, &file.load().secret_scope_id)
            .expect("persist non-secret scrobbling setting");

        assert_eq!(store.loads.load(Ordering::Relaxed), 0);
        assert_eq!(store.saves.load(Ordering::Relaxed), 0);
        assert_eq!(store.deletes.load(Ordering::Relaxed), 0);
        assert!(!file.load().scrobbling_secrets_present);
    }

    #[test]
    fn first_scrobbling_secret_write_skips_a_redundant_delete() {
        let file = SettingsFile::memory();
        let (store, secrets) = counting_secrets();
        let mut settings = load_scrobbling_settings(&file, &secrets);
        settings.listenbrainz.user_token = "token".to_string();

        persist_scrobbling_settings(&file, &secrets, &settings, &file.load().secret_scope_id)
            .expect("persist first scrobbling secret");

        assert_eq!(store.loads.load(Ordering::Relaxed), 2);
        assert_eq!(store.deletes.load(Ordering::Relaxed), 0);
        assert_eq!(store.saves.load(Ordering::Relaxed), 1);
        assert!(!file.load().scrobbling_secrets_present);
        assert!(
            file.load()
                .scrobbling_credentials
                .contains_key("listenbrainz-token")
        );
        assert!(file.load().scrobbling.listenbrainz.user_token.is_empty());
    }

    #[test]
    fn legacy_secret_state_records_an_empty_probe() {
        let file = SettingsFile::memory();
        file.update(|stored| {
            stored.scrobbling_secrets_present = true;
            Ok(())
        })
        .expect("mark legacy secret state");
        let (store, secrets) = counting_secrets();

        load_scrobbling_settings(&file, &secrets);

        assert_eq!(
            store.loads.load(Ordering::Relaxed),
            crate::scrobbling::secret_descriptors(&Default::default()).len()
        );
        assert!(!file.load().scrobbling_secrets_present);
    }

    #[test]
    fn unavailable_legacy_secret_state_remains_conservative() {
        struct Unavailable;

        impl SecretStore for Unavailable {
            fn save_secret(&self, _key: &SecretKey, _secret: &str) -> SecretResult<()> {
                unreachable!()
            }

            fn load_secret(&self, _key: &SecretKey) -> SecretResult<Option<String>> {
                Err(SecretError::Backend("unavailable".to_string()))
            }

            fn delete_secret(&self, _key: &SecretKey) -> SecretResult<()> {
                unreachable!()
            }
        }

        let file = SettingsFile::memory();
        file.update(|stored| {
            stored.scrobbling_secrets_present = true;
            Ok(())
        })
        .expect("mark legacy secret state");
        let backend: Arc<dyn SecretStore> = Arc::new(Unavailable);
        let secrets = Arc::new(SwitchableSecretStore::new(backend));

        load_scrobbling_settings(&file, &secrets);

        assert!(file.load().scrobbling_secrets_present);
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl SettingsOwner {
    pub fn download_settings(
        &self,
        source_id: SourceId,
    ) -> crate::downloads::SourceDownloadSettings {
        self.load().download_settings(&source_id)
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl SettingsOwner {
    pub fn suppress_fetched_lyrics(&self, media_uri: String) -> Result<(), SettingsError> {
        self.update_preferences(move |ui| {
            ui.lyrics.suppress_auto_lyrics(&media_uri);
            Ok(())
        })
        .map_err(Into::into)
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl SettingsOwner {
    pub fn set_theme_accent(&self, theme: String, accent: String) -> Result<(), SettingsError> {
        self.update_preferences(move |ui| {
            ui.appearance.theme_preference = ThemePreference::Named(theme.clone());
            ui.appearance.theme_accents.insert(theme, accent);
            Ok(())
        })
        .map_err(Into::into)
    }
}

/// The committed settings a reader holds: the shared value, read as the UI settings.
#[derive(Clone)]
pub struct Committed(Arc<StoredSettings>);

impl std::ops::Deref for Committed {
    type Target = UiSettings;

    fn deref(&self) -> &UiSettings {
        &self.0.ui
    }
}
