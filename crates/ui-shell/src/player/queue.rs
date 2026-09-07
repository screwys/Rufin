use crate::shell::Shell;
use crate::shell::playlist_picker::append_context_menu_picker_media_uris;
use localization::msgid;
use rufin_core::settings::ContextMenuItem;
use std::{rc::Rc, sync::Arc};
use ui_player::queue::QueueSelectionSnapshot;
use ui_shared::{
    controls::{PLAY_ICON, REMOVE_ICON},
    interactions::ContextMenuSurface,
};
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
