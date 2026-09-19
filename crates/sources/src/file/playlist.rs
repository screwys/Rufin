//! Native playlist access uses the same content reader as music scanning.
use std::path::Path;

use super::{local, media};

pub fn playlist_file_revision(path: &Path) -> std::io::Result<String> {
    let metadata = std::fs::metadata(path)?;
    let modified = metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(format!("{}:{modified}", metadata.len()))
}

/// Missing and unreadable media stay in playlists. Only a positive non-audio
/// result from the existing scanner rejects a readable file.
pub fn playlist_file_is_non_audio(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    matches!(
        local::media::read_media(&mut media::Worker::default(), path.to_owned(), None),
        media::MediaRead::Rejected
    )
}
