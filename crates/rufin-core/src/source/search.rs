use crate::runtime::SelectedLibrary;
use library::{
    LibraryError, LibraryResult, ReadCancellation, SearchMode, SearchRequest, SearchResults,
};

/// How many results of each kind a search shows.
const SEARCH_LIMIT: usize = 60;

/// What `query` finds in the selected source and music folder: titles, from its server when it
/// searches live and `live` asks, else from the Catalog; or lyrics, in every source, the
/// selected one in its music folder.
pub(crate) async fn search(
    owner: &super::SourceOwner,
    selected: Option<&SelectedLibrary>,
    mode: SearchMode,
    query: String,
    live: bool,
    cancel: &ReadCancellation,
) -> LibraryResult<SearchResults> {
    let catalog = &owner.shared.catalog;
    let request = SearchRequest::with_limit(query.as_str(), SEARCH_LIMIT);
    if mode == SearchMode::Lyrics {
        let folder = selected.and_then(|selected| selected.music_folder_key);
        return catalog
            .search_tracks_by_lyrics(&request, folder, cancel)
            .await;
    }
    let selected =
        selected.ok_or_else(|| LibraryError::InvalidRequest("No source selected".into()))?;
    if live
        && selected.source_id.as_str() != sources::LOCAL_LIBRARY_SOURCE_ID
        && let Some(source) = owner.cached_client(&selected.source_id)
    {
        let folder = selected
            .music_folder_key
            .zip(selected.music_folder_object_id.as_deref());
        let live = source.live_search(catalog, &query, SEARCH_LIMIT, folder);
        let result = tokio::select! {
            result = live => result,
            () = cancel.cancelled() => return Err(LibraryError::ReadCancelled),
        };
        if let Ok(rows) = result {
            return Ok(rows);
        }
    }
    catalog
        .search(
            selected.source_key,
            selected.music_folder_key,
            false,
            &request,
            cancel,
        )
        .await
}
