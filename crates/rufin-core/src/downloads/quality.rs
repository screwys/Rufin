use library::{AudioChannels, AudioCodec, AudioEncoding};
use sources::DownloadQuality;

pub const DOWNLOAD_BITRATES: &[u32] = &[
    64, 80, 96, 112, 128, 144, 160, 176, 192, 208, 224, 240, 256, 272, 288, 304, 320,
];

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
