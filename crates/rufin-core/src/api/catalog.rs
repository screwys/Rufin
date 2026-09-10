use super::*;
use library::{AlbumSort, FolderKey, ReadCancellation, SourceKey};

pub(super) async fn scope(
    products: &ProductHandles,
    parameters: &HashMap<String, String>,
) -> Result<(SourceKey, Option<FolderKey>), Error> {
    let source = sources::SourceId::new(required(parameters, "source")?);
    let source = products
        .library
        .source_identity_key(&source)
        .await
        .map_err(internal)?
        .ok_or_else(|| bad_request("Source not found"))?;
    let folder = if let Some(folder) = parameters.get("folder") {
        Some(
            products
                .library
                .folder_key_by_object(source, folder, &ReadCancellation::new())
                .await
                .map_err(internal)?
                .ok_or_else(|| bad_request("Folder not found"))?,
        )
    } else {
        None
    };
    Ok((source, folder))
}

pub(super) async fn handle(
    request: Request<Incoming>,
    products: &ProductHandles,
    parameters: &HashMap<String, String>,
) -> Result<Response<Body>, Error> {
    let offset = number(parameters, "offset", 0)?;
    let limit = number(parameters, "limit", 100)?.min(128);
    let filter = parameters.get("q").map(String::as_str).unwrap_or("");
    let descending = boolean(parameters, "descending")?;
    let favorites = boolean(parameters, "favorites")?;
    let cancellation = ReadCancellation::new();
    match request.uri().path() {
        "/api/albums" => {
            let (source, folder) = scope(products, parameters).await?;
            let sort = match parameters
                .get("sort")
                .map(String::as_str)
                .unwrap_or("title")
            {
                "title" => AlbumSort::Title,
                "album_artist" => AlbumSort::AlbumArtist,
                "year" => AlbumSort::Year,
                "release_date" => AlbumSort::ReleaseDate,
                "date_added" => AlbumSort::DateAdded,
                "last_played" => AlbumSort::LastPlayed,
                "play_count" => AlbumSort::PlayCount,
                "rating" => AlbumSort::Rating,
                "track_count" => AlbumSort::TrackCount,
                "duration" => AlbumSort::Duration,
                "favorite" => AlbumSort::Favorite,
                _ => return Err(bad_request("Unknown album sort")),
            };
            let rows = products
                .library
                .album_page(
                    source,
                    folder,
                    favorites,
                    filter,
                    sort,
                    descending,
                    offset,
                    limit,
                    &cancellation,
                )
                .await
                .map_err(internal)?;
            Ok(json_response(
                StatusCode::OK,
                json!({"offset":offset,"limit":limit,"albums":rows.iter().map(|row| json!({
                "id":row.album_key,"uri":row.media_uri,"title":row.title,"artist":row.display_artist,
                "year":row.year,"track_count":row.track_count,"duration_ms":row.duration_millis,"favorite":row.favorite,"rating":row.rating
            })).collect::<Vec<_>>()}),
            ))
        }
        "/api/albums/tracks" => {
            let album = key(parameters, "id")?;
            let folder = if parameters.contains_key("folder") {
                scope(products, parameters).await?.1
            } else {
                None
            };
            let sort = track_sort(
                parameters
                    .get("sort")
                    .map(String::as_str)
                    .unwrap_or("track_number"),
            )?;
            let rows = products
                .library
                .album_tracks_page(
                    album,
                    folder,
                    filter,
                    sort,
                    descending,
                    favorites,
                    offset,
                    limit,
                    &cancellation,
                )
                .await
                .map_err(bad_request)?;
            Ok(json_response(
                StatusCode::OK,
                json!({"offset":offset,"limit":limit,"tracks":rows.iter().map(track_row_json).collect::<Vec<_>>()}),
            ))
        }
        "/api/folders" => {
            let (source, folder) = scope(products, parameters).await?;
            let parent = if let Some(parent) = parameters.get("parent") {
                Some(
                    products
                        .library
                        .folder_key_by_object(source, parent, &cancellation)
                        .await
                        .map_err(internal)?
                        .ok_or_else(|| bad_request("Folder not found"))?,
                )
            } else {
                folder
            };
            let rows = products
                .library
                .folder_page(source, parent, offset, limit, &cancellation)
                .await
                .map_err(internal)?;
            Ok(json_response(
                StatusCode::OK,
                json!({"offset":offset,"limit":limit,"folders":rows.iter().map(|row| json!({"id":row.object_id,"name":row.name,"track_count":row.track_count})).collect::<Vec<_>>()}),
            ))
        }
        "/api/folders/live" => {
            let source_id = sources::SourceId::new(required(parameters, "source")?);
            let owner = products.source.clone();
            let source = tokio::task::spawn_blocking(move || owner.client(&source_id))
                .await
                .map_err(internal)?
                .map_err(bad_request)?;
            let page = source
                .browse_folder(
                    parameters.get("parent").map(String::as_str),
                    parameters.get("folder").map(String::as_str),
                )
                .await
                .map_err(bad_request)?;
            Ok(json_response(
                StatusCode::OK,
                json!({"folders":page.folders.iter().map(|folder| json!({"id":folder.object_id,"name":folder.name})).collect::<Vec<_>>(),"track_ids":page.tracks}),
            ))
        }
        _ => Err((StatusCode::NOT_FOUND, "Route not found".into())),
    }
}
