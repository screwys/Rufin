//! The controls a cover shows under the pointer: a large Play that opens Play Next and Play
//! Later out of its sides, the favorite in the top-left corner, which stays as a red tab on
//! favorites at rest, the rating at the top right and More at the bottom right. While its
//! media plays, the bottom left shows that it plays.

use crate::favorites::{
    favorite_button_is_active, install_favorite_heart, set_favorite_button_active,
};
use crate::ratings::RatingControl;
use crate::widgets::interactions::{
    CONTEXT_MENU_HOVER_HELD_CLASS, CONTEXT_MENU_HOVER_OWNER_CLASS, ContextMenuOpen,
    install_context_menu_openers,
};
use crate::widgets::localization::bind_widget_tooltip;
use adw::prelude::*;
use std::cell::Cell;
use std::rc::Rc;

/// The window class that keeps the favorite's corner on favorites at rest.
pub const GRID_FAVORITE_BADGES_CLASS: &str = "grid-favorite-badges";
/// The window class that shows the rating on a cover under the pointer.
pub const GRID_STAR_RATINGS_CLASS: &str = "grid-star-ratings";

/// The sizes the controls take, by the cover's width.
const COVER_TIERS: [(i32, &str); 4] = [
    (184, "cover-tier-large"),
    (150, "cover-tier-medium"),
    (128, "cover-tier-small"),
    (0, "cover-tier-tiny"),
];

/// Which of the controls a cover has.
#[derive(Clone, Copy)]
pub struct CoverParts {
    /// Play Next and Play Later beside Play.
    pub queue: bool,
    pub menu: bool,
    pub favorite: bool,
    pub rating: bool,
}

impl CoverParts {
    pub const ALL: Self = Self {
        queue: true,
        menu: true,
        favorite: true,
        rating: true,
    };
    pub const PLAY: Self = Self {
        queue: false,
        menu: false,
        favorite: false,
        rating: false,
    };
}

pub struct CoverHoverControls {
    shade: gtk::Box,
    morph: gtk::Box,
    sides: Option<[gtk::Revealer; 2]>,
    pub play_next: gtk::Button,
    pub play: gtk::Button,
    pub play_last: gtk::Button,
    pub menu: Option<gtk::Button>,
    pub favorite: Option<CoverFavorite>,
    pub rating: Option<CoverRating>,
    playing: gtk::Box,
    /// Whether the pointer is on the cover: the one record the controls and the favorite read.
    hovered: Rc<Cell<bool>>,
}

impl CoverHoverControls {
    pub fn new(play_label: &str, parts: CoverParts) -> Self {
        let resource = "routes/cover_hover.ui";
        let builder = crate::ui_resource::builder(resource);
        crate::objects!(builder, resource, {
            shade: gtk::Box, morph: gtk::Box,
            play_next_side: gtk::Revealer, play_last_side: gtk::Revealer,
            play_next: gtk::Button, play: gtk::Button, play_last: gtk::Button,
            rating: gtk::Box, menu: gtk::Button, favorite: gtk::Button,
            playing: gtk::Box,
        });
        bind_widget_tooltip(&play, play_label);
        let sides = if parts.queue {
            install_morph(&morph, [play_next_side.clone(), play_last_side.clone()]);
            Some([play_next_side, play_last_side])
        } else {
            morph.remove(&play_next_side);
            morph.remove(&play_last_side);
            None
        };
        let hovered = Rc::new(Cell::new(false));
        Self {
            shade,
            morph,
            sides,
            play_next,
            play,
            play_last,
            menu: parts.menu.then_some(menu),
            favorite: parts
                .favorite
                .then(|| CoverFavorite::new(favorite, Rc::clone(&hovered))),
            rating: parts.rating.then(|| CoverRating::new(rating)),
            playing,
            hovered,
        }
    }

    /// Shows on the cover that its media plays while `title` is marked playing and not paused.
    pub fn show_playing(&self, title: &gtk::Label) {
        let chip = self.playing.downgrade();
        title.connect_css_classes_notify(move |title| {
            let Some(chip) = chip.upgrade() else { return };
            let playing = title.has_css_class("track-row-playing")
                && !title.has_css_class("track-row-paused");
            if playing && chip.first_child().is_none() {
                let indicator = crate::widgets::playing_indicator::PlayingIndicator::new();
                indicator.set_hexpand(true);
                indicator.set_halign(gtk::Align::Center);
                indicator.set_valign(gtk::Align::Center);
                chip.append(&indicator);
            } else if !playing && let Some(indicator) = chip.first_child() {
                chip.remove(&indicator);
            }
            chip.set_visible(playing);
        });
    }

    /// Puts the controls over `overlay`'s cover and shows them while the pointer is on it.
    pub fn attach(&self, overlay: &gtk::Overlay) {
        overlay.add_css_class(CONTEXT_MENU_HOVER_OWNER_CLASS);
        overlay.add_overlay(&self.shade);
        overlay.add_overlay(&self.morph);
        overlay.add_overlay(&self.playing);
        if let Some(rating) = &self.rating {
            overlay.add_overlay(&rating.root);
        }
        if let Some(menu) = &self.menu {
            overlay.add_overlay(menu);
        }
        if let Some(favorite) = &self.favorite {
            overlay.add_overlay(&favorite.button);
        }
        let reveal = HoverReveal {
            hovered: Rc::clone(&self.hovered),
            shade: self.shade.clone(),
            morph: self.morph.clone(),
            sides: self.sides.clone(),
            menu: self.menu.clone(),
            rating: self
                .rating
                .as_ref()
                .map(|rating| (rating.root.clone(), Rc::clone(&rating.ratable))),
            favorite: self
                .favorite
                .as_ref()
                .map(|favorite| (favorite.button.clone(), Rc::clone(&favorite.available))),
        };
        let motion = gtk::EventControllerMotion::new();
        let shown = reveal.clone();
        let show = move |motion: &gtk::EventControllerMotion, x: f64, y: f64| {
            // GTK grab restoration can emit enter at (-1, -1) for an unrelated
            // cover. Only reveal controls for a position inside this cover.
            if let Some(overlay) = motion.widget()
                && overlay.contains(x, y)
            {
                shown.set(&overlay, true);
            }
        };
        // A rejected synthetic enter can still leave GTK's pointer state set;
        // real motion must reveal controls even if GTK emits no second enter.
        motion.connect_motion(show.clone());
        motion.connect_enter(show);
        let left = reveal.clone();
        motion.connect_leave(move |motion| {
            if let Some(overlay) = motion.widget()
                && !overlay.has_css_class(CONTEXT_MENU_HOVER_HELD_CLASS)
            {
                left.set(&overlay, false);
            }
        });
        // A context menu holds the controls open; when it lets go, they follow the pointer.
        let pointer = motion.clone();
        let was_held = Cell::new(false);
        overlay.connect_css_classes_notify(move |overlay| {
            let held = overlay.has_css_class(CONTEXT_MENU_HOVER_HELD_CLASS);
            if was_held.replace(held) && !held {
                let hovered = pointer.contains_pointer();
                if hovered != reveal.hovered.get() {
                    reveal.set(overlay.upcast_ref(), hovered);
                }
            }
        });
        overlay.add_controller(motion);
    }
}

#[derive(Clone)]
struct HoverReveal {
    hovered: Rc<Cell<bool>>,
    shade: gtk::Box,
    morph: gtk::Box,
    sides: Option<[gtk::Revealer; 2]>,
    menu: Option<gtk::Button>,
    rating: Option<(gtk::Box, Rc<Cell<bool>>)>,
    favorite: Option<(gtk::Button, Rc<Cell<bool>>)>,
}

impl HoverReveal {
    fn set(&self, overlay: &gtk::Widget, shown: bool) {
        self.hovered.set(shown);
        if !shown {
            if let Some(sides) = &self.sides {
                open_morph(&self.morph, sides, false);
            }
        }
        self.shade.set_visible(shown);
        self.morph.set_visible(shown);
        if let Some(menu) = &self.menu {
            menu.set_visible(shown);
        }
        if let Some((corner, available)) = &self.favorite {
            show_corner(corner, available.get(), shown);
        }
        if let Some((rating, ratable)) = &self.rating {
            // A cover too narrow for the stars shows its rating only in its menu.
            rating.set_visible(
                shown
                    && ratable.get()
                    && !overlay.has_css_class("cover-tier-tiny")
                    && overlay
                        .root()
                        .is_some_and(|root| root.has_css_class(GRID_STAR_RATINGS_CLASS)),
            );
        }
    }
}

/// Opens Play Next and Play Later while the pointer or the keyboard is on Play's group.
/// Play in the accent, with Play Next and Play Later coming out of its sides while the pointer or
/// the keyboard is on it, as on a grid cover: a row's play controls.
pub fn play_group(play_label: &str) -> (gtk::Box, [gtk::Button; 3]) {
    let resource = "routes/cover_hover.ui";
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, {
        morph: gtk::Box, play_next_side: gtk::Revealer, play_last_side: gtk::Revealer,
        play_next: gtk::Button, play: gtk::Button, play_last: gtk::Button,
    });
    bind_widget_tooltip(&play, play_label);
    install_morph(&morph, [play_next_side, play_last_side]);
    morph.set_visible(true);
    (morph, [play, play_next, play_last])
}

fn install_morph(morph: &gtk::Box, sides: [gtk::Revealer; 2]) {
    let motion = gtk::EventControllerMotion::new();
    let focus = gtk::EventControllerFocus::new();
    let (pointer, keyboard, target) = (motion.downgrade(), focus.downgrade(), morph.downgrade());
    // Both controllers update their state after their leave signal, so a leave says so.
    let update = Rc::new(move |pointer_left: bool, focus_left: bool| {
        let (Some(pointer), Some(keyboard), Some(morph)) =
            (pointer.upgrade(), keyboard.upgrade(), target.upgrade())
        else {
            return;
        };
        let open = (!pointer_left && pointer.contains_pointer())
            || (!focus_left && keyboard.contains_focus());
        // A cover too narrow for three buttons keeps Play alone.
        open_morph(&morph, &sides, open && !morph_tiny(&morph));
    });
    let entered = Rc::clone(&update);
    motion.connect_enter(move |_, _, _| entered(false, false));
    let left = Rc::clone(&update);
    motion.connect_leave(move |_| left(true, false));
    let focused = Rc::clone(&update);
    focus.connect_enter(move |_| focused(false, false));
    focus.connect_leave(move |_| update(false, true));
    morph.add_controller(motion);
    morph.add_controller(focus);
}

fn morph_tiny(morph: &gtk::Box) -> bool {
    morph
        .parent()
        .is_some_and(|overlay| overlay.has_css_class("cover-tier-tiny"))
}

fn open_morph(morph: &gtk::Box, sides: &[gtk::Revealer; 2], open: bool) {
    for side in sides {
        side.set_reveal_child(open);
    }
    if open {
        morph.add_css_class("open");
    } else {
        morph.remove_css_class("open");
    }
}

/// Sizes `overlay`'s controls for a cover `width` wide.
pub fn set_cover_tier(overlay: &impl IsA<gtk::Widget>, width: i32) {
    let tier = COVER_TIERS
        .iter()
        .find(|(from, _)| width >= *from)
        .map_or("cover-tier-tiny", |(_, class)| class);
    if overlay.has_css_class(tier) {
        return;
    }
    for (_, class) in COVER_TIERS {
        overlay.remove_css_class(class);
    }
    overlay.add_css_class(tier);
}

/// The favorite in the cover's corner: a red tab on favorites at rest when the grid's
/// badges are on, the corner button under the pointer.
pub struct CoverFavorite {
    button: gtk::Button,
    /// Whether the row can be a favorite.
    available: Rc<Cell<bool>>,
    /// Whether the pointer is on the cover, as its controls record it.
    hovered: Rc<Cell<bool>>,
}

/// Shows the corner under the pointer, and on a favorite at rest, whose red tab the window's
/// badges setting then shows or hides.
fn show_corner(corner: &gtk::Button, available: bool, hovered: bool) {
    corner.set_visible(available && (hovered || favorite_button_is_active(corner)));
    if hovered {
        corner.remove_css_class("resting");
    } else {
        corner.add_css_class("resting");
    }
}

impl CoverFavorite {
    fn new(button: gtk::Button, hovered: Rc<Cell<bool>>) -> Self {
        install_favorite_heart(&button);
        Self {
            button,
            available: Rc::new(Cell::new(false)),
            hovered,
        }
    }

    /// Shows whether the row is a favorite; a row that cannot be one shows no corner.
    pub fn set(&self, favorite: Option<bool>) {
        set_favorite_button_active(&self.button, favorite.unwrap_or(false));
        self.available.set(favorite.is_some());
        show_corner(&self.button, favorite.is_some(), self.hovered.get());
    }

    /// Calls `toggle` with the state a click asks for.
    pub fn connect_toggle(&self, toggle: impl Fn(bool) + 'static) {
        self.button
            .connect_clicked(move |button| toggle(!favorite_button_is_active(button)));
    }
}

/// The rating at the cover's top right: its five stars, which take a click.
pub struct CoverRating {
    root: gtk::Box,
    stars: RatingControl,
    ratable: Rc<Cell<bool>>,
}

impl CoverRating {
    fn new(root: gtk::Box) -> Self {
        let stars = RatingControl::new(None, false);
        // The stars take their own size here, so the last one ends at the cover's inset.
        let row = stars.widget();
        row.set_size_request(-1, -1);
        row.set_homogeneous(false);
        row.set_spacing(2);
        root.append(row);
        Self {
            root,
            stars,
            ratable: Rc::new(Cell::new(false)),
        }
    }

    /// Shows `rating` (0–10), or hides the stars for a row that takes none.
    pub fn set(&self, rating: Option<Option<i64>>, half_stars: bool) {
        self.ratable.set(rating.is_some());
        if rating.is_none() {
            self.root.set_visible(false);
        }
        let value = rating
            .flatten()
            .and_then(|value| u8::try_from(value).ok())
            .filter(|value| *value > 0);
        self.stars.set_rating(value, half_stars);
    }

    pub fn connect_commit(&self, commit: impl Fn(Option<u8>) + 'static) {
        self.stars.connect_commit(commit);
    }
}

/// A cover with Play alone on hover, and its menu from a secondary click.
pub fn cover_play_only_hover_controls(play_label: &str) -> CoverHoverControls {
    CoverHoverControls::new(play_label, CoverParts::PLAY)
}

/// `cover` in an overlay with `controls`; `context_menu` opens from More and a secondary
/// click.
pub fn cover_overlay(
    cover: &gtk::Widget,
    controls: &CoverHoverControls,
    context_menu: Option<ContextMenuOpen>,
) -> gtk::Overlay {
    let overlay = gtk::Overlay::new();
    overlay.add_css_class("cover-frame");
    overlay.set_halign(gtk::Align::Start);
    overlay.set_valign(gtk::Align::Start);
    overlay.set_child(Some(cover));
    if let Some(open) = context_menu {
        install_context_menu_openers(&overlay, Rc::clone(&open));
        if let Some(menu) = &controls.menu {
            let target = overlay.downgrade();
            menu.connect_clicked(move |_| {
                if let Some(target) = target.upgrade() {
                    open(target.upcast_ref(), elastic_cover_context_point(&target));
                }
            });
        }
    }
    controls.attach(&overlay);
    overlay
}

pub fn elastic_cover_context_point(widget: &impl IsA<gtk::Widget>) -> Option<(f64, f64)> {
    Some((20.0, f64::from(widget.height().saturating_sub(20))))
}

pub fn constrain_cover_widget(widget: &impl IsA<gtk::Widget>, size: i32) {
    widget.set_width_request(size);
    widget.set_height_request(size);
    widget.set_size_request(size, size);
    widget.set_hexpand(false);
    widget.set_halign(gtk::Align::Start);
}
