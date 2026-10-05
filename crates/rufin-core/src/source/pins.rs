use async_channel::Receiver;
use library::ReadCancellation;

use super::SourceOwner;
use crate::SettingsHandle;

impl SourceOwner {
    pub fn import_playlist_pins_once(
        &self,
        settings: SettingsHandle,
    ) -> Receiver<Result<bool, String>> {
        let stored = settings.load();
        let sources = self
            .list_sources()
            .sources
            .iter()
            .filter(|source| {
                source.kind != "local"
                    && !stored
                        .sidebar
                        .playlist_pin_imported_sources
                        .contains(&source.id)
            })
            .map(|source| source.id.clone())
            .collect::<Vec<_>>();
        self.reply(move |_, database| async move {
            let mut imports = Vec::new();
            let cancellation = ReadCancellation::new();
            for source_id in sources {
                let Some(source) = database
                    .source_identity_key(&source_id)
                    .await
                    .map_err(|error| error.to_string())?
                else {
                    continue;
                };
                let ids = database
                    .source_playlist_object_ids(source, &cancellation)
                    .await
                    .map_err(|error| error.to_string())?;
                if !ids.is_empty() {
                    imports.push((source_id, ids));
                }
            }
            settings.import_playlist_pins_once(imports)
        })
    }
}
