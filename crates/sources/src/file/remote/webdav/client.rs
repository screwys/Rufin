use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use digest_auth::{AuthContext, HttpMethod, Qop, WwwAuthenticateHeader};
use futures_util::TryStreamExt;
use reqwest::{
    Client, Method, Response, StatusCode,
    header::{self, HeaderMap, HeaderName, HeaderValue},
};
use tokio::io::BufReader;
use tokio_util::io::{ReaderStream, StreamReader};
use url::Url;

use super::dav;
use crate::{SourceError, SourceResult};

pub(crate) fn custom_headers(values: &[(String, String)]) -> SourceResult<HeaderMap> {
    let mut headers = HeaderMap::new();
    for (name, value) in values {
        headers.insert(
            HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| SourceError::InvalidConfig("Invalid HTTP header name".into()))?,
            HeaderValue::from_str(value)
                .map_err(|_| SourceError::InvalidConfig("Invalid HTTP header value".into()))?,
        );
    }
    Ok(headers)
}

pub(crate) enum Authentication {
    Anonymous,
    Password { username: String, password: String },
    Bearer(String),
}

pub(crate) struct WebDavClient {
    http: Client,
    root: Url,
    authentication: Authentication,
    headers: HeaderMap,
    digest: Mutex<Option<WwwAuthenticateHeader>>,
    failed: AtomicBool,
}

pub(crate) enum Body<'a> {
    Empty,
    Bytes(&'a [u8]),
    File(&'a Path),
}

impl WebDavClient {
    pub fn new(
        mut root: Url,
        authentication: Authentication,
        headers: HeaderMap,
        trust_invalid_certificate: bool,
        certificate_pem: Option<&[u8]>,
    ) -> SourceResult<Self> {
        if !matches!(root.scheme(), "http" | "https")
            || root.host_str().is_none()
            || !root.username().is_empty()
            || root.password().is_some()
            || root.fragment().is_some()
        {
            return Err(SourceError::InvalidConfig(
                "WebDAV requires an HTTP(S) collection URL and separate credentials".into(),
            ));
        }
        if !root.path().ends_with('/') {
            root.path_segments_mut()
                .map_err(|_| SourceError::InvalidConfig("Invalid WebDAV collection URL".into()))?
                .push("");
        }
        root.set_path(&normalized_path(&root));
        let mut http = Client::builder()
            .user_agent(concat!("Rufin/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(20))
            .read_timeout(Duration::from_secs(30))
            .danger_accept_invalid_certs(trust_invalid_certificate)
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                let from = attempt.previous().first();
                if from.is_some_and(|from| from.origin() != attempt.url().origin()) {
                    attempt.error("WebDAV redirected outside the configured server")
                } else if attempt.previous().len() >= 10 {
                    attempt.error("Too many WebDAV redirects")
                } else {
                    attempt.follow()
                }
            }));
        if let Some(pem) = certificate_pem {
            let certificates = reqwest::Certificate::from_pem_bundle(pem)
                .map_err(|e| SourceError::Tls(e.to_string()))?;
            if certificates.is_empty() {
                return Err(SourceError::Tls(
                    "The certificate file contains no PEM certificates".into(),
                ));
            }
            for certificate in certificates {
                http = http.add_root_certificate(certificate);
            }
        }
        let http = http.build().map_err(network_error)?;
        Ok(Self {
            http,
            root,
            authentication,
            headers,
            digest: Mutex::new(None),
            failed: AtomicBool::new(false),
        })
    }

    pub fn root(&self) -> &Url {
        &self.root
    }

    pub fn recursive_etags(&self) -> bool {
        // Nextcloud's file collections propagate child changes to ancestor ETags.
        self.root.path().contains("/remote.php/dav/files/")
            || self.root.path().contains("/remote.php/webdav/")
            || self.root.path().contains("/public.php/dav/files/")
    }

    pub fn is_disconnected(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }

    pub fn resolve_href(&self, collection: &Url, href: &str) -> SourceResult<Url> {
        let mut url = collection
            .join(href)
            .map_err(|e| SourceError::Other(e.to_string()))?;
        url.set_path(&normalized_path(&url));
        let root = self.root.path().trim_end_matches('/');
        if url.origin() != self.root.origin()
            || !(url.path() == root
                || url
                    .path()
                    .strip_prefix(root)
                    .is_some_and(|rest| rest.starts_with('/')))
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(SourceError::InvalidRequest(
                "WebDAV path is outside the configured collection",
            ));
        }
        Ok(url)
    }

    pub async fn list<F: std::future::Future<Output = SourceResult<()>>>(
        &self,
        url: &Url,
        depth: u8,
        accept: impl FnMut(dav::Entry) -> F,
    ) -> SourceResult<()> {
        const PROPERTIES: &[u8] = br#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:" xmlns:o="http://owncloud.org/ns"><d:prop><d:resourcetype/><d:getcontentlength/><d:getlastmodified/><d:getetag/><d:resource-id/><d:sync-token/><o:id/><o:fileid/><o:permissions/></d:prop></d:propfind>"#;
        let mut headers = xml_headers();
        headers.insert(
            "Depth",
            HeaderValue::from_static(if depth == 0 { "0" } else { "1" }),
        );
        let response = self
            .request(
                Method::from_bytes(b"PROPFIND").unwrap(),
                url,
                headers,
                Body::Bytes(PROPERTIES),
            )
            .await?;
        require_status(&response, &[StatusCode::MULTI_STATUS])?;
        let stream = response.bytes_stream().map_err(std::io::Error::other);
        dav::parse(BufReader::new(StreamReader::new(stream)), accept).await?;
        Ok(())
    }

    pub async fn stat(&self, url: &Url) -> SourceResult<dav::Entry> {
        let mut found = None;
        self.list(url, 0, |entry: dav::Entry| {
            std::future::ready((|| {
                let location = self.resolve_href(url, &entry.href)?;
                if location.path().trim_end_matches('/')
                    != normalized_path(url).trim_end_matches('/')
                {
                    return Ok(());
                }
                if let Some(status) = entry.status.filter(|status| !(200..300).contains(status)) {
                    return Err(match status {
                        404 => SourceError::NotFound,
                        401 => SourceError::Auth("WebDAV credentials were rejected".into()),
                        status => SourceError::Server {
                            status,
                            message: "Could not inspect WebDAV file".into(),
                        },
                    });
                }
                found = Some(entry);
                Ok(())
            })())
        })
        .await?;
        found.ok_or(SourceError::NotFound)
    }

    pub async fn sync<F: std::future::Future<Output = SourceResult<()>>>(
        &self,
        url: &Url,
        token: &str,
        accept: impl FnMut(dav::Entry) -> F,
    ) -> SourceResult<Option<String>> {
        let token = quick_xml::escape::escape(token);
        let body = format!(
            r#"<?xml version="1.0"?><d:sync-collection xmlns:d="DAV:" xmlns:o="http://owncloud.org/ns"><d:sync-token>{token}</d:sync-token><d:sync-level>1</d:sync-level><d:prop><d:resourcetype/><d:getcontentlength/><d:getlastmodified/><d:getetag/><d:resource-id/><o:id/><o:fileid/><o:permissions/></d:prop></d:sync-collection>"#
        );
        let response = self
            .request(
                Method::from_bytes(b"REPORT").unwrap(),
                url,
                xml_headers(),
                Body::Bytes(body.as_bytes()),
            )
            .await?;
        require_status(&response, &[StatusCode::MULTI_STATUS])?;
        let stream = response.bytes_stream().map_err(std::io::Error::other);
        dav::parse(BufReader::new(StreamReader::new(stream)), accept).await
    }

    /// A short exclusive write lock, when the server implements DAV locking.
    pub async fn lock(&self, url: &Url, revision: Option<&str>) -> SourceResult<Option<String>> {
        let mut headers = xml_headers();
        if let Some(etag) = revision.filter(|etag| etag.starts_with('"')) {
            headers.insert(header::IF_MATCH, value(etag)?);
        }
        headers.insert("Depth", HeaderValue::from_static("0"));
        headers.insert("Timeout", HeaderValue::from_static("Second-600"));
        let body = br#"<?xml version="1.0"?><d:lockinfo xmlns:d="DAV:"><d:lockscope><d:exclusive/></d:lockscope><d:locktype><d:write/></d:locktype><d:owner>Rufin</d:owner></d:lockinfo>"#;
        let response = self
            .request(
                Method::from_bytes(b"LOCK").unwrap(),
                url,
                headers,
                Body::Bytes(body),
            )
            .await?;
        if matches!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_IMPLEMENTED
        ) {
            return Ok(None);
        }
        require_status(&response, &[StatusCode::OK, StatusCode::CREATED])?;
        response
            .headers()
            .get("Lock-Token")
            .and_then(|h| h.to_str().ok())
            .map(str::to_owned)
            .map(Some)
            .ok_or_else(|| SourceError::Other("WebDAV lock response is missing its token".into()))
    }

    pub async fn unlock(&self, url: &Url, token: &str) -> SourceResult<()> {
        let mut headers = HeaderMap::new();
        headers.insert("Lock-Token", value(token)?);
        let response = self
            .request(
                Method::from_bytes(b"UNLOCK").unwrap(),
                url,
                headers,
                Body::Empty,
            )
            .await?;
        require_status(&response, &[StatusCode::NO_CONTENT])
    }

    pub async fn read(
        &self,
        url: &Url,
        range: Option<(u64, u64)>,
        validator: Option<&str>,
    ) -> SourceResult<Response> {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static("identity"),
        );
        if let Some((start, end)) = range {
            headers.insert(header::RANGE, value(&format!("bytes={start}-{end}"))?);
        }
        if let Some(validator) = validator {
            headers.insert(header::IF_RANGE, value(validator)?);
        }
        self.request(Method::GET, url, headers, Body::Empty).await
    }

    pub async fn upload(&self, url: &Url, file: &Path, create_only: bool) -> SourceResult<()> {
        let mut headers = HeaderMap::new();
        if create_only {
            headers.insert(header::IF_NONE_MATCH, HeaderValue::from_static("*"));
        }
        let response = self
            .request(Method::PUT, url, headers, Body::File(file))
            .await?;
        require_status(
            &response,
            &[StatusCode::CREATED, StatusCode::NO_CONTENT, StatusCode::OK],
        )
    }

    pub async fn move_file(
        &self,
        from: &Url,
        to: &Url,
        overwrite: bool,
        condition: Option<&str>,
    ) -> SourceResult<()> {
        self.resolve_href(&self.root, to.as_str())?;
        let mut headers = HeaderMap::new();
        headers.insert("Destination", value(to.as_str())?);
        headers.insert(
            "Overwrite",
            HeaderValue::from_static(if overwrite { "T" } else { "F" }),
        );
        if let Some(condition) = condition {
            headers.insert("If", value(condition)?);
        }
        let response = self
            .request(
                Method::from_bytes(b"MOVE").unwrap(),
                from,
                headers,
                Body::Empty,
            )
            .await?;
        require_status(&response, &[StatusCode::CREATED, StatusCode::NO_CONTENT])
    }

    pub async fn remove(&self, url: &Url) -> SourceResult<()> {
        let response = self
            .request(Method::DELETE, url, HeaderMap::new(), Body::Empty)
            .await?;
        require_status(&response, &[StatusCode::OK, StatusCode::NO_CONTENT])
    }

    pub(crate) async fn request(
        &self,
        method: Method,
        url: &Url,
        headers: HeaderMap,
        body: Body<'_>,
    ) -> SourceResult<Response> {
        self.resolve_href(&self.root, url.as_str())?;
        for attempt in 0..2 {
            let mut request = self
                .http
                .request(method.clone(), url.clone())
                .headers(self.headers.clone())
                .headers(headers.clone());
            match &self.authentication {
                Authentication::Anonymous => {}
                Authentication::Bearer(token) => request = request.bearer_auth(token),
                Authentication::Password { username, password } => {
                    let authorization = {
                        let mut digest = self.digest.lock().unwrap_or_else(|p| p.into_inner());
                        digest
                            .as_mut()
                            .map(|prompt| {
                                let uri =
                                    &url[url::Position::BeforePath..url::Position::AfterQuery];
                                let payload = match &body {
                                    Body::Bytes(bytes) => *bytes,
                                    _ => &[],
                                };
                                prompt.respond(&AuthContext::new_with_method(
                                    username.as_str(),
                                    password.as_str(),
                                    uri,
                                    Some(payload),
                                    HttpMethod(method.as_str().into()),
                                ))
                            })
                            .transpose()
                            .map_err(|e| SourceError::Auth(e.to_string()))?
                    };
                    if let Some(mut authorization) = authorization {
                        if authorization.qop == Some(Qop::AUTH_INT)
                            && let Body::File(path) = body
                        {
                            super::digest::file_response(
                                &mut authorization,
                                &method,
                                username,
                                password,
                                path,
                            )
                            .await?;
                        }
                        request = request.header(header::AUTHORIZATION, authorization.to_string());
                    } else {
                        request = request.basic_auth(username, Some(password));
                    }
                }
            }
            request = match &body {
                Body::Empty => request,
                Body::Bytes(bytes) => request.body(bytes.to_vec()),
                Body::File(path) => {
                    let file = tokio::fs::File::open(path).await.map_err(io_error)?;
                    let length = file.metadata().await.map_err(io_error)?.len();
                    request
                        .header(header::CONTENT_LENGTH, length)
                        .body(reqwest::Body::wrap_stream(ReaderStream::new(file)))
                }
            };
            let response = request.send().await.map_err(|error| {
                if error.is_connect() || error.is_timeout() || error.is_body() {
                    self.failed.store(true, Ordering::Relaxed);
                }
                network_error(error)
            })?;
            if matches!(response.status().as_u16(), 502 | 503 | 504) {
                self.failed.store(true, Ordering::Relaxed);
            }
            if response.status() == StatusCode::UNAUTHORIZED
                && attempt == 0
                && matches!(self.authentication, Authentication::Password { .. })
            {
                let challenge = response
                    .headers()
                    .get_all(header::WWW_AUTHENTICATE)
                    .iter()
                    .filter_map(|h| h.to_str().ok())
                    .find_map(|h| digest_auth::parse(h).ok());
                if let Some(challenge) = challenge {
                    *self.digest.lock().unwrap_or_else(|p| p.into_inner()) = Some(challenge);
                    continue;
                }
            }
            return Ok(response);
        }
        unreachable!()
    }
}

// DAV servers can return lowercase percent escapes for the same requested path.
fn normalized_path(url: &Url) -> String {
    let mut path = url.path().as_bytes().to_vec();
    for index in 0..path.len().saturating_sub(2) {
        if path[index] == b'%'
            && path[index + 1].is_ascii_hexdigit()
            && path[index + 2].is_ascii_hexdigit()
        {
            path[index + 1..=index + 2].make_ascii_uppercase();
        }
    }
    String::from_utf8(path).expect("URL paths are UTF-8")
}

fn xml_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/xml; charset=utf-8"),
    );
    headers
}

fn require_status(response: &Response, accepted: &[StatusCode]) -> SourceResult<()> {
    if accepted.contains(&response.status()) {
        return Ok(());
    }
    if response.status().is_success()
        && response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/html"))
    {
        return Err(SourceError::Auth("The server returned a browser sign-in page instead of WebDAV. Use an app password or the server's WebDAV address.".into()));
    }
    match response.status() {
        StatusCode::UNAUTHORIZED => {
            Err(SourceError::Auth("WebDAV credentials were rejected".into()))
        }
        StatusCode::NOT_FOUND => Err(SourceError::NotFound),
        status => Err(SourceError::Server {
            status: status.as_u16(),
            message: "WebDAV request failed".into(),
        }),
    }
}

fn value(value: &str) -> SourceResult<HeaderValue> {
    HeaderValue::from_str(value).map_err(|e| SourceError::InvalidConfig(e.to_string()))
}

fn network_error(error: reqwest::Error) -> SourceError {
    use std::error::Error;
    let error = error.without_url();
    let mut cause: Option<&(dyn Error + 'static)> = Some(&error);
    while let Some(current) = cause {
        let message = current.to_string();
        let lower = message.to_ascii_lowercase();
        if lower.contains("certificate") || lower.contains("tls") {
            return SourceError::Tls(message);
        }
        cause = current.source();
    }
    SourceError::Network(error.to_string())
}

fn io_error(error: std::io::Error) -> SourceError {
    SourceError::Other(error.to_string())
}
