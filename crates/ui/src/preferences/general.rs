use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use super::{
    INTEGRATIONS_ICON_NAME, LASTFM_API_CREATE_URL, LISTENBRAINZ_TOKEN_URL,
    context_menu::context_menus_expander, controlled_selection_row,
    layout::populate_home_block_rows, layout_group, quality_selection_row, selection_row,
    sidebar_items_expander,
};
use crate::player::{
    audio_output_dropdown, build_equalizer_preset_row, connect_equalizer_scale_commit,
    crossfade_duration_row, equalizer_band_title, equalizer_default_preset_bands,
    equalizer_preset_bands, equalizer_preset_name_at, equalizer_preset_position,
    equalizer_selected_preset, install_equalizer_scroll, install_sliding_value_bubble,
    playback_rate_row, preserve_pitch_row,
};
use crate::runtime::{ScrobblingConnection, ScrobblingConnectionEvent};
use crate::shell::Shell;
use crate::{AccentPreference, ThemePreference};
use adw::prelude::*;
use localization::{tr, tr_with};
use playback::StreamQuality;
use playback::{
    EQUALIZER_BAND_COUNT, LoudnessNormalization, LoudnessNormalizationScope,
    MAX_AUTO_DJ_REFILL_THRESHOLD, MAX_EBU_R128_TARGET_LUFS, MIN_AUTO_DJ_REFILL_THRESHOLD,
    MIN_EBU_R128_TARGET_LUFS, PlaybackTransitionMode, VolumeScale,
};

pub(crate) fn scrobbling_page(shell: &Rc<Shell>) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title(tr("Integrations"))
        .icon_name(INTEGRATIONS_ICON_NAME)
        .build();
    let settings = shell.products.scrobbling.preferences();

    let lastfm_group = adw::PreferencesGroup::builder()
        .title(tr("Last.fm"))
        .build();
    let lastfm_enabled = adw::SwitchRow::builder()
        .title(tr("Last.fm scrobbling"))
        .active(settings.lastfm.enabled)
        .build();
    let lastfm_enabled_shell = Rc::clone(shell);
    lastfm_enabled.connect_active_notify(move |row| {
        lastfm_enabled_shell.update_scrobbling_settings("Last.fm scrobbling setting", |settings| {
            if settings.lastfm.enabled == row.is_active() {
                return false;
            }
            settings.lastfm.enabled = row.is_active();
            true
        });
    });
    lastfm_group.add(&lastfm_enabled);

    let lastfm_api_help = adw::ActionRow::builder()
        .title(tr("API keys"))
        .subtitle(inline_link_markup(
            &tr("If you do not have API keys, create them"),
            LASTFM_API_CREATE_URL,
            &tr("here"),
            &tr(". You only need to fill email and an application name parts"),
        ))
        .use_markup(true)
        .build();
    lastfm_group.add(&lastfm_api_help);

    let lastfm_api_key = adw::PasswordEntryRow::builder()
        .title(tr("API key"))
        .text(&settings.lastfm.api_key)
        .show_apply_button(true)
        .build();
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
    lastfm_group.add(&lastfm_api_key);

    let lastfm_api_secret = adw::PasswordEntryRow::builder()
        .title(tr("Shared secret"))
        .text(&settings.lastfm.api_secret)
        .show_apply_button(true)
        .build();
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
    lastfm_group.add(&lastfm_api_secret);

    let lastfm_connection = adw::ActionRow::builder()
        .title(tr("Connection"))
        .subtitle(audioscrobbler_connection_subtitle(
            settings.lastfm.connected,
            &settings.lastfm.username,
        ))
        .build();
    let lastfm_connect_label = if settings.lastfm.connected {
        tr("Reconnect")
    } else {
        tr("Connect")
    };
    let lastfm_connect = gtk::Button::with_label(&lastfm_connect_label);
    lastfm_connect.set_valign(gtk::Align::Center);
    lastfm_connection.add_suffix(&lastfm_connect);
    lastfm_connection.set_activatable_widget(Some(&lastfm_connect));
    let lastfm_connect_shell = Rc::clone(shell);
    let lastfm_api_key_row = lastfm_api_key.clone();
    let lastfm_secret_row = lastfm_api_secret.clone();
    let lastfm_connection_row = lastfm_connection.clone();
    lastfm_connect.connect_clicked(move |button| {
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
    lastfm_group.add(&lastfm_connection);

    let lastfm_now_playing = adw::SwitchRow::builder()
        .title(tr("Now playing updates"))
        .active(settings.lastfm.now_playing_enabled)
        .build();
    let lastfm_now_playing_shell = Rc::clone(shell);
    lastfm_now_playing.connect_active_notify(move |row| {
        lastfm_now_playing_shell.update_scrobbling_settings(
            "Last.fm now playing setting",
            |settings| {
                if settings.lastfm.now_playing_enabled == row.is_active() {
                    return false;
                }
                settings.lastfm.now_playing_enabled = row.is_active();
                true
            },
        );
    });
    lastfm_group.add(&lastfm_now_playing);
    page.add(&lastfm_group);

    let librefm_group = adw::PreferencesGroup::builder()
        .title(tr("Libre.fm"))
        .description(tr(
            "If the page doesn't load, then Libre.fm blocks your IP range/VPN",
        ))
        .build();
    let librefm_enabled = adw::SwitchRow::builder()
        .title(tr("Libre.fm scrobbling"))
        .active(settings.librefm.enabled)
        .build();
    let librefm_enabled_shell = Rc::clone(shell);
    librefm_enabled.connect_active_notify(move |row| {
        librefm_enabled_shell.update_scrobbling_settings(
            "Libre.fm scrobbling setting",
            |settings| {
                if settings.librefm.enabled == row.is_active() {
                    return false;
                }
                settings.librefm.enabled = row.is_active();
                true
            },
        );
    });
    librefm_group.add(&librefm_enabled);

    let librefm_connection = adw::ActionRow::builder()
        .title(tr("Connection"))
        .subtitle(audioscrobbler_connection_subtitle(
            settings.librefm.connected,
            &settings.librefm.username,
        ))
        .build();
    let librefm_connect_label = if settings.librefm.connected {
        tr("Reconnect")
    } else {
        tr("Connect")
    };
    let librefm_connect = gtk::Button::with_label(&librefm_connect_label);
    librefm_connect.set_valign(gtk::Align::Center);
    librefm_connection.add_suffix(&librefm_connect);
    librefm_connection.set_activatable_widget(Some(&librefm_connect));
    let librefm_connect_shell = Rc::clone(shell);
    let librefm_connection_row = librefm_connection.clone();
    librefm_connect.connect_clicked(move |button| {
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
    librefm_group.add(&librefm_connection);

    let librefm_now_playing = adw::SwitchRow::builder()
        .title(tr("Now playing updates"))
        .active(settings.librefm.now_playing_enabled)
        .build();
    let librefm_now_playing_shell = Rc::clone(shell);
    librefm_now_playing.connect_active_notify(move |row| {
        librefm_now_playing_shell.update_scrobbling_settings(
            "Libre.fm now playing setting",
            |settings| {
                if settings.librefm.now_playing_enabled == row.is_active() {
                    return false;
                }
                settings.librefm.now_playing_enabled = row.is_active();
                true
            },
        );
    });
    librefm_group.add(&librefm_now_playing);
    page.add(&librefm_group);

    let listenbrainz_group = adw::PreferencesGroup::builder()
        .title(tr("ListenBrainz"))
        .build();
    let listenbrainz_enabled = adw::SwitchRow::builder()
        .title(tr("ListenBrainz scrobbling"))
        .active(settings.listenbrainz.enabled)
        .build();
    let listenbrainz_enabled_shell = Rc::clone(shell);
    listenbrainz_enabled.connect_active_notify(move |row| {
        listenbrainz_enabled_shell.update_scrobbling_settings(
            "ListenBrainz scrobbling setting",
            |settings| {
                if settings.listenbrainz.enabled == row.is_active() {
                    return false;
                }
                settings.listenbrainz.enabled = row.is_active();
                true
            },
        );
    });
    listenbrainz_group.add(&listenbrainz_enabled);

    let listenbrainz_token_help = adw::ActionRow::builder()
        .title(tr("Get token"))
        .subtitle(inline_link_markup(
            &tr("Find your ListenBrainz user token"),
            LISTENBRAINZ_TOKEN_URL,
            &tr("here"),
            ".",
        ))
        .use_markup(true)
        .build();
    listenbrainz_group.add(&listenbrainz_token_help);

    let listenbrainz_token = adw::PasswordEntryRow::builder()
        .title(tr("User token"))
        .text(&settings.listenbrainz.user_token)
        .show_apply_button(true)
        .build();
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
    listenbrainz_group.add(&listenbrainz_token);

    let listenbrainz_now_playing = adw::SwitchRow::builder()
        .title(tr("Now playing updates"))
        .active(settings.listenbrainz.now_playing_enabled)
        .build();
    let listenbrainz_now_playing_shell = Rc::clone(shell);
    listenbrainz_now_playing.connect_active_notify(move |row| {
        listenbrainz_now_playing_shell.update_scrobbling_settings(
            "ListenBrainz now playing setting",
            |settings| {
                if settings.listenbrainz.now_playing_enabled == row.is_active() {
                    return false;
                }
                settings.listenbrainz.now_playing_enabled = row.is_active();
                true
            },
        );
    });
    listenbrainz_group.add(&listenbrainz_now_playing);
    page.add(&listenbrainz_group);

    page
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
    let page = adw::PreferencesPage::builder()
        .title(tr("Playback"))
        .icon_name("rufin-tracks-symbolic")
        .build();

    let app_settings = shell.settings.current.borrow().clone();
    let settings = app_settings.playback.clone();

    let transition_group = adw::PreferencesGroup::builder()
        .title(tr("Queue and transitions"))
        .build();
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

    let skip_same_album_crossfade_row = adw::SwitchRow::builder()
        .title(tr("Skip same-album crossfade"))
        .subtitle(tr("Keep album transitions gapless when possible"))
        .active(settings.skip_same_album_crossfade)
        .build();
    let skip_same_album_crossfade_shell = Rc::clone(shell);
    skip_same_album_crossfade_row.connect_active_notify(move |row| {
        skip_same_album_crossfade_shell.update_playback_settings(|settings| {
            settings.skip_same_album_crossfade = row.is_active();
        });
    });
    transition_group.add(&skip_same_album_crossfade_row);

    let audio_fade_row = adw::SwitchRow::builder()
        .title(tr("Audio fade on play/pause"))
        .subtitle(tr("Fade audio when playback is paused or resumed"))
        .active(settings.audio_fade_on_status_change)
        .build();
    let audio_fade_shell = Rc::clone(shell);
    audio_fade_row.connect_active_notify(move |row| {
        audio_fade_shell.update_playback_settings(|settings| {
            settings.audio_fade_on_status_change = row.is_active();
        });
    });
    transition_group.add(&audio_fade_row);

    let refill_row = adw::ActionRow::builder()
        .title(tr("Auto DJ refill threshold"))
        .subtitle(tr("Add tracks when fewer than this many remain"))
        .build();
    let refill = gtk::SpinButton::with_range(
        f64::from(MIN_AUTO_DJ_REFILL_THRESHOLD),
        f64::from(MAX_AUTO_DJ_REFILL_THRESHOLD),
        1.0,
    );
    refill.set_value(f64::from(app_settings.auto_dj_refill_threshold));
    refill.set_valign(gtk::Align::Center);
    let refill_shell = Rc::clone(shell);
    refill.connect_value_changed(move |spin| {
        let threshold = spin.value().round() as u8;
        refill_shell.update_app_settings("Auto DJ setting", |settings| {
            if settings.auto_dj_refill_threshold == threshold {
                return false;
            }
            settings.auto_dj_refill_threshold = threshold;
            true
        });
    });
    refill_row.add_suffix(&refill);
    refill_row.set_activatable_widget(Some(&refill));
    transition_group.add(&refill_row);

    let clear_queue_row = adw::SwitchRow::builder()
        .title(tr("Clearing queue also clears the current song"))
        .active(app_settings.clear_queue_includes_current)
        .build();
    let clear_queue_shell = Rc::clone(shell);
    clear_queue_row.connect_active_notify(move |row| {
        clear_queue_shell.update_app_settings("clear queue setting", |settings| {
            if settings.clear_queue_includes_current == row.is_active() {
                return false;
            }
            settings.clear_queue_includes_current = row.is_active();
            true
        });
    });
    transition_group.add(&clear_queue_row);

    page.add(&transition_group);

    let audio_group = adw::PreferencesGroup::builder().title(tr("Audio")).build();
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

    let ebu_target_row = adw::ActionRow::builder()
        .title(tr("Target loudness"))
        .subtitle(tr("EBU R128 reference level"))
        .build();
    let ebu_target = gtk::Scale::with_range(
        gtk::Orientation::Horizontal,
        MIN_EBU_R128_TARGET_LUFS,
        MAX_EBU_R128_TARGET_LUFS,
        1.0,
    );
    ebu_target.add_css_class("playback-setting-scale");
    ebu_target.set_width_request(240);
    ebu_target.set_valign(gtk::Align::Center);
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
    ebu_target_row.add_suffix(&ebu_target);
    ebu_target_row.set_activatable_widget(Some(&ebu_target));

    let write_ebu_tags_row = adw::SwitchRow::builder()
        .title(tr("Write EBU R128 tags to files"))
        .subtitle(tr(
            "Store calculated loudness in supported Local music files",
        ))
        .active(settings.write_ebu_r128_tags)
        .build();
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
    let mode_buttons = Rc::new(mode_buttons);
    let mode_guard = Rc::new(Cell::new(false));
    for (index, button) in mode_buttons.iter().enumerate() {
        let shell = Rc::clone(shell);
        let buttons = Rc::clone(&mode_buttons);
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
            buttons[loudness_normalization_index(previous) as usize].set_active(true);
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
                    buttons[loudness_normalization_index(LoudnessNormalization::EbuR128) as usize]
                        .set_active(true);
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

    let output_row = adw::ActionRow::builder().title(tr("Audio output")).build();
    let output_dropdown = audio_output_dropdown(shell, 220);
    output_row.add_suffix(&output_dropdown);
    output_row.set_activatable_widget(Some(&output_dropdown));
    audio_group.add(&output_row);
    page.add(&audio_group);

    let equalizer_group = adw::PreferencesGroup::builder()
        .title(tr("Equalizer"))
        .build();
    let resetting_equalizer = Rc::new(Cell::new(false));
    let equalizer_row = adw::SwitchRow::builder()
        .title(tr("Enable equalizer"))
        .active(settings.equalizer.enabled)
        .build();
    let equalizer_shell = Rc::clone(shell);
    let switch_reset_guard = Rc::clone(&resetting_equalizer);
    equalizer_row.connect_active_notify(move |row| {
        if switch_reset_guard.get() {
            return;
        }
        equalizer_shell.update_playback_settings(|settings| {
            settings.equalizer.enabled = row.is_active();
        });
    });
    equalizer_group.add(&equalizer_row);

    let selected_preset =
        equalizer_preset_position(&equalizer_selected_preset(&settings.equalizer));
    let selected_preset = Rc::new(Cell::new(selected_preset));
    let preset_row = build_equalizer_preset_row("Preset", selected_preset.get());
    let preset_shell = Rc::clone(shell);
    let preset_switch = equalizer_row.clone();
    let preset_reset_guard = Rc::clone(&resetting_equalizer);
    equalizer_group.add(&preset_row);

    let band_scales = Rc::new(std::cell::RefCell::new(Vec::with_capacity(
        EQUALIZER_BAND_COUNT,
    )));
    let pending_equalizer_update = Rc::new(RefCell::new(None::<gtk::glib::SourceId>));
    let equalizer_drag_active = Rc::new(Cell::new(false));
    let equalizer_commit: Rc<dyn Fn()> = {
        let band_shell = Rc::clone(shell);
        let update_preset = preset_row.clone();
        let update_selected_preset = Rc::clone(&selected_preset);
        let update_guard = Rc::clone(&resetting_equalizer);
        let update_scales = Rc::clone(&band_scales);
        Rc::new(move || {
            let bands = update_scales
                .borrow()
                .iter()
                .map(gtk::Scale::value)
                .collect::<Vec<_>>();
            band_shell.update_playback_settings(|settings| {
                if settings.equalizer.bands.len() != EQUALIZER_BAND_COUNT {
                    settings.equalizer.sanitize();
                }
                settings.equalizer.bands = bands.clone();
                settings.equalizer.selected_preset = "Custom".to_string();
            });
            let preset =
                equalizer_selected_preset(&band_shell.settings.current.borrow().playback.equalizer);
            update_guard.set(true);
            update_selected_preset.set(equalizer_preset_position(&preset));
            update_preset.set_selected(equalizer_preset_position(&preset));
            update_guard.set(false);
        })
    };
    for index in 0..EQUALIZER_BAND_COUNT {
        let row = adw::ActionRow::builder()
            .title(equalizer_band_title(index))
            .build();
        let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, -12.0, 12.0, 0.5);
        scale.set_value(settings.equalizer.bands.get(index).copied().unwrap_or(0.0));
        scale.set_draw_value(true);
        scale.set_digits(1);
        scale.set_width_request(220);
        scale.set_valign(gtk::Align::Center);
        install_equalizer_scroll(&scale);
        connect_equalizer_scale_commit(
            &scale,
            Rc::clone(&resetting_equalizer),
            Rc::clone(&pending_equalizer_update),
            Rc::clone(&equalizer_drag_active),
            Rc::clone(&equalizer_commit),
        );
        row.add_suffix(&scale);
        row.set_activatable_widget(Some(&scale));
        equalizer_group.add(&row);
        band_scales.borrow_mut().push(scale);
    }

    let preset_scales = Rc::clone(&band_scales);
    let preset_selected_preset = Rc::clone(&selected_preset);
    preset_row.connect_selected_notify(move |row| {
        if preset_reset_guard.get() {
            return;
        }
        let Some(preset) = equalizer_preset_name_at(row.selected()) else {
            return;
        };
        let bands = equalizer_preset_bands(&preset);
        preset_reset_guard.set(true);
        preset_switch.set_active(true);
        preset_selected_preset.set(equalizer_preset_position(&preset));
        for (scale, gain) in preset_scales.borrow().iter().zip(bands.iter()) {
            scale.set_value(*gain);
        }
        preset_reset_guard.set(false);
        preset_shell.update_playback_settings(|settings| {
            settings.equalizer.enabled = true;
            settings.equalizer.selected_preset = preset.clone();
            settings.equalizer.bands = bands;
            settings.equalizer.sanitize();
        });
    });

    let reset_row = adw::ActionRow::builder()
        .title(tr("Reset equalizer"))
        .build();
    let reset_button = gtk::Button::with_label(&tr("Reset"));
    reset_button.set_valign(gtk::Align::Center);
    reset_button.add_css_class("destructive-action");
    let reset_shell = Rc::clone(shell);
    let reset_preset = preset_row.clone();
    let reset_selected_preset = Rc::clone(&selected_preset);
    let reset_scales = Rc::clone(&band_scales);
    let reset_guard = Rc::clone(&resetting_equalizer);
    reset_button.connect_clicked(move |_| {
        let preset = equalizer_preset_name_at(reset_selected_preset.get()).unwrap_or_else(|| {
            equalizer_selected_preset(&reset_shell.settings.current.borrow().playback.equalizer)
        });
        let bands = equalizer_default_preset_bands(&preset);
        reset_guard.set(true);
        reset_selected_preset.set(equalizer_preset_position(&preset));
        reset_preset.set_selected(equalizer_preset_position(&preset));
        for (scale, gain) in reset_scales.borrow().iter().zip(bands.iter()) {
            scale.set_value(*gain);
        }
        reset_guard.set(false);
        reset_shell.update_playback_settings(|settings| {
            settings.equalizer.selected_preset = preset;
            settings.equalizer.bands = bands;
            settings.equalizer.sanitize();
        });
    });
    reset_row.add_suffix(&reset_button);
    reset_row.set_activatable_widget(Some(&reset_button));
    equalizer_group.add(&reset_row);
    page.add(&equalizer_group);

    page
}
pub(crate) fn appearance_page(shell: &Rc<Shell>) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title(tr("Appearance"))
        .icon_name("rufin-preferences-desktop-appearance-symbolic")
        .build();

    page.add(&theme_group(shell));
    page.add(&seekbar_group(shell));
    page.add(&layout_group(shell));

    let sidebar_items_group = adw::PreferencesGroup::new();
    sidebar_items_group.add(&sidebar_items_expander(shell));
    page.add(&sidebar_items_group);

    let home_blocks = adw::ExpanderRow::builder()
        .title(tr("Home Blocks"))
        .expanded(false)
        .build();
    let rows = Rc::new(std::cell::RefCell::new(Vec::new()));
    populate_home_block_rows(shell, &home_blocks, &rows);
    let home_blocks_group = adw::PreferencesGroup::new();
    home_blocks_group.add(&home_blocks);
    page.add(&home_blocks_group);

    let context_menus_group = adw::PreferencesGroup::new();
    context_menus_group.add(&context_menus_expander(shell));
    page.add(&context_menus_group);

    page
}

fn seekbar_group(shell: &Rc<Shell>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title(tr("Seekbar"))
        .build();
    let waveform_row = adw::SwitchRow::builder()
        .title(tr("Waveform seekbar"))
        .subtitle(tr("Generate and cache waveforms for the current track"))
        .active(shell.settings.current.borrow().seekbar_waveform_enabled)
        .build();
    let waveform_shell = Rc::clone(shell);
    waveform_row.connect_active_notify(move |row| {
        waveform_shell.set_seekbar_waveform_enabled(row.is_active());
    });
    group.add(&waveform_row);
    group
}

fn theme_group(shell: &Rc<Shell>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title(tr("Theme")).build();
    let options = [tr("System"), tr("Light"), tr("Dark")];
    let selected = theme_preference_index(shell.settings.current.borrow().theme_preference);
    let theme_shell = Rc::clone(shell);
    let row = selection_row(&tr("Color scheme"), &options, selected, move |selected| {
        theme_shell.set_theme_preference(theme_preference_from_index(selected));
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
        accent_shell.set_accent_preference(accent_preference_from_index(row.selected()));
    });
    group.add(&accent_row);
    group
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
