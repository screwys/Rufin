use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use super::{
    LASTFM_API_CREATE_URL, LISTENBRAINZ_TOKEN_URL, configure_sidebar_items_expander,
    context_menu::configure_context_menus_expander, controlled_selection_row,
    layout::populate_home_block_rows, populate_layout_group, quality_selection_row, selection_row,
};
use crate::player::{
    EqualizerSurface, audio_output_dropdown, crossfade_duration_row,
    install_scale_scroll_forwarding, install_sliding_value_bubble, playback_rate_row,
    preserve_pitch_row,
};
use crate::runtime::{ScrobblingConnection, ScrobblingConnectionEvent, ScrobblingPreferences};
use crate::shell::Shell;
use crate::{AccentPreference, ThemePreference};
use adw::prelude::*;
use localization::{tr, tr_with};
use playback::StreamQuality;
use playback::{
    LoudnessNormalization, LoudnessNormalizationScope, MAX_AUTO_DJ_REFILL_THRESHOLD,
    MAX_EBU_R128_TARGET_LUFS, MIN_AUTO_DJ_REFILL_THRESHOLD, MIN_EBU_R128_TARGET_LUFS,
    PlaybackTransitionMode, VolumeScale,
};

const PREFERENCES_EQUALIZER_BAND_HEIGHT: i32 = 280;

pub(super) type ScrobblingCredentialDrafts = Rc<RefCell<ScrobblingPreferences>>;

pub(crate) fn scrobbling_page(
    shell: &Rc<Shell>,
    drafts: &ScrobblingCredentialDrafts,
) -> adw::PreferencesPage {
    let resource = crate::ui_resource::INTEGRATIONS_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    let page: adw::PreferencesPage =
        crate::ui_resource::object(&builder, resource, "integrations_page");
    let settings = shell.products.scrobbling.preferences();
    crate::ui_resource::objects!(builder, resource, {
        lastfm_enabled: adw::SwitchRow,
        lastfm_api_help: adw::ActionRow,
        lastfm_api_key: adw::PasswordEntryRow,
        lastfm_api_secret: adw::PasswordEntryRow,
        lastfm_connection: adw::ActionRow,
        lastfm_connect: gtk::Button,
        lastfm_now_playing: adw::SwitchRow,
        librefm_enabled: adw::SwitchRow,
        librefm_connection: adw::ActionRow,
        librefm_connect: gtk::Button,
        librefm_now_playing: adw::SwitchRow,
        listenbrainz_enabled: adw::SwitchRow,
        listenbrainz_token_help: adw::ActionRow,
        listenbrainz_token: adw::PasswordEntryRow,
        listenbrainz_now_playing: adw::SwitchRow,
    });

    lastfm_enabled.set_active(settings.lastfm.enabled);
    bind_scrobbling_switch(
        &lastfm_enabled,
        shell,
        "Last.fm scrobbling setting",
        |settings, active| replace_if_changed(&mut settings.lastfm.enabled, active),
    );
    lastfm_api_help.set_subtitle(&inline_link_markup(
        &tr("If you do not have API keys, create them"),
        LASTFM_API_CREATE_URL,
        &tr("here"),
        &tr(". You only need to fill email and an application name parts"),
    ));

    lastfm_api_key.set_text(&drafts.borrow().lastfm.api_key);
    let key_drafts = drafts.clone();
    lastfm_api_key.connect_text_notify(move |row| {
        key_drafts.borrow_mut().lastfm.api_key = row.text().to_string();
    });
    let lastfm_api_shell = Rc::clone(shell);
    lastfm_api_key.connect_apply(move |row| {
        let api_key = row.text().trim().to_string();
        if lastfm_api_shell
            .update_scrobbling_settings("Last.fm API key setting", |settings| {
                if settings.lastfm.api_key == api_key {
                    return false;
                }
                settings.lastfm.api_key = api_key.clone();
                true
            })
            .is_some()
        {
            lastfm_api_shell.retry_external_artwork("Last.fm API key setting");
        }
    });
    lastfm_api_secret.set_text(&drafts.borrow().lastfm.api_secret);
    let secret_drafts = drafts.clone();
    lastfm_api_secret.connect_text_notify(move |row| {
        secret_drafts.borrow_mut().lastfm.api_secret = row.text().to_string();
    });
    let lastfm_secret_shell = Rc::clone(shell);
    lastfm_api_secret.connect_apply(move |row| {
        let api_secret = row.text().trim().to_string();
        lastfm_secret_shell.update_scrobbling_settings(
            "Last.fm shared secret setting",
            |settings| {
                if settings.lastfm.api_secret == api_secret {
                    return false;
                }
                settings.lastfm.api_secret = api_secret;
                true
            },
        );
    });
    lastfm_connection.set_subtitle(&audioscrobbler_connection_subtitle(
        settings.lastfm.connected,
        &settings.lastfm.username,
    ));
    let lastfm_connect_label = if settings.lastfm.connected {
        tr("Reconnect")
    } else {
        tr("Connect")
    };
    lastfm_connect.set_label(&lastfm_connect_label);
    lastfm_connection.set_activatable_widget(Some(&lastfm_connect));
    let lastfm_connect_shell = Rc::clone(shell);
    let lastfm_api_key_row = lastfm_api_key.downgrade();
    let lastfm_secret_row = lastfm_api_secret.downgrade();
    let lastfm_connection_row = lastfm_connection.downgrade();
    lastfm_connect.connect_clicked(move |button| {
        let Some(lastfm_api_key_row) = lastfm_api_key_row.upgrade() else {
            return;
        };
        let Some(lastfm_secret_row) = lastfm_secret_row.upgrade() else {
            return;
        };
        let Some(lastfm_connection_row) = lastfm_connection_row.upgrade() else {
            return;
        };
        let api_key = lastfm_api_key_row.text().trim().to_string();
        let api_secret = lastfm_secret_row.text().trim().to_string();
        if api_key.is_empty() || api_secret.is_empty() {
            lastfm_connection_row.set_subtitle(&tr("Enter API credentials first"));
            return;
        }
        button.set_sensitive(false);
        lastfm_connection_row.set_subtitle(&tr("Opening Last.fm authorization..."));
        let shell = Rc::clone(&lastfm_connect_shell);
        let row = lastfm_connection_row.clone();
        let button = button.clone();
        gtk::glib::spawn_future_local(async move {
            let request = ScrobblingConnection::LastFm {
                api_key,
                api_secret,
            };
            match connect_scrobbling(
                &shell,
                request,
                "Failed to open Last.fm authorization: ",
                tr_with("Couldn't connect to {service}", &[("service", "Last.fm")]),
                true,
            )
            .await
            {
                Ok(username) => {
                    row.set_subtitle(&audioscrobbler_connected_subtitle(&username));
                    button.set_label(&tr("Reconnect"));
                }
                Err(error) => {
                    row.set_subtitle(&error);
                }
            }
            button.set_sensitive(true);
        });
    });
    lastfm_now_playing.set_active(settings.lastfm.now_playing_enabled);
    bind_scrobbling_switch(
        &lastfm_now_playing,
        shell,
        "Last.fm now playing setting",
        |settings, active| replace_if_changed(&mut settings.lastfm.now_playing_enabled, active),
    );
    librefm_enabled.set_active(settings.librefm.enabled);
    bind_scrobbling_switch(
        &librefm_enabled,
        shell,
        "Libre.fm scrobbling setting",
        |settings, active| replace_if_changed(&mut settings.librefm.enabled, active),
    );
    librefm_connection.set_subtitle(&audioscrobbler_connection_subtitle(
        settings.librefm.connected,
        &settings.librefm.username,
    ));
    let librefm_connect_label = if settings.librefm.connected {
        tr("Reconnect")
    } else {
        tr("Connect")
    };
    librefm_connect.set_label(&librefm_connect_label);
    librefm_connection.set_activatable_widget(Some(&librefm_connect));
    let librefm_connect_shell = Rc::clone(shell);
    let librefm_connection_row = librefm_connection.downgrade();
    librefm_connect.connect_clicked(move |button| {
        let Some(librefm_connection_row) = librefm_connection_row.upgrade() else {
            return;
        };
        button.set_sensitive(false);
        librefm_connection_row.set_subtitle(&tr("Opening Libre.fm authorization..."));
        let shell = Rc::clone(&librefm_connect_shell);
        let row = librefm_connection_row.clone();
        let button = button.clone();
        gtk::glib::spawn_future_local(async move {
            match connect_scrobbling(
                &shell,
                ScrobblingConnection::LibreFm,
                "Failed to open Libre.fm authorization: ",
                tr_with("Couldn't connect to {service}", &[("service", "Libre.fm")]),
                false,
            )
            .await
            {
                Ok(username) => {
                    row.set_subtitle(&audioscrobbler_connected_subtitle(&username));
                    button.set_label(&tr("Reconnect"));
                }
                Err(error) => {
                    row.set_subtitle(&error);
                }
            }
            button.set_sensitive(true);
        });
    });
    librefm_now_playing.set_active(settings.librefm.now_playing_enabled);
    bind_scrobbling_switch(
        &librefm_now_playing,
        shell,
        "Libre.fm now playing setting",
        |settings, active| replace_if_changed(&mut settings.librefm.now_playing_enabled, active),
    );
    listenbrainz_enabled.set_active(settings.listenbrainz.enabled);
    bind_scrobbling_switch(
        &listenbrainz_enabled,
        shell,
        "ListenBrainz scrobbling setting",
        |settings, active| replace_if_changed(&mut settings.listenbrainz.enabled, active),
    );
    listenbrainz_token_help.set_subtitle(&inline_link_markup(
        &tr("Find your ListenBrainz user token"),
        LISTENBRAINZ_TOKEN_URL,
        &tr("here"),
        ".",
    ));

    listenbrainz_token.set_text(&drafts.borrow().listenbrainz.user_token);
    let token_drafts = drafts.clone();
    listenbrainz_token.connect_text_notify(move |row| {
        token_drafts.borrow_mut().listenbrainz.user_token = row.text().to_string();
    });
    let listenbrainz_token_shell = Rc::clone(shell);
    listenbrainz_token.connect_apply(move |row| {
        let token = row.text().trim().to_string();
        listenbrainz_token_shell.update_scrobbling_settings(
            "ListenBrainz token setting",
            |settings| {
                if settings.listenbrainz.user_token == token {
                    return false;
                }
                settings.listenbrainz.user_token = token;
                true
            },
        );
    });
    listenbrainz_now_playing.set_active(settings.listenbrainz.now_playing_enabled);
    bind_scrobbling_switch(
        &listenbrainz_now_playing,
        shell,
        "ListenBrainz now playing setting",
        |settings, active| {
            replace_if_changed(&mut settings.listenbrainz.now_playing_enabled, active)
        },
    );
    page
}

fn bind_scrobbling_switch(
    row: &adw::SwitchRow,
    shell: &Rc<Shell>,
    warning_action: &'static str,
    update: impl Fn(&mut ScrobblingPreferences, bool) -> bool + 'static,
) {
    let shell = Rc::clone(shell);
    row.connect_active_notify(move |row| {
        shell.update_scrobbling_settings(warning_action, |settings| {
            update(settings, row.is_active())
        });
    });
}

fn replace_if_changed(current: &mut bool, next: bool) -> bool {
    if *current == next {
        return false;
    }
    *current = next;
    true
}

pub(crate) fn audioscrobbler_connection_subtitle(connected: bool, username: &str) -> String {
    if !connected {
        tr("Not connected")
    } else {
        audioscrobbler_connected_subtitle(username)
    }
}
pub(crate) fn audioscrobbler_connected_subtitle(username: &str) -> String {
    let username = username.trim();
    if username.is_empty() {
        tr("Connected")
    } else {
        tr_with("Connected as {username}", &[("username", username)])
    }
}
pub(crate) fn inline_link_markup(before: &str, url: &str, label: &str, after: &str) -> String {
    let before = gtk::glib::markup_escape_text(before);
    let url = gtk::glib::markup_escape_text(url);
    let label = gtk::glib::markup_escape_text(label);
    let after = gtk::glib::markup_escape_text(after);
    format!("{before} <a href=\"{url}\">{label}</a>{after}")
}

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
async fn connect_scrobbling(
    shell: &Rc<Shell>,
    request: ScrobblingConnection,
    open_error: &'static str,
    connection_error: String,
    refresh_external_artwork: bool,
) -> Result<String, String> {
    let events = shell.products.scrobbling.connect(request);
    while let Ok(event) = events.recv().await {
        match event {
            ScrobblingConnectionEvent::OpenUrl { url, opened } => {
                let launcher = gtk::UriLauncher::new(&url);
                match launcher.launch_future(Some(&shell.chrome.window)).await {
                    Ok(()) => {
                        let _ = opened.send(Ok(())).await;
                    }
                    Err(error) => {
                        let error = format!("{open_error}{error}");
                        let _ = opened.send(Err(error.clone())).await;
                        return Err(error);
                    }
                }
            }
            ScrobblingConnectionEvent::Connected { username } => {
                let preferences = shell.products.scrobbling.preferences();
                shell.settings.current.borrow_mut().lastfm_api_key = preferences.lastfm.api_key;
                if refresh_external_artwork {
                    shell.retry_external_artwork("Last.fm connection setting");
                }
                return Ok(username);
            }
            ScrobblingConnectionEvent::TimedOut => return Err(connection_error.clone()),
            ScrobblingConnectionEvent::Failed(error) => return Err(error),
        }
    }
    Err(connection_error)
}
pub(crate) fn playback_page(shell: &Rc<Shell>) -> adw::PreferencesPage {
    let resource = crate::ui_resource::PLAYBACK_PREFERENCES_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    let page: adw::PreferencesPage =
        crate::ui_resource::object(&builder, resource, "playback_page");

    let app_settings = shell.settings.current.borrow().clone();
    let settings = app_settings.playback.clone();
    crate::ui_resource::objects!(builder, resource, {
        transition_group: adw::PreferencesGroup,
        skip_same_album_crossfade_row: adw::SwitchRow,
        audio_fade_row: adw::SwitchRow,
        refill_row: adw::ActionRow,
        refill: gtk::SpinButton,
        clear_queue_row: adw::SwitchRow,
        audio_group: adw::PreferencesGroup,
        ebu_target_row: adw::ActionRow,
        ebu_target: gtk::Scale,
        write_ebu_tags_row: adw::SwitchRow,
        output_row: adw::ActionRow,
        equalizer_group: adw::PreferencesGroup,
    });

    let transition_shell = Rc::clone(shell);
    let transition_row = selection_row(
        &tr("Transition mode"),
        &[tr("Gapless"), tr("Crossfade")],
        transition_index(settings.transition_mode),
        move |selected| {
            transition_shell.update_playback_settings(|settings| {
                settings.transition_mode = transition_from_index(selected);
            });
        },
    );
    transition_group.add(&transition_row);

    let crossfade_row = crossfade_duration_row(shell, settings.crossfade_seconds, 220);
    transition_group.add(&crossfade_row);

    let speed_row = playback_rate_row(shell, settings.playback_rate, 220);
    transition_group.add(&speed_row);

    let pitch_row = preserve_pitch_row(shell, settings.preserve_pitch);
    transition_group.add(&pitch_row);

    skip_same_album_crossfade_row.set_active(settings.skip_same_album_crossfade);
    let skip_same_album_crossfade_shell = Rc::clone(shell);
    skip_same_album_crossfade_row.connect_active_notify(move |row| {
        skip_same_album_crossfade_shell.update_playback_settings(|settings| {
            settings.skip_same_album_crossfade = row.is_active();
        });
    });
    transition_group.add(&skip_same_album_crossfade_row);

    audio_fade_row.set_active(settings.audio_fade_on_status_change);
    let audio_fade_shell = Rc::clone(shell);
    audio_fade_row.connect_active_notify(move |row| {
        audio_fade_shell.update_playback_settings(|settings| {
            settings.audio_fade_on_status_change = row.is_active();
        });
    });
    transition_group.add(&audio_fade_row);

    refill.set_range(
        f64::from(MIN_AUTO_DJ_REFILL_THRESHOLD),
        f64::from(MAX_AUTO_DJ_REFILL_THRESHOLD),
    );
    refill.set_increments(1.0, 10.0);
    refill.set_value(f64::from(app_settings.auto_dj_refill_threshold));
    let refill_shell = Rc::clone(shell);
    refill.connect_value_changed(move |spin| {
        let threshold = spin.value().round() as u8;
        refill_shell.set_app_setting("Auto DJ setting", threshold, |settings| {
            &mut settings.auto_dj_refill_threshold
        });
    });
    refill_row.set_activatable_widget(Some(&refill));
    transition_group.add(&refill_row);

    clear_queue_row.set_active(app_settings.clear_queue_includes_current);
    let clear_queue_shell = Rc::clone(shell);
    clear_queue_row.connect_active_notify(move |row| {
        clear_queue_shell.set_app_setting("clear queue setting", row.is_active(), |settings| {
            &mut settings.clear_queue_includes_current
        });
    });
    transition_group.add(&clear_queue_row);

    let loudness_scope_shell = Rc::clone(shell);
    let loudness_scope_row = selection_row(
        &tr("Normalization scope"),
        &[tr("Track"), tr("Album")],
        loudness_scope_index(settings.loudness_normalization_scope),
        move |selected| {
            loudness_scope_shell.update_playback_settings(|settings| {
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
        target_shell.update_playback_settings(|settings| {
            settings.ebu_r128_target_lufs = scale.value().round();
        });
    });
    ebu_target_row.set_activatable_widget(Some(&ebu_target));

    write_ebu_tags_row.set_active(settings.write_ebu_r128_tags);
    let write_tags_shell = Rc::clone(shell);
    write_ebu_tags_row.connect_active_notify(move |row| {
        write_tags_shell.update_playback_settings(|settings| {
            settings.write_ebu_r128_tags = row.is_active();
        });
    });

    let mode_titles = [tr("Off"), tr("ReplayGain"), tr("EBU R128")];
    let (loudness_normalization_row, mode_buttons) = controlled_selection_row(
        &tr("Loudness normalization"),
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
                shell.update_playback_settings(|settings| {
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
                    shell.update_playback_settings(|settings| {
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
        &tr("Volume scale"),
        &[tr("Perceptual"), tr("Linear")],
        volume_scale_index(settings.volume_scale),
        move |selected| {
            volume_scale_shell.update_playback_settings(|settings| {
                settings.set_volume_scale_preserving_gain(volume_scale_from_index(selected));
            });
        },
    );
    volume_scale_row.set_subtitle(&tr(
        "Perceptual for a finer control at low volume; Linear for a direct scale",
    ));
    audio_group.add(&volume_scale_row);

    let quality_shell = Rc::clone(shell);
    let quality_row = quality_selection_row(
        &tr("Stream quality"),
        &[
            StreamQuality::Original,
            StreamQuality::MaxBitrateKbps(320),
            StreamQuality::MaxBitrateKbps(256),
            StreamQuality::MaxBitrateKbps(192),
            StreamQuality::MaxBitrateKbps(128),
        ],
        stream_quality_index(settings.stream_quality),
        move |selected| {
            quality_shell.update_playback_settings(|settings| {
                settings.stream_quality = stream_quality_from_index(selected);
            });
        },
    );
    audio_group.add(&quality_row);

    let output_dropdown = audio_output_dropdown(shell, 220);
    output_row.add_suffix(&output_dropdown);
    output_row.set_activatable_widget(Some(&output_dropdown));
    audio_group.add(&output_row);

    let equalizer = EqualizerSurface::new(&settings.equalizer);
    equalizer.set_band_height_request(PREFERENCES_EQUALIZER_BAND_HEIGHT);
    equalizer
        .root
        .add_css_class("preferences-equalizer-surface");
    let equalizer_shell = Rc::clone(shell);
    equalizer.connect_changed(move |equalizer| {
        equalizer_shell.update_playback_settings(|settings| {
            settings.equalizer = equalizer.clone();
        });
    });
    equalizer_group.add(&equalizer.root);

    page
}
pub(crate) fn appearance_page(shell: &Rc<Shell>) -> adw::PreferencesPage {
    let resource = crate::ui_resource::APPEARANCE_PREFERENCES_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    let page: adw::PreferencesPage =
        crate::ui_resource::object(&builder, resource, "appearance_page");
    crate::ui_resource::objects!(builder, resource, {
        theme_group: adw::PreferencesGroup,
        waveform_row: adw::SwitchRow,
        layout_group: adw::PreferencesGroup,
        lyrics_panel_row: adw::SwitchRow,
        visualizer_panel_row: adw::SwitchRow,
        bottom_bar_rating_row: adw::SwitchRow,
        narrow_row: adw::SwitchRow,
        threshold_row: adw::SpinRow,
        sidebar_items: adw::ExpanderRow,
        sidebar_pins_row: adw::SwitchRow,
        home_blocks: adw::ExpanderRow,
        context_menus: adw::ExpanderRow,
        context_menu_rating_row: adw::ActionRow,
        context_menu_rating_visible: gtk::Switch,
    });

    populate_theme_group(shell, &theme_group);
    waveform_row.set_active(shell.settings.current.borrow().seekbar_waveform_enabled);
    let waveform_shell = Rc::clone(shell);
    waveform_row.connect_active_notify(move |row| {
        if waveform_shell
            .set_app_setting("seekbar waveform setting", row.is_active(), |settings| {
                &mut settings.seekbar_waveform_enabled
            })
            .is_some()
        {
            waveform_shell.update_bottom_player();
        }
    });
    populate_layout_group(
        shell,
        &layout_group,
        lyrics_panel_row,
        visualizer_panel_row,
        bottom_bar_rating_row,
        narrow_row,
        threshold_row,
    );

    configure_sidebar_items_expander(shell, &sidebar_items, &sidebar_pins_row);

    let rows = Rc::new(std::cell::RefCell::new(Vec::new()));
    let home_shell = Rc::clone(shell);
    super::populate_expander_once(&home_blocks, move |home_blocks| {
        populate_home_block_rows(&home_shell, home_blocks, &rows);
    });

    configure_context_menus_expander(
        shell,
        &context_menus,
        &context_menu_rating_row,
        &context_menu_rating_visible,
    );

    page
}

fn populate_theme_group(shell: &Rc<Shell>, group: &adw::PreferencesGroup) {
    let options = [tr("System"), tr("Light"), tr("Dark")];
    let selected = theme_preference_index(shell.settings.current.borrow().theme_preference);
    let theme_shell = Rc::clone(shell);
    let row = selection_row(&tr("Color scheme"), &options, selected, move |selected| {
        let preference = theme_preference_from_index(selected);
        if let Some(settings) =
            theme_shell.set_app_setting("theme setting", preference, |settings| {
                &mut settings.theme_preference
            })
        {
            theme_shell.appearance.apply(&settings);
        }
    });
    group.add(&row);

    let accent_titles = AccentPreference::ALL.map(accent_preference_title);
    let accent_title_refs = accent_titles.each_ref().map(String::as_str);
    let accent_row = adw::ComboRow::builder()
        .title(tr("Accent color"))
        .model(&gtk::StringList::new(&accent_title_refs))
        .selected(accent_preference_index(
            shell.settings.current.borrow().accent_preference,
        ))
        .build();
    let accent_shell = Rc::clone(shell);
    accent_row.connect_selected_notify(move |row| {
        let preference = accent_preference_from_index(row.selected());
        if let Some(settings) =
            accent_shell.set_app_setting("accent setting", preference, |settings| {
                &mut settings.accent_preference
            })
        {
            accent_shell.appearance.apply(&settings);
        }
    });
    group.add(&accent_row);
}

pub(super) fn theme_preference_index(preference: ThemePreference) -> u32 {
    match preference {
        ThemePreference::System => 0,
        ThemePreference::Light => 1,
        ThemePreference::Dark => 2,
    }
}

pub(super) fn theme_preference_from_index(index: u32) -> ThemePreference {
    match index {
        1 => ThemePreference::Light,
        2 => ThemePreference::Dark,
        _ => ThemePreference::System,
    }
}

fn accent_preference_title(preference: AccentPreference) -> String {
    match preference {
        AccentPreference::System => tr("System"),
        AccentPreference::Blue => tr("Blue"),
        AccentPreference::Teal => tr("Teal"),
        AccentPreference::Green => tr("Green"),
        AccentPreference::Yellow => tr("Yellow"),
        AccentPreference::Orange => tr("Orange"),
        AccentPreference::Red => tr("Red"),
        AccentPreference::Pink => tr("Pink"),
        AccentPreference::Purple => tr("Purple"),
        AccentPreference::Slate => tr("Slate"),
    }
}

pub(super) fn accent_preference_index(preference: AccentPreference) -> u32 {
    AccentPreference::ALL
        .iter()
        .position(|candidate| *candidate == preference)
        .unwrap_or_default() as u32
}

pub(super) fn accent_preference_from_index(index: u32) -> AccentPreference {
    AccentPreference::ALL
        .get(index as usize)
        .copied()
        .unwrap_or_default()
}
pub(crate) fn transition_index(mode: PlaybackTransitionMode) -> u32 {
    match mode {
        PlaybackTransitionMode::Gapless => 0,
        PlaybackTransitionMode::Crossfade => 1,
    }
}
pub(crate) fn transition_from_index(index: u32) -> PlaybackTransitionMode {
    match index {
        1 => PlaybackTransitionMode::Crossfade,
        _ => PlaybackTransitionMode::Gapless,
    }
}
pub(crate) fn loudness_normalization_index(mode: LoudnessNormalization) -> u32 {
    match mode {
        LoudnessNormalization::Off => 0,
        LoudnessNormalization::ReplayGain => 1,
        LoudnessNormalization::EbuR128 => 2,
    }
}
pub(crate) fn loudness_normalization_from_index(index: u32) -> LoudnessNormalization {
    match index {
        1 => LoudnessNormalization::ReplayGain,
        2 => LoudnessNormalization::EbuR128,
        _ => LoudnessNormalization::Off,
    }
}
pub(crate) fn loudness_scope_index(scope: LoudnessNormalizationScope) -> u32 {
    match scope {
        LoudnessNormalizationScope::Track => 0,
        LoudnessNormalizationScope::Album => 1,
    }
}
pub(crate) fn loudness_scope_from_index(index: u32) -> LoudnessNormalizationScope {
    match index {
        0 => LoudnessNormalizationScope::Track,
        _ => LoudnessNormalizationScope::Album,
    }
}
pub(crate) fn volume_scale_index(scale: VolumeScale) -> u32 {
    match scale {
        VolumeScale::Perceptual => 0,
        VolumeScale::Linear => 1,
    }
}
pub(crate) fn volume_scale_from_index(index: u32) -> VolumeScale {
    match index {
        1 => VolumeScale::Linear,
        _ => VolumeScale::Perceptual,
    }
}
pub(crate) fn stream_quality_index(quality: StreamQuality) -> u32 {
    match quality {
        StreamQuality::Original => 0,
        StreamQuality::MaxBitrateKbps(320) => 1,
        StreamQuality::MaxBitrateKbps(256) => 2,
        StreamQuality::MaxBitrateKbps(192) => 3,
        StreamQuality::MaxBitrateKbps(128) => 4,
        StreamQuality::MaxBitrateKbps(_) => 0,
    }
}
pub(crate) fn stream_quality_from_index(index: u32) -> StreamQuality {
    match index {
        1 => StreamQuality::MaxBitrateKbps(320),
        2 => StreamQuality::MaxBitrateKbps(256),
        3 => StreamQuality::MaxBitrateKbps(192),
        4 => StreamQuality::MaxBitrateKbps(128),
        _ => StreamQuality::Original,
    }
}
