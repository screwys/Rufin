use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use localization::tr;
use playback::{
    MAX_CROSSFADE_SECONDS, MAX_PLAYBACK_RATE, MIN_CROSSFADE_SECONDS, MIN_PLAYBACK_RATE,
    PlaybackTransitionMode,
};

use crate::preferences::{
    selection_row, transition_from_index, transition_index, volume_scale_from_index,
    volume_scale_index,
};
use crate::shell::Shell;

use super::outputs::audio_output_dropdown;

const PLAYBACK_RATE_COMMIT_DELAY: Duration = Duration::from_millis(200);
const SCALE_SURFACE_SCROLL_FACTOR: f64 = 2.5;
const PLAYBACK_SETTINGS_SCALE_WIDTH: i32 = 180;

pub(crate) fn configure_playback_settings_popover(button: &gtk::MenuButton, shell: &Rc<Shell>) {
    let shell = Rc::clone(shell);
    button.set_create_popup_func(move |button| {
        button.set_popover(Some(&playback_settings_popover(&shell)));
    });
}

fn playback_settings_popover(shell: &Rc<Shell>) -> gtk::Popover {
    let settings = shell.settings.current.borrow().clone();
    let playback = settings.playback;
    let local_output = shell
        .products
        .playback
        .transport
        .playback_output()
        .is_local();

    let resource = crate::ui_resource::PLAYBACK_SETTINGS_POPOVER_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    crate::ui_resource::objects!(builder, resource, {
        popover: gtk::Popover,
        settings_group: adw::PreferencesGroup,
        output_row: adw::ActionRow,
        lyrics: adw::SwitchRow,
        visualizer: adw::SwitchRow,
    });
    if !local_output {
        settings_group.set_description(Some(&tr("Audio processing is unavailable while casting")));
    }
    let crossfade_row = crossfade_duration_row(
        shell,
        playback.crossfade_seconds,
        PLAYBACK_SETTINGS_SCALE_WIDTH,
    );
    crossfade_row.set_sensitive(playback.transition_mode == PlaybackTransitionMode::Crossfade);

    let transition_shell = Rc::clone(shell);
    let crossfade_row_for_transition = crossfade_row.clone();
    let transition_row = selection_row(
        &tr("Transition mode"),
        &[tr("Gapless"), tr("Crossfade")],
        transition_index(playback.transition_mode),
        move |selected| {
            let mode = transition_from_index(selected);
            crossfade_row_for_transition.set_sensitive(mode == PlaybackTransitionMode::Crossfade);
            transition_shell.update_playback_settings(|settings| settings.transition_mode = mode);
        },
    );
    transition_row.set_sensitive(local_output);
    settings_group.add(&transition_row);
    crossfade_row.set_sensitive(
        local_output && playback.transition_mode == PlaybackTransitionMode::Crossfade,
    );
    settings_group.add(&crossfade_row);
    let playback_rate =
        playback_rate_row(shell, playback.playback_rate, PLAYBACK_SETTINGS_SCALE_WIDTH);
    playback_rate.set_sensitive(local_output);
    settings_group.add(&playback_rate);
    let preserve_pitch = preserve_pitch_row(shell, playback.preserve_pitch);
    preserve_pitch.set_sensitive(local_output);
    settings_group.add(&preserve_pitch);

    let output_dropdown = audio_output_dropdown(shell, PLAYBACK_SETTINGS_SCALE_WIDTH);
    output_row.add_suffix(&output_dropdown);
    output_row.set_activatable_widget(Some(&output_dropdown));
    output_row.set_sensitive(local_output);
    settings_group.add(&output_row);

    let volume_scale_shell = Rc::clone(shell);
    let volume_scale_row = selection_row(
        &tr("Volume scale"),
        &[tr("Perceptual"), tr("Linear")],
        volume_scale_index(playback.volume_scale),
        move |selected| {
            let scale = volume_scale_from_index(selected);
            volume_scale_shell.update_playback_settings(|settings| {
                settings.set_volume_scale_preserving_gain(scale);
            });
        },
    );
    volume_scale_row.set_sensitive(local_output);
    settings_group.add(&volume_scale_row);

    lyrics.set_active(settings.lyrics_panel_visible);
    let lyrics_shell = Rc::clone(shell);
    lyrics.connect_active_notify(move |row| {
        lyrics_shell.set_lyrics_panel_visible(row.is_active());
    });
    settings_group.add(&lyrics);
    visualizer.set_active(settings.visualizer_panel_visible);
    visualizer.set_sensitive(local_output);
    let visualizer_shell = Rc::clone(shell);
    visualizer.connect_active_notify(move |row| {
        visualizer_shell.set_visualizer_panel_visible(row.is_active());
    });
    settings_group.add(&visualizer);
    popover
}

pub(crate) fn crossfade_duration_row(
    shell: &Rc<Shell>,
    initial_seconds: u8,
    scale_width: i32,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(tr("Crossfade duration"))
        .build();
    let scale = gtk::Scale::with_range(
        gtk::Orientation::Horizontal,
        f64::from(MIN_CROSSFADE_SECONDS),
        f64::from(MAX_CROSSFADE_SECONDS),
        1.0,
    );
    scale.add_css_class("playback-setting-scale");
    install_scale_scroll_forwarding(&scale);
    scale.set_width_request(scale_width);
    scale.set_valign(gtk::Align::Center);
    install_sliding_value_bubble(&scale, |seconds| format!("{seconds:.0}"));
    scale.add_mark(1.0, gtk::PositionType::Bottom, Some("1"));
    scale.add_mark(5.0, gtk::PositionType::Bottom, Some("5"));
    scale.add_mark(10.0, gtk::PositionType::Bottom, Some("10"));
    scale.add_mark(15.0, gtk::PositionType::Bottom, Some("15"));
    scale.add_mark(20.0, gtk::PositionType::Bottom, Some("20"));
    scale.add_mark(25.0, gtk::PositionType::Bottom, Some("25"));
    scale.add_mark(30.0, gtk::PositionType::Bottom, Some("30"));
    scale.set_value(f64::from(initial_seconds));
    let crossfade_shell = Rc::clone(shell);
    scale.connect_value_changed(move |scale| {
        let seconds = scale.value().round() as u8;
        crossfade_shell.update_playback_settings(|settings| settings.crossfade_seconds = seconds);
    });
    row.add_suffix(&scale);
    row.set_activatable_widget(Some(&scale));
    row
}

pub(crate) fn playback_rate_row(
    shell: &Rc<Shell>,
    initial_rate: f64,
    scale_width: i32,
) -> adw::ActionRow {
    let title_text = tr("Playback speed");
    let row = adw::ActionRow::builder().title(&title_text).build();
    let scale = gtk::Scale::with_range(
        gtk::Orientation::Horizontal,
        MIN_PLAYBACK_RATE,
        MAX_PLAYBACK_RATE,
        0.05,
    );
    scale.add_css_class("playback-setting-scale");
    install_scale_scroll_forwarding(&scale);
    scale.set_width_request(scale_width);
    install_sliding_value_bubble(&scale, playback_rate_value);
    scale.add_mark(0.5, gtk::PositionType::Bottom, Some("0.5"));
    scale.add_mark(0.75, gtk::PositionType::Bottom, Some("0.75"));
    scale.add_mark(1.0, gtk::PositionType::Bottom, Some("1"));
    scale.add_mark(1.25, gtk::PositionType::Bottom, Some("1.25"));
    scale.add_mark(1.5, gtk::PositionType::Bottom, Some("1.5"));
    scale.add_mark(1.75, gtk::PositionType::Bottom, Some("1.75"));
    scale.add_mark(2.0, gtk::PositionType::Bottom, Some("2"));
    scale.set_value(initial_rate);
    scale.update_property(&[gtk::accessible::Property::Label(&title_text)]);
    row.add_suffix(&scale);
    row.set_activatable_widget(Some(&scale));

    let pending_rate = Rc::new(RefCell::new(None::<glib::SourceId>));
    let speed_shell = Rc::clone(shell);
    let pending_rate_for_change = Rc::clone(&pending_rate);
    scale.connect_value_changed(move |scale| {
        let rate = scale.value();
        if let Some(source) = pending_rate_for_change.borrow_mut().take() {
            source.remove();
        }
        let commit_shell = Rc::clone(&speed_shell);
        let commit_source = Rc::clone(&pending_rate_for_change);
        let source = glib::timeout_add_local_once(PLAYBACK_RATE_COMMIT_DELAY, move || {
            commit_source.borrow_mut().take();
            commit_shell.update_playback_settings(|settings| settings.playback_rate = rate);
        });
        pending_rate_for_change.borrow_mut().replace(source);
    });
    row
}

pub(crate) fn preserve_pitch_row(shell: &Rc<Shell>, active: bool) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder()
        .title(tr("Preserve pitch"))
        .active(active)
        .build();
    let pitch_shell = Rc::clone(shell);
    row.connect_active_notify(move |row| {
        pitch_shell.update_playback_settings(|settings| {
            settings.preserve_pitch = row.is_active();
        });
    });
    row
}

pub(crate) fn install_scale_scroll_forwarding(scale: &gtk::Scale) {
    let controller = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let scale_weak = scale.downgrade();
    controller.connect_scroll(move |controller, _, dy| {
        if dy == 0.0 {
            return gtk::glib::Propagation::Proceed;
        }
        let Some(scale) = scale_weak.upgrade() else {
            return gtk::glib::Propagation::Stop;
        };
        if let Some(scroller) = nearest_parent_scroller(scale.upcast_ref()) {
            let adjustment = scroller.vadjustment();
            adjustment.set_value(scale_scroll_target(
                adjustment.value(),
                adjustment.lower(),
                adjustment.upper(),
                adjustment.page_size(),
                dy,
                controller.unit() == gtk::gdk::ScrollUnit::Surface,
            ));
        }
        gtk::glib::Propagation::Stop
    });
    scale.add_controller(controller);
}

fn nearest_parent_scroller(widget: &gtk::Widget) -> Option<gtk::ScrolledWindow> {
    let mut parent = widget.parent();
    while let Some(widget) = parent {
        if let Ok(scroller) = widget.clone().downcast::<gtk::ScrolledWindow>() {
            return Some(scroller);
        }
        parent = widget.parent();
    }
    None
}

fn scale_scroll_target(
    value: f64,
    lower: f64,
    upper: f64,
    page_size: f64,
    dy: f64,
    surface_units: bool,
) -> f64 {
    let multiplier = if surface_units {
        SCALE_SURFACE_SCROLL_FACTOR
    } else {
        page_size.powf(2.0 / 3.0)
    };
    (value + dy * multiplier).clamp(lower, (upper - page_size).max(lower))
}

pub(crate) fn install_sliding_value_bubble(
    scale: &gtk::Scale,
    format_value: impl Fn(f64) -> String + 'static,
) {
    scale.set_draw_value(false);
    let format_value: Rc<dyn Fn(f64) -> String> = Rc::new(format_value);
    let value = gtk::Label::new(Some(&format_value(scale.value())));
    value.set_halign(gtk::Align::Center);
    value.set_valign(gtk::Align::Center);
    value.set_xalign(0.5);
    value.set_yalign(0.5);
    let bubble = gtk::Popover::new();
    bubble.add_css_class("slider-value-popover");
    bubble.set_autohide(false);
    bubble.set_focusable(false);
    bubble.set_has_arrow(false);
    bubble.set_position(gtk::PositionType::Top);
    bubble.set_offset(0, -4);
    bubble.set_child(Some(&value));
    bubble.set_parent(scale);

    let value_for_change = value.clone();
    let bubble_for_change = bubble.clone();
    let format_for_change = Rc::clone(&format_value);
    scale.connect_value_changed(move |scale| {
        value_for_change.set_text(&format_for_change(scale.value()));
        point_value_bubble_at_slider(scale, &bubble_for_change);
    });

    let events = gtk::EventControllerLegacy::new();
    events.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak_scale = scale.downgrade();
    let value_for_event = value.clone();
    let bubble_for_event = bubble.clone();
    let format_for_event = Rc::clone(&format_value);
    events.connect_event(move |_, event| {
        let visible = match event.event_type() {
            gtk::gdk::EventType::ButtonPress
            | gtk::gdk::EventType::TouchBegin
            | gtk::gdk::EventType::KeyPress => Some(true),
            gtk::gdk::EventType::ButtonRelease
            | gtk::gdk::EventType::TouchEnd
            | gtk::gdk::EventType::TouchCancel
            | gtk::gdk::EventType::KeyRelease
            | gtk::gdk::EventType::GrabBroken => Some(false),
            _ => None,
        };
        if let Some(visible) = visible {
            if visible {
                if let Some(scale) = weak_scale.upgrade() {
                    value_for_event.set_text(&format_for_event(scale.value()));
                    point_value_bubble_at_slider(&scale, &bubble_for_event);
                    bubble_for_event.popup();
                }
            } else {
                bubble_for_event.popdown();
            }
        }
        glib::Propagation::Proceed
    });
    scale.add_controller(events);
    scale.connect_destroy(move |_| bubble.unparent());
}

fn point_value_bubble_at_slider(scale: &gtk::Scale, bubble: &gtk::Popover) {
    let (slider_start, slider_end) = scale.slider_range();
    let range = scale.range_rect();
    let slider_center = slider_start.saturating_add(slider_end.saturating_sub(slider_start) / 2);
    bubble.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
        slider_center,
        range.y(),
        1,
        1,
    )));
}

fn playback_rate_value(rate: f64) -> String {
    let mut value = format!("{rate:.2}");
    while value.ends_with('0') {
        value.pop();
    }
    if value.ends_with('.') {
        value.push('0');
    }
    format!("{value} x")
}

#[cfg(test)]
mod tests {
    use super::{playback_rate_value, scale_scroll_target};

    #[test]
    fn playback_rate_value_keeps_a_clear_multiplier() {
        assert_eq!(playback_rate_value(0.5), "0.5 x");
        assert_eq!(playback_rate_value(1.0), "1.0 x");
        assert_eq!(playback_rate_value(1.25), "1.25 x");
        assert_eq!(playback_rate_value(2.0), "2.0 x");
    }

    #[test]
    fn forwarded_scale_scroll_stays_inside_the_parent_page_range() {
        assert_eq!(
            scale_scroll_target(890.0, 0.0, 1_000.0, 100.0, 10.0, true),
            900.0
        );
        assert_eq!(
            scale_scroll_target(10.0, 0.0, 1_000.0, 100.0, -10.0, true),
            0.0
        );
        let target = scale_scroll_target(200.0, 0.0, 1_000.0, 100.0, 1.0, false);
        assert!((target - 221.544_346_900_318_83).abs() < 1e-9);
    }
}
