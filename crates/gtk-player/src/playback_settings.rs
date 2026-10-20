use gtk_widgets::scale::{install_scale_scroll_forwarding, install_sliding_value_bubble};
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

use crate::PlayerUi;

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
    let local_output = shell.playback_owner.playback_output().is_local();

    let resource = crate::ui_resource::PLAYBACK_SETTINGS_POPOVER_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        popover: gtk::Popover,
        settings_group: adw::PreferencesGroup,
        queue: adw::SwitchRow,
        lyrics: adw::SwitchRow,
        visualizer: adw::SwitchRow,
        combine: adw::SwitchRow,
        audio_output: adw::ActionRow,
        waveform_row: adw::SwitchRow,
    });
    if !local_output {
        settings_group.set_description(Some(&tr("Audio processing is unavailable while casting")));
    }
    let device = crate::outputs::audio_output_dropdown(shell, 180);
    gtk_widgets::interactions::keep_parent_grab_for_dropdown(&device);
    audio_output.add_suffix(&device);
    audio_output.set_activatable_widget(Some(&device));
    audio_output.set_sensitive(local_output);
    let (transition_row, crossfade_options) =
        transition_rows(shell, &playback, PLAYBACK_SETTINGS_SCALE_WIDTH);
    transition_row.set_sensitive(local_output);
    crossfade_options.set_sensitive(local_output);
    settings_group.add(&transition_row);
    settings_group.add(&crossfade_options);
    let playback_rate =
        playback_rate_row(shell, playback.playback_rate, PLAYBACK_SETTINGS_SCALE_WIDTH);
    playback_rate.set_sensitive(local_output);
    settings_group.add(&playback_rate);
    let preserve_pitch = preserve_pitch_row(shell, playback.preserve_pitch);
    preserve_pitch.set_sensitive(local_output);
    settings_group.add(&preserve_pitch);

    waveform_row.set_active(
        shell
            .settings
            .current
            .borrow()
            .player_display
            .seekbar_waveform_enabled,
    );
    let waveform_shell = Rc::clone(shell);
    waveform_row.connect_active_notify(move |row| {
        if waveform_shell
            .settings
            .set_app_setting("seekbar waveform setting", row.is_active(), |settings| {
                &mut settings.player_display.seekbar_waveform_enabled
            })
            .is_some()
        {
            waveform_shell.update_bottom_player_transport();
        }
    });
    settings_group.add(&waveform_row);

    queue.set_active(settings.right_panel.queue_visible);
    let queue_shell = Rc::downgrade(shell);
    queue.connect_active_notify(move |row| {
        if let Some(shell) = queue_shell.upgrade() {
            shell.set_sidebar_queue_visible(row.is_active());
        }
    });
    settings_group.add(&queue);

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
    combine.set_active(settings.right_panel.combined);
    let weak = Rc::downgrade(shell);
    combine.connect_active_notify(move |row| {
        if let Some(shell) = weak.upgrade() {
            shell.set_right_panel_combined(row.is_active());
        }
    });
    settings_group.add(&combine);
    popover
}

pub fn transition_rows(
    shell: &Rc<PlayerUi>,
    settings: &playback::PlaybackSettings,
    scale_width: i32,
) -> (adw::ActionRow, adw::PreferencesRow) {
    let resource = crate::ui_resource::TRANSITION_SETTINGS_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        mode: adw::ActionRow,
        crossfade: gtk::ToggleButton,
        options: adw::PreferencesRow,
        revealer: gtk::Revealer,
        rows: gtk::ListBox,
        skip_same_album: adw::SwitchRow,
    });
    rows.prepend(&crossfade_duration_row(
        shell,
        settings.crossfade_seconds,
        scale_width,
    ));
    let active = settings.transition_mode == PlaybackTransitionMode::Crossfade;
    crossfade.set_active(active);
    options.set_visible(active);
    revealer.set_reveal_child(active);
    skip_same_album.set_active(settings.skip_same_album_crossfade);
    let weak = Rc::downgrade(shell);
    skip_same_album.connect_active_notify(move |row| {
        if let Some(shell) = weak.upgrade() {
            shell.update_playback_settings(|settings| {
                settings.skip_same_album_crossfade = row.is_active()
            });
        }
    });
    let row = options.downgrade();
    revealer.connect_child_revealed_notify(move |revealer| {
        if !revealer.reveals_child()
            && !revealer.is_child_revealed()
            && let Some(row) = row.upgrade()
        {
            row.set_visible(false);
        }
    });
    let row = options.downgrade();
    let revealer = revealer.downgrade();
    let weak = Rc::downgrade(shell);
    crossfade.connect_active_notify(move |button| {
        let active = button.is_active();
        if let (Some(row), Some(revealer)) = (row.upgrade(), revealer.upgrade()) {
            if active {
                row.set_visible(true);
            }
            revealer.set_reveal_child(active);
        }
        if let Some(shell) = weak.upgrade() {
            shell.update_playback_settings(|settings| {
                settings.transition_mode = if active {
                    PlaybackTransitionMode::Crossfade
                } else {
                    PlaybackTransitionMode::Gapless
                };
            });
        }
    });
    (mode, options)
}

fn crossfade_duration_row(
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
