use std::cell::RefCell;

use tracing::warn;

use rufin_core::settings::app::{Settings, SettingsHandle};

pub struct SettingsState {
    pub current: RefCell<Settings>,
    pub persistence: SettingsHandle,
}

impl SettingsState {
    pub fn new(current: Settings, persistence: SettingsHandle) -> Self {
        Self {
            current: RefCell::new(current),
            persistence,
        }
    }

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
        let mut changed = false;
        let result = self.persistence.update_preferences(|settings| {
            changed = update(settings);
            Ok(())
        });
        match result {
            Ok(()) => {
                let committed = self.persistence.load();
                *self.current.borrow_mut() = committed.clone();
                changed.then_some(committed)
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
        match self.persistence.update_library_list_settings(key, update) {
            Ok(Some(committed)) => {
                *self.current.borrow_mut() = committed;
                true
            }
            Ok(None) => false,
            Err(error) => {
                warn!(%error, "failed to save library list settings");
                false
            }
        }
    }
}
