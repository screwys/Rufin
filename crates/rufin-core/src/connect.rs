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
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const PRESENCE: Duration = Duration::from_secs(300);

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
    pub has_playback: bool,
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

/// A request on the RPC protocol.
#[derive(Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
enum Request {
    Presence,
    Control {
        command: Control,
    },
    Cast(session::Cast),
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
    name: String,
    sequence: i64,
    playing: bool,
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
    playback: Arc<PlaybackOwner>,
    runtime: tokio::runtime::Handle,
    config: Mutex<DeviceConfig>,
    status: tokio::sync::watch::Sender<Status>,
    network: tokio::sync::Mutex<Option<Arc<ConnectNetwork>>>,
    session: Mutex<Option<Arc<Session>>>,
    /// Turning on and off, joining and network changes run one at a time.
    actions: tokio::sync::Mutex<()>,
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
        let mut revision = self.settings_apply.revision_changes();
        self.runtime
            .spawn(stop.clone().run_until_cancelled_owned(async move {
                while revision.changed().await.is_ok() {
                    let Some(owner) = weak.upgrade() else { return };
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

    /// Presence to each trusted device; pulls from those that hold newer records; reads the
    /// sync folder.
    async fn refresh(self: &Arc<Self>) {
        let Some(session) = self.session() else {
            return;
        };
        let Some(network) = self.network.lock().await.clone() else {
            return;
        };
        let peers = session.records.peers();
        let presence = serde_json::to_vec(&Request::Presence).unwrap_or_default();
        let probes = peers.into_iter().map(|peer| {
            let network = Arc::clone(&network);
            let presence = presence.clone();
            async move {
                let answer = network.test_connection(&peer, presence).await;
                (peer, answer)
            }
        });
        for (peer, answer) in futures_util::future::join_all(probes).await {
            let answer = answer.map_err(error).and_then(|(route, bytes)| {
                serde_json::from_slice::<Presence>(&bytes)
                    .map(|presence| (route, presence))
                    .map_err(error)
            });
            self.update_device(&peer, |device| match &answer {
                Ok((route, presence)) => {
                    device.name = presence.name.clone();
                    device.has_playback = presence.playing;
                    device.reachable = true;
                    device.connection = connection_label(route);
                }
                Err(_) => {
                    device.reachable = false;
                    device.has_playback = false;
                    device.connection = localization::tr("Waiting for device");
                }
            });
            if let Ok((_, presence)) = answer {
                self.pull_when_behind(&session, &peer, presence.sequence)
                    .await;
            }
        }
        folder::sync(self, &session).await;
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
        self.receive_from(&network, &session.records, &session.profile, peer, false)
            .await
    }

    async fn receive_from(
        &self,
        network: &ConnectNetwork,
        records: &Arc<Records>,
        profile: &str,
        peer: &str,
        joining: bool,
    ) -> Result<(), String> {
        let after = records.pulled(peer).await?;
        let mut pull = network.pull(peer, profile, after).await.map_err(error)?;
        let total = pull.total;
        let mut receiving = Receiving::default();
        let mut applied = 0;
        loop {
            let (rows, end) = match pull.next().await.map_err(error)? {
                Frame::Page(rows) => (rows, None),
                Frame::End { sequence } => (Vec::new(), Some(sequence)),
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
                return Ok(());
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
            records.add_device(
                &host,
                DeviceValue {
                    name,
                    trusted: true,
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
    /// Rufin playlists, favorites, activity, the queue, analysis results and the settings.
    async fn clear_data(&self) -> Result<(), String> {
        let stored = self.settings.load();
        self.source
            .restore_user_state(true, true, || async {
                let playback = Arc::clone(&self.playback);
                tokio::task::spawn_blocking(move || playback.shutdown())
                    .await
                    .map_err(error)?;
                let result = async {
                    for source in &stored.sources.configured {
                        self.source
                            .forget_source(source.configuration.source_id.clone())
                            .await
                            .map_err(error)?;
                    }
                    let catalog = &self.catalog;
                    catalog
                        .clear_state(library::StateGroups {
                            playlists: true,
                            favorites: true,
                            local_imports: true,
                            activity: true,
                            queue: true,
                        })
                        .await
                        .map_err(error)?;
                    catalog.clear_loudness_cache().await.map_err(error)?;
                    catalog.clear_waveform_cache().await.map_err(error)?;
                    catalog.clear_fingerprint_cache().await.map_err(error)?;
                    let settings = self.settings_apply.clone();
                    tokio::task::spawn_blocking(move || {
                        settings.restore(
                            |current| {
                                let mut cleared = crate::settings::StoredSettings::default();
                                cleared.secret_scope_id =
                                    std::mem::take(&mut current.secret_scope_id);
                                cleared.jellyfin_device_id =
                                    std::mem::take(&mut current.jellyfin_device_id);
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

    /// The device identity and the profile key, for "Clear all data".
    pub(crate) fn secret_keys(&self) -> Vec<SecretKey> {
        let Some(reference) = self.device_config().identity_ref else {
            return Vec::new();
        };
        let mut keys = vec![identity_key(&reference)];
        keys.extend(
            self.session()
                .map(|session| profile_key(&reference, &session.profile)),
        );
        keys
    }

    pub(crate) async fn close_storage(&self) -> Result<(), String> {
        let _action = self.actions.lock().await;
        let session = self
            .session
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        if let Some(network) = self.network.lock().await.take() {
            network.shutdown().await.map_err(error)?;
        }
        if let Some(session) = session {
            session.stop.cancel();
            session.records.store.close().await;
        }
        Ok(())
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
                            has_playback: false,
                            enrolled: true,
                            connection: String::new(),
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
        self.failed(localization::tr(REMOVED_FROM_PROFILE));
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
                        projection.view.transport.state == playback::TransportStatus::Playing
                    });
                serde_json::to_value(Presence {
                    name: self.device_config().name,
                    sequence: session.records.store.sequence().await.map_err(error)?,
                    playing,
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
                session.records.add_device(
                    &peer,
                    DeviceValue {
                        name,
                        trusted: true,
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
                    if let Err(error) = session.records.store.restart_peer(&peer).await {
                        return owner.failed(self::error(error));
                    }
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
            NetworkEvent::Reachable { peer } => {
                self.update_device(&peer, |device| {
                    device.reachable = true;
                    if device.connection == localization::tr("Waiting for device") {
                        device.connection.clear();
                    }
                });
            }
            NetworkEvent::Removed { .. } => {
                let owner = Arc::clone(self);
                self.runtime.spawn(async move { owner.removed().await });
            }
            NetworkEvent::Discovered { peer, name } => {
                self.status.send_if_modified(|status| {
                    if let Some(device) = status.devices.iter_mut().find(|device| device.id == peer)
                    {
                        if device.enrolled || device.name == name {
                            return false;
                        }
                        device.name = name;
                    } else {
                        status.devices.push(Device {
                            id: peer,
                            name,
                            reachable: true,
                            has_playback: false,
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
            Action::Discover => self.network().await?.discover(),
            Action::TestConnection { peer } => {
                let result = self
                    .network()
                    .await?
                    .test_connection(
                        &peer,
                        serde_json::to_vec(&Request::Presence).map_err(error)?,
                    )
                    .await
                    .map(|(route, _)| route)
                    .map_err(error);
                self.update_device(&peer, |device| {
                    device.reachable = result.is_ok();
                    device.connection = match &result {
                        Ok(route) => connection_label(route),
                        Err(_) => localization::tr("Waiting for device"),
                    };
                    if result.is_err() {
                        device.has_playback = false;
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
        INCOMPATIBLE => localization::tr(INCOMPATIBLE),
        REMOVED_FROM_PROFILE => localization::tr(REMOVED_FROM_PROFILE),
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
