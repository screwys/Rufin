//! Native playlist access uses the same content reader as music scanning.
use std::path::Path;

use super::{local, media};

pub async fn import_local_playlist_file(
    database: &library::Database,
    path: &Path,
    target: Option<library::PlaylistKey>,
    recognize: impl Fn(&str) -> Option<String>,
) -> crate::SourceResult<library::PlaylistImportReport> {
    let path = path.to_owned();
    let (mut document, path, mut locators, files) = tokio::task::spawn_blocking(move || {
        let input = std::fs::File::open(&path)
            .map_err(|error| crate::SourceError::Other(error.to_string()))?;
        let mut document = library::PlaylistFile::read(std::io::BufReader::new(input), &path)?;
        if target.is_none() {
            document.identity = None;
        }
        let mut worker = media::Worker::default();
        let mut locators = Vec::new();
        let mut files = Vec::new();
        let before = document.entries.len();
        document.entries.retain_mut(|entry| {
            let Some(uri) =
                library::playlist_locator(&entry.locator, path.parent().unwrap_or(Path::new(".")))
            else {
                return true;
            };
            let Some(access) = library::file_media_path(&uri) else {
                return true;
            };
            let Ok(metadata) = access.metadata() else {
                return true;
            };
            if !metadata.is_file() {
                return true;
            }
            match local::media::read_media(&mut worker, access.clone(), None) {
                media::MediaRead::Accepted(track) => {
                    let canonical = track.track.media_uri.expect("local media identity");
                    let access = std::fs::canonicalize(&access).unwrap_or(access);
                    let access_uri = url::Url::from_file_path(&access)
                        .expect("absolute local media path")
                        .to_string();
                    #[cfg(unix)]
                    let (device_id, inode) = {
                        use std::os::unix::fs::MetadataExt;
                        (
                            i64::try_from(metadata.dev()).ok(),
                            i64::try_from(metadata.ino()).ok(),
                        )
                    };
                    #[cfg(not(unix))]
                    let (device_id, inode) = (None, None);
                    files.push((
                        canonical.clone(),
                        access.to_string_lossy().into_owned(),
                        device_id,
                        inode,
                    ));
                    if canonical != access_uri {
                        locators.push(library::LocalLocatorWrite {
                            source_id: None,
                            media_uri: canonical.clone(),
                            origin: "import".into(),
                            path: access.to_string_lossy().into_owned(),
                            root: access
                                .parent()
                                .unwrap_or(Path::new("."))
                                .to_string_lossy()
                                .into_owned(),
                            relative_path: access
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into_owned(),
                            access_uri,
                        });
                    }
                    entry.locator = canonical;
                    true
                }
                media::MediaRead::Rejected => false,
                media::MediaRead::Unreadable => true,
            }
        });
        document.skipped += (before - document.entries.len()) as u64;
        Ok::<_, crate::SourceError>((document, path, locators, files))
    })
    .await
    .map_err(|error| crate::SourceError::Other(error.to_string()))??;
    let resolved = database
        .equivalent_local_file_uris(&files)
        .await?
        .into_iter()
        .collect::<std::collections::HashMap<_, _>>();
    for locator in &mut locators {
        if let Some(canonical) = resolved.get(&locator.media_uri) {
            locator.media_uri = canonical.clone();
        }
        database.upsert_local_locator(locator).await?;
    }
    for entry in &mut document.entries {
        if let Some(canonical) = resolved.get(&entry.locator) {
            entry.locator = canonical.clone();
        }
    }
    database
        .import_playlist_document(document, &path, target, recognize)
        .await
        .map_err(Into::into)
}

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
