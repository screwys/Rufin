use crate::localization::{bind_widget_accessible_label, bind_widget_tooltip};
use adw::prelude::*;
use localization::tr;
pub const PLAY_ICON: &str = "rufin-media-playback-start-symbolic";
pub const PLAY_NEXT_ICON: &str = "rufin-mail-forward-symbolic";
pub const PLAY_LATER_ICON: &str = "rufin-go-last-symbolic";
pub const EDIT_ICON: &str = "rufin-document-edit-symbolic";
pub const ADD_ICON: &str = "rufin-list-add-symbolic";
pub const REMOVE_ICON: &str = "rufin-list-remove-symbolic";
pub const DELETE_ICON: &str = "rufin-process-stop-symbolic";
pub const TRASH_ICON: &str = "rufin-user-trash-symbolic";
pub const MORE_ICON: &str = "rufin-more-symbolic";
const SORT_ORDER_ICON: &str = "rufin-sort-name-symbolic";
const SORT_ORDER_DESCENDING_ICON: &str = "rufin-sort-name-descending-symbolic";

pub fn sort_order_icon(descending: bool) -> &'static str {
    if descending {
        SORT_ORDER_DESCENDING_ICON
    } else {
        SORT_ORDER_ICON
    }
}

pub fn set_active_class(widget: &impl IsA<gtk::Widget>, active: bool) {
    if active {
        widget.add_css_class("active-toggle");
    } else {
        widget.remove_css_class("active-toggle");
    }
}
pub fn icon_button(icon_name: &str, label: &str) -> gtk::Button {
    let button = base_icon_button(icon_name);
    bind_widget_tooltip(&button, label);
    button
}

pub fn icon_button_without_tooltip(icon_name: &str, label: &str) -> gtk::Button {
    let button = base_icon_button(icon_name);
    bind_widget_accessible_label(&button, label);
    button
}

fn base_icon_button(icon_name: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon_name);
    button.add_css_class("icon-button");
    button.add_css_class("flat");
    button.add_css_class("circular");
    button.set_valign(gtk::Align::Center);
    button
}

#[derive(Clone, Copy)]
pub enum ActionButtonVariant {
    CoverSideTransport,
    CoverPrimaryTransport,
    CoverCornerMenu,
    CoverCornerFavorite,
    DetailAction,
    DetailPrimary,
    DetailFavorite,
}

pub const COVER_SIDE_ACTION_SIZE: i32 = 34;
pub const COVER_PRIMARY_ACTION_SIZE: i32 = 54;

pub fn configure_action_button(button: &gtk::Button, variant: ActionButtonVariant) {
    let is_cover = matches!(
        variant,
        ActionButtonVariant::CoverSideTransport
            | ActionButtonVariant::CoverPrimaryTransport
            | ActionButtonVariant::CoverCornerMenu
            | ActionButtonVariant::CoverCornerFavorite
    );
    if is_cover {
        button.add_css_class("cover-hover-button");
        button.add_css_class("cover-hover-animated");
        button.set_focus_on_click(false);
    } else {
        button.add_css_class("detail-showcase-action-button");
    }

    match variant {
        ActionButtonVariant::CoverSideTransport => {
            button.add_css_class("cover-side-button");
            pin_action_button(button, COVER_SIDE_ACTION_SIZE);
        }
        ActionButtonVariant::CoverPrimaryTransport => {
            button.add_css_class("cover-play-button");
            pin_action_button(button, COVER_PRIMARY_ACTION_SIZE);
        }
        ActionButtonVariant::CoverCornerMenu => {
            button.add_css_class("cover-menu-button");
            pin_action_button(button, COVER_SIDE_ACTION_SIZE);
            button.set_halign(gtk::Align::Start);
            button.set_valign(gtk::Align::End);
        }
        ActionButtonVariant::CoverCornerFavorite => {
            button.add_css_class("cover-favorite-button");
            pin_action_button(button, COVER_SIDE_ACTION_SIZE);
            button.set_halign(gtk::Align::End);
            button.set_valign(gtk::Align::Start);
        }
        ActionButtonVariant::DetailAction => {}
        ActionButtonVariant::DetailPrimary => {
            button.add_css_class("detail-showcase-play-button");
        }
        ActionButtonVariant::DetailFavorite => {}
    }
    let face_class = if is_cover {
        "cover-hover-face"
    } else {
        "detail-showcase-action-face"
    };
    wrap_button_child_in_action_layers(button, face_class);
}

fn pin_action_button(button: &gtk::Button, size: i32) {
    button.set_size_request(size, size);
    button.set_halign(gtk::Align::Center);
    button.set_valign(gtk::Align::Center);
}

fn wrap_button_child_in_action_layers(button: &gtk::Button, face_class: &str) {
    let Some(child) = button.child() else {
        return;
    };
    button.set_child(None::<&gtk::Widget>);
    child.set_halign(gtk::Align::Center);
    child.set_valign(gtk::Align::Center);

    let shadow = gtk::CenterBox::new();
    shadow.add_css_class("action-button-shadow");
    shadow.set_can_target(false);

    let face = gtk::CenterBox::new();
    face.add_css_class(face_class);
    face.set_can_target(false);
    face.set_center_widget(Some(&child));
    shadow.set_center_widget(Some(&face));
    button.set_child(Some(&shadow));
}

pub fn text_button(icon_name: &str, label: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("pill-button");
    button.add_css_class("pill");
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.append(&gtk::Image::from_icon_name(icon_name));
    content.append(&gtk::Label::new(Some(&tr(label))));
    button.set_child(Some(&content));
    button
}
