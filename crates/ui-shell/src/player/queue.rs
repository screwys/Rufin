use crate::shell::Shell;
use crate::shell::playlist_picker::append_context_menu_picker_media_uris;
use adw::prelude::*;
use localization::msgid;
use rufin_core::settings::ContextMenuItem;
use std::{rc::Rc, sync::Arc};
use ui_player::queue::QueueSelectionSnapshot;
use ui_shared::{
    controls::{PLAY_ICON, REMOVE_ICON},
    interactions::ContextMenuSurface,
};

pub(crate) fn connect_queue_playlist_button(shell: &Rc<Shell>) {
    let weak = Rc::downgrade(shell);
    shell
        .player_ui
        .right_panel
        .queue_playlist_button
        .connect_clicked(move |_| {
            let Some(shell) = weak.upgrade() else {
                return;
            };
            let queue = shell.products.playback.queue.clone();
            let task = shell
                .products
                .runtime
                .spawn_blocking(move || queue.media_uris());
            let weak = Rc::downgrade(&shell);
            gtk::glib::spawn_future_local(async move {
                let result = task.await;
                let Some(shell) = weak.upgrade() else {
                    return;
                };
                match result {
                    Ok(Ok(tracks)) => shell.new_playlist_dialog_with(String::new(), tracks),
                    Ok(Err(error)) => shell.control_feedback.show_feedback_toast(error),
                    Err(error) => shell
                        .control_feedback
                        .show_feedback_toast(error.to_string()),
                }
            });
        });
}
pub(crate) fn present_queue_selection_context_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    selection: QueueSelectionSnapshot,
    position: Option<(f64, f64)>,
) {
    let surface = ContextMenuSurface::new(target, "queue-selection", position);
    surface.append_fixed_action(msgid("Remove from Queue"), "remove-from-queue", REMOVE_ICON);
    surface.append_configurable_action(ContextMenuItem::Play, msgid("Play"), "play", PLAY_ICON);
    append_context_menu_picker_media_uris(&surface, shell, selection.media_uris.iter().cloned());
    let queue = shell.products.playback.queue.clone();
    let remove = Arc::clone(&selection.occurrences);
    surface.add_action("remove-from-queue", move || {
        queue.remove_many(remove.to_vec())
    });
    let queue = shell.products.playback.queue.clone();
    let play = selection.occurrences.first().cloned();
    surface.add_action_enabled("play", play.is_some(), move || {
        if let Some(play) = play.as_ref() {
            queue.activate(play.clone());
        }
    });
    surface.popup(&shell.settings.current.borrow().context_menu);
}
