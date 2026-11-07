use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, OnceLock, Weak},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use futures_util::{
    StreamExt,
    stream::{self, BoxStream},
};
use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey, TransportAddr, Watcher,
    address_lookup::{AddressLookup, Error, Item},
    endpoint::{ConnectOptions, Connection, ConnectionError, VarInt, presets},
    protocol::{AcceptError, ProtocolHandler, Router},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, RwLock, mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

use crate::{
    media::PeerByteResponse,
    nearby::{Discovery, Nearby},
    pairing,
    store::Store,
    sync::{SyncProtocol, Values},
};

pub(crate) const RPC_PROTOCOL: &[u8] = b"rufin/connect/rpc/2";
pub(crate) const PAIR_PROTOCOL: &[u8] = b"rufin/connect/pair/4";
const INVITE_PROTOCOL: &[u8] = b"rufin/connect/invite/2";
// An older device does not answer a newer protocol at all. Pairing also offers the previous
// versions, so one that picks them is reported as incompatible.
const PREVIOUS_PAIR_PROTOCOL: &[u8] = b"rufin/connect/pair/3";
const PREVIOUS_INVITE_PROTOCOL: &[u8] = b"rufin/connect/invite/1";
const MEDIA_PROTOCOL: &[u8] = b"rufin/connect/media/2";
pub(crate) const SYNC_PROTOCOL: &[u8] = b"rufin/connect/sync/4";
pub(crate) const MAX_FRAME: usize = 16 * 1024 * 1024;
// A device reads a frame of this size only from a member of the profile it admitted, such as
// a sync page or an RPC that carries a whole queue.
pub(crate) const TRUSTED_FRAME: usize = u32::MAX as usize;
// A device that removed this one closes its connections with this code.
const REMOVED: VarInt = VarInt::from_u32(2);

pub const INCOMPATIBLE: &str = localization::msgid(
    "Devices using Rufin Connect are no longer compatible. Please update Rufin.",
);
pub const REMOVED_FROM_PROFILE: &str =
    localization::msgid("This device was removed from the profile");

static DNS_RESOLVER: OnceLock<fn() -> iroh::dns::DnsResolver> = OnceLock::new();

/// Supply the platform's DNS resolver before creating Connect endpoints.
pub fn install_dns_resolver(resolver: fn() -> iroh::dns::DnsResolver) {
    let _ = DNS_RESOLVER.set(resolver);
}

/// The relay a person entered, the only one Connect uses; none without one.
fn relay_mode(address: Option<&str>) -> Result<RelayMode> {
    Ok(match address {
        Some(address) => RelayMode::custom([canonical_relay(address.parse()?)]),
        None => RelayMode::Disabled,
    })
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

/// The routes kept to one device. A sighting puts its routes first and the oldest fall off,
/// so the ports of a device that restarted age out instead of piling up.
const ROUTES: usize = 8;

/// The routes this device knows to others, newest first. Iroh looks devices up here.
#[derive(Clone, Debug, Default)]
pub(crate) struct Routes(Arc<std::sync::Mutex<HashMap<EndpointId, Vec<TransportAddr>>>>);

/// A device's routes as saved, newest first. An `EndpointAddr` keeps its routes sorted, so
/// the saved list keeps the order that decides which fall off.
#[derive(Serialize, Deserialize)]
struct Saved {
    id: EndpointId,
    addrs: Vec<TransportAddr>,
}

impl Routes {
    /// Puts these routes first; true when the routes kept changed.
    pub(crate) fn take(&self, address: EndpointAddr) -> bool {
        let address = canonical_address(address);
        self.put(address.id, address.addrs.into_iter().collect())
    }

    /// Takes routes another device relayed: those not known yet go first, as the newest, and
    /// the known ones keep their places, since a relayed list carries no order of its own.
    /// True when the routes kept changed.
    pub(crate) fn hear(&self, address: EndpointAddr) -> bool {
        let address = canonical_address(address);
        let known = self
            .0
            .lock()
            .unwrap()
            .get(&address.id)
            .cloned()
            .unwrap_or_default();
        let new = address
            .addrs
            .into_iter()
            .filter(|route| !known.contains(route))
            .collect::<Vec<_>>();
        !new.is_empty() && self.put(address.id, new)
    }

    fn put(&self, peer: EndpointId, mut taken: Vec<TransportAddr>) -> bool {
        let mut routes = self.0.lock().unwrap();
        let kept = routes.entry(peer).or_default();
        let before = kept.iter().cloned().collect::<BTreeSet<_>>();
        kept.retain(|route| !taken.contains(route));
        taken.append(kept);
        taken.truncate(ROUTES);
        *kept = taken;
        kept.iter().cloned().collect::<BTreeSet<_>>() != before
    }

    /// Takes the routes saved for a device, in their saved order.
    fn restore(&self, saved: &str) -> Result<()> {
        let saved: Saved = serde_json::from_str(saved)?;
        self.put(saved.id, saved.addrs);
        Ok(())
    }

    /// The routes to a device as saved, newest first.
    fn saved(&self, peer: EndpointId) -> Result<Option<String>> {
        let routes = self.0.lock().unwrap();
        let Some(addrs) = routes.get(&peer).filter(|addrs| !addrs.is_empty()) else {
            return Ok(None);
        };
        Ok(Some(serde_json::to_string(&Saved {
            id: peer,
            addrs: addrs.clone(),
        })?))
    }

    /// The routes to a device. An in-flight lookup can predate a newly learned route, so
    /// connections name them directly.
    pub(crate) fn get(&self, peer: EndpointId) -> EndpointAddr {
        let routes = self.0.lock().unwrap();
        EndpointAddr::from_parts(peer, routes.get(&peer).into_iter().flatten().cloned())
    }
}

impl AddressLookup for Routes {
    fn resolve(&self, id: EndpointId) -> Option<BoxStream<'static, Result<Item, Error>>> {
        let address = self.get(id);
        (!address.is_empty())
            .then(|| stream::iter([Ok(Item::new(address.into(), "connect", None))]).boxed())
    }
}

#[derive(Clone, Debug)]
pub struct NetworkConfig {
    pub name: String,
    pub nearby: bool,
    pub relay: Option<String>,
}

/// What this device serves while Connect is on.
pub struct Profile {
    pub id: String,
    /// Handed to a joining device; secret fields are sealed to it.
    pub key: String,
    pub store: Store,
    pub values: Arc<dyn Values>,
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
    /// This device joined the host `peer`'s profile.
    Paired {
        session: String,
        profile: String,
        key: String,
        peer: String,
        name: String,
    },
    MemberAdded {
        peer: String,
        name: String,
    },
    Request {
        peer: String,
        body: Vec<u8>,
        reply: oneshot::Sender<Result<Vec<u8>, String>>,
    },
    StreamRequest {
        peer: String,
        body: Vec<u8>,
        reply: oneshot::Sender<Result<PeerByteResponse, String>>,
    },
    /// `peer`'s records reached `sequence`. The device that pushed waits until `pulled` is
    /// answered or dropped.
    Changed {
        peer: String,
        sequence: i64,
        pulled: oneshot::Sender<()>,
    },
    /// `peer` answered, or `contacted` this device.
    Reachable {
        peer: String,
        contacted: bool,
    },
    /// `peer` refused this device because it was removed from the profile, as it knew the
    /// device when it was `paired`.
    Removed {
        peer: String,
        paired: i64,
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

/// Iroh owns encrypted connections. The core owns which devices are trusted.
pub struct ConnectNetwork {
    pub(crate) endpoint: Endpoint,
    pub(crate) name: RwLock<String>,
    pub(crate) profile: RwLock<Option<Arc<Profile>>>,
    // Device id and whether it is trusted, from Connect's device records.
    devices: std::sync::RwLock<HashMap<String, (bool, i64)>>,
    pub(crate) approvals: Mutex<HashMap<String, watch::Sender<Option<bool>>>>,
    pub(crate) events: mpsc::Sender<NetworkEvent>,
    routes: Routes,
    nearby: bool,
    stop: CancellationToken,
    relay: RelayMode,
    router: Mutex<Option<Router>>,
    pairing_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl ConnectNetwork {
    pub async fn spawn(
        config: NetworkConfig,
        credentials: SecretKey,
    ) -> Result<(Arc<Self>, mpsc::Receiver<NetworkEvent>)> {
        let routes = Routes::default();
        let relay = relay_mode(config.relay.as_deref())?;
        let mut builder = Endpoint::builder(presets::Minimal)
            .secret_key(credentials.clone())
            .address_lookup(routes.clone())
            .relay_mode(relay.clone());
        if let Some(resolver) = DNS_RESOLVER.get() {
            builder = builder.dns_resolver(resolver());
        }
        let mut nearby_error = None;
        let discoveries = if config.nearby {
            match Nearby::new(credentials.public(), routes.clone()) {
                Ok((nearby, discoveries)) => {
                    builder = builder.address_lookup(nearby);
                    Some(discoveries)
                }
                Err(error) => {
                    nearby_error = Some(format!("Nearby discovery is unavailable: {error}"));
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
        let (events, receiver) = mpsc::channel(64);
        let network = Arc::new(Self {
            endpoint,
            name: RwLock::new(config.name),
            profile: RwLock::new(None),
            devices: std::sync::RwLock::new(HashMap::new()),
            approvals: Mutex::new(HashMap::new()),
            events,
            routes,
            nearby: config.nearby,
            stop: CancellationToken::new(),
            relay,
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
        if let Some(error) = nearby_error {
            let _ = network.events.send(NetworkEvent::Error(error)).await;
        }
        if let Some(discoveries) = discoveries {
            network.run_nearby(discoveries);
        }
        let mut addresses = network.endpoint.watch_addr().stream();
        let stop = network.stop.clone();
        tokio::spawn(async move {
            loop {
                let address = tokio::select! { _ = stop.cancelled() => break, address = addresses.next() => address };
                let Some(address) = address else { break };
                let Some(network) = weak.upgrade() else {
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
        Ok((network, receiver))
    }

    fn run_nearby(self: &Arc<Self>, mut discoveries: mpsc::UnboundedReceiver<Discovery>) {
        let weak = Arc::downgrade(self);
        let stop = self.stop.clone();
        tokio::spawn(async move {
            loop {
                let event = tokio::select! { _ = stop.cancelled() => break, event = discoveries.recv() => event };
                let Some(event) = event else {
                    break;
                };
                let Some(network) = weak.upgrade() else { break };
                let event = match event {
                    Discovery::Expired(peer) => NetworkEvent::DiscoveryExpired {
                        peer: peer.to_string(),
                    },
                    Discovery::Found(info) => {
                        if network.trusted(info.endpoint_id) == Some(true) {
                            network.save(info.endpoint_id).await;
                        }
                        NetworkEvent::Discovered {
                            peer: info.endpoint_id.to_string(),
                            name: info
                                .data
                                .user_data()
                                .map(ToString::to_string)
                                .unwrap_or_else(|| info.endpoint_id.to_string()),
                        }
                    }
                };
                let _ = network.events.send(event).await;
            }
        });
    }

    pub fn identity(&self) -> String {
        self.endpoint.id().to_string()
    }

    pub async fn rename(&self, name: String) {
        self.endpoint
            .set_user_data_for_address_lookup(name.parse().ok());
        *self.name.write().await = name;
    }

    pub fn matches_configuration(&self, nearby: bool, relay: Option<&str>) -> Result<bool> {
        Ok(self.nearby == nearby && self.relay == relay_mode(relay)?)
    }

    pub async fn invitation(&self) -> Result<String> {
        Ok(serde_json::to_string(&self.endpoint.addr())?)
    }

    /// Serve this device's records while Connect is on; `None` once it turns off.
    pub async fn serve(&self, profile: Option<Profile>) -> Result<()> {
        if let Some(profile) = &profile {
            for peer in profile.store.peers().await? {
                if let Some(address) = &peer.address {
                    self.routes.restore(address)?;
                }
                // A joining device reached its host before it had records to keep it in.
                if let Some(known) = self.routes.saved(peer.device.parse()?)?
                    && peer.address.as_ref() != Some(&known)
                {
                    profile.store.set_address(&peer.device, &known).await?;
                }
            }
        }
        *self.profile.write().await = profile.map(Arc::new);
        Ok(())
    }

    /// The profile's devices, whether each is trusted and when it was last paired, from
    /// Connect's device records.
    pub fn set_devices(&self, devices: HashMap<String, (bool, i64)>) {
        *self.devices.write().unwrap() = devices;
    }

    fn device(&self, peer: EndpointId) -> Option<(bool, i64)> {
        self.devices.read().unwrap().get(&peer.to_string()).copied()
    }

    fn trusted(&self, peer: EndpointId) -> Option<bool> {
        self.device(peer).map(|(trusted, _)| trusted)
    }

    pub(crate) fn trusted_devices(&self) -> Vec<String> {
        let identity = self.identity();
        self.devices
            .read()
            .unwrap()
            .iter()
            .filter(|(peer, (trusted, _))| *trusted && **peer != identity)
            .map(|(peer, _)| peer.clone())
            .collect()
    }

    /// Lets a trusted device in. A removed one is told so, with the pairing its removal
    /// ended; an unknown one is closed.
    pub(crate) async fn admit(&self, connection: &Connection) -> bool {
        match self.device(connection.remote_id()) {
            Some((true, _)) => {
                self.reached(connection, true).await;
                true
            }
            Some((false, paired)) => {
                connection.close(REMOVED, paired.to_string().as_bytes());
                false
            }
            None => {
                connection.close(1u32.into(), b"not a member");
                false
            }
        }
    }

    pub(crate) async fn connect(&self, peer: &str, protocol: &[u8]) -> Result<Connection> {
        let peer: EndpointId = peer.parse()?;
        ensure!(
            self.trusted(peer) == Some(true),
            "Device is not a member of this profile"
        );
        Ok(self
            .endpoint
            .connect(self.routes.get(peer), protocol)
            .await?)
    }

    /// Ends an exchange with a trusted device: a refusal because this device was removed is
    /// reported, and a device that answered is remembered where it answered from.
    pub(crate) async fn settle<T>(&self, connection: &Connection, result: Result<T>) -> Result<T> {
        if let Some(ConnectionError::ApplicationClosed(close)) = connection.close_reason()
            && close.error_code == REMOVED
        {
            let _ = self
                .events
                .send(NetworkEvent::Removed {
                    peer: connection.remote_id().to_string(),
                    paired: std::str::from_utf8(&close.reason)
                        .ok()
                        .and_then(|paired| paired.parse().ok())
                        .unwrap_or(i64::MAX),
                })
                .await;
            bail!(localization::tr(REMOVED_FROM_PROFILE));
        }
        if result.is_ok() {
            self.reached(connection, false).await;
        }
        result
    }

    // A device that restarted listens on another port; its own connections tell where.
    async fn reached(&self, connection: &Connection, contacted: bool) {
        let peer = connection.remote_id();
        let _ = self
            .events
            .send(NetworkEvent::Reachable {
                peer: peer.to_string(),
                contacted,
            })
            .await;
        if let Some(address) = connection
            .paths()
            .iter()
            .find(|path| path.is_selected())
            .map(|path| path.remote_addr().clone())
        {
            self.remember(EndpointAddr::from_parts(peer, [address]))
                .await;
        }
    }

    /// Takes routes to a trusted device; true when they changed the routes it keeps. The device
    /// keeps them for its next start.
    async fn remember(&self, address: EndpointAddr) -> bool {
        let peer = address.id;
        if !self.routes.take(address) {
            return false;
        }
        self.save(peer).await;
        true
    }

    async fn save(&self, peer: EndpointId) {
        if let Some(profile) = self.profile.read().await.as_ref() {
            let saved = match self.routes.saved(peer) {
                Ok(Some(address)) => profile.store.set_address(&peer.to_string(), &address).await,
                Ok(None) => Ok(()),
                Err(error) => Err(error),
            };
            if let Err(error) = saved {
                tracing::warn!(%error, "Could not save a Connect device address");
            }
        }
    }

    /// The routes this device knows to the profile's other devices.
    pub fn addresses(&self) -> Vec<EndpointAddr> {
        self.trusted_devices()
            .into_iter()
            .filter_map(|peer| peer.parse().ok())
            .map(|peer| self.routes.get(peer))
            .filter(|address| !address.is_empty())
            .collect()
    }

    /// Takes the routes another device knows to the profile's devices; returns the devices
    /// reachable on routes this one did not know.
    pub async fn learn(&self, addresses: Vec<EndpointAddr>) -> Vec<String> {
        let mut learned = Vec::new();
        for address in addresses {
            let peer = address.id;
            if peer != self.endpoint.id()
                && self.trusted(peer) == Some(true)
                && self.routes.hear(address)
            {
                self.save(peer).await;
                learned.push(peer.to_string());
            }
        }
        learned
    }

    fn remember_invitation(&self, invitation: &str) -> Result<EndpointId> {
        if let Ok(id) = invitation.parse::<EndpointId>() {
            return Ok(id);
        }
        let address: EndpointAddr =
            serde_json::from_str(invitation).context("Invalid Connect invitation")?;
        let id = address.id;
        self.routes.take(address);
        Ok(id)
    }

    pub async fn begin_pairing(self: &Arc<Self>, invitation: &str) -> Result<()> {
        self.start_pairing(invitation, true).await
    }

    pub async fn invite(self: &Arc<Self>, peer: &str) -> Result<()> {
        ensure!(
            self.profile.read().await.is_some(),
            "Rufin Connect is disabled"
        );
        self.start_pairing(peer, false).await
    }

    async fn start_pairing(self: &Arc<Self>, invitation: &str, joining: bool) -> Result<()> {
        let peer = self.remember_invitation(invitation)?;
        self.cancel_pairing().await;
        let network = self.clone();
        *self.pairing_task.lock().await = Some(tokio::spawn(async move {
            let (protocol, previous) = if joining {
                (PAIR_PROTOCOL, PREVIOUS_PAIR_PROTOCOL)
            } else {
                (INVITE_PROTOCOL, PREVIOUS_INVITE_PROTOCOL)
            };
            let connected = async {
                let connection = network
                    .endpoint
                    .connect_with_opts(
                        network.routes.get(peer),
                        protocol,
                        ConnectOptions::new().with_additional_alpns(vec![previous.to_vec()]),
                    )
                    .await?
                    .await?;
                if connection.alpn() != protocol {
                    connection.close(1u32.into(), b"incompatible");
                    bail!(localization::tr(INCOMPATIBLE));
                }
                Ok(connection)
            };
            match connected.await {
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
        self.exchange(peer, &body, TRUSTED_FRAME, Duration::from_secs(30))
            .await
            .map(|(response, _)| response)
    }

    /// One RPC exchange, connecting within `deadline`, with frames up to `frame` bytes: the
    /// answer and the path the connection took.
    async fn exchange(
        &self,
        peer: &str,
        body: &[u8],
        frame: usize,
        deadline: Duration,
    ) -> Result<(Vec<u8>, Option<iroh::TransportAddr>)> {
        let connection = tokio::time::timeout(deadline, self.connect(peer, RPC_PROTOCOL))
            .await
            .context("Device is unreachable")??;
        let answered = async {
            let (mut send, mut recv) = connection.open_bi().await?;
            write_frame(&mut send, &body, frame).await?;
            send.finish()?;
            read_frame::<Result<Vec<u8>, String>>(&mut recv, frame).await
        }
        .await;
        let response = self.settle(&connection, answered).await;
        let path = connection
            .paths()
            .iter()
            .find(|path| path.is_selected())
            .map(|path| path.remote_addr().clone());
        connection.close(0u32.into(), b"complete");
        Ok((response?.map_err(anyhow::Error::msg)?, path))
    }

    pub async fn stream_request(&self, peer: &str, body: Vec<u8>) -> Result<PeerByteResponse> {
        let connection =
            tokio::time::timeout(Duration::from_secs(30), self.connect(peer, MEDIA_PROTOCOL))
                .await
                .context("Device is unreachable")??;
        let answered = async {
            let (mut send, mut recv) = connection.open_bi().await?;
            write_frame(&mut send, &body, MAX_FRAME).await?;
            send.finish()?;
            let metadata: Result<serde_json::Value, String> =
                read_frame(&mut recv, MAX_FRAME).await?;
            Ok((metadata, recv))
        }
        .await;
        let (metadata, recv) = self.settle(&connection, answered).await?;
        Ok(PeerByteResponse {
            metadata: metadata.map_err(anyhow::Error::msg)?,
            reader: Box::pin(MediaReader {
                _connection: connection,
                recv,
            }),
        })
    }

    pub async fn test_connection(&self, peer: &str, body: Vec<u8>) -> Result<(String, Vec<u8>)> {
        // A test answers within its deadline, not only connects.
        let deadline = Duration::from_secs(15);
        let (response, path) =
            tokio::time::timeout(deadline, self.exchange(peer, &body, MAX_FRAME, deadline))
                .await??;
        let route = path
            .map(|address| match address {
                iroh::TransportAddr::Relay(_) => localization::msgid("Via relay"),
                iroh::TransportAddr::Ip(address) => match address.ip() {
                    std::net::IpAddr::V4(ip)
                        if ip.is_private() || ip.is_loopback() || ip.is_link_local() =>
                    {
                        localization::msgid("Local network")
                    }
                    std::net::IpAddr::V6(ip)
                        if ip.is_unique_local()
                            || ip.is_loopback()
                            || ip.is_unicast_link_local() =>
                    {
                        localization::msgid("Local network")
                    }
                    _ => localization::msgid("Direct"),
                },
                _ => localization::msgid("Direct"),
            })
            .map_or_else(|| localization::tr("Waiting for device"), localization::tr);
        Ok((route, response))
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.stop.cancel();
        self.cancel_pairing().await;
        self.approvals.lock().await.clear();
        *self.profile.write().await = None;
        if let Some(router) = self.router.lock().await.take() {
            router.shutdown().await?;
        }
        Ok(())
    }
}

impl Drop for ConnectNetwork {
    fn drop(&mut self) {
        self.stop.cancel();
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
        if !network.admit(&connection).await {
            return Ok(());
        }
        let result: Result<()> = async {
            let (mut send, mut recv) = connection.accept_bi().await?;
            let body = read_frame(&mut recv, MAX_FRAME).await?;
            let (reply, response) = oneshot::channel();
            network
                .events
                .send(NetworkEvent::StreamRequest {
                    peer: connection.remote_id().to_string(),
                    body,
                    reply,
                })
                .await
                .map_err(|_| anyhow::anyhow!("Connect stopped"))?;
            let transfer = async {
                match response.await? {
                    Ok(mut audio) => {
                        write_frame(&mut send, &Ok::<_, String>(audio.metadata), MAX_FRAME).await?;
                        tokio::io::copy(&mut audio.reader, &mut send).await?;
                    }
                    Err(error) => {
                        write_frame(&mut send, &Err::<serde_json::Value, _>(error), MAX_FRAME)
                            .await?
                    }
                }
                send.finish()?;
                let _ = send.stopped().await;
                Ok::<_, anyhow::Error>(())
            };
            tokio::select! {
                result = transfer => result,
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
        if !network.admit(&conn).await {
            return Ok(());
        }
        let result: Result<()> = async {
            let (mut send, mut recv) = conn.accept_bi().await?;
            let body = read_frame(&mut recv, TRUSTED_FRAME).await?;
            let (reply, answer) = oneshot::channel();
            network
                .events
                .send(NetworkEvent::Request {
                    peer: conn.remote_id().to_string(),
                    body,
                    reply,
                })
                .await
                .map_err(|_| anyhow::anyhow!("Connect stopped"))?;
            write_frame(&mut send, &answer.await?, TRUSTED_FRAME).await?;
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

/// Writes one frame of at most `limit` bytes, the most its reader accepts.
pub(crate) async fn write_frame<T: Serialize>(
    send: &mut (impl tokio::io::AsyncWrite + Unpin),
    value: &T,
    limit: usize,
) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(
        bytes.len() <= limit,
        "Connect message exceeds transfer limit"
    );
    send.write_all(&(bytes.len() as u32).to_be_bytes()).await?;
    send.write_all(&bytes).await?;
    Ok(())
}

/// Reads one frame of at most `limit` bytes.
pub(crate) async fn read_frame<T: DeserializeOwned>(
    recv: &mut (impl tokio::io::AsyncRead + Unpin),
    limit: usize,
) -> Result<T> {
    let length = recv.read_u32().await? as usize;
    ensure!(length <= limit, "Connect message exceeds transfer limit");
    // The buffer grows with the bytes that arrive, not with the length the device claims.
    let mut bytes = Vec::with_capacity(length.min(MAX_FRAME));
    (&mut *recv)
        .take(length as u64)
        .read_to_end(&mut bytes)
        .await?;
    ensure!(bytes.len() == length, "Connect message ended early");
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn relay_aliases_share_one_address_and_disabled_transport_stays_direct() {
        let (network, _events) = ConnectNetwork::spawn(
            NetworkConfig {
                name: "Direct device".into(),
                nearby: false,
                relay: None,
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
        network
            .remember_invitation(&serde_json::to_string(&address).unwrap())
            .unwrap();
        let remembered = network.routes.get(peer.id());
        assert_eq!(remembered.relay_urls().count(), 1);
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
}
