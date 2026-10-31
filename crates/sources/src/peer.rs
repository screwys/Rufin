//! A paired device's Local library, read through Rufin Connect like a read-only server.
use std::collections::HashSet;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use audio_processing::ResolvedStream;
use futures_util::future::BoxFuture;
use library::{AudioChannels, AudioCodec, AudioEncoding, Freshness, SourceId, TrackObservation};
use serde::Deserialize;

use crate::file::remote::input::{FileInput, FileInputServer};
use crate::{
    ImageBytes, SourceConfiguration, SourceError, SourceReadProgress, SourceReadStage, SourceResult,
};

/// Observations asked of the owner at a time.
const PAGE: usize = 500;

pub struct PeerAudio {
    pub reader: Pin<Box<dyn tokio::io::AsyncRead + Send + Unpin>>,
    pub length: Option<u64>,
    pub offset: u64,
    pub content_type: Option<String>,
    pub validator: Option<String>,
}

/// What the device that owns a Local source answers about it, by the owner's source id and
/// object ids. Rufin Connect installs it.
pub trait PeerLink: Send + Sync {
    fn marker<'a>(
        &'a self,
        device: &'a str,
        source: &'a SourceId,
    ) -> BoxFuture<'a, SourceResult<Freshness>>;
    /// The owner's indexed track ids and the owner's clock (unix seconds).
    fn ids<'a>(
        &'a self,
        device: &'a str,
        source: &'a SourceId,
    ) -> BoxFuture<'a, SourceResult<(Vec<String>, i64)>>;
    fn changed_since<'a>(
        &'a self,
        device: &'a str,
        source: &'a SourceId,
        since: i64,
    ) -> BoxFuture<'a, SourceResult<Vec<String>>>;
    fn observations<'a>(
        &'a self,
        device: &'a str,
        source: &'a SourceId,
        ids: &'a [String],
    ) -> BoxFuture<'a, SourceResult<Vec<TrackObservation>>>;
    fn image<'a>(
        &'a self,
        device: &'a str,
        source: &'a SourceId,
        image_ref: &'a str,
        size: Option<u32>,
    ) -> BoxFuture<'a, SourceResult<ImageBytes>>;
    fn open<'a>(
        &'a self,
        device: &'a str,
        source: &'a SourceId,
        object: &'a str,
        encoding: Option<AudioEncoding>,
        range: Option<(u64, Option<u64>)>,
        window: Option<(u64, Option<u64>)>,
    ) -> BoxFuture<'a, SourceResult<PeerAudio>>;
}

/// The link of the running Connect. A runtime started again in the same process installs
/// its own.
static LINK: RwLock<Option<Arc<dyn PeerLink>>> = RwLock::new(None);

pub fn install_peer_link(link: Arc<dyn PeerLink>) {
    *LINK.write().unwrap_or_else(|p| p.into_inner()) = Some(link);
}

#[derive(Deserialize)]
struct PeerSourceConfig {
    device: String,
}

/// The source of kind `peer`: another device's Local source under its own source id.
#[derive(Clone)]
pub struct PeerSource {
    device: String,
    source: SourceId,
    link: Arc<dyn PeerLink>,
}

impl PeerSource {
    pub(crate) fn open(configuration: &SourceConfiguration) -> SourceResult<Self> {
        let config: PeerSourceConfig = crate::config::decode_provider_payload(configuration)?;
        Ok(Self {
            device: config.device,
            source: configuration.source_id.clone(),
            link: LINK
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .clone()
                .ok_or_else(|| SourceError::InvalidConfig("Rufin Connect is unavailable".into()))?,
        })
    }

    pub(crate) async fn freshness(&self) -> SourceResult<Option<Freshness>> {
        self.link.marker(&self.device, &self.source).await.map(Some)
    }

    /// Lists every track the owner indexes; with `since`, only the tracks added or removed
    /// and those whose files changed after it. Returns the owner's clock.
    pub(crate) async fn acquire_catalog(
        &self,
        observation: &mut library::Observation<'_>,
        since: Option<i64>,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<i64> {
        let (ids, clock) = self.link.ids(&self.device, &self.source).await?;
        let wanted = match since {
            None => ids,
            Some(since) => {
                let known = observation
                    .catalog
                    .source_track_ids(&self.source)
                    .await?
                    .into_iter()
                    .collect::<HashSet<_>>();
                let listed = ids.iter().map(String::as_str).collect::<HashSet<_>>();
                let gone = known
                    .iter()
                    .filter(|id| !listed.contains(id.as_str()))
                    .cloned()
                    .collect::<Vec<_>>();
                observation.remove_source_items("track", &gone).await?;
                let mut wanted = self
                    .link
                    .changed_since(
                        &self.device,
                        &self.source,
                        since - crate::source::SYNC_MARGIN,
                    )
                    .await?;
                wanted.extend(ids.into_iter().filter(|id| !known.contains(id)));
                wanted.sort_unstable();
                wanted.dedup();
                wanted
            }
        };
        let mut completed = 0;
        for page in wanted.chunks(PAGE) {
            if cancelled() {
                return Err(SourceError::Cancelled);
            }
            let tracks = self
                .link
                .observations(&self.device, &self.source, page)
                .await?;
            observation
                .apply_catalog(
                    &library::CatalogBatch {
                        tracks,
                        ..Default::default()
                    },
                    true,
                )
                .await?;
            completed += page.len();
            progress(SourceReadProgress {
                stage: SourceReadStage::Tracks,
                completed,
                total: Some(wanted.len()),
            });
        }
        if since.is_none() {
            if cancelled() {
                return Err(SourceError::Cancelled);
            }
            observation
                .complete_source_observation(&["track"], None, None)
                .await?;
        }
        Ok(clock)
    }

    /// The owner's file through the loopback server. A transcoding owner applies the window
    /// it is asked for; the original bytes are windowed here.
    pub(crate) async fn stream(
        &self,
        playable: &library::Playable,
        request: playback::StreamRequest,
    ) -> SourceResult<ResolvedStream> {
        let object = playable.object_id.as_deref().ok_or(SourceError::NotFound)?;
        let encoding = request
            .quality
            .max_bitrate_kbps()
            .map(|bitrate| AudioEncoding {
                codec: AudioCodec::Opus,
                bitrate_kbps: Some(bitrate),
                channels: AudioChannels::StereoLossy,
            });
        let input = FileInputServer::start(FileInput::Link {
            peer: self.clone(),
            encoding,
        })
        .await?;
        let stream = input.stream(object, &playable.media_uri);
        let stream = match playable.window {
            Some((start, end)) => stream.with_window(start, end),
            None => stream,
        };
        let stream = if encoding.is_some() {
            stream.with_time_seek()
        } else {
            stream
        };
        Ok(stream
            .with_transcoding(encoding.is_some())
            .with_content_type(encoding.map(|value| value.codec.content_type().into())))
    }

    pub(crate) async fn audio(
        &self,
        object: &str,
        encoding: Option<AudioEncoding>,
        range: Option<(u64, Option<u64>)>,
        window: Option<(u64, Option<u64>)>,
    ) -> SourceResult<PeerAudio> {
        self.link
            .open(&self.device, &self.source, object, encoding, range, window)
            .await
    }

    pub(crate) async fn image(
        &self,
        image: &crate::NativeImageRef,
        size: crate::ImageSize,
    ) -> SourceResult<ImageBytes> {
        let size = match size {
            crate::ImageSize::Original => None,
            crate::ImageSize::Thumbnail(size) => Some(size),
        };
        self.link
            .image(&self.device, &self.source, &image.item_id, size)
            .await
    }
}
