use rufin_core::runtime::{SourceEvent, SourceNoticeKind};

use crate::host::{AndroidError, error};

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
    /// The next notice to show: a settings save failure or a source notice.
    pub async fn next(&self) -> Result<String, AndroidError> {
        let mut settings_errors = self.settings_errors.lock().await;
        loop {
            tokio::select! {
                result = settings_errors.changed() => {
                    result.map_err(error)?;
                    if let Some(notice) = settings_errors.borrow_and_update().clone() {
                        return Ok(notice);
                    }
                },
                event = self.events.recv() => if let SourceEvent::Notice(notice) = event.map_err(error)? {
                    return Ok(localization::tr(match notice.kind {
                        SourceNoticeKind::ServerUnreachable => "Server is unreachable",
                        SourceNoticeKind::FavoriteRejected => "Could not update favorites",
                    }));
                },
            }
        }
    }
}
