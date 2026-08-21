use std::{cell::RefCell, collections::HashMap, rc::Rc};

use crate::localization::bind_widget_tooltip;
use crate::shell::Shell;
use ::library::{AlbumId, ArtistId, FavoriteItemId, TrackId};
use adw::prelude::*;
use gtk::glib;
use localization::tr;

pub(crate) const FAVORITE_ADD_ICON: &str = "rufin-heart-outline-symbolic";
pub(crate) const FAVORITE_REMOVE_ICON: &str = "rufin-heart-filled-symbolic";

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum FavoriteControlKey {
    Album(String),
    Track(String),
    Artist(String),
}

#[derive(Default)]
struct FavoriteControls {
    static_controls: RefCell<HashMap<FavoriteControlKey, Vec<glib::WeakRef<gtk::Button>>>>,
    dynamic_controls: RefCell<Vec<DynamicFavoriteControl>>,
}

struct DynamicFavoriteControl {
    key: Rc<dyn Fn() -> Option<FavoriteControlKey>>,
    button: glib::WeakRef<gtk::Button>,
}

#[derive(Default)]
pub(crate) struct FavoriteSessionState {
    controls: FavoriteControls,
    pending_intents: RefCell<HashMap<FavoriteItemId, bool>>,
}

impl Shell {
    pub(crate) fn register_favorite_button(&self, key: FavoriteControlKey, button: &gtk::Button) {
        if let Some(session) = self.selected_ui.session() {
            register_favorite_control(&session.favorites.controls, key, button);
        }
    }

    pub(crate) fn register_dynamic_favorite_button(
        &self,
        key: Rc<dyn Fn() -> Option<FavoriteControlKey>>,
        button: &gtk::Button,
    ) {
        if let Some(session) = self.selected_ui.session() {
            register_dynamic_favorite_control(&session.favorites.controls, key, button);
        }
    }

    pub(crate) fn clear_favorite_controls(&self) {
        if let Some(session) = self.selected_ui.session() {
            session
                .favorites
                .controls
                .static_controls
                .borrow_mut()
                .clear();
            session
                .favorites
                .controls
                .dynamic_controls
                .borrow_mut()
                .clear();
        }
    }
}

pub(crate) fn album_favorite_key(album_id: &AlbumId) -> FavoriteControlKey {
    FavoriteControlKey::Album(album_id.as_str().to_string())
}

pub(crate) fn track_favorite_key(track_id: &TrackId) -> FavoriteControlKey {
    FavoriteControlKey::Track(track_id.as_str().to_string())
}

pub(crate) fn artist_favorite_key(artist_id: &ArtistId) -> FavoriteControlKey {
    FavoriteControlKey::Artist(artist_id.as_str().to_string())
}

fn favorite_control_key(item_id: &FavoriteItemId) -> FavoriteControlKey {
    match item_id {
        FavoriteItemId::Album(album_id) => album_favorite_key(album_id),
        FavoriteItemId::Track(track_id) => track_favorite_key(track_id),
        FavoriteItemId::Artist(artist_id) => artist_favorite_key(artist_id),
    }
}

fn register_favorite_control(
    controls: &FavoriteControls,
    key: FavoriteControlKey,
    button: &gtk::Button,
) {
    let weak = glib::WeakRef::new();
    weak.set(Some(button));
    controls
        .static_controls
        .borrow_mut()
        .entry(key)
        .or_default()
        .push(weak);
}

fn register_dynamic_favorite_control(
    controls: &FavoriteControls,
    key: Rc<dyn Fn() -> Option<FavoriteControlKey>>,
    button: &gtk::Button,
) {
    let weak = glib::WeakRef::new();
    weak.set(Some(button));
    controls
        .dynamic_controls
        .borrow_mut()
        .push(DynamicFavoriteControl { key, button: weak });
}

fn update_favorite_controls(controls: &FavoriteControls, key: &FavoriteControlKey, favorite: bool) {
    if let Some(buttons) = controls.static_controls.borrow_mut().get_mut(key) {
        buttons.retain(|button| {
            let Some(button) = button.upgrade() else {
                return false;
            };
            set_favorite_button_active(&button, favorite);
            true
        });
    }
    controls.dynamic_controls.borrow_mut().retain(|control| {
        let Some(button) = control.button.upgrade() else {
            return false;
        };
        if (control.key)().as_ref() == Some(key) {
            set_favorite_button_active(&button, favorite);
        }
        true
    });
}

pub(crate) fn favorite_icon_button(label: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(FAVORITE_ADD_ICON);
    button.add_css_class("icon-button");
    button.add_css_class("flat");
    button.add_css_class("circular");
    button.add_css_class("favorite-toggle");
    button.set_valign(gtk::Align::Center);
    bind_widget_tooltip(&button, label);
    button
}

pub(crate) fn set_favorite_button_active(button: &gtk::Button, active: bool) {
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

pub(crate) fn favorite_button_is_active(button: &gtk::Button) -> bool {
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
        let Some((track_id, playback_fallback)) = self
            .selected_playback()
            .as_deref()
            .and_then(|player| player.transport.current.as_ref())
            .map(|entry| (entry.track.id.clone(), entry.track.favorite))
        else {
            return;
        };
        let favorite = !self.projected_track_favorite(&track_id, playback_fallback);
        self.set_favorite_with_feedback(
            FavoriteItemId::Track(track_id),
            favorite,
            Some(&self.player_view.player_controls.favorite_button),
        );
    }

    pub(crate) fn projected_track_favorite(
        &self,
        track_id: &TrackId,
        playback_fallback: bool,
    ) -> bool {
        self.projected_item_favorite(&FavoriteItemId::Track(track_id.clone()), playback_fallback)
    }

    pub(crate) fn projected_item_favorite(&self, item_id: &FavoriteItemId, fallback: bool) -> bool {
        if let Some(pending) = self.selected_ui.session().and_then(|session| {
            session
                .favorites
                .pending_intents
                .borrow()
                .get(item_id)
                .copied()
        }) {
            return pending;
        }
        self.selected_library()
            .as_deref()
            .and_then(|selected| match item_id {
                FavoriteItemId::Track(id) => selected
                    .library
                    .track(id)
                    .ok()
                    .flatten()
                    .map(|track| track.favorite),
                FavoriteItemId::Album(id) => selected
                    .library
                    .album(id)
                    .ok()
                    .flatten()
                    .map(|album| album.favorite),
                FavoriteItemId::Artist(id) => selected
                    .library
                    .artist(id)
                    .ok()
                    .flatten()
                    .map(|artist| artist.favorite),
            })
            .unwrap_or(fallback)
    }

    pub(crate) fn update_visible_favorite_buttons(&self, item_id: &FavoriteItemId, favorite: bool) {
        let key = favorite_control_key(item_id);
        if let Some(session) = self.selected_ui.session() {
            update_favorite_controls(&session.favorites.controls, &key, favorite);
        }
    }

    pub(crate) fn set_favorite_with_feedback(
        self: &Rc<Self>,
        item_id: FavoriteItemId,
        favorite: bool,
        button: Option<&gtk::Button>,
    ) {
        let Some(source) = self.selected_source_operations() else {
            return;
        };
        let track_favorite_changed = matches!(item_id, FavoriteItemId::Track(_));
        if let Some(session) = self.selected_ui.session() {
            session
                .favorites
                .pending_intents
                .borrow_mut()
                .insert(item_id.clone(), favorite);
        }
        if let Some(button) = button {
            set_favorite_button_active(button, favorite);
        }
        self.update_visible_favorite_buttons(&item_id, favorite);
        source.set_favorite(item_id.clone(), favorite);
        if track_favorite_changed {
            self.sync_bottom_player_favorite();
        }
        let title = if favorite {
            tr("Added to favorites")
        } else {
            tr("Removed from favorites")
        };
        self.show_control_feedback_toast(title);
    }

    pub(crate) fn apply_favorite_changed(
        self: &Rc<Self>,
        item_id: FavoriteItemId,
        favorite: bool,
    ) -> bool {
        if !self.favorite_response_matches_pending(&item_id, favorite) {
            return false;
        }
        self.update_visible_favorite_buttons(&item_id, favorite);
        if matches!(item_id, FavoriteItemId::Track(_)) {
            self.sync_bottom_player_favorite();
        }
        true
    }

    fn favorite_response_matches_pending(&self, item_id: &FavoriteItemId, favorite: bool) -> bool {
        let Some(session) = self.selected_ui.session() else {
            return false;
        };
        let mut pending = session.favorites.pending_intents.borrow_mut();
        match pending.get(item_id).copied() {
            Some(intent) if intent == favorite => {
                pending.remove(item_id);
                true
            }
            Some(_) => false,
            None => true,
        }
    }
}
