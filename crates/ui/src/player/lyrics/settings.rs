use std::{cell::RefCell, rc::Rc};

use adw::prelude::*;
use localization::{
    default_language_preference, language_option_index, language_options, msgid, tr,
};
use lyrics::ExternalLyricsProvider;

use crate::preferences::dialogs::popup::present_light_dismiss_dialog;
use crate::shell::Shell;

use super::{lyrics_popup_content_height, lyrics_popup_content_width};

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
    if let Some(settings_dialog) = lyrics.settings_dialog.upgrade() {
        settings_dialog.present(Some(&shell.chrome.window));
        return;
    }
    drop(lyrics);

    let (dialog, page) = build_lyrics_settings(shell);
    dialog.add(&page);
    if let Some(lyrics) = shell.selected_lyrics() {
        lyrics.settings_dialog.set(Some(&dialog));
    }
    present_light_dismiss_dialog(&dialog, &shell.chrome.window);
}

fn build_lyrics_settings(shell: &Rc<Shell>) -> (adw::PreferencesDialog, adw::PreferencesPage) {
    let settings = shell.settings.current.borrow().lyrics.clone();
    let resource = crate::ui_resource::LYRICS_SETTINGS_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    crate::ui_resource::objects!(builder, resource, {
        dialog: adw::PreferencesDialog,
        page: adw::PreferencesPage,
        sources: adw::PreferencesGroup,
        external: adw::SwitchRow,
        prefer_server: adw::SwitchRow,
        save_to_source: adw::SwitchRow,
        save_automatically: adw::ActionRow,
        automatic_toggle: gtk::Switch,
        storage: adw::ActionRow,
        embed: gtk::ToggleButton,
        sidecar: gtk::ToggleButton,
        translations: adw::SwitchRow,
        language_row: adw::ComboRow,
        furigana: adw::SwitchRow,
        romanization: adw::SwitchRow,
        karaoke: adw::SwitchRow,
        theme_accent: gtk::Box,
        color_button: gtk::Button,
        font: adw::ComboRow,
        size: adw::SpinRow,
        size_adjustment: gtk::Adjustment,
    });

    dialog.set_content_width(lyrics_popup_content_width());
    dialog.set_content_height(lyrics_popup_content_height(shell.chrome.window.height()));
    external.set_active(settings.external_lyrics_enabled);
    prefer_server.set_active(settings.prefer_server_lyrics);
    prefer_server.set_sensitive(settings.external_lyrics_enabled);
    let prefer_server_shell = Rc::clone(shell);
    prefer_server.connect_active_notify(move |row| {
        prefer_server_shell.set_lyrics_setting(
            "lyrics search setting",
            false,
            row.is_active(),
            |settings| &mut settings.prefer_server_lyrics,
        );
    });

    save_to_source.set_active(settings.save_lyrics_to_source);
    automatic_toggle.set_active(settings.save_lyrics_automatically);
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
    embed.set_active(!settings.save_lyrics_as_sidecar);
    sidecar.set_active(settings.save_lyrics_as_sidecar);
    storage.set_visible(
        settings.save_lyrics_to_source && selected_source_uses_local_lyrics_storage(shell),
    );
    let storage_shell = Rc::clone(shell);
    sidecar.connect_toggled(move |button| {
        if storage_shell.set_lyrics_setting(
            "lyrics storage setting",
            false,
            button.is_active(),
            |settings| &mut settings.save_lyrics_as_sidecar,
        ) {
            storage_shell.products.lyrics.refresh_write_access();
        }
    });

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
        if !external_shell.set_lyrics_setting(
            "lyrics setting",
            false,
            row.is_active(),
            |settings| &mut settings.external_lyrics_enabled,
        ) {
            return;
        }
        external_prefer_server.set_sensitive(row.is_active());
        external_save_automatically.set_sensitive(row.is_active());
        let Some(sources) = external_sources.upgrade() else {
            return;
        };
        populate_provider_rows(&external_shell, &sources, &external_provider_rows);
    });
    translations.set_active(settings.prefer_translations);

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
    language_row.set_model(Some(&language_model));
    language_row.set_selected(language_option_index(
        translation_languages.as_ref(),
        &settings.preferred_translation_language,
    ));
    language_row.set_sensitive(settings.prefer_translations);
    let language_shell = Rc::clone(shell);
    let translation_languages_for_row = Rc::clone(&translation_languages);
    language_row.connect_selected_notify(move |row| {
        let Some(language) = translation_languages_for_row.get(row.selected() as usize) else {
            return;
        };
        language_shell.set_lyrics_setting(
            "lyrics translation language",
            false,
            language.id.clone(),
            |settings| &mut settings.preferred_translation_language,
        );
    });
    let translations_shell = Rc::clone(shell);
    let translations_language_row = language_row.clone();
    translations.connect_active_notify(move |row| {
        if translations_shell.set_lyrics_setting(
            "lyrics translation setting",
            false,
            row.is_active(),
            |settings| &mut settings.prefer_translations,
        ) {
            translations_language_row.set_sensitive(row.is_active());
        }
    });

    furigana.set_active(settings.show_furigana);
    let furigana_shell = Rc::clone(shell);
    furigana.connect_active_notify(move |row| {
        furigana_shell.set_lyrics_setting(
            "lyrics furigana setting",
            true,
            row.is_active(),
            |settings| &mut settings.show_furigana,
        );
    });

    romanization.set_active(settings.show_romanization);
    let romanization_shell = Rc::clone(shell);
    romanization.connect_active_notify(move |row| {
        romanization_shell.set_lyrics_setting(
            "lyrics Romaji setting",
            true,
            row.is_active(),
            |settings| &mut settings.show_romanization,
        );
    });
    karaoke.set_active(settings.karaoke_mode);
    let karaoke_shell = Rc::clone(shell);
    karaoke.connect_active_notify(move |row| {
        karaoke_shell.set_lyrics_setting(
            "lyrics karaoke setting",
            true,
            row.is_active(),
            |settings| &mut settings.karaoke_mode,
        );
    });
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
    let font_families = Rc::new(system_font_families(&shell.chrome.window));
    let mut font_titles = vec![tr("Default")];
    font_titles.extend(font_families.iter().cloned());
    let font_title_refs = font_titles.iter().map(String::as_str).collect::<Vec<_>>();
    font.set_model(Some(&gtk::StringList::new(&font_title_refs)));
    font.set_selected(
        settings
            .lyrics_font_family
            .as_ref()
            .and_then(|selected| font_families.iter().position(|family| family == selected))
            .map_or(0, |index| index as u32 + 1),
    );
    let font_shell = Rc::clone(shell);
    let selected_families = Rc::clone(&font_families);
    font.connect_selected_notify(move |row| {
        let family = row
            .selected()
            .checked_sub(1)
            .and_then(|index| selected_families.get(index as usize))
            .cloned();
        if font_shell.set_lyrics_setting("lyrics font family", false, family, |settings| {
            &mut settings.lyrics_font_family
        }) {
            font_shell
                .appearance
                .apply(&font_shell.settings.current.borrow());
        }
    });
    size_adjustment.set_value(f64::from(settings.lyrics_font_size.unwrap_or(19)));
    let size_shell = Rc::clone(shell);
    size.connect_value_notify(move |row| {
        let size = row.value().round() as u16;
        if size_shell.set_lyrics_setting(
            "lyrics font size",
            false,
            (size != 19).then_some(size),
            |settings| &mut settings.lyrics_font_size,
        ) {
            size_shell
                .appearance
                .apply(&size_shell.settings.current.borrow());
        }
    });
    (dialog, page)
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
                    && selected_source_has_local_access(shell)
        })
}

fn selected_source_has_local_access(shell: &Shell) -> bool {
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
        .is_some_and(|summary| summary.access.is_some())
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
    let resource = crate::ui_resource::LYRICS_COLOR_CHOOSER_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    crate::ui_resource::objects!(builder, resource, {
        dialog: adw::Window,
        cancel: gtk::Button,
        default: gtk::Button,
        select: gtk::Button,
        body: gtk::Box,
    });
    let chooser = Rc::new(RefCell::new(lyrics_color_chooser(&initial)));
    body.append(&*chooser.borrow());
    dialog.set_transient_for(Some(parent));

    let close = dialog.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(close) = close.upgrade() {
            close.close();
        }
    });
    let default_chooser = Rc::clone(&chooser);
    let default_body = body.clone();
    default.connect_clicked(move |_| {
        let replacement = lyrics_color_chooser(&default_probe.color());
        default_body.remove(&*default_chooser.borrow());
        default_body.append(&replacement);
        default_chooser.replace(replacement);
    });
    let shell = Rc::downgrade(shell);
    let close = dialog.downgrade();
    select.connect_clicked(move |_| {
        let Some(shell) = shell.upgrade() else {
            return;
        };
        let color = chooser.borrow().rgba();
        if shell.set_lyrics_setting(
            "lyrics highlight color",
            true,
            Some(rgba_hex(&color)),
            |settings| &mut settings.lyrics_highlight_color,
        ) {
            shell.appearance.apply(&shell.settings.current.borrow());
        }
        if let Some(close) = close.upgrade() {
            close.close();
        }
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
        self.set_lyrics_setting(
            "automatic lyrics save setting",
            false,
            enabled,
            |settings| &mut settings.save_lyrics_automatically,
        )
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

    fn set_lyrics_setting<T: PartialEq>(
        self: &Rc<Self>,
        warning_action: &'static str,
        rerender_lyrics: bool,
        value: T,
        field: impl FnOnce(&mut lyrics::Settings) -> &mut T,
    ) -> bool {
        let updated = self
            .set_app_setting(warning_action, value, |settings| {
                field(&mut settings.lyrics)
            })
            .is_some();
        if updated && rerender_lyrics {
            self.render_lyrics_presentation();
        }
        updated
    }
}
