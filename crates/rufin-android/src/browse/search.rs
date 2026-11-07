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

impl AndroidLibrary {
    /// The results the Search page `read` reads, with what commits while it reads.
    async fn found<F>(
        self: Arc<Self>,
        read: impl FnOnce(library::ReadCancellation) -> F + Send + 'static,
    ) -> Result<AndroidSearch, AndroidError>
    where
        F: std::future::Future<Output = library::LibraryResult<rufin_core::view::Opened>>
            + Send
            + 'static,
    {
        let reading = AndroidReading::new(self.source.reading());
        let fields = {
            let settings = self.settings.load();
            CollectionCategory::ALL.map(|category| settings.library_list(category.key()).row_fields)
        };
        let library = Arc::clone(&self);
        run_read(&self.runtime, move |cancel| async move {
            let opened = read(cancel).await.map_err(error)?;
            let results = opened.view.search_results().cloned().unwrap_or_default();
            Ok(AndroidSearch {
                rows: search_rows(results, fields),
                view: library.view_of(opened.view),
                reading,
            })
        })
        .await
    }
}

/// A search's results, the Search page that found them and what committed while it searched.
#[derive(uniffi::Record)]
pub struct AndroidSearch {
    pub rows: Vec<AndroidBrowseRow>,
    pub view: Arc<AndroidView>,
    pub reading: Arc<AndroidReading>,
}

#[uniffi::export]
impl AndroidLibrary {
    /// Searches `text` in the library selected now; the page it opens says when a committed
    /// change, also one that committed while it searched, means it searches again.
    pub async fn search(
        self: Arc<Self>,
        text: String,
        mode: library::SearchMode,
    ) -> Result<AndroidSearch, AndroidError> {
        let options = OpenOptions {
            filter: text,
            search_mode: mode,
            ..Default::default()
        };
        let selected = self.source.selected_library();
        let source = self.source.clone();
        self.found(move |cancel| async move {
            source
                .open(
                    Route::Search,
                    selected,
                    options,
                    library::RouteSeedWindow::top(),
                    true,
                    &cancel,
                )
                .await
        })
        .await
    }

    /// Reads the Search page `view` again, as its refresh asked.
    pub async fn search_again(
        self: Arc<Self>,
        view: Arc<AndroidView>,
    ) -> Result<AndroidSearch, AndroidError> {
        let view = Arc::clone(&view.view);
        self.found(move |cancel| async move {
            view.reread(
                view.options().clone(),
                library::RouteSeedWindow::top(),
                &cancel,
            )
            .await
        })
        .await
    }

    /// The remembered search results the library still holds, as its rows show them in search.
    pub async fn recent_search_results(&self) -> Result<Vec<AndroidBrowseRow>, AndroidError> {
        let settings = self.settings.load();
        let recent = settings.recent_search_results.clone();
        let fields = CollectionCategory::ALL
            .map(|category| settings.library_list(category.key()).row_fields);
        let catalog = self.catalog.clone();
        run_read(&self.runtime, move |cancel| async move {
            catalog.loaded().await;
            let uris = recent
                .iter()
                .map(|row| row.media_uri.clone())
                .collect::<Vec<_>>();
            let tracks = catalog
                .tracks_by_uris(&uris, &cancel)
                .await
                .map_err(error)?;
            let collections = catalog.items(&uris.iter().map(String::as_str).collect::<Vec<_>>());
            let items = uris
                .iter()
                .zip(collections)
                .filter_map(|(uri, item)| {
                    item.or_else(|| {
                        let track = tracks.iter().find(|track| track.media_uri == *uri)?;
                        Some(library::Item::Track {
                            key: track.track_key,
                            entry: None,
                        })
                    })
                })
                .collect::<Vec<_>>();
            let rows = catalog
                .rows_of(&items, false, &cancel)
                .await
                .map_err(error)?;
            Ok(rows
                .into_iter()
                .enumerate()
                .map(|(index, row)| {
                    let category = match row {
                        Row::Track(_) => 0,
                        Row::Album(_) => 1,
                        _ => 2,
                    };
                    shown_row(row, &fields[category], index)
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
