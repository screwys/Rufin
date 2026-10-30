use std::{
    collections::{BTreeSet, HashMap},
    fmt,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::Result;
use futures_util::{
    StreamExt,
    stream::{self, BoxStream},
};
use iroh::{
    EndpointId,
    address_lookup::{
        AddressLookup, EndpointData, EndpointInfo, Error, Item, memory::MemoryLookup,
    },
};
use swarm_discovery::{Discoverer, DropGuard, IpClass, Peer};
use tokio::sync::{broadcast, mpsc};

pub(crate) enum Discovery {
    Found(EndpointInfo),
    Expired(EndpointId),
}

/// The nearby address lookup. Multicast discovery runs only between [`Nearby::start`] and
/// [`Nearby::stop`]; the endpoint's address is kept so a new run advertises it at once.
#[derive(Clone)]
pub(crate) struct Nearby(Arc<State>);

struct State {
    id: EndpointId,
    addresses: MemoryLookup,
    found: mpsc::Sender<Discovery>,
    changes: broadcast::Sender<EndpointInfo>,
    discovery: Mutex<Option<DropGuard>>,
    interfaces: Mutex<BTreeSet<Ipv4Addr>>,
    published: Mutex<Option<EndpointData>>,
}

impl fmt::Debug for Nearby {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Nearby").finish_non_exhaustive()
    }
}

impl Nearby {
    pub(crate) fn new(
        id: EndpointId,
        addresses: MemoryLookup,
    ) -> (Self, mpsc::Receiver<Discovery>) {
        let (found, receiver) = mpsc::channel(64);
        let (changes, _) = broadcast::channel(64);
        (
            Self(Arc::new(State {
                id,
                addresses,
                found,
                changes,
                discovery: Mutex::new(None),
                interfaces: Mutex::new(BTreeSet::new()),
                published: Mutex::new(None),
            })),
            receiver,
        )
    }

    pub(crate) fn start(&self) -> Result<()> {
        let mut discovery = self.0.discovery.lock().unwrap();
        if discovery.is_some() {
            return Ok(());
        }
        let (id, addresses, changes, found) = (
            self.0.id,
            self.0.addresses.clone(),
            self.0.changes.clone(),
            self.0.found.clone(),
        );
        let interfaces = multicast_interfaces();
        let guard = Discoverer::new(
            "rufin-connect".into(),
            data_encoding::BASE32_NOPAD
                .encode(self.0.id.as_bytes())
                .to_ascii_lowercase(),
        )
        .with_ip_class(IpClass::Auto)
        .with_multicast_interfaces_v4(interfaces.iter().copied().collect())
        .with_callback(move |peer_id: &str, peer: &Peer| {
            let Ok(peer_id) = peer_id.parse::<EndpointId>() else {
                return;
            };
            if peer_id == id {
                return;
            }
            let event = if peer.is_expiry() {
                Discovery::Expired(peer_id)
            } else {
                let mut data = EndpointData::from(
                    peer.addrs()
                        .iter()
                        .map(|(ip, port)| SocketAddr::new(*ip, *port))
                        .collect::<BTreeSet<_>>(),
                );
                if let Some(Some(relay)) = peer.txt_attribute("relay")
                    && let Ok(relay) = relay.parse()
                {
                    data.add_relay_url(crate::network::canonical_relay(relay));
                }
                data.set_user_data(
                    peer.txt_attribute("user-data")
                        .flatten()
                        .and_then(|name| name.parse().ok()),
                );
                let info = EndpointInfo::from_parts(peer_id, data);
                addresses.add_endpoint_info(info.clone());
                let _ = changes.send(info.clone());
                Discovery::Found(info)
            };
            let found = found.clone();
            tokio::spawn(async move {
                let _ = found.send(event).await;
            });
        })
        .spawn(&tokio::runtime::Handle::current())?;
        *self.0.interfaces.lock().unwrap() = interfaces;
        if let Some(data) = self.0.published.lock().unwrap().as_ref() {
            advertise(&guard, data);
        }
        *discovery = Some(guard);
        Ok(())
    }

    pub(crate) fn stop(&self) {
        self.0.discovery.lock().unwrap().take();
    }
}

impl AddressLookup for Nearby {
    fn resolve(&self, id: EndpointId) -> Option<BoxStream<'static, Result<Item, Error>>> {
        // MemoryLookup supplies known addresses. Keep receiving new routes while connecting.
        let changes = stream::unfold(self.0.changes.subscribe(), |mut receiver| async {
            loop {
                match receiver.recv().await {
                    Ok(info) => return Some((info, receiver)),
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        });
        Some(
            changes
                .take_until(tokio::time::sleep(Duration::from_secs(10)))
                .filter_map(move |info| async move {
                    (info.endpoint_id == id).then(|| Ok(Item::new(info, "nearby", None)))
                })
                .boxed(),
        )
    }

    fn publish(&self, data: &EndpointData) {
        *self.0.published.lock().unwrap() = Some(data.clone());
        let discovery = self.0.discovery.lock().unwrap();
        let Some(discovery) = discovery.as_ref() else {
            return;
        };
        // A VPN can own the default route while nearby peers are on Wi-Fi or Ethernet.
        // Endpoint updates also refresh the interfaces after a network change.
        let interfaces = multicast_interfaces();
        let mut previous = self.0.interfaces.lock().unwrap();
        for ip in previous.difference(&interfaces) {
            discovery.remove_interface_v4(*ip);
        }
        for ip in interfaces.difference(&previous) {
            discovery.add_interface_v4(*ip);
        }
        *previous = interfaces;
        advertise(discovery, data);
    }
}

fn advertise(discovery: &DropGuard, data: &EndpointData) {
    let mut addresses = HashMap::<u16, Vec<IpAddr>>::new();
    for address in data.ip_addrs() {
        addresses
            .entry(address.port())
            .or_default()
            .push(address.ip());
    }
    discovery.remove_all();
    for (port, ips) in addresses {
        discovery.add(port, ips);
    }
    for (key, value) in [
        ("relay", data.relay_urls().next().map(ToString::to_string)),
        ("user-data", data.user_data().map(ToString::to_string)),
    ] {
        if let Some(value) = value {
            if let Err(error) = discovery.set_txt_attribute(key.into(), Some(value)) {
                tracing::warn!(%error, key, "Could not publish nearby device attribute");
            }
        } else {
            discovery.remove_txt_attribute(key.into());
        }
    }
}

fn multicast_interfaces() -> BTreeSet<Ipv4Addr> {
    netdev::get_interfaces()
        .into_iter()
        .filter(|interface| {
            interface.is_up() && interface.is_multicast() && !interface.is_loopback()
        })
        .flat_map(|interface| interface.ipv4.into_iter().map(|address| address.addr()))
        .collect()
}
