use crate::PlayerUi;
use adw::prelude::*;
use rufin_core::settings::visualizer::{
    VisualizerAppearance, VisualizerFrequencyScale, VisualizerPreset, VisualizerStyle,
    VisualizerWeighting,
};
use std::cell::Cell;
use std::rc::Rc;

pub(super) fn build_visualizer_settings(shell: &Rc<PlayerUi>) -> adw::PreferencesPage {
    let resource = crate::ui_resource::VISUALIZER_SETTINGS_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        page: adw::PreferencesPage,
        builtin_names: gtk::StringList,
        custom_title: gtk::Label,
        fps_limit: gtk::Scale,
        ignore_volume: adw::SwitchRow,
        bar_count: gtk::Scale,
        mirrored: adw::SwitchRow,
        shadow: adw::ExpanderRow,
        shadow_ratio: gtk::Scale,
        shadow_opacity: gtk::Scale,
        shadow_brightness: gtk::Scale,
        shadow_fit: adw::SwitchRow,
        style: adw::ComboRow,
        frequency_scale: adw::ComboRow,
        weighting: adw::ComboRow,
        min_frequency: gtk::Scale,
        max_frequency: gtk::Scale,
        min_decibels: gtk::Scale,
        max_decibels: gtk::Scale,
        accent: adw::SwitchRow,
        start_color: gtk::Button,
        end_color: gtk::Button,
        start_color_swatch: gtk::DrawingArea,
        end_color_swatch: gtk::DrawingArea,
        start_color_row: gtk::Box,
        end_color_row: gtk::Box,
        spacing: gtk::Scale,
        opacity: gtk::Scale,
        sidebar_opacity: gtk::Scale,
        fullscreen_opacity: gtk::Scale,
        rise: gtk::Scale,
        fall: gtk::Scale,
        peaks: adw::ExpanderRow,
        peak_hold: gtk::Scale,
        peak_fall: gtk::Scale,
        presets: adw::ComboRow,
        save: gtk::Button,
        duplicate: gtk::Button,
        save_icon: gtk::Image,
        rename: gtk::Button,
        delete: gtk::Button,
        fft_size: adw::ComboRow,
        reset: gtk::Button,
        export: gtk::Button,
        import: gtk::Button,
    });

    for scale in [
        &min_frequency,
        &max_frequency,
        &min_decibels,
        &max_decibels,
        &fps_limit,
        &bar_count,
        &shadow_ratio,
        &shadow_opacity,
        &shadow_brightness,
        &spacing,
        &opacity,
        &sidebar_opacity,
        &fullscreen_opacity,
        &rise,
        &fall,
        &peak_hold,
        &peak_fall,
    ] {
        let digits = scale.digits() as usize;
        gtk_widgets::scale::install_sliding_value_bubble(scale, move |value| {
            format!("{value:.digits$}")
        });
    }

    ignore_volume.set_active(
        shell
            .settings
            .current
            .borrow()
            .playback
            .visualizer_ignore_volume,
    );

    let limit = shell.settings.current.borrow().visualizer.fps_limit;
    fps_limit.set_value(limit as f64);
    gtk_widgets::scale::install_scale_scroll_forwarding(&fps_limit);

    fft_size.set_selected(
        shell
            .settings
            .current
            .borrow()
            .playback
            .visualizer_fft_size
            .ilog2()
            .saturating_sub(10),
    );
    let indicator = crate::preset_name::PresetIndicator::default();
    let preset_names = gtk::StringList::new(&[]);
    presets.set_model(Some(&preset_names));
    let syncing = Rc::new(Cell::new(false));
    // The dialog owns the controls. Refresh callbacks retain only weak references to them.
    let refresh_presets: Rc<dyn Fn()> = {
        let shell = Rc::downgrade(shell);
        let syncing = Rc::clone(&syncing);
        let save = save.downgrade();
        let presets = presets.downgrade();
        let rename = rename.downgrade();
        let delete = delete.downgrade();
        Rc::new(move || {
            let Some(shell) = shell.upgrade() else { return };
            let settings = shell.settings.current.borrow().visualizer.clone();
            let previous = syncing.replace(true);
            let selected = settings.selected_preset();
            if let Some(row) = presets.upgrade() {
                let mut names = settings
                    .presets
                    .iter()
                    .enumerate()
                    .map(|(index, preset)| {
                        builtin_names
                            .string(index as u32)
                            .map(|name| name.to_string())
                            .unwrap_or_else(|| preset.name.clone())
                    })
                    .collect::<Vec<_>>();
                names.insert(0, custom_title.text().to_string());
                // Rebuilding the list during selection queues another selection notification.
                if preset_names.n_items() as usize != names.len()
                    || names.iter().enumerate().any(|(index, name)| {
                        preset_names.string(index as u32).as_deref() != Some(name.as_str())
                    })
                {
                    preset_names.splice(
                        0,
                        preset_names.n_items(),
                        &names.iter().map(String::as_str).collect::<Vec<_>>(),
                    );
                }
                row.set_selected(selected.map_or(0, |index| index as u32 + 1));
                indicator.update(&row, settings.modified());
            }
            if let Some(button) = save.upgrade() {
                button.set_sensitive(settings.modified());
            }
            let custom = selected.is_some_and(|index| {
                index >= rufin_core::settings::visualizer::BUILTIN_PRESET_COUNT
                    && index < settings.presets.len()
            });
            if let Some(button) = rename.upgrade() {
                button.set_sensitive(custom);
            }
            if let Some(button) = delete.upgrade() {
                button.set_sensitive(custom);
            }
            syncing.set(previous);
        })
    };
    let refresh: Rc<dyn Fn()> = {
        let refresh_presets = Rc::clone(&refresh_presets);
        let shell = Rc::downgrade(shell);
        let syncing = Rc::clone(&syncing);
        let style = style.downgrade();
        let fps_limit = fps_limit.downgrade();
        let fft_size = fft_size.downgrade();
        let ignore_volume = ignore_volume.downgrade();
        let frequency_scale = frequency_scale.downgrade();
        let weighting = weighting.downgrade();
        let accent = accent.downgrade();
        let colors = [start_color_swatch.downgrade(), end_color_swatch.downgrade()];
        let color_rows = [start_color_row.downgrade(), end_color_row.downgrade()];
        let scales = [
            &min_frequency,
            &max_frequency,
            &min_decibels,
            &max_decibels,
            &bar_count,
            &shadow_ratio,
            &shadow_opacity,
            &shadow_brightness,
            &spacing,
            &opacity,
            &sidebar_opacity,
            &fullscreen_opacity,
            &rise,
            &fall,
            &peak_hold,
            &peak_fall,
        ]
        .map(|row| row.downgrade());
        let peaks = peaks.downgrade();
        let mirrored = mirrored.downgrade();
        let shadow = shadow.downgrade();
        let shadow_fit = shadow_fit.downgrade();
        Rc::new(move || {
            let Some(shell) = shell.upgrade() else {
                return;
            };
            let settings = shell.settings.current.borrow().visualizer.clone();
            let playback = shell.settings.current.borrow().playback.clone();
            refresh_presets();
            syncing.set(true);
            if let Some(row) = fps_limit.upgrade() {
                row.set_value(settings.fps_limit as f64);
            }
            if let Some(row) = fft_size.upgrade() {
                row.set_selected(playback.visualizer_fft_size.ilog2().saturating_sub(10));
            }
            if let Some(row) = ignore_volume.upgrade() {
                row.set_active(playback.visualizer_ignore_volume);
            }
            let appearance = settings.appearance;
            if let Some(style) = style.upgrade() {
                style.set_selected(
                    VisualizerStyle::ALL
                        .iter()
                        .position(|style| *style == appearance.style)
                        .unwrap() as u32,
                );
            }
            if let Some(row) = frequency_scale.upgrade() {
                row.set_selected(match appearance.frequency_scale {
                    VisualizerFrequencyScale::Erb => 0,
                    VisualizerFrequencyScale::Logarithmic => 1,
                    VisualizerFrequencyScale::Linear => 2,
                });
            }
            if let Some(row) = weighting.upgrade() {
                row.set_selected(match appearance.weighting {
                    VisualizerWeighting::None => 0,
                    VisualizerWeighting::A => 1,
                    VisualizerWeighting::C => 2,
                });
            }
            if let Some(accent) = accent.upgrade() {
                accent.set_active(appearance.colors.is_none());
            }
            for swatch in &colors {
                if let Some(swatch) = swatch.upgrade() {
                    swatch.queue_draw();
                }
            }
            for row in &color_rows {
                if let Some(row) = row.upgrade() {
                    row.set_sensitive(appearance.colors.is_some());
                }
            }
            let values = [
                appearance.min_frequency,
                appearance.max_frequency,
                appearance.min_decibels,
                appearance.max_decibels,
                f64::from(appearance.bar_count),
                appearance.shadow_ratio * 100.0,
                appearance.shadow_opacity * 100.0,
                appearance.shadow_brightness * 100.0,
                appearance.spacing,
                appearance.opacity * 100.0,
                appearance.sidebar_lyrics_opacity * 100.0,
                appearance.fullscreen_lyrics_opacity * 100.0,
                appearance.rise * 100.0,
                appearance.fall * 100.0,
                appearance.peak_hold,
                appearance.peak_fall * 100.0,
            ];
            for (row, value) in scales.iter().zip(values) {
                if let Some(row) = row.upgrade() {
                    row.set_value(value);
                }
            }
            if let Some(peaks) = peaks.upgrade() {
                peaks.set_enable_expansion(appearance.peaks);
                peaks.set_expanded(appearance.peaks);
            }
            if let Some(row) = mirrored.upgrade() {
                row.set_active(appearance.mirrored);
            }
            if let Some(row) = shadow.upgrade() {
                row.set_enable_expansion(appearance.shadow);
                row.set_expanded(appearance.shadow);
            }
            if let Some(row) = shadow_fit.upgrade() {
                row.set_active(appearance.shadow_fit);
            }
            syncing.set(false);
        })
    };
    refresh();

    let edits = Rc::new(VisualizerEdits {
        shell: Rc::downgrade(shell),
        refresh_catalog: refresh_presets,
        refresh,
    });
    (edits.refresh_catalog)();
    let editor = Rc::clone(&edits);
    let changing = Rc::clone(&syncing);
    ignore_volume.connect_active_notify(move |row| {
        if changing.get() {
            return;
        }
        let value = row.is_active();
        editor.apply("visualizer volume", move |settings| {
            let changed = settings.playback.visualizer_ignore_volume != value;
            settings.playback.visualizer_ignore_volume = value;
            changed
        });
    });
    let editor = Rc::clone(&edits);
    let changing = Rc::clone(&syncing);
    fps_limit.connect_value_changed(move |row| {
        if changing.get() {
            return;
        }
        let value = row.value() as u32;
        editor.apply("visualizer frame rate", move |settings| {
            let changed = settings.visualizer.fps_limit != value;
            settings.visualizer.fps_limit = value;
            changed
        });
    });
    let editor = Rc::clone(&edits);
    let changing = Rc::clone(&syncing);
    fft_size.connect_selected_notify(move |row| {
        if changing.get() {
            return;
        }
        let value = 1_u32 << (row.selected() + 10);
        editor.apply("visualizer FFT", move |settings| {
            let changed = settings.playback.visualizer_fft_size != value;
            settings.playback.visualizer_fft_size = value;
            changed
        });
    });
    let editor = Rc::clone(&edits);
    let changing = Rc::clone(&syncing);
    style.connect_selected_notify(move |row| {
        if !changing.get()
            && let Some(style) = VisualizerStyle::ALL.get(row.selected() as usize).copied()
        {
            editor.appearance(move |appearance| appearance.style = style);
        }
    });
    for (row, weighting_choice) in [(frequency_scale, false), (weighting, true)] {
        let editor = Rc::clone(&edits);
        let syncing = Rc::clone(&syncing);
        row.connect_selected_notify(move |row| {
            if syncing.get() {
                return;
            }
            let selected = row.selected() as usize;
            editor.appearance(move |appearance| {
                if weighting_choice {
                    appearance.weighting = [
                        VisualizerWeighting::None,
                        VisualizerWeighting::A,
                        VisualizerWeighting::C,
                    ][selected];
                } else {
                    appearance.frequency_scale = [
                        VisualizerFrequencyScale::Erb,
                        VisualizerFrequencyScale::Logarithmic,
                        VisualizerFrequencyScale::Linear,
                    ][selected];
                }
            });
        });
    }
    macro_rules! slider {
        ($row:ident, $field:ident, $scale:expr) => {{
            let editor = Rc::clone(&edits);
            let syncing = Rc::clone(&syncing);
            gtk_widgets::scale::install_scale_scroll_forwarding(&$row);
            $row.connect_value_changed(move |row| {
                if syncing.get() {
                    return;
                }
                let value = row.value() / $scale;
                editor.appearance(move |appearance| appearance.$field = value as _);
            });
        }};
    }
    slider!(min_frequency, min_frequency, 1.0);
    slider!(max_frequency, max_frequency, 1.0);
    slider!(min_decibels, min_decibels, 1.0);
    slider!(max_decibels, max_decibels, 1.0);
    slider!(bar_count, bar_count, 1.0);
    slider!(shadow_ratio, shadow_ratio, 100.0);
    slider!(shadow_opacity, shadow_opacity, 100.0);
    slider!(shadow_brightness, shadow_brightness, 100.0);
    slider!(spacing, spacing, 1.0);
    slider!(opacity, opacity, 100.0);
    slider!(sidebar_opacity, sidebar_lyrics_opacity, 100.0);
    slider!(fullscreen_opacity, fullscreen_lyrics_opacity, 100.0);
    slider!(rise, rise, 100.0);
    slider!(fall, fall, 100.0);
    slider!(peak_hold, peak_hold, 1.0);
    slider!(peak_fall, peak_fall, 100.0);
    macro_rules! switch {
        ($row:ident, $field:ident) => {{
            let editor = Rc::clone(&edits);
            let syncing = Rc::clone(&syncing);
            $row.connect_active_notify(move |row| {
                if syncing.get() {
                    return;
                }
                let active = row.is_active();
                editor.appearance(move |appearance| appearance.$field = active);
            });
        }};
    }
    switch!(mirrored, mirrored);
    switch!(shadow_fit, shadow_fit);
    for (row, peak_choice) in [(shadow, false), (peaks, true)] {
        let editor = Rc::clone(&edits);
        let syncing = Rc::clone(&syncing);
        row.connect_enable_expansion_notify(move |row| {
            if syncing.get() {
                return;
            }
            let active = row.enables_expansion();
            editor.appearance(move |appearance| {
                if peak_choice {
                    appearance.peaks = active;
                } else {
                    appearance.shadow = active;
                }
            });
        });
    }
    let editor = Rc::clone(&edits);
    let changing = Rc::clone(&syncing);
    accent.connect_active_notify(move |row| {
        if changing.get() {
            return;
        }
        let Some(shell) = editor.shell.upgrade() else {
            return;
        };
        let colors = (!row.is_active()).then(|| {
            crate::visualizer::accent_gradient(shell.views.visualizer.sidebar_area.color())
        });
        editor.appearance(move |appearance| appearance.colors = colors);
    });
    for (index, (button, swatch)) in [
        (start_color, start_color_swatch),
        (end_color, end_color_swatch),
    ]
    .into_iter()
    .enumerate()
    {
        let appearance = Rc::clone(&shell.views.visualizer.appearance);
        swatch.set_draw_func(move |swatch, context, width, height| {
            let colors = appearance
                .borrow()
                .colors
                .unwrap_or_else(|| crate::visualizer::accent_gradient(swatch.color()));
            let color = colors[index];
            context.set_source_rgb(color[0].into(), color[1].into(), color[2].into());
            context.rectangle(0.0, 0.0, width.into(), height.into());
            let _ = context.fill();
        });
        let editor = Rc::clone(&edits);
        let title = button.tooltip_text().unwrap();
        button.connect_clicked(move |_| {
            let Some(shell) = editor.shell.upgrade() else {
                return;
            };
            let Some(window) = shell.window.upgrade() else {
                return;
            };
            let default_colors =
                crate::visualizer::accent_gradient(shell.views.visualizer.sidebar_area.color());
            let colors = shell
                .settings
                .current
                .borrow()
                .visualizer
                .appearance
                .colors
                .unwrap_or(default_colors);
            let rgba = |rgb: [f32; 3]| gtk::gdk::RGBA::new(rgb[0], rgb[1], rgb[2], 1.0);
            let editor = Rc::clone(&editor);
            crate::color_chooser::present_color_chooser(
                &window,
                &title,
                rgba(colors[index]),
                move || rgba(default_colors[index]),
                move |color| {
                    let color = [color.red(), color.green(), color.blue()];
                    editor.appearance(move |appearance| {
                        appearance.colors.get_or_insert(default_colors)[index] = color;
                    });
                },
            );
        });
    }
    let feedback: Rc<dyn Fn()> = Rc::new(gtk_widgets::feedback::icon_action(
        save.upcast_ref(),
        &save_icon,
    ));
    for (button, action) in [(save, 0), (duplicate, 1), (rename, 2)] {
        let editor = Rc::clone(&edits);
        let presets = presets.downgrade();
        let feedback = Rc::clone(&feedback);
        button.connect_clicked(move |_| {
            let Some(shell) = editor.shell.upgrade() else {
                return;
            };
            let slot = presets
                .upgrade()
                .and_then(|row| row.selected().checked_sub(1))
                .map(|slot| slot as usize);
            if action == 2
                && !slot.is_some_and(|slot| {
                    slot >= rufin_core::settings::visualizer::BUILTIN_PRESET_COUNT
                })
            {
                return;
            }
            if action == 0
                && slot.is_some_and(|slot| {
                    slot >= rufin_core::settings::visualizer::BUILTIN_PRESET_COUNT
                })
            {
                let saved = editor.commit("save visualizer preset", false, |settings| {
                    if let Some(slot) = settings.visualizer.save_target() {
                        settings.visualizer.save_preset(slot);
                        true
                    } else {
                        false
                    }
                });
                if saved {
                    feedback();
                }
                return;
            }
            let Some(window) = shell.window.upgrade() else {
                return;
            };
            let name = slot
                .and_then(|slot| {
                    shell
                        .settings
                        .current
                        .borrow()
                        .visualizer
                        .presets
                        .get(slot)
                        .map(|preset| preset.name.clone())
                })
                .unwrap_or_default();
            let editor = Rc::clone(&editor);
            let feedback = Rc::clone(&feedback);
            crate::preset_name::present(
                &window,
                match action {
                    2 => crate::preset_name::NameAction::Rename,
                    1 => crate::preset_name::NameAction::Duplicate,
                    _ => crate::preset_name::NameAction::Save,
                },
                &name,
                move |name| {
                    let saved = editor.commit("name visualizer preset", true, move |settings| {
                        if action == 2 {
                            if let Some(slot) = slot {
                                settings.visualizer.rename_preset(slot, name);
                            }
                        } else {
                            settings.visualizer.save_preset_as(name);
                        }
                        true
                    });
                    if saved && action == 0 {
                        feedback();
                    }
                },
            );
        });
    }
    let editor = Rc::clone(&edits);
    let preset_row = presets.downgrade();
    delete.connect_clicked(move |_| {
        let Some(shell) = editor.shell.upgrade() else {
            return;
        };
        let resource = crate::ui_resource::EQUALIZER_DIALOGS_RESOURCE;
        let builder = gtk_widgets::ui_resource::builder(resource);
        let dialog: adw::AlertDialog =
            gtk_widgets::ui_resource::object(&builder, resource, "confirm");
        let Some(slot) = preset_row
            .upgrade()
            .and_then(|row| row.selected().checked_sub(1))
            .map(|slot| slot as usize)
        else {
            return;
        };
        if slot < rufin_core::settings::visualizer::BUILTIN_PRESET_COUNT {
            return;
        }
        let settings = shell.settings.current.borrow().visualizer.clone();
        let Some(preset) = settings.presets.get(slot) else {
            return;
        };
        dialog.set_body(&preset.name);
        let Some(window) = shell.window.upgrade() else {
            return;
        };
        let editor = Rc::clone(&editor);
        gtk::glib::spawn_future_local(async move {
            if dialog.choose_future(Some(&window)).await == "accept" {
                editor.commit("delete visualizer preset", true, move |settings| {
                    settings.visualizer.delete_preset(slot);
                    true
                });
            }
        });
    });
    let editor = Rc::clone(&edits);
    let changing = Rc::clone(&syncing);
    presets.connect_selected_notify(move |row| {
        if changing.get() {
            return;
        }
        let selected = row.selected();
        editor.apply("select visualizer preset", move |settings| {
            let previous = settings.visualizer.clone();
            if selected == 0 {
                settings.visualizer.selected_preset = None;
            } else {
                settings.visualizer.select_preset(selected as usize - 1);
            }
            settings.visualizer != previous
        });
    });
    let editor = Rc::clone(&edits);
    reset.connect_clicked(move |_| {
        editor.appearance(|appearance| *appearance = VisualizerAppearance::default())
    });
    let editor = Rc::clone(&edits);
    export.connect_clicked(move |_| {
        let Some(shell) = editor.shell.upgrade() else {
            return;
        };
        let Some(window) = shell.window.upgrade() else {
            return;
        };
        let text = shell
            .settings
            .current
            .borrow()
            .visualizer
            .current_preset()
            .to_json();
        crate::preset_transfer::export(&window, &["visualizer.rufin.json"], move |_| {
            Ok(text.clone())
        });
    });
    let editor = Rc::clone(&edits);
    import.connect_clicked(move |_| {
        let Some(shell) = editor.shell.upgrade() else {
            return;
        };
        let Some(window) = shell.window.upgrade() else {
            return;
        };
        let editor = Rc::clone(&editor);
        crate::preset_transfer::import(&window, false, move |text, _| {
            let result = VisualizerPreset::from_json(&text).map(|preset| {
                editor.commit("import visualizer preset", true, move |settings| {
                    settings.visualizer.import_preset(preset);
                    true
                })
            });
            async move {
                let result = result.ok_or_else(|| localization::tr("Not valid"))?;
                if !result {
                    return Err(localization::tr("This isn't available"));
                }
                Ok(())
            }
        });
    });
    page
}

struct VisualizerEdits {
    shell: std::rc::Weak<PlayerUi>,
    refresh_catalog: Rc<dyn Fn()>,
    refresh: Rc<dyn Fn()>,
}

impl VisualizerEdits {
    fn commit(
        &self,
        action: &'static str,
        catalog: bool,
        edit: impl FnOnce(&mut rufin_core::settings::app::Settings) -> bool,
    ) -> bool {
        let Some(shell) = self.shell.upgrade() else {
            return false;
        };
        let changed = shell.settings.update_app_settings(action, edit).is_some();
        shell.apply_visualizer_appearance();
        if catalog {
            (self.refresh_catalog)();
        }
        (self.refresh)();
        changed
    }

    fn apply(
        &self,
        action: &'static str,
        edit: impl FnOnce(&mut rufin_core::settings::app::Settings) -> bool,
    ) {
        self.commit(action, false, edit);
    }

    fn appearance(self: &Rc<Self>, edit: impl FnOnce(&mut VisualizerAppearance) + Send + 'static) {
        self.apply("visualizer appearance", move |settings| {
            let previous = settings.visualizer.clone();
            edit(&mut settings.visualizer.appearance);
            settings.visualizer.appearance.normalize_analysis();
            previous != settings.visualizer
        });
    }
}
