use super::{EditDestination, EditSession, MetadataEditor};
use sources::{LyricsStorage, SourceEntityKind, SourceMetadataError as Error};
use std::sync::Arc;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct LyricsEdit {
    pub text: String,
    pub storage: LyricsStorage,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct MetadataLyricsSettings {
    pub storages: Vec<LyricsStorage>,
    pub storage: LyricsStorage,
    pub can_search: bool,
    pub artist: String,
    pub title: String,
    pub text: Option<String>,
}

impl MetadataEditor {
    async fn lyrics_session(&self) -> Result<Arc<EditSession>, Error> {
        if self.draft.kind != SourceEntityKind::Track {
            return Err(Error::Unavailable);
        }
        if let Some(session) = &self.session {
            return Ok(session.clone());
        }
        let scope = self.batch.as_ref().ok_or(Error::Unavailable)?;
        if scope.indices.len() != 1 {
            return Err(Error::Unavailable);
        }
        scope.batch.session(scope.indices[0]).await
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl MetadataEditor {
    pub async fn lyrics_settings(&self) -> Result<MetadataLyricsSettings, Error> {
        let session = self.lyrics_session().await?;
        let storages = match &session.destination {
            EditDestination::Files(_) => self
                .source
                .shared
                .runtime
                .spawn_blocking(move || {
                    let EditDestination::Files(files) = &session.destination else {
                        unreachable!()
                    };
                    files.lyrics_storage()
                })
                .await
                .map_err(|error| Error::Write(error.to_string()))?,
            EditDestination::Server { .. } => {
                if let Some(source) = session.source.clone() {
                    let database = self.source.shared.database.clone();
                    let uri = session.media_uri.clone();
                    self.source
                        .shared
                        .runtime
                        .spawn(async move { source.lyrics_storage(&database, &uri).await })
                        .await
                        .map_err(|error| Error::Write(error.to_string()))?
                } else {
                    Vec::new()
                }
            }
        };
        let settings = self.source.shared.settings.load().ui;
        let preferred = if settings.lyrics.preferences.save_lyrics_as_sidecar {
            LyricsStorage::Sidecar
        } else {
            LyricsStorage::Embedded
        };
        let state = self.state();
        let storage = state
            .lyrics
            .as_ref()
            .map(|lyrics| lyrics.storage)
            .filter(|storage| storages.contains(storage))
            .unwrap_or_else(|| {
                if storages.contains(&preferred) {
                    preferred
                } else {
                    storages.first().copied().unwrap_or(LyricsStorage::Server)
                }
            });
        Ok(MetadataLyricsSettings {
            can_search: settings
                .lyrics
                .preferences
                .external_lyrics_network_allowed(settings.application.private_mode),
            storages,
            storage,
            artist: state.values.get("artist").cloned().unwrap_or_default(),
            title: state.values.get("title").cloned().unwrap_or_default(),
            text: state.lyrics.as_ref().map(|lyrics| lyrics.text.clone()),
        })
    }

    pub async fn search_lyrics(
        &self,
        artist: String,
        title: String,
    ) -> Result<Vec<::lyrics::LyricsSearchResult>, Error> {
        let settings = self.source.shared.settings.load().ui;
        if !settings
            .lyrics
            .preferences
            .external_lyrics_network_allowed(settings.application.private_mode)
        {
            return Ok(Vec::new());
        }
        if artist.trim().is_empty() && title.trim().is_empty() {
            return Ok(Vec::new());
        }
        let providers = settings.lyrics.preferences.external_lyrics_providers;
        self.source.shared.runtime.spawn(async move { ::lyrics::search_lyrics(&providers, artist.trim(), title.trim()).await })
            .await.map_err(|error| Error::Write(error.to_string()))?.map_err(Error::Write)
    }

    pub async fn resolve_lyrics(
        &self,
        result: ::lyrics::LyricsSearchResult,
    ) -> Result<Option<String>, Error> {
        let settings = self.source.shared.settings.load().ui;
        if !settings
            .lyrics
            .preferences
            .external_lyrics_network_allowed(settings.application.private_mode)
            || !settings
                .lyrics
                .preferences
                .external_lyrics_providers
                .contains(&result.provider)
        {
            return Ok(None);
        }
        let bundle = self
            .source
            .shared
            .runtime
            .spawn(async move { ::lyrics::lyrics_from_search_result(&result).await })
            .await
            .map_err(|error| Error::Write(error.to_string()))?
            .map_err(Error::Write)?;
        Ok(bundle.and_then(|bundle| {
            bundle
                .selected_document(&settings.lyrics.preferences)
                .map(|document| ::lyrics::lyrics_to_lrc_text(document, 0))
        }))
    }

    pub fn set_lyrics(&self, text: String, storage: LyricsStorage) {
        self.state().lyrics = Some(LyricsEdit { text, storage });
    }
}

impl EditSession {
    pub(super) async fn save_lyrics(
        &self,
        owner: &crate::source::SourceOwner,
        edit: &LyricsEdit,
    ) -> Result<(), Error> {
        let text = ::lyrics::lyrics_from_edited_text(&edit.text)
            .and_then(|bundle| {
                bundle
                    .documents()
                    .first()
                    .map(|document| ::lyrics::lyrics_to_lrc_text(document, 0))
            })
            .ok_or_else(|| {
                Error::InvalidInput(localization::tr(localization::msgid("No lyrics")))
            })?;
        match &self.destination {
            EditDestination::Files(files) => {
                files
                    .save_lyrics(
                        &owner.shared.database,
                        &text,
                        edit.storage == LyricsStorage::Sidecar,
                    )
                    .await?
            }
            EditDestination::Server { .. } => {
                let source = self.source.as_ref().ok_or(Error::Unavailable)?;
                source
                    .write_lyrics(
                        &owner.shared.database,
                        &self.media_uri,
                        &text,
                        edit.storage == LyricsStorage::Sidecar,
                    )
                    .await?;
                source.refresh_written_lyrics().await?;
            }
        }
        if let Ok(playback) = owner.shared.playback() {
            playback.lyrics.accept_edited(&self.media_uri, &text).await;
        }
        Ok(())
    }
}
