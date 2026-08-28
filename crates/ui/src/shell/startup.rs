use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use tracing::warn;

use crate::preferences::source::source_operation_text;

use super::Shell;

pub(super) struct StartupState {
    pub(super) route_revealed: Cell<bool>,
    pub(super) initial_launch: Cell<bool>,
    pub(super) route_allocated: Cell<bool>,
    pub(super) reveal_deadline: RefCell<Option<glib::SourceId>>,
}

const STARTUP_ROUTE_REVEAL_MAX_MS: u64 = 3_000;

impl Shell {
    pub(crate) fn enter_startup_loading(self: &Rc<Self>) {
        self.startup.initial_launch.set(false);
        self.startup.route_revealed.set(false);
        self.startup.route_allocated.set(false);
        self.cancel_startup_route_reveal();
        self.clear_mounted_routes();
        self.render_startup_loading_view();
        self.update_layout();
    }

    pub(crate) fn render_startup_loading_view(&self) {
        self.chrome
            .root_stack
            .set_visible_child(&self.chrome.app_root_overlay);
        self.chrome.startup_loading_host.set_visible(true);
        while let Some(child) = self.chrome.startup_loading_host.first_child() {
            self.chrome.startup_loading_host.remove(&child);
        }
        self.chrome
            .startup_loading_host
            .append(&self.startup_loading_view());
    }
    pub(crate) fn startup_loading_view(&self) -> gtk::Widget {
        let wrapper = gtk::Box::new(gtk::Orientation::Vertical, 0);
        wrapper.add_css_class("startup-loading-page");
        wrapper.set_hexpand(true);
        wrapper.set_vexpand(true);
        wrapper.set_halign(gtk::Align::Center);
        wrapper.set_valign(gtk::Align::Center);

        let spinner = gtk::Spinner::new();
        spinner.start();
        wrapper.append(&spinner);
        let status = self.library_loading_status();
        if let Some(status) = status {
            let label = gtk::Label::new(Some(&status));
            label.add_css_class("dim-label");
            label.add_css_class("startup-loading-status");
            label.set_wrap(true);
            wrapper.append(&label);
        }
        wrapper.upcast()
    }

    fn library_loading_status(&self) -> Option<String> {
        let operation = self.source.operation.borrow();
        library_loading_status(&operation, self.startup.initial_launch.get())
    }
    pub(crate) fn schedule_startup_route_reveal(self: &Rc<Self>) {
        if self.startup.route_revealed.get() || self.source.login_screen_active() {
            return;
        }
        if self.startup.reveal_deadline.borrow().is_some() {
            self.try_reveal_startup_route();
            return;
        }

        self.startup.route_allocated.set(false);
        self.update_layout();
        self.begin_startup_cover_prime();
        self.prepare_startup_route_content();
        let shell = Rc::clone(self);
        let deadline = glib::timeout_add_local_once(
            Duration::from_millis(STARTUP_ROUTE_REVEAL_MAX_MS),
            move || {
                shell.startup.reveal_deadline.borrow_mut().take();
                if shell.startup.route_revealed.get() || shell.source.login_screen_active() {
                    shell.finish_startup_cover_prime_gate();
                    return;
                }
                let pending_covers = shell.startup_cover_prime_pending_count();
                if pending_covers > 0 {
                    warn!(
                        pending_covers,
                        elapsed_ms = STARTUP_ROUTE_REVEAL_MAX_MS,
                        "startup route cover prime expired"
                    );
                }
                shell.reveal_startup_route();
            },
        );
        self.startup.reveal_deadline.replace(Some(deadline));
        self.try_reveal_startup_route();
    }

    pub(crate) fn try_reveal_startup_route(self: &Rc<Self>) {
        let width = self.layout_width().min(self.chrome.root_stack.width());
        if !self.startup_route_ready_for_width(width) {
            return;
        }
        self.reveal_startup_route();
    }

    pub(in crate::shell) fn finish_startup_route_allocation(self: &Rc<Self>, width: i32) {
        if self.startup.reveal_deadline.borrow().is_none()
            || self.startup.route_revealed.get()
            || self.source.login_screen_active()
            || !self.has_active_mounted_route()
            || width <= 1
        {
            return;
        }
        self.position_startup_queue_for_reveal();
        self.startup.route_allocated.set(true);
        if self.startup_route_ready_for_width(width) {
            self.commit_startup_route_reveal();
        }
    }

    fn startup_route_ready_for_width(&self, width: i32) -> bool {
        self.startup.reveal_deadline.borrow().is_some()
            && !self.startup.route_revealed.get()
            && !self.source.login_screen_active()
            && self.has_active_mounted_route()
            && self.startup.route_allocated.get()
            && width > 1
            && self.startup_cover_prime_pending_count() == 0
    }

    fn commit_startup_route_reveal(self: &Rc<Self>) {
        self.startup.initial_launch.set(false);
        self.startup.route_revealed.set(true);
        self.cancel_startup_route_reveal();
        self.chrome.startup_loading_host.set_visible(false);
        if let Some(source) = self.selected_source_operations() {
            source.selected_library_revealed();
        }
    }

    pub(crate) fn cancel_startup_route_reveal(&self) {
        if let Some(deadline) = self.startup.reveal_deadline.borrow_mut().take() {
            deadline.remove();
        }
        self.finish_startup_cover_prime_gate();
    }
    pub(crate) fn prepare_startup_route_content(self: &Rc<Self>) {
        if self.has_active_mounted_route()
            || self.startup.route_revealed.get()
            || self.source.login_screen_active()
        {
            return;
        }

        self.render_current_route_content();
        self.render_queue_panel();
        self.render_lyrics_panel();
        self.update_bottom_player();
    }
    pub(crate) fn reveal_startup_route(self: &Rc<Self>) {
        if self.source.login_screen_active() || self.startup.route_revealed.get() {
            return;
        }

        if !self.has_active_mounted_route() {
            self.prepare_startup_route_content();
        }
        self.commit_startup_route_reveal();

        self.update_layout();
    }
}

fn library_loading_status(
    operation: &crate::runtime::source::SourceOperation,
    initial_launch: bool,
) -> Option<String> {
    if initial_launch {
        return Some(localization::tr("Preparing library..."));
    }
    source_operation_text(operation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::source::{SourceOperation, SourceProgress, SourceProgressStage};

    #[test]
    fn launch_is_preparing_while_an_explicit_change_is_switching() {
        let saved = sources::SourceId::new("saved");
        let operation = SourceOperation::Switching {
            target: saved.clone(),
            progress: SourceProgress {
                stage: SourceProgressStage::Connecting,
                completed: 0,
                total: None,
            },
        };
        assert_eq!(
            library_loading_status(&operation, true).as_deref(),
            Some("Preparing library...")
        );
        assert_eq!(
            library_loading_status(&operation, false).as_deref(),
            Some("Switching library...")
        );
    }
}
