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

pub fn set_icon_source(area: &gtk::DrawingArea, context: &gtk::cairo::Context) {
    let color = area.color();
    context.set_source_rgba(
        f64::from(color.red()),
        f64::from(color.green()),
        f64::from(color.blue()),
        f64::from(color.alpha()),
    );
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

pub fn lyrics_icon_area(open: Rc<Cell<bool>>) -> gtk::DrawingArea {
    let icon = gtk::DrawingArea::new();
    icon.set_content_width(TRANSPORT_ICON_SIZE);
    icon.set_content_height(TRANSPORT_ICON_SIZE);
    icon.set_halign(gtk::Align::Center);
    icon.set_valign(gtk::Align::Center);
    icon.set_draw_func(move |area, context, width, height| {
        set_icon_source(area, context);
        context.set_line_width(1.7);
        context.set_line_cap(gtk::cairo::LineCap::Round);
        context.set_line_join(gtk::cairo::LineJoin::Round);

        let width = f64::from(width);
        let height = f64::from(height);
        let left = width * 0.25;
        let right = width * 0.75;
        let top = height * 0.25;
        let bottom = height * 0.66;
        let radius = width * 0.09;

        context.move_to(left + radius, top);
        context.line_to(right - radius, top);
        context.curve_to(right, top, right, top, right, top + radius);
        context.line_to(right, bottom - radius);
        context.curve_to(right, bottom, right, bottom, right - radius, bottom);
        context.line_to(width * 0.45, bottom);
        context.line_to(width * 0.32, height * 0.79);
        context.line_to(width * 0.34, bottom);
        context.line_to(left + radius, bottom);
        context.curve_to(left, bottom, left, bottom, left, bottom - radius);
        context.line_to(left, top + radius);
        context.curve_to(left, top, left, top, left + radius, top);
        let _ = context.stroke();

        if open.get() {
            context.move_to(width * 0.36, height * 0.42);
            context.line_to(width * 0.64, height * 0.42);
            context.move_to(width * 0.36, height * 0.54);
            context.line_to(width * 0.58, height * 0.54);
            let _ = context.stroke();
        }
    });
    icon
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
