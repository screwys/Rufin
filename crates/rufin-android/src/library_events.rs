use std::sync::Arc;

use rufin_core::{
    favorites::FavoriteIntents,
    runtime::{SourceEvent, SourceNoticeKind},
};

use crate::host::{AndroidError, error};

#[derive(uniffi::Record)]
pub struct AndroidLibraryEvent {
    pub favorite: Option<rufin_core::runtime::FavoriteSettlement>,
    pub notice: Option<String>,
}

#[derive(uniffi::Object)]
pub struct AndroidLibraryEvents {
    events: async_channel::Receiver<SourceEvent>,
    intents: Arc<FavoriteIntents>,
    changes: tokio::sync::Mutex<tokio::sync::watch::Receiver<u64>>,
}

impl AndroidLibraryEvents {
    pub(crate) fn new(events: async_channel::Receiver<SourceEvent>) -> Self {
        let intents = Arc::new(FavoriteIntents::default());
        Self {
            events,
            changes: tokio::sync::Mutex::new(intents.subscribe()),
            intents,
        }
    }
    pub(crate) fn intents(&self) -> Arc<FavoriteIntents> {
        self.intents.clone()
    }
}

#[uniffi::export]
impl AndroidLibraryEvents {
    pub fn favorite_intents(&self) -> Arc<FavoriteIntents> {
        self.intents.clone()
    }

    pub async fn next(&self) -> Result<AndroidLibraryEvent, AndroidError> {
        let mut changes = self.changes.lock().await;
        loop {
            let mut favorite = None;
            let mut notice = None;
            tokio::select! {
                result = changes.changed() => { result.map_err(error)?; },
                event = self.events.recv() => match event.map_err(error)? {
                    SourceEvent::CatalogPublished(publication) => {
                        if let Some(settlement) = publication.favorite {
                            // The caller releases its intent when the favorite operation finishes.
                            if self.intents.projected_item_favorite(&settlement.target, settlement.requested) == settlement.requested {
                                favorite = Some(settlement);
                            }
                        } else { continue; }
                    }
                    SourceEvent::Notice(source_notice) => notice = Some(localization::tr(match source_notice.kind {
                        SourceNoticeKind::ServerUnreachable => "Server is unreachable",
                        SourceNoticeKind::FavoriteRejected => "Could not update favorites",
                    })),
                    _ => continue,
                }
            }
            changes.borrow_and_update();
            return Ok(AndroidLibraryEvent { favorite, notice });
        }
    }
}
