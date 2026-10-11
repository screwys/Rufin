use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use tracing::warn;

use rufin_core::runtime::source::source_operation_text;

use super::Shell;

pub(super) struct StartupState {
    pub(super) route_revealed: Cell<bool>,
    pub(super) initial_launch: Cell<bool>,
    pub(super) content: RefCell<Option<Rc<super::route_position::VisibleContent>>>,
    pub(super) reveal_deadline: RefCell<Option<glib::SourceId>>,
}

const STARTUP_ROUTE_REVEAL_MAX_MS: u64 = 3_000;

impl Shell {
    pub(crate) fn enter_startup_loading(self: &Rc<Self>) {
        self.startup.initial_launch.set(false);
        self.startup.route_revealed.set(false);
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
        let status = self.library_loading_status();
        let label = &self.chrome.startup_loading_status;
        label.set_visible(status.is_some());
        label.set_text(status.as_deref().unwrap_or_default());
    }

    fn library_loading_status(&self) -> Option<String> {
        let operation = self.source.operation.borrow();
        library_loading_status(&operation, self.startup.initial_launch.get())
    }
    pub(crate) fn schedule_startup_route_reveal(self: &Rc<Self>) {
        if self.startup.route_revealed.get() {
            return;
        }
        if self.startup.reveal_deadline.borrow().is_some() {
            self.try_reveal_startup_route();
            return;
        }

        self.update_layout();
        self.prepare_startup_route_content();
        let shell = Rc::clone(self);
        let deadline = glib::timeout_add_local_once(
            Duration::from_millis(STARTUP_ROUTE_REVEAL_MAX_MS),
            move || {
                shell.startup.reveal_deadline.borrow_mut().take();
                if shell.startup.route_revealed.get() {
                    return;
                }
                if !shell.mounted_route_is_ready() {
                    warn!(
                        elapsed_ms = STARTUP_ROUTE_REVEAL_MAX_MS,
                        "startup page is still loading"
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
            || !self.has_active_mounted_route()
            || width <= 1
        {
            return;
        }
        let content = self.startup.content.borrow().clone();
        if let Some(content) = content {
            content.check();
        }
        if self.startup_route_ready_for_width(width) {
            self.commit_startup_route_reveal();
        }
    }

    fn startup_route_ready_for_width(&self, width: i32) -> bool {
        self.startup.reveal_deadline.borrow().is_some()
            && !self.startup.route_revealed.get()
            && self.mounted_route_is_ready()
            && self
                .startup
                .content
                .borrow()
                .as_ref()
                .is_some_and(|content| content.is_ready())
            && width > 1
            && self.navigation.pin_cancellation.borrow().is_none()
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

    pub(super) fn prepare_startup_content_readiness(&self) {
        if self.startup.route_revealed.get() || self.startup.content.borrow().is_some() {
            return;
        }
        self.startup
            .content
            .replace(Some(super::route_position::VisibleContent::new(
                self.layout_state.owner.upcast_ref(),
                || {},
            )));
    }

    pub(crate) fn cancel_startup_route_reveal(&self) {
        if let Some(deadline) = self.startup.reveal_deadline.borrow_mut().take() {
            deadline.remove();
        }
        self.startup.content.take();
    }
    pub(crate) fn prepare_startup_route_content(self: &Rc<Self>) {
        if self.has_active_mounted_route() || self.startup.route_revealed.get() {
            return;
        }

        self.render_current_route_content();
        self.player_ui.render_lyrics_panel();
        self.player_ui.update_bottom_player();
    }
    pub(crate) fn reveal_startup_route(self: &Rc<Self>) {
        if self.startup.route_revealed.get() {
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
    operation: &rufin_core::runtime::source::SourceOperation,
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
    use rufin_core::runtime::source::{SourceOperation, SourceProgress, SourceProgressStage};

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
