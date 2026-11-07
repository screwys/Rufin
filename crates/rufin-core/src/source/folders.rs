use std::sync::Arc;

use crate::runtime::SelectedLibrary;
use library::{FolderKey, LibraryError, LibraryResult, ReadCancellation};

#[derive(Clone)]
pub enum FolderProjection {
    Live {
        folders: Arc<[sources::LiveFolder]>,
        objects: Arc<[String]>,
    },
    Cached(Option<FolderKey>),
}

impl FolderProjection {
    /// The tracks it lists: the live folder's own, or the source's in [`Self::folder`].
    pub fn collection(&self, source: library::SourceKey) -> library::Collection {
        match self {
            Self::Live { objects, .. } => library::Collection::Objects {
                source,
                ids: Arc::clone(objects),
            },
            Self::Cached(_) => library::Collection::Source(source),
        }
    }

    /// The cached folder whose tracks it lists.
    pub fn folder(&self) -> Option<FolderKey> {
        match self {
            Self::Live { .. } => None,
            Self::Cached(folder) => *folder,
        }
    }

    pub fn live_folders<'a>(
        &'a self,
        filter: &'a str,
        descending: bool,
    ) -> impl Iterator<Item = &'a sources::LiveFolder> {
        let folders = match self {
            Self::Live { folders, .. } => folders.as_ref(),
            Self::Cached(_) => &[],
        };
        let filter = filter.trim().to_lowercase();
        (0..folders.len())
            .map(move |index| {
                &folders[if descending {
                    folders.len() - 1 - index
                } else {
                    index
                }]
            })
            .filter(move |folder| folder.name.to_lowercase().contains(&filter))
    }
}

pub async fn resolve_folder(
    owner: &super::SourceOwner,
    catalog: &library::Catalog,
    selected: &SelectedLibrary,
    folder_object_id: Option<&str>,
    cancellation: &ReadCancellation,
) -> LibraryResult<FolderProjection> {
    let live = async {
        let source = owner
            .cached_client(&selected.source_id)
            .ok_or(sources::SourceError::NotFound)?;
        source
            .browse_folder(
                catalog,
                folder_object_id,
                selected.music_folder_object_id.as_deref(),
            )
            .await
    };
    let live = tokio::select! {
        result = live => result,
        () = cancellation.cancelled() => return Err(LibraryError::ReadCancelled),
    };
    // The live page lists what the source holds now; member reads show the resident tracks.
    match live {
        Ok(mut page) => {
            page.folders
                .sort_by_cached_key(|folder| folder.name.to_lowercase());
            return Ok(FolderProjection::Live {
                folders: page.folders.into(),
                objects: page.tracks.into(),
            });
        }
        Err(error) => {
            tracing::debug!(%error, "live Folder unavailable; using exact cached Folder");
        }
    }
    let resolved = folder_key(catalog, selected.source_key, folder_object_id, cancellation).await?;
    exact_cached_folder_scope(
        folder_object_id.is_some(),
        resolved,
        selected.music_folder_key,
    )
    .map(FolderProjection::Cached)
}

/// The key of a source's cached folder by object id: None for no folder, and while the Store
/// holds no such folder, where a source's pages and download rules read the whole source.
pub(crate) async fn folder_key(
    catalog: &library::Catalog,
    source: library::SourceKey,
    object_id: Option<&str>,
    cancellation: &ReadCancellation,
) -> LibraryResult<Option<FolderKey>> {
    let Some(object_id) = object_id else {
        return Ok(None);
    };
    Ok(catalog
        .catalog_keys(
            library::CatalogKind::Folder,
            Some(source),
            &[object_id.to_owned()],
            cancellation,
        )
        .await?
        .first()
        .map(|key| FolderKey::from_raw(*key)))
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
