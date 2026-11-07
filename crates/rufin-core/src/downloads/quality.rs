use library::{AudioChannels, AudioCodec, AudioEncoding};
use sources::DownloadQuality;

pub const DOWNLOAD_BITRATES: &[u32] = &[
    64, 80, 96, 112, 128, 144, 160, 176, 192, 208, 224, 240, 256, 272, 288, 304, 320,
];
/// The rate a transcoded download takes when none was chosen.
pub const DEFAULT_DOWNLOAD_BITRATE: u32 = 128;

/// The quality with another format: `None` keeps the original. FLAC has no rate; another
/// codec keeps the chosen rate, or takes the default one.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn download_quality_with_codec(
    quality: DownloadQuality,
    codec: Option<AudioCodec>,
) -> DownloadQuality {
    let Some(codec) = codec else {
        return DownloadQuality::Original;
    };
    let previous = quality.encoding();
    DownloadQuality::Transcoded(AudioEncoding {
        codec,
        bitrate_kbps: (codec != AudioCodec::Flac).then(|| {
            previous
                .and_then(|encoding| encoding.bitrate_kbps)
                .unwrap_or(DEFAULT_DOWNLOAD_BITRATE)
        }),
        channels: previous.map_or(AudioChannels::Preserve, |encoding| encoding.channels),
    })
}

/// The quality at another rate; the original and FLAC have none.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn download_quality_with_bitrate(
    quality: DownloadQuality,
    bitrate_kbps: u32,
) -> DownloadQuality {
    match quality {
        DownloadQuality::Transcoded(encoding) if encoding.codec != AudioCodec::Flac => {
            DownloadQuality::Transcoded(AudioEncoding {
                bitrate_kbps: Some(bitrate_kbps),
                ..encoding
            })
        }
        quality => quality,
    }
}

/// The quality with other channels; the original keeps its own.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn download_quality_with_channels(
    quality: DownloadQuality,
    channels: AudioChannels,
) -> DownloadQuality {
    match quality {
        DownloadQuality::Transcoded(encoding) => DownloadQuality::Transcoded(AudioEncoding {
            channels,
            ..encoding
        }),
        DownloadQuality::Original => DownloadQuality::Original,
    }
}

/// Whether the quality has a rate to choose.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn download_quality_shows_bitrate(quality: DownloadQuality) -> bool {
    quality
        .encoding()
        .is_some_and(|encoding| encoding.codec != AudioCodec::Flac)
}

/// Whether the quality has channels to choose.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn download_quality_shows_channels(quality: DownloadQuality) -> bool {
    quality.encoding().is_some()
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn download_bitrates() -> Vec<u32> {
    DOWNLOAD_BITRATES.to_vec()
}

pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<DownloadQuality, D::Error> {
    use serde::Deserialize;
    let value = serde_json::Value::deserialize(deserializer)?;
    if value.as_str() == Some("Original") {
        return Ok(DownloadQuality::Original);
    }
    if let Some(rate) = value.get("MaxBitrateKbps") {
        let rate = u32::deserialize(rate).map_err(serde::de::Error::custom)?;
        return Ok(DownloadQuality::Transcoded(AudioEncoding {
            codec: AudioCodec::Opus,
            bitrate_kbps: Some(rate),
            channels: AudioChannels::Preserve,
        }));
    }
    serde_json::from_value(value).map_err(serde::de::Error::custom)
}
