//! Native file opening and revision facts for the shared metadata reader.

use crate::LocalImageRef;
use crate::file::media::{MediaRead, Worker, read_media_input};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub(crate) fn read_media(
    worker: &mut Worker,
    path: PathBuf,
    sidecar: Option<LocalImageRef>,
) -> MediaRead {
    let mut file = match fs::File::open(&path) {
        Ok(file) => file,
        Err(_) => return MediaRead::Unreadable,
    };
    let Ok(uri) = url::Url::from_file_path(&path) else {
        return MediaRead::Unreadable;
    };
    let mut read = read_media_input(worker, path.clone(), &mut file, uri.as_str(), sidecar);
    if let MediaRead::Accepted(track) = &mut read {
        track.local_uri = Some(uri.into());
        track.audio_revision = audio_revision(&path);
    }
    read
}

fn audio_revision(path: &Path) -> blake3::Hasher {
    let mut hash = blake3::Hasher::new();
    hash.update(b"rufin-local-audio-v1\0");
    hash.update(path.to_string_lossy().as_bytes());
    if let Ok(metadata) = fs::metadata(path) {
        hash.update(&metadata.len().to_le_bytes());
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|time| time.as_nanos())
            .unwrap_or_default();
        hash.update(&modified.to_le_bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            hash.update(&metadata.dev().to_le_bytes());
            hash.update(&metadata.ino().to_le_bytes());
        }
    }
    hash.update(&super::scan::LOCAL_PARSER_VERSION.to_le_bytes());
    hash
}
