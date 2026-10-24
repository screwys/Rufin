//! Shared track identity with device-owned files and encoding choices.
use super::cache::CacheRequest;
use super::*;
use library::LocalMediaLocation;
use std::{io::Read, path::Path};

#[derive(Serialize, Deserialize)]
#[serde(tag = "stream", rename_all = "snake_case")]
pub(super) enum ByteRequest {
    Audio {
        media: rufin_connect::media::PeerMediaRequest,
    },
    ProfileSnapshot {
        format: u32,
    },
    Artwork {
        family: String,
        size: Option<u32>,
        offset: u64,
        validator: Option<String>,
    },
}

struct ProfileReader {
    reader: tokio::fs::File,
    _file: tempfile::TempPath,
}

impl tokio::io::AsyncRead for ProfileReader {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.reader).poll_read(context, buffer)
    }
}

fn blocking_audio(
    mut reader: impl Read + Send + 'static,
) -> std::pin::Pin<Box<dyn tokio::io::AsyncRead + Send + Unpin>> {
    let (sender, receiver) = tokio::sync::mpsc::channel(2);
    tokio::task::spawn_blocking(move || {
        let mut bytes = [0; 65536];
        loop {
            let chunk = match reader.read(&mut bytes) {
                Ok(0) => break,
                Ok(length) => Ok(std::io::Cursor::new(bytes[..length].to_vec())),
                Err(error) => Err(error),
            };
            let failed = chunk.is_err();
            if sender.blocking_send(chunk).is_err() || failed {
                break;
            }
        }
    });
    let stream = futures_util::stream::unfold(receiver, |mut receiver| async {
        receiver.recv().await.map(|chunk| (chunk, receiver))
    });
    Box::pin(tokio_util::io::StreamReader::new(Box::pin(stream)))
}

struct PeerConnection(Arc<ConnectNetwork>, tokio::sync::watch::Receiver<Status>);

pub(super) fn peer_connection(
    network: Arc<ConnectNetwork>,
    status: tokio::sync::watch::Receiver<Status>,
) -> Arc<dyn sources::PeerMedia> {
    Arc::new(PeerConnection(network, status))
}

impl PeerConnection {
    async fn members(&self) -> sources::SourceResult<Vec<String>> {
        let mut peers = self
            .0
            .members()
            .await
            .map_err(|error| sources::SourceError::Network(error.to_string()))?;
        let status = self.1.borrow();
        let identity = self.0.identity();
        peers.retain(|peer| *peer != identity);
        peers.sort_by_key(|peer| {
            !status
                .devices
                .iter()
                .any(|device| device.id == *peer && device.reachable)
        });
        Ok(peers)
    }
}

impl sources::PeerMedia for PeerConnection {
    fn assets<'a>(
        &'a self,
        uri: &'a str,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = sources::SourceResult<Option<library::LyricsCacheRow>>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let request = serde_json::to_vec(&Request::CacheFile {
                asset: CacheRequest::MediaAssets { uri: uri.into() },
            })?;
            for peer in self.members().await? {
                if let Ok(value) = self.0.request(&peer, request.clone()).await {
                    return serde_json::from_slice(&value).map_err(sources::SourceError::from);
                }
            }
            Ok(None)
        })
    }
    fn open<'a>(
        &'a self,
        uri: &'a str,
        encoding: Option<library::AudioEncoding>,
        range: Option<(u64, Option<u64>)>,
        window: Option<(u64, Option<u64>)>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = sources::SourceResult<sources::PeerAudio>> + Send + 'a,
        >,
    > {
        Box::pin(async move {
            let body = serde_json::to_vec(&ByteRequest::Audio {
                media: rufin_connect::media::PeerMediaRequest {
                    uri: uri.into(),
                    encoding,
                    range,
                    window,
                },
            })?;
            let mut last = sources::SourceError::NotFound;
            for peer in self.members().await? {
                match self.0.stream_request(&peer, body.clone()).await {
                    Ok(response) => {
                        let metadata: rufin_connect::media::PeerByteMetadata =
                            serde_json::from_value(response.metadata)?;
                        return Ok(sources::PeerAudio {
                            reader: response.reader,
                            length: metadata.length,
                            offset: metadata.offset,
                            content_type: metadata.content_type,
                            revision: metadata.revision,
                            validator: metadata
                                .validator
                                .map(|validator| format!("{peer}:{validator}")),
                        });
                    }
                    Err(error) => last = sources::SourceError::Network(error.to_string()),
                }
            }
            Err(last)
        })
    }
}

impl ConnectOwner {
    pub(super) async fn answer_byte_stream(
        &self,
        session: &Arc<Session>,
        peer: &str,
        body: &[u8],
    ) -> Result<rufin_connect::media::PeerByteResponse, String> {
        use rufin_connect::media::{FileMetadata, PeerByteResponse};
        match serde_json::from_slice(body).map_err(error)? {
            ByteRequest::Audio { media } => self.answer_media_stream(peer, media).await,
            ByteRequest::ProfileSnapshot { format } => {
                if format != 4 {
                    return Err(library::profile::INCOMPATIBLE_PROFILE.into());
                }
                let exported = self.export_profile(session).await?;
                let file = exported.reopen().map_err(error)?;
                let length = file.metadata().map_err(error)?.len();
                let mut metadata = serde_json::to_value(FileMetadata {
                    length,
                    offset: 0,
                    validator: None,
                })
                .map_err(error)?;
                metadata["format"] = serde_json::json!(4);
                Ok(PeerByteResponse {
                    metadata,
                    reader: Box::pin(ProfileReader {
                        reader: tokio::fs::File::from_std(file),
                        _file: exported.into_temp_path(),
                    }),
                })
            }
            ByteRequest::Artwork {
                family,
                size,
                offset,
                validator,
            } => {
                self.answer_artwork_stream(
                    artwork::CacheImageRequest {
                        family,
                        size: size
                            .map_or(sources::ImageSize::Original, sources::ImageSize::Thumbnail),
                    },
                    offset,
                    validator.as_deref(),
                )
                .await
            }
        }
    }

    async fn answer_media_stream(
        &self,
        peer: &str,
        request: rufin_connect::media::PeerMediaRequest,
    ) -> Result<rufin_connect::media::PeerByteResponse, String> {
        use rufin_connect::media::{PeerByteMetadata, PeerByteResponse};
        use tokio::io::AsyncReadExt;
        let uri = request.uri.as_str();
        let reference = self
            .database
            .connect_track_reference(uri)
            .await
            .map_err(error)?;
        if reference.is_none() {
            let snapshot = self
                .transfers
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(peer)
                .filter(|(started, _)| started.elapsed() < Duration::from_secs(300))
                .map(|(_, snapshot)| snapshot.queue.clone());
            let snapshot = match snapshot {
                Some(snapshot) => snapshot,
                None => {
                    let playback = Arc::clone(&self.playback);
                    tokio::task::spawn_blocking(move || playback.snapshot())
                        .await
                        .map_err(error)??
                }
            };
            let identity = self
                .database
                .catalog_identity(
                    library::CatalogKind::Track,
                    uri,
                    &library::ReadCancellation::new(),
                )
                .await
                .map_err(error)?
                .map(|identity| library::TrackKey::from_raw(identity.key));
            if !snapshot.entries.iter().any(|entry| {
                Some(entry.track_key) == identity
                    && !(entry.received_from_connect && raw_document_reference(uri))
            }) {
                return Err("The requested track is not the captured playback".into());
            }
        }
        let encoding_key = library::media_encoding_key(request.encoding);
        let mut receipt = self
            .database
            .connect_media_file(uri, &encoding_key)
            .await
            .map_err(error)?
            .filter(|file| {
                (file.source_id.is_some() && file.revision.is_empty())
                    || reference
                        .as_ref()
                        .is_none_or(|reference| file.revision == revision(reference))
            });
        if receipt.is_none()
            && let Some(reference) = reference.as_ref()
        {
            receipt = self
                .database
                .connect_backing_media_file(uri, &encoding_key, &revision(reference))
                .await
                .map_err(error)?;
        }
        let cached = receipt
            .as_ref()
            .filter(|_| request.window.is_none())
            .and_then(|file| location(&file.path));
        let cached = match cached {
            Some(location) => available_location(location).await?,
            None => None,
        };
        let ready_encoding = cached.is_some();
        let original = self.original_media_file(uri).await?;
        let actual_original = original.is_some();
        let access = match cached {
            Some(access) => access,
            None => original
                .or(self.database.connect_local_file(uri).await.map_err(error)?)
                .or_else(|| {
                    library::cue_media_parts(uri)
                        .map_or_else(|| location(uri), |(_, backing, _, _)| location(&backing))
                })
                .ok_or("The requested file is unavailable on this device")?,
        };
        if !ready_encoding && !actual_original {
            let access_uri = match &access {
                LocalMediaLocation::File(path) => file_uri(path)?,
                LocalMediaLocation::Document(uri) => uri.clone(),
            };
            if self
                .database
                .connect_media_files(uri)
                .await
                .map_err(error)?
                .iter()
                .any(|file| file.path == access_uri)
            {
                return Err("This device does not have the requested original audio".into());
            }
        }
        let revision = receipt
            .as_ref()
            .filter(|_| ready_encoding)
            .map(|receipt| receipt.revision.clone())
            .or_else(|| reference.as_ref().map(revision))
            .or_else(|| receipt.as_ref().map(|receipt| receipt.revision.clone()))
            .unwrap_or_default();
        let mut metadata = PeerByteMetadata {
            length: None,
            offset: 0,
            content_type: request
                .encoding
                .map(|encoding| encoding.codec.content_type().into()),
            revision,
            validator: None,
        };
        if let Some(encoding) = request.encoding.filter(|_| !ready_encoding) {
            let mut stream = match access {
                LocalMediaLocation::File(path) => {
                    audio_processing::ResolvedStream::new(file_uri(&path)?)
                }
                LocalMediaLocation::Document(uri) => sources::resolve_document_stream(&uri)
                    .await
                    .map_err(error)?,
            };
            if let Some((start, requested_end)) = request.window {
                // An empty interval must never widen into the whole recording.
                if requested_end.is_some_and(|end| end <= start) {
                    return Err(format!(
                        "requested audio interval {start}..{end:?} is empty",
                        end = requested_end
                    ));
                }
                let end = requested_end
                    .or(self
                        .database
                        .download_metadata(uri)
                        .await
                        .map_err(error)?
                        .map(|metadata| metadata.duration_millis.max(0) as u64)
                        .filter(|end| *end > start))
                    .unwrap_or(u64::MAX / 1_000_000);
                stream = stream.with_window(start, end);
            }
            let reader = tokio::task::spawn_blocking(move || {
                audio_processing::TranscodedAudioReader::new(&stream, encoding.into(), || false)
                    .map_err(error)
            })
            .await
            .map_err(error)??;
            return Ok(PeerByteResponse {
                metadata: serde_json::to_value(metadata).map_err(error)?,
                reader: blocking_audio(reader),
            });
        }
        let start = request.range.map_or(0, |(start, _)| start);
        let reader: std::pin::Pin<Box<dyn tokio::io::AsyncRead + Send + Unpin>> = match access {
            LocalMediaLocation::File(path) => {
                let (file_metadata, file) = rufin_connect::media::open_file(&path, start, None)
                    .await
                    .map_err(error)?;
                let length = file_metadata.length;
                metadata.length = Some(length);
                metadata.validator = file_metadata
                    .validator
                    .map(|validator| format!("{encoding_key}:{validator}"));
                let end = request
                    .range
                    .and_then(|(_, end)| end)
                    .unwrap_or(length)
                    .min(length);
                Box::pin(file.take(end.saturating_sub(start)))
            }
            LocalMediaLocation::Document(uri) => {
                let (reader, length, validator) = tokio::task::spawn_blocking(move || {
                    use std::io::Seek;
                    let mut reader = sources::open_document_input(&uri).map_err(error)?;
                    let length = reader.seek(std::io::SeekFrom::End(0)).map_err(error)?;
                    let validator = sources::stat_document(&uri).ok().and_then(|entry| {
                        entry.revision.map(|revision| {
                            blake3::hash(
                                format!(
                                    "{encoding_key}:{uri}:{}:{length}:{revision}",
                                    entry.native_id
                                )
                                .as_bytes(),
                            )
                            .to_hex()
                            .to_string()
                        })
                    });
                    reader
                        .seek(std::io::SeekFrom::Start(start))
                        .map_err(error)?;
                    let end = request
                        .range
                        .and_then(|(_, end)| end)
                        .unwrap_or(length)
                        .min(length);
                    Ok::<_, String>((reader.take(end.saturating_sub(start)), length, validator))
                })
                .await
                .map_err(error)??;
                metadata.length = Some(length);
                metadata.validator = validator;
                blocking_audio(reader)
            }
        };
        metadata.offset = start;
        Ok(PeerByteResponse {
            metadata: serde_json::to_value(metadata).map_err(error)?,
            reader,
        })
    }

    pub(crate) fn peer_access(&self) -> sources::PeerAccess {
        self.source.peer_access()
    }

    pub(super) async fn media_assets(
        &self,
        uri: &str,
    ) -> Result<Option<library::LyricsCacheRow>, String> {
        let Some(track) = self
            .database
            .track(uri, &library::ReadCancellation::new())
            .await
            .map_err(error)?
        else {
            return Ok(None);
        };
        if let Some(binding) = track.artwork_binding {
            if let Err(error) = self.source.shared.artwork.cache_source(&binding).await {
                tracing::warn!(%error, "Could not prepare transferred artwork");
            }
        }
        let input = library::source_entity_parts(uri).map_or([0; 32], |(source, _, _)| {
            self.source.source_input_digest(&source)
        });
        let cached = self
            .database
            .lyrics_cache_for_role(
                uri,
                "original",
                "",
                "",
                input,
                "source",
                &library::ReadCancellation::new(),
            )
            .await
            .map_err(error)?
            .and_then(|row| lyrics::LyricsBundle::from_cache_payload(&row.lyrics))
            .and_then(lyrics::lyrics_with_displayable_content)
            .filter(|bundle| {
                bundle.origin == lyrics::LyricsOrigin::Native
                    || bundle.origin == lyrics::LyricsOrigin::Local
                        && library::cue_media_parts(uri).is_none()
            });
        let bundle = match cached {
            Some(bundle) => Some(bundle),
            None => match self.source.media_client(uri).await {
                Ok(source) => source
                    .cached_lyrics(&self.database, uri, input)
                    .await
                    .map_err(error)?,
                Err(_) => sources::read_local_lyrics(
                    &self.database,
                    uri,
                    Some(&track.metadata.title),
                    &lyrics::UserPreferences::default(),
                )
                .await
                .map_err(error)?
                .and_then(lyrics::lyrics_with_displayable_content)
                .map(|mut bundle| {
                    bundle.origin = lyrics::LyricsOrigin::Native;
                    bundle
                }),
            },
        };
        let Some(bundle) = bundle else {
            return Ok(None);
        };
        let payload = bundle.cache_payload().map_err(error)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |time| time.as_secs().min(i64::MAX as u64) as i64);
        Ok(Some(library::LyricsCacheRow {
            media_uri: uri.into(),
            authority: "source".into(),
            role: "original".into(),
            language: String::new(),
            script: String::new(),
            cache_input_digest: input,
            lyrics: payload,
            updated_at: now,
        }))
    }
}

fn revision(reference: &serde_json::Value) -> String {
    reference["revision"].to_string()
}

fn file_uri(path: &Path) -> Result<String, String> {
    url::Url::from_file_path(path)
        .map(|url| url.to_string())
        .map_err(|()| "Media path must be absolute".into())
}

pub(super) fn location(uri: &str) -> Option<LocalMediaLocation> {
    if uri.starts_with("content://") {
        Some(LocalMediaLocation::Document(uri.into()))
    } else {
        library::file_media_path(uri).map(LocalMediaLocation::File)
    }
}

pub(super) fn raw_document_reference(uri: &str) -> bool {
    uri.starts_with("content://")
        || library::cue_media_parts(uri)
            .is_some_and(|(_, backing, _, _)| backing.starts_with("content://"))
}

async fn available_location(
    location: LocalMediaLocation,
) -> Result<Option<LocalMediaLocation>, String> {
    match &location {
        LocalMediaLocation::File(path) => Ok(path.is_file().then_some(location)),
        LocalMediaLocation::Document(uri) => {
            let uri = uri.clone();
            let present = tokio::task::spawn_blocking(move || match sources::stat_document(&uri) {
                Ok(entry) => Ok(!entry.directory),
                Err(sources::SourceError::NotFound) => Ok(false),
                Err(error) => Err(error.to_string()),
            })
            .await
            .map_err(error)??;
            Ok(present.then_some(location))
        }
    }
}

impl ConnectOwner {
    fn local_file_path(&self, path: PathBuf) -> PathBuf {
        self.source
            .shared
            .settings
            .local_configuration()
            .and_then(|local| local.local_file_location(&path))
            .map_or(path, |(_, access)| access)
    }

    async fn original_media_file(&self, uri: &str) -> Result<Option<LocalMediaLocation>, String> {
        let location = self
            .database
            .connect_original_file(uri, |path| self.local_file_path(path))
            .await
            .map_err(error)?;
        match location {
            Some(location) => available_location(location).await,
            None => Ok(None),
        }
    }

    pub(crate) async fn is_local_media(&self, uri: &str) -> Result<bool, String> {
        let cue = library::cue_media_parts(uri);
        let backing = cue.as_ref().map_or(uri, |(_, backing, _, _)| backing);
        if location(backing).is_some() || library::document_media_id(backing).is_some() {
            return Ok(true);
        }
        if library::source_entity_parts(uri).is_some_and(|(source, kind, _)| {
            kind == "track"
                && self
                    .source
                    .configuration(&source)
                    .is_some_and(|config| config.is_local())
        }) {
            return Ok(true);
        }
        Ok(self
            .database
            .connect_track_reference(uri)
            .await
            .map_err(error)?
            .is_some_and(|reference| reference["local_media"].as_bool() == Some(true)))
    }
}
