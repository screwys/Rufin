//! Deadline-bounded LAN discovery shared by source and output providers.
use crate::jellyfin_emby::normalize_base_url;
use crate::remote_http::{self, BodyLimit, RemoteHttpPolicy};
use crate::{SourceError, SourceResult};
use if_addrs::IfAddr;
use reqwest::{Client, StatusCode, header, redirect};
use serde_json::Value;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::time::{Instant, timeout_at};
use tracing::instrument;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryProvider {
    Jellyfin,
    Emby,
    Plex,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DiscoveredServer {
    pub id: Option<String>,
    pub name: String,
    pub address: String,
}

pub async fn discover_servers(
    provider: DiscoveryProvider,
    timeout: Duration,
) -> SourceResult<Vec<DiscoveredServer>> {
    match provider {
        DiscoveryProvider::Jellyfin => {
            discover_jellyfin_emby_servers(crate::ServerKind::Jellyfin, timeout).await
        }
        DiscoveryProvider::Emby => {
            discover_jellyfin_emby_servers(crate::ServerKind::Emby, timeout).await
        }
        DiscoveryProvider::Plex => discover_plex_servers(timeout).await,
    }
}

pub async fn probe<T>(
    targets: &[SocketAddrV4],
    messages: &[&[u8]],
    timeout: Duration,
    parse: impl Fn(&[u8], SocketAddr) -> Option<T>,
) -> SourceResult<Vec<T>> {
    let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))
        .await
        .map_err(|error| map_io_error("bind server discovery socket", error))?;
    socket
        .set_broadcast(true)
        .map_err(|error| map_io_error("enable server discovery broadcast", error))?;

    socket
        .set_multicast_ttl_v4(1)
        .map_err(|error| map_io_error("set discovery multicast TTL", error))?;

    let mut sent_any = false;
    let mut last_send_error = None;
    for target in targets {
        for message in messages {
            match socket.send_to(message, target).await {
                Ok(_) => sent_any = true,
                Err(error) => last_send_error = Some(error),
            }
        }
    }
    if !sent_any {
        return Err(map_io_error(
            "send server discovery broadcast",
            last_send_error.expect("fixed discovery targets are not empty"),
        ));
    }

    let deadline = Instant::now() + timeout;
    let mut buffer = [0_u8; 4096];
    let mut results = Vec::new();
    while Instant::now() < deadline {
        match timeout_at(deadline, socket.recv_from(&mut buffer)).await {
            Ok(Ok((size, source))) => {
                if let Some(server) = parse(&buffer[..size], source) {
                    results.push(server);
                }
            }
            Ok(Err(error)) => {
                return Err(map_io_error("receive server discovery response", error));
            }
            Err(_) => break,
        }
    }

    Ok(results)
}

pub fn broadcast_targets(port: u16) -> Vec<SocketAddrV4> {
    let interfaces = match if_addrs::get_if_addrs() {
        Ok(interfaces) => interfaces
            .into_iter()
            .filter_map(|interface| {
                let is_up = interface.is_oper_up();
                let is_point_to_point = interface.is_p2p();
                let IfAddr::V4(address) = interface.addr else {
                    return None;
                };
                Some(DiscoveryIpv4Interface {
                    address: address.ip,
                    broadcast: address.broadcast,
                    is_up,
                    is_point_to_point,
                })
            })
            .collect(),
        Err(error) => {
            tracing::debug!(
                %error,
                "could not enumerate interface broadcasts for server discovery"
            );
            Vec::new()
        }
    };
    discovery_targets_for(port, interfaces)
}

fn discovery_targets_for(
    port: u16,
    interfaces: impl IntoIterator<Item = DiscoveryIpv4Interface>,
) -> Vec<SocketAddrV4> {
    let mut targets = vec![
        SocketAddrV4::new(Ipv4Addr::BROADCAST, port),
        SocketAddrV4::new(Ipv4Addr::new(127, 255, 255, 255), port),
        SocketAddrV4::new(Ipv4Addr::LOCALHOST, port),
    ];
    for interface in interfaces {
        if !interface.is_up {
            continue;
        }
        let target = SocketAddrV4::new(interface.address, port);
        if !targets.contains(&target) {
            targets.push(target);
        }
        if interface.address.is_loopback() || interface.is_point_to_point {
            continue;
        }
        let Some(broadcast) = interface.broadcast else {
            continue;
        };
        let target = SocketAddrV4::new(broadcast, port);
        if !targets.contains(&target) {
            targets.push(target);
        }
    }
    targets
}

fn map_io_error(context: &str, error: io::Error) -> SourceError {
    SourceError::Network(format!("{context}: {error}"))
}

#[derive(Clone, Copy, Debug)]
struct DiscoveryIpv4Interface {
    address: Ipv4Addr,
    broadcast: Option<Ipv4Addr>,
    is_up: bool,
    is_point_to_point: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovery_include_localhost() {
        let targets = broadcast_targets(7359);

        assert!(targets.contains(&SocketAddrV4::new(Ipv4Addr::LOCALHOST, 7359)));
    }

    #[test]
    fn discovery_targets_active_interface_broadcasts() {
        let targets = discovery_targets_for(
            7359,
            [
                DiscoveryIpv4Interface {
                    address: Ipv4Addr::new(192, 168, 1, 103),
                    broadcast: Some(Ipv4Addr::new(192, 168, 1, 255)),
                    is_up: true,
                    is_point_to_point: false,
                },
                DiscoveryIpv4Interface {
                    address: Ipv4Addr::new(10, 2, 0, 2),
                    broadcast: Some(Ipv4Addr::new(10, 2, 0, 2)),
                    is_up: true,
                    is_point_to_point: true,
                },
                DiscoveryIpv4Interface {
                    address: Ipv4Addr::new(198, 51, 100, 20),
                    broadcast: Some(Ipv4Addr::new(198, 51, 100, 255)),
                    is_up: false,
                    is_point_to_point: false,
                },
            ],
        );

        assert!(targets.contains(&SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 255), 7359)));
        assert!(targets.contains(&SocketAddrV4::new(Ipv4Addr::new(10, 2, 0, 2), 7359)));
        assert!(targets.contains(&SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 103), 7359)));
        assert!(!targets.contains(&SocketAddrV4::new(Ipv4Addr::new(198, 51, 100, 255), 7359)));
    }
}

const JELLYFIN_EMBY_DISCOVERY_PORT: u16 = 7359;
const JELLYFIN_DISCOVERY_MESSAGES: &[&[u8]] =
    &[b"Who is JellyfinServer?", b"who is JellyfinServer?"];
const EMBY_DISCOVERY_MESSAGES: &[&[u8]] = &[b"who is EmbyServer?"];
#[instrument(skip_all, fields(timeout_ms = timeout.as_millis()))]
async fn discover_jellyfin_emby_servers(
    kind: crate::ServerKind,
    timeout: Duration,
) -> SourceResult<Vec<DiscoveredServer>> {
    let mut servers = Vec::new();
    for server in probe(
        &broadcast_targets(JELLYFIN_EMBY_DISCOVERY_PORT),
        match kind {
            crate::ServerKind::Jellyfin => JELLYFIN_DISCOVERY_MESSAGES,
            crate::ServerKind::Emby => EMBY_DISCOVERY_MESSAGES,
        },
        timeout,
        |packet, _sender| jellyfin_server_from_packet(packet),
    )
    .await?
    {
        push_server(&mut servers, server);
    }

    servers.sort_by_key(|server| (server.name.to_lowercase(), server.address.clone()));
    Ok(servers)
}

fn jellyfin_server_from_packet(packet: &[u8]) -> Option<DiscoveredServer> {
    let response: Value = serde_json::from_slice(packet).ok()?;
    jellyfin_server(
        response["Id"].as_str(),
        response["Name"].as_str(),
        response["Address"].as_str()?,
    )
}

fn jellyfin_server(
    id: Option<&str>,
    name: Option<&str>,
    address: &str,
) -> Option<DiscoveredServer> {
    Some(DiscoveredServer {
        id: id.filter(|id| !id.trim().is_empty()).map(str::to_owned),
        name: name
            .filter(|name| !name.trim().is_empty())
            .unwrap_or("Jellyfin")
            .to_string(),
        address: normalize_base_url(address)
            .ok()?
            .as_str()
            .trim_end_matches('/')
            .to_string(),
    })
}

fn push_server(servers: &mut Vec<DiscoveredServer>, server: DiscoveredServer) {
    if !servers
        .iter()
        .any(|existing| same_endpoint(&existing.address, &server.address))
    {
        servers.push(server);
    }
}

fn same_endpoint(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }
    let (Ok(left), Ok(right)) = (normalize_base_url(left), normalize_base_url(right)) else {
        return false;
    };
    is_loopback_endpoint(&left)
        && is_loopback_endpoint(&right)
        && left.scheme() == right.scheme()
        && left.port_or_known_default() == right.port_or_known_default()
        && left.path() == right.path()
}

fn is_loopback_endpoint(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

#[cfg(test)]
mod jellyfin_tests {
    use super::*;
    #[test]
    fn discovery_response_maps_server_address() {
        let packet = serde_json::json!({
            "Address": "http://192.0.2.20:8096/",
            "Id": "server-one",
            "Name": "Music Box",
            "EndpointAddress": "127.0.0.1:8096"
        })
        .to_string();

        let server = jellyfin_server_from_packet(packet.as_bytes()).expect("discovered server");

        assert_eq!(server.id.as_deref(), Some("server-one"));
        assert_eq!(server.name, "Music Box");
        assert_eq!(server.address, "http://192.0.2.20:8096");
    }

    #[test]
    fn discovery_preserves_scheme_custom_port_and_base_path() {
        for address in [
            "https://music.example:9443/emby",
            "http://10.2.0.2:18097/music",
        ] {
            let packet = serde_json::json!({"Address": address, "Name": "Music Box"});
            let server = jellyfin_server_from_packet(packet.to_string().as_bytes()).unwrap();
            assert_eq!(server.address, address);
        }
    }

    #[test]
    fn discovery_response_requires_an_advertised_address() {
        let packet = serde_json::json!({
            "Id": "server-one",
            "EndpointAddress": "192.0.2.10:8096"
        })
        .to_string();

        assert!(jellyfin_server_from_packet(packet.as_bytes()).is_none());
        for address in [
            serde_json::json!(42),
            serde_json::json!(""),
            serde_json::json!("http://["),
        ] {
            let packet = serde_json::json!({"Address": address, "Name": "Music Box"});
            assert!(jellyfin_server_from_packet(packet.to_string().as_bytes()).is_none());
        }
    }

    #[test]
    fn discovery_response_keeps_address_with_malformed_optional_fields() {
        for (id, name, expected_id, expected_name) in [
            (
                serde_json::json!({}),
                serde_json::json!("Music Box"),
                None,
                "Music Box",
            ),
            (
                serde_json::json!("server-one"),
                serde_json::json!([]),
                Some("server-one"),
                "Jellyfin",
            ),
        ] {
            let packet =
                serde_json::json!({"Address": "http://192.0.2.20:8096", "Id": id, "Name": name});
            let server = jellyfin_server_from_packet(packet.to_string().as_bytes())
                .expect("usable discovery address");
            assert_eq!(server.address, "http://192.0.2.20:8096");
            assert_eq!(server.id.as_deref(), expected_id);
            assert_eq!(server.name, expected_name);
        }
    }

    #[test]
    fn discovery_keeps_distinct_urls_and_one_loopback() {
        let mut servers = Vec::new();
        push_server(
            &mut servers,
            DiscoveredServer {
                id: Some("server-one".to_string()),
                name: "Music Box".to_string(),
                address: "http://music.local:8096".to_string(),
            },
        );
        push_server(
            &mut servers,
            DiscoveredServer {
                id: Some("server-one".to_string()),
                name: "Music Box".to_string(),
                address: "http://192.0.2.10:8096".to_string(),
            },
        );
        push_server(
            &mut servers,
            DiscoveredServer {
                id: Some("server-one".to_string()),
                name: "Music Box".to_string(),
                address: "http://127.0.0.1:8096".to_string(),
            },
        );
        push_server(
            &mut servers,
            DiscoveredServer {
                id: Some("server-one".to_string()),
                name: "Music Box".to_string(),
                address: "http://localhost:8096".to_string(),
            },
        );
        push_server(
            &mut servers,
            DiscoveredServer {
                id: Some("server-one".to_string()),
                name: "Music Box".to_string(),
                address: "http://[::1]:8096".to_string(),
            },
        );

        assert_eq!(servers.len(), 3);
        assert_eq!(servers[0].address, "http://music.local:8096");
        assert_eq!(servers[1].address, "http://192.0.2.10:8096");
        assert_eq!(servers[2].address, "http://127.0.0.1:8096");
    }
}

async fn discover_plex_servers(timeout: Duration) -> SourceResult<Vec<DiscoveredServer>> {
    let mut servers = probe(
        &[SocketAddrV4::new(Ipv4Addr::new(239, 0, 0, 250), 32414)],
        &[b"M-SEARCH * HTTP/1.0"],
        timeout,
        parse_plex_server,
    )
    .await?;
    servers.extend(plex_loopback_servers().await);
    servers.sort_by_key(|server| (server.name.to_lowercase(), server.address.clone()));
    servers.dedup_by(|a, b| a.id == b.id && a.address == b.address);
    Ok(servers)
}

pub(crate) async fn plex_loopback_servers() -> Vec<DiscoveredServer> {
    let mut servers = Vec::new();
    if let Ok(client) = Client::builder()
        .no_proxy()
        .redirect(redirect::Policy::none())
        .connect_timeout(Duration::from_millis(250))
        .timeout(Duration::from_millis(250))
        .build()
    {
        for target in ["http://127.0.0.1:32400", "http://[::1]:32400"] {
            if let Some(server) = probe_plex_localhost_server(&client, target).await {
                servers.push(server);
            }
        }
    }
    servers
}

async fn probe_plex_localhost_server(client: &Client, target: &str) -> Option<DiscoveredServer> {
    const BODY: BodyLimit = BodyLimit {
        max_bytes: 16 * 1024,
        context: "Plex discovery response",
    };
    const HTTP: RemoteHttpPolicy = RemoteHttpPolicy {
        service: "Plex discovery",
        auth_context: "Plex discovery returned",
        error_body: BODY,
        redact_error_url: None,
    };
    let response = client
        .get(format!("{target}/identity"))
        .header(header::ACCEPT, "application/json")
        .send()
        .await
        .ok()?;
    if response.status() != StatusCode::OK {
        return None;
    }
    let body = remote_http::bounded_response_body(response, HTTP, BODY)
        .await
        .ok()?;
    let response: serde_json::Value = serde_json::from_slice(&body).ok()?;
    Some(DiscoveredServer {
        id: Some(crate::remote_json::id(
            &response["MediaContainer"]["machineIdentifier"],
        )?),
        name: "Plex".into(),
        address: target.into(),
    })
}

fn parse_plex_server(packet: &[u8], sender: SocketAddr) -> Option<DiscoveredServer> {
    let packet = std::str::from_utf8(packet).ok()?;
    let mut lines = packet.lines();
    let mut status = lines.next()?.split_whitespace();
    if !status.next()?.starts_with("HTTP/") || status.next()? != "200" {
        return None;
    }
    let mut id = None;
    let mut name = None;
    let mut port = None;
    let mut is_server = false;
    for line in lines {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.to_ascii_lowercase().as_str() {
            "content-type" => is_server = value == "plex/media-server",
            "resource-identifier" => id = Some(value.to_owned()),
            "name" => name = Some(value.to_owned()),
            "port" => port = value.parse::<u16>().ok(),
            _ => {}
        }
    }
    if !is_server {
        return None;
    }
    Some(DiscoveredServer {
        id: Some(id?),
        name: name.unwrap_or_else(|| "Plex".to_owned()),
        address: format!("http://{}", SocketAddr::new(sender.ip(), port?)),
    })
}

#[cfg(test)]
mod plex_tests {
    use super::*;

    #[tokio::test]
    async fn loopback_identity_is_discovered_without_credentials() {
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{method, path},
        };
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/identity"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"MediaContainer":{"machineIdentifier":"local-server","size":0}}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        let found = probe_plex_localhost_server(&Client::new(), &server.uri())
            .await
            .unwrap();
        assert_eq!(found.id.as_deref(), Some("local-server"));
        assert_eq!(found.address, server.uri());
        let requests = server.received_requests().await.unwrap();
        assert!(!requests[0].headers.contains_key("X-Plex-Token"));
        assert!(!requests[0].headers.contains_key("Authorization"));
    }

    #[test]
    fn server_response_uses_sender_and_advertised_port() {
        let response = b"HTTP/1.0 200 OK\r\nContent-Type: plex/media-server\r\nResource-Identifier: server-one\r\nName: Music: home\r\nPort: 32400\r\n";
        let server = parse_plex_server(response, "192.0.2.20:32414".parse().unwrap()).unwrap();
        assert_eq!(server.id.as_deref(), Some("server-one"));
        assert_eq!(server.name, "Music: home");
        assert_eq!(server.address, "http://192.0.2.20:32400");
    }

    #[test]
    fn player_announcements_do_not_populate_source_setup() {
        let response = b"HTTP/1.0 200 OK\r\nContent-Type: plex/media-player\r\nResource-Identifier: player-one\r\nName: Plexamp\r\nPort: 32500\r\n";
        assert!(parse_plex_server(response, "192.0.2.20:32412".parse().unwrap()).is_none());
    }
}
