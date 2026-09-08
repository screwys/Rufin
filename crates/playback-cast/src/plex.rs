//! Native Plex Companion receiver transport. PMS authorization and queues remain in sources.
use std::fmt;
use std::io::Read;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use reqwest::blocking::{Client, RequestBuilder};
use roxmltree::Document;
use url::Url;

const RESPONSE_LIMIT: u64 = 2 * 1024 * 1024;
static COMMAND_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlexPlayer {
    pub endpoint: Url,
    pub machine_identifier: String,
    pub name: String,
    pub product: String,
    pub capabilities: Vec<String>,
}

impl PlexPlayer {
    pub fn supports_music_control(&self) -> bool {
        ["playback", "timeline", "playqueues"]
            .iter()
            .all(|required| {
                self.capabilities
                    .iter()
                    .any(|capability| capability == required)
            })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Timeline {
    pub state: String,
    pub time: Option<u64>,
    pub duration: u64,
    pub machine_identifier: Option<String>,
    pub play_queue_id: Option<u64>,
    pub play_queue_version: Option<u64>,
    pub play_queue_item_id: Option<u64>,
    pub rating_key: Option<String>,
    pub key: Option<String>,
    pub volume: Option<u8>,
    pub repeat: Option<u8>,
    pub shuffle: Option<bool>,
}

/// Only the short-lived delegation token is sent to a receiver.
pub struct PlayMedia<'a> {
    pub server_url: &'a Url,
    pub machine_identifier: &'a str,
    pub delegation_token: &'a str,
    pub key: &'a str,
    pub play_queue_id: u64,
    pub offset: u64,
    pub paused: bool,
}

impl fmt::Debug for PlayMedia<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlayMedia")
            .field("server", &self.machine_identifier)
            .field("key", &self.key)
            .field("play_queue_id", &self.play_queue_id)
            .field("offset", &self.offset)
            .field("paused", &self.paused)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct PlexClient {
    endpoint: Url,
    target: String,
    controller: String,
    client: Client,
    async_client: reqwest::Client,
}

impl PlexClient {
    pub fn new(endpoint: &str, target: &str, controller: &str) -> Result<Self, String> {
        let endpoint =
            Url::parse(endpoint).map_err(|_| "Invalid Plex player address".to_string())?;
        if !matches!(endpoint.scheme(), "http" | "https") || endpoint.host().is_none() {
            return Err("Plex player address must use HTTP or HTTPS".into());
        }
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| error.to_string())?;
        let async_client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(35))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self {
            endpoint,
            target: target.into(),
            controller: controller.into(),
            client,
            async_client,
        })
    }

    pub fn endpoint(&self) -> &Url {
        &self.endpoint
    }
    pub fn target(&self) -> &str {
        &self.target
    }

    fn request(&self, path: &str, params: &[(&str, String)]) -> Result<RequestBuilder, String> {
        let url = self
            .endpoint
            .join(path)
            .map_err(|_| "Invalid Plex player resource".to_string())?;
        Ok(self
            .client
            .get(url)
            .header("X-Plex-Client-Identifier", &self.controller)
            .header("X-Plex-Target-Client-Identifier", &self.target)
            .header("X-Plex-Product", "Rufin")
            .header("X-Plex-Version", env!("CARGO_PKG_VERSION"))
            .query(params))
    }

    fn controlled_request(
        &self,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<RequestBuilder, String> {
        self.request(path, params).map(|request| {
            request.query(&[
                ("type", "music".to_string()),
                (
                    "commandID",
                    COMMAND_ID
                        .fetch_add(1, Ordering::Relaxed)
                        .wrapping_add(1)
                        .to_string(),
                ),
            ])
        })
    }

    pub fn resources(&self) -> Result<PlexPlayer, String> {
        let body = response(self.request("/resources", &[])?)?;
        let document = Document::parse(&body)
            .map_err(|error| format!("Invalid Plex player resources: {error}"))?;
        document
            .descendants()
            .filter(|node| node.has_tag_name("Player"))
            .find(|node| node.attribute("machineIdentifier") == Some(self.target.as_str()))
            .and_then(|node| player(node, self.endpoint.clone()))
            .ok_or_else(|| "Plex player resources do not contain the selected receiver".into())
    }

    /// Call on the receiver observation worker; ordinary commands use a separate clone.
    /// The owner's worker lifetime controls polling; the HTTP wait is bounded.
    pub fn timeline(&self, wait: bool) -> Result<Option<Timeline>, String> {
        let controller = if wait {
            format!("{}-wait", self.controller)
        } else {
            self.controller.clone()
        };
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "X-Plex-Client-Identifier",
            controller
                .parse()
                .map_err(|_| "Invalid Plex controller identifier")?,
        );
        let request = self
            .controlled_request(
                "/player/timeline/poll",
                &[
                    ("wait", u8::from(wait).to_string()),
                    ("includeMetadata", "1".into()),
                ],
            )?
            // Plexamp removes a client's deferred poll when that same client
            // performs a non-waiting poll. Its native controller separates IDs.
            .headers(headers)
            .timeout(if wait {
                Duration::from_secs(35)
            } else {
                Duration::from_secs(10)
            });
        parse_timeline(&response(request)?)
    }

    /// Dropping this future cancels the in-flight long poll when the receiver is deselected.
    pub async fn timeline_async(&self, wait: bool) -> Result<Option<Timeline>, String> {
        let url = self
            .endpoint
            .join("/player/timeline/poll")
            .map_err(|_| "Invalid Plex player resource".to_string())?;
        let request = self
            .async_client
            .get(url)
            .header(
                "X-Plex-Client-Identifier",
                if wait {
                    format!("{}-wait", self.controller)
                } else {
                    self.controller.clone()
                },
            )
            .header("X-Plex-Target-Client-Identifier", &self.target)
            .header("X-Plex-Product", "Rufin")
            .query(&[
                ("type", "music".to_string()),
                ("wait", u8::from(wait).to_string()),
                ("includeMetadata", "1".into()),
                (
                    "commandID",
                    COMMAND_ID
                        .fetch_add(1, Ordering::Relaxed)
                        .wrapping_add(1)
                        .to_string(),
                ),
            ])
            .timeout(if wait {
                Duration::from_secs(35)
            } else {
                Duration::from_secs(10)
            });
        parse_timeline(&async_response(request).await?)
    }

    pub fn command(&self, command: &str, params: &[(&str, String)]) -> Result<(), String> {
        let body =
            response(self.controlled_request(&format!("/player/playback/{command}"), params)?)?;
        // Plexamp deliberately replies with plain OK (and some commands return no body).
        if let Ok(document) = Document::parse(&body)
            && let Some(code) = document
                .root_element()
                .attribute("code")
                .and_then(|code| code.parse::<u16>().ok())
            && code >= 400
        {
            return Err(format!("Plex player rejected command ({code})"));
        }
        Ok(())
    }

    pub fn play(&self) -> Result<(), String> {
        self.command("play", &[])
    }
    pub fn pause(&self) -> Result<(), String> {
        self.command("pause", &[])
    }
    pub fn stop(&self) -> Result<(), String> {
        self.command("stop", &[])
    }
    pub fn seek(&self, offset: u64) -> Result<(), String> {
        self.command("seekTo", &[("offset", offset.to_string())])
    }
    pub fn volume(&self, percent: u8) -> Result<(), String> {
        self.command("setParameters", &[("volume", percent.min(100).to_string())])
    }
    pub fn next(&self) -> Result<(), String> {
        self.command("skipNext", &[])
    }
    pub fn previous(&self) -> Result<(), String> {
        self.command("skipPrevious", &[("force", "1".into())])
    }
    pub fn skip_to(&self, key: &str, occurrence: u64) -> Result<(), String> {
        self.command(
            "skipTo",
            &[
                ("key", key.into()),
                ("playQueueItemID", occurrence.to_string()),
            ],
        )
    }
    pub fn repeat(&self, mode: playback::RepeatMode) -> Result<(), String> {
        self.command(
            "setParameters",
            &[(
                "repeat",
                match mode {
                    playback::RepeatMode::Off => "0",
                    playback::RepeatMode::All => "1",
                    playback::RepeatMode::One => "2",
                }
                .into(),
            )],
        )
    }
    pub fn shuffle(&self, shuffle: bool) -> Result<(), String> {
        self.command(
            "setParameters",
            &[("shuffle", u8::from(shuffle).to_string())],
        )
    }
    pub fn refresh_queue(&self, queue: u64) -> Result<(), String> {
        self.command("refreshPlayQueue", &[("playQueueID", queue.to_string())])
    }

    /// PMS timeline reporting has already selected the occurrence before this command.
    pub fn play_media(&self, media: &PlayMedia<'_>) -> Result<(), String> {
        let address = media
            .server_url
            .host_str()
            .ok_or("Plex server address has no host")?;
        let port = media
            .server_url
            .port_or_known_default()
            .ok_or("Plex server address has no port")?;
        self.command(
            "playMedia",
            &[
                ("machineIdentifier", media.machine_identifier.into()),
                ("address", address.trim_matches(['[', ']']).into()),
                ("port", port.to_string()),
                ("protocol", media.server_url.scheme().into()),
                ("token", media.delegation_token.into()),
                (
                    "containerKey",
                    format!("/playQueues/{}?own=1", media.play_queue_id),
                ),
                ("key", media.key.into()),
                ("offset", media.offset.to_string()),
                ("paused", u8::from(media.paused).to_string()),
            ],
        )
    }
}

fn response(request: RequestBuilder) -> Result<String, String> {
    let response = request
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|error| error.without_url().to_string())?;
    let mut body = String::new();
    response
        .take(RESPONSE_LIMIT + 1)
        .read_to_string(&mut body)
        .map_err(|error| format!("Cannot read Plex player response: {error}"))?;
    if body.len() as u64 > RESPONSE_LIMIT {
        return Err("Plex player response exceeds 2 MiB".into());
    }
    Ok(body)
}

async fn async_response(request: reqwest::RequestBuilder) -> Result<String, String> {
    let mut response = request
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| error.without_url().to_string())?;
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| error.without_url().to_string())?
    {
        if body.len() + chunk.len() > RESPONSE_LIMIT as usize {
            return Err("Plex player response exceeds 2 MiB".into());
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|_| "Plex player response is not UTF-8".into())
}

pub async fn discover_plex_players(timeout: Duration) -> Result<Vec<PlexPlayer>, String> {
    use futures_util::{StreamExt, stream};
    let mut candidates = sources::discovery::probe(
        &sources::discovery::broadcast_targets(32412),
        &[b"M-SEARCH * HTTP/1.0\r\n\r\n"],
        timeout,
        parse_gdm,
    )
    .await
    .map_err(|error| error.to_string())?;
    candidates.sort_by(|left, right| left.machine_identifier.cmp(&right.machine_identifier));
    candidates.dedup_by(|left, right| left.machine_identifier == right.machine_identifier);
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| error.to_string())?;
    let mut players = stream::iter(candidates.into_iter().map(|candidate| {
        let client = client.clone();
        async move {
            let resource = candidate.endpoint.join("/resources").ok()?;
            let xml = async_response(client.get(resource)).await.ok()?;
            let player = parse_resources(&xml, candidate.endpoint).ok()?;
            (player.machine_identifier == candidate.machine_identifier
                && player.supports_music_control())
            .then_some(player)
        }
    }))
    .buffer_unordered(4)
    .filter_map(|player| async move { player })
    .collect::<Vec<_>>()
    .await;
    players.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(players)
}

pub fn parse_timeline(xml: &str) -> Result<Option<Timeline>, String> {
    let document =
        Document::parse(xml).map_err(|error| format!("Invalid Plex player timeline: {error}"))?;
    Ok(document
        .descendants()
        .find(|node| node.has_tag_name("Timeline") && node.attribute("type") == Some("music"))
        .map(|node| {
            let number = |key| {
                node.attribute(key)
                    .and_then(|value| value.parse::<u64>().ok())
            };
            let text = |key| {
                node.attribute(key)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
            };
            Timeline {
                state: node.attribute("state").unwrap_or("stopped").into(),
                // Fractional milliseconds come from Plexamp's drag preview.
                // Ignore that position instead of moving lyrics or resetting to zero.
                time: number("time"),
                duration: number("duration").unwrap_or_default(),
                machine_identifier: text("machineIdentifier"),
                play_queue_id: number("playQueueID"),
                play_queue_version: number("playQueueVersion"),
                play_queue_item_id: number("playQueueItemID"),
                rating_key: text("ratingKey"),
                key: text("key"),
                volume: number("volume")
                    .and_then(|value| u8::try_from(value).ok())
                    .filter(|value| *value <= 100),
                repeat: number("repeat")
                    .and_then(|value| u8::try_from(value).ok())
                    .filter(|value| *value <= 2),
                shuffle: node.attribute("shuffle").and_then(|value| match value {
                    "1" | "true" => Some(true),
                    "0" | "false" => Some(false),
                    _ => None,
                }),
            }
        }))
}

pub fn parse_resources(xml: &str, endpoint: Url) -> Result<PlexPlayer, String> {
    let document =
        Document::parse(xml).map_err(|error| format!("Invalid Plex player resources: {error}"))?;
    document
        .descendants()
        .filter(|node| node.has_tag_name("Player"))
        .find_map(|node| player(node, endpoint.clone()))
        .ok_or_else(|| "Plex receiver resources omit player identity".into())
}

fn player(node: roxmltree::Node<'_, '_>, endpoint: Url) -> Option<PlexPlayer> {
    Some(PlexPlayer {
        endpoint,
        machine_identifier: node.attribute("machineIdentifier")?.to_owned(),
        name: node
            .attribute("title")
            .or_else(|| node.attribute("name"))
            .unwrap_or("Plex player")
            .into(),
        product: node.attribute("product").unwrap_or_default().into(),
        capabilities: node
            .attribute("protocolCapabilities")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect(),
    })
}

/// GDM player announcements use the sender IP and advertised player port, never the PMS port.
pub fn parse_gdm(bytes: &[u8], sender: SocketAddr) -> Option<PlexPlayer> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut lines = text.lines();
    if lines.next()?.split_whitespace().nth(1)? != "200" {
        return None;
    }
    let fields = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.trim().to_ascii_lowercase(), value.trim()))
        .collect::<std::collections::HashMap<_, _>>();
    if !fields
        .get("content-type")?
        .eq_ignore_ascii_case("plex/media-player")
    {
        return None;
    }
    let port = fields.get("port")?.parse().ok()?;
    let mut endpoint = Url::parse("http://localhost/").ok()?;
    endpoint.set_ip_host(sender.ip()).ok()?;
    endpoint.set_port(Some(port)).ok()?;
    Some(PlexPlayer {
        endpoint,
        machine_identifier: fields.get("resource-identifier")?.to_string(),
        name: fields.get("name").copied().unwrap_or("Plex player").into(),
        product: fields.get("product").copied().unwrap_or_default().into(),
        capabilities: fields
            .get("protocol-capabilities")
            .copied()
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    type Request = (Url, String, String);

    fn server(bodies: Vec<&'static str>) -> (String, thread::JoinHandle<Vec<Request>>) {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let address = format!("http://{}", server.server_addr());
        let base = address.clone();
        let worker = thread::spawn(move || {
            bodies
                .into_iter()
                .map(|body| {
                    let request = server
                        .recv_timeout(Duration::from_secs(5))
                        .unwrap()
                        .expect("receiver request");
                    let header = |name| {
                        request
                            .headers()
                            .iter()
                            .find(|header| header.field.equiv(name))
                            .unwrap()
                            .value
                            .to_string()
                    };
                    let observed = (
                        Url::parse(&base).unwrap().join(request.url()).unwrap(),
                        header("X-Plex-Client-Identifier"),
                        header("X-Plex-Target-Client-Identifier"),
                    );
                    request
                        .respond(tiny_http::Response::from_string(body))
                        .unwrap();
                    observed
                })
                .collect()
        });
        (address, worker)
    }

    #[test]
    fn commands_preserve_occurrences_and_paused_handoff_with_shared_command_ids() {
        let (address, worker) = server(vec![
            "OK", "", "OK", "OK", "OK", "OK", "OK", "OK", "OK", "OK", "OK", "OK",
        ]);
        let client = PlexClient::new(&address, "player", "controller").unwrap();
        let clone = client.clone();
        client.play().unwrap();
        clone.pause().unwrap();
        client.seek(12500).unwrap();
        client.volume(80).unwrap();
        client.next().unwrap();
        client.previous().unwrap();
        client.skip_to("/library/metadata/42", 501).unwrap();
        clone.skip_to("/library/metadata/42", 502).unwrap();
        client.repeat(playback::RepeatMode::One).unwrap();
        client.shuffle(true).unwrap();
        client.refresh_queue(99).unwrap();
        let server_url = Url::parse("http://[2001:db8::1]:32400/").unwrap();
        let transfer = PlayMedia {
            server_url: &server_url,
            machine_identifier: "server",
            delegation_token: "delegation-only",
            key: "/library/metadata/42",
            play_queue_id: 99,
            offset: 12500,
            paused: true,
        };
        assert!(!format!("{transfer:?}").contains("delegation-only"));
        let recreated = PlexClient::new(&address, "player", "controller").unwrap();
        recreated.play_media(&transfer).unwrap();
        let requests = worker.join().unwrap();
        let mut previous = 0;
        for (url, controller, target) in &requests {
            assert_eq!(controller, "controller");
            assert_eq!(target, "player");
            let id = param(url, "commandID").parse::<u64>().unwrap();
            assert!(id > previous);
            previous = id;
            assert_eq!(param(url, "type"), "music");
        }
        assert_eq!(param(&requests[5].0, "force"), "1");
        assert_eq!(param(&requests[6].0, "playQueueItemID"), "501");
        assert_eq!(param(&requests[7].0, "playQueueItemID"), "502");
        assert_eq!(param(&requests[8].0, "repeat"), "2");
        let handoff = &requests[11].0;
        assert_eq!(handoff.path(), "/player/playback/playMedia");
        assert_eq!(param(handoff, "address"), "2001:db8::1");
        assert_eq!(param(handoff, "port"), "32400");
        assert_eq!(param(handoff, "containerKey"), "/playQueues/99?own=1");
        assert_eq!(param(handoff, "paused"), "1");
        assert_eq!(param(handoff, "offset"), "12500");
        assert!(
            !handoff
                .query_pairs()
                .any(|(key, _)| key == "playQueueItemID" || key == "center")
        );
    }

    fn param(url: &Url, key: &str) -> String {
        url.query_pairs()
            .find(|(name, _)| name == key)
            .unwrap()
            .1
            .into_owned()
    }

    #[test]
    fn waiting_polls_do_not_replace_the_command_clients_subscription() {
        let xml = r#"<MediaContainer><Timeline type="music" state="paused"/></MediaContainer>"#;
        let (address, worker) = server(vec![xml; 4]);
        let client = PlexClient::new(&address, "player", "controller").unwrap();
        client.timeline(true).unwrap();
        client.timeline(false).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            client.timeline_async(true).await.unwrap();
            client.timeline_async(false).await.unwrap();
        });
        let requests = worker.join().unwrap();
        assert_eq!(
            requests
                .iter()
                .map(|request| request.1.as_str())
                .collect::<Vec<_>>(),
            [
                "controller-wait",
                "controller",
                "controller-wait",
                "controller"
            ]
        );
    }

    #[test]
    fn music_timeline_keeps_server_queue_occurrence_and_external_state() {
        let xml = r#"<MediaContainer><Timeline type="video" state="playing"/><Timeline type="music" state="paused" time="12000" duration="180000" machineIdentifier="pms" playQueueID="99" playQueueVersion="8" playQueueItemID="502" ratingKey="42" key="/library/metadata/42" volume="80" repeat="2" shuffle="1" unrelated="ignored"/></MediaContainer>"#;
        let (address, worker) = server(vec![xml]);
        let client = PlexClient::new(&address, "player", "controller").unwrap();
        let timeline = client.timeline(false).unwrap().unwrap();
        assert_eq!(timeline.state, "paused");
        assert_eq!(timeline.time, Some(12000));
        let preview = parse_timeline(&xml.replace("time=\"12000\"", "time=\"141195.625\""))
            .unwrap()
            .unwrap();
        assert_eq!(preview.time, None);
        assert_eq!(preview.play_queue_item_id, timeline.play_queue_item_id);
        assert_eq!(timeline.machine_identifier.as_deref(), Some("pms"));
        assert_eq!(timeline.play_queue_item_id, Some(502));
        assert_eq!(timeline.play_queue_version, Some(8));
        assert_eq!(timeline.repeat, Some(2));
        assert_eq!(timeline.shuffle, Some(true));
        let requests = worker.join().unwrap();
        assert_eq!(param(&requests[0].0, "wait"), "0");
        assert_eq!(param(&requests[0].0, "includeMetadata"), "1");
        let changed = parse_timeline(r#"<MediaContainer><Timeline type="music" state="playing" playQueueID="100" playQueueVersion="1" playQueueItemID="700" volume="broken" time="bad"/></MediaContainer>"#).unwrap().unwrap();
        assert_eq!(changed.play_queue_id, Some(100));
        assert_eq!(changed.play_queue_item_id, Some(700));
        assert_eq!(changed.volume, None);
    }

    #[test]
    fn discovery_verifies_player_capabilities_and_preserves_sender_ipv6() {
        let announcement = b"HTTP/1.0 200 OK\r\nContent-Type: plex/media-player\r\nResource-Identifier: player\r\nName: Living: Room\r\nProduct: Plexamp\r\nPort: 32500\r\n";
        let discovered = parse_gdm(announcement, "[2001:db8::2]:32412".parse().unwrap()).unwrap();
        assert_eq!(discovered.name, "Living: Room");
        assert_eq!(discovered.endpoint.as_str(), "http://[2001:db8::2]:32500/");
        let xml = r#"<MediaContainer><Player machineIdentifier="player" title="Living Room" product="Plexamp" protocolCapabilities="timeline,playback,playqueues" /></MediaContainer>"#;
        assert!(
            parse_resources(xml, discovered.endpoint.clone())
                .unwrap()
                .supports_music_control()
        );
        for product in ["Plex for Windows", "Third-party Companion", ""] {
            let mut receiver = parse_resources(xml, discovered.endpoint.clone()).unwrap();
            receiver.product = product.into();
            assert!(receiver.supports_music_control());
            receiver
                .capabilities
                .retain(|capability| capability != "playqueues");
            assert!(!receiver.supports_music_control());
        }
        assert!(parse_gdm(b"HTTP/1.0 200 OK\r\nContent-Type: plex/media-server\r\nResource-Identifier: pms\r\nPort: 32400\r\n","127.0.0.1:32414".parse().unwrap()).is_none());
        let (address, worker) = server(vec![xml]);
        assert!(
            PlexClient::new(&address, "player", "controller")
                .unwrap()
                .resources()
                .unwrap()
                .supports_music_control()
        );
        worker.join().unwrap();
    }

    #[test]
    fn receiver_xml_errors_do_not_expose_delegation_tokens() {
        let (address, worker) = server(vec![r#"<Response code="500" status="Failure"/>"#]);
        let client = PlexClient::new(&address, "player", "controller").unwrap();
        let error = client
            .command("playMedia", &[("token", "secret-delegation".into())])
            .unwrap_err();
        assert!(!error.contains("secret-delegation"));
        assert!(error.contains("500"));
        worker.join().unwrap();
    }

    #[test]
    fn cancelling_timeline_closes_the_in_flight_long_poll() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let client = PlexClient::new(&address, "player", "controller").unwrap();
        let (started, received) = tokio::sync::oneshot::channel();
        let worker = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut header = Vec::new();
            let mut byte = [0];
            while !header.ends_with(b"\r\n\r\n") {
                assert_eq!(socket.read(&mut byte).unwrap(), 1);
                header.push(byte[0]);
            }
            assert!(String::from_utf8(header).unwrap().contains("wait=1"));
            started.send(()).unwrap();
            assert_eq!(
                socket.read(&mut byte).unwrap(),
                0,
                "cancelled poll closes socket"
            );
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut poll = Box::pin(client.timeline_async(true));
            tokio::select! {
                result = &mut poll => panic!("receiver did not respond: {result:?}"),
                result = received => {result.unwrap();}
            }
            drop(poll);
            // Drive the HTTP cancellation to the socket before dropping the runtime.
            tokio::task::yield_now().await;
        });
        drop(runtime);
        worker.join().unwrap();
    }
}
