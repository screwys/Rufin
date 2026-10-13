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
    pub fn song_query(&self, source: library::SourceKey) -> library::SongQuery {
        let mut query = library::SongQuery::new(match self {
            Self::Live { objects, .. } => library::SongScope::Objects {
                source,
                ids: Arc::clone(objects),
            },
            Self::Cached(_) => library::SongScope::Tracks(source),
        });
        if let Self::Cached(folder) = self {
            query.folder = *folder;
        }
        query
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
    database: &library::Database,
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
                database,
                folder_object_id,
                selected.music_folder_object_id.as_deref(),
            )
            .await
    };
    let live = tokio::select! {
        result = live => result,
        () = cancellation.cancelled() => return Err(LibraryError::ReadCancelled),
    };
    match live {
        Ok(page) => {
            let known = database
                .track_object_count(selected.source_key, &page.tracks, cancellation)
                .await?;
            if known as usize == page.tracks.len() {
                let mut folders = page.folders;
                folders.sort_by_cached_key(|folder| folder.name.to_lowercase());
                return Ok(FolderProjection::Live {
                    folders: folders.into(),
                    objects: page.tracks.into(),
                });
            }
            tracing::debug!("live Folder is newer than its accepted cache; using fallback");
        }
        Err(error) => {
            tracing::debug!(%error, "live Folder unavailable; using exact cached Folder");
        }
    }
    let resolved = match folder_object_id {
        Some(object) => {
            database
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
