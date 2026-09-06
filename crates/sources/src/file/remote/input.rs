//! Source-owned HTTP byte access for existing playback and download consumers.
//! URLs carry an observed path; Library remains the owner of media identity.

use std::collections::HashMap;
use std::convert::Infallible;
use std::io;
use std::sync::{Arc, Mutex, Weak};

use bytes::Bytes;
use futures_util::TryStreamExt;
use http_body_util::{BodyExt, Empty, StreamBody, combinators::UnsyncBoxBody};
use hyper::{Method, Request, Response, StatusCode, body::Frame, header};
use hyper_util::rt::TokioIo;
use tokio::task::{AbortHandle, JoinSet};
use url::Url;

use crate::file::remote::{smb::SmbClient, webdav::client::WebDavClient};
use crate::{SourceError, SourceResult};

pub(crate) enum FileInput {
    Smb(Arc<SmbClient>),
    WebDav(Arc<WebDavClient>),
}

pub(crate) struct FileInputServer {
    base: Url,
    task: AbortHandle,
    input: Arc<FileInput>,
    spooled: Mutex<HashMap<(String, String), Weak<tempfile::TempPath>>>,
}

type Body = UnsyncBoxBody<Bytes, io::Error>;

impl FileInputServer {
    pub(crate) fn input(&self) -> &FileInput {
        &self.input
    }
    pub async fn start(input: FileInput) -> SourceResult<Arc<Self>> {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(io_error)?;
        let mut token = [0_u8; 24];
        getrandom::fill(&mut token).map_err(|e| SourceError::Other(e.to_string()))?;
        let token: String = token.iter().map(|b| format!("{b:02x}")).collect();
        let prefix = format!("/{token}/");
        let base = Url::parse(&format!(
            "http://{}{prefix}",
            listener.local_addr().map_err(io_error)?
        ))
        .map_err(|e| SourceError::Other(e.to_string()))?;
        let input = Arc::new(input);
        let serving_input = Arc::clone(&input);
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((socket, _)) = accepted else { break; };
                        let input = Arc::clone(&serving_input);
                        let prefix = prefix.clone();
                        connections.spawn(async move {
                            let service = hyper::service::service_fn(move |request| {
                                let input = Arc::clone(&input);
                                let prefix = prefix.clone();
                                async move { Ok::<_, Infallible>(serve(&input, &prefix, request).await) }
                            });
                            let _ = hyper::server::conn::http1::Builder::new()
                                .keep_alive(false)
                                .serve_connection(TokioIo::new(socket), service).await;
                        });
                    }
                    _ = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        }).abort_handle();
        Ok(Arc::new(Self {
            base,
            task,
            input,
            spooled: Mutex::new(HashMap::new()),
        }))
    }

    pub fn stream(self: &Arc<Self>, path: &str, media_uri: &str) -> playback::ResolvedStream {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .expect("HTTP URL")
            .pop_if_empty()
            .push(path);
        playback::ResolvedStream::with_redacted(url.as_str(), media_uri)
            .with_resource(Arc::clone(self) as Arc<dyn Send + Sync>)
    }

    pub async fn playback_stream(
        self: &Arc<Self>,
        path: &str,
        revision: &str,
        media_uri: &str,
    ) -> SourceResult<playback::ResolvedStream> {
        let FileInput::WebDav(client) = &*self.input else {
            return Ok(self.stream(path, media_uri));
        };
        let key = (path.to_owned(), revision.to_owned());
        {
            let mut spooled = self.spooled.lock().unwrap_or_else(|p| p.into_inner());
            spooled.retain(|_, file| file.strong_count() > 0);
            if let Some(file) = spooled.get(&key).and_then(Weak::upgrade) {
                return temporary_stream(file, media_uri);
            }
        }
        let url = client.resolve_href(client.root(), path)?;
        // Nextcloud treats a zero range end as open-ended; probe two bytes instead.
        let mut response = client.read(&url, Some((0, 1)), None).await?;
        if response.status() == StatusCode::PARTIAL_CONTENT {
            let range = response
                .headers()
                .get(header::CONTENT_RANGE)
                .and_then(|h| h.to_str().ok());
            if range
                .and_then(super::reader::content_range)
                .is_some_and(|(start, end, length)| start == 0 && end == 1.min(length - 1))
            {
                return Ok(self.stream(path, media_uri));
            }
            // A partial response without a usable total length cannot back a seekable decoder.
            response = client.read(&url, None, None).await?;
        }
        if response.status() != StatusCode::OK {
            return Err(SourceError::Server {
                status: response.status().as_u16(),
                message: "WebDAV file could not be read".into(),
            });
        }
        let temporary = tempfile::NamedTempFile::new().map_err(io_error)?;
        let (file, path) = temporary.into_parts();
        let mut file = tokio::fs::File::from_std(file);
        let mut reader =
            tokio_util::io::StreamReader::new(response.bytes_stream().map_err(io::Error::other));
        tokio::io::copy(&mut reader, &mut file)
            .await
            .map_err(io_error)?;
        drop(file);
        let path = Arc::new(path);
        self.spooled
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(key, Arc::downgrade(&path));
        temporary_stream(path, media_uri)
    }
}

impl Drop for FileInputServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(
    input: &FileInput,
    prefix: &str,
    request: Request<hyper::body::Incoming>,
) -> Response<Body> {
    if request.method() != Method::GET && request.method() != Method::HEAD {
        return empty(StatusCode::METHOD_NOT_ALLOWED);
    }
    let Some(path) = request.uri().path().strip_prefix(prefix) else {
        return empty(StatusCode::NOT_FOUND);
    };
    let Ok(path) = percent_encoding::percent_decode_str(path).decode_utf8() else {
        return empty(StatusCode::BAD_REQUEST);
    };
    let result = match input {
        FileInput::Smb(client) => smb_response(client, &path, &request).await,
        FileInput::WebDav(client) => dav_response(client, &path, &request).await,
    };
    result.unwrap_or_else(|error| {
        empty(match error {
            SourceError::NotFound => StatusCode::NOT_FOUND,
            SourceError::Auth(_) => StatusCode::UNAUTHORIZED,
            SourceError::InvalidRequest(_) | SourceError::InvalidConfig(_) => {
                StatusCode::BAD_REQUEST
            }
            SourceError::Server { status, .. } => {
                StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY)
            }
            _ => StatusCode::BAD_GATEWAY,
        })
    })
}

async fn smb_response(
    client: &SmbClient,
    path: &str,
    request: &Request<hyper::body::Incoming>,
) -> SourceResult<Response<Body>> {
    let (file, entry) = client.open_read(path).await?;
    let etag = format!(
        "\"{}:{}\"",
        entry.native_id.as_deref().unwrap_or(""),
        entry.revision
    );
    let range = request.headers().get(header::RANGE).filter(|_| {
        request
            .headers()
            .get(header::IF_RANGE)
            .is_none_or(|value| value == etag.as_str())
    });
    let (start, end, partial) = match range {
        Some(value) => match value
            .to_str()
            .ok()
            .and_then(|value| byte_range(value, entry.size))
        {
            Some((start, end)) => (start, end, true),
            None => {
                file.close()
                    .await
                    .map_err(|e| SourceError::Network(e.to_string()))?;
                let mut response = empty(StatusCode::RANGE_NOT_SATISFIABLE);
                response.headers_mut().insert(
                    header::CONTENT_RANGE,
                    format!("bytes */{}", entry.size).parse().unwrap(),
                );
                return Ok(response);
            }
        },
        None => (0, entry.size, false),
    };
    let mut response = empty(if partial {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    });
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_LENGTH, (end - start).into());
    headers.insert(
        header::ACCEPT_RANGES,
        header::HeaderValue::from_static("bytes"),
    );
    headers.insert(header::ETAG, etag.parse().unwrap());
    if partial {
        headers.insert(
            header::CONTENT_RANGE,
            format!("bytes {start}-{}/{}", end - 1, entry.size)
                .parse()
                .unwrap(),
        );
    }
    if request.method() == Method::HEAD || end == start {
        file.close()
            .await
            .map_err(|e| SourceError::Network(e.to_string()))?;
    } else {
        let stream =
            futures_util::stream::try_unfold((file, start), move |(file, offset)| async move {
                if offset >= end {
                    file.close().await.map_err(io::Error::other)?;
                    return Ok(None);
                }
                let bytes = SmbClient::read(&file, offset, (end - offset).min(64 * 1024) as usize)
                    .await
                    .map_err(io::Error::other)?;
                let count = bytes.len();
                if count == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "SMB file ended before its reported length",
                    ));
                }
                Ok(Some((
                    Frame::data(Bytes::from(bytes)),
                    (file, offset + count as u64),
                )))
            });
        *response.body_mut() = BodyExt::boxed_unsync(StreamBody::new(stream));
    }
    Ok(response)
}

async fn dav_response(
    client: &WebDavClient,
    path: &str,
    request: &Request<hyper::body::Incoming>,
) -> SourceResult<Response<Body>> {
    let url = client.resolve_href(client.root(), path)?;
    let mut headers = header::HeaderMap::new();
    headers.insert(
        header::ACCEPT_ENCODING,
        header::HeaderValue::from_static("identity"),
    );
    for name in [header::RANGE, header::IF_RANGE] {
        if let Some(value) = request.headers().get(&name) {
            headers.insert(name, value.clone());
        }
    }
    let remote = client
        .request(
            request.method().clone(),
            &url,
            headers,
            crate::file::remote::webdav::client::Body::Empty,
        )
        .await?;
    let mut response = empty(remote.status());
    for name in [
        header::CONTENT_LENGTH,
        header::CONTENT_TYPE,
        header::CONTENT_RANGE,
        header::ACCEPT_RANGES,
        header::ETAG,
        header::LAST_MODIFIED,
    ] {
        if let Some(value) = remote.headers().get(&name) {
            response.headers_mut().insert(name, value.clone());
        }
    }
    if request.method() != Method::HEAD {
        let stream = remote
            .bytes_stream()
            .map_ok(Frame::data)
            .map_err(io::Error::other);
        *response.body_mut() = BodyExt::boxed_unsync(StreamBody::new(stream));
    }
    Ok(response)
}

/// One byte range, with an exclusive end. Multiple ranges are not used by our consumers.
fn byte_range(value: &str, length: u64) -> Option<(u64, u64)> {
    let (start, end) = value.strip_prefix("bytes=")?.split_once('-')?;
    if start.is_empty() {
        let count = end.parse::<u64>().ok()?;
        return (count > 0 && length > 0).then_some((length.saturating_sub(count), length));
    }
    let start = start.parse::<u64>().ok()?;
    let end = if end.is_empty() {
        length
    } else {
        end.parse::<u64>().ok()?.saturating_add(1).min(length)
    };
    (start < end && start < length).then_some((start, end))
}

fn empty(status: StatusCode) -> Response<Body> {
    let mut response = Response::new(
        Empty::<Bytes>::new()
            .map_err(|never| match never {})
            .boxed_unsync(),
    );
    *response.status_mut() = status;
    response
}

fn io_error(error: io::Error) -> SourceError {
    SourceError::Network(error.to_string())
}

fn temporary_stream(
    path: Arc<tempfile::TempPath>,
    media_uri: &str,
) -> SourceResult<playback::ResolvedStream> {
    let uri = Url::from_file_path(path.as_ref()).map_err(|_| {
        SourceError::Other("Temporary media path could not be represented as a URI".into())
    })?;
    Ok(playback::ResolvedStream::with_redacted(uri.as_str(), media_uri).with_resource(path))
}
