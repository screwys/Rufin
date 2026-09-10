use super::*;
use library::{PlaylistEntryKey, PlaylistEntrySort, PlaylistKey, PlaylistSort, ReadCancellation};

pub(super) async fn handle(
    request: Request<Incoming>,
    products: &ProductHandles,
    parameters: &HashMap<String, String>,
) -> Result<Response<Body>, Error> {
    let path = request.uri().path().to_owned();
    let cancellation = ReadCancellation::new();
    match (request.method(), path.as_str()) {
        (&Method::GET, "/api/playlists") => {
            let (source, folder) = if parameters.contains_key("source") {
                let (source, folder) = catalog::scope(products, parameters).await?;
                (Some(source), folder)
            } else {
                (None, None)
            };
            let sort = match parameters
                .get("sort")
                .map(String::as_str)
                .unwrap_or("position")
            {
                "position" => PlaylistSort::Position,
                "title" => PlaylistSort::Title,
                "track_count" => PlaylistSort::TrackCount,
                "duration" => PlaylistSort::Duration,
                _ => return Err(bad_request("Unknown playlist sort")),
            };
            let offset = number(parameters, "offset", 0)?;
            let limit = number(parameters, "limit", 100)?.min(128);
            let rows = products
                .library
                .playlist_page(
                    source,
                    folder,
                    sort,
                    boolean(parameters, "descending")?,
                    parameters.get("q").map(String::as_str).unwrap_or(""),
                    offset,
                    limit,
                    &cancellation,
                )
                .await
                .map_err(internal)?;
            Ok(json_response(
                StatusCode::OK,
                json!({"offset":offset,"limit":limit,"playlists":rows.iter().map(|row| json!({"id":row.playlist_key,"object_id":row.object_id,"source":row.source_id,"name":row.name,"writable":row.writable,"track_count":row.track_count,"duration_ms":row.duration_millis})).collect::<Vec<_>>()}),
            ))
        }
        (&Method::GET, "/api/playlists/entries") => {
            let id = key(parameters, "id")?;
            let sort = match parameters
                .get("sort")
                .map(String::as_str)
                .unwrap_or("position")
            {
                "position" => PlaylistEntrySort::Position,
                "title" => PlaylistEntrySort::Title,
                "artist" => PlaylistEntrySort::Artist,
                "album" => PlaylistEntrySort::Album,
                _ => return Err(bad_request("Unknown playlist entry sort")),
            };
            let folder = if parameters.contains_key("folder") {
                catalog::scope(products, parameters).await?.1
            } else {
                None
            };
            let offset = number(parameters, "offset", 0)?;
            let limit = number(parameters, "limit", 100)?.min(256);
            let rows = products
                .library
                .playlist_entries_page(
                    id,
                    folder,
                    sort,
                    boolean(parameters, "descending")?,
                    parameters.get("q").map(String::as_str).unwrap_or(""),
                    offset,
                    limit,
                    &cancellation,
                )
                .await
                .map_err(internal)?;
            Ok(json_response(
                StatusCode::OK,
                json!({"offset":offset,"limit":limit,"entries":rows.iter().map(|row| json!({"id":row.playlist_entry_key,"position":row.position,"uri":row.media_uri,"title":row.title,"artist":row.artist,"album":row.album,"duration_ms":row.duration_millis})).collect::<Vec<_>>()}),
            ))
        }
        (&Method::POST, "/api/playlists") => {
            #[derive(Deserialize)]
            struct Input {
                source: Option<sources::SourceId>,
                name: String,
                #[serde(default)]
                uris: Vec<String>,
            }
            let input: Input = body(request).await?;
            let source = input.source.clone();
            let object_id = completion(crate::playlists::create_playlist(
                &products.source,
                input.source,
                input.name,
                input.uris,
            ))
            .await?;
            let id = if let Some(object) = object_id.as_deref() {
                products
                    .library
                    .playlist_key_by_identity(source.as_ref(), object, &cancellation)
                    .await
                    .map_err(internal)?
            } else {
                None
            };
            Ok(json_response(
                StatusCode::OK,
                json!({"id":id,"object_id":object_id}),
            ))
        }
        (&Method::PATCH, "/api/playlists") => {
            #[derive(Deserialize)]
            struct Input {
                id: PlaylistKey,
                name: String,
            }
            let input: Input = body(request).await?;
            let changed = completion(crate::playlists::rename_playlist(
                &products.source,
                input.id,
                input.name,
            ))
            .await?;
            Ok(json_response(StatusCode::OK, json!({"changed":changed})))
        }
        (&Method::DELETE, "/api/playlists") => {
            let changed = completion(crate::playlists::delete_playlist(
                &products.source,
                key(parameters, "id")?,
            ))
            .await?;
            Ok(json_response(StatusCode::OK, json!({"changed":changed})))
        }
        (&Method::POST, "/api/playlists/entries") => {
            #[derive(Deserialize)]
            struct Input {
                id: PlaylistKey,
                uris: Vec<String>,
                #[serde(default)]
                skip_duplicates: bool,
            }
            let input: Input = body(request).await?;
            let added = completion(crate::playlists::add_playlist_tracks(
                &products.source,
                input.id,
                input.uris,
                input.skip_duplicates,
            ))
            .await?;
            Ok(json_response(StatusCode::OK, json!({"added":added})))
        }
        (&Method::DELETE, "/api/playlists/entries") => {
            #[derive(Deserialize)]
            struct Input {
                id: PlaylistKey,
                entries: Vec<PlaylistEntryKey>,
            }
            let input: Input = body(request).await?;
            let changed = completion(crate::playlists::remove_playlist_entries(
                &products.source,
                input.id,
                input.entries,
            ))
            .await?;
            Ok(json_response(StatusCode::OK, json!({"changed":changed})))
        }
        (&Method::PATCH, "/api/playlists/entries") => {
            #[derive(Deserialize)]
            struct Input {
                id: PlaylistKey,
                entry: PlaylistEntryKey,
                position: usize,
            }
            let input: Input = body(request).await?;
            let changed = completion(crate::playlists::move_playlist_entry(
                &products.source,
                input.id,
                input.entry,
                input.position,
            ))
            .await?;
            Ok(json_response(StatusCode::OK, json!({"changed":changed})))
        }
        _ => Err((StatusCode::NOT_FOUND, "Route not found".into())),
    }
}
