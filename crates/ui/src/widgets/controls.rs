use crate::widgets::localization::{bind_widget_accessible_label, bind_widget_tooltip};
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

pub fn configure_dialog_header(header: &adw::HeaderBar) {
    if let Some(settings) = gtk::Settings::default() {
        settings
            .bind_property("gtk-decoration-layout", header, "decoration-layout")
            .sync_create()
            .build();
    }
}

pub fn configure_search_entry(entry: &gtk::SearchEntry) {
    entry
        .first_child()
        .and_downcast::<gtk::Image>()
        .expect("search entry icon")
        .set_icon_name(Some("rufin-search-symbolic"));
    let click = gtk::GestureClick::new();
    click.set_button(1);
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    let search = entry.downgrade();
    click.connect_pressed(move |_, _, _, _| {
        if let Some(search) = search.upgrade() {
            search.grab_focus();
        }
    });
    entry.add_controller(click);
}

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
/// Play, Play Next and Play Later at the end of `row`, shown while the pointer is over the row or
/// its menu holds it: the hover controls of the sidebar's Saved rows and Home's rows.
pub fn install_row_transport_controls(
    row: &gtk::Overlay,
    play: impl Fn(rufin_core::playback::QueuePlacement) + 'static,
) {
    use crate::widgets::interactions::CONTEXT_MENU_HOVER_HELD_CLASS;
    use rufin_core::playback::QueuePlacement;
    let play = std::rc::Rc::new(play);
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 1);
    controls.add_css_class("row-transport-controls");
    controls.set_halign(gtk::Align::End);
    controls.set_valign(gtk::Align::Center);
    controls.set_margin_end(2);
    controls.set_visible(false);
    for (icon_name, label, placement) in [
        (PLAY_ICON, "Play", QueuePlacement::Now),
        (PLAY_NEXT_ICON, "Play Next", QueuePlacement::Next),
        (PLAY_LATER_ICON, "Play Later", QueuePlacement::Last),
    ] {
        let button = icon_button(icon_name, label);
        button.add_css_class("row-transport-control");
        let play = std::rc::Rc::clone(&play);
        button.connect_clicked(move |_| play(placement));
        controls.append(&button);
    }
    row.add_overlay(&controls);
    row.set_measure_overlay(&controls, false);

    let motion = gtk::EventControllerMotion::new();
    let controls_for_enter = controls.clone();
    motion.connect_enter(move |_, _, _| controls_for_enter.set_visible(true));
    let controls_for_leave = controls.clone();
    let row_for_leave = row.downgrade();
    motion.connect_leave(move |_| {
        if row_for_leave
            .upgrade()
            .is_some_and(|row| !row.has_css_class(CONTEXT_MENU_HOVER_HELD_CLASS))
        {
            controls_for_leave.set_visible(false);
        }
    });
    let motion_for_hold = motion.clone();
    row.connect_css_classes_notify(move |row| {
        if !row.has_css_class(CONTEXT_MENU_HOVER_HELD_CLASS) {
            controls.set_visible(motion_for_hold.contains_pointer());
        }
    });
    row.add_controller(motion);
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
}

pub const COVER_SIDE_ACTION_SIZE: i32 = 34;
pub const COVER_PRIMARY_ACTION_SIZE: i32 = 54;

pub fn configure_action_button(button: &gtk::Button, variant: ActionButtonVariant) {
    button.add_css_class("cover-hover-button");
    button.add_css_class("cover-hover-animated");
    button.set_focus_on_click(false);

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
    }
    wrap_button_child_in_action_layers(button);
}

fn pin_action_button(button: &gtk::Button, size: i32) {
    button.set_size_request(size, size);
    button.set_halign(gtk::Align::Center);
    button.set_valign(gtk::Align::Center);
}

fn wrap_button_child_in_action_layers(button: &gtk::Button) {
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
    face.add_css_class("cover-hover-face");
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
