//! Native file opening and revision facts for the shared metadata reader.

use crate::LocalImageRef;
use crate::file::media::{MediaRead, Worker, read_media_input};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub fn local_media_identity_path(path: &Path) -> PathBuf {
    let access = fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let host = library::playlist_host_path(&access);
    if host == access {
        access
    } else {
        fs::canonicalize(&host).unwrap_or(host)
    }
}

pub fn read_local_track_reference(path: &Path) -> Option<library::TrackReference> {
    let MediaRead::Accepted(track) = read_media(&mut Worker::default(), path.to_owned(), None)
    else {
        return None;
    };
    let uri = url::Url::from_file_path(path).ok()?.to_string();
    Some(crate::file::media::track_reference(*track, uri))
}

pub(crate) fn read_media(
    worker: &mut Worker,
    path: PathBuf,
    sidecar: Option<LocalImageRef>,
) -> MediaRead {
    if crate::file::media::excluded_from_audio_scan(&path) {
        return MediaRead::Rejected;
    }
    let mut file = match fs::File::open(&path) {
        Ok(file) => file,
        Err(_) => return MediaRead::Unreadable,
    };
    let identity = local_media_identity_path(&path);
    let Ok(uri) = url::Url::from_file_path(&identity) else {
        return MediaRead::Unreadable;
    };
    let Ok(access_uri) = url::Url::from_file_path(&path) else {
        return MediaRead::Unreadable;
    };
    let mut read = read_media_input(worker, path, &mut file, access_uri.as_str(), sidecar);
    if let MediaRead::Accepted(track) = &mut read {
        track.track.object_id = crate::file::media::track_id(&identity);
        track.track.media_uri = Some(uri.into());
        track.audio_revision = audio_revision(
            track
                .track
                .source_path
                .as_deref()
                .expect("file source path"),
            &identity,
        );
    }
    read
}

pub(crate) fn audio_revision(access: &str, identity: &Path) -> blake3::Hasher {
    let mut hash = blake3::Hasher::new();
    hash.update(b"rufin-local-audio-v1\0");
    hash.update(identity.to_string_lossy().as_bytes());
    if let Ok(metadata) = fs::metadata(access) {
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
