use super::{LibraryListKey, LibraryListSettings, LibraryListSettingsEntry, SettingsOwner};

impl SettingsOwner {
    pub fn update_library_list_settings(
        &self,
        key: LibraryListKey,
        update: impl FnOnce(&mut LibraryListSettings),
    ) -> Result<Option<super::app::Settings>, String> {
        let previous = self.file.load();
        let changed = self.file.update(move |stored| {
            let previous = stored.ui.library_list(key);
            let mut next = previous.clone();
            update(&mut next);
            next.sanitize(key);
            if next == previous {
                return Ok(false);
            }
            if let Some(entry) = stored
                .ui
                .library_lists
                .iter_mut()
                .find(|entry| entry.key == key)
            {
                entry.settings = next;
            } else {
                stored.ui.library_lists.push(LibraryListSettingsEntry {
                    key,
                    settings: next,
                });
            }
            Ok(true)
        })?;
        if !changed {
            return Ok(None);
        }
        let current = self.file.load();
        (self.on_change)(&previous, &current, false);
        Ok(Some(current.ui))
    }
}
