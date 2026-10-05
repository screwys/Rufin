use super::*;
use rufin_core::settings::app::{RecentSearchKind, RecentSearchResult};

fn search_kind(kind: &str) -> Result<RecentSearchKind, AndroidError> {
    match kind {
        "track" => Ok(RecentSearchKind::Track),
        "album" => Ok(RecentSearchKind::Album),
        "artist" => Ok(RecentSearchKind::Artist),
        _ => Err(error("Choose a search result")),
    }
}

#[uniffi::export]
impl AndroidLibrary {
    pub fn recent_search_results(&self) -> Vec<AndroidBrowseRow> {
        self.settings
            .load()
            .recent_search_results
            .into_iter()
            .map(|result| {
                let (kind, route, playback_context_id) = match result.kind {
                    RecentSearchKind::Track => ("track", None, None),
                    RecentSearchKind::Album => (
                        "album",
                        Some(route_json(Route::AlbumDetail(result.media_uri.clone()))),
                        Some(
                            rufin_core::playback::PlaybackTarget::Album(result.media_uri.clone())
                                .context_id(),
                        ),
                    ),
                    RecentSearchKind::Artist => (
                        "artist",
                        Some(route_json(Route::ArtistDetail(result.media_uri.clone()))),
                        Some(
                            rufin_core::playback::PlaybackTarget::Artist(result.media_uri.clone())
                                .context_id(),
                        ),
                    ),
                };
                let source_id = library::source_entity_parts(&result.media_uri)
                    .map(|(source, _, _)| source.to_string());
                AndroidBrowseRow {
                    kind: kind.into(),
                    playback_context_id,
                    key: result.media_uri.clone(),
                    media_uri: result.media_uri,
                    title: result.title,
                    subtitle: result.subtitle,
                    artist: String::new(),
                    album: String::new(),
                    fields: Vec::new(),
                    favorite: false,
                    duration_millis: 0,
                    detail_route: route,
                    pin: None,
                    section: String::new(),
                    section_id: String::new(),
                    section_kind: String::new(),
                    section_refreshable: false,
                    year: None,
                    track_count: 0,
                    downloaded: false,
                    artwork_identity: result.artwork_binding,
                    source_id,
                    source_name: String::new(),
                    last_played: None,
                    writable: false,
                }
            })
            .collect()
    }

    pub fn remember_search_result(&self, row: AndroidBrowseRow) -> Result<bool, AndroidError> {
        self.settings
            .remember_search_result(RecentSearchResult {
                kind: search_kind(&row.kind)?,
                media_uri: row.media_uri,
                title: row.title,
                subtitle: row.subtitle,
                artwork_binding: row.artwork_identity,
            })
            .map_err(error)
    }

    pub fn remove_search_result(
        &self,
        kind: String,
        media_uri: String,
    ) -> Result<bool, AndroidError> {
        self.settings
            .remove_search_result(search_kind(&kind)?, &media_uri)
            .map_err(error)
    }

    pub fn clear_search_results(&self) -> Result<bool, AndroidError> {
        self.settings.clear_search_results().map_err(error)
    }
}
