use gtk::prelude::*;

pub fn copy_action(button: &gtk::Widget, icon: &gtk::Image) -> impl Fn(&str) + use<> {
    let feedback = crate::feedback::icon_action(button, icon);
    let button = button.downgrade();
    move |text| {
        if let Some(button) = button.upgrade() {
            button.clipboard().set_text(text);
            feedback();
        }
    }
}
