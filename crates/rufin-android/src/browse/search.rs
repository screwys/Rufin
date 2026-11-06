use super::*;
use rufin_core::settings::app::{RecentSearchKind, RecentSearchResult};

/// Search results; each row shows the row fields of its category's list (`fields`, in
/// `CollectionCategory::ALL` order), as GTK's search does.
fn search_rows(
    mut rows: library::SearchResults,
    fields: [Vec<LibraryField>; 3],
) -> Vec<AndroidBrowseRow> {
    let [tracks, albums, artists] = fields;
    let shown = |rows: Vec<Row>, fields: &[LibraryField]| {
        rows.into_iter()
            .enumerate()
            .map(|(index, row)| shown_row(row, fields, index))
            .collect::<Vec<_>>()
    };
    let lyrics = &mut rows.lyrics;
    rows.tracks
        .into_iter()
        .enumerate()
        .map(|(index, track)| AndroidBrowseRow {
            lyrics_snippet: lyrics.remove(&track.track_key),
            ..shown_row(Row::Track(track), &tracks, index)
        })
        .chain(shown(
            rows.albums.into_iter().map(Row::Album).collect(),
            &albums,
        ))
        .chain(shown(
            rows.artists.into_iter().map(Row::Artist).collect(),
            &artists,
        ))
        .collect()
}

/// Search results and the source a title search read them in.
#[derive(uniffi::Record)]
pub struct AndroidSearch {
    pub rows: Vec<AndroidBrowseRow>,
    pub source_id: Option<String>,
}

#[uniffi::export]
impl AndroidLibrary {
    pub async fn search(
        &self,
        text: String,
        mode: library::SearchMode,
    ) -> Result<AndroidSearch, AndroidError> {
        let selected = self.source.selected_library();
        let source_id = selected
            .as_ref()
            .filter(|_| mode == library::SearchMode::Title)
            .map(|selected| selected.source_id.to_string());
        let source = self.source.clone();
        let fields = {
            let settings = self.settings.load();
            CollectionCategory::ALL.map(|category| settings.library_list(category.key()).row_fields)
        };
        let options = OpenOptions {
            filter: text,
            search_mode: mode,
            ..Default::default()
        };
        run_read(&self.runtime, move |cancel| async move {
            let opened = source
                .open(
                    Route::Search,
                    selected,
                    options,
                    library::RouteSeedWindow::top(),
                    true,
                    &cancel,
                )
                .await
                .map_err(error)?;
            let results = opened.view.search_results().cloned().unwrap_or_default();
            Ok(AndroidSearch {
                rows: search_rows(results, fields),
                source_id,
            })
        })
        .await
    }

    /// The remembered search results, each showing whether it is downloaded.
    pub async fn recent_search_results(&self) -> Result<Vec<AndroidBrowseRow>, AndroidError> {
        let recent = self.settings.load().recent_search_results.clone();
        let catalog = self.catalog.clone();
        run_read(&self.runtime, move |cancel| async move {
            catalog.loaded().await;
            let items = recent
                .iter()
                .filter_map(|row| catalog.item(&row.media_uri))
                .collect::<Vec<_>>();
            let downloaded = catalog
                .rows_of(&items, false, &cancel)
                .await
                .map_err(error)?
                .into_iter()
                .filter_map(|row| {
                    let uri = match &row {
                        Row::Track(track) => track.media_uri.clone(),
                        Row::Album(album) => album.media_uri.clone(),
                        Row::Artist(artist) => artist.media_uri.clone(),
                        _ => return None,
                    };
                    row.downloaded().then_some(uri)
                })
                .collect::<std::collections::HashSet<_>>();
            Ok(recent
                .into_iter()
                .map(|row| {
                    let route = match row.kind {
                        RecentSearchKind::Album => Some(Route::AlbumDetail(row.media_uri.clone())),
                        RecentSearchKind::Artist => {
                            Some(Route::ArtistDetail(row.media_uri.clone()))
                        }
                        RecentSearchKind::Track => None,
                    };
                    let shown = downloaded.contains(&row.media_uri);
                    let item = AndroidBrowseItem::Recent { row };
                    AndroidBrowseRow {
                        downloaded: shown,
                        ..match route {
                            Some(route) => AndroidBrowseRow::opening(item, route),
                            None => AndroidBrowseRow::new(item),
                        }
                    }
                })
                .collect())
        })
        .await
    }

    pub fn remember_search_result(&self, row: RecentSearchResult) -> Result<bool, AndroidError> {
        self.settings.remember_search_result(row).map_err(error)
    }

    pub fn remove_search_result(
        &self,
        kind: RecentSearchKind,
        media_uri: String,
    ) -> Result<bool, AndroidError> {
        self.settings
            .remove_search_result(kind, &media_uri)
            .map_err(error)
    }

    pub fn clear_search_results(&self) -> Result<bool, AndroidError> {
        self.settings.clear_search_results().map_err(error)
    }
}
