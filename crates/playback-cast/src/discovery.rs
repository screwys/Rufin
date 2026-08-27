use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, ToSocketAddrs, UdpSocket};
use std::thread;
use std::time::{Duration, Instant};

use if_addrs::IfAddr;
use mdns_sd::{ServiceDaemon, ServiceEvent};
use playback::{RemoteOutput, RemoteOutputProtocol};
use socket2::{Domain, Protocol, SockAddr, Socket, Type};

use crate::upnp_transport::UpnpDevice;

const CAST_SERVICE: &str = "_googlecast._tcp.local.";
const MEDIA_RENDERER: &str = "urn:schemas-upnp-org:device:MediaRenderer:1";
const SSDP_ADDRESS: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(239, 255, 255, 250), 1900);
const SSDP_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Clone, Debug)]
pub(crate) enum DiscoveredTarget {
    Upnp {
        output: RemoteOutput,
        device: Box<UpnpDevice>,
        address: SocketAddr,
    },
    GoogleCast {
        output: RemoteOutput,
        address: SocketAddr,
    },
}

impl DiscoveredTarget {
    pub(crate) fn output(&self) -> &RemoteOutput {
        match self {
            Self::Upnp { output, .. } | Self::GoogleCast { output, .. } => output,
        }
    }

    pub(crate) fn address(&self) -> SocketAddr {
        match self {
            Self::Upnp { address, .. } | Self::GoogleCast { address, .. } => *address,
        }
    }
}

pub(crate) fn discover_upnp(
    timeout: Duration,
    local_address: Option<IpAddr>,
) -> Result<Vec<DiscoveredTarget>, String> {
    let locations = discover_upnp_locations(timeout, local_address)?;
    let mut targets = Vec::new();
    for (location, identity) in locations {
        let device = match UpnpDevice::from_url(&location, local_address) {
            Ok(device) => device,
            Err(error) => {
                tracing::debug!(%location, ?local_address, %error, "UPnP renderer description was unusable");
                continue;
            }
        };
        let Some(address) = device_address(&device) else {
            tracing::debug!(url = %device.url(), "UPnP renderer address could not be resolved");
            continue;
        };
        let output = RemoteOutput {
            id: format!("upnp:{identity}"),
            name: device.friendly_name().to_string(),
            protocol: RemoteOutputProtocol::Upnp,
        };
        targets.push(DiscoveredTarget::Upnp {
            output,
            device: Box::new(device),
            address,
        });
    }
    targets.sort_by(|left, right| left.output().name.cmp(&right.output().name));
    targets.dedup_by(|left, right| left.output().id == right.output().id);
    Ok(targets)
}

fn discover_upnp_locations(
    timeout: Duration,
    local_address: Option<IpAddr>,
) -> Result<HashMap<String, String>, String> {
    let interfaces = if_addrs::get_if_addrs().map_err(|error| error.to_string())?;
    let addresses = interfaces
        .into_iter()
        .filter_map(|interface| match interface.addr {
            IfAddr::V4(address) if !address.ip.is_loopback() && !address.ip.is_unspecified() => {
                Some(address.ip)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let addresses = selected_discovery_addresses(addresses, local_address);

    let sockets = addresses
        .into_iter()
        .filter_map(|address| match discovery_socket(address) {
            Ok(socket) => Some(socket),
            Err(error) => {
                tracing::debug!(%address, %error, "could not open SSDP discovery socket");
                None
            }
        })
        .collect::<Vec<_>>();
    if sockets.is_empty() {
        return Err("no IPv4 network interface is available for UPnP discovery".to_string());
    }
    let message = format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: {SSDP_ADDRESS}\r\nMAN: \"ssdp:discover\"\r\nMX: 1\r\nST: {MEDIA_RENDERER}\r\n\r\n"
    );
    for socket in &sockets {
        if let Err(error) = socket.send_to(message.as_bytes(), SSDP_ADDRESS) {
            tracing::debug!(%error, "could not send SSDP discovery request");
        }
    }
    let deadline = Instant::now() + timeout;
    let mut locations = HashMap::new();
    let mut buffer = [0_u8; 8_192];
    while Instant::now() < deadline {
        let mut received = false;
        for socket in &sockets {
            loop {
                match socket.recv_from(&mut buffer) {
                    Ok((length, _)) => {
                        received = true;
                        if let Ok(response) = std::str::from_utf8(&buffer[..length])
                            && let Some((location, identity)) = parse_ssdp_renderer(response)
                        {
                            locations.insert(location.to_string(), identity.to_string());
                        }
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                    Err(error) => {
                        tracing::debug!(%error, "could not receive SSDP discovery response");
                        break;
                    }
                }
            }
        }
        if !received {
            thread::sleep(SSDP_POLL_INTERVAL);
        }
    }
    Ok(locations)
}

fn selected_discovery_addresses(
    mut addresses: Vec<Ipv4Addr>,
    local_address: Option<IpAddr>,
) -> Vec<Ipv4Addr> {
    addresses.sort_unstable();
    addresses.dedup();
    match local_address {
        Some(IpAddr::V4(selected)) => addresses.retain(|address| *address == selected),
        Some(IpAddr::V6(_)) => addresses.clear(),
        None => {}
    }
    addresses
}

fn discovery_socket(address: Ipv4Addr) -> Result<UdpSocket, String> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
        .map_err(|error| error.to_string())?;
    socket
        .bind(&SockAddr::from(SocketAddrV4::new(address, 0)))
        .map_err(|error| error.to_string())?;
    socket
        .set_multicast_if_v4(&address)
        .map_err(|error| error.to_string())?;
    socket
        .set_multicast_ttl_v4(2)
        .map_err(|error| error.to_string())?;
    socket
        .set_nonblocking(true)
        .map_err(|error| error.to_string())?;
    Ok(socket.into())
}

fn parse_ssdp_renderer(response: &str) -> Option<(&str, &str)> {
    let mut lines = response.split("\r\n");
    let status = lines.next()?;
    if !status.starts_with("HTTP/1.1 200") {
        return None;
    }
    let mut location = None;
    let mut search_target = None;
    let mut usn = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("location") {
            location = Some(value.trim());
        } else if name.eq_ignore_ascii_case("st") {
            search_target = Some(value.trim());
        } else if name.eq_ignore_ascii_case("usn") {
            usn = Some(value.trim());
        }
    }
    if !search_target?.eq_ignore_ascii_case(MEDIA_RENDERER) {
        return None;
    }
    let location = location?;
    let identity = usn
        .and_then(|value| value.split_once("::").map(|(udn, _)| udn))
        .unwrap_or(location);
    Some((location, identity))
}

fn device_address(device: &UpnpDevice) -> Option<SocketAddr> {
    let host = device.url().host_str()?;
    let port = device.url().port().unwrap_or(80);
    (host, port).to_socket_addrs().ok()?.next()
}

pub(crate) fn discover_google_cast(timeout: Duration) -> Result<Vec<DiscoveredTarget>, String> {
    let daemon = ServiceDaemon::new().map_err(|error| error.to_string())?;
    let receiver = daemon
        .browse(CAST_SERVICE)
        .map_err(|error| error.to_string())?;
    let deadline = Instant::now() + timeout;
    let mut targets = Vec::new();
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        let event = match receiver.recv_timeout(remaining) {
            Ok(event) => event,
            Err(_) => break,
        };
        let ServiceEvent::ServiceResolved(info) = event else {
            continue;
        };
        let Some(ip) = preferred_address(
            info.get_addresses()
                .iter()
                .map(|address| address.to_ip_addr()),
        ) else {
            continue;
        };
        let id = info
            .get_property_val_str("id")
            .map_or_else(|| info.get_fullname().to_string(), str::to_string);
        let name = info.get_property_val_str("fn").map_or_else(
            || {
                info.get_fullname()
                    .trim_end_matches(CAST_SERVICE)
                    .to_string()
            },
            str::to_string,
        );
        targets.push(DiscoveredTarget::GoogleCast {
            output: RemoteOutput {
                id: format!("google-cast:{id}"),
                name,
                protocol: RemoteOutputProtocol::GoogleCast,
            },
            address: SocketAddr::new(ip, info.get_port()),
        });
    }
    let _ = daemon.stop_browse(CAST_SERVICE);
    let _ = daemon.shutdown();
    targets.sort_by(|left, right| left.output().name.cmp(&right.output().name));
    targets.dedup_by(|left, right| left.output().id == right.output().id);
    Ok(targets)
}

fn preferred_address(addresses: impl IntoIterator<Item = IpAddr>) -> Option<IpAddr> {
    let mut fallback = None;
    for address in addresses {
        if address.is_ipv4() {
            return Some(address);
        }
        fallback = Some(address);
    }
    fallback
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::upnp_transport::device_type_matches;

    #[test]
    fn media_renderer_response_yields_its_description_location() {
        let response = concat!(
            "HTTP/1.1 200 OK\r\n",
            "CACHE-CONTROL: max-age=1800\r\n",
            "LOCATION: http://192.0.2.10:1096/\r\n",
            "ST: urn:schemas-upnp-org:device:MediaRenderer:1\r\n",
            "USN: uuid:renderer::urn:schemas-upnp-org:device:MediaRenderer:1\r\n",
            "\r\n",
        );

        assert_eq!(
            parse_ssdp_renderer(response),
            Some(("http://192.0.2.10:1096/", "uuid:renderer"))
        );
        assert_eq!(
            parse_ssdp_renderer(&response.replace("MediaRenderer", "MediaServer")),
            None
        );
    }

    #[test]
    fn newer_media_renderer_versions_remain_discoverable() {
        assert!(device_type_matches(
            "urn:schemas-upnp-org:device:MediaRenderer:3",
            "MediaRenderer",
        ));
        assert!(!device_type_matches(
            "urn:schemas-upnp-org:device:MediaServer:3",
            "MediaRenderer",
        ));
    }

    #[test]
    fn selected_network_excludes_virtual_discovery_interfaces() {
        let lan = "192.168.1.103".parse().expect("LAN address");
        let tailscale = "100.64.0.12".parse().expect("Tailscale address");

        assert_eq!(
            selected_discovery_addresses(vec![tailscale, lan], Some(IpAddr::V4(lan)),),
            vec![lan]
        );
    }
}
