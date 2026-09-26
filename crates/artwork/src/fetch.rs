use std::sync::{Arc, Mutex};

use sources::{ImageSize, SourceError, SourceImageRequest};
use tokio::runtime::Handle;

use crate::selection::Candidate;
use crate::{ExternalPolicy, SourceResolver};

#[derive(Debug)]
pub(crate) enum FetchOutcome {
    Ready(Vec<u8>),
    Missing,
}

#[derive(Clone)]
pub(crate) struct FetchContext {
    source_resolver: Arc<Mutex<Option<Arc<SourceResolver>>>>,
    database: Arc<Mutex<Option<Arc<library::Database>>>>,
}

impl FetchContext {
    pub(crate) fn new(source_resolver: Arc<Mutex<Option<Arc<SourceResolver>>>>) -> Self {
        Self {
            source_resolver,
            database: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn install_database(&self, database: Arc<library::Database>) {
        *self
            .database
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(database);
    }

    fn source(&self, source_id: &sources::SourceId) -> Result<Arc<sources::Source>, String> {
        let resolver = self
            .source_resolver
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .ok_or_else(|| "artwork source is unavailable".to_string())?;
        resolver(source_id).ok_or_else(|| "artwork source is unavailable".to_string())
    }

    pub(crate) fn fetch(
        &self,
        runtime: &Handle,
        candidate: &Candidate,
        size: ImageSize,
        policy: &ExternalPolicy,
    ) -> Result<FetchOutcome, String> {
        match candidate {
            Candidate::Playlist(image) => {
                let database = self
                    .database
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone()
                    .ok_or_else(|| "Playlist artwork storage is unavailable".to_string())?;
                runtime
                    .block_on(database.playlist_image(image))
                    .map(|bytes| {
                        bytes
                            .map(FetchOutcome::Ready)
                            .unwrap_or(FetchOutcome::Missing)
                    })
                    .map_err(|error| error.to_string())
            }
            Candidate::Artist {
                name,
                musicbrainz_id,
                ..
            } => {
                if !policy.allow_musicbrainz {
                    return Ok(FetchOutcome::Missing);
                }
                metadata_lookup::lookup_artist_image(
                    name,
                    musicbrainz_id.as_deref(),
                    size == ImageSize::Original,
                )
                .map(|bytes| {
                    bytes
                        .map(FetchOutcome::Ready)
                        .unwrap_or(FetchOutcome::Missing)
                })
            }
            Candidate::Native(image_ref) => {
                let source = self.source(&image_ref.source_id)?;
                runtime
                    .block_on(source.image(SourceImageRequest::Native {
                        image_ref: image_ref.image.clone(),
                        size,
                    }))
                    .map(|image| {
                        if image.bytes.is_empty() {
                            FetchOutcome::Missing
                        } else {
                            FetchOutcome::Ready(image.bytes)
                        }
                    })
                    .or_else(source_result)
            }
            Candidate::Local(reference) => {
                let path = match reference {
                    sources::LocalImageRef::File { path, .. }
                    | sources::LocalImageRef::Embedded { path, .. } => path,
                };
                let image = if !["http://", "https://", "smb://"]
                    .iter()
                    .any(|prefix| path.starts_with(prefix))
                {
                    sources::read_local_image(reference)
                } else {
                    runtime.block_on(
                        self.source(reference.source_id())?
                            .image(SourceImageRequest::Local(reference.clone())),
                    )
                };
                image
                    .map(|image| {
                        if image.bytes.is_empty() {
                            FetchOutcome::Missing
                        } else {
                            FetchOutcome::Ready(image.bytes)
                        }
                    })
                    .or_else(source_result)
            }
            Candidate::Album(album) => metadata_lookup::lookup_album_cover(
                album,
                match size {
                    ImageSize::Original => None,
                    ImageSize::Thumbnail(size) => Some(size),
                },
                &metadata_lookup::AlbumCoverPolicy::new(
                    policy.lastfm_api_key.clone(),
                    policy.allow_musicbrainz,
                ),
            )
            .map(|bytes| {
                bytes
                    .map(FetchOutcome::Ready)
                    .unwrap_or(FetchOutcome::Missing)
            }),
        }
    }
}

fn source_result(error: SourceError) -> Result<FetchOutcome, String> {
    match error {
        SourceError::NotFound => Ok(FetchOutcome::Missing),
        error => Err(error.to_string()),
    }
}
