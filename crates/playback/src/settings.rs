use serde::{Deserialize, Serialize};

pub const EQUALIZER_BAND_COUNT: usize = 10;
pub const DEFAULT_EBU_R128_TARGET_LUFS: f64 = -23.0;
pub const MIN_EBU_R128_TARGET_LUFS: f64 = -48.0;
pub const MAX_EBU_R128_TARGET_LUFS: f64 = 0.0;
pub const MIN_CROSSFADE_SECONDS: u8 = 1;
pub const MAX_CROSSFADE_SECONDS: u8 = 30;
pub const MIN_PLAYBACK_RATE: f64 = 0.5;
pub const MAX_PLAYBACK_RATE: f64 = 2.0;
pub const DEFAULT_PLAYBACK_RATE: f64 = 1.0;
pub const DEFAULT_AUTO_DJ_REFILL_THRESHOLD: u8 = 1;
pub const MIN_AUTO_DJ_REFILL_THRESHOLD: u8 = 1;
pub const MAX_AUTO_DJ_REFILL_THRESHOLD: u8 = 10;
const PERCEPTUAL_DB_RANGE: f64 = 50.0;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum StreamQuality {
    #[default]
    Original,
    MaxBitrateKbps(u32),
}

impl StreamQuality {
    pub const fn max_bitrate_kbps(self) -> Option<u32> {
        match self {
            Self::Original => None,
            Self::MaxBitrateKbps(kbps) => Some(kbps),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum PlaybackTransitionMode {
    #[default]
    #[serde(alias = "Default")]
    Gapless,
    Crossfade,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum LoudnessNormalization {
    Off,
    #[default]
    ReplayGain,
    EbuR128,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum LoudnessNormalizationScope {
    Track,
    #[default]
    Album,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum VolumeScale {
    #[default]
    Perceptual,
    Linear,
}

impl VolumeScale {
    pub fn gain(self, position: f64) -> f64 {
        let position = sanitize_volume(position);
        match self {
            Self::Perceptual if position == 0.0 => 0.0,
            Self::Perceptual => 10_f64.powf(PERCEPTUAL_DB_RANGE * (position - 1.0) / 20.0),
            Self::Linear => position,
        }
    }

    pub fn position_for_gain(self, gain: f64) -> f64 {
        let gain = sanitize_volume(gain);
        match self {
            Self::Perceptual if gain <= perceptual_gain_floor() => 0.0,
            Self::Perceptual => sanitize_volume(1.0 + 20.0 * gain.log10() / PERCEPTUAL_DB_RANGE),
            Self::Linear => gain,
        }
    }
}

fn perceptual_gain_floor() -> f64 {
    10_f64.powf(-PERCEPTUAL_DB_RANGE / 20.0)
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct EqualizerSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_equalizer_selected_preset")]
    pub selected_preset: String,
    #[serde(default = "default_equalizer_bands")]
    pub bands: Vec<f64>,
}

impl Default for EqualizerSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            selected_preset: "Flat".to_string(),
            bands: default_equalizer_bands(),
        }
    }
}

impl EqualizerSettings {
    pub fn sanitize(&mut self) {
        if self.selected_preset.trim().is_empty() {
            self.selected_preset = default_equalizer_selected_preset();
        }
        sanitize_equalizer_bands(&mut self.bands);
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PlaybackSettings {
    pub transition_mode: PlaybackTransitionMode,
    pub crossfade_seconds: u8,
    pub skip_same_album_crossfade: bool,
    pub audio_fade_on_status_change: bool,
    pub loudness_normalization: LoudnessNormalization,
    pub loudness_normalization_scope: LoudnessNormalizationScope,
    pub ebu_r128_target_lufs: f64,
    pub write_ebu_r128_tags: bool,
    pub stream_quality: StreamQuality,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_output: Option<String>,
    pub equalizer: EqualizerSettings,
    pub playback_rate: f64,
    pub preserve_pitch: bool,
    pub volume: f64,
    pub volume_scale: VolumeScale,
    pub muted: bool,
}

#[derive(Deserialize)]
struct SavedPlaybackSettings {
    #[serde(default)]
    transition_mode: PlaybackTransitionMode,
    #[serde(default = "default_crossfade_seconds")]
    crossfade_seconds: u8,
    #[serde(default)]
    skip_same_album_crossfade: bool,
    #[serde(default = "default_true")]
    audio_fade_on_status_change: bool,
    #[serde(default)]
    loudness_normalization: Option<String>,
    #[serde(default)]
    loudness_normalization_scope: Option<LoudnessNormalizationScope>,
    #[serde(default)]
    ebu_r128_target_lufs: Option<f64>,
    #[serde(default)]
    write_ebu_r128_tags: bool,
    #[serde(default, rename = "replay_gain")]
    legacy_replay_gain: Option<LoudnessNormalizationScope>,
    #[serde(default)]
    stream_quality: StreamQuality,
    #[serde(default)]
    audio_output: Option<String>,
    #[serde(default)]
    equalizer: EqualizerSettings,
    #[serde(default = "default_playback_rate")]
    playback_rate: f64,
    #[serde(default = "default_true")]
    preserve_pitch: bool,
    #[serde(default = "default_volume")]
    volume: f64,
    #[serde(default)]
    volume_scale: Option<VolumeScale>,
    #[serde(default)]
    muted: bool,
}

impl<'de> Deserialize<'de> for PlaybackSettings {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let saved = SavedPlaybackSettings::deserialize(deserializer)?;
        let legacy_gain = sanitize_volume(saved.volume);
        let (volume, volume_scale) = match saved.volume_scale {
            Some(scale) => (saved.volume, scale),
            None => (
                VolumeScale::Perceptual.position_for_gain(legacy_gain),
                VolumeScale::Perceptual,
            ),
        };
        let (loudness_normalization, loudness_normalization_scope) = restore_loudness_settings(
            saved.loudness_normalization.as_deref(),
            saved.loudness_normalization_scope,
            saved.legacy_replay_gain,
        );
        Ok(Self {
            transition_mode: saved.transition_mode,
            crossfade_seconds: saved.crossfade_seconds,
            skip_same_album_crossfade: saved.skip_same_album_crossfade,
            audio_fade_on_status_change: saved.audio_fade_on_status_change,
            loudness_normalization,
            loudness_normalization_scope,
            ebu_r128_target_lufs: saved
                .ebu_r128_target_lufs
                .unwrap_or(DEFAULT_EBU_R128_TARGET_LUFS),
            write_ebu_r128_tags: saved.write_ebu_r128_tags,
            stream_quality: saved.stream_quality,
            audio_output: saved.audio_output,
            equalizer: saved.equalizer,
            playback_rate: saved.playback_rate,
            preserve_pitch: saved.preserve_pitch,
            volume,
            volume_scale,
            muted: saved.muted,
        })
    }
}

impl Default for PlaybackSettings {
    fn default() -> Self {
        Self {
            transition_mode: PlaybackTransitionMode::Gapless,
            crossfade_seconds: default_crossfade_seconds(),
            skip_same_album_crossfade: false,
            audio_fade_on_status_change: true,
            loudness_normalization: LoudnessNormalization::ReplayGain,
            loudness_normalization_scope: LoudnessNormalizationScope::Album,
            ebu_r128_target_lufs: DEFAULT_EBU_R128_TARGET_LUFS,
            write_ebu_r128_tags: false,
            stream_quality: StreamQuality::Original,
            audio_output: None,
            equalizer: EqualizerSettings::default(),
            playback_rate: DEFAULT_PLAYBACK_RATE,
            preserve_pitch: true,
            volume: default_volume(),
            volume_scale: VolumeScale::Perceptual,
            muted: false,
        }
    }
}

impl PlaybackSettings {
    pub fn set_volume_scale_preserving_gain(&mut self, volume_scale: VolumeScale) {
        if self.volume_scale == volume_scale {
            return;
        }
        let gain = self.volume_scale.gain(self.volume);
        self.volume = volume_scale.position_for_gain(gain);
        self.volume_scale = volume_scale;
    }

    pub fn sanitize(&mut self) {
        self.crossfade_seconds = self
            .crossfade_seconds
            .clamp(MIN_CROSSFADE_SECONDS, MAX_CROSSFADE_SECONDS);
        self.playback_rate = sanitize_playback_rate(self.playback_rate);
        self.ebu_r128_target_lufs = sanitize_ebu_r128_target(self.ebu_r128_target_lufs);
        self.volume = sanitize_volume(self.volume);
        if self.audio_output.as_deref().is_some_and(|output| {
            output.trim().is_empty()
                || matches!(
                    output,
                    "autoaudiosink"
                        | "pipewiresink"
                        | "pulsesink"
                        | "alsasink"
                        | "jackaudiosink"
                        | "osxaudiosink"
                        | "wasapisink"
                        | "directsoundsink"
                )
        }) {
            self.audio_output = None;
        }
        self.equalizer.sanitize();
    }
}

fn restore_loudness_settings(
    saved: Option<&str>,
    scope: Option<LoudnessNormalizationScope>,
    legacy_replay_gain: Option<LoudnessNormalizationScope>,
) -> (LoudnessNormalization, LoudnessNormalizationScope) {
    match saved {
        Some("Off") => (
            LoudnessNormalization::Off,
            scope.or(legacy_replay_gain).unwrap_or_default(),
        ),
        Some("Track") => (
            LoudnessNormalization::EbuR128,
            LoudnessNormalizationScope::Track,
        ),
        Some("Album") => (
            LoudnessNormalization::EbuR128,
            LoudnessNormalizationScope::Album,
        ),
        Some("EbuR128") => (LoudnessNormalization::EbuR128, scope.unwrap_or_default()),
        Some("ReplayGain") => (
            LoudnessNormalization::ReplayGain,
            scope.or(legacy_replay_gain).unwrap_or_default(),
        ),
        _ if legacy_replay_gain.is_some() => (
            LoudnessNormalization::ReplayGain,
            legacy_replay_gain.unwrap_or_default(),
        ),
        _ => (LoudnessNormalization::ReplayGain, scope.unwrap_or_default()),
    }
}

fn sanitize_ebu_r128_target(target: f64) -> f64 {
    if target.is_finite() {
        target.clamp(MIN_EBU_R128_TARGET_LUFS, MAX_EBU_R128_TARGET_LUFS)
    } else {
        DEFAULT_EBU_R128_TARGET_LUFS
    }
}

fn default_true() -> bool {
    true
}

fn default_volume() -> f64 {
    1.0
}

fn default_playback_rate() -> f64 {
    DEFAULT_PLAYBACK_RATE
}

pub fn sanitize_playback_rate(rate: f64) -> f64 {
    if rate.is_finite() {
        rate.clamp(MIN_PLAYBACK_RATE, MAX_PLAYBACK_RATE)
    } else {
        DEFAULT_PLAYBACK_RATE
    }
}

fn sanitize_volume(volume: f64) -> f64 {
    if volume.is_finite() {
        volume.clamp(0.0, 1.0)
    } else {
        default_volume()
    }
}

fn default_crossfade_seconds() -> u8 {
    5
}

fn default_equalizer_bands() -> Vec<f64> {
    vec![0.0; EQUALIZER_BAND_COUNT]
}

fn default_equalizer_selected_preset() -> String {
    "Custom".to_string()
}

fn sanitize_equalizer_bands(bands: &mut Vec<f64>) {
    if bands.len() != EQUALIZER_BAND_COUNT {
        bands.resize(EQUALIZER_BAND_COUNT, 0.0);
    }
    for gain in bands {
        if !gain.is_finite() {
            *gain = 0.0;
        }
        *gain = gain.clamp(-12.0, 12.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_default_transition_migrates_to_gapless() {
        let mode = serde_json::from_str::<PlaybackTransitionMode>(r#""Default""#)
            .expect("deserialize the legacy transition mode");

        assert_eq!(mode, PlaybackTransitionMode::Gapless);
        assert_eq!(
            serde_json::to_string(&mode).expect("serialize the migrated transition mode"),
            r#""Gapless""#
        );
        assert_eq!(
            PlaybackSettings::default().transition_mode,
            PlaybackTransitionMode::Gapless
        );
    }

    #[test]
    fn playback_settings_clamp_crossfade_range() {
        let mut settings = PlaybackSettings {
            crossfade_seconds: 0,
            ..PlaybackSettings::default()
        };
        settings.sanitize();
        assert_eq!(settings.crossfade_seconds, MIN_CROSSFADE_SECONDS);

        settings.crossfade_seconds = MAX_CROSSFADE_SECONDS + 1;
        settings.sanitize();
        assert_eq!(settings.crossfade_seconds, MAX_CROSSFADE_SECONDS);
    }

    #[test]
    fn playback_settings_sanitize_playback_rate() {
        let mut settings = PlaybackSettings {
            playback_rate: 0.25,
            ..PlaybackSettings::default()
        };
        settings.sanitize();
        assert_eq!(settings.playback_rate, MIN_PLAYBACK_RATE);

        settings.playback_rate = 4.0;
        settings.sanitize();
        assert_eq!(settings.playback_rate, MAX_PLAYBACK_RATE);

        settings.playback_rate = f64::NAN;
        settings.sanitize();
        assert_eq!(settings.playback_rate, DEFAULT_PLAYBACK_RATE);
    }

    #[test]
    fn playback_settings_migrate_backend_factories_to_system_default() {
        for output in [
            "autoaudiosink",
            "pipewiresink",
            "pulsesink",
            "alsasink",
            "jackaudiosink",
            "osxaudiosink",
            "wasapisink",
            "directsoundsink",
        ] {
            let mut settings = PlaybackSettings {
                audio_output: Some(output.to_string()),
                ..PlaybackSettings::default()
            };

            settings.sanitize();

            assert_eq!(settings.audio_output, None, "legacy output {output}");
        }
    }

    #[test]
    fn missing_playback_rate_restores_normal_speed() {
        let restored = serde_json::from_str::<PlaybackSettings>("{}")
            .expect("restore playback settings without a playback rate");

        assert_eq!(restored.playback_rate, DEFAULT_PLAYBACK_RATE);
    }

    #[test]
    fn preserve_pitch_defaults_on_and_round_trips_off() {
        let restored = serde_json::from_str::<PlaybackSettings>("{}")
            .expect("restore playback settings without pitch preservation");
        assert!(restored.preserve_pitch);

        let settings = PlaybackSettings {
            preserve_pitch: false,
            ..PlaybackSettings::default()
        };
        let restored = serde_json::from_value::<PlaybackSettings>(
            serde_json::to_value(settings).expect("serialize pitch preservation"),
        )
        .expect("restore pitch preservation");
        assert!(!restored.preserve_pitch);
    }

    #[test]
    fn replay_gain_is_the_default_without_enabling_analysis() {
        assert_eq!(
            LoudnessNormalization::default(),
            LoudnessNormalization::ReplayGain
        );
        assert_eq!(
            PlaybackSettings::default().loudness_normalization,
            LoudnessNormalization::ReplayGain
        );

        let restored = serde_json::from_str::<PlaybackSettings>("{}")
            .expect("restore playback settings without loudness normalization");
        assert_eq!(
            restored.loudness_normalization,
            LoudnessNormalization::ReplayGain
        );
        assert_eq!(
            restored.loudness_normalization_scope,
            LoudnessNormalizationScope::Album
        );
        assert_eq!(restored.ebu_r128_target_lufs, DEFAULT_EBU_R128_TARGET_LUFS);
    }

    #[test]
    fn legacy_replay_gain_restores_replay_gain_scope() {
        let restored = serde_json::from_str::<PlaybackSettings>(r#"{"replay_gain":"Track"}"#)
            .expect("restore legacy ReplayGain setting");
        assert_eq!(
            restored.loudness_normalization,
            LoudnessNormalization::ReplayGain
        );
        assert_eq!(
            restored.loudness_normalization_scope,
            LoudnessNormalizationScope::Track
        );

        let saved = serde_json::to_value(restored).expect("serialize loudness normalization");
        assert_eq!(saved["loudness_normalization"], "ReplayGain");
        assert!(saved.get("replay_gain").is_none());
    }

    #[test]
    fn legacy_ebu_loudness_scope_migrates_to_the_ebu_mode() {
        let restored =
            serde_json::from_str::<PlaybackSettings>(r#"{"loudness_normalization":"Album"}"#)
                .expect("restore loudness normalization setting");
        assert_eq!(
            restored.loudness_normalization,
            LoudnessNormalization::EbuR128
        );
        assert_eq!(
            restored.loudness_normalization_scope,
            LoudnessNormalizationScope::Album
        );
    }

    #[test]
    fn explicit_ebu_settings_round_trip() {
        let settings = PlaybackSettings {
            loudness_normalization: LoudnessNormalization::EbuR128,
            loudness_normalization_scope: LoudnessNormalizationScope::Track,
            ebu_r128_target_lufs: -21.0,
            write_ebu_r128_tags: true,
            ..PlaybackSettings::default()
        };
        let restored = serde_json::from_value::<PlaybackSettings>(
            serde_json::to_value(settings).expect("serialize EBU settings"),
        )
        .expect("restore EBU settings");
        assert_eq!(
            restored.loudness_normalization,
            LoudnessNormalization::EbuR128
        );
        assert_eq!(
            restored.loudness_normalization_scope,
            LoudnessNormalizationScope::Track
        );
        assert_eq!(restored.ebu_r128_target_lufs, -21.0);
        assert!(restored.write_ebu_r128_tags);
    }

    #[test]
    fn perceptual_volume_uses_half_decibel_steps() {
        assert_eq!(VolumeScale::Perceptual.gain(0.0), 0.0);
        assert_eq!(VolumeScale::Perceptual.gain(1.0), 1.0);

        let mut previous_db = 20.0 * VolumeScale::Perceptual.gain(0.01).log10();
        assert!((previous_db + 49.5).abs() < 1e-12);
        for percent in 2..=100 {
            let gain = VolumeScale::Perceptual.gain(f64::from(percent) / 100.0);
            let db = 20.0 * gain.log10();
            assert!((db - previous_db - 0.5).abs() < 1e-12);
            previous_db = db;
        }

        for position in [0.01, 0.1, 0.5, 1.0] {
            let gain = VolumeScale::Perceptual.gain(position);
            assert!((VolumeScale::Perceptual.position_for_gain(gain) - position).abs() < 1e-12);
        }
        assert_eq!(VolumeScale::Perceptual.position_for_gain(0.0), 0.0);
    }

    #[test]
    fn legacy_linear_volume_migrates_to_perceptual_without_changing_gain() {
        let mut value =
            serde_json::to_value(PlaybackSettings::default()).expect("serialize settings");
        value
            .as_object_mut()
            .expect("playback settings object")
            .remove("volume_scale");
        value["volume"] = 0.5.into();

        let migrated =
            serde_json::from_value::<PlaybackSettings>(value).expect("migrate playback settings");

        assert_eq!(migrated.volume_scale, VolumeScale::Perceptual);
        assert!((migrated.volume_scale.gain(migrated.volume) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn explicit_perceptual_volume_round_trips_as_the_same_position() {
        let settings = PlaybackSettings {
            volume: 0.5,
            volume_scale: VolumeScale::Perceptual,
            ..PlaybackSettings::default()
        };
        let restored = serde_json::from_value::<PlaybackSettings>(
            serde_json::to_value(settings).expect("serialize perceptual volume"),
        )
        .expect("restore perceptual volume");

        assert_eq!(restored.volume_scale, VolumeScale::Perceptual);
        assert_eq!(restored.volume, 0.5);
        assert!(
            (restored.volume_scale.gain(restored.volume) - 0.056_234_132_519_034_91).abs() < 1e-12
        );
    }

    #[test]
    fn explicit_linear_volume_round_trips_without_migration() {
        let settings = PlaybackSettings {
            volume: 0.5,
            volume_scale: VolumeScale::Linear,
            ..PlaybackSettings::default()
        };
        let restored = serde_json::from_value::<PlaybackSettings>(
            serde_json::to_value(settings).expect("serialize linear volume"),
        )
        .expect("restore linear volume");

        assert_eq!(restored.volume_scale, VolumeScale::Linear);
        assert_eq!(restored.volume, 0.5);
        assert_eq!(restored.volume_scale.gain(restored.volume), 0.5);
    }

    #[test]
    fn changing_volume_scale_preserves_output_gain() {
        let mut settings = PlaybackSettings {
            volume: 0.5,
            volume_scale: VolumeScale::Linear,
            ..PlaybackSettings::default()
        };

        settings.set_volume_scale_preserving_gain(VolumeScale::Perceptual);
        assert!((settings.volume_scale.gain(settings.volume) - 0.5).abs() < 1e-12);

        settings.set_volume_scale_preserving_gain(VolumeScale::Linear);
        assert!((settings.volume - 0.5).abs() < 1e-12);
        assert!((settings.volume_scale.gain(settings.volume) - 0.5).abs() < 1e-12);
    }
}
