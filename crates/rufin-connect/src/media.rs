//! Authenticated byte streams for audio and files.
use std::{path::Path, pin::Pin};

use anyhow::Result;
use tokio::io::{AsyncRead, AsyncSeekExt};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PeerByteMetadata {
    pub length: Option<u64>,
    pub offset: u64,
    pub content_type: Option<String>,
    #[serde(default)]
    pub validator: Option<String>,
}

pub struct PeerByteResponse {
    pub metadata: serde_json::Value,
    pub reader: Pin<Box<dyn AsyncRead + Send + Unpin>>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct FileMetadata {
    pub length: u64,
    pub offset: u64,
    pub validator: Option<String>,
}

pub async fn open_file(path: &Path, offset: u64) -> Result<(FileMetadata, tokio::fs::File)> {
    let mut file = tokio::fs::File::open(path).await?;
    let stamp = file.metadata().await?;
    let length = stamp.len();
    let validator = stamp.modified().ok().map(|modified| {
        blake3::hash(format!("{path:?}:{length}:{modified:?}").as_bytes())
            .to_hex()
            .to_string()
    });
    let offset = offset.min(length);
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    Ok((
        FileMetadata {
            length,
            offset,
            validator,
        },
        file,
    ))
}
