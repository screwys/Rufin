use library::FavoriteTarget;
impl crate::shell::Shell {
    pub(crate) fn half_stars_enabled(&self, media_uri: &str, source_id: Option<&str>) -> bool {
        let configured = self.source.configured.borrow();
        let uri_source = library::source_entity_parts(media_uri).map(|(source, _, _)| source);
        let source_id = source_id
            .or_else(|| uri_source.as_ref().map(sources::SourceId::as_str))
            .or_else(|| {
                configured
                    .selected_source_id
                    .as_ref()
                    .map(sources::SourceId::as_str)
            });
        configured
            .sources
            .iter()
            .find(|source| Some(source.id.as_str()) == source_id)
            .is_some_and(|source| source.half_stars_enabled)
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
