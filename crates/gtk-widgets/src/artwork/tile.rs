use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

struct CoverReveal {
    animation: adw::TimedAnimation,
    softened: gtk::Picture,
}

#[derive(Clone)]
pub struct ArtworkTile {
    pub area: gtk::Overlay,
    image: gtk::Picture,
    size: Rc<Cell<i32>>,
    request_complete: Rc<Cell<bool>>,
    request_key: Rc<RefCell<Option<artwork::ArtworkKey>>>,
    artwork_request: Rc<RefCell<Option<glib::JoinHandle<()>>>>,
    generation: Rc<Cell<u64>>,
    request_cleanup_installed: Rc<Cell<bool>>,
    animation: Rc<RefCell<Option<super::animation::Lease>>>,
    transition_pending: Rc<Cell<bool>>,
    transition: Rc<RefCell<Option<CoverReveal>>>,
}

#[derive(Clone)]
pub struct ArtworkTileWeak {
    area: glib::WeakRef<gtk::Overlay>,
    image: glib::WeakRef<gtk::Picture>,
    size: Rc<Cell<i32>>,
    request_complete: Rc<Cell<bool>>,
    request_key: Rc<RefCell<Option<artwork::ArtworkKey>>>,
    artwork_request: Rc<RefCell<Option<glib::JoinHandle<()>>>>,
    generation: Rc<Cell<u64>>,
    request_cleanup_installed: Rc<Cell<bool>>,
    animation: Rc<RefCell<Option<super::animation::Lease>>>,
    transition_pending: Rc<Cell<bool>>,
    transition: Rc<RefCell<Option<CoverReveal>>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArtworkBindOutcome {
    pub generation: u64,
    pub request_needed: bool,
    pub request_changed: bool,
}

impl ArtworkTile {
    pub fn new(size: i32) -> Self {
        Self::new_sized(size, size)
    }

    pub fn new_elastic_square() -> Self {
        let tile = Self::new_sized(1, 1);
        tile.area.set_hexpand(true);
        tile.area.set_vexpand(true);
        tile.area.set_halign(gtk::Align::Fill);
        tile.area.set_valign(gtk::Align::Fill);
        tile
    }

    pub fn new_sized(width: i32, height: i32) -> Self {
        let area = gtk::Overlay::new();
        area.add_css_class("cover-tile");
        area.add_css_class("card");
        area.set_width_request(width);
        area.set_height_request(height);
        area.set_size_request(width, height);
        area.set_hexpand(false);
        area.set_vexpand(false);
        area.set_halign(gtk::Align::Start);
        area.set_valign(gtk::Align::Start);
        area.set_overflow(gtk::Overflow::Hidden);

        let sizing = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        sizing.set_can_target(false);
        sizing.set_accessible_role(gtk::AccessibleRole::Presentation);
        area.set_child(Some(&sizing));

        let image = cover_picture(gtk::ContentFit::Cover);
        image.set_visible(false);
        area.add_overlay(&image);
        area.set_measure_overlay(&image, false);
        area.set_clip_overlay(&image, true);
        area.set_opacity(0.0);

        Self::from_widgets(area, image, width.max(height))
    }

    pub fn from_widgets(area: gtk::Overlay, image: gtk::Picture, size: i32) -> Self {
        area.set_measure_overlay(&image, false);
        area.set_clip_overlay(&image, true);

        let size = Rc::new(Cell::new(size));
        let request_complete = Rc::new(Cell::new(false));
        let request_key = Rc::new(RefCell::new(None::<artwork::ArtworkKey>));
        let artwork_request = Rc::new(RefCell::new(None));
        let generation = Rc::new(Cell::new(0));
        let request_cleanup_installed = Rc::new(Cell::new(false));

        Self {
            area,
            image,
            size,
            request_complete,
            request_key,
            artwork_request,
            generation,
            request_cleanup_installed,
            animation: Rc::new(RefCell::new(None)),
            transition_pending: Rc::new(Cell::new(false)),
            transition: Rc::new(RefCell::new(None)),
        }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.area.clone().upcast()
    }

    pub fn drag_paintable_source(&self) -> gtk::Picture {
        self.image.clone()
    }

    pub fn downgrade(&self) -> ArtworkTileWeak {
        ArtworkTileWeak {
            area: self.area.downgrade(),
            image: self.image.downgrade(),
            size: Rc::clone(&self.size),
            request_complete: Rc::clone(&self.request_complete),
            request_key: Rc::clone(&self.request_key),
            artwork_request: Rc::clone(&self.artwork_request),
            generation: Rc::clone(&self.generation),
            request_cleanup_installed: Rc::clone(&self.request_cleanup_installed),
            animation: Rc::clone(&self.animation),
            transition_pending: Rc::clone(&self.transition_pending),
            transition: Rc::clone(&self.transition),
        }
    }

    pub fn install_request_cleanup_once(&self) {
        if self.request_cleanup_installed.replace(true) {
            return;
        }

        for mapped in [true, false] {
            let animation = Rc::clone(&self.animation);
            let refresh = move |_: &gtk::Overlay| {
                let session = animation
                    .borrow()
                    .as_ref()
                    .map(|lease| Rc::clone(&lease.session));
                if let Some(session) = session {
                    session.refresh();
                }
            };
            if mapped {
                // Mapping resumes an existing animation; artwork requests belong to bind.
                // ast-grep-ignore: artwork-tile-must-not-admit-on-map
                self.area.connect_map(refresh);
            } else {
                self.area.connect_unmap(refresh);
            }
        }
        let animation = Rc::clone(&self.animation);
        let artwork_request = Rc::clone(&self.artwork_request);
        let transition = Rc::clone(&self.transition);
        self.area.connect_destroy(move |area| {
            let transition = transition.borrow_mut().take();
            if let Some(transition) = transition {
                transition.animation.pause();
                if transition.softened.parent().is_some() {
                    area.remove_overlay(&transition.softened);
                }
            }
            let lease = animation.borrow_mut().take();
            drop(lease);
            if let Some(request) = artwork_request.borrow_mut().take() {
                request.abort();
            }
        });
    }

    fn advance_generation(&self) {
        self.generation.set(self.generation.get().saturating_add(1));
    }

    pub fn bind_selected_cover(
        &self,
        request_key: artwork::ArtworkKey,
        keep_previous: bool,
    ) -> ArtworkBindOutcome {
        let same_artwork = self
            .request_key
            .borrow()
            .as_ref()
            .is_some_and(|previous| previous.same_asset(&request_key));
        let same_request = self.request_key.borrow().as_ref() == Some(&request_key);
        if same_request && self.request_complete.get() {
            return ArtworkBindOutcome {
                generation: self.generation.get(),
                request_needed: false,
                request_changed: false,
            };
        }

        let request_changed = !same_artwork || !same_request;
        if request_changed {
            self.finish_transition();
            self.transition_pending.set(!same_artwork && keep_previous);
            self.advance_generation();
            *self.request_key.borrow_mut() = Some(request_key);
            self.request_complete.set(false);
        }

        if !same_artwork && !keep_previous {
            self.image.set_paintable(Option::<&gtk::gdk::Texture>::None);
        }
        let has_texture = self.image.paintable().is_some();
        if has_texture || self.area.has_css_class("cover-fallback") {
            self.sync_presentation(has_texture, true);
        } else {
            self.image.set_visible(false);
            self.area.remove_css_class("cover-fallback");
            self.area.add_css_class("cover-pending");
            self.area.set_opacity(1.0);
        }
        self.area.queue_draw();

        ArtworkBindOutcome {
            generation: self.generation.get(),
            request_needed: !self.request_complete.get(),
            request_changed,
        }
    }

    pub fn has_artwork_request(&self) -> bool {
        self.artwork_request.borrow().is_some()
    }

    pub fn generation_is_current(&self, generation: u64) -> bool {
        self.generation.get() == generation
    }

    pub fn replace_artwork_request(&self, request: glib::JoinHandle<()>) {
        self.cancel_artwork_request();
        self.artwork_request.replace(Some(request));
    }

    pub(super) fn animation_target(&self, generation: u64, scale: f64) -> super::animation::Target {
        super::animation::Target {
            area: self.area.downgrade(),
            picture: self.image.downgrade(),
            generation: Rc::clone(&self.generation),
            expected_generation: generation,
            size: Rc::clone(&self.size),
            scale: Cell::new(scale),
        }
    }

    pub(super) fn set_animation(&self, lease: super::animation::Lease) {
        let session = Rc::clone(&lease.session);
        self.animation.replace(Some(lease));
        session.refresh();
    }

    pub(super) fn set_animation_scale(&self, scale: f64) {
        let active = self
            .animation
            .borrow()
            .as_ref()
            .map(|lease| (Rc::clone(&lease.session), Rc::clone(&lease.target)));
        if let Some((session, target)) = active {
            target.scale.set(scale);
            session.refresh();
        }
    }

    pub fn cancel_artwork_request(&self) {
        let lease = self.animation.borrow_mut().take();
        drop(lease);
        if let Some(request) = self.artwork_request.borrow_mut().take() {
            request.abort();
        }
    }

    pub fn set_square_size(&self, size: i32) {
        let size = size.max(1);
        if self.size.replace(size) == size {
            return;
        }
        self.area.set_width_request(size);
        self.area.set_height_request(size);
        self.area.set_size_request(size, size);
        self.area.queue_resize();
        let session = self
            .animation
            .borrow()
            .as_ref()
            .map(|lease| Rc::clone(&lease.session));
        if let Some(session) = session {
            session.refresh();
        }
    }

    pub fn bind_missing(&self) -> u64 {
        self.bind_image_state(None, true)
    }

    fn bind_image_state(&self, texture: Option<gtk::gdk::Texture>, request_complete: bool) -> u64 {
        self.finish_transition();
        self.transition_pending.set(false);
        let generation = self.generation.get().saturating_add(1);
        self.generation.set(generation);
        let has_texture = texture.is_some();
        self.image.set_paintable(texture.as_ref());
        self.request_complete.set(request_complete);
        *self.request_key.borrow_mut() = None;
        self.sync_presentation(has_texture, true);
        generation
    }

    pub fn set_texture_if_current(&self, generation: u64, texture: gtk::gdk::Texture) -> bool {
        self.set_cover_texture(generation, texture, true)
    }

    fn set_cover_texture(
        &self,
        generation: u64,
        texture: gtk::gdk::Texture,
        complete: bool,
    ) -> bool {
        if self.generation.get() != generation {
            return false;
        }
        let reveal = self.transition_pending.replace(false);
        self.image.set_paintable(Some(&texture));
        self.request_complete.set(complete);
        self.sync_presentation(true, true);
        if complete {
            self.complete_preview_if_current(generation);
        }
        if reveal {
            self.reveal(&texture);
        }
        true
    }

    pub(super) fn set_preview_if_current(
        &self,
        generation: u64,
        texture: gtk::gdk::Texture,
    ) -> bool {
        self.set_cover_texture(generation, texture, false)
    }

    pub(super) fn complete_preview_if_current(&self, generation: u64) {
        if !self.generation_is_current(generation) {
            return;
        }
        self.request_complete.set(true);
        let animation = self.transition.borrow().as_ref().and_then(|reveal| {
            (reveal.softened.is_visible()
                && reveal.animation.state() == adw::AnimationState::Finished)
                .then(|| reveal.animation.clone())
        });
        if let Some(animation) = animation {
            animation.set_value_from(0.2);
            animation.set_duration(80);
            animation.play();
        }
    }

    fn reveal(&self, texture: &gtk::gdk::Texture) {
        self.finish_transition();
        let width = texture.width() as f32;
        let height = texture.height() as f32;
        let bounds = gtk::graphene::Rect::new(0.0, 0.0, width, height);
        let snapshot = gtk::Snapshot::new();
        snapshot.push_clip(&bounds);
        snapshot.push_blur(f64::from(width.min(height)) * 0.06);
        snapshot.append_texture(texture, &bounds);
        snapshot.pop();
        snapshot.pop();
        let softened = cover_picture(self.image.content_fit());
        softened.set_paintable(
            snapshot
                .to_paintable(Some(&gtk::graphene::Size::new(width, height)))
                .as_ref(),
        );
        self.area.add_overlay(&softened);
        self.area.set_measure_overlay(&softened, false);
        self.area.set_clip_overlay(&softened, true);
        let picture = softened.downgrade();
        let complete = Rc::clone(&self.request_complete);
        let target = adw::CallbackAnimationTarget::new(move |opacity| {
            if let Some(picture) = picture.upgrade() {
                picture.set_opacity(if complete.get() {
                    opacity
                } else {
                    opacity.max(0.2)
                });
            }
        });
        let transition = adw::TimedAnimation::new(&self.area, 1.0, 0.0, 200, target);
        transition.set_easing(adw::Easing::EaseInOutCubic);
        let area = self.area.downgrade();
        let picture = softened.downgrade();
        let complete = Rc::clone(&self.request_complete);
        transition.connect_done(move |_| {
            if let (Some(area), Some(picture)) = (area.upgrade(), picture.upgrade()) {
                if complete.get()
                    || !area.is_mapped()
                    || !area.settings().is_gtk_enable_animations()
                {
                    picture.set_visible(false);
                    picture.set_paintable(None::<&gtk::gdk::Paintable>);
                }
            }
        });
        self.transition.replace(Some(CoverReveal {
            animation: transition.clone(),
            softened,
        }));
        transition.play();
    }

    fn finish_transition(&self) {
        let transition = self.transition.borrow_mut().take();
        if let Some(transition) = transition {
            transition.animation.pause();
            self.area.remove_overlay(&transition.softened);
        }
    }

    pub fn clear_image(&self) {
        self.finish_transition();
        self.transition_pending.set(false);
        self.advance_generation();
        self.image.set_paintable(Option::<&gtk::gdk::Texture>::None);
        self.request_complete.set(false);
        *self.request_key.borrow_mut() = None;
        self.sync_presentation(false, false);
    }

    pub fn set_fallback_if_current(&self, generation: u64) -> bool {
        if self.generation.get() != generation {
            return false;
        }
        self.finish_transition();
        self.transition_pending.set(false);
        self.generation.set(self.generation.get().saturating_add(1));
        self.image.set_paintable(Option::<&gtk::gdk::Texture>::None);
        self.request_complete.set(false);
        *self.request_key.borrow_mut() = None;
        self.sync_presentation(false, true);
        true
    }

    pub fn set_missing_if_current(&self, generation: u64) -> bool {
        if self.generation.get() != generation {
            return false;
        }
        self.finish_transition();
        self.transition_pending.set(false);
        self.generation.set(self.generation.get().saturating_add(1));
        self.image.set_paintable(Option::<&gtk::gdk::Texture>::None);
        self.request_complete.set(true);
        self.sync_presentation(false, true);
        true
    }

    fn sync_presentation(&self, has_texture: bool, bound: bool) {
        self.area.remove_css_class("cover-pending");
        self.image.set_visible(has_texture);
        if bound && !has_texture {
            self.area.add_css_class("cover-fallback");
        } else {
            self.area.remove_css_class("cover-fallback");
        }
        self.area.set_opacity(if bound { 1.0 } else { 0.0 });
    }
}

impl ArtworkTileWeak {
    pub fn upgrade(&self) -> Option<ArtworkTile> {
        Some(ArtworkTile {
            area: self.area.upgrade()?,
            image: self.image.upgrade()?,
            size: Rc::clone(&self.size),
            request_complete: Rc::clone(&self.request_complete),
            request_key: Rc::clone(&self.request_key),
            artwork_request: Rc::clone(&self.artwork_request),
            generation: Rc::clone(&self.generation),
            request_cleanup_installed: Rc::clone(&self.request_cleanup_installed),
            animation: Rc::clone(&self.animation),
            transition_pending: Rc::clone(&self.transition_pending),
            transition: Rc::clone(&self.transition),
        })
    }
}

fn cover_picture(content_fit: gtk::ContentFit) -> gtk::Picture {
    let picture = gtk::Picture::new();
    picture.set_accessible_role(gtk::AccessibleRole::Presentation);
    picture.set_can_shrink(true);
    picture.set_content_fit(content_fit);
    picture.set_hexpand(true);
    picture.set_vexpand(true);
    picture.set_halign(gtk::Align::Fill);
    picture.set_valign(gtk::Align::Fill);
    picture.set_can_target(false);
    picture
}
