use super::SettingsOwner;
use sources::SourceId;

impl SettingsOwner {
    pub(crate) fn import_playlist_pins_once(
        &self,
        imports: Vec<(SourceId, Vec<String>)>,
    ) -> Result<bool, String> {
        let current = self.load();
        if imports.iter().all(|(source, _)| {
            current
                .sidebar
                .playlist_pin_imported_sources
                .contains(source)
        }) {
            return Ok(false);
        }
        let mut changed = false;
        self.restore(
            |stored| {
                for (source, ids) in imports {
                    changed |= stored.ui.sidebar.import_playlist_pins_once(source, ids);
                }
                Ok(())
            },
            false,
        )?;
        Ok(changed)
    }
}
