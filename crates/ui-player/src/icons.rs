use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;
use playback::RepeatMode;

use localization::tr;

pub const TRANSPORT_ICON_SIZE: i32 = 23;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VolumeIcon {
    Low,
    Medium,
    High,
    Muted,
}

pub fn volume_icon_state(muted: bool, volume: f64) -> VolumeIcon {
    if muted || volume <= 0.0 {
        VolumeIcon::Muted
    } else if volume <= 1.0 / 3.0 {
        VolumeIcon::Low
    } else if volume <= 2.0 / 3.0 {
        VolumeIcon::Medium
    } else {
        VolumeIcon::High
    }
}

pub fn set_volume_icon(icon: &gtk::Image, state: VolumeIcon) {
    let name = match state {
        VolumeIcon::Low => "rufin-audio-volume-low-symbolic",
        VolumeIcon::Medium => "rufin-audio-volume-medium-symbolic",
        VolumeIcon::High => "rufin-audio-volume-high-symbolic",
        VolumeIcon::Muted => "rufin-audio-volume-muted-symbolic",
    };
    icon.set_icon_name(Some(name));
}

pub fn set_repeat_button_icon(button: &gtk::Button, repeat_mode: RepeatMode) {
    let name = if repeat_mode == RepeatMode::One {
        "rufin-repeat-one-symbolic"
    } else {
        "rufin-repeat-symbolic"
    };
    let icon = button
        .child()
        .and_downcast::<gtk::Image>()
        .expect("repeat icon");
    if icon.icon_name().as_deref() != Some(name) {
        icon.set_icon_name(Some(name));
    }
}

pub fn widget_icon_button(label: &str, icon: &impl IsA<gtk::Widget>) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("icon-button");
    button.add_css_class("flat");
    button.add_css_class("circular");
    button.set_tooltip_text(Some(&tr(label)));
    button.set_child(Some(icon));
    button
}

pub fn skip_icon_button(forward: bool, label: &str) -> gtk::Button {
    let icon = gtk::Image::from_icon_name(if forward {
        "rufin-media-skip-forward-symbolic"
    } else {
        "rufin-media-skip-backward-symbolic"
    });
    icon.set_halign(gtk::Align::Center);
    icon.set_valign(gtk::Align::Center);
    widget_icon_button(label, &icon)
}

pub fn play_icon_button(label: &str) -> (gtk::Button, gtk::Image) {
    let icon = gtk::Image::from_icon_name("rufin-media-playback-start-symbolic");
    icon.set_halign(gtk::Align::Center);
    icon.set_valign(gtk::Align::Center);
    icon.set_margin_start(4);
    let button = widget_icon_button(label, &icon);
    (button, icon)
}

pub fn set_play_icon(icon: &gtk::Image, playing: bool) {
    icon.set_margin_start(if playing { 2 } else { 4 });
    icon.set_icon_name(Some(if playing {
        "rufin-media-playback-pause-symbolic"
    } else {
        "rufin-media-playback-start-symbolic"
    }));
}

fn transport_image(name: &str) -> gtk::Image {
    let icon = gtk::Image::from_icon_name(name);
    icon.set_pixel_size(TRANSPORT_ICON_SIZE);
    icon.set_halign(gtk::Align::Center);
    icon.set_valign(gtk::Align::Center);
    icon
}

pub fn shuffle_icon_button(label: &str) -> gtk::Button {
    widget_icon_button(label, &transport_image("rufin-shuffle-symbolic"))
}

pub fn repeat_icon_button(label: &str) -> gtk::Button {
    let button = widget_icon_button(label, &transport_image("rufin-repeat-symbolic"));
    button.add_css_class("player-repeat-button");
    button
}

pub fn volume_icon_button(label: &str) -> (gtk::Button, gtk::Image, Rc<Cell<VolumeIcon>>) {
    let state = Rc::new(Cell::new(VolumeIcon::High));
    let icon = gtk::Image::from_icon_name("rufin-audio-volume-high-symbolic");
    icon.set_halign(gtk::Align::Center);
    icon.set_valign(gtk::Align::Center);
    let button = gtk::Button::new();
    button.add_css_class("icon-button");
    button.add_css_class("flat");
    button.add_css_class("circular");
    button.set_tooltip_text(Some(&tr(label)));
    button.set_child(Some(&icon));
    (button, icon, state)
}

pub fn auto_dj_icon_button(label: &str) -> gtk::Button {
    widget_icon_button(label, &transport_image("rufin-auto-dj-symbolic"))
}

pub fn random_clover_icon_button(label: &str) -> gtk::Button {
    widget_icon_button(label, &transport_image("rufin-random-symbolic"))
}

pub fn queue_sidebar_button(label: &str) -> (gtk::Button, gtk::Image) {
    let icon = gtk::Image::from_icon_name("rufin-sidebar-collapse-right-symbolic");
    let button = gtk::Button::new();
    button.add_css_class("icon-button");
    button.add_css_class("flat");
    button.add_css_class("circular");
    let label = tr(label);
    button.set_tooltip_text(Some(&label));
    button.update_property(&[gtk::accessible::Property::Label(&label)]);
    button.set_child(Some(&icon));
    (button, icon)
}

pub fn set_queue_sidebar_icon(icon: &gtk::Image, visible: bool) {
    icon.set_icon_name(Some(if visible {
        "rufin-sidebar-collapse-right-symbolic"
    } else {
        "rufin-sidebar-expand-right-symbolic"
    }));
}
