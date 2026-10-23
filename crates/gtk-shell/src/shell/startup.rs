use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;

use rufin_core::runtime::source::source_operation_text;

use super::Shell;

pub(super) struct StartupState {
    pub(super) route_revealed: Cell<bool>,
    pub(super) initial_launch: Cell<bool>,
}

impl Shell {
    pub(crate) fn render_startup_loading_view(&self) {
        self.chrome
            .root_stack
            .set_visible_child(&self.chrome.app_root_overlay);
        self.chrome.startup_loading_host.set_visible(true);
        let operation = self.source.operation.borrow();
        let status = library_loading_status(&operation, self.startup.initial_launch.get());
        let label = &self.chrome.startup_loading_status;
        label.set_visible(status.is_some());
        label.set_text(status.as_deref().unwrap_or_default());
    }

    pub(super) fn finish_startup_route(self: &Rc<Self>) {
        if self.startup.route_revealed.replace(true) {
            return;
        }
        self.startup.initial_launch.set(false);
        self.chrome.startup_loading_host.set_visible(false);
        if let Some(selected) = self.selected_library() {
            self.products
                .source
                .selected_library_revealed(selected.source_id.clone());
        }
        self.update_layout();
    }

    pub(crate) fn prepare_startup_route_content(self: &Rc<Self>) {
        if self.has_active_mounted_route()
            || self.route_viewport.order_cancellation.borrow().is_some()
            || self.startup.route_revealed.get()
        {
            return;
        }
        self.render_current_route();
        self.player_ui.render_lyrics_panel();
        self.player_ui.update_bottom_player();
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
