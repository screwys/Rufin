//! Selects, fetches, caches, and decodes artwork.
//!
//! The caller owns final decoded results. This crate chooses the image source,
//! avoids duplicate work, prioritizes requests, and caches originals and thumbnails.

use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::{future::Future, pin::Pin};

use md5_digest::{Digest, Md5};
use sources::{Source, SourceId};
use thiserror::Error;
use tokio::runtime::Handle;
use tokio::sync::oneshot;

mod animation;
mod cache;
mod decode;
mod fetch;
mod pipeline;
mod selection;

pub use animation::{Animation, AnimationFrame};
pub use cache::{ArtworkCacheEntries, ArtworkCacheEntry};
pub use decode::{
    DecodedImage, RgbaImage, collage_png, decode_rgba, image_mime, square_thumbnail_png,
};
pub use selection::ArtworkBinding;
pub use sources::ImageSize;

pub(crate) type SourceResolver = dyn Fn(&SourceId) -> Option<Arc<Source>> + Send + Sync;
pub(crate) type CacheResolver = dyn Fn(
        CacheImageRequest,
    ) -> Pin<Box<dyn Future<Output = Option<(String, tempfile::TempPath)>> + Send>>
    + Send
    + Sync;

#[derive(Clone, Debug)]
pub struct CacheImageRequest {
    pub family: String,
    pub size: ImageSize,
    pub fallback: bool,
}

impl CacheImageRequest {
    pub fn select_key<'a>(&self, keys: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
        keys.into_iter()
            .filter_map(|key| {
                if !cache::valid_cache_key(key) {
                    return None;
                }
                let (state, rest) = key.split_once('/')?;
                let (family, file) = rest.rsplit_once('/')?;
                if family != self.family {
                    return None;
                }
                let rank = if state == "originals" {
                    (
                        u8::from(self.size != ImageSize::Original && !self.fallback),
                        0,
                    )
                } else {
                    let size = cache::thumbnail_size(file)?;
                    if self.fallback {
                        (1, u32::MAX - size)
                    } else if let ImageSize::Thumbnail(requested) = self.size
                        && size >= requested
                    {
                        (0, size)
                    } else {
                        return None;
                    }
                };
                Some((rank, key))
            })
            .min_by_key(|(rank, _)| *rank)
            .map(|(_, key)| key)
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
        binding: &ArtworkBinding,
        sizes: (ImageSize, u32),
        external: Option<&ExternalPolicy>,
        allow_fetch: bool,
        epochs: (u64, u64),
    ) -> Self {
        let policy = external
            .map(|policy| format!("{policy:?}\0{}", epochs.1))
            .unwrap_or_default();
        Self {
            asset: Self::binding_digest(binding.asset_identity()),
            binding: Self::binding_digest(binding.stable_identity()),
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
        format!("{:x}", Md5::digest(identity.as_bytes()))
    }

    /// The same revision and source policy, independent of display size.
    pub fn same_image(&self, other: &Self) -> bool {
        self.binding == other.binding && self.variant == other.variant
    }

    /// Whether an existing image can stay visible while this request refreshes it.
    pub fn same_asset(&self, other: &Self) -> bool {
        self.asset == other.asset && self.variant == other.variant
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
    job: ArtworkKey,
    request_id: RequestId,
    completion: Option<oneshot::Receiver<pipeline::Resolution>>,
    pipeline: Arc<pipeline::Pipeline>,
}

impl PendingArtwork {
    pub async fn finish(self) -> ArtworkOutcome {
        match self.finish_resolution().await {
            pipeline::Resolution::Ready { image, .. } => ArtworkOutcome::Ready(image),
            pipeline::Resolution::Missing => ArtworkOutcome::Missing,
            pipeline::Resolution::Failed(error) => ArtworkOutcome::Failed(error),
            pipeline::Resolution::Invalidated => ArtworkOutcome::Invalidated,
            pipeline::Resolution::Cached => {
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

#[derive(Clone, Debug)]
pub struct ArtworkPreview {
    pub image: RgbaImage,
    pub png: Arc<Vec<u8>>,
}

#[derive(Debug, Error)]
pub enum ArtworkError {
    #[error("artwork cache failed: {0}")]
    Cache(#[from] std::io::Error),
    #[error("artwork decode failed: {0}")]
    Decode(String),
    #[error("artwork fetch setup failed: {0}")]
    FetchSetup(String),
    #[error("artwork request was cancelled")]
    Cancelled,
    #[error("artwork source operation failed: {0}")]
    Source(#[from] sources::SourceError),
    #[error("artwork retention failed: {0}")]
    Library(#[from] library::LibraryError),
}

#[derive(Clone)]
pub struct Artwork {
    pipeline: Arc<pipeline::Pipeline>,
    source_resolver: Arc<Mutex<Option<Arc<SourceResolver>>>>,
}

impl Artwork {
    pub fn preview(
        &self,
        binding: ArtworkBinding,
        allow_cached_external: bool,
    ) -> Option<Arc<ArtworkPreview>> {
        self.pipeline.preview(&binding, allow_cached_external)
    }

    pub async fn clear_cache(
        &self,
        database: &library::Database,
        active_media_uris: &[String],
    ) -> Result<(), ArtworkError> {
        let mut retention = database.artwork_cache_retention(active_media_uris).await?;
        loop {
            let bindings = retention.next_bindings().await?;
            if bindings.is_empty() {
                break;
            }
            let families: Vec<_> = bindings
                .iter()
                .flat_map(|binding| {
                    ArtworkBinding::opaque(binding)
                        .candidates
                        .into_iter()
                        .map(|candidate| cache::FilesystemCache::candidate_family(&candidate))
                })
                .collect();
            for page in families.chunks(128) {
                retention.retain(page).await?;
            }
        }
        let cache = self.pipeline.cache().clone();
        let mut entries = tokio::task::spawn_blocking(move || cache.entries())
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))??;
        loop {
            let (next, page) = tokio::task::spawn_blocking(move || {
                let page = entries
                    .by_ref()
                    .take(128)
                    .collect::<std::io::Result<Vec<_>>>();
                (entries, page)
            })
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))?;
            entries = next;
            let page = page?;
            if page.is_empty() {
                break;
            }
            let families: Vec<_> = page.iter().map(|entry| entry.family().to_owned()).collect();
            let remove = retention.unretained(&families).await?;
            let discarded: Vec<_> = page
                .into_iter()
                .zip(remove)
                .filter_map(|(entry, remove)| remove.then_some(entry))
                .collect();
            let pipeline = self.pipeline.clone();
            tokio::task::spawn_blocking(move || pipeline.remove_cache_entries(&discarded))
                .await
                .map_err(|error| std::io::Error::other(error.to_string()))??;
        }
        retention.finish().await?;
        let pipeline = self.pipeline.clone();
        tokio::task::spawn_blocking(move || pipeline.finish_cache_clear())
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))?
    }

    /// Keys are relative to the existing v1 cache layout, never filesystem paths.
    pub fn cache_file(&self, key: &str) -> Result<Option<PathBuf>, ArtworkError> {
        Ok(self.pipeline.cache().file(key)?)
    }

    pub fn cached_family_file(
        &self,
        request: &CacheImageRequest,
    ) -> Result<Option<ArtworkCacheEntry>, ArtworkError> {
        Ok(self.pipeline.cache().family_file(request)?)
    }

    /// Import existing bytes without fetching, decoding or announcing another local write.
    pub fn import_missing_cache_file(&self, key: &str, path: &Path) -> Result<bool, ArtworkError> {
        self.pipeline.import_missing_cache_file(key, path)
    }

    /// Refresh live images and jobs after a changed import batch, preserving files on disk.
    pub fn invalidate_decoded_cache(&self) {
        self.pipeline.invalidate_decoded_cache();
    }

    pub fn cache_revision(&self) -> u64 {
        self.pipeline.cache_revision()
    }

    pub async fn install_database(
        &self,
        database: Arc<library::Database>,
    ) -> Result<(), ArtworkError> {
        self.pipeline.install_database(Arc::clone(&database));
        let legacy = self.pipeline.cache().preview_index_path();
        if tokio::fs::try_exists(&legacy).await? {
            database.import_artwork_previews(&legacy).await?;
            tokio::fs::remove_file(legacy).await?;
        }
        Ok(())
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

    pub fn install_cache_resolver<F>(
        &self,
        resolver: impl Fn(CacheImageRequest) -> F + Send + Sync + 'static,
    ) where
        F: Future<Output = Option<(String, tempfile::TempPath)>> + Send + 'static,
    {
        self.pipeline
            .install_cache_resolver(Arc::new(move |keys| Box::pin(resolver(keys))));
    }

    pub fn key(&self, request: &ArtworkRequest) -> ArtworkKey {
        self.pipeline.key(request)
    }

    pub fn load(&self, request: ArtworkRequest) -> ArtworkLoad {
        self.pipeline.load(request)
    }

    pub async fn cache_source(&self, binding: &[u8]) -> Result<bool, ArtworkError> {
        self.pipeline
            .cache_source(ArtworkBinding::opaque(binding))
            .await
    }

    pub fn cache_only_file(&self, request: &ArtworkRequest) -> Option<PathBuf> {
        self.pipeline
            .cache_only_file(request, &[request.fetch_size])
    }

    /// Read existing artwork for a binding without fetching or selecting a network policy.
    /// Disk access and decoding are synchronous; callers should use a worker thread.
    pub fn cached_image(
        &self,
        binding: &ArtworkBinding,
        render_size: u32,
    ) -> Result<Option<RgbaImage>, ArtworkError> {
        self.pipeline
            .cached_binding_file(binding, render_size)
            .map(|path| {
                decode_rgba(
                    &std::fs::read(path).map_err(ArtworkError::Cache)?,
                    render_size,
                )
            })
            .transpose()
    }

    /// Reuse cached image bytes before asking the source for the requested size.
    pub async fn image_bytes(&self, request: ArtworkRequest) -> Result<Option<Vec<u8>>, String> {
        let pipeline = self.pipeline.clone();
        let cached_request = request.clone();
        let cached = tokio::task::spawn_blocking(move || {
            pipeline
                .cache_only_file(
                    &cached_request,
                    &[cached_request.fetch_size, ImageSize::Original],
                )
                .map(std::fs::read)
                .transpose()
        })
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
        if cached.is_some() {
            return Ok(cached);
        }
        let mut request = request;
        if let ImageSize::Thumbnail(size) = request.fetch_size {
            request.render_size = request.render_size.max(size);
        }
        let loaded = match self.load(request.clone()) {
            ArtworkLoad::Ready(loaded) => loaded,
            ArtworkLoad::Missing => return Ok(None),
            ArtworkLoad::Pending(pending) => match pending.finish().await {
                ArtworkOutcome::Ready(loaded) => loaded,
                ArtworkOutcome::Missing | ArtworkOutcome::Invalidated => return Ok(None),
                ArtworkOutcome::Failed(error) => return Err(error.to_string()),
            },
        };
        if let Some(bytes) = loaded.original() {
            return Ok(Some(bytes.to_vec()));
        }
        tokio::task::spawn_blocking(move || loaded.pixels.encode_png().map(Some))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())
    }

    pub fn retry_external(&self) -> Result<(), ArtworkError> {
        self.pipeline.retry_external()
    }

    pub fn invalidate_source(&self, source_id: &SourceId) -> Result<(), ArtworkError> {
        self.pipeline.invalidate_source(source_id)
    }

    pub fn invalidate_image(&self, binding: &ArtworkBinding) -> Result<bool, ArtworkError> {
        self.pipeline.invalidate_image(binding)
    }
}

#[cfg(test)]
mod tests {
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
            match picture_index {
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
            }
            .artwork_binding()
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
        let mut native = sources::NativeImageRef::new("cover", None);
        native.blur_hash = Some("LEHV6nWB2yk8pyo0adR*.7kCMdnj".into());
        let prepared = sources::native_artwork_binding("source", &native).unwrap();
        let binding = ArtworkBinding::opaque(&prepared);
        assert_eq!(
            artwork.key(&ArtworkRequest::new(binding.clone(), 128, 64)),
            identity_without_resolver
        );
        let preview = artwork.preview(binding.clone(), false).unwrap();
        assert!(Arc::ptr_eq(
            &preview,
            &artwork.preview(binding, false).unwrap()
        ));
        assert_eq!(&preview.png[..8], b"\x89PNG\r\n\x1a\n");
        assert!(Arc::ptr_eq(
            &preview,
            &artwork
                .preview(ArtworkBinding::opaque(&encoded), false)
                .unwrap()
        ));
        native.blur_hash = Some(blurhash::encode(4, 3, 1, 1, &[0, 0, 255, 255]).unwrap());
        let changed =
            ArtworkBinding::opaque(&sources::native_artwork_binding("source", &native).unwrap());
        let changed_preview = artwork.preview(changed, false).unwrap();
        assert!(!Arc::ptr_eq(&preview, &changed_preview));
        assert!(Arc::ptr_eq(
            &changed_preview,
            &artwork
                .preview(ArtworkBinding::opaque(&encoded), false)
                .unwrap()
        ));
        let new_revision = sources::native_artwork_binding(
            "source",
            &sources::NativeImageRef::new("cover", Some("new".into())),
        )
        .unwrap();
        assert!(
            artwork
                .preview(ArtworkBinding::opaque(&new_revision), false)
                .is_none()
        );
        assert_eq!(resolutions.load(Ordering::Relaxed), 0);
        assert!(!directory.path().join("covers/v1/previews.sqlite3").exists());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cached_native_artwork_reuses_pixels_without_resolving_a_provider() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("covers");
        let artwork = Artwork::new(&root, Handle::current()).unwrap();
        let database = library::Database::open(directory.path().join("library.sqlite3"))
            .await
            .unwrap();
        artwork
            .install_database(Arc::new(database.clone()))
            .await
            .unwrap();
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
                binding.candidates.first().unwrap(),
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

        let mut fallback = vec![library::ArtworkCandidate::new(
            library::ArtworkImage::Artist {
                name: "Artist without a cached portrait".into(),
                musicbrainz_id: None,
            },
            None,
        )];
        fallback.extend(binding.candidates.clone());
        let fallback_bytes = serde_json::to_vec(&fallback).unwrap();
        assert!(
            artwork
                .preview(ArtworkBinding::opaque(&fallback_bytes), true)
                .is_none()
        );
        assert_eq!(resolutions.load(Ordering::Relaxed), 0);

        let request = ArtworkRequest::new(binding.clone(), 256, 128);
        let key = artwork.key(&request);
        let ArtworkLoad::Pending(pending) = artwork.load(request.clone()) else {
            panic!("disk cache requires worker decoding");
        };
        let ArtworkOutcome::Ready(image) = pending.finish().await else {
            panic!("cached image must decode");
        };
        assert_eq!(image.key(), &key);
        let ArtworkLoad::Ready(smaller) =
            artwork.load(ArtworkRequest::new(binding.clone(), 96, 64))
        else {
            panic!("live pixels must be reused");
        };
        assert!(Arc::ptr_eq(&smaller, &image));
        assert_eq!(resolutions.load(Ordering::Relaxed), 0);
        assert_eq!(
            artwork.key(&ArtworkRequest::new(binding.clone(), 256, 128)),
            key
        );
        let preview = artwork.preview(binding.clone(), false).unwrap();
        assert_eq!((preview.image.width(), preview.image.height()), (32, 32));
        assert!(Arc::ptr_eq(
            &preview,
            &artwork.preview(binding.clone(), false).unwrap()
        ));
        fallback[0].blur_hash = Some("LEHV6nWB2yk8pyo0adR*.7kCMdnj".into());
        let fallback = ArtworkBinding::opaque(&serde_json::to_vec(&fallback).unwrap());
        let external_preview = artwork.preview(fallback.clone(), true).unwrap();
        let source_preview = artwork.preview(fallback, false).unwrap();
        assert_ne!(external_preview.image.rgba(), source_preview.image.rgba());
        assert_eq!(resolutions.load(Ordering::Relaxed), 0);
        assert!(
            artwork
                .image_bytes(ArtworkRequest::new(binding.clone(), 1024, 512))
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(
            resolutions.load(Ordering::Relaxed),
            1,
            "a small cached cover is a fallback after the larger request fails"
        );

        database
            .reconcile_source(&library::SourceId::new("source"), "server", "Source")
            .await
            .unwrap();
        database
            .catalog("source", None)
            .await
            .unwrap()
            .apply_catalog(
                &library::CatalogBatch {
                    tracks: vec![library::TrackObservation {
                        object_id: "track".into(),
                        artwork_binding: Some(encoded.clone()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                true,
            )
            .await
            .unwrap();
        let uri = library::source_entity_uri(&SourceId::new("source"), "track", "track");
        let before = database
            .artwork_bindings(
                std::slice::from_ref(&uri),
                &library::ReadCancellation::new(),
            )
            .await
            .unwrap()
            .pop()
            .unwrap()
            .unwrap();
        assert!(
            ArtworkBinding::opaque(&before).candidates[0]
                .blur_hash
                .is_some()
        );
        let (updates, mut received) = tokio::sync::mpsc::unbounded_channel();
        database.observe_catalog(move |_, change| {
            let _ = updates.send(change);
            std::future::ready(())
        });
        let update = {
            let mut catalog = database.catalog("source", None).await.unwrap();
            catalog
                .apply_catalog(
                    &library::CatalogBatch {
                        tracks: vec![library::TrackObservation {
                            object_id: "track".into(),
                            artwork_binding: Some(encoded.clone()),
                            ..Default::default()
                        }],
                        ..Default::default()
                    },
                    true,
                )
                .await
                .unwrap();
            catalog.into_changes()
        };
        assert!(
            !update.changed(),
            "unchanged artwork must not invalidate catalog rows: {update:?}"
        );
        database.publish_catalog_update("source", update).await;
        assert!(received.try_recv().is_err());
        assert_eq!(
            database
                .artwork_bindings(
                    std::slice::from_ref(&uri),
                    &library::ReadCancellation::new()
                )
                .await
                .unwrap()[0]
                .as_ref(),
            Some(&before)
        );
        let candidate = binding.candidates.first().unwrap();
        let large = cache
            .write_ready(candidate, ImageSize::Thumbnail(731), png.get_ref())
            .unwrap();
        let original = cache
            .write_ready(candidate, ImageSize::Original, png.get_ref())
            .unwrap();
        let ordinary = ArtworkBinding::opaque(
            &sources::native_artwork_binding(
                "source",
                &sources::NativeImageRef::new("ordinary", None),
            )
            .unwrap(),
        );
        let discarded = cache
            .write_ready(
                &ordinary.candidates[0],
                ImageSize::Thumbnail(512),
                png.get_ref(),
            )
            .unwrap();
        cache
            .mark_missing(candidate, ImageSize::Thumbnail(1024))
            .unwrap();
        artwork
            .clear_cache(
                &database,
                &[library::source_entity_uri(
                    &SourceId::new("source"),
                    "track",
                    "track",
                )],
            )
            .await
            .unwrap();
        assert!(large.is_file());
        assert!(original.is_file());
        assert!(!discarded.exists());
        assert!(!cache.is_missing(candidate, ImageSize::Thumbnail(1024)));
        assert_eq!(artwork.key(&request), key);
        artwork.clear_cache(&database, &[]).await.unwrap();
        assert!(!large.exists());
        assert!(!original.exists());
        let reopened = Artwork::new(&root, Handle::current()).unwrap();
        reopened
            .install_database(Arc::new(database.clone()))
            .await
            .unwrap();
        let restored = database
            .artwork_bindings(
                &[library::source_entity_uri(
                    &SourceId::new("source"),
                    "track",
                    "track",
                )],
                &library::ReadCancellation::new(),
            )
            .await
            .unwrap()
            .pop()
            .unwrap()
            .unwrap();
        assert_eq!(
            reopened
                .preview(ArtworkBinding::opaque(&restored), false)
                .unwrap()
                .image
                .rgba(),
            preview.image.rgba()
        );

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
        let mut native = sources::NativeImageRef::new("cover", None);
        native.blur_hash = Some("LEHV6nWB2yk8pyo0adR*.7kCMdnj".into());
        let encoded = sources::native_artwork_binding("source", &native).unwrap();
        assert!(
            artwork
                .preview(ArtworkBinding::opaque(&encoded), false)
                .is_some()
        );
        assert_eq!(resolutions.load(Ordering::Relaxed), 0);
        assert!(!directory.path().join("covers/v1/previews.sqlite3").exists());
        let request = ArtworkRequest::new(ArtworkBinding::opaque(&encoded), 128, 64);
        assert_eq!(resolutions.load(Ordering::Relaxed), 0);

        let ArtworkLoad::Pending(pending) = artwork.load(request.clone()) else {
            panic!("uncached native artwork should enter the worker");
        };
        assert!(matches!(pending.finish().await, ArtworkOutcome::Failed(_)));
        assert_eq!(resolutions.load(Ordering::Relaxed), 1);
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(8, 8)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        artwork.install_cache_resolver(move |request| {
            let bytes = png.get_ref().clone();
            async move {
                if !request.fallback {
                    return None;
                }
                let file = tempfile::NamedTempFile::new().unwrap();
                std::fs::write(file.path(), bytes).unwrap();
                Some((
                    format!("ready/{}/512.png", request.family),
                    file.into_temp_path(),
                ))
            }
        });
        let ArtworkLoad::Pending(pending) =
            artwork.load(ArtworkRequest::original(request.binding, 1024))
        else {
            panic!("uncached original must request artwork");
        };
        assert!(
            matches!(pending.finish().await, ArtworkOutcome::Ready(_)),
            "offline original keeps an available peer thumbnail"
        );
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
        let encoded = sources::LocalImageRef::File {
            source_id: SourceId::new("configured-local"),
            path: image.to_string_lossy().into_owned(),
            revision: "fixture".to_string(),
        }
        .artwork_binding();
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
        assert!(Arc::ptr_eq(&reused, &decoded));
        let cached = artwork.cache_only_file(&request).unwrap();
        assert!(cached.is_file());
        let prepared = encoded.clone();
        let preview = artwork
            .preview(ArtworkBinding::opaque(&prepared), false)
            .unwrap();
        let mut updated = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(1, 1, image::Rgb([0, 0, 255])))
            .write_to(&mut updated, image::ImageFormat::Png)
            .unwrap();
        std::fs::write(&image, updated.into_inner()).unwrap();
        artwork.invalidate_image(&request.binding).unwrap();
        let ArtworkLoad::Pending(pending) = artwork.load(request.clone()) else {
            panic!("changed image must be read again");
        };
        assert!(matches!(pending.finish().await, ArtworkOutcome::Ready(_)));

        assert_ne!(
            artwork
                .preview(ArtworkBinding::opaque(&prepared), false)
                .unwrap()
                .image
                .rgba(),
            preview.image.rgba()
        );
        assert!(artwork.cache_source(&encoded).await.unwrap());
        std::fs::remove_file(&image).unwrap();
        let offline = ArtworkRequest::new(ArtworkBinding::opaque(&encoded), 512, 512).cache_only();
        assert!(artwork.cache_only_file(&offline).is_some());
        let ArtworkLoad::Pending(pending) = artwork.load(offline) else {
            panic!("offline artwork must read the downloaded rendition");
        };
        assert!(matches!(pending.finish().await, ArtworkOutcome::Ready(_)));

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
