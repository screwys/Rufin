//! Reviewed metadata operations and publication.
mod batch;
mod editor;
mod lyrics;
mod review;
mod scan;
use crate::runtime::CatalogChange;
use crate::source::{SourceOwner, string_error};
use async_channel::Receiver;
pub use batch::{MetadataBatch, MetadataTrackSummary};
pub use editor::{
    MetadataEditor, MetadataEditorSnapshot, MetadataFieldPresentation, MetadataSaveSummary,
    mixed_field_label,
};
use library::Database;
pub use lyrics::MetadataLyricsSettings;
pub use metadata_lookup::{ArtworkQuery, ArtworkResult};
pub use metadata_lookup::{MetadataCandidate, MetadataMatchKind};
pub use review::{
    MetadataLookupField, MetadataLookupItem, MetadataLookupProgress, MetadataLookupReview,
};
pub use sources::MetadataDraft;
use sources::{Implementation, Source, SourceEntityKind, SourceMetadataError};
use std::sync::Arc;
pub const MAX_ARTWORK_BYTES: usize = 32 * 1024 * 1024;

struct RuntimeFuture<F> {
    runtime: tokio::runtime::Handle,
    future: Option<std::pin::Pin<Box<F>>>,
}

impl<F: std::future::Future> std::future::Future for RuntimeFuture<F> {
    type Output = F::Output;
    fn poll(
        self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let this = self.get_mut();
        let _entered = this.runtime.enter();
        this.future
            .as_mut()
            .expect("metadata future")
            .as_mut()
            .poll(context)
    }
}

impl<F> Drop for RuntimeFuture<F> {
    fn drop(&mut self) {
        let _entered = self.runtime.enter();
        self.future.take();
    }
}

// Foreign callers poll and cancel on their own threads. SQLx also needs the
// application runtime when a cancelled future returns a connection to its pool.
fn on_runtime<F: std::future::Future>(
    runtime: tokio::runtime::Handle,
    future: F,
) -> RuntimeFuture<F> {
    RuntimeFuture {
        runtime,
        future: Some(Box::pin(future)),
    }
}

pub(super) struct EditSession {
    source: Option<Arc<Source>>,
    destination: EditDestination,
    previous_artwork: Option<Vec<u8>>,
    media_uri: String,
}

enum EditDestination {
    Files(sources::FileEditSession),
    Server { object: String, revision: String },
}

impl EditSession {
    pub(super) async fn open(
        owner: &SourceOwner,
        item: &MetadataItemId,
    ) -> Result<(Arc<Self>, MetadataDraft), SourceMetadataError> {
        let database = &owner.shared.database;
        let cancel = library::ReadCancellation::new();
        let source_id = owner
            .media_source_id(item.media_uri())
            .await
            .map_err(SourceMetadataError::Write)?;
        let source = match source_id {
            Some(id) => Some(
                owner
                    .open_client(id)
                    .await
                    .map_err(SourceMetadataError::Write)?,
            ),
            None => None,
        };
        let server = source.as_ref().is_some_and(|source| {
            matches!(
                source.implementation,
                Implementation::JellyfinEmby(_) | Implementation::Plex(_)
            )
        });
        let (destination, draft) = if server {
            let source = source.as_ref().unwrap();
            let error =
                |error: library::LibraryError| SourceMetadataError::Write(error.to_string());
            let (object, mut draft, binding) = match item.kind() {
                SourceEntityKind::Track => {
                    let row = database
                        .track(item.media_uri(), &cancel)
                        .await
                        .map_err(error)?
                        .ok_or(SourceMetadataError::Unavailable)?;
                    let binding = track_artwork_binding(database, &row).await?;
                    let object = row
                        .object_id
                        .clone()
                        .ok_or(SourceMetadataError::Unavailable)?;
                    let draft = match &source.implementation {
                        Implementation::JellyfinEmby(provider) => {
                            provider.read_track_metadata(row).await?
                        }
                        Implementation::Plex(provider) => provider.read_track_metadata(row).await?,
                        _ => unreachable!(),
                    };
                    (object, draft, binding)
                }
                SourceEntityKind::Album => {
                    let row = database
                        .album(item.media_uri(), false, &cancel)
                        .await
                        .map_err(error)?
                        .ok_or(SourceMetadataError::Unavailable)?;
                    let binding = row.artwork_binding.clone();
                    let object = row.object_id.clone();
                    let draft = match &source.implementation {
                        Implementation::JellyfinEmby(provider) => {
                            provider.read_album_metadata(row).await?
                        }
                        Implementation::Plex(provider) => provider.read_album_metadata(row).await?,
                        _ => unreachable!(),
                    };
                    (object, draft, binding)
                }
                SourceEntityKind::Artist => {
                    let row = database
                        .artist(item.media_uri(), false, false, &cancel)
                        .await
                        .map_err(error)?
                        .ok_or(SourceMetadataError::Unavailable)?;
                    let binding = row.artwork_binding.clone();
                    let object = row.object_id.clone();
                    let draft = match &source.implementation {
                        Implementation::JellyfinEmby(provider) => {
                            provider.read_artist_metadata(row).await?
                        }
                        Implementation::Plex(provider) => {
                            provider.read_artist_metadata(row).await?
                        }
                        _ => unreachable!(),
                    };
                    (object, draft, binding)
                }
            };
            draft.artwork = draft.artwork.with_binding(binding, true);
            (
                EditDestination::Server {
                    object,
                    revision: draft.revision.clone().unwrap_or_default(),
                },
                draft,
            )
        } else {
            let (target, draft) = sources::FileEditSession::open(
                source.clone(),
                database,
                item.media_uri().into(),
                item.kind(),
            )
            .await?;
            (EditDestination::Files(target), draft)
        };
        Ok((
            Arc::new(Self {
                source,
                media_uri: item.media_uri().to_string(),
                destination,
                previous_artwork: draft.artwork.binding.clone(),
            }),
            draft,
        ))
    }
    async fn invalidate_artwork(
        owner: &SourceOwner,
        source: Option<&Arc<Source>>,
        binding: Option<&[u8]>,
        result: &Result<(), SourceMetadataError>,
    ) -> Result<(), SourceMetadataError> {
        if matches!(
            result,
            Ok(()) | Err(SourceMetadataError::PartiallySaved { .. })
        ) {
            if let Some(binding) = binding {
                let artwork = owner.shared.artwork.clone();
                let binding = binding.to_vec();
                let invalidated = tokio::task::spawn_blocking(move || {
                    artwork.invalidate_image(&artwork::ArtworkBinding::opaque(&binding))
                })
                .await
                .map_err(string_error)
                .and_then(|result| result.map_err(string_error))
                .map_err(SourceMetadataError::SavedRefreshFailed)?;
                if invalidated && let Some(source) = source {
                    owner
                        .artwork_invalidated(source.source_id(), CatalogChange::Broad)
                        .await;
                }
            }
        }
        Ok(())
    }

    async fn save(
        &self,
        owner: &SourceOwner,
        edit: sources::MetadataEdit,
        lyrics: Option<lyrics::LyricsEdit>,
    ) -> Result<(), SourceMetadataError> {
        if edit.changes.is_empty() && edit.artwork.is_none() && lyrics.is_none() {
            return Ok(());
        }
        let metadata_changed = !edit.changes.is_empty() || edit.artwork.is_some();
        let artwork_changed = edit.artwork.is_some();
        let result = async {
            match &self.destination {
                EditDestination::Files(target) => target.save(&owner.shared.database, edit).await,
                EditDestination::Server { object, revision } => {
                    let source = self
                        .source
                        .as_ref()
                        .ok_or(SourceMetadataError::Unavailable)?;
                    let tags_saved = !edit.changes.is_empty();
                    let (object, artwork, saved) = match &source.implementation {
                        Implementation::Plex(provider) => {
                            provider.write_metadata(object, revision, &edit).await?;
                            (
                                object.clone(),
                                provider.write_artwork(object, edit.artwork.as_ref()).await,
                                tags_saved,
                            )
                        }
                        Implementation::JellyfinEmby(provider) => {
                            let object = provider.write_metadata(object, revision, &edit).await?;
                            let result = if let Some(artwork) = edit.artwork.as_ref() {
                                provider.write_artwork(&object, artwork).await
                            } else {
                                Ok(())
                            };
                            (object, result, tags_saved)
                        }
                        _ => unreachable!(),
                    };
                    let refresh = source
                        .apply_items(&owner.shared.database, vec![object], Vec::new())
                        .await
                        .map_err(|error| {
                            SourceMetadataError::SavedRefreshFailed(error.to_string())
                        });
                    sources::finish_metadata_save(artwork, refresh, saved)
                }
            }
        }
        .await;
        if artwork_changed {
            Self::invalidate_artwork(
                owner,
                self.source.as_ref(),
                self.previous_artwork.as_deref(),
                &result,
            )
            .await?;
        }
        if self.source.is_none()
            && matches!(
                &result,
                Ok(()) | Err(SourceMetadataError::PartiallySaved { .. })
            )
            && let EditDestination::Files(target) = &self.destination
            && owner
                .shared
                .database
                .track_source_by_uri(target.media_uri(), &library::ReadCancellation::new())
                .await
                .map_err(|error| SourceMetadataError::SavedRefreshFailed(error.to_string()))?
                .is_none()
        {
            owner
                .publish_current_catalog(None, CatalogChange::Broad)
                .await;
        }
        result?;
        if let Some(lyrics) = lyrics {
            let saved = self.save_lyrics(owner, &lyrics).await;
            sources::finish_metadata_save(saved.map(|_| ()), Ok(()), metadata_changed)?;
        }
        Ok(())
    }
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

async fn track_artwork_binding(
    database: &Database,
    track: &library::TrackView,
) -> Result<Option<Vec<u8>>, SourceMetadataError> {
    if track.artwork_binding.is_some() {
        return Ok(track.artwork_binding.clone());
    }
    let Some(uri) = &track.links.album_media_uri else {
        return Ok(None);
    };
    Ok(database
        .album(uri, false, &library::ReadCancellation::new())
        .await
        .map_err(|error| SourceMetadataError::Write(error.to_string()))?
        .and_then(|album| album.artwork_binding))
}
