use super::*;
use crate::runtime::source::*;

pub(super) fn routes() -> Router<ProductHandles> {
    Router::new()
        .route(
            "/api/sources",
            get(list).post(add).patch(update).delete(forget),
        )
        .route("/api/sources/progress", get(progress))
        .route("/api/sources/events", get(events))
        .route("/api/sources/local", post(add))
        .route("/api/sources/select", post(select))
        .route("/api/sources/library", post(select_library))
        .route("/api/sources/edit", get(editable))
        .route("/api/sources/refresh", post(select))
        .route("/api/sources/login/plex", post(plex_login))
        .route("/api/sources/login/emby", post(emby_login))
        .route("/api/sources/login/jellyfin", post(jellyfin_login))
        .route("/api/sources/login/nextcloud", post(nextcloud_login))
        .route("/api/sources/smb/shares", post(smb_shares))
        .route("/api/sources/plex/profiles", post(plex_profiles))
        .route("/api/sources/plex/logins", get(plex_logins))
        .route("/api/sources/plex/servers", post(plex_servers))
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
            "libraries": selected.as_ref().map(|source|source.music_folders.iter().map(|folder|json!({"id":folder.object_id,"name":folder.name})).collect::<Vec<_>>()).unwrap_or_default(),
            "sources": configured.sources.iter().map(|s| json!({"id":s.id,"name":s.name,"kind":s.kind})).collect::<Vec<_>>()
        }),
    ))
}

async fn select_library(
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
        Some(selected) => selected,
        None => completion(products.source.select_source(input.source)).await?,
    };
    completion(selected.operations.set_music_folder(input.library)).await?;
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
    if let sources::EditableSource::Local { roots, .. } =
        configuration.editable().map_err(bad_request)?
    {
        return Ok(json_response(
            StatusCode::OK,
            json!({"id":id,"kind":"local","roots":roots}),
        ));
    }
    let preset = products
        .source
        .configured_source(&id)
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

async fn select(
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
        completion(source.refresh_source(id)).await?;
        Ok(json_response(StatusCode::OK, json!({"refreshed":true})))
    } else {
        let selected = completion(source.select_source(id)).await?;
        Ok(json_response(
            StatusCode::OK,
            json!({"id":selected.source_id}),
        ))
    }
}

async fn forget(
    State(products): State<ProductHandles>,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let parameters = &parameters;
    let source = &products.source;
    completion(source.forget_source(sources::SourceId::new(required(parameters, "id")?))).await?;
    Ok(json_response(StatusCode::OK, json!({"removed":true})))
}

async fn plex_login(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let source = &products.source;
    Ok(login_response(source.plex_login(body(request).await?)))
}

async fn emby_login(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let source = &products.source;
    Ok(login_response(
        source.emby_connect_login(body(request).await?),
    ))
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
    Ok(login_response(source.jellyfin_quick_connect(
        input.server_url,
        input.trust_invalid_cert,
    )))
}

async fn nextcloud_login(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let source = &products.source;
    let input: Files = body(request).await?;
    Ok(login_response(
        source.nextcloud_login(input.settings, input.credentials),
    ))
}

async fn smb_shares(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let source = &products.source;
    let input: Files = body(request).await?;
    let shares = completion(source.smb_shares(input.settings, input.credentials)).await?;
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
