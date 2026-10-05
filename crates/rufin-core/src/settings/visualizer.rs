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
    pub fps_limit: u32,
    pub appearance: VisualizerAppearance,
    pub presets: Vec<VisualizerPreset>,
    pub selected_preset: Option<usize>,
}

impl Default for VisualizerSettings {
    fn default() -> Self {
        Self {
            fps_limit: 30,
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
            style: VisualizerStyle::Circular,
            spacing: 2.0,
            rise: 0.55,
            fall: 0.12,
            peaks: true,
            peak_hold: 0.3,
            peak_fall: 0.8,
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
            style: VisualizerStyle::Segmented,
            spacing: 2.0,
            rise: 0.9,
            fall: 0.24,
            peaks: true,
            peak_hold: 0.7,
            peak_fall: 0.45,
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
    let grid_height = (height - VISUALIZER_TOP_GAP).max(height * 0.64);
    let rows = (((grid_height + gap) / (cell + gap)).floor() as usize).clamp(8, 32);
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

pub fn visualizer_bar_levels(levels: &[f64], columns: usize) -> Vec<f64> {
    if columns == 0 || levels.is_empty() {
        return Vec::new();
    }
    (0..columns)
        .map(|column| {
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
