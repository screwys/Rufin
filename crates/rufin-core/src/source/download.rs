use async_channel::Receiver;

use super::SourceOwner;
use crate::runtime::source::{SourceOperation, SourceProgress, SourceProgressStage};
use sources::{SourceConfiguration, SourceError, SourceId};
use std::sync::{Arc, atomic::Ordering};

impl SourceOwner {
    pub(crate) fn source_input_digest(&self, source_id: &SourceId) -> [u8; 32] {
        self.configuration(source_id)
            .and_then(|configuration| configuration.input_identity().ok())
            .map_or([0; 32], |input| input.digest)
    }

    pub(crate) fn request_asset_downloads(&self) {
        self.shared.asset_download_requested.notify_one();
    }

    pub(crate) fn start_asset_downloader(&self) {
        let weak = Arc::downgrade(&self.shared);
        let requested = Arc::clone(&self.shared.asset_download_requested);
        let task = self.shared.runtime.spawn(async move {
            loop {
                requested.notified().await;
                let Some(shared) = weak.upgrade() else { break };
                if !shared.started.load(Ordering::Acquire) {
                    break;
                }
                SourceOwner { shared }.download_assets().await;
            }
        });
        *self
            .shared
            .asset_download
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(task);
        self.request_asset_downloads();
    }

    fn asset_download_enabled(&self, stage: SourceProgressStage) -> bool {
        let (artwork, lyrics) = self.shared.settings.cache_downloads();
        match stage {
            SourceProgressStage::Artwork => artwork,
            SourceProgressStage::Lyrics => lyrics,
            _ => false,
        }
    }

    async fn download_assets(&self) {
        for configured in self.shared.settings.load().sources.configured {
            for stage in [SourceProgressStage::Artwork, SourceProgressStage::Lyrics] {
                if !self.asset_download_enabled(stage)
                    || (stage == SourceProgressStage::Lyrics && configured.configuration.is_local())
                {
                    continue;
                }
                if let Err(error) = self
                    .download_source_assets(&configured.configuration, stage)
                    .await
                {
                    tracing::warn!(source = %configured.configuration.name, %error, "could not download source assets");
                }
            }
        }
        self.publish_asset_operation(SourceOperation::Idle);
    }

    async fn download_source_assets(
        &self,
        configuration: &SourceConfiguration,
        stage: SourceProgressStage,
    ) -> Result<(), String> {
        let source_id = &configuration.source_id;
        let database = &self.shared.database;
        let Some(source_key) = database
            .source_identity_key(source_id)
            .await
            .map_err(super::string_error)?
        else {
            return Ok(());
        };
        let cancel_read = library::ReadCancellation::new();
        let mut reported_at = std::time::Instant::now();
        let mut report = |completed| {
            if completed == 0 || reported_at.elapsed() >= std::time::Duration::from_millis(100) {
                self.publish_asset_operation(SourceOperation::Downloading {
                    source_id: source_id.clone(),
                    progress: SourceProgress {
                        stage,
                        completed,
                        total: None,
                    },
                });
                reported_at = std::time::Instant::now();
            }
        };
        let mut completed = 0;
        if stage == SourceProgressStage::Artwork {
            let mut after = None;
            loop {
                let page = database
                    .source_artwork_page(source_key, after.as_deref(), 128, &cancel_read)
                    .await
                    .map_err(super::string_error)?;
                if page.is_empty() {
                    break;
                }
                if completed == 0 {
                    report(0);
                }
                after = page.last().cloned();
                for binding in page {
                    if !self.asset_download_enabled(stage) {
                        return Ok(());
                    }
                    self.shared
                        .artwork
                        .cache_source(&binding)
                        .await
                        .map_err(super::string_error)?;
                    completed += 1;
                    report(completed);
                }
            }
        } else {
            let input = configuration
                .input_identity()
                .map_err(super::string_error)?
                .digest;
            let mut source = None;
            let mut after = None;
            loop {
                let page = database
                    .uncached_source_lyrics_page(source_key, input, after, &cancel_read)
                    .await
                    .map_err(super::string_error)?;
                if page.is_empty() {
                    break;
                }
                if completed == 0 {
                    report(0);
                }
                let source = match &source {
                    Some(source) => source,
                    None => source.insert(self.open_client(source_id.clone()).await?),
                };
                after = page.last().map(|(key, _)| *key);
                for (_, uri) in page {
                    if !self.asset_download_enabled(stage) {
                        return Ok(());
                    }
                    let input = if library::source_entity_parts(&uri).is_some() {
                        input
                    } else {
                        [0; 32]
                    };
                    match source.cached_lyrics(database, &uri, input).await {
                        Ok(_) => {}
                        Err(
                            error @ (SourceError::Network(_)
                            | SourceError::Auth(_)
                            | SourceError::Tls(_)
                            | SourceError::Cancelled),
                        ) => return Err(error.to_string()),
                        // One unreadable track must not stop the rest of the source.
                        Err(error) => tracing::warn!(%error, uri, "could not download lyrics"),
                    }
                    completed += 1;
                    report(completed);
                }
            }
        }
        Ok(())
    }

    fn publish_asset_operation(&self, operation: SourceOperation) {
        let finished = !matches!(operation, SourceOperation::Downloading { .. });
        if self.shared.operation.borrow().blocks_library()
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
        selection: library::MemberSelection,
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
        selection: library::MemberSelection,
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
