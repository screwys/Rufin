use crate::{AccentPreference, Preferences, ThemePreference, appearance::ApplicationAppearance};
use adw::prelude::*;
use gtk::glib;
use rufin_core::themes::Accent;
use std::rc::Rc;

pub(super) fn page(shell: &Rc<Preferences>) -> adw::PreferencesPage {
    let resource = crate::ui_resource::THEME_PREFERENCES_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    bind(shell, &builder, resource);
    gtk_widgets::ui_resource::object(&builder, resource, "theme_page")
}

pub(super) fn bind(shell: &Rc<Preferences>, builder: &gtk::Builder, resource: &str) {
    gtk_widgets::objects!(builder, resource, {
        theme_grid: gtk::FlowBox, theme_errors: gtk::Label,
        themes_folder: gtk::Button, themes_reload: gtk::Button,
        standard_accent: gtk::SingleSelection, standard_themes: gtk::StringList,
    });
    standard_accent.set_selected(
        AccentPreference::ALL
            .iter()
            .position(|accent| {
                *accent == shell.settings.current.borrow().appearance.accent_preference
            })
            .unwrap_or_default() as u32,
    );
    let weak = Rc::downgrade(shell);
    standard_accent.connect_selected_notify(move |dropdown| {
        let Some(shell) = weak.upgrade() else { return };
        let Some(accent) = AccentPreference::ALL.get(dropdown.selected() as usize) else {
            return;
        };
        if let Some(settings) =
            shell
                .settings
                .set_app_setting("accent setting", *accent, |settings| {
                    &mut settings.appearance.accent_preference
                })
        {
            shell.appearance.apply(&settings);
            shell.publish_controller_appearance();
        }
    });
    let weak = Rc::downgrade(shell);
    let reload_button = themes_reload.downgrade();
    themes_folder.connect_clicked(move |_| {
        if let Some(shell) = weak.upgrade() {
            let folder = ApplicationAppearance::folder();
            let created = match rufin_core::themes::create_folder(&folder) {
                Ok(created) => created,
                Err(error) => {
                    shell
                        .control_feedback
                        .show_feedback_toast(localization::tr_with(
                            "Could not open folder: {error}",
                            &[("error", &error.to_string())],
                        ));
                    return;
                }
            };
            if created && let Some(button) = reload_button.upgrade() {
                button.emit_clicked();
            }
            super::open_folder(&shell, gtk::gio::File::for_path(folder));
        }
    });
    theme_errors.set_text(&shell.appearance.errors.borrow().join("\n"));
    theme_errors.set_visible(!shell.appearance.errors.borrow().is_empty());
    populate(shell, &theme_grid, &standard_accent, &standard_themes);
    let weak = Rc::downgrade(shell);
    let grid = theme_grid.downgrade();
    let errors = theme_errors.downgrade();
    let dropdown = standard_accent.downgrade();
    let reload = move || {
        let (Some(shell), Some(grid), Some(errors), Some(dropdown)) = (
            weak.upgrade(),
            grid.upgrade(),
            errors.upgrade(),
            dropdown.upgrade(),
        ) else {
            return;
        };
        let messages = shell.appearance.reload();
        errors.set_text(&messages.join("\n"));
        errors.set_visible(!messages.is_empty());
        let settings = shell.settings.current.borrow().clone();
        shell.appearance.apply(&settings);
        shell.publish_controller_appearance();
        populate(&shell, &grid, &dropdown, &standard_themes);
    };
    themes_reload.connect_clicked(move |_| reload());
}

fn populate(
    shell: &Rc<Preferences>,
    grid: &gtk::FlowBox,
    standard_accent: &gtk::SingleSelection,
    names: &gtk::StringList,
) {
    grid.remove_all();
    let standard_colors: Vec<_> = AccentPreference::ALL
        .iter()
        .enumerate()
        .map(|(index, preference)| Accent {
            name: standard_accent
                .item(index as u32)
                .and_downcast::<gtk::StringObject>()
                .unwrap()
                .string()
                .into(),
            color: preference.color().map(String::from).unwrap_or_else(|| {
                shell
                    .appearance
                    .system_style
                    .accent_color_rgba()
                    .to_string()
            }),
            foreground: "#ffffff".into(),
        })
        .collect();
    let themes = shell.appearance.themes.borrow().clone();
    let choices = std::iter::once(None).chain(themes.iter().map(Some));
    let mut group = None;
    for (index, theme) in choices.enumerate() {
        let resource = crate::ui_resource::THEME_TILE_RESOURCE;
        let builder = gtk_widgets::ui_resource::builder(resource);
        gtk_widgets::objects!(builder, resource, {
            tile: gtk::Box, select: gtk::ToggleButton, preview: gtk::DrawingArea,
            name: gtk::Label, accent: gtk::DropDown, edit: gtk::Button,
        });
        if let Some(path) = theme.and_then(|theme| theme.path.as_ref()) {
            edit.set_visible(true);
            let file = gtk::gio::File::for_path(path);
            let weak = Rc::downgrade(shell);
            edit.connect_clicked(move |_| {
                let Some(shell) = weak.upgrade() else { return };
                let file = file.clone();
                gtk::glib::spawn_future_local(async move {
                    if let Err(error) = gtk::FileLauncher::new(Some(&file))
                        .launch_future(Some(&shell.window))
                        .await
                    {
                        if !error.matches(gtk::DialogError::Dismissed)
                            && !error.matches(gtk::DialogError::Cancelled)
                        {
                            shell
                                .control_feedback
                                .show_feedback_toast(localization::tr_with(
                                    "Could not open file: {error}",
                                    &[("error", &error.to_string())],
                                ));
                        }
                    }
                });
            });
        }
        let preference = match index {
            0 => ThemePreference::System,
            1 => ThemePreference::Light,
            2 => ThemePreference::Dark,
            _ => ThemePreference::Named(theme.unwrap().id.clone()),
        };
        let title = if index < 3 {
            names.string(index as u32).unwrap().to_string()
        } else {
            theme.unwrap().name.clone()
        };
        name.set_text(&title);
        select.set_tooltip_text(Some(&title));
        select.update_property(&[gtk::accessible::Property::Label(&title)]);
        select.set_group(group.as_ref());
        if group.is_none() {
            group = Some(select.clone());
        }
        select
            .set_active(shell.settings.current.borrow().appearance.theme_preference == preference);
        let weak = Rc::downgrade(shell);
        let selection = preference.clone();
        select.connect_toggled(move |button| {
            if !button.is_active() {
                return;
            }
            let Some(shell) = weak.upgrade() else { return };
            if let Some(settings) =
                shell
                    .settings
                    .set_app_setting("theme setting", selection.clone(), |settings| {
                        &mut settings.appearance.theme_preference
                    })
            {
                shell.appearance.apply(&settings);
                shell.publish_controller_appearance();
            }
        });

        grid.insert(&tile, -1);
        let weak = Rc::downgrade(shell);
        preview.set_draw_func(move |_, context, width, height| {
            let Some(shell) = weak.upgrade() else { return };
            let themes = shell.appearance.themes.borrow();
            let theme = &themes[if index == 0 {
                usize::from(shell.appearance.system_style.is_dark())
            } else {
                index - 1
            }];
            let settings = shell.settings.current.borrow();
            let base = &themes[usize::from(theme.mode == rufin_core::themes::Mode::Dark)];
            let color = |name: &str| {
                theme
                    .colors
                    .get(name)
                    .and_then(|value| gtk::gdk::RGBA::parse(value).ok())
                    .or_else(|| {
                        base.colors
                            .get(name)
                            .and_then(|value| gtk::gdk::RGBA::parse(value).ok())
                    })
                    .unwrap_or(gtk::gdk::RGBA::new(0.0, 0.0, 6.0 / 255.0, 0.8))
            };
            let accent = theme
                .accent(
                    settings
                        .appearance
                        .theme_accents
                        .get(&theme.id)
                        .map(String::as_str),
                )
                .and_then(|accent| gtk::gdk::RGBA::parse(&accent.color).ok())
                .or_else(|| {
                    settings
                        .appearance
                        .accent_preference
                        .color()
                        .and_then(|value| gtk::gdk::RGBA::parse(value).ok())
                })
                .unwrap_or_else(|| shell.appearance.system_style.accent_color_rgba());
            let foreground = color("view-fg-color");
            context.scale(f64::from(width) / 124.0, f64::from(height) / 96.0);
            let rect = |color: gtk::gdk::RGBA, alpha: f64, x, y, width, height| {
                context.set_source_rgba(
                    f64::from(color.red()),
                    f64::from(color.green()),
                    f64::from(color.blue()),
                    f64::from(color.alpha()) * alpha,
                );
                context.rectangle(x, y, width, height);
                let _ = context.fill();
            };
            rect(color("window-bg-color"), 1.0, 0.0, 0.0, 124.0, 96.0);
            rect(color("sidebar-bg-color"), 1.0, 3.0, 12.0, 23.0, 69.0);
            rect(color("view-bg-color"), 1.0, 29.0, 12.0, 62.0, 69.0);
            rect(
                color("secondary-sidebar-bg-color"),
                1.0,
                94.0,
                12.0,
                27.0,
                69.0,
            );
            rect(accent, 0.2, 32.0, 16.0, 56.0, 17.0);
            rect(foreground, 0.3, 48.0, 20.0, 33.0, 2.0);
            rect(foreground, 0.3, 48.0, 26.0, 24.0, 2.0);
            rect(foreground, 0.15, 35.0, 19.0, 10.0, 10.0);
            rect(foreground, 0.3, 32.0, 38.0, 20.0, 2.0);
            for row in 0..2 {
                for column in 0..3 {
                    rect(
                        foreground,
                        0.15,
                        32.0 + f64::from(column) * 19.0,
                        45.0 + f64::from(row) * 18.0,
                        16.0,
                        14.0,
                    );
                }
            }
            rect(foreground, 0.15, 42.0, 4.0, 40.0, 4.0);
            rect(accent, 1.0, 52.0, 86.0, 20.0, 4.0);
        });
        if theme.is_none_or(|theme| theme.accents.is_empty()) {
            let redraw = gtk::glib::closure_local!(
                #[watch]
                preview,
                move |_object: gtk::glib::Object, _property: gtk::glib::ParamSpec| {
                    preview.queue_draw();
                }
            );
            standard_accent.connect_closure("notify::selected", false, redraw.clone());
            shell.appearance.system_style.connect_closure(
                "notify::accent-color",
                false,
                redraw.clone(),
            );
            if theme.is_none() {
                shell
                    .appearance
                    .system_style
                    .connect_closure("notify::dark", false, redraw);
            }
        }
        accent.set_visible(index < 3 || theme.is_some_and(|theme| theme.accents.len() > 1));
        if index < 3 {
            accent.set_model(Some(standard_accent));
            standard_accent
                .bind_property("selected", &accent, "selected")
                .bidirectional()
                .sync_create()
                .build();
        }
        if let Some(theme) = theme.filter(|theme| theme.accents.len() > 1) {
            accent.set_model(Some(&gtk::StringList::new(
                &theme
                    .accents
                    .iter()
                    .map(|accent| accent.name.as_str())
                    .collect::<Vec<_>>(),
            )));
            let selected = theme
                .accent(
                    shell
                        .settings
                        .current
                        .borrow()
                        .appearance
                        .theme_accents
                        .get(&theme.id)
                        .map(String::as_str),
                )
                .unwrap();
            accent.set_selected(
                theme
                    .accents
                    .iter()
                    .position(|accent| accent.name == selected.name)
                    .unwrap() as u32,
            );
            let weak = Rc::downgrade(shell);
            let theme = theme.clone();
            let button = select.downgrade();
            let preview = preview.downgrade();
            accent.connect_selected_notify(move |dropdown| {
                let Some(shell) = weak.upgrade() else { return };
                let Some(accent) = theme.accents.get(dropdown.selected() as usize) else {
                    return;
                };
                if let Some(settings) =
                    shell
                        .settings
                        .update_app_settings("theme accent setting", |settings| {
                            let changed = settings.appearance.theme_preference != preference
                                || settings.appearance.theme_accents.get(&theme.id)
                                    != Some(&accent.name);
                            settings.appearance.theme_preference = preference.clone();
                            settings
                                .appearance
                                .theme_accents
                                .insert(theme.id.clone(), accent.name.clone());
                            changed
                        })
                {
                    shell.appearance.apply(&settings);
                    shell.publish_controller_appearance();
                    if let Some(preview) = preview.upgrade() {
                        preview.queue_draw();
                    }
                    if let Some(button) = button.upgrade() {
                        button.set_active(true);
                    }
                }
            });
        }
        if accent.is_visible() {
            let colors = if index < 3 {
                &standard_colors
            } else {
                &theme.unwrap().accents
            };
            accent.set_factory(Some(&accent_factory(colors, false)));
            accent.set_list_factory(Some(&accent_factory(colors, true)));
            let tooltip = |dropdown: &gtk::DropDown| {
                if let Some(value) = dropdown.selected_item().and_downcast::<gtk::StringObject>() {
                    dropdown.set_tooltip_text(Some(&value.string()));
                }
            };
            tooltip(&accent);
            accent.connect_selected_notify(tooltip);
        }
    }
}

fn accent_factory(accents: &[Accent], show_name: bool) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    let accents = accents.to_vec();
    factory.connect_setup(move |_, object| {
        let list_item = object.downcast_ref::<gtk::ListItem>().unwrap();
        let resource = crate::ui_resource::THEME_ACCENT_RESOURCE;
        let builder = gtk_widgets::ui_resource::builder(resource);
        gtk_widgets::objects!(builder, resource, { item: gtk::Box, label: gtk::Label, swatch: gtk::Box });
        label.set_visible(show_name);
        let provider = gtk::CssProvider::new();
        #[allow(deprecated)]
        swatch.style_context().add_provider(&provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        let accents = accents.clone();
        list_item.connect_item_notify(move |list_item| {
            let Some(value) = list_item.item().and_downcast::<gtk::StringObject>() else { return };
            label.set_text(&value.string());
            if let Some(accent) = accents.iter().find(|accent| accent.name == value.string()) {
                provider.load_from_string(&format!("* {{ background: {}; }}", accent.color));
            }
        });
        list_item.set_child(Some(&item));
    });
    factory
}
