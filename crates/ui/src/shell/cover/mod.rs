use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use artwork::{ArtworkBinding, ArtworkRequest, SourceImages};
use gtk::glib;
use tracing::warn;

use crate::Settings as UiSettings;

use super::Shell;

pub(crate) const THUMB_COVER_SIZE: u32 = 96;
pub(crate) const MEDIUM_COVER_SIZE: u32 = 256;
pub(crate) const LARGE_COVER_SIZE: u32 = 512;
const ROUTE_ARTWORK_SCROLL_SETTLE: Duration = Duration::from_millis(160);
const THUMBNAIL_WARM_WINDOW: usize = 16;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlaybackArtworkPath {
    pub(crate) path: PathBuf,
}

pub(crate) mod presentation;
mod texture_cache;
mod tile;
mod tiles;

use texture_cache::TextureCache;
pub(crate) use tile::{ArtworkTile, ArtworkTileWeak};
pub(crate) use tiles::CoverGroupProjection;

pub(crate) fn cover_decode_size(display_size: i32, fetch_size: u32, scale: f64) -> u32 {
    let display_size = f64::from(display_size.max(1));
    let scale = if scale.is_finite() {
        scale.max(1.0)
    } else {
        1.0
    };
    let scaled = (display_size * scale).ceil().min(f64::from(u32::MAX)) as u32;
    scaled.min(fetch_size.max(1))
}

pub(crate) fn cover_fetch_size_for_display(display_size: i32) -> u32 {
    if display_size <= THUMB_COVER_SIZE as i32 {
        THUMB_COVER_SIZE
    } else if display_size <= MEDIUM_COVER_SIZE as i32 {
        MEDIUM_COVER_SIZE
    } else {
        LARGE_COVER_SIZE
    }
}

fn cover_request_sizes(display_size: i32, fetch_size_cap: u32, scale: f64) -> (u32, u32) {
    let fetch_size_cap = fetch_size_cap.max(1);
    let render_size = cover_decode_size(display_size, fetch_size_cap, scale);
    let fetch_size = cover_fetch_size_for_display(render_size as i32);
    (fetch_size.min(fetch_size_cap), render_size)
}

const fn artwork_work_allowed(requires_mapping: bool, mapped: bool) -> bool {
    !requires_mapping || mapped
}

const fn artwork_binding_needs_work(
    tile_needs_request: bool,
    exact_ready: bool,
    terminal_missing: bool,
    request_active: bool,
) -> bool {
    tile_needs_request || (!exact_ready && !terminal_missing && !request_active)
}

#[derive(Clone)]
pub(super) struct LiveArtworkBinding {
    tile: ArtworkTileWeak,
    source_id: Option<::library::SourceId>,
    artwork: ArtworkBinding,
    render_size: i32,
    fetch_size: u32,
    defer_during_route_scroll: bool,
    refresh_desktop_on_ready: bool,
}

#[derive(Default)]
pub(super) struct RouteArtworkInteraction {
    active: Cell<bool>,
    deadline: Cell<Option<Instant>>,
    settle: RefCell<Option<glib::JoinHandle<()>>>,
    deferred: RefCell<HashSet<usize>>,
    adjustment_handler: RefCell<Option<RouteArtworkAdjustmentHandler>>,
}

struct RouteArtworkAdjustmentHandler {
    object: glib::WeakRef<glib::Object>,
    signal: glib::SignalHandlerId,
}

pub(super) struct ArtworkState {
    pub(super) startup_prime: StartupArtworkPrime,
    pub(super) thumbnail_warm: ThumbnailWarmState,
    pub(super) live_bindings: RefCell<HashMap<usize, LiveArtworkBinding>>,
    pub(super) route_interaction: Rc<RouteArtworkInteraction>,
    pub(super) textures: RefCell<TextureCache>,
}

#[derive(Default)]
pub(super) struct ThumbnailWarmState {
    generation: Cell<u64>,
    task: RefCell<Option<glib::JoinHandle<()>>>,
}

#[derive(Default)]
pub(super) struct StartupArtworkPrime {
    active: Cell<bool>,
    generation: Cell<u64>,
    pending: Cell<usize>,
}

impl StartupArtworkPrime {
    fn begin(&self) {
        self.generation
            .set(self.generation.get().wrapping_add(1).max(1));
        self.pending.set(0);
        self.active.set(true);
    }

    fn reserve(&self) -> Option<u64> {
        if !self.active.get() {
            return None;
        }
        self.pending.set(self.pending.get().saturating_add(1));
        Some(self.generation.get())
    }

    fn release(&self, generation: u64) -> bool {
        if !self.active.get() || self.generation.get() != generation {
            return false;
        }
        self.pending.set(self.pending.get().saturating_sub(1));
        self.pending.get() == 0
    }

    fn finish(&self) {
        self.active.set(false);
        self.pending.set(0);
        self.generation
            .set(self.generation.get().wrapping_add(1).max(1));
    }

    fn pending(&self) -> usize {
        self.active.get().then_some(self.pending.get()).unwrap_or(0)
    }
}

struct StartupArtworkLease {
    shell: std::rc::Weak<Shell>,
    generation: u64,
}

impl Drop for StartupArtworkLease {
    fn drop(&mut self) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        if shell.artwork.startup_prime.release(self.generation) {
            shell.try_reveal_startup_route();
        }
    }
}

impl Shell {
    pub(crate) fn connect_artwork_scale_refresh(self: &Rc<Self>) {
        let shell = Rc::downgrade(self);
        self.chrome.window.connect_realize(move |window| {
            let Some(surface) = window.surface() else {
                return;
            };
            let shell = shell.clone();
            surface.connect_scale_notify(move |_| {
                let Some(shell) = shell.upgrade() else {
                    return;
                };
                shell.refresh_artwork_bindings();
                if shell.startup.route_revealed.get() {
                    shell.start_source_thumbnail_warm();
                }
            });
        });
    }

    fn artwork_scale(&self) -> f64 {
        self.chrome.window.surface().map_or_else(
            || f64::from(self.chrome.window.scale_factor()),
            |surface| surface.scale(),
        )
    }

    fn artwork_source(&self, source_id: Option<&::library::SourceId>) -> Option<SourceImages> {
        let selected = self.selected_library();
        match source_id {
            None => selected.as_ref().map(|selected| selected.artwork.clone()),
            Some(source_id) => selected
                .as_ref()
                .filter(|selected| &selected.source_id == source_id)
                .map_or_else(
                    || Some(SourceImages::cache_only(source_id.clone())),
                    |selected| Some(selected.artwork.clone()),
                ),
        }
    }

    pub(crate) fn bind_artwork_tile(
        self: &Rc<Self>,
        tile: &ArtworkTile,
        artwork: ArtworkBinding,
        render_size: i32,
        fetch_size: u32,
    ) {
        self.bind_live_artwork_tile(
            tile,
            LiveArtworkBinding {
                tile: tile.downgrade(),
                source_id: None,
                artwork,
                render_size,
                fetch_size,
                defer_during_route_scroll: true,
                refresh_desktop_on_ready: false,
            },
        );
    }

    pub(crate) fn bind_playback_artwork_tile(
        self: &Rc<Self>,
        tile: &ArtworkTile,
        source_id: &::library::SourceId,
        artwork: ArtworkBinding,
        render_size: i32,
        fetch_size: u32,
    ) {
        self.bind_live_artwork_tile(
            tile,
            LiveArtworkBinding {
                tile: tile.downgrade(),
                source_id: Some(source_id.clone()),
                artwork,
                render_size,
                fetch_size,
                defer_during_route_scroll: false,
                refresh_desktop_on_ready: true,
            },
        );
    }

    fn bind_live_artwork_tile(self: &Rc<Self>, tile: &ArtworkTile, binding: LiveArtworkBinding) {
        if binding.artwork.is_empty() {
            self.cancel_artwork_tile_request(tile);
            tile.bind_missing();
            self.artwork
                .route_interaction
                .deferred
                .borrow_mut()
                .remove(&tile.identity());
            self.remember_artwork_binding(tile, binding);
            return;
        }

        if !artwork_work_allowed(binding.defer_during_route_scroll, tile.area.is_mapped()) {
            self.cancel_artwork_tile_request(tile);
            tile.bind_pending();
            self.artwork
                .route_interaction
                .deferred
                .borrow_mut()
                .remove(&tile.identity());
            self.remember_artwork_binding(tile, binding);
            return;
        }

        let source_id = binding.source_id.clone();
        let artwork = binding.artwork.clone();
        let render_size = binding.render_size;
        let fetch_size_cap = binding.fetch_size;
        let refresh_desktop_on_ready = binding.refresh_desktop_on_ready;
        let cache_only =
            binding.defer_during_route_scroll && self.artwork.route_interaction.active.get();

        let (fetch_size, render_size) =
            cover_request_sizes(render_size, fetch_size_cap, self.artwork_scale());
        let external = artwork_external_policy(&self.settings.current.borrow());
        let Some(source) = self.artwork_source(source_id.as_ref()) else {
            self.cancel_artwork_tile_request(tile);
            tile.bind_pending();
            self.remember_artwork_binding(tile, binding);
            return;
        };
        let texture_source_id = source.source_id.clone();
        let request = ArtworkRequest::new(artwork, fetch_size, render_size).with_external(external);
        let prepared = self.products.artwork.prepare(source, request);
        if cache_only && prepared.ready.is_none() {
            let preview = prepared
                .preview
                .as_ref()
                .and_then(|image| self.texture_for_decoded(&texture_source_id, Arc::clone(image)));
            self.defer_route_artwork_binding(tile, binding, preview);
            return;
        }

        self.artwork
            .route_interaction
            .deferred
            .borrow_mut()
            .remove(&tile.identity());
        self.remember_artwork_binding(tile, binding);
        let outcome = tile.bind_selected_cover(
            prepared.identity.visual.clone(),
            prepared.identity.request.clone(),
        );
        if !artwork_binding_needs_work(
            outcome.request_needed,
            prepared.ready.is_some(),
            outcome.terminal_missing,
            tile.has_artwork_request(),
        ) {
            return;
        }
        if let Some(image) = prepared.ready.as_ref() {
            self.cancel_artwork_tile_request(tile);
            if let Some(texture) = self.texture_for_decoded(&texture_source_id, Arc::clone(image)) {
                tile.set_texture_if_current(outcome.generation, texture);
            } else {
                tile.set_fallback_if_current(outcome.generation);
            }
            return;
        }
        if let Some(image) = prepared.preview.as_ref()
            && let Some(texture) = self.texture_for_decoded(&texture_source_id, Arc::clone(image))
        {
            tile.set_texture_if_current(outcome.generation, texture);
        }
        if !outcome.request_changed && tile.has_artwork_request() {
            return;
        }
        self.cancel_artwork_tile_request(tile);

        match self.products.artwork.request_prepared(prepared) {
            Ok(load) => match load {
                artwork::ArtworkLoad::Pending(pending) => {
                    self.start_artwork_tile_request(
                        tile,
                        outcome.generation,
                        texture_source_id,
                        refresh_desktop_on_ready,
                        pending,
                    );
                }
                artwork::ArtworkLoad::Ready(image) => {
                    if let Some(texture) = self.texture_for_decoded(&texture_source_id, image) {
                        tile.set_texture_if_current(outcome.generation, texture);
                    } else {
                        tile.set_fallback_if_current(outcome.generation);
                    }
                }
                artwork::ArtworkLoad::Missing => {
                    tile.set_missing_if_current(outcome.generation);
                }
            },
            Err(error) => {
                warn!(%error, "failed to start artwork request");
                tile.set_fallback_if_current(outcome.generation);
            }
        }
    }

    fn start_artwork_tile_request(
        self: &Rc<Self>,
        tile: &ArtworkTile,
        generation: u64,
        source_id: ::library::SourceId,
        refresh_desktop_on_ready: bool,
        pending: artwork::PendingArtwork,
    ) {
        let tile_weak = tile.downgrade();
        let shell = Rc::downgrade(self);
        let startup_prime = self.reserve_startup_cover_prime();
        let request = glib::spawn_future_local(async move {
            let outcome = pending.finish().await;
            let Some(tile) = tile_weak.upgrade() else {
                return;
            };
            let Some(shell) = shell.upgrade() else {
                return;
            };
            let ready = match outcome {
                artwork::ArtworkOutcome::Ready(image) => {
                    if let Some(texture) = shell.texture_for_decoded(&source_id, image) {
                        tile.set_texture_if_current(generation, texture)
                    } else {
                        tile.set_fallback_if_current(generation);
                        false
                    }
                }
                artwork::ArtworkOutcome::Missing => {
                    tile.set_missing_if_current(generation);
                    false
                }
                artwork::ArtworkOutcome::Failed(error) => {
                    warn!(%error, "artwork request failed");
                    tile.set_fallback_if_current(generation);
                    false
                }
                artwork::ArtworkOutcome::Invalidated => {
                    tile.set_fallback_if_current(generation);
                    false
                }
            };
            if ready && refresh_desktop_on_ready {
                let player = shell.selected_playback().as_deref().cloned();
                shell.refresh_now_playing_notification(player.as_ref());
                shell.update_media_controls();
            }
            drop(startup_prime);
        });
        tile.replace_artwork_request(request);
    }

    fn defer_route_artwork_binding(
        self: &Rc<Self>,
        tile: &ArtworkTile,
        binding: LiveArtworkBinding,
        preview: Option<gtk::gdk::Texture>,
    ) {
        self.cancel_artwork_tile_request(tile);
        let generation = tile.bind_pending();
        if let Some(texture) = preview {
            tile.set_texture_if_current(generation, texture);
        }
        self.remember_artwork_binding(tile, binding);
        self.artwork
            .route_interaction
            .deferred
            .borrow_mut()
            .insert(tile.identity());
    }

    pub(crate) fn clear_artwork_tile(self: &Rc<Self>, tile: &ArtworkTile) {
        self.artwork
            .live_bindings
            .borrow_mut()
            .remove(&tile.identity());
        self.artwork
            .route_interaction
            .deferred
            .borrow_mut()
            .remove(&tile.identity());
        self.cancel_artwork_tile_request(tile);
        tile.clear_image();
    }

    fn texture_for_decoded(
        &self,
        source_id: &::library::SourceId,
        image: Arc<artwork::DecodedImage>,
    ) -> Option<gtk::gdk::Texture> {
        self.artwork.textures.borrow_mut().texture(source_id, image)
    }

    fn try_retain_source_warm_texture(
        &self,
        source_id: &::library::SourceId,
        image: Arc<artwork::DecodedImage>,
    ) -> bool {
        self.artwork
            .textures
            .borrow_mut()
            .try_retain_source_warm_texture(source_id, image)
    }

    pub(crate) fn release_artwork_textures(&self, source_id: &::library::SourceId) {
        self.artwork.textures.borrow_mut().release_source(source_id);
    }

    fn remember_artwork_binding(self: &Rc<Self>, tile: &ArtworkTile, binding: LiveArtworkBinding) {
        let mapped_shell = Rc::downgrade(self);
        let cleanup_shell = Rc::downgrade(self);
        tile.install_lifecycle_hooks_once(
            move |identity| {
                let Some(shell) = mapped_shell.upgrade() else {
                    return;
                };
                let binding = shell.artwork.live_bindings.borrow().get(&identity).cloned();
                let Some(binding) = binding else {
                    return;
                };
                let Some(tile) = binding.tile.upgrade() else {
                    return;
                };
                shell.bind_live_artwork_tile(&tile, binding);
            },
            move |identity| {
                let Some(shell) = cleanup_shell.upgrade() else {
                    return;
                };
                shell.release_artwork_tile_registration(identity);
            },
        );
        tile.mark_artwork_bound();
        self.artwork
            .live_bindings
            .borrow_mut()
            .insert(tile.identity(), binding);
    }

    pub(crate) fn refresh_artwork_bindings(self: &Rc<Self>) {
        let defer_route_bindings = self.artwork.route_interaction.active.get();
        let bindings = {
            let mut bindings = self.artwork.live_bindings.borrow_mut();
            let mut deferred = self.artwork.route_interaction.deferred.borrow_mut();
            bindings.retain(|_, binding| binding.tile.is_bound());
            bindings
                .iter()
                .filter_map(|(identity, binding)| {
                    if defer_route_bindings && binding.defer_during_route_scroll {
                        deferred.insert(*identity);
                        None
                    } else {
                        Some(binding.clone())
                    }
                })
                .collect::<Vec<_>>()
        };
        for binding in bindings {
            let Some(tile) = binding.tile.upgrade() else {
                continue;
            };
            self.bind_live_artwork_tile(&tile, binding);
        }
    }

    pub(crate) fn install_route_artwork_interaction(self: &Rc<Self>, adjustment: &gtk::Adjustment) {
        let shell = Rc::downgrade(self);
        replace_route_artwork_adjustment_handler(
            &self.artwork.route_interaction,
            adjustment,
            move || {
                let Some(shell) = shell.upgrade() else {
                    return;
                };
                shell.defer_route_artwork_until_scroll_settles();
            },
        );
    }

    pub(crate) fn cancel_route_artwork_interaction(&self) {
        cancel_route_artwork_settle(&self.artwork.route_interaction);
    }

    fn defer_route_artwork_until_scroll_settles(self: &Rc<Self>) {
        let shell = Rc::downgrade(self);
        defer_route_artwork_settle(
            &glib::MainContext::default(),
            &self.artwork.route_interaction,
            ROUTE_ARTWORK_SCROLL_SETTLE,
            move || {
                let Some(shell) = shell.upgrade() else {
                    return;
                };
                shell.refresh_deferred_route_artwork_bindings();
            },
        );
    }

    fn refresh_deferred_route_artwork_bindings(self: &Rc<Self>) {
        let deferred = std::mem::take(&mut *self.artwork.route_interaction.deferred.borrow_mut());
        let bindings = {
            let mut bindings = self.artwork.live_bindings.borrow_mut();
            bindings.retain(|_, binding| binding.tile.is_bound());
            deferred
                .into_iter()
                .filter_map(|identity| bindings.get(&identity).cloned())
                .collect::<Vec<_>>()
        };
        for binding in bindings {
            let Some(tile) = binding.tile.upgrade() else {
                continue;
            };
            self.bind_live_artwork_tile(&tile, binding);
        }
    }

    fn cancel_artwork_tile_request(self: &Rc<Self>, tile: &ArtworkTile) {
        tile.cancel_artwork_request();
    }

    fn release_artwork_tile_registration(self: &Rc<Self>, identity: usize) {
        self.artwork.live_bindings.borrow_mut().remove(&identity);
        self.artwork
            .route_interaction
            .deferred
            .borrow_mut()
            .remove(&identity);
    }

    pub(crate) fn current_playback_cached_artwork_path(
        &self,
        source_id: &::library::SourceId,
        media: &playback::CurrentMedia,
        preferred_size: u32,
    ) -> Option<PlaybackArtworkPath> {
        let candidates = ArtworkBinding::track(&media.track);
        let settings = self.settings.current.borrow().clone();
        let external = artwork_external_policy(&settings);
        let request =
            ArtworkRequest::new(candidates, preferred_size, preferred_size).with_external(external);
        let path = self.products.artwork.cache_only_file(source_id, &request)?;
        Some(PlaybackArtworkPath { path })
    }

    pub(crate) fn reset_cover_pipeline_state(&self) {
        self.cancel_source_thumbnail_warm();
        for binding in self.artwork.live_bindings.borrow().values() {
            if let Some(tile) = binding.tile.upgrade() {
                tile.cancel_artwork_request();
            }
        }
        self.finish_startup_cover_prime_gate();
    }

    pub(in crate::shell) fn begin_startup_cover_prime(&self) {
        self.artwork.startup_prime.begin();
    }

    pub(in crate::shell) fn startup_cover_prime_pending_count(&self) -> usize {
        self.artwork.startup_prime.pending()
    }

    pub(in crate::shell) fn finish_startup_cover_prime_gate(&self) {
        self.artwork.startup_prime.finish();
    }

    fn reserve_startup_cover_prime(self: &Rc<Self>) -> Option<StartupArtworkLease> {
        let generation = self.artwork.startup_prime.reserve()?;
        Some(StartupArtworkLease {
            shell: Rc::downgrade(self),
            generation,
        })
    }

    pub(in crate::shell) fn start_source_thumbnail_warm(self: &Rc<Self>) {
        self.cancel_source_thumbnail_warm();
        let Some(selected) = self.selected_library().as_deref().cloned() else {
            return;
        };
        self.artwork
            .textures
            .borrow_mut()
            .release_source_warm_textures(&selected.source_id);
        let render_size = cover_decode_size(48, MEDIUM_COVER_SIZE, self.artwork_scale());
        let binding_limit = self
            .artwork
            .textures
            .borrow()
            .source_thumbnail_warm_limit(render_size);
        if binding_limit == 0 {
            return;
        }
        let generation = self.artwork.thumbnail_warm.generation.get();
        let shell = Rc::downgrade(self);
        let loaded = Arc::clone(&selected.library);
        let music_folder_id = selected.music_folder_id.clone();
        let prefer_server_playlist_covers =
            self.settings.current.borrow().prefer_server_playlist_covers;
        let mut external = artwork_external_policy(&self.settings.current.borrow());
        external.allow_network = false;
        let task = glib::spawn_future_local(async move {
            let bindings = match gtk::gio::spawn_blocking(move || {
                source_thumbnail_bindings(
                    &loaded,
                    music_folder_id.as_ref(),
                    prefer_server_playlist_covers,
                    binding_limit,
                )
            })
            .await
            {
                Ok(Ok(bindings)) => bindings,
                Ok(Err(error)) => {
                    warn!(%error, "failed to read selected-source artwork");
                    return;
                }
                Err(_) => {
                    warn!("selected-source artwork read panicked");
                    return;
                }
            };
            let Some(shell) = shell.upgrade() else {
                return;
            };
            if shell.artwork.thumbnail_warm.generation.get() != generation {
                return;
            }
            let mut bindings = bindings.iter();
            let mut pending = VecDeque::with_capacity(THUMBNAIL_WARM_WINDOW);
            loop {
                while pending.len() < THUMBNAIL_WARM_WINDOW {
                    let Some(binding) = bindings.next() else {
                        break;
                    };
                    let request =
                        ArtworkRequest::new(binding.clone(), MEDIUM_COVER_SIZE, render_size)
                            .with_external(external.clone());
                    let prepared = shell
                        .products
                        .artwork
                        .prepare(selected.artwork.clone(), request);
                    match shell.products.artwork.warm_prepared(prepared) {
                        Ok(artwork::ArtworkLoad::Ready(image)) => {
                            if !shell.try_retain_source_warm_texture(&selected.source_id, image) {
                                return;
                            }
                        }
                        Ok(artwork::ArtworkLoad::Pending(request)) => {
                            pending.push_back(request);
                        }
                        Ok(artwork::ArtworkLoad::Missing) => {}
                        Err(error) => {
                            warn!(%error, "failed to start source thumbnail warm");
                        }
                    }
                }
                let Some(request) = pending.pop_front() else {
                    break;
                };
                let outcome = request.finish().await;
                if shell.artwork.thumbnail_warm.generation.get() != generation {
                    return;
                }
                if let artwork::ArtworkOutcome::Ready(image) = outcome {
                    if !shell.try_retain_source_warm_texture(&selected.source_id, image) {
                        return;
                    }
                }
            }
        });
        self.artwork.thumbnail_warm.task.replace(Some(task));
    }

    pub(in crate::shell) fn cancel_source_thumbnail_warm(&self) {
        let state = &self.artwork.thumbnail_warm;
        state
            .generation
            .set(state.generation.get().wrapping_add(1).max(1));
        if let Some(task) = state.task.borrow_mut().take() {
            task.abort();
        }
    }
}

fn source_thumbnail_bindings(
    loaded: &Arc<::library::Library>,
    music_folder_id: Option<&::library::MusicFolderId>,
    prefer_server_playlist_covers: bool,
    limit: usize,
) -> Result<Arc<[ArtworkBinding]>, ::library::LibraryQueryError> {
    if limit == 0 {
        return Ok(Arc::default());
    }
    let mut bindings = Vec::new();
    let mut seen = HashSet::new();

    for album in loaded.albums(music_folder_id)?.iter().take(limit) {
        if !push_source_thumbnail_binding(
            &mut bindings,
            &mut seen,
            ArtworkBinding::album_artwork(&album.artwork),
            limit,
        ) {
            return Ok(bindings.into());
        }
    }
    for artist in loaded
        .artists(music_folder_id)?
        .iter()
        .chain(loaded.album_artists(music_folder_id)?.iter())
        .take(limit)
    {
        if !push_source_thumbnail_binding(
            &mut bindings,
            &mut seen,
            ArtworkBinding::artist(&artist.artwork),
            limit,
        ) {
            return Ok(bindings.into());
        }
    }
    for genre in loaded.genres(music_folder_id)?.iter().take(limit) {
        for binding in ArtworkBinding::genre_slots(&genre.genre, &genre.representative_albums) {
            if !push_source_thumbnail_binding(&mut bindings, &mut seen, binding, limit) {
                return Ok(bindings.into());
            }
        }
    }
    for mood in loaded.moods(music_folder_id)?.iter().take(limit) {
        for binding in ArtworkBinding::mood_slots(&mood.mood, &mood.representative_albums) {
            if !push_source_thumbnail_binding(&mut bindings, &mut seen, binding, limit) {
                return Ok(bindings.into());
            }
        }
    }
    for playlist in loaded.playlists()?.iter().take(limit) {
        for binding in ArtworkBinding::playlist_slots(
            &playlist.playlist,
            &playlist.representative_albums,
            prefer_server_playlist_covers,
        ) {
            if !push_source_thumbnail_binding(&mut bindings, &mut seen, binding, limit) {
                return Ok(bindings.into());
            }
        }
    }
    for playlist in loaded.smart_playlists(music_folder_id)?.iter().take(limit) {
        for binding in ArtworkBinding::smart_playlist_slots(
            &playlist.smart_playlist,
            &playlist.representative_albums,
        ) {
            if !push_source_thumbnail_binding(&mut bindings, &mut seen, binding, limit) {
                return Ok(bindings.into());
            }
        }
    }
    let tracks = loaded.track_list(music_folder_id, ::library::TrackSort::Title, false)?;
    for position in 0..tracks.len().min(limit) {
        let Some(track) = tracks.track(position)? else {
            return Err(::library::LibraryQueryError::StaleTrackSelection);
        };
        if !push_source_thumbnail_binding(
            &mut bindings,
            &mut seen,
            ArtworkBinding::track(&track),
            limit,
        ) {
            return Ok(bindings.into());
        }
    }

    Ok(bindings.into())
}

fn push_source_thumbnail_binding(
    bindings: &mut Vec<ArtworkBinding>,
    seen: &mut HashSet<ArtworkBinding>,
    binding: ArtworkBinding,
    limit: usize,
) -> bool {
    if !binding.is_empty() && seen.insert(binding.clone()) {
        bindings.push(binding);
    }
    bindings.len() < limit
}

fn defer_route_artwork_settle(
    context: &glib::MainContext,
    interaction: &Rc<RouteArtworkInteraction>,
    delay: Duration,
    settle: impl Fn() + 'static,
) {
    interaction.active.set(true);
    interaction.deadline.set(Some(Instant::now() + delay));
    if interaction.settle.borrow().is_some() {
        return;
    }
    schedule_route_artwork_settle(context.clone(), interaction, delay, Rc::new(settle));
}

fn schedule_route_artwork_settle(
    context: glib::MainContext,
    interaction: &Rc<RouteArtworkInteraction>,
    delay: Duration,
    settle: Rc<dyn Fn()>,
) {
    let interaction_weak = Rc::downgrade(interaction);
    let next_context = context.clone();
    let pending = context.spawn_local(async move {
        glib::timeout_future(delay).await;
        let Some(interaction) = interaction_weak.upgrade() else {
            return;
        };
        interaction.settle.borrow_mut().take();
        let remaining = interaction.deadline.get().and_then(|deadline| {
            let remaining = deadline.saturating_duration_since(Instant::now());
            (!remaining.is_zero()).then_some(remaining)
        });
        if let Some(remaining) = remaining {
            schedule_route_artwork_settle(next_context, &interaction, remaining, settle);
            return;
        }
        interaction.deadline.set(None);
        interaction.active.set(false);
        settle();
    });
    interaction.settle.replace(Some(pending));
}

fn cancel_route_artwork_settle(interaction: &RouteArtworkInteraction) {
    disconnect_route_artwork_adjustment_handler(interaction);
    if let Some(pending) = interaction.settle.borrow_mut().take() {
        pending.abort();
    }
    interaction.deadline.set(None);
    interaction.active.set(false);
    interaction.deferred.borrow_mut().clear();
}

fn replace_route_artwork_adjustment_handler(
    interaction: &RouteArtworkInteraction,
    adjustment: &gtk::Adjustment,
    changed: impl Fn() + 'static,
) {
    replace_route_artwork_signal_handler(interaction, adjustment, || {
        adjustment.connect_value_changed(move |_| changed())
    });
}

fn replace_route_artwork_signal_handler(
    interaction: &RouteArtworkInteraction,
    object: &impl IsA<glib::Object>,
    connect: impl FnOnce() -> glib::SignalHandlerId,
) {
    disconnect_route_artwork_adjustment_handler(interaction);
    let signal = connect();
    interaction
        .adjustment_handler
        .replace(Some(RouteArtworkAdjustmentHandler {
            object: object.as_ref().downgrade(),
            signal,
        }));
}

fn disconnect_route_artwork_adjustment_handler(interaction: &RouteArtworkInteraction) {
    let Some(handler) = interaction.adjustment_handler.borrow_mut().take() else {
        return;
    };
    let Some(object) = handler.object.upgrade() else {
        return;
    };
    object.disconnect(handler.signal);
}

fn artwork_external_policy(settings: &UiSettings) -> artwork::ExternalPolicy {
    artwork::ExternalPolicy::new(
        settings.external_metadata_enabled,
        settings.allows_external_metadata_lookup(),
        settings.lastfm_api_key.clone(),
    )
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::Duration;

    use gtk::gio;
    use gtk::gio::prelude::ActionExt;
    use library::{ImageRef, SourceId, TrackId};

    use super::{
        ArtworkBinding, RouteArtworkInteraction, StartupArtworkPrime, artwork_binding_needs_work,
        artwork_work_allowed, cover_decode_size, cover_request_sizes, defer_route_artwork_settle,
        disconnect_route_artwork_adjustment_handler, replace_route_artwork_signal_handler,
        source_thumbnail_bindings,
    };
    use crate::test_support::{album, loaded_source, playlist, playlist_snapshot};

    #[test]
    fn cover_decode_size_uses_the_surface_scale_without_exceeding_the_fetch_size() {
        assert_eq!(cover_decode_size(48, 96, 1.0), 48);
        assert_eq!(cover_decode_size(48, 96, 1.25), 60);
        assert_eq!(cover_decode_size(48, 96, 1.5), 72);
        assert_eq!(cover_decode_size(48, 96, 1.75), 84);
        assert_eq!(cover_decode_size(48, 96, 2.0), 96);
        assert_eq!(cover_decode_size(160, 256, 2.0), 256);
        assert_eq!(cover_decode_size(512, 256, 1.0), 256);
    }

    #[test]
    fn artwork_requests_use_physical_size_and_canonical_fetch_tiers() {
        assert_eq!(cover_request_sizes(200, 512, 1.0), (256, 200));
        assert_eq!(cover_request_sizes(200, 512, 1.25), (256, 250));
        assert_eq!(cover_request_sizes(200, 512, 1.5), (512, 300));
        assert_eq!(cover_request_sizes(200, 512, 2.0), (512, 400));
        assert_eq!(cover_request_sizes(200, 256, 2.0), (256, 256));
        assert_eq!(cover_request_sizes(48, 96, 2.0), (96, 96));
    }

    #[test]
    fn route_artwork_waits_for_mapping_but_playback_artwork_does_not() {
        assert!(!artwork_work_allowed(true, false));
        assert!(artwork_work_allowed(true, true));
        assert!(artwork_work_allowed(false, false));
    }

    #[test]
    fn cancelled_exact_request_restarts_behind_a_preview() {
        assert!(artwork_binding_needs_work(false, false, false, false));
        assert!(!artwork_binding_needs_work(false, false, false, true));
        assert!(!artwork_binding_needs_work(false, true, false, false));
        assert!(!artwork_binding_needs_work(false, false, true, false));
    }

    #[test]
    fn source_thumbnail_warm_includes_album_and_collection_artwork() {
        let mut album = album(1, "Album");
        album.image_ref = Some(ImageRef::new("album-cover", None));
        let expected_album = ArtworkBinding::album(&album);
        let mut playlist = playlist(1, "Playlist");
        playlist.image_ref = Some(ImageRef::new("playlist-cover", None));
        let expected_playlist = ArtworkBinding::playlist(&playlist, &[], true);
        let loaded = loaded_source(
            SourceId::new("source-thumbnail-warm"),
            vec![album],
            Vec::new(),
            vec![playlist_snapshot(
                playlist,
                std::iter::empty::<(String, TrackId)>(),
            )],
        );

        let bindings = source_thumbnail_bindings(&loaded, None, true, usize::MAX)
            .expect("source artwork projection");

        assert!(bindings.contains(&expected_album));
        assert!(bindings.contains(&expected_playlist));
    }

    #[test]
    fn source_thumbnail_warm_projection_stops_at_its_budget() {
        let albums = (0..50_000)
            .map(|index| {
                let mut album = album(index, format!("Album {index}"));
                album.image_ref = Some(ImageRef::new(format!("cover-{index}"), None));
                album
            })
            .collect();
        let loaded = loaded_source(
            SourceId::new("bounded-thumbnail-warm"),
            albums,
            Vec::new(),
            Vec::new(),
        );

        let bindings =
            source_thumbnail_bindings(&loaded, None, true, 32).expect("bounded artwork projection");

        assert_eq!(bindings.len(), 32);
    }

    #[test]
    fn startup_artwork_completion_only_releases_its_own_reveal_gate() {
        let prime = StartupArtworkPrime::default();
        prime.begin();
        let first = prime.reserve().expect("first cover joins startup gate");
        let second = prime.reserve().expect("second cover joins startup gate");
        assert_eq!(prime.pending(), 2);
        assert!(!prime.release(first));
        assert_eq!(prime.pending(), 1);

        prime.begin();
        let current = prime.reserve().expect("replacement cover joins new gate");
        assert!(!prime.release(second));
        assert_eq!(prime.pending(), 1);
        assert!(prime.release(current));
        assert_eq!(prime.pending(), 0);

        prime.finish();
        assert_eq!(prime.pending(), 0);
    }

    #[test]
    fn scroll_burst_keeps_one_settle_task_and_runs_one_refresh_after_quiescence() {
        let context = gtk::glib::MainContext::new();
        let interaction = Rc::new(RouteArtworkInteraction::default());
        let refresh_count = Rc::new(Cell::new(0));
        let mut settle_source = None;

        for _ in 0..3 {
            let refresh_count = Rc::clone(&refresh_count);
            defer_route_artwork_settle(&context, &interaction, Duration::ZERO, move || {
                refresh_count.set(refresh_count.get() + 1);
            });
            let current_source = interaction
                .settle
                .borrow()
                .as_ref()
                .and_then(gtk::glib::JoinHandle::as_raw_source_id);
            if let Some(settle_source) = settle_source {
                assert_eq!(current_source, Some(settle_source));
            } else {
                settle_source = current_source;
            }
        }

        assert!(interaction.active.get());
        for _ in 0..8 {
            context.iteration(false);
            if !interaction.active.get() {
                break;
            }
        }

        assert!(!interaction.active.get());
        assert!(interaction.settle.borrow().is_none());
        assert_eq!(refresh_count.get(), 1);
    }

    #[test]
    fn replacing_route_adjustment_disconnects_the_previous_route_callback() {
        let interaction = RouteArtworkInteraction::default();
        let previous = gio::SimpleAction::new("previous", None);
        let current = gio::SimpleAction::new("current", None);
        let previous_calls = Rc::new(Cell::new(0));
        let current_calls = Rc::new(Cell::new(0));

        let calls = Rc::clone(&previous_calls);
        replace_route_artwork_signal_handler(&interaction, &previous, || {
            previous.connect_activate(move |_, _| calls.set(calls.get() + 1))
        });
        let calls = Rc::clone(&current_calls);
        replace_route_artwork_signal_handler(&interaction, &current, || {
            current.connect_activate(move |_, _| calls.set(calls.get() + 1))
        });

        previous.activate(None);
        current.activate(None);

        assert_eq!(previous_calls.get(), 0);
        assert_eq!(current_calls.get(), 1);

        disconnect_route_artwork_adjustment_handler(&interaction);
        current.activate(None);
        assert_eq!(current_calls.get(), 1);
    }
}
