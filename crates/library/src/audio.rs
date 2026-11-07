use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum AudioCodec {
    #[default]
    Opus,
    Aac,
    Mp3,
    Vorbis,
    Flac,
}

impl AudioCodec {
    pub fn name(self) -> &'static str {
        match self {
            Self::Opus => "opus",
            Self::Aac => "aac",
            Self::Mp3 => "mp3",
            Self::Vorbis => "vorbis",
            Self::Flac => "flac",
        }
    }
    pub fn extension(self) -> &'static str {
        if self == Self::Vorbis {
            "ogg"
        } else {
            self.name()
        }
    }
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Opus => "audio/ogg; codecs=opus",
            Self::Vorbis => "audio/ogg; codecs=vorbis",
            Self::Aac => "audio/aac",
            Self::Mp3 => "audio/mpeg",
            Self::Flac => "audio/flac",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum AudioChannels {
    #[default]
    Preserve,
    StereoLossy,
    Stereo,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AudioEncoding {
    pub codec: AudioCodec,
    pub bitrate_kbps: Option<u32>,
    #[serde(default)]
    pub channels: AudioChannels,
}

impl AudioEncoding {
    pub const fn mp3() -> Self {
        Self {
            codec: AudioCodec::Mp3,
            bitrate_kbps: None,
            channels: AudioChannels::Preserve,
        }
    }
    pub fn stereo(self) -> bool {
        self.channels == AudioChannels::Stereo
            || self.channels == AudioChannels::StereoLossy && self.codec != AudioCodec::Flac
    }
}
