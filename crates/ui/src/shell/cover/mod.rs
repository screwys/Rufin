use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use artwork::{ArtworkBinding, ArtworkRequest, SourceImages};
use gtk::glib;
use tracing::warn;

use crate::Settings as UiSettings;

use super::Shell;

pub(crate) const THUMB_COVER_SIZE: u32 = 96;
pub(crate) const MEDIUM_COVER_SIZE: u32 = 256;
pub(crate) const LARGE_COVER_SIZE: u32 = 512;
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlaybackArtworkPath {
    pub(crate) path: PathBuf,
}

pub(crate) mod presentation;
mod texture_cache;
mod tile;
mod tiles;

use texture_cache::TextureCache;
pub(crate) use tile::ArtworkTile;
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

const fn artwork_binding_needs_work(
    tile_needs_request: bool,
    exact_ready: bool,
    terminal_missing: bool,
    request_active: bool,
) -> bool {
    tile_needs_request || (!exact_ready && !terminal_missing && !request_active)
}

pub(super) struct ArtworkState {
    pub(super) startup_prime: StartupArtworkPrime,
    pub(super) textures: RefCell<TextureCache>,
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
    fn artwork_scale(&self) -> f64 {
        self.chrome.window.surface().map_or_else(
            || f64::from(self.chrome.window.scale_factor()),
            |surface| surface.scale(),
        )
    }

    fn artwork_source(&self, source_id: Option<&::sources::SourceId>) -> Option<SourceImages> {
        let selected = self.selected_library();
        match source_id {
            None => selected.as_ref().map(|selected| selected.artwork.clone()),
            Some(source_id) => selected
                .as_ref()
                .filter(|selected| &selected.artwork.source_id == source_id)
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
        self.bind_artwork_tile_request(tile, None, artwork, render_size, fetch_size, false);
    }

    pub(crate) fn bind_playback_artwork_tile(
        self: &Rc<Self>,
        tile: &ArtworkTile,
        source_id: &::sources::SourceId,
        artwork: ArtworkBinding,
        render_size: i32,
        fetch_size: u32,
    ) {
        self.bind_artwork_tile_request(
            tile,
            Some(source_id.clone()),
            artwork,
            render_size,
            fetch_size,
            true,
        );
    }

    fn bind_artwork_tile_request(
        self: &Rc<Self>,
        tile: &ArtworkTile,
        source_id: Option<::sources::SourceId>,
        artwork: ArtworkBinding,
        render_size: i32,
        fetch_size_cap: u32,
        refresh_desktop_on_ready: bool,
    ) {
        tile.install_request_cleanup_once();
        if artwork.stable_identity().is_empty() {
            self.cancel_artwork_tile_request(tile);
            tile.bind_missing();
            return;
        }
        let (fetch_size, render_size) =
            cover_request_sizes(render_size, fetch_size_cap, self.artwork_scale());
        let external = artwork_external_policy(&self.settings.current.borrow());
        let Some(source) = self.artwork_source(source_id.as_ref()) else {
            self.cancel_artwork_tile_request(tile);
            tile.bind_pending();
            return;
        };
        let texture_source_id = source.source_id.clone();
        let request = ArtworkRequest::new(artwork, fetch_size, render_size).with_external(external);
        let prepared = self.products.artwork.prepare(source, request);
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
        source_id: ::sources::SourceId,
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

    pub(crate) fn clear_artwork_tile(self: &Rc<Self>, tile: &ArtworkTile) {
        self.cancel_artwork_tile_request(tile);
        tile.clear_image();
    }

    fn texture_for_decoded(
        &self,
        source_id: &::sources::SourceId,
        image: Arc<artwork::DecodedImage>,
    ) -> Option<gtk::gdk::Texture> {
        self.artwork.textures.borrow_mut().texture(source_id, image)
    }

    pub(crate) fn release_artwork_textures(&self, source_id: &::sources::SourceId) {
        self.artwork.textures.borrow_mut().release_source(source_id);
    }

    fn cancel_artwork_tile_request(self: &Rc<Self>, tile: &ArtworkTile) {
        tile.cancel_artwork_request();
    }

    pub(crate) fn current_playback_cached_artwork_path(
        &self,
        source_id: &::sources::SourceId,
        media: &playback::CurrentMedia,
        preferred_size: u32,
    ) -> Option<PlaybackArtworkPath> {
        let candidates = media
            .track
            .artwork_binding
            .as_deref()
            .map(ArtworkBinding::opaque)
            .unwrap_or_default();
        let settings = self.settings.current.borrow().clone();
        let external = artwork_external_policy(&settings);
        let request =
            ArtworkRequest::new(candidates, preferred_size, preferred_size).with_external(external);
        let path = self.products.artwork.cache_only_file(source_id, &request)?;
        Some(PlaybackArtworkPath { path })
    }

    pub(crate) fn reset_cover_pipeline_state(&self) {
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
    use super::{
        StartupArtworkPrime, artwork_binding_needs_work, cover_decode_size, cover_request_sizes,
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
    fn cancelled_exact_request_restarts_without_a_visible_preview() {
        assert!(artwork_binding_needs_work(false, false, false, false));
        assert!(!artwork_binding_needs_work(false, false, false, true));
        assert!(!artwork_binding_needs_work(false, true, false, false));
        assert!(!artwork_binding_needs_work(false, false, true, false));
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
}
