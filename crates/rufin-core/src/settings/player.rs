use super::{
    SettingsOwner,
    visualizer::{self, PreferenceIndex},
};
use playback::{
    LoudnessNormalization, LoudnessNormalizationScope, PlaybackSettings, PlaybackTransitionMode,
    StreamQuality, VolumeScale,
};

#[cfg_attr(feature = "bindings", derive(uniffi::Error))]
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("{reason}")]
    Message { reason: String },
}

impl From<String> for SettingsError {
    fn from(message: String) -> Self {
        Self::Message { reason: message }
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Clone, Debug)]
pub struct PlayerPreferences {
    pub application: super::ApplicationPreferences,
    pub playback: PlaybackSettings,
    pub lyrics: lyrics::UserPreferences,
    pub visualizer: visualizer::VisualizerSettings,
    pub display: super::PlayerDisplayPreferences,
    pub right_panel: super::right_panel::RightPanelSettings,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PlayerPreferenceOptions {
    pub equalizer_band_titles: Vec<String>,
    pub equalizer_presets: Vec<String>,
    pub lyrics_scroll_pause_millis: u64,
    pub visualizer_stale_micros: u64,
    pub min_crossfade_seconds: u8,
    pub max_crossfade_seconds: u8,
    pub min_playback_rate: f64,
    pub max_playback_rate: f64,
    pub min_ebu_r128_target_lufs: f64,
    pub max_ebu_r128_target_lufs: f64,
    pub min_auto_dj_refill_threshold: u8,
    pub max_auto_dj_refill_threshold: u8,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl SettingsOwner {
    pub fn player_preferences(&self) -> PlayerPreferences {
        let stored = self
            .file
            .value
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let ui = &stored.ui;
        let mut playback = ui.playback.clone();
        playback.equalizer.selected_preset =
            playback::equalizer_selected_preset(&playback.equalizer);
        let mut visualizer = ui.visualizer.clone();
        visualizer.selected_preset = Some(visualizer.selected_preset());
        PlayerPreferences {
            application: ui.application.clone(),
            playback,
            lyrics: ui.lyrics.preferences.clone(),
            visualizer,
            display: ui.player_display.clone(),
            right_panel: ui.right_panel.clone(),
        }
    }

    pub fn cast_network_interface(&self) -> Option<String> {
        self.load().cast_network_interface
    }

    pub fn set_cast_network_interface(&self, value: Option<String>) -> Result<(), SettingsError> {
        self.update_preferences(move |ui| {
            ui.cast_network_interface = value;
            Ok(())
        })
        .map_err(Into::into)
    }

    pub fn player_preference_options(&self) -> PlayerPreferenceOptions {
        PlayerPreferenceOptions {
            equalizer_band_titles: (0..playback::EQUALIZER_BAND_COUNT)
                .map(playback::equalizer_band_title)
                .collect(),
            equalizer_presets: playback::equalizer_preset_names()
                .into_iter()
                .map(str::to_owned)
                .collect(),
            lyrics_scroll_pause_millis: lyrics::LYRICS_USER_SCROLL_PAUSE_MS,
            visualizer_stale_micros: visualizer::VISUALIZER_STALE_MICROS as u64,
            min_crossfade_seconds: playback::MIN_CROSSFADE_SECONDS,
            max_crossfade_seconds: playback::MAX_CROSSFADE_SECONDS,
            min_playback_rate: playback::MIN_PLAYBACK_RATE,
            max_playback_rate: playback::MAX_PLAYBACK_RATE,
            min_ebu_r128_target_lufs: playback::MIN_EBU_R128_TARGET_LUFS,
            max_ebu_r128_target_lufs: playback::MAX_EBU_R128_TARGET_LUFS,
            min_auto_dj_refill_threshold: playback::MIN_AUTO_DJ_REFILL_THRESHOLD,
            max_auto_dj_refill_threshold: playback::MAX_AUTO_DJ_REFILL_THRESHOLD,
        }
    }

    pub fn set_volume_scale(&self, value: VolumeScale) -> Result<(), SettingsError> {
        self.update_preferences(move |ui| {
            ui.playback.set_volume_scale_preserving_gain(value);
            Ok(())
        })
        .map_err(Into::into)
    }

    pub fn set_right_panel_combined(&self, value: bool) -> Result<(), SettingsError> {
        self.update_preferences(move |ui| {
            ui.right_panel.combined = value;
            Ok(())
        })
        .map_err(Into::into)
    }

    pub fn set_equalizer_enabled(&self, enabled: bool) -> Result<(), SettingsError> {
        self.restore(
            move |stored| {
                stored.ui.playback.equalizer.enabled = enabled;
                Ok(())
            },
            false,
        )
        .map_err(Into::into)
    }

    pub fn select_equalizer_preset(&self, preset: String) -> Result<(), SettingsError> {
        self.update_preferences(move |ui| {
            ui.playback.equalizer.select_preset(preset);
            Ok(())
        })
        .map_err(Into::into)
    }

    pub fn set_equalizer_band(
        &self,
        index: PreferenceIndex,
        value: f64,
    ) -> Result<(), SettingsError> {
        self.update_preferences(move |ui| {
            let equalizer = &mut ui.playback.equalizer;
            let band = equalizer
                .bands
                .get_mut(index)
                .ok_or_else(|| localization::tr("This isn't available"))?;
            *band = value;
            equalizer.mark_custom();
            Ok(())
        })
        .map_err(Into::into)
    }

    pub fn set_visualizer_appearance(
        &self,
        value: visualizer::VisualizerAppearance,
    ) -> Result<(), SettingsError> {
        self.restore(
            move |stored| {
                stored.ui.visualizer.selected_preset = Some(stored.ui.visualizer.selected_preset());
                stored.ui.visualizer.appearance = value;
                Ok(())
            },
            false,
        )
        .map_err(Into::into)
    }

    pub fn set_visualizer_frame_limit(&self, value: u32) -> Result<(), SettingsError> {
        self.restore(
            move |stored| {
                stored.ui.visualizer.fps_limit = value;
                Ok(())
            },
            false,
        )
        .map_err(Into::into)
    }

    pub fn update_visualizer_preset(
        &self,
        slot: PreferenceIndex,
        save: bool,
    ) -> Result<(), SettingsError> {
        self.restore(
            move |stored| {
                if save {
                    stored.ui.visualizer.save_preset(slot);
                } else {
                    stored.ui.visualizer.select_preset(slot);
                }
                Ok(())
            },
            false,
        )
        .map_err(Into::into)
    }

    pub fn copy_visualizer_preset(&self) -> String {
        visualizer::VisualizerPreset {
            appearance: self.visualizer_render_settings().1,
        }
        .to_json()
    }

    pub fn paste_visualizer_preset(&self, text: String) -> Result<(), SettingsError> {
        let preset = visualizer::VisualizerPreset::from_json(&text)
            .ok_or_else(|| SettingsError::from(localization::tr("This isn't available")))?;
        self.set_visualizer_appearance(preset.appearance)
    }

    pub fn reset_visualizer_appearance(&self) -> Result<(), SettingsError> {
        self.set_visualizer_appearance(Default::default())
    }
}

macro_rules! preference_target {
    ($ui:ident ($($path:ident).+)) => { $ui.$($path).+ };
}

macro_rules! native_preference_setters {
    ([$ui:ident $path:tt $before:block]
        $( $(#[$attribute:meta])* $field:ident: $ty:ty = $default:expr $(=> $setter:ident)?; )*) => {
        #[cfg_attr(feature = "bindings", uniffi::export)]
        impl SettingsOwner {
            $($(pub fn $setter(&self, value: $ty) -> Result<(), SettingsError> {
                self.update_preferences(move |$ui| {
                    $before;
                    (preference_target!($ui $path)).$field = value;
                    Ok(())
                }).map_err(Into::into)
            })?)*
        }
    };
    ([$($args:tt)*] direct { $($direct:tt)* } migrated { $($migrated:tt)* }) => {
        native_preference_setters! { [$($args)*] $($direct)* $($migrated)* }
    };
}

use lyrics::ExternalLyricsProvider;
use visualizer::{VisualizerColors, VisualizerStyle};
playback::playback_preference_fields!(native_preference_setters, ui, (playback), {});
lyrics::lyrics_preference_fields!(native_preference_setters, ui, (lyrics.preferences), {});
visualizer::visualizer_preference_fields!(
    native_preference_setters,
    ui,
    (visualizer.appearance),
    {
        ui.visualizer.selected_preset = Some(ui.visualizer.selected_preset());
    }
);
