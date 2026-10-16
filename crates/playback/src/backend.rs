use crate::{
    EqualizerSettings, LoudnessNormalization, LoudnessNormalizationScope, PlaybackSettings,
    StreamQuality, VolumeScale,
};
use audio_processing::ResolvedStream;
use library::LoudnessMeasurement;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StreamRequest {
    pub media_uri: String,
    pub quality: StreamQuality,
    #[serde(default)]
    pub session_identifier: Option<String>,
}

impl StreamRequest {
    pub fn original(media_uri: impl Into<String>) -> Self {
        Self::new(media_uri, StreamQuality::Original)
    }

    pub fn new(media_uri: impl Into<String>, quality: StreamQuality) -> Self {
        Self {
            media_uri: media_uri.into(),
            quality,
            session_identifier: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrackLoudness {
    pub track: Option<Box<LoudnessMeasurement>>,
    pub album: Option<Box<LoudnessMeasurement>>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RunId(u64);

#[cfg(feature = "bindings")]
uniffi::custom_type!(RunId, u64, {
    lower: |value| value.get(),
    try_lift: |value| Ok(RunId::new(value)),
});

impl RunId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for RunId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum NextTransition {
    #[default]
    Gapless,
    Crossfade {
        duration_millis: u64,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct PreparedStream {
    pub stream: Box<ResolvedStream>,
    pub loudness: Option<TrackLoudness>,
    pub row: Option<std::sync::Arc<crate::QueueRow>>,
    pub artwork_path: Option<Arc<PathBuf>>,
    pub allows_preloading: bool,
}

impl PreparedStream {
    pub fn new(stream: ResolvedStream, loudness: Option<TrackLoudness>) -> Self {
        Self {
            stream: Box::new(stream),
            loudness,
            row: None,
            artwork_path: None,
            allows_preloading: true,
        }
    }

    pub fn without_preloading(mut self) -> Self {
        self.allows_preloading = false;
        self
    }

    pub fn with_row(
        mut self,
        row: std::sync::Arc<crate::QueueRow>,
        content_type: Option<String>,
    ) -> Self {
        self.row = Some(row);
        if self.stream.content_type.is_none() {
            self.stream.content_type = content_type;
        }
        self
    }
}

impl From<ResolvedStream> for PreparedStream {
    fn from(stream: ResolvedStream) -> Self {
        Self::new(stream, Some(TrackLoudness::default()))
    }
}

impl Deref for PreparedStream {
    type Target = ResolvedStream;

    fn deref(&self) -> &Self::Target {
        &self.stream
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PreparedNext {
    pub run: RunId,
    pub stream: PreparedStream,
    pub transition: NextTransition,
}

impl PreparedNext {
    pub fn new(run: RunId, stream: impl Into<PreparedStream>, transition: NextTransition) -> Self {
        Self {
            run,
            stream: stream.into(),
            transition,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BackendAudioSettings {
    pub loudness_normalization: LoudnessNormalization,
    pub loudness_normalization_scope: LoudnessNormalizationScope,
    pub ebu_r128_target_lufs: f64,
    pub audio_output: Option<String>,
    pub equalizer: EqualizerSettings,
    pub preserve_pitch: bool,
    pub volume: f64,
    pub volume_scale: VolumeScale,
    pub muted: bool,
    pub fade_on_status_change: bool,
}

impl Default for BackendAudioSettings {
    fn default() -> Self {
        Self::from(PlaybackSettings::default())
    }
}

impl BackendAudioSettings {
    pub fn output_gain(&self) -> f64 {
        self.volume_scale.gain(self.volume)
    }
}

impl From<PlaybackSettings> for BackendAudioSettings {
    fn from(mut settings: PlaybackSettings) -> Self {
        settings.sanitize();
        Self {
            loudness_normalization: settings.loudness_normalization,
            loudness_normalization_scope: settings.loudness_normalization_scope,
            ebu_r128_target_lufs: settings.ebu_r128_target_lufs,
            audio_output: settings.audio_output,
            equalizer: settings.equalizer,
            preserve_pitch: settings.preserve_pitch,
            volume: settings.volume,
            volume_scale: settings.volume_scale,
            muted: settings.muted,
            fade_on_status_change: settings.audio_fade_on_status_change,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BackendState {
    #[default]
    Stopped,
    Buffering,
    Paused,
    Playing,
}
#[derive(Clone, Debug, PartialEq)]
pub enum BackendCommand {
    Start {
        run: RunId,
        current: PreparedStream,
        next: Option<PreparedNext>,
        start_position_millis: u64,
        playback_rate: f64,
    },
    PrepareNext {
        current_run: RunId,
        next: Option<PreparedNext>,
    },
    Play {
        run: RunId,
    },
    Pause {
        run: RunId,
    },
    Stop {
        run: RunId,
    },
    Seek {
        run: RunId,
        position_millis: u64,
    },
    SetOutputVolume {
        volume: f64,
        volume_scale: VolumeScale,
        muted: bool,
    },
    ConfigureAudio(BackendAudioSettings),
    UpdateLoudness {
        run: RunId,
        loudness: TrackLoudness,
    },
    SetPlaybackRate(f64),
    SetVisualizerEnabled(bool),
}

impl BackendCommand {
    pub fn run(&self) -> Option<RunId> {
        match self {
            Self::Start { run, .. }
            | Self::Play { run }
            | Self::Pause { run }
            | Self::Stop { run }
            | Self::Seek { run, .. }
            | Self::UpdateLoudness { run, .. } => Some(*run),
            Self::PrepareNext { current_run, .. } => Some(*current_run),
            Self::SetOutputVolume { .. }
            | Self::ConfigureAudio(_)
            | Self::SetPlaybackRate(_)
            | Self::SetVisualizerEnabled(_) => None,
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub enum BackendEvent {
    Started {
        run: RunId,
    },
    State {
        run: RunId,
        state: BackendState,
    },
    /// Transport observed on a receiver, including changes made on the receiver.
    TransportObserved {
        run: RunId,
        state: BackendState,
    },
    Position {
        run: RunId,
        millis: u64,
    },
    Duration {
        run: RunId,
        millis: u64,
    },
    Seekable {
        run: RunId,
        seekable: bool,
    },
    Buffering {
        run: RunId,
        percent: u8,
    },
    Ended {
        run: RunId,
    },
    Transitioned {
        old_run: RunId,
        new_run: RunId,
    },
    NextNeeded {
        run: RunId,
    },
    NextPreparationFailed {
        current_run: RunId,
        next_run: RunId,
        error: BackendFailure,
    },
    AudioApplied {
        volume: f64,
        muted: bool,
        output: Option<String>,
    },
    Visualizer {
        run: RunId,
        levels: Vec<f64>,
    },
    /// The backend can no longer play this run.
    Error {
        run: RunId,
        error: BackendFailure,
    },
    /// A command failed while the current playback remains usable.
    OperationFailed {
        run: Option<RunId>,
        error: BackendFailure,
    },
    /// Play or Pause was rejected while the current playback remains usable.
    TransportRejected {
        run: RunId,
        requested_playing: bool,
        error: BackendFailure,
    },
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioOutput {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CastNetwork {
    pub id: String,
    pub name: String,
    pub address: IpAddr,
}
#[derive(Debug, Error)]
pub enum BackendError {
    #[error("playback backend failed: {0}")]
    Backend(String),
    #[error("playback command channel closed")]
    ChannelClosed,
}
#[derive(Clone, Debug, Error, PartialEq)]
#[error("{message}")]
pub struct BackendFailure {
    message: String,
}

impl BackendFailure {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

pub trait PlaybackBackend: Send {
    fn send(&mut self, command: BackendCommand) -> Result<(), BackendError>;
    fn drain_events(&mut self) -> Vec<BackendEvent>;

    fn shutdown(&mut self) -> Result<(), BackendError> {
        Ok(())
    }
}
