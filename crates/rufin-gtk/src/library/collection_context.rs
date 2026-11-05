use crate::library::CatalogUi;
use crate::menus::media_menus::{
    present_playlist_entry_menu, present_playlist_entry_selection_menu, present_track_menu,
    present_track_selection_menu,
};
use std::rc::Rc;
pub fn present_track_context_menu(
    target: &gtk::Widget,
    shell: &Rc<CatalogUi>,
    media_uri: String,
    position: Option<(f64, f64)>,
) {
    if let Some(selection) = shell.current_route_track_selection(&media_uri) {
        present_track_selection_menu(target, shell.shell(), selection, position);
        return;
    }
    let catalog = shell.products.library.clone();
    let task = shell.products.runtime.spawn(async move {
        catalog
            .track(&media_uri, &library::ReadCancellation::new())
            .await
    });
    let target = target.clone();
    let window = Rc::downgrade(shell.shell());
    gtk::glib::spawn_future_local(async move {
        if let (Some(window), Ok(Ok(Some(song)))) = (window.upgrade(), task.await) {
            present_track_menu(&target, &window, song, position, None, None);
        }
    });
}
pub fn present_playlist_entry_context_menu(
    target: &gtk::Widget,
    shell: &Rc<CatalogUi>,
    playlist: library::PlaylistKey,
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
    present_playlist_entry_menu(target, shell.shell(), entry_row, position, playlist, single);
}
