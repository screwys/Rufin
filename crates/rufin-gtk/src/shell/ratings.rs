use library::FavoriteTarget;
impl crate::shell::shell::Shell {
    pub(crate) fn half_stars_enabled(&self, media_uri: &str, source_id: Option<&str>) -> bool {
        self.source
            .configured
            .borrow()
            .half_stars_enabled(media_uri, source_id)
    }

    /// Whether downloads are offered for a track of this source; local files are already here.
    pub(crate) fn source_download_available(&self, source_id: &sources::SourceId) -> bool {
        self.source
            .configured
            .borrow()
            .sources
            .iter()
            .any(|source| &source.id == source_id && source.kind != "local")
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
            .map(|entry| entry.row.track.media_uri.clone());
        if let Some(media_uri) = track {
            self.set_rating(FavoriteTarget::Track(media_uri), rating);
        }
    }
}
