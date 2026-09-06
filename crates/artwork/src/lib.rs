//! Selects, fetches, caches, and decodes artwork.
//!
//! The caller owns final decoded results. This crate chooses the image source,
//! avoids duplicate work, prioritizes requests, and keeps normalized images on
//! disk.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use sources::{Source, SourceId};
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

pub(crate) type SourceResolver = dyn Fn(&SourceId) -> Option<Arc<Source>> + Send + Sync;

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
pub struct ArtworkKey {
    binding: String,
    variant: String,
    fetch_size: u32,
    render_size: u32,
}

impl ArtworkKey {
    fn derive(
        identity: &str,
        sizes: (u32, u32),
        external: Option<&ExternalPolicy>,
        allow_fetch: bool,
        epochs: (u64, u64),
    ) -> Self {
        let policy = external
            .map(|policy| format!("{policy:?}\0{}", epochs.1))
            .unwrap_or_default();
        Self {
            binding: Self::binding_digest(identity),
            variant: Self::binding_digest(&format!("{policy}\0{allow_fetch}\0{}", epochs.0)),
            fetch_size: sizes.0,
            render_size: sizes.1,
        }
    }

    fn binding_digest(identity: &str) -> String {
        format!("{:x}", md5::compute(identity.as_bytes()))
    }

    fn reuse_group(&self) -> (String, String) {
        (self.binding.clone(), self.variant.clone())
    }

    pub fn same_image(&self, other: &Self) -> bool {
        self.binding == other.binding && self.variant == other.variant
    }
}

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

pub struct PreparedArtwork {
    pub key: ArtworkKey,
    pub ready: Option<Arc<DecodedImage>>,
    request: ArtworkRequest,
    allow_fetch: bool,
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
    source_resolver: Arc<Mutex<Option<Arc<SourceResolver>>>>,
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
        let source_resolver = Arc::new(Mutex::new(None));
        let pipeline = pipeline::Pipeline::new(&cache_root, runtime, Arc::clone(&source_resolver))?;
        Ok(Self {
            pipeline: Arc::new(pipeline),
            source_resolver,
        })
    }

    pub fn install_source_resolver(
        &self,
        resolver: impl Fn(&SourceId) -> Option<Arc<Source>> + Send + Sync + 'static,
    ) {
        *self
            .source_resolver
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::new(resolver));
    }

    fn prepare_request(&self, request: ArtworkRequest, allow_fetch: bool) -> PreparedArtwork {
        let (key, ready) = self.pipeline.key_and_image(&request, allow_fetch);
        PreparedArtwork {
            key,
            ready,
            request,
            allow_fetch,
        }
    }

    pub fn prepare(&self, request: ArtworkRequest) -> PreparedArtwork {
        self.prepare_request(request, true)
    }

    pub fn prepare_cache_only(&self, request: ArtworkRequest) -> PreparedArtwork {
        self.prepare_request(request, false)
    }

    pub fn request_prepared(&self, prepared: PreparedArtwork) -> Result<ArtworkLoad, ArtworkError> {
        self.pipeline
            .request(prepared.request, prepared.allow_fetch)
    }

    pub fn source_preparation_complete(
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
        source_id: &SourceId,
        accepted_digest: [u8; 32],
        progress: &(dyn Fn(u64, usize) + Send + Sync),
        cancelled: Arc<AtomicBool>,
    ) -> Result<Option<u64>, ArtworkError> {
        let accepted_revision = digest_revision(&accepted_digest);
        if self.source_preparation_complete(source_id, accepted_revision)? {
            return Ok(None);
        }
        let mut completed = 0_usize;
        let mut visible_progress = false;
        let revision = accepted_revision;
        let manifest = self.begin_source_manifest(source_id.clone(), revision)?;
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
            let page_cancelled = Arc::clone(&cancelled);
            let summary = tokio::task::spawn_blocking(move || {
                pipeline.prefetch_source_artwork(page.into(), &|_, _| {}, &|| {
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

    pub fn cache_only_file(&self, request: &ArtworkRequest) -> Option<PathBuf> {
        self.pipeline.cache_only_file(request)
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

    #[test]
    fn preparing_a_binding_does_not_construct_its_source() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let artwork =
            Artwork::new(directory.path().join("covers"), runtime.handle().clone()).unwrap();
        let encoded =
            sources::native_artwork_binding("source", &sources::NativeImageRef::new("cover", None))
                .unwrap();
        let request = ArtworkRequest::new(ArtworkBinding::opaque(&encoded), 128, 64);
        let identity_without_resolver = artwork.prepare(request.clone()).key;
        let resolutions = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&resolutions);
        artwork.install_source_resolver(move |_| {
            observed.fetch_add(1, Ordering::Relaxed);
            None
        });

        let identity_with_resolver = artwork.prepare(request.clone()).key;
        let _cache_only = artwork.prepare_cache_only(request.clone());
        let _cached_file = artwork.cache_only_file(&request);

        assert_eq!(identity_without_resolver, identity_with_resolver);
        assert_eq!(resolutions.load(Ordering::Relaxed), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cached_native_artwork_reuses_pixels_without_resolving_a_provider() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("covers");
        let artwork = Artwork::new(&root, Handle::current()).unwrap();
        let encoded = sources::native_artwork_binding(
            "source",
            &sources::NativeImageRef::new("cover", Some("revision".into())),
        )
        .unwrap();
        let binding = ArtworkBinding::opaque(&encoded);
        let cache = cache::FilesystemCache::new(root.join("v1")).unwrap();
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            32,
            32,
            image::Rgba([40, 80, 120, 255]),
        ))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
        cache
            .write_ready(binding.candidate().unwrap(), 256, png.get_ref())
            .unwrap();
        let resolutions = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&resolutions);
        artwork.install_source_resolver(move |_| {
            observed.fetch_add(1, Ordering::Relaxed);
            None
        });

        let prepared = artwork.prepare(ArtworkRequest::new(binding.clone(), 256, 128));
        let key = prepared.key.clone();
        let ArtworkLoad::Pending(pending) = artwork.request_prepared(prepared).unwrap() else {
            panic!("disk cache requires worker decoding");
        };
        let ArtworkOutcome::Ready(image) = pending.finish().await else {
            panic!("cached image must decode");
        };
        assert_eq!(image.key(), &key);
        let smaller = artwork.prepare(ArtworkRequest::new(binding.clone(), 96, 64));
        assert!(Arc::ptr_eq(smaller.ready.as_ref().unwrap(), &image));
        assert_eq!(resolutions.load(Ordering::Relaxed), 0);

        artwork.invalidate_source(&SourceId::new("source")).unwrap();
        let invalidated = artwork.prepare(ArtworkRequest::new(binding, 256, 128));
        assert_ne!(invalidated.key, key);
        assert!(invalidated.ready.is_none());
        assert_eq!(resolutions.load(Ordering::Relaxed), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_native_cache_miss_resolves_its_source_in_the_worker() {
        let directory = tempfile::tempdir().unwrap();
        let artwork = Artwork::new(directory.path().join("covers"), Handle::current()).unwrap();
        let resolutions = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&resolutions);
        artwork.install_source_resolver(move |_| {
            observed.fetch_add(1, Ordering::Relaxed);
            None
        });
        let encoded =
            sources::native_artwork_binding("source", &sources::NativeImageRef::new("cover", None))
                .unwrap();
        let prepared = artwork.prepare(ArtworkRequest::new(
            ArtworkBinding::opaque(&encoded),
            128,
            64,
        ));
        assert_eq!(resolutions.load(Ordering::Relaxed), 0);

        let ArtworkLoad::Pending(pending) = artwork.request_prepared(prepared).unwrap() else {
            panic!("uncached native artwork should enter the worker");
        };
        assert!(matches!(pending.finish().await, ArtworkOutcome::Failed(_)));
        assert_eq!(resolutions.load(Ordering::Relaxed), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_cache_only_miss_never_resolves_a_source() {
        let directory = tempfile::tempdir().unwrap();
        let artwork = Artwork::new(directory.path().join("covers"), Handle::current()).unwrap();
        let resolutions = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&resolutions);
        artwork.install_source_resolver(move |_| {
            observed.fetch_add(1, Ordering::Relaxed);
            None
        });
        let encoded =
            sources::native_artwork_binding("source", &sources::NativeImageRef::new("cover", None))
                .unwrap();
        let prepared = artwork.prepare_cache_only(ArtworkRequest::new(
            ArtworkBinding::opaque(&encoded),
            128,
            64,
        ));

        let ArtworkLoad::Pending(pending) = artwork.request_prepared(prepared).unwrap() else {
            panic!("uncached artwork should check the cache worker");
        };
        assert!(matches!(pending.finish().await, ArtworkOutcome::Missing));
        assert_eq!(resolutions.load(Ordering::Relaxed), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_local_binding_reads_its_locator_without_a_source() {
        let directory = tempfile::tempdir().unwrap();
        let image = directory.path().join("cover.png");
        let mut encoded_image = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(1, 1, image::Rgb([255, 0, 0])))
            .write_to(&mut encoded_image, image::ImageFormat::Png)
            .unwrap();
        std::fs::write(&image, encoded_image.into_inner()).unwrap();
        let artwork = Artwork::new(directory.path().join("covers"), Handle::current()).unwrap();
        let resolutions = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&resolutions);
        artwork.install_source_resolver(move |_| {
            observed.fetch_add(1, Ordering::Relaxed);
            None
        });
        let encoded = serde_json::to_vec(&sources::LocalImageRef::File {
            source_id: SourceId::new("configured-local"),
            path: image.to_string_lossy().into_owned(),
            revision: "fixture".to_string(),
        })
        .unwrap();
        let request = ArtworkRequest::new(ArtworkBinding::opaque(&encoded), 128, 64);
        let prepared = artwork.prepare(request.clone());
        let key = prepared.key.clone();

        let ArtworkLoad::Pending(pending) = artwork.request_prepared(prepared).unwrap() else {
            panic!("uncached Local artwork should enter the worker");
        };
        let ArtworkOutcome::Ready(decoded) = pending.finish().await else {
            panic!("Local image must decode");
        };
        assert!(Arc::ptr_eq(
            artwork.prepare(request.clone()).ready.as_ref().unwrap(),
            &decoded
        ));
        let cached = artwork.cache_only_file(&request).unwrap();
        assert!(cached.is_file());
        let manifest = artwork
            .begin_source_manifest(SourceId::new("configured-local"), 1)
            .unwrap();
        manifest.record_page(&[encoded]).unwrap();
        manifest.finish().unwrap();
        assert!(
            cached.is_file(),
            "current Local bindings survive manifest reconciliation"
        );
        artwork
            .invalidate_source(&SourceId::new("configured-local"))
            .unwrap();
        assert!(!cached.exists());
        let invalidated = artwork.prepare(request);
        assert_ne!(invalidated.key, key);
        assert!(
            invalidated.ready.is_none(),
            "Forget must remove even a still-live decoded binding"
        );
        assert_eq!(resolutions.load(Ordering::Relaxed), 0);
    }

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
                &source,
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
                &SourceId::new("source"),
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
