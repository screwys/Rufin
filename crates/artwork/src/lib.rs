//! Selects, fetches, caches, and decodes artwork.
//!
//! The caller owns final decoded results. This crate chooses the image source,
//! avoids duplicate work, prioritizes requests, and keeps normalized images on
//! disk.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sources::{ImageBytes, Source, SourceId, SourceImageRequest, SourceResult};
use thiserror::Error;
use tokio::runtime::Handle;
use tokio::sync::oneshot;

mod cache;
mod decode;
mod fetch;
mod pipeline;
mod selection;

pub use decode::{DecodedImage, RgbaImage, decode_rgba, square_thumbnail_png};
pub use selection::ArtworkBinding;

#[derive(Clone)]
pub struct SourceImages {
    pub source_id: SourceId,
    source: Option<Arc<Source>>,
}

impl SourceImages {
    pub fn new(source: Arc<Source>) -> Self {
        Self {
            source_id: source.source_id().clone(),
            source: Some(source),
        }
    }

    pub fn cache_only(source_id: SourceId) -> Self {
        Self {
            source_id,
            source: None,
        }
    }

    fn can_fetch(&self) -> bool {
        self.source.is_some()
    }

    async fn image(&self, request: SourceImageRequest) -> SourceResult<ImageBytes> {
        if let Some(source) = &self.source {
            return source.image(request).await;
        }
        Err(sources::SourceError::InvalidRequest(
            "artwork source is not connected",
        ))
    }
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
pub(crate) struct ArtworkPreparation {
    pub total: usize,
    pub ready: usize,
    pub cached: usize,
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
    #[error("artwork library operation failed: {0}")]
    Library(#[from] library::LibraryError),
    #[error("artwork source operation failed: {0}")]
    Source(#[from] sources::SourceError),
}

#[derive(Clone)]
pub struct Artwork {
    pipeline: Arc<pipeline::Pipeline>,
}

struct SourceManifest {
    pipeline: Arc<pipeline::Pipeline>,
    source_id: SourceId,
    revision: u64,
    staging: PathBuf,
}

impl SourceManifest {
    pub fn record_page(&self, bindings: &[Vec<u8>]) -> Result<(), ArtworkError> {
        Ok(self
            .pipeline
            .mark_source_manifest(&self.staging, bindings)?)
    }

    pub fn finish(self) -> Result<(), ArtworkError> {
        Ok(self
            .pipeline
            .complete_source_manifest(&self.source_id, self.revision, &self.staging)?)
    }
}

impl Artwork {
    fn begin_source_manifest(
        &self,
        source_id: SourceId,
        revision: u64,
    ) -> Result<SourceManifest, ArtworkError> {
        let staging = self.pipeline.begin_source_manifest(&source_id, revision)?;
        Ok(SourceManifest {
            pipeline: Arc::clone(&self.pipeline),
            source_id,
            revision,
            staging,
        })
    }
    pub fn new(cache_root: impl AsRef<Path>, runtime: Handle) -> Result<Self, ArtworkError> {
        let cache_root = cache::current_layout(cache_root.as_ref())?;
        let pipeline = pipeline::Pipeline::new(&cache_root, runtime)?;
        Ok(Self {
            pipeline: Arc::new(pipeline),
        })
    }

    pub fn prepare(&self, source: SourceImages, request: ArtworkRequest) -> PreparedArtwork {
        let (identity, ready) = self.pipeline.binding_identity_and_image(&source, &request);
        PreparedArtwork {
            identity,
            ready,
            source,
            request,
        }
    }

    pub fn request_prepared(&self, prepared: PreparedArtwork) -> Result<ArtworkLoad, ArtworkError> {
        self.pipeline.request(prepared.source, prepared.request)
    }

    fn source_preparation_complete(
        &self,
        source_id: &SourceId,
        revision: u64,
    ) -> Result<bool, ArtworkError> {
        self.pipeline
            .source_preparation_complete(source_id, revision)
    }

    pub async fn prepare_database_source(
        &self,
        database: &library::Database,
        source_key: library::SourceKey,
        source: SourceImages,
        accepted_digest: [u8; 32],
        progress: &(dyn Fn(u64, usize) + Send + Sync),
        cancelled: Arc<AtomicBool>,
    ) -> Result<Option<u64>, ArtworkError> {
        let accepted_revision = digest_revision(&accepted_digest);
        if self.source_preparation_complete(&source.source_id, accepted_revision)? {
            return Ok(None);
        }
        let mut completed = 0_usize;
        let mut visible_progress = false;
        if let Some(provider) = source.source.as_ref() {
            let mut after = None;
            loop {
                if cancelled.load(Ordering::Acquire) {
                    return Err(ArtworkError::Cancelled);
                }
                let page = provider
                    .prepare_local_artwork_page(database, source_key, after)
                    .await?;
                completed = completed.saturating_add(page.completed);
                if page.completed > 0 {
                    visible_progress = true;
                    progress(accepted_revision, completed);
                }
                after = page.next_album;
                if after.is_none() {
                    break;
                }
            }
        }
        let revision = digest_revision(&database.finalize_artwork_digest(source_key).await?);
        if self.source_preparation_complete(&source.source_id, revision)? {
            return Ok(Some(revision));
        }
        let manifest = self.begin_source_manifest(source.source_id.clone(), revision)?;
        let mut after_binding = None;
        loop {
            if cancelled.load(Ordering::Acquire) {
                return Err(ArtworkError::Cancelled);
            }
            let page = database
                .artwork_preparation_page(
                    source_key,
                    after_binding.as_deref(),
                    128,
                    &library::ReadCancellation::new(),
                )
                .await?;
            if page.is_empty() {
                break;
            }
            after_binding = page.last().cloned();
            manifest.record_page(&page)?;
            let pipeline = Arc::clone(&self.pipeline);
            let images = source.clone();
            let bindings: Arc<[Vec<u8>]> = page.into();
            let page_cancelled = Arc::clone(&cancelled);
            let summary = tokio::task::spawn_blocking(move || {
                pipeline.prefetch_source_artwork(images, bindings, &|_, _| {}, &move || {
                    page_cancelled.load(Ordering::Acquire)
                })
            })
            .await
            .map_err(|error| ArtworkError::Decode(error.to_string()))??;
            completed = completed.saturating_add(summary.total);
            if summary.total > summary.cached.saturating_add(summary.missing) {
                visible_progress = true;
            }
            if visible_progress {
                progress(revision, completed);
            }
        }
        manifest.finish()?;
        Ok(Some(revision))
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

fn digest_revision(digest: &[u8; 32]) -> u64 {
    u64::from_le_bytes(digest[..8].try_into().expect("digest prefix"))
}

#[cfg(test)]
mod preparation_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[tokio::test]
    async fn completed_database_source_emits_no_preparation_progress() {
        let directory = tempfile::tempdir().unwrap();
        let database = library::Database::open(directory.path().join("library.sqlite"))
            .await
            .unwrap();
        let scan = library::Scan::begin(&database, "source", "Source", "source", None)
            .await
            .unwrap();
        let publication = match scan.finish().await.unwrap() {
            library::ScanOutcome::Changed(publication)
            | library::ScanOutcome::ArtworkChanged(publication)
            | library::ScanOutcome::Identical(publication) => publication,
            outcome => panic!("unexpected Scan outcome: {outcome:?}"),
        };
        let artwork = Artwork::new(directory.path().join("covers"), Handle::current()).unwrap();
        let source = SourceId::new("source");
        artwork
            .begin_source_manifest(source.clone(), digest_revision(&publication.artwork_digest))
            .unwrap()
            .finish()
            .unwrap();
        let progress = AtomicUsize::new(0);
        let result = artwork
            .prepare_database_source(
                &database,
                publication.source,
                SourceImages::cache_only(source),
                publication.artwork_digest,
                &|_, _| {
                    progress.fetch_add(1, Ordering::Relaxed);
                },
                Arc::new(AtomicBool::new(false)),
            )
            .await
            .unwrap();
        assert_eq!(result, None);
        assert_eq!(progress.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn manifest_reconciliation_without_fetch_work_stays_silent() {
        let directory = tempfile::tempdir().unwrap();
        let database = library::Database::open(directory.path().join("library.sqlite"))
            .await
            .unwrap();
        let scan = library::Scan::begin(&database, "source", "Source", "source", None)
            .await
            .unwrap();
        let publication = match scan.finish().await.unwrap() {
            library::ScanOutcome::Changed(publication)
            | library::ScanOutcome::ArtworkChanged(publication)
            | library::ScanOutcome::Identical(publication) => publication,
            outcome => panic!("unexpected Scan outcome: {outcome:?}"),
        };
        let artwork = Artwork::new(directory.path().join("covers"), Handle::current()).unwrap();
        let progress = AtomicUsize::new(0);
        let result = artwork
            .prepare_database_source(
                &database,
                publication.source,
                SourceImages::cache_only(SourceId::new("source")),
                publication.artwork_digest,
                &|_, _| {
                    progress.fetch_add(1, Ordering::Relaxed);
                },
                Arc::new(AtomicBool::new(false)),
            )
            .await
            .unwrap();
        assert!(result.is_some());
        assert_eq!(progress.load(Ordering::Relaxed), 0);
    }
}
