//! Connect composition shared by desktop, headless and HTTP clients.
use library::profile::ProfileStore;
mod cache;
pub(crate) use cache::LyricsCacheQuery;
pub(crate) mod casting;
mod media;
pub mod portable;
#[cfg(test)]
mod tests;

use crate::{playback::PlaybackOwner, settings::SettingsOwner, source::SourceOwner};
use library::ConnectRecord;
use rufin_connect::{ConnectNetwork, Credentials, NetworkConfig, NetworkEvent};
use secrets::{SecretKey, SecretStore, SwitchableSecretStore};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub type FolderMappings = BTreeMap<String, downloads::DownloadDirectory>;
#[cfg(feature = "bindings")]
uniffi::custom_type!(FolderMappings, std::collections::HashMap<String, downloads::DownloadDirectory>, {
    remote,
    lower: |folders| folders.into_iter().collect(),
    try_lift: |folders| Ok(folders.into_iter().collect()),
});

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ConnectSettings {
    pub enabled: bool,
    pub name: String,
    pub identity_ref: Option<String>,
    pub profile: Option<String>,
    pub file_profiles: Vec<String>,
    pub nearby: bool,
    pub relay: Option<String>,
    pub public_relay: bool,
    pub folders: FolderMappings,
    pub pending_import: Option<PendingImport>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum PendingImport {
    Peer {
        profile: String,
        peer: Option<String>,
    },
    Prepared {
        profile: String,
        path: PathBuf,
    },
}

impl PendingImport {
    fn profile(&self) -> &str {
        match self {
            Self::Peer { profile, .. } | Self::Prepared { profile, .. } => profile,
        }
    }
}

impl Default for ConnectSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            name: if cfg!(target_os = "android") {
                "Rufin Android"
            } else {
                "Rufin Desktop"
            }
            .into(),
            identity_ref: None,
            profile: None,
            file_profiles: Vec::new(),
            nearby: true,
            relay: None,
            public_relay: false,
            folders: BTreeMap::new(),
            pending_import: None,
        }
    }
}

impl ConnectSettings {
    fn from_device_file(
        bytes: &[u8],
    ) -> serde_json::Result<(Self, Option<sources::DownloadQuality>)> {
        let value: serde_json::Value = serde_json::from_slice(bytes)?;
        let encoding = value
            .get("encoding")
            .map(|value| {
                if value.as_str() == Some("mp3") {
                    Ok(sources::DownloadQuality::Transcoded(
                        library::AudioEncoding::mp3(),
                    ))
                } else {
                    serde_json::from_value(value.clone())
                }
            })
            .transpose()?;
        let has_enabled = value.get("enabled").is_some();
        let unfinished = value["adopting"].as_bool() == Some(true)
            || value["setup_pending"].as_bool() == Some(true);
        let mut config: Self = serde_json::from_value(value)?;
        if !has_enabled {
            config.enabled = config.profile.is_some();
        }
        if unfinished && config.pending_import.is_none() {
            config.pending_import = config.profile.clone().map(|profile| PendingImport::Peer {
                profile,
                peer: None,
            });
        }
        Ok((config, encoding))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Device {
    pub id: String,
    pub name: String,
    pub reachable: bool,
    pub has_playback: bool,
    #[serde(default)]
    pub download_profiles: bool,
    #[serde(default)]
    pub enrolled: bool,
    pub connection: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Pairing {
    pub session: String,
    pub peer: String,
    pub name: String,
    pub emoji: Vec<String>,
    #[serde(default)]
    pub approved: bool,
    #[serde(default)]
    pub verified: bool,
    #[serde(default)]
    pub joining: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum CollectionStage {
    Preparing,
    Receiving,
    Validating,
    SavingChanges,
    Importing,
    Applying,
    Finalizing,
}

impl CollectionStage {
    fn label(self) -> String {
        match self {
            Self::Preparing => localization::tr("Preparing collection"),
            Self::Receiving => localization::tr("Receiving collection"),
            Self::Validating => localization::tr("Checking profile"),
            Self::SavingChanges => localization::tr("Saving changes"),
            Self::Importing => localization::tr("Importing profile"),
            Self::Applying => localization::tr("Applying profile"),
            Self::Finalizing => localization::tr("Finishing setup"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct CollectionProgress {
    pub stage: CollectionStage,
    pub completed: u64,
    pub total: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Status {
    pub settings: ConnectSettings,
    pub identity: Option<String>,
    pub invitation: Option<String>,
    pub devices: Vec<Device>,
    pub pairing: Option<Pairing>,
    #[serde(default)]
    pub connecting: bool,
    #[serde(default)]
    pub receiving_collection: bool,
    #[serde(default)]
    pub completed_pairings: u64,
    pub profile_status: String,
    #[serde(default)]
    pub collection_progress: Option<CollectionProgress>,
    pub error: Option<String>,
}

impl Status {
    pub fn continuation_device(&self) -> Option<&Device> {
        self.devices.iter().find(|device| {
            self.settings.enabled && device.enrolled && device.reachable && device.has_playback
        })
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum Action {
    Enable {
        enabled: bool,
    },
    Create,
    Join {
        invitation: String,
        replace: bool,
    },
    Invite {
        peer: String,
    },
    Pair {
        session: String,
        approve: bool,
        #[serde(default)]
        replace: bool,
    },
    CancelPairing,
    Rename {
        name: String,
    },
    Remove {
        peer: String,
    },
    Leave,
    Refresh,
    Retry,
    Discover,
    TestConnection {
        peer: String,
    },
    Network {
        nearby: bool,
        relay: Option<String>,
        #[serde(default)]
        public_relay: bool,
    },
    Continue {
        peer: String,
    },
    Control {
        peer: String,
        command: Control,
    },
    Folder {
        source: String,
        root_id: Option<String>,
        path: downloads::DownloadDirectory,
    },
    Download {
        peer: String,
        uri: String,
    },
    CancelDownload,
    Export {
        path: PathBuf,
        source_id: Option<sources::SourceId>,
    },
    Import {
        path: PathBuf,
        source_id: Option<sources::SourceId>,
        replace: bool,
        key: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum Confirmation {
    ReplaceProfile,
}

#[derive(Debug, thiserror::Error)]
#[cfg_attr(feature = "bindings", derive(uniffi::Error))]
pub enum ActionError {
    #[error("{0}")]
    Failed(String),
    #[error("{reason}")]
    ConfirmationRequired {
        confirmation: Confirmation,
        reason: String,
    },
}

impl From<String> for ActionError {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

impl From<&str> for ActionError {
    fn from(message: &str) -> Self {
        Self::Failed(message.to_owned())
    }
}

impl Confirmation {
    fn required(self) -> ActionError {
        let message = match self {
            Self::ReplaceProfile => localization::tr(
                "Connecting an existing profile will remove all data in this device, do you really want to continue?",
            ),
        };
        ActionError::ConfirmationRequired {
            confirmation: self,
            reason: message,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum Control {
    PlayPause,
    Play,
    Pause,
    Stop,
    Next,
    Previous,
    Seek {
        millis: u64,
    },
    Volume {
        value: f64,
    },
    PersistVolume {
        value: f64,
    },
    Mute {
        muted: bool,
    },
    Repeat {
        mode: playback::RepeatMode,
    },
    Shuffle {
        enabled: bool,
    },
    AutoDj {
        enabled: bool,
    },
    Activate {
        position: usize,
    },
    Remove {
        positions: Vec<usize>,
    },
    Reorder {
        positions: Vec<usize>,
        target: library::QueueReorderTarget,
    },
    MoveNext {
        position: usize,
    },
    Clear {
        include_current: bool,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
enum Request {
    Settings {
        revision: u64,
        known_revision: Option<u64>,
        records: Option<Vec<library::StampedRecord>>,
    },
    CacheFile {
        asset: cache::CacheRequest,
    },
    CastFrom {
        transfer: bool,
    },
    CastState,
    CastQueue,
    CastQueuePage {
        offset: u64,
        limit: u32,
        filter: String,
    },
    CastInput {
        header: playback::ContinuationHeader,
        placement: library::QueuePlacement,
        target: Option<library::QueueReorderTarget>,
    },
    Presence,
    Continuation,
    Queue {
        offset: usize,
    },
    TrackReference {
        uri: String,
    },
    MediaAvailability {
        uris: Vec<String>,
    },
    StopContinuation,
    Control {
        command: Control,
    },
}

#[derive(Serialize, Deserialize)]
struct Enrollment {
    file_key: String,
}

#[derive(Clone, Copy, Default)]
struct SettingsPeer {
    sent: Option<u64>,
    received: Option<u64>,
}

#[derive(Serialize, Deserialize)]
struct SettingsReply {
    revision: u64,
    acknowledged: Option<u64>,
    records: Option<Vec<library::StampedRecord>>,
}

struct Session {
    profile: String,
    documents: Arc<ProfileStore>,
    identity: String,
    stop: tokio_util::sync::CancellationToken,
    receiving: Mutex<BTreeSet<String>>,
    settings_peers: Mutex<BTreeMap<String, SettingsPeer>>,
    revision: std::sync::atomic::AtomicI64,
    received_changes: std::sync::atomic::AtomicBool,
}

struct ImportOperation {
    session: Option<String>,
    replace: bool,
    cancel: tokio_util::sync::CancellationToken,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct ConnectOwner {
    directory: PathBuf,
    database: Arc<library::Database>,
    settings_apply: SettingsOwner,
    secrets: Arc<SwitchableSecretStore>,
    source: Arc<SourceOwner>,
    playback: Arc<PlaybackOwner>,
    runtime: tokio::runtime::Handle,
    status: tokio::sync::watch::Sender<Status>,
    network: tokio::sync::Mutex<Option<Arc<ConnectNetwork>>>,
    session: tokio::sync::RwLock<Option<Arc<Session>>>,
    actions: tokio::sync::Mutex<()>,
    sync: tokio::sync::Mutex<()>,
    transfers: Mutex<BTreeMap<String, (Instant, playback::Continuation)>>,
    import_operation: Mutex<Option<ImportOperation>>,
}

impl ConnectOwner {
    pub(crate) fn new(
        directory: PathBuf,
        database: Arc<library::Database>,
        settings_apply: SettingsOwner,
        secrets: Arc<SwitchableSecretStore>,
        source: Arc<SourceOwner>,
        playback: Arc<PlaybackOwner>,
        runtime: tokio::runtime::Handle,
    ) -> Arc<Self> {
        if let Ok(files) = std::fs::read_dir(&directory) {
            for file in files.flatten() {
                if file.file_name() == "transfers" {
                    if let Err(error) = std::fs::remove_dir_all(file.path()) {
                        tracing::warn!(%error, "Could not remove the old Connect transfer cache");
                    }
                } else if file
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with("profile-transfer-"))
                {
                    if let Err(error) = std::fs::remove_file(file.path()) {
                        tracing::warn!(%error, "Could not remove interrupted profile transfer");
                    }
                }
            }
        }
        let loaded = std::fs::read(directory.join("device.json"));
        let (config, encoding, mut error) = match loaded {
            Ok(bytes) => match ConnectSettings::from_device_file(&bytes) {
                Ok((config, encoding)) => (config, encoding, None),
                Err(error) => (ConnectSettings::default(), None, Some(error.to_string())),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                (ConnectSettings::default(), None, None)
            }
            Err(error) => (ConnectSettings::default(), None, Some(error.to_string())),
        };
        if let Some(encoding) = encoding {
            let migrated = settings_apply
                .restore(
                    |stored| {
                        for source in &stored.sources.configured {
                            let source = &source.configuration;
                            if source.is_local()
                                && !stored
                                    .ui
                                    .downloads
                                    .iter()
                                    .any(|entry| entry.source_id == source.source_id)
                            {
                                stored
                                    .ui
                                    .set_download_quality(source.source_id.clone(), encoding);
                            }
                        }
                        Ok(())
                    },
                    false,
                )
                .and_then(|_| {
                    crate::settings::write_private(
                        &directory.join("device.json"),
                        &serde_json::to_vec(&config).map_err(|cause| cause.to_string())?,
                    )
                });
            if let Err(cause) = migrated {
                error = Some(cause);
            }
        }
        let owner = Arc::new(Self {
            directory: directory.clone(),
            database,
            settings_apply,
            secrets,
            source,
            playback,
            runtime,
            status: tokio::sync::watch::channel(Status {
                profile_status: if config.enabled {
                    localization::tr("Waiting for a peer")
                } else {
                    localization::tr("Connect is not enabled")
                },
                settings: config,
                identity: None,
                invitation: None,
                connecting: false,
                receiving_collection: false,
                completed_pairings: 0,
                devices: vec![],
                pairing: None,
                collection_progress: None,
                error,
            })
            .0,
            network: tokio::sync::Mutex::new(None),
            session: tokio::sync::RwLock::new(None),
            actions: tokio::sync::Mutex::new(()),
            sync: tokio::sync::Mutex::new(()),
            transfers: Mutex::new(BTreeMap::new()),
            import_operation: Mutex::new(None),
        });
        owner.playback.install_connect(&owner);
        owner.playback.lyrics.handle().install_connect(&owner);
        owner.source.install_connect(&owner);
        let cache_owner = Arc::downgrade(&owner);
        owner
            .source
            .shared
            .artwork
            .install_cache_resolver(move |keys| {
                let owner = cache_owner.clone();
                async move { owner.upgrade()?.fetch_cached_artwork(keys).await }
            });
        owner
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl ConnectOwner {
    pub fn status(&self) -> Status {
        self.status.borrow().clone()
    }
}

impl ConnectOwner {
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<Status> {
        self.status.subscribe()
    }

    pub(crate) async fn media_availability(&self, uris: &[String]) -> Result<Vec<bool>, String> {
        let settings = self.status().settings;
        if !settings.enabled || settings.profile.is_none() {
            return Ok(vec![false; uris.len()]);
        }
        let session = self.active().await?;
        let peers = self
            .connected_network()
            .await?
            .members()
            .await
            .map_err(error)?;
        let mut available = vec![false; uris.len()];
        for peer in peers.into_iter().filter(|peer| *peer != session.identity) {
            let answer: Vec<bool> = serde_json::from_value(
                self.request(
                    &peer,
                    Request::MediaAvailability {
                        uris: uris.to_vec(),
                    },
                )
                .await?,
            )
            .map_err(error)?;
            if answer.len() != uris.len() {
                return Err("Invalid media availability response".into());
            }
            for (current, incoming) in available.iter_mut().zip(answer) {
                *current |= incoming;
            }
        }
        Ok(available)
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl ConnectOwner {
    pub async fn roots(&self, source: String) -> Result<Vec<library::ConnectRoot>, ActionError> {
        let sources = self.source.list_sources();
        if !sources
            .sources
            .iter()
            .any(|entry| entry.id.as_str() == source && entry.kind == "local")
        {
            return Ok(Vec::new());
        }
        let database = self.database.clone();
        self.runtime
            .spawn(async move {
                database
                    .connect_roots(&source)
                    .await
                    .map_err(|error| ActionError::Failed(error.to_string()))
            })
            .await
            .map_err(|error| ActionError::Failed(error.to_string()))?
    }
}

impl ConnectOwner {
    pub(crate) fn start(self: &Arc<Self>) {
        let owner = Arc::clone(self);
        self.runtime.spawn(async move {
            let _action = owner.actions.lock().await;
            let result = async {
                if let Some(PendingImport::Peer { profile, .. }) =
                    owner.status().settings.pending_import
                {
                    let path = owner.directory.join(format!(
                        "{}.profile.sqlite",
                        blake3::hash(profile.as_bytes()).to_hex()
                    ));
                    if tokio::fs::try_exists(&path).await.map_err(error)?
                        && ProfileStore::database_profile(&path)
                            .await
                            .map_err(error)?
                            .as_deref()
                            == Some(&profile)
                    {
                        owner.save(|config| {
                            config.pending_import = Some(PendingImport::Prepared { profile, path })
                        })?;
                    }
                }
                let working = owner.migrate_legacy_profile().await?;
                let settings = owner.status().settings;
                if settings.profile != working && settings.enabled {
                    owner.save(|config| {
                        if config.pending_import.is_none() {
                            config.pending_import =
                                config.profile.clone().map(|profile| PendingImport::Peer {
                                    profile,
                                    peer: None,
                                });
                        }
                        config.profile = working.clone();
                    })?;
                }
                if owner.status().settings.enabled {
                    owner.network().await?;
                }
                if owner.status().settings.enabled && owner.session.read().await.is_none() {
                    if let Some(profile) = working {
                        owner.open(profile, false).await?;
                    }
                }
                let pending = owner.status().settings.pending_import;
                if pending.as_ref().is_some_and(|pending| {
                    matches!(pending, PendingImport::Prepared { .. })
                        || Some(pending.profile()) == owner.status().settings.profile.as_deref()
                }) {
                    let cancel = owner.begin_import(None);
                    owner.resume_import(cancel).await?;
                } else if pending.is_some() && owner.status().error.is_none() {
                    owner.failed(localization::tr(
                        "Profile download is incomplete. Retry to continue.",
                    ));
                }
                if owner.session.read().await.is_some() {
                    if owner.source.selected_library().is_none() {
                        owner.source.connect_changed(false).await?;
                    }
                    owner.source.connect_media_changed();
                }
                Ok::<_, String>(())
            }
            .await;
            if let Err(error) = result {
                owner.failed(error);
            }
        });
    }

    fn start_session(self: &Arc<Self>, session: Arc<Session>) {
        let weak = Arc::downgrade(self);
        let stop = session.stop.clone();
        let mut configuration = self.source.configuration_changes();
        let mut catalog = self.source.catalog_changes();
        let mut settings = self.settings_apply.shared_changes();
        let mut cache = self.database.subscribe_cache_writes();
        self.runtime.spawn(async move {
            stop.run_until_cancelled(async move {
                let mut configure = true;
                loop {
                    let Some(owner) = weak.upgrade() else { return };
                    if configure {
                        if let Err(error) = owner.configure_network().await {
                            owner.failed(error);
                        }
                    }
                    loop {
                        let mut busy = false;
                        match owner.database.connect_seed_page().await {
                            Ok(seeded) => busy |= seeded,
                            Err(error) => owner.failed(error.to_string()),
                        }
                        if let Err(error) = owner.synchronize().await {
                            owner.failed(error);
                        }
                        match session
                            .documents
                            .prune_history(&session.identity, &[], library::CONNECT_PAGE_SIZE)
                            .await
                        {
                            Ok(processed) => busy |= processed > 0,
                            Err(error) => owner.failed(error.to_string()),
                        }
                        if !busy {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                    if let Err(error) = owner.refresh_devices().await {
                        owner.failed(error);
                    }
                    drop(owner);
                    configure = tokio::select! {
                        biased;
                        _ = tokio::time::sleep(Duration::from_secs(300)) => true,
                        _ = configuration.changed() => true,
                        _ = catalog.changed() => false,
                        _ = settings.changed() => false,
                        _ = cache.changed() => false,
                    };
                    catalog.borrow_and_update();
                    if !configure
                        && let Some(owner) = weak.upgrade()
                        && let Some(network) = owner.network.lock().await.as_ref()
                    {
                        network.refresh();
                    }
                }
            })
            .await;
        });
    }

    fn failed(&self, error: String) {
        tracing::warn!(%error, "Rufin Connect failed");
        self.status.send_modify(|status| {
            status.error = Some(error);
            if status.collection_progress.is_some() {
                status.connecting = false;
                status.receiving_collection = false;
            }
            status.collection_progress = None;
        });
    }

    fn collection_progress(&self, stage: CollectionStage, completed: u64, total: Option<u64>) {
        self.status.send_if_modified(|status| {
            let next = CollectionProgress {
                stage,
                completed,
                total,
            };
            if status.collection_progress.as_ref().is_some_and(|previous| {
                let step = if matches!(
                    stage,
                    CollectionStage::Receiving | CollectionStage::Validating
                ) {
                    1_048_576
                } else {
                    1
                };
                previous.stage == stage
                    && previous.total == total
                    && previous.completed / step == completed / step
                    && (Some(previous.completed) == total) == (Some(completed) == total)
            }) {
                return false;
            }
            status.profile_status = stage.label();
            status.collection_progress = Some(next);
            true
        });
    }

    fn save(&self, edit: impl FnOnce(&mut ConnectSettings)) -> Result<(), String> {
        let mut config = self.status().settings;
        let previous = config.pending_import.clone();
        edit(&mut config);
        write_device_config(&self.directory, &config)?;
        if previous != config.pending_import
            && let Some(PendingImport::Prepared { path, .. }) = previous
            && let Err(error) = std::fs::remove_file(path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%error, "Could not remove prepared profile");
        }
        self.source.peer_access().set_folders(
            config
                .folders
                .iter()
                .map(|(key, directory)| {
                    (
                        key.clone(),
                        match directory {
                            downloads::DownloadDirectory::Native(path) => {
                                library::LocalMediaLocation::File(path.clone())
                            }
                            downloads::DownloadDirectory::Document { uri } => {
                                library::LocalMediaLocation::Document(uri.clone())
                            }
                        },
                    )
                })
                .collect(),
        );
        self.status.send_modify(|status| {
            status.settings = config;
        });
        Ok(())
    }

    fn identity_reference(&self) -> Result<String, String> {
        if let Some(reference) = self.status().settings.identity_ref {
            return Ok(reference);
        }
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(error)?;
        let reference = bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        // Persist before reading credentials so another Rufin data directory using
        // the same system keyring still owns a distinct private identity.
        self.save(|config| config.identity_ref = Some(reference.clone()))?;
        Ok(reference)
    }

    async fn secret(&self, key: SecretKey) -> Result<Option<String>, String> {
        let secrets = Arc::clone(&self.secrets);
        tokio::task::spawn_blocking(move || secrets.load_secret(&key).map_err(error))
            .await
            .map_err(error)?
    }

    async fn save_secret(&self, key: SecretKey, value: String) -> Result<(), String> {
        let secrets = Arc::clone(&self.secrets);
        tokio::task::spawn_blocking(move || secrets.save_secret(&key, &value).map_err(error))
            .await
            .map_err(error)?
    }

    async fn local_key(
        &self,
        profile: &str,
        create: bool,
    ) -> Result<age::x25519::Identity, String> {
        let reference = local_key(&self.identity_reference()?, profile);
        if let Some(value) = self.secret(reference.clone()).await? {
            return value.parse().map_err(error);
        }
        if !create {
            return Err(
                "The Connect local secrets key is unavailable. Pair with a trusted device again"
                    .into(),
            );
        }
        let key = portable::new_key();
        self.save_secret(reference, key.clone()).await?;
        key.parse().map_err(error)
    }

    async fn install_file_key(&self, profile: &str, key: String) -> Result<(), String> {
        let _: age::x25519::Identity = key.parse().map_err(error)?;
        if !self
            .status()
            .settings
            .file_profiles
            .iter()
            .any(|id| id == profile)
        {
            self.save(|config| config.file_profiles.push(profile.to_owned()))?;
        }
        if let Some(previous) = self
            .secret(file_key(&self.identity_reference()?, profile))
            .await?
        {
            if previous == key {
                return Ok(());
            }
            let mut retained: Vec<String> = self
                .secret(previous_file_keys(&self.identity_reference()?, profile))
                .await?
                .map(|value| serde_json::from_str(&value))
                .transpose()
                .map_err(error)?
                .unwrap_or_default();
            if !retained.contains(&previous) {
                retained.push(previous);
                self.save_secret(
                    previous_file_keys(&self.identity_reference()?, profile),
                    serde_json::to_string(&retained).map_err(error)?,
                )
                .await?;
            }
        }
        self.save_secret(file_key(&self.identity_reference()?, profile), key)
            .await
    }

    async fn credentials(&self) -> Result<Credentials, String> {
        let key = identity_key(&self.identity_reference()?);
        match self.secret(key.clone()).await? {
            Some(value) => {
                let encoded: serde_json::Value = serde_json::from_str(&value).map_err(error)?;
                // Preserve the existing Ed25519 identity when dropping the old
                // encryption-key bundle. Device identity does not change.
                if let Some(signing_key) = encoded
                    .get("signing_key")
                    .and_then(serde_json::Value::as_str)
                {
                    let credentials: Credentials = signing_key.parse().map_err(error)?;
                    self.save_secret(key, serde_json::to_string(&credentials).map_err(error)?)
                        .await?;
                    Ok(credentials)
                } else {
                    serde_json::from_value(encoded).map_err(error)
                }
            }
            None => {
                let credentials = Credentials::generate();
                self.save_secret(key, serde_json::to_string(&credentials).map_err(error)?)
                    .await?;
                Ok(credentials)
            }
        }
    }

    async fn connected_network(&self) -> Result<Arc<ConnectNetwork>, String> {
        self.network
            .lock()
            .await
            .clone()
            .ok_or_else(|| "Rufin Connect is disabled".into())
    }

    fn network(
        self: &Arc<Self>,
    ) -> futures_util::future::BoxFuture<'_, Result<Arc<ConnectNetwork>, String>> {
        Box::pin(async move {
            let mut current = self.network.lock().await;
            if let Some(network) = current.as_ref() {
                return Ok(Arc::clone(network));
            }
            std::fs::create_dir_all(&self.directory).map_err(error)?;
            let credentials = self.credentials().await?;
            let settings = self.status().settings;
            // Discard the preview's duplicate audio cache. Missing media uses
            // the normal download path; existing library files remain usable.
            match tokio::fs::remove_dir_all(self.directory.join("blobs")).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(cause) => return Err(error(cause)),
            }
            let (network, mut events) = ConnectNetwork::spawn(
                NetworkConfig {
                    database: self.directory.join("network.sqlite"),
                    name: settings.name,
                    nearby: settings.nearby,
                    relay: settings.relay,
                    public_relay: settings.public_relay,
                },
                credentials,
            )
            .await
            .map_err(error)?;
            let identity = network.identity().to_string();
            let invitation = network.invitation().await.map_err(error)?;
            self.status.send_modify(|status| {
                status.identity = Some(identity);
                status.invitation = Some(invitation);
                if !status.connecting {
                    status.profile_status = localization::tr("Waiting for a peer");
                }
            });
            let weak = Arc::downgrade(self);
            let event_network = Arc::downgrade(&network);
            self.runtime.spawn(async move {
                while let Some(event) = events.recv().await {
                    let Some(owner) = weak.upgrade() else { return };
                    let Some(event_network) = event_network.upgrade() else {
                        return;
                    };
                    if !owner
                        .network
                        .lock()
                        .await
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, &event_network))
                    {
                        return;
                    }
                    if let Err(error) = owner.event(event).await {
                        owner.failed(error);
                    }
                }
            });
            self.source
                .peer_access()
                .install(Some(media::peer_connection(
                    network.clone(),
                    self.status.subscribe(),
                )));
            self.source.peer_access().set_folders(
                self.status()
                    .settings
                    .folders
                    .into_iter()
                    .map(|(key, directory)| {
                        (
                            key,
                            match directory {
                                downloads::DownloadDirectory::Native(path) => {
                                    library::LocalMediaLocation::File(path)
                                }
                                downloads::DownloadDirectory::Document { uri } => {
                                    library::LocalMediaLocation::Document(uri)
                                }
                            },
                        )
                    })
                    .collect(),
            );
            *current = Some(Arc::clone(&network));
            Ok(network)
        })
    }

    async fn migrate_legacy_profile(&self) -> Result<Option<String>, String> {
        let working = ProfileStore::database_profile(self.database.path())
            .await
            .map_err(error)?;
        if working.is_some() {
            return Ok(working);
        }
        let settings = self.status().settings;
        let Some(profile) = settings.profile else {
            return Ok(None);
        };
        if settings
            .pending_import
            .as_ref()
            .is_some_and(|pending| pending.profile() == profile)
        {
            return Ok(None);
        }
        let path = self.directory.join(format!(
            "{}.profile.sqlite",
            blake3::hash(profile.as_bytes()).to_hex()
        ));
        if !ProfileStore::migrate_legacy_database(self.database.as_ref(), &path, &profile)
            .await
            .map_err(error)?
        {
            return Ok(None);
        }
        Ok(Some(profile))
    }

    async fn open(self: &Arc<Self>, profile: String, create: bool) -> Result<(), String> {
        let identity = self.credentials().await?.public().to_string();
        self.status
            .send_modify(|status| status.identity = Some(identity.clone()));
        let key = match self
            .secret(file_key(&self.identity_reference()?, &profile))
            .await?
        {
            Some(key) => key,
            None if create => {
                let key = portable::new_key();
                self.install_file_key(&profile, key.clone()).await?;
                key
            }
            None => {
                return Err(
                    "The Connect file key is unavailable. Pair with a trusted device again".into(),
                );
            }
        };
        if !self.status().settings.file_profiles.contains(&profile) {
            self.save(|config| config.file_profiles.push(profile.clone()))?;
        }
        let peer = u64::from_le_bytes(
            blake3::hash(identity.as_bytes()).as_bytes()[..8]
                .try_into()
                .unwrap(),
        );
        let path = self.directory.join(format!(
            "{}.profile.sqlite",
            blake3::hash(profile.as_bytes()).to_hex()
        ));
        let working = self.migrate_legacy_profile().await?;
        if working.as_deref() != Some(&profile) && !create {
            return Err(localization::tr(
                "Profile download is incomplete. Retry to continue.",
            ));
        }
        let local_key = self.local_key(&profile, create).await?;
        let documents = Arc::new(
            ProfileStore::open(self.database.as_ref().clone(), peer, local_key)
                .await
                .map_err(error)?,
        );
        documents
            .bind_profile(&profile, &identity)
            .await
            .map_err(error)?;
        let (legacy_settings, baseline) =
            documents.legacy_settings_records().await.map_err(error)?;
        if !legacy_settings.is_empty() {
            let settings = self.settings_apply.clone();
            tokio::task::spawn_blocking(move || {
                settings.migrate_legacy_settings(&legacy_settings, &baseline)
            })
            .await
            .map_err(error)??;
        }
        documents.clear_legacy_settings().await.map_err(error)?;
        if tokio::fs::try_exists(&path).await.map_err(error)?
            && !matches!(&self.status().settings.pending_import, Some(PendingImport::Prepared { path: pending, .. }) if pending == &path)
        {
            for suffix in ["", "-wal", "-shm", "-journal"] {
                let old = PathBuf::from(format!("{}{suffix}", path.display()));
                match tokio::fs::remove_file(old).await {
                    Ok(()) => {}
                    Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => {}
                    Err(cause) => return Err(error(cause)),
                }
            }
        }
        if let Some(previous) = self.session.write().await.replace(Arc::new(Session {
            profile: profile.clone(),
            documents: documents.clone(),
            identity,
            stop: tokio_util::sync::CancellationToken::new(),
            receiving: Mutex::new(BTreeSet::new()),
            settings_peers: Mutex::new(BTreeMap::new()),
            revision: std::sync::atomic::AtomicI64::new(-1),
            received_changes: std::sync::atomic::AtomicBool::new(false),
        })) {
            previous.stop.cancel();
            previous.documents.close().await.map_err(error)?;
        }
        if self.status().settings.enabled {
            if let Err(error) = self
                .network()
                .await?
                .open_profile(
                    &profile,
                    create,
                    serde_json::to_vec(&Enrollment {
                        file_key: key.clone(),
                    })
                    .map_err(error)?,
                )
                .await
            {
                *self.session.write().await = None;
                return Err(error.to_string());
            }
        }
        if self.status().settings.enabled {
            self.network()
                .await?
                .attach_documents(documents)
                .await
                .map_err(error)?;
        }
        if create {
            self.active()
                .await?
                .documents
                .write_records(&[
                    ConnectRecord {
                        kind: "connect_key".into(),
                        key: "file".into(),
                        value: Some(serde_json::json!(key)),
                    },
                    ConnectRecord {
                        kind: "connect_network".into(),
                        key: "relay".into(),
                        value: Some(serde_json::json!(self.status().settings.relay)),
                    },
                    ConnectRecord {
                        kind: "connect_network".into(),
                        key: "public_relay".into(),
                        value: Some(serde_json::json!(self.status().settings.public_relay)),
                    },
                ])
                .await
                .map_err(error)?;
            self.publish_device().await?;
        }
        self.database
            .connect_set_active(true)
            .await
            .map_err(error)?;
        crate::playlist_files::backfill_file_playlists(&self.source).await?;
        self.save(|config| config.profile = Some(profile.clone()))?;
        self.start_session(self.active().await?);
        Ok(())
    }

    async fn create_profile(self: &Arc<Self>) -> Result<(), String> {
        self.open(ConnectNetwork::new_profile_id(), true).await?;
        self.database
            .connect_initialize_profile()
            .await
            .map_err(error)?;
        self.database.connect_seed_page().await.map_err(error)?;
        Ok(())
    }

    async fn active(&self) -> Result<Arc<Session>, String> {
        self.session
            .read()
            .await
            .clone()
            .ok_or_else(|| "Create or join a Connect profile first".into())
    }

    async fn configure_network(self: &Arc<Self>) -> Result<(), String> {
        let mut current = self.network.lock().await;
        let settings = self.status().settings;
        let Some(network) = current.as_ref() else {
            return Ok(());
        };
        if network
            .matches_configuration(
                settings.nearby,
                settings.relay.as_deref(),
                settings.public_relay,
            )
            .map_err(error)?
        {
            return Ok(());
        }
        // Iroh fixes the available transports at bind time. An empty relay map
        // still allows connections to peers' relays, so rebuild when disabled.
        current.take().unwrap().shutdown().await.map_err(error)?;
        drop(current);
        let network = self.network().await?;
        if let Some(session) = self.session.read().await.clone() {
            let key = self
                .secret(file_key(&self.identity_reference()?, &session.profile))
                .await?
                .ok_or("The Connect file key is unavailable")?;
            network
                .open_profile(
                    &session.profile,
                    false,
                    serde_json::to_vec(&Enrollment { file_key: key }).map_err(error)?,
                )
                .await
                .map_err(error)?;
            network
                .attach_documents(session.documents.clone())
                .await
                .map_err(error)?;
        }
        Ok(())
    }

    pub(crate) async fn close_storage(&self) -> Result<(), String> {
        self.cancel_download();
        let _actions = self.actions.lock().await;
        if let Some(session) = self.session.write().await.take() {
            session.stop.cancel();
            session.documents.close().await.map_err(error)?;
        }
        if let Some(network) = self.network.lock().await.take() {
            network.shutdown().await.map_err(error)?;
        }
        Ok(())
    }

    async fn leave_profile(&self) -> Result<(), String> {
        let profile = self.status().settings.profile;
        self.save(|config| {
            *config = ConnectSettings {
                name: config.name.clone(),
                identity_ref: config.identity_ref.clone(),
                ..ConnectSettings::default()
            }
        })?;
        let session = self.session.write().await.take();
        if let Some(session) = &session {
            session.stop.cancel();
        }
        let network = self.network.lock().await.take();
        self.source.peer_access().install(None);
        self.status.send_modify(|status| {
            status.devices.clear();
            status.pairing = None;
            status.connecting = false;
            status.receiving_collection = false;
            status.collection_progress = None;
            status.invitation = None;
            status.identity = None;
            status.profile_status = localization::tr("Connect is not enabled");
        });
        let _sync = self.sync.lock().await;
        self.transfers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        let inactive = self.database.connect_set_active(false).await.map_err(error);
        let closed = if let Some(network) = network {
            if let Some(profile) = profile {
                if let Err(error) = network.leave_profile(&profile).await {
                    tracing::warn!(%error, "Could not announce Connect departure");
                }
            }
            network.shutdown().await.map_err(error)
        } else {
            Ok(())
        };
        if let Some(session) = session {
            session.documents.close().await.map_err(error)?;
        }
        ProfileStore::clear_database(&self.database)
            .await
            .map_err(error)?;
        self.cleanup_profiles(None).await?;
        inactive.and(closed)
    }

    async fn cleanup_profiles(&self, keep: Option<&str>) -> Result<(), String> {
        let keep = keep.map(|profile| blake3::hash(profile.as_bytes()).to_hex().to_string());
        let pending = match self.status().settings.pending_import {
            Some(PendingImport::Prepared { path, .. }) => Some(path),
            _ => None,
        };
        let mut files = match tokio::fs::read_dir(&self.directory).await {
            Ok(files) => files,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(cause) => return Err(error(cause)),
        };
        while let Some(file) = files.next_entry().await.map_err(error)? {
            let name = file.file_name();
            let Some((profile, suffix)) = name.to_str().and_then(|name| name.split_once('.'))
            else {
                continue;
            };
            if profile.len() != 64
                || !profile.bytes().all(|byte| byte.is_ascii_hexdigit())
                || pending
                    .as_ref()
                    .is_some_and(|pending| pending == &file.path())
                || keep.as_ref().is_some_and(|keep| {
                    keep.eq_ignore_ascii_case(profile) && suffix.starts_with("profile.sqlite")
                })
                || !matches!(
                    suffix,
                    "sqlite"
                        | "sqlite-wal"
                        | "sqlite-shm"
                        | "sqlite-journal"
                        | "profile.sqlite"
                        | "profile.sqlite-wal"
                        | "profile.sqlite-shm"
                        | "profile.sqlite-journal"
                        | "pending.sqlite"
                        | "pending.sqlite-wal"
                        | "pending.sqlite-shm"
                        | "pending.sqlite-journal"
                )
            {
                continue;
            }
            match tokio::fs::remove_file(file.path()).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(cause) => return Err(error(cause)),
            }
        }
        Ok(())
    }

    fn cancel_download(&self) {
        if let Some(operation) = self
            .import_operation
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            operation.cancel.cancel();
        }
    }

    fn begin_import(&self, session: Option<String>) -> tokio_util::sync::CancellationToken {
        let cancel = tokio_util::sync::CancellationToken::new();
        if let Some(previous) = self
            .import_operation
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .replace(ImportOperation {
                session,
                replace: true,
                cancel: cancel.clone(),
            })
        {
            previous.cancel.cancel();
        }
        cancel
    }

    pub(crate) async fn reset_secret_storage(&self) -> Result<(), String> {
        self.cancel_download();
        let _actions = self.actions.lock().await;
        self.leave_profile().await
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl ConnectOwner {
    pub async fn import_document(
        self: &Arc<Self>,
        uri: String,
        replace: bool,
        key: Option<String>,
    ) -> Result<Status, ActionError> {
        let owner = Arc::clone(self);
        self.runtime
            .spawn(async move {
                tokio::fs::create_dir_all(&owner.directory)
                    .await
                    .map_err(error)?;
                let input = tempfile::Builder::new()
                    .prefix("profile-transfer-")
                    .tempfile_in(&owner.directory)
                    .map_err(error)?;
                let path = input.path().to_owned();
                tokio::task::spawn_blocking(move || {
                    sources::copy_document(&uri, &path).map_err(error)
                })
                .await
                .map_err(error)??;
                let _action = owner.actions.lock().await;
                let result = owner
                    .read_profile_file(input.path().to_owned(), replace, key, None, true)
                    .await;
                match &result {
                    Ok(()) => owner.status.send_modify(|status| status.error = None),
                    Err(ActionError::Failed(message)) => owner.failed(message.clone()),
                    Err(_) => {}
                }
                result?;
                Ok(owner.status())
            })
            .await
            .map_err(|error| ActionError::Failed(error.to_string()))?
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl ConnectOwner {
    pub async fn list_profile_files(
        &self,
        source_id: sources::SourceId,
        folder: String,
    ) -> Result<Vec<String>, ActionError> {
        let source = self.source.clone();
        self.runtime
            .spawn(async move {
                source
                    .client(&source_id)?
                    .profile_file_source()
                    .map_err(|failure| ActionError::Failed(failure.to_string()))?
                    .list_profile_files(&folder)
                    .await
                    .map(|files| files.into_iter().map(|(path, _)| path).collect())
                    .map_err(|failure| ActionError::Failed(failure.to_string()))
            })
            .await
            .map_err(|error| ActionError::Failed(error.to_string()))?
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl ConnectOwner {
    pub async fn export_document(self: &Arc<Self>, uri: String) -> Result<Status, ActionError> {
        let owner = Arc::clone(self);
        self.runtime
            .spawn(async move {
                tokio::fs::create_dir_all(&owner.directory)
                    .await
                    .map_err(error)?;
                let output = tempfile::Builder::new()
                    .prefix("profile-transfer-")
                    .tempfile_in(&owner.directory)
                    .map_err(error)?;
                owner
                    .execute(Action::Export {
                        path: output.path().to_owned(),
                        source_id: None,
                    })
                    .await?;
                tokio::task::spawn_blocking(move || {
                    sources::save_document(&uri, output.path(), None).map_err(error)
                })
                .await
                .map_err(error)??;
                Ok(owner.status())
            })
            .await
            .map_err(|error| ActionError::Failed(error.to_string()))?
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl ConnectOwner {
    pub async fn execute(self: &Arc<Self>, action: Action) -> Result<Status, ActionError> {
        let owner = Arc::clone(self);
        self.runtime
            .spawn(async move {
                if matches!(action, Action::Join { replace: false, .. }) {
                    return Err(Confirmation::ReplaceProfile.required());
                }
                if matches!(action, Action::Leave | Action::Enable { enabled: false }) {
                    if let Some(session) = owner.session.read().await.as_ref() {
                        session.stop.cancel();
                    }
                }
                if matches!(
                    action,
                    Action::CancelDownload
                        | Action::Leave
                        | Action::Enable { enabled: false }
                        | Action::Join { replace: true, .. }
                        | Action::CancelPairing
                ) {
                    owner.cancel_download();
                }
                if matches!(action, Action::CancelDownload) {
                    owner.status.send_modify(|status| {
                        status.connecting = false;
                        status.receiving_collection = false;
                        status.collection_progress = None;
                    });
                    return Ok(owner.status());
                }
                let refresh = matches!(action, Action::Refresh);
                // Playback and ordinary refresh do not change device setup.
                let _action = if matches!(action, Action::Control { .. }) || refresh {
                    None
                } else {
                    Some(owner.actions.lock().await)
                };
                if matches!(action, Action::Retry) {
                    owner.status.send_modify(|status| status.error = None);
                }
                let preserve_error = matches!(action, Action::CancelPairing);
                let result = async {
                    match action {
                        Action::Enable { enabled: false } | Action::Leave => {
                            owner.leave_profile().await?
                        }
                        Action::Enable { enabled: true } => {
                            owner.save(|config| config.enabled = true)?;
                            if owner.status().settings.pending_import.is_some() {
                                let cancel = owner.begin_import(None);
                                owner.resume_import(cancel).await?;
                            } else if let Some(profile) = owner.status().settings.profile {
                                if owner.session.read().await.is_none() {
                                    owner.open(profile, false).await?;
                                }
                            } else {
                                let _sync = owner.sync.lock().await;
                                owner.create_profile().await?;
                            }
                            owner.synchronize().await?;
                            owner
                                .cleanup_profiles(owner.status().settings.profile.as_deref())
                                .await?;
                        }
                        Action::Create => {
                            if owner.status().settings.profile.is_some() {
                                return Err("This device already has a Connect profile".into());
                            }
                            owner.save(|config| config.enabled = true)?;
                            {
                                let _sync = owner.sync.lock().await;
                                owner.create_profile().await?;
                            }
                            owner.synchronize().await?;
                            owner
                                .cleanup_profiles(owner.status().settings.profile.as_deref())
                                .await?;
                        }
                        Action::Join { invitation, .. } => {
                            owner.begin_import(None);
                            owner.status.send_modify(|status| {
                                status.connecting = true;
                                status.receiving_collection = false;
                                status.collection_progress = None;
                                status.error = None;
                                status.profile_status = localization::tr("Connecting devices");
                            });
                            owner.save(|config| config.enabled = true)?;
                            if let Err(error) =
                                owner.network().await?.begin_pairing(&invitation).await
                            {
                                owner
                                    .import_operation
                                    .lock()
                                    .unwrap_or_else(|p| p.into_inner())
                                    .take();
                                owner.status.send_modify(|status| status.connecting = false);
                                return Err(error.to_string().into());
                            }
                        }
                        Action::Invite { peer } => {
                            owner.save(|config| config.enabled = true)?;
                            if owner.status().settings.profile.is_none() {
                                owner.create_profile().await?;
                            } else if owner.network.lock().await.is_none() {
                                owner
                                    .open(owner.status().settings.profile.unwrap(), false)
                                    .await?;
                            }
                            owner.status.send_modify(|status| {
                                status.connecting = true;
                                status.profile_status = localization::tr("Connecting devices");
                            });
                            let result = owner
                                .connected_network()
                                .await?
                                .invite(&peer)
                                .await
                                .map_err(error);
                            if result.is_err() {
                                owner.status.send_modify(|status| status.connecting = false);
                            }
                            result?;
                        }
                        Action::Pair {
                            session,
                            approve,
                            replace,
                        } => {
                            if approve
                                && owner.status().pairing.as_ref().is_some_and(|pairing| {
                                    pairing.session == session && pairing.approved
                                })
                            {
                                return Ok(owner.status());
                            }
                            if approve
                                && owner.status().pairing.as_ref().is_some_and(|pairing| {
                                    pairing.session == session && pairing.joining
                                })
                                && !replace
                                && !owner
                                    .import_operation
                                    .lock()
                                    .unwrap_or_else(|p| p.into_inner())
                                    .as_ref()
                                    .is_some_and(|operation| operation.replace)
                            {
                                return Err(Confirmation::ReplaceProfile.required());
                            }
                            if !approve {
                                owner.cancel_download();
                            }
                            owner
                                .connected_network()
                                .await?
                                .confirm_pairing(&session, approve)
                                .await
                                .map_err(error)?;
                            owner.status.send_modify(|status| {
                                if let Some(pairing) = &mut status.pairing
                                    && pairing.session == session
                                {
                                    pairing.approved = approve;
                                }
                            });
                            if !approve {
                                owner.status.send_modify(|status| {
                                    status.pairing = None;
                                    status.connecting = false;
                                });
                            }
                        }
                        Action::CancelPairing => {
                            owner.connected_network().await?.cancel_pairing().await;
                            owner.status.send_modify(|status| {
                                status.pairing = None;
                                status.connecting = false;
                                status.receiving_collection = false;
                                status.collection_progress = None;
                            });
                        }
                        Action::Rename { name } => {
                            owner.save(|config| config.name = name.clone())?;
                            if let Some(network) = owner.network.lock().await.as_ref() {
                                network.rename(name).await;
                            }
                            owner.publish_device().await?;
                        }
                        Action::Remove { peer } => {
                            let session = owner.active().await?;
                            owner
                                .connected_network()
                                .await?
                                .remove_member(&peer)
                                .await
                                .map_err(error)?;
                            let key = portable::new_key();
                            let _sync = owner.sync.lock().await;
                            owner
                                .install_file_key(&session.profile, key.clone())
                                .await?;
                            owner
                                .connected_network()
                                .await?
                                .set_enrollment_data(
                                    serde_json::to_vec(&Enrollment {
                                        file_key: key.clone(),
                                    })
                                    .map_err(error)?,
                                )
                                .await;
                            session
                                .documents
                                .write_records(&[ConnectRecord {
                                    kind: "connect_key".into(),
                                    key: "file".into(),
                                    value: Some(serde_json::json!(key)),
                                }])
                                .await
                                .map_err(error)?;
                            drop(_sync);
                            owner.synchronize().await?;
                            owner.refresh_devices().await?;
                        }
                        Action::Refresh | Action::Retry => {
                            if !refresh && owner.status().settings.pending_import.is_some() {
                                let cancel = owner.begin_import(None);
                                owner.resume_import(cancel).await?;
                            } else if !refresh
                                && owner.session.read().await.is_none()
                                && let Some(profile) = owner.status().settings.profile
                            {
                                owner.open(profile, false).await?;
                            }
                            let session = owner.session.read().await.clone();
                            if let Some(session) = session {
                                session
                                    .stop
                                    .run_until_cancelled(async {
                                        owner.synchronize().await?;
                                        if let Some(network) = owner.network.lock().await.as_ref() {
                                            network.refresh();
                                        }
                                        if owner.status().settings.enabled {
                                            owner.refresh_devices().await?;
                                        }
                                        Ok::<_, ActionError>(())
                                    })
                                    .await
                                    .transpose()?;
                            }
                        }
                        Action::Discover => {
                            owner.save(|config| config.enabled = true)?;
                            if owner.network.lock().await.is_none() {
                                if let Some(profile) = owner.status().settings.profile {
                                    let _sync = owner.sync.lock().await;
                                    owner.open(profile, false).await?;
                                } else {
                                    owner.network().await?;
                                }
                            }
                        }
                        Action::TestConnection { peer } => {
                            let result = owner
                                .connected_network()
                                .await?
                                .test_connection(
                                    &peer,
                                    serde_json::to_vec(&Request::Presence).map_err(error)?,
                                )
                                .await
                                .map(|(route, _)| route)
                                .map_err(error);
                            owner.status.send_modify(|status| {
                                if let Some(device) =
                                    status.devices.iter_mut().find(|device| device.id == peer)
                                {
                                    device.reachable = result.is_ok();
                                    device.connection = match &result {
                                        Ok(route) => connection_label(route),
                                        Err(_) => localization::tr("Waiting for device"),
                                    };
                                    if result.is_err() {
                                        device.has_playback = false;
                                    }
                                }
                            });
                            result?;
                        }
                        Action::Network {
                            nearby,
                            relay,
                            public_relay,
                        } => {
                            if let Some(relay) = &relay {
                                reqwest::Url::parse(relay).map_err(error)?;
                            }
                            if let Some(session) = owner.session.read().await.clone() {
                                session
                                    .documents
                                    .write_records(&[
                                        ConnectRecord {
                                            kind: "connect_network".into(),
                                            key: "relay".into(),
                                            value: Some(serde_json::json!(relay)),
                                        },
                                        ConnectRecord {
                                            kind: "connect_network".into(),
                                            key: "public_relay".into(),
                                            value: Some(serde_json::json!(public_relay)),
                                        },
                                    ])
                                    .await
                                    .map_err(error)?;
                                owner.synchronize().await?;
                            }
                            owner.save(|config| {
                                config.nearby = nearby;
                                config.relay = relay;
                                config.public_relay = public_relay;
                            })?;
                            owner.configure_network().await?;
                        }
                        Action::Continue { peer } => owner.continue_from(&peer).await?,
                        Action::Control { peer, command } => {
                            owner.request(&peer, Request::Control { command }).await?;
                        }
                        Action::Folder {
                            source,
                            root_id,
                            path,
                        } => {
                            let key = root_id
                                .map_or_else(|| source.clone(), |root| format!("{source}/{root}"));
                            owner.save(|config| {
                                if match &path {
                                    downloads::DownloadDirectory::Native(path) => {
                                        path.as_os_str().is_empty()
                                    }
                                    downloads::DownloadDirectory::Document { uri } => {
                                        uri.is_empty()
                                    }
                                } {
                                    config.folders.remove(&key);
                                } else {
                                    config.folders.insert(key, path);
                                }
                            })?;
                            owner.source.connect_media_changed();
                        }
                        Action::Download { peer: _, uri } => {
                            owner.source.download_media(
                                downloads::DownloadSubject::Prepared {
                                    context_id: "connect".into(),
                                    title: None,
                                },
                                vec![uri],
                            );
                        }
                        Action::CancelDownload => owner.cancel_download(),
                        Action::Export { path, source_id } => {
                            let output = owner.export().await?;
                            if let Some(source_id) = source_id {
                                let source = owner.source.client(&source_id)?;
                                source
                                    .profile_file_source()
                                    .map_err(error)?
                                    .write_profile_file(&path.to_string_lossy(), output, Some(None))
                                    .await
                                    .map_err(error)?;
                            } else {
                                portable::install(output, path).await?;
                            }
                        }
                        Action::Import {
                            path,
                            source_id,
                            replace,
                            key,
                        } => {
                            if let Some(source_id) = source_id {
                                let source = owner.source.client(&source_id)?;
                                std::fs::create_dir_all(&owner.directory).map_err(error)?;
                                let incoming = tempfile::Builder::new()
                                    .prefix("profile-transfer-")
                                    .tempfile_in(&owner.directory)
                                    .map_err(error)?;
                                if source
                                    .profile_file_source()
                                    .map_err(error)?
                                    .copy_file(&path.to_string_lossy(), incoming.path())
                                    .await
                                    .map_err(error)?
                                    .is_none()
                                {
                                    return Err("The Connect file was not found".into());
                                }
                                owner
                                    .read_profile_file(
                                        incoming.path().to_owned(),
                                        replace,
                                        key,
                                        None,
                                        true,
                                    )
                                    .await?;
                            } else {
                                owner.import_file(path, replace, key).await?;
                            }
                        }
                    }
                    if !preserve_error {
                        owner.status.send_modify(|status| status.error = None);
                    }
                    Ok(owner.status())
                }
                .await;
                if let Err(ActionError::Failed(message)) = &result {
                    owner.failed(message.clone());
                }
                result
            })
            .await
            .map_err(|error| ActionError::Failed(error.to_string()))?
    }
}

impl ConnectOwner {
    async fn event(self: &Arc<Self>, event: NetworkEvent) -> Result<(), String> {
        match event {
            NetworkEvent::Invitation(invitation) => {
                self.status.send_if_modified(|status| {
                    if status.invitation.as_ref() == Some(&invitation) {
                        return false;
                    }
                    status.invitation = Some(invitation);
                    true
                });
            }
            NetworkEvent::Pairing {
                session,
                peer,
                name,
                emoji,
                joining,
            } => {
                if joining {
                    let mut operation = self
                        .import_operation
                        .lock()
                        .unwrap_or_else(|p| p.into_inner());
                    match operation.as_mut() {
                        Some(operation) if operation.session.is_none() => {
                            operation.session = Some(session.clone())
                        }
                        _ => {
                            *operation = Some(ImportOperation {
                                session: Some(session.clone()),
                                replace: false,
                                cancel: tokio_util::sync::CancellationToken::new(),
                            });
                        }
                    }
                }
                self.status.send_modify(|status| {
                    status.error = None;
                    status.connecting = false;
                    status.pairing = Some(Pairing {
                        session,
                        peer,
                        name,
                        emoji: emoji.into(),
                        approved: false,
                        verified: false,
                        joining,
                    });
                });
            }
            NetworkEvent::PairingVerified { session } => {
                self.status.send_modify(|status| {
                    if let Some(pairing) = &mut status.pairing
                        && pairing.session == session
                    {
                        pairing.approved = true;
                        pairing.verified = true;
                    }
                });
            }
            NetworkEvent::Paired {
                session,
                profile_id,
                peer,
                name,
                enrollment_data,
            } => {
                let owner = self.clone();
                let network = self.connected_network().await?;
                self.runtime.spawn(async move {
                    if let Err(error) = owner
                        .adopt_paired(&network, &session, profile_id, peer, name, enrollment_data)
                        .await
                    {
                        owner.status.send_modify(|status| {
                            status.pairing = None;
                            status.connecting = false;
                            status.receiving_collection = false;
                            status.collection_progress = None;
                        });
                        owner.failed(error);
                    }
                });
            }
            NetworkEvent::MemberAdded { peer, name } => {
                self.status.send_modify(|status| {
                    status.completed_pairings += 1;
                    status.error = None;
                    status.pairing = None;
                    status.connecting = false;
                    status.devices.retain(|device| device.id != peer);
                    status.devices.push(Device {
                        id: peer,
                        name,
                        reachable: true,
                        has_playback: false,
                        download_profiles: false,
                        enrolled: true,
                        connection: String::new(),
                    });
                });
            }
            NetworkEvent::Discovered { peer, name } => {
                self.status.send_if_modified(|status| {
                    if let Some(device) = status.devices.iter_mut().find(|device| device.id == peer)
                    {
                        if device.name == name {
                            return false;
                        }
                        device.name = name;
                    } else {
                        status.devices.push(Device {
                            id: peer,
                            name,
                            reachable: true,
                            has_playback: false,
                            download_profiles: false,
                            enrolled: false,
                            connection: String::new(),
                        });
                    }
                    true
                });
            }
            NetworkEvent::DiscoveryExpired { peer } => {
                self.status.send_if_modified(|status| {
                    let previous = status.devices.len();
                    status
                        .devices
                        .retain(|device| device.enrolled || device.id != peer);
                    status.devices.len() != previous
                });
            }
            NetworkEvent::MembershipChanged => {
                if self.session.read().await.is_some() {
                    let owner = self.clone();
                    self.runtime.spawn(async move {
                        let result = async {
                            let network = owner.connected_network().await?;
                            if network
                                .members()
                                .await
                                .map_err(error)?
                                .contains(&network.identity())
                            {
                                owner.refresh_devices().await
                            } else {
                                owner
                                    .execute(Action::Leave)
                                    .await
                                    .map(|_| ())
                                    .map_err(error)
                            }
                        }
                        .await;
                        if let Err(error) = result {
                            owner.failed(error);
                        }
                    });
                }
            }
            NetworkEvent::Settings { profile, peer } => {
                if self.status().settings.profile.as_deref() == Some(&profile) {
                    let owner = Arc::clone(self);
                    self.runtime.spawn(async move {
                        let result = async {
                            let session = owner.active().await?;
                            if session.profile != profile {
                                return Ok(());
                            }
                            session
                                .stop
                                .run_until_cancelled(owner.exchange_settings(&session, &peer))
                                .await
                                .unwrap_or(Ok(()))
                        }
                        .await;
                        if let Err(error) = result {
                            tracing::debug!(%error, "Connect settings exchange unavailable");
                        }
                    });
                }
            }
            NetworkEvent::Request {
                profile,
                peer,
                body,
                reply,
            } => {
                let owner = Arc::clone(self);
                self.runtime.spawn(async move {
                    let result = match owner.active().await {
                        Ok(session) => session
                            .stop
                            .run_until_cancelled(owner.answer(&session, &profile, &peer, &body))
                            .await
                            .unwrap_or_else(|| Err("Transfer cancelled".into())),
                        Err(error) => Err(error),
                    };
                    let _ = reply.send(result);
                });
            }
            NetworkEvent::StreamRequest {
                profile,
                peer,
                body,
                reply,
            } => {
                let owner = Arc::clone(self);
                self.runtime.spawn(async move {
                    let mut reply = reply;
                    let answer = async {
                        match owner.active().await {
                            Ok(session) if session.profile == profile => session
                                .stop
                                .run_until_cancelled(
                                    owner.answer_byte_stream(&session, &peer, &body),
                                )
                                .await
                                .unwrap_or_else(|| Err("Transfer cancelled".into())),
                            _ => Err("Connect profile is no longer active".into()),
                        }
                    };
                    let result = tokio::select! {
                        _ = reply.closed() => return,
                        result = answer => result,
                    };
                    let _ = reply.send(result);
                });
            }
            NetworkEvent::Syncing {
                profile,
                peer,
                active,
            } => {
                let Some(session) = self.session.read().await.clone() else {
                    return Ok(());
                };
                if session.profile != profile {
                    return Ok(());
                }
                {
                    let mut receiving = session.receiving.lock().unwrap_or_else(|p| p.into_inner());
                    if active {
                        session
                            .received_changes
                            .store(true, std::sync::atomic::Ordering::Release);
                        receiving.insert(peer.clone());
                    } else {
                        receiving.remove(&peer);
                    }
                }
                self.status.send_if_modified(|status| {
                    if !active {
                        return false;
                    }
                    let profile_status = if status.connecting {
                        localization::tr("Receiving profile")
                    } else {
                        localization::tr("Receiving collection")
                    };
                    let changed =
                        !status.receiving_collection || profile_status != status.profile_status;
                    status.receiving_collection = true;
                    status.profile_status = profile_status;
                    changed
                });
                if !active {
                    let owner = Arc::clone(self);
                    self.runtime.spawn(async move {
                        session
                            .stop
                            .run_until_cancelled(async {
                                if let Err(error) = owner.synchronize().await {
                                    owner.failed(error);
                                }
                                if let Err(error) = owner.refresh_devices().await {
                                    owner.failed(error);
                                }
                            })
                            .await;
                    });
                }
            }
            NetworkEvent::Reachable { profile, peer } => {
                if self.status().settings.profile.as_deref() == Some(&profile) {
                    self.status.send_if_modified(|status| {
                        let Some(device) =
                            status.devices.iter_mut().find(|device| device.id == peer)
                        else {
                            return false;
                        };
                        if device.reachable {
                            return false;
                        }
                        device.reachable = true;
                        device.connection.clear();
                        true
                    });
                }
            }
            NetworkEvent::Unavailable { profile, peer } => {
                if self.status().settings.profile.as_deref() == Some(&profile) {
                    self.status.send_if_modified(|status| {
                        let Some(device) =
                            status.devices.iter_mut().find(|device| device.id == peer)
                        else {
                            return false;
                        };
                        if !device.reachable {
                            return false;
                        }
                        device.reachable = false;
                        device.has_playback = false;
                        device.connection = localization::tr("Waiting for device");
                        true
                    });
                }
            }
            NetworkEvent::Error(message) => return Err(message),
            NetworkEvent::PairingFailed {
                session,
                error: message,
            } => {
                let mut operation = self
                    .import_operation
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                let matches = self.status().pairing.as_ref().is_some_and(|pairing| {
                    session
                        .as_ref()
                        .is_none_or(|session| &pairing.session == session)
                }) || (session.is_none()
                    && operation
                        .as_ref()
                        .is_some_and(|operation| operation.session.is_none()));
                if matches {
                    operation.take();
                    self.status.send_modify(|status| {
                        status.pairing = None;
                        status.connecting = false;
                    });
                    return Err(message);
                }
            }
        }
        Ok(())
    }

    async fn adopt_paired(
        self: &Arc<Self>,
        network: &Arc<ConnectNetwork>,
        session: &str,
        profile: String,
        peer: String,
        name: String,
        bytes: Vec<u8>,
    ) -> Result<(), String> {
        let _action = self.actions.lock().await;
        let cancel = {
            let operation = self
                .import_operation
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let Some(operation) = operation
                .as_ref()
                .filter(|operation| operation.session.as_deref() == Some(session))
            else {
                return Ok(());
            };
            operation.cancel.clone()
        };
        if !self
            .network
            .lock()
            .await
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, network))
        {
            return Ok(());
        }
        let enrollment: Enrollment = serde_json::from_slice(&bytes).map_err(error)?;
        let _: age::x25519::Identity = enrollment.file_key.parse().map_err(error)?;
        let sync = self.sync.lock().await;
        self.install_file_key(&profile, enrollment.file_key.clone())
            .await?;
        self.save(|config| {
            if !config.file_profiles.contains(&profile) {
                config.file_profiles.push(profile.clone());
            }
            config.pending_import = Some(PendingImport::Peer {
                profile: profile.clone(),
                peer: Some(peer.clone()),
            });
        })?;
        if let Some(current) = self
            .session
            .read()
            .await
            .as_ref()
            .filter(|current| current.profile == profile)
        {
            current
                .documents
                .write_records(&[ConnectRecord {
                    kind: "connect_key".into(),
                    key: "file".into(),
                    value: Some(serde_json::json!(enrollment.file_key)),
                }])
                .await
                .map_err(error)?;
            network.set_enrollment_data(bytes).await;
        }
        drop(sync);
        self.status.send_modify(|status| status.pairing = None);
        if cancel.is_cancelled() {
            return Ok(());
        }
        self.resume_import(cancel).await?;
        if self.status().settings.pending_import.is_some() {
            return Ok(());
        }
        self.publish_device().await?;
        self.synchronize().await?;
        self.status.send_modify(|status| {
            status.error = None;
            status.devices.retain(|device| device.id != peer);
            status.devices.push(Device {
                id: peer,
                name,
                reachable: true,
                has_playback: false,
                download_profiles: false,
                enrolled: true,
                connection: String::new(),
            });
            status.completed_pairings += 1;
            status.profile_status = localization::tr("Profile up to date");
        });
        Ok(())
    }

    async fn resume_import(
        self: &Arc<Self>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), String> {
        self.status.send_modify(|status| {
            status.connecting = true;
            status.error = None;
        });
        let result = async {
            match self.status().settings.pending_import {
                Some(PendingImport::Prepared { .. }) => {
                    let _sync = self.sync.lock().await;
                    Box::pin(self.finish_adoption()).await
                }
                Some(PendingImport::Peer { profile, peer }) => {
                    let network = self.network().await?;
                    if ProfileStore::database_profile(self.database.path())
                        .await
                        .map_err(error)?
                        .as_deref()
                        == Some(&profile)
                    {
                        let _sync = self.sync.lock().await;
                        if self.session.read().await.is_none() {
                            self.open(profile.clone(), false).await?;
                        }
                        network
                            .attach_documents(self.active().await?.documents.clone())
                            .await
                            .map_err(error)?;
                        self.save(|config| config.pending_import = None)?;
                        return Ok(());
                    }
                    let members = network.enrolled_members(&profile).await.map_err(error)?;
                    let peer = peer
                        .filter(|peer| members.contains(peer))
                        .or_else(|| members.into_iter().find(|peer| peer != &network.identity()))
                        .ok_or("Waiting for device")?;
                    self.status.send_modify(|status| {
                        status.receiving_collection = true;
                        status.profile_status = localization::tr("Receiving profile");
                    });
                    let received = self
                        .fetch_snapshot(&network, &profile, &peer, cancel.clone())
                        .await?
                        .ok_or("The peer did not provide its profile")?;
                    Box::pin(self.read_profile_file(
                        received.to_path_buf(),
                        true,
                        None,
                        Some(&profile),
                        true,
                    ))
                    .await
                    .map_err(error)
                }
                None => Ok(()),
            }
        }
        .await;
        self.import_operation
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        self.status.send_modify(|status| {
            status.connecting = false;
            status.receiving_collection = false;
            status.collection_progress = None;
        });
        if cancel.is_cancelled() {
            Ok(())
        } else {
            result
        }
    }

    async fn fetch_snapshot(
        &self,
        network: &ConnectNetwork,
        profile: &str,
        peer: &str,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<Option<tempfile::TempPath>, String> {
        use rufin_connect::media::{FileMetadata, receive_file};
        let started = Instant::now();
        self.collection_progress(CollectionStage::Preparing, 0, None);
        let response = tokio::select! {
            _ = cancel.cancelled() => return Err("Transfer cancelled".into()),
            response = network.stream_request_for_profile(profile, peer, serde_json::to_vec(&media::ByteRequest::ProfileSnapshot { format: 4 }).map_err(error)?) => response.map_err(error)?,
        };
        if response.metadata.is_null() {
            return Ok(None);
        }
        if response.metadata["format"].as_u64() != Some(4) {
            return Err(library::profile::INCOMPATIBLE_PROFILE.into());
        }
        let metadata: FileMetadata = serde_json::from_value(response.metadata).map_err(error)?;
        self.collection_progress(CollectionStage::Receiving, 0, Some(metadata.length));
        let staged = tempfile::Builder::new()
            .prefix("profile-transfer-")
            .tempfile_in(&self.directory)
            .map_err(error)?
            .into_temp_path();
        let (sent, mut received) = tokio::sync::watch::channel(0);
        {
            let fetched = receive_file(response.reader, &metadata, &staged, cancel, sent);
            tokio::pin!(fetched);
            loop {
                tokio::select! {
                    result = &mut fetched => { result.map_err(error)?; break; },
                    Ok(()) = received.changed() => {
                        self.collection_progress(CollectionStage::Receiving, *received.borrow_and_update(), Some(metadata.length));
                    }
                }
            }
        }
        tracing::info!(
            bytes = metadata.length,
            elapsed_ms = started.elapsed().as_millis(),
            "Received Connect profile"
        );
        self.collection_progress(
            CollectionStage::Receiving,
            metadata.length,
            Some(metadata.length),
        );
        Ok(Some(staged))
    }

    async fn synchronize(&self) -> Result<(), String> {
        let _sync = self.sync.lock().await;
        let Some(session) = self.session.read().await.clone() else {
            return Ok(());
        };
        let revision = session.documents.revision().await.map_err(error)?;
        let previous = session.revision.load(std::sync::atomic::Ordering::Acquire);
        if previous != revision {
            self.apply_profile_controls(&session, previous).await?;
            session
                .revision
                .store(revision, std::sync::atomic::Ordering::Release);
            if let Some(network) = self.network.lock().await.as_ref() {
                network.refresh();
            }
        }
        let receiving = !session
            .receiving
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_empty();
        if !receiving
            && session
                .received_changes
                .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            self.source.connect_changed(false).await?;
            self.source.connect_media_changed();
        }
        if !receiving && !self.status().connecting {
            self.status.send_if_modified(|status| {
                if !status.receiving_collection {
                    return false;
                }
                status.receiving_collection = false;
                status.collection_progress = None;
                if status.error.is_none() {
                    status.profile_status = localization::tr("Profile up to date");
                }
                true
            });
        }
        Ok(())
    }

    async fn apply_profile_controls(&self, session: &Session, after: i64) -> Result<(), String> {
        let records = session
            .documents
            .control_records(after)
            .await
            .map_err(error)?;
        for record in &records {
            if record.kind == "device"
                && let Some(name) = record.value.as_ref().and_then(serde_json::Value::as_str)
            {
                self.status.send_if_modified(|status| {
                    let Some(device) = status
                        .devices
                        .iter_mut()
                        .find(|device| device.id == record.key)
                    else {
                        return false;
                    };
                    if device.name == name {
                        return false;
                    }
                    device.name = name.to_owned();
                    true
                });
            }
            if record.kind == "connect_network" && record.key == "relay" {
                let relay: Option<String> =
                    serde_json::from_value(record.value.clone().unwrap_or_default())
                        .map_err(error)?;
                self.save(|config| config.relay = relay)?;
            }
            if record.kind == "connect_network" && record.key == "public_relay" {
                let public = record
                    .value
                    .as_ref()
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                self.save(|config| config.public_relay = public)?;
            }
            if record.kind == "connect_key"
                && let Some(key) = record.value.as_ref().and_then(serde_json::Value::as_str)
            {
                self.install_file_key(&session.profile, key.to_owned())
                    .await?;
                if let Some(network) = self.network.lock().await.as_ref() {
                    network
                        .set_enrollment_data(
                            serde_json::to_vec(&Enrollment {
                                file_key: key.to_owned(),
                            })
                            .map_err(error)?,
                        )
                        .await;
                }
            }
        }
        Ok(())
    }

    async fn publish_device(&self) -> Result<(), String> {
        let Some(session) = self.session.read().await.clone() else {
            return Ok(());
        };
        session
            .documents
            .write_records(&[ConnectRecord {
                kind: "device".into(),
                key: session.identity.clone(),
                value: Some(serde_json::json!(self.status().settings.name)),
            }])
            .await
            .map_err(error)?;
        Ok(())
    }

    async fn refresh_devices(self: &Arc<Self>) -> Result<(), String> {
        use futures_util::StreamExt;
        let sync = self.sync.lock().await;
        let Some(session) = self.session.read().await.clone() else {
            return Ok(());
        };
        let Some(network) = self.network.lock().await.clone() else {
            return Ok(());
        };
        let mut devices = Vec::new();
        for peer in network.members().await.map_err(error)? {
            if peer == session.identity {
                continue;
            }
            let mut device = self
                .status()
                .devices
                .into_iter()
                .find(|device| device.id == peer)
                .unwrap_or(Device {
                    id: peer.clone(),
                    name: "Rufin".into(),
                    reachable: false,
                    has_playback: false,
                    download_profiles: false,
                    enrolled: true,
                    connection: String::new(),
                });
            if let Some(name) = session.documents.device_name(&peer).await.map_err(error)? {
                device.name = name;
            }
            device.enrolled = true;
            devices.push(device);
        }
        let presence = serde_json::to_vec(&Request::Presence).map_err(error)?;
        let mut probes = devices
            .iter()
            .map(|device| {
                let peer = device.id.clone();
                let network = Arc::clone(&network);
                let presence = presence.clone();
                async move {
                    let answer = network.test_connection(&peer, presence).await;
                    (peer, answer)
                }
            })
            .collect::<futures_util::stream::FuturesUnordered<_>>();
        self.status.send_if_modified(|status| {
            if !status.settings.enabled || session.stop.is_cancelled() {
                return false;
            }
            let mut current = status
                .devices
                .iter()
                .filter(|existing| {
                    !existing.enrolled && !devices.iter().any(|device| device.id == existing.id)
                })
                .cloned()
                .collect::<Vec<_>>();
            current.extend(devices);
            if current == status.devices {
                return false;
            }
            status.devices = current;
            true
        });
        drop(sync);
        while let Some((peer, answer)) = probes.next().await {
            let _sync = self.sync.lock().await;
            if session.stop.is_cancelled()
                || !self
                    .session
                    .read()
                    .await
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &session))
                || !self
                    .network
                    .lock()
                    .await
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &network))
            {
                return Ok(());
            }
            if !network.members().await.map_err(error)?.contains(&peer) {
                continue;
            }
            let answer = answer.map_err(error).and_then(|(route, bytes)| {
                serde_json::from_slice::<serde_json::Value>(&bytes)
                    .map(|answer| (route, answer))
                    .map_err(error)
            });
            self.status.send_if_modified(|status| {
                let Some(device) = status
                    .devices
                    .iter_mut()
                    .find(|device| device.id == peer && device.enrolled)
                else {
                    return false;
                };
                let previous = device.clone();
                if let Ok((route, answer)) = answer {
                    device.name = answer["name"].as_str().unwrap_or(&device.name).to_owned();
                    device.has_playback = answer["playback"].as_bool().unwrap_or(false);
                    device.download_profiles =
                        answer["download_profiles"].as_bool().unwrap_or(false);
                    device.reachable = true;
                    device.connection = connection_label(&route);
                } else {
                    device.reachable = false;
                    device.has_playback = false;
                    device.connection = localization::tr("Waiting for device");
                }
                *device != previous
            });
        }
        Ok(())
    }

    async fn request(&self, peer: &str, request: Request) -> Result<serde_json::Value, String> {
        let answer = self
            .connected_network()
            .await?
            .request(peer, serde_json::to_vec(&request).map_err(error)?)
            .await
            .map_err(error)?;
        serde_json::from_slice(&answer).map_err(error)
    }

    async fn answer(
        &self,
        session: &Arc<Session>,
        profile: &str,
        peer: &str,
        bytes: &[u8],
    ) -> Result<Vec<u8>, String> {
        let network = self.connected_network().await?;
        if session.profile != profile
            || !network
                .members()
                .await
                .map_err(error)?
                .iter()
                .any(|member| member == peer)
        {
            return Err("This device is not a member of the active profile".into());
        }
        let request: Request = serde_json::from_slice(bytes).map_err(error)?;
        let answer = match request {
            Request::Settings {
                revision,
                known_revision,
                records,
            } => {
                if let Some(records) = records {
                    self.merge_settings(records).await?;
                    session
                        .settings_peers
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .entry(peer.into())
                        .or_default()
                        .received = Some(revision);
                }
                let revision = self.settings_apply.shared_revision();
                let records = if known_revision != Some(revision) {
                    let settings = self.settings_apply.clone();
                    Some(
                        tokio::task::spawn_blocking(move || settings.shared_records())
                            .await
                            .map_err(error)??,
                    )
                } else {
                    None
                };
                let acknowledged = {
                    let mut peers = session
                        .settings_peers
                        .lock()
                        .unwrap_or_else(|p| p.into_inner());
                    let state = peers.entry(peer.into()).or_default();
                    state.sent = known_revision;
                    state.received
                };
                serde_json::to_value(SettingsReply {
                    revision,
                    acknowledged,
                    records,
                })
                .map_err(error)?
            }
            Request::CacheFile { asset } => self.answer_cache(asset).await?,
            Request::CastFrom { transfer } => {
                if transfer {
                    self.continue_from_with_state(peer, None).await?;
                } else {
                    let playback = Arc::clone(&self.playback);
                    tokio::task::spawn_blocking(move || {
                        playback.select_playback_output(
                            playback::PlaybackOutput::Local,
                            Arc::new(std::sync::atomic::AtomicBool::new(false)),
                        )
                    })
                    .await
                    .map_err(error)??;
                }
                serde_json::Value::Null
            }
            Request::CastState => {
                serde_json::to_value(self.playback.receiver_state().await?).map_err(error)?
            }
            Request::CastQueuePage {
                offset,
                limit,
                filter,
            } => {
                let page = self
                    .playback
                    .receiver_queue_page(offset, limit, filter)
                    .await?;
                serde_json::to_value(casting::ReceiverQueuePage {
                    rows: page
                        .rows
                        .iter()
                        .map(|row| library::QueueTransferRow::from(row.as_ref()))
                        .collect(),
                    total: page.total,
                    window_offset: page.window_offset,
                })
                .map_err(error)?
            }
            Request::CastQueue => {
                let snapshot = self.playback.cast_queue_snapshot().await?;
                let header = snapshot.as_ref().map(|snapshot| snapshot.header.clone());
                let stopped = self
                    .playback
                    .current_projection()
                    .is_some_and(|projection| {
                        projection.view.transport.state == playback::TransportStatus::Stopped
                    });
                if let Some(snapshot) = snapshot {
                    self.transfers
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .insert(peer.into(), (Instant::now(), snapshot));
                }
                let mut value = serde_json::to_value(header).map_err(error)?;
                if !value.is_null() {
                    value["stopped"] = serde_json::json!(stopped);
                }
                value
            }
            Request::CastInput {
                header,
                placement,
                target,
            } => {
                self.receive_cast_input(peer, header, placement, target)
                    .await?;
                serde_json::Value::Null
            }
            Request::Presence => {
                let active = self.playback.has_continuation();
                serde_json::json!({"name":self.status().settings.name,"playback":active,"download_profiles":true})
            }
            Request::Continuation => {
                let playback = Arc::clone(&self.playback);
                let snapshot =
                    tokio::task::spawn_blocking(move || playback.continuation_snapshot())
                        .await
                        .map_err(error)??
                        .ok_or("There is no playback to continue")?;
                let value = serde_json::to_value(&snapshot.header).map_err(error)?;
                let mut transfers = self.transfers.lock().unwrap_or_else(|p| p.into_inner());
                transfers.retain(|_, (started, _)| started.elapsed() < Duration::from_secs(300));
                transfers.insert(peer.to_owned(), (Instant::now(), snapshot));
                value
            }
            Request::Queue { offset } => {
                let snapshot = self
                    .transfers
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .get(peer)
                    .map(|(_, snapshot)| snapshot.clone())
                    .ok_or("Continuation expired")?;
                serde_json::to_value(self.playback.continuation_page(&snapshot, offset).await?)
                    .map_err(error)?
            }
            Request::TrackReference { uri } => serde_json::to_value(
                self.database
                    .connect_track_reference(&uri)
                    .await
                    .map_err(error)?,
            )
            .map_err(error)?,
            Request::MediaAvailability { uris } => {
                if uris.len() > 256 {
                    return Err("Too many media availability requests".into());
                }
                let paths = self
                    .database
                    .local_media_locations(&uris)
                    .await
                    .map_err(error)?;
                let paths = paths.into_iter().map(|(_, paths, _)| paths).collect();
                serde_json::to_value(sources::local_files_available(paths).await.map_err(error)?)
                    .map_err(error)?
            }
            Request::StopContinuation => {
                let snapshot = self
                    .transfers
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(peer)
                    .map(|(_, snapshot)| snapshot)
                    .ok_or("Continuation expired")?;
                let header = if snapshot.header.listen.is_none()
                    && !self.playback.has_continuation()
                {
                    let fresh = self
                        .playback
                        .cast_queue_snapshot()
                        .await?
                        .ok_or("Playback changed. Request continuation again")?;
                    if fresh.queue.entries != snapshot.queue.entries
                        || fresh.queue.order != snapshot.queue.order
                        || fresh.header.current != snapshot.header.current
                    {
                        return Err("Playback changed. Request continuation again".into());
                    }
                    fresh.header
                } else {
                    let playback = Arc::clone(&self.playback);
                    tokio::task::spawn_blocking(move || playback.stop_continuation(snapshot.header))
                        .await
                        .map_err(error)??
                        .ok_or("Playback changed. Request continuation again")?
                };
                serde_json::to_value(header).map_err(error)?
            }
            Request::Control { command } => {
                let playback = Arc::clone(&self.playback);
                tokio::task::spawn_blocking(move || match command {
                    Control::PlayPause => playback.play_pause(),
                    Control::Play => playback.play(),
                    Control::Pause => playback.pause(),
                    Control::Stop => playback.stop(),
                    Control::Next => playback.next(),
                    Control::Previous => playback.previous(),
                    Control::Seek { millis } => playback.seek_millis(millis),
                    Control::Volume { value } => playback.set_volume(value),
                    Control::PersistVolume { value } => playback.persist_volume(value),
                    Control::Mute { muted } => playback.set_muted(muted),
                    Control::Repeat { mode } => playback.set_repeat(mode),
                    Control::Shuffle { enabled } => playback.set_shuffle(enabled),
                    Control::AutoDj { enabled } => playback.send_cast_auto_dj(enabled),
                    Control::Activate { position } => playback.activate(position),
                    Control::Remove { positions } => {
                        playback.remove_many(crate::playback::QueueSelection::Positions(positions))
                    }
                    Control::Reorder { positions, target } => playback.reorder(
                        crate::playback::QueueSelection::Positions(positions),
                        target,
                    ),
                    Control::MoveNext { position } => playback.move_after_current(position),
                    Control::Clear { include_current } => playback.clear(include_current),
                })
                .await
                .map_err(error)?;
                serde_json::Value::Null
            }
        };
        serde_json::to_vec(&answer).map_err(error)
    }

    async fn continue_from(&self, peer: &str) -> Result<(), String> {
        self.continue_from_with_state(peer, Some(true)).await
    }

    pub(crate) async fn continue_from_with_state(
        &self,
        peer: &str,
        playing: Option<bool>,
    ) -> Result<(), String> {
        let answer = self
            .request(
                peer,
                if playing.is_some() {
                    Request::Continuation
                } else {
                    Request::CastQueue
                },
            )
            .await?;
        if answer.is_null() {
            return self.playback.return_empty_local().await;
        }
        let stopped = playing.is_none() && answer["stopped"].as_bool().unwrap_or(false);
        let header: playback::ContinuationHeader = serde_json::from_value(answer).map_err(error)?;
        let transfer = format!("connect-{}", blake3::hash(peer.as_bytes()).to_hex());
        let result = async {
            let mut offset = 0;
            let mut current = None;
            while offset < header.total {
                let page: library::QueueTransferPage =
                    serde_json::from_value(self.request(peer, Request::Queue { offset }).await?)
                        .map_err(error)?;
                if page.rows.is_empty() {
                    return Err("The source queue changed during continuation".into());
                }
                if let Some(row) = page.rows.iter().find(|row| row.position == header.current) {
                    current = Some(row.item.clone());
                }
                offset += page.rows.len();
                self.playback
                    .stage_continuation_page(&transfer, &page)
                    .await?;
            }
            let current = current.ok_or("The source queue has no current track")?;
            if media::raw_document_reference(&current.media_uri) {
                return Err(
                    "The source device did not provide a portable document identity".into(),
                );
            } else if self.is_local_media(&current.media_uri).await?
                && self
                    .database
                    .connect_track_reference(&current.media_uri)
                    .await
                    .map_err(error)?
                    .is_none()
            {
                let reference = self
                    .request(
                        peer,
                        Request::TrackReference {
                            uri: current.media_uri.clone(),
                        },
                    )
                    .await?;
                if !reference.is_null() {
                    self.database
                        .connect_apply(&[ConnectRecord {
                            kind: "track".into(),
                            key: current.media_uri.clone(),
                            value: Some(reference),
                        }])
                        .await
                        .map_err(error)?;
                }
            }
            let prepared = self
                .playback
                .prepare_continuation(transfer.clone(), header)
                .await?;
            let mut fresh: playback::ContinuationHeader =
                serde_json::from_value(self.request(peer, Request::StopContinuation).await?)
                    .map_err(error)?;
            if let Some(playing) = playing {
                fresh.desired_playing = playing;
            }
            self.playback.apply_continuation(prepared, fresh).await?;
            if stopped {
                self.playback.stop();
            }
            Ok(())
        }
        .await;
        if result.is_err() {
            let _ = self.playback.discard_continuation(&transfer).await;
        }
        result
    }
}

impl ConnectOwner {
    async fn exchange_settings(&self, session: &Session, peer: &str) -> Result<(), String> {
        for _ in 0..2 {
            let revision = self.settings_apply.shared_revision();
            let previous = session
                .settings_peers
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(peer)
                .copied()
                .unwrap_or_default();
            let records = if previous.sent != Some(revision) {
                let settings = self.settings_apply.clone();
                Some(
                    tokio::task::spawn_blocking(move || settings.shared_records())
                        .await
                        .map_err(error)??,
                )
            } else {
                None
            };
            let reply: SettingsReply = serde_json::from_value(
                self.request(
                    peer,
                    Request::Settings {
                        revision,
                        known_revision: previous.received,
                        records,
                    },
                )
                .await?,
            )
            .map_err(error)?;
            if let Some(records) = reply.records {
                self.merge_settings(records).await?;
            }
            let mut peers = session
                .settings_peers
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let state = peers.entry(peer.into()).or_default();
            state.sent = reply.acknowledged;
            state.received = Some(reply.revision);
            if reply.acknowledged == Some(revision) {
                return Ok(());
            }
        }
        Err("Connect settings were not accepted".into())
    }

    async fn merge_settings(&self, records: Vec<library::StampedRecord>) -> Result<(), String> {
        let settings = self.settings_apply.clone();
        let sources_changed =
            tokio::task::spawn_blocking(move || settings.merge_shared(&records, None))
                .await
                .map_err(error)??;
        if sources_changed {
            self.source.connect_changed(true).await?;
        }
        Ok(())
    }

    async fn prepare_snapshot(&self, session: &Arc<Session>) -> Result<(), String> {
        loop {
            if !self
                .session
                .read()
                .await
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, session))
            {
                break;
            }
            if !self.database.connect_seed_page().await.map_err(error)? {
                break;
            }
            tokio::task::yield_now().await;
        }
        self.synchronize().await
    }

    async fn export(&self) -> Result<tempfile::NamedTempFile, String> {
        self.export_profile(&self.active().await?).await
    }

    async fn export_profile(
        &self,
        session: &Arc<Session>,
    ) -> Result<tempfile::NamedTempFile, String> {
        crate::playlist_files::backfill_file_playlists(&self.source).await?;
        self.prepare_snapshot(session).await?;
        let key: age::x25519::Identity = self
            .secret(file_key(&self.identity_reference()?, &session.profile))
            .await?
            .ok_or("The Connect file key is unavailable")?
            .parse()
            .map_err(error)?;
        let output = tempfile::Builder::new()
            .prefix("profile-transfer-")
            .tempfile_in(&self.directory)
            .map_err(error)?;
        let settings = self.settings_apply.clone();
        let (records, personal) = tokio::task::spawn_blocking(move || {
            Ok::<_, String>((settings.shared_records()?, settings.initial_preferences()?))
        })
        .await
        .map_err(error)??;
        session
            .documents
            .export_database(output.path(), &key, &records, &personal)
            .await
            .map_err(error)?;
        Ok(output)
    }

    async fn import_file(
        self: &Arc<Self>,
        path: PathBuf,
        replace: bool,
        key: Option<String>,
    ) -> Result<(), ActionError> {
        self.read_profile_file(path, replace, key, None, false)
            .await
    }

    async fn read_profile_file(
        self: &Arc<Self>,
        path: PathBuf,
        replace: bool,
        key: Option<String>,
        expected_profile: Option<&str>,
        move_input: bool,
    ) -> Result<(), ActionError> {
        let cancel = if expected_profile.is_none() {
            self.begin_import(None)
        } else {
            self.import_operation
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_ref()
                .map(|operation| operation.cancel.clone())
                .unwrap_or_default()
        };
        self.status.send_modify(|status| status.connecting = true);
        self.collection_progress(CollectionStage::Validating, 0, None);
        let explicit_key = key.clone();
        let mut keys = match key {
            Some(key) => vec![key],
            None => {
                let mut profiles = self.status().settings.file_profiles;
                if let Some(current) = self.session.read().await.as_ref()
                    && !profiles.contains(&current.profile)
                {
                    profiles.push(current.profile.clone());
                }
                let mut keys = Vec::new();
                for profile in profiles {
                    if let Some(key) = self
                        .secret(file_key(&self.identity_reference()?, &profile))
                        .await?
                    {
                        keys.push(key);
                    }
                    keys.extend(
                        self.secret(previous_file_keys(&self.identity_reference()?, &profile))
                            .await?
                            .map(|value| serde_json::from_str::<Vec<String>>(&value))
                            .transpose()
                            .map_err(error)?
                            .unwrap_or_default(),
                    );
                }
                keys
            }
        };
        if keys.is_empty() {
            return Err("Pair with a trusted device to obtain access to this Connect file".into());
        }
        let owner = Arc::clone(self);
        let cancelled = cancel.clone();
        let (profile, staged, legacy) = self
            .read_profile_input(
                path,
                &keys,
                expected_profile,
                move_input,
                move |completed, total| {
                    if !cancelled.is_cancelled() {
                        owner.collection_progress(
                            CollectionStage::Validating,
                            completed,
                            Some(total),
                        );
                    }
                },
            )
            .await?;
        if cancel.is_cancelled() {
            return Err("Transfer cancelled".into());
        }
        let current = self.session.read().await.clone();
        let working_profile = ProfileStore::database_profile(self.database.path())
            .await
            .map_err(error)?;
        let adoption = working_profile.as_deref() != Some(&profile);
        if adoption && !replace {
            return Err(Confirmation::ReplaceProfile.required());
        }
        let local_key = if adoption {
            Some(self.local_key(&profile, true).await?)
        } else {
            None
        };
        let previous_keys = self
            .secret(previous_file_keys(&self.identity_reference()?, &profile))
            .await?
            .map(|value| serde_json::from_str::<Vec<String>>(&value))
            .transpose()
            .map_err(error)?
            .unwrap_or_default();
        let selected_key = explicit_key
            .or(self
                .secret(file_key(&self.identity_reference()?, &profile))
                .await?)
            .or_else(|| previous_keys.first().cloned());
        if let Some(selected) = &selected_key {
            keys.retain(|key| key != selected);
            keys.insert(0, selected.clone());
        }
        let input_key = self
            .prepare_profile_input(
                &profile,
                &staged,
                legacy,
                &keys,
                local_key.as_ref(),
                |count| {
                    self.collection_progress(CollectionStage::Importing, count as u64, None);
                },
            )
            .await?;
        self.collection_progress(CollectionStage::SavingChanges, 0, None);
        if adoption {
            ProfileStore::validate_adoption_database(staged.path())
                .await
                .map_err(error)?;
            let key = selected_key.ok_or("The Connect file key is unavailable")?;
            let prepared_path = self.directory.join(format!(
                "{}.pending.sqlite",
                blake3::hash(profile.as_bytes()).to_hex()
            ));
            let destination = prepared_path.clone();
            tokio::task::spawn_blocking(move || {
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(staged.path())
                    .map_err(error)?
                    .sync_all()
                    .map_err(error)?;
                staged.into_temp_path().persist(destination).map_err(error)
            })
            .await
            .map_err(error)??;
            let committed = async {
                self.install_file_key(&profile, key).await?;
                self.save(|config| {
                    config.enabled = true;
                    if !config.file_profiles.contains(&profile) {
                        config.file_profiles.push(profile.clone());
                    }
                    config.pending_import = Some(PendingImport::Prepared {
                        profile: profile.clone(),
                        path: prepared_path.clone(),
                    });
                })
            }
            .await;
            if let Err(cause) = committed {
                let _ = tokio::fs::remove_file(&prepared_path).await;
                return Err(cause.into());
            }
            if cancel.is_cancelled() {
                return Ok(());
            }
            let _sync = self.sync.lock().await;
            Box::pin(self.finish_adoption()).await?;
        } else {
            if cancel.is_cancelled() {
                return Ok(());
            }
            let _sync = self.sync.lock().await;
            let session = match current.filter(|session| session.profile == profile) {
                Some(session) => session,
                None => {
                    self.save(|config| config.enabled = true)?;
                    self.open(profile.clone(), false).await?;
                    self.active().await?
                }
            };
            let (settings, _) = ProfileStore::read_export_settings(staged.path(), &input_key)
                .await
                .map_err(error)?;
            self.merge_settings(settings).await?;
            let changed = session
                .documents
                .merge_database(staged.path(), &input_key)
                .await
                .map_err(error)?;
            if changed {
                self.source.connect_changed(false).await?;
                self.source.connect_media_changed();
            }
        }
        self.synchronize().await?;
        self.status.send_modify(|status| {
            status.connecting = false;
            status.collection_progress = None;
        });
        Ok(())
    }

    async fn finish_adoption(self: &Arc<Self>) -> Result<(), String> {
        let Some(PendingImport::Prepared { profile, path }) = self.status().settings.pending_import
        else {
            return Ok(());
        };
        let local_key = self.local_key(&profile, false).await?;
        let previous_profile = ProfileStore::database_profile(self.database.path())
            .await
            .map_err(error)?;
        let replacing = previous_profile.as_deref() != Some(&profile);
        let settings_path = if tokio::fs::try_exists(&path).await.map_err(error)? {
            path.as_path()
        } else if !replacing {
            self.database.path()
        } else {
            return Err(
                "The prepared profile is unavailable. Pair with a trusted device again".into(),
            );
        };
        let (shared_settings, personal_settings) =
            ProfileStore::read_export_settings(settings_path, &local_key)
                .await
                .map_err(error)?;
        self.source
            .restore_user_state(true, false, || async {
                let playback = Arc::clone(&self.playback);
                tokio::task::spawn_blocking(move || playback.shutdown())
                    .await
                    .map_err(error)?;
                let result = async {
                    if replacing {
                        if let Some(previous) = self.session.write().await.take() {
                            previous.stop.cancel();
                            previous.documents.close().await.map_err(error)?;
                        }
                        if let Some(network) = self.network.lock().await.as_ref() {
                            network.close_profile().await.map_err(error)?;
                        }
                        self.collection_progress(CollectionStage::Applying, 0, None);
                        ProfileStore::install_prepared_database(
                            self.database.as_ref().clone(),
                            &path,
                        )
                        .await
                        .map_err(error)?;
                    }
                    self.save(|config| {
                        config.profile = Some(profile.clone());
                    })?;
                    let settings = self.settings_apply.clone();
                    tokio::task::spawn_blocking(move || {
                        settings.merge_shared(&shared_settings, Some(&personal_settings))
                    })
                    .await
                    .map_err(error)??;
                    if self.session.read().await.is_none() {
                        self.open(profile.clone(), false).await?;
                    }
                    Ok::<_, String>(())
                }
                .await;
                self.playback.start().await?;
                result
            })
            .await?;
        self.save(|config| config.pending_import = None)?;
        self.status
            .send_modify(|status| status.devices.retain(|device| !device.enrolled));
        self.source.connect_changed(false).await?;
        self.source.connect_media_changed();
        if let Some(previous) = previous_profile.filter(|previous| previous != &profile)
            && let Some(network) = self.network.lock().await.clone()
        {
            self.runtime.spawn(async move {
                if let Err(error) = network.leave_profile(&previous).await {
                    tracing::warn!(%error, "Could not announce Connect departure");
                }
            });
        }
        self.cleanup_profiles(Some(&profile)).await?;
        Ok(())
    }

    async fn read_profile_input(
        &self,
        path: PathBuf,
        keys: &[String],
        expected_profile: Option<&str>,
        move_input: bool,
        progress: impl FnMut(u64, u64) + Send + 'static,
    ) -> Result<(String, tempfile::NamedTempFile, Option<portable::Snapshot>), String> {
        use std::io::Read;
        let input = path.clone();
        let sqlite = tokio::task::spawn_blocking(move || {
            let mut header = [0; 16];
            let mut input = std::fs::File::open(input).map_err(error)?;
            Ok::<_, String>(
                input.read(&mut header).map_err(error)? == header.len()
                    && &header == b"SQLite format 3\0",
            )
        })
        .await
        .map_err(error)??;
        let (profile, legacy) = if sqlite {
            (
                ProfileStore::database_profile(&path)
                    .await
                    .map_err(error)?
                    .ok_or(library::profile::INCOMPATIBLE_PROFILE)?,
                None,
            )
        } else {
            let input = path.clone();
            let keys = keys.to_vec();
            let (profile, snapshot) =
                tokio::task::spawn_blocking(move || portable::decrypt(&input, &keys, progress))
                    .await
                    .map_err(error)??;
            (profile, Some(snapshot))
        };
        if expected_profile.is_some_and(|expected| expected != profile) {
            return Err("The received profile does not match the verified device".into());
        }
        let staged = tempfile::Builder::new()
            .prefix("profile-transfer-")
            .tempfile_in(&self.directory)
            .map_err(error)?;
        if sqlite {
            if move_input {
                tokio::fs::rename(path, staged.path())
                    .await
                    .map_err(error)?;
            } else {
                tokio::fs::copy(path, staged.path()).await.map_err(error)?;
            }
        }
        Ok((profile, staged, legacy))
    }

    async fn prepare_profile_input(
        &self,
        profile: &str,
        staged: &tempfile::NamedTempFile,
        legacy: Option<portable::Snapshot>,
        keys: &[String],
        local_key: Option<&age::x25519::Identity>,
        progress: impl Fn(usize) + Send,
    ) -> Result<age::x25519::Identity, String> {
        if let Some(snapshot) = legacy {
            let local_key = match local_key {
                Some(key) => key.clone(),
                None => self.local_key(profile, false).await?,
            };
            let identity = self.credentials().await?.public().to_string();
            let peer = u64::from_le_bytes(
                blake3::hash(identity.as_bytes()).as_bytes()[..8]
                    .try_into()
                    .unwrap(),
            );
            let database = library::Database::open_snapshot(staged.path())
                .await
                .map_err(error)?;
            let converted = async {
                let documents = ProfileStore::open(database.clone(), peer, local_key.clone())
                    .await
                    .map_err(error)?;
                let result = async {
                    documents
                        .replace_snapshot(snapshot.reader()?, progress)
                        .await
                        .map_err(error)?;
                    documents
                        .bind_profile(profile, &identity)
                        .await
                        .map_err(error)
                }
                .await;
                let closed = documents.close().await.map_err(error);
                result.and(closed)
            }
            .await;
            let closed = database.close().await.map_err(error);
            converted.and(closed)?;
            Ok(local_key)
        } else {
            let mut failure = None;
            for key in keys {
                let key = key.parse().map_err(error)?;
                let prepared_key = local_key.unwrap_or(&key);
                match ProfileStore::prepare_database(staged.path(), &key, prepared_key).await {
                    Ok(()) => return Ok(prepared_key.clone()),
                    Err(cause) => failure = Some(error(cause)),
                }
            }
            Err(failure.unwrap_or_else(|| "The Connect file key is unavailable".into()))
        }
    }
}

pub(crate) fn write_device_config(
    directory: &std::path::Path,
    config: &ConnectSettings,
) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(error)?;
    crate::settings::write_private(
        &directory.join("device.json"),
        &serde_json::to_vec(config).map_err(error)?,
    )
}

pub(crate) fn secret_keys(settings: &ConnectSettings) -> Vec<SecretKey> {
    let Some(identity) = &settings.identity_ref else {
        return Vec::new();
    };
    let mut keys = vec![identity_key(identity)];
    let profiles = settings
        .profile
        .iter()
        .chain(settings.file_profiles.iter())
        .map(String::as_str)
        .chain(settings.pending_import.as_ref().map(PendingImport::profile))
        .collect::<std::collections::BTreeSet<_>>();
    for profile in profiles {
        keys.extend([
            file_key(identity, profile),
            previous_file_keys(identity, profile),
            local_key(identity, profile),
        ]);
    }
    keys
}

fn identity_key(identity_ref: &str) -> SecretKey {
    SecretKey::namespaced("connect", identity_ref, "Rufin Connect device identity")
}

fn previous_file_keys(identity_ref: &str, profile: &str) -> SecretKey {
    SecretKey::namespaced(
        "connect-file-previous",
        format!("{identity_ref}:{profile}"),
        "Rufin Connect previous profile file keys",
    )
}

fn file_key(identity_ref: &str, profile: &str) -> SecretKey {
    SecretKey::namespaced(
        "connect-file",
        format!("{identity_ref}:{profile}"),
        "Rufin Connect profile file",
    )
}

fn local_key(identity_ref: &str, profile: &str) -> SecretKey {
    SecretKey::namespaced(
        "connect-local",
        format!("{identity_ref}:{profile}"),
        "Rufin Connect local secrets key",
    )
}
fn error(error: impl std::fmt::Display) -> String {
    let message = error.to_string();
    if message == library::profile::INCOMPATIBLE_PROFILE {
        localization::tr(
            "Devices using Rufin Connect are no longer compatible. Please update Rufin.",
        )
    } else {
        message
    }
}

fn connection_label(value: &str) -> String {
    match value {
        "Local network" => localization::tr("Local network"),
        "Via relay" => localization::tr("Via relay"),
        "Direct" => localization::tr("Direct"),
        other => other.into(),
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct ConnectSubscription {
    changes: tokio::sync::Mutex<tokio::sync::watch::Receiver<Status>>,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl ConnectOwner {
    pub fn changes(&self) -> Arc<ConnectSubscription> {
        let mut changes = self.subscribe();
        changes.mark_changed();
        Arc::new(ConnectSubscription {
            changes: tokio::sync::Mutex::new(changes),
        })
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl ConnectSubscription {
    pub async fn next(&self) -> Result<Status, ActionError> {
        let mut changes = self.changes.lock().await;
        changes
            .changed()
            .await
            .map_err(|failure| ActionError::Failed(failure.to_string()))?;
        Ok(changes.borrow_and_update().clone())
    }
}
