//! Connect composition shared by desktop, headless and HTTP clients.
mod media;
pub mod portable;
#[cfg(test)]
mod tests;

use crate::{playback::PlaybackOwner, settings::SettingsOwner, source::SourceOwner};
use library::ConnectRecord;
use playback::TransportCommandPort;
use rufin_connect::{
    ConnectNetwork, Credentials, NetworkConfig, NetworkEvent, profile::ProfileStore,
};
use secrets::{SecretKey, SecretStore, SwitchableSecretStore};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnectSettings {
    pub enabled: bool,
    pub name: String,
    pub identity_ref: Option<String>,
    pub profile: Option<String>,
    pub file_profiles: Vec<String>,
    pub nearby: bool,
    pub relay: Option<String>,
    pub public_relay: bool,
    pub destination: Option<portable::Destination>,
    pub folders: BTreeMap<String, PathBuf>,
    pub encoding: Encoding,
    pub adopting: bool,
    #[serde(alias = "joined")]
    pub established: bool,
    pub setup_pending: bool,
    /// This device's received queue digest and its original player's identity.
    pub continued_queue: Option<(String, String)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Encoding {
    #[default]
    Original,
    Mp3,
}

impl Default for ConnectSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            name: "Rufin".into(),
            identity_ref: None,
            profile: None,
            file_profiles: Vec::new(),
            nearby: true,
            relay: None,
            public_relay: false,
            destination: None,
            folders: BTreeMap::new(),
            encoding: Encoding::Original,
            adopting: false,
            established: false,
            setup_pending: false,
            continued_queue: None,
        }
    }
}

impl ConnectSettings {
    fn from_device_file(bytes: &[u8]) -> serde_json::Result<Self> {
        let value: serde_json::Value = serde_json::from_slice(bytes)?;
        let has_enabled = value.get("enabled").is_some();
        let mut config: Self = serde_json::from_value(value)?;
        if !has_enabled {
            config.enabled = config.profile.is_some();
        }
        Ok(config)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub reachable: bool,
    pub has_playback: bool,
    #[serde(default)]
    pub queue_content_id: Option<String>,
    pub enrolled: bool,
    pub connection: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Pairing {
    pub session: String,
    pub peer: String,
    pub name: String,
    pub emoji: [String; 7],
    #[serde(default)]
    pub approved: bool,
    #[serde(default)]
    pub verified: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Status {
    pub settings: ConnectSettings,
    #[serde(default)]
    pub local_folder: Option<PathBuf>,
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
    pub media_status: Option<String>,
    pub error: Option<String>,
}

impl Status {
    pub fn storage_path(&self, source: Option<&sources::SourceId>) -> String {
        match (source, self.settings.destination.as_ref()) {
            (None, Some(portable::Destination::Local { path })) => {
                path.to_string_lossy().into_owned()
            }
            (Some(selected), Some(portable::Destination::Remote { source_id, path }))
                if selected == source_id =>
            {
                path.clone()
            }
            (Some(_), _) => "Rufin Connect".into(),
            (None, _) => self
                .local_folder
                .as_deref()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    Enable {
        enabled: bool,
    },
    Create,
    Join {
        invitation: String,
        replace: bool,
    },
    Pair {
        session: String,
        approve: bool,
    },
    CancelPairing,
    FinishSetup,
    Rename {
        name: String,
    },
    Remove {
        peer: String,
    },
    Leave,
    Refresh,
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
        path: PathBuf,
    },
    Encoding {
        encoding: Encoding,
        #[serde(default)]
        confirm: bool,
    },
    Download {
        peer: String,
        uri: String,
    },
    CancelDownload,
    Destination {
        destination: Option<portable::Destination>,
    },
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

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Control {
    Play,
    Pause,
    Stop,
    Next,
    Previous,
    Seek { millis: u64 },
    Volume { value: f64 },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
enum Request {
    Presence,
    ProfileSnapshot {
        #[serde(default)]
        setup: bool,
    },
    Continuation,
    Queue {
        offset: usize,
    },
    TrackReference {
        uri: String,
    },
    StopContinuation,
    Control {
        command: Control,
    },
    Media {
        id: String,
        uri: String,
        encoding: Encoding,
        occurrence: Option<library::OccurrenceId>,
    },
    CancelMedia {
        id: String,
    },
}

#[derive(Serialize, Deserialize)]
struct Enrollment {
    file_key: String,
}

struct Session {
    settings_revision: Mutex<Option<(u64, u64)>>,
    profile: String,
    documents: Arc<ProfileStore>,
    identity: String,
    file_exchange: tokio::sync::Mutex<Option<FileExchange>>,
    stop: tokio_util::sync::CancellationToken,
    receiving: Mutex<BTreeSet<String>>,
    catalog_changed: std::sync::atomic::AtomicBool,
}

struct FileExchange {
    destination: portable::Destination,
    files: BTreeMap<String, Option<String>>,
    revision: Option<i64>,
}

pub struct ConnectOwner {
    directory: PathBuf,
    database: Arc<library::Database>,
    settings_apply: SettingsOwner,
    secrets: Arc<SwitchableSecretStore>,
    source: Arc<SourceOwner>,
    playback: Arc<PlaybackOwner>,
    runtime: tokio::runtime::Handle,
    status: tokio::sync::watch::Sender<Status>,
    started: tokio::sync::watch::Sender<bool>,
    network: tokio::sync::Mutex<Option<Arc<ConnectNetwork>>>,
    session: tokio::sync::RwLock<Option<Arc<Session>>>,
    actions: tokio::sync::Mutex<()>,
    sync: tokio::sync::Mutex<()>,
    transfers: Mutex<BTreeMap<String, (Instant, playback::Continuation)>>,
    joining: std::sync::atomic::AtomicBool,
    download_cancel: Mutex<Option<tokio_util::sync::CancellationToken>>,
    media_jobs: Mutex<BTreeMap<String, tokio_util::sync::CancellationToken>>,
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
        let loaded = std::fs::read(directory.join("device.json"));
        let (config, error) = match loaded {
            Ok(bytes) => match ConnectSettings::from_device_file(&bytes) {
                Ok(config) => (config, None),
                Err(error) => (ConnectSettings::default(), Some(error.to_string())),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                (ConnectSettings::default(), None)
            }
            Err(error) => (ConnectSettings::default(), Some(error.to_string())),
        };
        let owner = Arc::new(Self {
            directory: directory.clone(),
            database,
            settings_apply,
            secrets,
            source,
            playback,
            runtime,
            status: tokio::sync::watch::channel(Status {
                local_folder: config
                    .profile
                    .as_deref()
                    .map(|profile| local_profile_folder(&directory, profile)),
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
                media_status: None,
                error,
            })
            .0,
            network: tokio::sync::Mutex::new(None),
            session: tokio::sync::RwLock::new(None),
            started: tokio::sync::watch::channel(false).0,
            actions: tokio::sync::Mutex::new(()),
            sync: tokio::sync::Mutex::new(()),
            transfers: Mutex::new(BTreeMap::new()),
            download_cancel: Mutex::new(None),
            joining: std::sync::atomic::AtomicBool::new(false),
            media_jobs: Mutex::new(BTreeMap::new()),
        });
        owner.playback.install_connect(&owner);
        owner.source.install_connect(&owner);
        owner
    }

    pub fn status(&self) -> Status {
        self.status.borrow().clone()
    }
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<Status> {
        self.status.subscribe()
    }

    pub async fn roots(&self, source: &str) -> Result<Vec<library::ConnectRoot>, String> {
        let sources = self.source.list_sources();
        if !sources
            .sources
            .iter()
            .any(|entry| entry.id.as_str() == source && entry.kind == "local")
        {
            return Ok(Vec::new());
        }
        self.database.connect_roots(source).await.map_err(error)
    }

    async fn queue_content_id(&self) -> Result<String, String> {
        let local = self.playback.queue_content_id().await?;
        Ok(match self.status().settings.continued_queue {
            Some((received, original)) if received == local => original,
            _ => local,
        })
    }

    /// Called when a client offers continuation after inactivity. This only reads
    /// reachable players; accepting the offer is a separate explicit action.
    pub async fn continuation_offer(&self) -> Result<Option<Device>, String> {
        let mut started = self.started.subscribe();
        if tokio::time::timeout(Duration::from_secs(10), started.wait_for(|ready| *ready))
            .await
            .is_err()
        {
            return Ok(None);
        }
        if !self.status().settings.enabled || self.session.read().await.is_none() {
            return Ok(None);
        }
        self.refresh_devices().await?;
        let local_queue = self.queue_content_id().await?;
        Ok(self.status().devices.into_iter().find(|device| {
            device.enrolled
                && device.reachable
                && device.has_playback
                && device.queue_content_id.as_deref() != Some(local_queue.as_str())
        }))
    }

    pub(crate) fn start(self: &Arc<Self>) {
        let owner = Arc::clone(self);
        self.runtime.spawn(async move {
            {
                let _action = owner.actions.lock().await;
                let _sync = owner.sync.lock().await;
                if !owner.joining.load(std::sync::atomic::Ordering::Acquire)
                    && owner.session.read().await.is_none()
                {
                    if let Some(profile) = owner.status().settings.profile {
                        if let Err(error) = owner.open(profile, false).await {
                            owner.failed(error);
                        }
                    } else if owner.status().settings.enabled {
                        if let Err(error) = owner.network().await {
                            owner.failed(error);
                        }
                    }
                }
                if !owner.joining.load(std::sync::atomic::Ordering::Acquire)
                    && owner.session.read().await.is_some()
                    && owner.status().settings.adopting
                {
                    if let Err(error) = owner.finish_adoption().await {
                        owner.failed(error);
                    }
                }
                if owner.session.read().await.is_some()
                    && !owner.status().settings.setup_pending
                    && owner.source.selected_library().is_none()
                    && let Err(error) = owner.source.connect_changed(false).await
                {
                    owner.failed(error);
                }
                owner.started.send_replace(true);
                owner.source.connect_media_changed();
            }
            let weak = Arc::downgrade(&owner);
            drop(owner);
            let mut last_exchange = Instant::now();
            let mut destination = None;
            let mut busy = false;
            loop {
                if busy {
                    tokio::task::yield_now().await;
                } else {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                busy = false;
                let Some(owner) = weak.upgrade() else { return };
                if owner.session.read().await.is_none() || owner.status().settings.setup_pending {
                    continue;
                }
                if !owner.joining.load(std::sync::atomic::Ordering::Acquire)
                    && !owner.status().settings.adopting
                {
                    match owner.database.connect_seed_page().await {
                        Ok(seeded) => busy |= seeded,
                        Err(error) => owner.failed(error.to_string()),
                    }
                }
                match owner.synchronize().await {
                    Ok(changed) => busy |= changed,
                    Err(error) => owner.failed(error),
                }
                if let Err(error) = owner.configure_network().await {
                    owner.failed(error);
                }
                if let Some(session) = owner.session.read().await.clone() {
                    match session
                        .documents
                        .prune_history(&session.identity, &[], library::CONNECT_PAGE_SIZE)
                        .await
                    {
                        Ok(processed) => busy |= processed > 0,
                        Err(error) => owner.failed(error.to_string()),
                    }
                }
                let current_destination = owner.status().settings.destination;
                if last_exchange.elapsed() >= Duration::from_secs(60)
                    || destination != current_destination
                {
                    destination = current_destination;
                    last_exchange = Instant::now();
                    if let Err(error) = owner.exchange().await {
                        owner.failed(error);
                    }
                    if owner.status().settings.enabled {
                        if let Err(error) = owner.refresh_devices().await {
                            owner.failed(error);
                        }
                    }
                }
            }
        });
    }

    fn failed(&self, error: String) {
        tracing::warn!(%error, "Rufin Connect failed");
        self.status.send_modify(|status| {
            status.error = Some(error);
            status.profile_status = localization::tr("Action required");
        });
    }

    fn save(&self, edit: impl FnOnce(&mut ConnectSettings)) -> Result<(), String> {
        let mut config = self.status().settings;
        edit(&mut config);
        std::fs::create_dir_all(&self.directory).map_err(error)?;
        crate::settings::write_private(
            &self.directory.join("device.json"),
            &serde_json::to_vec(&config).map_err(error)?,
        )?;
        self.status.send_modify(|status| {
            status.local_folder = config
                .profile
                .as_deref()
                .map(|profile| local_profile_folder(&self.directory, profile));
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
            let (network, mut events) = ConnectNetwork::spawn(
                NetworkConfig {
                    database: self.directory.join("network.sqlite"),
                    media_directory: self.directory.join("blobs"),
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
            *current = Some(Arc::clone(&network));
            Ok(network)
        })
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
            "{}.sqlite",
            blake3::hash(profile.as_bytes()).to_hex()
        ));
        let documents = Arc::new(ProfileStore::open(&path, peer).await.map_err(error)?);
        *self.session.write().await = Some(Arc::new(Session {
            settings_revision: Mutex::new(None),
            profile: profile.clone(),
            documents: documents.clone(),
            identity,
            file_exchange: tokio::sync::Mutex::new(None),
            stop: tokio_util::sync::CancellationToken::new(),
            receiving: Mutex::new(BTreeSet::new()),
            catalog_changed: std::sync::atomic::AtomicBool::new(false),
        }));
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
        if self.status().settings.enabled && !self.status().settings.setup_pending {
            self.network()
                .await?
                .attach_documents(documents)
                .await
                .map_err(error)?;
        }
        if self.status().settings.enabled
            && !self.status().settings.established
            && self.network().await?.members().await.map_err(error)?.len() > 1
        {
            self.save(|config| config.established = true)?;
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
                    ConnectRecord {
                        kind: "connect_storage".into(),
                        key: "destination".into(),
                        value: Some(serde_json::json!(
                            self.status()
                                .settings
                                .destination
                                .filter(|destination| { destination.is_remote() })
                        )),
                    },
                ])
                .await
                .map_err(error)?;
            self.publish_device().await?;
        }
        self.database
            .connect_capture_enabled(true)
            .await
            .map_err(error)?;
        if !self.joining.load(std::sync::atomic::Ordering::Acquire) {
            self.save(|config| config.profile = Some(profile.clone()))?;
        }
        if self.status().settings.destination.as_ref().is_none_or(|destination| {
            matches!(destination, portable::Destination::Local { path } if path.as_os_str().is_empty())
        }) {
            self.save(|config| {
                config.destination = Some(self.local_destination(&profile));
            })?;
        }
        if self.status().settings.setup_pending {
            self.status.send_modify(|status| {
                status.profile_status = localization::tr("Set up this device");
            });
        }
        Ok(())
    }

    fn local_destination(&self, profile: &str) -> portable::Destination {
        portable::Destination::Local {
            path: local_profile_folder(&self.directory, profile),
        }
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
            if !settings.setup_pending {
                network
                    .attach_documents(session.documents.clone())
                    .await
                    .map_err(error)?;
            }
        }
        Ok(())
    }

    async fn close_network(&self) -> Result<(), String> {
        let mut current = self.network.lock().await;
        if let Some(network) = current.take() {
            network.shutdown().await.map_err(error)?;
        }
        drop(current);
        if let Some(session) = self.session.read().await.as_ref() {
            session
                .receiving
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clear();
        }
        Ok(())
    }

    async fn leave_profile(&self) -> Result<(), String> {
        self.database
            .connect_capture_enabled(false)
            .await
            .map_err(error)?;
        if let Some(session) = self.session.write().await.take() {
            session.stop.cancel();
        }
        self.close_network().await?;
        self.joining
            .store(false, std::sync::atomic::Ordering::Release);
        self.save(|config| {
            config.enabled = false;
            config.profile = None;
            config.adopting = false;
            config.established = false;
            config.setup_pending = false;
            config.destination = None;
        })?;
        self.status.send_modify(|status| {
            status.devices.clear();
            status.pairing = None;
            status.connecting = false;
            status.receiving_collection = false;
            status.invitation = None;
            status.identity = None;
            status.profile_status = localization::tr("Connect is not enabled");
        });
        Ok(())
    }

    fn cancel_download(&self) {
        if let Some(cancel) = self
            .download_cancel
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            cancel.cancel();
        }
    }

    pub(crate) async fn reset_secret_storage(&self) -> Result<(), String> {
        self.cancel_download();
        let _actions = self.actions.lock().await;
        let _sync = self.sync.lock().await;
        self.leave_profile().await
    }

    async fn restore_after_pairing(self: &Arc<Self>) -> Result<(), String> {
        self.save(|config| config.setup_pending = false)?;
        let result = self.reopen_profile().await;
        self.joining
            .store(false, std::sync::atomic::Ordering::Release);
        self.status.send_modify(|status| {
            status.pairing = None;
            status.connecting = false;
        });
        result
    }

    async fn reopen_profile(self: &Arc<Self>) -> Result<(), String> {
        let _sync = self.sync.lock().await;
        if let Some(network) = self.network.lock().await.as_ref() {
            network.close_profile().await.map_err(error)?;
        }
        if let Some(session) = self.session.write().await.take() {
            session.stop.cancel();
        }
        if let Some(profile) = self.status().settings.profile {
            self.open(profile, false).await?;
        }
        self.joining
            .store(false, std::sync::atomic::Ordering::Release);
        Ok(())
    }

    pub async fn execute(self: &Arc<Self>, action: Action) -> Result<Status, String> {
        if matches!(action, Action::Leave | Action::Join { replace: true, .. }) {
            if let Some(session) = self.session.read().await.as_ref() {
                session.stop.cancel();
            }
        }
        if matches!(
            action,
            Action::CancelDownload
                | Action::Leave
                | Action::Enable { enabled: false }
                | Action::Join { replace: true, .. }
        ) {
            self.cancel_download();
        }
        if matches!(action, Action::CancelDownload) {
            return Ok(self.status());
        }
        if matches!(action, Action::Join { replace: true, .. }) {
            if self.joining.swap(true, std::sync::atomic::Ordering::AcqRel) {
                return Err("Pairing is already in progress".into());
            }
            self.status.send_modify(|status| {
                status.connecting = true;
                status.receiving_collection = false;
                status.error = None;
                status.profile_status = localization::tr("Connecting devices");
            });
        }
        // Controlling a player must not wait behind a long media transfer.
        let _action = if matches!(action, Action::Control { .. }) {
            None
        } else {
            Some(self.actions.lock().await)
        };
        match action {
            Action::Enable { enabled } => {
                if enabled {
                    self.save(|config| config.enabled = true)?;
                    if let Some(profile) = self.status().settings.profile {
                        if self.network.lock().await.is_none() {
                            let _sync = self.sync.lock().await;
                            self.open(profile, false).await?;
                        }
                    } else {
                        let _sync = self.sync.lock().await;
                        self.create_profile().await?;
                    }
                    self.synchronize().await?;
                } else {
                    let _sync = self.sync.lock().await;
                    self.save(|config| config.enabled = false)?;
                    for cancel in self
                        .media_jobs
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .values()
                    {
                        cancel.cancel();
                    }
                    self.close_network().await?;
                    self.joining
                        .store(false, std::sync::atomic::Ordering::Release);
                    self.status.send_modify(|status| {
                        status.devices.retain(|device| device.enrolled);
                        for device in &mut status.devices {
                            device.reachable = false;
                            device.has_playback = false;
                            device.connection.clear();
                        }
                        status.pairing = None;
                        status.connecting = false;
                        status.receiving_collection = false;
                        status.invitation = None;
                        status.profile_status = localization::tr("Connect is not enabled");
                    });
                }
            }
            Action::Create => {
                if self.status().settings.profile.is_some() {
                    return Err("This device already has a Connect profile".into());
                }
                self.save(|config| config.enabled = true)?;
                {
                    let _sync = self.sync.lock().await;
                    self.create_profile().await?;
                }
                self.synchronize().await?;
            }
            Action::Join {
                invitation,
                replace,
            } => {
                if !replace {
                    return Err("Connecting an existing profile will remove all data in this device, do you really want to continue?".into());
                }
                // An earlier queued disconnect may have completed while this
                // request waited for the action owner.
                self.joining
                    .store(true, std::sync::atomic::Ordering::Release);
                self.status.send_modify(|status| {
                    status.connecting = true;
                    status.receiving_collection = false;
                    status.error = None;
                    status.profile_status = localization::tr("Connecting devices");
                });
                let result = async {
                    self.save(|config| config.enabled = true)?;
                    self.network().await?.close_profile().await.map_err(error)?;
                    self.network()
                        .await?
                        .begin_pairing(&invitation)
                        .await
                        .map_err(error)
                }
                .await;
                if let Err(error) = result {
                    self.restore_after_pairing().await?;
                    return Err(error);
                }
            }
            Action::Pair { session, approve } => {
                if approve
                    && self
                        .status()
                        .pairing
                        .as_ref()
                        .is_some_and(|pairing| pairing.session == session && pairing.approved)
                {
                    return Ok(self.status());
                }
                self.connected_network()
                    .await?
                    .confirm_pairing(&session, approve)
                    .await
                    .map_err(error)?;
                self.status.send_modify(|status| {
                    if let Some(pairing) = &mut status.pairing
                        && pairing.session == session
                    {
                        pairing.approved = approve;
                    }
                });
                if !approve {
                    self.status.send_modify(|status| {
                        status.pairing = None;
                        status.connecting = false;
                    });
                    if self.joining.load(std::sync::atomic::Ordering::Acquire) {
                        self.restore_after_pairing().await?;
                    }
                }
            }
            Action::CancelPairing => {
                self.connected_network().await?.cancel_pairing().await;
                self.restore_after_pairing().await?;
            }
            Action::FinishSetup => {
                if self.status().settings.setup_pending {
                    let session = self.active().await?;
                    if self.status().settings.enabled {
                        self.connected_network()
                            .await?
                            .attach_documents(session.documents.clone())
                            .await
                            .map_err(error)?;
                    }
                    self.save(|config| config.setup_pending = false)?;
                    self.source.connect_changed(false).await?;
                    self.source.connect_media_changed();
                    self.status.send_modify(|status| {
                        status.completed_pairings += 1;
                        status.receiving_collection = true;
                        status.profile_status = localization::tr("Receiving collection");
                    });
                }
            }
            Action::Rename { name } => {
                self.save(|config| config.name = name.clone())?;
                if let Some(network) = self.network.lock().await.as_ref() {
                    network.rename(name).await;
                }
                self.publish_device().await?;
            }
            Action::Remove { peer } => {
                let session = self.active().await?;
                self.connected_network()
                    .await?
                    .remove_member(&peer)
                    .await
                    .map_err(error)?;
                let key = portable::new_key();
                let _sync = self.sync.lock().await;
                self.install_file_key(&session.profile, key.clone()).await?;
                self.connected_network()
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
                    .write_records(&[
                        ConnectRecord {
                            kind: "connect_key".into(),
                            key: "file".into(),
                            value: Some(serde_json::json!(key)),
                        },
                        ConnectRecord {
                            kind: "device".into(),
                            key: peer.clone(),
                            value: None,
                        },
                    ])
                    .await
                    .map_err(error)?;
                drop(_sync);
                self.synchronize().await?;
                self.refresh_devices().await?;
            }
            Action::Leave => {
                let _sync = self.sync.lock().await;
                self.leave_profile().await?;
            }
            Action::Refresh => {
                if self.session.read().await.is_some() {
                    self.synchronize().await?;
                    self.exchange().await?;
                    if self.status().settings.enabled {
                        self.refresh_devices().await?;
                    }
                }
            }
            Action::Discover => {
                self.save(|config| config.enabled = true)?;
                if self.network.lock().await.is_none() {
                    if let Some(profile) = self.status().settings.profile {
                        let _sync = self.sync.lock().await;
                        self.open(profile, false).await?;
                    } else {
                        self.network().await?;
                    }
                }
            }
            Action::TestConnection { peer } => {
                let result = self
                    .connected_network()
                    .await?
                    .test_connection(
                        &peer,
                        serde_json::to_vec(&Request::Presence).map_err(error)?,
                    )
                    .await
                    .map_err(error);
                self.status.send_modify(|status| {
                    if let Some(device) = status.devices.iter_mut().find(|device| device.id == peer)
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
                if let Some(session) = self.session.read().await.clone() {
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
                    self.synchronize().await?;
                }
                self.save(|config| {
                    config.nearby = nearby;
                    config.relay = relay;
                    config.public_relay = public_relay;
                })?;
                self.configure_network().await?;
            }
            Action::Continue { peer } => self.continue_from(&peer).await?,
            Action::Control { peer, command } => {
                self.request(&peer, Request::Control { command }).await?;
            }
            Action::Folder {
                source,
                root_id,
                path,
            } => {
                let key = root_id.map_or_else(|| source.clone(), |root| format!("{source}/{root}"));
                self.save(|config| {
                    if path.as_os_str().is_empty() {
                        config.folders.remove(&key);
                    } else {
                        config.folders.insert(key, path);
                    }
                })?;
                self.source.connect_media_changed();
            }
            Action::Encoding { encoding, confirm } => {
                let settings = self.status().settings;
                if settings.encoding != encoding {
                    let redownload = settings.established && !settings.setup_pending;
                    if redownload && !confirm {
                        return Err(localization::tr(
                            "Changing this will redownload all local files.",
                        ));
                    }
                    self.save(|config| config.encoding = encoding)?;
                    if redownload {
                        self.source.connect_media_changed();
                    }
                }
            }
            Action::Download { peer, uri } => {
                self.download(&peer, &uri).await?;
            }
            Action::CancelDownload => self.cancel_download(),
            Action::Destination { destination } => {
                let destination = destination.filter(|destination| {
                    !matches!(destination, portable::Destination::Local { path } if path.as_os_str().is_empty())
                }).or_else(|| {
                    self.status()
                        .settings
                        .profile
                        .as_deref()
                        .map(|profile| self.local_destination(profile))
                });
                let shared = destination
                    .as_ref()
                    .is_some_and(portable::Destination::is_remote)
                    || matches!(
                        self.status().settings.destination,
                        Some(portable::Destination::Remote { .. })
                    );
                let session = self.session.read().await.clone();
                if shared && let Some(session) = &session {
                    session
                        .documents
                        .write_records(&[ConnectRecord {
                            kind: "connect_storage".into(),
                            key: "destination".into(),
                            value: Some(serde_json::json!(
                                destination
                                    .as_ref()
                                    .filter(|destination| { destination.is_remote() })
                            )),
                        }])
                        .await
                        .map_err(error)?;
                }
                self.save(|config| config.destination = destination)?;
                if shared && session.is_some() {
                    self.synchronize().await?;
                }
            }
            Action::Export { path, source_id } => {
                let output = self.export().await?;
                if let Some(source_id) = source_id {
                    let source = self.source.client(&source_id)?;
                    source
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
                    let source = self.source.client(&source_id)?;
                    std::fs::create_dir_all(&self.directory).map_err(error)?;
                    let incoming =
                        tempfile::NamedTempFile::new_in(&self.directory).map_err(error)?;
                    if source
                        .read_profile_file(&path.to_string_lossy(), incoming.path())
                        .await
                        .map_err(error)?
                        .is_none()
                    {
                        return Err("The Connect file was not found".into());
                    }
                    self.import_file(incoming.path().to_owned(), replace, key)
                        .await?;
                } else {
                    self.import_file(path, replace, key).await?;
                }
            }
        }
        self.status.send_modify(|status| status.error = None);
        Ok(self.status())
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
            } => {
                self.status.send_modify(|status| {
                    status.error = None;
                    status.connecting = false;
                    status.pairing = Some(Pairing {
                        session,
                        peer,
                        name,
                        emoji,
                        approved: false,
                        verified: false,
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
                profile_id,
                peer,
                name,
                enrollment_data,
            } => {
                let owner = self.clone();
                let network = self.connected_network().await?;
                self.status.send_modify(|status| {
                    status.pairing = None;
                    status.connecting = true;
                    status.profile_status = localization::tr("Receiving profile");
                });
                self.runtime.spawn(async move {
                    if let Err(error) = owner
                        .adopt_paired(&network, profile_id, peer, name, enrollment_data)
                        .await
                    {
                        let _action = owner.actions.lock().await;
                        if !owner.joining.load(std::sync::atomic::Ordering::Acquire)
                            || !owner
                                .network
                                .lock()
                                .await
                                .as_ref()
                                .is_some_and(|current| Arc::ptr_eq(current, &network))
                        {
                            return;
                        }
                        if !owner.status().settings.adopting {
                            let _ = owner.restore_after_pairing().await;
                        }
                        owner
                            .joining
                            .store(false, std::sync::atomic::Ordering::Release);
                        owner.status.send_modify(|status| {
                            status.pairing = None;
                            status.connecting = false;
                            status.receiving_collection = false;
                        });
                        owner.failed(error);
                    }
                });
            }
            NetworkEvent::MemberAdded { peer, name } => {
                self.save(|config| config.established = true)?;
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
                        queue_content_id: None,
                        enrolled: true,
                        connection: String::new(),
                    });
                });
            }
            NetworkEvent::Discovered { peer, name } => {
                self.status.send_modify(|status| {
                    if !status.devices.iter().any(|device| device.id == peer) {
                        status.devices.push(Device {
                            id: peer,
                            name,
                            reachable: true,
                            has_playback: false,
                            queue_content_id: None,
                            enrolled: false,
                            connection: String::new(),
                        });
                    }
                });
            }
            NetworkEvent::MembershipChanged => {
                if !self.joining.load(std::sync::atomic::Ordering::Acquire) {
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
                                owner.execute(Action::Leave).await.map(|_| ())
                            }
                        }
                        .await;
                        if let Err(error) = result {
                            owner.failed(error);
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
                    let _ = reply.send(owner.answer(&profile, &peer, &body).await);
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
                        receiving.insert(peer);
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
                if let Some(session) = &session
                    && !self
                        .status()
                        .pairing
                        .as_ref()
                        .is_some_and(|pairing| &pairing.session == session)
                {
                    return Ok(());
                }
                if self.joining.load(std::sync::atomic::Ordering::Acquire) {
                    let owner = self.clone();
                    self.runtime.spawn(async move {
                        let _action = owner.actions.lock().await;
                        if owner.joining.load(std::sync::atomic::Ordering::Acquire)
                            && session.as_ref().is_none_or(|session| {
                                owner
                                    .status()
                                    .pairing
                                    .as_ref()
                                    .is_some_and(|pairing| &pairing.session == session)
                            })
                            && !owner.status().settings.adopting
                        {
                            let _ = owner.restore_after_pairing().await;
                            owner.failed(message);
                        }
                    });
                } else {
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
        profile: String,
        peer: String,
        name: String,
        bytes: Vec<u8>,
    ) -> Result<(), String> {
        if !self.joining.load(std::sync::atomic::Ordering::Acquire) {
            return Err("This device is not joining a profile".into());
        }
        let enrollment: Enrollment = serde_json::from_slice(&bytes).map_err(error)?;
        let _: age::x25519::Identity = enrollment.file_key.parse().map_err(error)?;
        network
            .open_profile(&profile, false, bytes)
            .await
            .map_err(error)?;
        network.check_membership(&peer).await.map_err(error)?;
        let staged = self.fetch_snapshot(network, &peer, true).await?;
        let _action = self.actions.lock().await;
        if !self.joining.load(std::sync::atomic::Ordering::Acquire)
            || !self
                .network
                .lock()
                .await
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, network))
        {
            return Ok(());
        }
        self.save(|config| config.setup_pending = true)?;
        self.read_profile_file(
            staged.to_path_buf(),
            true,
            Some(enrollment.file_key),
            Some(&profile),
        )
        .await?;
        if self.active().await?.stop.is_cancelled() {
            // Joining pauses file exchange even when this is the profile already
            // imported locally. Resume it with the newly verified membership.
            self.reopen_profile().await?;
        }
        self.publish_device().await?;
        self.save(|config| config.established = true)?;
        self.joining
            .store(false, std::sync::atomic::Ordering::Release);
        self.status.send_modify(|status| {
            status.error = None;
            status.pairing = None;
            status.connecting = false;
            status.devices.retain(|device| device.id != peer);
            status.devices.push(Device {
                id: peer.clone(),
                name,
                reachable: true,
                has_playback: false,
                queue_content_id: None,
                enrolled: true,
                connection: String::new(),
            });
            status.receiving_collection = false;
            status.profile_status = localization::tr("Set up this device");
        });
        Ok(())
    }

    async fn fetch_snapshot(
        &self,
        network: &ConnectNetwork,
        peer: &str,
        setup: bool,
    ) -> Result<tempfile::TempPath, String> {
        let answer = network
            .request(
                peer,
                serde_json::to_vec(&Request::ProfileSnapshot { setup }).map_err(error)?,
            )
            .await
            .map_err(error)?;
        let answer: serde_json::Value = serde_json::from_slice(&answer).map_err(error)?;
        let hash = answer["hash"]
            .as_str()
            .ok_or("The peer did not provide its profile")?;
        // Blob installation replaces this path; close its handle before fetching on Windows.
        let staged = tempfile::NamedTempFile::new_in(&self.directory)
            .map_err(error)?
            .into_temp_path();
        network
            .media()
            .fetch(
                peer,
                hash,
                &staged,
                tokio_util::sync::CancellationToken::new(),
                tokio::sync::watch::channel(0).0,
            )
            .await
            .map_err(error)?;
        Ok(staged)
    }

    async fn synchronize(&self) -> Result<bool, String> {
        let _sync = self.sync.lock().await;
        if self.status().settings.adopting
            || self.status().settings.setup_pending
            || self.joining.load(std::sync::atomic::Ordering::Acquire)
        {
            return Ok(false);
        }
        let Some(session) = self.session.read().await.clone() else {
            return Ok(false);
        };
        let revision = (self.settings_apply.revision(), self.secrets.revision());
        if *session
            .settings_revision
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            != Some(revision)
        {
            let settings = self.settings_apply.clone();
            let secrets = Arc::clone(&self.secrets);
            let records = tokio::task::spawn_blocking(move || settings.connect_records(&secrets))
                .await
                .map_err(error)??;
            session
                .documents
                .write_settings(&records)
                .await
                .map_err(error)?;
            *session
                .settings_revision
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = Some(revision);
        }
        let captured = session
            .documents
            .capture(&self.database)
            .await
            .map_err(error)?;
        let projected = self.project(&session).await?;
        Ok(captured > 0 || projected)
    }

    async fn project(&self, session: &Session) -> Result<bool, String> {
        let (changed, sources_changed, projected) = self.apply_projection(session, false).await?;
        if changed {
            session
                .catalog_changed
                .store(true, std::sync::atomic::Ordering::Release);
        }
        let receiving = !session
            .receiving
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_empty();
        let pending = session
            .documents
            .projection_pending()
            .await
            .map_err(error)?;
        if sources_changed || !pending {
            let changed = session
                .catalog_changed
                .load(std::sync::atomic::Ordering::Acquire);
            if changed || sources_changed {
                self.source.connect_changed(sources_changed).await?;
                self.source.connect_media_changed();
                session
                    .catalog_changed
                    .store(false, std::sync::atomic::Ordering::Release);
            }
        }
        if !receiving && !pending && !self.status().connecting {
            self.status.send_if_modified(|status| {
                let changed = status.receiving_collection;
                status.receiving_collection = false;
                let message = if status.settings.enabled {
                    localization::tr("Profile up to date")
                } else {
                    localization::tr("Connect is not enabled")
                };
                if status.error.is_none() && status.profile_status != message {
                    status.profile_status = message;
                    return true;
                }
                changed
            });
        }
        Ok(projected)
    }

    async fn apply_projection(
        &self,
        session: &Session,
        all: bool,
    ) -> Result<(bool, bool, bool), String> {
        let mut projected = false;
        let mut changed = false;
        let mut sources_changed = false;
        loop {
            let (records, library_changed) = session
                .documents
                .project(&self.database)
                .await
                .map_err(error)?;
            if records.is_empty() {
                break;
            }
            projected = true;
            changed |= library_changed;
            let settings = self.settings_apply.clone();
            let secrets = Arc::clone(&self.secrets);
            let apply = records.clone();
            sources_changed |= tokio::task::spawn_blocking(move || {
                settings.apply_connect_records(&apply, &secrets)
            })
            .await
            .map_err(error)??;
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
                if record.kind == "connect_storage" && record.key == "destination" {
                    let destination: Option<portable::Destination> =
                        serde_json::from_value(record.value.clone().unwrap_or_default())
                            .map_err(error)?;
                    match destination {
                        Some(destination @ portable::Destination::Remote { .. }) => {
                            self.save(|config| config.destination = Some(destination))?;
                        }
                        None => self.save(|config| {
                            if matches!(
                                config.destination,
                                Some(portable::Destination::Remote { .. })
                            ) {
                                config.destination = Some(self.local_destination(&session.profile));
                            }
                        })?,
                        Some(portable::Destination::Local { .. }) => {}
                    }
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
            session
                .documents
                .acknowledge_projection(&records)
                .await
                .map_err(error)?;
            if !all {
                break;
            }
        }
        Ok((changed, sources_changed, projected))
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

    async fn refresh_devices(&self) -> Result<(), String> {
        let _sync = self.sync.lock().await;
        if self.joining.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(());
        }
        let session = self.active().await?;
        let mut devices = Vec::new();
        let network = self.network.lock().await.clone();
        let Some(network) = network else {
            return Ok(());
        };
        let members = network.members().await.map_err(error)?;
        drop(_sync);
        for peer in members {
            if peer == session.identity {
                continue;
            }
            let previous = self
                .status()
                .devices
                .into_iter()
                .find(|device| device.id == peer);
            let name = session.documents.device_name(&peer).await.map_err(error)?;
            let mut device = previous.unwrap_or(Device {
                id: peer.clone(),
                name: "Rufin".into(),
                reachable: false,
                has_playback: false,
                queue_content_id: None,
                enrolled: true,
                connection: String::new(),
            });
            if let Some(name) = name {
                device.name = name;
            }
            device.enrolled = true;
            let answer = tokio::time::timeout(
                Duration::from_secs(3),
                self.request(&peer, Request::Presence),
            )
            .await;
            if let Ok(Ok(answer)) = answer {
                device.name = answer["name"].as_str().unwrap_or(&device.name).to_owned();
                device.has_playback = answer["playback"].as_bool().unwrap_or(false);
                device.queue_content_id = answer["queue_content_id"].as_str().map(str::to_owned);
                device.reachable = true;
                device.connection = connection_label(
                    &network
                        .test_connection(
                            &peer,
                            serde_json::to_vec(&Request::Presence).map_err(error)?,
                        )
                        .await
                        .unwrap_or_default(),
                );
            } else {
                device.reachable = false;
                device.has_playback = false;
                device.connection = localization::tr("Waiting for device");
            }
            devices.push(device);
        }
        self.status.send_modify(|status| {
            if status.settings.enabled {
                status.devices = devices;
            }
        });
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

    async fn answer(&self, profile: &str, peer: &str, bytes: &[u8]) -> Result<Vec<u8>, String> {
        let session = self.active().await?;
        let network = self.connected_network().await?;
        if session.profile != profile
            || self.joining.load(std::sync::atomic::Ordering::Acquire)
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
            Request::ProfileSnapshot { setup } => {
                let snapshot = self.export_session(&session, setup).await?;
                let hash = network
                    .media()
                    .publish(snapshot.path(), "profile-snapshot")
                    .await
                    .map_err(error)?;
                serde_json::json!({"hash":hash})
            }
            Request::Presence => {
                let active = self.playback.has_continuation();
                serde_json::json!({"name":self.status().settings.name,"playback":active,
                    "queue_content_id":self.queue_content_id().await?})
            }
            Request::Continuation => {
                let playback = Arc::clone(&self.playback);
                let snapshot =
                    tokio::task::spawn_blocking(move || playback.continuation_snapshot())
                        .await
                        .map_err(error)??
                        .ok_or("There is no playback to continue")?;
                let mut value = serde_json::to_value(&snapshot.header).map_err(error)?;
                value["queue_content_id"] = serde_json::json!(self.queue_content_id().await?);
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
            Request::StopContinuation => {
                let snapshot = self
                    .transfers
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(peer)
                    .map(|(_, snapshot)| snapshot)
                    .ok_or("Continuation expired")?;
                let playback = Arc::clone(&self.playback);
                let header = tokio::task::spawn_blocking(move || {
                    playback.stop_continuation(snapshot.header)
                })
                .await
                .map_err(error)??
                .ok_or("Playback changed. Request continuation again")?;
                serde_json::to_value(header).map_err(error)?
            }
            Request::Control { command } => {
                let playback = Arc::clone(&self.playback);
                tokio::task::spawn_blocking(move || match command {
                    Control::Play => playback.play(),
                    Control::Pause => playback.pause(),
                    Control::Stop => playback.stop(),
                    Control::Next => playback.next(),
                    Control::Previous => playback.previous(),
                    Control::Seek { millis } => playback.seek_millis(millis),
                    Control::Volume { value } => playback.set_volume(value),
                })
                .await
                .map_err(error)?;
                serde_json::Value::Null
            }
            Request::Media {
                id,
                uri,
                encoding,
                occurrence,
            } => {
                self.serve_media(peer, &id, &uri, encoding, occurrence.as_ref())
                    .await?
            }
            Request::CancelMedia { id } => {
                self.cancel_serving(peer, &id);
                serde_json::Value::Null
            }
        };
        serde_json::to_vec(&answer).map_err(error)
    }

    async fn continue_from(&self, peer: &str) -> Result<(), String> {
        let answer = self.request(peer, Request::Continuation).await?;
        let original_queue = answer["queue_content_id"].as_str().map(str::to_owned);
        let header: playback::ContinuationHeader = serde_json::from_value(answer).map_err(error)?;
        let transfer = format!("connect-{}", blake3::hash(peer.as_bytes()).to_hex());
        let result = async {
            let mut offset = 0;
            let mut current = None;
            while offset < header.total {
                let page: library::QueueTransferPage =
                    serde_json::from_value(self.request(peer, Request::Queue { offset }).await?)
                        .map_err(error)?;
                if page.occurrences.is_empty() {
                    return Err("The source queue changed during continuation".into());
                }
                if let Some(row) = page
                    .occurrences
                    .iter()
                    .find(|row| row.occurrence == header.current)
                {
                    current = Some(row.item.clone());
                }
                offset += page.occurrences.len();
                self.playback
                    .stage_continuation_page(&transfer, &page)
                    .await?;
            }
            let current = current.ok_or("The source queue has no current track")?;
            if self
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
                } else if library::file_media_path(&current.media_uri).is_some()
                    || library::cue_media_parts(&current.media_uri).is_some()
                {
                    self.fetch_direct_continuation(peer, &current, &header.current)
                        .await?;
                }
            }
            let prepared = self
                .playback
                .prepare_continuation(transfer.clone(), header)
                .await?;
            let fresh =
                serde_json::from_value(self.request(peer, Request::StopContinuation).await?)
                    .map_err(error)?;
            self.playback.apply_continuation(prepared, fresh).await?;
            let local_queue = self.playback.queue_content_id().await?;
            self.save(|config| {
                config.continued_queue = original_queue
                    .filter(|original| original != &local_queue)
                    .map(|original| (local_queue, original));
            })
        }
        .await;
        if result.is_err() {
            let _ = self.playback.discard_continuation(&transfer).await;
        }
        result
    }
}

impl ConnectOwner {
    async fn export(&self) -> Result<tempfile::NamedTempFile, String> {
        let session = self.active().await?;
        self.export_session(&session, false).await
    }

    async fn export_session(
        &self,
        session: &Arc<Session>,
        setup: bool,
    ) -> Result<tempfile::NamedTempFile, String> {
        self.synchronize().await?;
        if !setup {
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
                let seeded = self.database.connect_seed_page().await.map_err(error)?;
                let captured = self.synchronize().await?;
                if !seeded && !captured {
                    break;
                }
                tokio::task::yield_now().await;
            }
        }
        let key = self
            .secret(file_key(&self.identity_reference()?, &session.profile))
            .await?
            .ok_or("The Connect file key is unavailable")?;
        let snapshot = tempfile::tempfile_in(&self.directory).map_err(error)?;
        if setup {
            // Folder identities and labels are setup data; track rows can follow
            // after this device chooses which existing folders to reuse.
            for source in self.source.list_sources().sources.iter() {
                let roots = self.roots(source.id.as_str()).await?;
                let records = roots
                    .into_iter()
                    .map(|root| ConnectRecord {
                        kind: "root".into(),
                        key: serde_json::json!([source.id, root.id]).to_string(),
                        value: Some(serde_json::json!({
                            "source_id": source.id, "root_id": root.id, "root_label": root.label,
                        })),
                    })
                    .collect::<Vec<_>>();
                session
                    .documents
                    .write_records(&records)
                    .await
                    .map_err(error)?;
            }
            session
                .documents
                .export_setup_snapshot(snapshot.try_clone().map_err(error)?)
                .await
                .map_err(error)?;
        } else {
            session
                .documents
                .export_device_snapshot(snapshot.try_clone().map_err(error)?, &session.identity)
                .await
                .map_err(error)?;
            let members = match self.network.lock().await.as_ref() {
                Some(network) => network.members().await.map_err(error)?,
                None => vec![session.identity.clone()],
            };
            session
                .documents
                .finish_device_snapshot(snapshot.try_clone().map_err(error)?, &members)
                .await
                .map_err(error)?;
        }
        let profile = session.profile.clone();
        let directory = self.directory.clone();
        tokio::task::spawn_blocking(move || {
            let output = tempfile::NamedTempFile::new_in(directory).map_err(error)?;
            portable::encrypt(snapshot, output.path(), &profile, &key)?;
            Ok(output)
        })
        .await
        .map_err(error)?
    }

    async fn import_file(
        self: &Arc<Self>,
        path: PathBuf,
        replace: bool,
        key: Option<String>,
    ) -> Result<(), String> {
        self.read_profile_file(path, replace, key, None).await
    }

    async fn read_profile_file(
        self: &Arc<Self>,
        path: PathBuf,
        replace: bool,
        key: Option<String>,
        expected_profile: Option<&str>,
    ) -> Result<(), String> {
        let current = self.session.read().await.clone();
        let explicit_key = key.clone();
        let keys = match key {
            Some(key) => vec![key],
            None => {
                let mut profiles = self.status().settings.file_profiles;
                if let Some(current) = &current {
                    if !profiles.contains(&current.profile) {
                        profiles.push(current.profile.clone());
                    }
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
                if keys.is_empty() {
                    return Err(
                        "Pair with a trusted device to obtain access to this Connect file".into(),
                    );
                }
                keys
            }
        };
        let directory = self.directory.clone();
        let (profile, staged) = tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&directory).map_err(error)?;
            portable::decrypt(&path, &keys, &directory)
        })
        .await
        .map_err(error)??;
        let key = match explicit_key {
            Some(key) => key,
            None => self
                .secret(file_key(&self.identity_reference()?, &profile))
                .await?
                .ok_or("The Connect file key is unavailable")?,
        };
        if expected_profile.is_some_and(|expected| expected != profile) {
            return Err("The received profile does not match the verified device".into());
        }
        let _sync = self.sync.lock().await;
        let current = self.session.read().await.clone();
        let adoption = current
            .as_ref()
            .is_none_or(|session| session.profile != profile);
        if adoption && !replace {
            return Err("Connecting an existing profile will remove all data in this device, do you really want to continue?".into());
        }
        if adoption || self.status().settings.adopting {
            let identity = self.credentials().await?.public().to_string();
            let peer = u64::from_le_bytes(
                blake3::hash(identity.as_bytes()).as_bytes()[..8]
                    .try_into()
                    .unwrap(),
            );
            let path = self.directory.join(format!(
                "{}.sqlite",
                blake3::hash(profile.as_bytes()).to_hex()
            ));
            let documents = ProfileStore::open(&path, peer).await.map_err(error)?;
            // Validate the complete snapshot transaction before touching working
            // settings, queues or collection. A failed import keeps them intact.
            documents
                .replace_snapshot(staged.try_clone().map_err(error)?)
                .await
                .map_err(error)?;
            self.joining
                .store(true, std::sync::atomic::Ordering::Release);
            self.install_file_key(&profile, key).await?;
            self.save(|config| {
                if matches!(
                    config.destination,
                    Some(portable::Destination::Local { .. })
                ) {
                    config.destination = Some(self.local_destination(&profile));
                }
                config.profile = Some(profile.clone());
                config.adopting = true;
            })?;
            if let Some(network) = self.network.lock().await.as_ref() {
                network.close_profile().await.map_err(error)?;
            }
            if let Some(session) = self.session.write().await.take() {
                session.stop.cancel();
            }
            drop(documents);
            self.open(profile, false).await?;
            self.finish_adoption().await?;
            if expected_profile.is_none() {
                self.joining
                    .store(false, std::sync::atomic::Ordering::Release);
            }
        } else {
            let session = self.active().await?;
            let settings = self.settings_apply.clone();
            let secrets = Arc::clone(&self.secrets);
            let records = tokio::task::spawn_blocking(move || settings.connect_records(&secrets))
                .await
                .map_err(error)??;
            session
                .documents
                .write_settings(&records)
                .await
                .map_err(error)?;
            while session
                .documents
                .capture(&self.database)
                .await
                .map_err(error)?
                > 0
            {}
            drop(_sync);
            session
                .documents
                .import_snapshot(staged.try_clone().map_err(error)?)
                .await
                .map_err(error)?;
            let _sync = self.sync.lock().await;
            if self
                .session
                .read()
                .await
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &session))
            {
                self.project(&session).await?;
            }
        }
        Ok(())
    }

    async fn finish_adoption(&self) -> Result<(), String> {
        let session = self.active().await?;
        self.source
            .restore_user_state(true, || async {
                let playback = Arc::clone(&self.playback);
                tokio::task::spawn_blocking(move || playback.shutdown())
                    .await
                    .map_err(error)?;
                let result = async {
                    self.database
                        .connect_replace_profile()
                        .await
                        .map_err(error)?;
                    let settings = self.settings_apply.clone();
                    let secrets = Arc::clone(&self.secrets);
                    tokio::task::spawn_blocking(move || settings.connect_replace_profile(&secrets))
                        .await
                        .map_err(error)??;
                    session.documents.reproject_all().await.map_err(error)?;
                    self.apply_projection(&session, true).await?;
                    Ok::<_, String>(())
                }
                .await;
                self.playback.start().await?;
                result
            })
            .await?;
        if !self.status().settings.setup_pending {
            self.source.connect_changed(false).await?;
        }
        self.database
            .connect_capture_enabled(true)
            .await
            .map_err(error)?;
        self.save(|config| {
            config.adopting = false;
            config.established = true;
        })?;
        Ok(())
    }

    async fn exchange_import(
        &self,
        session: &Arc<Session>,
        path: PathBuf,
    ) -> Result<std::fs::File, String> {
        let identity = self.identity_reference()?;
        let mut keys = vec![
            self.secret(file_key(&identity, &session.profile))
                .await?
                .ok_or("The Connect file key is unavailable")?,
        ];
        if let Some(previous) = self
            .secret(previous_file_keys(&identity, &session.profile))
            .await?
        {
            keys.extend(serde_json::from_str::<Vec<String>>(&previous).map_err(error)?);
        }
        let directory = self.directory.clone();
        let (profile, staged) =
            tokio::task::spawn_blocking(move || portable::decrypt(&path, &keys, &directory))
                .await
                .map_err(error)??;
        if profile != session.profile {
            return Err("The Connect file belongs to another profile".into());
        }
        // Background exchange only updates the profile it started with. Reading
        // a file must not hold the lock needed by Join or Disconnect.
        session
            .documents
            .import_snapshot(staged.try_clone().map_err(error)?)
            .await
            .map_err(error)?;
        Ok(staged)
    }

    async fn exchange(self: &Arc<Self>) -> Result<(), String> {
        if self.joining.load(std::sync::atomic::Ordering::Acquire)
            || self.status().settings.adopting
            || self.status().settings.setup_pending
        {
            return Ok(());
        }
        let session = self.active().await?;
        session
            .stop
            .run_until_cancelled(self.exchange_session(&session))
            .await
            .unwrap_or(Ok(()))
    }

    async fn exchange_session(self: &Arc<Self>, session: &Arc<Session>) -> Result<(), String> {
        let Some(destination) = self.status().settings.destination else {
            return Ok(());
        };
        let mut previous = session.file_exchange.lock().await;
        let cached = previous.get_or_insert_with(|| FileExchange {
            destination: destination.clone(),
            files: BTreeMap::new(),
            revision: None,
        });
        if cached.destination != destination {
            *cached = FileExchange {
                destination: destination.clone(),
                files: BTreeMap::new(),
                revision: None,
            };
        }
        let source = match &destination {
            portable::Destination::Remote { source_id, .. } => Some(self.source.client(source_id)?),
            portable::Destination::Local { .. } => None,
        };
        let entries = match &destination {
            portable::Destination::Local { path } => {
                tokio::fs::create_dir_all(path).await.map_err(error)?;
                let mut directory = tokio::fs::read_dir(path).await.map_err(error)?;
                let mut files = Vec::new();
                while let Some(entry) = directory.next_entry().await.map_err(error)? {
                    let name = entry.file_name();
                    let Some(name) = name
                        .to_str()
                        .filter(|name| name.ends_with(".rufin-connect"))
                    else {
                        continue;
                    };
                    let metadata = entry.metadata().await.map_err(error)?;
                    if metadata.is_file() {
                        files.push((
                            name.to_owned(),
                            Some(format!(
                                "{:?}:{}",
                                metadata.modified().map_err(error)?,
                                metadata.len()
                            )),
                        ));
                    }
                }
                files
            }
            portable::Destination::Remote { path, .. } => source
                .as_ref()
                .unwrap()
                .profile_files(path)
                .await
                .map_err(error)?,
        };
        let filename = format!("{}.rufin-connect", session.identity);
        let own_path = match &destination {
            portable::Destination::Local { .. } => filename,
            portable::Destination::Remote { path, .. } => {
                let folder = path.trim_matches('/');
                if folder.is_empty() {
                    filename
                } else {
                    format!("{folder}/{filename}")
                }
            }
        };
        let own_exists = entries.iter().any(|(path, _)| path == &own_path);
        for (path, version) in entries {
            if version.is_some() && cached.files.get(&path) == Some(&version) {
                continue;
            }
            let incoming = tempfile::NamedTempFile::new_in(&self.directory).map_err(error)?;
            let input = match &destination {
                portable::Destination::Local { path: folder } => folder.join(&path),
                portable::Destination::Remote { .. } => {
                    if source
                        .as_ref()
                        .unwrap()
                        .read_profile_file(&path, incoming.path())
                        .await
                        .map_err(error)?
                        .is_none()
                    {
                        continue;
                    }
                    incoming.path().to_owned()
                }
            };
            let imported = self.exchange_import(session, input).await?;
            self.synchronize().await?;
            if path == own_path
                && session
                    .documents
                    .snapshot_contains_current(imported)
                    .await
                    .map_err(error)?
            {
                cached.revision = Some(session.documents.revision().await.map_err(error)?);
            }
            cached.files.insert(path, version);
        }
        let revision = session.documents.revision().await.map_err(error)?;
        if own_exists && cached.revision == Some(revision) {
            return Ok(());
        }
        let output = self.export_session(session, false).await?;
        let version = match &destination {
            portable::Destination::Local { path } => {
                let path = path.join(&own_path);
                portable::install(output, path.clone()).await?;
                let metadata = tokio::fs::metadata(path).await.map_err(error)?;
                Some(format!(
                    "{:?}:{}",
                    metadata.modified().map_err(error)?,
                    metadata.len()
                ))
            }
            portable::Destination::Remote { path, .. } => {
                source
                    .as_ref()
                    .unwrap()
                    .write_profile_file(&own_path, output, Some(None))
                    .await
                    .map_err(error)?;
                source
                    .as_ref()
                    .unwrap()
                    .profile_files(path)
                    .await
                    .map_err(error)?
                    .into_iter()
                    .find_map(|(path, version)| (path == own_path).then_some(version))
                    .flatten()
            }
        };
        cached.revision = Some(revision);
        // This device only writes its own file; other writers' snapshots remain intact.
        cached.files.insert(own_path, version);
        Ok(())
    }
}

fn local_profile_folder(directory: &std::path::Path, profile: &str) -> PathBuf {
    directory.join(blake3::hash(profile.as_bytes()).to_hex().as_str())
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
fn error(error: impl std::fmt::Display) -> String {
    let message = error.to_string();
    if message == rufin_connect::profile::INCOMPATIBLE_PROFILE {
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
