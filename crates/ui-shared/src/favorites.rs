use std::{cell::RefCell, collections::HashMap, rc::Rc};

use crate::localization::bind_widget_tooltip;
use ::library::FavoriteTarget;
use adw::prelude::*;
use gtk::glib;

pub const FAVORITE_ADD_ICON: &str = "rufin-heart-outline-symbolic";
pub const FAVORITE_REMOVE_ICON: &str = "rufin-heart-filled-symbolic";
pub const FAVORITE_COLUMN_TITLE: &str = " ♡";
pub const FAVORITE_COLUMN_WIDTH: i32 = 32;

pub type FavoriteControlKey = FavoriteTarget;

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
pub struct FavoriteSessionState {
    controls: FavoriteControls,
    pending_intents: RefCell<HashMap<FavoriteTarget, bool>>,
}

pub fn album_favorite_key(media_uri: &str) -> FavoriteControlKey {
    FavoriteTarget::Album(media_uri.to_string())
}

pub fn track_favorite_key(media_uri: &str) -> FavoriteControlKey {
    FavoriteTarget::Track(media_uri.to_string())
}

pub fn artist_favorite_key(media_uri: &str) -> FavoriteControlKey {
    FavoriteTarget::Artist(media_uri.to_string())
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
    button
}

pub fn column_favorite_icon_button(label: &str) -> gtk::Button {
    let button = row_favorite_icon_button(label);
    button.set_halign(gtk::Align::Start);
    if let Some(image) = button.child().and_then(icon_image_from_widget) {
        image.set_halign(gtk::Align::Start);
    }
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

impl FavoriteSessionState {
    pub fn register_favorite_button(&self, key: FavoriteControlKey, button: &gtk::Button) {
        register_favorite_control(&self.controls, key, button);
    }
    pub fn register_dynamic_favorite_button(
        &self,
        key: Rc<dyn Fn() -> Option<FavoriteControlKey>>,
        button: &gtk::Button,
    ) {
        register_dynamic_favorite_control(&self.controls, key, button);
    }
    pub fn clear_favorite_controls(&self) {
        self.controls.static_controls.borrow_mut().clear();
        self.controls.dynamic_controls.borrow_mut().clear();
    }
    pub fn projected_item_favorite(&self, item: &FavoriteTarget, fallback: bool) -> bool {
        self.pending_intents
            .borrow()
            .get(item)
            .copied()
            .unwrap_or(fallback)
    }
    pub fn set_pending(&self, item: FavoriteTarget, favorite: bool) {
        self.pending_intents.borrow_mut().insert(item, favorite);
    }
    pub fn update_visible_favorite_buttons(&self, item: &FavoriteTarget, favorite: bool) {
        update_favorite_controls(&self.controls, item, favorite);
    }
    pub fn response_matches_pending(&self, item: &FavoriteTarget, requested: bool) -> bool {
        let mut pending = self.pending_intents.borrow_mut();
        match pending.get(item).copied() {
            Some(intent) if intent == requested => {
                pending.remove(item);
                true
            }
            Some(_) => false,
            None => true,
        }
    }
}
