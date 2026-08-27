use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use playback::{CastNetwork, PreparedStream};
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use tiny_http::{Header, Method, Request, Response, ResponseBox, Server, StatusCode};
use url::Url;

const PREFERRED_RELAY_PORT: u16 = 9_876;

pub(crate) type ArtworkResolver = Arc<dyn Fn(&PreparedStream) -> Option<PathBuf> + Send + Sync>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RelayRepresentation {
    Automatic,
    Source,
    Mp3,
}

#[derive(Clone)]
struct RelayResource {
    stream: PreparedStream,
    artwork_path: Option<PathBuf>,
    content_type: String,
    content_length: Option<u64>,
    transcode: bool,
}

#[derive(Clone)]
pub(crate) struct PublishedResource {
    pub(crate) uri: String,
    pub(crate) content_type: String,
    pub(crate) content_length: Option<u64>,
    pub(crate) logical_offset_millis: u64,
    pub(crate) resource_duration_millis: Option<u64>,
    pub(crate) seekable: bool,
    pub(crate) artwork_uri: Option<String>,
    pub(crate) relay_token: Option<String>,
}

impl PublishedResource {
    pub(crate) fn logical_position_millis(&self, renderer_position_millis: u64) -> u64 {
        self.logical_offset_millis
            .saturating_add(renderer_position_millis)
    }
}

pub(crate) struct RelayServer {
    base_url: String,
    target_is_local: bool,
    proxy_media: Arc<AtomicBool>,
    resources: Arc<Mutex<HashMap<String, RelayResource>>>,
    artwork_resolver: Option<ArtworkResolver>,
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl RelayServer {
    pub(crate) fn start(
        target: SocketAddr,
        proxy_media: Arc<AtomicBool>,
        network_interface: Option<&str>,
    ) -> Result<Self, String> {
        let (local_ip, bound_interface) = network_binding_for(target, network_interface)?;
        let bind_ip = if local_ip.is_ipv4() {
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        } else {
            IpAddr::V6(Ipv6Addr::UNSPECIFIED)
        };
        let server = relay_server(bind_ip, PREFERRED_RELAY_PORT, bound_interface)
            .or_else(|_| relay_server(bind_ip, 0, bound_interface))?;
        let address = server
            .server_addr()
            .to_ip()
            .ok_or_else(|| "the cast relay did not bind an IP socket".to_string())?;
        let resources = Arc::new(Mutex::new(HashMap::new()));
        let running = Arc::new(AtomicBool::new(true));
        let thread_resources = Arc::clone(&resources);
        let thread_running = Arc::clone(&running);
        let thread = thread::Builder::new()
            .name("rufin-cast-relay".to_string())
            .spawn(move || serve(server, thread_resources, thread_running))
            .map_err(|error| error.to_string())?;
        let host = match local_ip {
            IpAddr::V4(ip) => ip.to_string(),
            IpAddr::V6(ip) => format!("[{ip}]"),
        };
        Ok(Self {
            base_url: format!("http://{host}:{}", address.port()),
            target_is_local: target_is_local(target),
            proxy_media,
            resources,
            artwork_resolver: None,
            running,
            thread: Some(thread),
        })
    }

    pub(crate) fn with_artwork_resolver(mut self, resolver: ArtworkResolver) -> Self {
        self.artwork_resolver = Some(resolver);
        self
    }

    pub(crate) fn publish(&self, stream: &PreparedStream) -> Result<PublishedResource, String> {
        self.publish_at(stream, RelayRepresentation::Automatic, 0)
    }

    pub(crate) fn publish_as(
        &self,
        stream: &PreparedStream,
        representation: RelayRepresentation,
    ) -> Result<PublishedResource, String> {
        self.publish_at(stream, representation, 0)
    }

    pub(crate) fn publish_at(
        &self,
        stream: &PreparedStream,
        representation: RelayRepresentation,
        logical_offset_millis: u64,
    ) -> Result<PublishedResource, String> {
        let artwork_path = stream.artwork_path.as_deref().cloned().or_else(|| {
            let resolver = self.artwork_resolver.as_ref()?;
            resolver(stream)
        });
        let original_content_type = source_content_type(stream);
        let transcode = representation == RelayRepresentation::Mp3
            || stream.window().is_some()
            || (representation == RelayRepresentation::Automatic
                && !directly_supported(&original_content_type));
        let logical_duration_millis = stream_duration_millis(stream);
        let (stream, logical_offset_millis) = if transcode && logical_offset_millis > 0 {
            let duration = logical_duration_millis.ok_or_else(|| {
                "cast transcode cannot restore a position without a track duration".to_string()
            })?;
            if logical_offset_millis >= duration {
                return Err("cast position is outside the track duration".to_string());
            }
            (
                clipped_stream(stream, logical_offset_millis, duration),
                logical_offset_millis,
            )
        } else {
            (stream.clone(), 0)
        };
        let content_type = if transcode {
            "audio/mpeg".to_string()
        } else {
            original_content_type
        };
        let content_length = if transcode {
            None
        } else {
            stream_content_length(&stream)
        };
        let direct = !self.proxy_media.load(Ordering::Acquire)
            && direct_media_uri(&stream, transcode, self.target_is_local);
        let needs_relay_resource = !direct || artwork_path.is_some();
        let token = needs_relay_resource.then(random_token).transpose()?;
        if let Some(token) = &token {
            self.resources
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(
                    token.clone(),
                    RelayResource {
                        stream: stream.clone(),
                        artwork_path: artwork_path.clone(),
                        content_type: content_type.clone(),
                        content_length,
                        transcode,
                    },
                );
        }
        let uri = if direct {
            stream.uri().to_string()
        } else {
            format!(
                "{}/{}/media.{}",
                self.base_url,
                token.as_deref().expect("relay token"),
                content_extension(&content_type),
            )
        };
        tracing::debug!(
            upstream_transport = transport_scheme(stream.uri()),
            renderer_transport = transport_scheme(&uri),
            relayed = !direct,
            relay_address = %self.base_url,
            %content_type,
            content_length,
            transcode,
            logical_offset_millis,
            "published cast media"
        );
        let resource_duration_millis =
            logical_duration_millis.map(|duration| duration.saturating_sub(logical_offset_millis));
        Ok(PublishedResource {
            artwork_uri: artwork_path.as_ref().map(|_| {
                format!(
                    "{}/{}/artwork",
                    self.base_url,
                    token.as_deref().expect("artwork relay token")
                )
            }),
            uri,
            content_type,
            content_length,
            logical_offset_millis,
            resource_duration_millis,
            seekable: !transcode,
            relay_token: token,
        })
    }

    pub(crate) fn remove(&self, resource: &PublishedResource) {
        let Some(token) = &resource.relay_token else {
            return;
        };
        self.resources
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(token);
    }

    pub(crate) fn clear(&self) {
        self.resources
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }

    pub(crate) fn shutdown(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.resources
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }
}

fn relay_server(
    bind_ip: IpAddr,
    port: u16,
    network_interface: Option<&str>,
) -> Result<Server, String> {
    let listener = relay_listener(bind_ip, port, network_interface)?;
    Server::from_listener(listener, None).map_err(|error| error.to_string())
}

fn relay_listener(
    bind_ip: IpAddr,
    port: u16,
    network_interface: Option<&str>,
) -> Result<TcpListener, String> {
    let domain = if bind_ip.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))
        .map_err(|error| error.to_string())?;
    if let Some(network_interface) = network_interface {
        bind_relay_interface(&socket, network_interface)?;
    }
    socket
        .bind(&SockAddr::from(SocketAddr::new(bind_ip, port)))
        .map_err(|error| error.to_string())?;
    socket.listen(128).map_err(|error| error.to_string())?;
    Ok(socket.into())
}

#[cfg(any(target_os = "android", target_os = "linux"))]
fn bind_relay_interface(socket: &Socket, network_interface: &str) -> Result<(), String> {
    socket
        .bind_device(Some(network_interface.as_bytes()))
        .map_err(|error| error.to_string())
}

#[cfg(not(any(target_os = "android", target_os = "linux")))]
fn bind_relay_interface(_socket: &Socket, _network_interface: &str) -> Result<(), String> {
    Ok(())
}

fn stream_duration_millis(stream: &PreparedStream) -> Option<u64> {
    stream
        .end_millis()
        .map(|end| end.saturating_sub(stream.start_millis()))
        .or_else(|| {
            stream
                .track
                .as_ref()
                .and_then(|track| u64::try_from(track.duration_millis).ok())
        })
}

fn clipped_stream(
    stream: &PreparedStream,
    logical_offset_millis: u64,
    duration_millis: u64,
) -> PreparedStream {
    let source_start_millis = stream.start_millis().saturating_add(logical_offset_millis);
    let source_end_millis = stream.start_millis().saturating_add(duration_millis);
    let mut clipped = stream.clone();
    clipped.stream = Box::new(
        (*stream.stream)
            .clone()
            .with_window(source_start_millis, source_end_millis),
    );
    clipped
}

fn direct_media_uri(stream: &PreparedStream, transcode: bool, target_is_local: bool) -> bool {
    if transcode || stream.trust_invalid_certificate() || stream.window().is_some() {
        return false;
    }
    Url::parse(stream.uri()).is_ok_and(|uri| {
        if !matches!(uri.scheme(), "http" | "https") {
            return false;
        }
        let loopback = uri.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        });
        !loopback || target_is_local
    })
}

fn target_is_local(target: SocketAddr) -> bool {
    target.ip().is_loopback()
        || if_addrs::get_if_addrs().is_ok_and(|interfaces| {
            interfaces
                .into_iter()
                .any(|interface| interface.ip() == target.ip())
        })
}

impl Drop for RelayServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub(crate) fn available_networks() -> Result<Vec<CastNetwork>, String> {
    let mut interfaces = BTreeMap::<String, Vec<IpAddr>>::new();
    for (name, address) in local_interface_addresses()? {
        interfaces.entry(name).or_default().push(address);
    }
    Ok(interfaces
        .into_iter()
        .filter_map(|(name, mut addresses)| {
            addresses.sort_unstable();
            let address = addresses
                .iter()
                .copied()
                .find(IpAddr::is_ipv4)
                .or_else(|| addresses.first().copied())?;
            Some(CastNetwork {
                id: name.clone(),
                name,
                address,
            })
        })
        .collect())
}

pub(crate) fn network_address(network_interface: &str) -> Result<Option<IpAddr>, String> {
    let mut addresses = local_interface_addresses()?
        .into_iter()
        .filter_map(|(name, address)| (name == network_interface).then_some(address))
        .collect::<Vec<_>>();
    addresses.sort_unstable();
    Ok(addresses
        .iter()
        .copied()
        .find(IpAddr::is_ipv4)
        .or_else(|| addresses.first().copied()))
}

fn network_binding_for<'a>(
    target: SocketAddr,
    network_interface: Option<&'a str>,
) -> Result<(IpAddr, Option<&'a str>), String> {
    if let Some(network_interface) = network_interface {
        let interfaces = local_interface_addresses()?;
        if let Some(address) = selected_interface_address(
            interfaces
                .iter()
                .map(|(name, address)| (name.as_str(), *address)),
            network_interface,
            target.ip(),
        ) {
            return Ok((address, Some(network_interface)));
        }
        tracing::warn!(
            network_interface,
            "selected casting network is unavailable; using automatic routing"
        );
    }
    automatic_local_address_for(target).map(|address| (address, None))
}

pub(crate) fn local_address_for(
    target: SocketAddr,
    network_interface: Option<&str>,
) -> Result<IpAddr, String> {
    network_binding_for(target, network_interface).map(|(address, _)| address)
}

fn automatic_local_address_for(target: SocketAddr) -> Result<IpAddr, String> {
    let bind = if target.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let socket = UdpSocket::bind(bind).map_err(|error| error.to_string())?;
    socket.connect(target).map_err(|error| error.to_string())?;
    socket
        .local_addr()
        .map(|address| address.ip())
        .map_err(|error| error.to_string())
}

fn local_interface_addresses() -> Result<Vec<(String, IpAddr)>, String> {
    let mut interfaces = if_addrs::get_if_addrs()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter_map(|interface| {
            let address = interface.ip();
            (!address.is_loopback()
                && !address.is_unspecified()
                && !matches!(address, IpAddr::V6(address) if address.is_unicast_link_local()))
            .then_some((interface.name, address))
        })
        .collect::<Vec<_>>();
    interfaces.sort_unstable();
    interfaces.dedup();
    Ok(interfaces)
}

fn selected_interface_address<'a>(
    interfaces: impl IntoIterator<Item = (&'a str, IpAddr)>,
    selected: &str,
    target: IpAddr,
) -> Option<IpAddr> {
    let mut addresses = interfaces
        .into_iter()
        .filter_map(|(name, address)| (name == selected).then_some(address))
        .collect::<Vec<_>>();
    addresses.sort_unstable();
    addresses
        .iter()
        .copied()
        .find(|address| address.is_ipv4() == target.is_ipv4())
        .or_else(|| addresses.first().copied())
}

fn serve(
    server: Server,
    resources: Arc<Mutex<HashMap<String, RelayResource>>>,
    running: Arc<AtomicBool>,
) {
    while running.load(Ordering::Acquire) {
        let request = match server.recv_timeout(Duration::from_millis(100)) {
            Ok(Some(request)) => request,
            Ok(None) => continue,
            Err(error) => {
                tracing::debug!(%error, "cast relay stopped receiving requests");
                break;
            }
        };
        let request_resources = Arc::clone(&resources);
        let _ = thread::Builder::new()
            .name("rufin-cast-request".to_string())
            .spawn(move || respond(request, &request_resources));
    }
}

fn respond(request: Request, resources: &Mutex<HashMap<String, RelayResource>>) {
    let path = request
        .url()
        .split('?')
        .next()
        .unwrap_or_default()
        .trim_start_matches('/');
    let mut segments = path.split('/');
    let token = segments.next().unwrap_or_default();
    let artwork = segments.next() == Some("artwork");
    let resource = resources
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(token)
        .cloned();
    tracing::debug!(
        method = ?request.method(),
        artwork,
        range = request.headers().iter().any(|header| header.field.equiv("Range")),
        active = resource.is_some(),
        "received cast relay request"
    );
    let response = match resource {
        Some(resource) => resource_response(&request, resource, artwork),
        None => Ok(empty_response(StatusCode(404))),
    }
    .unwrap_or_else(|error| {
        tracing::debug!(%error, "cast relay request failed");
        empty_response(StatusCode(502))
    });
    let _ = request.respond(response);
}

fn resource_response(
    request: &Request,
    resource: RelayResource,
    artwork: bool,
) -> Result<ResponseBox, String> {
    if !matches!(request.method(), Method::Get | Method::Head) {
        return Ok(empty_response(StatusCode(405)));
    }
    let range = request
        .headers()
        .iter()
        .find(|header| header.field.equiv("Range"))
        .map(|header| header.value.as_str().to_string());
    if artwork {
        artwork_response(request.method(), range.as_deref(), resource)
    } else if resource.transcode {
        transcoded_response(request.method(), range.as_deref(), resource)
    } else if resource.stream.uri().starts_with("file:") {
        local_response(request.method(), range.as_deref(), resource)
    } else {
        remote_response(request.method(), range.as_deref(), resource)
    }
}

fn transcoded_response(
    method: &Method,
    range: Option<&str>,
    resource: RelayResource,
) -> Result<ResponseBox, String> {
    if range.is_some() {
        return Ok(empty_response(StatusCode(416)));
    }
    let headers = vec![
        header("Content-Type", "audio/mpeg")?,
        header("Access-Control-Allow-Origin", "*")?,
        header("transferMode.dlna.org", "Streaming")?,
        header(
            "contentFeatures.dlna.org",
            "DLNA.ORG_PN=MP3;DLNA.ORG_OP=00;DLNA.ORG_CI=1;DLNA.ORG_FLAGS=01500000000000000000000000000000",
        )?,
    ];
    if matches!(method, Method::Head) {
        return Ok(Response::new(
            StatusCode(200),
            headers,
            Cursor::new(Vec::new()),
            None,
            None,
        )
        .boxed());
    }
    let reader = playback_gstreamer::TranscodedAudioReader::mp3(&resource.stream.stream)?;
    Ok(Response::new(StatusCode(200), headers, reader, None, None).boxed())
}

fn local_response(
    method: &Method,
    range: Option<&str>,
    resource: RelayResource,
) -> Result<ResponseBox, String> {
    let url = Url::parse(resource.stream.uri()).map_err(|error| error.to_string())?;
    let path = url
        .to_file_path()
        .map_err(|()| "the local cast URL is not a file path".to_string())?;
    file_response(method, range, path, &resource.content_type)
}

fn artwork_response(
    method: &Method,
    range: Option<&str>,
    resource: RelayResource,
) -> Result<ResponseBox, String> {
    let path = resource
        .artwork_path
        .ok_or_else(|| "cast artwork is unavailable".to_string())?;
    let content_type = artwork_content_type(&path)?;
    file_response(method, range, path, content_type)
}

fn file_response(
    method: &Method,
    range: Option<&str>,
    path: PathBuf,
    content_type: &str,
) -> Result<ResponseBox, String> {
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    let total = file.metadata().map_err(|error| error.to_string())?.len();
    let selected = match parse_range(range, total) {
        Ok(selected) => selected,
        Err(_) => return Ok(empty_response(StatusCode(416))),
    };
    let (status, start, length) = selected.map_or((StatusCode(200), 0, total), |(start, end)| {
        (StatusCode(206), start, end - start + 1)
    });
    file.seek(SeekFrom::Start(start))
        .map_err(|error| error.to_string())?;
    let headers = relay_headers(
        content_type,
        selected.map(|(start, end)| (start, end, total)),
    )?;
    if matches!(method, Method::Head) {
        let length =
            usize::try_from(length).map_err(|_| "cast resource is too large".to_string())?;
        return Ok(
            Response::new(status, headers, Cursor::new(Vec::new()), Some(length), None)
                .with_chunked_threshold(usize::MAX)
                .boxed(),
        );
    }
    let length = usize::try_from(length).map_err(|_| "cast resource is too large".to_string())?;
    Ok(Response::new(
        status,
        headers,
        file.take(length as u64),
        Some(length),
        None,
    )
    .with_chunked_threshold(usize::MAX)
    .boxed())
}

fn artwork_content_type(path: &Path) -> Result<&'static str, String> {
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    let mut signature = [0_u8; 12];
    let length = file
        .read(&mut signature)
        .map_err(|error| error.to_string())?;
    if length >= 8 && signature[..8] == *b"\x89PNG\r\n\x1a\n" {
        Ok("image/png")
    } else if length >= 3 && signature[..3] == [0xff, 0xd8, 0xff] {
        Ok("image/jpeg")
    } else if length >= 6 && matches!(&signature[..6], b"GIF87a" | b"GIF89a") {
        Ok("image/gif")
    } else if length >= 12 && signature[..4] == *b"RIFF" && signature[8..12] == *b"WEBP" {
        Ok("image/webp")
    } else {
        Ok("application/octet-stream")
    }
}

fn remote_response(
    method: &Method,
    range: Option<&str>,
    resource: RelayResource,
) -> Result<ResponseBox, String> {
    if matches!(method, Method::Head)
        && let Some(length) = resource.content_length
    {
        let headers = relay_headers(&resource.content_type, None)?;
        let length =
            usize::try_from(length).map_err(|_| "cast resource is too large".to_string())?;
        return Ok(Response::new(
            StatusCode(200),
            headers,
            Cursor::new(Vec::new()),
            Some(length),
            None,
        )
        .with_chunked_threshold(usize::MAX)
        .boxed());
    }
    let client = upstream_client(&resource.stream)?;
    let mut upstream = if matches!(method, Method::Head) {
        client.head(resource.stream.uri())
    } else {
        client.get(resource.stream.uri())
    };
    if let Some(range) = range {
        upstream = upstream.header(reqwest::header::RANGE, range);
    }
    let response = upstream.send().map_err(|error| error.to_string())?;
    tracing::debug!(
        method = ?method,
        range = range.is_some(),
        status = response.status().as_u16(),
        content_length = response.content_length(),
        "received cast relay upstream response"
    );
    let status = StatusCode(response.status().as_u16());
    let length = response
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .or_else(|| {
            response
                .content_length()
                .and_then(|length| usize::try_from(length).ok())
        })
        .or_else(|| {
            range
                .is_none()
                .then_some(resource.content_length)
                .flatten()
                .and_then(|length| usize::try_from(length).ok())
        });
    let mut headers = relay_headers(&resource.content_type, None)?;
    if let Some(content_range) = response.headers().get(reqwest::header::CONTENT_RANGE)
        && let Ok(content_range) = content_range.to_str()
    {
        headers.push(header("Content-Range", content_range)?);
    }
    if matches!(method, Method::Head) {
        return Ok(
            Response::new(status, headers, Cursor::new(Vec::new()), length, None)
                .with_chunked_threshold(usize::MAX)
                .boxed(),
        );
    }
    Ok(Response::new(status, headers, response, length, None)
        .with_chunked_threshold(usize::MAX)
        .boxed())
}

fn parse_range(value: Option<&str>, total: u64) -> Result<Option<(u64, u64)>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(value) = value.strip_prefix("bytes=") else {
        return Err("unsupported cast byte range".to_string());
    };
    if value.contains(',') {
        return Err("multiple cast byte ranges are unsupported".to_string());
    }
    let (start, end) = value
        .split_once('-')
        .ok_or_else(|| "invalid cast byte range".to_string())?;
    if total == 0 {
        return Err("cast byte range is outside the resource".to_string());
    }
    if start.is_empty() {
        let suffix = end
            .parse::<u64>()
            .map_err(|_| "invalid cast byte range suffix".to_string())?;
        if suffix == 0 {
            return Err("invalid cast byte range suffix".to_string());
        }
        return Ok(Some((total.saturating_sub(suffix.min(total)), total - 1)));
    }
    let start = start
        .parse::<u64>()
        .map_err(|_| "invalid cast byte range start".to_string())?;
    let end = if end.is_empty() {
        total.saturating_sub(1)
    } else {
        end.parse::<u64>()
            .map_err(|_| "invalid cast byte range end".to_string())?
            .min(total.saturating_sub(1))
    };
    if start > end || start >= total {
        return Err("cast byte range is outside the resource".to_string());
    }
    Ok(Some((start, end)))
}

fn relay_headers(
    content_type: &str,
    content_range: Option<(u64, u64, u64)>,
) -> Result<Vec<Header>, String> {
    let profile = match content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "audio/mpeg" | "audio/mp3" => "DLNA.ORG_PN=MP3;",
        _ => "",
    };
    let mut headers = vec![
        header("Content-Type", content_type)?,
        header("Accept-Ranges", "bytes")?,
        header("Access-Control-Allow-Origin", "*")?,
        header("transferMode.dlna.org", "Streaming")?,
        header(
            "contentFeatures.dlna.org",
            &format!(
                "{profile}DLNA.ORG_OP=01;DLNA.ORG_CI=0;DLNA.ORG_FLAGS=01500000000000000000000000000000"
            ),
        )?,
    ];
    if let Some((start, end, total)) = content_range {
        headers.push(header(
            "Content-Range",
            &format!("bytes {start}-{end}/{total}"),
        )?);
    }
    Ok(headers)
}

fn stream_content_length(stream: &PreparedStream) -> Option<u64> {
    if stream.uri().starts_with("file:") {
        return Url::parse(stream.uri())
            .ok()?
            .to_file_path()
            .ok()?
            .metadata()
            .ok()
            .map(|metadata| metadata.len());
    }
    let client = upstream_client(stream).ok()?;
    let head = client.head(stream.uri()).send().ok();
    if let Some(length) = head.as_ref().and_then(response_content_length) {
        return Some(length);
    }
    let response = client
        .get(stream.uri())
        .header(reqwest::header::RANGE, "bytes=0-0")
        .send()
        .ok()?;
    response
        .headers()
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.rsplit_once('/'))
        .and_then(|(_, total)| total.parse::<u64>().ok())
        .or_else(|| {
            (response.status().as_u16() == 200)
                .then(|| response_content_length(&response))
                .flatten()
        })
}

fn response_content_length(response: &reqwest::blocking::Response) -> Option<u64> {
    response
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
}

fn upstream_client(stream: &PreparedStream) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .danger_accept_invalid_certs(stream.trust_invalid_certificate())
        .build()
        .map_err(|error| error.to_string())
}

fn empty_response(status: StatusCode) -> ResponseBox {
    Response::new(status, Vec::new(), Cursor::new(Vec::new()), Some(0), None).boxed()
}

fn header(name: &str, value: &str) -> Result<Header, String> {
    Header::from_bytes(name.as_bytes(), value.as_bytes())
        .map_err(|()| format!("invalid cast relay header {name}"))
}

fn random_token() -> Result<String, String> {
    let mut bytes = [0_u8; 24];
    getrandom::fill(&mut bytes).map_err(|error| error.to_string())?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn content_type_from_uri(uri: &str) -> String {
    let path = uri.split('?').next().unwrap_or(uri).to_ascii_lowercase();
    if path.ends_with(".mp3") {
        "audio/mpeg"
    } else if path.ends_with(".flac") {
        "audio/flac"
    } else if path.ends_with(".m4a") || path.ends_with(".mp4") {
        "audio/mp4"
    } else if path.ends_with(".ogg") || path.ends_with(".opus") {
        "audio/ogg"
    } else if path.ends_with(".wav") {
        "audio/wav"
    } else if path.ends_with(".webm") {
        "audio/webm"
    } else {
        "application/octet-stream"
    }
    .to_string()
}

pub(crate) fn source_content_type(stream: &PreparedStream) -> String {
    stream
        .content_type
        .clone()
        .unwrap_or_else(|| content_type_from_uri(stream.uri()))
}

fn content_extension(content_type: &str) -> &'static str {
    match normalize_content_type(content_type)
        .to_ascii_lowercase()
        .as_str()
    {
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/flac" | "audio/x-flac" => "flac",
        "audio/mp4" | "audio/aac" | "audio/alac" => "m4a",
        "audio/ogg" | "audio/opus" => "ogg",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/webm" => "webm",
        _ => "bin",
    }
}

fn normalize_content_type(content_type: &str) -> &str {
    content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim()
}

fn transport_scheme(uri: &str) -> &str {
    uri.split_once(':')
        .map(|(scheme, _)| scheme)
        .unwrap_or("unknown")
}

fn directly_supported(content_type: &str) -> bool {
    matches!(
        content_type
            .split(';')
            .next()
            .unwrap_or(content_type)
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "audio/mpeg"
            | "audio/mp3"
            | "audio/flac"
            | "audio/mp4"
            | "audio/aac"
            | "audio/ogg"
            | "audio/opus"
            | "audio/wav"
            | "audio/x-wav"
            | "audio/webm"
    )
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;

    use super::*;

    #[test]
    fn byte_ranges_are_bounded_to_the_resource() {
        assert_eq!(parse_range(None, 10).unwrap(), None);
        assert_eq!(parse_range(Some("bytes=2-5"), 10).unwrap(), Some((2, 5)));
        assert_eq!(parse_range(Some("bytes=7-"), 10).unwrap(), Some((7, 9)));
        assert_eq!(parse_range(Some("bytes=-3"), 10).unwrap(), Some((7, 9)));
        assert!(parse_range(Some("bytes=10-"), 10).is_err());
        assert!(parse_range(Some("items=1-2"), 10).is_err());
    }

    #[test]
    fn flac_is_not_advertised_as_a_nonstandard_dlna_profile() {
        let headers = relay_headers("audio/flac", None).expect("FLAC relay headers");
        let features = headers
            .iter()
            .find(|header| header.field.equiv("contentFeatures.dlna.org"))
            .map(|header| header.value.as_str())
            .expect("DLNA content features");

        assert!(!features.contains("DLNA.ORG_PN"));
        assert!(features.contains("DLNA.ORG_OP=01"));
    }

    #[test]
    fn direct_media_requires_a_receiver_reachable_url() {
        let loopback = PreparedStream::from(playback::ResolvedStream::new(
            "http://127.0.0.1:8096/audio.flac",
        ));
        assert!(!direct_media_uri(&loopback, false, false));
        assert!(direct_media_uri(&loopback, false, true));

        let remote = PreparedStream::from(playback::ResolvedStream::new(
            "https://music.example/audio.flac",
        ));
        assert!(direct_media_uri(&remote, false, false));
        assert!(!direct_media_uri(&remote, true, false));
    }

    #[test]
    fn transcoded_representation_begins_at_the_saved_logical_position() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("album.flac");
        File::create(&path)
            .expect("create album")
            .write_all(b"flac")
            .expect("write album");
        let stream = PreparedStream::from(
            playback::ResolvedStream::new(
                Url::from_file_path(&path).expect("album URL").to_string(),
            )
            .with_window(10_000, 100_000),
        );
        let mut relay = RelayServer::start(
            "127.0.0.1:9".parse().expect("target"),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .expect("relay");

        let mut negotiated_source = PreparedStream::from(playback::ResolvedStream::new(
            Url::from_file_path(&path).expect("source URL").to_string(),
        ));
        negotiated_source.content_type = Some("audio/alac".to_string());
        let source = relay
            .publish_as(&negotiated_source, RelayRepresentation::Source)
            .expect("publish negotiated source representation");
        assert_eq!(source.content_type, "audio/alac");
        assert!(source.seekable);
        assert!(source.uri.ends_with("/media.m4a"));

        let published = relay
            .publish_at(&stream, RelayRepresentation::Mp3, 42_000)
            .expect("publish clipped MP3 representation");

        assert_eq!(published.content_type, "audio/mpeg");
        assert!(!published.seekable);
        assert!(published.uri.ends_with("/media.mp3"));
        assert_eq!(published.logical_offset_millis, 42_000);
        assert_eq!(published.resource_duration_millis, Some(48_000));
        assert_eq!(published.logical_position_millis(5_000), 47_000);
        let resource = relay
            .resources
            .lock()
            .expect("relay resources")
            .get(published.relay_token.as_ref().expect("relay token"))
            .cloned()
            .expect("published resource");
        assert_eq!(resource.stream.start_millis(), 52_000);
        assert_eq!(resource.stream.end_millis(), Some(100_000));
        relay.shutdown();
    }

    #[test]
    fn selected_casting_network_owns_the_advertised_address() {
        let interfaces = [
            ("docker0", "172.17.0.1".parse().expect("Docker address")),
            ("wlan0", "192.168.1.103".parse().expect("Wi-Fi address")),
            ("wlan0", "fd00::103".parse().expect("Wi-Fi IPv6 address")),
        ];

        assert_eq!(
            selected_interface_address(
                interfaces.iter().copied(),
                "wlan0",
                "192.168.1.50".parse().expect("renderer address"),
            ),
            Some("192.168.1.103".parse().expect("selected address"))
        );
        assert_eq!(
            selected_interface_address(
                interfaces.iter().copied(),
                "wlan0",
                "fd00::50".parse().expect("renderer IPv6 address"),
            ),
            Some("fd00::103".parse().expect("selected IPv6 address"))
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn selected_relay_interface_is_inherited_by_receiver_connections() {
        let listener = relay_listener(IpAddr::V4(Ipv4Addr::LOCALHOST), 0, Some("lo"))
            .expect("interface-bound relay listener");
        let address = listener.local_addr().expect("relay address");
        let receiver = thread::spawn(move || {
            std::net::TcpStream::connect(address).expect("receiver connection")
        });
        let (accepted, _) = listener.accept().expect("accepted receiver connection");

        assert_eq!(
            socket2::SockRef::from(&listener)
                .device()
                .expect("listener interface"),
            Some(b"lo".to_vec())
        );
        assert_eq!(
            socket2::SockRef::from(&accepted)
                .device()
                .expect("accepted interface"),
            Some(b"lo".to_vec())
        );
        receiver.join().expect("receiver thread");
    }

    #[test]
    fn cue_windows_are_published_as_bounded_nonseekable_media() {
        let stream = PreparedStream::from(
            playback::ResolvedStream::new("file:///music/album.flac").with_window(523_613, 612_345),
        );
        let mut relay = RelayServer::start(
            "127.0.0.1:9".parse().expect("target"),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .expect("relay");

        let published = relay.publish(&stream).expect("publish CUE window");

        assert_eq!(published.content_type, "audio/mpeg");
        assert_eq!(published.logical_offset_millis, 0);
        assert_eq!(published.resource_duration_millis, Some(88_732));
        assert!(!published.seekable);
        relay.shutdown();
    }

    #[test]
    fn relay_serves_local_files_heads_ranges_and_expires_urls_on_clear() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("track.mp3");
        let artwork_path = directory.path().join("cover.img");
        let artwork_resolutions = Arc::new(AtomicUsize::new(0));
        let audio = b"0123456789".repeat(4_000);
        File::create(&path)
            .expect("create track")
            .write_all(&audio)
            .expect("write track");
        File::create(&artwork_path)
            .expect("create artwork")
            .write_all(b"\x89PNG\r\n\x1a\ncover")
            .expect("write artwork");
        let uri = Url::from_file_path(&path).expect("file URL").to_string();
        let stream = PreparedStream::from(playback::ResolvedStream::new(uri));
        let resolver_count = Arc::clone(&artwork_resolutions);
        let mut relay = RelayServer::start(
            "127.0.0.1:9".parse().expect("target"),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .expect("start relay")
        .with_artwork_resolver(Arc::new(move |_| {
            resolver_count.fetch_add(1, Ordering::Relaxed);
            Some(artwork_path.clone())
        }));
        assert_eq!(artwork_resolutions.load(Ordering::Relaxed), 0);
        let published = relay.publish(&stream).expect("publish stream");
        assert_eq!(artwork_resolutions.load(Ordering::Relaxed), 1);
        let artwork_uri = published.artwork_uri.expect("artwork URL");
        let first = published.uri;
        let client = reqwest::blocking::Client::new();

        let head = client.head(&first).send().expect("HEAD request");
        assert_eq!(head.status(), reqwest::StatusCode::OK);
        assert_eq!(
            head.headers()
                .get(reqwest::header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok()),
            Some("40000")
        );
        assert!(
            !head
                .headers()
                .contains_key(reqwest::header::TRANSFER_ENCODING)
        );
        let full_range = client
            .get(&first)
            .header(reqwest::header::RANGE, "bytes=0-")
            .send()
            .expect("full range request");
        assert_eq!(full_range.status(), reqwest::StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            full_range
                .headers()
                .get(reqwest::header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok()),
            Some("40000")
        );
        assert!(
            !full_range
                .headers()
                .contains_key(reqwest::header::TRANSFER_ENCODING)
        );
        let range = client
            .get(&first)
            .header(reqwest::header::RANGE, "bytes=2-5")
            .send()
            .expect("range request");
        assert_eq!(range.status(), reqwest::StatusCode::PARTIAL_CONTENT);
        assert_eq!(range.bytes().expect("range body").as_ref(), b"2345");
        let artwork = client.get(artwork_uri).send().expect("artwork request");
        assert_eq!(
            artwork
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("image/png")
        );
        assert!(
            artwork
                .bytes()
                .expect("artwork body")
                .starts_with(b"\x89PNG")
        );

        let second = relay.publish(&stream).expect("replace stream URL").uri;
        assert_ne!(first, second);
        assert_eq!(
            client
                .get(&first)
                .send()
                .expect("retained request")
                .status(),
            reqwest::StatusCode::OK
        );
        relay.clear();
        assert_eq!(
            client.get(first).send().expect("expired request").status(),
            reqwest::StatusCode::NOT_FOUND
        );
        relay.shutdown();
    }

    #[test]
    fn remote_http_media_uses_the_provider_url_directly() {
        let upstream = Server::http("127.0.0.1:0").expect("upstream server");
        let upstream_address = upstream.server_addr().to_ip().expect("upstream address");
        let (sent, received) = mpsc::channel();
        let upstream_thread = thread::spawn(move || {
            for _ in 0..3 {
                let request = upstream.recv().expect("upstream request");
                sent.send(request.url().to_string())
                    .expect("record upstream URL");
                request
                    .respond(Response::from_string("remote audio"))
                    .expect("upstream response");
            }
        });
        let stream = PreparedStream::from(playback::ResolvedStream::new(format!(
            "http://{upstream_address}/audio.mp3?api_key=secret"
        )));
        let mut relay =
            RelayServer::start(upstream_address, Arc::new(AtomicBool::new(false)), None)
                .expect("relay");

        let published = relay.publish(&stream).expect("publish remote stream");

        assert_eq!(published.uri, stream.uri());
        assert!(published.uri.contains("api_key=secret"));
        let head = reqwest::blocking::Client::new()
            .head(&published.uri)
            .send()
            .expect("relay HEAD");
        assert_eq!(
            head.headers()
                .get(reqwest::header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok()),
            Some("12")
        );
        let body = reqwest::blocking::get(published.uri)
            .expect("relay request")
            .text()
            .expect("relay body");
        assert_eq!(body, "remote audio");
        let upstream_urls = received.try_iter().collect::<Vec<_>>();
        assert_eq!(upstream_urls.len(), 3);
        assert!(
            upstream_urls
                .iter()
                .all(|url| url == "/audio.mp3?api_key=secret")
        );
        upstream_thread.join().expect("upstream thread");
        relay.shutdown();
    }

    #[test]
    fn proxy_setting_hides_provider_urls_from_the_renderer() {
        let proxy_media = Arc::new(AtomicBool::new(false));
        let mut relay = RelayServer::start(
            "127.0.0.1:9".parse().expect("target"),
            Arc::clone(&proxy_media),
            None,
        )
        .expect("relay");
        let stream = PreparedStream::from(playback::ResolvedStream::new(
            "https://music.example/audio.mp3?api_key=secret",
        ));

        let direct = relay.publish(&stream).expect("direct resource");
        assert_eq!(direct.uri, stream.uri());

        proxy_media.store(true, Ordering::Release);
        let proxied = relay.publish(&stream).expect("proxied resource");
        assert_ne!(proxied.uri, stream.uri());
        assert!(proxied.uri.starts_with("http://"));
        assert!(!proxied.uri.contains("api_key=secret"));
        relay.shutdown();
    }
}
