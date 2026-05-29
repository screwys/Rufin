use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use rufin_core::{ImageRef, ServerId};
use rufin_provider::MusicProvider;
use rufin_secrets::SecretStore;
use rufin_store::{SavedServer, image_cache_key};
use tokio::runtime::Runtime;
use tracing::{debug, info, warn};

use crate::external_metadata;

mod cache;
mod candidates;
mod fetch;

use super::{
    AppController, ControllerEvent, IMAGE_TAG_UNTAGGED, StoreHandle, acquire_cover_slot,
    load_settings_from_store, provider_for_saved, release_cover_slot,
};
use cache::*;
use candidates::*;
pub(super) use fetch::is_provider_not_found_error;
use fetch::{fetch_and_cache_cover, fetch_and_cache_provider_cover};

const EXTERNAL_PREFETCH_PAGE_SIZE: usize = 500;
const EXTERNAL_PREFETCH_COVER_SIZE: u32 = 256;
const EXTERNAL_THUMB_COVER_SIZE: u32 = 96;
const EXTERNAL_DETAIL_COVER_SIZE: u32 = 512;
const EXTERNAL_PREFETCH_DELAY: Duration = Duration::from_secs(1);

#[derive(Default)]
struct SyncedImagePrefetchStats {
    album_rows: usize,
    album_image_refs: usize,
    artist_rows: usize,
    artist_image_refs: usize,
    album_artist_rows: usize,
    album_artist_image_refs: usize,
    cache_hits: usize,
    known_misses: usize,
    skipped: usize,
    fetched: usize,
    misses: usize,
    errors: usize,
}

#[derive(Default)]
struct ProviderCoverPrefetchStats {
    album_rows: usize,
    track_rows: usize,
    artist_rows: usize,
    album_artist_rows: usize,
    genre_rows: usize,
    playlist_rows: usize,
    image_refs: usize,
    cache_hits: usize,
    skipped: usize,
    fetched: usize,
    misses: usize,
    errors: usize,
}

pub(in crate::controller) struct ExternalCoverPrefetchRequest {
    pub(in crate::controller) store: StoreHandle,
    pub(in crate::controller) runtime: Arc<Runtime>,
    pub(in crate::controller) secrets: Arc<dyn SecretStore>,
    pub(in crate::controller) events: Sender<ControllerEvent>,
    pub(in crate::controller) cover_in_flight: Arc<Mutex<HashSet<String>>>,
    pub(in crate::controller) external_cover_prefetch_in_flight: Arc<Mutex<HashSet<ServerId>>>,
    pub(in crate::controller) cover_slots: Arc<(Mutex<usize>, Condvar)>,
    pub(in crate::controller) saved: SavedServer,
}

struct CoverPrefetchContext<'a> {
    store: &'a StoreHandle,
    runtime: &'a Runtime,
    secrets: &'a Arc<dyn SecretStore>,
    events: &'a Sender<ControllerEvent>,
    cover_in_flight: &'a Arc<Mutex<HashSet<String>>>,
    cover_slots: &'a Arc<(Mutex<usize>, Condvar)>,
    saved: &'a SavedServer,
}

#[derive(Clone, Copy)]
enum SyncedImagePrefetchOutcome {
    CacheHit,
    KnownMiss,
    Skipped,
    Fetched,
    Miss,
    Error,
}

impl SyncedImagePrefetchOutcome {
    fn used_network(self) -> bool {
        matches!(self, Self::Fetched | Self::Miss | Self::Error)
    }
}

impl AppController {
    #[cfg(test)]
    pub fn cover_key(&self, image_ref: &ImageRef, size: u32) -> Option<String> {
        let server = self
            .store
            .with_store(|store| store.active_server())
            .ok()
            .flatten()?
            .server;
        Some(image_cache_key(
            &server.id,
            &image_ref.item_id,
            image_ref.tag.as_deref().unwrap_or(IMAGE_TAG_UNTAGGED),
            size,
        ))
    }

    pub fn cached_cover_path(&self, image_ref: &ImageRef, size: u32) -> Option<PathBuf> {
        let saved = self
            .store
            .with_store(|store| store.active_server())
            .ok()
            .flatten()?;
        cached_cover_path_for_saved(&self.store, &saved, image_ref, size)
            .ok()
            .flatten()
    }

    pub fn cached_cover_path_for_key(&self, key: &str) -> Option<PathBuf> {
        cached_cover_path_for_key(key)
    }

    pub fn external_cover_lookup_known_missing(&self, image_ref: &ImageRef, size: u32) -> bool {
        if !external_metadata::is_external_image_ref(image_ref) {
            return false;
        }
        let Some(saved) = self
            .store
            .with_store(|store| store.active_server())
            .ok()
            .flatten()
        else {
            return false;
        };
        external_lookup_miss_size_candidates(size)
            .into_iter()
            .any(|candidate_size| {
                external_lookup_miss_cached(&self.store, &saved, image_ref, candidate_size)
                    .unwrap_or(false)
            })
    }

    pub fn retry_external_cover_lookups(&self) -> Result<(), String> {
        let Some(saved) = self.store.with_store(|store| store.active_server())? else {
            return Ok(());
        };
        self.store
            .with_store(|store| store.clear_external_image_lookup_misses(&saved.server.id))?;
        start_external_metadata_cover_prefetch_thread(ExternalCoverPrefetchRequest {
            store: self.store.clone(),
            runtime: Arc::clone(&self.runtime),
            secrets: Arc::clone(&self.secrets),
            events: self.events.clone(),
            cover_in_flight: Arc::clone(&self.cover_in_flight),
            external_cover_prefetch_in_flight: Arc::clone(&self.external_cover_prefetch_in_flight),
            cover_slots: Arc::clone(&self.cover_slots),
            saved,
        });
        Ok(())
    }

    #[cfg(test)]
    pub fn request_cover(&self, image_ref: ImageRef, size: u32) {
        let Some(saved) = self
            .store
            .with_store(|store| store.active_server())
            .unwrap_or(None)
        else {
            return;
        };
        if saved.server.provider == "fake" {
            return;
        }
        if let Some(path) = self.cached_cover_path(&image_ref, size) {
            if let Some(key) = self.cover_key(&image_ref, size) {
                let _sent = self.events.send(ControllerEvent::CoverReady { key, path });
            }
            return;
        }
        let tag = image_ref
            .tag
            .clone()
            .unwrap_or_else(|| IMAGE_TAG_UNTAGGED.to_string());
        let key = image_cache_key(&saved.server.id, &image_ref.item_id, &tag, size);
        match self.cover_in_flight.lock() {
            Ok(mut in_flight) => {
                if !in_flight.insert(key.clone()) {
                    return;
                }
            }
            Err(_) => return,
        }

        let store = self.store.clone();
        let runtime = Arc::clone(&self.runtime);
        let secrets = Arc::clone(&self.secrets);
        let events = self.events.clone();
        let cover_in_flight = Arc::clone(&self.cover_in_flight);
        let cover_slots = Arc::clone(&self.cover_slots);
        thread::spawn(move || {
            if !acquire_cover_slot(&cover_slots) {
                if let Ok(mut in_flight) = cover_in_flight.lock() {
                    in_flight.remove(&key);
                }
                return;
            }
            let result = fetch_and_cache_cover(&store, &runtime, &secrets, &saved, image_ref, size);
            release_cover_slot(&cover_slots);
            if let Ok(mut in_flight) = cover_in_flight.lock() {
                in_flight.remove(&key);
            }
            match result {
                Ok(path) => {
                    let _sent = events.send(ControllerEvent::CoverReady { key, path });
                }
                Err(error) => {
                    warn!(%error, "failed to fetch cover");
                }
            }
        });
    }

    pub fn request_cover_for_key(&self, key: String, image_ref: ImageRef, size: u32) {
        match self.cover_in_flight.lock() {
            Ok(mut in_flight) => {
                if !in_flight.insert(key.clone()) {
                    return;
                }
            }
            Err(_) => return,
        }

        let store = self.store.clone();
        let runtime = Arc::clone(&self.runtime);
        let secrets = Arc::clone(&self.secrets);
        let events = self.events.clone();
        let cover_in_flight = Arc::clone(&self.cover_in_flight);
        let cover_slots = Arc::clone(&self.cover_slots);
        thread::spawn(move || {
            let is_external_cover = external_metadata::is_external_image_ref(&image_ref);
            let miss_item_id = image_ref.item_id.clone();
            let miss_image_tag = image_ref
                .tag
                .clone()
                .unwrap_or_else(|| IMAGE_TAG_UNTAGGED.to_string());
            let result = (|| -> Result<Option<PathBuf>, String> {
                let settings = load_settings_from_store(&store);
                if is_external_cover && !external_metadata::enabled(&settings) {
                    return Ok(None);
                }
                if let Some(path) = cached_cover_path_for_key(&key) {
                    return Ok(Some(path));
                }

                let Some(saved) = store.with_store(|store| store.active_server())? else {
                    return Ok(None);
                };
                if saved.server.provider == "fake" {
                    return Ok(None);
                }

                let tag = image_ref.tag.as_deref().unwrap_or(IMAGE_TAG_UNTAGGED);
                let expected_key = image_cache_key(&saved.server.id, &image_ref.item_id, tag, size);
                if expected_key != key {
                    return Ok(None);
                }

                if let Some(path) = cached_cover_path_for_saved(&store, &saved, &image_ref, size)? {
                    return Ok(Some(path));
                }
                if is_external_cover
                    && external_lookup_miss_cached(&store, &saved, &image_ref, size)?
                {
                    return Ok(None);
                }

                if !acquire_cover_slot(&cover_slots) {
                    return Ok(None);
                }
                let result =
                    fetch_and_cache_cover(&store, &runtime, &secrets, &saved, image_ref, size)
                        .map(Some);
                release_cover_slot(&cover_slots);
                result
            })();

            if let Ok(mut in_flight) = cover_in_flight.lock() {
                in_flight.remove(&key);
            }
            match result {
                Ok(Some(path)) => {
                    let _sent = events.send(ControllerEvent::CoverReady { key, path });
                }
                Ok(None) => {}
                Err(error) => {
                    if is_external_cover && external_metadata::is_expected_lookup_miss(&error) {
                        let _saved_miss = store.with_store(|store| {
                            if let Some(saved) = store.active_server()? {
                                store.save_external_image_lookup_miss(
                                    &saved.server.id,
                                    &miss_item_id,
                                    &miss_image_tag,
                                    size,
                                    &error,
                                )?;
                            }
                            Ok(())
                        });
                        debug!(%error, "external metadata cover was not available");
                    } else if is_provider_not_found_error(&error) {
                        debug!(%error, "cached cover source item is no longer available");
                    } else {
                        warn!(%error, "failed to prepare cover");
                    }
                }
            }
        });
    }
}

pub(super) fn start_external_metadata_cover_prefetch_thread(request: ExternalCoverPrefetchRequest) {
    let ExternalCoverPrefetchRequest {
        store,
        runtime,
        secrets,
        events,
        cover_in_flight,
        external_cover_prefetch_in_flight,
        cover_slots,
        saved,
    } = request;
    if saved.server.provider == "fake" {
        return;
    }

    let server_id = saved.server.id.clone();
    match external_cover_prefetch_in_flight.lock() {
        Ok(mut running) => {
            if !running.insert(server_id.clone()) {
                return;
            }
        }
        Err(_) => return,
    }

    thread::spawn(move || {
        info!(
            server_id = %saved.server.id,
            "started synced image prefetch"
        );
        let mut stats = SyncedImagePrefetchStats::default();
        let context = CoverPrefetchContext {
            store: &store,
            runtime: &runtime,
            secrets: &secrets,
            events: &events,
            cover_in_flight: &cover_in_flight,
            cover_slots: &cover_slots,
            saved: &saved,
        };
        let result = prefetch_synced_images(&context, &mut stats);
        match result {
            Ok(()) => {
                info!(
                    server_id = %saved.server.id,
                    album_rows = stats.album_rows,
                    album_image_refs = stats.album_image_refs,
                    artist_rows = stats.artist_rows,
                    artist_image_refs = stats.artist_image_refs,
                    album_artist_rows = stats.album_artist_rows,
                    album_artist_image_refs = stats.album_artist_image_refs,
                    cache_hits = stats.cache_hits,
                    known_misses = stats.known_misses,
                    skipped = stats.skipped,
                    fetched = stats.fetched,
                    misses = stats.misses,
                    errors = stats.errors,
                    "completed synced image prefetch"
                );
            }
            Err(error) => {
                warn!(
                    %error,
                    server_id = %saved.server.id,
                    album_rows = stats.album_rows,
                    album_image_refs = stats.album_image_refs,
                    artist_rows = stats.artist_rows,
                    artist_image_refs = stats.artist_image_refs,
                    album_artist_rows = stats.album_artist_rows,
                    album_artist_image_refs = stats.album_artist_image_refs,
                    cache_hits = stats.cache_hits,
                    known_misses = stats.known_misses,
                    skipped = stats.skipped,
                    fetched = stats.fetched,
                    misses = stats.misses,
                    errors = stats.errors,
                    "failed to prefetch synced images"
                );
            }
        }
        if let Ok(mut running) = external_cover_prefetch_in_flight.lock() {
            running.remove(&server_id);
        }
    });
}

pub(super) fn prefetch_initial_provider_cover_cache(
    store: &StoreHandle,
    runtime: &Runtime,
    secrets: &Arc<dyn SecretStore>,
    events: &Sender<ControllerEvent>,
    cover_in_flight: &Arc<Mutex<HashSet<String>>>,
    cover_slots: &Arc<(Mutex<usize>, Condvar)>,
    saved: &SavedServer,
) -> Result<(), String> {
    if saved.server.provider == "fake" {
        return Ok(());
    }

    let provider = provider_for_saved(store, runtime, secrets, saved)?;
    let mut provider_stats = ProviderCoverPrefetchStats::default();
    let context = CoverPrefetchContext {
        store,
        runtime,
        events,
        secrets,
        cover_in_flight,
        cover_slots,
        saved,
    };
    prefetch_synced_provider_covers(&context, provider.as_music_provider(), &mut provider_stats)?;
    info!(
        server_id = %saved.server.id,
        album_rows = provider_stats.album_rows,
        track_rows = provider_stats.track_rows,
        artist_rows = provider_stats.artist_rows,
        album_artist_rows = provider_stats.album_artist_rows,
        genre_rows = provider_stats.genre_rows,
        playlist_rows = provider_stats.playlist_rows,
        image_refs = provider_stats.image_refs,
        cache_hits = provider_stats.cache_hits,
        skipped = provider_stats.skipped,
        fetched = provider_stats.fetched,
        misses = provider_stats.misses,
        errors = provider_stats.errors,
        "completed initial provider cover cache prefetch"
    );
    Ok(())
}

fn prefetch_synced_images(
    context: &CoverPrefetchContext<'_>,
    stats: &mut SyncedImagePrefetchStats,
) -> Result<(), String> {
    prefetch_synced_album_covers(context, stats)?;
    prefetch_synced_artist_covers(context, false, stats)?;
    prefetch_synced_artist_covers(context, true, stats)
}

fn prefetch_synced_provider_covers(
    context: &CoverPrefetchContext<'_>,
    provider: &dyn MusicProvider,
    stats: &mut ProviderCoverPrefetchStats,
) -> Result<(), String> {
    let mut seen = HashSet::new();
    let image_refs = synced_provider_cover_refs(context.store, context.saved, &mut seen, stats)?;
    stats.image_refs = image_refs.len();
    for image_ref in image_refs {
        if active_server_changed(context.store, context.saved)? {
            info!(
                server_id = %context.saved.server.id,
                "stopped initial provider cover prefetch because active server changed"
            );
            return Ok(());
        }
        let outcome = prefetch_provider_image_ref(context, provider, image_ref)?;
        record_provider_cover_prefetch_outcome(stats, outcome);
    }
    Ok(())
}

fn synced_provider_cover_refs(
    store: &StoreHandle,
    saved: &SavedServer,
    seen: &mut HashSet<(String, String)>,
    stats: &mut ProviderCoverPrefetchStats,
) -> Result<Vec<ImageRef>, String> {
    let mut image_refs = Vec::new();
    let mut offset = 0;
    loop {
        let page = store.with_store(|store| {
            store.load_albums(&saved.server.id, offset, EXTERNAL_PREFETCH_PAGE_SIZE)
        })?;
        let item_count = page.items.len();
        if item_count == 0 {
            break;
        }
        stats.album_rows += item_count;
        push_provider_album_image_refs(&mut image_refs, seen, page.items);
        offset += item_count;
    }

    let mut offset = 0;
    loop {
        let page = store.with_store(|store| {
            store.load_tracks(&saved.server.id, offset, EXTERNAL_PREFETCH_PAGE_SIZE)
        })?;
        let item_count = page.items.len();
        if item_count == 0 {
            break;
        }
        stats.track_rows += item_count;
        push_provider_track_image_refs(&mut image_refs, seen, page.items);
        offset += item_count;
    }

    for album_artist in [false, true] {
        let mut offset = 0;
        loop {
            let page = store.with_store(|store| {
                store.load_artists(
                    &saved.server.id,
                    album_artist,
                    offset,
                    EXTERNAL_PREFETCH_PAGE_SIZE,
                )
            })?;
            let item_count = page.items.len();
            if item_count == 0 {
                break;
            }
            if album_artist {
                stats.album_artist_rows += item_count;
            } else {
                stats.artist_rows += item_count;
            }
            push_provider_artist_image_refs(&mut image_refs, seen, page.items);
            offset += item_count;
        }
    }

    let mut offset = 0;
    loop {
        let page = store.with_store(|store| {
            store.load_genres(&saved.server.id, offset, EXTERNAL_PREFETCH_PAGE_SIZE)
        })?;
        let item_count = page.items.len();
        if item_count == 0 {
            break;
        }
        stats.genre_rows += item_count;
        push_provider_genre_image_refs(&mut image_refs, seen, page.items);
        offset += item_count;
    }

    let mut offset = 0;
    loop {
        let page = store.with_store(|store| {
            store.load_playlists(&saved.server.id, offset, EXTERNAL_PREFETCH_PAGE_SIZE)
        })?;
        let item_count = page.items.len();
        if item_count == 0 {
            break;
        }
        stats.playlist_rows += item_count;
        push_provider_playlist_image_refs(&mut image_refs, seen, page.items);
        offset += item_count;
    }

    Ok(image_refs)
}

fn prefetch_provider_image_ref(
    context: &CoverPrefetchContext<'_>,
    provider: &dyn MusicProvider,
    image_ref: ImageRef,
) -> Result<SyncedImagePrefetchOutcome, String> {
    let tag = image_ref.tag.as_deref().unwrap_or(IMAGE_TAG_UNTAGGED);
    let key = image_cache_key(
        &context.saved.server.id,
        &image_ref.item_id,
        tag,
        EXTERNAL_PREFETCH_COVER_SIZE,
    );
    if cached_cover_path_for_key(&key).is_some()
        || cached_cover_path_for_saved(
            context.store,
            context.saved,
            &image_ref,
            EXTERNAL_PREFETCH_COVER_SIZE,
        )?
        .is_some()
    {
        return Ok(SyncedImagePrefetchOutcome::CacheHit);
    }
    match context.cover_in_flight.lock() {
        Ok(mut in_flight) => {
            if !in_flight.insert(key.clone()) {
                return Ok(SyncedImagePrefetchOutcome::Skipped);
            }
        }
        Err(_) => return Ok(SyncedImagePrefetchOutcome::Skipped),
    }

    if !acquire_cover_slot(context.cover_slots) {
        if let Ok(mut in_flight) = context.cover_in_flight.lock() {
            in_flight.remove(&key);
        }
        return Ok(SyncedImagePrefetchOutcome::Skipped);
    }
    let result = fetch_and_cache_provider_cover(
        context.store,
        context.runtime,
        context.saved,
        provider,
        image_ref.clone(),
        EXTERNAL_PREFETCH_COVER_SIZE,
    );
    release_cover_slot(context.cover_slots);
    if let Ok(mut in_flight) = context.cover_in_flight.lock() {
        in_flight.remove(&key);
    }

    match result {
        Ok(path) => {
            let _sent = context
                .events
                .send(ControllerEvent::CoverReady { key, path });
            Ok(SyncedImagePrefetchOutcome::Fetched)
        }
        Err(error) => {
            if is_provider_not_found_error(&error) {
                debug!(%error, "initial provider image was not available");
                Ok(SyncedImagePrefetchOutcome::Miss)
            } else {
                warn!(%error, "failed to prefetch initial provider image");
                Ok(SyncedImagePrefetchOutcome::Error)
            }
        }
    }
}

fn prefetch_synced_album_covers(
    context: &CoverPrefetchContext<'_>,
    stats: &mut SyncedImagePrefetchStats,
) -> Result<(), String> {
    let mut offset = 0;
    loop {
        let settings = load_settings_from_store(context.store);
        if !external_metadata::enabled(&settings) {
            info!(
                server_id = %context.saved.server.id,
                private_mode = settings.private_mode,
                external_metadata_enabled = settings.external_metadata_enabled,
                "skipped synced external album cover prefetch"
            );
            return Ok(());
        }
        if active_server_changed(context.store, context.saved)? {
            info!(
                server_id = %context.saved.server.id,
                "stopped synced external album cover prefetch because active server changed"
            );
            return Ok(());
        }
        let page = context.store.with_store(|store| {
            store.load_albums(
                &context.saved.server.id,
                offset,
                EXTERNAL_PREFETCH_PAGE_SIZE,
            )
        })?;
        if page.items.is_empty() {
            return Ok(());
        }
        let album_count = page.items.len();
        stats.album_rows += album_count;
        let image_refs = external_album_image_refs_from_albums(page.items, &settings);
        stats.album_image_refs += image_refs.len();
        for image_ref in image_refs {
            if !external_metadata::enabled(&load_settings_from_store(context.store))
                || active_server_changed(context.store, context.saved)?
            {
                return Ok(());
            }
            let outcome = prefetch_image_ref(context, image_ref)?;
            record_synced_image_prefetch_outcome(stats, outcome);
            if outcome.used_network() {
                thread::sleep(EXTERNAL_PREFETCH_DELAY);
            }
        }
        offset += album_count;
    }
}

fn prefetch_synced_artist_covers(
    context: &CoverPrefetchContext<'_>,
    album_artist: bool,
    stats: &mut SyncedImagePrefetchStats,
) -> Result<(), String> {
    let mut offset = 0;
    loop {
        if active_server_changed(context.store, context.saved)? {
            info!(
                server_id = %context.saved.server.id,
                album_artist,
                "stopped synced provider artist image prefetch because active server changed"
            );
            return Ok(());
        }
        let page = context.store.with_store(|store| {
            store.load_artists(
                &context.saved.server.id,
                album_artist,
                offset,
                EXTERNAL_PREFETCH_PAGE_SIZE,
            )
        })?;
        let artists = page.items;
        if artists.is_empty() {
            return Ok(());
        }
        let artist_count = artists.len();
        if album_artist {
            stats.album_artist_rows += artist_count;
        } else {
            stats.artist_rows += artist_count;
        }
        let image_refs = provider_artist_image_refs_from_artists(artists);
        if album_artist {
            stats.album_artist_image_refs += image_refs.len();
        } else {
            stats.artist_image_refs += image_refs.len();
        }
        for image_ref in image_refs {
            if active_server_changed(context.store, context.saved)? {
                return Ok(());
            }
            let outcome = prefetch_image_ref(context, image_ref)?;
            record_synced_image_prefetch_outcome(stats, outcome);
        }
        offset += artist_count;
    }
}

fn prefetch_image_ref(
    context: &CoverPrefetchContext<'_>,
    image_ref: ImageRef,
) -> Result<SyncedImagePrefetchOutcome, String> {
    let is_external_image = external_metadata::is_external_image_ref(&image_ref);
    let tag = image_ref.tag.as_deref().unwrap_or(IMAGE_TAG_UNTAGGED);
    let key = image_cache_key(
        &context.saved.server.id,
        &image_ref.item_id,
        tag,
        EXTERNAL_PREFETCH_COVER_SIZE,
    );
    if cached_cover_path_for_key(&key).is_some()
        || cached_cover_path_for_saved(
            context.store,
            context.saved,
            &image_ref,
            EXTERNAL_PREFETCH_COVER_SIZE,
        )?
        .is_some()
    {
        return Ok(SyncedImagePrefetchOutcome::CacheHit);
    }
    if is_external_image
        && external_lookup_miss_cached(
            context.store,
            context.saved,
            &image_ref,
            EXTERNAL_PREFETCH_COVER_SIZE,
        )?
    {
        return Ok(SyncedImagePrefetchOutcome::KnownMiss);
    }
    match context.cover_in_flight.lock() {
        Ok(mut in_flight) => {
            if !in_flight.insert(key.clone()) {
                return Ok(SyncedImagePrefetchOutcome::Skipped);
            }
        }
        Err(_) => return Ok(SyncedImagePrefetchOutcome::Skipped),
    }

    if !acquire_cover_slot(context.cover_slots) {
        if let Ok(mut in_flight) = context.cover_in_flight.lock() {
            in_flight.remove(&key);
        }
        return Ok(SyncedImagePrefetchOutcome::Skipped);
    }
    let result = fetch_and_cache_cover(
        context.store,
        context.runtime,
        context.secrets,
        context.saved,
        image_ref.clone(),
        EXTERNAL_PREFETCH_COVER_SIZE,
    );
    release_cover_slot(context.cover_slots);
    if let Ok(mut in_flight) = context.cover_in_flight.lock() {
        in_flight.remove(&key);
    }

    match result {
        Ok(path) => {
            let _sent = context
                .events
                .send(ControllerEvent::CoverReady { key, path });
            Ok(SyncedImagePrefetchOutcome::Fetched)
        }
        Err(error) => {
            if is_external_image && external_metadata::is_expected_lookup_miss(&error) {
                save_external_lookup_miss(
                    context.store,
                    context.saved,
                    &image_ref,
                    EXTERNAL_PREFETCH_COVER_SIZE,
                    &error,
                )?;
                debug!(%error, "synced external image was not available");
                Ok(SyncedImagePrefetchOutcome::Miss)
            } else if !is_external_image && is_provider_not_found_error(&error) {
                debug!(%error, "synced provider image was not available");
                Ok(SyncedImagePrefetchOutcome::Miss)
            } else {
                warn!(%error, "failed to prefetch synced image");
                Ok(SyncedImagePrefetchOutcome::Error)
            }
        }
    }
}

fn record_synced_image_prefetch_outcome(
    stats: &mut SyncedImagePrefetchStats,
    outcome: SyncedImagePrefetchOutcome,
) {
    match outcome {
        SyncedImagePrefetchOutcome::CacheHit => stats.cache_hits += 1,
        SyncedImagePrefetchOutcome::KnownMiss => stats.known_misses += 1,
        SyncedImagePrefetchOutcome::Skipped => stats.skipped += 1,
        SyncedImagePrefetchOutcome::Fetched => stats.fetched += 1,
        SyncedImagePrefetchOutcome::Miss => stats.misses += 1,
        SyncedImagePrefetchOutcome::Error => stats.errors += 1,
    }
}

fn record_provider_cover_prefetch_outcome(
    stats: &mut ProviderCoverPrefetchStats,
    outcome: SyncedImagePrefetchOutcome,
) {
    match outcome {
        SyncedImagePrefetchOutcome::CacheHit => stats.cache_hits += 1,
        SyncedImagePrefetchOutcome::KnownMiss => stats.misses += 1,
        SyncedImagePrefetchOutcome::Skipped => stats.skipped += 1,
        SyncedImagePrefetchOutcome::Fetched => stats.fetched += 1,
        SyncedImagePrefetchOutcome::Miss => stats.misses += 1,
        SyncedImagePrefetchOutcome::Error => stats.errors += 1,
    }
}

fn active_server_changed(store: &StoreHandle, saved: &SavedServer) -> Result<bool, String> {
    Ok(store
        .with_store(|store| store.active_server())?
        .is_none_or(|active| active.server.id != saved.server.id))
}
