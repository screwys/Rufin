use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gio, glib};
use playback::{EQUALIZER_BAND_COUNT, EqualizerSettings};

use localization::tr;

pub const EQUALIZER_FALLBACK_COMMIT_DELAY_MS: u64 = 200;
pub const EQUALIZER_BAND_SPACING: i32 = 6;
pub const EQUALIZER_LABEL_HEIGHT: i32 = 50;
pub const CUSTOM_PRESET: &str = "Custom";

pub fn equalizer_band_title(index: usize) -> String {
    const BANDS: [&str; EQUALIZER_BAND_COUNT] = [
        "60 Hz", "170 Hz", "310 Hz", "600 Hz", "1 kHz", "3 kHz", "6 kHz", "12 kHz", "14 kHz",
        "16 kHz",
    ];
    BANDS.get(index).copied().unwrap_or("Band").to_string()
}

pub fn equalizer_band_label_parts(index: usize) -> (String, String) {
    let title = equalizer_band_title(index);
    title
        .split_once(' ')
        .map(|(value, unit)| (value.to_string(), unit.to_string()))
        .unwrap_or_else(|| (title, String::new()))
}

pub fn equalizer_presets() -> Vec<(&'static str, Vec<f64>)> {
    vec![
        ("Flat", vec![0.0; EQUALIZER_BAND_COUNT]),
        (
            "Classical",
            vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -7.2, -7.2, -7.2, -9.6],
        ),
        (
            "Club",
            vec![0.0, 0.0, 3.2, 5.6, 5.6, 5.6, 3.2, 0.0, 0.0, 0.0],
        ),
        (
            "Dance",
            vec![9.6, 7.2, 2.4, 0.0, 0.0, -5.6, -7.2, -7.2, 0.0, 0.0],
        ),
        (
            "Full Bass",
            vec![9.6, 9.6, 9.6, 5.6, 1.6, -4.0, -8.0, -10.4, -11.2, -11.2],
        ),
        (
            "Full Treble",
            vec![-9.6, -9.6, -9.6, -4.0, 2.4, 11.2, 12.0, 12.0, 12.0, 12.0],
        ),
        (
            "Laptop/Headphones",
            vec![4.8, 11.2, 5.6, -3.2, -2.4, 1.6, 4.8, 9.6, 12.0, 12.0],
        ),
        (
            "Rock",
            vec![8.0, 4.8, -5.6, -8.0, -3.2, 4.0, 8.8, 11.2, 11.2, 11.2],
        ),
        (
            "Pop",
            vec![-1.6, 4.8, 7.2, 8.0, 5.6, 0.0, -2.4, -2.4, -1.6, -1.6],
        ),
        (
            "Techno",
            vec![8.0, 5.6, 0.0, -5.6, -4.8, 0.0, 8.0, 9.6, 9.6, 8.8],
        ),
    ]
}

pub fn equalizer_preset_names() -> Vec<&'static str> {
    equalizer_presets()
        .into_iter()
        .map(|(name, _)| name)
        .chain(std::iter::once(CUSTOM_PRESET))
        .collect()
}

pub fn equalizer_selected_preset(equalizer: &EqualizerSettings) -> String {
    if equalizer_preset_names()
        .iter()
        .any(|name| *name == equalizer.selected_preset)
    {
        equalizer.selected_preset.clone()
    } else {
        CUSTOM_PRESET.to_string()
    }
}

pub fn equalizer_preset_title(name: &str) -> String {
    match name {
        "Custom" => tr("Custom"),
        "Flat" => tr("Flat"),
        "Classical" => tr("Classical"),
        "Club" => tr("Club"),
        "Dance" => tr("Dance"),
        "Full Bass" => tr("Full Bass"),
        "Full Treble" => tr("Full Treble"),
        "Laptop/Headphones" => tr("Laptop/Headphones"),
        "Rock" => tr("Rock"),
        "Pop" => tr("Pop"),
        "Techno" => tr("Techno"),
        _ => name.to_string(),
    }
}

pub fn equalizer_default_preset_bands(name: &str) -> Vec<f64> {
    if name == CUSTOM_PRESET {
        return vec![0.0; EQUALIZER_BAND_COUNT];
    }
    equalizer_presets()
        .into_iter()
        .find_map(|(preset, bands)| (preset == name).then_some(bands))
        .unwrap_or_else(|| vec![0.0; EQUALIZER_BAND_COUNT])
}

pub fn equalizer_preset_bands(name: &str) -> Vec<f64> {
    equalizer_default_preset_bands(name)
}

#[derive(Clone)]
pub struct EqualizerSurface {
    pub root: gtk::Box,
    pub band_row: gtk::Box,
    pub reset: gtk::Button,
    pub controls: Rc<EqualizerControls>,
}

pub struct EqualizerControls {
    enabled: Cell<bool>,
    pub preset: gtk::MenuButton,
    selected_preset: RefCell<String>,
    reset: gtk::Button,
    reset_preset: RefCell<String>,
    pub scales: Vec<gtk::Scale>,
    pub syncing: Rc<Cell<bool>>,
}

impl EqualizerSurface {
    pub fn new(settings: &EqualizerSettings) -> Self {
        let resource = crate::ui_resource::EQUALIZER_RESOURCE;
        let builder = ui_shared::ui_resource::builder(resource);
        ui_shared::objects!(builder, resource, {
            root: gtk::Box,
            preset: gtk::MenuButton,
            reset_button: gtk::Button,
            band_row: gtk::Box,
            bands: gtk::Box,
        });
        preset.set_width_request(160);
        let mut scales = Vec::with_capacity(EQUALIZER_BAND_COUNT);
        for index in 0..EQUALIZER_BAND_COUNT {
            let band = gtk::Box::new(gtk::Orientation::Vertical, EQUALIZER_BAND_SPACING);
            band.set_width_request(1);
            band.set_halign(gtk::Align::Fill);
            band.set_valign(gtk::Align::Fill);
            band.set_hexpand(true);
            band.set_vexpand(true);
            let scale = gtk::Scale::with_range(gtk::Orientation::Vertical, -12.0, 12.0, 0.5);
            scale.add_css_class("equalizer-surface-scale");
            scale.set_inverted(true);
            scale.set_draw_value(false);
            scale.set_width_request(1);
            scale.set_halign(gtk::Align::Fill);
            scale.set_valign(gtk::Align::Fill);
            scale.set_hexpand(true);
            scale.set_vexpand(true);
            scale.set_tooltip_text(Some(&equalizer_band_title(index)));
            ui_shared::scale::install_scale_scroll_forwarding(&scale);
            band.append(&scale);
            band.append(&equalizer_band_label(index));
            bands.append(&band);
            scales.push(scale);
        }
        let surface = Self {
            root,
            band_row,
            reset: reset_button.clone(),
            controls: Rc::new(EqualizerControls {
                enabled: Cell::new(settings.enabled),
                preset,
                selected_preset: RefCell::new(CUSTOM_PRESET.to_string()),
                reset: reset_button,
                reset_preset: RefCell::new("Flat".to_string()),
                scales,
                syncing: Rc::new(Cell::new(false)),
            }),
        };
        // Keep callback state alive until the mounted widget is released.
        let controls = Rc::clone(&surface.controls);
        let _ = surface
            .root
            .add_weak_ref_notify_local(move || drop(controls));
        surface.set_settings(settings);
        surface
    }

    pub fn connect_changed(&self, changed: impl Fn(EqualizerSettings) + 'static) {
        let changed: Rc<dyn Fn(EqualizerSettings)> = Rc::new(changed);

        let pending_update = Rc::new(RefCell::new(None::<glib::SourceId>));
        let pointer_active = Rc::new(Cell::new(false));
        let scale_controls = Rc::downgrade(&self.controls);
        let scale_changed = Rc::clone(&changed);
        let commit: Rc<dyn Fn()> = Rc::new(move || {
            let Some(controls) = scale_controls.upgrade() else {
                return;
            };
            let mut settings = controls.settings();
            settings.enabled = true;
            settings.selected_preset = CUSTOM_PRESET.to_string();
            controls.set_settings(&settings);
            scale_changed(settings);
        });
        for scale in &self.controls.scales {
            connect_equalizer_scale_commit(
                scale,
                Rc::clone(&self.controls.syncing),
                Rc::clone(&pending_update),
                Rc::clone(&pointer_active),
                Rc::clone(&commit),
            );
        }

        let preset_actions = gio::SimpleActionGroup::new();
        let select_preset = gio::SimpleAction::new("select", Some(glib::VariantTy::STRING));
        let preset_controls = Rc::downgrade(&self.controls);
        let preset_changed = Rc::clone(&changed);
        select_preset.connect_activate(move |_, parameter| {
            let Some(controls) = preset_controls.upgrade() else {
                return;
            };
            let Some(preset) = parameter.and_then(|value| value.get::<String>()) else {
                return;
            };
            let mut settings = controls.settings();
            settings.enabled = true;
            settings.selected_preset = preset.clone();
            settings.bands = equalizer_preset_bands(&preset);
            settings.sanitize();
            controls.set_settings(&settings);
            preset_changed(settings);
        });
        preset_actions.add_action(&select_preset);
        self.controls
            .preset
            .insert_action_group("equalizer", Some(&preset_actions));

        let menu = gio::Menu::new();
        let flat = gio::Menu::new();
        let flat_item = gio::MenuItem::new(
            Some(&equalizer_preset_title("Flat")),
            Some("equalizer.select"),
        );
        flat_item.set_attribute_value("target", Some(&"Flat".to_variant()));
        flat.append_item(&flat_item);
        menu.append_section(None, &flat);

        let presets = gio::Menu::new();
        for preset in equalizer_preset_names().into_iter().skip(1) {
            let item = gio::MenuItem::new(
                Some(&equalizer_preset_title(preset)),
                Some("equalizer.select"),
            );
            item.set_attribute_value("target", Some(&preset.to_variant()));
            presets.append_item(&item);
        }
        menu.append_section(None, &presets);
        self.controls.preset.set_menu_model(Some(&menu));

        let reset_controls = Rc::downgrade(&self.controls);
        self.reset.connect_clicked(move |_| {
            let Some(controls) = reset_controls.upgrade() else {
                return;
            };
            let preset = controls.reset_preset.borrow().clone();
            let mut settings = controls.settings();
            settings.enabled = true;
            settings.selected_preset = preset.clone();
            settings.bands = equalizer_preset_bands(&preset);
            settings.sanitize();
            controls.set_settings(&settings);
            changed(settings);
        });
    }

    pub fn set_band_height_request(&self, height: i32) {
        self.band_row.set_height_request(height);
    }

    pub fn set_settings(&self, settings: &EqualizerSettings) {
        self.controls.set_settings(settings);
    }
}

impl EqualizerControls {
    pub fn set_settings(&self, settings: &EqualizerSettings) {
        self.syncing.set(true);
        let selected_preset = equalizer_selected_preset(settings);
        if selected_preset != CUSTOM_PRESET {
            *self.reset_preset.borrow_mut() = selected_preset.clone();
        }
        let is_custom = selected_preset == CUSTOM_PRESET;
        self.reset.set_visible(true);
        self.reset.set_opacity(if is_custom { 1.0 } else { 0.0 });
        self.reset.set_sensitive(is_custom);
        self.reset.set_can_target(is_custom);
        self.enabled.set(settings.enabled);
        *self.selected_preset.borrow_mut() = selected_preset.clone();
        self.preset
            .set_label(&equalizer_preset_title(&selected_preset));
        for (index, scale) in self.scales.iter().enumerate() {
            scale.set_value(settings.bands.get(index).copied().unwrap_or(0.0));
        }
        self.syncing.set(false);
    }

    pub fn settings(&self) -> EqualizerSettings {
        let mut settings = EqualizerSettings {
            enabled: self.enabled.get(),
            selected_preset: self.selected_preset.borrow().clone(),
            bands: self.scales.iter().map(gtk::Scale::value).collect(),
        };
        settings.sanitize();
        settings
    }
}

pub fn equalizer_band_label(index: usize) -> gtk::Widget {
    let (value, unit) = equalizer_band_label_parts(index);
    let label = gtk::Box::new(gtk::Orientation::Vertical, 0);
    label.add_css_class("equalizer-surface-band-label");
    label.set_height_request(EQUALIZER_LABEL_HEIGHT - EQUALIZER_BAND_SPACING);
    label.set_halign(gtk::Align::Center);
    label.set_valign(gtk::Align::Center);
    for text in [value, unit] {
        let row = gtk::Label::new(Some(&text));
        row.add_css_class("muted");
        row.set_xalign(0.5);
        row.set_width_chars(1);
        row.set_max_width_chars(4);
        row.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.append(&row);
    }
    label.upcast()
}

pub fn connect_equalizer_scale_commit(
    scale: &gtk::Scale,
    guard: Rc<Cell<bool>>,
    pending_update: Rc<RefCell<Option<glib::SourceId>>>,
    pointer_active: Rc<Cell<bool>>,
    commit: Rc<dyn Fn()>,
) {
    let changed = Rc::new(Cell::new(false));

    let guard_for_change = Rc::clone(&guard);
    let pending_for_change = Rc::clone(&pending_update);
    let pointer_for_change = Rc::clone(&pointer_active);
    let changed_for_change = Rc::clone(&changed);
    let commit_for_change = Rc::clone(&commit);
    scale.connect_value_changed(move |_| {
        if guard_for_change.get() {
            return;
        }
        changed_for_change.set(true);
        if let Some(source_id) = pending_for_change.borrow_mut().take() {
            source_id.remove();
        }
        if pointer_for_change.get() {
            return;
        }
        let pending_for_timeout = Rc::clone(&pending_for_change);
        let changed_for_timeout = Rc::clone(&changed_for_change);
        let commit_for_timeout = Rc::clone(&commit_for_change);
        let source_id = glib::timeout_add_local_once(
            Duration::from_millis(EQUALIZER_FALLBACK_COMMIT_DELAY_MS),
            move || {
                *pending_for_timeout.borrow_mut() = None;
                if changed_for_timeout.replace(false) {
                    commit_for_timeout();
                }
            },
        );
        *pending_for_change.borrow_mut() = Some(source_id);
    });

    let events = gtk::EventControllerLegacy::new();
    events.set_propagation_phase(gtk::PropagationPhase::Capture);
    let pending_for_press = Rc::clone(&pending_update);
    let pointer_for_press = Rc::clone(&pointer_active);
    let changed_for_press = Rc::clone(&changed);
    let finish_pointer_commit = {
        let guard = Rc::clone(&guard);
        let pending_update = Rc::clone(&pending_update);
        let pointer_active = Rc::clone(&pointer_active);
        let changed = Rc::clone(&changed);
        let commit = Rc::clone(&commit);
        Rc::new(move || {
            pointer_active.set(false);
            if let Some(source_id) = pending_update.borrow_mut().take() {
                source_id.remove();
            }
            if !guard.get() && changed.replace(false) {
                let commit_for_idle = Rc::clone(&commit);
                glib::idle_add_local_once(move || commit_for_idle());
            }
        })
    };
    events.connect_event(move |_, event| {
        match event.event_type() {
            gtk::gdk::EventType::ButtonPress | gtk::gdk::EventType::TouchBegin => {
                pointer_for_press.set(true);
                changed_for_press.set(false);
                if let Some(source_id) = pending_for_press.borrow_mut().take() {
                    source_id.remove();
                }
            }
            gtk::gdk::EventType::ButtonRelease
            | gtk::gdk::EventType::TouchEnd
            | gtk::gdk::EventType::TouchCancel
            | gtk::gdk::EventType::GrabBroken => finish_pointer_commit(),
            _ => {}
        }
        glib::Propagation::Proceed
    });
    scale.add_controller(events);
}

#[cfg(test)]
mod tests {
    use super::{CUSTOM_PRESET, EQUALIZER_BAND_COUNT, equalizer_preset_names, equalizer_presets};

    #[test]
    pub fn equalizer_presets_cover_all_bands() {
        for (_, bands) in equalizer_presets() {
            assert_eq!(bands.len(), EQUALIZER_BAND_COUNT);
            assert!(bands.iter().all(|gain| (-12.0..=12.0).contains(gain)));
        }
    }

    #[test]
    pub fn equalizer_preset_menu_order_is_grouped() {
        let names = equalizer_preset_names();
        assert_eq!(names.first(), Some(&"Flat"));
        assert_eq!(names.last(), Some(&CUSTOM_PRESET));
    }
}
