use std::collections::BTreeMap;
use std::sync::Arc;

use async_channel::Receiver;
use library::{Change, Scope, SourceKey, TrackKey};
use sources::{SourceConfiguration, SourceError, SourceId};

use super::{Shared, SourceOwner};
use crate::features::follow;
use crate::runtime::source::{SourceOperation, SourceProgress, SourceProgressStage};

/// Download rules: the download queue reconciles the rules of the sources a Change touched.
pub(super) async fn follow_rules(shared: Arc<Shared>, changes: Option<Vec<Arc<Change>>>) {
    let ruled = shared
        .settings
        .load()
        .ui
        .downloads
        .iter()
        .any(|settings| !settings.rules.is_empty());
    let heard = |change: &Arc<Change>| {
        change.scopes.iter().any(|scope| {
            matches!(
                scope,
                Scope::Reload
                    | Scope::Tracks {
                        source: Some(_),
                        ..
                    }
                    | Scope::Collections { .. }
                    | Scope::Source { .. }
                    | Scope::State { .. }
                    | Scope::Playlist { entries: true, .. }
                    | Scope::Playlists
            )
        })
    };
    if ruled
        && changes
            .as_ref()
            .is_none_or(|changes| changes.iter().any(heard))
    {
        shared.downloads.library_changed(changes);
    }
}

impl SourceOwner {
    /// Installs automatic artwork and lyrics downloads while either is on: one pass over every
    /// source when installed, then fetches for the tracks the change feed reports as added.
    pub(crate) fn asset_downloads_changed(&self) {
        let (artwork, lyrics) = self.shared.settings.cache_downloads();
        let mut installed = self
            .shared
            .asset_downloads
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(task) = installed.take() {
            task.abort();
            self.publish_asset_operation(SourceOperation::Idle);
        }
        if artwork || lyrics {
            *installed = Some(follow(
                &self.shared.runtime,
                &self.shared.catalog,
                Arc::downgrade(&self.shared),
                download_added,
            ));
        }
    }

    pub(crate) fn source_input_digest(&self, source_id: &SourceId) -> [u8; 32] {
        self.configuration(source_id)
            .and_then(|configuration| configuration.input_identity().ok())
            .map_or([0; 32], |input| input.digest)
    }

    /// Fetches artwork and lyrics for every source, or only for these tracks.
    async fn download_assets(&self, only: Option<&BTreeMap<SourceKey, Vec<TrackKey>>>) {
        let (artwork, lyrics) = self.shared.settings.cache_downloads();
        for configured in self.shared.settings.load().sources.configured {
            let source_id = &configured.configuration.source_id;
            let Ok(Some(source_key)) = self.shared.catalog.source_identity_key(source_id).await
            else {
                continue;
            };
            let keys = match only {
                Some(only) => match only.get(&source_key) {
                    Some(keys) => Some(keys.as_slice()),
                    None => continue,
                },
                None => None,
            };
            for (stage, enabled) in [
                (SourceProgressStage::Artwork, artwork),
                (
                    SourceProgressStage::Lyrics,
                    lyrics && !configured.configuration.is_local(),
                ),
            ] {
                if !enabled {
                    continue;
                }
                if let Err(error) = self
                    .download_source_assets(&configured.configuration, source_key, stage, keys)
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
        source_key: SourceKey,
        stage: SourceProgressStage,
        keys: Option<&[TrackKey]>,
    ) -> Result<(), String> {
        let source_id = &configuration.source_id;
        let catalog = &self.shared.catalog;
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
            let mut chunks = keys.map(|keys| keys.chunks(128));
            loop {
                let page = match &mut chunks {
                    Some(chunks) => match chunks.next() {
                        Some(keys) => catalog
                            .track_key_artwork_bindings(keys)
                            .await
                            .map_err(super::string_error)?,
                        None => break,
                    },
                    None => {
                        let page = catalog
                            .source_artwork_page(source_key, after.as_deref(), 128, &cancel_read)
                            .await
                            .map_err(super::string_error)?;
                        if page.is_empty() {
                            break;
                        }
                        after = page.last().cloned();
                        page
                    }
                };
                if completed == 0 {
                    report(0);
                }
                for binding in page {
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
                let page = catalog
                    .uncached_source_lyrics_page(source_key, input, after, keys, &cancel_read)
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
                    let input = if library::source_entity_parts(&uri).is_some() {
                        input
                    } else {
                        [0; 32]
                    };
                    match source.cached_lyrics(catalog, &uri, input).await {
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
        self.reply(move |owner, catalog| async move {
            let uris = catalog
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
        self.reply(move |owner, catalog| async move {
            let uris = catalog
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

/// Fetches artwork and lyrics for the tracks the Changes added, or for every source.
async fn download_added(shared: Arc<Shared>, changes: Option<Vec<Arc<Change>>>) {
    let mut added = BTreeMap::<SourceKey, Vec<TrackKey>>::new();
    let mut full = changes.is_none();
    for scope in changes.iter().flatten().flat_map(|change| &change.scopes) {
        match scope {
            Scope::Reload => full = true,
            Scope::Tracks {
                source: Some(source),
                added: keys,
                ..
            } => added.entry(*source).or_default().extend(keys),
            _ => {}
        }
    }
    added.retain(|_, keys| !keys.is_empty());
    if full || !added.is_empty() {
        SourceOwner { shared }
            .download_assets((!full).then_some(&added))
            .await;
    }
}
