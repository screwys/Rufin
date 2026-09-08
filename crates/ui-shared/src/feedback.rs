use crate::settings::SettingsState;
use gtk::{glib, prelude::*};
use std::{cell::Cell, rc::Rc, time::Duration};
const CONTROL_TOAST_TIMEOUT: u32 = 2;
pub struct ControlFeedbackState {
    generation: Rc<Cell<u64>>,
    label: glib::WeakRef<gtk::Label>,
    settings: Rc<SettingsState>,
}
impl ControlFeedbackState {
    pub fn new(label: &gtk::Label, settings: Rc<SettingsState>) -> Rc<Self> {
        Rc::new(Self {
            generation: Rc::new(Cell::new(0)),
            label: label.downgrade(),
            settings,
        })
    }
}
impl ControlFeedbackState {
    pub fn show_control_feedback_toast(&self, title: String) {
        if !self.settings.current.borrow().control_notifications_enabled {
            return;
        }
        self.show_feedback_toast(title);
    }

    /// Default presenter for informational toasts: one message, no animation or close button.
    /// Use `show_control_feedback_toast` for optional playback-control notifications.
    pub fn show_feedback_toast(&self, title: String) {
        let Some(label) = self.label.upgrade() else {
            return;
        };
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        label.set_text(&title);
        label.set_visible(true);
        let label = label.clone();
        let active_generation = Rc::clone(&self.generation);
        glib::timeout_add_local_once(
            Duration::from_secs(u64::from(CONTROL_TOAST_TIMEOUT)),
            move || {
                if active_generation.get() == generation {
                    label.set_visible(false);
                }
            },
        );
    }
}
