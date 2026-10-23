use std::cell::{OnceCell, RefCell};
use std::future::Future;
use std::rc::Rc;
use tokio::sync::{mpsc, oneshot};

use tracing::warn;

use rufin_core::settings::app::{Settings, SettingsHandle};

pub struct SettingsState {
    pub current: RefCell<Settings>,
    pub persistence: SettingsHandle,
    edits: OnceCell<mpsc::UnboundedSender<SettingsEdit>>,
}

struct SettingsEdit {
    update: Box<dyn FnOnce(&mut Settings) -> bool + Send>,
    reply: oneshot::Sender<Result<bool, String>>,
}

fn edit_settings(
    persistence: &SettingsHandle,
    update: impl FnOnce(&mut Settings) -> bool,
) -> Result<bool, String> {
    let mut changed = false;
    persistence.update_preferences(|settings| {
        changed = update(settings);
        if changed {
            settings.sanitize();
        }
        Ok(())
    })?;
    Ok(changed)
}

impl SettingsState {
    pub fn new(current: Settings, persistence: SettingsHandle) -> Self {
        Self {
            current: RefCell::new(current),
            persistence,
            edits: OnceCell::new(),
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
        self.finish_edit(warning_action, edit_settings(&self.persistence, update))
    }

    /// Queue the edit now; awaiting the result refreshes the GTK settings projection.
    pub fn update_app_settings_async<F>(
        self: &Rc<Self>,
        runtime: &tokio::runtime::Handle,
        warning_action: &'static str,
        update: F,
    ) -> impl Future<Output = Option<Settings>> + use<F>
    where
        F: FnOnce(&mut Settings) -> bool + Send + 'static,
    {
        let sender = self.edits.get_or_init(|| {
            let (sender, mut receiver) = mpsc::unbounded_channel::<SettingsEdit>();
            let persistence = self.persistence.clone();
            runtime.spawn(async move {
                while let Some(edit) = receiver.recv().await {
                    let persistence = persistence.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        edit_settings(&persistence, edit.update)
                    })
                    .await
                    .unwrap_or_else(|error| Err(error.to_string()));
                    let _ = edit.reply.send(result);
                }
            });
            sender
        });
        let (reply, result) = oneshot::channel();
        let _ = sender.send(SettingsEdit {
            update: Box::new(update),
            reply,
        });
        let state = Rc::clone(self);
        async move {
            let result = result.await.unwrap_or_else(|error| Err(error.to_string()));
            state.finish_edit(warning_action, result)
        }
    }

    fn finish_edit(
        &self,
        warning_action: &'static str,
        result: Result<bool, String>,
    ) -> Option<Settings> {
        match result {
            Ok(changed) => {
                // Another edit may already have committed while GTK awaited this result.
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
