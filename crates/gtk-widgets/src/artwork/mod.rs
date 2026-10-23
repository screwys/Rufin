use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use artwork::{ArtworkBinding, ArtworkRequest};
use gtk::glib;
use sources::ImageSize;
use tracing::warn;

use rufin_core::settings::app::Settings as UiSettings;

use crate::settings::SettingsState;

pub const THUMB_COVER_SIZE: u32 = 96;
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

pub fn cover_decode_size(display_size: i32, scale: f64) -> u32 {
    let display_size = f64::from(display_size.max(1));
    let scale = if scale.is_finite() {
        scale.max(1.0)
    } else {
        1.0
    };
    (display_size * scale).ceil().min(f64::from(u32::MAX)) as u32
}

fn cover_fetch_size(render_size: u32) -> u32 {
    match render_size {
        0..=96 => 96,
        97..=256 => 256,
        257..=512 => 512,
        _ => render_size,
    }
}

pub struct ArtworkState {
    service: artwork::Artwork,
    settings: Rc<SettingsState>,
    scale: Box<dyn Fn() -> f64>,
    playback_ready: Box<dyn Fn()>,
    textures: RefCell<TextureCache>,
    animations: RefCell<animation::Animations>,
}

impl ArtworkState {
    pub fn new(
        service: artwork::Artwork,
        settings: Rc<SettingsState>,
        scale: Box<dyn Fn() -> f64>,
        playback_ready: Box<dyn Fn()>,
    ) -> Rc<Self> {
        Rc::new(Self {
            service,
            settings,
            scale,
            playback_ready,
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
    ) {
        self.bind_artwork_tile_request(tile, artwork, render_size, false, false);
    }

    pub fn bind_cache_only_artwork_tile(
        self: &Rc<Self>,
        tile: &ArtworkTile,
        artwork: ArtworkBinding,
        render_size: i32,
    ) {
        self.bind_artwork_tile_request(tile, artwork, render_size, false, true);
    }

    pub fn bind_playback_artwork_tile(
        self: &Rc<Self>,
        tile: &ArtworkTile,
        artwork: ArtworkBinding,
        render_size: i32,
    ) {
        self.bind_artwork_tile_request(tile, artwork, render_size, true, false);
    }

    fn bind_artwork_tile_request(
        self: &Rc<Self>,
        tile: &ArtworkTile,
        artwork: ArtworkBinding,
        render_size: i32,
        refresh_desktop_on_ready: bool,
        cache_only: bool,
    ) {
        tile.set_animation_scale(self.artwork_scale());
        if artwork.stable_identity().is_empty() {
            tile.cancel_artwork_request();
            tile.bind_missing();
            return;
        }
        let display_size = render_size;
        let render_size = cover_decode_size(render_size, self.artwork_scale());
        let external = if cache_only {
            cache_only_artwork_external_policy()
        } else {
            artwork_external_policy(&self.settings.current.borrow())
        };
        let request = if refresh_desktop_on_ready {
            ArtworkRequest::original(artwork, render_size)
        } else {
            ArtworkRequest {
                fetch_size: ImageSize::Thumbnail(cover_fetch_size(render_size)),
                ..ArtworkRequest::new(artwork, render_size)
            }
        }
        .with_external(external);
        let request = if cache_only {
            request.cache_only()
        } else {
            request
        };
        let key = self.service.key(&request);
        tile.bind_artwork_request(
            Rc::downgrade(self),
            key,
            request,
            refresh_desktop_on_ready,
            display_size,
        );
    }

    fn load_artwork_tile(
        self: &Rc<Self>,
        tile: &ArtworkTile,
        generation: u64,
        refresh_desktop_on_ready: bool,
        request: ArtworkRequest,
    ) {
        let key = self.service.key(&request);
        if !refresh_desktop_on_ready {
            let texture = self.textures.borrow_mut().cached_texture(&key);
            if let Some(texture) = texture {
                tile.set_texture_if_current(generation, texture);
                return;
            }
        }
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
        let immediate = refresh_desktop_on_ready || request.cache_only;
        let load = immediate.then(|| self.service.load(request.clone()));
        if let Some(artwork::ArtworkLoad::Ready(image)) = load.as_ref() {
            self.apply_artwork(
                tile,
                generation,
                artwork::ArtworkOutcome::Ready(Arc::clone(image)),
                refresh_desktop_on_ready,
            );
            return;
        }
        if let Some(preview) = self
            .service
            .preview(request.binding.clone(), request.external.allow_cached)
            && let Some(texture) = texture_cache::texture_from_preview(preview.image.clone())
        {
            tile.set_preview_if_current(generation, texture);
        }
        let service = self.service.clone();
        let tile_weak = tile.downgrade();
        let shell = Rc::downgrade(self);
        // Let GTK allocation unmap offscreen pooled rows before starting their cover work.
        let priority = if immediate {
            glib::Priority::DEFAULT
        } else {
            glib::Priority::DEFAULT_IDLE
        };
        let request = glib::MainContext::ref_thread_default().spawn_local_with_priority(
            priority,
            async move {
                let load = if let Some(load) = load {
                    load
                } else {
                    let Some(tile) = tile_weak.upgrade() else {
                        return;
                    };
                    if !tile.generation_is_current(generation) || !tile.area.is_mapped() {
                        return;
                    }
                    service.load(request)
                };
                let outcome = match load {
                    artwork::ArtworkLoad::Ready(image) => artwork::ArtworkOutcome::Ready(image),
                    artwork::ArtworkLoad::Missing => artwork::ArtworkOutcome::Missing,
                    artwork::ArtworkLoad::Pending(pending) => pending.finish().await,
                };
                let (Some(tile), Some(shell)) = (tile_weak.upgrade(), shell.upgrade()) else {
                    return;
                };
                if tile.has_preview(generation)
                    && matches!(
                        outcome,
                        artwork::ArtworkOutcome::Missing | artwork::ArtworkOutcome::Failed(_)
                    )
                {
                    if let artwork::ArtworkOutcome::Failed(error) = outcome {
                        warn!(%error, "cover request failed");
                    }
                    return;
                }
                let ready =
                    shell.apply_artwork(&tile, generation, outcome, refresh_desktop_on_ready);
                if ready && refresh_desktop_on_ready {
                    (shell.playback_ready)();
                }
            },
        );
        tile.replace_artwork_request(request);
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
                let key = loaded.key().clone();
                if let Some(texture) = self.texture_for_decoded(Arc::clone(&loaded)) {
                    let current = tile.set_texture_if_current(generation, texture.clone());
                    if current
                        && animate
                        && let Some(bytes) = loaded.animation_bytes()
                    {
                        let lease = self.animations.borrow_mut().attach(
                            key,
                            Arc::clone(bytes),
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

    pub fn clear_artwork_tile(self: &Rc<Self>, tile: &ArtworkTile) {
        tile.cancel_artwork_request();
        tile.clear_image();
    }

    pub async fn load_texture(
        &self,
        binding: ArtworkBinding,
        display_size: i32,
    ) -> Option<gtk::gdk::Texture> {
        if binding.stable_identity().is_empty() {
            return None;
        }
        let render_size = cover_decode_size(display_size, self.artwork_scale());
        let request = ArtworkRequest {
            fetch_size: ImageSize::Thumbnail(cover_fetch_size(render_size)),
            ..ArtworkRequest::new(binding, render_size)
        }
        .with_external(artwork_external_policy(&self.settings.current.borrow()));
        let outcome = match self.service.load(request) {
            artwork::ArtworkLoad::Ready(image) => artwork::ArtworkOutcome::Ready(image),
            artwork::ArtworkLoad::Missing => artwork::ArtworkOutcome::Missing,
            artwork::ArtworkLoad::Pending(pending) => pending.finish().await,
        };
        match outcome {
            artwork::ArtworkOutcome::Ready(image) => self.texture_for_decoded(image),
            artwork::ArtworkOutcome::Failed(error) => {
                warn!(%error, "artwork request failed");
                None
            }
            _ => None,
        }
    }

    fn texture_for_decoded(&self, image: Arc<artwork::DecodedImage>) -> Option<gtk::gdk::Texture> {
        self.textures.borrow_mut().texture(image)
    }

    pub fn current_playback_cached_artwork_path(
        &self,
        media: &playback::CurrentMedia,
        preferred_size: u32,
    ) -> Option<PlaybackArtworkPath> {
        let candidates = media
            .row
            .track
            .artwork_binding
            .as_deref()
            .map(ArtworkBinding::opaque)
            .unwrap_or_default();
        let settings = self.settings.current.borrow().clone();
        let external = artwork_external_policy(&settings);
        let request = ArtworkRequest::new(candidates, preferred_size).with_external(external);
        let path = self.service.cache_only_file(&request)?;
        Some(PlaybackArtworkPath { path })
    }
}

fn artwork_external_policy(settings: &UiSettings) -> artwork::ExternalPolicy {
    artwork::ExternalPolicy::new(
        settings.application.external_metadata_enabled,
        settings.allows_external_metadata_lookup(),
        settings.lastfm_api_key.clone(),
    )
}

fn cache_only_artwork_external_policy() -> artwork::ExternalPolicy {
    artwork::ExternalPolicy::new(true, false, String::new()).with_musicbrainz(false)
}

#[cfg(test)]
mod tests {
    use super::{cache_only_artwork_external_policy, cover_decode_size, cover_fetch_size};

    #[test]
    fn artwork_decoding_uses_physical_size() {
        assert_eq!(cover_decode_size(200, 1.0), 200);
        assert_eq!(cover_decode_size(200, 1.25), 250);
        assert_eq!(cover_decode_size(200, 1.5), 300);
        assert_eq!(cover_decode_size(200, 2.0), 400);
        assert_eq!(cover_decode_size(512, 2.0), 1024);
        assert_eq!(cover_decode_size(48, 2.0), 96);
        for (physical, requested) in [(48, 96), (96, 96), (100, 256), (300, 512), (1024, 1024)] {
            assert_eq!(cover_fetch_size(physical), requested);
        }
    }

    #[test]
    fn drag_preview_artwork_never_starts_external_network_work() {
        let policy = cache_only_artwork_external_policy();
        assert!(policy.allow_cached);
        assert!(!policy.allow_network);
        assert!(!policy.allow_musicbrainz);
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
