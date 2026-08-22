use std::fs;
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use library::{
    Album, AlbumArtwork, AlbumArtworkFacts, AlbumId, AlbumRelations, ImageRef, SourceArtwork,
    SourceId, Track, TrackData, TrackId, TrackRelations,
};
use sources::{ImageBytes, SourceError, SourceImageRequest, SourceResult};
use tempfile::TempDir;
use tokio::runtime::{Builder, Handle, Runtime};

use crate::{
    Artwork, ArtworkBinding, ArtworkLoad, ArtworkOutcome, ArtworkRequest, ExternalPolicy,
    PendingArtwork, SourceImages, TestImageSource,
};

struct StaticImages {
    calls: AtomicUsize,
    bytes: Vec<u8>,
}

#[test]
fn source_preparation_key_depends_only_on_accepted_artwork_bindings() {
    let temporary = TempDir::new().expect("temporary artwork cache");
    let images = Arc::new(StaticImages {
        calls: AtomicUsize::new(0),
        bytes: png_bytes(),
    });
    let source_id = SourceId::new("binding-key-source");
    let source = SourceImages::testing(source_id.clone(), images.clone());
    let artwork = Artwork::new(temporary.path(), runtime()).expect("artwork");
    let first = SourceArtwork::Native(ImageRef::new("cover:first", Some("v1".to_string())));
    let second = SourceArtwork::Native(ImageRef::new("cover:second", None));

    let accepted = artwork.source_preparation_key(&[first.clone(), second.clone()]);
    let reordered = artwork.source_preparation_key(&[second.clone(), first.clone()]);
    let changed = artwork.source_preparation_key(&[
        SourceArtwork::Native(ImageRef::new("cover:first", Some("v2".to_string()))),
        second.clone(),
    ]);

    artwork
        .prepare_source_artwork(
            source.clone(),
            accepted,
            Arc::new([first, second]),
            &|_, _| {},
            &|| false,
        )
        .expect("prepare accepted artwork bindings");

    assert_eq!(accepted, reordered);
    assert_ne!(accepted, changed);
    assert!(
        artwork
            .source_preparation_complete(&source_id, reordered)
            .expect("reuse reordered accepted bindings")
    );
    assert!(
        !artwork
            .source_preparation_complete(&source_id, changed)
            .expect("changed binding invalidates preparation")
    );
    let changed_facts: Arc<[SourceArtwork]> = Arc::new([
        SourceArtwork::Native(ImageRef::new("cover:first", Some("v2".to_string()))),
        SourceArtwork::Native(ImageRef::new("cover:second", None)),
    ]);
    artwork
        .prepare_source_artwork(
            source.clone(),
            changed,
            Arc::clone(&changed_facts),
            &|_, _| {},
            &|| false,
        )
        .expect("prepare changed artwork bindings");
    let repeated = artwork
        .prepare_source_artwork(source, changed, changed_facts, &|_, _| {}, &|| false)
        .expect("reuse changed artwork bindings");

    assert_eq!(repeated, Default::default());
    assert_eq!(images.calls.load(Ordering::Relaxed), 3);
}

#[async_trait(?Send)]
impl TestImageSource for StaticImages {
    async fn image(&self, _request: SourceImageRequest) -> SourceResult<ImageBytes> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(ImageBytes {
            bytes: self.bytes.clone(),
            content_type: Some("image/png".to_string()),
        })
    }
}

struct MissingImages {
    calls: AtomicUsize,
}

struct MixedImages {
    calls: AtomicUsize,
    bytes: Vec<u8>,
}

#[async_trait(?Send)]
impl TestImageSource for MixedImages {
    async fn image(&self, request: SourceImageRequest) -> SourceResult<ImageBytes> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if matches!(
            request,
            SourceImageRequest::Native { ref image_ref, .. }
                if image_ref.item_id == "missing-image"
        ) {
            return Err(SourceError::NotFound);
        }
        Ok(ImageBytes {
            bytes: self.bytes.clone(),
            content_type: Some("image/png".to_string()),
        })
    }
}

#[test]
fn album_binding_uses_the_album_image_before_its_track_fallback() {
    let album = album_with_image("album-image");
    let track = track_with_artwork(Arc::clone(&album), "track-image");

    let binding = ArtworkBinding::album_artwork(&AlbumArtwork {
        album,
        representative_track: Some(track),
    });
    let identities = binding
        .candidates()
        .iter()
        .map(|candidate| candidate.stable_identity())
        .collect::<Vec<_>>();

    assert!(identities[0].contains("album-image"));
    assert!(identities[1].contains("track-image"));
}

fn album_with_image(image_id: &str) -> Arc<Album> {
    Arc::new(Album {
        id: AlbumId::new("album-artwork-order"),
        title: "Album".to_string(),
        artist: "Artist".to_string(),
        year: 2024,
        release_date: None,
        date_added: None,
        last_played: None,
        play_count: None,
        user_rating: None,
        favorite: false,
        color_seed: 0,
        image_ref: Some(ImageRef::new(image_id, None)),
        local_artwork: None,
        release_types: Vec::new(),
        is_compilation: None,
        musicbrainz_album_id: None,
        musicbrainz_release_group_id: None,
        relations: AlbumRelations::default(),
    })
}

fn track_with_artwork(album: Arc<Album>, image_id: &str) -> Track {
    Track::new(TrackData {
        id: TrackId::new("track-artwork-order"),
        album_id: Some(album.id.clone()),
        title: "Track".to_string(),
        artist: "Artist".to_string(),
        album: "Album".to_string(),
        album_artwork: Some(Arc::new(AlbumArtworkFacts::from(album.as_ref()))),
        year: 2024,
        release_date: None,
        date_added: None,
        last_played: None,
        play_count: None,
        user_rating: None,
        duration_seconds: 180,
        favorite: false,
        disc_number: 1,
        track_number: 1,
        image_ref: Some(ImageRef::new(image_id, None)),
        local_artwork: None,
        musicbrainz_recording_id: None,
        musicbrainz_release_track_id: None,
        source_path: None,
        cue: None,
        source_format: None,
        comment: None,
        skip_count: None,
        bpm: None,
        relations: TrackRelations::default(),
    })
}

#[test]
fn source_preparation_populates_disk_without_decoded_residency() {
    let temporary = TempDir::new().expect("temporary artwork directory");
    let images = Arc::new(StaticImages {
        calls: AtomicUsize::new(0),
        bytes: png_bytes(),
    });
    let source_id = SourceId::new("source-preparation-disk");
    let source = SourceImages::testing(source_id.clone(), images.clone());
    let artwork = Artwork::new(temporary.path(), runtime()).expect("artwork service starts");
    let album = album_with_image("shared-album-image");
    let track = track_with_artwork(Arc::clone(&album), "per-track-alias");
    let source_artwork: Arc<[SourceArtwork]> = Arc::new([SourceArtwork::Native(
        album.image_ref.clone().expect("album image"),
    )]);

    let preparation = artwork
        .prefetch_source_artwork(source.clone(), source_artwork, &|_, _| {}, &|| false)
        .expect("prepare source artwork");
    assert_eq!(preparation.ready, 1);
    assert_eq!(images.calls.load(Ordering::Relaxed), 1);

    let album_request = ArtworkRequest::new(
        ArtworkBinding::album_artwork(&AlbumArtwork {
            album,
            representative_track: Some(track.clone()),
        }),
        256,
        256,
    );
    let track_request = ArtworkRequest::new(ArtworkBinding::track(&track), 256, 256);
    assert!(
        artwork
            .prepare(source.clone(), album_request.clone())
            .ready
            .is_none(),
        "source preparation must not retain decoded images"
    );
    assert!(
        artwork
            .cache_only_file(&source_id, &album_request)
            .is_some(),
        "source preparation must leave a ready disk entry"
    );

    let album_ready = match finish(
        prepare_and_request(&artwork, source.clone(), album_request)
            .expect("request prepared album artwork"),
    ) {
        ArtworkOutcome::Ready(image) => image,
        _ => panic!("prepared album artwork was not ready"),
    };
    let track_ready = artwork
        .prepare(source, track_request)
        .ready
        .expect("decoded album artwork satisfies the Track binding");
    assert!(Arc::ptr_eq(&album_ready, &track_ready));
    assert_eq!(images.calls.load(Ordering::Relaxed), 1);
}

#[test]
fn cached_fallback_does_not_bypass_an_available_album_primary() {
    let temporary = TempDir::new().expect("temporary artwork directory");
    let images = Arc::new(StaticImages {
        calls: AtomicUsize::new(0),
        bytes: png_bytes(),
    });
    let source = SourceImages::testing(SourceId::new("source-primary-order"), images.clone());
    let artwork = Artwork::new(temporary.path(), runtime()).expect("artwork service starts");
    let album = album_with_image("uncached-album-primary");
    let track = track_with_artwork(Arc::clone(&album), "cached-track-fallback");
    let fallback = SourceArtwork::Native(track.image_ref.clone().expect("Track image"));
    let fallback_artwork: Arc<[SourceArtwork]> = Arc::new([fallback]);

    artwork
        .prefetch_source_artwork(source.clone(), fallback_artwork, &|_, _| {}, &|| false)
        .expect("prepare fallback artwork");

    let track_request = ArtworkRequest::new(ArtworkBinding::track(&track), 256, 256);
    assert!(
        artwork
            .prepare(source.clone(), track_request.clone())
            .ready
            .is_none()
    );
    let pending = prepare_and_request(&artwork, source, track_request)
        .expect("request album-primary Track artwork");
    wait_for_ready(pending);

    assert_eq!(images.calls.load(Ordering::Relaxed), 2);
}

#[test]
fn source_preparation_caches_ready_and_missing_images_without_a_second_fetch() {
    let temporary = TempDir::new().expect("temporary artwork directory");
    let images = Arc::new(MixedImages {
        calls: AtomicUsize::new(0),
        bytes: png_bytes(),
    });
    let source = SourceImages::testing(SourceId::new("source-preparation-cache"), images.clone());
    let artwork = Artwork::new(temporary.path(), runtime()).expect("artwork service starts");
    let facts: Arc<[SourceArtwork]> = Arc::new([
        SourceArtwork::Native(ImageRef::new("ready-image", None)),
        SourceArtwork::Native(ImageRef::new("missing-image", None)),
    ]);

    let first = artwork
        .prefetch_source_artwork(source.clone(), Arc::clone(&facts), &|_, _| {}, &|| false)
        .expect("prepare source artwork");
    let second = artwork
        .prefetch_source_artwork(source, facts, &|_, _| {}, &|| false)
        .expect("reuse prepared source artwork");

    assert_eq!(first.total, 2);
    assert_eq!(first.ready, 1);
    assert_eq!(first.missing, 1);
    assert_eq!(first.failed, 0);
    assert_eq!(second, first);
    assert_eq!(images.calls.load(Ordering::Relaxed), 2);
}

#[test]
fn cancelled_source_preparation_starts_no_image_work() {
    let temporary = TempDir::new().expect("temporary artwork directory");
    let images = Arc::new(StaticImages {
        calls: AtomicUsize::new(0),
        bytes: png_bytes(),
    });
    let source = SourceImages::testing(SourceId::new("source-preparation-cancel"), images.clone());
    let artwork = Artwork::new(temporary.path(), runtime()).expect("artwork service starts");
    let cancelled = AtomicBool::new(true);

    let result = artwork.prefetch_source_artwork(
        source,
        Arc::new([SourceArtwork::Native(ImageRef::new(
            "cancelled-image",
            None,
        ))]),
        &|_, _| {},
        &|| cancelled.load(Ordering::Acquire),
    );

    assert!(matches!(result, Err(crate::ArtworkError::Cancelled)));
    assert_eq!(images.calls.load(Ordering::Relaxed), 0);
}

#[async_trait(?Send)]
impl TestImageSource for MissingImages {
    async fn image(&self, _request: SourceImageRequest) -> SourceResult<ImageBytes> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Err(SourceError::NotFound)
    }
}

#[derive(Default)]
struct GateState {
    started: usize,
    released: bool,
    finished: bool,
}

#[derive(Default)]
struct BlockingImages {
    state: Mutex<GateState>,
    changed: Condvar,
}

impl BlockingImages {
    fn wait_started(&self) {
        self.wait_started_count(1);
    }

    fn wait_started_count(&self, count: usize) {
        self.wait_for(|state| state.started >= count);
    }

    fn started_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .started
    }

    fn release(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.released = true;
        self.changed.notify_all();
    }

    fn wait_finished(&self) {
        self.wait_for(|state| state.finished);
    }

    fn wait_for(&self, condition: impl Fn(&GateState) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !condition(&state) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "artwork worker did not reach gate");
            state = self
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }
}

#[async_trait(?Send)]
impl TestImageSource for BlockingImages {
    async fn image(&self, _request: SourceImageRequest) -> SourceResult<ImageBytes> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.started += 1;
        self.changed.notify_all();
        while !state.released {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        state.finished = true;
        self.changed.notify_all();
        Ok(ImageBytes {
            bytes: png_bytes(),
            content_type: Some("image/png".to_string()),
        })
    }
}

#[test]
fn source_preparation_streams_every_image_through_a_bounded_window() {
    let temporary = TempDir::new().expect("temporary artwork directory");
    let images = Arc::new(BlockingImages::default());
    let source = SourceImages::testing(SourceId::new("bounded-source-preparation"), images.clone());
    let total = super::pipeline::PREPARATION_WINDOW * 3 + 1;
    let facts: Arc<[SourceArtwork]> = (0..total)
        .map(|index| SourceArtwork::Native(ImageRef::new(format!("image-{index}"), None)))
        .collect::<Vec<_>>()
        .into();
    let progress = Arc::new(Mutex::new(Vec::new()));
    let artwork = Artwork::new(temporary.path(), runtime()).expect("artwork service starts");
    let worker_artwork = artwork.clone();
    let worker_progress = Arc::clone(&progress);
    let preparation = thread::spawn(move || {
        worker_artwork.prefetch_source_artwork(
            source,
            facts,
            &|completed, total| {
                worker_progress
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push((completed, total));
            },
            &|| false,
        )
    });

    images.wait_started_count(super::pipeline::PREPARATION_WORKERS);
    let (preparation_interests, jobs) = artwork.pipeline.preparation_work();
    assert_eq!(preparation_interests, super::pipeline::PREPARATION_WINDOW);
    assert!(jobs <= super::pipeline::PREPARATION_WINDOW);
    images.release();

    let summary = preparation
        .join()
        .expect("preparation thread")
        .expect("source preparation");
    assert_eq!(summary.total, total);
    assert_eq!(summary.ready, total);
    assert_eq!(images.started_count(), total);
    let progress = progress
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(progress.len(), total);
    assert_eq!(progress.first(), Some(&(1, total)));
    assert_eq!(progress.last(), Some(&(total, total)));
}

#[test]
fn cancelling_source_preparation_discards_the_unstarted_window() {
    let temporary = TempDir::new().expect("temporary artwork directory");
    let images = Arc::new(BlockingImages::default());
    let source = SourceImages::testing(
        SourceId::new("cancelled-source-preparation"),
        images.clone(),
    );
    let facts: Arc<[SourceArtwork]> = (0..super::pipeline::PREPARATION_WINDOW * 3)
        .map(|index| SourceArtwork::Native(ImageRef::new(format!("image-{index}"), None)))
        .collect::<Vec<_>>()
        .into();
    let cancelled = Arc::new(AtomicBool::new(false));
    let artwork = Artwork::new(temporary.path(), runtime()).expect("artwork service starts");
    let worker_artwork = artwork.clone();
    let worker_cancelled = Arc::clone(&cancelled);
    let preparation = thread::spawn(move || {
        worker_artwork.prefetch_source_artwork(source, facts, &|_, _| {}, &|| {
            worker_cancelled.load(Ordering::Acquire)
        })
    });

    images.wait_started_count(super::pipeline::PREPARATION_WORKERS);
    cancelled.store(true, Ordering::Release);
    let result = preparation.join().expect("preparation thread");
    assert!(matches!(result, Err(crate::ArtworkError::Cancelled)));
    let (preparation_interests, jobs) = artwork.pipeline.preparation_work();
    assert_eq!(preparation_interests, 0);
    assert!(jobs <= super::pipeline::PREPARATION_WORKERS);
    assert_eq!(images.started_count(), super::pipeline::PREPARATION_WORKERS);
    images.release();

    let deadline = Instant::now() + Duration::from_secs(3);
    while artwork.pipeline.preparation_work().1 != 0 {
        assert!(
            Instant::now() < deadline,
            "cancelled artwork jobs did not finish"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn duplicate_visible_leases_share_one_fetch() {
    let temporary = TempDir::new().expect("temporary artwork directory");
    let runtime = runtime();
    let images = Arc::new(BlockingImages::default());
    let source_id = SourceId::new("source-one");
    let source = SourceImages::testing(source_id.clone(), images.clone());
    let request = request("cover-one");
    let artwork = Artwork::new(temporary.path(), runtime).expect("artwork service starts");

    let first = artwork
        .request_prepared(artwork.prepare(source.clone(), request.clone()))
        .expect("first request");
    images.wait_started();
    let second = artwork
        .request_prepared(artwork.prepare(source, request.clone()))
        .expect("second request");
    images.release();

    wait_for_ready(first);
    wait_for_ready(second);
    assert_eq!(images.started_count(), 1);
    let cached = artwork
        .cache_only_file(&source_id, &request)
        .expect("cache-only file");
    assert!(cached.is_file());
}

#[test]
fn different_bindings_share_their_first_album_candidate() {
    let temporary = TempDir::new().expect("temporary artwork directory");
    let images = Arc::new(BlockingImages::default());
    let source = SourceImages::testing(
        SourceId::new("source-shared-album-candidate"),
        images.clone(),
    );
    let album = album_with_image("shared-album-candidate");
    let track = track_with_artwork(Arc::clone(&album), "track-fallback");
    let album_request = ArtworkRequest::new(ArtworkBinding::album(&album), 256, 256);
    let track_request = ArtworkRequest::new(ArtworkBinding::track(&track), 256, 256);
    assert_ne!(
        album_request.binding.stable_identity(),
        track_request.binding.stable_identity()
    );
    let artwork = Artwork::new(temporary.path(), runtime()).expect("artwork service starts");

    let album_pending = prepare_and_request(&artwork, source.clone(), album_request)
        .expect("album artwork request");
    images.wait_started();
    let track_pending =
        prepare_and_request(&artwork, source, track_request).expect("track artwork request");
    images.release();

    wait_for_ready(album_pending);
    wait_for_ready(track_pending);
    assert_eq!(images.started_count(), 1);
}

#[test]
fn prepared_artwork_reuses_a_decoded_result_while_its_consumer_owns_it() {
    let temporary = TempDir::new().expect("temporary artwork directory");
    let images = Arc::new(StaticImages {
        calls: AtomicUsize::new(0),
        bytes: png_bytes(),
    });
    let source_id = SourceId::new("source-prepared-ready");
    let source = SourceImages::testing(source_id.clone(), images.clone());
    let request = request("prepared-ready-cover");
    let artwork = Artwork::new(temporary.path(), runtime()).expect("artwork service starts");

    let miss = artwork.prepare(source.clone(), request.clone());
    assert!(miss.ready.is_none());
    assert_eq!(images.calls.load(Ordering::Relaxed), 0);

    let pending = artwork
        .request_prepared(miss)
        .expect("request prepared miss");
    let retained = match finish(pending) {
        ArtworkOutcome::Ready(image) => image,
        _ => panic!("prepared artwork was not ready"),
    };
    assert_eq!(images.calls.load(Ordering::Relaxed), 1);

    let hit = artwork
        .prepare(source.clone(), request.clone())
        .ready
        .expect("the live decoded result is reusable");
    assert!(Arc::ptr_eq(&retained, &hit));
    assert_eq!(images.calls.load(Ordering::Relaxed), 1);

    drop(hit);
    drop(retained);
    assert!(
        artwork.prepare(source, request.clone()).ready.is_none(),
        "the artwork pipeline must not retain final decoded pixels"
    );

    let cold_artwork =
        Artwork::new(temporary.path(), runtime()).expect("cold artwork service starts");
    let filesystem_only = cold_artwork.prepare(SourceImages::cache_only(source_id), request);
    assert!(filesystem_only.ready.is_none());
}

#[test]
fn dropping_the_final_decoded_consumer_keeps_only_the_disk_entry() {
    let temporary = TempDir::new().expect("temporary artwork directory");
    let images = Arc::new(StaticImages {
        calls: AtomicUsize::new(0),
        bytes: png_bytes(),
    });
    let source_id = SourceId::new("transient-decoded-source");
    let source = SourceImages::testing(source_id.clone(), images.clone());
    let request = request("transient-decoded-cover");
    let artwork = Artwork::new(temporary.path(), runtime()).expect("artwork service starts");

    let retained = match finish(
        prepare_and_request(&artwork, source.clone(), request.clone())
            .expect("decoded artwork request"),
    ) {
        ArtworkOutcome::Ready(image) => image,
        _ => panic!("decoded artwork was not ready"),
    };
    let reused = artwork
        .prepare(source.clone(), request.clone())
        .ready
        .expect("the live decoded result is indexed");
    assert!(Arc::ptr_eq(&retained, &reused));
    drop(reused);
    drop(retained);

    assert!(
        artwork.prepare(source, request.clone()).ready.is_none(),
        "dropping the final consumer releases decoded pixels"
    );
    assert!(
        artwork.cache_only_file(&source_id, &request).is_some(),
        "the normalized disk entry remains available"
    );
    wait_for_ready(
        prepare_and_request(&artwork, SourceImages::cache_only(source_id), request)
            .expect("decode the preserved disk entry"),
    );
    assert_eq!(images.calls.load(Ordering::Relaxed), 1);
}

#[test]
fn larger_decoded_cover_satisfies_a_smaller_request_without_another_fetch() {
    let temporary = TempDir::new().expect("temporary artwork directory");
    let images = Arc::new(StaticImages {
        calls: AtomicUsize::new(0),
        bytes: png_bytes_at(800, 800),
    });
    let source = SourceImages::testing(
        SourceId::new("source-reusable-decoded-size"),
        images.clone(),
    );
    let candidates = ArtworkBinding::from_native(Some(&ImageRef::new("shared-cover", None)));
    let large = ArtworkRequest::new(candidates.clone(), 256, 256);
    let small = ArtworkRequest::new(candidates, 96, 96);
    let artwork = Artwork::new(temporary.path(), runtime()).expect("artwork service starts");

    let pending = artwork
        .request_prepared(artwork.prepare(source.clone(), large))
        .expect("large artwork request");
    let retained = match finish(pending) {
        ArtworkOutcome::Ready(image) => image,
        _ => panic!("large artwork was not ready"),
    };

    let reused = artwork
        .prepare(source, small)
        .ready
        .expect("the decoded grid cover satisfies the row request");
    assert!(Arc::ptr_eq(&retained, &reused));
    assert_eq!(reused.width(), 256);
    assert_eq!(reused.height(), 256);
    assert_eq!(images.calls.load(Ordering::Relaxed), 1);
}

#[test]
fn smaller_decoded_cover_previews_a_larger_request_without_satisfying_it() {
    let temporary = TempDir::new().expect("temporary artwork directory");
    let images = Arc::new(StaticImages {
        calls: AtomicUsize::new(0),
        bytes: png_bytes_at(800, 800),
    });
    let source = SourceImages::testing(SourceId::new("source-size-upgrade"), images.clone());
    let candidates = ArtworkBinding::from_native(Some(&ImageRef::new("shared-cover", None)));
    let small = ArtworkRequest::new(candidates.clone(), 256, 48);
    let large = ArtworkRequest::new(candidates, 256, 256);
    let artwork = Artwork::new(temporary.path(), runtime()).expect("artwork service starts");

    let small = match finish(
        artwork
            .request_prepared(artwork.prepare(source.clone(), small))
            .expect("small artwork request"),
    ) {
        ArtworkOutcome::Ready(image) => image,
        _ => panic!("small artwork was not ready"),
    };
    assert_eq!(small.width(), 48);
    let prepared = artwork.prepare(source.clone(), large.clone());
    assert!(
        prepared.ready.is_none(),
        "the small decode must not be presented as a large cover"
    );
    let preview = prepared
        .preview
        .as_ref()
        .expect("the small decode can be shown while the large cover loads");
    assert!(Arc::ptr_eq(&small, preview));

    let large = match finish(
        artwork
            .request_prepared(prepared)
            .expect("large artwork request"),
    ) {
        ArtworkOutcome::Ready(image) => image,
        _ => panic!("large artwork was not ready"),
    };
    assert!(!Arc::ptr_eq(&small, &large));
    assert_eq!(large.width(), 256);
    assert_eq!(large.height(), 256);
    assert_eq!(
        images.calls.load(Ordering::Relaxed),
        1,
        "the normalized disk entry should satisfy the size upgrade"
    );
}

#[test]
fn dropping_a_pending_lease_cancels_its_pipeline_subscription() {
    let temporary = TempDir::new().expect("temporary artwork directory");
    let images = Arc::new(BlockingImages::default());
    let source = SourceImages::testing(SourceId::new("source-cancel"), images.clone());
    let artwork = Artwork::new(temporary.path(), runtime()).expect("artwork service starts");
    let pending = pending_artwork(
        prepare_and_request(&artwork, source, request("slow-cover")).expect("slow request"),
    );

    images.wait_started();
    drop(pending);
    images.release();
    images.wait_finished();
}

#[test]
fn source_invalidation_rejects_an_in_flight_result_and_removes_its_file() {
    let temporary = TempDir::new().expect("temporary artwork directory");
    let images = Arc::new(BlockingImages::default());
    let source_id = SourceId::new("source-stale");
    let source = SourceImages::testing(source_id.clone(), images.clone());
    let request = request("stale-cover");
    let artwork = Artwork::new(temporary.path(), runtime()).expect("artwork service starts");
    let pending = pending_artwork(
        prepare_and_request(&artwork, source, request.clone()).expect("stale request"),
    );

    images.wait_started();
    artwork
        .invalidate_source(&source_id)
        .expect("source invalidation");
    images.release();
    images.wait_finished();

    assert!(artwork.cache_only_file(&source_id, &request).is_none());
    assert!(matches!(
        runtime().block_on(pending.finish()),
        ArtworkOutcome::Invalidated
    ));
}

#[test]
fn binding_identity_separates_visual_changes_from_rerequest_changes() {
    let temporary = TempDir::new().expect("temporary artwork directory");
    let source_id = SourceId::new("source-identity");
    let candidates = ArtworkBinding::album_text("Artist", "Album");
    let artwork = Artwork::new(temporary.path(), runtime()).expect("artwork service");
    let source = SourceImages::cache_only(source_id.clone());
    let base = ArtworkRequest::new(candidates.clone(), 96, 96)
        .with_external(ExternalPolicy::new(false, false, ""));
    let initial = artwork.prepare(source.clone(), base).identity;

    let resized = ArtworkRequest::new(candidates.clone(), 256, 192)
        .with_external(ExternalPolicy::new(false, false, ""));
    let resized_identity = artwork.prepare(source.clone(), resized).identity;
    assert_eq!(initial.visual, resized_identity.visual);
    assert_ne!(initial.request, resized_identity.request);

    let network = ArtworkRequest::new(candidates.clone(), 96, 96)
        .with_external(ExternalPolicy::new(false, true, "key"));
    let network_identity = artwork.prepare(source.clone(), network.clone()).identity;
    assert_eq!(initial.visual, network_identity.visual);
    assert_ne!(initial.request, network_identity.request);

    let lastfm_only = ArtworkRequest::new(candidates.clone(), 96, 96)
        .with_external(ExternalPolicy::new(false, true, "key").with_musicbrainz(false));
    let lastfm_only_identity = artwork.prepare(source.clone(), lastfm_only).identity;
    assert_eq!(network_identity.visual, lastfm_only_identity.visual);
    assert_ne!(network_identity.request, lastfm_only_identity.request);

    artwork.retry_external().expect("retry external artwork");
    let retried = artwork.prepare(source.clone(), network).identity;
    assert_eq!(network_identity.visual, retried.visual);
    assert_ne!(network_identity.request, retried.request);

    let cached = ArtworkRequest::new(candidates, 96, 96)
        .with_external(ExternalPolicy::new(true, true, "key"));
    let cached_identity = artwork.prepare(source.clone(), cached.clone()).identity;
    assert_ne!(retried.visual, cached_identity.visual);
    assert_ne!(retried.request, cached_identity.request);

    artwork
        .invalidate_source(&source_id)
        .expect("source invalidation");
    let invalidated = artwork.prepare(source.clone(), cached).identity;
    assert_ne!(cached_identity.visual, invalidated.visual);
    assert_ne!(cached_identity.request, invalidated.request);

    let native = ArtworkRequest::new(
        ArtworkBinding::from_native(Some(&ImageRef::new("native-cover", None))),
        96,
        96,
    );
    let cache_only_native = artwork.prepare(source, native.clone()).identity;
    let provider = Arc::new(StaticImages {
        calls: AtomicUsize::new(0),
        bytes: png_bytes(),
    });
    let fetchable_source = SourceImages::testing(source_id, provider);
    let fetchable_native = artwork.prepare(fetchable_source, native).identity;
    assert_eq!(cache_only_native.visual, fetchable_native.visual);
    assert_ne!(cache_only_native.request, fetchable_native.request);
}

#[test]
fn larger_cached_cover_satisfies_a_smaller_request_without_another_fetch() {
    let temporary = TempDir::new().expect("temporary artwork directory");
    let images = Arc::new(StaticImages {
        calls: AtomicUsize::new(0),
        bytes: png_bytes_at(800, 800),
    });
    let source_id = SourceId::new("source-reusable-cached-size");
    let source = SourceImages::testing(source_id, images.clone());
    let candidates = ArtworkBinding::from_native(Some(&ImageRef::new("shared-cover", None)));
    let large = ArtworkRequest::new(candidates.clone(), 256, 256);
    let small = ArtworkRequest::new(candidates, 96, 96);
    let artwork = Artwork::new(temporary.path(), runtime()).expect("artwork service starts");

    wait_for_ready(
        artwork
            .request_prepared(artwork.prepare(source.clone(), large))
            .expect("large artwork request"),
    );
    wait_for_ready(
        artwork
            .request_prepared(artwork.prepare(source, small))
            .expect("small artwork request"),
    );

    assert_eq!(images.calls.load(Ordering::Relaxed), 1);
}

#[test]
fn provider_images_are_cached_at_each_requested_size() {
    let temporary = TempDir::new().expect("temporary artwork directory");
    let runtime = runtime();
    let images = Arc::new(StaticImages {
        calls: AtomicUsize::new(0),
        bytes: png_bytes_at(800, 600),
    });
    let source_id = SourceId::new("source-sized-cache");
    let source = SourceImages::testing(source_id.clone(), images.clone());
    let candidates = ArtworkBinding::from_native(Some(&ImageRef::new("large-cover", None)));
    let artwork = Artwork::new(temporary.path(), runtime).expect("artwork service starts");

    for size in [96, 256, 512] {
        let request = ArtworkRequest::new(candidates.clone(), size, size);
        let pending =
            prepare_and_request(&artwork, source.clone(), request.clone()).expect("sized request");
        wait_for_ready(pending);
        let path = artwork
            .cache_only_file(&source_id, &request)
            .expect("sized cache file");
        let bytes = fs::read(path).expect("read sized cache file");
        let cached = crate::decode_rgba(&bytes, u32::MAX).expect("decode sized cache file");
        assert_eq!(cached.width().max(cached.height()), size);
    }

    assert_eq!(images.calls.load(Ordering::Relaxed), 3);
}

#[test]
fn cache_only_sources_are_scoped_and_do_not_create_native_misses() {
    let temporary = TempDir::new().expect("temporary artwork directory");
    let source_id = SourceId::new("source-cached");
    let request = request("shared-cover");
    let seeded_images = Arc::new(StaticImages {
        calls: AtomicUsize::new(0),
        bytes: png_bytes(),
    });
    let seeder = Artwork::new(temporary.path(), runtime()).expect("artwork service starts");
    let seeded = prepare_and_request(
        &seeder,
        SourceImages::testing(source_id.clone(), seeded_images.clone()),
        request.clone(),
    )
    .expect("seed source artwork");
    wait_for_ready(seeded);
    assert_eq!(seeded_images.calls.load(Ordering::Relaxed), 1);

    let artwork = Artwork::new(temporary.path(), runtime()).expect("fresh artwork service starts");
    let cached = prepare_and_request(
        &artwork,
        SourceImages::cache_only(source_id),
        request.clone(),
    )
    .expect("request matching cached source");
    wait_for_ready(cached);

    let other_source_id = SourceId::new("source-other");
    let uncached = prepare_and_request(
        &artwork,
        SourceImages::cache_only(other_source_id.clone()),
        request.clone(),
    )
    .expect("request other cached source");
    wait_for_missing(uncached);

    let other_images = Arc::new(StaticImages {
        calls: AtomicUsize::new(0),
        bytes: png_bytes(),
    });
    let fetched = prepare_and_request(
        &artwork,
        SourceImages::testing(other_source_id, other_images.clone()),
        request,
    )
    .expect("fetch after cache-only miss");
    wait_for_ready(fetched);
    assert_eq!(other_images.calls.load(Ordering::Relaxed), 1);
}

#[test]
fn cancelled_queue_entries_do_not_strand_later_artwork() {
    let temporary = TempDir::new().expect("temporary artwork directory");
    let blockers = Arc::new(BlockingImages::default());
    let blocking_source = SourceImages::testing(
        SourceId::new("source-cancelled-queue-blockers"),
        blockers.clone(),
    );
    let images = Arc::new(StaticImages {
        calls: AtomicUsize::new(0),
        bytes: png_bytes(),
    });
    let source = SourceImages::testing(SourceId::new("source-cancelled-queue"), images.clone());
    let artwork = Artwork::new(temporary.path(), runtime()).expect("artwork service starts");

    let mut blocking = Vec::new();
    for index in 0..super::pipeline::WORKERS {
        blocking.push(
            prepare_and_request(
                &artwork,
                blocking_source.clone(),
                request(&format!("blocking-cover-{index}")),
            )
            .expect("blocking artwork request"),
        );
    }
    blockers.wait_started_count(super::pipeline::WORKERS);

    let mut pending = Vec::new();
    for index in 0..=super::pipeline::WORKERS {
        pending.push(
            prepare_and_request(
                &artwork,
                source.clone(),
                request(&format!("queue-cover-{index}")),
            )
            .expect("queued artwork request"),
        );
    }
    for pending in pending.drain(..super::pipeline::WORKERS) {
        drop(pending);
    }
    blockers.release();

    let last = pending.pop().expect("last queued request");
    wait_for_ready(last);
    assert_eq!(images.calls.load(Ordering::Relaxed), 1);
}

#[test]
fn terminal_missing_is_cached_until_the_source_is_invalidated() {
    let temporary = TempDir::new().expect("temporary artwork directory");
    let images = Arc::new(MissingImages {
        calls: AtomicUsize::new(0),
    });
    let source_id = SourceId::new("source-missing");
    let source = SourceImages::testing(source_id.clone(), images.clone());
    let request = request("absent-cover");
    let artwork = Artwork::new(temporary.path(), runtime()).expect("artwork service starts");

    let first = prepare_and_request(&artwork, source.clone(), request.clone())
        .expect("first missing request");
    wait_for_missing(first);
    let second = prepare_and_request(&artwork, source.clone(), request.clone())
        .expect("cached missing request");
    wait_for_missing(second);
    assert_eq!(images.calls.load(Ordering::Relaxed), 1);

    artwork
        .invalidate_source(&source_id)
        .expect("source invalidation");
    let third = prepare_and_request(&artwork, source, request).expect("request after invalidation");
    wait_for_missing(third);
    assert_eq!(images.calls.load(Ordering::Relaxed), 2);
}

#[test]
fn disabled_external_policy_does_not_reuse_decoded_external_art() {
    let temporary = TempDir::new().expect("temporary artwork directory");
    let source_id = SourceId::new("source-external-policy");
    let candidates = ArtworkBinding::album_text("Artist", "Album");
    let candidate = candidates.candidates().first().expect("album candidate");
    let layout = crate::cache::current_layout(temporary.path()).expect("cache layout");
    crate::cache::FilesystemCache::new(layout)
        .expect("filesystem cache")
        .write_ready(&source_id, candidate, 96, &png_bytes())
        .expect("seed external artwork");
    let images = Arc::new(MissingImages {
        calls: AtomicUsize::new(0),
    });
    let source = SourceImages::testing(source_id, images.clone());
    let artwork = Artwork::new(temporary.path(), runtime()).expect("artwork service starts");
    let allowed = ArtworkRequest::new(candidates.clone(), 96, 96)
        .with_external(ExternalPolicy::new(true, false, ""));

    let ready =
        prepare_and_request(&artwork, source.clone(), allowed).expect("cached external request");
    wait_for_ready(ready);

    let denied = prepare_and_request(&artwork, source, ArtworkRequest::new(candidates, 96, 96))
        .expect("disabled external request");
    wait_for_missing(denied);

    assert_eq!(images.calls.load(Ordering::Relaxed), 0);
}

#[test]
fn product_artwork_formats_normalize_to_rgba_png() {
    for format in [
        ImageFormat::Bmp,
        ImageFormat::Gif,
        ImageFormat::Jpeg,
        ImageFormat::Png,
        ImageFormat::Tiff,
        ImageFormat::WebP,
    ] {
        let normalized = crate::decode::normalize_for_cache(encoded_image(7, 5, format), 64)
            .expect("supported artwork format");
        let decoded = crate::decode_rgba(normalized.bytes(), 64).expect("normalized artwork");

        assert_eq!((decoded.width(), decoded.height()), (7, 5));
        assert_eq!(decoded.row_stride(), 7 * 4);
        assert_eq!(decoded.rgba().len(), 7 * 5 * 4);
    }
}

#[test]
fn normalization_applies_embedded_orientation_before_scaling() {
    let mut jpeg = encoded_image(2, 3, ImageFormat::Jpeg);
    jpeg.splice(2..2, exif_rotate_90());

    let normalized =
        crate::decode::normalize_for_cache(jpeg, 64).expect("oriented JPEG normalization");
    let decoded = crate::decode_rgba(normalized.bytes(), 64).expect("oriented cached artwork");

    assert_eq!((decoded.width(), decoded.height()), (3, 2));
}

#[test]
fn decoded_pixels_keep_straight_rgba_channel_order() {
    let image = RgbaImage::from_pixel(1, 1, Rgba([0x2f, 0x81, 0xf7, 0x42]));
    let bytes = write_image(DynamicImage::ImageRgba8(image), ImageFormat::Png);

    let decoded = crate::decode_rgba(&bytes, 1).expect("RGBA artwork");

    assert_eq!(decoded.rgba(), &[0x2f, 0x81, 0xf7, 0x42]);
}

fn request(id: &str) -> ArtworkRequest {
    let image_ref = ImageRef::new(id, None);
    ArtworkRequest::new(ArtworkBinding::from_native(Some(&image_ref)), 96, 96)
}

fn prepare_and_request(
    artwork: &Artwork,
    source: SourceImages,
    request: ArtworkRequest,
) -> Result<ArtworkLoad, crate::ArtworkError> {
    artwork.request_prepared(artwork.prepare(source, request))
}

fn runtime() -> Handle {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("test Tokio runtime")
        })
        .handle()
        .clone()
}

fn png_bytes() -> Vec<u8> {
    png_bytes_at(2, 2)
}

fn png_bytes_at(width: u32, height: u32) -> Vec<u8> {
    encoded_image(width, height, ImageFormat::Png)
}

fn encoded_image(width: u32, height: u32, format: ImageFormat) -> Vec<u8> {
    let image = RgbaImage::from_pixel(width, height, Rgba([0x2f, 0x81, 0xf7, 0xff]));
    let image = DynamicImage::ImageRgba8(image);
    let image = if format == ImageFormat::Jpeg {
        DynamicImage::ImageRgb8(image.into_rgb8())
    } else {
        image
    };
    write_image(image, format)
}

fn write_image(image: DynamicImage, format: ImageFormat) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    image
        .write_to(&mut bytes, format)
        .expect("encode test artwork");
    bytes.into_inner()
}

fn exif_rotate_90() -> [u8; 36] {
    [
        0xff, 0xe1, 0x00, 0x22, b'E', b'x', b'i', b'f', 0x00, 0x00, b'M', b'M', 0x00, 0x2a, 0x00,
        0x00, 0x00, 0x08, 0x00, 0x01, 0x01, 0x12, 0x00, 0x03, 0x00, 0x00, 0x00, 0x01, 0x00, 0x06,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ]
}

fn pending_artwork(load: ArtworkLoad) -> PendingArtwork {
    match load {
        ArtworkLoad::Pending(pending) => pending,
        ArtworkLoad::Ready(_) => panic!("expected a pending artwork request"),
        ArtworkLoad::Missing => panic!("expected a pending artwork request"),
    }
}

fn finish(load: ArtworkLoad) -> ArtworkOutcome {
    match load {
        ArtworkLoad::Ready(image) => ArtworkOutcome::Ready(image),
        ArtworkLoad::Missing => ArtworkOutcome::Missing,
        ArtworkLoad::Pending(pending) => runtime().block_on(pending.finish()),
    }
}

fn wait_for_ready(load: ArtworkLoad) {
    assert!(matches!(finish(load), ArtworkOutcome::Ready(_)));
}

fn wait_for_missing(load: ArtworkLoad) {
    assert!(matches!(finish(load), ArtworkOutcome::Missing));
}
