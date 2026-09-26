//! Reviewed metadata operations and publication.
use crate::runtime::CatalogChange;
use crate::source::{SourceOwner, string_error};
use async_channel::Receiver;
pub use metadata_lookup::{ArtworkQuery, ArtworkResult};
use sources::{
    AlbumMetadata, AlbumMetadataValues, ArtistMetadata, ArtistMetadataValues, Source,
    SourceMetadataError, TrackMetadata, TrackMetadataValues,
};
use std::sync::Arc;
pub fn track_metadata(
    owner: &SourceOwner,
    media_uri: String,
) -> Receiver<Result<TrackMetadata, SourceMetadataError>> {
    owner.reply(move |owner, database| async move {
        let target = owner.media_client(&media_uri).await;
        match target {
            Ok(source) => source.read_track_metadata(&database, &media_uri).await,
            Err(_) => Source::read_direct_file_metadata(&media_uri),
        }
    })
}

pub fn album_metadata(
    owner: &SourceOwner,
    media_uri: String,
) -> Receiver<Result<AlbumMetadata, SourceMetadataError>> {
    owner.reply(move |owner, database| async move {
        let target = owner.media_client(&media_uri).await;
        target
            .map_err(|_| SourceMetadataError::Unavailable)?
            .read_album_metadata(&database, &media_uri)
            .await
    })
}

pub fn artist_metadata(
    owner: &SourceOwner,
    media_uri: String,
) -> Receiver<Result<ArtistMetadata, SourceMetadataError>> {
    owner.reply(move |owner, database| async move {
        let target = owner.media_client(&media_uri).await;
        target
            .map_err(|_| SourceMetadataError::Unavailable)?
            .read_artist_metadata(&database, &media_uri)
            .await
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
        let target = owner.media_client(&media_uri).await;
        let _lane = owner.shared.lane.lock().await;
        let source = match target {
            Ok(source) => source,
            Err(_) => {
                return match edit {
                    sources::MetadataEdit::Track(edit) => Source::write_direct_file_metadata(
                        &media_uri,
                        revision.as_deref().unwrap_or_default(),
                        &edit,
                    ),
                    _ => Err(SourceMetadataError::Unavailable),
                };
            }
        };
        let previous_artwork = edit.artwork().and(previous_artwork);
        let result = source
            .write_metadata(
                &database,
                &media_uri,
                revision.as_deref().unwrap_or_default(),
                token.as_deref(),
                edit,
            )
            .await;
        let mut refresh = Ok(false);
        match &result {
            Ok(outcome)
            | Err(SourceMetadataError::PartiallySaved {
                outcome: Some(outcome),
                ..
            }) => {
                if let Some(binding) = previous_artwork {
                    let artwork = owner.shared.artwork.clone();
                    refresh = tokio::task::spawn_blocking(move || {
                        artwork.invalidate_image(&artwork::ArtworkBinding::opaque(&binding))
                    })
                    .await
                    .map_err(string_error)
                    .and_then(|result| result.map_err(string_error));
                }
                let outcome = match (*outcome, &refresh) {
                    (library::ScanOutcome::Identical(publication), Ok(true)) => {
                        library::ScanOutcome::ArtworkChanged(publication)
                    }
                    (outcome, _) => outcome,
                };
                owner
                    .accept_scan(source.source_id(), outcome, CatalogChange::Broad)
                    .await;
            }
            Err(_) => {}
        }
        result?;
        refresh
            .map(|_| ())
            .map_err(SourceMetadataError::SavedRefreshFailed)
    })
}

pub fn identify_track_metadata(
    owner: &SourceOwner,
    _media_uri: String,
    values: TrackMetadataValues,
) -> Receiver<Result<Option<(TrackMetadataValues, Option<String>)>, String>> {
    let external = owner
        .shared
        .settings
        .load()
        .ui
        .allows_external_metadata_lookup();
    owner.reply(move |_, _| async move {
        if external {
            tokio::task::spawn_blocking(move || metadata_lookup::identify_track_metadata(&values))
                .await
                .map_err(string_error)
                .and_then(|result| result)
                .map(|found| found.map(|values| (values, None)))
        } else {
            Ok(None)
        }
    })
}

pub fn identify_album_metadata(
    owner: &SourceOwner,
    media_uri: String,
    values: AlbumMetadataValues,
) -> Receiver<Result<Option<(AlbumMetadataValues, Option<String>)>, String>> {
    let external = owner
        .shared
        .settings
        .load()
        .ui
        .allows_external_metadata_lookup();
    owner.reply(move |owner, database| async move {
        let exact = external
            && (values
                .musicbrainz_album_id
                .as_deref()
                .is_some_and(|id| !id.trim().is_empty())
                || values
                    .musicbrainz_release_group_id
                    .as_deref()
                    .is_some_and(|id| !id.trim().is_empty()));
        if exact {
            let copy = values.clone();
            if let Some(values) =
                tokio::task::spawn_blocking(move || metadata_lookup::identify_album_metadata(&copy))
                    .await
                    .map_err(string_error)??
            {
                return Ok(Some((values, None)));
            }
        }
        let native_source = library::source_entity_parts(&media_uri)
            .map(|(id, _, _)| id)
            .filter(|id| {
                owner.configuration(id).is_some_and(|configuration| {
                    matches!(configuration.kind.as_str(), "jellyfin" | "emby")
                })
            });
        let source = match native_source {
            Some(id) => tokio::task::spawn_blocking(move || owner.client(&id).ok())
                .await
                .map_err(string_error)?,
            None => None,
        };
        if let Some(source) = source
            && let Some((values, token)) = source
                .identify_album_metadata(&database, &media_uri, &values)
                .await?
        {
            return Ok(Some((values, Some(token))));
        }
        if !external {
            return Ok(None);
        }
        tokio::task::spawn_blocking(move || metadata_lookup::identify_album_metadata(&values))
            .await
            .map_err(string_error)?
            .map(|found| found.map(|values| (values, None)))
    })
}

pub fn identify_artist_metadata(
    owner: &SourceOwner,
    media_uri: String,
    values: ArtistMetadataValues,
) -> Receiver<Result<Option<(ArtistMetadataValues, Option<String>)>, String>> {
    let external = owner
        .shared
        .settings
        .load()
        .ui
        .allows_external_metadata_lookup();
    owner.reply(move |owner, database| async move {
        let exact = external
            && values
                .musicbrainz_artist_id
                .as_deref()
                .is_some_and(|id| !id.trim().is_empty());
        if exact {
            let copy = values.clone();
            if let Some(values) = tokio::task::spawn_blocking(move || {
                metadata_lookup::identify_artist_metadata(&copy)
            })
            .await
            .map_err(string_error)??
            {
                return Ok(Some((values, None)));
            }
        }
        let native_source = library::source_entity_parts(&media_uri)
            .map(|(id, _, _)| id)
            .filter(|id| {
                owner.configuration(id).is_some_and(|configuration| {
                    matches!(configuration.kind.as_str(), "jellyfin" | "emby")
                })
            });
        let source = match native_source {
            Some(id) => tokio::task::spawn_blocking(move || owner.client(&id).ok())
                .await
                .map_err(string_error)?,
            None => None,
        };
        if let Some(source) = source
            && let Some((values, token)) = source
                .identify_artist_metadata(&database, &media_uri, &values)
                .await?
        {
            return Ok(Some((values, Some(token))));
        }
        if !external {
            return Ok(None);
        }
        tokio::task::spawn_blocking(move || metadata_lookup::identify_artist_metadata(&values))
            .await
            .map_err(string_error)?
            .map(|found| found.map(|values| (values, None)))
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MetadataItemId {
    Track(String),
    Album(String),
    Artist(String),
}

impl MetadataItemId {
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
                    stored.ui.external_metadata_enabled,
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
    if bytes.len() > 32 * 1024 * 1024 {
        return Err("Artwork exceeds 32 MiB".into());
    }
    artwork::decode_rgba(&bytes, 512).map_err(string_error)?;
    let content_type = artwork::image_mime(&bytes).map(str::to_owned);
    Ok(Arc::new(sources::ImageBytes {
        bytes,
        content_type,
    }))
}
