use serde::{Deserialize, Serialize};

pub type VisualizerColors = [[f32; 3]; 2];
pub type PreferenceIndex = usize;

#[cfg(feature = "bindings")]
uniffi::custom_type!(PreferenceIndex, u64, {
    remote,
    lower: |value| value as u64,
    try_lift: |value| Ok(usize::try_from(value)?),
});

#[cfg(feature = "bindings")]
uniffi::custom_type!(VisualizerColors, Vec<Vec<f32>>, {
    remote,
    lower: |value| value.into_iter().map(Vec::from).collect(),
    try_lift: |value| {
        let colors = value.into_iter().map(|color| color.try_into()
            .map_err(|_| uniffi::deps::anyhow::anyhow!("A color needs three components")))
            .collect::<uniffi::Result<Vec<[f32; 3]>>>()?;
        colors.try_into().map_err(|_| uniffi::deps::anyhow::anyhow!("A gradient needs two colors"))
    },
});

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum VisualizerStyle {
    Segmented,
    Solid,
    #[default]
    Rounded,
    Outline,
    Line,
    Filled,
    Mirrored,
    Circular,
    Ribbon,
    Halo,
}

impl VisualizerStyle {
    pub const ALL: [Self; 10] = [
        Self::Segmented,
        Self::Solid,
        Self::Rounded,
        Self::Outline,
        Self::Line,
        Self::Filled,
        Self::Mirrored,
        Self::Circular,
        Self::Ribbon,
        Self::Halo,
    ];
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum VisualizerFrequencyScale {
    #[default]
    Erb,
    Logarithmic,
    Linear,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum VisualizerWeighting {
    #[default]
    None,
    A,
    C,
}

macro_rules! visualizer_preference_fields {
    ($callback:ident $(, $args:tt)*) => {
        $callback! { [$($args)*]
            style: VisualizerStyle = VisualizerStyle::Rounded => set_visualizer_style;
            /// None follows the application's accent. Custom gradients store their two endpoints.
            colors: Option<VisualizerColors> = None => set_visualizer_colors;
            bar_count: u32 = 52 => set_visualizer_bar_count;
            frequency_scale: VisualizerFrequencyScale = VisualizerFrequencyScale::Erb => set_visualizer_frequency_scale;
            min_frequency: f64 = 30.0 => set_visualizer_min_frequency;
            max_frequency: f64 = 12_000.0 => set_visualizer_max_frequency;
            min_decibels: f64 = -80.0 => set_visualizer_min_decibels;
            max_decibels: f64 = 0.0 => set_visualizer_max_decibels;
            weighting: VisualizerWeighting = VisualizerWeighting::None => set_visualizer_weighting;
            mirrored: bool = false => set_visualizer_mirrored;
            shadow: bool = false => set_visualizer_shadow;
            shadow_ratio: f64 = 0.25 => set_visualizer_shadow_ratio;
            shadow_opacity: f64 = 0.15 => set_visualizer_shadow_opacity;
            shadow_brightness: f64 = 1.0 => set_visualizer_shadow_brightness;
            shadow_fit: bool = true => set_visualizer_shadow_fit;
            spacing: f64 = 1.0 => set_visualizer_spacing;
            opacity: f64 = 1.0 => set_visualizer_opacity;
            sidebar_lyrics_opacity: f64 = 0.48 => set_visualizer_sidebar_lyrics_opacity;
            fullscreen_lyrics_opacity: f64 = 0.25 => set_visualizer_fullscreen_lyrics_opacity;
            rise: f64 = 0.72 => set_visualizer_rise;
            fall: f64 = 0.16 => set_visualizer_fall;
            peaks: bool = false => set_visualizer_peaks;
            peak_hold: f64 = 0.5 => set_visualizer_peak_hold;
            peak_fall: f64 = 0.6 => set_visualizer_peak_fall;
        }
    };
}

macro_rules! define_visualizer_appearance {
    ([] $( $(#[$attribute:meta])* $field:ident: $ty:ty = $default:expr => $setter:ident; )*) => {
        #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
        #[cfg_attr(feature = "bindings", derive(uniffi::Record))]
        #[serde(default)]
        pub struct VisualizerAppearance { $( $(#[$attribute])* pub $field: $ty, )* }
        impl Default for VisualizerAppearance {
            fn default() -> Self { Self { $( $field: $default, )* } }
        }
    };
}
visualizer_preference_fields!(define_visualizer_appearance);

pub(super) use visualizer_preference_fields;

impl VisualizerAppearance {
    /// Clamp edited analysis bounds, leaving at least one Hz or dB between endpoints.
    pub fn normalize_analysis(&mut self) {
        self.min_frequency = self.min_frequency.clamp(20.0, 19_999.0);
        self.max_frequency = self.max_frequency.clamp(self.min_frequency + 1.0, 20_000.0);
        self.min_decibels = self.min_decibels.clamp(-120.0, -1.0);
        self.max_decibels = self.max_decibels.clamp(self.min_decibels + 1.0, 0.0);
    }
}

/// Project canonical ERB amplitudes into the preset's frequency and level ranges.
pub fn visualizer_spectrum_levels(
    amplitudes: &[f64],
    appearance: &VisualizerAppearance,
) -> Vec<f64> {
    use audio_processing::{visualizer_frequency, visualizer_position};
    let length = amplitudes.len();
    if length == 0 {
        return Vec::new();
    }
    let min_position = visualizer_position(appearance.min_frequency);
    let max_position = visualizer_position(appearance.max_frequency);
    let reference = appearance.weighting.response(1000.0);
    let frequency_at = |position| match appearance.frequency_scale {
        VisualizerFrequencyScale::Erb => {
            visualizer_frequency(visualizer_lerp(min_position, max_position, position))
        }
        VisualizerFrequencyScale::Logarithmic => {
            appearance.min_frequency
                * (appearance.max_frequency / appearance.min_frequency).powf(position)
        }
        VisualizerFrequencyScale::Linear => {
            visualizer_lerp(appearance.min_frequency, appearance.max_frequency, position)
        }
    };
    let weighted = |index: usize| {
        if appearance.weighting == VisualizerWeighting::None {
            return amplitudes[index];
        }
        let frequency = visualizer_frequency((index as f64 + 0.5) / length as f64);
        amplitudes[index] * appearance.weighting.response(frequency) / reference
    };
    (0..length)
        .map(|index| {
            let start = (visualizer_position(frequency_at(index as f64 / length as f64))
                * length as f64
                - 0.5)
                .ceil()
                .max(0.0) as usize;
            let end = (visualizer_position(frequency_at((index + 1) as f64 / length as f64))
                * length as f64
                - 0.5)
                .ceil()
                .min(length as f64) as usize;
            let amplitude = if end > start + 1 {
                let (total, peak) = (start..end)
                    .map(weighted)
                    .fold((0.0, 0.0_f64), |(total, peak), value| {
                        (total + value, peak.max(value))
                    });
                0.4 * total / (end - start) as f64 + 0.6 * peak
            } else {
                let frequency = frequency_at((index as f64 + 0.5) / length as f64);
                let source = (visualizer_position(frequency) * length as f64 - 0.5)
                    .clamp(0.0, (length - 1) as f64);
                let lower = source.floor() as usize;
                visualizer_lerp(
                    weighted(lower),
                    weighted((lower + 1).min(length - 1)),
                    source.fract(),
                )
            };
            if amplitude == 0.0 {
                return 0.0;
            }
            let decibels = 20.0 * amplitude.log10();
            ((decibels - appearance.min_decibels)
                / (appearance.max_decibels - appearance.min_decibels))
                .clamp(0.0, 1.0)
        })
        .collect()
}

impl VisualizerWeighting {
    fn response(self, frequency: f64) -> f64 {
        if self == Self::None {
            return 1.0;
        }
        let square = frequency * frequency;
        let c = square * 12_194.0_f64.powi(2)
            / ((square + 20.6_f64.powi(2)) * (square + 12_194.0_f64.powi(2)));
        if self == Self::A {
            c * square / ((square + 107.7_f64.powi(2)) * (square + 737.9_f64.powi(2))).sqrt()
        } else {
            c
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct VisualizerPreset {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub appearance: VisualizerAppearance,
}

impl VisualizerPreset {
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self)
            .expect("visualizer presets contain JSON-compatible values")
    }

    pub fn from_json(text: &str) -> Option<Self> {
        let preset: Self = serde_json::from_str(text).ok()?;
        let appearance = &preset.appearance;
        // Clipboard values must fit the controls and the renderer's normalized ranges.
        let valid = (8..=256).contains(&appearance.bar_count)
            && (20.0..=20_000.0).contains(&appearance.min_frequency)
            && (20.0..=20_000.0).contains(&appearance.max_frequency)
            && appearance.min_frequency < appearance.max_frequency
            && (-120.0..=0.0).contains(&appearance.min_decibels)
            && (-120.0..=0.0).contains(&appearance.max_decibels)
            && appearance.min_decibels < appearance.max_decibels
            && (0.0..=12.0).contains(&appearance.spacing)
            && (0.0..=0.95).contains(&appearance.shadow_ratio)
            && (0.0..=2.0).contains(&appearance.shadow_brightness)
            && [
                appearance.opacity,
                appearance.shadow_opacity,
                appearance.sidebar_lyrics_opacity,
                appearance.fullscreen_lyrics_opacity,
            ]
            .into_iter()
            .all(|value| (0.0..=1.0).contains(&value))
            && [appearance.rise, appearance.fall]
                .into_iter()
                .all(|value| (0.01..=1.0).contains(&value))
            && (0.0..=5.0).contains(&appearance.peak_hold)
            && (0.01..=2.0).contains(&appearance.peak_fall)
            && appearance.colors.is_none_or(|colors| {
                colors
                    .into_iter()
                    .flatten()
                    .all(|value| (0.0..=1.0).contains(&value))
            });
        valid.then_some(preset)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct VisualizerSettings {
    pub fps_limit: u32,
    pub appearance: VisualizerAppearance,
    pub presets: Vec<VisualizerPreset>,
    /// Last selected preset, retained as the save target while its appearance is edited.
    #[serde(default)]
    pub selected_preset: Option<PreferenceIndex>,
}

impl Default for VisualizerSettings {
    fn default() -> Self {
        Self {
            fps_limit: 30,
            appearance: VisualizerAppearance::default(),
            selected_preset: Some(0),
            presets: builtin_presets(),
        }
    }
}

pub const BUILTIN_PRESET_NAMES: [&str; 7] = [
    "Rounded spectrum",
    "Circular spectrum",
    "Soft spectrum",
    "Centered bars",
    "LED spectrum",
    "Ribbon",
    "Halo",
];
pub const BUILTIN_PRESET_COUNT: usize = BUILTIN_PRESET_NAMES.len();

fn builtin_presets() -> Vec<VisualizerPreset> {
    BUILTIN_PRESET_NAMES
        .iter()
        .enumerate()
        .map(|(slot, name)| VisualizerPreset {
            name: (*name).into(),
            appearance: default_preset(slot),
        })
        .collect()
}

fn default_preset(slot: usize) -> VisualizerAppearance {
    match slot {
        1 => VisualizerAppearance {
            frequency_scale: VisualizerFrequencyScale::Erb,
            min_frequency: 40.0,
            max_frequency: 16_000.0,
            min_decibels: -75.0,
            max_decibels: -5.0,
            weighting: VisualizerWeighting::C,
            style: VisualizerStyle::Circular,
            bar_count: 32,
            spacing: 2.0,
            rise: 0.55,
            fall: 0.12,
            peaks: true,
            peak_hold: 0.3,
            peak_fall: 0.8,
            ..Default::default()
        },
        2 => VisualizerAppearance {
            frequency_scale: VisualizerFrequencyScale::Linear,
            min_frequency: 30.0,
            max_frequency: 18_000.0,
            min_decibels: -95.0,
            max_decibels: -10.0,
            weighting: VisualizerWeighting::A,
            style: VisualizerStyle::Filled,
            bar_count: 96,
            opacity: 0.8,
            sidebar_lyrics_opacity: 0.3,
            fullscreen_lyrics_opacity: 0.18,
            rise: 0.4,
            fall: 0.08,
            ..Default::default()
        },
        3 => VisualizerAppearance {
            frequency_scale: VisualizerFrequencyScale::Logarithmic,
            min_frequency: 40.0,
            max_frequency: 16_000.0,
            min_decibels: -75.0,
            max_decibels: -5.0,
            weighting: VisualizerWeighting::A,
            style: VisualizerStyle::Mirrored,
            bar_count: 40,
            spacing: 3.0,
            rise: 0.8,
            fall: 0.2,
            ..Default::default()
        },
        4 => VisualizerAppearance {
            frequency_scale: VisualizerFrequencyScale::Logarithmic,
            min_frequency: 30.0,
            max_frequency: 16_000.0,
            min_decibels: -65.0,
            max_decibels: -5.0,
            weighting: VisualizerWeighting::None,
            style: VisualizerStyle::Segmented,
            bar_count: 64,
            spacing: 2.0,
            rise: 0.9,
            fall: 0.24,
            peaks: true,
            peak_hold: 0.7,
            peak_fall: 0.45,
            ..Default::default()
        },
        5 => VisualizerAppearance {
            frequency_scale: VisualizerFrequencyScale::Erb,
            min_frequency: 60.0,
            max_frequency: 18_000.0,
            min_decibels: -90.0,
            max_decibels: -10.0,
            weighting: VisualizerWeighting::A,
            style: VisualizerStyle::Ribbon,
            bar_count: 128,
            rise: 0.4,
            fall: 0.08,
            ..Default::default()
        },
        6 => VisualizerAppearance {
            frequency_scale: VisualizerFrequencyScale::Logarithmic,
            min_frequency: 40.0,
            max_frequency: 18_000.0,
            min_decibels: -85.0,
            max_decibels: -5.0,
            weighting: VisualizerWeighting::C,
            style: VisualizerStyle::Halo,
            bar_count: 128,
            rise: 0.55,
            fall: 0.12,
            ..Default::default()
        },
        _ => VisualizerAppearance::default(),
    }
}

fn legacy_preset(slot: usize) -> VisualizerAppearance {
    VisualizerAppearance {
        frequency_scale: VisualizerFrequencyScale::Erb,
        min_frequency: 30.0,
        max_frequency: 12_000.0,
        min_decibels: -80.0,
        max_decibels: 0.0,
        weighting: VisualizerWeighting::None,
        ..default_preset(slot)
    }
}

impl VisualizerSettings {
    pub(crate) fn migrate_preset_bar_counts(&mut self) {
        for (slot, preset) in self.presets.iter_mut().enumerate().take(5) {
            let updated = legacy_preset(slot);
            let mut previous = updated.clone();
            previous.bar_count = 52;
            if preset.appearance == previous {
                if self.appearance == previous {
                    self.appearance = updated.clone();
                }
                preset.appearance = updated;
            }
        }
    }

    pub fn selected_preset(&self) -> Option<usize> {
        self.selected_preset
            .filter(|slot| *slot < self.presets.len())
    }

    pub fn modified(&self) -> bool {
        self.selected_preset()
            .is_none_or(|slot| self.presets[slot].appearance != self.appearance)
    }

    pub fn save_target(&self) -> Option<usize> {
        self.selected_preset
            .filter(|slot| (BUILTIN_PRESET_COUNT..self.presets.len()).contains(slot))
    }

    pub fn select_preset(&mut self, slot: usize) {
        self.appearance = self.preset(slot);
        self.selected_preset = Some(slot);
    }

    pub fn preset(&self, slot: usize) -> VisualizerAppearance {
        self.presets
            .get(slot)
            .map(|preset| preset.appearance.clone())
            .unwrap_or_else(|| default_preset(slot))
    }

    pub fn save_preset(&mut self, slot: usize) {
        if slot >= BUILTIN_PRESET_COUNT
            && let Some(preset) = self.presets.get_mut(slot)
        {
            preset.appearance = self.appearance.clone();
            self.selected_preset = Some(slot);
        }
    }

    pub fn save_preset_as(&mut self, name: String) -> usize {
        let slot = self.presets.len();
        self.presets.push(VisualizerPreset {
            name,
            appearance: self.appearance.clone(),
        });
        self.selected_preset = Some(slot);
        slot
    }

    pub fn current_preset(&self) -> VisualizerPreset {
        VisualizerPreset {
            name: self
                .selected_preset()
                .map(|slot| self.presets[slot].name.clone())
                .unwrap_or_else(|| "Custom".into()),
            appearance: self.appearance.clone(),
        }
    }

    pub fn import_preset(&mut self, preset: VisualizerPreset) {
        self.appearance = preset.appearance;
        self.save_preset_as(if preset.name.trim().is_empty() {
            "Imported preset".into()
        } else {
            preset.name
        });
    }

    pub fn rename_preset(&mut self, slot: usize, name: String) {
        if slot >= BUILTIN_PRESET_COUNT
            && let Some(preset) = self.presets.get_mut(slot)
        {
            preset.name = name;
        }
    }

    pub fn delete_preset(&mut self, slot: usize) {
        if slot < BUILTIN_PRESET_COUNT || slot >= self.presets.len() {
            return;
        }
        let selected = self.selected_preset;
        self.presets.remove(slot);
        self.selected_preset = selected
            .filter(|selected| *selected != slot)
            .map(|selected| selected - usize::from(selected > slot));
    }

    pub(crate) fn migrate_named_presets(&mut self) {
        if self.presets.iter().any(|preset| !preset.name.is_empty()) {
            return;
        }
        let selected = self.selected_preset.or_else(|| {
            self.presets
                .iter()
                .position(|preset| preset.appearance == self.appearance)
        });
        let previous = std::mem::replace(&mut self.presets, builtin_presets());
        for (slot, mut preset) in previous.into_iter().enumerate() {
            let target = if preset.appearance == legacy_preset(slot) {
                if selected == Some(slot) && self.appearance == preset.appearance {
                    self.appearance = default_preset(slot);
                }
                slot
            } else {
                preset.name = format!("Preset {}", slot + 1);
                self.presets.push(preset);
                self.presets.len() - 1
            };
            if selected == Some(slot) {
                self.selected_preset = Some(target);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_slot_survives_edits_and_reload_even_when_presets_match() {
        let mut settings = VisualizerSettings::default();
        settings.save_preset_as("First".into());
        let slot = settings.save_preset_as("Second".into());
        settings.select_preset(slot);
        settings.appearance.spacing = 4.5;
        let restored: VisualizerSettings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored.selected_preset(), Some(slot));
        assert!(restored.modified());
        assert_eq!(restored.save_target(), Some(slot));
        assert_eq!(restored.appearance.spacing, 4.5);
    }

    #[test]
    fn existing_settings_infer_the_selected_preset_from_appearance() {
        let mut settings = VisualizerSettings::default();
        settings.appearance = legacy_preset(4);
        settings.presets.truncate(5);
        for (slot, preset) in settings.presets.iter_mut().enumerate() {
            preset.name.clear();
            preset.appearance = legacy_preset(slot);
        }
        let mut value = serde_json::to_value(&settings).unwrap();
        value.as_object_mut().unwrap().remove("selected_preset");
        let mut restored: VisualizerSettings = serde_json::from_value(value).unwrap();
        restored.migrate_named_presets();
        assert_eq!(restored.selected_preset(), Some(4));
    }

    #[test]
    fn missing_settings_preserve_motion_and_follow_accent() {
        let settings: VisualizerSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings.appearance, VisualizerAppearance::default());
        assert_eq!(settings.presets, VisualizerSettings::default().presets);
        assert_eq!(settings.appearance.colors, None);
        assert_eq!(settings.appearance.bar_count, 52);
    }

    #[test]
    fn named_presets_are_snapshots_and_builtins_are_preserved() {
        let mut settings = VisualizerSettings::default();
        let builtin = settings.preset(0);
        settings.appearance.bar_count = 256;
        settings.appearance.frequency_scale = VisualizerFrequencyScale::Logarithmic;
        settings.appearance.min_frequency = 100.0;
        settings.appearance.max_frequency = 18_000.0;
        settings.appearance.min_decibels = -60.0;
        settings.appearance.max_decibels = -10.0;
        settings.appearance.weighting = VisualizerWeighting::A;
        settings.save_preset(0);
        assert_eq!(settings.preset(0), builtin);
        assert_eq!(settings.selected_preset(), Some(0));
        assert!(settings.modified());
        assert_eq!(settings.save_target(), None);
        let slot = settings.save_preset_as("My spectrum".into());
        let saved = settings.appearance.clone();
        settings.select_preset(0);
        settings.select_preset(slot);
        assert_eq!(settings.appearance, saved);
        settings.appearance.style = VisualizerStyle::Ribbon;
        assert_eq!(settings.preset(slot).style, VisualizerStyle::Rounded);
        assert_eq!(settings.selected_preset(), Some(slot));
        assert!(settings.modified());
        assert_eq!(settings.save_target(), Some(slot));
        settings.save_preset(slot);
        assert_eq!(settings.selected_preset(), Some(slot));
        assert!(!settings.modified());
        settings.rename_preset(slot, "Ribbon".into());
        let restored: VisualizerSettings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored, settings);
        assert_eq!(restored.presets[slot].name, "Ribbon");
        assert_eq!(restored.preset(slot).bar_count, 256);
        settings.delete_preset(0);
        assert_eq!(settings.presets.len(), BUILTIN_PRESET_COUNT + 1);
        settings.delete_preset(slot);
        assert_eq!(settings.selected_preset(), None);
        assert_eq!(settings.presets.len(), BUILTIN_PRESET_COUNT);
        assert_eq!(settings.appearance.style, VisualizerStyle::Ribbon);
    }

    #[test]
    fn missing_presets_load_distinct_configs_and_old_slots_keep_custom_values() {
        let mut settings: VisualizerSettings = serde_json::from_str(r#"{"presets":[]}"#).unwrap();
        settings.migrate_named_presets();
        let defaults = VisualizerSettings::default();
        assert_eq!(settings.presets, defaults.presets);
        for slot in 0..BUILTIN_PRESET_COUNT {
            assert_eq!(settings.preset(slot).colors, None);
            for other in 0..slot {
                assert_ne!(settings.preset(slot).style, settings.preset(other).style);
                assert_ne!(
                    visualizer_spectrum_levels(&[0.03; 256], &settings.preset(slot)),
                    visualizer_spectrum_levels(&[0.03; 256], &settings.preset(other)),
                    "presets {slot} and {other} should have distinct spectrum responses"
                );
            }
        }
        let mut legacy = crate::settings::StoredSettings::default();
        legacy.ui.visualizer.presets.truncate(5);
        for (slot, preset) in legacy.ui.visualizer.presets.iter_mut().enumerate() {
            preset.name.clear();
            preset.appearance = legacy_preset(slot);
            preset.appearance.bar_count = 52;
        }
        legacy.ui.visualizer.presets[4].appearance.spacing = 7.0;
        legacy.ui.visualizer.select_preset(4);
        legacy.ui.visualizer.appearance.spacing = 8.0;
        let mut json = serde_json::to_value(&legacy).unwrap();
        json.as_object_mut()
            .unwrap()
            .remove("visualizer_preset_bar_counts_updated");
        let mut restored: crate::settings::StoredSettings = serde_json::from_value(json).unwrap();
        restored.migrate_defaults();
        assert_eq!(restored.ui.visualizer.preset(1).bar_count, 32);
        assert_eq!(restored.ui.visualizer.preset(2).bar_count, 96);
        assert_eq!(restored.ui.visualizer.preset(3).bar_count, 40);
        let slot = restored.ui.visualizer.save_target().unwrap();
        assert_eq!(slot, BUILTIN_PRESET_COUNT);
        assert_eq!(restored.ui.visualizer.preset(slot).bar_count, 52);
        assert_eq!(restored.ui.visualizer.preset(slot).spacing, 7.0);
        assert_eq!(restored.ui.visualizer.appearance.spacing, 8.0);
        let mut reloaded: crate::settings::StoredSettings =
            serde_json::from_value(serde_json::to_value(&restored).unwrap()).unwrap();
        reloaded.migrate_defaults();
        assert_eq!(reloaded.ui.visualizer, restored.ui.visualizer);
    }

    #[test]
    fn copied_presets_round_trip_without_names() {
        let preset = VisualizerPreset {
            name: String::new(),
            appearance: VisualizerAppearance {
                colors: Some([[0.1, 0.2, 0.3], [0.7, 0.8, 0.9]]),
                frequency_scale: VisualizerFrequencyScale::Linear,
                min_frequency: 200.0,
                max_frequency: 18_000.0,
                min_decibels: -100.0,
                max_decibels: -20.0,
                weighting: VisualizerWeighting::C,
                rise: 0.35,
                bar_count: 128,
                mirrored: true,
                shadow: true,
                shadow_ratio: 0.4,
                shadow_opacity: 0.6,
                shadow_brightness: 1.5,
                shadow_fit: false,
                peaks: true,
                ..Default::default()
            },
        };
        let json = preset.to_json();
        assert!(!json.contains("name"));
        assert_eq!(VisualizerPreset::from_json(&json), Some(preset.clone()));
        let mut settings = VisualizerSettings::default();
        settings.import_preset(VisualizerPreset::from_json(&json).unwrap());
        assert_eq!(settings.appearance, preset.appearance);
        let slot = settings.selected_preset().unwrap();
        assert_eq!(settings.presets[slot].name, "Imported preset");
        settings.rename_preset(slot, "My spectrum".into());
        let exported = settings.current_preset().to_json();
        settings.import_preset(VisualizerPreset::from_json(&exported).unwrap());
        assert_eq!(settings.current_preset().name, "My spectrum");
        assert_eq!(settings.appearance, preset.appearance);
        assert_eq!(settings.presets.len(), BUILTIN_PRESET_COUNT + 2);
        assert!(VisualizerPreset::from_json("{}").is_none());
        assert!(VisualizerPreset::from_json(r#"{"appearance":{"rise":2}}"#).is_none());
        assert!(VisualizerPreset::from_json("not a preset").is_none());
        for count in [0, 7, 257, u32::MAX] {
            assert!(
                VisualizerPreset::from_json(&format!(
                    r#"{{"appearance":{{"bar_count":{count}}}}}"#
                ))
                .is_none()
            );
        }
        let old = VisualizerPreset::from_json(r#"{"appearance":{"spacing":2}}"#).unwrap();
        assert_eq!(old.appearance.bar_count, 52);
        assert_eq!(
            old.appearance.frequency_scale,
            VisualizerFrequencyScale::Erb
        );
        assert_eq!(old.appearance.weighting, VisualizerWeighting::None);
        assert_eq!(old.appearance.min_frequency, 30.0);
        assert_eq!(old.appearance.max_frequency, 12_000.0);
        assert_eq!(old.appearance.min_decibels, -80.0);
        assert_eq!(old.appearance.max_decibels, 0.0);
        for invalid in [
            r#"{"min_frequency":0}"#,
            r#"{"max_frequency":21000}"#,
            r#"{"min_frequency":12000,"max_frequency":12000}"#,
            r#"{"min_decibels":-121}"#,
            r#"{"max_decibels":1}"#,
            r#"{"min_decibels":-20,"max_decibels":-80}"#,
        ] {
            assert!(
                VisualizerPreset::from_json(&format!(r#"{{"appearance":{invalid}}}"#)).is_none()
            );
        }
        assert!(!old.appearance.mirrored);
        assert!(!old.appearance.shadow);
        assert!(VisualizerPreset::from_json(r#"{"appearance":{"shadow_ratio":1}}"#).is_none());
        assert!(VisualizerPreset::from_json(r#"{"appearance":{"shadow_opacity":2}}"#).is_none());
        assert!(VisualizerPreset::from_json(r#"{"appearance":{"shadow_brightness":3}}"#).is_none());
    }
}

pub const VISUALIZER_ZERO_THRESHOLD: f64 = 0.004;
pub const VISUALIZER_TOP_GAP: f64 = 50.0;
pub const VISUALIZER_REFERENCE_FRAME_MICROS: f64 = 1_000_000.0 / 60.0;
pub const VISUALIZER_STALE_MICROS: i64 = 133_333;

#[derive(Clone, Copy, Default)]
pub struct VisualizerPeak {
    pub level: f64,
    pub hold: f64,
}

impl VisualizerPeak {
    pub fn advance(&mut self, level: f64, elapsed: f64, hold: f64, fall: f64) {
        if level >= self.level {
            self.level = level;
            self.hold = hold;
        } else {
            let falling = (elapsed - self.hold).max(0.0);
            self.hold = (self.hold - elapsed).max(0.0);
            self.level = (self.level - falling * fall).max(level);
        }
    }
}

pub fn visualizer_column_geometry(width: f64, level_count: usize, gap: f64) -> (usize, f64) {
    let available_width = (width * 0.984).max(1.0);
    let fitting_columns = ((available_width + gap) / (2.0 + gap)).floor() as usize;
    let columns = level_count.min(fitting_columns.max(1)).max(1);
    let cell =
        ((available_width - gap * columns.saturating_sub(1) as f64) / columns as f64).max(2.0);
    (columns, cell)
}

pub fn visualizer_row_geometry(height: f64, cell: f64, gap: f64) -> (usize, f64) {
    let grid_height = (height - VISUALIZER_TOP_GAP).max(height * 0.64).max(1.0);
    let fitting_rows = (((grid_height + gap) / (1.0 + gap)).floor() as usize).max(1);
    let rows = (((grid_height + gap) / (cell + gap)).floor() as usize)
        .clamp(8, 32)
        .min(fitting_rows);
    let row_height = ((grid_height - gap * rows.saturating_sub(1) as f64) / rows as f64).max(1.0);
    (rows, row_height)
}

pub fn visualizer_accent_gradient(base: [f32; 3]) -> [[f32; 3]; 2] {
    [
        base.map(|value| value * 0.75),
        base.map(|value| value + (1.0 - value) * 0.32),
    ]
}

pub fn visualizer_lerp(start: f64, end: f64, mix: f64) -> f64 {
    start + (end - start) * mix
}

pub fn visualizer_shadow_geometry(height: f64, appearance: &VisualizerAppearance) -> (f64, f64) {
    if !appearance.shadow
        || matches!(
            appearance.style,
            VisualizerStyle::Circular | VisualizerStyle::Halo
        )
    {
        return (height, 0.0);
    }
    let reserved = height * appearance.shadow_ratio.clamp(0.0, 0.95);
    let main = height - reserved;
    let reflected = if reserved == 0.0 {
        0.0
    } else if appearance.shadow_fit {
        reserved
    } else {
        main
    };
    (main, reflected)
}

pub fn visualizer_radial_shadow_geometry(
    radius: f64,
    extent: f64,
    appearance: &VisualizerAppearance,
) -> (f64, f64) {
    if !appearance.shadow {
        return (0.0, 0.0);
    }
    let limit = radius * appearance.shadow_ratio.clamp(0.0, 0.95);
    let scale = if appearance.shadow_fit { limit } else { extent };
    (scale, limit)
}

pub fn visualizer_bar_levels(levels: &[f64], columns: usize, mirrored: bool) -> Vec<f64> {
    if columns == 0 || levels.is_empty() {
        return Vec::new();
    }
    if mirrored {
        let half = visualizer_bar_levels(levels, columns.div_ceil(2), false);
        return (0..columns)
            .map(|column| half[(2 * column + 1).abs_diff(columns) / 2])
            .collect();
    }
    (0..columns)
        .map(|column| {
            if columns > levels.len() {
                let position = column as f64 * (levels.len() - 1) as f64 / (columns - 1) as f64;
                let lower = position.floor() as usize;
                let upper = (lower + 1).min(levels.len() - 1);
                return visualizer_lerp(levels[lower], levels[upper], position.fract())
                    .clamp(0.0, 1.0);
            }
            let start = column * levels.len() / columns;
            let end = ((column + 1) * levels.len() / columns).max(start + 1);
            let mut total = 0.0;
            let mut peak = 0.0_f64;
            let mut count = 0;
            for level in &levels[start..end.min(levels.len())] {
                let level = level.clamp(0.0, 1.0);
                total += level;
                peak = peak.max(level);
                count += 1;
            }
            let average = if count == 0 {
                0.0
            } else {
                total / count as f64
            };
            average * 0.4 + peak * 0.6
        })
        .collect()
}

pub fn visualizer_smoothing_weights(elapsed_frames: f64, rise: f64, fall: f64) -> (f64, f64) {
    (
        1.0 - (1.0 - rise).powf(elapsed_frames),
        1.0 - (1.0 - fall).powf(elapsed_frames),
    )
}

pub fn visualizer_smoothed_level(value: f64, target: f64, weight: f64) -> f64 {
    let next = target * weight + value * (1.0 - weight);
    if target == 0.0 && next < VISUALIZER_ZERO_THRESHOLD {
        0.0
    } else {
        next
    }
}
