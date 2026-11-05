use super::*;
use library::{PlaylistEntryKey, PlaylistKey};

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

fn playlist_selection(
    uris: Vec<String>,
    selection: Option<MediaSelection>,
) -> library::MemberSelection {
    let Some(selection) = selection else {
        return library::MemberSelection::Uris(uris.into());
    };
    let scope = match selection {
        MediaSelection::Album { id } => library::Collection::Album(id),
        MediaSelection::Artist { id, album_artists } => library::Collection::Artist {
            key: id,
            album_artist: album_artists,
        },
        MediaSelection::Playlist { id } => library::Collection::Playlist(id),
        MediaSelection::Genre { id } => library::Collection::Genre(id),
        MediaSelection::SmartPlaylist { id } => library::Collection::Smart {
            key: id,
            now: catalog::now(),
        },
    };
    library::MemberRead::new(scope).selection()
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
            "/api/playlists/remove-unavailable",
            post(remove_unavailable),
        )
        .route("/api/playlists/public", get(public))
        .route("/api/playlists/destinations", get(destinations))
        .route(
            "/api/playlists/file",
            get(file_settings).post(file_action).patch(file_update),
        )
}

/// The playlists tracks can be added to, as the playlist list orders them: `source`'s and
/// Rufin's own.
async fn destinations(
    State(products): State<ProductHandles>,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let source = match parameters.get("source").filter(|source| !source.is_empty()) {
        Some(source) => Some(catalog::scope(&products, source, None).await?.0),
        None => None,
    };
    let playlists = products
        .library
        .playlist_destinations(source, &library::ReadCancellation::new())
        .await
        .map_err(internal)?
        .iter()
        .map(|row| json!({"id":row.playlist_key,"name":row.name}))
        .collect::<Vec<_>>();
    Ok(json_response(
        StatusCode::OK,
        json!({"playlists":playlists}),
    ))
}

async fn remove_unavailable(
    State(products): State<ProductHandles>,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let removed = completion(rufin_core::playlists::remove_unavailable_tracks(
        &products.source,
        key(&parameters, "id")?,
    ))
    .await?;
    Ok(json_response(StatusCode::OK, json!({"removed": removed})))
}

async fn source_file_settings(
    State(products): State<ProductHandles>,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let source = parameters
        .get("source")
        .ok_or_else(|| bad_request("Missing source"))?;
    let enabled = completion(rufin_core::playlist_files::source_auto_save(
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
    completion(rufin_core::playlist_files::set_source_auto_save(
        &products.source,
        input.source,
        input.auto_save,
    ))
    .await?;
    Ok(json_response(StatusCode::OK, json!({"changed":true})))
}

async fn public(
    State(products): State<ProductHandles>,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let public = completion(rufin_core::playlists::playlist_public(
        &products.source,
        key(&parameters, "id")?,
    ))
    .await?;
    Ok(json_response(StatusCode::OK, json!({"public": public})))
}

async fn file_settings(
    State(products): State<ProductHandles>,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let settings = completion(rufin_core::playlist_files::settings(
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
    completion(rufin_core::playlist_files::update(
        &products.source,
        input.id,
        input.auto_refresh,
        input.auto_save,
        input.path_mode,
    ))
    .await?;
    rufin_core::playlists::update_playlist(
        &products.source,
        input.id,
        Some(input.name),
        None,
        None,
    )
    .await
    .map_err(bad_request)?;
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
        "save" => rufin_core::playlist_files::save(owner, input.id, input.force),
        "reload" => rufin_core::playlist_files::reload(owner, input.id, input.force),
        "unlink" => rufin_core::playlist_files::unlink(owner, input.id),
        "delete" => rufin_core::playlist_files::delete_file(owner, input.id),
        "rename" => rufin_core::playlist_files::rename_file(
            owner,
            input.id,
            input.path.ok_or_else(|| bad_request("Missing filename"))?,
        ),
        "link" => {
            let path = input
                .path
                .ok_or_else(|| bad_request("Missing playlist file path"))?;
            if let Some(source) = input.source {
                rufin_core::playlist_files::link_source(owner, input.id, source, path, input.force)
            } else {
                rufin_core::playlist_files::link_local(owner, input.id, path.into(), input.force)
            }
        }
        _ => return Err(bad_request("Unknown playlist file action")),
    };
    match result.recv().await.map_err(internal)? {
        Ok(()) => {}
        Err(error) if rufin_core::playlist_files::is_conflict(&error) => {
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
            completion(rufin_core::playlists::import_source_playlist(
                &products.source,
                source.clone(),
                path,
                !input.copy,
            ))
            .await?
        } else {
            completion(rufin_core::playlists::import_playlist(
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
    let target = rufin_core::runtime::source::PlaylistExport::Playlist(input.id);
    let receiver = rufin_core::playlists::export_playlist(
        &products.source,
        input.source,
        input.path.into(),
        target,
        input.path_mode,
        input.linked,
    );
    completion(receiver).await?;
    Ok(json_response(StatusCode::OK, json!({"saved":true})))
}

pub(super) async fn create(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let cancellation = library::ReadCancellation::new();
    #[derive(Deserialize)]
    struct Input {
        source: Option<sources::SourceId>,
        current: Option<bool>,
        public: Option<bool>,
        name: String,
        #[serde(default)]
        uris: Vec<String>,
        selection: Option<MediaSelection>,
    }
    let input: Input = body(request).await?;
    let source = input.source.clone();
    let selection = playlist_selection(input.uris, input.selection);
    let created = rufin_core::playlists::create_playlist(
        &products.source,
        input.source,
        input.name,
        selection.into(),
        input.public,
        input.current,
    )
    .await
    .map_err(bad_request)?;
    let settings_error = created
        .as_ref()
        .and_then(|created| created.settings_error.as_ref());
    let object_id = created.as_ref().map(|created| created.object_id.as_str());
    // A playlist of a source the Catalog does not hold has no key; None is Rufin's own.
    let owner = match &source {
        Some(source) => products.library.source_key(source).map(Some),
        None => Some(None),
    };
    let id = match (object_id, owner) {
        (Some(object), Some(owner)) => products
            .library
            .catalog_keys(
                library::CatalogKind::Playlist,
                owner,
                &[object.to_owned()],
                &cancellation,
            )
            .await
            .map_err(internal)?
            .first()
            .copied()
            .map(PlaylistKey::from_raw),
        _ => None,
    };
    Ok(json_response(
        StatusCode::OK,
        json!({"id":id,"object_id":object_id,"settings_error":settings_error}),
    ))
}

pub(super) async fn rename(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let products = &products;

    #[derive(Deserialize)]
    struct Input {
        id: PlaylistKey,
        name: Option<String>,
        public: Option<bool>,
    }
    let input: Input = body(request).await?;
    let changed = rufin_core::playlists::update_playlist(
        &products.source,
        input.id,
        input.name,
        input.public,
        None,
    )
    .await
    .map_err(bad_request)?;
    Ok(json_response(StatusCode::OK, json!({"changed":changed})))
}

pub(super) async fn delete(
    State(products): State<ProductHandles>,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let parameters = &parameters;

    let changed = rufin_core::playlists::delete_playlist(&products.source, key(parameters, "id")?)
        .await
        .map_err(bad_request)?;
    Ok(json_response(StatusCode::OK, json!({"changed":changed})))
}

pub(super) async fn add_entries(
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
        #[serde(default)]
        skip_duplicates: bool,
    }
    let input: Input = body(request).await?;
    let selection = playlist_selection(input.uris, input.selection);
    let added = rufin_core::playlists::add_playlist_tracks(
        &products.source,
        input.id,
        selection.into(),
        input.skip_duplicates,
    )
    .await
    .map_err(bad_request)?;
    Ok(json_response(StatusCode::OK, json!({"added":added})))
}

pub(super) async fn remove_entries(
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
    let changed = completion(rufin_core::playlists::remove_playlist_entries(
        &products.source,
        input.id,
        library::MemberSelection::Entries(((input.entries).to_vec()).into()),
    ))
    .await?;
    Ok(json_response(
        StatusCode::OK,
        json!({"changed":!changed.is_empty()}),
    ))
}

pub(super) async fn move_entry(
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
    let changed = completion(rufin_core::playlists::move_playlist_entry(
        &products.source,
        input.id,
        input.entry,
        input.position,
    ))
    .await?;
    Ok(json_response(StatusCode::OK, json!({"changed":changed})))
}
