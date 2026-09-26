//! Selects, fetches, caches, and decodes artwork.
//!
//! The caller owns final decoded results. This crate chooses the image source,
//! avoids duplicate work, prioritizes requests, and caches originals and thumbnails.

use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use sources::{Source, SourceId};
use thiserror::Error;
use tokio::runtime::Handle;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

mod animation;
mod cache;
mod decode;
mod fetch;
mod pipeline;
mod selection;

pub use animation::{Animation, AnimationFrame};
pub use decode::{DecodedImage, RgbaImage, decode_rgba, image_mime, square_thumbnail_png};
pub use selection::ArtworkBinding;
pub use sources::ImageSize;

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
    pub fetch_size: ImageSize,
    pub render_size: u32,
    pub external: ExternalPolicy,
    pub cache_only: bool,
}

impl ArtworkRequest {
    pub fn new(binding: ArtworkBinding, fetch_size: u32, render_size: u32) -> Self {
        Self {
            binding,
            fetch_size: ImageSize::Thumbnail(fetch_size.max(1)),
            render_size: render_size.max(1),
            external: ExternalPolicy::disabled(),
            cache_only: false,
        }
    }

    pub fn original(binding: ArtworkBinding, render_size: u32) -> Self {
        Self {
            fetch_size: ImageSize::Original,
            ..Self::new(binding, render_size, render_size)
        }
    }

    pub fn cache_only(mut self) -> Self {
        self.cache_only = true;
        self
    }

    pub fn with_external(mut self, external: ExternalPolicy) -> Self {
        self.external = external;
        self
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ArtworkKey {
    asset: String,
    binding: String,
    variant: String,
    fetch_size: ImageSize,
    render_size: u32,
}

impl ArtworkKey {
    fn derive(
        candidate: Option<&selection::Candidate>,
        sizes: (ImageSize, u32),
        external: Option<&ExternalPolicy>,
        allow_fetch: bool,
        epochs: (u64, u64),
    ) -> Self {
        let policy = external
            .map(|policy| format!("{policy:?}\0{}", epochs.1))
            .unwrap_or_default();
        Self {
            asset: Self::binding_digest(
                &candidate
                    .map(selection::Candidate::asset_identity)
                    .unwrap_or_default(),
            ),
            binding: Self::binding_digest(
                &candidate
                    .map(selection::Candidate::stable_identity)
                    .unwrap_or_default(),
            ),
            variant: Self::binding_digest(&format!(
                "{policy}\0{allow_fetch}\0{}\0{}",
                epochs.0,
                sizes.0 == ImageSize::Original
            )),
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

    /// Whether an existing image can stay visible while this request refreshes it.
    pub fn same_asset(&self, other: &Self) -> bool {
        self.asset == other.asset && self.variant == other.variant
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct RequestId(u64);

#[derive(Clone, Debug)]
pub struct LoadedArtwork {
    pub image: Arc<DecodedImage>,
    pub original: Option<Arc<[u8]>>,
}

pub enum ArtworkLoad {
    Ready(LoadedArtwork),
    Missing,
    Pending(PendingArtwork),
}

#[derive(Clone, Debug)]
pub enum ArtworkOutcome {
    Ready(LoadedArtwork),
    Missing,
    Failed(Arc<str>),
    Invalidated,
}

pub struct PendingArtwork {
    job: ArtworkKey,
    request_id: RequestId,
    completion: Option<oneshot::Receiver<pipeline::Resolution>>,
    pipeline: Arc<pipeline::Pipeline>,
}

impl PendingArtwork {
    pub async fn finish(self) -> ArtworkOutcome {
        match self.finish_resolution().await {
            pipeline::Resolution::Ready {
                image, original, ..
            } => ArtworkOutcome::Ready(LoadedArtwork { image, original }),
            pipeline::Resolution::Missing => ArtworkOutcome::Missing,
            pipeline::Resolution::Failed(error) => ArtworkOutcome::Failed(error),
            pipeline::Resolution::Invalidated => ArtworkOutcome::Invalidated,
            pipeline::Resolution::Cached | pipeline::Resolution::Fetched => {
                unreachable!("foreground requests finish after decoding")
            }
        }
    }

    async fn finish_resolution(mut self) -> pipeline::Resolution {
        let completion = self.completion.take().expect("pending artwork receiver");
        completion.await.unwrap_or_else(|_| {
            pipeline::Resolution::Failed("artwork request ended unexpectedly".into())
        })
    }
}

impl Drop for PendingArtwork {
    fn drop(&mut self) {
        self.pipeline.cancel(&self.job, self.request_id);
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ArtworkPreparation {
    pub total: usize,
    pub ready: usize,
    pub cached: usize,
    pub missing: usize,
    pub failed: usize,
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
    #[error("artwork source operation failed: {0}")]
    Source(#[from] sources::SourceError),
}

#[derive(Clone)]
pub struct Artwork {
    pipeline: Arc<pipeline::Pipeline>,
    source_resolver: Arc<Mutex<Option<Arc<SourceResolver>>>>,
}

pub struct SourceManifest {
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
    pub fn begin_source_manifest(
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

    pub fn key(&self, request: &ArtworkRequest) -> ArtworkKey {
        self.pipeline.key(request)
    }

    pub fn load(&self, request: ArtworkRequest) -> ArtworkLoad {
        self.pipeline.load(request)
    }

    pub fn source_preparation_complete(
        &self,
        source_id: &SourceId,
        revision: u64,
    ) -> Result<bool, ArtworkError> {
        self.pipeline
            .source_preparation_complete(source_id, revision)
    }

    pub async fn prefetch_source_artwork(
        &self,
        bindings: &[Vec<u8>],
        cancelled: &CancellationToken,
    ) -> Result<ArtworkPreparation, ArtworkError> {
        self.pipeline
            .prefetch_source_artwork(bindings, cancelled)
            .await
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

#[cfg(test)]
mod preparation_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn refreshed_artwork_keeps_its_asset_but_requests_the_new_revision() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let artwork = Artwork::new(directory.path(), runtime.handle().clone()).unwrap();
        let key = |encoded: Vec<u8>| {
            artwork.key(&ArtworkRequest::new(
                ArtworkBinding::opaque(&encoded),
                128,
                64,
            ))
        };
        let native = |source: &str, image: &str, revision: &str| {
            sources::native_artwork_binding(
                source,
                &sources::NativeImageRef::new(image, Some(revision.into())),
            )
            .unwrap()
        };
        let first = key(native("source", "cover", "one"));
        let revised = key(native("source", "cover", "two"));
        assert_ne!(first, revised);
        assert!(first.same_asset(&revised));
        assert!(!first.same_asset(&key(native("source", "other-cover", "one"))));
        assert!(!first.same_asset(&key(native("other-source", "cover", "one"))));

        let local = |source: &str, revision: &str, picture_index: Option<u32>| {
            let source_id = SourceId::new(source);
            let path = directory
                .path()
                .join("track.flac")
                .to_string_lossy()
                .into_owned();
            let revision = revision.to_owned();
            serde_json::to_vec(&match picture_index {
                Some(picture_index) => sources::LocalImageRef::Embedded {
                    source_id,
                    path,
                    revision,
                    picture_index,
                },
                None => sources::LocalImageRef::File {
                    source_id,
                    path,
                    revision,
                },
            })
            .unwrap()
        };
        for picture_index in [None, Some(0)] {
            let first = key(local("source", "one", picture_index));
            let revised = key(local("source", "two", picture_index));
            assert_ne!(first, revised);
            assert!(first.same_asset(&revised));
            assert!(!first.same_asset(&key(local("other-source", "one", picture_index))));
            assert!(!first.same_asset(&key(local("source", "one", Some(1)))));
        }

        artwork.invalidate_source(&SourceId::new("source")).unwrap();
        assert!(!first.same_asset(&key(native("source", "cover", "one"))));
    }

    #[test]
    fn identifying_a_binding_does_not_construct_its_source() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let artwork =
            Artwork::new(directory.path().join("covers"), runtime.handle().clone()).unwrap();
        let encoded =
            sources::native_artwork_binding("source", &sources::NativeImageRef::new("cover", None))
                .unwrap();
        let request = ArtworkRequest::new(ArtworkBinding::opaque(&encoded), 128, 64);
        let identity_without_resolver = artwork.key(&request);
        let resolutions = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&resolutions);
        artwork.install_source_resolver(move |_| {
            observed.fetch_add(1, Ordering::Relaxed);
            None
        });

        let identity_with_resolver = artwork.key(&request);
        let _cache_only = artwork.key(&request.clone().cache_only());
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
            .write_ready(
                binding.candidate().unwrap(),
                ImageSize::Thumbnail(256),
                png.get_ref(),
            )
            .unwrap();
        let resolutions = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&resolutions);
        artwork.install_source_resolver(move |_| {
            observed.fetch_add(1, Ordering::Relaxed);
            None
        });

        let request = ArtworkRequest::new(binding.clone(), 256, 128);
        let key = artwork.key(&request);
        let ArtworkLoad::Pending(pending) = artwork.load(request.clone()) else {
            panic!("disk cache requires worker decoding");
        };
        let ArtworkOutcome::Ready(image) = pending.finish().await else {
            panic!("cached image must decode");
        };
        assert_eq!(image.image.key(), &key);
        let ArtworkLoad::Ready(smaller) =
            artwork.load(ArtworkRequest::new(binding.clone(), 96, 64))
        else {
            panic!("live pixels must be reused");
        };
        assert!(Arc::ptr_eq(&smaller.image, &image.image));
        assert_eq!(resolutions.load(Ordering::Relaxed), 0);

        artwork.invalidate_source(&SourceId::new("source")).unwrap();
        let invalidated = ArtworkRequest::new(binding, 256, 128);
        assert_ne!(artwork.key(&invalidated), key);
        assert!(matches!(artwork.load(invalidated), ArtworkLoad::Pending(_)));
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
        let request = ArtworkRequest::new(ArtworkBinding::opaque(&encoded), 128, 64);
        assert_eq!(resolutions.load(Ordering::Relaxed), 0);

        let ArtworkLoad::Pending(pending) = artwork.load(request.clone()) else {
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
        let request = ArtworkRequest::new(ArtworkBinding::opaque(&encoded), 128, 64).cache_only();

        let ArtworkLoad::Pending(pending) = artwork.load(request.clone()) else {
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
        let key = artwork.key(&request);

        let ArtworkLoad::Pending(pending) = artwork.load(request.clone()) else {
            panic!("uncached Local artwork should enter the worker");
        };
        let ArtworkOutcome::Ready(decoded) = pending.finish().await else {
            panic!("Local image must decode");
        };
        let ArtworkLoad::Ready(reused) = artwork.load(request.clone()) else {
            panic!("live pixels must be reused");
        };
        assert!(Arc::ptr_eq(&reused.image, &decoded.image));
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
        assert_ne!(artwork.key(&request), key);
        assert!(
            matches!(artwork.load(request), ArtworkLoad::Pending(_)),
            "Forget must remove even a still-live decoded binding"
        );
        assert_eq!(resolutions.load(Ordering::Relaxed), 0);
    }
}
