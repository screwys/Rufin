use crate::CatalogUi;
use gtk_media_menus::media_menus::{
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
        present_track_selection_menu(target, &shell.media_menus, selection, position);
        return;
    }
    let database = shell.library.clone();
    let task = shell.runtime.spawn(async move {
        library::CatalogQuery::uris(library::CatalogKind::Track, vec![media_uri])
            .rows(&database, 0, 1, &library::ReadCancellation::new())
            .await
    });
    let target = target.clone();
    let menus = Rc::downgrade(&shell.media_menus);
    gtk::glib::spawn_future_local(async move {
        if let (Some(menus), Ok(Ok(library::CatalogRows::Tracks(mut rows)))) =
            (menus.upgrade(), task.await)
            && let Some(song) = rows.pop()
        {
            present_track_menu(&target, &menus, song, position, None, None);
        }
    });
}
pub fn present_playlist_entry_context_menu(
    target: &gtk::Widget,
    shell: &Rc<CatalogUi>,
    playlist: library::PlaylistKey,
    entry: library::PlaylistEntryKey,
    entry_row: library::PlaylistEntryView,
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
