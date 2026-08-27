use crate::{ImageBytes, SourceError, SourceResult};
use reqwest::{Client, StatusCode, header};
use serde::de::DeserializeOwned;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tracing::{debug, warn};

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BodyLimit {
    pub max_bytes: usize,
    pub context: &'static str,
}

#[derive(Clone, Copy)]
pub struct RemoteHttpPolicy {
    pub service: &'static str,
    pub auth_context: &'static str,
    pub error_body: BodyLimit,
    pub redact_error_url: Option<fn(&mut reqwest::Url)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RemoteTimeouts {
    pub connect: Duration,
    pub request: Duration,
}

pub fn build_client(
    trust_invalid_cert: bool,
    timeouts: RemoteTimeouts,
    policy: RemoteHttpPolicy,
) -> SourceResult<Client> {
    configured_client_builder(trust_invalid_cert, timeouts)
        .build()
        .map_err(|error| map_reqwest_error(error, policy))
}

pub fn build_http1_client(
    trust_invalid_cert: bool,
    timeouts: RemoteTimeouts,
    policy: RemoteHttpPolicy,
) -> SourceResult<Client> {
    configured_client_builder(trust_invalid_cert, timeouts)
        .http1_only()
        .build()
        .map_err(|error| map_reqwest_error(error, policy))
}

fn configured_client_builder(
    trust_invalid_cert: bool,
    timeouts: RemoteTimeouts,
) -> reqwest::ClientBuilder {
    Client::builder()
        .danger_accept_invalid_certs(trust_invalid_cert)
        .connect_timeout(timeouts.connect)
        .timeout(timeouts.request)
}

pub async fn json<T: DeserializeOwned>(
    request: reqwest::RequestBuilder,
    policy: RemoteHttpPolicy,
    limit: BodyLimit,
) -> SourceResult<T> {
    let checked = checked_response(request, policy).await?;
    let bytes =
        response_bytes_bounded(checked.response, policy, limit, Some(&checked.request)).await?;
    deserialize_json(&bytes, &checked.request)
}

pub async fn json_with_header<T: DeserializeOwned>(
    request: reqwest::RequestBuilder,
    policy: RemoteHttpPolicy,
    limit: BodyLimit,
    response_header: &header::HeaderName,
) -> SourceResult<(T, Option<String>)> {
    let checked = checked_response(request, policy).await?;
    let value = checked
        .response
        .headers()
        .get(response_header)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes =
        response_bytes_bounded(checked.response, policy, limit, Some(&checked.request)).await?;
    Ok((deserialize_json(&bytes, &checked.request)?, value))
}

fn deserialize_json<T: DeserializeOwned>(
    bytes: &[u8],
    request: &RequestMetadata,
) -> SourceResult<T> {
    let mut deserializer = serde_json::Deserializer::from_slice(&bytes);
    serde_path_to_error::deserialize::<_, T>(&mut deserializer).map_err(|error| {
        let field = error.path().to_string();
        warn!(
            request = request.id,
            service = request.service,
            method = %request.method,
            endpoint = %request.endpoint,
            %field,
            error = %error.inner(),
            "remote JSON response did not match the expected shape"
        );
        SourceError::Other(format!(
            "{} response at {} field {}: {}",
            request.service,
            request.endpoint,
            field,
            error.inner()
        ))
    })
}

pub async fn unit(request: reqwest::RequestBuilder, policy: RemoteHttpPolicy) -> SourceResult<()> {
    checked_response(request, policy).await?;
    Ok(())
}

pub async fn bytes(
    request: reqwest::RequestBuilder,
    policy: RemoteHttpPolicy,
    limit: BodyLimit,
) -> SourceResult<ImageBytes> {
    let checked = checked_response(request, policy).await?;
    let content_type = checked
        .response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes =
        response_bytes_bounded(checked.response, policy, limit, Some(&checked.request)).await?;
    let declared_image = content_type.as_deref().is_none_or(|value| {
        let value = value.split(';').next().unwrap_or(value).trim();
        value.starts_with("image/") || value.eq_ignore_ascii_case("application/octet-stream")
    });
    let first = bytes
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace());
    if bytes.is_empty() || !declared_image || matches!(first, Some(b'{') | Some(b'[') | Some(b'<'))
    {
        return Err(SourceError::NotFound);
    }
    Ok(ImageBytes {
        bytes,
        content_type,
    })
}

pub fn map_reqwest_error(mut error: reqwest::Error, policy: RemoteHttpPolicy) -> SourceError {
    if let Some(redact) = policy.redact_error_url
        && let Some(url) = error.url_mut()
    {
        redact(url);
    }
    let message = error.to_string();
    let lowered = message.to_lowercase();
    if lowered.contains("certificate") || lowered.contains("tls") {
        SourceError::Tls(message)
    } else if error.is_connect() || error.is_request() || error.is_timeout() {
        SourceError::Network(message)
    } else if let Some(status) = error.status() {
        SourceError::Server {
            status: status.as_u16(),
            message,
        }
    } else {
        SourceError::Other(message)
    }
}

async fn checked_response(
    request: reqwest::RequestBuilder,
    policy: RemoteHttpPolicy,
) -> SourceResult<CheckedResponse> {
    let (client, request) = request.build_split();
    let request = request.map_err(|error| map_reqwest_error(error, policy))?;
    let request_metadata = RequestMetadata::new(&request, policy.service);
    let started = Instant::now();
    debug!(
        request = request_metadata.id,
        service = request_metadata.service,
        method = %request_metadata.method,
        endpoint = %request_metadata.endpoint,
        query_keys = %request_metadata.query_keys,
        "sending remote request"
    );
    let response = client.execute(request).await.map_err(|error| {
        let error = map_reqwest_error(error, policy);
        warn!(
            request = request_metadata.id,
            service = request_metadata.service,
            method = %request_metadata.method,
            endpoint = %request_metadata.endpoint,
            elapsed_ms = started.elapsed().as_millis(),
            %error,
            "remote request failed"
        );
        error
    })?;
    let status = response.status();
    debug!(
        request = request_metadata.id,
        service = request_metadata.service,
        method = %request_metadata.method,
        endpoint = %request_metadata.endpoint,
        status = status.as_u16(),
        elapsed_ms = started.elapsed().as_millis(),
        "received remote response"
    );
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return Err(SourceError::Auth(format!(
            "{} {}",
            policy.auth_context,
            status.as_u16()
        )));
    }
    if status == StatusCode::NOT_FOUND {
        return Err(SourceError::NotFound);
    }
    if status.is_client_error() || status.is_server_error() {
        let message =
            response_text_or_status(response, status, policy, Some(&request_metadata)).await;
        return Err(SourceError::Server {
            status: status.as_u16(),
            message,
        });
    }
    Ok(CheckedResponse {
        response,
        request: request_metadata,
    })
}

async fn response_text_or_status(
    response: reqwest::Response,
    status: StatusCode,
    policy: RemoteHttpPolicy,
    request: Option<&RequestMetadata>,
) -> String {
    match response_bytes_bounded(response, policy, policy.error_body, request).await {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(_) => status.to_string(),
    }
}

async fn response_bytes_bounded(
    mut response: reqwest::Response,
    policy: RemoteHttpPolicy,
    limit: BodyLimit,
    request: Option<&RequestMetadata>,
) -> SourceResult<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > limit.max_bytes as u64)
    {
        return Err(size_error(limit));
    }

    let mut bytes = Vec::with_capacity(
        response
            .content_length()
            .unwrap_or_default()
            .min(limit.max_bytes as u64) as usize,
    );
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| map_reqwest_error(error, policy))?
    {
        if bytes
            .len()
            .checked_add(chunk.len())
            .is_none_or(|length| length > limit.max_bytes)
        {
            return Err(size_error(limit));
        }
        bytes.extend_from_slice(&chunk);
    }
    if let Some(request) = request {
        debug!(
            request = request.id,
            service = request.service,
            method = %request.method,
            endpoint = %request.endpoint,
            bytes = bytes.len(),
            "read remote response body"
        );
    }
    Ok(bytes)
}

pub(crate) async fn bounded_response_body(
    response: reqwest::Response,
    policy: RemoteHttpPolicy,
    limit: BodyLimit,
) -> SourceResult<Vec<u8>> {
    response_bytes_bounded(response, policy, limit, None).await
}

struct CheckedResponse {
    response: reqwest::Response,
    request: RequestMetadata,
}

struct RequestMetadata {
    id: u64,
    service: &'static str,
    method: String,
    endpoint: String,
    query_keys: String,
}

impl RequestMetadata {
    fn new(request: &reqwest::Request, service: &'static str) -> Self {
        Self {
            id: NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed),
            service,
            method: request.method().to_string(),
            endpoint: request.url().path().to_string(),
            query_keys: request
                .url()
                .query_pairs()
                .map(|(key, _)| key.into_owned())
                .collect::<Vec<_>>()
                .join(","),
        }
    }
}

fn size_error(limit: BodyLimit) -> SourceError {
    SourceError::Other(format!(
        "{} exceeded {} MiB limit",
        limit.context,
        limit.max_bytes / 1024 / 1024
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose};
    use serde::Deserialize;
    use std::sync::Arc;
    use tokio::net::TcpListener;
    use tokio_rustls::TlsAcceptor;
    use tokio_rustls::rustls::{ServerConfig, pki_types::PrivateKeyDer};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SMALL_BODY: BodyLimit = BodyLimit {
        max_bytes: 3,
        context: "test response",
    };
    const POLICY: RemoteHttpPolicy = RemoteHttpPolicy {
        service: "test-server",
        auth_context: "Test server returned",
        error_body: BodyLimit {
            max_bytes: 32,
            context: "test error response",
        },
        redact_error_url: None,
    };

    #[derive(Debug, Deserialize)]
    struct Payload {
        value: String,
    }

    #[derive(Debug, Deserialize)]
    #[expect(dead_code, reason = "the fixture must fail before producing Songs")]
    struct Songs {
        songs: Vec<Song>,
    }

    #[derive(Debug, Deserialize)]
    #[expect(
        dead_code,
        reason = "the invalid duration is the deserialization boundary"
    )]
    struct Song {
        duration: u32,
    }

    #[tokio::test]
    async fn json_reads_bounded_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/payload"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": "ok"
            })))
            .mount(&server)
            .await;

        let client = Client::new();
        let url = format!("{}/payload", server.uri());
        let payload: Payload = json(
            client.get(url),
            POLICY,
            BodyLimit {
                max_bytes: 32,
                context: "test JSON response",
            },
        )
        .await
        .expect("payload");

        assert_eq!(payload.value, "ok");
    }

    #[tokio::test]
    async fn json_errors_name_the_endpoint_and_field_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/songs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "songs": [{"duration": 747.9273376464844}]
            })))
            .mount(&server)
            .await;

        let client = Client::new();
        let error = json::<Songs>(
            client.get(format!("{}/songs", server.uri())),
            POLICY,
            BodyLimit {
                max_bytes: 1_024,
                context: "test JSON response",
            },
        )
        .await
        .expect_err("invalid duration");

        let message = error.to_string();
        assert!(message.contains("/songs"));
        assert!(message.contains("songs[0].duration"));
    }

    #[tokio::test]
    async fn status_errors_map_to_provider_errors() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/auth"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/missing"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/broken"))
            .respond_with(ResponseTemplate::new(500).set_body_string("broken"))
            .mount(&server)
            .await;

        let client = Client::new();
        let auth = unit(client.get(format!("{}/auth", server.uri())), POLICY)
            .await
            .expect_err("auth error");
        let missing = unit(client.get(format!("{}/missing", server.uri())), POLICY)
            .await
            .expect_err("missing error");
        let broken = unit(client.get(format!("{}/broken", server.uri())), POLICY)
            .await
            .expect_err("server error");

        assert!(matches!(auth, SourceError::Auth(_)));
        assert!(matches!(missing, SourceError::NotFound));
        assert!(matches!(
            broken,
            SourceError::Server {
                status: 500,
                message
            } if message == "broken"
        ));
    }

    #[tokio::test]
    async fn bytes_preserve_content_type_and_limit_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/image"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/jpeg")
                    .set_body_bytes(vec![1_u8, 2, 3]),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/large"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0_u8; 4]))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/not-image"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_string("{\"error\":\"missing cover\"}"),
            )
            .mount(&server)
            .await;

        let client = Client::new();
        let image = bytes(
            client.get(format!("{}/image", server.uri())),
            POLICY,
            SMALL_BODY,
        )
        .await
        .expect("image");
        let large = bytes(
            client.get(format!("{}/large", server.uri())),
            POLICY,
            SMALL_BODY,
        )
        .await
        .expect_err("oversized body");
        let not_image = bytes(
            client.get(format!("{}/not-image", server.uri())),
            POLICY,
            BodyLimit {
                max_bytes: 1_024,
                context: "test image response",
            },
        )
        .await
        .expect_err("non-image success body");

        assert_eq!(image.bytes, vec![1, 2, 3]);
        assert_eq!(image.content_type.as_deref(), Some("image/jpeg"));
        assert!(large.to_string().contains("test response exceeded"));
        assert!(matches!(not_image, SourceError::NotFound));
    }

    #[tokio::test]
    async fn timeout_maps_to_network_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/slow"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(200))
                    .set_body_string("{}"),
            )
            .mount(&server)
            .await;
        let client = build_client(
            false,
            RemoteTimeouts {
                connect: Duration::from_secs(1),
                request: Duration::from_millis(20),
            },
            POLICY,
        )
        .expect("client");

        let error = unit(client.get(format!("{}/slow", server.uri())), POLICY)
            .await
            .expect_err("timeout");

        assert!(matches!(error, SourceError::Network(_)));
    }

    #[tokio::test]
    async fn http1_client_does_not_offer_http2_during_tls_negotiation() {
        const CERTIFICATE: &str = "MIIBkjCCATmgAwIBAgIUPRq7UtGROBB8IhPuCf+lPcpCA0UwCgYIKoZIzj0EAwIwFDESMBAGA1UEAwwJbG9jYWxob3N0MB4XDTI2MDgxMTE3NDEzM1oXDTM2MDgwODE3NDEzM1owFDESMBAGA1UEAwwJbG9jYWxob3N0MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE9qqA1jD7b5Z5WOQHe4rbPv6v2Uie5/t4dGjMa9X3WgyKShtzCWUlq5NcPUCa0RGdmeeccSMXBj3/3lpCX2mZMKNpMGcwHQYDVR0OBBYEFMfL7ybwXwNbtfHaiX6cC62zXEz2MB8GA1UdIwQYMBaAFMfL7ybwXwNbtfHaiX6cC62zXEz2MA8GA1UdEwEB/wQFMAMBAf8wFAYDVR0RBA0wC4IJbG9jYWxob3N0MAoGCCqGSM49BAMCA0cAMEQCICt22OMG72rCqjYhjfmM0JmgLEXVeANQIG21eHjZ7lqWAiB2XDCaK7EVao3BVbf1j3e34+nh1r+6DCC+aMtZcKh5Kg==";
        const PRIVATE_KEY: &str = "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgFd6Y2EAjgZb2gDA8FH395jckz20p2BhtiZsYV6cITSuhRANCAAT2qoDWMPtvlnlY5Ad7its+/q/ZSJ7n+3h0aMxr1fdaDIpKG3MJZSWrk1w9QJrREZ2Z55xxIxcGPf/eWkJfaZkw";

        let certificate = general_purpose::STANDARD
            .decode(CERTIFICATE)
            .expect("test certificate");
        let private_key = general_purpose::STANDARD
            .decode(PRIVATE_KEY)
            .expect("test private key");
        let mut tls_config = ServerConfig::builder_with_provider(Arc::new(
            tokio_rustls::rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("TLS protocol versions")
        .with_no_client_auth()
        .with_single_cert(
            vec![certificate.into()],
            PrivateKeyDer::try_from(private_key).expect("PKCS#8 test private key"),
        )
        .expect("TLS server configuration");
        tls_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        let tls_acceptor = TlsAcceptor::from(Arc::new(tls_config));
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("TLS listener");
        let address = listener.local_addr().expect("TLS listener address");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("TLS connection");
            let stream = tls_acceptor.accept(stream).await.expect("TLS handshake");
            stream
                .get_ref()
                .1
                .alpn_protocol()
                .expect("negotiated ALPN protocol")
                .to_vec()
        });
        let client = build_http1_client(
            true,
            RemoteTimeouts {
                connect: Duration::from_secs(1),
                request: Duration::from_secs(1),
            },
            POLICY,
        )
        .expect("HTTP/1 client");

        let request = client.get(format!("https://localhost:{}/socket", address.port()));
        let (_, protocol) = tokio::join!(request.send(), server);

        assert_eq!(protocol.expect("TLS server"), b"http/1.1");
    }
}
