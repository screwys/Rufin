use std::sync::{Arc, Mutex};

use sources::{SourceError, SourceImageRequest};
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
}

impl FetchContext {
    pub(crate) fn new(source_resolver: Arc<Mutex<Option<Arc<SourceResolver>>>>) -> Self {
        Self { source_resolver }
    }

    fn source(&self, source_id: &library::SourceId) -> Result<Arc<sources::Source>, String> {
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
        size: u32,
        policy: &ExternalPolicy,
    ) -> Result<FetchOutcome, String> {
        match candidate {
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
                size,
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
