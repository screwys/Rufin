//! Rufin Connect: pairing, the records paired devices sync and the sync folder. Connect is on
//! exactly when `connect.sqlite` holds its profile row.
mod folder;
mod peer;
mod records;
mod session;

use crate::{
    playback::PlaybackOwner,
    settings::{SettingsFile, SettingsOwner},
    source::SourceOwner,
};
use age::secrecy::ExposeSecret;
use records::{DeviceValue, Receiving, Records};
use rufin_connect::{
    ConnectNetwork, Credentials, NetworkConfig, NetworkEvent, Store,
    network::{INCOMPATIBLE, REMOVED_FROM_PROFILE},
    store::Cursor,
    sync::Frame,
};
use secrets::{SecretKey, SecretStore, SwitchableSecretStore};
use serde::{Deserialize, Serialize};
pub use session::Continuation;
pub(crate) use session::{Observed, Observing};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const PRESENCE: Duration = Duration::from_secs(300);
const KEYRING_UNAVAILABLE: &str =
    "The system keyring is unavailable, so Rufin Connect cannot start now";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ConnectSettings {
    pub enabled: bool,
    pub name: String,
    pub profile: Option<String>,
    pub nearby: bool,
    pub relay: Option<String>,
    pub public_relay: bool,
    pub folder: Option<SyncFolder>,
}

/// Where the sync folder lives: a folder here, an Android document tree, or a folder on a
/// saved WebDAV or SMB connection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SyncFolder {
    Connection {
        connection: sources::SourceId,
        path: String,
    },
    Path {
        path: PathBuf,
    },
    Document {
        document: String,
    },
}

/// `device.json`: what this device keeps of Connect while it is off.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
struct DeviceConfig {
    name: String,
    identity_ref: Option<String>,
    nearby: bool,
    relay: Option<String>,
    public_relay: bool,
}

impl Default for DeviceConfig {
    fn default() -> Self {
        Self {
            name: if cfg!(target_os = "android") {
                "Rufin Android"
            } else {
                "Rufin Desktop"
            }
            .into(),
            identity_ref: None,
            nearby: true,
            relay: None,
            public_relay: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Device {
    pub id: String,
    pub name: String,
    pub reachable: bool,
    #[serde(default)]
    pub enrolled: bool,
    pub connection: String,
    /// A paired device whose session records something to continue, online or not.
    #[serde(default)]
    pub continuable: bool,
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

/// Records applied of the records the joined device held.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct CollectionProgress {
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
    /// Another device's playback that this device can continue.
    #[serde(default)]
    pub continuation: Option<Continuation>,
    pub error: Option<String>,
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
    Folder {
        folder: Option<SyncFolder>,
    },
    Continue {
        peer: String,
    },
    Control {
        peer: String,
        command: Control,
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
        mode: playback::QueueRepeatMode,
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

/// A request on the RPC protocol.
#[derive(Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
enum Request {
    Presence,
    Control {
        command: Control,
    },
    Cast(session::Cast),
    /// The whole queue, for a selection of it.
    Queue,
    QueuePage {
        offset: u64,
        limit: u32,
        filter: String,
    },
}

/// A request on the media protocol.
#[derive(Serialize, Deserialize)]
#[serde(tag = "stream", rename_all = "snake_case")]
enum Stream {
    /// For the peer source on another device: a read of one of this device's Local sources.
    Catalog {
        source: sources::SourceId,
        read: peer::Read,
    },
    Image {
        source: sources::SourceId,
        image: String,
        size: Option<u32>,
    },
    Audio {
        source: sources::SourceId,
        object: String,
        encoding: Option<library::AudioEncoding>,
        range: Option<(u64, Option<u64>)>,
        window: Option<(u64, Option<u64>)>,
    },
    /// This device's playback state, for a device that casts here.
    Observe,
}

#[derive(Serialize, Deserialize)]
struct Presence {
    sequence: i64,
    playing: bool,
    /// The routes the device knows to the profile's devices, so devices that joined through
    /// it reach each other.
    addresses: Vec<rufin_connect::EndpointAddr>,
}

/// Connect while it is on.
struct Session {
    profile: String,
    records: Arc<Records>,
    stop: CancellationToken,
    folder: folder::State,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct ConnectOwner {
    directory: PathBuf,
    catalog: Arc<library::Catalog>,
    settings: SettingsFile,
    settings_apply: SettingsOwner,
    secrets: Arc<SwitchableSecretStore>,
    source: Arc<SourceOwner>,
    pub(crate) playback: Arc<PlaybackOwner>,
    lyrics: crate::lyrics::LyricsHandle,
    runtime: tokio::runtime::Handle,
    config: Mutex<DeviceConfig>,
    status: tokio::sync::watch::Sender<Status>,
    network: tokio::sync::Mutex<Option<Arc<ConnectNetwork>>>,
    session: Mutex<Option<Arc<Session>>>,
    /// Turning on and off, joining and network changes run one at a time.
    actions: tokio::sync::Mutex<()>,
    /// The number of each device's latest presence probe. Only the latest shows: an earlier
    /// one can still be waiting on an address the device left.
    probes: Mutex<HashMap<String, u64>>,
}

impl ConnectOwner {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        directory: PathBuf,
        catalog: Arc<library::Catalog>,
        settings: SettingsFile,
        settings_apply: SettingsOwner,
        secrets: Arc<SwitchableSecretStore>,
        source: Arc<SourceOwner>,
        playback: Arc<PlaybackOwner>,
        lyrics: crate::lyrics::LyricsHandle,
        runtime: tokio::runtime::Handle,
    ) -> Arc<Self> {
        let (config, error) = match std::fs::read(directory.join("device.json")) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(config) => (config, None),
                Err(error) => (DeviceConfig::default(), Some(error.to_string())),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                (DeviceConfig::default(), None)
            }
            Err(error) => (DeviceConfig::default(), Some(error.to_string())),
        };
        Arc::new(Self {
            directory,
            catalog,
            settings,
            settings_apply,
            secrets,
            source,
            playback,
            lyrics,
            runtime,
            status: tokio::sync::watch::channel(Status {
                settings: settings_of(&config, None, None),
                identity: None,
                invitation: None,
                devices: Vec::new(),
                pairing: None,
                connecting: false,
                receiving_collection: false,
                completed_pairings: 0,
                profile_status: localization::tr("Connect is not enabled"),
                collection_progress: None,
                continuation: None,
                error,
            })
            .0,
            config: Mutex::new(config),
            network: tokio::sync::Mutex::new(None),
            session: Mutex::new(None),
            actions: tokio::sync::Mutex::new(()),
            probes: Mutex::new(HashMap::new()),
        })
    }

    /// Resumes Connect when it was on. With Connect off this is one `stat`.
    pub(crate) fn start(self: &Arc<Self>) {
        peer::install(self);
        let owner = Arc::clone(self);
        self.runtime.spawn(async move {
            let _action = owner.actions.lock().await;
            if let Err(error) = owner.resume().await {
                owner.failed(error);
            }
        });
    }

    async fn resume(self: &Arc<Self>) -> Result<(), String> {
        let Some((store, row)) = Store::open(&self.store_path()).await.map_err(error)? else {
            return Ok(());
        };
        let Some(key) = self.profile_key(&row.profile).await? else {
            // A keyring that cannot be read now keeps the key for a later start.
            if !self.secrets.is_persistent() {
                return Err(KEYRING_UNAVAILABLE.into());
            }
            store.delete().await.map_err(error)?;
            return Err(
                "The Connect profile key is unavailable. Pair with a trusted device again".into(),
            );
        };
        self.begin(store, row.profile, key, row.folder).await
    }

    fn store_path(&self) -> PathBuf {
        self.directory.join("connect.sqlite")
    }

    fn session(&self) -> Option<Arc<Session>> {
        self.session
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    fn active(&self) -> Result<Arc<Session>, String> {
        self.session()
            .ok_or_else(|| localization::tr("Connect is not enabled"))
    }

    fn device_config(&self) -> DeviceConfig {
        self.config
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    fn save(&self, edit: impl FnOnce(&mut DeviceConfig)) -> Result<(), String> {
        let mut config = self.device_config();
        edit(&mut config);
        std::fs::create_dir_all(&self.directory).map_err(error)?;
        crate::settings::write_private(
            &self.directory.join("device.json"),
            &serde_json::to_vec(&config).map_err(error)?,
        )?;
        self.status.send_modify(|status| {
            status.settings = settings_of(
                &config,
                status.settings.profile.clone(),
                status.settings.folder.clone(),
            );
        });
        *self.config.lock().unwrap_or_else(|p| p.into_inner()) = config;
        Ok(())
    }

    fn failed(&self, error: String) {
        tracing::warn!(%error, "Rufin Connect failed");
        self.status.send_modify(|status| {
            status.error = Some(error);
            status.connecting = false;
            status.receiving_collection = false;
            status.collection_progress = None;
        });
    }

    fn identity_reference(&self) -> Result<String, String> {
        if let Some(reference) = self.device_config().identity_ref {
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

    async fn delete_secret(&self, key: SecretKey) -> Result<(), String> {
        let secrets = Arc::clone(&self.secrets);
        tokio::task::spawn_blocking(move || secrets.delete_secret(&key).map_err(error))
            .await
            .map_err(error)?
    }

    async fn credentials(&self) -> Result<Credentials, String> {
        let key = identity_key(&self.identity_reference()?);
        match self.secret(key.clone()).await? {
            Some(value) => serde_json::from_str(&value).map_err(error),
            None => {
                let credentials = Credentials::generate();
                self.save_secret(key, serde_json::to_string(&credentials).map_err(error)?)
                    .await?;
                // An identity kept only for this run would make the device another one later.
                if !self.secrets.is_persistent() {
                    return Err(KEYRING_UNAVAILABLE.into());
                }
                Ok(credentials)
            }
        }
    }

    async fn profile_key(&self, profile: &str) -> Result<Option<age::x25519::Identity>, String> {
        self.secret(profile_key(&self.identity_reference()?, profile))
            .await?
            .map(|key| key.parse().map_err(error))
            .transpose()
    }

    /// The network, started for pairing and discovery when Connect is off. Boxed: its event
    /// loop leads back here.
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
            let config = self.device_config();
            let (network, mut events) = ConnectNetwork::spawn(
                NetworkConfig {
                    name: config.name,
                    nearby: config.nearby,
                    relay: config.relay,
                    public_relay: config.public_relay,
                },
                credentials,
            )
            .await
            .map_err(error)?;
            let identity = network.identity();
            let invitation = network.invitation().await.map_err(error)?;
            self.status.send_modify(|status| {
                status.identity = Some(identity);
                status.invitation = Some(invitation);
            });
            let weak = Arc::downgrade(self);
            let events_of = Arc::downgrade(&network);
            self.runtime.spawn(async move {
                while let Some(event) = events.recv().await {
                    let (Some(owner), Some(network)) = (weak.upgrade(), events_of.upgrade()) else {
                        return;
                    };
                    if !owner
                        .network
                        .lock()
                        .await
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, &network))
                    {
                        return;
                    }
                    owner.event(&network, event).await;
                }
            });
            *current = Some(Arc::clone(&network));
            Ok(network)
        })
    }

    /// Connect is on: serve the records, follow the Store and settings, meet the devices.
    async fn begin(
        self: &Arc<Self>,
        store: Store,
        profile: String,
        key: age::x25519::Identity,
        folder: Option<String>,
    ) -> Result<(), String> {
        let network = self.network().await?;
        let records = Records::new(self, store, network.identity(), key);
        records.load_devices().await?;
        records.load_sessions().await?;
        network
            .serve(Some(rufin_connect::Profile {
                id: profile.clone(),
                key: records.key_text(),
                store: records.store.clone(),
                values: records.clone(),
            }))
            .await
            .map_err(error)?;
        network.set_devices(records.trusted());
        let folder = folder
            .map(|folder| serde_json::from_str(&folder).map_err(error))
            .transpose()?;
        let session = Arc::new(Session {
            profile: profile.clone(),
            records,
            stop: CancellationToken::new(),
            folder: folder::State::default(),
        });
        *self.session.lock().unwrap_or_else(|p| p.into_inner()) = Some(Arc::clone(&session));
        let config = self.device_config();
        self.status.send_modify(|status| {
            status.settings = settings_of(&config, Some(profile), folder);
            status.error = None;
            status.profile_status = localization::tr("Profile up to date");
        });
        self.publish_devices();
        self.run(&session);
        Ok(())
    }

    /// The tasks of a session; each ends when the session stops.
    fn run(self: &Arc<Self>, session: &Arc<Session>) {
        let stop = session.stop.clone();
        let owner = Arc::downgrade(self);
        let follow = crate::features::follow(
            &self.runtime,
            &self.catalog,
            Arc::downgrade(&session.records),
            move |records, changes| {
                let owner = owner.clone();
                async move {
                    match records.pass(changes.as_deref()).await {
                        Ok(Some(sequence)) => {
                            if let Some(owner) = owner.upgrade() {
                                owner.changed(sequence).await;
                            }
                        }
                        Ok(None) => {}
                        Err(error) => tracing::warn!(%error, "Connect could not record changes"),
                    }
                }
            },
        );
        self.runtime.spawn({
            let stop = stop.clone();
            async move {
                stop.cancelled().await;
                follow.abort();
            }
        });
        let weak = Arc::downgrade(self);
        let mut changes = self.settings_apply.changes();
        let mut shared = records::shared_settings(&self.settings.load());
        self.runtime
            .spawn(stop.clone().run_until_cancelled_owned(async move {
                while changes.changed().await.is_ok() {
                    let Some(owner) = weak.upgrade() else { return };
                    let now = records::shared_settings(&owner.settings.load());
                    if now.is_some() && now == shared {
                        continue;
                    }
                    shared = now;
                    owner.settings_changed().await;
                }
            }));
        let weak = Arc::downgrade(self);
        self.runtime
            .spawn(stop.clone().run_until_cancelled_owned(async move {
                loop {
                    let Some(owner) = weak.upgrade() else { return };
                    owner.refresh().await;
                    drop(owner);
                    tokio::time::sleep(PRESENCE).await;
                }
            }));
        folder::watch(self, session);
        self.follow_sessions(session);
    }

    /// Records the settings records and this device's record after a settings change.
    async fn settings_changed(self: &Arc<Self>) {
        let Some(session) = self.session() else {
            return;
        };
        match session.records.settings_pass().await {
            Ok(Some(sequence)) => self.changed(sequence).await,
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, "Connect could not record settings"),
        }
    }

    /// This device's records reached `sequence`: tell the devices and the sync folder.
    async fn changed(self: &Arc<Self>, sequence: i64) {
        if let Some(network) = self.network.lock().await.clone() {
            self.runtime
                .spawn(async move { network.push(sequence).await });
        }
        if let Some(session) = self.session() {
            let owner = Arc::clone(self);
            self.runtime
                .spawn(async move { folder::write(&owner, &session).await });
        }
    }

    /// Presence of each trusted device; reads the sync folder.
    async fn refresh(self: &Arc<Self>) {
        let Some(session) = self.session() else {
            return;
        };
        if self.network.lock().await.is_none() {
            return;
        }
        let peers = session.records.peers();
        futures_util::future::join_all(peers.iter().map(|peer| self.meet(peer))).await;
        folder::sync(self, &session).await;
    }

    /// Presence of a trusted device: whether and how it is reachable, then the devices it
    /// reaches on routes this one did not know, and the records it holds that this one lacks.
    fn meet<'a>(
        self: &'a Arc<Self>,
        peer: &'a str,
    ) -> futures_util::future::BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let network = self.network().await?;
            let presence = serde_json::to_vec(&Request::Presence).map_err(error)?;
            let probe = {
                let mut probes = self.probes.lock().unwrap();
                let latest = probes.entry(peer.to_owned()).or_default();
                *latest += 1;
                *latest
            };
            let answer = network
                .test_connection(peer, presence)
                .await
                .map_err(error)
                .and_then(|(route, bytes)| {
                    serde_json::from_slice::<Presence>(&bytes)
                        .map(|presence| (route, presence))
                        .map_err(error)
                });
            if self.probes.lock().unwrap().get(peer) == Some(&probe) {
                self.update_device(peer, |device| match &answer {
                    Ok((route, _)) => {
                        device.reachable = true;
                        device.connection = connection_label(route);
                    }
                    Err(_) => {
                        device.reachable = false;
                        device.connection = localization::tr("Waiting for device");
                    }
                });
            }
            let (_, presence) = answer?;
            for learned in network.learn(presence.addresses).await {
                self.met(learned);
            }
            if let Some(session) = self.session() {
                self.pull_when_behind(&session, peer, presence.sequence)
                    .await;
            }
            Ok(())
        })
    }

    fn met(self: &Arc<Self>, peer: String) {
        let owner = Arc::clone(self);
        self.runtime.spawn(async move {
            let _ = owner.meet(&peer).await;
        });
    }

    async fn pull_when_behind(self: &Arc<Self>, session: &Arc<Session>, peer: &str, sequence: i64) {
        let behind = match session.records.pulled(peer).await {
            Ok(pulled) => sequence > pulled,
            Err(error) => {
                tracing::warn!(%error, "Connect could not read its cursor");
                return;
            }
        };
        if behind && let Err(error) = self.pull(session, peer).await {
            tracing::warn!(%error, peer, "Connect could not pull records");
        }
    }

    /// Applies the records a device holds after this device's cursor, in one stream.
    async fn pull(self: &Arc<Self>, session: &Arc<Session>, peer: &str) -> Result<(), String> {
        let network = self.network().await?;
        let applied = self
            .receive_from(&network, &session.records, &session.profile, peer, false)
            .await?;
        if applied > 0 {
            self.relay(session).await;
        }
        Ok(())
    }

    /// Records from another device applied here: tell the other devices, so rows travel on
    /// through any device that is online. Rows a device already holds apply nowhere, so this
    /// ends where they came from.
    async fn relay(&self, session: &Session) {
        let Some(network) = self.network.lock().await.clone() else {
            return;
        };
        match session.records.store.sequence().await {
            Ok(sequence) => {
                self.runtime
                    .spawn(async move { network.push(sequence).await });
            }
            Err(error) => tracing::warn!(%error, "Connect could not read its sequence"),
        }
    }

    async fn receive_from(
        &self,
        network: &ConnectNetwork,
        records: &Arc<Records>,
        profile: &str,
        peer: &str,
        joining: bool,
    ) -> Result<u64, String> {
        let after = records.pulled(peer).await?;
        let mut pull = network.pull(peer, profile, after).await.map_err(error)?;
        let total = pull.total;
        let mut receiving = Receiving::default();
        let mut applied = 0;
        // A page is applied once the next frame arrived, so the last one stores with the end.
        let mut page = Vec::new();
        loop {
            let (rows, end) = match pull.next().await.map_err(error)? {
                Frame::Page(rows) if page.is_empty() => {
                    page = rows;
                    continue;
                }
                Frame::Page(rows) => (std::mem::replace(&mut page, rows), None),
                Frame::End { sequence } => (std::mem::take(&mut page), Some(sequence)),
            };
            applied += records
                .receive(peer, rows, &mut receiving, end, |device, sequence| {
                    Cursor::Pulled { device, sequence }
                })
                .await?;
            if joining {
                self.status.send_modify(|status| {
                    status.collection_progress = Some(CollectionProgress {
                        completed: applied,
                        total: Some(total),
                    });
                });
            }
            if end.is_some() {
                return Ok(applied);
            }
        }
    }

    /// The joining device becomes a newly added device of the host's profile: its data is
    /// cleared, then it applies every record the host holds. Connect is on once that ends.
    async fn join(
        self: &Arc<Self>,
        network: Arc<ConnectNetwork>,
        profile: String,
        key: String,
        host: String,
        name: String,
    ) -> Result<(), String> {
        let _action = self.actions.lock().await;
        let identity: age::x25519::Identity = key.parse().map_err(error)?;
        self.status.send_modify(|status| {
            status.pairing = None;
            status.connecting = true;
            status.receiving_collection = true;
            status.error = None;
            status.profile_status = localization::tr("Receiving profile");
            status.collection_progress = Some(CollectionProgress {
                completed: 0,
                total: None,
            });
        });
        self.end(Some(&network), false).await?;
        let reference = self.identity_reference()?;
        self.save_secret(profile_key(&reference, &profile), key)
            .await?;
        let result = async {
            self.clear_data().await?;
            let store = Store::create(&self.store_path(), &network.identity())
                .await
                .map_err(error)?;
            let records = Records::new(self, store, network.identity(), identity.clone());
            // Until the host's own record arrives.
            records.add_device(
                &host,
                DeviceValue {
                    name,
                    trusted: true,
                    paired: 0,
                },
            );
            network.set_devices(records.trusted());
            let pulled = self
                .receive_from(&network, &records, &profile, &host, true)
                .await;
            if let Err(error) = pulled {
                records.store.clone().delete().await.map_err(self::error)?;
                return Err(error);
            }
            records.store.turn_on(&profile).await.map_err(error)?;
            Ok(records.store.clone())
        }
        .await;
        let store = match result {
            Ok(store) => store,
            Err(error) => {
                if let Err(error) = self.delete_secret(profile_key(&reference, &profile)).await {
                    tracing::warn!(%error, "Could not remove the Connect profile key");
                }
                return Err(error);
            }
        };
        self.status.send_modify(|status| {
            status.connecting = false;
            status.receiving_collection = false;
            status.collection_progress = None;
            status.completed_pairings += 1;
        });
        self.begin(store, profile, identity, None).await
    }

    /// What the "Clear all data" reset removes, done in place: every source and its library,
    /// Rufin playlists, favorites, activity, the queue, every cache, this device's secrets
    /// and the settings. Connect's own keys stay; the joined profile holds them.
    async fn clear_data(&self) -> Result<(), String> {
        let stored = self.settings.load();
        self.source
            .restore_user_state(true, true, || async {
                let playback = Arc::clone(&self.playback);
                tokio::task::spawn_blocking(move || playback.shutdown())
                    .await
                    .map_err(error)?;
                let result = async {
                    // Playlists go first, so forgetting a source finds no entries holding its
                    // tracks and deletes them all.
                    self.catalog
                        .clear_state(library::StateGroups {
                            playlists: true,
                            favorites: true,
                            local_imports: true,
                            activity: true,
                            queue: true,
                        })
                        .await
                        .map_err(error)?;
                    for source in &stored.sources.configured {
                        self.source
                            .forget_source(source.configuration.source_id.clone())
                            .await
                            .map_err(error)?;
                    }
                    for kind in crate::runtime::CacheKind::ALL {
                        kind.clear(&self.catalog, self.source.downloads(), &self.lyrics)
                            .await?;
                    }
                    let secrets = Arc::clone(&self.secrets);
                    let backup =
                        crate::settings::backup_password_store(&stored, self.settings.config_dir());
                    let keys = crate::settings::device_secrets(&stored);
                    let settings = self.settings_apply.clone();
                    tokio::task::spawn_blocking(move || {
                        for key in keys {
                            secrets.delete_secret(&key).map_err(error)?;
                        }
                        backup
                            .delete_secret(&crate::settings::backup_password_key())
                            .map_err(error)?;
                        settings.restore(
                            |current| {
                                let mut cleared = crate::settings::StoredSettings::default();
                                cleared.secret_scope_id =
                                    std::mem::take(&mut current.secret_scope_id);
                                cleared.jellyfin_device_id =
                                    crate::settings::random_identity("rufin-")?;
                                cleared.ui.application.secret_storage_mode =
                                    current.ui.application.secret_storage_mode;
                                *current = cleared;
                                Ok(())
                            },
                            true,
                        )
                    })
                    .await
                    .map_err(error)?
                }
                .await;
                self.playback.start();
                result
            })
            .await
    }

    /// Turns Connect off and stops the network. A removed device does not announce its
    /// departure.
    async fn leave(self: &Arc<Self>, removed: bool) -> Result<(), String> {
        let network = self.network.lock().await.take();
        let ended = self.end(network.as_deref(), removed).await;
        let closed = match &network {
            Some(network) => network.shutdown().await.map_err(error),
            None => Ok(()),
        };
        let config = self.device_config();
        self.status.send_modify(|status| {
            status.settings = settings_of(&config, None, None);
            status.devices.clear();
            status.pairing = None;
            status.connecting = false;
            status.receiving_collection = false;
            status.collection_progress = None;
            status.continuation = None;
            status.invitation = None;
            status.identity = None;
            status.profile_status = localization::tr("Connect is not enabled");
        });
        ended.and(closed)
    }

    /// Ends the session: the departure reaches the devices online, then `connect.sqlite` and
    /// the profile key go.
    async fn end(&self, network: Option<&ConnectNetwork>, removed: bool) -> Result<(), String> {
        let Some(session) = self
            .session
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        else {
            return Ok(());
        };
        session.stop.cancel();
        if !removed {
            match session.records.leave().await {
                Ok(Some(sequence)) => {
                    if let Some(network) = network {
                        network.push(sequence).await;
                    }
                }
                Ok(None) => {}
                Err(error) => tracing::warn!(%error, "Could not announce Connect departure"),
            }
        }
        if let Some(network) = network {
            network.serve(None).await.map_err(error)?;
        }
        session
            .records
            .store
            .clone()
            .delete()
            .await
            .map_err(error)?;
        self.delete_secret(profile_key(&self.identity_reference()?, &session.profile))
            .await
    }

    /// Turns Connect off, as the secret storage change requires.
    pub(crate) async fn turn_off(self: &Arc<Self>) -> Result<(), String> {
        let _action = self.actions.lock().await;
        self.leave(false).await
    }

    /// The device identity, for "Clear all data".
    pub(crate) fn secret_keys(&self) -> Vec<SecretKey> {
        self.device_config()
            .identity_ref
            .map(|reference| identity_key(&reference))
            .into_iter()
            .collect()
    }

    /// A storage reset clears the data Connect's records describe, so the device leaves the
    /// profile first, as turning Connect off does.
    pub(crate) async fn close_storage(self: &Arc<Self>) -> Result<(), String> {
        self.turn_off().await
    }

    /// Creates a profile on this device. Connect is on once its row commits.
    async fn create(self: &Arc<Self>) -> Result<(), String> {
        if self.session().is_some() {
            return Err("This device already has a Connect profile".into());
        }
        let network = self.network().await?;
        let profile = rufin_connect::store::new_profile();
        let key = age::x25519::Identity::generate();
        self.save_secret(
            profile_key(&self.identity_reference()?, &profile),
            key.to_string().expose_secret().to_owned(),
        )
        .await?;
        let store = Store::create(&self.store_path(), &network.identity())
            .await
            .map_err(error)?;
        store.turn_on(&profile).await.map_err(error)?;
        self.begin(store, profile, key, None).await
    }

    fn update_device(&self, peer: &str, edit: impl FnOnce(&mut Device)) {
        self.status.send_if_modified(|status| {
            let Some(device) = status
                .devices
                .iter_mut()
                .find(|device| device.id == peer && device.enrolled)
            else {
                return false;
            };
            let previous = device.clone();
            edit(device);
            *device != previous
        });
    }

    /// The device list: trusted devices from the device records, then the nearby ones.
    fn publish_devices(&self) {
        let trusted = self
            .session()
            .map(|session| session.records.devices())
            .unwrap_or_default();
        self.status.send_if_modified(|status| {
            let mut devices = trusted
                .iter()
                .map(|(id, name)| {
                    status
                        .devices
                        .iter()
                        .find(|device| &device.id == id && device.enrolled)
                        .cloned()
                        .map(|device| Device {
                            name: name.clone(),
                            ..device
                        })
                        .unwrap_or_else(|| Device {
                            id: id.clone(),
                            name: name.clone(),
                            reachable: false,
                            enrolled: true,
                            connection: String::new(),
                            continuable: false,
                        })
                })
                .collect::<Vec<_>>();
            devices.extend(
                status
                    .devices
                    .iter()
                    .filter(|device| !device.enrolled && !trusted.contains_key(&device.id))
                    .cloned(),
            );
            if devices == status.devices {
                return false;
            }
            status.devices = devices;
            true
        });
        self.offer();
    }

    /// Device records changed: trust follows them, and a record that removed this device
    /// turns Connect off.
    fn devices_changed(self: &Arc<Self>, removed: bool) {
        let owner = Arc::clone(self);
        self.runtime.spawn(async move {
            if removed {
                return owner.removed().await;
            }
            if let (Some(session), Some(network)) =
                (owner.session(), owner.network.lock().await.clone())
            {
                network.set_devices(session.records.trusted());
            }
            owner.publish_devices();
        });
    }

    async fn removed(self: &Arc<Self>) {
        let _action = self.actions.lock().await;
        if self.session().is_none() {
            return;
        }
        if let Err(error) = self.leave(true).await {
            tracing::warn!(%error, "Could not turn Connect off");
        }
        self.failed(error(REMOVED_FROM_PROFILE));
    }

    /// The relay record changed the shared relay choice.
    async fn relay_changed(self: &Arc<Self>, relay: Option<String>, public: bool) {
        let _action = self.actions.lock().await;
        let result = async {
            self.save(|config| {
                config.relay = relay;
                config.public_relay = public;
            })?;
            self.configure_network().await
        }
        .await;
        if let Err(error) = result {
            self.failed(error);
        }
    }

    /// Iroh fixes its transports at bind time, so a changed network setting starts it again.
    async fn configure_network(self: &Arc<Self>) -> Result<(), String> {
        let config = self.device_config();
        let mut current = self.network.lock().await;
        let Some(network) = current.as_ref() else {
            return Ok(());
        };
        if network
            .matches_configuration(config.nearby, config.relay.as_deref(), config.public_relay)
            .map_err(error)?
        {
            return Ok(());
        }
        current.take().unwrap().shutdown().await.map_err(error)?;
        drop(current);
        let network = self.network().await?;
        if let Some(session) = self.session() {
            network
                .serve(Some(rufin_connect::Profile {
                    id: session.profile.clone(),
                    key: session.records.key_text(),
                    store: session.records.store.clone(),
                    values: session.records.clone(),
                }))
                .await
                .map_err(error)?;
            network.set_devices(session.records.trusted());
        }
        Ok(())
    }

    async fn request(&self, peer: &str, request: Request) -> Result<serde_json::Value, String> {
        let network = self
            .network
            .lock()
            .await
            .clone()
            .ok_or_else(|| localization::tr("Connect is not enabled"))?;
        let answer = network
            .request(peer, serde_json::to_vec(&request).map_err(error)?)
            .await
            .map_err(error)?;
        serde_json::from_slice(&answer).map_err(error)
    }

    async fn answer(&self, body: &[u8]) -> Result<Vec<u8>, String> {
        let answer = match serde_json::from_slice(body).map_err(error)? {
            Request::Presence => {
                let session = self.active()?;
                let playing = self
                    .playback
                    .current_projection()
                    .is_some_and(|projection| {
                        projection.view.transport.effective_state()
                            == playback::TransportStatus::Playing
                    });
                let addresses = self
                    .network
                    .lock()
                    .await
                    .as_ref()
                    .map(|network| network.addresses())
                    .unwrap_or_default();
                serde_json::to_value(Presence {
                    sequence: session.records.store.sequence().await.map_err(error)?,
                    playing,
                    addresses,
                })
                .map_err(error)?
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
            Request::Cast(cast) => {
                self.receive_cast(cast).await?;
                serde_json::Value::Null
            }
            Request::Queue => serde_json::to_value(self.answer_queue().await?).map_err(error)?,
            Request::QueuePage {
                offset,
                limit,
                filter,
            } => serde_json::to_value(self.answer_queue_page(offset, limit, filter).await?)
                .map_err(error)?,
        };
        serde_json::to_vec(&answer).map_err(error)
    }

    async fn event(self: &Arc<Self>, network: &Arc<ConnectNetwork>, event: NetworkEvent) {
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
            NetworkEvent::PairingFailed { session, error } => {
                let matches = self.status.borrow().pairing.as_ref().is_none_or(|pairing| {
                    session
                        .as_ref()
                        .is_none_or(|session| &pairing.session == session)
                });
                if matches {
                    self.status.send_modify(|status| status.pairing = None);
                    self.failed(self::error(error));
                }
            }
            NetworkEvent::Paired {
                profile,
                key,
                peer,
                name,
                ..
            } => {
                let owner = Arc::clone(self);
                let network = Arc::clone(network);
                self.runtime.spawn(async move {
                    if let Err(error) = owner.join(network, profile, key, peer, name).await {
                        owner.failed(error);
                    }
                });
            }
            NetworkEvent::MemberAdded { peer, name } => {
                let Some(session) = self.session() else {
                    return;
                };
                let paired = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |elapsed| elapsed.as_millis() as i64);
                session.records.add_device(
                    &peer,
                    DeviceValue {
                        name,
                        trusted: true,
                        paired,
                    },
                );
                network.set_devices(session.records.trusted());
                self.status.send_modify(|status| {
                    status.completed_pairings += 1;
                    status.error = None;
                    status.pairing = None;
                    status.connecting = false;
                });
                self.publish_devices();
                let owner = Arc::clone(self);
                self.runtime.spawn(async move {
                    match session.records.write_device(&peer).await {
                        Ok(Some(sequence)) => owner.changed(sequence).await,
                        Ok(None) => {}
                        Err(error) => owner.failed(error),
                    }
                });
            }
            NetworkEvent::Request { body, reply, .. } => {
                let owner = Arc::clone(self);
                self.runtime.spawn(async move {
                    let _ = reply.send(owner.answer(&body).await);
                });
            }
            NetworkEvent::StreamRequest { body, reply, .. } => {
                let owner = Arc::clone(self);
                self.runtime.spawn(async move {
                    let _ = reply.send(owner.answer_stream(&body).await);
                });
            }
            NetworkEvent::Changed {
                peer,
                sequence,
                pulled,
            } => {
                let Some(session) = self.session() else {
                    return;
                };
                let owner = Arc::clone(self);
                self.runtime.spawn(async move {
                    owner.pull_when_behind(&session, &peer, sequence).await;
                    let _ = pulled.send(());
                });
            }
            NetworkEvent::Reachable { peer, contacted } => {
                let mut unreachable = false;
                self.update_device(&peer, |device| {
                    unreachable = !device.reachable;
                    device.reachable = true;
                    if device.connection == localization::tr("Waiting for device") {
                        device.connection.clear();
                    }
                });
                // A device that was away and contacts this one may hold records it could not
                // push while away.
                if contacted && unreachable {
                    self.met(peer);
                }
            }
            NetworkEvent::Removed { .. } => {
                let owner = Arc::clone(self);
                self.runtime.spawn(async move { owner.removed().await });
            }
            NetworkEvent::Discovered { peer, name } => {
                let mut enrolled = false;
                self.status.send_if_modified(|status| {
                    if let Some(device) = status.devices.iter_mut().find(|device| device.id == peer)
                    {
                        enrolled = device.enrolled;
                        if device.enrolled || device.name == name {
                            return false;
                        }
                        device.name = name;
                    } else {
                        status.devices.push(Device {
                            id: peer.clone(),
                            name,
                            reachable: true,
                            enrolled: false,
                            connection: String::new(),
                            continuable: false,
                        });
                    }
                    true
                });
                // A device found nearby, newly or at new addresses, is reachable again there.
                if enrolled {
                    self.met(peer);
                }
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
            NetworkEvent::Error(message) => self.failed(message),
        }
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
                // Playback control and refresh change no device setup.
                let _action = if matches!(
                    action,
                    Action::Control { .. } | Action::Refresh | Action::Continue { .. }
                ) {
                    None
                } else {
                    Some(owner.actions.lock().await)
                };
                if matches!(action, Action::Retry) {
                    owner.status.send_modify(|status| status.error = None);
                }
                let preserve_error = matches!(action, Action::CancelPairing);
                let result = owner.act(action).await;
                match &result {
                    Ok(()) if !preserve_error => {
                        owner.status.send_modify(|status| status.error = None)
                    }
                    Err(ActionError::Failed(message)) => owner.failed(message.clone()),
                    _ => {}
                }
                result.map(|()| owner.status())
            })
            .await
            .map_err(|error| ActionError::Failed(error.to_string()))?
    }
}

impl ConnectOwner {
    async fn act(self: &Arc<Self>, action: Action) -> Result<(), ActionError> {
        match action {
            Action::Enable { enabled: false } | Action::Leave => self.leave(false).await?,
            Action::Enable { enabled: true } => {
                if self.session().is_none() {
                    self.create().await?;
                }
            }
            Action::Create => self.create().await?,
            Action::Join { invitation, .. } => {
                self.status.send_modify(|status| {
                    status.connecting = true;
                    status.receiving_collection = false;
                    status.collection_progress = None;
                    status.error = None;
                    status.profile_status = localization::tr("Connecting devices");
                });
                if let Err(error) = self.network().await?.begin_pairing(&invitation).await {
                    self.status.send_modify(|status| status.connecting = false);
                    return Err(self::error(error).into());
                }
            }
            Action::Invite { peer } => {
                if self.session().is_none() {
                    self.create().await?;
                }
                self.status.send_modify(|status| {
                    status.connecting = true;
                    status.profile_status = localization::tr("Connecting devices");
                });
                if let Err(error) = self.network().await?.invite(&peer).await {
                    self.status.send_modify(|status| status.connecting = false);
                    return Err(self::error(error).into());
                }
            }
            Action::Pair {
                session,
                approve,
                replace,
            } => {
                let pairing = self.status.borrow().pairing.clone();
                if approve
                    && pairing
                        .as_ref()
                        .is_some_and(|pairing| pairing.session == session && pairing.approved)
                {
                    return Ok(());
                }
                if approve
                    && !replace
                    && pairing
                        .as_ref()
                        .is_some_and(|pairing| pairing.session == session && pairing.joining)
                {
                    return Err(Confirmation::ReplaceProfile.required());
                }
                self.network()
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
                    if !approve {
                        status.pairing = None;
                        status.connecting = false;
                    }
                });
            }
            Action::CancelPairing => {
                if let Some(network) = self.network.lock().await.clone() {
                    network.cancel_pairing().await;
                }
                self.status.send_modify(|status| {
                    status.pairing = None;
                    status.connecting = false;
                    status.receiving_collection = false;
                    status.collection_progress = None;
                });
            }
            Action::Rename { name } => {
                self.save(|config| config.name = name.clone())?;
                if let Some(network) = self.network.lock().await.as_ref() {
                    network.rename(name).await;
                }
                self.settings_changed().await;
            }
            Action::Remove { peer } => {
                let session = self.active()?;
                if let Some(sequence) = session.records.remove_device(&peer).await? {
                    // The removed device pulls its record before its access goes.
                    if let Some(network) = self.network.lock().await.clone() {
                        network.push(sequence).await;
                    }
                    self.devices_changed(false);
                    let owner = Arc::clone(self);
                    self.runtime
                        .spawn(async move { folder::write(&owner, &session).await });
                }
            }
            Action::Refresh => self.refresh().await,
            Action::Retry => {
                if self.session().is_none() {
                    self.resume().await?;
                }
                self.refresh().await;
            }
            // Starting the network starts nearby discovery, which then keeps running.
            Action::Discover => {
                self.network().await?;
            }
            Action::TestConnection { peer } => self.meet(&peer).await?,
            Action::Network {
                nearby,
                relay,
                public_relay,
            } => {
                if let Some(relay) = &relay {
                    reqwest::Url::parse(relay).map_err(error)?;
                }
                self.save(|config| {
                    config.nearby = nearby;
                    config.relay = relay;
                    config.public_relay = public_relay;
                })?;
                self.configure_network().await?;
                self.settings_changed().await;
            }
            Action::Folder { folder } => {
                let session = self.active()?;
                session
                    .records
                    .store
                    .set_folder(
                        folder
                            .as_ref()
                            .map(serde_json::to_string)
                            .transpose()
                            .map_err(error)?
                            .as_deref(),
                    )
                    .await
                    .map_err(error)?;
                self.status
                    .send_modify(|status| status.settings.folder = folder.clone());
                folder::watch(self, &session);
                folder::sync(self, &session).await;
            }
            Action::Continue { peer } => {
                if !self.continue_from(&peer, true).await? {
                    return Err("There is no playback to continue".into());
                }
            }
            Action::Control { peer, command } => {
                self.request(&peer, Request::Control { command }).await?;
            }
        }
        Ok(())
    }
}

fn settings_of(
    config: &DeviceConfig,
    profile: Option<String>,
    folder: Option<SyncFolder>,
) -> ConnectSettings {
    ConnectSettings {
        enabled: profile.is_some(),
        name: config.name.clone(),
        profile,
        nearby: config.nearby,
        relay: config.relay.clone(),
        public_relay: config.public_relay,
        folder,
    }
}

fn identity_key(identity_ref: &str) -> SecretKey {
    SecretKey::namespaced("connect", identity_ref, "Rufin Connect device identity")
}

/// The profile key: secret fields leave the device sealed to it, and sync folder segments are
/// encrypted to it.
fn profile_key(identity_ref: &str, profile: &str) -> SecretKey {
    SecretKey::namespaced(
        "connect-profile",
        format!("{identity_ref}:{profile}"),
        "Rufin Connect profile key",
    )
}

fn error(error: impl std::fmt::Display) -> String {
    let message = error.to_string();
    match message.as_str() {
        INCOMPATIBLE => localization::tr(
            "Devices using Rufin Connect are no longer compatible. Please update Rufin.",
        ),
        REMOVED_FROM_PROFILE => localization::tr("This device was removed from the profile"),
        _ => message,
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
