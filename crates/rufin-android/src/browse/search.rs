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
            .map(|row| AndroidBrowseRow::new(AndroidBrowseItem::Recent { row }))
            .collect()
    }

    pub fn remember_search_result(&self, row: RecentSearchResult) -> Result<bool, AndroidError> {
        self.settings.remember_search_result(row).map_err(error)
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
