//! Checks media locations for explicit cleanup commands.
use super::{SourceOwner, string_error};
use std::collections::HashSet;

impl SourceOwner {
    pub(crate) async fn missing_media(&self, uris: &[String]) -> Result<HashSet<String>, String> {
        let database = &self.shared.database;
        let mut missing = HashSet::new();
        let source_uris: Vec<_> = uris
            .iter()
            .filter_map(|uri| {
                library::source_entity_parts(uri)
                    .filter(|(_, kind, _)| kind == "track")
                    .map(|(source, _, _)| (uri.clone(), source))
            })
            .collect();
        let catalog_missing: std::collections::HashSet<_> = database
            .catalog_missing_media(&source_uris)
            .await
            .map_err(string_error)?
            .into_iter()
            .collect();
        let paths = database
            .local_media_locations(uris)
            .await
            .map_err(string_error)?;
        let shared: Vec<_> = paths.iter().map(|(_, _, shared)| *shared).collect();
        let paths = paths.into_iter().map(|(_, paths, _)| paths).collect();
        let mut available = sources::local_files_available(paths)
            .await
            .map_err(string_error)?;
        let native: Vec<_> = uris
            .iter()
            .enumerate()
            .filter(|(index, uri)| direct_local_media(uri) && !available[*index])
            .map(|(index, uri)| (index, uri.clone()))
            .collect();
        if !native.is_empty() {
            let connect = self
                .shared
                .connect
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .upgrade();
            if let Some(connect) = connect {
                let requested: Vec<_> = native.iter().map(|(_, uri)| uri.clone()).collect();
                let peers = connect.media_availability(&requested).await?;
                for ((index, _), peer) in native.iter().zip(peers) {
                    available[*index] = peer;
                }
            } else if native.iter().any(|(index, _)| shared[*index]) {
                return Err("Rufin Connect is unavailable".into());
            }
        }
        for (index, uri) in uris.iter().enumerate() {
            if available[index] {
                continue;
            }
            if direct_local_media(uri) || catalog_missing.contains(uri) {
                missing.insert(uri.clone());
                continue;
            }
            let Some((source, kind, _)) = library::source_entity_parts(uri) else {
                continue;
            };
            if kind != "track"
                || !self
                    .configuration(&source)
                    .is_some_and(|source| source.is_file_library())
            {
                continue;
            }
            let Some(file) = database
                .observed_media_file(uri)
                .await
                .map_err(string_error)?
            else {
                continue;
            };
            if !self
                .client(&source)?
                .media_file_exists(&file.path)
                .await
                .map_err(string_error)?
            {
                missing.insert(uri.clone());
            }
        }
        Ok(missing)
    }
}

fn direct_local_media(uri: &str) -> bool {
    let backing = library::cue_media_parts(uri).map(|(_, backing, _, _)| backing);
    let backing = backing.as_deref().unwrap_or(uri);
    library::file_media_path(backing).is_some()
        || library::document_media_id(backing).is_some()
        || backing.starts_with("content:")
}
