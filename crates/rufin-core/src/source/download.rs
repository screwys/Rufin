use async_channel::Receiver;

use super::SourceOwner;

impl SourceOwner {
    pub fn download_selection(
        &self,
        selection: library::SongSelection,
        subject: downloads::DownloadSubject,
    ) -> Receiver<Result<(), String>> {
        self.reply(move |owner, database| async move {
            let uris = database
                .selected_track_uris(&selection, &library::ReadCancellation::new())
                .await
                .map_err(|error| error.to_string())?;
            owner.download_media(subject, uris);
            Ok(())
        })
    }

    pub fn remove_download_selection(
        &self,
        selection: library::SongSelection,
    ) -> Receiver<Result<(), String>> {
        self.reply(move |owner, database| async move {
            let uris = database
                .selected_track_uris(&selection, &library::ReadCancellation::new())
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
