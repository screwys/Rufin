use gtk_widgets::downloads::{OperationFeedback, OperationFeedbackKind};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;
use downloads::{DownloadEvent, DownloadFeedbackKind};
use gtk::glib;
use localization::{tr, track_count_text};

use crate::shell::Shell;

const OPERATION_FEEDBACK_DURATION: Duration = Duration::from_secs(3);

impl Shell {
    pub(crate) fn apply_download_event(self: &Rc<Self>, event: DownloadEvent) {
        match event {
            DownloadEvent::Queue {
                source_id,
                snapshot,
            } => {
                self.downloads
                    .snapshots
                    .borrow_mut()
                    .insert(source_id, Arc::clone(&snapshot));
                self.downloads.refresh_queue();
            }
            event @ (DownloadEvent::Changed { .. } | DownloadEvent::SubjectChanged { .. }) => {
                self.apply_download_change_to_mounted_route(&event);
            }
            DownloadEvent::Feedback(feedback) => {
                self.show_operation_feedback(&OperationFeedback {
                    subject: feedback.subject,
                    preview_uris: feedback.preview_uris,
                    item_count: feedback.item_count,
                    kind: match feedback.kind {
                        DownloadFeedbackKind::Started => OperationFeedbackKind::DownloadStarted,
                        DownloadFeedbackKind::Queued => OperationFeedbackKind::DownloadQueued,
                    },
                });
            }
            DownloadEvent::Notice(message) => self.show_download_notice(&message),
        }
    }

    pub(crate) fn show_operation_feedback(self: &Rc<Self>, feedback: &OperationFeedback) {
        self.show_operation_feedback_with_action(feedback, None);
    }

    pub(crate) fn show_undoable_operation_feedback(
        self: &Rc<Self>,
        feedback: &OperationFeedback,
        undo: impl FnOnce() + 'static,
    ) {
        self.show_operation_feedback_with_action(feedback, Some(Box::new(undo)));
    }

    fn show_operation_feedback_with_action(
        self: &Rc<Self>,
        feedback: &OperationFeedback,
        action: Option<Box<dyn FnOnce()>>,
    ) {
        crate::preferences::dialogs::release_notes::dismiss_release_notification(&self.preferences);
        let title = gtk_widgets::downloads::download_subject_title(&feedback.subject);
        let count = track_count_text(feedback.item_count as u64);
        let subtitle = operation_feedback_subtitle(&feedback.kind, &count);
        self.chrome
            .operation_feedback
            .remove_css_class("operation-feedback-notice");
        self.chrome.operation_feedback_artwork.set_visible(true);
        while let Some(child) = self.chrome.operation_feedback_artwork.first_child() {
            self.chrome.operation_feedback_artwork.remove(&child);
        }
        self.chrome
            .operation_feedback_artwork
            .append(&gtk_widgets::downloads::media_artwork(
                &self.artwork,
                &self.products.library,
                &self.products.runtime,
                &feedback.preview_uris,
                48,
            ));
        self.chrome.operation_feedback_title.set_text(&title);
        self.chrome.operation_feedback_subtitle.set_visible(true);
        self.chrome.operation_feedback_subtitle.set_text(&subtitle);
        let opens_queue = matches!(
            feedback.kind,
            OperationFeedbackKind::DownloadStarted | OperationFeedbackKind::DownloadQueued
        );
        self.download_feedback.feedback_opens_queue.set(opens_queue);
        self.chrome.operation_feedback_action.set_label(&tr("Undo"));
        self.chrome
            .operation_feedback_action
            .set_visible(action.is_some());
        self.download_feedback.feedback_action.replace(action);
        self.present_download_feedback();
    }

    fn show_download_notice(self: &Rc<Self>, message: &str) {
        self.chrome
            .operation_feedback
            .add_css_class("operation-feedback-notice");
        self.chrome.operation_feedback_artwork.set_visible(false);
        self.chrome.operation_feedback_title.set_text(message);
        self.chrome.operation_feedback_subtitle.set_visible(false);
        self.download_feedback.feedback_opens_queue.set(false);
        self.chrome.operation_feedback_action.set_visible(false);
        self.download_feedback.feedback_action.borrow_mut().take();
        self.present_download_feedback();
    }

    fn present_download_feedback(self: &Rc<Self>) {
        let generation = self
            .download_feedback
            .feedback_generation
            .get()
            .wrapping_add(1);
        self.download_feedback.feedback_generation.set(generation);
        self.chrome.operation_feedback.set_visible(true);
        let shell = Rc::downgrade(self);
        glib::timeout_add_local_once(OPERATION_FEEDBACK_DURATION, move || {
            let Some(shell) = shell.upgrade() else {
                return;
            };
            if shell.download_feedback.feedback_generation.get() == generation {
                shell.chrome.operation_feedback.set_visible(false);
                shell.download_feedback.feedback_action.borrow_mut().take();
            }
        });
    }

    pub(crate) fn connect_operation_feedback(self: &Rc<Self>) {
        let action_shell = Rc::downgrade(self);
        self.chrome
            .operation_feedback_action
            .connect_clicked(move |_| {
                let Some(shell) = action_shell.upgrade() else {
                    return;
                };
                shell.download_feedback.feedback_generation.set(
                    shell
                        .download_feedback
                        .feedback_generation
                        .get()
                        .wrapping_add(1),
                );
                shell.chrome.operation_feedback.set_visible(false);
                let action = shell.download_feedback.feedback_action.borrow_mut().take();
                if let Some(action) = action {
                    action();
                }
            });

        let click = gtk::GestureClick::new();
        let weak_shell = Rc::downgrade(self);
        click.connect_released(move |_, _, _, _| {
            let Some(shell) = weak_shell.upgrade() else {
                return;
            };
            if shell.download_feedback.feedback_opens_queue.get() {
                crate::preferences::present_downloads_preferences_dialog(&shell.preferences);
            }
            shell.chrome.operation_feedback.set_visible(false);
            shell.download_feedback.feedback_action.borrow_mut().take();
        });
        self.chrome.operation_feedback.add_controller(click);
    }
}

fn operation_feedback_subtitle(kind: &OperationFeedbackKind, count: &str) -> String {
    match kind {
        OperationFeedbackKind::DownloadStarted => {
            format!("{} · {count}", tr("Download started"))
        }
        OperationFeedbackKind::DownloadQueued => {
            format!("{} · {count}", tr("Download queued"))
        }
        OperationFeedbackKind::PlaylistAdded { destination } => {
            format!("{} {destination} · {count}", tr("Added to"))
        }
        OperationFeedbackKind::PlaylistRemoved { destination } => {
            format!("{} {destination} · {count}", tr("Removed from"))
        }
    }
}

#[derive(Default)]
pub(crate) struct DownloadFeedbackState {
    feedback_generation: Rc<Cell<u64>>,
    feedback_opens_queue: Cell<bool>,
    feedback_action: RefCell<Option<Box<dyn FnOnce()>>>,
}
