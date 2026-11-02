use crate::player::queue::QueueSelectionSnapshot;
use crate::shell::shell::Shell;
use crate::shell::shell::playlist_picker::append_context_menu_picker_media_uris;
use crate::widgets::{
    controls::{PLAY_ICON, REMOVE_ICON},
    interactions::ContextMenuSurface,
};
use adw::prelude::*;
use localization::msgid;
use rufin_core::playback::QueueSelection;
use rufin_core::settings::ContextMenuItem;
use std::rc::Rc;

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
            let queue = shell.products.playback.clone();
            let weak = Rc::downgrade(&shell);
            gtk::glib::spawn_future_local(async move {
                let result = queue.selection().await;
                let Some(shell) = weak.upgrade() else {
                    return;
                };
                match result {
                    Ok(tracks) => {
                        shell.new_playlist_dialog_with(String::new(), tracks.as_ref().clone())
                    }
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
    let queue = shell.products.playback.clone();
    let remove = selection.selection.clone();
    surface.append_fixed_action(msgid("Remove from Queue"), REMOVE_ICON, move || {
        queue.remove_many(remove.clone())
    });
    let queue = shell.products.playback.clone();
    let play = selection.selection.clone();
    surface.append_configurable_action(
        ContextMenuItem::Play,
        msgid("Play"),
        PLAY_ICON,
        move || match &play {
            QueueSelection::Occurrences(ids) => {
                if let Some(id) = ids.first() {
                    queue.activate_occurrence(*id);
                }
            }
            QueueSelection::Positions(positions) => {
                if let Some(position) = positions.first() {
                    queue.activate(*position);
                }
            }
        },
    );
    append_context_menu_picker_media_uris(&surface, shell, selection.media_uris.iter().cloned());
    surface.popup(&shell.settings.current.borrow().application.context_menu);
}
