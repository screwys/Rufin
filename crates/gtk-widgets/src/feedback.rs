use crate::settings::SettingsState;
use gtk::{glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};
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
        if !self
            .settings
            .current
            .borrow()
            .application
            .control_notifications_enabled
        {
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

/// Briefly replace an action icon with a checkmark.
pub fn icon_action(button: &gtk::Widget, icon: &gtk::Image) -> impl Fn() + use<> {
    let reset = Rc::new(RefCell::new(None::<gtk::glib::SourceId>));
    let original = icon.icon_name();
    let restore = original.clone();
    let pending = reset.clone();
    let image = icon.downgrade();
    button.connect_unrealize(move |_| {
        if let Some(source) = pending.borrow_mut().take() {
            source.remove();
        }
        if let Some(image) = image.upgrade() {
            image.set_icon_name(restore.as_deref());
        }
    });
    let icon = icon.downgrade();
    move || {
        if let Some(source) = reset.borrow_mut().take() {
            source.remove();
        }
        if let Some(icon) = icon.upgrade() {
            icon.set_icon_name(Some("rufin-object-select-symbolic"));
        }
        let icon = icon.clone();
        let pending = reset.clone();
        let original = original.clone();
        reset.replace(Some(gtk::glib::timeout_add_local_once(
            std::time::Duration::from_millis(1500),
            move || {
                pending.borrow_mut().take();
                if let Some(icon) = icon.upgrade() {
                    icon.set_icon_name(original.as_deref());
                }
            },
        )));
    }
}
