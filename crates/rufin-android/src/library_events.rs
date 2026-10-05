use std::sync::Arc;

use library::FavoriteTarget;
use rufin_core::{
    favorites::FavoriteIntents,
    runtime::{SourceEvent, SourceNoticeKind},
};

use crate::host::{AndroidError, error};

#[derive(uniffi::Record)]
pub struct AndroidFavoriteSettlement {
    pub kind: String,
    pub media_uri: String,
    pub effective: bool,
}

#[derive(uniffi::Record)]
pub struct AndroidLibraryEvent {
    pub favorite: Option<AndroidFavoriteSettlement>,
    pub notice: Option<String>,
}

pub(crate) fn favorite_target(kind: &str, uri: String) -> Result<FavoriteTarget, AndroidError> {
    match kind {
        "track" => Ok(FavoriteTarget::Track(uri)),
        "album" | "album_header" => Ok(FavoriteTarget::Album(uri)),
        "artist" => Ok(FavoriteTarget::Artist(uri)),
        _ => Err(error("This item has no favorite action")),
    }
}

fn identity(target: FavoriteTarget) -> (String, String) {
    match target {
        FavoriteTarget::Track(uri) => ("track".into(), uri),
        FavoriteTarget::Album(uri) => ("album".into(), uri),
        FavoriteTarget::Artist(uri) => ("artist".into(), uri),
    }
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
    pub fn projected_favorite(
        &self,
        kind: String,
        media_uri: String,
        fallback: bool,
    ) -> Result<bool, AndroidError> {
        Ok(self
            .intents
            .projected_item_favorite(&favorite_target(&kind, media_uri)?, fallback))
    }
    pub fn set_favorite_intent(
        &self,
        kind: String,
        media_uri: String,
        favorite: bool,
    ) -> Result<(), AndroidError> {
        self.intents
            .set_pending(favorite_target(&kind, media_uri)?, favorite);
        Ok(())
    }

    pub fn release_favorite_intent(
        &self,
        kind: String,
        media_uri: String,
        requested: bool,
    ) -> Result<(), AndroidError> {
        self.intents
            .response_matches_pending(&favorite_target(&kind, media_uri)?, requested);
        Ok(())
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
                                let (kind, media_uri) = identity(settlement.target);
                                favorite = Some(AndroidFavoriteSettlement { kind, media_uri, effective: settlement.effective });
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
