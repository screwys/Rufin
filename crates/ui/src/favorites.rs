use std::rc::Rc;

use crate::shell::shell::Shell;
use crate::widgets::localization::bind_widget_tooltip;
use ::library::FavoriteTarget;
use adw::prelude::*;

pub const FAVORITE_ADD_ICON: &str = "rufin-heart-outline-symbolic";
pub const FAVORITE_REMOVE_ICON: &str = "rufin-heart-filled-symbolic";
pub const FAVORITE_COLUMN_WIDTH: i32 = 44;

/// How long a heart pops and its small hearts fly.
const FAVORITE_BURST_MILLIS: u64 = 700;
const FAVORITE_BURST_COUNT: usize = 7;

pub fn favorite_icon_button(label: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(FAVORITE_ADD_ICON);
    button.add_css_class("icon-button");
    button.add_css_class("flat");
    button.add_css_class("circular");
    button.add_css_class("favorite-toggle");
    button.set_valign(gtk::Align::Center);
    bind_widget_tooltip(&button, label);
    install_favorite_heart(&button);
    button
}

/// Gives `button` the heart every favorite shares: a click that makes a favorite pops it and
/// sends small hearts out of it, one that removes it shrinks it back.
pub fn install_favorite_heart(button: &gtk::Button) {
    let heart = button
        .child()
        .and_downcast::<gtk::Image>()
        .unwrap_or_else(|| gtk::Image::from_icon_name(FAVORITE_ADD_ICON));
    button.set_child(None::<&gtk::Widget>);
    let face = gtk::Overlay::new();
    face.set_halign(heart.halign());
    face.set_valign(heart.valign());
    heart.set_halign(gtk::Align::Center);
    heart.set_valign(gtk::Align::Center);
    face.set_child(Some(&heart));
    let burst = (0..FAVORITE_BURST_COUNT)
        .map(|index| {
            let spark = gtk::Image::from_icon_name(FAVORITE_REMOVE_ICON);
            spark.add_css_class("favorite-burst");
            spark.add_css_class(&format!("favorite-burst-{index}"));
            spark.set_halign(gtk::Align::Center);
            spark.set_valign(gtk::Align::Center);
            spark.set_can_target(false);
            spark.set_visible(false);
            face.add_overlay(&spark);
            spark
        })
        .collect::<Vec<_>>();
    button.set_child(Some(&face));
    button.add_css_class("favorite-heart");
    let running = Rc::new(std::cell::RefCell::new(None::<gtk::glib::SourceId>));
    button.connect_clicked(move |button| {
        // Clicked handlers run before the favorite changes, so the heart shows its next state.
        celebrate(button, &burst, &running, !favorite_button_is_active(button));
    });
}

fn celebrate(
    button: &gtk::Button,
    burst: &[gtk::Image],
    running: &Rc<std::cell::RefCell<Option<gtk::glib::SourceId>>>,
    favorite: bool,
) {
    if let Some(source) = running.borrow_mut().take() {
        source.remove();
    }
    button.remove_css_class("pop");
    button.remove_css_class("drop");
    for spark in burst {
        spark.set_visible(false);
    }
    if !gtk::Settings::default().is_some_and(|settings| settings.is_gtk_enable_animations()) {
        return;
    }
    button.add_css_class(if favorite { "pop" } else { "drop" });
    for spark in burst.iter().filter(|_| favorite) {
        spark.set_visible(true);
    }
    let button = button.downgrade();
    let burst = burst
        .iter()
        .map(|spark| spark.downgrade())
        .collect::<Vec<_>>();
    let finished = Rc::clone(running);
    running.replace(Some(gtk::glib::timeout_add_local_once(
        std::time::Duration::from_millis(FAVORITE_BURST_MILLIS),
        move || {
            finished.take();
            if let Some(button) = button.upgrade() {
                button.remove_css_class("pop");
                button.remove_css_class("drop");
            }
            for spark in burst.iter().filter_map(|spark| spark.upgrade()) {
                spark.set_visible(false);
            }
        },
    )));
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
    let heart = button
        .child()
        .and_downcast::<gtk::Overlay>()
        .and_then(|face| face.child())
        .and_downcast::<gtk::Image>();
    match heart {
        Some(heart) => heart.set_icon_name(Some(icon_name)),
        None => button.set_icon_name(icon_name),
    }
}

pub fn favorite_button_is_active(button: &gtk::Button) -> bool {
    button.has_css_class("active-toggle")
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
