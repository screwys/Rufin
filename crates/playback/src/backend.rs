use crate::{
    EqualizerSettings, LoudnessNormalizationMode, PlaybackSettings, StreamQuality, VolumeScale,
};
use library::LoudnessMeasurement;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::net::IpAddr;
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StreamRequest {
    pub track_object_id: String,
    pub quality: StreamQuality,
    pub media_uri: Option<String>,
    pub cue_start_millis: Option<u64>,
    pub cue_end_millis: Option<u64>,
}

impl StreamRequest {
    pub fn original(track_object_id: impl Into<String>) -> Self {
        Self::new(track_object_id, StreamQuality::Original)
    }

    pub fn new(track_object_id: impl Into<String>, quality: StreamQuality) -> Self {
        Self {
            track_object_id: track_object_id.into(),
            quality,
            media_uri: None,
            cue_start_millis: None,
            cue_end_millis: None,
        }
    }

    pub fn for_media(media: &crate::PlaybackMedia, quality: StreamQuality) -> Self {
        Self {
            track_object_id: media.track_object_id.clone(),
            quality,
            media_uri: media.media_uri.clone(),
            cue_start_millis: media
                .cue_start_millis
                .and_then(|value| u64::try_from(value).ok()),
            cue_end_millis: media
                .cue_end_millis
                .and_then(|value| u64::try_from(value).ok()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamWindow {
    pub start_millis: u64,
    pub end_millis: u64,
}

#[derive(Clone, Eq, PartialEq)]
pub struct ResolvedStream {
    uri: String,
    redacted_uri: String,
    trust_invalid_certificate: bool,
    window: Option<StreamWindow>,
}

impl ResolvedStream {
    pub fn new(uri: impl Into<String>) -> Self {
        let uri = uri.into();
        Self {
            redacted_uri: redact_sensitive_uri(&uri),
            uri,
            trust_invalid_certificate: false,
            window: None,
        }
    }

    pub fn with_redacted(uri: impl Into<String>, redacted_uri: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            redacted_uri: redacted_uri.into(),
            trust_invalid_certificate: false,
            window: None,
        }
    }

    pub fn with_trust_invalid_certificate(mut self, trust: bool) -> Self {
        self.trust_invalid_certificate = trust;
        self
    }

    pub fn with_window(mut self, start_millis: u64, end_millis: u64) -> Self {
        if end_millis > start_millis {
            self.window = Some(StreamWindow {
                start_millis,
                end_millis,
            });
        }
        self
    }

    pub fn uri(&self) -> &str {
        &self.uri
    }
    pub fn redacted_uri(&self) -> &str {
        &self.redacted_uri
    }
    pub fn trust_invalid_certificate(&self) -> bool {
        self.trust_invalid_certificate
    }
    pub fn start_millis(&self) -> u64 {
        self.window.as_ref().map_or(0, |window| window.start_millis)
    }
    pub fn end_millis(&self) -> Option<u64> {
        self.window.as_ref().map(|window| window.end_millis)
    }
    pub fn window(&self) -> Option<&StreamWindow> {
        self.window.as_ref()
    }
}

impl fmt::Debug for ResolvedStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedStream")
            .field("uri", &self.redacted_uri)
            .field("window", &self.window)
            .finish()
    }
}

fn redact_sensitive_uri(uri: &str) -> String {
    let Some((base, query)) = uri.split_once('?') else {
        return uri.to_string();
    };
    let query = query
        .split('&')
        .map(|pair| {
            let Some((key, value)) = pair.split_once('=') else {
                return pair.to_string();
            };
            let lower = key.to_ascii_lowercase();
            if lower.contains("token") || lower.contains("key") {
                format!("{key}=<redacted>")
            } else {
                format!("{key}={value}")
            }
        })
        .collect::<Vec<_>>()
        .join("&");
    format!("{base}?{query}")
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrackLoudness {
    pub track: Option<LoudnessMeasurement>,
    pub album: Option<LoudnessMeasurement>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RunId(u64);

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
    pub loudness: TrackLoudness,
    pub track: Option<Box<crate::PlaybackMedia>>,
    pub content_type: Option<String>,
    pub artwork_path: Option<Arc<PathBuf>>,
    pub allows_preloading: bool,
    pub allows_timing_queries: bool,
}

impl PreparedStream {
    pub fn new(stream: ResolvedStream, loudness: TrackLoudness) -> Self {
        Self {
            stream: Box::new(stream),
            loudness,
            track: None,
            content_type: None,
            artwork_path: None,
            allows_preloading: true,
            allows_timing_queries: true,
        }
    }

    pub fn without_preloading(mut self) -> Self {
        self.allows_preloading = false;
        self
    }

    pub fn without_timing_queries(mut self) -> Self {
        self.allows_timing_queries = false;
        self
    }

    pub fn with_media(mut self, track: crate::PlaybackMedia, content_type: Option<String>) -> Self {
        self.track = Some(Box::new(track));
        self.content_type = content_type;
        self
    }

    pub fn with_artwork_path(mut self, artwork_path: Option<PathBuf>) -> Self {
        self.artwork_path = artwork_path.map(Arc::new);
        self
    }
}

impl From<ResolvedStream> for PreparedStream {
    fn from(stream: ResolvedStream) -> Self {
        Self::new(stream, TrackLoudness::default())
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
    pub loudness_normalization: LoudnessNormalizationMode,
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
            | Self::Seek { run, .. } => Some(*run),
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
    Error {
        run: RunId,
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
