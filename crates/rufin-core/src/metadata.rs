//! Reviewed metadata operations and publication.
use crate::runtime::CatalogChange;
use crate::source::{SourceOwner, string_error};
use async_channel::Receiver;
use sources::{
    AlbumMetadata, AlbumMetadataEdit, AlbumMetadataValues, ArtistMetadata, ArtistMetadataEdit,
    ArtistMetadataValues, Source, SourceMetadataError, TrackMetadata, TrackMetadataEdit,
    TrackMetadataValues,
};
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

pub fn write_reviewed_track_metadata(
    owner: &SourceOwner,
    media_uri: String,
    revision: Option<String>,
    token: Option<String>,
    edit: TrackMetadataEdit,
) -> Receiver<Result<(), SourceMetadataError>> {
    owner.reply(move |owner, database| async move {
        let target = owner.media_client(&media_uri).await;
        let _lane = owner.shared.lane.lock().await;
        let source = match target {
            Ok(source) => source,
            Err(_) => {
                return Source::write_direct_file_metadata(
                    &media_uri,
                    revision.as_deref().unwrap_or_default(),
                    &edit,
                );
            }
        };
        let outcome = source
            .write_track_metadata(
                &database,
                &media_uri,
                revision.as_deref().unwrap_or_default(),
                token.as_deref(),
                edit,
            )
            .await?;
        owner
            .accept_scan(source.source_id(), outcome, CatalogChange::Broad)
            .await;
        Ok(())
    })
}

pub fn write_reviewed_album_metadata(
    owner: &SourceOwner,
    media_uri: String,
    revision: Option<String>,
    token: Option<String>,
    edit: AlbumMetadataEdit,
) -> Receiver<Result<(), SourceMetadataError>> {
    owner.reply(move |owner, database| async move {
        let target = owner.media_client(&media_uri).await;
        let _lane = owner.shared.lane.lock().await;
        let source = target.map_err(|_| SourceMetadataError::Unavailable)?;
        let outcome = source
            .write_album_metadata(
                &database,
                &media_uri,
                revision.as_deref().unwrap_or_default(),
                token.as_deref(),
                edit,
            )
            .await?;
        owner
            .accept_scan(source.source_id(), outcome, CatalogChange::Broad)
            .await;
        Ok(())
    })
}

pub fn write_reviewed_artist_metadata(
    owner: &SourceOwner,
    media_uri: String,
    revision: Option<String>,
    token: Option<String>,
    edit: ArtistMetadataEdit,
) -> Receiver<Result<(), SourceMetadataError>> {
    owner.reply(move |owner, database| async move {
        let target = owner.media_client(&media_uri).await;
        let _lane = owner.shared.lane.lock().await;
        let source = target.map_err(|_| SourceMetadataError::Unavailable)?;
        let outcome = source
            .write_artist_metadata(
                &database,
                &media_uri,
                revision.as_deref().unwrap_or_default(),
                token.as_deref(),
                edit,
            )
            .await?;
        owner
            .accept_scan(source.source_id(), outcome, CatalogChange::Broad)
            .await;
        Ok(())
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
