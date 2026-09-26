use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use artwork::{ArtworkBinding, ArtworkRequest};
use gtk::glib;
use tracing::{debug, warn};

use rufin_core::settings::app::Settings as UiSettings;

use crate::settings::SettingsState;

pub const THUMB_COVER_SIZE: u32 = 96;
pub const MEDIUM_COVER_SIZE: u32 = 256;
pub const LARGE_COVER_SIZE: u32 = 512;
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaybackArtworkPath {
    pub path: PathBuf,
}

mod animation;
mod cover_group;
pub mod presentation;
mod texture_cache;
mod tile;

pub use cover_group::CoverGroupProjection;
use texture_cache::TextureCache;
pub use tile::{ArtworkTile, ArtworkTileWeak};

pub fn cover_decode_size(display_size: i32, fetch_size: u32, scale: f64) -> u32 {
    let display_size = f64::from(display_size.max(1));
    let scale = if scale.is_finite() {
        scale.max(1.0)
    } else {
        1.0
    };
    let scaled = (display_size * scale).ceil().min(f64::from(u32::MAX)) as u32;
    scaled.min(fetch_size.max(1))
}

pub fn cover_fetch_size_for_display(display_size: i32) -> u32 {
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

pub struct ArtworkState {
    service: artwork::Artwork,
    settings: Rc<SettingsState>,
    scale: Box<dyn Fn() -> f64>,
    startup_ready: Box<dyn Fn()>,
    route_ready: Box<dyn Fn(u64)>,
    playback_ready: Box<dyn Fn()>,
    startup_prime: ArtworkPrime,
    route_prime: ArtworkPrime,
    route_registration_open: Cell<bool>,
    route_registrations: RefCell<Vec<(glib::WeakRef<gtk::Widget>, Box<dyn FnOnce()>)>>,
    textures: RefCell<TextureCache>,
    animations: RefCell<animation::Animations>,
}

#[derive(Default)]
struct ArtworkPrime {
    active: Cell<bool>,
    generation: Cell<u64>,
    pending: Cell<usize>,
}

impl ArtworkPrime {
    fn begin(&self) -> u64 {
        self.generation
            .set(self.generation.get().wrapping_add(1).max(1));
        self.pending.set(0);
        self.active.set(true);
        self.generation.get()
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
    shell: std::rc::Weak<ArtworkState>,
    generation: u64,
}

struct RouteArtworkLease {
    shell: std::rc::Weak<ArtworkState>,
    generation: u64,
}

impl Drop for StartupArtworkLease {
    fn drop(&mut self) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        if shell.startup_prime.release(self.generation) {
            (shell.startup_ready)();
        }
    }
}

impl Drop for RouteArtworkLease {
    fn drop(&mut self) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        if shell.route_prime.release(self.generation) {
            (shell.route_ready)(self.generation);
        }
    }
}

impl ArtworkState {
    pub fn new(
        service: artwork::Artwork,
        settings: Rc<SettingsState>,
        scale: Box<dyn Fn() -> f64>,
        startup_ready: Box<dyn Fn()>,
        route_ready: Box<dyn Fn(u64)>,
        playback_ready: Box<dyn Fn()>,
    ) -> Rc<Self> {
        Rc::new(Self {
            service,
            settings,
            scale,
            startup_ready,
            route_ready,
            playback_ready,
            startup_prime: Default::default(),
            route_prime: Default::default(),
            route_registration_open: Cell::new(false),
            route_registrations: RefCell::new(Vec::new()),
            textures: RefCell::new(Default::default()),
            animations: RefCell::new(Default::default()),
        })
    }

    fn artwork_scale(&self) -> f64 {
        (self.scale)()
    }

    pub fn bind_artwork_tile(
        self: &Rc<Self>,
        tile: &ArtworkTile,
        artwork: ArtworkBinding,
        render_size: i32,
        fetch_size: u32,
    ) {
        self.bind_artwork_tile_request(tile, artwork, render_size, fetch_size, false, false);
    }

    pub fn bind_cache_only_artwork_tile(
        self: &Rc<Self>,
        tile: &ArtworkTile,
        artwork: ArtworkBinding,
        render_size: i32,
        fetch_size: u32,
    ) {
        self.bind_artwork_tile_request(tile, artwork, render_size, fetch_size, false, true);
    }

    pub fn bind_playback_artwork_tile(
        self: &Rc<Self>,
        tile: &ArtworkTile,
        artwork: ArtworkBinding,
        render_size: i32,
        fetch_size: u32,
    ) {
        self.bind_artwork_tile_request(tile, artwork, render_size, fetch_size, true, false);
    }

    fn bind_artwork_tile_request(
        self: &Rc<Self>,
        tile: &ArtworkTile,
        artwork: ArtworkBinding,
        render_size: i32,
        fetch_size_cap: u32,
        refresh_desktop_on_ready: bool,
        cache_only: bool,
    ) {
        tile.install_request_cleanup_once();
        tile.set_animation_scale(self.artwork_scale());
        if artwork.stable_identity().is_empty() {
            self.cancel_artwork_tile_request(tile);
            tile.bind_missing();
            return;
        }
        let (fetch_size, render_size) =
            cover_request_sizes(render_size, fetch_size_cap, self.artwork_scale());
        let external = if cache_only {
            cache_only_artwork_external_policy()
        } else {
            artwork_external_policy(&self.settings.current.borrow())
        };
        let request = if refresh_desktop_on_ready {
            ArtworkRequest::original(artwork, LARGE_COVER_SIZE)
        } else {
            ArtworkRequest::new(artwork, fetch_size, render_size)
        }
        .with_external(external);
        let request = if cache_only {
            request.cache_only()
        } else {
            request
        };
        let outcome =
            tile.bind_selected_cover(self.service.key(&request), refresh_desktop_on_ready);
        if !outcome.request_needed {
            return;
        }
        if !outcome.request_changed && tile.has_artwork_request() {
            return;
        }
        self.cancel_artwork_tile_request(tile);

        if self.route_registration_open.get() {
            self.defer_route_artwork_request(
                tile,
                outcome.generation,
                refresh_desktop_on_ready,
                request,
            );
            return;
        }

        self.load_artwork_tile(tile, outcome.generation, refresh_desktop_on_ready, request);
    }

    fn load_artwork_tile(
        self: &Rc<Self>,
        tile: &ArtworkTile,
        generation: u64,
        refresh_desktop_on_ready: bool,
        request: ArtworkRequest,
    ) {
        let key = self.service.key(&request);
        if refresh_desktop_on_ready {
            let existing =
                self.animations
                    .borrow()
                    .existing(&key, tile, generation, self.artwork_scale());
            if let Some((lease, texture)) = existing {
                if tile.set_texture_if_current(generation, texture) {
                    tile.set_animation(lease);
                }
                return;
            }
        }
        if !refresh_desktop_on_ready
            && let Some(texture) = self.textures.borrow_mut().cached_texture(&key)
        {
            tile.set_texture_if_current(generation, texture);
            return;
        }
        let preview = refresh_desktop_on_ready.then(|| {
            ArtworkRequest::new(
                request.binding.clone(),
                MEDIUM_COVER_SIZE,
                MEDIUM_COVER_SIZE,
            )
            .with_external(request.external.clone())
            .cache_only()
        });
        match self.service.load(request) {
            artwork::ArtworkLoad::Pending(pending) => {
                self.start_artwork_tile_request(
                    tile,
                    generation,
                    refresh_desktop_on_ready,
                    pending,
                    preview,
                );
            }
            artwork::ArtworkLoad::Ready(image) => {
                self.apply_artwork(
                    tile,
                    generation,
                    artwork::ArtworkOutcome::Ready(image),
                    refresh_desktop_on_ready,
                );
            }
            artwork::ArtworkLoad::Missing => {
                self.apply_artwork(
                    tile,
                    generation,
                    artwork::ArtworkOutcome::Missing,
                    refresh_desktop_on_ready,
                );
            }
        }
    }

    fn apply_artwork(
        self: &Rc<Self>,
        tile: &ArtworkTile,
        generation: u64,
        outcome: artwork::ArtworkOutcome,
        animate: bool,
    ) -> bool {
        match outcome {
            artwork::ArtworkOutcome::Ready(loaded) => {
                let key = loaded.image.key().clone();
                if let Some(texture) = self.texture_for_decoded(loaded.image) {
                    let current = tile.set_texture_if_current(generation, texture.clone());
                    if current
                        && animate
                        && let Some(bytes) = loaded.original
                    {
                        let lease = self.animations.borrow_mut().attach(
                            key,
                            bytes,
                            tile,
                            generation,
                            self.artwork_scale(),
                            texture,
                        );
                        tile.set_animation(lease);
                    }
                    current
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
        }
    }

    fn start_artwork_tile_request(
        self: &Rc<Self>,
        tile: &ArtworkTile,
        generation: u64,
        refresh_desktop_on_ready: bool,
        pending: artwork::PendingArtwork,
        preview: Option<ArtworkRequest>,
    ) {
        let preview = preview.and_then(|request| {
            self.service
                .cache_only_file(&request)
                .map(|_| self.service.load(request))
        });
        let tile_weak = tile.downgrade();
        let shell = Rc::downgrade(self);
        let mut startup_prime = self.reserve_startup_cover_prime();
        let mut route_prime = self.reserve_route_cover_prime();
        let request = glib::spawn_future_local(async move {
            let preview = match preview {
                Some(artwork::ArtworkLoad::Ready(loaded)) => Some(loaded),
                Some(artwork::ArtworkLoad::Pending(preview)) => match preview.finish().await {
                    artwork::ArtworkOutcome::Ready(loaded) => Some(loaded),
                    _ => None,
                },
                _ => None,
            };
            let preview_shown = preview.is_some_and(|loaded| {
                let (Some(tile), Some(shell)) = (tile_weak.upgrade(), shell.upgrade()) else {
                    return false;
                };
                shell
                    .texture_for_decoded(loaded.image)
                    .is_some_and(|texture| tile.set_preview_if_current(generation, texture))
            });
            if preview_shown {
                if let Some(shell) = shell.upgrade() {
                    (shell.playback_ready)();
                }
                drop(startup_prime.take());
                drop(route_prime.take());
            }
            let outcome = pending.finish().await;
            let Some(tile) = tile_weak.upgrade() else {
                return;
            };
            let Some(shell) = shell.upgrade() else {
                return;
            };
            let ready = if preview_shown
                && matches!(
                    outcome,
                    artwork::ArtworkOutcome::Missing | artwork::ArtworkOutcome::Failed(_)
                ) {
                if let artwork::ArtworkOutcome::Failed(error) = outcome {
                    warn!(%error, "original cover request failed");
                }
                tile.complete_preview_if_current(generation);
                false
            } else {
                shell.apply_artwork(&tile, generation, outcome, refresh_desktop_on_ready)
            };
            if ready && refresh_desktop_on_ready {
                (shell.playback_ready)();
            }
            drop(startup_prime);
            drop(route_prime);
        });
        tile.replace_artwork_request(request);
    }

    pub fn clear_artwork_tile(self: &Rc<Self>, tile: &ArtworkTile) {
        self.cancel_artwork_tile_request(tile);
        tile.clear_image();
    }

    fn texture_for_decoded(&self, image: Arc<artwork::DecodedImage>) -> Option<gtk::gdk::Texture> {
        self.textures.borrow_mut().texture(image)
    }

    fn cancel_artwork_tile_request(self: &Rc<Self>, tile: &ArtworkTile) {
        tile.cancel_artwork_request();
    }

    pub fn current_playback_cached_artwork_path(
        &self,
        media: &playback::CurrentMedia,
        preferred_size: u32,
    ) -> Option<PlaybackArtworkPath> {
        let candidates = media
            .artwork_binding
            .as_deref()
            .map(ArtworkBinding::opaque)
            .unwrap_or_default();
        let settings = self.settings.current.borrow().clone();
        let external = artwork_external_policy(&settings);
        let request =
            ArtworkRequest::new(candidates, preferred_size, preferred_size).with_external(external);
        let path = self.service.cache_only_file(&request)?;
        Some(PlaybackArtworkPath { path })
    }

    pub fn reset_cover_pipeline_state(&self) {
        self.finish_startup_cover_prime_gate();
        self.finish_route_cover_prime_gate();
    }

    pub fn begin_startup_cover_prime(&self) {
        self.startup_prime.begin();
    }

    pub fn startup_cover_prime_pending_count(&self) -> usize {
        self.startup_prime.pending()
    }

    pub fn finish_startup_cover_prime_gate(&self) {
        self.startup_prime.finish();
    }

    fn reserve_startup_cover_prime(self: &Rc<Self>) -> Option<StartupArtworkLease> {
        let generation = self.startup_prime.reserve()?;
        Some(StartupArtworkLease {
            shell: Rc::downgrade(self),
            generation,
        })
    }

    pub fn begin_route_cover_prime(&self) -> u64 {
        self.route_registration_open.set(true);
        self.route_registrations.borrow_mut().clear();
        self.route_prime.begin()
    }

    pub fn close_route_cover_registration(&self, generation: u64, route_viewport: &gtk::Widget) {
        if self.route_prime.generation.get() == generation && self.route_registration_open.get() {
            let registrations = self.route_registrations.take();
            let registered = registrations.len();
            let mut warm = Vec::new();
            for (widget, start) in registrations {
                if artwork_tile_intersects_viewport(&widget, route_viewport) {
                    start();
                } else {
                    warm.push(start);
                }
            }
            self.route_registration_open.set(false);
            debug!(
                generation,
                registered,
                visible = registered - warm.len(),
                pending = self.route_prime.pending(),
                "route artwork registration closed"
            );
            for start in warm {
                start();
            }
        }
    }

    pub fn route_cover_prime_ready(&self, generation: u64) -> bool {
        self.route_prime.generation.get() == generation
            && !self.route_registration_open.get()
            && self.route_prime.pending() == 0
    }

    pub fn finish_route_cover_prime_gate(&self) {
        self.route_registration_open.set(false);
        self.route_registrations.borrow_mut().clear();
        self.route_prime.finish();
    }

    fn defer_route_artwork_request(
        self: &Rc<Self>,
        tile: &ArtworkTile,
        generation: u64,
        refresh_desktop_on_ready: bool,
        request: ArtworkRequest,
    ) {
        debug_assert!(self.route_registration_open.get());
        let widget = tile.widget();
        let tile = tile.downgrade();
        let shell = Rc::downgrade(self);
        let mut registrations = self.route_registrations.borrow_mut();
        registrations.retain(|(registered, _)| {
            registered
                .upgrade()
                .is_some_and(|registered| registered != widget)
        });
        registrations.push((
            widget.downgrade(),
            Box::new(move || {
                let (Some(shell), Some(tile)) = (shell.upgrade(), tile.upgrade()) else {
                    return;
                };
                if tile.generation_is_current(generation) {
                    shell.load_artwork_tile(&tile, generation, refresh_desktop_on_ready, request);
                }
            }),
        ));
    }

    fn reserve_route_cover_prime(self: &Rc<Self>) -> Option<RouteArtworkLease> {
        if !self.route_registration_open.get() {
            return None;
        }
        let generation = self.route_prime.reserve()?;
        Some(RouteArtworkLease {
            shell: Rc::downgrade(self),
            generation,
        })
    }
}

fn artwork_tile_intersects_viewport(
    widget: &glib::WeakRef<gtk::Widget>,
    route_viewport: &gtk::Widget,
) -> bool {
    let Some(widget) = widget.upgrade().filter(|widget| widget.is_mapped()) else {
        return false;
    };
    let viewport = widget
        .ancestor(gtk::ScrolledWindow::static_type())
        .unwrap_or_else(|| route_viewport.clone());
    let Some(bounds) = widget.compute_bounds(&viewport) else {
        return false;
    };
    bounds.x() < viewport.width() as f32
        && bounds.y() < viewport.height() as f32
        && bounds.x() + bounds.width() > 0.0
        && bounds.y() + bounds.height() > 0.0
}

fn artwork_external_policy(settings: &UiSettings) -> artwork::ExternalPolicy {
    artwork::ExternalPolicy::new(
        settings.external_metadata_enabled,
        settings.allows_external_metadata_lookup(),
        settings.lastfm_api_key.clone(),
    )
}

fn cache_only_artwork_external_policy() -> artwork::ExternalPolicy {
    artwork::ExternalPolicy::new(true, false, String::new()).with_musicbrainz(false)
}

#[cfg(test)]
mod tests {
    use super::{
        ArtworkPrime, cache_only_artwork_external_policy, cover_decode_size, cover_request_sizes,
    };

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
    fn drag_preview_artwork_never_starts_external_network_work() {
        let policy = cache_only_artwork_external_policy();
        assert!(policy.allow_cached);
        assert!(!policy.allow_network);
        assert!(!policy.allow_musicbrainz);
    }

    #[test]
    fn startup_artwork_completion_only_releases_its_own_reveal_gate() {
        let prime = ArtworkPrime::default();
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
}

pub async fn texture_from_image(
    image: Arc<sources::ImageBytes>,
    size: u32,
) -> Result<gtk::gdk::Texture, String> {
    let pixels = gtk::gio::spawn_blocking(move || ::artwork::decode_rgba(&image.bytes, size))
        .await
        .expect("artwork decoder worker completed")
        .map_err(|error| error.to_string())?;
    let width = pixels.width() as i32;
    let height = pixels.height() as i32;
    let stride = pixels.row_stride() as usize;
    Ok(gtk::gdk::MemoryTexture::new(
        width,
        height,
        gtk::gdk::MemoryFormat::R8g8b8a8,
        &gtk::glib::Bytes::from_owned(pixels),
        stride,
    )
    .upcast())
}
