use serde::{Deserialize, Deserializer, Serialize};

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
    /// The format's name as a choice shows it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Opus => "Opus",
            Self::Aac => "AAC",
            Self::Mp3 => "MP3",
            Self::Vorbis => "Vorbis",
            Self::Flac => "FLAC",
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

/// The audio a download or a stream is asked for: the source's own, or an encoding of it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum AudioQuality {
    #[default]
    Original,
    Transcoded(AudioEncoding),
}

impl AudioQuality {
    pub fn encoding(self) -> Option<AudioEncoding> {
        match self {
            Self::Original => None,
            Self::Transcoded(encoding) => Some(encoding),
        }
    }
    pub fn extension(self) -> Option<&'static str> {
        self.encoding().map(|encoding| encoding.codec.extension())
    }
}

impl<'de> Deserialize<'de> for AudioQuality {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        enum Saved {
            #[serde(rename = "original", alias = "Original")]
            Original,
            #[serde(rename = "transcoded")]
            Transcoded(AudioEncoding),
            /// A bitrate limit saved before the format could be chosen.
            MaxBitrateKbps(u32),
        }
        Ok(match Saved::deserialize(deserializer)? {
            Saved::Original => Self::Original,
            Saved::Transcoded(encoding) => Self::Transcoded(encoding),
            Saved::MaxBitrateKbps(rate) => Self::Transcoded(AudioEncoding {
                codec: AudioCodec::Opus,
                bitrate_kbps: Some(rate),
                channels: AudioChannels::Preserve,
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_qualities_of_every_age_are_read() {
        let read = |saved: &str| serde_json::from_str::<AudioQuality>(saved).unwrap();
        let opus = |rate| {
            AudioQuality::Transcoded(AudioEncoding {
                codec: AudioCodec::Opus,
                bitrate_kbps: Some(rate),
                channels: AudioChannels::Preserve,
            })
        };
        assert_eq!(read(r#""Original""#), AudioQuality::Original);
        assert_eq!(read(r#""original""#), AudioQuality::Original);
        assert_eq!(read(r#"{"MaxBitrateKbps":192}"#), opus(192));
        let saved = serde_json::to_string(&opus(96)).unwrap();
        assert_eq!(
            saved,
            r#"{"transcoded":{"codec":"opus","bitrate_kbps":96,"channels":"preserve"}}"#
        );
        assert_eq!(read(&saved), opus(96));
    }
}
