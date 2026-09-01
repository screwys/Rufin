use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use playback::{EQUALIZER_BAND_COUNT, EqualizerSettings};

use localization::tr;

const EQUALIZER_FALLBACK_COMMIT_DELAY_MS: u64 = 200;
const EQUALIZER_BAND_SPACING: i32 = 6;
const EQUALIZER_LABEL_HEIGHT: i32 = 50;
const CUSTOM_PRESET: &str = "Custom";

fn equalizer_band_title(index: usize) -> String {
    const BANDS: [&str; EQUALIZER_BAND_COUNT] = [
        "60 Hz", "170 Hz", "310 Hz", "600 Hz", "1 kHz", "3 kHz", "6 kHz", "12 kHz", "14 kHz",
        "16 kHz",
    ];
    BANDS.get(index).copied().unwrap_or("Band").to_string()
}

fn equalizer_band_label_parts(index: usize) -> (String, String) {
    let title = equalizer_band_title(index);
    title
        .split_once(' ')
        .map(|(value, unit)| (value.to_string(), unit.to_string()))
        .unwrap_or_else(|| (title, String::new()))
}

pub(crate) fn equalizer_presets() -> Vec<(&'static str, Vec<f64>)> {
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

fn equalizer_preset_names() -> Vec<&'static str> {
    std::iter::once(CUSTOM_PRESET)
        .chain(equalizer_presets().iter().map(|(name, _)| *name))
        .collect()
}

fn equalizer_selected_preset(equalizer: &EqualizerSettings) -> String {
    if equalizer_preset_names()
        .iter()
        .any(|name| *name == equalizer.selected_preset)
    {
        equalizer.selected_preset.clone()
    } else {
        CUSTOM_PRESET.to_string()
    }
}

fn equalizer_preset_position(name: &str) -> u32 {
    equalizer_preset_names()
        .iter()
        .position(|preset| *preset == name)
        .unwrap_or_default() as u32
}

fn equalizer_preset_name_at(position: u32) -> Option<String> {
    equalizer_preset_names()
        .get(position as usize)
        .map(|name| (*name).to_string())
}

fn equalizer_preset_title(name: &str) -> String {
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

fn equalizer_default_preset_bands(name: &str) -> Vec<f64> {
    if name == CUSTOM_PRESET {
        return vec![0.0; EQUALIZER_BAND_COUNT];
    }
    equalizer_presets()
        .into_iter()
        .find_map(|(preset, bands)| (preset == name).then_some(bands))
        .unwrap_or_else(|| vec![0.0; EQUALIZER_BAND_COUNT])
}

fn equalizer_preset_bands(name: &str) -> Vec<f64> {
    equalizer_default_preset_bands(name)
}

fn equalizer_preset_model() -> gtk::StringList {
    let titles = equalizer_preset_names()
        .into_iter()
        .map(equalizer_preset_title)
        .collect::<Vec<_>>();
    let title_refs = titles.iter().map(String::as_str).collect::<Vec<_>>();
    gtk::StringList::new(&title_refs)
}

fn build_equalizer_preset_dropdown(selected: u32) -> gtk::DropDown {
    let model = equalizer_preset_model();
    gtk::DropDown::builder()
        .model(&model)
        .selected(selected)
        .build()
}

#[derive(Clone)]
pub(crate) struct EqualizerSurface {
    pub(crate) root: gtk::Box,
    band_row: gtk::Box,
    reset: gtk::Button,
    controls: Rc<EqualizerControls>,
}

struct EqualizerControls {
    enabled: gtk::Switch,
    preset: gtk::DropDown,
    scales: Vec<gtk::Scale>,
    syncing: Rc<Cell<bool>>,
}

impl EqualizerSurface {
    pub(crate) fn new(settings: &EqualizerSettings) -> Self {
        let resource = crate::ui_resource::EQUALIZER_RESOURCE;
        let builder = crate::ui_resource::builder(resource);
        crate::ui_resource::objects!(builder, resource, {
            root: gtk::Box,
            enabled: gtk::Switch,
            preset_host: gtk::Box,
            reset_button: gtk::Button,
            band_row: gtk::Box,
            bands: gtk::Box,
        });
        let preset = build_equalizer_preset_dropdown(0);
        preset.set_valign(gtk::Align::Center);
        preset_host.append(&preset);
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
            super::playback_settings::install_scale_scroll_forwarding(&scale);
            band.append(&scale);
            band.append(&equalizer_band_label(index));
            bands.append(&band);
            scales.push(scale);
        }
        let surface = Self {
            root,
            band_row,
            reset: reset_button,
            controls: Rc::new(EqualizerControls {
                enabled,
                preset,
                scales,
                syncing: Rc::new(Cell::new(false)),
            }),
        };
        surface.set_settings(settings);
        surface
    }

    pub(crate) fn connect_changed(&self, changed: impl Fn(EqualizerSettings) + 'static) {
        let changed: Rc<dyn Fn(EqualizerSettings)> = Rc::new(changed);

        let switch_controls = Rc::downgrade(&self.controls);
        let switch_changed = Rc::clone(&changed);
        self.controls
            .enabled
            .connect_state_set(move |row, enabled| {
                let Some(controls) = switch_controls.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                if controls.syncing.get() {
                    return glib::Propagation::Proceed;
                }
                row.set_state(enabled);
                let mut settings = controls.settings();
                settings.enabled = enabled;
                controls.set_settings(&settings);
                switch_changed(settings);
                glib::Propagation::Stop
            });

        let pending_update = Rc::new(RefCell::new(None::<glib::SourceId>));
        let pointer_active = Rc::new(Cell::new(false));
        let scale_controls = Rc::downgrade(&self.controls);
        let scale_changed = Rc::clone(&changed);
        let commit: Rc<dyn Fn()> = Rc::new(move || {
            let Some(controls) = scale_controls.upgrade() else {
                return;
            };
            let mut settings = controls.settings();
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

        let preset_controls = Rc::downgrade(&self.controls);
        let preset_changed = Rc::clone(&changed);
        self.controls
            .preset
            .connect_selected_notify(move |dropdown| {
                let Some(controls) = preset_controls.upgrade() else {
                    return;
                };
                if controls.syncing.get() {
                    return;
                }
                let Some(preset) = equalizer_preset_name_at(dropdown.selected()) else {
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

        let reset_controls = Rc::downgrade(&self.controls);
        self.reset.connect_clicked(move |_| {
            let Some(controls) = reset_controls.upgrade() else {
                return;
            };
            let mut settings = controls.settings();
            settings.bands = equalizer_default_preset_bands(&settings.selected_preset);
            settings.sanitize();
            controls.set_settings(&settings);
            changed(settings);
        });
    }

    pub(crate) fn set_band_height_request(&self, height: i32) {
        self.band_row.set_height_request(height);
    }

    pub(crate) fn set_settings(&self, settings: &EqualizerSettings) {
        self.controls.set_settings(settings);
    }
}

impl EqualizerControls {
    fn set_settings(&self, settings: &EqualizerSettings) {
        self.syncing.set(true);
        self.enabled.set_active(settings.enabled);
        self.preset
            .set_selected(equalizer_preset_position(&equalizer_selected_preset(
                settings,
            )));
        for (index, scale) in self.scales.iter().enumerate() {
            scale.set_value(settings.bands.get(index).copied().unwrap_or(0.0));
        }
        self.syncing.set(false);
    }

    fn settings(&self) -> EqualizerSettings {
        let mut settings = EqualizerSettings {
            enabled: self.enabled.is_active(),
            selected_preset: equalizer_preset_name_at(self.preset.selected())
                .unwrap_or_else(|| CUSTOM_PRESET.to_string()),
            bands: self.scales.iter().map(gtk::Scale::value).collect(),
        };
        settings.sanitize();
        settings
    }
}

fn equalizer_band_label(index: usize) -> gtk::Widget {
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

fn connect_equalizer_scale_commit(
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
    use super::{EQUALIZER_BAND_COUNT, EqualizerSurface, equalizer_presets};
    use gtk::prelude::*;

    #[test]
    fn equalizer_presets_cover_all_bands() {
        for (_, bands) in equalizer_presets() {
            assert_eq!(bands.len(), EQUALIZER_BAND_COUNT);
            assert!(bands.iter().all(|gain| (-12.0..=12.0).contains(gain)));
        }
    }

    #[test]
    #[ignore = "requires a GTK display"]
    fn connected_equalizer_surface_releases_every_control() {
        gtk::init().expect("GTK display");
        crate::application::verify_interface_resources().expect("Rufin resources");
        let surface = EqualizerSurface::new(&playback::EqualizerSettings::default());
        surface.connect_changed(|_| {});
        let root = surface.root.downgrade();
        let controls = surface
            .controls
            .scales
            .iter()
            .map(gtk::prelude::ObjectExt::downgrade)
            .collect::<Vec<_>>();
        drop(surface);
        while gtk::glib::MainContext::default().iteration(false) {}
        assert!(root.upgrade().is_none());
        assert!(controls.iter().all(|control| control.upgrade().is_none()));
    }
}
