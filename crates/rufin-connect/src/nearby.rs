use std::{
    collections::{BTreeSet, HashMap},
    fmt,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Mutex,
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

pub(crate) struct Nearby {
    discovery: DropGuard,
    interfaces: Mutex<BTreeSet<Ipv4Addr>>,
    changes: broadcast::Sender<EndpointInfo>,
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
    ) -> Result<(Self, mpsc::Receiver<Discovery>)> {
        let (sender, receiver) = mpsc::channel(64);
        let (changes, _) = broadcast::channel(64);
        let discovered = changes.clone();
        let interfaces = multicast_interfaces();
        let discovery = Discoverer::new_interactive(
            "rufin-connect".into(),
            data_encoding::BASE32_NOPAD
                .encode(id.as_bytes())
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
                let _ = discovered.send(info.clone());
                Discovery::Found(info)
            };
            let sender = sender.clone();
            tokio::spawn(async move {
                let _ = sender.send(event).await;
            });
        })
        .spawn(&tokio::runtime::Handle::current())?;
        Ok((
            Self {
                discovery,
                interfaces: Mutex::new(interfaces),
                changes,
            },
            receiver,
        ))
    }
}

impl AddressLookup for Nearby {
    fn resolve(&self, id: EndpointId) -> Option<BoxStream<'static, Result<Item, Error>>> {
        // MemoryLookup supplies known addresses. Keep receiving new routes while connecting.
        let changes = stream::unfold(self.changes.subscribe(), |mut receiver| async {
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
        // A VPN can own the default route while nearby peers are on Wi-Fi or Ethernet.
        // Endpoint updates also refresh the interfaces after a network change.
        let interfaces = multicast_interfaces();
        let mut previous = self.interfaces.lock().unwrap();
        for ip in previous.difference(&interfaces) {
            self.discovery.remove_interface_v4(*ip);
        }
        for ip in interfaces.difference(&previous) {
            self.discovery.add_interface_v4(*ip);
        }
        *previous = interfaces;

        let mut addresses = HashMap::<u16, Vec<IpAddr>>::new();
        for address in data.ip_addrs() {
            addresses
                .entry(address.port())
                .or_default()
                .push(address.ip());
        }
        self.discovery.remove_all();
        for (port, ips) in addresses {
            self.discovery.add(port, ips);
        }
        for (key, value) in [
            ("relay", data.relay_urls().next().map(ToString::to_string)),
            ("user-data", data.user_data().map(ToString::to_string)),
        ] {
            if let Some(value) = value {
                if let Err(error) = self.discovery.set_txt_attribute(key.into(), Some(value)) {
                    tracing::warn!(%error, key, "Could not publish nearby device attribute");
                }
            } else {
                self.discovery.remove_txt_attribute(key.into());
            }
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
