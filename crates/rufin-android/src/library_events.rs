use rufin_core::runtime::{SourceEvent, SourceNoticeKind};

use crate::host::{AndroidError, error};

#[derive(uniffi::Record)]
pub struct AndroidLibraryEvent {
    /// Favorites a committed change set; rows show them as they are now.
    pub favorites: Vec<rufin_core::runtime::FavoriteState>,
    pub notice: Option<String>,
}

#[derive(uniffi::Object)]
pub struct AndroidLibraryEvents {
    events: async_channel::Receiver<SourceEvent>,
    settings_errors: tokio::sync::Mutex<tokio::sync::watch::Receiver<Option<String>>>,
}

impl AndroidLibraryEvents {
    pub(crate) fn new(
        events: async_channel::Receiver<SourceEvent>,
        settings_errors: tokio::sync::watch::Receiver<Option<String>>,
    ) -> Self {
        Self {
            events,
            settings_errors: tokio::sync::Mutex::new(settings_errors),
        }
    }
}

#[uniffi::export]
impl AndroidLibraryEvents {
    pub async fn next(&self) -> Result<AndroidLibraryEvent, AndroidError> {
        let mut settings_errors = self.settings_errors.lock().await;
        loop {
            let mut favorites = Vec::new();
            let mut notice = None;
            tokio::select! {
                result = settings_errors.changed() => {
                    result.map_err(error)?;
                    notice = settings_errors.borrow_and_update().clone();
                    if notice.is_none() { continue; }
                },
                event = self.events.recv() => match event.map_err(error)? {
                    SourceEvent::CatalogPublished(publication) if !publication.favorites.is_empty() => {
                        favorites = publication.favorites;
                    }
                    SourceEvent::Notice(source_notice) => notice = Some(localization::tr(match source_notice.kind {
                        SourceNoticeKind::ServerUnreachable => "Server is unreachable",
                        SourceNoticeKind::FavoriteRejected => "Could not update favorites",
                    })),
                    _ => continue,
                }
            }
            return Ok(AndroidLibraryEvent { favorites, notice });
        }
    }
}
