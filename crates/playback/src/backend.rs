use crate::{
    EqualizerSettings, LoudnessNormalization, LoudnessNormalizationScope, PlaybackSettings,
    StreamQuality, VolumeScale,
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
    pub media_uri: String,
    pub quality: StreamQuality,
}

impl StreamRequest {
    pub fn original(media_uri: impl Into<String>) -> Self {
        Self::new(media_uri, StreamQuality::Original)
    }

    pub fn new(media_uri: impl Into<String>, quality: StreamQuality) -> Self {
        Self {
            media_uri: media_uri.into(),
            quality,
        }
    }

    pub fn for_item(item: &crate::QueueItem, quality: StreamQuality) -> Self {
        Self::new(item.media_uri.clone(), quality)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamWindow {
    pub start_millis: u64,
    pub end_millis: u64,
}

#[derive(Clone)]
pub struct ResolvedStream {
    pub content_type: Option<String>,
    uri: String,
    redacted_uri: String,
    trust_invalid_certificate: bool,
    window: Option<StreamWindow>,
    resource: Option<std::sync::Arc<dyn Send + Sync>>,
}

impl PartialEq for ResolvedStream {
    fn eq(&self, other: &Self) -> bool {
        self.uri == other.uri
            && self.redacted_uri == other.redacted_uri
            && self.trust_invalid_certificate == other.trust_invalid_certificate
            && self.window == other.window
            && self.content_type == other.content_type
    }
}

impl Eq for ResolvedStream {}

impl ResolvedStream {
    pub fn new(uri: impl Into<String>) -> Self {
        let uri = uri.into();
        Self {
            redacted_uri: redact_sensitive_uri(&uri),
            uri,
            trust_invalid_certificate: false,
            window: None,
            resource: None,
            content_type: None,
        }
    }

    pub fn with_redacted(uri: impl Into<String>, redacted_uri: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            redacted_uri: redacted_uri.into(),
            trust_invalid_certificate: false,
            window: None,
            resource: None,
            content_type: None,
        }
    }

    pub fn with_trust_invalid_certificate(mut self, trust: bool) -> Self {
        self.trust_invalid_certificate = trust;
        self
    }

    pub fn with_content_type(mut self, content_type: Option<String>) -> Self {
        self.content_type = content_type;
        self
    }

    /// Retain temporary input or its serving endpoint while this stream is in use.
    /// Resource ownership does not participate in stream identity or equality.
    pub fn with_resource(mut self, resource: std::sync::Arc<dyn Send + Sync>) -> Self {
        self.resource = Some(resource);
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
    pub track: Option<Box<LoudnessMeasurement>>,
    pub album: Option<Box<LoudnessMeasurement>>,
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
    pub occurrence: Option<std::sync::Arc<crate::QueueOccurrence>>,
    pub artwork_path: Option<Arc<PathBuf>>,
    pub allows_preloading: bool,
}

impl PreparedStream {
    pub fn new(stream: ResolvedStream, loudness: TrackLoudness) -> Self {
        Self {
            stream: Box::new(stream),
            loudness,
            occurrence: None,
            artwork_path: None,
            allows_preloading: true,
        }
    }

    pub fn without_preloading(mut self) -> Self {
        self.allows_preloading = false;
        self
    }

    pub fn with_occurrence(
        mut self,
        occurrence: std::sync::Arc<crate::QueueOccurrence>,
        content_type: Option<String>,
    ) -> Self {
        self.occurrence = Some(occurrence);
        if self.stream.content_type.is_none() {
            self.stream.content_type = content_type;
        }
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
