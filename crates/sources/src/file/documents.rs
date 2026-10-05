//! Document providers keep their own opaque identifiers and file handles.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use serde::{Deserialize, Serialize};

use crate::{SourceError, SourceResult};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocumentRoot {
    pub id: String,
    pub uri: String,
    pub name: String,
}

impl DocumentRoot {
    pub fn new(uri: impl Into<String>, name: impl Into<String>) -> SourceResult<Self> {
        let mut id = [0_u8; 16];
        getrandom::fill(&mut id).map_err(|error| SourceError::Other(error.to_string()))?;
        Ok(Self {
            id: id.iter().map(|byte| format!("{byte:02x}")).collect(),
            uri: uri.into(),
            name: name.into(),
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DocumentEntry {
    pub uri: String,
    pub native_id: String,
    pub name: String,
    pub directory: bool,
    pub mime_type: String,
    pub size: Option<u64>,
    pub revision: Option<String>,
    pub writable: bool,
    pub deletable: bool,
    pub can_create: bool,
}

#[derive(Deserialize, Serialize)]
pub struct DocumentPage {
    pub entries: Vec<DocumentEntry>,
    pub next: Option<u64>,
}

/// The host owns ContentResolver calls and seekable views of asset descriptors.
pub trait DocumentHost: Send + Sync {
    fn request(&self, request: &str) -> SourceResult<String>;
    fn read(&self, handle: u64, offset: u64, length: u32) -> SourceResult<Vec<u8>>;
    fn close(&self, handle: u64);
}

static HOST: OnceLock<Arc<dyn DocumentHost>> = OnceLock::new();

pub fn install_document_host(host: Arc<dyn DocumentHost>) {
    HOST.get_or_init(|| host);
}

fn host() -> SourceResult<&'static Arc<dyn DocumentHost>> {
    HOST.get().ok_or(SourceError::InvalidRequest(
        "Document access is unavailable on this host",
    ))
}

fn request<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> SourceResult<T> {
    let response = host()?.request(&value.to_string())?;
    let value: serde_json::Value = serde_json::from_str(&response)?;
    if let Some(error) = value.get("error").and_then(serde_json::Value::as_str) {
        return Err(
            match value.get("status").and_then(serde_json::Value::as_u64) {
                Some(404) => SourceError::NotFound,
                Some(401 | 403) => SourceError::Auth(error.into()),
                Some(status) => SourceError::Server {
                    status: status as u16,
                    message: error.into(),
                },
                None => SourceError::Other(error.into()),
            },
        );
    }
    Ok(serde_json::from_value(value)?)
}

pub fn stat_document(uri: &str) -> SourceResult<DocumentEntry> {
    request(serde_json::json!({"op":"stat", "uri":uri}))
}

pub fn document_identity(uri: &str) -> SourceResult<String> {
    #[derive(Deserialize)]
    struct Identity {
        native_id: String,
    }
    request::<Identity>(serde_json::json!({"op":"identity", "uri":uri}))
        .map(|identity| identity.native_id)
}

pub fn read_document_prefix(uri: &str, length: u32) -> SourceResult<Vec<u8>> {
    use base64::Engine;
    #[derive(Deserialize)]
    struct Prefix {
        bytes: String,
    }
    let prefix: Prefix = request(serde_json::json!({"op":"prefix", "uri":uri, "length":length}))?;
    base64::engine::general_purpose::STANDARD
        .decode(prefix.bytes)
        .map_err(|error| SourceError::Other(error.to_string()))
}

pub fn document_root(uri: &str) -> SourceResult<DocumentRoot> {
    request(serde_json::json!({"op":"root", "uri":uri}))
}

pub fn list_documents(uri: &str, offset: u64) -> SourceResult<DocumentPage> {
    request(serde_json::json!({"op":"list", "uri":uri, "offset":offset}))
}

pub fn find_document_child(parent: &str, name: &str) -> SourceResult<Option<DocumentEntry>> {
    let mut offset = 0;
    loop {
        let page = list_documents(parent, offset)?;
        if let Some(entry) = page.entries.into_iter().find(|entry| entry.name == name) {
            return Ok(Some(entry));
        }
        match page.next {
            Some(next) => offset = next,
            None => return Ok(None),
        }
    }
}

pub fn find_document(root: &str, components: &[String]) -> SourceResult<Option<DocumentEntry>> {
    let mut entry = match stat_document(root) {
        Ok(entry) => entry,
        Err(SourceError::NotFound) => return Ok(None),
        Err(error) => return Err(error),
    };
    for name in components {
        let Some(child) = find_document_child(&entry.uri, name)? else {
            return Ok(None);
        };
        entry = child;
    }
    Ok(Some(entry))
}

pub fn create_document_directories(
    root: &str,
    components: &[String],
) -> SourceResult<DocumentEntry> {
    let mut entry = stat_document(root)?;
    for name in components {
        entry = match find_document_child(&entry.uri, name)? {
            Some(child) => child,
            None => create_document_directory(&entry.uri, name)?,
        };
        if !entry.directory {
            return Err(SourceError::InvalidRequest(
                "A document occupies the requested folder",
            ));
        }
    }
    Ok(entry)
}

pub fn save_document(
    uri: &str,
    source: &Path,
    expected_revision: Option<&str>,
) -> SourceResult<()> {
    request::<serde_json::Value>(
        serde_json::json!({"op":"save", "uri":uri, "source":source, "revision":expected_revision}),
    )?;
    Ok(())
}

pub fn create_document(
    parent: &str,
    name: &str,
    mime_type: &str,
    source: &Path,
) -> SourceResult<DocumentEntry> {
    request(
        serde_json::json!({"op":"create", "uri":parent, "name":name, "mime_type":mime_type, "source":source}),
    )
}

pub fn create_document_directory(parent: &str, name: &str) -> SourceResult<DocumentEntry> {
    request(serde_json::json!({"op":"mkdir", "uri":parent, "name":name}))
}

pub fn rename_document(uri: &str, name: &str) -> SourceResult<DocumentEntry> {
    request(serde_json::json!({"op":"rename", "uri":uri, "name":name}))
}

pub fn delete_document(uri: &str) -> SourceResult<()> {
    request::<serde_json::Value>(serde_json::json!({"op":"delete", "uri":uri}))?;
    Ok(())
}

pub fn copy_document(uri: &str, destination: &Path) -> SourceResult<()> {
    request::<serde_json::Value>(
        serde_json::json!({"op":"copy", "uri":uri, "destination":destination}),
    )?;
    Ok(())
}

pub fn resolve_document_relative(
    uri: &str,
    relative: &str,
    roots: &[DocumentRoot],
) -> SourceResult<DocumentEntry> {
    request(serde_json::json!({"op":"resolve", "uri":uri, "relative":relative, "roots":roots}))
}

pub async fn read_document_queue_item(uri: &str) -> SourceResult<Option<library::QueueItem>> {
    let stream = resolve_document_stream(uri).await?;
    let playback_uri = stream.uri().to_owned();
    let mut input = crate::file::remote::reader::FileReader::open(stream).await?;
    let uri = uri.to_owned();
    tokio::task::spawn_blocking(move || {
        let entry = stat_document(&uri)?;
        let mut worker = crate::file::media::Worker::network();
        let read = crate::file::media::read_media_input(
            &mut worker,
            entry.name.into(),
            &mut input,
            &playback_uri,
            None,
        );
        Ok(match read {
            crate::file::media::MediaRead::Accepted(track) => {
                Some(crate::file::media::queue_item(*track, uri))
            }
            _ => None,
        })
    })
    .await
    .map_err(|error| SourceError::Other(error.to_string()))?
}

pub async fn resolve_document_stream(uri: &str) -> SourceResult<playback::ResolvedStream> {
    let access = Arc::new(DocumentAccess::new(DocumentRoot::new(uri, "")?));
    let input = crate::file::remote::input::FileInputServer::start(
        crate::file::remote::input::FileInput::Documents(access),
    )
    .await?;
    input.playback_stream("", "", uri).await
}

pub struct DocumentReader {
    lease: DocumentLease,
    position: u64,
}

pub fn open_document_input(uri: &str) -> SourceResult<DocumentReader> {
    Ok(DocumentReader {
        lease: DocumentLease::open(uri)?,
        position: 0,
    })
}

impl Read for DocumentReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let size = (buffer.len() as u64)
            .min(self.lease.length.saturating_sub(self.position))
            .min(65536) as u32;
        if size == 0 {
            return Ok(0);
        }
        let bytes = self
            .lease
            .read(self.position, size)
            .map_err(std::io::Error::other)?;
        let size = bytes.len();
        buffer[..size].copy_from_slice(&bytes);
        self.position += size as u64;
        Ok(size)
    }
}

impl Seek for DocumentReader {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        let position = match position {
            SeekFrom::Start(position) => i128::from(position),
            SeekFrom::End(offset) => i128::from(self.lease.length) + i128::from(offset),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
        };
        self.position = u64::try_from(position).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "Invalid document seek")
        })?;
        Ok(self.position)
    }
}

#[derive(Deserialize)]
struct OpenedDocument {
    handle: u64,
    length: u64,
}

pub(crate) struct DocumentLease {
    host: Arc<dyn DocumentHost>,
    handle: u64,
    pub length: u64,
}

impl DocumentLease {
    fn open(uri: &str) -> SourceResult<Self> {
        let opened: OpenedDocument = request(serde_json::json!({"op":"open", "uri":uri}))?;
        Ok(Self {
            host: Arc::clone(host()?),
            handle: opened.handle,
            length: opened.length,
        })
    }

    pub fn read(&self, offset: u64, length: u32) -> SourceResult<Vec<u8>> {
        self.host.read(self.handle, offset, length)
    }
}

impl Drop for DocumentLease {
    fn drop(&mut self) {
        self.host.close(self.handle);
    }
}

pub(crate) struct DocumentAccess {
    pub root: DocumentRoot,
    opened: Mutex<HashMap<String, Weak<DocumentLease>>>,
}

impl DocumentAccess {
    pub fn new(root: DocumentRoot) -> Self {
        Self {
            root,
            opened: Mutex::new(HashMap::new()),
        }
    }

    /// A fragment is our locator metadata, never part of a provider URI.
    pub fn resolve(&self, relative: &str) -> SourceResult<DocumentEntry> {
        self.resolve_path(relative).map(|(_, entry)| entry)
    }

    pub fn resolve_path(&self, relative: &str) -> SourceResult<(String, DocumentEntry)> {
        if let Some(uri) = relative_parts(relative).1 {
            let (_, uri) = library::document_fragment_parts(uri).ok_or(SourceError::NotFound)?;
            return Ok((relative_parts(relative).0.to_owned(), stat_document(&uri)?));
        }
        let mut entry = stat_document(&self.root.uri)?;
        let mut path = Vec::new();
        for name in relative.split('/').filter(|name| !name.is_empty()) {
            if let Some((_, uri)) = name.rsplit_once('\u{1f}') {
                let uri = percent_encoding::percent_decode_str(uri)
                    .decode_utf8()
                    .map_err(|_| SourceError::NotFound)?;
                entry = stat_document(&uri)?;
            } else {
                entry = find_document_child(&entry.uri, name)?.ok_or(SourceError::NotFound)?;
            }
            path.push(if entry.directory {
                format!(
                    "{}\u{1f}{}",
                    entry.name,
                    percent_encoding::utf8_percent_encode(
                        &entry.uri,
                        percent_encoding::NON_ALPHANUMERIC
                    )
                )
            } else {
                entry.name.clone()
            });
        }
        Ok((path.join("/"), entry))
    }

    pub fn open(&self, relative: &str) -> SourceResult<Arc<DocumentLease>> {
        let uri = if let Some(uri) = relative_parts(relative).1 {
            library::document_fragment_parts(uri)
                .ok_or(SourceError::NotFound)?
                .1
        } else if relative.is_empty() {
            self.root.uri.clone()
        } else {
            self.resolve(relative)?.uri
        };
        {
            let mut opened = self.opened.lock().unwrap_or_else(|p| p.into_inner());
            opened.retain(|_, lease| lease.strong_count() > 0);
            if let Some(lease) = opened.get(&uri).and_then(Weak::upgrade) {
                return Ok(lease);
            }
        }
        let lease = Arc::new(DocumentLease::open(&uri)?);
        let mut opened = self.opened.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(lease) = opened.get(&uri).and_then(Weak::upgrade) {
            return Ok(lease);
        }
        opened.insert(uri, Arc::downgrade(&lease));
        Ok(lease)
    }

    pub fn save(
        &self,
        relative: &str,
        source: &Path,
        revision: Option<&str>,
        existed: bool,
    ) -> SourceResult<()> {
        if existed {
            let entry = self.resolve(relative)?;
            return save_document(&entry.uri, source, revision);
        }
        let (parent, name) = relative.rsplit_once('/').unwrap_or(("", relative));
        let parent = self.resolve(parent)?;
        create_document(&parent.uri, name, "application/octet-stream", source)?;
        Ok(())
    }

    pub fn remove(&self, relative: &str) -> SourceResult<()> {
        delete_document(&self.resolve(relative)?.uri)
    }

    pub fn rename(&self, from: &str, to: &str) -> SourceResult<()> {
        let entry = self.resolve(from)?;
        let (parent, name) = to.rsplit_once('/').unwrap_or(("", to));
        let parent = self.resolve(parent)?;
        request::<serde_json::Value>(
            serde_json::json!({"op":"move", "uri":entry.uri, "parent":parent.uri, "name":name}),
        )?;
        Ok(())
    }

    pub fn create_directory(&self, relative: &str) -> SourceResult<()> {
        let (parent, name) = relative.rsplit_once('/').unwrap_or(("", relative));
        let parent = self.resolve(parent)?;
        request::<serde_json::Value>(
            serde_json::json!({"op":"mkdir", "uri":parent.uri, "name":name}),
        )?;
        Ok(())
    }
}

pub(crate) fn document_relative(path: &str, native_id: &str, uri: &str) -> String {
    format!(
        "{path}#{}",
        library::document_locator_fragment(native_id, uri)
    )
}

pub(crate) fn relative_parts(path: &str) -> (&str, Option<&str>) {
    path.rsplit_once('#')
        .filter(|(_, fragment)| library::document_fragment_parts(fragment).is_some())
        .map_or((path, None), |(path, uri)| (path, Some(uri)))
}

pub(crate) fn display_path(path: &str) -> String {
    relative_parts(path)
        .0
        .split('/')
        .map(|part| part.split('\u{1f}').next().unwrap_or(part))
        .collect::<Vec<_>>()
        .join("/")
}
