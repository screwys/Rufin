use std::cell::RefCell;

use tracing::warn;

use super::{Settings, SettingsHandle};
use crate::shell::Shell;

pub(crate) struct SettingsState {
    pub(crate) current: RefCell<Settings>,
    pub(crate) persistence: SettingsHandle,
}

impl Shell {
    pub(crate) fn set_app_setting<T: PartialEq>(
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

    pub(crate) fn update_app_settings(
        &self,
        warning_action: &'static str,
        update: impl FnOnce(&mut Settings) -> bool,
    ) -> Option<Settings> {
        let mut settings = self.settings.persistence.load();
        if !update(&mut settings) {
            return None;
        }
        settings.sanitize();
        match self.settings.persistence.save(&settings) {
            Ok(committed) => {
                *self.settings.current.borrow_mut() = committed.clone();
                Some(committed)
            }
            Err(error) => {
                warn!(%error, action = warning_action, "failed to save settings");
                None
            }
        }
    }
}
