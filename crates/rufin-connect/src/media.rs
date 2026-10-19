//! Authenticated byte streams for audio, profile files and ordinary cache files.
use std::{path::Path, pin::Pin};

use anyhow::{Result, bail};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PeerMediaRequest {
    pub uri: String,
    pub encoding: Option<library::AudioEncoding>,
    pub range: Option<(u64, Option<u64>)>,
    pub window: Option<(u64, Option<u64>)>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PeerByteMetadata {
    pub length: Option<u64>,
    pub offset: u64,
    pub content_type: Option<String>,
    pub revision: String,
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

pub async fn open_file(
    path: &Path,
    offset: u64,
    expected: Option<&str>,
) -> Result<(FileMetadata, tokio::fs::File)> {
    let mut file = tokio::fs::File::open(path).await?;
    let stamp = file.metadata().await?;
    let length = stamp.len();
    let validator = stamp.modified().ok().map(|modified| {
        blake3::hash(format!("{path:?}:{length}:{modified:?}").as_bytes())
            .to_hex()
            .to_string()
    });
    let offset = if expected.is_some_and(|expected| validator.as_deref() != Some(expected)) {
        0
    } else {
        offset.min(length)
    };
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

pub async fn receive_file(
    mut reader: Pin<Box<dyn AsyncRead + Send + Unpin>>,
    metadata: &FileMetadata,
    destination: &Path,
    cancel: CancellationToken,
    progress: watch::Sender<u64>,
) -> Result<()> {
    if let Some(parent) = destination.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(destination)
        .await?;
    file.set_len(metadata.offset).await?;
    file.seek(std::io::SeekFrom::Start(metadata.offset)).await?;
    progress.send_replace(metadata.offset);
    let result = async {
        let mut written = metadata.offset;
        let mut buffer = [0; 65536];
        loop {
            let length = reader.read(&mut buffer).await?;
            if length == 0 {
                break;
            }
            file.write_all(&buffer[..length]).await?;
            written += length as u64;
            progress.send_replace(written);
        }
        if written != metadata.length {
            bail!("Incomplete file response");
        }
        Result::<()>::Ok(())
    };
    let result = tokio::select! {
        _ = cancel.cancelled() => Err(anyhow::anyhow!("Transfer cancelled")),
        result = result => result,
    };
    file.flush().await?;
    result?;
    file.sync_all().await?;
    Ok(())
}
