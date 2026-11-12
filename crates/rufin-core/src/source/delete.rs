use std::collections::BTreeMap;

use async_channel::Receiver;
use library::{MemberSelection, ReadCancellation, SourceId};

use super::SourceOwner;

impl SourceOwner {
    /// Delete permanently: the files of the tracks of `selection`, by each source that holds
    /// them or, for opened files, where they were opened from; then the downloads of what was
    /// deleted.
    pub fn delete_selection(&self, selection: MemberSelection) -> Receiver<Result<(), String>> {
        self.reply(move |owner, catalog| async move {
            let settings = owner.shared.settings.load();
            if !settings.ui.application.allow_permanent_delete {
                return Err("Preferences do not allow permanently deleting tracks".to_owned());
            }
            let uris = catalog
                .selected_track_uris(&selection, None, &ReadCancellation::new())
                .await
                .map_err(|error| error.to_string())?;
            let mut sources = BTreeMap::<SourceId, Vec<_>>::new();
            for track in catalog
                .playables_by_uri(&uris)
                .await
                .map_err(|error| error.to_string())?
            {
                match track.source_id.clone() {
                    Some(source) => sources.entry(source).or_default().push(track),
                    None => sources::delete_opened_track(&track)
                        .await
                        .map_err(|error| error.to_string())?,
                }
            }
            let mut result = Ok(());
            for (id, tracks) in sources {
                let source = owner.client(&id)?;
                let (deleted, deletion) = source.delete_tracks(&catalog, &tracks).await;
                owner.remove_download_media(deleted);
                if let Err(error) = deletion {
                    result = Err(error.to_string());
                    break;
                }
            }
            result
        })
    }
}
