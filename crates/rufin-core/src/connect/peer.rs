//! The peer source over the media protocol: this device answers for its own Local sources from
//! its Store, and asks the device that owns a peer source here.
use super::{ConnectOwner, Stream, error};
use futures_util::future::BoxFuture;
use rufin_connect::media::{PeerByteMetadata, PeerByteResponse};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sources::{PeerAudio, SourceError, SourceId, SourceResult};
use std::{
    io::Read as _,
    pin::Pin,
    sync::{Arc, Weak},
};
use tokio::io::{AsyncRead, AsyncReadExt};

/// A read of the catalog of one of the owner's Local sources.
#[derive(Serialize, Deserialize)]
#[serde(tag = "read", rename_all = "snake_case")]
pub(super) enum Read {
    Marker,
    Ids,
    ChangedSince { since: i64 },
    Observations { ids: Vec<String> },
}

/// Asks the device that owns a peer source.
pub(super) struct Link(pub(super) Weak<ConnectOwner>);

impl Link {
    async fn stream(&self, device: &str, request: Stream) -> SourceResult<PeerByteResponse> {
        let owner = self.0.upgrade().ok_or(SourceError::Cancelled)?;
        let network = owner
            .network
            .lock()
            .await
            .clone()
            .ok_or_else(|| SourceError::Network(localization::tr("Connect is not enabled")))?;
        network
            .stream_request(device, serde_json::to_vec(&request)?)
            .await
            .map_err(|failure| SourceError::Network(error(failure)))
    }

    async fn read<T: DeserializeOwned>(
        &self,
        device: &str,
        source: &SourceId,
        read: Read,
    ) -> SourceResult<T> {
        let mut response = self
            .stream(
                device,
                Stream::Catalog {
                    source: source.clone(),
                    read,
                },
            )
            .await?;
        let mut bytes = Vec::new();
        response
            .reader
            .read_to_end(&mut bytes)
            .await
            .map_err(|failure| SourceError::Network(failure.to_string()))?;
        Ok(serde_json::from_slice(&bytes)?)
    }
}

impl sources::PeerLink for Link {
    fn marker<'a>(
        &'a self,
        device: &'a str,
        source: &'a SourceId,
    ) -> BoxFuture<'a, SourceResult<library::Freshness>> {
        Box::pin(async move {
            let marker: Vec<u8> = self.read(device, source, Read::Marker).await?;
            Ok(library::Freshness::new(marker)?)
        })
    }

    fn ids<'a>(
        &'a self,
        device: &'a str,
        source: &'a SourceId,
    ) -> BoxFuture<'a, SourceResult<(Vec<String>, i64)>> {
        Box::pin(self.read(device, source, Read::Ids))
    }

    fn changed_since<'a>(
        &'a self,
        device: &'a str,
        source: &'a SourceId,
        since: i64,
    ) -> BoxFuture<'a, SourceResult<Vec<String>>> {
        Box::pin(self.read(device, source, Read::ChangedSince { since }))
    }

    fn observations<'a>(
        &'a self,
        device: &'a str,
        source: &'a SourceId,
        ids: &'a [String],
    ) -> BoxFuture<'a, SourceResult<Vec<library::TrackObservation>>> {
        Box::pin(self.read(device, source, Read::Observations { ids: ids.to_vec() }))
    }

    fn image<'a>(
        &'a self,
        device: &'a str,
        source: &'a SourceId,
        image_ref: &'a str,
        size: Option<u32>,
    ) -> BoxFuture<'a, SourceResult<sources::ImageBytes>> {
        Box::pin(async move {
            let mut response = self
                .stream(
                    device,
                    Stream::Image {
                        source: source.clone(),
                        image: image_ref.to_owned(),
                        size,
                    },
                )
                .await?;
            let mut bytes = Vec::new();
            response
                .reader
                .read_to_end(&mut bytes)
                .await
                .map_err(|failure| SourceError::Network(failure.to_string()))?;
            Ok(sources::ImageBytes {
                bytes,
                content_type: response.metadata["content_type"]
                    .as_str()
                    .map(str::to_owned),
            })
        })
    }

    fn open<'a>(
        &'a self,
        device: &'a str,
        source: &'a SourceId,
        object: &'a str,
        encoding: Option<library::AudioEncoding>,
        range: Option<(u64, Option<u64>)>,
        window: Option<(u64, Option<u64>)>,
    ) -> BoxFuture<'a, SourceResult<PeerAudio>> {
        Box::pin(async move {
            let response = self
                .stream(
                    device,
                    Stream::Audio {
                        source: source.clone(),
                        object: object.to_owned(),
                        encoding,
                        range,
                        window,
                    },
                )
                .await?;
            let metadata: PeerByteMetadata = serde_json::from_value(response.metadata)?;
            Ok(PeerAudio {
                reader: response.reader,
                length: metadata.length,
                offset: metadata.offset,
                content_type: metadata.content_type,
                validator: metadata.validator,
            })
        })
    }
}

impl ConnectOwner {
    /// Answers a request on the media protocol.
    pub(super) async fn answer_stream(&self, body: &[u8]) -> Result<PeerByteResponse, String> {
        match serde_json::from_slice(body).map_err(error)? {
            Stream::Catalog { source, read } => {
                self.local_source(&source)?;
                let answer = match read {
                    Read::Marker => serde_json::to_vec(
                        self.catalog
                            .source_marker(&source)
                            .await
                            .map_err(error)?
                            .as_bytes(),
                    ),
                    Read::Ids => {
                        // Taken before the read, so a change during it is still after the clock.
                        let clock = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_or(0, |time| time.as_secs() as i64);
                        let ids = self
                            .catalog
                            .source_track_ids(&source)
                            .await
                            .map_err(error)?;
                        serde_json::to_vec(&(ids, clock))
                    }
                    Read::ChangedSince { since } => serde_json::to_vec(
                        &self
                            .catalog
                            .source_changed_since(&source, since)
                            .await
                            .map_err(error)?,
                    ),
                    Read::Observations { ids } => {
                        let mut tracks = self
                            .catalog
                            .source_observations(&source, &ids)
                            .await
                            .map_err(error)?;
                        for track in &mut tracks {
                            let image = sources::NativeImageRef::new(
                                format!("track:{}", track.object_id),
                                track
                                    .audio_revision
                                    .map(|revision| super::records::hex(&revision)),
                            );
                            track.artwork_binding = Some(
                                sources::native_artwork_binding(source.as_str(), &image)
                                    .map_err(error)?,
                            );
                        }
                        serde_json::to_vec(&tracks)
                    }
                }
                .map_err(error)?;
                Ok(PeerByteResponse {
                    metadata: serde_json::Value::Null,
                    reader: Box::pin(std::io::Cursor::new(answer)),
                })
            }
            Stream::Image {
                source,
                image,
                size,
            } => self.answer_image(&source, &image, size).await,
            Stream::Audio {
                source,
                object,
                encoding,
                range,
                window,
            } => {
                self.answer_audio(&source, &object, encoding, range, window)
                    .await
            }
            Stream::Observe => self.observed(),
        }
    }

    /// Only this device's own Local sources are served.
    fn local_source(&self, source: &SourceId) -> Result<(), String> {
        self.settings
            .load()
            .sources
            .configured
            .iter()
            .any(|item| item.configuration.source_id == *source && item.configuration.is_local())
            .then_some(())
            .ok_or_else(|| "This device does not hold the requested source".into())
    }

    async fn local_track(&self, source: &SourceId, object: &str) -> Result<String, String> {
        self.local_source(source)?;
        self.catalog
            .track_uri(source, object)
            .await
            .map_err(error)?
            .ok_or_else(|| "The requested track is unavailable on this device".into())
    }

    /// The picture the track's row shows here, from its own files; no picture is an empty body.
    async fn answer_image(
        &self,
        source: &SourceId,
        image: &str,
        size: Option<u32>,
    ) -> Result<PeerByteResponse, String> {
        let object = image
            .strip_prefix("track:")
            .ok_or("The requested image is unavailable on this device")?;
        let uri = self.local_track(source, object).await?;
        let binding = self
            .catalog
            .track_artwork_bindings(&[uri], &library::ReadCancellation::new())
            .await
            .map_err(error)?
            .pop();
        let bytes = match binding {
            Some(binding) => {
                let binding = artwork::ArtworkBinding::opaque(&binding);
                let request = match size {
                    Some(size) => artwork::ArtworkRequest::new(binding, size),
                    None => artwork::ArtworkRequest::original(binding, 512),
                };
                self.source
                    .shared
                    .artwork
                    .image_bytes(request)
                    .await?
                    .unwrap_or_default()
            }
            None => Vec::new(),
        };
        Ok(PeerByteResponse {
            metadata: serde_json::json!({ "content_type": artwork::image_mime(&bytes) }),
            reader: Box::pin(std::io::Cursor::new(bytes)),
        })
    }

    /// The track's file: transcoded with the window asked for, or its original bytes in the
    /// range asked for.
    async fn answer_audio(
        &self,
        source: &SourceId,
        object: &str,
        encoding: Option<library::AudioEncoding>,
        range: Option<(u64, Option<u64>)>,
        window: Option<(u64, Option<u64>)>,
    ) -> Result<PeerByteResponse, String> {
        let uri = self.local_track(source, object).await?;
        let location = self
            .catalog
            .playable_by_uri(&uri)
            .await
            .map_err(error)?
            .and_then(|playable| playable.local_uri())
            .ok_or("The requested file is unavailable on this device")?;
        let path = library::file_media_path(&location);
        let mut metadata = PeerByteMetadata {
            length: None,
            offset: 0,
            content_type: encoding.map(|encoding| encoding.codec.content_type().into()),
            validator: None,
        };
        if let Some(encoding) = encoding {
            let mut stream = if path.is_some() {
                audio_processing::ResolvedStream::new(location)
            } else {
                sources::resolve_document_stream(&location)
                    .await
                    .map_err(error)?
            };
            if let Some((start, end)) = window {
                // An empty interval must never widen into the whole recording.
                if end.is_some_and(|end| end <= start) {
                    return Err(format!(
                        "requested audio interval {start}..{end:?} is empty"
                    ));
                }
                stream = stream.with_window(start, end.unwrap_or(u64::MAX / 1_000_000));
            }
            let reader = tokio::task::spawn_blocking(move || {
                audio_processing::TranscodedAudioReader::new(&stream, encoding.into(), || false)
                    .map_err(error)
            })
            .await
            .map_err(error)??;
            return Ok(PeerByteResponse {
                metadata: serde_json::to_value(metadata).map_err(error)?,
                reader: blocking_reader(reader),
            });
        }
        let start = range.map_or(0, |(start, _)| start);
        let end = move |length: u64| range.and_then(|(_, end)| end).unwrap_or(length).min(length);
        let reader: Pin<Box<dyn AsyncRead + Send + Unpin>> = match path {
            Some(path) => {
                let (file, reader) = rufin_connect::media::open_file(&path, start, None)
                    .await
                    .map_err(error)?;
                metadata.length = Some(file.length);
                metadata.validator = file.validator;
                Box::pin(reader.take(end(file.length).saturating_sub(start)))
            }
            None => {
                let (reader, length, validator) = tokio::task::spawn_blocking(move || {
                    use std::io::Seek;
                    let mut reader = sources::open_document_input(&location).map_err(error)?;
                    let length = reader.seek(std::io::SeekFrom::End(0)).map_err(error)?;
                    let validator = sources::stat_document(&location).ok().and_then(|entry| {
                        entry.revision.map(|revision| {
                            blake3::hash(
                                format!("{location}:{}:{length}:{revision}", entry.native_id)
                                    .as_bytes(),
                            )
                            .to_hex()
                            .to_string()
                        })
                    });
                    reader
                        .seek(std::io::SeekFrom::Start(start))
                        .map_err(error)?;
                    Ok::<_, String>((
                        reader.take(end(length).saturating_sub(start)),
                        length,
                        validator,
                    ))
                })
                .await
                .map_err(error)??;
                metadata.length = Some(length);
                metadata.validator = validator;
                blocking_reader(reader)
            }
        };
        metadata.offset = start;
        Ok(PeerByteResponse {
            metadata: serde_json::to_value(metadata).map_err(error)?,
            reader,
        })
    }
}

/// A blocking reader served from a blocking thread.
fn blocking_reader(
    mut reader: impl std::io::Read + Send + 'static,
) -> Pin<Box<dyn AsyncRead + Send + Unpin>> {
    let (sender, receiver) = tokio::sync::mpsc::channel(2);
    tokio::task::spawn_blocking(move || {
        let mut bytes = [0; 65536];
        loop {
            let chunk = match reader.read(&mut bytes) {
                Ok(0) => break,
                Ok(length) => Ok(std::io::Cursor::new(bytes[..length].to_vec())),
                Err(failure) => Err(failure),
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

/// Installed once at startup; the sources' peer source asks through it.
pub(super) fn install(owner: &Arc<ConnectOwner>) {
    sources::install_peer_link(Arc::new(Link(Arc::downgrade(owner))));
}
