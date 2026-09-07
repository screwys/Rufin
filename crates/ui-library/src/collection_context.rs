use crate::CatalogUi;
use std::rc::Rc;
use ui_shared::media_menus::{
    present_playback_media_menu, present_playlist_entry_menu,
    present_playlist_entry_selection_menu, present_track_selection_menu,
};
pub fn present_track_context_menu(
    target: &gtk::Widget,
    shell: &Rc<CatalogUi>,
    media_uri: String,
    position: Option<(f64, f64)>,
) {
    if let Some(selection) = shell.current_route_track_selection(&media_uri) {
        present_track_selection_menu(target, &shell.media_menus, selection, position);
        return;
    }
    present_playback_media_menu(
        target,
        &shell.media_menus,
        media_uri,
        None,
        position,
        None,
        None,
    );
}
pub fn present_playlist_entry_context_menu(
    target: &gtk::Widget,
    shell: &Rc<CatalogUi>,
    playlist: library::PlaylistKey,
    entry: library::PlaylistEntryKey,
    entry_row: library::PlaylistEntryRow,
    position: Option<(f64, f64)>,
) {
    if let Some(selection) = shell.current_playlist_entry_selection(entry) {
        present_playlist_entry_selection_menu(target, &shell.media_menus, selection, position);
        return;
    }
    present_playlist_entry_menu(
        target,
        &shell.media_menus,
        entry_row,
        position,
        playlist,
        shell
            .current_playlist_entry_selection_owner()
            .map(|selection| selection.single_entry(entry)),
    );
}
