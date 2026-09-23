use std::cell::RefCell;

use tracing::warn;

use rufin_core::settings::app::{Settings, SettingsHandle};

pub struct SettingsState {
    pub current: RefCell<Settings>,
    pub persistence: SettingsHandle,
}

impl SettingsState {
    pub fn set_app_setting<T: PartialEq>(
        &self,
        warning_action: &'static str,
        value: T,
        field: impl FnOnce(&mut Settings) -> &mut T,
    ) -> Option<Settings> {
        self.update_app_settings(warning_action, |settings| {
            let current = field(settings);
            if *current == value {
                return false;
            }
            *current = value;
            true
        })
    }

    pub fn update_app_settings(
        &self,
        warning_action: &'static str,
        update: impl FnOnce(&mut Settings) -> bool,
    ) -> Option<Settings> {
        let mut settings = self.persistence.load();
        if !update(&mut settings) {
            return None;
        }
        settings.sanitize();
        match self.persistence.save(&settings) {
            Ok(committed) => {
                *self.current.borrow_mut() = committed.clone();
                Some(committed)
            }
            Err(error) => {
                warn!(%error, action = warning_action, "failed to save settings");
                None
            }
        }
    }
}

mod presentation;
pub use presentation::{home_block_title, library_field_title, library_list_title};

impl SettingsState {
    pub fn update_library_list_settings(
        &self,
        key: rufin_core::settings::LibraryListKey,
        update: impl FnOnce(&mut rufin_core::settings::LibraryListSettings),
    ) -> bool {
        use rufin_core::settings::LibraryListSettings;
        self.update_app_settings("library list settings", |settings| {
            if !settings.library_lists.iter().any(|entry| entry.key == key) {
                settings
                    .library_lists
                    .push(rufin_core::settings::LibraryListSettingsEntry {
                        key,
                        settings: LibraryListSettings::for_key(key),
                    });
            }
            if let Some(entry) = settings
                .library_lists
                .iter_mut()
                .find(|entry| entry.key == key)
            {
                let previous = entry.settings.clone();
                update(&mut entry.settings);
                entry.settings.sanitize(key);
                return entry.settings != previous;
            }
            false
        })
        .is_some()
    }
}
