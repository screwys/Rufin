use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
}

impl VisualizerStyle {
    pub const ALL: [Self; 8] = [
        Self::Segmented,
        Self::Solid,
        Self::Rounded,
        Self::Outline,
        Self::Line,
        Self::Filled,
        Self::Mirrored,
        Self::Circular,
    ];
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VisualizerAppearance {
    pub style: VisualizerStyle,
    /// None follows the application's accent. Custom gradients store their two endpoints.
    pub colors: Option<[[f32; 3]; 2]>,
    pub spacing: f64,
    pub opacity: f64,
    pub sidebar_lyrics_opacity: f64,
    pub fullscreen_lyrics_opacity: f64,
    pub rise: f64,
    pub fall: f64,
    pub peaks: bool,
    pub peak_hold: f64,
    pub peak_fall: f64,
}

impl Default for VisualizerAppearance {
    fn default() -> Self {
        Self {
            style: VisualizerStyle::Rounded,
            colors: None,
            spacing: 1.0,
            opacity: 1.0,
            sidebar_lyrics_opacity: 0.48,
            fullscreen_lyrics_opacity: 0.25,
            rise: 0.72,
            fall: 0.16,
            peaks: false,
            peak_hold: 0.5,
            peak_fall: 0.6,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VisualizerPreset {
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
        let valid = (0.0..=12.0).contains(&appearance.spacing)
            && [
                appearance.opacity,
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
pub struct VisualizerSettings {
    pub appearance: VisualizerAppearance,
    pub presets: Vec<VisualizerPreset>,
    pub selected_preset: Option<usize>,
}

impl Default for VisualizerSettings {
    fn default() -> Self {
        Self {
            appearance: VisualizerAppearance::default(),
            selected_preset: None,
            presets: (0..5)
                .map(|slot| VisualizerPreset {
                    appearance: default_preset(slot),
                })
                .collect(),
        }
    }
}

fn default_preset(slot: usize) -> VisualizerAppearance {
    match slot {
        1 => VisualizerAppearance {
            style: VisualizerStyle::Segmented,
            spacing: 2.0,
            rise: 0.9,
            fall: 0.24,
            peaks: true,
            peak_hold: 0.7,
            peak_fall: 0.45,
            ..Default::default()
        },
        2 => VisualizerAppearance {
            style: VisualizerStyle::Filled,
            opacity: 0.8,
            sidebar_lyrics_opacity: 0.3,
            fullscreen_lyrics_opacity: 0.18,
            rise: 0.4,
            fall: 0.08,
            ..Default::default()
        },
        3 => VisualizerAppearance {
            style: VisualizerStyle::Mirrored,
            spacing: 3.0,
            rise: 0.8,
            fall: 0.2,
            ..Default::default()
        },
        4 => VisualizerAppearance {
            style: VisualizerStyle::Circular,
            spacing: 2.0,
            rise: 0.55,
            fall: 0.12,
            peaks: true,
            peak_hold: 0.3,
            peak_fall: 0.8,
            ..Default::default()
        },
        _ => VisualizerAppearance::default(),
    }
}

impl VisualizerSettings {
    pub fn selected_preset(&self) -> usize {
        self.selected_preset.unwrap_or_else(|| {
            (0..5)
                .find(|slot| self.preset(*slot) == self.appearance)
                .unwrap_or(0)
        })
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
        while self.presets.len() <= slot {
            self.presets.push(VisualizerPreset {
                appearance: default_preset(self.presets.len()),
            });
        }
        self.presets[slot].appearance = self.appearance.clone();
        self.selected_preset = Some(slot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_slot_survives_edits_and_reload_even_when_presets_match() {
        let mut settings = VisualizerSettings::default();
        settings.presets[4] = settings.presets[0].clone();
        settings.select_preset(4);
        settings.appearance.spacing = 4.5;
        let restored: VisualizerSettings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored.selected_preset(), 4);
        assert_eq!(restored.appearance.spacing, 4.5);
    }

    #[test]
    fn existing_settings_infer_the_selected_preset_from_appearance() {
        let mut settings = VisualizerSettings::default();
        settings.appearance = settings.preset(4);
        let mut value = serde_json::to_value(&settings).unwrap();
        value.as_object_mut().unwrap().remove("selected_preset");
        let restored: VisualizerSettings = serde_json::from_value(value).unwrap();
        assert_eq!(restored.selected_preset(), 4);
    }

    #[test]
    fn missing_settings_preserve_motion_and_follow_accent() {
        let settings: VisualizerSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings, VisualizerSettings::default());
        assert_eq!(settings.appearance.colors, None);
    }

    #[test]
    fn numbered_presets_are_snapshots_and_can_be_replaced() {
        let mut settings = VisualizerSettings::default();
        settings.appearance.colors = Some([[0.1, 0.2, 0.3], [0.7, 0.8, 0.9]]);
        settings.appearance.rise = 0.4;
        settings.save_preset(0);
        settings.appearance.style = VisualizerStyle::Circular;
        settings.appearance.rise = 0.9;
        assert_eq!(settings.presets[0].appearance.rise, 0.4);
        assert_eq!(
            settings.presets[0].appearance.style,
            VisualizerStyle::Rounded
        );
        settings.save_preset(0);
        assert_eq!(settings.presets.len(), 5);
        let restored: VisualizerSettings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored, settings);
        assert_eq!(
            restored.presets[0].appearance.style,
            VisualizerStyle::Circular
        );
    }

    #[test]
    fn missing_presets_load_distinct_configs_and_saving_preserves_other_slots() {
        let mut settings: VisualizerSettings = serde_json::from_str(r#"{"presets":[]}"#).unwrap();
        let defaults = VisualizerSettings::default();
        for slot in 0..5 {
            assert_eq!(settings.preset(slot), defaults.preset(slot));
            assert_eq!(settings.preset(slot).colors, None);
            for other in 0..slot {
                assert_ne!(settings.preset(slot).style, settings.preset(other).style);
            }
        }
        settings.appearance.style = VisualizerStyle::Circular;
        settings.appearance.spacing = 4.5;
        settings.save_preset(4);
        for slot in 0..4 {
            assert_eq!(settings.preset(slot), defaults.preset(slot));
        }
        assert_eq!(settings.preset(4), settings.appearance);
        assert_eq!(settings.presets.len(), 5);
    }

    #[test]
    fn copied_presets_round_trip_without_names() {
        let preset = VisualizerPreset {
            appearance: VisualizerAppearance {
                colors: Some([[0.1, 0.2, 0.3], [0.7, 0.8, 0.9]]),
                rise: 0.35,
                peaks: true,
                ..Default::default()
            },
        };
        let json = preset.to_json();
        assert!(!json.contains("name"));
        assert_eq!(VisualizerPreset::from_json(&json), Some(preset));
        assert!(VisualizerPreset::from_json("{}").is_none());
        assert!(VisualizerPreset::from_json(r#"{"appearance":{"rise":2}}"#).is_none());
        assert!(VisualizerPreset::from_json("not a preset").is_none());
    }
}
