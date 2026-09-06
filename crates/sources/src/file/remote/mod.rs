//! Remote file acquisition, working copies and protocol implementations.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use url::Url;

use crate::file::remote::input::{FileInput, FileInputServer};
use crate::file::remote::webdav::client::{Authentication, WebDavClient};
use crate::{SourceConfiguration, SourceError, SourceId, SourceResult};

mod artwork;
pub(crate) mod changes;
mod cue;
pub(crate) mod input;
mod metadata;
mod playlist;
pub(crate) mod reader;
mod scan;
pub(crate) mod smb;
#[cfg(test)]
mod tests;
pub(crate) mod webdav;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileAuthentication {
    Password,
    Anonymous,
    Bearer,
}

/// Header values and the password/token belong in the existing credential store.
#[derive(Clone, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct FileCredentials {
    pub secret: String,
    pub headers: Vec<(String, String)>,
}

impl std::fmt::Debug for FileCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FileCredentials")
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Default, Eq, PartialEq)]
pub struct FileCredentialsEdit {
    pub secret: Option<String>,
    pub headers: Option<Vec<(String, String)>>,
}

impl std::fmt::Debug for FileCredentialsEdit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FileCredentialsEdit")
            .finish_non_exhaustive()
    }
}

pub(crate) async fn connect(
    source_id: SourceId,
    kind: &str,
    name: String,
    settings: FileSourceSettings,
    credentials: FileCredentials,
) -> SourceResult<crate::ConnectedSource> {
    let configuration = settings.configuration(source_id, kind, name)?;
    let credential = Some(serde_json::to_string(&credentials)?);
    let source = RemoteSource::open(&configuration, credential.clone())?;
    let input = source.input().await?;
    if source.stat(&input, "").await?.kind != library::LocalFileKind::Directory {
        return Err(SourceError::InvalidConfig(
            "File source root is not a directory".into(),
        ));
    }
    Ok(crate::ConnectedSource::files(
        configuration,
        source,
        credential,
    ))
}

pub(crate) async fn edit(
    current: SourceConfiguration,
    current_credential: Option<String>,
    name: String,
    settings: FileSourceSettings,
    credentials: FileCredentialsEdit,
) -> SourceResult<crate::SourceEditResult> {
    let mut saved: FileCredentials = current_credential
        .as_deref()
        .map(serde_json::from_str)
        .transpose()?
        .unwrap_or_default();
    if let Some(secret) = credentials.secret {
        saved.secret = secret;
    }
    if let Some(headers) = credentials.headers {
        saved.headers = headers;
    }
    let credentials = saved;
    let mut next =
        settings.configuration(current.source_id.clone(), &current.kind, name.clone())?;
    let current_payload: Payload = crate::config::decode_provider_payload(&current)?;
    let mut payload: Payload = crate::config::decode_provider_payload(&next)?;
    payload.namespace_url = current_payload.namespace_url;
    next.provider_payload = serde_json::to_string(&payload)?;
    let encoded = Some(serde_json::to_string(&credentials)?);
    if current.provider_payload == next.provider_payload && current_credential == encoded {
        return Ok(if current.name == next.name {
            crate::SourceEditResult::Unchanged
        } else {
            crate::SourceEditResult::ConfigurationOnly(next)
        });
    }
    let source = RemoteSource::open(&next, encoded.clone())?;
    source.input().await?;
    Ok(crate::SourceEditResult::Connected(Box::new(
        crate::ConnectedSource::files(next, source, encoded),
    )))
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FileSourceSettings {
    pub url: String,
    pub alternate_urls: Vec<String>,
    #[serde(default)]
    pub folders: Vec<String>,
    pub username: String,
    pub domain: String,
    pub authentication: FileAuthentication,
    pub trust_invalid_certificate: bool,
    pub certificate_pem: Option<String>,
    pub require_smb_encryption: bool,
}

#[derive(Deserialize, Serialize)]
struct Payload {
    version: u32,
    namespace_url: String,
    #[serde(flatten)]
    settings: FileSourceSettings,
}

pub(crate) struct RemoteSource {
    source_id: SourceId,
    kind: String,
    name: String,
    namespace_url: String,
    settings: FileSourceSettings,
    credentials: FileCredentials,
    input: Mutex<Option<Arc<FileInputServer>>>,
}

impl FileSourceSettings {
    pub(crate) fn from_configuration(configuration: &SourceConfiguration) -> SourceResult<Self> {
        let payload: Payload = crate::config::decode_provider_payload(configuration)?;
        crate::config::require_payload_version(payload.version, 1)?;
        payload.settings.validate(&configuration.kind)?;
        Ok(payload.settings)
    }

    pub(crate) fn validate(&self, kind: &str) -> SourceResult<()> {
        for address in std::iter::once(&self.url).chain(&self.alternate_urls) {
            let url = collection_url(address)?;
            let valid = match kind {
                "smb" => {
                    url.scheme() == "smb"
                        && url.path_segments().is_some_and(|mut parts| {
                            parts.next().is_some_and(|share| !share.is_empty())
                        })
                }
                "webdav" => matches!(url.scheme(), "http" | "https"),
                _ => false,
            };
            if !valid {
                return Err(SourceError::InvalidConfig(
                    "Invalid file server collection URL".into(),
                ));
            }
        }
        for folder in &self.folders {
            if folder.starts_with('/')
                || folder.split('/').any(|part| {
                    part.is_empty() || matches!(part, "." | "..") || part.contains(['\\', '\0'])
                })
            {
                return Err(SourceError::InvalidConfig(
                    "Selected folders must be relative paths inside the collection".into(),
                ));
            }
        }
        if kind == "smb" && self.authentication == FileAuthentication::Bearer {
            return Err(SourceError::InvalidConfig(
                "SMB uses a username/password or guest access".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn configuration(
        &self,
        source_id: SourceId,
        kind: &str,
        name: String,
    ) -> SourceResult<SourceConfiguration> {
        self.validate(kind)?;
        Ok(crate::config::encode_provider_payload(
            source_id,
            kind,
            name,
            serde_json::to_value(Payload {
                version: 1,
                namespace_url: self.url.clone(),
                settings: self.clone(),
            })?,
        ))
    }
}

impl RemoteSource {
    pub(crate) fn open(
        configuration: &SourceConfiguration,
        credential: Option<String>,
    ) -> SourceResult<Self> {
        let settings = FileSourceSettings::from_configuration(configuration)?;
        let payload: Payload = crate::config::decode_provider_payload(configuration)?;
        let credentials = credential
            .map(|text| serde_json::from_str(&text))
            .transpose()?
            .unwrap_or_default();
        Ok(Self {
            source_id: configuration.source_id.clone(),
            kind: configuration.kind.clone(),
            name: configuration.name.clone(),
            namespace_url: payload.namespace_url,
            settings,
            credentials,
            input: Mutex::new(None),
        })
    }

    pub(crate) async fn freshness(&self) -> SourceResult<Option<library::Freshness>> {
        if self.kind != "webdav" {
            return Ok(None);
        }
        let input = self.input().await?;
        let FileInput::WebDav(client) = input.input() else {
            return Ok(None);
        };
        if !client.recursive_etags() {
            return Ok(None);
        }
        let mut hash = blake3::Hasher::new();
        hash.update(&scan::PARSER_VERSION.to_le_bytes());
        for folder in if self.settings.folders.is_empty() {
            vec![String::new()]
        } else {
            self.settings.folders.clone()
        } {
            let url = Url::parse(&self.input_path(input.input(), &folder)?)
                .map_err(|_| SourceError::NotFound)?;
            let entry = client.stat(&url).await?;
            let Some(etag) = entry.etag else {
                return Ok(None);
            };
            hash.update(&(folder.len() as u64).to_le_bytes());
            hash.update(folder.as_bytes());
            hash.update(&(etag.len() as u64).to_le_bytes());
            hash.update(etag.as_bytes());
        }
        Ok(Some(library::Freshness::new(
            hash.finalize().as_bytes().to_vec(),
        )?))
    }

    pub(crate) async fn input(&self) -> SourceResult<Arc<FileInputServer>> {
        let mut current = self.input.lock().await;
        if let Some(input) = &*current
            && !match input.input() {
                FileInput::Smb(client) => client.is_disconnected(),
                FileInput::WebDav(client) => client.is_disconnected(),
            }
        {
            return Ok(Arc::clone(input));
        }
        *current = None;
        let mut failure = SourceError::NotFound;
        for address in std::iter::once(&self.settings.url).chain(&self.settings.alternate_urls) {
            match self.connect_input(address).await {
                Ok(input) => {
                    let input = FileInputServer::start(input).await?;
                    *current = Some(Arc::clone(&input));
                    return Ok(input);
                }
                Err(error @ SourceError::Network(_)) => failure = error,
                Err(
                    error @ SourceError::Server {
                        status: 502 | 503 | 504,
                        ..
                    },
                ) => failure = error,
                Err(error) => return Err(error),
            }
        }
        Err(failure)
    }

    async fn connect_input(&self, address: &str) -> SourceResult<FileInput> {
        let url = collection_url(address)?;
        if self.kind == "smb" {
            let parts = decoded_parts(&url)?;
            let guest = self.settings.authentication == FileAuthentication::Anonymous;
            let username = if guest {
                String::new()
            } else if self.settings.domain.is_empty() {
                self.settings.username.clone()
            } else {
                format!("{}\\{}", self.settings.domain, self.settings.username)
            };
            let client = crate::file::remote::smb::SmbClient::connect(
                url.host_str().ok_or(SourceError::NotFound)?,
                url.port().unwrap_or(445),
                &parts[0],
                &username,
                if guest {
                    String::new()
                } else {
                    self.credentials.secret.clone()
                },
                guest,
                self.settings.require_smb_encryption,
            )
            .await?
            .with_root(parts[1..].join("/"))
            .await?;
            Ok(FileInput::Smb(Arc::new(client)))
        } else {
            let authentication = match self.settings.authentication {
                FileAuthentication::Anonymous => Authentication::Anonymous,
                FileAuthentication::Password => Authentication::Password {
                    username: self.settings.username.clone(),
                    password: self.credentials.secret.clone(),
                },
                FileAuthentication::Bearer => {
                    Authentication::Bearer(self.credentials.secret.clone())
                }
            };
            let headers = webdav::client::custom_headers(&self.credentials.headers)?;
            let client = WebDavClient::new(
                url,
                authentication,
                headers,
                self.settings.trust_invalid_certificate,
                self.settings.certificate_pem.as_deref().map(str::as_bytes),
            )?;
            if !client.stat(client.root()).await?.directory {
                return Err(SourceError::InvalidConfig(
                    "The WebDAV URL must identify a collection".into(),
                ));
            }
            Ok(FileInput::WebDav(Arc::new(client)))
        }
    }

    pub(crate) fn includes(&self, relative: &str) -> bool {
        self.settings.folders.is_empty()
            || self.settings.folders.iter().any(|folder| {
                relative == folder
                    || relative
                        .strip_prefix(folder)
                        .is_some_and(|rest| rest.starts_with('/'))
            })
    }

    fn location(&self, relative: &str) -> SourceResult<String> {
        let mut url = collection_url(&self.namespace_url)?;
        {
            let mut parts = url.path_segments_mut().map_err(|_| SourceError::NotFound)?;
            parts.pop_if_empty();
            for part in relative.split('/').filter(|part| !part.is_empty()) {
                if matches!(part, "." | "..") || part.contains(['\\', '\0']) {
                    return Err(SourceError::InvalidRequest(
                        "File path is outside the configured collection",
                    ));
                }
                parts.push(part);
            }
            if relative.is_empty() {
                parts.push("");
            }
        }
        Ok(url.into())
    }

    fn relative(&self, location: &str) -> SourceResult<String> {
        let base = collection_url(&self.namespace_url)?;
        let url = Url::parse(location).map_err(|_| SourceError::NotFound)?;
        // SMB URLs have opaque web origins; compare their actual server authority.
        if base.scheme() != url.scheme() || base.host() != url.host() || base.port() != url.port() {
            return Err(SourceError::NotFound);
        }
        let relative = url
            .path()
            .strip_prefix(base.path())
            .ok_or(SourceError::NotFound)?;
        let relative = percent_encoding::percent_decode_str(relative)
            .decode_utf8()
            .map_err(|_| SourceError::NotFound)?
            .into_owned();
        self.location(&relative)?;
        Ok(relative.trim_end_matches('/').into())
    }

    fn input_path(&self, input: &FileInput, relative: &str) -> SourceResult<String> {
        match input {
            FileInput::Smb(_) => Ok(relative.into()),
            FileInput::WebDav(client) => {
                let mut url = client.root().clone();
                {
                    let mut parts = url.path_segments_mut().map_err(|_| SourceError::NotFound)?;
                    parts.pop_if_empty();
                    for part in relative.split('/') {
                        parts.push(part);
                    }
                }
                Ok(url.into())
            }
        }
    }

    pub(crate) async fn stream(
        &self,
        database: &library::Database,
        media_uri: &str,
    ) -> SourceResult<playback::ResolvedStream> {
        let (source, kind, _) =
            library::source_entity_parts(media_uri).ok_or(SourceError::NotFound)?;
        if source != self.source_id || kind != "track" {
            return Err(SourceError::NotFound);
        }
        let file = database
            .observed_media_file(media_uri)
            .await?
            .ok_or(SourceError::NotFound)?;
        let relative = self.relative(&file.path)?;
        let input = self.input().await?;
        let mut stream = input
            .playback_stream(
                &self.input_path(input.input(), &relative)?,
                file.revision.as_deref().unwrap_or_default(),
                media_uri,
            )
            .await?;
        if let Some((start, end)) = file.cue_start_millis.zip(file.cue_end_millis) {
            stream = stream.with_window(start.max(0) as u64, end.max(0) as u64);
        }
        Ok(stream)
    }

    pub(crate) async fn small_file(
        &self,
        input: &Arc<FileInputServer>,
        relative: &str,
        limit: usize,
    ) -> SourceResult<Vec<u8>> {
        let path = self.input_path(input.input(), relative)?;
        let stream = input.stream(&path, &self.location(relative)?);
        let client = reqwest::Client::builder()
            .no_proxy()
            .read_timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| SourceError::Network(e.without_url().to_string()))?;
        let mut response = client
            .get(stream.uri())
            .send()
            .await
            .map_err(|e| SourceError::Network(e.without_url().to_string()))?;
        if !response.status().is_success() {
            return Err(match response.status().as_u16() {
                404 => SourceError::NotFound,
                401 => SourceError::Auth("File server credentials were rejected".into()),
                status => SourceError::Server {
                    status,
                    message: "Could not read remote file".into(),
                },
            });
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| SourceError::Network(e.without_url().to_string()))?
        {
            if bytes.len().saturating_add(chunk.len()) > limit {
                return Err(SourceError::Other(
                    "Remote sidecar exceeds size limit".into(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    pub(crate) async fn image(
        &self,
        request: crate::SourceImageRequest,
    ) -> SourceResult<crate::ImageBytes> {
        let crate::SourceImageRequest::Local(reference) = request else {
            return Err(SourceError::NotFound);
        };
        let (source_id, path, revision) = match &reference {
            crate::LocalImageRef::File {
                source_id,
                path,
                revision,
            }
            | crate::LocalImageRef::Embedded {
                source_id,
                path,
                revision,
                ..
            } => (source_id, path, revision),
        };
        if source_id != &self.source_id {
            return Err(SourceError::NotFound);
        }
        let relative = self.relative(path)?;
        let input = self.input().await?;
        match reference {
            crate::LocalImageRef::File { .. } => Ok(crate::ImageBytes {
                bytes: self.small_file(&input, &relative, 32 * 1024 * 1024).await?,
                content_type: crate::file::artwork::content_type(std::path::Path::new(&relative)),
            }),
            crate::LocalImageRef::Embedded { picture_index, .. } => {
                let stream = input
                    .playback_stream(&self.input_path(input.input(), &relative)?, revision, path)
                    .await?;
                let uri = stream.uri().to_string();
                let mut reader = crate::file::remote::reader::FileReader::open(stream).await?;
                tokio::task::spawn_blocking(move || {
                    crate::file::artwork::read_image_input(
                        &mut crate::file::discovery::Reader::network(),
                        &mut reader,
                        &uri,
                        picture_index,
                    )
                })
                .await
                .map_err(|e| SourceError::Other(e.to_string()))?
            }
        }
    }
}

fn collection_url(address: &str) -> SourceResult<Url> {
    let mut url = Url::parse(address).map_err(|e| SourceError::InvalidConfig(e.to_string()))?;
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.query().is_some()
    {
        return Err(SourceError::InvalidConfig(
            "Enter a collection URL with credentials in the separate fields".into(),
        ));
    }
    if !url.path().ends_with('/') {
        url.path_segments_mut()
            .map_err(|_| SourceError::NotFound)?
            .push("");
    }
    Ok(url)
}

fn decoded_parts(url: &Url) -> SourceResult<Vec<String>> {
    url.path_segments()
        .ok_or(SourceError::NotFound)?
        .filter(|p| !p.is_empty())
        .map(|part| {
            percent_encoding::percent_decode_str(part)
                .decode_utf8()
                .map(|s| s.into_owned())
                .map_err(|_| SourceError::InvalidConfig("Invalid UTF-8 in collection URL".into()))
        })
        .collect()
}

fn referenced_path(cue: &str, value: &str) -> SourceResult<String> {
    let value = value.replace('\\', "/");
    if value.starts_with('/') || value.contains([':', '\0']) {
        return Err(SourceError::InvalidRequest(
            "File is outside the configured collection",
        ));
    }
    let mut parts: Vec<_> = cue
        .rsplit_once('/')
        .map(|(parent, _)| parent.split('/').collect())
        .unwrap_or_default();
    for part in value.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(SourceError::InvalidRequest(
                        "File is outside the configured collection",
                    ));
                }
            }
            _ => parts.push(part),
        }
    }
    Ok(parts.join("/"))
}
