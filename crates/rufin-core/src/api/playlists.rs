use super::*;
use library::{PlaylistEntryKey, PlaylistEntrySort, PlaylistKey, PlaylistSort};

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum MediaSelection {
    Album {
        id: library::AlbumKey,
    },
    Artist {
        id: library::ArtistKey,
        #[serde(default)]
        album_artists: bool,
    },
    Playlist {
        id: PlaylistKey,
    },
    SmartPlaylist {
        id: library::SmartPlaylistKey,
    },
}

async fn selected_uris(
    products: &ProductHandles,
    uris: Vec<String>,
    selection: Option<MediaSelection>,
    source: Option<String>,
    folder: Option<String>,
) -> Result<Vec<String>, Error> {
    let Some(selection) = selection else {
        return Ok(uris);
    };
    let target = match selection {
        MediaSelection::Album { id } => crate::playback::PlaybackTarget::AlbumKey(id),
        MediaSelection::Artist { id, album_artists } => {
            crate::playback::PlaybackTarget::ArtistKey(id, album_artists)
        }
        MediaSelection::Playlist { id } => crate::playback::PlaybackTarget::Playlist(id),
        MediaSelection::SmartPlaylist { id } => crate::playback::PlaybackTarget::SmartPlaylist(id),
    };
    let (source, folder) = if let Some(source) = source {
        let mut parameters = HashMap::from([("source".into(), source)]);
        if let Some(folder) = folder {
            parameters.insert("folder".into(), folder);
        }
        let (source, folder) = catalog::scope(products, &parameters).await?;
        (Some(source), folder)
    } else {
        products
            .source
            .selected_library()
            .map_or((None, None), |selected| {
                (Some(selected.source_key), selected.music_folder_key)
            })
    };
    target
        .resolve_media_uris(&products.library, source, folder)
        .await
        .map_err(bad_request)
}

pub(super) fn entry_sort(value: &str) -> Result<PlaylistEntrySort, Error> {
    match value {
        "position" => Ok(PlaylistEntrySort::Position),
        "title" => Ok(PlaylistEntrySort::Title),
        "artist" => Ok(PlaylistEntrySort::Artist),
        "album" => Ok(PlaylistEntrySort::Album),
        _ => Err(bad_request("Unknown playlist entry sort")),
    }
}

pub(super) fn routes() -> Router<ProductHandles> {
    Router::new()
        .route(
            "/api/playlists",
            get(list).post(create).patch(rename).delete(delete),
        )
        .route(
            "/api/playlists/entries",
            get(entries)
                .post(add_entries)
                .delete(remove_entries)
                .patch(move_entry),
        )
}

async fn list(
    State(products): State<ProductHandles>,
    headers: hyper::HeaderMap,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let parameters = &parameters;
    let cancellation = library::ReadCancellation::new();
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
    web::page(
        &headers,
        &parameters,
        json!({"offset":offset,"limit":limit,"playlists":rows.iter().map(|row| json!({"id":row.playlist_key,"object_id":row.object_id,"source":row.source_id,"name":row.name,"writable":row.writable,"track_count":row.track_count,"duration_ms":row.duration_millis})).collect::<Vec<_>>()}),
    )
}

async fn entries(
    State(products): State<ProductHandles>,
    headers: hyper::HeaderMap,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let parameters = &parameters;
    let cancellation = library::ReadCancellation::new();
    let id = key(parameters, "id")?;
    let sort = entry_sort(
        parameters
            .get("sort")
            .map(String::as_str)
            .unwrap_or("position"),
    )?;
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
    web::page(
        &headers,
        &parameters,
        json!({"offset":offset,"limit":limit,"entries":rows.iter().map(|row| json!({"id":row.playlist_entry_key,"position":row.position,"uri":row.media_uri,"title":row.title,"artist":row.artist,"album":row.album,"favorite":row.favorite,"duration_ms":row.duration_millis})).collect::<Vec<_>>()}),
    )
}

async fn create(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let cancellation = library::ReadCancellation::new();
    #[derive(Deserialize)]
    struct Input {
        source: Option<sources::SourceId>,
        name: String,
        #[serde(default)]
        uris: Vec<String>,
        selection: Option<MediaSelection>,
        folder: Option<String>,
    }
    let input: Input = body(request).await?;
    let source = input.source.clone();
    let uris = selected_uris(
        products,
        input.uris,
        input.selection,
        input.source.as_ref().map(|id| id.as_str().to_owned()),
        input.folder,
    )
    .await?;
    let object_id = completion(crate::playlists::create_playlist(
        &products.source,
        input.source,
        input.name,
        uris,
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

async fn rename(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let products = &products;

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

async fn delete(
    State(products): State<ProductHandles>,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let parameters = &parameters;

    let changed = completion(crate::playlists::delete_playlist(
        &products.source,
        key(parameters, "id")?,
    ))
    .await?;
    Ok(json_response(StatusCode::OK, json!({"changed":changed})))
}

async fn add_entries(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let products = &products;

    #[derive(Deserialize)]
    struct Input {
        id: PlaylistKey,
        #[serde(default)]
        uris: Vec<String>,
        selection: Option<MediaSelection>,
        source: Option<String>,
        folder: Option<String>,
        #[serde(default)]
        skip_duplicates: bool,
    }
    let input: Input = body(request).await?;
    let uris = selected_uris(
        products,
        input.uris,
        input.selection,
        input.source,
        input.folder,
    )
    .await?;
    let added = completion(crate::playlists::add_playlist_tracks(
        &products.source,
        input.id,
        uris,
        input.skip_duplicates,
    ))
    .await?;
    Ok(json_response(StatusCode::OK, json!({"added":added})))
}

async fn remove_entries(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let products = &products;

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

async fn move_entry(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let products = &products;

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
