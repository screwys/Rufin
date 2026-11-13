use crate::widgets::settings::SettingsState;
use gtk::{glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};
const CONTROL_TOAST_TIMEOUT: u32 = 2;
/// One message at a time over an overlay. The message shows in a popover, its own surface, so
/// it stays above dialogs presented over the window; it sits where the overlay's label would.
pub struct ControlFeedbackState {
    generation: Rc<Cell<u64>>,
    label: glib::WeakRef<gtk::Label>,
    popover: glib::WeakRef<gtk::Popover>,
    /// The label's distance from the overlay's bottom edge.
    bottom: i32,
    settings: Rc<SettingsState>,
}
impl ControlFeedbackState {
    pub fn new(label: &gtk::Label, settings: Rc<SettingsState>) -> Rc<Self> {
        let overlay = label
            .parent()
            .and_downcast::<gtk::Overlay>()
            .expect("a feedback label is an overlay child");
        let bottom = label.margin_bottom();
        overlay.remove_overlay(label);
        for side in [
            gtk::Widget::set_margin_bottom,
            gtk::Widget::set_margin_start,
            gtk::Widget::set_margin_end,
        ] {
            side(label.upcast_ref(), 0);
        }
        label.set_visible(true);
        label.set_wrap(true);
        label.set_max_width_chars(60);
        let popover = gtk::Popover::builder()
            .child(label)
            .autohide(false)
            .has_arrow(false)
            .position(gtk::PositionType::Top)
            .can_focus(false)
            .can_target(false)
            .css_classes(["control-feedback-toast"])
            .build();
        popover.set_parent(&overlay);
        let owned = popover.clone();
        overlay.connect_destroy(move |_| owned.unparent());
        Rc::new(Self {
            generation: Rc::new(Cell::new(0)),
            label: label.downgrade(),
            popover: popover.downgrade(),
            bottom,
            settings,
        })
    }

    /// Shows `title` until the next message replaces it; returns the message's generation.
    fn show(&self, title: &str) -> Option<(u64, gtk::Popover)> {
        let (label, popover) = (self.label.upgrade()?, self.popover.upgrade()?);
        let overlay = popover.parent()?;
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        label.set_text(title);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
            overlay.width() / 2,
            overlay.height() - self.bottom,
            1,
            1,
        )));
        popover.popup();
        Some((generation, popover))
    }

    /// Shows work in progress until its result's message replaces it.
    pub fn show_working(&self, title: String) {
        self.show(&title);
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
        let Some((generation, popover)) = self.show(&title) else {
            return;
        };
        let active_generation = Rc::clone(&self.generation);
        glib::timeout_add_local_once(
            Duration::from_secs(u64::from(CONTROL_TOAST_TIMEOUT)),
            move || {
                if active_generation.get() == generation {
                    popover.popdown();
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
