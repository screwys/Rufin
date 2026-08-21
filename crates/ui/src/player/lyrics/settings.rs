use std::{cell::RefCell, rc::Rc};

use adw::prelude::*;
use localization::{
    default_language_preference, language_option_index, language_options, msgid, tr,
};
use lyrics::ExternalLyricsProvider;

use crate::preferences::dialogs::popup::present_light_dismiss_dialog;
use crate::shell::Shell;

#[derive(Clone)]
pub(crate) struct LyricsSettingsDialog {
    pub(crate) dialog: adw::PreferencesDialog,
    word_by_word_highlighting: adw::SwitchRow,
}

pub(crate) fn connect_lyrics_settings_controls(shell: &Rc<Shell>) {
    let Some(lyrics) = shell.selected_lyrics() else {
        return;
    };
    for pane in [lyrics.right_pane.clone(), lyrics.fullscreen_pane.clone()] {
        let settings_shell = Rc::downgrade(shell);
        pane.connect_settings_clicked(move || {
            if let Some(shell) = settings_shell.upgrade() {
                present_lyrics_settings_dialog(&shell);
            }
        });
    }
}

fn present_lyrics_settings_dialog(shell: &Rc<Shell>) {
    let Some(lyrics) = shell.selected_lyrics() else {
        return;
    };
    if let Some(settings_dialog) = lyrics.settings_dialog.borrow().as_ref() {
        settings_dialog.dialog.present(Some(&shell.chrome.window));
        return;
    }
    drop(lyrics);

    let (page, word_by_word_highlighting) = build_lyrics_settings(shell);
    let dialog = adw::PreferencesDialog::builder()
        .title(tr("Lyrics settings"))
        .search_enabled(false)
        .content_width(500)
        .content_height(600)
        .build();
    dialog.add(&page);
    let settings_dialog = LyricsSettingsDialog {
        dialog: dialog.clone(),
        word_by_word_highlighting,
    };
    if let Some(lyrics) = shell.selected_lyrics() {
        lyrics.settings_dialog.replace(Some(settings_dialog));
    }

    let close_shell = Rc::clone(shell);
    dialog.connect_closed(move |_| {
        if let Some(lyrics) = close_shell.selected_lyrics() {
            lyrics.settings_dialog.borrow_mut().take();
        }
    });
    present_light_dismiss_dialog(&dialog, &shell.chrome.window);
}

pub(crate) fn refresh_word_highlighting_availability(shell: &Rc<Shell>) {
    let Some(lyrics) = shell.selected_lyrics() else {
        return;
    };
    if let Some(settings_dialog) = lyrics.settings_dialog.borrow().as_ref() {
        settings_dialog
            .word_by_word_highlighting
            .set_sensitive(shell.visible_lyrics_have_word_timing());
    }
}

fn build_lyrics_settings(shell: &Rc<Shell>) -> (adw::PreferencesPage, adw::SwitchRow) {
    let settings = shell.settings.current.borrow().lyrics.clone();
    let page = adw::PreferencesPage::builder()
        .title(tr("Lyrics settings"))
        .icon_name("rufin-applications-system-symbolic")
        .build();

    let sources = adw::PreferencesGroup::builder()
        .title(tr("Sources"))
        .build();
    let external = switch_row(
        msgid("External lyric lookup"),
        settings.external_lyrics_enabled,
    );
    sources.add(&external);

    let prefer_server = switch_row(msgid("Prefer server lyrics"), settings.prefer_server_lyrics);
    prefer_server.set_sensitive(settings.external_lyrics_enabled);
    let prefer_server_shell = Rc::clone(shell);
    prefer_server.connect_active_notify(move |row| {
        prefer_server_shell.set_prefer_server_lyrics(row.is_active());
    });
    sources.add(&prefer_server);

    let provider_rows = Rc::new(RefCell::new(Vec::new()));
    populate_provider_rows(shell, &sources, &provider_rows);
    let external_shell = Rc::clone(shell);
    let external_sources = sources.downgrade();
    let external_provider_rows = Rc::clone(&provider_rows);
    let external_prefer_server = prefer_server.clone();
    external.connect_active_notify(move |row| {
        if !external_shell.set_external_lyrics_enabled(row.is_active()) {
            return;
        }
        external_prefer_server.set_sensitive(row.is_active());
        let Some(sources) = external_sources.upgrade() else {
            return;
        };
        populate_provider_rows(&external_shell, &sources, &external_provider_rows);
    });
    page.add(&sources);

    let language_and_readings = adw::PreferencesGroup::builder()
        .title(tr("Language and readings"))
        .build();
    let translations = switch_row(msgid("Prefer translations"), settings.prefer_translations);
    language_and_readings.add(&translations);

    let translation_languages = Rc::new(
        language_options()
            .into_iter()
            .filter(|option| option.id != default_language_preference())
            .collect::<Vec<_>>(),
    );
    let language_titles = translation_languages
        .iter()
        .map(|option| option.title.as_str())
        .collect::<Vec<_>>();
    let language_model = gtk::StringList::new(&language_titles);
    let language_row = adw::ComboRow::builder()
        .title(tr("Translation language"))
        .model(&language_model)
        .selected(language_option_index(
            translation_languages.as_ref(),
            &settings.preferred_translation_language,
        ))
        .build();
    language_row.set_sensitive(settings.prefer_translations);
    let language_shell = Rc::clone(shell);
    let translation_languages_for_row = Rc::clone(&translation_languages);
    language_row.connect_selected_notify(move |row| {
        let Some(language) = translation_languages_for_row.get(row.selected() as usize) else {
            return;
        };
        language_shell.set_preferred_lyrics_translation_language(&language.id);
    });
    language_and_readings.add(&language_row);
    let translations_shell = Rc::clone(shell);
    let translations_language_row = language_row.clone();
    translations.connect_active_notify(move |row| {
        if translations_shell.set_prefer_lyrics_translations(row.is_active()) {
            translations_language_row.set_sensitive(row.is_active());
        }
    });

    let furigana = reading_switch_row(msgid("Furigana"), settings.show_furigana);
    let furigana_shell = Rc::clone(shell);
    furigana.connect_active_notify(move |row| {
        furigana_shell.set_lyrics_furigana(row.is_active());
    });
    language_and_readings.add(&furigana);

    let romanization = reading_switch_row(msgid("Romaji"), settings.show_romanization);
    let romanization_shell = Rc::clone(shell);
    romanization.connect_active_notify(move |row| {
        romanization_shell.set_lyrics_romanization(row.is_active());
    });
    language_and_readings.add(&romanization);
    page.add(&language_and_readings);

    let playback = adw::PreferencesGroup::builder()
        .title(tr("Playback"))
        .build();
    let karaoke = adw::SwitchRow::builder()
        .title(tr(msgid("Karaoke mode")))
        .subtitle(tr(msgid("Requires OpenSubsonic songLyrics v2")))
        .active(settings.word_by_word_highlighting)
        .build();
    karaoke.set_sensitive(shell.visible_lyrics_have_word_timing());
    let karaoke_shell = Rc::clone(shell);
    karaoke.connect_active_notify(move |row| {
        karaoke_shell.set_lyrics_word_highlighting(row.is_active());
    });
    playback.add(&karaoke);
    page.add(&playback);
    (page, karaoke)
}

fn populate_provider_rows(
    shell: &Rc<Shell>,
    group: &adw::PreferencesGroup,
    rows: &Rc<RefCell<Vec<gtk::glib::WeakRef<adw::ActionRow>>>>,
) {
    for row in rows.borrow_mut().drain(..) {
        if let Some(row) = row.upgrade() {
            group.remove(&row);
        }
    }

    let settings = shell.settings.current.borrow().lyrics.clone();
    let enabled = settings.external_lyrics_providers;
    let mut providers = enabled.clone();
    providers.extend(
        ExternalLyricsProvider::all()
            .into_iter()
            .filter(|provider| !enabled.contains(provider)),
    );
    for (index, provider) in providers.into_iter().enumerate() {
        let provider_enabled = enabled.contains(&provider);
        let row = adw::ActionRow::builder()
            .title(tr(provider.title()))
            .build();
        row.add_css_class("lyrics-provider-order");
        row.set_sensitive(settings.external_lyrics_enabled);
        if provider_enabled {
            let drag = gtk::Image::from_icon_name("rufin-list-drag-handle-symbolic");
            drag.add_css_class("dim-label");
            drag.set_tooltip_text(Some(&tr("Drag to reorder")));
            row.add_prefix(&drag);

            let source = gtk::DragSource::builder()
                .actions(gtk::gdk::DragAction::MOVE)
                .build();
            let provider_id = provider.key().to_string();
            source.connect_prepare(move |_, _, _| {
                Some(gtk::gdk::ContentProvider::for_value(
                    &provider_id.to_value(),
                ))
            });
            drag.add_controller(source);

            let drop_target =
                gtk::DropTarget::new(String::static_type(), gtk::gdk::DragAction::MOVE);
            let drop_shell = Rc::clone(shell);
            let drop_group = group.downgrade();
            let drop_rows = Rc::downgrade(rows);
            let drop_row = row.downgrade();
            drop_target.connect_drop(move |_, value, _, y| {
                let Ok(source_id) = value.get::<String>() else {
                    return false;
                };
                let Some(source_provider) = ExternalLyricsProvider::from_key(&source_id) else {
                    return false;
                };
                let Some(row) = drop_row.upgrade() else {
                    return false;
                };
                let after = y > f64::from(row.height()) / 2.0;
                if !drop_shell.reorder_external_lyrics_provider(source_provider, provider, after) {
                    return false;
                }
                let (Some(group), Some(rows)) = (drop_group.upgrade(), drop_rows.upgrade()) else {
                    return false;
                };
                populate_provider_rows(&drop_shell, &group, &rows);
                true
            });
            row.add_controller(drop_target);

            let up = small_icon_button("rufin-go-up-symbolic", msgid("Move provider up"));
            up.set_sensitive(index > 0);
            let up_shell = Rc::clone(shell);
            let up_group = group.downgrade();
            let up_rows = Rc::clone(rows);
            up.connect_clicked(move |_| {
                if !up_shell.move_external_lyrics_provider(provider, -1) {
                    return;
                }
                let Some(group) = up_group.upgrade() else {
                    return;
                };
                populate_provider_rows(&up_shell, &group, &up_rows);
            });
            row.add_suffix(&up);
            let down = small_icon_button("rufin-go-down-symbolic", msgid("Move provider down"));
            down.set_sensitive(index + 1 < enabled.len());
            let down_shell = Rc::clone(shell);
            let down_group = group.downgrade();
            let down_rows = Rc::clone(rows);
            down.connect_clicked(move |_| {
                if !down_shell.move_external_lyrics_provider(provider, 1) {
                    return;
                }
                let Some(group) = down_group.upgrade() else {
                    return;
                };
                populate_provider_rows(&down_shell, &group, &down_rows);
            });
            row.add_suffix(&down);
        }
        let toggle = gtk::Switch::builder()
            .active(provider_enabled)
            .valign(gtk::Align::Center)
            .build();
        let provider_shell = Rc::clone(shell);
        let provider_group = group.downgrade();
        let provider_rows = Rc::clone(rows);
        toggle.connect_active_notify(move |switch| {
            if !provider_shell.set_external_lyrics_provider_enabled(provider, switch.is_active()) {
                return;
            }
            let Some(group) = provider_group.upgrade() else {
                return;
            };
            populate_provider_rows(&provider_shell, &group, &provider_rows);
        });
        row.add_suffix(&toggle);
        group.add(&row);
        rows.borrow_mut().push(row.downgrade());
    }
}

fn switch_row(title: &str, active: bool) -> adw::SwitchRow {
    adw::SwitchRow::builder()
        .title(tr(title))
        .active(active)
        .build()
}

fn reading_switch_row(title: &str, active: bool) -> adw::SwitchRow {
    adw::SwitchRow::builder()
        .title(tr(title))
        .subtitle(tr(msgid("Increases memory usage")))
        .active(active)
        .build()
}

fn small_icon_button(icon: &str, label: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    button.add_css_class("flat");
    button.add_css_class("circular");
    button.set_tooltip_text(Some(&tr(label)));
    button
}

impl Shell {
    pub(crate) fn set_external_lyrics_enabled(self: &Rc<Self>, enabled: bool) -> bool {
        self.update_lyrics_settings("lyrics setting", false, |settings| {
            if settings.external_lyrics_enabled == enabled {
                return false;
            }
            settings.external_lyrics_enabled = enabled;
            true
        })
    }

    pub(crate) fn set_prefer_server_lyrics(self: &Rc<Self>, enabled: bool) {
        self.update_lyrics_settings("lyrics search setting", false, |settings| {
            if settings.prefer_server_lyrics == enabled {
                return false;
            }
            settings.prefer_server_lyrics = enabled;
            true
        });
    }

    pub(crate) fn set_external_lyrics_provider_enabled(
        self: &Rc<Self>,
        provider: ExternalLyricsProvider,
        enabled: bool,
    ) -> bool {
        self.update_lyrics_settings("lyrics provider setting", false, |settings| {
            let has_provider = settings.external_lyrics_providers.contains(&provider);
            if has_provider == enabled {
                return false;
            }
            if enabled {
                settings.external_lyrics_providers.push(provider);
            } else {
                settings
                    .external_lyrics_providers
                    .retain(|candidate| *candidate != provider);
            }
            true
        })
    }

    pub(crate) fn move_external_lyrics_provider(
        self: &Rc<Self>,
        provider: ExternalLyricsProvider,
        direction: isize,
    ) -> bool {
        self.update_lyrics_settings("lyrics provider order", false, |settings| {
            settings.move_external_lyrics_provider(provider, direction)
        })
    }

    pub(crate) fn reorder_external_lyrics_provider(
        self: &Rc<Self>,
        source: ExternalLyricsProvider,
        target: ExternalLyricsProvider,
        after: bool,
    ) -> bool {
        self.update_lyrics_settings("lyrics provider order", false, |settings| {
            settings.reorder_external_lyrics_provider(source, target, after)
        })
    }

    pub(crate) fn set_prefer_lyrics_translations(self: &Rc<Self>, enabled: bool) -> bool {
        self.update_lyrics_settings("lyrics translation setting", false, |settings| {
            if settings.prefer_translations == enabled {
                return false;
            }
            settings.prefer_translations = enabled;
            true
        })
    }

    pub(crate) fn set_preferred_lyrics_translation_language(self: &Rc<Self>, language: &str) {
        let language = language.to_string();
        self.update_lyrics_settings("lyrics translation language", false, move |settings| {
            if settings.preferred_translation_language == language {
                return false;
            }
            settings.preferred_translation_language = language;
            true
        });
    }

    pub(crate) fn set_lyrics_furigana(self: &Rc<Self>, enabled: bool) {
        self.update_lyrics_settings("lyrics furigana setting", true, |settings| {
            if settings.show_furigana == enabled {
                return false;
            }
            settings.show_furigana = enabled;
            true
        });
    }

    pub(crate) fn set_lyrics_romanization(self: &Rc<Self>, enabled: bool) {
        self.update_lyrics_settings("lyrics Romaji setting", true, |settings| {
            if settings.show_romanization == enabled {
                return false;
            }
            settings.show_romanization = enabled;
            true
        });
    }

    pub(crate) fn set_lyrics_word_highlighting(self: &Rc<Self>, enabled: bool) {
        self.update_lyrics_settings("lyrics karaoke setting", true, |settings| {
            if settings.word_by_word_highlighting == enabled {
                return false;
            }
            settings.word_by_word_highlighting = enabled;
            true
        });
    }

    fn update_lyrics_settings(
        self: &Rc<Self>,
        warning_action: &'static str,
        rerender_lyrics: bool,
        update: impl FnOnce(&mut lyrics::Settings) -> bool,
    ) -> bool {
        let updated = self
            .update_app_settings(warning_action, |settings| update(&mut settings.lyrics))
            .is_some();
        if !updated {
            return false;
        }
        if rerender_lyrics {
            self.render_lyrics_presentation();
        }
        true
    }
}
