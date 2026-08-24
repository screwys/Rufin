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

    let auto_embed = adw::SwitchRow::builder()
        .title(tr("Auto-embed lyrics"))
        .subtitle(tr(
            "Write fetched lyrics into the audio file tags for offline playback",
        ))
        .active(settings.auto_embed_lyrics)
        .build();
    auto_embed.set_sensitive(settings.external_lyrics_enabled);
    let auto_embed_shell = Rc::clone(shell);
    auto_embed.connect_active_notify(move |row| {
        auto_embed_shell.set_auto_embed_lyrics(row.is_active());
    });
    sources.add(&auto_embed);

    let provider_rows = Rc::new(RefCell::new(Vec::new()));
    populate_provider_rows(shell, &sources, &provider_rows);
    let external_shell = Rc::clone(shell);
    let external_sources = sources.downgrade();
    let external_provider_rows = Rc::clone(&provider_rows);
    let external_prefer_server = prefer_server.clone();
    let external_auto_embed = auto_embed.clone();
    external.connect_active_notify(move |row| {
        if !external_shell.set_external_lyrics_enabled(row.is_active()) {
            return;
        }
        external_prefer_server.set_sensitive(row.is_active());
        external_auto_embed.set_sensitive(row.is_active());
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
        .title(tr("Karaoke Playback"))
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

    let highlight_color_row = adw::ActionRow::builder()
        .title(tr("Karaoke highlight color"))
        .subtitle(tr("Color for the active word during playback"))
        .build();
    let initial_rgba = gtk::gdk::RGBA::parse(settings.lyrics_highlight_color.as_str())
        .unwrap_or(gtk::gdk::RGBA::new(0.91, 0.59, 0.17, 1.0));
    let color_button = gtk::ColorDialogButton::builder()
        .rgba(&initial_rgba)
        .css_classes(["lyrics-color-button"])
        .build();
    let color_dialog = gtk::ColorDialog::builder()
        .title(tr("Karaoke highlight color"))
        .modal(true)
        .build();
    color_button.set_dialog(&color_dialog);
    let color_shell = Rc::clone(shell);
    color_button.connect_rgba_notify(move |btn| {
        let rgba = btn.rgba();
        let r = (rgba.red() * 255.0) as u8;
        let g = (rgba.green() * 255.0) as u8;
        let b = (rgba.blue() * 255.0) as u8;
        let hex = format!("#{r:02x}{g:02x}{b:02x}");
        color_shell.set_lyrics_highlight_color(hex);
    });
    highlight_color_row.add_suffix(&color_button);
    highlight_color_row.set_activatable_widget(Some(&color_button));
    playback.add(&highlight_color_row);
    page.add(&playback);

    let typography = adw::PreferencesGroup::builder()
        .title(tr("Typography"))
        .build();

    let use_custom_font = adw::SwitchRow::builder()
        .title(tr("Use custom font"))
        .active(settings.lyrics_use_custom_font)
        .build();
    typography.add(&use_custom_font);

    let font_names = collect_system_font_names(&shell.chrome.window);
    let font_titles: Vec<&str> = font_names.iter().map(|s| s.as_str()).collect();
    let font_model = gtk::StringList::new(&font_titles);
    let current_font_index = if settings.lyrics_font_family.is_empty() {
        0
    } else {
        font_names
            .iter()
            .position(|n| n == &settings.lyrics_font_family)
            .unwrap_or(0) as u32
    };
    let font_row = adw::ComboRow::builder()
        .title(tr("Font family"))
        .model(&font_model)
        .selected(current_font_index)
        .visible(settings.lyrics_use_custom_font)
        .build();
    let font_shell = Rc::clone(shell);
    let font_names_for_row = font_names;
    font_row.connect_selected_notify(move |row| {
        let family = font_names_for_row
            .get(row.selected() as usize)
            .cloned()
            .unwrap_or_default();
        font_shell.set_lyrics_font_family(family);
    });
    typography.add(&font_row);

    let size_adjustment = gtk::Adjustment::builder()
        .lower(12.0)
        .upper(28.0)
        .step_increment(1.0)
        .page_increment(1.0)
        .value(settings.lyrics_font_size.unwrap_or(19) as f64)
        .build();
    let size_row = adw::SpinRow::builder()
        .title(tr("Font size (px)"))
        .adjustment(&size_adjustment)
        .digits(0)
        .visible(settings.lyrics_use_custom_font)
        .build();
    let has_custom_size = settings.lyrics_font_size.is_some();
    let default_label = tr("Default");
    let size_suffix_label = gtk::Label::builder()
        .label(if has_custom_size { "" } else { &default_label })
        .css_classes(vec!["dim-label".to_string()])
        .build();
    size_row.add_suffix(&size_suffix_label);
    let size_shell = Rc::clone(shell);
    size_row.connect_value_notify(move |row| {
        let raw = row.value() as u16;
        let size = if raw == 19 { None } else { Some(raw) };
        size_shell.set_lyrics_font_size(size);
        let label = if size.is_some() {
            String::new()
        } else {
            tr("Default")
        };
        size_suffix_label.set_label(&label);
    });
    typography.add(&size_row);
    page.add(&typography);

    let toggle_font_row = font_row.clone();
    let toggle_size_row = size_row.clone();
    let use_custom_shell = Rc::clone(shell);
    use_custom_font.connect_active_notify(move |row| {
        let active = row.is_active();
        use_custom_shell.set_lyrics_use_custom_font(active);
        toggle_font_row.set_visible(active);
        toggle_size_row.set_visible(active);
    });

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

fn collect_system_font_names(window: &impl IsA<gtk::Widget>) -> Vec<String> {
    let font_map = match window.pango_context().font_map() {
        Some(map) => map,
        None => return vec![],
    };
    let mut families: Vec<String> = font_map
        .list_families()
        .into_iter()
        .map(|f| f.name().to_string())
        .collect();
    families.sort();
    families.dedup();
    families
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

    pub(crate) fn set_auto_embed_lyrics(self: &Rc<Self>, enabled: bool) {
        self.update_lyrics_settings("lyrics auto embed setting", false, |settings| {
            if settings.auto_embed_lyrics == enabled {
                return false;
            }
            settings.auto_embed_lyrics = enabled;
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

    pub(crate) fn set_lyrics_use_custom_font(self: &Rc<Self>, enabled: bool) {
        self.update_lyrics_settings("lyrics custom font", true, |settings| {
            if settings.lyrics_use_custom_font == enabled {
                return false;
            }
            settings.lyrics_use_custom_font = enabled;
            true
        });
    }

    pub(crate) fn set_lyrics_font_family(self: &Rc<Self>, family: String) {
        self.update_lyrics_settings("lyrics font family", true, |settings| {
            if settings.lyrics_font_family == family {
                return false;
            }
            settings.lyrics_font_family = family;
            true
        });
    }

    pub(crate) fn set_lyrics_font_size(self: &Rc<Self>, size: Option<u16>) {
        self.update_lyrics_settings("lyrics font size", true, |settings| {
            if settings.lyrics_font_size == size {
                return false;
            }
            settings.lyrics_font_size = size;
            true
        });
    }

    pub(crate) fn set_lyrics_highlight_color(self: &Rc<Self>, color: String) {
        self.update_lyrics_settings("lyrics highlight color", true, |settings| {
            if settings.lyrics_highlight_color == color {
                return false;
            }
            settings.lyrics_highlight_color = color;
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
        self.appearance.apply(&self.settings.current.borrow());
        if rerender_lyrics {
            self.render_lyrics_presentation();
        }
        true
    }
}
