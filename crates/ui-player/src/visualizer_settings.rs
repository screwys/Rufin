use crate::PlayerUi;
use adw::prelude::*;
use rufin_core::settings::visualizer::{VisualizerAppearance, VisualizerPreset, VisualizerStyle};
use std::cell::Cell;
use std::rc::Rc;

pub(super) fn build_visualizer_settings(shell: &Rc<PlayerUi>) -> adw::PreferencesPage {
    let resource = crate::ui_resource::VISUALIZER_SETTINGS_RESOURCE;
    let builder = ui_shared::ui_resource::builder(resource);
    ui_shared::objects!(builder, resource, {
        page: adw::PreferencesPage,
        style: adw::ComboRow,
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
        reset: gtk::Button,
        copy: gtk::Button,
        paste: gtk::Button,
        saved_message: gtk::Label,
    });

    let syncing = Rc::new(Cell::new(false));
    // The dialog owns the controls. Refresh callbacks retain only weak references to them.
    let refresh: Rc<dyn Fn()> = {
        let shell = Rc::downgrade(shell);
        let syncing = Rc::clone(&syncing);
        let style = style.downgrade();
        let accent = accent.downgrade();
        let colors = [start_color_swatch.downgrade(), end_color_swatch.downgrade()];
        let color_rows = [start_color_row.downgrade(), end_color_row.downgrade()];
        let scales = [
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
        Rc::new(move || {
            let Some(shell) = shell.upgrade() else {
                return;
            };
            let settings = shell.settings.current.borrow().visualizer.clone();
            let appearance = settings.appearance;
            syncing.set(true);
            if let Some(style) = style.upgrade() {
                style.set_selected(
                    VisualizerStyle::ALL
                        .iter()
                        .position(|style| *style == appearance.style)
                        .unwrap() as u32,
                );
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
            syncing.set(false);
        })
    };
    refresh();

    let weak_shell = Rc::downgrade(shell);
    let changing = Rc::clone(&syncing);
    style.connect_selected_notify(move |row| {
        if changing.get() {
            return;
        }
        if let (Some(shell), Some(style)) = (
            weak_shell.upgrade(),
            VisualizerStyle::ALL.get(row.selected() as usize),
        ) {
            update_appearance(&shell, |appearance| appearance.style = *style);
        }
    });
    macro_rules! slider {
        ($row:ident, $field:ident, $scale:expr) => {{
            let shell = Rc::downgrade(shell);
            let syncing = Rc::clone(&syncing);
            ui_shared::scale::install_scale_scroll_forwarding(&$row);
            $row.connect_value_changed(move |row| {
                if syncing.get() {
                    return;
                }
                if let Some(shell) = shell.upgrade() {
                    update_appearance(&shell, |appearance| {
                        appearance.$field = row.value() / $scale
                    });
                }
            });
        }};
    }
    slider!(spacing, spacing, 1.0);
    slider!(opacity, opacity, 100.0);
    slider!(sidebar_opacity, sidebar_lyrics_opacity, 100.0);
    slider!(fullscreen_opacity, fullscreen_lyrics_opacity, 100.0);
    slider!(rise, rise, 100.0);
    slider!(fall, fall, 100.0);
    slider!(peak_hold, peak_hold, 1.0);
    slider!(peak_fall, peak_fall, 100.0);

    let weak_shell = Rc::downgrade(shell);
    let changing = Rc::clone(&syncing);
    let refresh_colors = Rc::clone(&refresh);
    accent.connect_active_notify(move |row| {
        if changing.get() {
            return;
        }
        let Some(shell) = weak_shell.upgrade() else {
            return;
        };
        let colors = if row.is_active() {
            None
        } else {
            Some(crate::visualizer::accent_gradient(
                shell.views.visualizer.sidebar_area.color(),
            ))
        };
        update_appearance(&shell, |appearance| appearance.colors = colors);
        refresh_colors();
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
        let weak_shell = Rc::downgrade(shell);
        let refresh_colors = Rc::clone(&refresh);
        let title = button.tooltip_text().unwrap();
        button.connect_clicked(move |_| {
            let Some(shell) = weak_shell.upgrade() else {
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
            let initial = rgba(colors[index]);
            let default = rgba(default_colors[index]);
            let weak = Rc::downgrade(&shell);
            let refresh = Rc::clone(&refresh_colors);
            crate::color_chooser::present_color_chooser(
                &window,
                &title,
                initial,
                move || default,
                move |color| {
                    if let Some(shell) = weak.upgrade() {
                        update_appearance(&shell, |appearance| {
                            let colors = appearance.colors.get_or_insert(default_colors);
                            colors[index] = [color.red(), color.green(), color.blue()];
                        });
                        refresh();
                    }
                },
            );
        });
    }
    let weak_shell = Rc::downgrade(shell);
    let changing = Rc::clone(&syncing);
    peaks.connect_enable_expansion_notify(move |row| {
        if changing.get() {
            return;
        }
        if let Some(shell) = weak_shell.upgrade() {
            update_appearance(&shell, |appearance| {
                appearance.peaks = row.enables_expansion()
            });
        }
    });

    let weak_shell = Rc::downgrade(shell);
    let preset_row = presets.downgrade();
    let saved_message = saved_message.text();
    save.connect_clicked(move |_| {
        let (Some(shell), Some(row)) = (weak_shell.upgrade(), preset_row.upgrade()) else {
            return;
        };
        let slot = row.selected() as usize;
        if shell
            .settings
            .update_app_settings("save visualizer preset", |settings| {
                settings.visualizer.save_preset(slot);
                true
            })
            .is_some()
        {
            shell
                .control_feedback
                .show_feedback_toast(saved_message.to_string());
        }
    });
    let weak_shell = Rc::downgrade(shell);
    let refresh_applied = Rc::clone(&refresh);
    presets.connect_selected_notify(move |row| {
        let Some(shell) = weak_shell.upgrade() else {
            return;
        };
        let preset = shell
            .settings
            .current
            .borrow()
            .visualizer
            .preset(row.selected() as usize);
        update_appearance(&shell, |appearance| *appearance = preset);
        refresh_applied();
    });
    let weak_shell = Rc::downgrade(shell);
    let refresh_reset = Rc::clone(&refresh);
    reset.connect_clicked(move |_| {
        if let Some(shell) = weak_shell.upgrade() {
            update_appearance(&shell, |appearance| {
                *appearance = VisualizerAppearance::default()
            });
            refresh_reset();
        }
    });
    let weak_shell = Rc::downgrade(shell);
    copy.connect_clicked(move |button| {
        let Some(shell) = weak_shell.upgrade() else {
            return;
        };
        let preset = VisualizerPreset {
            appearance: shell
                .settings
                .current
                .borrow()
                .visualizer
                .appearance
                .clone(),
        };
        button.clipboard().set_text(&preset.to_json());
    });
    let weak_shell = Rc::downgrade(shell);
    paste.connect_clicked(move |button| {
        if let Some(shell) = weak_shell.upgrade() {
            present_paste_dialog(&shell, Rc::clone(&refresh), button.clipboard());
        }
    });
    page
}

fn present_paste_dialog(
    shell: &Rc<PlayerUi>,
    refresh: Rc<dyn Fn()>,
    clipboard: gtk::gdk::Clipboard,
) {
    let Some(window) = shell.window.upgrade() else {
        return;
    };
    let resource = crate::ui_resource::VISUALIZER_PASTE_RESOURCE;
    let builder = ui_shared::ui_resource::builder(resource);
    ui_shared::objects!(builder, resource, {
        dialog: adw::Dialog,
        text: gtk::TextView,
        apply: gtk::Button,
        error: gtk::Label,
    });
    let buffer = text.buffer();
    let weak_buffer = buffer.downgrade();
    gtk::glib::spawn_future_local(async move {
        if let Ok(Some(contents)) = clipboard.read_text_future().await
            && let Some(buffer) = weak_buffer.upgrade()
            && buffer.char_count() == 0
        {
            buffer.set_text(&contents);
        }
    });
    let weak_shell = Rc::downgrade(shell);
    let close = dialog.downgrade();
    let error_for_edit = error.downgrade();
    buffer.connect_changed(move |_| {
        if let Some(error) = error_for_edit.upgrade() {
            error.set_visible(false);
        }
    });
    apply.connect_clicked(move |_| {
        let contents = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
        let Some(preset) = VisualizerPreset::from_json(&contents) else {
            error.set_visible(true);
            return;
        };
        let Some(shell) = weak_shell.upgrade() else {
            return;
        };
        update_appearance(&shell, |appearance| *appearance = preset.appearance);
        refresh();
        if let Some(dialog) = close.upgrade() {
            dialog.close();
        }
    });
    dialog.set_focus(Some(&text));
    ui_shared::popup::present_light_dismiss_dialog(&dialog, &window);
}

fn update_appearance(shell: &PlayerUi, update: impl FnOnce(&mut VisualizerAppearance)) {
    shell
        .settings
        .update_app_settings("visualizer appearance", |settings| {
            let previous = settings.visualizer.appearance.clone();
            update(&mut settings.visualizer.appearance);
            previous != settings.visualizer.appearance
        });
    shell.apply_visualizer_appearance();
}
