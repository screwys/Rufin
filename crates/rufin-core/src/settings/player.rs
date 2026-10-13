use super::{
    SettingsOwner,
    visualizer::{self, PreferenceIndex, VisualizerPreferenceEdit},
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
    pub private_mode: bool,
    pub playback: PlaybackSettings,
    pub lyrics: lyrics::UserPreferences,
    pub visualizer: visualizer::VisualizerSettings,
    pub clear_queue_includes_current: bool,
    pub fullscreen_dynamic_background: bool,
    pub fullscreen_background_image: bool,
    pub fullscreen_lyrics_visible: bool,
    pub fullscreen_current_lyrics_line_visible: bool,
    pub fullscreen_visualizer_visible: bool,
    pub right_panel_combined: bool,
    pub seekbar_waveform_enabled: bool,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PlayerPreferenceOptions {
    pub equalizer_band_titles: Vec<String>,
    pub equalizer_presets: Vec<String>,
    pub lyrics_scroll_pause_millis: u64,
    pub visualizer_stale_micros: u64,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
#[derive(Clone, Debug)]
pub enum PlayerPreferenceEdit {
    TransitionMode { value: PlaybackTransitionMode },
    CrossfadeSeconds { value: u8 },
    SkipSameAlbumCrossfade { value: bool },
    AudioFadeOnStatusChange { value: bool },
    LoudnessNormalization { value: LoudnessNormalization },
    LoudnessNormalizationScope { value: LoudnessNormalizationScope },
    EbuR128TargetLufs { value: f64 },
    WriteEbuR128Tags { value: bool },
    StreamQuality { value: StreamQuality },
    PlaybackRate { value: f64 },
    PreservePitch { value: bool },
    VolumeScale { value: VolumeScale },
    ClearQueueIncludesCurrent { value: bool },
    FullscreenDynamicBackground { value: bool },
    FullscreenBackgroundImage { value: bool },
    FullscreenLyricsVisible { value: bool },
    FullscreenCurrentLyricsLineVisible { value: bool },
    FullscreenVisualizerVisible { value: bool },
    RightPanelCombined { value: bool },
    SeekbarWaveformEnabled { value: bool },
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
            private_mode: ui.private_mode,
            playback,
            lyrics: ui.lyrics.preferences.clone(),
            visualizer,
            clear_queue_includes_current: ui.clear_queue_includes_current,
            fullscreen_dynamic_background: ui.fullscreen_dynamic_background,
            fullscreen_background_image: ui.fullscreen_background_image,
            fullscreen_lyrics_visible: ui.fullscreen_lyrics_visible,
            fullscreen_current_lyrics_line_visible: ui.fullscreen_current_lyrics_line_visible,
            fullscreen_visualizer_visible: ui.fullscreen_visualizer_visible,
            right_panel_combined: ui.right_panel.combined,
            seekbar_waveform_enabled: ui.seekbar_waveform_enabled,
        }
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
        }
    }

    pub fn edit_player_preferences(&self, edit: PlayerPreferenceEdit) -> Result<(), SettingsError> {
        self.update_preferences(move |ui| {
            match edit {
                PlayerPreferenceEdit::TransitionMode { value } => {
                    ui.playback.transition_mode = value
                }
                PlayerPreferenceEdit::CrossfadeSeconds { value } => {
                    ui.playback.crossfade_seconds = value
                }
                PlayerPreferenceEdit::SkipSameAlbumCrossfade { value } => {
                    ui.playback.skip_same_album_crossfade = value
                }
                PlayerPreferenceEdit::AudioFadeOnStatusChange { value } => {
                    ui.playback.audio_fade_on_status_change = value
                }
                PlayerPreferenceEdit::LoudnessNormalization { value } => {
                    ui.playback.loudness_normalization = value
                }
                PlayerPreferenceEdit::LoudnessNormalizationScope { value } => {
                    ui.playback.loudness_normalization_scope = value
                }
                PlayerPreferenceEdit::EbuR128TargetLufs { value } => {
                    ui.playback.ebu_r128_target_lufs = value
                }
                PlayerPreferenceEdit::WriteEbuR128Tags { value } => {
                    ui.playback.write_ebu_r128_tags = value
                }
                PlayerPreferenceEdit::StreamQuality { value } => ui.playback.stream_quality = value,
                PlayerPreferenceEdit::PlaybackRate { value } => ui.playback.playback_rate = value,
                PlayerPreferenceEdit::PreservePitch { value } => ui.playback.preserve_pitch = value,
                PlayerPreferenceEdit::VolumeScale { value } => {
                    ui.playback.set_volume_scale_preserving_gain(value)
                }
                PlayerPreferenceEdit::ClearQueueIncludesCurrent { value } => {
                    ui.clear_queue_includes_current = value
                }
                PlayerPreferenceEdit::FullscreenDynamicBackground { value } => {
                    ui.fullscreen_dynamic_background = value
                }
                PlayerPreferenceEdit::FullscreenBackgroundImage { value } => {
                    ui.fullscreen_background_image = value
                }
                PlayerPreferenceEdit::FullscreenLyricsVisible { value } => {
                    ui.fullscreen_lyrics_visible = value
                }
                PlayerPreferenceEdit::FullscreenCurrentLyricsLineVisible { value } => {
                    ui.fullscreen_current_lyrics_line_visible = value
                }
                PlayerPreferenceEdit::FullscreenVisualizerVisible { value } => {
                    ui.fullscreen_visualizer_visible = value
                }
                PlayerPreferenceEdit::RightPanelCombined { value } => {
                    ui.right_panel.combined = value
                }
                PlayerPreferenceEdit::SeekbarWaveformEnabled { value } => {
                    ui.seekbar_waveform_enabled = value
                }
            }
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

    pub fn edit_lyrics_preferences(
        &self,
        edit: lyrics::LyricsPreferenceEdit,
    ) -> Result<(), SettingsError> {
        self.update_preferences(move |ui| {
            ui.lyrics.preferences.apply(edit);
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

    pub fn edit_visualizer_preferences(
        &self,
        edit: VisualizerPreferenceEdit,
    ) -> Result<(), SettingsError> {
        self.update_preferences(move |ui| {
            ui.visualizer.selected_preset = Some(ui.visualizer.selected_preset());
            ui.visualizer.appearance.apply(edit);
            Ok(())
        })
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
