use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;
use playback::RepeatMode;

use localization::tr;

const TRANSPORT_ICON_SIZE: i32 = 23;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum VolumeIcon {
    Low,
    Medium,
    High,
    Muted,
}

pub(super) fn volume_icon_state(muted: bool, volume: f64) -> VolumeIcon {
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

pub(super) fn set_volume_icon(icon: &gtk::Image, state: VolumeIcon) {
    let name = match state {
        VolumeIcon::Low => "rufin-audio-volume-low-symbolic",
        VolumeIcon::Medium => "rufin-audio-volume-medium-symbolic",
        VolumeIcon::High => "rufin-audio-volume-high-symbolic",
        VolumeIcon::Muted => "rufin-audio-volume-muted-symbolic",
    };
    icon.set_icon_name(Some(name));
}

pub(super) fn set_repeat_button_icon(button: &gtk::Button, repeat_mode: RepeatMode) {
    button.set_child(Some(&repeat_icon_area(repeat_mode)));
}

fn set_icon_source(area: &gtk::DrawingArea, context: &gtk::cairo::Context) {
    let color = area.color();
    context.set_source_rgba(
        f64::from(color.red()),
        f64::from(color.green()),
        f64::from(color.blue()),
        f64::from(color.alpha()),
    );
}

fn drawing_icon_button(label: &str, icon: gtk::DrawingArea) -> gtk::Button {
    widget_icon_button(label, &icon)
}

fn widget_icon_button(label: &str, icon: &impl IsA<gtk::Widget>) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("icon-button");
    button.add_css_class("flat");
    button.add_css_class("circular");
    button.set_tooltip_text(Some(&tr(label)));
    button.set_child(Some(icon));
    button
}

pub(super) fn skip_icon_button(forward: bool, label: &str) -> gtk::Button {
    let icon = gtk::Image::from_icon_name(if forward {
        "rufin-media-skip-forward-symbolic"
    } else {
        "rufin-media-skip-backward-symbolic"
    });
    icon.set_halign(gtk::Align::Center);
    icon.set_valign(gtk::Align::Center);
    widget_icon_button(label, &icon)
}

pub(super) fn play_icon_button(label: &str) -> (gtk::Button, gtk::Image) {
    let icon = gtk::Image::from_icon_name("rufin-media-playback-start-symbolic");
    icon.set_halign(gtk::Align::Center);
    icon.set_valign(gtk::Align::Center);
    icon.set_margin_start(4);
    let button = widget_icon_button(label, &icon);
    (button, icon)
}

pub(super) fn set_play_icon(icon: &gtk::Image, playing: bool) {
    icon.set_margin_start(if playing { 2 } else { 4 });
    icon.set_icon_name(Some(if playing {
        "rufin-media-playback-pause-symbolic"
    } else {
        "rufin-media-playback-start-symbolic"
    }));
}

pub(super) fn shuffle_icon_button(label: &str) -> gtk::Button {
    let icon = gtk::DrawingArea::new();
    icon.set_content_width(TRANSPORT_ICON_SIZE);
    icon.set_content_height(TRANSPORT_ICON_SIZE);
    icon.set_halign(gtk::Align::Center);
    icon.set_valign(gtk::Align::Center);
    icon.set_draw_func(move |area, context, width, height| {
        set_icon_source(area, context);
        context.set_line_width(1.8);
        context.set_line_cap(gtk::cairo::LineCap::Round);
        context.set_line_join(gtk::cairo::LineJoin::Round);

        let width = f64::from(width);
        let height = f64::from(height);
        let left = width * 0.22;
        let right = width * 0.72;
        let arrow = width * 0.13;
        let top_y = height * 0.34;
        let bottom_y = height * 0.66;

        context.move_to(left, top_y);
        context.curve_to(width * 0.38, top_y, width * 0.43, bottom_y, right, bottom_y);
        context.line_to(right - arrow, bottom_y - arrow * 0.75);
        context.move_to(right, bottom_y);
        context.line_to(right - arrow, bottom_y + arrow * 0.75);

        context.move_to(left, bottom_y);
        context.curve_to(width * 0.38, bottom_y, width * 0.43, top_y, right, top_y);
        context.line_to(right - arrow, top_y - arrow * 0.75);
        context.move_to(right, top_y);
        context.line_to(right - arrow, top_y + arrow * 0.75);
        let _ = context.stroke();
    });
    drawing_icon_button(label, icon)
}

pub(super) fn repeat_icon_button(label: &str) -> gtk::Button {
    let button = drawing_icon_button(label, repeat_icon_area(RepeatMode::Off));
    button.add_css_class("player-repeat-button");
    button
}

fn repeat_icon_area(repeat_mode: RepeatMode) -> gtk::DrawingArea {
    let icon = gtk::DrawingArea::new();
    icon.set_content_width(TRANSPORT_ICON_SIZE);
    icon.set_content_height(TRANSPORT_ICON_SIZE);
    icon.set_halign(gtk::Align::Center);
    icon.set_valign(gtk::Align::Center);
    icon.set_draw_func(move |area, context, width, height| {
        set_icon_source(area, context);
        context.set_line_width(1.9);
        context.set_line_cap(gtk::cairo::LineCap::Round);
        context.set_line_join(gtk::cairo::LineJoin::Round);

        let width = f64::from(width);
        let height = f64::from(height);
        let center_x = width / 2.0;
        let center_y = height / 2.0;
        let radius = width.min(height) * 0.29;
        context.arc(
            center_x,
            center_y,
            radius,
            -0.15,
            std::f64::consts::PI * 1.72,
        );
        let _ = context.stroke();

        let arrow_x = center_x + radius * (-0.15_f64).cos();
        let arrow_y = center_y + radius * (-0.15_f64).sin();
        let arrow = width.min(height) * 0.13;
        context.move_to(arrow_x, arrow_y);
        context.line_to(arrow_x - arrow * 0.98, arrow_y - arrow * 0.35);
        context.move_to(arrow_x, arrow_y);
        context.line_to(arrow_x - arrow * 0.38, arrow_y + arrow);
        let _ = context.stroke();

        if repeat_mode == RepeatMode::One {
            context.set_line_width(1.35);
            let one_x = width / 2.0;
            let one_top = height * 0.40;
            let one_bottom = height * 0.66;
            context.move_to(one_x, one_top);
            context.line_to(one_x, one_bottom);
            context.move_to(one_x - 1.5, one_top + 1.0);
            context.line_to(one_x, one_top);
            let _ = context.stroke();
        }
    });
    icon
}

pub(super) fn volume_icon_button(label: &str) -> (gtk::Button, gtk::Image, Rc<Cell<VolumeIcon>>) {
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

pub(super) fn lyrics_icon_area(open: Rc<Cell<bool>>) -> gtk::DrawingArea {
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

pub(super) fn auto_dj_icon_button(label: &str) -> gtk::Button {
    let icon = gtk::DrawingArea::new();
    icon.set_content_width(TRANSPORT_ICON_SIZE);
    icon.set_content_height(TRANSPORT_ICON_SIZE);
    icon.set_halign(gtk::Align::Center);
    icon.set_valign(gtk::Align::Center);
    icon.set_draw_func(move |area, context, width, height| {
        set_icon_source(area, context);
        context.set_line_width(1.8);
        context.set_line_cap(gtk::cairo::LineCap::Round);
        context.set_line_join(gtk::cairo::LineJoin::Round);

        let width = f64::from(width);
        let height = f64::from(height);
        let center_x = width / 2.0;
        let center_y = height * 0.53;
        let radius = width.min(height) * 0.29;

        context.arc(
            center_x,
            center_y,
            radius,
            std::f64::consts::PI,
            std::f64::consts::TAU,
        );
        let _ = context.stroke();

        context.rectangle(width * 0.22, height * 0.50, width * 0.13, height * 0.24);
        context.rectangle(width * 0.65, height * 0.50, width * 0.13, height * 0.24);
        let _ = context.stroke();

        context.set_line_width(1.45);
        context.move_to(width * 0.75, height * 0.21);
        context.line_to(width * 0.75, height * 0.36);
        context.move_to(width * 0.68, height * 0.285);
        context.line_to(width * 0.82, height * 0.285);
        let _ = context.stroke();
    });
    drawing_icon_button(label, icon)
}

pub(super) fn random_clover_icon_button(label: &str) -> gtk::Button {
    let icon = gtk::DrawingArea::new();
    icon.set_content_width(TRANSPORT_ICON_SIZE);
    icon.set_content_height(TRANSPORT_ICON_SIZE);
    icon.set_halign(gtk::Align::Center);
    icon.set_valign(gtk::Align::Center);
    icon.set_draw_func(move |area, context, width, height| {
        set_icon_source(area, context);
        let width = f64::from(width);
        let height = f64::from(height);
        let center_x = width / 2.0;
        let center_y = height * 0.48;
        let leaf_radius = width.min(height) * 0.14;

        for (x, y) in [
            (center_x - leaf_radius, center_y - leaf_radius),
            (center_x + leaf_radius, center_y - leaf_radius),
            (center_x - leaf_radius, center_y + leaf_radius),
            (center_x + leaf_radius, center_y + leaf_radius),
        ] {
            context.arc(x, y, leaf_radius, 0.0, std::f64::consts::TAU);
            let _ = context.fill();
        }

        context.set_line_width(1.6);
        context.set_line_cap(gtk::cairo::LineCap::Round);
        context.move_to(center_x + width * 0.02, center_y + leaf_radius * 1.25);
        context.curve_to(
            center_x + width * 0.12,
            height * 0.70,
            center_x + width * 0.02,
            height * 0.77,
            center_x - width * 0.10,
            height * 0.83,
        );
        let _ = context.stroke();
    });
    drawing_icon_button(label, icon)
}

pub(super) fn queue_sidebar_button(label: &str) -> (gtk::Button, gtk::Image) {
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

pub(super) fn set_queue_sidebar_icon(icon: &gtk::Image, visible: bool) {
    icon.set_icon_name(Some(if visible {
        "rufin-sidebar-collapse-right-symbolic"
    } else {
        "rufin-sidebar-expand-right-symbolic"
    }));
}
