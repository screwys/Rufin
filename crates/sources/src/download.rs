use audio_processing::ResolvedStream;
use library::{AudioCodec, AudioEncoding};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum DownloadQuality {
    #[default]
    Original,
    Transcoded(AudioEncoding),
}

impl DownloadQuality {
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolvedDownload {
    Ready {
        stream: ResolvedStream,
        codec: Option<AudioCodec>,
    },
    Encode {
        stream: ResolvedStream,
        encoding: AudioEncoding,
    },
}

impl ResolvedDownload {
    pub(crate) fn ready(stream: ResolvedStream, codec: Option<AudioCodec>) -> Self {
        Self::Ready { stream, codec }
    }
    pub(crate) fn encode(stream: ResolvedStream, quality: DownloadQuality) -> Self {
        match quality.encoding() {
            Some(encoding) => Self::Encode {
                stream: stream.without_window(),
                encoding,
            },
            None => Self::ready(stream, None),
        }
    }
}

pub fn downloaded_codec(path: &std::path::Path) -> Option<AudioCodec> {
    use lofty::file::FileType;
    match crate::file::lofty::probe_file_type(path)? {
        FileType::Opus => Some(AudioCodec::Opus),
        FileType::Vorbis => Some(AudioCodec::Vorbis),
        FileType::Mpeg => Some(AudioCodec::Mp3),
        FileType::Aac => Some(AudioCodec::Aac),
        FileType::Flac => Some(AudioCodec::Flac),
        _ => None,
    }
}
