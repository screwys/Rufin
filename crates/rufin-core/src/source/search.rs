use crate::runtime::SelectedLibrary;
use library::{ReadCancellation, SearchRequest, SearchResults};

pub async fn acquire_search(
    owner: &super::SourceOwner,
    database: &library::Database,
    selected: &SelectedLibrary,
    query: String,
    limit: usize,
    cancellation: ReadCancellation,
) -> Result<SearchResults, String> {
    if selected.source_id.as_str() != sources::LOCAL_LIBRARY_SOURCE_ID
        && let Some(source) = owner.cached_client(&selected.source_id)
    {
        let live = source.live_search(database, &query, limit);
        let result = tokio::select! {
            result = live => result,
            () = cancellation.cancelled() => return Err(library::LibraryError::ReadCancelled.to_string()),
        };
        if let Ok(rows) = result {
            return Ok(rows);
        }
    }
    database
        .search(
            selected.source_key,
            selected.music_folder_key,
            false,
            &SearchRequest::with_limit(query, limit),
            &cancellation,
        )
        .await
        .map_err(|error| error.to_string())
}
