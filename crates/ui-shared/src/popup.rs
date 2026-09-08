use adw::prelude::*;

const LIGHT_DISMISS_CLASS: &str = "light-dismiss-dialog";

pub fn present_light_dismiss_dialog<D>(dialog: &D, parent: &impl IsA<gtk::Widget>)
where
    D: IsA<adw::Dialog> + Clone + 'static,
{
    install_light_dismiss(dialog);
    dialog.present(Some(parent));
}

fn install_light_dismiss<D>(dialog: &D)
where
    D: IsA<adw::Dialog> + Clone + 'static,
{
    let dialog = dialog.clone().upcast::<adw::Dialog>();
    if dialog.has_css_class(LIGHT_DISMISS_CLASS) {
        return;
    }
    dialog.add_css_class(LIGHT_DISMISS_CLASS);

    let click = gtk::GestureClick::new();
    click.set_button(0);
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    let dialog_weak = dialog.downgrade();
    click.connect_pressed(move |gesture, _, x, y| {
        let Some(dialog) = dialog_weak.upgrade() else {
            return;
        };
        if dialog_child_contains(&dialog, x, y) {
            return;
        }
        gesture.set_state(gtk::EventSequenceState::Claimed);
        dialog.close();
    });
    dialog.add_controller(click);
}

fn dialog_child_contains(dialog: &adw::Dialog, x: f64, y: f64) -> bool {
    let Some(child) = dialog.child().or_else(|| dialog.first_child()) else {
        return true;
    };
    child
        .compute_bounds(dialog)
        .is_none_or(|bounds| bounds.contains_point(&gtk::graphene::Point::new(x as f32, y as f32)))
}

use std::cell::RefCell;
#[derive(Default)]
pub struct SelectedDialogState {
    dialogs: RefCell<Vec<gtk::glib::WeakRef<adw::Dialog>>>,
}

impl SelectedDialogState {
    pub fn register(&self, dialog: &adw::Dialog) {
        let mut dialogs = self.dialogs.borrow_mut();
        dialogs.retain(|current| current.upgrade().is_some());
        if !dialogs
            .iter()
            .filter_map(gtk::glib::WeakRef::upgrade)
            .any(|current| current == *dialog)
        {
            dialogs.push(dialog.downgrade());
        }
    }
}

impl Drop for SelectedDialogState {
    fn drop(&mut self) {
        for dialog in self.dialogs.get_mut().drain(..) {
            if let Some(dialog) = dialog.upgrade() {
                dialog.force_close();
            }
        }
    }
}
