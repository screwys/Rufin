pub const NORMALIZATION_SCOPE_TITLE: &str = localization::msgid("Normalization scope");
pub const NORMALIZATION_TITLE: &str = localization::msgid("Loudness normalization");
pub const VOLUME_SCALE_TITLE: &str = localization::msgid("Volume scale");
pub const SAME_ARTIST_TITLE: &str = localization::msgid("Same artist");
pub const STREAM_QUALITY_TITLE: &str = localization::msgid("Stream quality");
use crate::player::equalizer::EqualizerSurface;
use crate::player::outputs::audio_output_dropdown;
use crate::player::playback_settings::{
    playback_rate_row, preserve_pitch_row, transition_rows, volume_scale_from_index,
    volume_scale_index,
};
use crate::widgets::forms::{controlled_selection_row, selection_row};
use crate::widgets::scale::{install_scale_scroll_forwarding, install_sliding_value_bubble};
use std::{cell::Cell, rc::Rc};

use crate::shell::shell::Shell;
use adw::prelude::*;
use localization::tr;
use playback::{
    LoudnessNormalization, LoudnessNormalizationScope, MAX_AUTO_DJ_REFILL_COUNT,
    MAX_AUTO_DJ_REFILL_THRESHOLD, MAX_EBU_R128_TARGET_LUFS, MIN_AUTO_DJ_REFILL_COUNT,
    MIN_AUTO_DJ_REFILL_THRESHOLD, MIN_EBU_R128_TARGET_LUFS,
};
fn selected_source_is_local(shell: &Shell) -> bool {
    let configured = shell.source.configured.borrow();
    let Some(selected) = configured.selected_source_id.as_ref() else {
        return false;
    };
    configured
        .sources
        .iter()
        .find(|source| &source.id == selected)
        .is_some_and(|source| source.kind == sources::LOCAL_SOURCE_ID)
}

pub fn playback_page(shell: &Rc<Shell>) -> gtk::Box {
    let resource = "preferences/playback.ui";
    let builder = crate::ui_resource::builder(resource);
    let page: gtk::Box = crate::ui_resource::object(&builder, resource, "playback_page");

    let app_settings = shell.settings.current.borrow().clone();
    let settings = app_settings.playback.clone();
    crate::objects!(builder, resource, {
        transition_group: adw::PreferencesGroup,
        audio_fade_row: adw::SwitchRow,
        skip_unplayable_row: adw::SwitchRow,
        auto_dj_group: adw::PreferencesGroup,
        server_recommendations_row: adw::SwitchRow,
        similar_artists_row: adw::SwitchRow,
        refill_row: adw::ActionRow,
        refill: gtk::SpinButton,
        refill_count_row: adw::ActionRow,
        refill_count: gtk::SpinButton,
        skip_low_rated_row: adw::SwitchRow,
        clear_queue_row: adw::SwitchRow,
        audio_group: adw::PreferencesGroup,
        stream_quality_group: adw::PreferencesGroup,
        ebu_target_row: adw::ActionRow,
        ebu_target: gtk::Scale,
        write_ebu_tags_row: adw::SwitchRow,
        output_row: adw::ActionRow,
    });

    let (transition_row, crossfade_options) = transition_rows(&shell.player_ui, &settings, 220);
    transition_group.add(&transition_row);
    transition_group.add(&crossfade_options);

    let speed_row = playback_rate_row(&shell.player_ui, settings.playback_rate, 220);
    transition_group.add(&speed_row);

    let pitch_row = preserve_pitch_row(&shell.player_ui, settings.preserve_pitch);
    transition_group.add(&pitch_row);

    audio_fade_row.set_active(settings.audio_fade_on_status_change);
    let audio_fade_shell = Rc::clone(shell);
    audio_fade_row.connect_active_notify(move |row| {
        audio_fade_shell
            .player_ui
            .update_playback_settings(|settings| {
                settings.audio_fade_on_status_change = row.is_active();
            });
    });
    transition_group.add(&audio_fade_row);

    skip_unplayable_row.set_active(settings.skip_unplayable_tracks);
    let skip_unplayable_shell = Rc::clone(shell);
    skip_unplayable_row.connect_active_notify(move |row| {
        skip_unplayable_shell
            .player_ui
            .update_playback_settings(|settings| {
                settings.skip_unplayable_tracks = row.is_active();
            });
    });
    transition_group.add(&skip_unplayable_row);

    refill.set_range(
        f64::from(MIN_AUTO_DJ_REFILL_THRESHOLD),
        f64::from(MAX_AUTO_DJ_REFILL_THRESHOLD),
    );
    refill.set_increments(1.0, 10.0);
    refill.set_value(f64::from(app_settings.application.auto_dj_refill_threshold));
    let refill_shell = Rc::clone(shell);
    refill.connect_value_changed(move |spin| {
        let threshold = spin.value().round() as u8;
        refill_shell
            .settings
            .set_app_setting("Auto DJ setting", threshold, |settings| {
                &mut settings.application.auto_dj_refill_threshold
            });
    });
    refill_row.set_activatable_widget(Some(&refill));

    // The choice shows where a configured server recommends; unset reads as off.
    server_recommendations_row.set_active(app_settings.uses_server_recommendations());
    server_recommendations_row.set_visible(
        rufin_core::runtime::source::shows_server_recommendations(
            shell.source.configured.borrow().sources.clone(),
        ),
    );
    let server_shell = Rc::clone(shell);
    server_recommendations_row.connect_active_notify(move |row| {
        server_shell.settings.set_app_setting(
            "Auto DJ setting",
            Some(row.is_active()),
            |settings| &mut settings.application.server_recommendations,
        );
    });
    auto_dj_group.add(&server_recommendations_row);
    shell.settings.bind_switch(
        &similar_artists_row,
        "Auto DJ setting",
        |settings| &mut settings.application.auto_dj_similar_artists,
        |_| {},
    );
    auto_dj_group.add(&similar_artists_row);
    auto_dj_group.add(&refill_row);
    refill_count.set_range(
        f64::from(MIN_AUTO_DJ_REFILL_COUNT),
        f64::from(MAX_AUTO_DJ_REFILL_COUNT),
    );
    refill_count.set_increments(1.0, 5.0);
    refill_count.set_value(f64::from(app_settings.application.auto_dj_refill_count));
    let refill_count_shell = Rc::clone(shell);
    refill_count.connect_value_changed(move |spin| {
        let count = spin.value().round() as u8;
        refill_count_shell
            .settings
            .set_app_setting("Auto DJ setting", count, |settings| {
                &mut settings.application.auto_dj_refill_count
            });
    });
    refill_count_row.set_activatable_widget(Some(&refill_count));
    auto_dj_group.add(&refill_count_row);
    let same_artist_shell = Rc::clone(shell);
    let same_artist = [
        library::SameArtist::Few,
        library::SameArtist::Some,
        library::SameArtist::Many,
    ];
    auto_dj_group.add(&selection_row(
        &tr(SAME_ARTIST_TITLE),
        &[tr("Few"), tr("Some"), tr("Many")],
        same_artist
            .iter()
            .position(|share| *share == app_settings.application.auto_dj_same_artist)
            .unwrap_or(1) as u32,
        move |selected| {
            same_artist_shell.settings.set_app_setting(
                "Auto DJ setting",
                same_artist[selected as usize],
                |settings| &mut settings.application.auto_dj_same_artist,
            );
        },
    ));
    shell.settings.bind_switch(
        &skip_low_rated_row,
        "Auto DJ setting",
        |settings| &mut settings.application.auto_dj_skip_low_rated,
        |_| {},
    );
    auto_dj_group.add(&skip_low_rated_row);

    shell.settings.bind_switch(
        &clear_queue_row,
        "clear queue setting",
        |settings| &mut settings.player_display.clear_queue_includes_current,
        |_| {},
    );
    transition_group.add(&clear_queue_row);

    let loudness_scope_shell = Rc::clone(shell);
    let loudness_scope_row = selection_row(
        &tr(NORMALIZATION_SCOPE_TITLE),
        &[tr("Track"), tr("Album")],
        loudness_scope_index(settings.loudness_normalization_scope),
        move |selected| {
            loudness_scope_shell
                .player_ui
                .update_playback_settings(|settings| {
                    settings.loudness_normalization_scope = loudness_scope_from_index(selected);
                });
        },
    );
    loudness_scope_row.set_subtitle(&tr(
        "Track evens out every song, while Album preserves the intended differences within each album",
    ));

    ebu_target.set_range(MIN_EBU_R128_TARGET_LUFS, MAX_EBU_R128_TARGET_LUFS);
    ebu_target.set_increments(1.0, 10.0);
    install_scale_scroll_forwarding(&ebu_target);
    install_sliding_value_bubble(&ebu_target, |value| format!("{value:.0} LUFS"));
    for mark in [-48.0, -36.0, -30.0, -23.0, -18.0, -12.0, 0.0] {
        let label = format!("{mark:.0}");
        ebu_target.add_mark(mark, gtk::PositionType::Bottom, Some(&label));
    }
    ebu_target.set_value(settings.ebu_r128_target_lufs);
    let target_shell = Rc::clone(shell);
    ebu_target.connect_value_changed(move |scale| {
        target_shell.player_ui.update_playback_settings(|settings| {
            settings.ebu_r128_target_lufs = scale.value().round();
        });
    });
    ebu_target_row.set_activatable_widget(Some(&ebu_target));

    write_ebu_tags_row.set_active(settings.write_ebu_r128_tags);
    let write_tags_shell = Rc::clone(shell);
    write_ebu_tags_row.connect_active_notify(move |row| {
        write_tags_shell
            .player_ui
            .update_playback_settings(|settings| {
                settings.write_ebu_r128_tags = row.is_active();
            });
    });

    let mode_titles = [tr("Off"), tr("ReplayGain"), tr("EBU R128")];
    let (loudness_normalization_row, mode_buttons) = controlled_selection_row(
        &tr(NORMALIZATION_TITLE),
        &mode_titles,
        loudness_normalization_index(settings.loudness_normalization),
    );
    let weak_mode_buttons: Rc<[gtk::glib::WeakRef<gtk::ToggleButton>]> = mode_buttons
        .iter()
        .map(gtk::prelude::ObjectExt::downgrade)
        .collect();
    let mode_guard = Rc::new(Cell::new(false));
    for (index, button) in mode_buttons.iter().enumerate() {
        let shell = Rc::clone(shell);
        let buttons = Rc::clone(&weak_mode_buttons);
        let guard = Rc::clone(&mode_guard);
        let scope = loudness_scope_row.clone();
        let target = ebu_target_row.clone();
        let write = write_ebu_tags_row.clone();
        button.connect_toggled(move |button| {
            if !button.is_active() || guard.get() {
                return;
            }
            let next = loudness_normalization_from_index(index as u32);
            let previous = shell
                .settings
                .current
                .borrow()
                .playback
                .loudness_normalization;
            let apply = |mode| {
                shell.player_ui.update_playback_settings(|settings| {
                    settings.loudness_normalization = mode;
                });
                scope.set_visible(mode != LoudnessNormalization::Off);
                let ebu = mode == LoudnessNormalization::EbuR128;
                target.set_visible(ebu);
                write.set_visible(ebu && selected_source_is_local(&shell));
            };
            if next != LoudnessNormalization::EbuR128
                || previous == LoudnessNormalization::EbuR128
            {
                apply(next);
                return;
            }
            guard.set(true);
            if let Some(button) =
                buttons[loudness_normalization_index(previous) as usize].upgrade()
            {
                button.set_active(true);
            }
            guard.set(false);
            let confirm = adw::AlertDialog::builder()
                .heading(tr("Enable EBU R128 Analysis?"))
                .body(tr("Rufin will calculate the missing EBU R128 metadata for the whole library. This can use significant CPU, battery, and network bandwidth for a long time."))
                .build();
            confirm.add_response("cancel", &tr("Cancel"));
            confirm.add_response("enable", &tr("Enable"));
            confirm.set_default_response(Some("cancel"));
            confirm.set_close_response("cancel");
            let shell = Rc::clone(&shell);
            let buttons = Rc::clone(&buttons);
            let guard = Rc::clone(&guard);
            let scope = scope.clone();
            let target = target.clone();
            let write = write.clone();
            let window = shell.chrome.window.clone();
            confirm.choose(
                Some(&window),
                None::<&gtk::gio::Cancellable>,
                move |response| {
                    if response.as_str() != "enable" {
                        return;
                    }
                    shell.player_ui.update_playback_settings(|settings| {
                        settings.loudness_normalization = LoudnessNormalization::EbuR128;
                    });
                    scope.set_visible(true);
                    target.set_visible(true);
                    write.set_visible(selected_source_is_local(&shell));
                    guard.set(true);
                    if let Some(button) = buttons
                        [loudness_normalization_index(LoudnessNormalization::EbuR128) as usize]
                        .upgrade()
                    {
                        button.set_active(true);
                    }
                    guard.set(false);
                },
            );
        });
    }
    let mode_enabled = settings.loudness_normalization != LoudnessNormalization::Off;
    let ebu_enabled = settings.loudness_normalization == LoudnessNormalization::EbuR128;
    loudness_scope_row.set_visible(mode_enabled);
    ebu_target_row.set_visible(ebu_enabled);
    write_ebu_tags_row.set_visible(ebu_enabled && selected_source_is_local(shell));
    audio_group.add(&loudness_normalization_row);
    audio_group.add(&loudness_scope_row);
    audio_group.add(&ebu_target_row);
    audio_group.add(&write_ebu_tags_row);

    let volume_scale_shell = Rc::clone(shell);
    let volume_scale_row = selection_row(
        &tr(VOLUME_SCALE_TITLE),
        &[tr("Perceptual"), tr("Linear")],
        volume_scale_index(settings.volume_scale),
        move |selected| {
            volume_scale_shell
                .player_ui
                .update_playback_settings(|settings| {
                    settings.set_volume_scale_preserving_gain(volume_scale_from_index(selected));
                });
        },
    );
    volume_scale_row.set_subtitle(&tr(
        "Perceptual for a finer control at low volume; Linear for a direct scale",
    ));
    audio_group.add(&volume_scale_row);

    let quality_shell = Rc::clone(shell);
    let quality_rows =
        crate::preferences::audio_quality::rows(settings.stream_quality, move |quality| {
            quality_shell
                .player_ui
                .update_playback_settings(|settings| settings.stream_quality = quality);
        });
    for row in &quality_rows {
        stream_quality_group.add(row);
    }

    let output_dropdown = audio_output_dropdown(&shell, 220);
    output_row.add_suffix(&output_dropdown);
    output_row.set_activatable_widget(Some(&output_dropdown));
    audio_group.add(&output_row);

    page
}

pub fn equalizer_page(shell: &Rc<Shell>) -> adw::PreferencesPage {
    let resource = "preferences/playback.ui";
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, {
        equalizer_page: adw::PreferencesPage,
        equalizer_group: adw::PreferencesGroup,
    });
    let equalizer = EqualizerSurface::new(&shell);
    equalizer_group.add(&equalizer.root);

    equalizer_page
}
pub fn loudness_normalization_index(mode: LoudnessNormalization) -> u32 {
    match mode {
        LoudnessNormalization::Off => 0,
        LoudnessNormalization::ReplayGain => 1,
        LoudnessNormalization::EbuR128 => 2,
    }
}
pub fn loudness_normalization_from_index(index: u32) -> LoudnessNormalization {
    match index {
        1 => LoudnessNormalization::ReplayGain,
        2 => LoudnessNormalization::EbuR128,
        _ => LoudnessNormalization::Off,
    }
}
pub fn loudness_scope_index(scope: LoudnessNormalizationScope) -> u32 {
    match scope {
        LoudnessNormalizationScope::Track => 0,
        LoudnessNormalizationScope::Album => 1,
    }
}
pub fn loudness_scope_from_index(index: u32) -> LoudnessNormalizationScope {
    match index {
        0 => LoudnessNormalizationScope::Track,
        _ => LoudnessNormalizationScope::Album,
    }
}
