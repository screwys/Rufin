use async_channel::Receiver;
use library::{FolderKey, SourceKey};

use super::SourceOwner;
use crate::playback::PlaybackTarget;

impl SourceOwner {
    pub fn download_target(
        &self,
        target: PlaybackTarget,
        source: Option<SourceKey>,
        folder: Option<FolderKey>,
    ) -> Receiver<Result<(), String>> {
        self.reply(move |owner, database| async move {
            let media_uris = target.resolve_media_uris(&database, source, folder).await?;
            owner.download_media(target.download_subject(), media_uris);
            Ok(())
        })
    }

    pub fn remove_download_target(
        &self,
        target: PlaybackTarget,
        source: Option<SourceKey>,
        folder: Option<FolderKey>,
    ) -> Receiver<Result<(), String>> {
        self.reply(move |owner, database| async move {
            let media_uris = target.resolve_media_uris(&database, source, folder).await?;
            owner.remove_download_media(media_uris);
            Ok(())
        })
    }

    pub fn remove_download_media(&self, media_uris: Vec<String>) {
        self.shared.downloads.remove(media_uris, true);
    }
}
