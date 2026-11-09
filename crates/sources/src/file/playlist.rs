//! Native playlist access uses the same content reader as music scanning.
use std::path::{Path, PathBuf};

use super::{local, media};

/// Entries name files in host coordinates, as track identities do; each is read through the
/// library folder (`roots`) that holds it, which is the only access a sandbox has.
pub async fn import_local_playlist_file(
    catalog: &library::Catalog,
    path: &Path,
    roots: &[PathBuf],
    target: Option<library::PlaylistKey>,
    recognize: impl Fn(&str) -> Option<String>,
) -> crate::SourceResult<library::PlaylistImportReport> {
    let path = path.to_owned();
    let roots = roots.to_vec();
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
        let base = library::playlist_base(&path);
        document.entries.retain_mut(|entry| {
            let Some(uri) = library::playlist_locator(&entry.locator, &base) else {
                return true;
            };
            let Some(host) = library::file_media_path(&uri) else {
                return true;
            };
            let access =
                local::local_file_location(&roots, &host).map_or(host, |(_, access)| access);
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
                    locators.extend(local::media::local_access_locator(&canonical, &access));
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
    let resolved = catalog
        .equivalent_local_file_uris(&files)
        .await?
        .into_iter()
        .collect::<std::collections::HashMap<_, _>>();
    for locator in &mut locators {
        if let Some(canonical) = resolved.get(&locator.media_uri) {
            locator.media_uri = canonical.clone();
        }
        catalog.upsert_local_locator(locator).await?;
    }
    for entry in &mut document.entries {
        if let Some(canonical) = resolved.get(&entry.locator) {
            entry.locator = canonical.clone();
        }
    }
    catalog
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
