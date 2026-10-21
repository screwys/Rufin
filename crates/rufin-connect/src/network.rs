use library::profile::{DocumentVersion, ProfileStore, SyncStream};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, OnceLock, Weak},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use futures_util::StreamExt;
use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey, TransportAddr, Watcher,
    address_lookup::{
        AddressLookup, DnsAddressLookup, EndpointData, PkarrPublisher, PkarrResolver,
        memory::MemoryLookup,
    },
    endpoint::{Connection, ConnectionError, presets},
    protocol::{AcceptError, ProtocolHandler, Router},
};
use loro::{ExportMode, LoroDoc, ToJson};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sqlx::{Connection as _, Row, SqliteConnection};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, Notify, RwLock, mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

use crate::{
    media::PeerByteResponse,
    nearby::{Discovery, Nearby},
    pairing,
};

pub(crate) const RPC_PROTOCOL: &[u8] = b"rufin/connect/rpc/1";
pub(crate) const PAIR_PROTOCOL: &[u8] = b"rufin/connect/pair/2";
const INVITE_PROTOCOL: &[u8] = b"rufin/connect/invite/1";
const MEDIA_PROTOCOL: &[u8] = b"rufin/connect/media/1";
const SYNC_PROTOCOL: &[u8] = b"rufin/connect/sync/2";
const MAX_FRAME: usize = 16 * 1024 * 1024;
const SYNC_PAGE: usize = 64;
const PROFILE_CLOSED: &[u8] = b"profile closed";

static DNS_RESOLVER: OnceLock<fn() -> iroh::dns::DnsResolver> = OnceLock::new();

/// Supply the platform's DNS resolver before creating Connect endpoints.
pub fn install_dns_resolver(resolver: fn() -> iroh::dns::DnsResolver) {
    let _ = DNS_RESOLVER.set(resolver);
}

// Device names are advertised nearby, not in the public address directory.
#[derive(Debug)]
struct PublicAddressPublisher(PkarrPublisher);

impl AddressLookup for PublicAddressPublisher {
    fn publish(&self, data: &EndpointData) {
        let mut data = data.clone();
        data.set_user_data(None);
        self.0.publish(&data);
    }
}

fn relay_mode(address: Option<&str>, public: bool) -> Result<RelayMode> {
    let mode = match address {
        Some(address) => RelayMode::custom([address.parse::<iroh::RelayUrl>()?]),
        None if public => RelayMode::Default,
        None => RelayMode::Disabled,
    };
    if matches!(mode, RelayMode::Disabled) {
        return Ok(mode);
    }
    Ok(RelayMode::Custom(
        mode.relay_map()
            .relays::<Vec<_>>()
            .into_iter()
            .map(|config| {
                let mut config = (*config).clone();
                config.url = canonical_relay(config.url);
                config
            })
            .collect(),
    ))
}

pub(super) fn canonical_relay(relay: iroh::RelayUrl) -> iroh::RelayUrl {
    let mut url = (*relay).clone();
    if let Some(domain) = url.domain().map(str::to_owned) {
        url.set_host(Some(domain.trim_end_matches('.')))
            .expect("an existing domain is valid");
    }
    url.into()
}

fn canonical_address(mut address: EndpointAddr) -> EndpointAddr {
    address.addrs = address
        .addrs
        .into_iter()
        .map(|addr| match addr {
            TransportAddr::Relay(url) => TransportAddr::Relay(canonical_relay(url)),
            other => other,
        })
        .collect();
    address
}

pub(crate) fn peer_address(addresses: &MemoryLookup, peer: EndpointId) -> EndpointAddr {
    // An in-flight lookup can predate a newly learned address. Supply known routes directly.
    addresses
        .get_endpoint_info(peer)
        .map(EndpointAddr::from)
        .unwrap_or_else(|| peer.into())
}

// DNS permits a trailing dot, but Iroh keys relay connections by the URL.
// Normalize lookup results too, so one server cannot receive two connections
// claiming the same device identity.
#[derive(Debug)]
struct CanonicalRelays<T>(T);

impl<T: AddressLookup> AddressLookup for CanonicalRelays<T> {
    fn publish(&self, data: &EndpointData) {
        self.0.publish(data);
    }

    fn resolve(
        &self,
        id: EndpointId,
    ) -> Option<
        futures_util::stream::BoxStream<
            'static,
            std::result::Result<iroh::address_lookup::Item, iroh::address_lookup::Error>,
        >,
    > {
        Some(
            self.0
                .resolve(id)?
                .map(|result| {
                    result.map(|item| {
                        let mut info = item.endpoint_info().clone();
                        let relays = info
                            .data
                            .relay_urls()
                            .cloned()
                            .map(canonical_relay)
                            .collect::<Vec<_>>();
                        info.data.clear_relay_urls();
                        for relay in relays {
                            info.data.add_relay_url(relay);
                        }
                        iroh::address_lookup::Item::new(
                            info,
                            item.provenance(),
                            item.last_updated(),
                        )
                    })
                })
                .boxed(),
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NetworkConfig {
    pub database: PathBuf,
    pub name: String,
    pub nearby: bool,
    pub relay: Option<String>,
    #[serde(default)]
    pub public_relay: bool,
}

pub enum NetworkEvent {
    Invitation(String),
    Pairing {
        session: String,
        peer: String,
        name: String,
        emoji: [String; 7],
        joining: bool,
    },
    PairingVerified {
        session: String,
    },
    PairingFailed {
        session: Option<String>,
        error: String,
    },
    Paired {
        session: String,
        profile_id: String,
        peer: String,
        name: String,
        enrollment_data: Vec<u8>,
    },
    MemberAdded {
        peer: String,
        name: String,
    },
    MembershipChanged,
    Request {
        profile: String,
        peer: String,
        body: Vec<u8>,
        reply: oneshot::Sender<Result<Vec<u8>, String>>,
    },
    Settings {
        profile: String,
        peer: String,
    },
    StreamRequest {
        profile: String,
        peer: String,
        body: Vec<u8>,
        reply: oneshot::Sender<Result<PeerByteResponse, String>>,
    },
    Syncing {
        profile: String,
        peer: String,
        active: bool,
    },
    Reachable {
        profile: String,
        peer: String,
    },
    Unavailable {
        profile: String,
        peer: String,
    },
    Discovered {
        peer: String,
        name: String,
    },
    DiscoveryExpired {
        peer: String,
    },
    Error(String),
}

struct Profile {
    id: String,
    documents: RwLock<Option<Arc<ProfileStore>>>,
    stop: CancellationToken,
}

/// Iroh owns encrypted connections. Loro owns document and trusted-device merges.
/// The network database remembers addresses and device trust.
pub struct ConnectNetwork {
    pub(crate) endpoint: Endpoint,
    pub(crate) name: RwLock<String>,
    profile: RwLock<Option<Arc<Profile>>>,
    pub(crate) enrollment_data: RwLock<Vec<u8>>,
    pub(crate) approvals: Mutex<HashMap<String, watch::Sender<Option<bool>>>>,
    pub(crate) events: mpsc::Sender<NetworkEvent>,
    pub(crate) membership_changed: watch::Sender<()>,
    sync_requested: Arc<Notify>,
    database: Mutex<SqliteConnection>,
    addresses: MemoryLookup,
    transfers: Mutex<HashMap<String, Weak<Mutex<()>>>>,
    stop: CancellationToken,
    relay: RelayMode,
    nearby: bool,
    router: Mutex<Option<Router>>,
    pairing_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

#[derive(Serialize, Deserialize)]
struct SyncRequest {
    profile: String,
    after: i64,
    stream: SyncStream,
    roster: Vec<u8>,
    addresses: Vec<EndpointAddr>,
}

#[derive(Serialize, Deserialize)]
struct SyncPage {
    versions: Vec<DocumentVersion>,
    roster: Vec<u8>,
    addresses: Vec<EndpointAddr>,
}

#[derive(Serialize, Deserialize)]
struct SyncAcknowledgement {
    versions: Vec<DocumentVersion>,
    roster: Vec<u8>,
}

impl ConnectNetwork {
    pub async fn spawn(
        config: NetworkConfig,
        credentials: SecretKey,
    ) -> Result<(Arc<Self>, mpsc::Receiver<NetworkEvent>)> {
        if let Some(parent) = config.database.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut database = SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&config.database)
                .create_if_missing(true),
        )
        .await?;
        sqlx::raw_sql("PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS connect_addresses(peer TEXT PRIMARY KEY,address TEXT NOT NULL) STRICT;
            CREATE TABLE IF NOT EXISTS connect_rosters(profile TEXT PRIMARY KEY,snapshot BLOB NOT NULL) STRICT;")
            .execute(&mut database).await?;
        let addresses = MemoryLookup::new();
        for row in sqlx::query("SELECT address FROM connect_addresses")
            .fetch_all(&mut database)
            .await?
        {
            let addr: EndpointAddr = serde_json::from_str(row.get::<&str, _>(0))?;
            addresses.add_endpoint_info(canonical_address(addr));
        }
        let relay = relay_mode(config.relay.as_deref(), config.public_relay)?;
        let mut builder = Endpoint::builder(presets::Minimal)
            .secret_key(credentials.clone())
            .address_lookup(addresses.clone())
            .relay_mode(relay.clone());
        if let Some(resolver) = DNS_RESOLVER.get() {
            builder = builder.dns_resolver(resolver());
        }
        let mut discovery_error = None;
        let discoveries = if config.nearby {
            match Nearby::new(credentials.public(), addresses.clone()) {
                Ok((nearby, discoveries)) => {
                    builder = builder.address_lookup(nearby);
                    Some(discoveries)
                }
                Err(error) => {
                    discovery_error = Some(format!("Nearby discovery is unavailable: {error}"));
                    None
                }
            }
        } else {
            None
        };
        if let Ok(name) = config.name.parse() {
            builder = builder.user_data_for_address_lookup(name);
        }
        let endpoint = builder.bind().await?;
        let dns = endpoint.dns_resolver()?.clone();
        endpoint.address_lookup()?.add(CanonicalRelays(
            PkarrResolver::n0_dns()
                .dns_resolver(dns.clone())
                .build(endpoint.tls_config().clone()),
        ));
        endpoint.address_lookup()?.add(CanonicalRelays(
            DnsAddressLookup::n0_dns().dns_resolver(dns.clone()).build(),
        ));
        endpoint.address_lookup()?.add(PublicAddressPublisher(
            PkarrPublisher::n0_dns()
                .dns_resolver(dns)
                .build(credentials.clone(), endpoint.tls_config().clone()),
        ));
        let (events, receiver) = mpsc::channel(64);
        let network = Arc::new(Self {
            endpoint,
            name: RwLock::new(config.name),
            profile: RwLock::new(None),
            enrollment_data: RwLock::new(Vec::new()),
            approvals: Mutex::new(HashMap::new()),
            events,
            membership_changed: watch::channel(()).0,
            sync_requested: Arc::new(Notify::new()),
            database: Mutex::new(database),
            addresses,
            transfers: Mutex::new(HashMap::new()),
            stop: CancellationToken::new(),
            relay,
            nearby: config.nearby,
            router: Mutex::new(None),
            pairing_task: Mutex::new(None),
        });
        let weak = Arc::downgrade(&network);
        let router = Router::builder(network.endpoint.clone())
            .accept(RPC_PROTOCOL, RpcProtocol(weak.clone()))
            .accept(PAIR_PROTOCOL, pairing::PairProtocol(weak.clone(), false))
            .accept(INVITE_PROTOCOL, pairing::PairProtocol(weak.clone(), true))
            .accept(SYNC_PROTOCOL, SyncProtocol(weak.clone()))
            .accept(MEDIA_PROTOCOL, MediaProtocol(weak.clone()))
            .spawn();
        *network.router.lock().await = Some(router);
        if let Some(error) = discovery_error {
            let _ = network.events.send(NetworkEvent::Error(error)).await;
        }
        if let Some(mut discoveries) = discoveries {
            let weak = weak.clone();
            let stop = network.stop.clone();
            tokio::spawn(async move {
                loop {
                    let event = tokio::select! { _ = stop.cancelled() => break, event = discoveries.recv() => event };
                    let Some(event) = event else {
                        break;
                    };
                    let Some(network) = weak.upgrade() else { break };
                    if let Discovery::Expired(endpoint_id) = event {
                        let _ = network
                            .events
                            .send(NetworkEvent::DiscoveryExpired {
                                peer: endpoint_id.to_string(),
                            })
                            .await;
                        continue;
                    }
                    let Discovery::Found(info) = event else {
                        continue;
                    };
                    let name = info
                        .data
                        .user_data()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| info.endpoint_id.to_string());
                    let address: EndpointAddr = info.into();
                    if let Err(error) = network.remember_address(&address).await {
                        let _ = network
                            .events
                            .send(NetworkEvent::Error(error.to_string()))
                            .await;
                    }
                    let _ = network
                        .events
                        .send(NetworkEvent::Discovered {
                            peer: address.id.to_string(),
                            name,
                        })
                        .await;
                }
            });
        }
        let mut addresses = network.endpoint.watch_addr().stream();
        let stop = network.stop.clone();
        let address_owner = weak.clone();
        tokio::spawn(async move {
            loop {
                let address = tokio::select! { _ = stop.cancelled() => break, address = addresses.next() => address };
                let Some(address) = address else { break };
                let Some(network) = address_owner.upgrade() else {
                    break;
                };
                if let Ok(invitation) = serde_json::to_string(&address) {
                    let _ = network
                        .events
                        .send(NetworkEvent::Invitation(invitation))
                        .await;
                }
            }
        });
        let stop = network.stop.clone();
        let mut membership = network.membership_changed.subscribe();
        let sync_requested = network.sync_requested.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(300));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut syncing = HashSet::new();
            // A refresh requested during a sync runs once that peer finishes.
            let mut pending = HashSet::new();
            let mut tasks = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    _ = stop.cancelled() => break,
                    Some(completed) = tasks.join_next(), if !tasks.is_empty() => {
                        if let Ok(peer) = completed { syncing.remove(&peer); }
                    },
                    Ok(()) = membership.changed() => interval.reset_immediately(),
                    _ = sync_requested.notified() => interval.reset_immediately(),
                    _ = interval.tick() => {
                        let Some(network) = weak.upgrade() else { break };
                        pending = network.members().await.unwrap_or_default().into_iter().collect();
                        pending.remove(&network.identity());
                    }
                }
                let Some(network) = weak.upgrade() else { break };
                let Some(profile) = network.profile.read().await.clone() else {
                    continue;
                };
                if profile.documents.read().await.is_none() {
                    continue;
                }
                for peer in pending.extract_if(|peer| syncing.insert(peer.clone())) {
                    let network = network.clone();
                    let profile = profile.clone();
                    tasks.spawn(async move {
                        // Each peer retries independently. sync_peer closes
                        // its connection explicitly when its profile closes.
                        let _ = network.sync_peer(&profile, &peer).await;
                        peer
                    });
                }
            }
        });
        Ok((network, receiver))
    }

    pub fn identity(&self) -> String {
        self.endpoint.id().to_string()
    }
    pub fn refresh(&self) {
        self.sync_requested.notify_one();
    }
    pub async fn file_transfer(&self, key: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = {
            let mut transfers = self.transfers.lock().await;
            transfers.retain(|_, lock| lock.strong_count() != 0);
            let lock = transfers
                .get(key)
                .and_then(Weak::upgrade)
                .unwrap_or_default();
            transfers.insert(key.into(), Arc::downgrade(&lock));
            lock
        };
        lock.lock_owned().await
    }
    pub async fn rename(&self, name: String) {
        self.endpoint
            .set_user_data_for_address_lookup(name.parse().ok());
        *self.name.write().await = name;
    }
    pub fn matches_configuration(
        &self,
        nearby: bool,
        relay: Option<&str>,
        public: bool,
    ) -> Result<bool> {
        Ok(self.nearby == nearby && self.relay == relay_mode(relay, public)?)
    }
    pub async fn set_enrollment_data(&self, data: Vec<u8>) {
        *self.enrollment_data.write().await = data;
    }
    pub async fn invitation(&self) -> Result<String> {
        Ok(serde_json::to_string(&self.endpoint.addr())?)
    }
    async fn remember_address(&self, address: &EndpointAddr) -> Result<()> {
        let address = canonical_address(address.clone());
        let mut db = self.database.lock().await;
        self.addresses.add_endpoint_info(address.clone());
        let remembered: EndpointAddr = self
            .addresses
            .get_endpoint_info(address.id)
            .expect("the address was just inserted")
            .into();
        sqlx::query("INSERT INTO connect_addresses(peer,address) VALUES(?1,?2) ON CONFLICT(peer) DO UPDATE SET address=excluded.address")
            .bind(address.id.to_string()).bind(serde_json::to_string(&remembered)?).execute(&mut *db).await?;
        Ok(())
    }
    pub async fn remember_peer(&self, invitation: &str) -> Result<String> {
        if let Ok(id) = invitation.parse::<EndpointId>() {
            return Ok(id.to_string());
        }
        let address: EndpointAddr =
            serde_json::from_str(invitation).context("Invalid Connect invitation")?;
        self.remember_address(&address).await?;
        Ok(address.id.to_string())
    }
    pub fn new_profile_id() -> String {
        SecretKey::generate().public().to_string()
    }
    pub(crate) async fn current_profile(&self) -> Result<String> {
        self.profile
            .read()
            .await
            .as_ref()
            .map(|p| p.id.clone())
            .context("No Connect profile is open")
    }
    pub async fn open_profile(
        self: &Arc<Self>,
        profile: &str,
        create: bool,
        data: Vec<u8>,
    ) -> Result<()> {
        ensure!(
            self.profile.read().await.is_none(),
            "A Connect profile is already open"
        );
        let mut db = self.database.lock().await;
        let existing: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM connect_rosters WHERE profile=?1)")
                .bind(profile)
                .fetch_one(&mut *db)
                .await?;
        if create || !existing {
            // Existing local profiles retain their identity and Loro state across
            // the transport migration. Other devices still require verification.
            let doc = self.load_roster(&mut db, profile).await?;
            if !existing {
                doc.get_map("devices").insert(&self.identity(), true)?;
            }
            save_roster(&mut db, profile, &doc).await?;
        }
        drop(db);
        *self.enrollment_data.write().await = data;
        *self.profile.write().await = Some(Arc::new(Profile {
            id: profile.to_owned(),
            documents: RwLock::new(None),
            stop: CancellationToken::new(),
        }));
        self.membership_changed.send_replace(());
        Ok(())
    }
    pub async fn attach_documents(&self, documents: Arc<ProfileStore>) -> Result<()> {
        let profile = self
            .profile
            .read()
            .await
            .clone()
            .context("No Connect profile is open")?;
        documents.register_members(&self.members().await?).await?;
        *profile.documents.write().await = Some(documents);
        let mut db = self.database.lock().await;
        let roster = self.load_roster(&mut db, &profile.id).await?;
        drop(db);
        self.retire_members(&profile.id, &roster).await?;
        self.membership_changed.send_replace(());
        Ok(())
    }

    async fn retire_members(&self, profile: &str, roster: &LoroDoc) -> Result<()> {
        let records = roster
            .get_map("devices")
            .get_deep_value()
            .to_json_value()
            .as_object()
            .into_iter()
            .flat_map(|members| members.iter())
            .filter(|(_, value)| value.as_bool() == Some(false))
            .map(|(peer, _)| library::ConnectRecord {
                kind: "device".into(),
                key: peer.clone(),
                value: None,
            })
            .collect::<Vec<_>>();
        if !records.is_empty()
            && let Some(current) = self.profile.read().await.clone()
            && current.id == profile
            && let Some(documents) = current.documents.read().await.clone()
        {
            documents.write_records(&records).await?;
        }
        Ok(())
    }
    async fn load_roster(&self, db: &mut SqliteConnection, profile: &str) -> Result<LoroDoc> {
        let doc = LoroDoc::new();
        doc.set_peer_id(u64::from_le_bytes(
            self.endpoint.id().as_bytes()[..8].try_into()?,
        ))?;
        if let Some(bytes) = sqlx::query_scalar::<_, Vec<u8>>(
            "SELECT snapshot FROM connect_rosters WHERE profile=?1",
        )
        .bind(profile)
        .fetch_optional(db)
        .await?
        {
            doc.import(&bytes)?;
        }
        Ok(doc)
    }
    pub async fn members(&self) -> Result<Vec<String>> {
        self.enrolled_members(&self.current_profile().await?).await
    }
    pub async fn enrolled_members(&self, profile: &str) -> Result<Vec<String>> {
        let mut db = self.database.lock().await;
        let doc = self.load_roster(&mut db, profile).await?;
        if !trusted(&doc, self.endpoint.id()) {
            return Ok(Vec::new());
        }
        Ok(doc
            .get_map("devices")
            .get_deep_value()
            .to_json_value()
            .as_object()
            .into_iter()
            .flat_map(|map| map.iter())
            .filter(|(_, value)| value.as_bool() == Some(true))
            .map(|(key, _)| key.clone())
            .collect())
    }
    pub(crate) async fn authorize(&self, peer: EndpointId) -> Result<()> {
        let profile = self.current_profile().await?;
        self.authorize_profile(&profile, peer).await
    }
    async fn authorize_profile(&self, profile: &str, peer: EndpointId) -> Result<()> {
        let mut db = self.database.lock().await;
        let doc = self.load_roster(&mut db, profile).await?;
        ensure!(
            trusted(&doc, self.endpoint.id()),
            "This device was removed from the profile"
        );
        ensure!(
            trusted(&doc, peer),
            "Device is not a member of this profile"
        );
        Ok(())
    }
    pub async fn check_membership(&self, peer: &str) -> Result<()> {
        self.authorize(peer.parse()?).await
    }
    pub(crate) async fn enroll(&self, peer: EndpointId, name: &str) -> Result<Vec<u8>> {
        let profile = self.current_profile().await?;
        let mut db = self.database.lock().await;
        let doc = self.load_roster(&mut db, &profile).await?;
        ensure!(
            trusted(&doc, self.endpoint.id()),
            "This device was removed from the profile"
        );
        doc.get_map("devices").insert(&peer.to_string(), true)?;
        save_roster(&mut db, &profile, &doc).await?;
        self.membership_changed.send_replace(());
        drop(db);
        if let Some(profile) = self.profile.read().await.as_ref()
            && let Some(documents) = profile.documents.read().await.as_ref()
        {
            documents.register_members(&[peer.to_string()]).await?;
            documents
                .write_records(&[library::ConnectRecord {
                    kind: "device".into(),
                    key: peer.to_string(),
                    value: Some(serde_json::json!(name)),
                }])
                .await?;
        }
        Ok(doc.export(ExportMode::Snapshot)?)
    }
    pub(crate) async fn accept_enrollment(
        &self,
        profile: &str,
        bytes: &[u8],
        host: EndpointId,
    ) -> Result<()> {
        let enrollment = LoroDoc::new();
        enrollment.import(bytes)?;
        ensure!(
            trusted(&enrollment, self.endpoint.id()) && trusted(&enrollment, host),
            "The verified enrollment does not include both devices"
        );
        let mut db = self.database.lock().await;
        let doc = self.load_roster(&mut db, profile).await?;
        doc.import(bytes)?;
        // Fresh bilateral approval supersedes either device's earlier removal.
        // Keep the merged history, including removals of unrelated devices.
        let devices = doc.get_map("devices");
        devices.insert(&self.identity(), true)?;
        devices.insert(&host.to_string(), true)?;
        save_roster(&mut db, profile, &doc).await?;
        self.membership_changed.send_replace(());
        Ok(())
    }
    pub async fn remove_member(&self, peer: &str) -> Result<()> {
        let peer: EndpointId = peer.parse()?;
        let profile = self.current_profile().await?;
        let mut db = self.database.lock().await;
        let doc = self.load_roster(&mut db, &profile).await?;
        ensure!(
            trusted(&doc, self.endpoint.id()),
            "This device was removed from the profile"
        );
        doc.get_map("devices").insert(&peer.to_string(), false)?;
        save_roster(&mut db, &profile, &doc).await?;
        self.membership_changed.send_replace(());
        drop(db);
        self.retire_members(&profile, &doc).await?;
        let _ = self.events.send(NetworkEvent::MembershipChanged).await;
        Ok(())
    }
    pub async fn leave_profile(&self, profile: &str) -> Result<()> {
        let mut db = self.database.lock().await;
        let doc = self.load_roster(&mut db, profile).await?;
        let peers = doc
            .get_map("devices")
            .get_deep_value()
            .to_json_value()
            .as_object()
            .into_iter()
            .flat_map(|map| map.iter())
            .filter(|(_, value)| value.as_bool() == Some(true))
            .filter_map(|(peer, _)| peer.parse::<EndpointId>().ok())
            .filter(|peer| *peer != self.endpoint.id())
            .collect::<Vec<_>>();
        doc.get_map("devices").insert(&self.identity(), false)?;
        save_roster(&mut db, profile, &doc).await?;
        let request = SyncRequest {
            profile: profile.to_owned(),
            after: 0,
            stream: SyncStream::Authored,
            roster: doc.export(ExportMode::Snapshot)?,
            addresses: Vec::new(),
        };
        drop(db);
        futures_util::stream::iter(peers)
            .for_each_concurrent(None, |peer| {
                let request = &request;
                async move {
                    let _ = tokio::time::timeout(Duration::from_secs(15), async {
                        let conn = self
                            .endpoint
                            .connect(peer_address(&self.addresses, peer), SYNC_PROTOCOL)
                            .await?;
                        let (mut send, mut recv) = conn.open_bi().await?;
                        write_frame(&mut send, request).await?;
                        send.finish()?;
                        let _: Option<SyncPage> = read_frame(&mut recv).await?;
                        conn.close(0u32.into(), b"profile left");
                        Ok::<(), anyhow::Error>(())
                    })
                    .await;
                }
            })
            .await;
        Ok(())
    }
    async fn roster(&self, profile: &str) -> Result<Vec<u8>> {
        let mut db = self.database.lock().await;
        Ok(self
            .load_roster(&mut db, profile)
            .await?
            .export(ExportMode::Snapshot)?)
    }
    async fn merge_roster(&self, profile: &str, bytes: &[u8]) -> Result<bool> {
        let mut db = self.database.lock().await;
        let doc = self.load_roster(&mut db, profile).await?;
        let before = doc.oplog_vv();
        doc.import(bytes)?;
        let member = trusted(&doc, self.endpoint.id());
        if before != doc.oplog_vv() {
            save_roster(&mut db, profile, &doc).await?;
            self.membership_changed.send_replace(());
            drop(db);
            self.retire_members(profile, &doc).await?;
            let _ = self.events.send(NetworkEvent::MembershipChanged).await;
        }
        Ok(member)
    }
    async fn revocation(&self, profile: &str, peer: EndpointId) -> Result<Option<Vec<u8>>> {
        let mut db = self.database.lock().await;
        let doc = self.load_roster(&mut db, profile).await?;
        let removed = doc
            .get_map("devices")
            .get(&peer.to_string())
            .is_some_and(|value| {
                value
                    .as_value()
                    .is_some_and(|value| value.to_json_value().as_bool() == Some(false))
            });
        if removed && trusted(&doc, self.endpoint.id()) {
            return Ok(Some(doc.export(ExportMode::Snapshot)?));
        }
        Ok(None)
    }
    async fn peer_addresses(&self) -> Result<Vec<EndpointAddr>> {
        let mut addresses = vec![self.endpoint.addr()];
        for peer in self.members().await? {
            let peer = peer.parse()?;
            if let Some(info) = self.addresses.get_endpoint_info(peer) {
                addresses.push(info.into());
            }
        }
        Ok(addresses)
    }
    async fn accept_addresses(&self, addresses: Vec<EndpointAddr>) -> Result<()> {
        let members = self.members().await?;
        for address in addresses {
            if address.id != self.endpoint.id() && members.contains(&address.id.to_string()) {
                self.remember_address(&address).await?;
            }
        }
        Ok(())
    }
    pub async fn begin_pairing(self: &Arc<Self>, invitation: &str) -> Result<()> {
        self.start_pairing(invitation, true).await
    }
    pub async fn invite(self: &Arc<Self>, peer: &str) -> Result<()> {
        self.current_profile().await?;
        self.start_pairing(peer, false).await
    }
    async fn start_pairing(self: &Arc<Self>, invitation: &str, joining: bool) -> Result<()> {
        let peer: EndpointId = self.remember_peer(invitation).await?.parse()?;
        self.cancel_pairing().await;
        let network = self.clone();
        *self.pairing_task.lock().await = Some(tokio::spawn(async move {
            let protocol = if joining {
                PAIR_PROTOCOL
            } else {
                INVITE_PROTOCOL
            };
            match network
                .endpoint
                .connect(peer_address(&network.addresses, peer), protocol)
                .await
            {
                Ok(connection) => {
                    let _ = pairing::run(network, connection, joining, true).await;
                }
                Err(error) => {
                    let _ = network
                        .events
                        .send(NetworkEvent::PairingFailed {
                            session: None,
                            error: error.to_string(),
                        })
                        .await;
                }
            }
        }));
        Ok(())
    }
    pub async fn cancel_pairing(&self) {
        if let Some(task) = self.pairing_task.lock().await.take() {
            task.abort();
            let _ = task.await;
        }
        for (_, decision) in self.approvals.lock().await.drain() {
            let _ = decision.send(Some(false));
        }
    }
    pub async fn confirm_pairing(&self, session: &str, approved: bool) -> Result<()> {
        self.approvals
            .lock()
            .await
            .get(session)
            .context("Pairing is no longer pending")?
            .send(Some(approved))
            .map_err(|_| anyhow::anyhow!("Pairing disconnected"))
    }
    pub async fn request(&self, peer: &str, body: Vec<u8>) -> Result<Vec<u8>> {
        let peer = peer.parse()?;
        let profile = self.current_profile().await?;
        self.authorize(peer).await?;
        let conn = tokio::time::timeout(
            Duration::from_secs(30),
            self.endpoint
                .connect(peer_address(&self.addresses, peer), RPC_PROTOCOL),
        )
        .await
        .context("Device is unreachable")??;
        let (mut send, mut recv) = conn.open_bi().await?;
        write_frame(&mut send, &body).await?;
        send.finish()?;
        let response: Result<Vec<u8>, String> = read_frame(&mut recv).await?;
        let _ = self
            .events
            .send(NetworkEvent::Reachable {
                profile,
                peer: peer.to_string(),
            })
            .await;
        conn.close(0u32.into(), b"complete");
        response.map_err(anyhow::Error::msg)
    }
    pub async fn stream_request(&self, peer: &str, body: Vec<u8>) -> Result<PeerByteResponse> {
        self.stream_request_for_profile(&self.current_profile().await?, peer, body)
            .await
    }

    /// Read from a verified enrollment without changing the currently active profile.
    pub async fn stream_request_for_profile(
        &self,
        profile: &str,
        peer: &str,
        body: Vec<u8>,
    ) -> Result<PeerByteResponse> {
        let peer = peer.parse()?;
        self.authorize_profile(profile, peer).await?;
        let connection = self
            .endpoint
            .connect(peer_address(&self.addresses, peer), MEDIA_PROTOCOL)
            .await?;
        let (mut send, mut recv) = connection.open_bi().await?;
        write_frame(&mut send, &body).await?;
        send.finish()?;
        let metadata: Result<serde_json::Value, String> = read_frame(&mut recv).await?;
        Ok(PeerByteResponse {
            metadata: metadata.map_err(anyhow::Error::msg)?,
            reader: Box::pin(MediaReader {
                _connection: connection,
                recv,
            }),
        })
    }

    pub async fn test_connection(&self, peer: &str, body: Vec<u8>) -> Result<(String, Vec<u8>)> {
        let peer = peer.parse()?;
        let profile = self.current_profile().await?;
        self.authorize(peer).await?;
        tokio::time::timeout(Duration::from_secs(15), async {
            let conn = self
                .endpoint
                .connect(peer_address(&self.addresses, peer), RPC_PROTOCOL)
                .await?;
            let (mut send, mut recv) = conn.open_bi().await?;
            write_frame(&mut send, &body).await?;
            send.finish()?;
            let response: Result<Vec<u8>, String> = read_frame(&mut recv).await?;
            let response = response.map_err(anyhow::Error::msg)?;
            let _ = self
                .events
                .send(NetworkEvent::Reachable {
                    profile,
                    peer: peer.to_string(),
                })
                .await;
            let route = conn
                .paths()
                .iter()
                .find(|path| path.is_selected())
                .map(|path| match path.remote_addr() {
                    iroh::TransportAddr::Relay(_) => "Via relay",
                    iroh::TransportAddr::Ip(address) => match address.ip() {
                        std::net::IpAddr::V4(ip)
                            if ip.is_private() || ip.is_loopback() || ip.is_link_local() =>
                        {
                            "Local network"
                        }
                        std::net::IpAddr::V6(ip)
                            if ip.is_unique_local()
                                || ip.is_loopback()
                                || ip.is_unicast_link_local() =>
                        {
                            "Local network"
                        }
                        _ => "Direct",
                    },
                    _ => "Direct",
                })
                .unwrap_or("Waiting for device")
                .to_owned();
            conn.close(0u32.into(), b"connection test complete");
            Ok((route, response))
        })
        .await?
    }
    async fn sync_peer(&self, profile: &Arc<Profile>, peer: &str) -> Result<()> {
        let peer: EndpointId = peer.parse()?;
        self.authorize(peer).await?;
        // Each peer has an independent connection. An offline peer cannot hold up
        // settings, playback requests, or another peer's collection.
        let conn = tokio::select! {
            biased;
            _ = profile.stop.cancelled() => return Ok(()),
            result = tokio::time::timeout(Duration::from_secs(30), self.endpoint.connect(peer_address(&self.addresses, peer), SYNC_PROTOCOL)) => result??,
        };
        let result = tokio::select! {
            biased;
            _ = profile.stop.cancelled() => {
                conn.close(0u32.into(), PROFILE_CLOSED);
                Ok(())
            },
            result = self.pull(&conn, profile, peer) => result,
        };
        let result = if result.as_ref().is_err_and(|error| {
            error.is::<std::io::Error>()
                || error.is::<ConnectionError>()
                || error.is::<iroh::endpoint::ClosedStream>()
        }) && matches!(conn.close_reason(), Some(ConnectionError::ApplicationClosed(close))
            if close.error_code == 0u32.into() && close.reason.as_ref() == PROFILE_CLOSED)
        {
            Ok(())
        } else {
            result
        };
        conn.close(
            if result.is_ok() { 0u32 } else { 1u32 }.into(),
            b"sync complete",
        );
        let _ = self
            .events
            .send(NetworkEvent::Syncing {
                profile: profile.id.clone(),
                peer: peer.to_string(),
                active: false,
            })
            .await;
        if let Err(error) = &result {
            let event = if error.is::<std::io::Error>()
                || error.is::<ConnectionError>()
                || error.is::<iroh::endpoint::ClosedStream>()
            {
                NetworkEvent::Unavailable {
                    profile: profile.id.clone(),
                    peer: peer.to_string(),
                }
            } else {
                NetworkEvent::Error(error.to_string())
            };
            let _ = self.events.send(event).await;
        }
        result
    }
    async fn pull(
        &self,
        conn: &Connection,
        profile: &Arc<Profile>,
        peer: EndpointId,
    ) -> Result<()> {
        let Some(documents) = profile.documents.read().await.clone() else {
            return Ok(());
        };
        let _ = self
            .events
            .send(NetworkEvent::Settings {
                profile: profile.id.clone(),
                peer: peer.to_string(),
            })
            .await;
        let mut receiving = false;
        loop {
            let mut more = false;
            for stream in [SyncStream::Authored, SyncStream::NativeCache] {
                self.authorize(peer).await?;
                let after = documents.sync_cursor_in(&peer.to_string(), stream).await?;
                let (mut send, mut recv) = conn.open_bi().await?;
                write_frame(
                    &mut send,
                    &SyncRequest {
                        profile: profile.id.clone(),
                        after,
                        stream,
                        roster: self.roster(&profile.id).await?,
                        addresses: self.peer_addresses().await?,
                    },
                )
                .await?;
                let page: Option<SyncPage> = read_frame(&mut recv).await?;
                let Some(page) = page else {
                    return Ok(());
                };
                if !self.merge_roster(&profile.id, &page.roster).await? {
                    return Ok(());
                }
                self.authorize(peer).await?;
                self.accept_addresses(page.addresses).await?;
                let names = page
                    .versions
                    .iter()
                    .map(|v| v.name.clone())
                    .collect::<Vec<_>>();
                let versions = documents.versions(&names).await?;
                write_frame(&mut send, &versions).await?;
                // Commit the bounded page together instead of fsyncing once
                // for every catalog row in the initial collection.
                let mut updates = Vec::with_capacity(names.len());
                loop {
                    let length = recv.read_u64().await?;
                    if length == 0 {
                        break;
                    }
                    let mut update = Vec::new();
                    let read = (&mut recv).take(length).read_to_end(&mut update).await?;
                    ensure!(read as u64 == length, "Incomplete profile update");
                    updates.push(update);
                }
                let changed = documents.import_updates(&updates).await?;
                if changed && !receiving {
                    receiving = true;
                    let _ = self
                        .events
                        .send(NetworkEvent::Syncing {
                            profile: profile.id.clone(),
                            peer: peer.to_string(),
                            active: true,
                        })
                        .await;
                }
                documents
                    .acknowledge_versions(&peer.to_string(), &page.versions, &self.members().await?)
                    .await?;
                let versions = documents.versions(&names).await?;
                let roster = self.roster(&profile.id).await?;
                write_frame(&mut send, &SyncAcknowledgement { versions, roster }).await?;
                send.finish()?;
                // The server sends a final acknowledgement only after it has
                // consumed ours. Keep the connection alive through that read.
                let _: bool = read_frame(&mut recv).await?;
                if let Some(last) = page.versions.last() {
                    documents
                        .acknowledge_sync_in(&peer.to_string(), stream, last.revision)
                        .await?;
                }
                let complete = page.versions.len() < SYNC_PAGE
                    && !page
                        .versions
                        .last()
                        .is_some_and(|version| version.name.starts_with("playlist_artwork:"));
                more |= !complete;
            }
            if !more {
                return Ok(());
            }
            tokio::task::yield_now().await;
        }
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.stop.cancel();
        self.cancel_pairing().await;
        self.approvals.lock().await.clear();
        self.close_profile().await?;
        if let Some(router) = self.router.lock().await.take() {
            router.shutdown().await?;
        }
        Ok(())
    }
    pub async fn close_profile(&self) -> Result<()> {
        if let Some(profile) = self.profile.write().await.take() {
            profile.stop.cancel();
        }
        self.membership_changed.send_replace(());
        Ok(())
    }
}

impl Drop for ConnectNetwork {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

fn trusted(doc: &LoroDoc, peer: EndpointId) -> bool {
    doc.get_map("devices")
        .get(&peer.to_string())
        .is_some_and(|v| {
            v.as_value()
                .is_some_and(|v| v.to_json_value().as_bool() == Some(true))
        })
}
async fn save_roster(db: &mut SqliteConnection, profile: &str, doc: &LoroDoc) -> Result<()> {
    sqlx::query("INSERT INTO connect_rosters(profile,snapshot) VALUES(?1,?2) ON CONFLICT(profile) DO UPDATE SET snapshot=excluded.snapshot")
        .bind(profile).bind(doc.export(ExportMode::Snapshot)?).execute(db).await?;
    Ok(())
}

#[derive(Debug)]
struct SyncProtocol(Weak<ConnectNetwork>);
impl ProtocolHandler for SyncProtocol {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let Some(network) = self.0.upgrade() else {
            return Ok(());
        };
        let peer = conn.remote_id();
        let Some(profile) = network.profile.read().await.clone() else {
            conn.close(0u32.into(), PROFILE_CLOSED);
            return Ok(());
        };
        loop {
            let stream = tokio::select! {
                biased;
                _ = profile.stop.cancelled() => {
                    conn.close(0u32.into(), PROFILE_CLOSED);
                    break;
                },
                stream = conn.accept_bi() => stream,
            };
            let Ok((mut send, mut recv)) = stream else {
                break;
            };
            let result = async {
                let request: SyncRequest = read_frame(&mut recv).await?;
                ensure!(request.profile == profile.id, "The Connect profile changed");
                // A removed member may receive its removal, but no profile data,
                // addresses, or opportunity to merge its own membership changes.
                if let Some(roster) = network.revocation(&profile.id, peer).await? {
                    write_frame(
                        &mut send,
                        &Some(SyncPage {
                            versions: Vec::new(),
                            roster,
                            addresses: Vec::new(),
                        }),
                    )
                    .await?;
                    send.finish()?;
                    let _ = send.stopped().await;
                    return Ok(());
                }
                network.authorize(peer).await?;
                if !network.merge_roster(&profile.id, &request.roster).await? {
                    return Ok(());
                }
                if network.revocation(&profile.id, peer).await?.is_some() {
                    write_frame(&mut send, &None::<SyncPage>).await?;
                    send.finish()?;
                    let _ = send.stopped().await;
                    return Ok(());
                }
                network.authorize(peer).await?;
                network.accept_addresses(request.addresses).await?;
                let documents = profile.documents.read().await.clone();
                let Some(documents) = documents else {
                    write_frame(&mut send, &None::<SyncPage>).await?;
                    send.finish()?;
                    let _ = send.stopped().await;
                    return Ok(());
                };
                let versions = documents
                    .changes_in_stream(request.after, SYNC_PAGE, request.stream)
                    .await?;
                let names = versions.iter().map(|v| v.name.clone()).collect::<Vec<_>>();
                write_frame(
                    &mut send,
                    &Some(SyncPage {
                        versions,
                        roster: network.roster(&profile.id).await?,
                        addresses: network.peer_addresses().await?,
                    }),
                )
                .await?;
                let versions: Vec<DocumentVersion> = read_frame(&mut recv).await?;
                ensure!(
                    versions.len() == names.len()
                        && versions.iter().zip(&names).all(|(v, n)| &v.name == n),
                    "Sync response does not match the requested documents"
                );
                network.authorize(peer).await?;
                for version in &versions {
                    for update in documents.updates(std::slice::from_ref(version)).await? {
                        send.write_u64(update.len() as u64).await?;
                        send.write_all(&update).await?;
                    }
                }
                send.write_u64(0).await?;
                let acknowledged: SyncAcknowledgement = read_frame(&mut recv).await?;
                {
                    let SyncAcknowledgement { versions, roster } = acknowledged;
                    ensure!(
                        versions
                            .iter()
                            .map(|version| &version.name)
                            .eq(names.iter()),
                        "Sync acknowledgement does not match the requested documents"
                    );
                    if !network.merge_roster(&profile.id, &roster).await? {
                        return Ok(());
                    }
                    documents
                        .acknowledge_versions(
                            &peer.to_string(),
                            &versions,
                            &network.members().await?,
                        )
                        .await?;
                }
                write_frame(&mut send, &true).await?;
                send.finish()?;
                let _ = send.stopped().await;
                Ok(())
            };
            let result: Result<()> = tokio::select! {
                biased;
                _ = profile.stop.cancelled() => {
                    conn.close(0u32.into(), PROFILE_CLOSED);
                    break;
                },
                result = result => result,
            };
            if result.is_err() {
                conn.close(1u32.into(), b"sync ended");
                break;
            }
        }
        Ok(())
    }
}

struct MediaReader {
    _connection: Connection,
    recv: iroh::endpoint::RecvStream,
}

impl tokio::io::AsyncRead for MediaReader {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.recv).poll_read(cx, buffer)
    }
}

#[derive(Debug)]
struct MediaProtocol(Weak<ConnectNetwork>);
impl ProtocolHandler for MediaProtocol {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let Some(network) = self.0.upgrade() else {
            return Ok(());
        };
        let result: Result<()> = async {
            let peer = connection.remote_id();
            network.authorize(peer).await?;
            let profile = network
                .profile
                .read()
                .await
                .clone()
                .context("Connect is disabled")?;
            let (mut send, mut recv) = connection.accept_bi().await?;
            let body = read_frame(&mut recv).await?;
            let (reply, response) = oneshot::channel();
            network
                .events
                .send(NetworkEvent::StreamRequest {
                    profile: profile.id.clone(),
                    peer: peer.to_string(),
                    body,
                    reply,
                })
                .await
                .map_err(|_| anyhow::anyhow!("Connect stopped"))?;
            let transfer = async {
                match response.await? {
                    Ok(mut audio) => {
                        write_frame(&mut send, &Ok::<_, String>(audio.metadata)).await?;
                        tokio::io::copy(&mut audio.reader, &mut send).await?;
                    }
                    Err(error) => {
                        write_frame(&mut send, &Err::<serde_json::Value, _>(error)).await?
                    }
                }
                send.finish()?;
                let _ = send.stopped().await;
                Ok::<_, anyhow::Error>(())
            };
            tokio::select! {
                result = transfer => result,
                _ = profile.stop.cancelled() => Ok(()),
                _ = connection.closed() => Ok(()),
            }
        }
        .await;
        if result.is_err() {
            connection.close(1u32.into(), b"media ended");
        }
        Ok(())
    }
}

#[derive(Debug)]
struct RpcProtocol(Weak<ConnectNetwork>);
impl ProtocolHandler for RpcProtocol {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let Some(network) = self.0.upgrade() else {
            return Ok(());
        };
        let result: Result<()> = async {
            let peer = conn.remote_id();
            let profile = network.current_profile().await?;
            network.authorize(peer).await?;
            let (mut send, mut recv) = conn.accept_bi().await?;
            let body = read_frame(&mut recv).await?;
            network.authorize(peer).await?;
            let _ = network
                .events
                .send(NetworkEvent::Reachable {
                    profile: profile.clone(),
                    peer: peer.to_string(),
                })
                .await;
            let (reply, answer) = oneshot::channel();
            network
                .events
                .send(NetworkEvent::Request {
                    profile,
                    peer: peer.to_string(),
                    body,
                    reply,
                })
                .await
                .map_err(|_| anyhow::anyhow!("Connect stopped"))?;
            write_frame(&mut send, &answer.await?).await?;
            send.finish()?;
            let _ = send.stopped().await;
            let _ = conn.closed().await;
            Ok(())
        }
        .await;
        if result.is_err() {
            conn.close(1u32.into(), b"request rejected");
        }
        Ok(())
    }
}

pub(crate) async fn write_frame<T: Serialize>(
    send: &mut (impl tokio::io::AsyncWrite + Unpin),
    value: &T,
) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(
        bytes.len() <= MAX_FRAME,
        "Connect message exceeds transfer limit"
    );
    send.write_all(&(bytes.len() as u32).to_be_bytes()).await?;
    send.write_all(&bytes).await?;
    Ok(())
}

pub(crate) async fn read_frame<T: DeserializeOwned>(
    recv: &mut (impl tokio::io::AsyncRead + Unpin),
) -> Result<T> {
    let mut length = [0u8; 4];
    recv.read_exact(&mut length).await?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_FRAME {
        bail!("Connect message exceeds transfer limit");
    }
    let mut bytes = vec![0; length];
    recv.read_exact(&mut bytes).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "AGE-SECRET-KEY-1GQ9778VQXMMJVE8SK7J6VT8UJ4HDQAJUVSFCWCM02D8GEWQ72PVQ2Y5J33";

    #[tokio::test]
    async fn relay_aliases_share_one_address_and_disabled_transport_stays_direct() {
        let root = tempfile::tempdir().unwrap();
        let (network, _events) = ConnectNetwork::spawn(
            NetworkConfig {
                database: root.path().join("network.sqlite"),
                name: "Direct device".into(),
                nearby: false,
                relay: None,
                public_relay: false,
            },
            SecretKey::generate(),
        )
        .await
        .unwrap();
        let peer = Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .alpns(vec![RPC_PROTOCOL.to_vec()])
            .bind()
            .await
            .unwrap();
        let address = peer
            .addr()
            .with_relay_url("https://relay.example./".parse().unwrap())
            .with_relay_url("https://relay.example/".parse().unwrap());
        network.remember_address(&address).await.unwrap();
        let remembered: EndpointAddr = network
            .addresses
            .get_endpoint_info(peer.id())
            .unwrap()
            .into();
        assert_eq!(remembered.relay_urls().count(), 1);
        let raw_lookup = MemoryLookup::new();
        raw_lookup.add_endpoint_info(address);
        let lookup = CanonicalRelays(raw_lookup);
        let item = lookup
            .resolve(peer.id())
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(item.to_endpoint_addr().relay_urls().count(), 1);
        assert_eq!(
            item.to_endpoint_addr()
                .relay_urls()
                .next()
                .unwrap()
                .as_str(),
            "https://relay.example/"
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            let (outgoing, incoming) =
                tokio::join!(network.endpoint.connect(peer.id(), RPC_PROTOCOL), async {
                    peer.accept().await.unwrap().await
                });
            outgoing.unwrap().close(0u32.into(), b"done");
            incoming.unwrap().close(0u32.into(), b"done");
        })
        .await
        .unwrap();
        network.shutdown().await.unwrap();
        peer.close().await;
    }

    #[tokio::test]
    async fn direct_sync_acknowledges_committed_state_for_pruning() {
        tokio::time::timeout(Duration::from_secs(20), async {
            let directory = tempfile::tempdir().unwrap();
            let config = |name: &str| NetworkConfig {
                database: directory.path().join(format!("{name}-network.sqlite")),
                name: name.into(),
                nearby: false,
                relay: None,
                public_relay: false,
            };
            let (host, _host_events) = ConnectNetwork::spawn(config("host"), SecretKey::generate())
                .await
                .unwrap();
            let (peer, _peer_events) = ConnectNetwork::spawn(config("peer"), SecretKey::generate())
                .await
                .unwrap();
            let profile = ConnectNetwork::new_profile_id();
            host.open_profile(&profile, true, Vec::new()).await.unwrap();
            let roster = host.enroll(peer.endpoint.id(), "peer").await.unwrap();
            peer.accept_enrollment(&profile, &roster, host.endpoint.id())
                .await
                .unwrap();
            peer.open_profile(&profile, false, Vec::new())
                .await
                .unwrap();
            let database = library::Database::open(&directory.path().join("host.sqlite"))
                .await
                .unwrap();
            let documents = Arc::new(
                ProfileStore::open(database.clone(), 1, KEY.parse().unwrap())
                    .await
                    .unwrap(),
            );
            documents
                .bind_profile(&profile, &host.identity())
                .await
                .unwrap();
            database.connect_set_active(true).await.unwrap();
            for value in 0..80 {
                documents
                    .write_records(&[library::ConnectRecord {
                        kind: "preference".into(),
                        key: "example".into(),
                        value: Some(serde_json::json!(value)),
                    }])
                    .await
                    .unwrap();
            }
            host.attach_documents(documents.clone()).await.unwrap();
            let receiving_documents = Arc::new(
                ProfileStore::open(
                    library::Database::open(&directory.path().join("peer.sqlite"))
                        .await
                        .unwrap(),
                    2,
                    KEY.parse().unwrap(),
                )
                .await
                .unwrap(),
            );
            let file = directory.path().join("snapshot");
            documents
                .export_database(&file, &KEY.parse().unwrap(), &[], &[])
                .await
                .unwrap();
            // A cursor established before member tracking must exchange versions
            // again, without importing the same catalog twice.
            receiving_documents
                .acknowledge_sync_in(
                    &host.identity(),
                    SyncStream::Authored,
                    documents.revision().await.unwrap(),
                )
                .await
                .unwrap();
            receiving_documents
                .merge_database(&file, &KEY.parse().unwrap())
                .await
                .unwrap();
            assert_eq!(
                receiving_documents
                    .sync_cursor_in(&host.identity(), SyncStream::Authored)
                    .await
                    .unwrap(),
                0
            );
            let uri = library::source_entity_uri(
                &library::SourceId::new("remote"),
                "track",
                "after-snapshot",
            );
            database
                .record_listen(
                    &library::ListenWrite {
                        external_id: Some("after-snapshot".into()),
                        media_uri: uri.clone(),
                        title: "Saved track".into(),
                        artist: "Artist".into(),
                        album: "Album".into(),
                        duration_millis: 180000,
                        disc_number: None,
                        track_number: None,
                        year: None,
                        release_date: None,
                        source_format: None,
                        musicbrainz_recording_id: None,
                        musicbrainz_release_track_id: None,
                        started_at: 1700000000,
                        local_period: "2023-11".into(),
                        listened_millis: 120000,
                        skipped: false,
                    },
                    None,
                    &[],
                )
                .await
                .unwrap();
            database
                .create_playlist(
                    None,
                    "Saved",
                    &library::MemberSelection::from_uris(vec![uri.clone()]),
                )
                .await
                .unwrap()
                .unwrap();
            database
                .reconcile_source(&library::SourceId::new("remote"), "webdav", "Files")
                .await
                .unwrap();
            let path = "https://files.example.test/music/after-snapshot.flac";
            database
                .catalog("remote", None)
                .await
                .unwrap()
                .write_file_observations(
                    &[(
                        library::LocalFileWrite {
                            path: path.into(),
                            root: "https://files.example.test/music".into(),
                            relative_path: "after-snapshot.flac".into(),
                            kind: library::LocalFileKind::Media,
                            size_bytes: Some(12),
                            mtime_ns: 0,
                            device_id: None,
                            inode: None,
                            native_id: None,
                            picture_index: None,
                            artist_pictures: None,
                            revision: Some("v1".into()),
                            parse_version: Some(1),
                            state: library::LocalFileState::Accepted,
                        },
                        Vec::new(),
                    )],
                    vec![library::TrackObservation {
                        object_id: "after-snapshot".into(),
                        media_uri: Some(uri.clone()),
                        source_path: Some(path.into()),
                        metadata: library::TrackMetadata {
                            title: "Current catalog".into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    }],
                    true,
                )
                .await
                .unwrap();
            let current = documents
                .current_records(&[(
                    "catalog:file".into(),
                    serde_json::json!(["remote", path]).to_string(),
                )])
                .await
                .unwrap()
                .remove(0);
            peer.attach_documents(receiving_documents.clone())
                .await
                .unwrap();
            peer.remember_peer(&host.invitation().await.unwrap())
                .await
                .unwrap();
            documents
                .export_database(&file, &KEY.parse().unwrap(), &[], &[])
                .await
                .unwrap();
            let before = {
                let mut snapshot = sqlx::SqliteConnection::connect_with(
                    &sqlx::sqlite::SqliteConnectOptions::new()
                        .filename(&file)
                        .read_only(true),
                )
                .await
                .unwrap();
                let bytes: i64 = sqlx::query_scalar(
                    "SELECT length(snapshot) FROM documents WHERE name='preference:example'",
                )
                .fetch_one(&mut snapshot)
                .await
                .unwrap();
                snapshot.close().await.unwrap();
                bytes
            };
            documents
                .prune_history(&host.identity(), &[], 10)
                .await
                .unwrap();
            documents
                .export_database(&file, &KEY.parse().unwrap(), &[], &[])
                .await
                .unwrap();
            let receiving = peer.profile.read().await.clone().unwrap();
            peer.sync_peer(&receiving, &host.identity()).await.unwrap();
            let mut received = Vec::new();
            for stream in [SyncStream::Authored, SyncStream::NativeCache] {
                let mut cursor = 0;
                loop {
                    let page = receiving_documents
                        .changes_in_stream(cursor, 64, stream)
                        .await
                        .unwrap();
                    let Some(last) = page.last() else { break };
                    cursor = last.revision;
                    received.extend(page);
                }
            }
            assert!(
                received
                    .iter()
                    .any(|value| value.name == "listen:after-snapshot")
            );
            assert!(
                !received
                    .iter()
                    .any(|value| value.name == "track:after-snapshot")
            );
            assert!(
                received
                    .iter()
                    .any(|value| value.name == format!("reference:{uri}"))
            );
            assert_eq!(
                receiving_documents
                    .current_records(&[(current.kind.clone(), current.key.clone())])
                    .await
                    .unwrap(),
                [current]
            );
            let unchanged = receiving_documents.revision().await.unwrap();
            peer.sync_peer(&receiving, &host.identity()).await.unwrap();
            assert_eq!(receiving_documents.revision().await.unwrap(), unchanged);

            let authored = peer
                .endpoint
                .connect(
                    peer_address(&peer.addresses, host.endpoint.id()),
                    SYNC_PROTOCOL,
                )
                .await
                .unwrap();
            let (mut send, mut recv) = authored.open_bi().await.unwrap();
            let reference = documents
                .versions(&[format!("reference:{uri}")])
                .await
                .unwrap()
                .remove(0);
            assert!(reference.revision > 0);
            write_frame(
                &mut send,
                &serde_json::json!({
                    "profile": profile,
                    "after": reference.revision - 1,
                    "stream": SyncStream::Authored,
                    "roster": peer.roster(&profile).await.unwrap(),
                    "addresses": peer.peer_addresses().await.unwrap(),
                }),
            )
            .await
            .unwrap();
            let page: Option<SyncPage> = read_frame(&mut recv).await.unwrap();
            let page = page.unwrap();
            assert!(
                page.versions
                    .iter()
                    .any(|value| value.name == format!("reference:{uri}"))
            );
            assert!(
                !page
                    .versions
                    .iter()
                    .any(|value| value.name.starts_with("catalog:"))
            );
            authored.close(0u32.into(), b"authored request checked");
            documents
                .prune_history(&host.identity(), &[], 10)
                .await
                .unwrap();
            documents
                .export_database(&file, &KEY.parse().unwrap(), &[], &[])
                .await
                .unwrap();
            let mut snapshot = sqlx::SqliteConnection::connect_with(
                &sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(&file)
                    .read_only(true),
            )
            .await
            .unwrap();
            let after: i64 = sqlx::query_scalar(
                "SELECT length(snapshot) FROM documents WHERE name='preference:example'",
            )
            .fetch_one(&mut snapshot)
            .await
            .unwrap();
            snapshot.close().await.unwrap();
            assert!(after < before);
            host.shutdown().await.unwrap();
            peer.shutdown().await.unwrap();
        })
        .await
        .expect("direct sync did not complete");
    }

    #[tokio::test]
    async fn closing_a_profile_during_sync_does_not_fail_the_next_pairing() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let host_dir = tempfile::tempdir().unwrap();
            let peer_dir = tempfile::tempdir().unwrap();
            let config = |directory: &std::path::Path| NetworkConfig {
                database: directory.join("network.sqlite"),
                name: "Device".into(),
                nearby: false,
                relay: None,
                public_relay: false,
            };
            let (host, mut host_events) = ConnectNetwork::spawn(config(host_dir.path()), SecretKey::generate()).await.unwrap();
            let (peer, mut peer_events) = ConnectNetwork::spawn(config(peer_dir.path()), SecretKey::generate()).await.unwrap();
            let profile_id = ConnectNetwork::new_profile_id();
            host.open_profile(&profile_id, true, Vec::new()).await.unwrap();
            let roster = host.enroll(peer.endpoint.id(), "peer").await.unwrap();
            peer.accept_enrollment(&profile_id, &roster, host.endpoint.id()).await.unwrap();
            peer.open_profile(&profile_id, false, Vec::new()).await.unwrap();
            peer.remember_peer(&host.invitation().await.unwrap()).await.unwrap();
            peer.attach_documents(Arc::new(ProfileStore::open(library::Database::open(&peer_dir.path().join("profile.sqlite")).await.unwrap(), 2, KEY.parse().unwrap()).await.unwrap())).await.unwrap();

            // Hold the server's document attachment until the request has
            // arrived. Closing its profile must interrupt this active sync.
            let serving = host.profile.read().await.clone().unwrap();
            let documents = serving.documents.write().await;
            let syncing = peer.clone();
            let receiving = peer.profile.read().await.clone().unwrap();
            let host_id = host.identity();
            let transfer = tokio::spawn(async move { syncing.sync_peer(&receiving, &host_id).await });
            while host.addresses.get_endpoint_info(peer.endpoint.id()).is_none() {
                tokio::task::yield_now().await;
            }
            host.close_profile().await.unwrap();
            transfer.await.unwrap().unwrap();
            drop(documents);
            peer.close_profile().await.unwrap();
            host.open_profile(&profile_id, false, Vec::new()).await.unwrap();
            peer.begin_pairing(&host.invitation().await.unwrap()).await.unwrap();

            let mut host_session = None;
            let mut peer_session = None;
            let mut added = false;
            let mut paired = false;
            while !added || !paired {
                tokio::select! {
                    event = host_events.recv() => match event.unwrap() {
                        NetworkEvent::Pairing { session, .. } => {
                            host.confirm_pairing(&session, true).await.unwrap();
                            host_session = Some(session);
                        },
                        NetworkEvent::MemberAdded { .. } => added = true,
                        NetworkEvent::Error(error) | NetworkEvent::PairingFailed { error, .. } => panic!("host: {error}"),
                        _ => {},
                    },
                    event = peer_events.recv() => match event.unwrap() {
                        NetworkEvent::Pairing { session, .. } => {
                            peer.confirm_pairing(&session, true).await.unwrap();
                            peer_session = Some(session);
                        },
                        NetworkEvent::Paired { .. } => paired = true,
                        NetworkEvent::Error(error) | NetworkEvent::PairingFailed { error, .. } => panic!("peer: {error}"),
                        _ => {},
                    },
                }
            }
            assert_eq!(host_session, peer_session);
            assert!(host_session.is_some());
            host.shutdown().await.unwrap();
            peer.shutdown().await.unwrap();
        }).await.expect("Profile closure blocked sync or pairing");
    }
}
