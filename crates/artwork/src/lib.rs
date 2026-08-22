//! Selects, fetches, caches, and decodes artwork.
//!
//! The caller owns final decoded results. This crate chooses the image source,
//! avoids duplicate work, prioritizes requests, and keeps normalized images on
//! disk.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use library::{SourceArtwork, SourceId};
use sources::{ImageBytes, Source, SourceImageRequest, SourceResult};
use thiserror::Error;
use tokio::runtime::Handle;
use tokio::sync::oneshot;

mod cache;
mod decode;
mod fetch;
mod pipeline;
mod selection;

#[cfg(test)]
mod tests;

pub use decode::{DecodedImage, RgbaImage, decode_rgba, square_thumbnail_png};
pub use selection::{ArtworkBinding, ArtworkBindings, BoundArtwork};

#[derive(Clone)]
pub struct SourceImages {
    pub source_id: SourceId,
    source: Option<Arc<Source>>,
    #[cfg(test)]
    test_source: Option<Arc<dyn TestImageSource + Send + Sync>>,
}

impl SourceImages {
    pub fn new(source: Arc<Source>) -> Self {
        Self {
            source_id: source.source_id().clone(),
            source: Some(source),
            #[cfg(test)]
            test_source: None,
        }
    }

    pub fn cache_only(source_id: SourceId) -> Self {
        Self {
            source_id,
            source: None,
            #[cfg(test)]
            test_source: None,
        }
    }

    fn can_fetch(&self) -> bool {
        self.source.is_some() || {
            #[cfg(test)]
            {
                self.test_source.is_some()
            }
            #[cfg(not(test))]
            {
                false
            }
        }
    }

    async fn image(&self, request: SourceImageRequest) -> SourceResult<ImageBytes> {
        if let Some(source) = &self.source {
            return source.image(request).await;
        }
        #[cfg(test)]
        if let Some(source) = &self.test_source {
            return source.image(request).await;
        }
        Err(sources::SourceError::InvalidRequest(
            "artwork source is not connected",
        ))
    }

    #[cfg(test)]
    fn testing(source_id: SourceId, source: Arc<dyn TestImageSource + Send + Sync>) -> Self {
        Self {
            source_id,
            source: None,
            test_source: Some(source),
        }
    }
}

#[cfg(test)]
#[async_trait::async_trait(?Send)]
trait TestImageSource {
    async fn image(&self, request: SourceImageRequest) -> SourceResult<ImageBytes>;
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExternalPolicy {
    pub allow_cached: bool,
    pub allow_network: bool,
    pub allow_musicbrainz: bool,
    pub lastfm_api_key: String,
}

impl ExternalPolicy {
    pub fn new(allow_cached: bool, allow_network: bool, lastfm_api_key: impl Into<String>) -> Self {
        Self {
            allow_cached,
            allow_network,
            allow_musicbrainz: true,
            lastfm_api_key: lastfm_api_key.into(),
        }
    }

    pub const fn with_musicbrainz(mut self, allow: bool) -> Self {
        self.allow_musicbrainz = allow;
        self
    }

    pub const fn disabled() -> Self {
        Self {
            allow_cached: false,
            allow_network: false,
            allow_musicbrainz: false,
            lastfm_api_key: String::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtworkRequest {
    pub binding: ArtworkBinding,
    pub fetch_size: u32,
    pub render_size: u32,
    pub external: ExternalPolicy,
}

impl ArtworkRequest {
    pub fn new(binding: ArtworkBinding, fetch_size: u32, render_size: u32) -> Self {
        Self {
            binding,
            fetch_size: fetch_size.max(1),
            render_size: render_size.max(1),
            external: ExternalPolicy::disabled(),
        }
    }

    pub fn with_external(mut self, external: ExternalPolicy) -> Self {
        self.external = external;
        self
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct ArtworkKey(String);

impl ArtworkKey {
    fn new(identity: String) -> Self {
        Self(format!("{:x}", md5::compute(identity.as_bytes())))
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DecodedImageIdentity(ArtworkKey);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct RequestId(u64);

pub enum ArtworkLoad {
    Ready(Arc<DecodedImage>),
    Missing,
    Pending(PendingArtwork),
}

#[derive(Clone, Debug)]
pub enum ArtworkOutcome {
    Ready(Arc<DecodedImage>),
    Missing,
    Failed(Arc<str>),
    Invalidated,
}

pub struct PendingArtwork {
    request_id: RequestId,
    completion: Option<oneshot::Receiver<ArtworkOutcome>>,
    pipeline: Arc<pipeline::Pipeline>,
}

impl PendingArtwork {
    pub async fn finish(mut self) -> ArtworkOutcome {
        let Some(completion) = self.completion.take() else {
            return ArtworkOutcome::Failed("artwork request ended unexpectedly".into());
        };
        match completion.await {
            Ok(outcome) => outcome,
            Err(_) => ArtworkOutcome::Failed("artwork request ended unexpectedly".into()),
        }
    }
}

impl Drop for PendingArtwork {
    fn drop(&mut self) {
        self.pipeline.cancel(self.request_id);
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ArtworkPreparation {
    pub total: usize,
    pub ready: usize,
    pub missing: usize,
    pub failed: usize,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ArtworkVisualIdentity(String);

impl ArtworkVisualIdentity {
    fn new(identity: String) -> Self {
        Self(format!("{:x}", md5::compute(identity.as_bytes())))
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ArtworkRequestIdentity(String);

impl ArtworkRequestIdentity {
    fn new(identity: String) -> Self {
        Self(format!("{:x}", md5::compute(identity.as_bytes())))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtworkBindingIdentity {
    pub visual: ArtworkVisualIdentity,
    pub request: ArtworkRequestIdentity,
}

pub struct PreparedArtwork {
    pub identity: ArtworkBindingIdentity,
    pub ready: Option<Arc<DecodedImage>>,
    pub preview: Option<Arc<DecodedImage>>,
    source: SourceImages,
    request: ArtworkRequest,
}

#[derive(Debug, Error)]
pub enum ArtworkError {
    #[error("artwork cache failed: {0}")]
    Cache(#[from] std::io::Error),
    #[error("artwork decode failed: {0}")]
    Decode(String),
    #[error("artwork fetch setup failed: {0}")]
    FetchSetup(String),
    #[error("artwork preparation was cancelled")]
    Cancelled,
}

#[derive(Clone)]
pub struct Artwork {
    pipeline: Arc<pipeline::Pipeline>,
}

impl Artwork {
    pub fn new(cache_root: impl AsRef<Path>, runtime: Handle) -> Result<Self, ArtworkError> {
        let cache_root = cache::current_layout(cache_root.as_ref())?;
        let pipeline = pipeline::Pipeline::new(&cache_root, runtime)?;
        Ok(Self {
            pipeline: Arc::new(pipeline),
        })
    }

    pub fn prepare(&self, source: SourceImages, request: ArtworkRequest) -> PreparedArtwork {
        let (identity, ready, preview) =
            self.pipeline.binding_identity_and_images(&source, &request);
        PreparedArtwork {
            identity,
            ready,
            preview,
            source,
            request,
        }
    }

    pub fn request_prepared(&self, prepared: PreparedArtwork) -> Result<ArtworkLoad, ArtworkError> {
        self.pipeline.request(prepared.source, prepared.request)
    }

    /// Caches candidate source-owned images without publishing a manifest.
    pub fn prefetch_source_artwork(
        &self,
        source: SourceImages,
        artwork: Arc<[SourceArtwork]>,
        progress: &(dyn Fn(usize, usize) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<ArtworkPreparation, ArtworkError> {
        self.pipeline
            .prefetch_source_artwork(source, artwork, progress, cancelled)
    }

    pub fn source_preparation_complete(
        &self,
        source_id: &SourceId,
        revision: u64,
    ) -> Result<bool, ArtworkError> {
        self.pipeline
            .source_preparation_complete(source_id, revision)
    }

    pub fn source_preparation_key(&self, artwork: &[SourceArtwork]) -> u64 {
        let mut identities = artwork
            .iter()
            .map(|artwork| {
                ArtworkBinding::source_artwork(artwork)
                    .stable_identity()
                    .to_string()
            })
            .collect::<Vec<_>>();
        identities.sort_unstable();
        let mut digest = md5::Context::new();
        for identity in identities {
            digest.consume(identity.len().to_le_bytes());
            digest.consume(identity.as_bytes());
        }
        u64::from_le_bytes(digest.finalize().0[..8].try_into().expect("MD5 prefix"))
    }

    /// Caches and reconciles the accepted source-owned artwork manifest.
    pub fn prepare_source_artwork(
        &self,
        source: SourceImages,
        revision: u64,
        artwork: Arc<[SourceArtwork]>,
        progress: &(dyn Fn(usize, usize) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<ArtworkPreparation, ArtworkError> {
        self.pipeline
            .prepare_source_artwork(source, revision, artwork, progress, cancelled)
    }

    pub fn cache_only_file(
        &self,
        source_id: &SourceId,
        request: &ArtworkRequest,
    ) -> Option<PathBuf> {
        self.pipeline.cache_only_file(source_id, request)
    }

    pub fn retry_external(&self) -> Result<(), ArtworkError> {
        self.pipeline.retry_external()
    }

    pub fn invalidate_source(&self, source_id: &SourceId) -> Result<(), ArtworkError> {
        self.pipeline.invalidate_source(source_id)
    }
}
