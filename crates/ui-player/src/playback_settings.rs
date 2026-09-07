use crate::outputs::audio_output_dropdown;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use ui_shared::scale::{install_scale_scroll_forwarding, install_sliding_value_bubble};

use adw::prelude::*;
use gtk::glib;
use localization::tr;
use playback::{
    MAX_CROSSFADE_SECONDS, MAX_PLAYBACK_RATE, MIN_CROSSFADE_SECONDS, MIN_PLAYBACK_RATE,
    PlaybackTransitionMode,
};

use crate::PlayerUi;
use ui_shared::forms::selection_row;

const PLAYBACK_RATE_COMMIT_DELAY: Duration = Duration::from_millis(200);
const PLAYBACK_SETTINGS_SCALE_WIDTH: i32 = 180;

pub fn configure_playback_settings_popover(
    button: &gtk::MenuButton,
    shell: &Rc<PlayerUi>,
    set_media_visibility: Rc<dyn Fn(bool, bool)>,
) {
    let shell = Rc::clone(shell);
    button.set_create_popup_func(move |button| {
        button.set_popover(Some(&playback_settings_popover(
            &shell,
            Rc::clone(&set_media_visibility),
        )));
    });
}

fn playback_settings_popover(
    shell: &Rc<PlayerUi>,
    set_media_visibility: Rc<dyn Fn(bool, bool)>,
) -> gtk::Popover {
    let settings = shell.settings.current.borrow().clone();
    let playback = settings.playback;
    let local_output = shell
        .playback_handles
        .transport
        .playback_output()
        .is_local();

    let resource = crate::ui_resource::PLAYBACK_SETTINGS_POPOVER_RESOURCE;
    let builder = ui_shared::ui_resource::builder(resource);
    ui_shared::objects!(builder, resource, {
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

    let output_dropdown = audio_output_dropdown(&shell, PLAYBACK_SETTINGS_SCALE_WIDTH);
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
    let lyrics_visibility = Rc::clone(&set_media_visibility);
    lyrics.connect_active_notify(move |row| {
        lyrics_visibility(
            row.is_active(),
            lyrics_shell.right_panel.visualizer_visible.get(),
        );
    });
    settings_group.add(&lyrics);
    visualizer.set_active(settings.visualizer_panel_visible);
    visualizer.set_sensitive(local_output);
    let visualizer_shell = Rc::clone(shell);
    visualizer.connect_active_notify(move |row| {
        set_media_visibility(visualizer_shell.lyrics.panel_visible.get(), row.is_active());
    });
    settings_group.add(&visualizer);
    popover
}

pub fn crossfade_duration_row(
    shell: &Rc<PlayerUi>,
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

pub fn playback_rate_row(
    shell: &Rc<PlayerUi>,
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

pub fn preserve_pitch_row(shell: &Rc<PlayerUi>, active: bool) -> adw::SwitchRow {
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
    use super::playback_rate_value;

    #[test]
    fn playback_rate_value_keeps_a_clear_multiplier() {
        assert_eq!(playback_rate_value(0.5), "0.5 x");
        assert_eq!(playback_rate_value(1.0), "1.0 x");
        assert_eq!(playback_rate_value(1.25), "1.25 x");
        assert_eq!(playback_rate_value(2.0), "2.0 x");
    }
}

use playback::VolumeScale;
pub fn transition_index(mode: PlaybackTransitionMode) -> u32 {
    match mode {
        PlaybackTransitionMode::Gapless => 0,
        PlaybackTransitionMode::Crossfade => 1,
    }
}
pub fn transition_from_index(index: u32) -> PlaybackTransitionMode {
    match index {
        1 => PlaybackTransitionMode::Crossfade,
        _ => PlaybackTransitionMode::Gapless,
    }
}
pub fn volume_scale_index(scale: VolumeScale) -> u32 {
    match scale {
        VolumeScale::Perceptual => 0,
        VolumeScale::Linear => 1,
    }
}
pub fn volume_scale_from_index(index: u32) -> VolumeScale {
    match index {
        1 => VolumeScale::Linear,
        _ => VolumeScale::Perceptual,
    }
}
