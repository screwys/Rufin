use crate::controls::{
    ActionButtonVariant, COVER_PRIMARY_ACTION_SIZE, COVER_SIDE_ACTION_SIZE, MORE_ICON, PLAY_ICON,
    PLAY_LATER_ICON, PLAY_NEXT_ICON, configure_action_button, icon_button,
    icon_button_without_tooltip,
};
use crate::favorites::{favorite_icon_button, set_favorite_button_active};
use crate::interactions::{
    CONTEXT_MENU_HOVER_HELD_CLASS, CONTEXT_MENU_HOVER_OWNER_CLASS, ContextMenuOpen,
    install_context_menu_openers,
};
use adw::prelude::*;
use std::rc::Rc;
pub const COVER_CORNER_HORIZONTAL_INSET: i32 = 4;
pub const COVER_CORNER_VERTICAL_INSET: i32 = 8;
const COVER_TRANSPORT_COMPACT_GAP: i32 = 3;
const COVER_TRANSPORT_REGULAR_GAP: i32 = 8;

fn cover_hover_transport_width(spacing: i32) -> i32 {
    COVER_SIDE_ACTION_SIZE * 2 + COVER_PRIMARY_ACTION_SIZE + spacing * 2
}

pub fn cover_hover_transport_spacing(cover_width: i32) -> i32 {
    let available_spacing = cover_width
        .saturating_sub(COVER_CORNER_HORIZONTAL_INSET * 2)
        .saturating_sub(cover_hover_transport_width(0))
        / 2;
    available_spacing.clamp(COVER_TRANSPORT_COMPACT_GAP, COVER_TRANSPORT_REGULAR_GAP)
}

pub struct CoverHoverControls {
    pub shade: gtk::Box,
    pub transport: gtk::Box,
    pub play_next: gtk::Button,
    pub play: gtk::Button,
    pub play_last: gtk::Button,
    pub favorite: Option<gtk::Button>,
    pub menu: Option<gtk::Button>,
}

impl CoverHoverControls {
    pub fn add_context_button(&mut self) -> gtk::Button {
        let menu = icon_button_without_tooltip(MORE_ICON, "More actions");
        configure_action_button(&menu, ActionButtonVariant::CoverCornerMenu);
        menu.set_margin_start(COVER_CORNER_HORIZONTAL_INSET);
        menu.set_margin_bottom(COVER_CORNER_VERTICAL_INSET);
        menu.set_visible(false);
        self.menu = Some(menu.clone());
        menu
    }

    pub fn add_to_overlay(&self, overlay: &gtk::Overlay) {
        overlay.add_overlay(&self.shade);
        overlay.add_overlay(&self.transport);
        if let Some(menu) = self.menu.as_ref() {
            overlay.add_overlay(menu);
        }
        if let Some(favorite) = self.favorite.as_ref() {
            overlay.add_overlay(favorite);
        }
    }

    pub fn connect_hover(&self, overlay: &gtk::Overlay) {
        overlay.add_css_class(CONTEXT_MENU_HOVER_OWNER_CLASS);
        let motion = gtk::EventControllerMotion::new();
        let shade_for_enter = self.shade.clone();
        let transport_for_enter = self.transport.clone();
        let favorite_for_enter = self.favorite.clone();
        let menu_for_enter = self.menu.clone();
        let show_controls = move |motion: &gtk::EventControllerMotion, x: f64, y: f64| {
            // GTK grab restoration can emit enter at (-1, -1) for an unrelated
            // cover. Only reveal controls for a position inside this cover.
            if !motion
                .widget()
                .is_some_and(|overlay| overlay.contains(x, y))
            {
                return;
            }
            shade_for_enter.set_visible(true);
            transport_for_enter.set_visible(true);
            if let Some(favorite) = favorite_for_enter.as_ref() {
                favorite.set_visible(true);
            }
            if let Some(menu) = menu_for_enter.as_ref() {
                menu.set_visible(true);
            }
        };
        // A rejected synthetic enter can still leave GTK's pointer state set;
        // real motion must reveal controls even if GTK emits no second enter.
        motion.connect_motion(show_controls.clone());
        motion.connect_enter(show_controls);
        let shade_for_leave = self.shade.clone();
        let transport_for_leave = self.transport.clone();
        let favorite_for_leave = self.favorite.clone();
        let menu_for_leave = self.menu.clone();
        let overlay_for_leave = overlay.downgrade();
        motion.connect_leave(move |_| {
            if overlay_for_leave
                .upgrade()
                .is_some_and(|overlay| overlay.has_css_class(CONTEXT_MENU_HOVER_HELD_CLASS))
            {
                return;
            }
            shade_for_leave.set_visible(false);
            transport_for_leave.set_visible(false);
            if let Some(favorite) = favorite_for_leave.as_ref() {
                favorite.set_visible(false);
            }
            if let Some(menu) = menu_for_leave.as_ref() {
                menu.set_visible(false);
            }
        });
        let motion_for_hold = motion.clone();
        let shade_for_hold = self.shade.clone();
        let transport_for_hold = self.transport.clone();
        let favorite_for_hold = self.favorite.clone();
        let menu_for_hold = self.menu.clone();
        overlay.connect_css_classes_notify(move |overlay| {
            if overlay.has_css_class(CONTEXT_MENU_HOVER_HELD_CLASS) {
                return;
            }
            let visible = motion_for_hold.contains_pointer();
            shade_for_hold.set_visible(visible);
            transport_for_hold.set_visible(visible);
            if let Some(favorite) = favorite_for_hold.as_ref() {
                favorite.set_visible(visible);
            }
            if let Some(menu) = menu_for_hold.as_ref() {
                menu.set_visible(visible);
            }
        });
        overlay.add_controller(motion);
    }
}

pub fn showcase_cover_overlay(
    cover: &gtk::Widget,
    mut controls: CoverHoverControls,
    context_menu: Option<ContextMenuOpen>,
) -> gtk::Overlay {
    let overlay = gtk::Overlay::new();
    overlay.add_css_class("cover-frame");
    overlay.set_halign(gtk::Align::Start);
    overlay.set_valign(gtk::Align::Start);
    overlay.set_child(Some(cover));

    if let Some(open) = context_menu {
        let menu = controls.add_context_button();
        install_context_menu_openers(&overlay, Rc::clone(&open));
        let target = overlay.downgrade();
        menu.connect_clicked(move |_| {
            let Some(target) = target.upgrade() else {
                return;
            };
            open(target.upcast_ref(), elastic_cover_context_point(&target));
        });
    }
    controls.add_to_overlay(&overlay);
    controls.connect_hover(&overlay);
    overlay
}

pub fn cover_hover_controls(
    size: i32,
    play_label: &str,
    favorite_active: bool,
) -> CoverHoverControls {
    cover_hover_controls_with_favorite(size, play_label, favorite_active).0
}

pub fn cover_hover_controls_with_favorite(
    size: i32,
    play_label: &str,
    favorite_active: bool,
) -> (CoverHoverControls, gtk::Button) {
    let mut controls = cover_play_hover_controls(size, play_label);
    let favorite = favorite_icon_button("Favorite");
    configure_action_button(&favorite, ActionButtonVariant::CoverCornerFavorite);
    favorite.set_margin_top(COVER_CORNER_VERTICAL_INSET);
    favorite.set_margin_end(COVER_CORNER_HORIZONTAL_INSET);
    favorite.set_visible(false);
    set_favorite_button_active(&favorite, favorite_active);
    controls.favorite = Some(favorite.clone());
    (controls, favorite)
}

pub fn cover_play_hover_controls(size: i32, play_label: &str) -> CoverHoverControls {
    let shade = gtk::Box::new(gtk::Orientation::Vertical, 0);
    shade.add_css_class("cover-hover-layer");
    if size > 0 {
        constrain_cover_widget(&shade, size);
    } else {
        shade.set_hexpand(true);
        shade.set_vexpand(true);
        shade.set_halign(gtk::Align::Fill);
        shade.set_valign(gtk::Align::Fill);
    }
    shade.set_can_target(false);
    shade.set_visible(false);

    let play_next = icon_button(PLAY_NEXT_ICON, "Play Next");
    configure_action_button(&play_next, ActionButtonVariant::CoverSideTransport);
    play_next.set_visible(true);

    let play = icon_button(PLAY_ICON, play_label);
    configure_action_button(&play, ActionButtonVariant::CoverPrimaryTransport);
    play.set_visible(true);

    let play_last = icon_button(PLAY_LATER_ICON, "Play Later");
    configure_action_button(&play_last, ActionButtonVariant::CoverSideTransport);
    play_last.set_visible(true);

    let transport = gtk::Box::new(gtk::Orientation::Horizontal, COVER_TRANSPORT_REGULAR_GAP);
    transport.add_css_class("cover-hover-transport");
    transport.set_halign(gtk::Align::Center);
    transport.set_valign(gtk::Align::Center);
    transport.set_visible(false);
    transport.append(&play_next);
    transport.append(&play);
    transport.append(&play_last);

    CoverHoverControls {
        shade,
        transport,
        play_next,
        play,
        play_last,
        favorite: None,
        menu: None,
    }
}

pub fn cover_play_only_hover_controls(
    size: i32,
    play_label: &str,
) -> (CoverHoverControls, gtk::Button) {
    let controls = cover_play_hover_controls(size, play_label);
    controls.play_next.set_visible(false);
    controls.play_last.set_visible(false);
    let play = controls.play.clone();
    (controls, play)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library_fields::{COLLECTION_GRID_MAX_CARD_WIDTH, COLLECTION_GRID_MIN_CARD_WIDTH};
    #[test]
    pub fn cover_hover_transport_spacing_uses_available_grid_width() {
        assert_eq!(
            cover_hover_transport_width(COVER_TRANSPORT_COMPACT_GAP),
            COLLECTION_GRID_MIN_CARD_WIDTH
        );

        let regular_width = cover_hover_transport_width(COVER_TRANSPORT_REGULAR_GAP)
            + COVER_CORNER_HORIZONTAL_INSET * 2;
        assert_eq!(cover_hover_transport_spacing(regular_width - 1), 7);
        assert_eq!(
            cover_hover_transport_spacing(regular_width),
            COVER_TRANSPORT_REGULAR_GAP
        );

        let mut previous_spacing = COVER_TRANSPORT_COMPACT_GAP;
        for cover_width in COLLECTION_GRID_MIN_CARD_WIDTH..=COLLECTION_GRID_MAX_CARD_WIDTH {
            let spacing = cover_hover_transport_spacing(cover_width);
            assert!(spacing >= previous_spacing);
            assert!(cover_hover_transport_width(spacing) <= cover_width);
            if spacing > COVER_TRANSPORT_COMPACT_GAP {
                assert!(
                    cover_hover_transport_width(spacing) + COVER_CORNER_HORIZONTAL_INSET * 2
                        <= cover_width
                );
            }
            previous_spacing = spacing;
        }
    }
}
