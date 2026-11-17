use library::{AudioChannels, AudioCodec, AudioEncoding};
use sources::AudioQuality;

pub const AUDIO_BITRATES: &[u32] = &[
    64, 80, 96, 112, 128, 144, 160, 176, 192, 208, 224, 240, 256, 272, 288, 304, 320,
];
/// The rate a transcoded download takes when none was chosen.
pub const DEFAULT_AUDIO_BITRATE: u32 = 128;

/// The quality with another format: `None` keeps the original. FLAC has no rate; another
/// codec keeps the chosen rate, or takes the default one.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn audio_quality_with_codec(quality: AudioQuality, codec: Option<AudioCodec>) -> AudioQuality {
    let Some(codec) = codec else {
        return AudioQuality::Original;
    };
    let previous = quality.encoding();
    AudioQuality::Transcoded(AudioEncoding {
        codec,
        bitrate_kbps: (codec != AudioCodec::Flac).then(|| {
            previous
                .and_then(|encoding| encoding.bitrate_kbps)
                .unwrap_or(DEFAULT_AUDIO_BITRATE)
        }),
        channels: previous.map_or(AudioChannels::Preserve, |encoding| encoding.channels),
    })
}

/// The quality at another rate; the original and FLAC have none.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn audio_quality_with_bitrate(quality: AudioQuality, bitrate_kbps: u32) -> AudioQuality {
    match quality {
        AudioQuality::Transcoded(encoding) if encoding.codec != AudioCodec::Flac => {
            AudioQuality::Transcoded(AudioEncoding {
                bitrate_kbps: Some(bitrate_kbps),
                ..encoding
            })
        }
        quality => quality,
    }
}

/// The quality with other channels; the original keeps its own.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn audio_quality_with_channels(quality: AudioQuality, channels: AudioChannels) -> AudioQuality {
    match quality {
        AudioQuality::Transcoded(encoding) => AudioQuality::Transcoded(AudioEncoding {
            channels,
            ..encoding
        }),
        AudioQuality::Original => AudioQuality::Original,
    }
}

/// Whether the quality has a rate to choose.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn audio_quality_shows_bitrate(quality: AudioQuality) -> bool {
    quality
        .encoding()
        .is_some_and(|encoding| encoding.codec != AudioCodec::Flac)
}

/// Whether the quality has channels to choose.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn audio_quality_shows_channels(quality: AudioQuality) -> bool {
    quality.encoding().is_some()
}

/// The format's name as a choice shows it.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn audio_codec_label(codec: AudioCodec) -> String {
    codec.label().into()
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn audio_bitrates() -> Vec<u32> {
    AUDIO_BITRATES.to_vec()
}

/// The bitrates labelled under the bitrate slider.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn audio_bitrate_marks() -> Vec<u32> {
    AUDIO_BITRATES.iter().step_by(4).copied().collect()
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn default_audio_bitrate() -> u32 {
    DEFAULT_AUDIO_BITRATE
}
