use crate::library::CatalogUi;
use crate::menus::media_menus::{
    present_playlist_entry_menu, present_playlist_entry_selection_menu, present_row_menu,
    present_track_selection_menu,
};
use std::rc::Rc;
pub fn present_track_context_menu(
    target: &gtk::Widget,
    shell: &Rc<CatalogUi>,
    track: library::TrackRow,
    position: Option<(f64, f64)>,
) {
    if let Some(selection) = shell.current_route_track_selection(&track.media_uri) {
        present_track_selection_menu(target, shell.shell(), selection, position);
        return;
    }
    present_row_menu(
        target,
        shell.shell(),
        library::Row::Track(track),
        None,
        position,
    );
}
pub fn present_playlist_entry_context_menu(
    target: &gtk::Widget,
    shell: &Rc<CatalogUi>,
    entry: library::PlaylistEntryKey,
    entry_row: library::TrackRow,
    position: Option<(f64, f64)>,
) {
    if let Some(selection) = shell.current_playlist_entry_selection(entry) {
        present_playlist_entry_selection_menu(target, shell.shell(), selection, position);
        return;
    }
    let single = shell
        .current_playlist_entry_selection_owner()
        .zip(entry_row.entry.clone())
        .map(|(selection, entry)| selection.single_entry(entry));
    present_playlist_entry_menu(target, shell.shell(), entry_row, position, single);
}
