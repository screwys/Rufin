use crate::shell::Shell;
use library::FavoriteTarget;
use localization::tr;
use std::rc::Rc;
use ui_shared::favorites::{favorite_button_is_active, set_favorite_button_active};
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
            .map(|entry| entry.media_uri.clone())
        else {
            return;
        };
        let target = FavoriteTarget::Track(media_uri);
        let favorite = !self.projected_item_favorite(
            &target,
            favorite_button_is_active(&self.player_ui.views.player_controls.favorite_button),
        );
        self.set_favorite_with_feedback(
            target,
            favorite,
            Some(&self.player_ui.views.player_controls.favorite_button),
        );
    }

    pub(crate) fn projected_track_favorite(
        &self,
        media_uri: &str,
        playback_fallback: bool,
    ) -> bool {
        self.projected_item_favorite(
            &FavoriteTarget::Track(media_uri.to_string()),
            playback_fallback,
        )
    }

    pub(crate) fn projected_item_favorite(&self, item: &FavoriteTarget, fallback: bool) -> bool {
        self.selected_ui.session().map_or(fallback, |session| {
            session.favorites.projected_item_favorite(item, fallback)
        })
    }

    pub(crate) fn update_visible_favorite_buttons(&self, item: &FavoriteTarget, favorite: bool) {
        if let Some(session) = self.selected_ui.session() {
            session
                .favorites
                .update_visible_favorite_buttons(item, favorite);
        }
    }

    pub(crate) fn set_favorite_with_feedback(
        self: &Rc<Self>,
        item_id: FavoriteTarget,
        favorite: bool,
        button: Option<&gtk::Button>,
    ) {
        if let Some(session) = self.selected_ui.session() {
            session.favorites.set_pending(item_id.clone(), favorite);
        }
        if let Some(button) = button {
            set_favorite_button_active(button, favorite);
        }
        self.update_visible_favorite_buttons(&item_id, favorite);
        self.player_ui
            .set_bottom_player_favorite(&item_id, favorite);
        self.products.source.set_favorite(item_id.clone(), favorite);
        let title = if favorite {
            tr("Added to favorites")
        } else {
            tr("Removed from favorites")
        };
        self.control_feedback.show_control_feedback_toast(title);
    }

    pub(crate) fn apply_favorite_settlement(
        self: &Rc<Self>,
        item_id: FavoriteTarget,
        requested: bool,
        effective: bool,
    ) -> bool {
        if !self.favorite_response_matches_pending(&item_id, requested) {
            return false;
        }
        self.update_visible_favorite_buttons(&item_id, effective);
        self.player_ui
            .set_bottom_player_favorite(&item_id, effective);
        true
    }

    fn favorite_response_matches_pending(&self, item: &FavoriteTarget, requested: bool) -> bool {
        self.selected_ui
            .session()
            .is_some_and(|session| session.favorites.response_matches_pending(item, requested))
    }
}
