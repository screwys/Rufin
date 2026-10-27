use crate::shell::Shell;
use gtk_widgets::favorites::favorite_button_is_active;
use library::FavoriteTarget;
use std::rc::Rc;
impl Shell {
    pub(crate) fn clear_favorite_controls(&self) {
        if let Some(session) = self.selected_ui.session() {
            session.favorites.clear_favorite_controls();
        }
    }
}
impl Shell {
    pub(crate) fn toggle_current_track_favorite(self: &Rc<Self>) {
        let Some(media_uri) = self
            .player_ui
            .selected_playback()
            .as_deref()
            .and_then(|player| player.transport.current.as_ref())
            .map(|entry| entry.row.track.media_uri.clone())
        else {
            return;
        };
        let favorite =
            !favorite_button_is_active(&self.player_ui.views.player_controls.favorite_button);
        self.set_favorite(FavoriteTarget::Track(media_uri), favorite);
    }

    pub(crate) fn update_visible_favorite_buttons(&self, item: &FavoriteTarget, favorite: bool) {
        if let Some(session) = self.selected_ui.session() {
            session
                .favorites
                .update_visible_favorite_buttons(item, favorite);
        }
    }

    /// Hearts change when the favorite is committed, from the publication that follows.
    pub(crate) fn set_favorite(self: &Rc<Self>, item: FavoriteTarget, favorite: bool) {
        self.products.source.set_favorite(item, favorite);
    }

    /// Shows a committed favorite on every visible heart of the item.
    pub(crate) fn apply_favorite_state(&self, item: &FavoriteTarget, favorite: bool) {
        self.update_visible_favorite_buttons(item, favorite);
        self.player_ui.set_bottom_player_favorite(item, favorite);
    }
}
