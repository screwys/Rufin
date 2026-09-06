use std::ffi::OsString;
use std::fs;
use std::io::{ErrorKind, Write as _};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use ::ui::{
    HomeBlockKind, HomeSectionKind, LibraryField, LibraryListKey, LibraryListSettings,
    LibraryListSettingsEntry, Settings as UiSettings,
};
use playback::StreamQuality;
use scrobbling::Settings as ScrobblingSettings;
use secrets::{
    CachedSecretStore, ConfigSecretStore, SecretKey, SecretStorageMode, SecretStore,
    SwitchableSecretStore,
};
use serde::{Deserialize, Serialize};
use sources::{SourceConfiguration, SourceId};
use tracing::warn;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct CredentialRef(String);

impl CredentialRef {
    pub(crate) fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

pub(crate) fn fresh_credential_ref() -> Result<CredentialRef, String> {
    random_identity("source-").map(CredentialRef::new)
}

pub(crate) fn fresh_source_id() -> Result<sources::SourceId, String> {
    random_identity("rufin-source-").map(sources::SourceId::new)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct ConfiguredSource {
    #[serde(flatten)]
    pub(crate) configuration: SourceConfiguration,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) credential_ref: Option<CredentialRef>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) selected_source_id: Option<SourceId>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
pub(crate) enum LegacyTrackSortKey {
    TrackNumber,
    #[default]
    Title,
    Artist,
    Album,
    Year,
    Duration,
    Favorite,
}

impl LegacyTrackSortKey {
    fn library_field(self) -> LibraryField {
        match self {
            Self::TrackNumber => LibraryField::TrackNumber,
            Self::Title => LibraryField::Title,
            Self::Artist => LibraryField::Artist,
            Self::Album => LibraryField::Album,
            Self::Year => LibraryField::Year,
            Self::Duration => LibraryField::Duration,
            Self::Favorite => LibraryField::Favorite,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub(crate) struct LegacyTrackTableSettings {
    #[serde(default)]
    sort_key: LegacyTrackSortKey,
    #[serde(default)]
    descending: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct StoredSettings {
    #[serde(flatten)]
    pub(crate) ui: UiSettings,
    #[serde(default)]
    pub(crate) scrobbling: ScrobblingSettings,
    #[serde(default = "legacy_scrobbling_secrets_present")]
    pub(crate) scrobbling_secrets_present: bool,
    #[serde(default)]
    pub(crate) sources: SourceSettings,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) secret_scope_id: String,
    #[serde(default)]
    pub(crate) jellyfin_device_id: String,
    #[serde(default, rename = "home_sections", skip_serializing)]
    pub(crate) legacy_home_sections: Option<Vec<HomeSectionKind>>,
    #[serde(default, rename = "track_table", skip_serializing)]
    pub(crate) legacy_track_table: Option<LegacyTrackTableSettings>,
}

impl Default for StoredSettings {
    fn default() -> Self {
        Self {
            ui: UiSettings::default(),
            scrobbling: ScrobblingSettings::default(),
            scrobbling_secrets_present: false,
            sources: SourceSettings::default(),
            secret_scope_id: String::new(),
            jellyfin_device_id: String::new(),
            legacy_home_sections: None,
            legacy_track_table: None,
        }
    }
}

#[derive(Deserialize)]
struct SourceAuthorizationRecovery {
    #[serde(default)]
    sources: SourceSettings,
    #[serde(default)]
    secret_scope_id: String,
    #[serde(default)]
    jellyfin_device_id: String,
    #[serde(default)]
    secret_storage_mode: Option<SecretStorageMode>,
}

impl SourceAuthorizationRecovery {
    fn into_stored_settings(self) -> StoredSettings {
        let mut stored = StoredSettings {
            sources: self.sources,
            secret_scope_id: self.secret_scope_id,
            jellyfin_device_id: self.jellyfin_device_id,
            ..StoredSettings::default()
        };
        if let Some(mode) = self.secret_storage_mode {
            stored.ui.secret_storage_mode = mode;
        }
        stored
    }
}

impl StoredSettings {
    pub(crate) fn migrate_defaults(&mut self) {
        if self.ui.lastfm_api_key.trim().is_empty() && !self.scrobbling.lastfm.api_key.is_empty() {
            self.ui.lastfm_api_key = self.scrobbling.lastfm.api_key.clone();
        }
        self.scrobbling.lastfm.api_key.clear();
        self.scrobbling.sanitize();
        self.migrate_home_blocks();
        self.migrate_legacy_track_table();
        self.ui.sanitize();
        for download in &mut self.ui.downloads {
            let limit = self
                .sources
                .configured
                .iter()
                .find(|source| source.configuration.source_id == download.source_id)
                .and_then(|source| {
                    source
                        .configuration
                        .transcoded_download_bitrate_limit_kbps()
                });
            if let (StreamQuality::MaxBitrateKbps(bitrate), Some(limit)) = (download.quality, limit)
                && bitrate > limit
            {
                download.quality = StreamQuality::MaxBitrateKbps(limit);
            }
        }
    }

    pub(crate) fn scrobbling_runtime_settings(&self) -> ScrobblingSettings {
        let mut settings = self.scrobbling.clone();
        settings.lastfm.api_key = self.ui.lastfm_api_key.clone();
        settings
    }

    fn migrate_home_blocks(&mut self) {
        if self.ui.home_blocks.is_empty() {
            let home_sections = self
                .legacy_home_sections
                .take()
                .filter(|sections| !sections.is_empty())
                .unwrap_or_else(default_home_sections);
            self.ui.home_blocks = Vec::with_capacity(home_sections.len() + 2);
            self.ui.home_blocks.push(HomeBlockKind::Showcase);
            for section in home_sections {
                self.ui.home_blocks.push(match section {
                    HomeSectionKind::Explore => HomeBlockKind::Explore,
                    HomeSectionKind::MostPlayed => HomeBlockKind::MostPlayed,
                    HomeSectionKind::NewlyAdded => HomeBlockKind::NewlyAdded,
                    HomeSectionKind::RecentlyPlayed => HomeBlockKind::RecentlyPlayed,
                    HomeSectionKind::RecentlyReleased => HomeBlockKind::RecentlyReleased,
                });
            }
            if !self.ui.home_blocks.contains(&HomeBlockKind::Genres) {
                self.ui.home_blocks.push(HomeBlockKind::Genres);
            }
        } else {
            self.legacy_home_sections.take();
        }
    }

    fn migrate_legacy_track_table(&mut self) {
        let Some(legacy) = self.legacy_track_table.take() else {
            return;
        };
        if self
            .ui
            .library_lists
            .iter()
            .any(|entry| entry.key == LibraryListKey::Tracks)
        {
            return;
        }

        let mut settings = LibraryListSettings::for_key(LibraryListKey::Tracks);
        settings.sort_key = legacy.sort_key.library_field();
        settings.descending = legacy.descending;
        self.ui.library_lists.push(LibraryListSettingsEntry {
            key: LibraryListKey::Tracks,
            settings,
        });
    }
}

fn default_home_sections() -> Vec<HomeSectionKind> {
    vec![
        HomeSectionKind::Explore,
        HomeSectionKind::MostPlayed,
        HomeSectionKind::NewlyAdded,
        HomeSectionKind::RecentlyPlayed,
        HomeSectionKind::RecentlyReleased,
    ]
}

#[derive(Clone)]
pub(crate) struct SettingsFile {
    path: Option<PathBuf>,
    value: Arc<Mutex<StoredSettings>>,
}

impl SettingsFile {
    pub(crate) fn open(path: PathBuf) -> Result<Self, String> {
        let mut value = read_startup_settings(&path)?;
        value.migrate_defaults();
        let mut changed = false;
        if value.jellyfin_device_id.trim().is_empty() {
            match random_identity("rufin-") {
                Ok(identity) => {
                    value.jellyfin_device_id = identity;
                    changed = true;
                }
                Err(error) => warn!(%error, "could not create a Jellyfin device identity"),
            }
        }
        let file = Self {
            path: Some(path),
            value: Arc::new(Mutex::new(value)),
        };
        if changed {
            let current = file.load();
            if let Err(error) = file.write(&current) {
                warn!(%error, "could not save startup settings");
            }
        }
        Ok(file)
    }

    pub(crate) fn memory() -> Self {
        let mut value = StoredSettings::default();
        value.migrate_defaults();
        value.jellyfin_device_id = random_identity("rufin-").unwrap_or_default();
        Self {
            path: None,
            value: Arc::new(Mutex::new(value)),
        }
    }

    pub(crate) fn load(&self) -> StoredSettings {
        self.value
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub(crate) fn playback_stream_quality(&self) -> StreamQuality {
        self.value
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .ui
            .playback
            .stream_quality
    }

    pub(crate) fn update<T>(
        &self,
        operation: impl FnOnce(&mut StoredSettings) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut current = self
            .value
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut next = current.clone();
        let output = operation(&mut next)?;
        next.migrate_defaults();
        if next.ui.backup.schedule.frequency != library::BackupFrequency::Off
            && next.ui.backup.schedule.schedule_id.is_empty()
        {
            next.ui.backup.schedule.schedule_id = random_identity("schedule-")?;
        }
        next.ui.backup.schedule.retention = 2;
        if next.ui.backup.enabled
            && next.ui.backup.schedule.frequency == library::BackupFrequency::Off
        {
            next.ui.backup.schedule.frequency = library::BackupFrequency::Daily;
            if next.ui.backup.schedule.schedule_id.is_empty() {
                next.ui.backup.schedule.schedule_id = random_identity("schedule-")?;
            }
        }
        next.ui.backup.schedule.hour = next.ui.backup.schedule.hour.min(23);
        next.ui.backup.schedule.weekday = next.ui.backup.schedule.weekday.min(6);
        if let Some(path) = &self.path {
            write_settings(path, &next)?;
        }
        *current = next;
        Ok(output)
    }

    fn write(&self, value: &StoredSettings) -> Result<(), String> {
        if let Some(path) = &self.path {
            write_settings(path, value)?;
        }
        *self
            .value
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = value.clone();
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct SettingsUiPort {
    file: SettingsFile,
    on_change: Arc<dyn Fn(&StoredSettings, &StoredSettings, bool) + Send + Sync>,
}

impl SettingsUiPort {
    pub(crate) fn new(
        file: SettingsFile,
        on_change: impl Fn(&StoredSettings, &StoredSettings, bool) + Send + Sync + 'static,
    ) -> Rc<Self> {
        Rc::new(Self {
            file,
            on_change: Arc::new(on_change),
        })
    }

    fn save_ui(&self, settings: &UiSettings) -> Result<UiSettings, String> {
        let previous = self.file.load();
        self.file.update(|stored| {
            stored.ui = settings.clone();
            Ok(())
        })?;
        let current = self.file.load();
        (self.on_change)(&previous, &current, false);
        Ok(current.ui)
    }

    pub(crate) fn restore(
        &self,
        restore: impl FnOnce(&mut StoredSettings) -> Result<(), String>,
        credentials_changed: bool,
    ) -> Result<(), String> {
        let previous = self.file.load();
        self.file.update(restore)?;
        (self.on_change)(&previous, &self.file.load(), credentials_changed);
        Ok(())
    }
}

impl ::ui::SettingsPort for SettingsUiPort {
    fn load(&self) -> UiSettings {
        self.file.load().ui
    }

    fn save(&self, settings: &UiSettings) -> Result<UiSettings, String> {
        self.save_ui(settings)
    }
}

pub(crate) fn platform_secret_store(settings: &StoredSettings) -> Arc<dyn SecretStore> {
    match settings.ui.secret_storage_mode {
        SecretStorageMode::ConfigFile => Arc::new(CachedSecretStore::new(Arc::new(
            ConfigSecretStore::with_scope(
                crate::paths::secrets_file(),
                settings.secret_scope_id.clone(),
            ),
        ))),
        SecretStorageMode::SystemKeyring => system_keyring_secret_store(&settings.secret_scope_id),
    }
}

fn system_keyring_secret_store(scope_id: &str) -> Arc<dyn SecretStore> {
    match secrets::SystemKeyringStore::new(scope_id.to_string()) {
        Ok(store) => Arc::new(CachedSecretStore::new(Arc::new(store))),
        Err(error) => Arc::new(secrets::UnavailableSecretStore::new(error.to_string())),
    }
}

pub(crate) fn provider_secret_key(reference: &CredentialRef) -> SecretKey {
    SecretKey::provider_token(reference.as_str())
}

pub(crate) fn all_secret_keys(settings: &StoredSettings) -> Vec<SecretKey> {
    let mut keys = settings
        .sources
        .configured
        .iter()
        .filter_map(|source| source.credential_ref.as_ref())
        .map(provider_secret_key)
        .collect::<Vec<_>>();
    keys.extend(
        scrobbling::secret_descriptors()
            .iter()
            .map(|descriptor| scrobbling_secret_key(*descriptor)),
    );
    keys
}

pub(crate) fn backup_password_key() -> SecretKey {
    SecretKey::namespaced("backup", "password", "Rufin scheduled backup password")
}

pub(crate) fn backup_password_store(settings: &StoredSettings) -> Arc<dyn SecretStore> {
    match settings.ui.secret_storage_mode {
        SecretStorageMode::ConfigFile => Arc::new(ConfigSecretStore::with_scope(
            crate::paths::backup_password_file(),
            settings.secret_scope_id.clone(),
        )),
        SecretStorageMode::SystemKeyring => system_keyring_secret_store(&settings.secret_scope_id),
    }
}

pub(crate) fn persist_scrobbling_settings(
    file: &SettingsFile,
    secrets: &Arc<SwitchableSecretStore>,
    input: &ScrobblingSettings,
) -> Result<ScrobblingSettings, String> {
    let mut input = input.clone();
    input.sanitize();
    let stored = file.load();
    let mut current = stored.scrobbling_runtime_settings();
    if stored.scrobbling_secrets_present {
        for descriptor in scrobbling::secret_descriptors() {
            if !descriptor.value(&current).trim().is_empty() {
                continue;
            }
            let key = scrobbling_secret_key(*descriptor);
            if let Some(secret) = load_secret(Arc::clone(secrets), key.clone())
                .map_err(|error| format!("failed to load scrobbling secret {key:?}: {error}"))?
            {
                *descriptor.value_mut(&mut current) = secret;
            }
        }
    }
    current.sanitize();

    let changed_secrets = scrobbling::secret_descriptors()
        .iter()
        .copied()
        .filter_map(|descriptor| {
            let inline_secret = !descriptor.value(&stored.scrobbling).trim().is_empty();
            let changed = inline_secret || descriptor.value(&current) != descriptor.value(&input);
            changed.then(|| {
                (
                    descriptor,
                    scrobbling_secret_key(descriptor),
                    descriptor.value(&input).to_string(),
                )
            })
        })
        .collect::<Vec<_>>();

    // Removing the previous fixed-key value first makes an interrupted account
    // change disconnected rather than pairing a new username with an old session.
    if stored.scrobbling_secrets_present {
        for (_, key, _) in &changed_secrets {
            delete_secret(Arc::clone(secrets), key.clone())
                .map_err(|error| format!("failed to replace scrobbling secret {key:?}: {error}"))?;
        }
    }

    let mut persisted = input.clone();
    for descriptor in scrobbling::secret_descriptors() {
        descriptor.value_mut(&mut persisted).clear();
    }
    persisted.lastfm.api_key.clear();
    file.update(|stored| {
        stored.ui.lastfm_api_key = input.lastfm.api_key.clone();
        stored.scrobbling = persisted;
        stored.scrobbling_secrets_present = scrobbling_secrets_present(&input);
        Ok(())
    })?;

    // Descriptor order keeps each session after the credentials that make it
    // usable. A partial write therefore still cannot connect the wrong account.
    for (_, key, value) in changed_secrets {
        if !value.is_empty() {
            save_secret(Arc::clone(secrets), key.clone(), value)
                .map_err(|error| format!("failed to save scrobbling secret {key:?}: {error}"))?;
        }
    }
    Ok(input)
}

pub(crate) fn load_scrobbling_settings(
    file: &SettingsFile,
    secrets: &Arc<SwitchableSecretStore>,
) -> ScrobblingSettings {
    let stored = file.load();
    let mut settings = stored.scrobbling_runtime_settings();
    if !stored.scrobbling_secrets_present {
        return settings;
    }
    let mut loaded = true;
    for descriptor in scrobbling::secret_descriptors() {
        let value = descriptor.value_mut(&mut settings);
        if !value.trim().is_empty() {
            continue;
        }
        match load_secret(Arc::clone(secrets), scrobbling_secret_key(*descriptor)) {
            Ok(Some(secret)) => *value = secret,
            Ok(None) => {}
            Err(error) => {
                loaded = false;
                warn!(%error, "failed to load a scrobbling secret");
                break;
            }
        }
    }
    settings.sanitize();
    if loaded {
        let present = scrobbling_secrets_present(&settings);
        if stored.scrobbling_secrets_present != present
            && let Err(error) = file.update(|stored| {
                stored.scrobbling_secrets_present = present;
                Ok(())
            })
        {
            warn!(%error, "could not save scrobbling secret presence");
        }
    }
    settings
}

fn scrobbling_secrets_present(settings: &ScrobblingSettings) -> bool {
    scrobbling::secret_descriptors()
        .iter()
        .any(|descriptor| !descriptor.value(settings).trim().is_empty())
}

fn legacy_scrobbling_secrets_present() -> bool {
    true
}

pub(crate) fn startup_scrobbling_settings(
    file: &SettingsFile,
    secrets: &Arc<SwitchableSecretStore>,
) -> ScrobblingSettings {
    let stored = file.load();
    let settings = stored.scrobbling_runtime_settings();
    let has_inline_secrets = scrobbling::secret_descriptors()
        .iter()
        .any(|descriptor| !descriptor.value(&stored.scrobbling).trim().is_empty());
    let has_enabled_service =
        settings.lastfm.enabled || settings.librefm.enabled || settings.listenbrainz.enabled;
    if !has_inline_secrets && !has_enabled_service {
        return settings;
    }

    if has_inline_secrets {
        for descriptor in scrobbling::secret_descriptors() {
            let value = descriptor.value(&stored.scrobbling);
            if value.is_empty() {
                continue;
            }
            if descriptor.value(&file.load().scrobbling) != value {
                continue;
            }
            let result = save_secret(
                Arc::clone(secrets),
                scrobbling_secret_key(*descriptor),
                value.to_owned(),
            )
            .and_then(|()| {
                file.update(|current| {
                    if descriptor.value(&current.scrobbling) == value {
                        descriptor.value_mut(&mut current.scrobbling).clear();
                        current.scrobbling_secrets_present = true;
                    }
                    Ok(())
                })
            });
            if let Err(error) = result {
                warn!(%error, "could not move scrobbling credentials to secret storage");
                return settings;
            }
        }
    }
    load_scrobbling_settings(file, secrets)
}

pub(crate) fn scrobbling_secret_key(descriptor: scrobbling::SecretDescriptor) -> SecretKey {
    SecretKey::namespaced(
        descriptor.namespace(),
        descriptor.kind(),
        descriptor.label(),
    )
}

pub(crate) fn load_provider_secret(
    secrets: &Arc<SwitchableSecretStore>,
    reference: &CredentialRef,
) -> Result<Option<String>, String> {
    load_secret(Arc::clone(secrets), provider_secret_key(reference))
}

pub(crate) fn save_provider_secret(
    secrets: &Arc<SwitchableSecretStore>,
    reference: &CredentialRef,
    value: String,
) -> Result<(), String> {
    save_secret(Arc::clone(secrets), provider_secret_key(reference), value)
}

pub(crate) fn delete_provider_secret(
    secrets: &Arc<SwitchableSecretStore>,
    reference: &CredentialRef,
) -> Result<(), String> {
    delete_secret(Arc::clone(secrets), provider_secret_key(reference))
}

fn load_secret<S>(store: Arc<S>, key: SecretKey) -> Result<Option<String>, String>
where
    S: SecretStore + ?Sized + 'static,
{
    blocking_secret(move || store.load_secret(&key))
}

fn save_secret<S>(store: Arc<S>, key: SecretKey, value: String) -> Result<(), String>
where
    S: SecretStore + ?Sized + 'static,
{
    blocking_secret(move || store.save_secret(&key, &value))
}

fn delete_secret<S>(store: Arc<S>, key: SecretKey) -> Result<(), String>
where
    S: SecretStore + ?Sized + 'static,
{
    blocking_secret(move || store.delete_secret(&key))
}

fn blocking_secret<T: Send + 'static>(
    operation: impl FnOnce() -> secrets::SecretResult<T> + Send + 'static,
) -> Result<T, String> {
    std::thread::Builder::new()
        .name("rufin-secrets".to_string())
        .spawn(operation)
        .map_err(|error| error.to_string())?
        .join()
        .map_err(|_| "the secrets operation panicked".to_string())?
        .map_err(|error| error.to_string())
}

fn random_identity(prefix: &str) -> Result<String, String> {
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
fn read_startup_settings(path: &Path) -> Result<StoredSettings, String> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) if raw.trim().is_empty() => {
            return Ok(StoredSettings::default());
        }
        Ok(raw) => raw,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok(StoredSettings::default());
        }
        Err(error) => return Err(error.to_string()),
    };
    match serde_json::from_str(&raw) {
        Ok(stored) => Ok(stored),
        Err(error) => {
            let recovered = serde_json::from_str::<SourceAuthorizationRecovery>(&raw)
                .ok()
                .filter(|recovered| !recovered.sources.configured.is_empty());
            let preserved = preserve_unreadable_settings(path)?;
            if let Some(recovered) = recovered {
                let mut stored = recovered.into_stored_settings();
                stored.migrate_defaults();
                write_settings(path, &stored)?;
                warn!(
                    %error,
                    path = %path.display(),
                    preserved_path = %preserved.display(),
                    configured_sources = stored.sources.configured.len(),
                    "preserved incompatible settings and recovered source authorization"
                );
                return Ok(stored);
            }
            warn!(
                %error,
                path = %path.display(),
                preserved_path = %preserved.display(),
                "preserved unreadable settings and continued with defaults"
            );
            Ok(StoredSettings::default())
        }
    }
}

fn preserve_unreadable_settings(path: &Path) -> Result<PathBuf, String> {
    let parent = path.parent().filter(|path| !path.as_os_str().is_empty());
    let file_name = path
        .file_name()
        .ok_or_else(|| "settings path has no file name".to_string())?;
    let mut number = 0_u64;
    loop {
        let mut candidate_name = OsString::from(file_name);
        candidate_name.push(format!(".damaged-{}-{number}", std::process::id()));
        let candidate = parent.map_or_else(
            || PathBuf::from(&candidate_name),
            |parent| parent.join(&candidate_name),
        );
        if !candidate.exists() {
            fs::rename(path, &candidate).map_err(|error| error.to_string())?;
            return Ok(candidate);
        }
        number = number
            .checked_add(1)
            .ok_or_else(|| "could not choose a preserved settings path".to_string())?;
    }
}

pub(crate) fn write_settings(path: &Path, value: &StoredSettings) -> Result<(), String> {
    let mut json = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
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
            value: Arc::new(Mutex::new(StoredSettings::default())),
        };
        assert!(!file.load().ui.backup.enabled);
        file.update(|stored| {
            stored.ui.backup.schedule.frequency = library::BackupFrequency::Daily;
            Ok(())
        })
        .unwrap();
        let identity = file.load().ui.backup.schedule.schedule_id;
        assert!(!identity.is_empty());
        file.update(|stored| {
            stored.ui.backup.contents.saved_logins = true;
            stored.ui.backup.schedule.retention = 0;
            Ok(())
        })
        .unwrap();
        let restored = read_startup_settings(&path).unwrap();
        assert_eq!(restored.ui.backup.schedule.schedule_id, identity);
        assert_eq!(restored.ui.backup.schedule.retention, 2);
        assert!(restored.ui.backup.contents.saved_logins);
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
    fn fresh_settings_persist_empty_secret_storage_while_legacy_settings_probe_once() {
        let fresh = StoredSettings::default();
        let serialized = serde_json::to_value(&fresh).expect("serialize fresh settings");
        let mut legacy_json = serialized.clone();
        legacy_json
            .as_object_mut()
            .expect("settings object")
            .remove("scrobbling_secrets_present");
        let legacy: StoredSettings =
            serde_json::from_value(legacy_json).expect("deserialize old settings");

        assert_eq!(serialized["scrobbling_secrets_present"], false);
        assert!(!fresh.scrobbling_secrets_present);
        assert!(legacy.scrobbling_secrets_present);
    }

    #[test]
    fn fresh_scrobbling_preferences_and_toggles_do_not_open_secret_storage() {
        let file = SettingsFile::memory();
        let (store, secrets) = counting_secrets();

        let mut settings = load_scrobbling_settings(&file, &secrets);
        settings.lastfm.enabled = true;
        persist_scrobbling_settings(&file, &secrets, &settings)
            .expect("persist non-secret scrobbling setting");

        assert_eq!(store.loads.load(Ordering::Relaxed), 0);
        assert_eq!(store.saves.load(Ordering::Relaxed), 0);
        assert_eq!(store.deletes.load(Ordering::Relaxed), 0);
        assert!(file.load().scrobbling.lastfm.enabled);
        assert!(!file.load().scrobbling_secrets_present);
    }

    #[test]
    fn first_scrobbling_secret_write_skips_a_redundant_delete() {
        let file = SettingsFile::memory();
        let (store, secrets) = counting_secrets();
        let mut settings = load_scrobbling_settings(&file, &secrets);
        settings.listenbrainz.user_token = "token".to_string();

        let committed = persist_scrobbling_settings(&file, &secrets, &settings)
            .expect("persist first scrobbling secret");

        assert_eq!(store.loads.load(Ordering::Relaxed), 0);
        assert_eq!(store.deletes.load(Ordering::Relaxed), 0);
        assert_eq!(store.saves.load(Ordering::Relaxed), 1);
        assert_eq!(committed.listenbrainz.user_token, "token");
        assert!(file.load().scrobbling_secrets_present);
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
            scrobbling::secret_descriptors().len()
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
