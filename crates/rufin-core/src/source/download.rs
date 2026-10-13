use async_channel::Receiver;

use super::SourceOwner;

impl SourceOwner {
    pub fn download_selection(
        &self,
        selection: library::SongSelection,
        context: String,
        title: Option<String>,
    ) -> Receiver<Result<(), String>> {
        self.reply(move |owner, database| async move {
            let uris = match selection {
                library::SongSelection::Uris(uris) => uris.to_vec(),
                selection => database
                    .selected_track_uris(&selection, None, &library::ReadCancellation::new())
                    .await
                    .map_err(|error| error.to_string())?,
            };
            let subject =
                downloads::DownloadSubject::for_media_uris(&context, title.as_deref(), &uris);
            owner.download_media(subject, uris);
            Ok(())
        })
    }

    pub fn remove_download_selection(
        &self,
        selection: library::SongSelection,
    ) -> Receiver<Result<(), String>> {
        self.reply(move |owner, database| async move {
            let uris = match selection {
                library::SongSelection::Uris(uris) => uris.to_vec(),
                selection => database
                    .selected_track_uris(&selection, None, &library::ReadCancellation::new())
                    .await
                    .map_err(|error| error.to_string())?,
            };
            owner.remove_download_media(uris);
            Ok(())
        })
    }

    pub fn remove_download_media(&self, media_uris: Vec<String>) {
        self.shared.downloads.remove(media_uris, true);
    }
}
