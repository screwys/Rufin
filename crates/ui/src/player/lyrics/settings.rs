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

    let page = build_lyrics_settings(shell);
    let dialog = adw::PreferencesDialog::builder()
        .title(tr("Lyrics settings"))
        .search_enabled(false)
        .content_width(500)
        .content_height(600)
        .build();
    dialog.add(&page);
    let settings_dialog = LyricsSettingsDialog {
        dialog: dialog.clone(),
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

fn build_lyrics_settings(shell: &Rc<Shell>) -> adw::PreferencesPage {
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

    let save_to_source = switch_row(
        msgid("Saves lyrics to your source"),
        settings.save_lyrics_to_source,
    );
    save_to_source.set_subtitle(&tr(msgid(
        "Save action saves the lyrics directly to your source",
    )));
    sources.add(&save_to_source);

    let save_automatically = adw::ActionRow::builder()
        .title(tr(msgid("Save lyrics automatically")))
        .build();
    let automatic_toggle = gtk::Switch::builder()
        .active(settings.save_lyrics_automatically)
        .valign(gtk::Align::Center)
        .build();
    automatic_toggle.set_can_target(false);
    automatic_toggle.set_focusable(false);
    save_automatically.add_suffix(&automatic_toggle);
    save_automatically.set_activatable(true);
    save_automatically.set_visible(settings.save_lyrics_to_source);
    save_automatically.set_sensitive(settings.external_lyrics_enabled);
    let automatic_shell = Rc::clone(shell);
    let automatic_switch = automatic_toggle.clone();
    save_automatically.connect_activated(move |_| {
        if automatic_shell
            .settings
            .current
            .borrow()
            .lyrics
            .save_lyrics_automatically
        {
            automatic_shell.set_save_lyrics_automatically(false);
            automatic_switch.set_active(false);
            automatic_switch.set_state(false);
            return;
        }
        let confirm = adw::AlertDialog::builder()
            .heading(tr("Save lyrics automatically"))
            .body(tr(msgid("This will overwrite your lyrics file with what Rufin fetched, it may be a better experience to use a dedicated program or plugin to fetch lyrics instead.")))
            .build();
        confirm.add_response("cancel", &tr("Cancel"));
        confirm.add_response("confirm", &tr("Confirm"));
        confirm.set_default_response(Some("cancel"));
        confirm.set_close_response("cancel");
        let shell = Rc::clone(&automatic_shell);
        let toggle = automatic_switch.clone();
        confirm.choose(
            Some(&automatic_shell.chrome.window),
            None::<&gtk::gio::Cancellable>,
            move |response| {
                if response.as_str() != "confirm" {
                    return;
                }
                shell.set_save_lyrics_automatically(true);
                toggle.set_active(true);
                toggle.set_state(true);
            },
        );
    });
    sources.add(&save_automatically);

    let storage = adw::ActionRow::builder()
        .title(tr(msgid("Lyrics storage")))
        .build();
    let storage_choices = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    storage_choices.add_css_class("linked");
    storage_choices.add_css_class("preference-selection-buttons");
    storage_choices.set_valign(gtk::Align::Center);
    let embed = gtk::ToggleButton::with_label(&tr(msgid("Embed in track")));
    let sidecar = gtk::ToggleButton::with_label(&tr(msgid("separate .lrc file")));
    embed.add_css_class("preference-selection-button");
    sidecar.add_css_class("preference-selection-button");
    sidecar.set_group(Some(&embed));
    embed.set_active(!settings.save_lyrics_as_sidecar);
    sidecar.set_active(settings.save_lyrics_as_sidecar);
    storage_choices.append(&embed);
    storage_choices.append(&sidecar);
    storage.add_suffix(&storage_choices);
    storage.set_visible(
        settings.save_lyrics_to_source && selected_source_uses_local_lyrics_storage(shell),
    );
    let storage_shell = Rc::clone(shell);
    sidecar.connect_toggled(move |button| {
        storage_shell.set_save_lyrics_as_sidecar(button.is_active());
    });
    sources.add(&storage);

    let save_shell = Rc::clone(shell);
    let automatic_row = save_automatically.clone();
    let automatic_switch = automatic_toggle.clone();
    let storage_row = storage.clone();
    save_to_source.connect_active_notify(move |row| {
        let enabled = row.is_active();
        if !save_shell.set_save_lyrics_to_source(enabled) {
            return;
        }
        automatic_row.set_visible(enabled);
        if !enabled {
            automatic_switch.set_active(false);
        }
        storage_row.set_visible(enabled && selected_source_uses_local_lyrics_storage(&save_shell));
    });

    let provider_rows = Rc::new(RefCell::new(Vec::new()));
    populate_provider_rows(shell, &sources, &provider_rows);
    let external_shell = Rc::clone(shell);
    let external_sources = sources.downgrade();
    let external_provider_rows = Rc::clone(&provider_rows);
    let external_prefer_server = prefer_server.clone();
    let external_save_automatically = save_automatically.clone();
    external.connect_active_notify(move |row| {
        if !external_shell.set_external_lyrics_enabled(row.is_active()) {
            return;
        }
        external_prefer_server.set_sensitive(row.is_active());
        external_save_automatically.set_sensitive(row.is_active());
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
        .subtitle(tr(msgid(
            "Uses available karaoke lyrics or fetches them from the internet",
        )))
        .active(settings.karaoke_mode)
        .build();
    let karaoke_shell = Rc::clone(shell);
    karaoke.connect_active_notify(move |row| {
        karaoke_shell.set_lyrics_karaoke_mode(row.is_active());
    });
    playback.add(&karaoke);
    let highlight_color = adw::ActionRow::builder()
        .title(tr("Karaoke highlight color"))
        .subtitle(tr("Color for the active word during playback"))
        .build();
    let theme_accent = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    theme_accent.add_css_class("lyrics-theme-accent-probe");
    theme_accent.set_opacity(0.0);
    theme_accent.set_can_target(false);
    let preview = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    preview.add_css_class("lyrics-highlight-swatch");
    let color_button = gtk::Button::builder()
        .child(&preview)
        .css_classes(["flat"])
        .width_request(32)
        .height_request(32)
        .valign(gtk::Align::Center)
        .build();
    let color_shell = Rc::clone(shell);
    let default_accent = theme_accent.clone();
    color_button.connect_clicked(move |_| {
        let default_color = default_accent.color();
        let initial = color_shell
            .settings
            .current
            .borrow()
            .lyrics
            .lyrics_highlight_color
            .as_deref()
            .and_then(|color| gtk::gdk::RGBA::parse(color).ok())
            .unwrap_or(default_color);
        present_lyrics_color_chooser(
            &color_shell,
            &color_shell.chrome.window,
            initial,
            default_accent.clone().upcast(),
        );
    });
    highlight_color.add_suffix(&theme_accent);
    highlight_color.add_suffix(&color_button);
    highlight_color.set_activatable_widget(Some(&color_button));
    playback.add(&highlight_color);
    page.add(&playback);

    let typography = adw::PreferencesGroup::builder()
        .title(tr("Typography"))
        .build();
    let font_families = Rc::new(system_font_families(&shell.chrome.window));
    let mut font_titles = vec![tr("Default")];
    font_titles.extend(font_families.iter().cloned());
    let font_title_refs = font_titles.iter().map(String::as_str).collect::<Vec<_>>();
    let font = adw::ComboRow::builder()
        .title(tr("Font family"))
        .enable_search(true)
        .model(&gtk::StringList::new(&font_title_refs))
        .selected(
            settings
                .lyrics_font_family
                .as_ref()
                .and_then(|selected| font_families.iter().position(|family| family == selected))
                .map_or(0, |index| index as u32 + 1),
        )
        .build();
    let font_shell = Rc::clone(shell);
    let selected_families = Rc::clone(&font_families);
    font.connect_selected_notify(move |row| {
        let family = row
            .selected()
            .checked_sub(1)
            .and_then(|index| selected_families.get(index as usize))
            .cloned();
        font_shell.set_lyrics_font_family(family);
    });
    typography.add(&font);
    let size = adw::SpinRow::builder()
        .title(tr("Font size (px)"))
        .adjustment(
            &gtk::Adjustment::builder()
                .lower(12.0)
                .upper(28.0)
                .step_increment(1.0)
                .value(f64::from(settings.lyrics_font_size.unwrap_or(19)))
                .build(),
        )
        .digits(0)
        .build();
    let size_shell = Rc::clone(shell);
    size.connect_value_notify(move |row| {
        let size = row.value().round() as u16;
        size_shell.set_lyrics_font_size((size != 19).then_some(size));
    });
    typography.add(&size);
    page.add(&typography);
    page
}

fn selected_source_uses_local_lyrics_storage(shell: &Shell) -> bool {
    let Some(source_id) = shell
        .selected_library()
        .as_deref()
        .map(|selected| selected.artwork.source_id.clone())
    else {
        return false;
    };
    shell
        .source
        .configured
        .borrow()
        .sources
        .iter()
        .find(|source| source.id == source_id)
        .is_some_and(|source| {
            source.kind == "local"
                || matches!(source.kind.as_str(), "navidrome" | "subsonic")
                    && selected_source_has_local_mapping(shell)
        })
}

fn selected_source_has_local_mapping(shell: &Shell) -> bool {
    let Some(source_id) = shell
        .selected_library()
        .as_deref()
        .map(|selected| selected.artwork.source_id.clone())
    else {
        return false;
    };
    shell
        .source
        .configured
        .borrow()
        .local_access
        .iter()
        .find(|summary| summary.source_id == source_id)
        .is_some_and(|summary| {
            summary.status.direct_match_count
                + summary.status.prefix_match_count
                + summary.status.metadata_match_count
                > 0
        })
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

fn system_font_families(widget: &impl IsA<gtk::Widget>) -> Vec<String> {
    let Some(font_map) = widget.pango_context().font_map() else {
        return Vec::new();
    };
    let mut families = font_map
        .list_families()
        .into_iter()
        .map(|family| family.name().to_string())
        .collect::<Vec<_>>();
    families.sort_unstable();
    families.dedup();
    families
}

#[allow(deprecated)]
fn present_lyrics_color_chooser(
    shell: &Rc<Shell>,
    parent: &impl IsA<gtk::Window>,
    initial: gtk::gdk::RGBA,
    default_probe: gtk::Widget,
) {
    let chooser = Rc::new(RefCell::new(lyrics_color_chooser(&initial)));

    let cancel = gtk::Button::with_label(&tr("Cancel"));
    let default = gtk::Button::with_label(&tr("Default"));
    let select = gtk::Button::with_label(&tr("Save"));
    select.add_css_class("suggested-action");
    let header = adw::HeaderBar::new();
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    header.set_title_widget(Some(&adw::WindowTitle::new(
        &tr("Karaoke highlight color"),
        "",
    )));
    header.pack_start(&cancel);
    header.pack_end(&select);
    header.pack_end(&default);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.set_vexpand(true);
    body.append(&*chooser.borrow());
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&body));
    let dialog = adw::Window::builder()
        .title(tr("Karaoke highlight color"))
        .default_width(520)
        .default_height(420)
        .modal(true)
        .resizable(false)
        .transient_for(parent)
        .content(&toolbar)
        .build();

    let close = dialog.clone();
    cancel.connect_clicked(move |_| close.close());
    let default_chooser = Rc::clone(&chooser);
    let default_body = body.clone();
    default.connect_clicked(move |_| {
        let replacement = lyrics_color_chooser(&default_probe.color());
        default_body.remove(&*default_chooser.borrow());
        default_body.append(&replacement);
        default_chooser.replace(replacement);
    });
    let shell = Rc::clone(shell);
    let close = dialog.clone();
    select.connect_clicked(move |_| {
        let color = chooser.borrow().rgba();
        shell.set_lyrics_highlight_color(Some(rgba_hex(&color)));
        close.close();
    });
    dialog.present();
}

#[allow(deprecated)]
fn lyrics_color_chooser(color: &gtk::gdk::RGBA) -> gtk::ColorChooserWidget {
    let chooser = gtk::ColorChooserWidget::new();
    chooser.set_show_editor(true);
    chooser.set_use_alpha(false);
    chooser.set_rgba(color);
    chooser.set_hexpand(true);
    chooser.set_vexpand(true);
    chooser
}

fn rgba_hex(color: &gtk::gdk::RGBA) -> String {
    format!(
        "#{:02x}{:02x}{:02x}",
        (color.red() * 255.0).round() as u8,
        (color.green() * 255.0).round() as u8,
        (color.blue() * 255.0).round() as u8,
    )
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

    pub(crate) fn set_save_lyrics_to_source(self: &Rc<Self>, enabled: bool) -> bool {
        self.update_lyrics_settings("save lyrics destination setting", false, |settings| {
            if settings.save_lyrics_to_source == enabled {
                return false;
            }
            settings.save_lyrics_to_source = enabled;
            if !enabled {
                settings.save_lyrics_automatically = false;
            }
            true
        })
    }

    pub(crate) fn set_save_lyrics_automatically(self: &Rc<Self>, enabled: bool) -> bool {
        self.update_lyrics_settings("automatic lyrics save setting", false, |settings| {
            if settings.save_lyrics_automatically == enabled {
                return false;
            }
            settings.save_lyrics_automatically = enabled;
            true
        })
    }

    pub(crate) fn set_save_lyrics_as_sidecar(self: &Rc<Self>, sidecar: bool) -> bool {
        let changed = self.update_lyrics_settings("lyrics storage setting", false, |settings| {
            if settings.save_lyrics_as_sidecar == sidecar {
                return false;
            }
            settings.save_lyrics_as_sidecar = sidecar;
            true
        });
        if changed {
            self.products.lyrics.refresh_write_access();
        }
        changed
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

    pub(crate) fn set_lyrics_karaoke_mode(self: &Rc<Self>, enabled: bool) {
        self.update_lyrics_settings("lyrics karaoke setting", true, |settings| {
            if settings.karaoke_mode == enabled {
                return false;
            }
            settings.karaoke_mode = enabled;
            true
        });
    }

    pub(crate) fn set_lyrics_highlight_color(self: &Rc<Self>, color: Option<String>) {
        if self.update_lyrics_settings("lyrics highlight color", true, |settings| {
            if settings.lyrics_highlight_color == color {
                return false;
            }
            settings.lyrics_highlight_color = color;
            true
        }) {
            self.apply_lyrics_appearance();
        }
    }

    pub(crate) fn set_lyrics_font_family(self: &Rc<Self>, family: Option<String>) {
        if self.update_lyrics_settings("lyrics font family", false, |settings| {
            if settings.lyrics_font_family == family {
                return false;
            }
            settings.lyrics_font_family = family;
            true
        }) {
            self.apply_lyrics_appearance();
        }
    }

    pub(crate) fn set_lyrics_font_size(self: &Rc<Self>, size: Option<u16>) {
        if self.update_lyrics_settings("lyrics font size", false, |settings| {
            if settings.lyrics_font_size == size {
                return false;
            }
            settings.lyrics_font_size = size;
            true
        }) {
            self.apply_lyrics_appearance();
        }
    }

    fn apply_lyrics_appearance(&self) {
        self.appearance.apply(&self.settings.current.borrow());
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
