use std::{
    collections::{BTreeSet, HashMap},
    fmt,
    net::{IpAddr, SocketAddr},
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
    address_lookup::{AddressLookup, EndpointData, EndpointInfo, Error, Item},
};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use tokio::sync::{broadcast, mpsc};

use crate::network::Routes;

/// A device publishes one instance per port of its direct addresses, named `<id>-<port>`.
const SERVICE: &str = "_rufin-connect._udp.local.";

pub(crate) enum Discovery {
    /// A device seen for the first time since it expired, or at new addresses.
    Found(EndpointInfo),
    Expired(EndpointId),
}

/// The nearby address lookup. Multicast discovery, on every interface, runs while the
/// endpoint does.
pub(crate) struct Nearby {
    daemon: ServiceDaemon,
    name: String,
    published: Mutex<BTreeSet<String>>,
    changes: broadcast::Sender<EndpointInfo>,
}

impl fmt::Debug for Nearby {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Nearby").finish_non_exhaustive()
    }
}

impl Drop for Nearby {
    fn drop(&mut self) {
        let _ = self.daemon.shutdown();
    }
}

/// What one instance of a device announces.
#[derive(Default)]
struct Instance {
    addrs: BTreeSet<SocketAddr>,
    relay: Option<String>,
    user_data: Option<String>,
}

impl Nearby {
    pub(crate) fn new(
        id: EndpointId,
        routes: Routes,
    ) -> Result<(Self, mpsc::UnboundedReceiver<Discovery>)> {
        let (found, receiver) = mpsc::unbounded_channel();
        let (changes, _) = broadcast::channel(64);
        let discovered = changes.clone();
        let daemon = ServiceDaemon::new()?;
        let events = daemon.browse(SERVICE)?;
        std::thread::Builder::new()
            .name("connect-nearby".into())
            .spawn(move || {
                let mut instances = HashMap::<EndpointId, HashMap<String, Instance>>::new();
                // Devices answer every query; only what changed is news.
                let mut seen = HashMap::<EndpointId, EndpointData>::new();
                while let Ok(event) = events.recv() {
                    let peer = match event {
                        ServiceEvent::ServiceResolved(service) => {
                            let Some(peer) = device(&service.fullname) else {
                                continue;
                            };
                            let property = |key: &str| {
                                service
                                    .get_property_val_str(key)
                                    .filter(|value| !value.is_empty())
                                    .map(str::to_owned)
                            };
                            let instance = Instance {
                                addrs: service
                                    .get_addresses()
                                    .iter()
                                    .map(|ip| SocketAddr::new(ip.to_ip_addr(), service.port))
                                    .collect(),
                                relay: property("relay"),
                                user_data: property("user-data"),
                            };
                            instances
                                .entry(peer)
                                .or_default()
                                .insert(service.fullname.clone(), instance);
                            peer
                        }
                        ServiceEvent::ServiceRemoved(_, fullname) => {
                            let Some(peer) = device(&fullname) else {
                                continue;
                            };
                            if let Some(parts) = instances.get_mut(&peer) {
                                parts.remove(&fullname);
                                if parts.is_empty() {
                                    instances.remove(&peer);
                                }
                            }
                            peer
                        }
                        _ => continue,
                    };
                    if peer == id {
                        continue;
                    }
                    let event = match instances.get(&peer) {
                        None => {
                            if seen.remove(&peer).is_none() {
                                continue;
                            }
                            Discovery::Expired(peer)
                        }
                        Some(parts) => {
                            let mut data = EndpointData::from(
                                parts
                                    .values()
                                    .flat_map(|part| part.addrs.iter().copied())
                                    .collect::<BTreeSet<_>>(),
                            );
                            if let Some(relay) = parts
                                .values()
                                .find_map(|part| part.relay.as_deref()?.parse().ok())
                            {
                                data.add_relay_url(crate::network::canonical_relay(relay));
                            }
                            data.set_user_data(
                                parts
                                    .values()
                                    .find_map(|part| part.user_data.as_deref()?.parse().ok()),
                            );
                            if seen.get(&peer) == Some(&data) {
                                continue;
                            }
                            seen.insert(peer, data.clone());
                            let info = EndpointInfo::from_parts(peer, data);
                            routes.take(info.to_endpoint_addr());
                            let _ = discovered.send(info.clone());
                            Discovery::Found(info)
                        }
                    };
                    let _ = found.send(event);
                }
            })?;
        Ok((
            Self {
                daemon,
                name: data_encoding::BASE32_NOPAD
                    .encode(id.as_bytes())
                    .to_ascii_lowercase(),
                published: Mutex::default(),
                changes,
            },
            receiver,
        ))
    }
}

/// The device an instance's full name belongs to.
fn device(fullname: &str) -> Option<EndpointId> {
    let instance = fullname.strip_suffix(SERVICE)?.strip_suffix('.')?;
    instance.rsplit_once('-')?.0.parse().ok()
}

impl AddressLookup for Nearby {
    fn resolve(&self, id: EndpointId) -> Option<BoxStream<'static, Result<Item, Error>>> {
        // Routes supply known addresses. Keep receiving new routes while connecting.
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
        let mut ports = HashMap::<u16, Vec<IpAddr>>::new();
        for address in data.ip_addrs() {
            ports.entry(address.port()).or_default().push(address.ip());
        }
        let properties = [
            ("relay", data.relay_urls().next().map(ToString::to_string)),
            ("user-data", data.user_data().map(ToString::to_string)),
        ]
        .into_iter()
        .filter_map(|(key, value)| Some((key, value?)))
        .collect::<Vec<_>>();
        let mut published = self.published.lock().unwrap();
        let mut current = BTreeSet::new();
        for (port, ips) in ports {
            let instance = format!("{}-{port}", self.name);
            let info = ServiceInfo::new(
                SERVICE,
                &instance,
                &format!("{instance}.local."),
                &ips[..],
                port,
                &properties[..],
            )
            .and_then(|info| {
                current.insert(info.get_fullname().to_owned());
                self.daemon.register(info)
            });
            if let Err(error) = info {
                tracing::warn!(%error, port, "Could not publish this device nearby");
            }
        }
        for gone in published.difference(&current) {
            let _ = self.daemon.unregister(gone);
        }
        *published = current;
    }
}
