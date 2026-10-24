pub const NORMALIZATION_SCOPE_TITLE: &str = localization::msgid("Normalization scope");
pub const NORMALIZATION_TITLE: &str = localization::msgid("Loudness normalization");
pub const VOLUME_SCALE_TITLE: &str = localization::msgid("Volume scale");
pub const STREAM_QUALITY_TITLE: &str = localization::msgid("Stream quality");
use gtk_player::equalizer::EqualizerSurface;
use gtk_player::outputs::audio_output_dropdown;
use gtk_player::playback_settings::{
    playback_rate_row, preserve_pitch_row, transition_rows, volume_scale_from_index,
    volume_scale_index,
};
use gtk_widgets::forms::{controlled_selection_row, quality_selection_row, selection_row};
use gtk_widgets::scale::{install_scale_scroll_forwarding, install_sliding_value_bubble};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use super::{
    LASTFM_API_CREATE_URL, LISTENBRAINZ_TOKEN_URL, configure_sidebar_items_expander,
    context_menu::configure_context_menus,
    layout::populate_home_block_rows,
    layout::{
        discord_display_from_index, discord_display_index, discord_link_from_index,
        discord_link_index,
    },
    populate_layout_group, show_discord_flatpak_help,
};

use crate::Preferences;
use adw::prelude::*;
use localization::{tr, tr_with};
use playback::StreamQuality;
use playback::{
    LoudnessNormalization, LoudnessNormalizationScope, MAX_AUTO_DJ_REFILL_THRESHOLD,
    MAX_EBU_R128_TARGET_LUFS, MIN_AUTO_DJ_REFILL_THRESHOLD, MIN_EBU_R128_TARGET_LUFS,
};
use rufin_core::runtime::{ScrobblingConnection, ScrobblingConnectionEvent, ScrobblingPreferences};

pub(super) type ScrobblingCredentialDrafts = Rc<RefCell<ScrobblingPreferences>>;

pub fn scrobbling_page(
    shell: &Rc<Preferences>,
    drafts: &ScrobblingCredentialDrafts,
) -> adw::PreferencesPage {
    let resource = crate::ui_resource::INTEGRATIONS_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    let page: adw::PreferencesPage =
        gtk_widgets::ui_resource::object(&builder, resource, "integrations_page");
    super::source::login::bind_integrations(shell, &page, &builder);
    let settings = shell.products.scrobbling.preferences();
    gtk_widgets::objects!(builder, resource, {
        external_links_row: adw::SwitchRow,
        lastfm_links_row: adw::SwitchRow,
        musicbrainz_links_row: adw::SwitchRow,
        server_links_row: adw::SwitchRow,
        presence_row: adw::SwitchRow,
        display_row: adw::ComboRow,
        link_row: adw::ComboRow,
        paused_row: adw::SwitchRow,
        listening_row: adw::SwitchRow,
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

    {
        let settings = shell.settings.current.borrow().clone();
        let external_links = settings.application.external_site_links.clone();
        external_links_row.set_active(external_links.enabled);
        lastfm_links_row.set_active(external_links.lastfm);
        lastfm_links_row.set_sensitive(external_links.enabled);
        musicbrainz_links_row.set_active(external_links.musicbrainz);
        musicbrainz_links_row.set_sensitive(external_links.enabled);
        server_links_row.set_active(external_links.server);
        server_links_row.set_sensitive(external_links.enabled);
        let external_links_shell = Rc::clone(shell);
        let lastfm_links_for_master = lastfm_links_row.clone();
        let musicbrainz_links_for_master = musicbrainz_links_row.clone();
        let server_links_for_master = server_links_row.clone();
        external_links_row.connect_active_notify(move |row| {
            let enabled = row.is_active();
            lastfm_links_for_master.set_sensitive(enabled);
            musicbrainz_links_for_master.set_sensitive(enabled);
            server_links_for_master.set_sensitive(enabled);
            if external_links_shell
                .settings
                .set_app_setting("external site links setting", enabled, |settings| {
                    &mut settings.application.external_site_links.enabled
                })
                .is_some()
            {
                (external_links_shell.effects)(crate::Effect::CatalogChanged);
            }
        });
        let lastfm_links_shell = Rc::clone(shell);
        lastfm_links_row.connect_active_notify(move |row| {
            if lastfm_links_shell
                .settings
                .set_app_setting("Last.fm site links setting", row.is_active(), |settings| {
                    &mut settings.application.external_site_links.lastfm
                })
                .is_some()
            {
                (lastfm_links_shell.effects)(crate::Effect::CatalogChanged);
            }
        });
        let musicbrainz_links_shell = Rc::clone(shell);
        musicbrainz_links_row.connect_active_notify(move |row| {
            if musicbrainz_links_shell
                .settings
                .set_app_setting(
                    "MusicBrainz site links setting",
                    row.is_active(),
                    |settings| &mut settings.application.external_site_links.musicbrainz,
                )
                .is_some()
            {
                (musicbrainz_links_shell.effects)(crate::Effect::CatalogChanged);
            }
        });
        let server_links_shell = Rc::clone(shell);
        server_links_row.connect_active_notify(move |row| {
            if server_links_shell
                .settings
                .set_app_setting("server site links setting", row.is_active(), |settings| {
                    &mut settings.application.external_site_links.server
                })
                .is_some()
            {
                (server_links_shell.effects)(crate::Effect::CatalogChanged);
            }
        });
        presence_row.set_active(settings.rich_presence.enabled);
        let presence_shell = Rc::clone(shell);
        presence_row.connect_active_notify(move |row| {
            presence_shell.settings.set_app_setting(
                "Discord presence setting",
                row.is_active(),
                |settings| &mut settings.rich_presence.enabled,
            );
            show_discord_flatpak_help(row);
        });
        let display_titles = [tr("Application name"), tr("Song title"), tr("Artist name")];
        let display_refs = display_titles
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let display_options = gtk::StringList::new(&display_refs);
        display_row.set_model(Some(&display_options));
        display_row.set_selected(discord_display_index(settings.rich_presence.display_type));
        let display_shell = Rc::clone(shell);
        display_row.connect_selected_notify(move |row| {
            display_shell.settings.set_app_setting(
                "Discord display setting",
                discord_display_from_index(row.selected()),
                |settings| &mut settings.rich_presence.display_type,
            );
        });
        let link_titles = [
            tr("None"),
            tr("Last.fm"),
            tr("MusicBrainz"),
            tr("MusicBrainz + Last.fm"),
        ];
        let link_refs = link_titles.iter().map(String::as_str).collect::<Vec<_>>();
        let link_options = gtk::StringList::new(&link_refs);
        link_row.set_model(Some(&link_options));
        link_row.set_selected(discord_link_index(settings.rich_presence.link_type));
        let link_shell = Rc::clone(shell);
        link_row.connect_selected_notify(move |row| {
            link_shell.settings.set_app_setting(
                "Discord link setting",
                discord_link_from_index(row.selected()),
                |settings| &mut settings.rich_presence.link_type,
            );
        });
        paused_row.set_active(settings.rich_presence.show_paused);
        let paused_shell = Rc::clone(shell);
        paused_row.connect_active_notify(move |row| {
            paused_shell.settings.set_app_setting(
                "Discord paused setting",
                row.is_active(),
                |settings| &mut settings.rich_presence.show_paused,
            );
        });
        listening_row.set_active(settings.rich_presence.show_as_listening);
        let listening_shell = Rc::clone(shell);
        listening_row.connect_active_notify(move |row| {
            listening_shell.settings.set_app_setting(
                "Discord activity type setting",
                row.is_active(),
                |settings| &mut settings.rich_presence.show_as_listening,
            );
        });
    }

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
        lastfm_api_shell.save_scrobbling_credential(
            "Last.fm API key setting",
            rufin_core::scrobbling::ScrobblingCredentialEdit::LastFm {
                api_key: Some(api_key),
                api_secret: None,
            },
        );
    });
    lastfm_api_secret.set_text(&drafts.borrow().lastfm.api_secret);
    let secret_drafts = drafts.clone();
    lastfm_api_secret.connect_text_notify(move |row| {
        secret_drafts.borrow_mut().lastfm.api_secret = row.text().to_string();
    });
    let lastfm_secret_shell = Rc::clone(shell);
    lastfm_api_secret.connect_apply(move |row| {
        let api_secret = row.text().trim().to_string();
        lastfm_secret_shell.save_scrobbling_credential(
            "Last.fm shared secret setting",
            rufin_core::scrobbling::ScrobblingCredentialEdit::LastFm {
                api_key: None,
                api_secret: Some(api_secret),
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
        listenbrainz_token_shell.save_scrobbling_credential(
            "ListenBrainz token setting",
            rufin_core::scrobbling::ScrobblingCredentialEdit::ListenBrainz { user_token: token },
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
    let owner = shell.products.scrobbling.clone();
    let initial_key = lastfm_api_key.text();
    let initial_secret = lastfm_api_secret.text();
    let initial_token = listenbrainz_token.text();
    let weak_page = page.downgrade();
    gtk::glib::spawn_future_local(async move {
        let Ok(loaded) = owner.load_preferences().await else {
            return;
        };
        if weak_page.upgrade().is_none() {
            return;
        }
        for (row, initial, value) in [
            (lastfm_api_key, initial_key, loaded.lastfm.api_key),
            (lastfm_api_secret, initial_secret, loaded.lastfm.api_secret),
            (
                listenbrainz_token,
                initial_token,
                loaded.listenbrainz.user_token,
            ),
        ] {
            if row.text() == initial {
                row.set_text(&value);
            }
        }
        for (button, row, connected, username) in [
            (
                lastfm_connect,
                lastfm_connection,
                loaded.lastfm.connected,
                loaded.lastfm.username,
            ),
            (
                librefm_connect,
                librefm_connection,
                loaded.librefm.connected,
                loaded.librefm.username,
            ),
        ] {
            if button.is_sensitive() {
                row.set_subtitle(&audioscrobbler_connection_subtitle(connected, &username));
                button.set_label(&if connected {
                    tr("Reconnect")
                } else {
                    tr("Connect")
                });
            }
        }
    });
    page
}

fn bind_scrobbling_switch(
    row: &adw::SwitchRow,
    shell: &Rc<Preferences>,
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

pub fn audioscrobbler_connection_subtitle(connected: bool, username: &str) -> String {
    if !connected {
        tr("Not connected")
    } else {
        audioscrobbler_connected_subtitle(username)
    }
}
pub fn audioscrobbler_connected_subtitle(username: &str) -> String {
    let username = username.trim();
    if username.is_empty() {
        tr("Connected")
    } else {
        tr_with("Connected as {username}", &[("username", username)])
    }
}
pub fn inline_link_markup(before: &str, url: &str, label: &str, after: &str) -> String {
    let before = gtk::glib::markup_escape_text(before);
    let url = gtk::glib::markup_escape_text(url);
    let label = gtk::glib::markup_escape_text(label);
    let after = gtk::glib::markup_escape_text(after);
    format!("{before} <a href=\"{url}\">{label}</a>{after}")
}

fn selected_source_is_local(shell: &Preferences) -> bool {
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
    shell: &Rc<Preferences>,
    request: ScrobblingConnection,
    open_error: &'static str,
    connection_error: String,
    refresh_external_artwork: bool,
) -> Result<String, String> {
    let events = shell.products.scrobbling.connect(request);
    while let Ok(event) = events.next().await {
        match event {
            ScrobblingConnectionEvent::OpenUrl { url, opened } => {
                let launcher = gtk::UriLauncher::new(&url);
                match launcher.launch_future(Some(&shell.window)).await {
                    Ok(()) => {
                        opened.complete(None).await;
                    }
                    Err(error) => {
                        let error = format!("{open_error}{error}");
                        opened.complete(Some(error.clone())).await;
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
pub fn playback_page(shell: &Rc<Preferences>) -> adw::PreferencesPage {
    let resource = crate::ui_resource::PLAYBACK_PREFERENCES_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    let page: adw::PreferencesPage =
        gtk_widgets::ui_resource::object(&builder, resource, "playback_page");

    let app_settings = shell.settings.current.borrow().clone();
    let settings = app_settings.playback.clone();
    gtk_widgets::objects!(builder, resource, {
        transition_group: adw::PreferencesGroup,
        audio_fade_row: adw::SwitchRow,
        skip_unplayable_row: adw::SwitchRow,
        refill_row: adw::ActionRow,
        refill: gtk::SpinButton,
        clear_queue_row: adw::SwitchRow,
        audio_group: adw::PreferencesGroup,
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
    transition_group.add(&refill_row);

    clear_queue_row.set_active(app_settings.player_display.clear_queue_includes_current);
    let clear_queue_shell = Rc::clone(shell);
    clear_queue_row.connect_active_notify(move |row| {
        clear_queue_shell.settings.set_app_setting(
            "clear queue setting",
            row.is_active(),
            |settings| &mut settings.player_display.clear_queue_includes_current,
        );
    });
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
            let window = shell.window.clone();
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
    let quality_row = quality_selection_row(
        &tr(STREAM_QUALITY_TITLE),
        &[
            StreamQuality::Original,
            StreamQuality::MaxBitrateKbps(320),
            StreamQuality::MaxBitrateKbps(256),
            StreamQuality::MaxBitrateKbps(192),
            StreamQuality::MaxBitrateKbps(128),
        ],
        stream_quality_index(settings.stream_quality),
        move |selected| {
            quality_shell
                .player_ui
                .update_playback_settings(|settings| {
                    settings.stream_quality = stream_quality_from_index(selected);
                });
        },
    );
    audio_group.add(&quality_row);

    let output_dropdown = audio_output_dropdown(&shell.player_ui, 220);
    output_row.add_suffix(&output_dropdown);
    output_row.set_activatable_widget(Some(&output_dropdown));
    audio_group.add(&output_row);

    page
}

pub fn equalizer_page(shell: &Rc<Preferences>) -> adw::PreferencesPage {
    let resource = crate::ui_resource::PLAYBACK_PREFERENCES_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        equalizer_page: adw::PreferencesPage,
        equalizer_group: adw::PreferencesGroup,
    });
    let equalizer = EqualizerSurface::new(&shell.player_ui);
    equalizer_group.add(&equalizer.root);

    equalizer_page
}
pub fn appearance_page(shell: &Rc<Preferences>) -> adw::PreferencesPage {
    let resource = crate::ui_resource::APPEARANCE_PREFERENCES_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    let page: adw::PreferencesPage =
        gtk_widgets::ui_resource::object(&builder, resource, "appearance_page");
    gtk_widgets::objects!(builder, resource, {
        reduce_motion: adw::SwitchRow,
        sidebar_items: adw::ExpanderRow,
        sidebar_pins_row: adw::SwitchRow,
        home_blocks: adw::ExpanderRow,
    });

    reduce_motion.set_active(shell.settings.current.borrow().application.reduce_motion);
    let motion_shell = Rc::clone(shell);
    reduce_motion.connect_active_notify(move |row| {
        if let Some(settings) =
            motion_shell
                .settings
                .set_app_setting("reduced motion", row.is_active(), |settings| {
                    &mut settings.application.reduce_motion
                })
        {
            motion_shell.appearance.apply(&settings);
        }
    });
    populate_layout_group(shell, &builder);

    configure_sidebar_items_expander(shell, &sidebar_items, &sidebar_pins_row);

    let rows = Rc::new(std::cell::RefCell::new(Vec::new()));
    let home_shell = Rc::clone(shell);
    super::populate_expander_once(&home_blocks, move |home_blocks| {
        populate_home_block_rows(&home_shell, home_blocks, &rows);
    });

    configure_context_menus(shell, &builder);

    page
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

pub fn stream_quality_index(quality: StreamQuality) -> u32 {
    match quality {
        StreamQuality::Original => 0,
        StreamQuality::MaxBitrateKbps(320) => 1,
        StreamQuality::MaxBitrateKbps(256) => 2,
        StreamQuality::MaxBitrateKbps(192) => 3,
        StreamQuality::MaxBitrateKbps(128) => 4,
        StreamQuality::MaxBitrateKbps(_) => 0,
    }
}
pub fn stream_quality_from_index(index: u32) -> StreamQuality {
    match index {
        1 => StreamQuality::MaxBitrateKbps(320),
        2 => StreamQuality::MaxBitrateKbps(256),
        3 => StreamQuality::MaxBitrateKbps(192),
        4 => StreamQuality::MaxBitrateKbps(128),
        _ => StreamQuality::Original,
    }
}
