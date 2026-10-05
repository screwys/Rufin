use library::FavoriteTarget;
impl crate::shell::Shell {
    pub(crate) fn half_stars_enabled(&self, media_uri: &str, source_id: Option<&str>) -> bool {
        self.source
            .configured
            .borrow()
            .half_stars_enabled(media_uri, source_id)
    }

    pub(crate) fn set_rating(&self, item: FavoriteTarget, rating: Option<u8>) {
        self.products.source.set_rating(item, rating);
    }

    pub(crate) fn set_current_track_rating(&self, rating: Option<u8>) {
        let track = self
            .player_ui
            .selected_playback()
            .as_deref()
            .and_then(|player| player.transport.current.as_ref())
            .map(|entry| entry.media_uri.clone());
        if let Some(media_uri) = track {
            self.set_rating(FavoriteTarget::Track(media_uri), rating);
        }
    }
}
