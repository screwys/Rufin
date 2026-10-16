use async_channel::Receiver;

use super::SourceOwner;
use crate::runtime::source::{SourceOperation, SourceProgress, SourceProgressStage};
use sources::{SourceError, SourceId, SourceResult};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[cfg_attr(feature = "bindings", uniffi::export)]
impl SourceOwner {
    pub fn download_artwork(&self, source_id: SourceId) -> SourceResult<()> {
        self.start_asset_download(source_id, SourceProgressStage::Artwork)
    }

    pub fn download_lyrics(&self, source_id: SourceId) -> SourceResult<()> {
        self.start_asset_download(source_id, SourceProgressStage::Lyrics)
    }

    pub fn cancel_source_download(&self, source_id: SourceId) {
        if let Some((source, token)) = self
            .shared
            .asset_download
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            && source == &source_id
            && let Some(token) = token.upgrade()
        {
            token.cancel();
        }
    }
}

impl SourceOwner {
    pub(crate) fn source_input_digest(&self, source_id: &SourceId) -> [u8; 32] {
        self.configuration(source_id)
            .and_then(|configuration| configuration.input_identity().ok())
            .map_or([0; 32], |input| input.digest)
    }

    fn start_asset_download(
        &self,
        source_id: SourceId,
        stage: SourceProgressStage,
    ) -> SourceResult<()> {
        let configured = super::configured_source(&self.shared.settings.load().sources, &source_id)
            .map_err(SourceError::Other)?;
        let cancelled = Arc::new(CancellationToken::new());
        {
            let mut active = self
                .shared
                .asset_download
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if let Some(previous) = active.take().and_then(|(_, token)| token.upgrade()) {
                previous.cancel();
            }
            *active = Some((source_id.clone(), Arc::downgrade(&cancelled)));
        }
        let owner = self.clone();
        self.shared.runtime.spawn(async move {
            let mut reported_at = std::time::Instant::now();
            let mut report = |completed| {
                if completed == 0 || reported_at.elapsed() >= std::time::Duration::from_millis(100)
                {
                    owner.publish_asset_operation(
                        &cancelled,
                        SourceOperation::Downloading {
                            source_id: source_id.clone(),
                            progress: SourceProgress {
                                stage,
                                completed,
                                total: None,
                            },
                        },
                    );
                    reported_at = std::time::Instant::now();
                }
            };
            report(0);
            let result = cancelled
                .run_until_cancelled(async {
                    let database = &owner.shared.database;
                    let source_key = database
                        .source_identity_key(&source_id)
                        .await
                        .map_err(super::string_error)?
                        .ok_or_else(super::source_access_unavailable)?;
                    let cancel_read = library::ReadCancellation::new();
                    let folder = match configured.music_folder_id.as_deref() {
                        Some(id) => Some(
                            database
                                .catalog_keys(
                                    library::CatalogKind::Folder,
                                    Some(source_key),
                                    &[(id).to_owned()],
                                    &cancel_read,
                                )
                                .await
                                .map(|keys| {
                                    keys.into_iter().next().map(library::FolderKey::from_raw)
                                })
                                .map_err(super::string_error)?
                                .ok_or_else(super::source_access_unavailable)?,
                        ),
                        None => None,
                    };
                    let mut completed = 0;
                    if stage == SourceProgressStage::Artwork {
                        let mut after = None;
                        loop {
                            let page = database
                                .source_artwork_page(
                                    source_key,
                                    folder,
                                    after.as_deref(),
                                    128,
                                    &cancel_read,
                                )
                                .await
                                .map_err(super::string_error)?;
                            if page.is_empty() {
                                break;
                            }
                            after = page.last().cloned();
                            for binding in page {
                                owner
                                    .shared
                                    .artwork
                                    .cache_source(&binding)
                                    .await
                                    .map_err(super::string_error)?;
                                completed += 1;
                                report(completed);
                            }
                        }
                    } else {
                        let source = owner.open_client(source_id.clone()).await?;
                        let input = configured
                            .configuration
                            .input_identity()
                            .map_err(super::string_error)?
                            .digest;
                        let mut after = None;
                        loop {
                            let page = database
                                .source_track_uri_page(source_key, folder, after, &cancel_read)
                                .await
                                .map_err(super::string_error)?;
                            if page.is_empty() {
                                break;
                            }
                            after = page.last().map(|(key, _)| *key);
                            for (_, uri) in page {
                                let input = if library::source_entity_parts(&uri).is_some() {
                                    input
                                } else {
                                    [0; 32]
                                };
                                source
                                    .cached_lyrics(database, &uri, input)
                                    .await
                                    .map_err(super::string_error)?;
                                completed += 1;
                                report(completed);
                            }
                        }
                    }
                    Ok::<_, String>(())
                })
                .await;
            owner.publish_asset_operation(
                &cancelled,
                match result {
                    Some(Err(message)) => SourceOperation::Failed {
                        source_id: Some(source_id),
                        message,
                        add_form: false,
                    },
                    _ => SourceOperation::Idle,
                },
            );
        });
        Ok(())
    }

    fn publish_asset_operation(&self, token: &Arc<CancellationToken>, operation: SourceOperation) {
        let mut active = self
            .shared
            .asset_download
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if active
            .as_ref()
            .is_none_or(|(_, current)| !current.ptr_eq(&Arc::downgrade(token)))
        {
            return;
        }
        let finished = !matches!(operation, SourceOperation::Downloading { .. });
        if finished {
            *active = None;
        }
        if self.shared.operation.borrow().blocks_library()
            || (!finished && token.is_cancelled())
            || (finished
                && !matches!(
                    *self.shared.operation.borrow(),
                    SourceOperation::Downloading { .. }
                ))
        {
            return;
        }
        self.shared.operation.send_replace(operation.clone());
        let _ = self
            .shared
            .outputs
            .events
            .try_send(crate::runtime::SourceEvent::Operation(operation));
    }
}

impl SourceOwner {
    pub fn download_selection(
        &self,
        selection: library::TrackSelection,
        context: String,
        title: Option<String>,
    ) -> Receiver<Result<(), String>> {
        self.reply(move |owner, database| async move {
            let uris = database
                .selected_track_uris(&selection, None, &library::ReadCancellation::new())
                .await
                .map_err(|error| error.to_string())?;
            let subject =
                downloads::DownloadSubject::for_media_uris(&context, title.as_deref(), &uris);
            owner.download_media(subject, uris);
            Ok(())
        })
    }

    pub fn remove_download_selection(
        &self,
        selection: library::TrackSelection,
    ) -> Receiver<Result<(), String>> {
        self.reply(move |owner, database| async move {
            let uris = database
                .selected_track_uris(&selection, None, &library::ReadCancellation::new())
                .await
                .map_err(|error| error.to_string())?;
            owner.remove_download_media(uris);
            Ok(())
        })
    }

    pub fn remove_download_media(&self, media_uris: Vec<String>) {
        self.shared.downloads.remove(media_uris, true);
    }
}
