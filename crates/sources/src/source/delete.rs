//! Delete permanently. A server deletes its tracks itself; a file source, or a server whose
//! folder is mapped here, has the files deleted where they are. The Catalog then withdraws
//! what was deleted.

use std::collections::BTreeMap;

use library::{Catalog, LocalMediaLocation, Playable};

use super::{Implementation, Source};
use crate::{SourceError, SourceResult};

impl Source {
    /// Deletes the files of `tracks`, all of this source, and withdraws them from the Catalog.
    /// Returns the URIs it deleted, and the failure that stopped the rest.
    pub async fn delete_tracks(
        &self,
        catalog: &Catalog,
        tracks: &[Playable],
    ) -> (Vec<String>, SourceResult<()>) {
        let mut deleted = Vec::new();
        // Server ids to read again, or file paths to walk again.
        let mut gone = Vec::new();
        let files = matches!(
            self.implementation,
            Implementation::Local(_) | Implementation::Files(_)
        );
        let result = async {
            if files {
                return self
                    .delete_files(catalog, tracks, &mut deleted, &mut gone)
                    .await;
            }
            for track in tracks {
                let object = track.object_id.as_deref().ok_or(SourceError::NotFound)?;
                match &self.implementation {
                    Implementation::JellyfinEmby(provider) => provider.delete_item(object).await,
                    Implementation::Plex(provider) => provider.delete_track(object).await,
                    Implementation::OpenSubsonic(_) => delete_mapped(track).await,
                    _ => Err(SourceError::InvalidRequest(
                        "This source cannot delete tracks",
                    )),
                }
                .map_err(|error| match error {
                    SourceError::Auth(_) | SourceError::Server { status: 400, .. } => {
                        SourceError::Other("The server did not allow deleting this track".into())
                    }
                    error => error,
                })?;
                deleted.push(track.media_uri.clone());
                gone.push(self.item_request_id("track", object));
            }
            Ok(())
        }
        .await;
        if !gone.is_empty() {
            let withdrawn = if files {
                self.apply_file_paths(catalog, &gone, &[], &|| false).await
            } else {
                self.apply_items(catalog, Vec::new(), gone).await
            };
            return (deleted, result.and(withdrawn));
        }
        (deleted, result)
    }

    /// Deletes each file once: a CUE image goes whole, with its sheet, when all of its tracks
    /// are selected.
    async fn delete_files(
        &self,
        catalog: &Catalog,
        tracks: &[Playable],
        deleted: &mut Vec<String>,
        gone: &mut Vec<String>,
    ) -> SourceResult<()> {
        let mut files = BTreeMap::<&str, Vec<&Playable>>::new();
        for track in tracks {
            let owner = track.cue_path.as_deref().or(track.source_path.as_deref());
            files
                .entry(owner.ok_or(SourceError::NotFound)?)
                .or_default()
                .push(track);
        }
        for (owner, tracks) in files {
            let mut paths = tracks
                .iter()
                .filter_map(|track| track.source_path.as_deref())
                .collect::<Vec<_>>();
            paths.sort_unstable();
            paths.dedup();
            if let Some(source) = tracks[0].source.filter(|_| tracks[0].cue_path.is_some()) {
                if catalog.file_track_objects(source, &[owner]).await?.len() > tracks.len() {
                    return Err(SourceError::InvalidRequest(
                        "A CUE image is deleted whole: select all of its tracks",
                    ));
                }
                paths.push(owner);
            }
            for path in paths {
                self.delete_file(path).await?;
                gone.push(path.to_owned());
            }
            deleted.extend(tracks.iter().map(|track| track.media_uri.clone()));
        }
        Ok(())
    }

    async fn delete_file(&self, path: &str) -> SourceResult<()> {
        if let Some(files) = self.file_source(Some(path)) {
            return files.remove_file(&files.relative(path)?).await;
        }
        std::fs::remove_file(path).map_err(|error| SourceError::Other(error.to_string()))
    }
}

/// Deletes an opened file that no source holds: the document it was opened from, or its file.
pub async fn delete_opened_track(track: &Playable) -> SourceResult<()> {
    let location = match &track.locator {
        Some(locator) if locator.access_uri.starts_with("content:") => {
            LocalMediaLocation::Document(locator.access_uri.clone())
        }
        Some(locator) => LocalMediaLocation::File(locator.path.clone().into()),
        None => LocalMediaLocation::File(
            url::Url::parse(&track.media_uri)
                .ok()
                .and_then(|uri| uri.to_file_path().ok())
                .ok_or(SourceError::NotFound)?,
        ),
    };
    tokio::task::spawn_blocking(move || delete_location(location))
        .await
        .map_err(|error| SourceError::Other(error.to_string()))?
}

fn delete_location(location: LocalMediaLocation) -> SourceResult<()> {
    match location {
        LocalMediaLocation::File(path) => {
            std::fs::remove_file(path).map_err(|error| SourceError::Other(error.to_string()))
        }
        LocalMediaLocation::Document(uri) => crate::delete_document(&uri),
    }
}

/// Deletes a server track's file in the folder mapped to it, when it is the file the server
/// lists.
async fn delete_mapped(track: &Playable) -> SourceResult<()> {
    let mapped = track.mapped.clone().ok_or(SourceError::InvalidRequest(
        "Map this server's music folder to a folder here to delete its tracks",
    ))?;
    tokio::task::spawn_blocking(move || {
        delete_location(mapped.locate_playable().ok_or(SourceError::NotFound)?)
    })
    .await
    .map_err(|error| SourceError::Other(error.to_string()))?
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use library::ReadCancellation;

    /// One second of silence.
    fn silent_wav() -> Vec<u8> {
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&16_036_u32.to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        for field in [16_u32, 0x0001_0001, 8_000, 16_000, 0x0010_0002] {
            bytes.extend_from_slice(&field.to_le_bytes());
        }
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&16_000_u32.to_le_bytes());
        bytes.resize(44 + 16_000, 0);
        bytes
    }

    #[tokio::test]
    async fn deleting_local_and_opened_tracks_removes_their_files() {
        let directory = tempfile::tempdir().unwrap();
        let music = directory.path().join("music");
        std::fs::create_dir(&music).unwrap();
        let (kept, deleted) = (music.join("kept.wav"), music.join("deleted.wav"));
        std::fs::write(&kept, silent_wav()).unwrap();
        std::fs::write(&deleted, silent_wav()).unwrap();
        let catalog = library::Catalog::open(directory.path().join("library.db"))
            .await
            .unwrap();
        let (configuration, source, _) = crate::Source::connect(
            crate::SourceId::new("delete"),
            crate::SourceSetup::Local { roots: vec![music] },
            "",
        )
        .await
        .unwrap()
        .into_parts();
        catalog
            .reconcile_source(
                &configuration.source_id,
                &configuration.kind,
                &configuration.name,
            )
            .await
            .unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        source
            .acquire_catalog(&catalog, &|_| {}, cancelled, crate::Refresh::Changed)
            .await
            .unwrap();
        let uri = |path| url::Url::from_file_path(path).unwrap().to_string();
        let track = catalog
            .playable_by_uri(&uri(&deleted))
            .await
            .unwrap()
            .expect("the track to delete");
        let listed = async || {
            let source = library::Collection::Source(track.source.unwrap());
            let read = library::Read::Members(library::MemberRead::new(source));
            let order = catalog.order(&read, &ReadCancellation::new()).await;
            order.unwrap().len()
        };
        assert_eq!(listed().await, 2);

        let (uris, result) = source
            .delete_tracks(&catalog, std::slice::from_ref(&track))
            .await;

        result.unwrap();
        assert_eq!(uris, [uri(&deleted)]);
        assert!(!deleted.exists() && kept.exists());
        assert_eq!(listed().await, 1);

        // The same file opened from outside the library: no source holds it.
        let library = catalog.playable_by_uri(&uri(&kept)).await.unwrap().unwrap();
        let opened = library::Playable {
            source: None,
            source_id: None,
            locator: None,
            ..library
        };
        super::delete_opened_track(&opened).await.unwrap();
        assert!(!kept.exists());
    }
}
