//! Reviewed metadata operations and publication.
mod editor;
use crate::runtime::CatalogChange;
use crate::source::{SourceOwner, string_error};
use async_channel::Receiver;
pub use editor::{MetadataEditor, MetadataEditorSnapshot, MetadataFieldPresentation};
use library::Database;
pub use metadata_lookup::{ArtworkQuery, ArtworkResult};
pub use sources::MetadataDraft;
use sources::{
    Implementation, MetadataChanges, MetadataOwner, Source, SourceEntityKind, SourceMetadataError,
};
use std::sync::Arc;
pub const MAX_ARTWORK_BYTES: usize = 32 * 1024 * 1024;

pub fn read_metadata(
    owner: &SourceOwner,
    item: MetadataItemId,
) -> Receiver<Result<MetadataDraft, SourceMetadataError>> {
    owner.reply(move |owner, database| async move {
        let row = match metadata_row(&database, item.media_uri(), item.kind()).await {
            Ok(row) => row,
            Err(SourceMetadataError::Unavailable) if matches!(item, MetadataItemId::Track(_)) => {
                return Source::read_direct_file_metadata(item.media_uri());
            }
            Err(error) => return Err(error),
        };
        let source = match metadata_source(&owner, &row) {
            Ok(source) => source,
            Err(_) if matches!(item, MetadataItemId::Track(_)) => {
                return Source::read_direct_file_metadata(item.media_uri());
            }
            Err(error) => return Err(error),
        };
        let binding = metadata_binding(&database, &row).await?;
        let server = matches!(
            source.implementation,
            Implementation::JellyfinEmby(_) | Implementation::Plex(_)
        );
        let mut metadata = match (&source.implementation, row) {
            (Implementation::JellyfinEmby(provider), MetadataOwner::Track(row)) => {
                provider.read_track_metadata(*row).await?
            }
            (Implementation::JellyfinEmby(provider), MetadataOwner::Album(row)) => {
                provider.read_album_metadata(row).await?
            }
            (Implementation::JellyfinEmby(provider), MetadataOwner::Artist(row)) => {
                provider.read_artist_metadata(row).await?
            }
            (Implementation::Plex(provider), MetadataOwner::Track(row)) => {
                provider.read_track_metadata(*row).await?
            }
            (Implementation::Plex(provider), MetadataOwner::Album(row)) => {
                provider.read_album_metadata(row).await?
            }
            (Implementation::Plex(provider), MetadataOwner::Artist(row)) => {
                provider.read_artist_metadata(row).await?
            }
            (_, MetadataOwner::Track(row)) => {
                source.read_file_track_metadata(&database, *row).await?
            }
            (_, row) => source.read_file_collection_metadata(&database, row).await?,
        };
        metadata.artwork = metadata.artwork.with_binding(binding, server);
        Ok(metadata)
    })
}

pub fn write_reviewed_metadata(
    owner: &SourceOwner,
    media_uri: String,
    revision: Option<String>,
    token: Option<String>,
    edit: sources::MetadataEdit,
    previous_artwork: Option<Vec<u8>>,
) -> Receiver<Result<(), SourceMetadataError>> {
    owner.reply(move |owner, database| async move {
        let _lane = owner.shared.lane.lock().await;
        let row = metadata_row(&database, &media_uri, edit.kind).await;
        let target = row
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|row| metadata_source(&owner, row));
        let source = match target {
            Ok(source) => source,
            Err(_) => {
                return match edit.kind {
                    SourceEntityKind::Track => Source::write_direct_file_metadata(
                        &media_uri,
                        revision.as_deref().unwrap_or_default(),
                        &edit,
                    ),
                    _ => Err(SourceMetadataError::Unavailable),
                };
            }
        };
        let previous_artwork = edit.artwork.as_ref().and(previous_artwork);
        let row = row?;
        let expected = revision.as_deref().unwrap_or_default();
        let result = async {
            let object = row.object_id();
            let tags_saved = !edit.changes.is_empty();
            let (result, object, saved) = match &source.implementation {
                Implementation::Plex(provider) => {
                    let object = object.ok_or(SourceMetadataError::Unavailable)?;
                    provider.write_metadata(object, expected, &edit).await?;
                    (
                        provider.write_artwork(object, edit.artwork.as_ref()).await,
                        object.to_owned(),
                        tags_saved,
                    )
                }
                Implementation::JellyfinEmby(provider) => {
                    let object = provider
                        .write_metadata(
                            object.ok_or(SourceMetadataError::Unavailable)?,
                            expected,
                            token.as_deref(),
                            &edit,
                        )
                        .await?;
                    let result = if let Some(artwork) = edit.artwork.as_ref() {
                        provider.write_artwork(&object, artwork).await
                    } else {
                        Ok(())
                    };
                    (result, object, tags_saved || token.is_some())
                }
                _ => {
                    let binding = metadata_binding(&database, &row).await?;
                    let folder = sources::artwork_file(binding.as_deref());
                    return source
                        .write_file_metadata(
                            &database,
                            &media_uri,
                            &row,
                            expected,
                            &edit,
                            folder.as_deref(),
                        )
                        .await;
                }
            };
            let refresh = source
                .apply_items(&database, vec![object], Vec::new())
                .await
                .map_err(|error| SourceMetadataError::SavedRefreshFailed(error.to_string()));
            sources::finish_metadata_save(result, refresh, saved)
        }
        .await;
        let mut refresh = Ok(false);
        match &result {
            Ok(()) | Err(SourceMetadataError::PartiallySaved { .. }) => {
                if let Some(binding) = previous_artwork {
                    let artwork = owner.shared.artwork.clone();
                    refresh = tokio::task::spawn_blocking(move || {
                        artwork.invalidate_image(&artwork::ArtworkBinding::opaque(&binding))
                    })
                    .await
                    .map_err(string_error)
                    .and_then(|result| result.map_err(string_error));
                }
                if matches!(refresh, Ok(true)) {
                    owner
                        .artwork_invalidated(source.source_id(), CatalogChange::Broad)
                        .await;
                }
            }
            Err(_) => {}
        }
        result?;
        refresh
            .map(|_| ())
            .map_err(SourceMetadataError::SavedRefreshFailed)
    })
}

pub struct MetadataIdentification {
    pub changes: MetadataChanges,
    pub token: Option<String>,
}

pub fn identify_metadata(
    owner: &SourceOwner,
    media_uri: String,
    kind: SourceEntityKind,
    values: MetadataChanges,
) -> Receiver<Result<Option<MetadataIdentification>, String>> {
    let external = owner
        .shared
        .settings
        .load()
        .ui
        .allows_external_metadata_lookup();
    owner.reply(move |owner, _database| async move {
        let exact = editor::exact_identity(kind, &values);
        if external && (exact || kind == SourceEntityKind::Track) {
            let input = values.clone();
            if let Some(changes) = tokio::task::spawn_blocking(move || {
                metadata_lookup::identify_metadata(kind, &input)
            })
            .await
            .map_err(string_error)??
            {
                return Ok(Some(MetadataIdentification {
                    changes,
                    token: None,
                }));
            }
        }
        if kind == SourceEntityKind::Track {
            return Ok(None);
        }
        let native = library::source_entity_parts(&media_uri).filter(|(id, _, _)| {
            owner.configuration(id).is_some_and(|configuration| {
                matches!(configuration.kind.as_str(), "jellyfin" | "emby")
            })
        });
        let source = match &native {
            Some((id, _, _)) => {
                let id = id.clone();
                tokio::task::spawn_blocking(move || owner.client(&id).ok())
                    .await
                    .map_err(string_error)?
            }
            None => None,
        };
        if let Some((_, _, object)) = native
            && let Some(source) = source
            && let Implementation::JellyfinEmby(provider) = &source.implementation
        {
            let id = object.as_str();
            let found = match kind {
                SourceEntityKind::Album => provider.identify_album_metadata(id, &values).await?,
                SourceEntityKind::Artist => provider.identify_artist_metadata(id, &values).await?,
                SourceEntityKind::Track => unreachable!(),
            };
            if let Some((changes, token)) = found {
                return Ok(Some(MetadataIdentification {
                    changes,
                    token: Some(token),
                }));
            }
        }
        if !external || exact {
            return Ok(None);
        }
        tokio::task::spawn_blocking(move || metadata_lookup::identify_metadata(kind, &values))
            .await
            .map_err(string_error)?
            .map(|found| {
                found.map(|changes| MetadataIdentification {
                    changes,
                    token: None,
                })
            })
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum MetadataItemId {
    Track(String),
    Album(String),
    Artist(String),
}

impl MetadataItemId {
    fn kind(&self) -> SourceEntityKind {
        match self {
            Self::Track(_) => SourceEntityKind::Track,
            Self::Album(_) => SourceEntityKind::Album,
            Self::Artist(_) => SourceEntityKind::Artist,
        }
    }

    pub fn media_uri(&self) -> &str {
        match self {
            Self::Track(uri) | Self::Album(uri) | Self::Artist(uri) => uri,
        }
    }
}

pub fn search_artwork(
    owner: &SourceOwner,
    query: ArtworkQuery,
) -> Receiver<Result<Vec<ArtworkResult>, String>> {
    owner.reply(move |owner, _| async move {
        if !owner
            .shared
            .settings
            .load()
            .ui
            .allows_external_metadata_lookup()
        {
            return Ok(Vec::new());
        }
        tokio::task::spawn_blocking(move || metadata_lookup::search_artwork(&query))
            .await
            .map_err(string_error)?
    })
}

pub fn download_artwork(
    owner: &SourceOwner,
    url: String,
) -> Receiver<Result<Arc<sources::ImageBytes>, String>> {
    owner.reply(move |owner, _| async move {
        if !owner
            .shared
            .settings
            .load()
            .ui
            .allows_external_metadata_lookup()
        {
            return Err("External metadata lookup is disabled".into());
        }
        tokio::task::spawn_blocking(move || {
            metadata_lookup::download_artwork(&url).and_then(checked_artwork)
        })
        .await
        .map_err(string_error)?
    })
}

pub fn prepare_artwork(
    owner: &SourceOwner,
    bytes: Vec<u8>,
) -> Receiver<Result<Arc<sources::ImageBytes>, String>> {
    owner.reply(move |_, _| async move {
        tokio::task::spawn_blocking(move || checked_artwork(bytes))
            .await
            .map_err(string_error)?
    })
}

/// Use the cached image the user has seen before asking its source for an original.
pub fn current_artwork(
    owner: &SourceOwner,
    binding: Vec<u8>,
) -> Receiver<Result<Option<Arc<sources::ImageBytes>>, String>> {
    owner.reply(move |owner, _| async move {
        let stored = owner.shared.settings.load();
        let request =
            artwork::ArtworkRequest::original(artwork::ArtworkBinding::opaque(&binding), 512)
                .with_external(artwork::ExternalPolicy::new(
                    stored.ui.application.external_metadata_enabled,
                    stored.ui.allows_external_metadata_lookup(),
                    stored.ui.lastfm_api_key,
                ));
        let bytes = owner.shared.artwork.image_bytes(request).await?;
        tokio::task::spawn_blocking(move || bytes.map(checked_artwork).transpose())
            .await
            .map_err(string_error)?
    })
}

fn checked_artwork(bytes: Vec<u8>) -> Result<Arc<sources::ImageBytes>, String> {
    if bytes.len() > MAX_ARTWORK_BYTES {
        return Err("Artwork exceeds 32 MiB".into());
    }
    artwork::decode_rgba(&bytes, 512).map_err(string_error)?;
    let content_type = artwork::image_mime(&bytes).map(str::to_owned);
    Ok(Arc::new(sources::ImageBytes {
        bytes,
        content_type,
    }))
}

async fn metadata_row(
    database: &Database,
    uri: &str,
    kind: SourceEntityKind,
) -> Result<MetadataOwner, SourceMetadataError> {
    let cancel = library::ReadCancellation::new();
    let missing = SourceMetadataError::Unavailable;
    let error = |error: library::LibraryError| SourceMetadataError::Write(error.to_string());
    Ok(match kind {
        SourceEntityKind::Track => MetadataOwner::Track(Box::new(
            database
                .track_row_by_uri(uri, &cancel)
                .await
                .map_err(error)?
                .ok_or(missing)?,
        )),
        SourceEntityKind::Album => MetadataOwner::Album(
            database
                .album_row_by_media_uri(uri, &cancel)
                .await
                .map_err(error)?
                .ok_or(missing)?,
        ),
        SourceEntityKind::Artist => MetadataOwner::Artist(
            database
                .artist_row_by_media_uri(uri, &cancel)
                .await
                .map_err(error)?
                .ok_or(missing)?,
        ),
    })
}

fn metadata_source(
    owner: &SourceOwner,
    row: &MetadataOwner,
) -> Result<Arc<Source>, SourceMetadataError> {
    let source = match row {
        MetadataOwner::Track(row) => row.source_id.as_deref().map(sources::SourceId::new),
        MetadataOwner::Album(row) => {
            library::source_entity_parts(&row.media_uri).map(|(id, _, _)| id)
        }
        MetadataOwner::Artist(row) => {
            library::source_entity_parts(&row.media_uri).map(|(id, _, _)| id)
        }
    }
    .ok_or(SourceMetadataError::Unavailable)?;
    owner
        .client(&source)
        .map_err(|_| SourceMetadataError::Unavailable)
}

async fn metadata_binding(
    database: &Database,
    row: &MetadataOwner,
) -> Result<Option<Vec<u8>>, SourceMetadataError> {
    match row {
        MetadataOwner::Track(row) => track_artwork_binding(database, row).await,
        MetadataOwner::Album(row) => Ok(row.artwork_binding.clone()),
        MetadataOwner::Artist(row) => Ok(row.artwork_binding.clone()),
    }
}
async fn track_artwork_binding(
    database: &Database,
    track: &library::TrackRow,
) -> Result<Option<Vec<u8>>, SourceMetadataError> {
    if track.artwork_binding.is_some() {
        return Ok(track.artwork_binding.clone());
    }
    let Some(uri) = &track.album_media_uri else {
        return Ok(None);
    };
    Ok(database
        .album_row_by_media_uri(uri, &library::ReadCancellation::new())
        .await
        .map_err(|error| SourceMetadataError::Write(error.to_string()))?
        .and_then(|album| album.artwork_binding))
}
