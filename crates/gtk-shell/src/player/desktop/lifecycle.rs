use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use crate::shell::Shell;

pub(crate) fn install_application_quit(shell: &Rc<Shell>) {
    shell.chrome.application.remove_action("quit");
    let quit = gio::SimpleAction::new("quit", None);
    let quit_shell = Rc::downgrade(shell);
    quit.connect_activate(move |_, _| {
        if let Some(shell) = quit_shell.upgrade() {
            shell.request_quit("application action");
        }
    });
    shell.chrome.application.add_action(&quit);
}
