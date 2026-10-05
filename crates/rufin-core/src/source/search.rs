use crate::runtime::SelectedLibrary;
use library::{ReadCancellation, SearchRequest, SearchResults};

pub async fn acquire_search(
    selected: &SelectedLibrary,
    query: String,
    limit: usize,
    cancellation: ReadCancellation,
) -> Result<SearchResults, String> {
    if selected.source_id.as_str() != sources::LOCAL_LIBRARY_SOURCE_ID {
        if let Ok(Ok(rows)) = selected
            .operations
            .search(query.clone(), limit)
            .recv()
            .await
        {
            return Ok(rows);
        }
    }
    let database = selected.database.clone();
    let source = selected.source_key;
    let folder = selected.music_folder_key;
    selected
        .runtime
        .spawn(async move {
            database
                .search(
                    source,
                    folder,
                    false,
                    &SearchRequest::with_limit(query, limit),
                    &cancellation,
                )
                .await
        })
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}
