use crate::runtime::SelectedLibrary;
use library::{FolderKey, LibraryError, LibraryResult, ReadCancellation};

pub enum FolderProjection {
    Live {
        folders: Vec<sources::LiveFolder>,
        candidates: Vec<String>,
    },
    Cached(Option<FolderKey>),
}

pub async fn resolve_folder(
    selected: &SelectedLibrary,
    folder_object_id: Option<&str>,
    cancellation: &ReadCancellation,
) -> LibraryResult<FolderProjection> {
    let live = selected.operations.folder(
        folder_object_id.map(str::to_owned),
        selected.music_folder_object_id.clone(),
    );
    match live.recv().await {
        Ok(Ok(page)) => {
            let candidates = selected
                .database
                .track_media_uris_by_objects(selected.source_key, &page.tracks, cancellation)
                .await?;
            if candidates.len() == page.tracks.len() {
                return Ok(FolderProjection::Live {
                    folders: page.folders,
                    candidates,
                });
            }
            tracing::debug!("live Folder is newer than its accepted cache; using fallback");
        }
        Ok(Err(error)) => {
            tracing::debug!(%error, "live Folder unavailable; using exact cached Folder");
        }
        Err(error) => {
            tracing::debug!(%error, "live Folder ended; using exact cached Folder");
        }
    }
    let resolved = match folder_object_id {
        Some(object) => {
            selected
                .database
                .folder_key_by_object(selected.source_key, object, cancellation)
                .await?
        }
        None => None,
    };
    exact_cached_folder_scope(
        folder_object_id.is_some(),
        resolved,
        selected.music_folder_key,
    )
    .map(FolderProjection::Cached)
}

fn exact_cached_folder_scope(
    nested: bool,
    resolved: Option<FolderKey>,
    root_scope: Option<FolderKey>,
) -> LibraryResult<Option<FolderKey>> {
    if nested {
        resolved.map(Some).ok_or_else(|| {
            LibraryError::InvalidRequest("the exact cached Folder is unavailable".to_string())
        })
    } else {
        Ok(root_scope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_nested_folder_never_substitutes_the_root_scope() {
        let root = FolderKey::from_raw(1);
        assert!(exact_cached_folder_scope(true, None, Some(root)).is_err());
        assert_eq!(
            exact_cached_folder_scope(false, None, Some(root)).unwrap(),
            Some(root)
        );
    }
}
