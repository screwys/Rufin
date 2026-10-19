//! Peer bytes are another access route to an existing file identity.
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use audio_processing::ResolvedStream;
use library::{AudioChannels, AudioCodec, AudioEncoding, Database, LocalMediaLocation};

use crate::file::remote::input::{FileInput, FileInputServer};
use crate::{DownloadQuality, ResolvedDownload, SourceError, SourceResult};

pub const PEER_REVISION_HEADER: &str = "x-rufin-revision";

pub fn peer_revision(value: &[u8]) -> SourceResult<String> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|error| SourceError::Other(error.to_string()))?;
    String::from_utf8(bytes).map_err(|error| SourceError::Other(error.to_string()))
}

pub(crate) fn encode_peer_revision(value: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value)
}

pub struct PeerAudio {
    pub reader: Pin<Box<dyn tokio::io::AsyncRead + Send + Unpin>>,
    pub length: Option<u64>,
    pub offset: u64,
    pub content_type: Option<String>,
    pub revision: String,
    pub validator: Option<String>,
}

fn same_location(left: &LocalMediaLocation, right: &LocalMediaLocation) -> bool {
    match (left, right) {
        (LocalMediaLocation::File(left), LocalMediaLocation::File(right)) => left == right,
        (LocalMediaLocation::Document(left), LocalMediaLocation::Document(right)) => left == right,
        _ => false,
    }
}

pub trait PeerMedia: Send + Sync {
    fn assets<'a>(
        &'a self,
        _uri: &'a str,
    ) -> Pin<Box<dyn Future<Output = SourceResult<Option<library::LyricsCacheRow>>> + Send + 'a>>
    {
        Box::pin(async { Ok(None) })
    }
    fn open<'a>(
        &'a self,
        uri: &'a str,
        encoding: Option<AudioEncoding>,
        range: Option<(u64, Option<u64>)>,
        window: Option<(u64, Option<u64>)>,
    ) -> Pin<Box<dyn Future<Output = SourceResult<PeerAudio>> + Send + 'a>>;
}

#[derive(Clone, Default)]
pub struct PeerAccess {
    peer: Arc<Mutex<Option<Arc<dyn PeerMedia>>>>,
    folders: Arc<Mutex<BTreeMap<String, LocalMediaLocation>>>,
}

impl PeerAccess {
    pub async fn cache_assets(
        &self,
        database: &Database,
        uri: &str,
        input: [u8; 32],
    ) -> SourceResult<()> {
        let Some(row) = self.peer()?.assets(uri).await? else {
            return Ok(());
        };
        if row.media_uri != uri || row.authority != "source" {
            return Err(SourceError::InvalidRequest(
                "Transferred lyrics do not match this track",
            ));
        }
        let bundle = lyrics::LyricsBundle::from_cache_payload(&row.lyrics)
            .ok_or(SourceError::InvalidRequest("Invalid transferred lyrics"))?;
        if !matches!(
            bundle.origin,
            lyrics::LyricsOrigin::Native | lyrics::LyricsOrigin::Local
        ) {
            return Err(SourceError::InvalidRequest(
                "Transferred lyrics are not from the source",
            ));
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |time| time.as_secs().min(i64::MAX as u64) as i64);
        database
            .write_lyrics_cache(uri, "source", "original", "", "", input, &row.lyrics, now)
            .await?;
        Ok(())
    }
    pub fn install(&self, peer: Option<Arc<dyn PeerMedia>>) {
        *self.peer.lock().unwrap_or_else(|p| p.into_inner()) = peer;
    }

    pub fn set_folders(&self, folders: BTreeMap<String, LocalMediaLocation>) {
        *self.folders.lock().unwrap_or_else(|p| p.into_inner()) = folders;
    }

    pub(crate) fn peer(&self) -> SourceResult<Arc<dyn PeerMedia>> {
        self.peer
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .ok_or_else(|| SourceError::Network("Waiting for a device with this media".into()))
    }

    pub async fn stream(&self, request: playback::StreamRequest) -> SourceResult<ResolvedStream> {
        let encoding = request
            .quality
            .max_bitrate_kbps()
            .map(|bitrate| AudioEncoding {
                codec: AudioCodec::Opus,
                bitrate_kbps: Some(bitrate),
                channels: AudioChannels::StereoLossy,
            });
        self.encoded_stream(&request.media_uri, encoding).await
    }

    async fn encoded_stream(
        &self,
        uri: &str,
        encoding: Option<AudioEncoding>,
    ) -> SourceResult<ResolvedStream> {
        let input = FileInputServer::start(FileInput::Peer {
            peer: self.peer()?,
            encoding,
        })
        .await?;
        let stream = input.stream(uri, uri);
        let stream = if encoding.is_some() {
            stream.with_time_seek()
        } else {
            stream
        };
        Ok(stream
            .with_transcoding(encoding.is_some())
            .with_content_type(encoding.map(|value| value.codec.content_type().into())))
    }

    pub async fn original_file(
        &self,
        database: &Database,
        uri: &str,
    ) -> SourceResult<Option<LocalMediaLocation>> {
        if let Some(location) = database.connect_original_file(uri, |path| path).await?
            && crate::local_files_available(vec![vec![location.clone()]]).await?[0]
        {
            return Ok(Some(location));
        }
        let Some(location) = self.mapped_file(database, uri).await? else {
            return Ok(None);
        };
        if let Some(previous) = database.connect_media_file(uri, "original").await? {
            let previous_location = if previous.path.starts_with("content://") {
                Some(LocalMediaLocation::Document(previous.path.clone()))
            } else {
                library::file_media_path(&previous.path).map(LocalMediaLocation::File)
            };
            if previous_location
                .as_ref()
                .is_some_and(|previous| same_location(previous, &location))
                && database
                    .connect_track_reference(uri)
                    .await?
                    .is_some_and(|reference| {
                        let revision = reference["revision"].to_string();
                        previous.revision != revision
                    })
            {
                return Ok(None);
            }
        }
        let available = crate::local_files_available(vec![vec![location.clone()]]).await?[0];
        Ok(available.then_some(location))
    }

    pub async fn download(
        &self,
        database: &Database,
        uri: &str,
        quality: DownloadQuality,
    ) -> SourceResult<ResolvedDownload> {
        if let Some(location) = self.original_file(database, uri).await? {
            let stream = match location {
                LocalMediaLocation::File(path) => ResolvedStream::new(
                    url::Url::from_file_path(path)
                        .map_err(|_| SourceError::InvalidRequest("Media path must be absolute"))?
                        .to_string(),
                ),
                LocalMediaLocation::Document(uri) => crate::resolve_document_stream(&uri).await?,
            };
            return Ok(ResolvedDownload::encode(stream, quality));
        }
        Ok(ResolvedDownload::Ready {
            stream: self.encoded_stream(uri, quality.encoding()).await?,
            codec: quality.encoding().map(|value| value.codec),
        })
    }

    pub async fn mapped_file(
        &self,
        database: &Database,
        uri: &str,
    ) -> SourceResult<Option<LocalMediaLocation>> {
        let Some((root, parts)) = self.destination(database, uri).await? else {
            return Ok(None);
        };
        mapped_location(root, &parts).await
    }

    pub async fn destination(
        &self,
        database: &Database,
        uri: &str,
    ) -> SourceResult<Option<(LocalMediaLocation, Vec<String>)>> {
        let Some(reference) = database.connect_track_reference(uri).await? else {
            return Ok(None);
        };
        let Some(relative) = reference["relative_path"].as_str() else {
            return Ok(None);
        };
        self.folder_destination(&reference, reference["root_id"].as_str(), relative)
    }

    fn folder_destination(
        &self,
        reference: &serde_json::Value,
        root_id: Option<&str>,
        relative: &str,
    ) -> SourceResult<Option<(LocalMediaLocation, Vec<String>)>> {
        let source = reference["source_id"].as_str().unwrap_or_default();
        let root = {
            let folders = self.folders.lock().unwrap_or_else(|p| p.into_inner());
            root_id
                .and_then(|id| folders.get(&format!("{source}/{id}")))
                .or_else(|| {
                    (reference["root_count"].as_u64().unwrap_or(1) <= 1)
                        .then(|| folders.get(source))
                        .flatten()
                })
                .cloned()
        };
        let Some(root) = root else { return Ok(None) };
        let parts = relative_parts(relative)?;
        Ok(Some((root, parts)))
    }
}

async fn mapped_location(
    root: LocalMediaLocation,
    parts: &[String],
) -> SourceResult<Option<LocalMediaLocation>> {
    match root {
        LocalMediaLocation::File(root) => {
            let path = parts.iter().fold(root, |path, part| path.join(part));
            Ok(path.is_file().then_some(LocalMediaLocation::File(path)))
        }
        LocalMediaLocation::Document(root) => {
            let parts = parts.to_vec();
            let entry = tokio::task::spawn_blocking(move || crate::find_document(&root, &parts))
                .await
                .map_err(|error| SourceError::Other(error.to_string()))??;
            Ok(entry
                .filter(|entry| !entry.directory)
                .map(|entry| LocalMediaLocation::Document(entry.uri)))
        }
    }
}

fn relative_parts(relative: &str) -> SourceResult<Vec<String>> {
    let parts = relative
        .split('/')
        .filter(|part| *part != ".")
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if parts.is_empty()
        || parts
            .iter()
            .any(|part| part.is_empty() || part == ".." || part.contains('\\'))
    {
        return Err(SourceError::InvalidRequest(
            "The shared track has an invalid source-relative path",
        ));
    }
    Ok(parts)
}
