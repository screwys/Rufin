//! A detail page's picture, changed from its cover through the editor that already changes it:
//! a playlist's or smart playlist's own editor, an album's or artist's metadata editor.

use std::rc::Rc;

use library::{PlaylistListRow, Row};

use crate::shell::shell::Shell;

/// Whether `row`'s cover can change its picture; `edit_metadata` is whether its menu offers
/// Edit metadata.
pub(crate) async fn artwork_writable(shell: &Shell, row: &Row, edit_metadata: bool) -> bool {
    match row {
        Row::Playlist(PlaylistListRow::Playlist(playlist)) => {
            rufin_core::playlists::playlist_artwork_editing(
                &shell.products.source,
                playlist.playlist_key,
            )
            .recv()
            .await
            .is_ok_and(|editing| editing.is_ok_and(|editing| editing.writable))
        }
        Row::Playlist(PlaylistListRow::SmartPlaylist(_)) => true,
        Row::Album(_) | Row::AlbumHeading(_) | Row::Artist(_) => edit_metadata,
        _ => false,
    }
}

impl Shell {
    /// Opens the editor that changes `row`'s picture.
    pub(crate) fn edit_row_artwork(self: &Rc<Self>, row: Row) {
        match row {
            Row::Playlist(PlaylistListRow::Playlist(playlist)) => {
                self.edit_playlist_dialog(playlist.playlist_key, playlist.name)
            }
            Row::Playlist(PlaylistListRow::SmartPlaylist(playlist)) => {
                self.edit_smart_playlist_dialog(playlist)
            }
            row => {
                if let Some(item) = crate::menus::media_menus::metadata_item(&row) {
                    crate::editors::metadata::present_metadata_dialog(self, item, true);
                }
            }
        }
    }
}
