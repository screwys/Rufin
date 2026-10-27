use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gtk::glib;

use tokio::sync::watch;
use tracing::warn;

use rufin_core::settings::app::{Settings, SettingsHandle};

/// `current` holds the committed preferences as of the last `sync`.
pub struct SettingsState {
    pub current: RefCell<Settings>,
    pub persistence: SettingsHandle,
    changes: RefCell<watch::Receiver<Arc<Settings>>>,
}

impl SettingsState {
    pub fn new(persistence: SettingsHandle) -> Self {
        let changes = persistence.preference_changes();
        let current = Settings::clone(&changes.borrow());
        Self {
            current: RefCell::new(current),
            persistence,
            changes: RefCell::new(changes),
        }
    }

    /// Keeps `current` on what every writer commits. `sidebar_moved` runs when such a commit
    /// changed the sidebar before a local write took it in.
    pub fn follow(self: &Rc<Self>, sidebar_moved: impl Fn() + 'static) {
        let mut changes = self.changes.borrow().clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while changes.changed().await.is_ok() {
                changes.borrow_and_update();
                let Some(state) = weak.upgrade() else { break };
                let Some(previous) = state.sync() else {
                    continue;
                };
                if previous.sidebar != state.current.borrow().sidebar {
                    sidebar_moved();
                }
            }
        });
    }

    /// Takes the latest committed preferences and returns the ones they replaced.
    pub fn sync(&self) -> Option<Settings> {
        let next = {
            let mut changes = self.changes.borrow_mut();
            if !changes.has_changed().unwrap_or(false) {
                return None;
            }
            Arc::clone(&changes.borrow_and_update())
        };
        Some(self.current.replace(Settings::clone(&next)))
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
                self.sync();
                changed.then(|| self.current.borrow().clone())
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
            Ok(changed) => {
                if changed {
                    self.sync();
                }
                changed
            }
            Err(error) => {
                warn!(%error, "failed to save library list settings");
                false
            }
        }
    }
}
