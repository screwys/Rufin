use std::{
    collections::HashMap,
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
    endpoint::{ConnectOptions, Connection, ConnectionError, VarInt, presets},
    protocol::{AcceptError, ProtocolHandler, Router},
};
use serde::{Serialize, de::DeserializeOwned};
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
// A device that removed this one closes its connections with this code.
const REMOVED: VarInt = VarInt::from_u32(2);

pub const INCOMPATIBLE: &str =
    "Devices using Rufin Connect are no longer compatible. Please update Rufin.";
pub const REMOVED_FROM_PROFILE: &str = "This device was removed from the profile";

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

#[derive(Clone, Debug)]
pub struct NetworkConfig {
    pub name: String,
    pub nearby: bool,
    pub relay: Option<String>,
    pub public_relay: bool,
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
    Reachable {
        peer: String,
    },
    /// `peer` refused this device because it was removed from the profile.
    Removed {
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

/// Iroh owns encrypted connections. The core owns which devices are trusted.
pub struct ConnectNetwork {
    pub(crate) endpoint: Endpoint,
    pub(crate) name: RwLock<String>,
    pub(crate) profile: RwLock<Option<Arc<Profile>>>,
    // Device id and whether it is trusted, from Connect's device records.
    devices: std::sync::RwLock<HashMap<String, bool>>,
    pub(crate) approvals: Mutex<HashMap<String, watch::Sender<Option<bool>>>>,
    pub(crate) events: mpsc::Sender<NetworkEvent>,
    addresses: MemoryLookup,
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
        let addresses = MemoryLookup::new();
        let relay = relay_mode(config.relay.as_deref(), config.public_relay)?;
        let mut builder = Endpoint::builder(presets::Minimal)
            .secret_key(credentials.clone())
            .address_lookup(addresses.clone())
            .relay_mode(relay.clone());
        if let Some(resolver) = DNS_RESOLVER.get() {
            builder = builder.dns_resolver(resolver());
        }
        let mut nearby_error = None;
        let discoveries = if config.nearby {
            match Nearby::new(credentials.public(), addresses.clone()) {
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
        let dns = endpoint.dns_resolver()?.clone();
        endpoint.address_lookup()?.add(CanonicalRelays(
            PkarrResolver::n0_dns()
                .dns_resolver(dns.clone())
                .build(endpoint.tls_config().clone()),
        ));
        endpoint.address_lookup()?.add(CanonicalRelays(
            DnsAddressLookup::n0_dns().dns_resolver(dns.clone()).build(),
        ));
        // Only public relays publish this device's address to n0's directory.
        if config.public_relay && config.relay.is_none() {
            endpoint.address_lookup()?.add(PublicAddressPublisher(
                PkarrPublisher::n0_dns()
                    .dns_resolver(dns)
                    .build(credentials.clone(), endpoint.tls_config().clone()),
            ));
        }
        let (events, receiver) = mpsc::channel(64);
        let network = Arc::new(Self {
            endpoint,
            name: RwLock::new(config.name),
            profile: RwLock::new(None),
            devices: std::sync::RwLock::new(HashMap::new()),
            approvals: Mutex::new(HashMap::new()),
            events,
            addresses,
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

    fn run_nearby(self: &Arc<Self>, mut discoveries: mpsc::Receiver<Discovery>) {
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
                    Discovery::Found(info) => NetworkEvent::Discovered {
                        peer: info.endpoint_id.to_string(),
                        name: info
                            .data
                            .user_data()
                            .map(ToString::to_string)
                            .unwrap_or_else(|| info.endpoint_id.to_string()),
                    },
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

    pub fn matches_configuration(
        &self,
        nearby: bool,
        relay: Option<&str>,
        public: bool,
    ) -> Result<bool> {
        Ok(self.nearby == nearby && self.relay == relay_mode(relay, public)?)
    }

    pub async fn invitation(&self) -> Result<String> {
        Ok(serde_json::to_string(&self.endpoint.addr())?)
    }

    /// Serve this device's records while Connect is on; `None` once it turns off.
    pub async fn serve(&self, profile: Option<Profile>) -> Result<()> {
        if let Some(profile) = &profile {
            for peer in profile.store.peers().await? {
                if let Some(address) = &peer.address {
                    self.addresses
                        .add_endpoint_info(canonical_address(serde_json::from_str(address)?));
                }
                // A joining device reached its host before it had records to keep it in.
                let known = peer_address(&self.addresses, peer.device.parse()?);
                if !known.is_empty() {
                    let known = serde_json::to_string(&known)?;
                    if peer.address.as_ref() != Some(&known) {
                        profile.store.set_address(&peer.device, &known).await?;
                    }
                }
            }
        }
        *self.profile.write().await = profile.map(Arc::new);
        Ok(())
    }

    /// The profile's devices and whether each is trusted, from Connect's device records.
    pub fn set_devices(&self, devices: HashMap<String, bool>) {
        *self.devices.write().unwrap() = devices;
    }

    fn trusted(&self, peer: EndpointId) -> Option<bool> {
        self.devices.read().unwrap().get(&peer.to_string()).copied()
    }

    pub(crate) fn trusted_devices(&self) -> Vec<String> {
        let identity = self.identity();
        self.devices
            .read()
            .unwrap()
            .iter()
            .filter(|(peer, trusted)| **trusted && **peer != identity)
            .map(|(peer, _)| peer.clone())
            .collect()
    }

    /// Lets a trusted device in. A removed one is told so; an unknown one is closed.
    pub(crate) async fn admit(&self, connection: &Connection) -> bool {
        match self.trusted(connection.remote_id()) {
            Some(true) => {
                self.reached(connection).await;
                true
            }
            Some(false) => {
                connection.close(REMOVED, b"device removed");
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
            .connect(peer_address(&self.addresses, peer), protocol)
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
                })
                .await;
            bail!(REMOVED_FROM_PROFILE);
        }
        if result.is_ok() {
            self.reached(connection).await;
        }
        result
    }

    // A device that restarted listens on another port; its own connections tell where.
    async fn reached(&self, connection: &Connection) {
        let peer = connection.remote_id();
        let _ = self
            .events
            .send(NetworkEvent::Reachable {
                peer: peer.to_string(),
            })
            .await;
        let Some(address) = connection
            .paths()
            .iter()
            .find(|path| path.is_selected())
            .map(|path| path.remote_addr().clone())
        else {
            return;
        };
        let before = self
            .addresses
            .get_endpoint_info(peer)
            .map(EndpointAddr::from);
        self.addresses
            .add_endpoint_info(canonical_address(EndpointAddr::from_parts(peer, [address])));
        let after = peer_address(&self.addresses, peer);
        if before.as_ref() == Some(&after) {
            return;
        }
        if let Some(profile) = self.profile.read().await.as_ref() {
            let saved = match serde_json::to_string(&after) {
                Ok(address) => profile.store.set_address(&peer.to_string(), &address).await,
                Err(error) => Err(error.into()),
            };
            if let Err(error) = saved {
                tracing::warn!(%error, "Could not save a Connect device address");
            }
        }
    }

    fn remember_invitation(&self, invitation: &str) -> Result<EndpointId> {
        if let Ok(id) = invitation.parse::<EndpointId>() {
            return Ok(id);
        }
        let address: EndpointAddr =
            serde_json::from_str(invitation).context("Invalid Connect invitation")?;
        self.addresses
            .add_endpoint_info(canonical_address(address.clone()));
        Ok(address.id)
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
                        peer_address(&network.addresses, peer),
                        protocol,
                        ConnectOptions::new().with_additional_alpns(vec![previous.to_vec()]),
                    )
                    .await?
                    .await?;
                if connection.alpn() != protocol {
                    connection.close(1u32.into(), b"incompatible");
                    bail!(INCOMPATIBLE);
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
        let connection =
            tokio::time::timeout(Duration::from_secs(30), self.connect(peer, RPC_PROTOCOL))
                .await
                .context("Device is unreachable")??;
        let answered = async {
            let (mut send, mut recv) = connection.open_bi().await?;
            write_frame(&mut send, &body, MAX_FRAME).await?;
            send.finish()?;
            read_frame::<Result<Vec<u8>, String>>(&mut recv, MAX_FRAME).await
        }
        .await;
        let response = self.settle(&connection, answered).await?;
        connection.close(0u32.into(), b"complete");
        response.map_err(anyhow::Error::msg)
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
        tokio::time::timeout(Duration::from_secs(15), async {
            let connection = self.connect(peer, RPC_PROTOCOL).await?;
            let answered = async {
                let (mut send, mut recv) = connection.open_bi().await?;
                write_frame(&mut send, &body, MAX_FRAME).await?;
                send.finish()?;
                read_frame::<Result<Vec<u8>, String>>(&mut recv, MAX_FRAME).await
            }
            .await;
            let response = self
                .settle(&connection, answered)
                .await?
                .map_err(anyhow::Error::msg)?;
            let route = connection
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
            connection.close(0u32.into(), b"connection test complete");
            Ok((route, response))
        })
        .await?
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
            let body = read_frame(&mut recv, MAX_FRAME).await?;
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
            write_frame(&mut send, &answer.await?, MAX_FRAME).await?;
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
        network
            .remember_invitation(&serde_json::to_string(&address).unwrap())
            .unwrap();
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
}
