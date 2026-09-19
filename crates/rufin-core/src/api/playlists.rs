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
    Genre {
        id: library::GenreKey,
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
        MediaSelection::Genre { id } => crate::playback::PlaybackTarget::Genre(id),
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
            "/api/playlists/source-settings",
            get(source_file_settings).patch(source_file_update),
        )
        .route("/api/playlists/import", post(import_file))
        .route("/api/playlists/export", post(export_file))
        .route(
            "/api/playlists",
            get(list).post(create).patch(rename).delete(delete),
        )
        .route(
            "/api/playlists/file",
            get(file_settings).post(file_action).patch(file_update),
        )
        .route(
            "/api/playlists/entries",
            get(entries)
                .post(add_entries)
                .delete(remove_entries)
                .patch(move_entry),
        )
}

async fn source_file_settings(
    State(products): State<ProductHandles>,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let source = parameters
        .get("source")
        .ok_or_else(|| bad_request("Missing source"))?;
    let enabled = completion(crate::playlist_files::source_auto_save(
        &products.source,
        sources::SourceId::new(source.clone()),
    ))
    .await?;
    Ok(json_response(StatusCode::OK, json!({"auto_save":enabled})))
}

async fn source_file_update(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    #[derive(Deserialize)]
    struct Input {
        source: sources::SourceId,
        auto_save: bool,
    }
    let input: Input = body(request).await?;
    completion(crate::playlist_files::set_source_auto_save(
        &products.source,
        input.source,
        input.auto_save,
    ))
    .await?;
    Ok(json_response(StatusCode::OK, json!({"changed":true})))
}

async fn file_settings(
    State(products): State<ProductHandles>,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let settings = completion(crate::playlist_files::settings(
        &products.source,
        key(&parameters, "id")?,
    ))
    .await?;
    Ok(json_response(
        StatusCode::OK,
        json!({"link":settings.link,"display_path":settings.display_path,"source_auto_save":settings.source_auto_save,"can_link":settings.can_link}),
    ))
}

async fn file_update(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    #[derive(Deserialize)]
    struct Input {
        id: PlaylistKey,
        name: String,
        auto_refresh: bool,
        auto_save: Option<bool>,
        path_mode: library::PlaylistPathMode,
    }
    let input: Input = body(request).await?;
    completion(crate::playlist_files::update(
        &products.source,
        input.id,
        input.name,
        input.auto_refresh,
        input.auto_save,
        input.path_mode,
    ))
    .await?;
    Ok(json_response(StatusCode::OK, json!({"changed":true})))
}

async fn file_action(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    #[derive(Deserialize)]
    struct Input {
        id: PlaylistKey,
        action: String,
        path: Option<String>,
        source: Option<sources::SourceId>,
        #[serde(default)]
        force: bool,
    }
    let input: Input = body(request).await?;
    let owner = &products.source;
    let result = match input.action.as_str() {
        "save" => crate::playlist_files::save(owner, input.id, input.force),
        "reload" => crate::playlist_files::reload(owner, input.id, input.force),
        "unlink" => crate::playlist_files::unlink(owner, input.id),
        "delete" => crate::playlist_files::delete_file(owner, input.id),
        "rename" => crate::playlist_files::rename_file(
            owner,
            input.id,
            input.path.ok_or_else(|| bad_request("Missing filename"))?,
        ),
        "link" => {
            let path = input
                .path
                .ok_or_else(|| bad_request("Missing playlist file path"))?;
            if let Some(source) = input.source {
                crate::playlist_files::link_source(owner, input.id, source, path, input.force)
            } else {
                crate::playlist_files::link_local(owner, input.id, path.into(), input.force)
            }
        }
        _ => return Err(bad_request("Unknown playlist file action")),
    };
    match result.recv().await.map_err(internal)? {
        Ok(()) => {}
        Err(error) if crate::playlist_files::is_conflict(&error) => {
            return Ok(json_response(
                StatusCode::CONFLICT,
                json!({"error":error,"conflict":true}),
            ));
        }
        Err(error) => return Err(bad_request(error)),
    }
    Ok(json_response(StatusCode::OK, json!({"changed":true})))
}

async fn import_file(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    #[derive(Deserialize)]
    struct Input {
        paths: Vec<String>,
        source: Option<sources::SourceId>,
        #[serde(default)]
        copy: bool,
    }
    let input: Input = body(request).await?;
    let mut reports = Vec::new();
    for path in input.paths {
        let report = if let Some(source) = &input.source {
            completion(crate::playlists::import_source_playlist(
                &products.source,
                source.clone(),
                path,
                !input.copy,
            ))
            .await?
        } else {
            completion(crate::playlists::import_playlist(
                &products.source,
                path.into(),
                None,
                !input.copy,
            ))
            .await?
        };
        reports.push(json!({"id":report.playlist,"skipped":report.skipped}));
    }
    Ok(json_response(StatusCode::OK, json!({"playlists":reports})))
}

async fn export_file(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    #[derive(Deserialize)]
    struct Input {
        id: PlaylistKey,
        path: String,
        source: Option<sources::SourceId>,
        #[serde(default)]
        path_mode: library::PlaylistPathMode,
        #[serde(default)]
        linked: bool,
    }
    let input: Input = body(request).await?;
    let target = crate::runtime::source::PlaylistExport::Playlist(input.id);
    let receiver = crate::playlists::export_playlist(
        &products.source,
        input.source,
        input.path.into(),
        target,
        None,
        input.path_mode,
        input.linked,
    );
    completion(receiver).await?;
    Ok(json_response(StatusCode::OK, json!({"saved":true})))
}

async fn list(
    State(products): State<ProductHandles>,
    headers: hyper::HeaderMap,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    web::page(
        &headers,
        &parameters,
        list_data(&products, &parameters).await?,
    )
}

pub(super) async fn list_data(
    products: &ProductHandles,
    parameters: &HashMap<String, String>,
) -> Result<Value, Error> {
    let prefer_server_covers = products
        .source
        .shared
        .settings
        .load()
        .ui
        .prefer_server_playlist_covers;
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
    Ok(
        json!({"offset":offset,"limit":limit,"playlists":rows.iter().map(|row| json!({"id":row.playlist_key,"object_id":row.object_id,"source":row.source_id,"name":row.name,"writable":row.writable,"track_count":row.track_count,"duration_ms":row.duration_millis,"artwork_count":crate::playlists::playlist_artwork_bindings(row, prefer_server_covers).len()})).collect::<Vec<_>>()}),
    )
}

async fn entries(
    State(products): State<ProductHandles>,
    headers: hyper::HeaderMap,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    web::page(
        &headers,
        &parameters,
        entries_data(&products, &parameters).await?,
    )
}

pub(super) async fn entries_data(
    products: &ProductHandles,
    parameters: &HashMap<String, String>,
) -> Result<Value, Error> {
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
    Ok(
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
        selection_source: Option<String>,
        current: Option<bool>,
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
        input
            .selection_source
            .or_else(|| input.source.as_ref().map(|id| id.as_str().to_owned())),
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
    let mut settings_error = None;
    let id = if let Some(object) = object_id.as_deref() {
        let settings = products.source.shared.settings.clone();
        let pin = crate::settings::SidebarPin::Playlist {
            source_id: source.clone(),
            playlist_id: object.to_owned(),
        };
        settings_error = tokio::task::spawn_blocking(move || {
            settings.update(|stored| {
                stored.ui.sidebar.set_pinned(pin, true);
                if let Some(current) = input.current {
                    stored.ui.new_playlist_current = current;
                }
                Ok(())
            })
        })
        .await
        .map_err(|error| error.to_string())
        .and_then(|result| result)
        .err();
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
        json!({"id":id,"object_id":object_id,"settings_error":settings_error}),
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
