use audio_processing::ResolvedStream;
use library::{AudioCodec, AudioEncoding, AudioQuality};

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
    pub(crate) fn encode(stream: ResolvedStream, quality: AudioQuality) -> Self {
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
