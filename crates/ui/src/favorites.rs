use std::rc::Rc;

use crate::shell::shell::Shell;
use crate::widgets::localization::bind_widget_tooltip;
use ::library::FavoriteTarget;
use adw::prelude::*;

pub const FAVORITE_ADD_ICON: &str = "rufin-heart-outline-symbolic";
pub const FAVORITE_REMOVE_ICON: &str = "rufin-heart-filled-symbolic";
pub const FAVORITE_COLUMN_WIDTH: i32 = 44;

pub fn favorite_icon_button(label: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(FAVORITE_ADD_ICON);
    button.add_css_class("icon-button");
    button.add_css_class("flat");
    button.add_css_class("circular");
    button.add_css_class("favorite-toggle");
    button.set_valign(gtk::Align::Center);
    bind_widget_tooltip(&button, label);
    button
}

pub fn row_favorite_icon_button(label: &str) -> gtk::Button {
    let button = favorite_icon_button(label);
    button.add_css_class("row-favorite-button");
    button.set_halign(gtk::Align::Center);
    button
}

pub fn set_favorite_button_active(button: &gtk::Button, active: bool) {
    if active {
        button.add_css_class("active-toggle");
    } else {
        button.remove_css_class("active-toggle");
    }
    let icon_name = if active {
        FAVORITE_REMOVE_ICON
    } else {
        FAVORITE_ADD_ICON
    };
    if let Some(image) = button.child().and_then(icon_image_from_widget) {
        image.set_icon_name(Some(icon_name));
    } else {
        button.set_icon_name(icon_name);
    }
}

pub fn favorite_button_is_active(button: &gtk::Button) -> bool {
    button.has_css_class("active-toggle")
}

fn icon_image_from_widget(widget: gtk::Widget) -> Option<gtk::Image> {
    let widget = match widget.downcast::<gtk::Image>() {
        Ok(image) => return Some(image),
        Err(widget) => widget,
    };
    widget
        .downcast::<gtk::CenterBox>()
        .ok()
        .and_then(|face| face.center_widget())
        .and_then(icon_image_from_widget)
}

impl Shell {
    pub(crate) fn toggle_current_track_favorite(self: &Rc<Self>) {
        let Some(media_uri) = self
            .player_ui
            .selected_playback()
            .as_deref()
            .and_then(|player| player.transport.current.as_ref())
            .map(|entry| entry.row.track.media_uri.clone())
        else {
            return;
        };
        let favorite =
            !favorite_button_is_active(&self.player_ui.views.player_controls.favorite_button);
        self.set_favorite(FavoriteTarget::Track(media_uri), favorite);
    }

    /// Hearts change when the favorite is committed, from the publication that follows.
    pub(crate) fn set_favorite(self: &Rc<Self>, item: FavoriteTarget, favorite: bool) {
        self.products.source.set_favorite(item, favorite);
    }
}
