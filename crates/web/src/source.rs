use super::*;
use rufin_core::source::authorization::SourceAuthorizationMethod;
use sources::SourceSetup;

pub(super) fn routes() -> Router<ProductHandles> {
    Router::new()
        .route(
            "/api/sources",
            get(list).post(add).patch(update).delete(forget),
        )
        .route("/api/downloads/quality", post(download_quality))
        .route("/api/sources/progress", get(progress))
        .route("/api/sources/events", get(events))
        .route("/api/sources/local", post(add))
        .route("/api/sources/select", post(select))
        .route("/api/sources/library", post(select_library))
        .route("/api/sources/edit", get(editable))
        .route("/api/sources/refresh", post(select))
        .route(
            "/api/downloads/cache-settings",
            get(cache_settings).post(cache_settings),
        )
        .route(
            "/api/downloads/clear-artwork-cache",
            post(clear_artwork_cache),
        )
        .route(
            "/api/downloads/clear-lyrics-cache",
            post(clear_lyrics_cache),
        )
        .route("/api/sources/login/plex", post(plex_login))
        .route("/api/sources/login/emby", post(emby_login))
        .route("/api/sources/login/jellyfin", post(jellyfin_login))
        .route("/api/sources/login/nextcloud", post(nextcloud_login))
        .route("/api/sources/smb/shares", post(smb_shares))
        .route("/api/sources/plex/profiles", post(plex_profiles))
        .route("/api/sources/plex/logins", get(plex_logins))
        .route("/api/sources/plex/servers", post(plex_servers))
        .route(
            "/api/integrations/files",
            get(file_integrations)
                .post(add_file_integration)
                .patch(update_file_integration),
        )
        .route("/api/integrations/files/edit", get(edit_file_integration))
}

async fn download_quality(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    #[derive(Deserialize)]
    struct Input {
        source: sources::SourceId,
        quality: sources::DownloadQuality,
    }
    let input: Input = body(request).await?;
    products
        .settings
        .set_download_quality(input.source, input.quality)
        .map_err(bad_request)?;
    Ok(accepted())
}

async fn file_integrations(
    State(products): State<ProductHandles>,
) -> axum::Json<Vec<rufin_core::source::FileIntegration>> {
    axum::Json(products.source.file_integrations())
}

async fn add_file_integration(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let id = completion(
        products
            .source
            .configure_file_integration(body(request).await?),
    )
    .await?;
    Ok(json_response(StatusCode::OK, json!({"id": id})))
}

async fn update_file_integration(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    completion(
        products
            .source
            .update_file_integration(body(request).await?),
    )
    .await?;
    Ok(accepted())
}

async fn edit_file_integration(
    State(products): State<ProductHandles>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let id = query
        .get("id")
        .ok_or_else(|| bad_request("id is required"))?;
    let preset = products
        .source
        .editable(sources::SourceId::new(id))
        .map_err(bad_request)?
        .ok_or_else(|| bad_request("Connection not found"))?;
    Ok(json_response(StatusCode::OK, json!(preset)))
}

async fn list(State(products): State<ProductHandles>) -> Result<Response<Body>, Error> {
    let products = &products;
    let source = &products.source;
    let configured = source.list_sources();
    let selected = source.selected_library();
    Ok(json_response(
        StatusCode::OK,
        json!({
            "selected": configured.selected_source_id,
            "library": selected.as_ref().and_then(|source|source.music_folder_object_id.clone()),
            "new_playlist_current": products.settings.load().application.new_playlist_current,
            "libraries": selected.as_ref().map(|source|source.music_folders.iter().map(|folder|json!({"id":folder.object_id,"name":folder.name})).collect::<Vec<_>>()).unwrap_or_default(),
            "sources": configured.sources.iter().map(|s| json!({"id":s.id,"name":s.name,"kind":s.kind,"supports_playlist_public":s.supports_playlist_public,"quality":products.settings.download_settings(s.id.clone()).quality})).collect::<Vec<_>>()
        }),
    ))
}

pub(super) async fn select_library(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    #[derive(Deserialize)]
    struct Input {
        source: sources::SourceId,
        library: Option<String>,
    }
    let input: Input = body(request).await?;
    let selected = match products
        .source
        .selected_library()
        .filter(|selected| selected.source_id == input.source)
    {
        Some(selected) => selected.source_id,
        None => products
            .source
            .select_source(input.source)
            .await
            .map_err(bad_request)?,
    };
    completion(products.source.set_music_folder(selected, input.library)).await?;
    Ok(accepted())
}

async fn editable(
    State(products): State<ProductHandles>,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let id = sources::SourceId::new(required(&parameters, "id")?);
    let configuration = products
        .source
        .configuration(&id)
        .ok_or_else(|| bad_request("Source not found"))?;
    if let sources::EditableSource::Local {
        roots,
        excluded_folders,
        ..
    } = configuration.editable().map_err(bad_request)?
    {
        return Ok(json_response(
            StatusCode::OK,
            json!({"id":id,"kind":"local","roots":roots,"excluded_folders":excluded_folders}),
        ));
    }
    let preset = products
        .source
        .editable(id.clone())
        .map_err(bad_request)?
        .ok_or_else(|| bad_request("Source not found"))?;
    Ok(json_response(
        StatusCode::OK,
        json!({"id":id,"kind":preset.source.kind,"preset":preset}),
    ))
}

async fn progress(State(products): State<ProductHandles>) -> Result<Response<Body>, Error> {
    let products = &products;
    let source = &products.source;
    Ok(json_response(
        StatusCode::OK,
        serde_json::to_value(&*source.operation().borrow()).map_err(internal)?,
    ))
}

async fn events(State(products): State<ProductHandles>) -> Result<Response<Body>, Error> {
    let products = &products;
    let source = &products.source;
    let stream = futures_util::stream::unfold(
        (source.operation(), true),
        |(mut operation, first)| async move {
            if !first && operation.changed().await.is_err() {
                return None;
            }
            let value = serde_json::to_value(&*operation.borrow_and_update())
                .expect("source operation serialization");
            Some((
                Ok::<_, Infallible>(Frame::data(Bytes::from(format!(
                    "event: source\ndata: {value}\n\n"
                )))),
                (operation, false),
            ))
        },
    );
    Ok(event_response(Body::new(StreamBody::new(stream))))
}

async fn add(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let path = request.uri().path().to_owned();
    let source = &products.source;
    let setup = if path.ends_with("/local") {
        #[derive(Deserialize)]
        struct Input {
            paths: Vec<PathBuf>,
        }
        SourceSetup::Local {
            roots: body::<Input>(request).await?.paths,
        }
    } else {
        body::<SourceSetup>(request).await?
    };
    let selected = completion(source.configure_source(setup)).await?;
    Ok(json_response(
        StatusCode::OK,
        json!({"id":selected.source_id}),
    ))
}

async fn update(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let source = &products.source;
    completion(source.update_source(body(request).await?)).await?;
    Ok(json_response(StatusCode::OK, json!({"updated":true})))
}

pub(super) async fn select(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let path = request.uri().path().to_owned();
    let source = &products.source;
    let input: Identifier = body(request).await?;
    if input.id.is_empty() {
        return Err(bad_request("id is required"));
    }
    let id = sources::SourceId::new(input.id);
    if path.ends_with("/refresh") {
        source
            .refresh_source(id, false)
            .await
            .map_err(bad_request)?;
        Ok(json_response(StatusCode::OK, json!({"refreshed":true})))
    } else {
        let selected = source.select_source(id).await.map_err(bad_request)?;
        Ok(json_response(StatusCode::OK, json!({"id":selected})))
    }
}

async fn cache_settings(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    #[derive(Deserialize)]
    struct Input {
        artwork: bool,
        lyrics: bool,
    }
    if request.method() == hyper::Method::POST {
        let input: Input = body(request).await?;
        products
            .settings
            .update_preferences(|settings| {
                settings.application.download_artwork_automatically = input.artwork;
                settings.application.download_lyrics_automatically = input.lyrics;
                Ok(())
            })
            .map_err(bad_request)?;
    }
    let settings = products.settings.load();
    Ok(json_response(
        StatusCode::OK,
        json!({
            "artwork": settings.application.download_artwork_automatically,
            "lyrics": settings.application.download_lyrics_automatically,
        }),
    ))
}

async fn clear_artwork_cache(
    State(products): State<ProductHandles>,
) -> Result<Response<Body>, Error> {
    products
        .clear_cached_data(rufin_core::runtime::CacheKind::Artwork)
        .await
        .map_err(bad_request)?;
    Ok(json_response(StatusCode::OK, json!({"cleared": true})))
}

async fn clear_lyrics_cache(
    State(products): State<ProductHandles>,
) -> Result<Response<Body>, Error> {
    products
        .clear_cached_data(rufin_core::runtime::CacheKind::Lyrics)
        .await
        .map_err(bad_request)?;
    Ok(json_response(StatusCode::OK, json!({"cleared": true})))
}

async fn forget(
    State(products): State<ProductHandles>,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let parameters = &parameters;
    let source = &products.source;
    source
        .forget_source(sources::SourceId::new(required(parameters, "id")?))
        .await
        .map_err(bad_request)?;
    Ok(json_response(StatusCode::OK, json!({"removed":true})))
}

async fn plex_login(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let source = &products.source;
    Ok(login_response(source.login(
        SourceAuthorizationMethod::Plex {
            method: body(request).await?,
        },
    )))
}

async fn emby_login(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let source = &products.source;
    Ok(login_response(source.login(
        SourceAuthorizationMethod::Emby {
            method: body(request).await?,
        },
    )))
}

async fn jellyfin_login(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let source = &products.source;
    #[derive(Deserialize)]
    struct Input {
        server_url: String,
        #[serde(default)]
        trust_invalid_cert: bool,
    }
    let input: Input = body(request).await?;
    Ok(login_response(source.login(
        SourceAuthorizationMethod::Jellyfin {
            url: input.server_url,
            trust_invalid_cert: input.trust_invalid_cert,
        },
    )))
}

async fn nextcloud_login(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let source = &products.source;
    let input: Files = body(request).await?;
    Ok(login_response(source.login(
        SourceAuthorizationMethod::Nextcloud {
            settings: input.settings,
            credentials: input.credentials,
        },
    )))
}

async fn smb_shares(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let source = &products.source;
    let input: Files = body(request).await?;
    let shares = source
        .smb_shares(input.settings, input.credentials)
        .await
        .map_err(bad_request)?;
    let shares = shares
        .into_iter()
        .map(|share| (share.title, share.address.unwrap_or_default()))
        .collect::<Vec<_>>();
    Ok(json_response(StatusCode::OK, json!({"shares":shares})))
}

async fn plex_profiles(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let source = &products.source;
    #[derive(Deserialize)]
    struct Input {
        login: sources::PlexLogin,
    }
    let (login, profiles) =
        completion(source.plex_profiles(body::<Input>(request).await?.login)).await?;
    Ok(json_response(
        StatusCode::OK,
        json!({"login":login,"profiles":profiles}),
    ))
}

async fn plex_logins(State(products): State<ProductHandles>) -> Result<Response<Body>, Error> {
    let products = &products;
    let source = &products.source;
    let logins = completion(source.plex_saved_logins()).await?;
    Ok(json_response(
        StatusCode::OK,
        json!({"logins":logins.into_iter().map(|(name, login)| json!({"name":name,"login":login})).collect::<Vec<_>>()}),
    ))
}

async fn plex_servers(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let source = &products.source;
    #[derive(Deserialize)]
    struct Input {
        login: sources::PlexLogin,
        profile: sources::PlexProfile,
        pin: Option<String>,
        #[serde(default)]
        lan: Vec<sources::DiscoveredServer>,
    }
    let input: Input = body(request).await?;
    let (login, servers) =
        completion(source.plex_servers(input.login, input.profile, input.pin, input.lan)).await?;
    Ok(json_response(
        StatusCode::OK,
        json!({"login":login,"servers":servers}),
    ))
}

#[derive(Deserialize)]
struct Files {
    settings: sources::FileSourceSettings,
    credentials: sources::FileCredentials,
}

fn login_response<T: serde::Serialize + Send + 'static>(
    receiver: async_channel::Receiver<Result<T, String>>,
) -> Response<Body> {
    let stream = futures_util::stream::unfold(receiver, |receiver| async move {
        let event = receiver.recv().await.ok()?;
        let value = match event {
            Ok(event) => serde_json::to_value(event)
                .unwrap_or_else(|error| json!({"error":error.to_string()})),
            Err(error) => json!({"error":error}),
        };
        Some((
            Ok::<_, Infallible>(Frame::data(Bytes::from(format!(
                "event: login\ndata: {value}\n\n"
            )))),
            receiver,
        ))
    });
    event_response(Body::new(StreamBody::new(stream)))
}
