//! Full-page browser requests use the same catalog and playback handlers as the API.
use super::*;
use askama::Template;
use axum::{Form, response::Redirect};

pub(super) fn routes() -> Router<ProductHandles> {
    Router::new()
        .route("/", get(index))
        .route("/session", post(login))
        .route("/session/logout", post(logout))
        .route("/action", post(action))
}

fn random_secret() -> Result<String, Error> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(internal)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn cookie_name(headers: &hyper::HeaderMap) -> String {
    let port = headers
        .get(hyper::header::HOST)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<axum::http::uri::Authority>().ok())
        .and_then(|v| v.port_u16())
        .unwrap_or(0);
    format!("rufin-session-{port}")
}

fn cookie(headers: &hyper::HeaderMap) -> Option<&str> {
    let cookie_name = cookie_name(headers);
    headers
        .get_all(hyper::header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|v| v.trim().split_once('='))
        .find_map(|(name, value)| (name == cookie_name).then_some(value))
}

pub(super) fn session(auth: &Authorization, headers: &hyper::HeaderMap) -> Option<String> {
    let (nonce, signature) = cookie(headers)?.split_once('.')?;
    let signature = blake3::Hash::from_hex(signature).ok()?;
    let expected = blake3::keyed_hash(&auth.browser_key, nonce.as_bytes());
    (signature == expected).then(|| nonce.to_owned())
}

fn form_session(
    auth: &Authorization,
    headers: &hyper::HeaderMap,
    form: &HashMap<String, String>,
) -> Result<(), Error> {
    let csrf = session(auth, headers)
        .ok_or_else(|| error(StatusCode::UNAUTHORIZED, "Please connect again"))?;
    if form.get("csrf") != Some(&csrf) {
        return Err(error(StatusCode::FORBIDDEN, "Invalid form token"));
    }
    Ok(())
}

fn set_cookie(
    response: &mut Response<Body>,
    headers: &hyper::HeaderMap,
    value: &str,
    age: Option<u64>,
) {
    let name = cookie_name(headers);
    let lifetime = age
        .map(|age| format!("; Max-Age={age}"))
        .unwrap_or_default();
    let secure = headers
        .get(hyper::header::ORIGIN)
        .is_some_and(|v| v.as_bytes().starts_with(b"https://"));
    response.headers_mut().insert(
        hyper::header::SET_COOKIE,
        format!(
            "{name}={value}; Path=/; HttpOnly; SameSite=Lax{lifetime}{}",
            if secure { "; Secure" } else { "" }
        )
        .parse()
        .expect("session cookie"),
    );
    response
        .headers_mut()
        .insert(hyper::header::CACHE_CONTROL, "no-store".parse().unwrap());
}

async fn login(
    Extension(auth): Extension<Arc<Authorization>>,
    Extension(peer): Extension<SocketAddr>,
    headers: hyper::HeaderMap,
    Form(form): Form<HashMap<String, String>>,
) -> Response<Body> {
    // The submitted API token authenticates login; an existing cookie does not.
    let token = format!(
        "Bearer {}",
        form.get("token").map(String::as_str).unwrap_or_default()
    );
    if let Err(response) = auth.check(peer.ip(), Some(&token), Instant::now()) {
        if headers
            .get(hyper::header::ACCEPT)
            .is_some_and(|v| v == "application/json")
        {
            return *response;
        }
        if response.status() == StatusCode::UNAUTHORIZED {
            return Redirect::to("/?login=failed").into_response();
        }
        return *response;
    }
    let csrf = match random_secret() {
        Ok(nonce) => nonce,
        Err(error) => return error.into_response(),
    };
    let signature = blake3::keyed_hash(&auth.browser_key, csrf.as_bytes());
    let value = format!("{csrf}.{}", signature.to_hex());
    let mut response = if headers
        .get(hyper::header::ACCEPT)
        .is_some_and(|v| v == "application/json")
    {
        json_response(StatusCode::OK, json!({"csrf":csrf}))
    } else {
        Redirect::to("/").into_response()
    };
    set_cookie(&mut response, &headers, &value, None);
    response
}

async fn logout(
    Extension(auth): Extension<Arc<Authorization>>,
    headers: hyper::HeaderMap,
    Form(form): Form<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    form_session(&auth, &headers, &form)?;
    let mut response = Redirect::to("/").into_response();
    set_cookie(&mut response, &headers, "", Some(0));
    Ok(response)
}

pub(super) struct QueueRow {
    pub id: String,
    pub title: String,
    pub artist: String,
}

pub(super) struct Page {
    pub connected: bool,
    pub appearance: Value,
    pub csrf: String,
    pub error: String,
    pub source: String,
    pub sources: Vec<(String, String)>,
    pub parameters: HashMap<String, String>,
    pub content: String,
    pub previous: Option<String>,
    pub next: Option<String>,
    pub playing: String,
    pub current_favorite: bool,
    pub artist: String,
    pub album: String,
    pub controls: Value,
    pub position: u64,
    pub duration: u64,
    pub queue: Vec<QueueRow>,
    pub queue_total: usize,
    pub paused: bool,
    pub volume: f64,
    pub folders: Vec<(String, String)>,
}

pub(super) fn link(parameters: &HashMap<String, String>) -> String {
    format!(
        "/?{}",
        url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(parameters)
            .finish()
    )
}

impl Page {
    pub fn theme(&self) -> String {
        self.appearance["colors"]["color-scheme"]
            .as_str()
            .unwrap_or_else(|| self.appearance["theme"].as_str().unwrap_or("System"))
            .to_lowercase()
    }
    pub fn color_style(&self) -> String {
        self.appearance["colors"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(name, value)| value.as_str().map(|value| format!("{name}:{value};")))
            .collect()
    }
    pub fn parent_view(&self) -> &str {
        match self.view() {
            "album" => "albums",
            "artist" => "artists",
            "genre" => "home",
            "playlist" => "playlists",
            "smart-playlist" => "smart-playlists",
            _ => "",
        }
    }
    pub fn can_play_collection(&self) -> bool {
        !self.parent_view().is_empty()
            || (!self.source.is_empty() && matches!(self.view(), "tracks" | "favorites"))
    }
    pub fn needs_source(&self) -> bool {
        self.source.is_empty()
            && !matches!(
                self.view(),
                "playlists" | "playlist" | "smart-playlists" | "smart-playlist"
            )
    }

    pub fn current_id(&self) -> &str {
        self.controls["current"]["id"].as_str().unwrap_or_default()
    }
    pub fn has_current(&self) -> bool {
        !self.controls["current"].is_null()
    }
    pub fn current_selection(&self) -> String {
        json!({"kind":"track", "uri":self.controls["current"]["track"]["uri"], "favorite":!self.current_favorite}).to_string()
    }
    pub fn position_time(&self) -> String {
        format!("{}:{:02}", self.position / 60000, self.position / 1000 % 60)
    }
    pub fn duration_time(&self) -> String {
        format!("{}:{:02}", self.duration / 60000, self.duration / 1000 % 60)
    }
    pub fn nav(&self, view: &str) -> String {
        let mut parameters = HashMap::from([("view".into(), view.into())]);
        if !self.source.is_empty() {
            parameters.insert("source".into(), self.source.clone());
        }
        if let Some(folder) = self.parameters.get("folder") {
            parameters.insert("folder".into(), folder.clone());
        }
        link(&parameters)
    }
    pub fn current(&self) -> String {
        link(&self.parameters)
    }
    pub fn view(&self) -> &str {
        self.parameters
            .get("view")
            .map(String::as_str)
            .unwrap_or("home")
    }
    pub fn query(&self) -> &str {
        self.parameters.get("q").map(String::as_str).unwrap_or("")
    }
    pub fn folder(&self) -> &str {
        self.parameters
            .get("folder")
            .map(String::as_str)
            .unwrap_or("")
    }
    pub fn source_name(&self) -> &str {
        self.sources
            .iter()
            .find(|(id, _)| id == &self.source)
            .map(|(_, name)| name.as_str())
            .unwrap_or("")
    }
    pub fn sort(&self) -> &str {
        self.parameters
            .get("sort")
            .map(String::as_str)
            .unwrap_or(match self.view() {
                "album" => "track_number",
                "playlist" => "position",
                _ => "title",
            })
    }
    pub fn descending(&self) -> bool {
        self.parameters
            .get("descending")
            .is_some_and(|v| v == "true")
    }
    pub fn enabled(&self, name: &str) -> bool {
        self.controls[name].as_bool().unwrap_or(false)
    }
    pub fn repeat_title(&self) -> String {
        localization::tr(match self.controls["repeat"].as_str() {
            Some("all") => "Repeat all",
            Some("one") => "Repeat one",
            _ => "Repeat off",
        })
    }
    pub fn title(&self) -> String {
        if let Some(title) = self.parameters.get("title") {
            return title.clone();
        }
        localization::tr(match self.view() {
            "home" => "Home",
            "favorites" => "Favorites",
            "albums" => "Albums",
            "artists" => "Artists",
            "playlists" => "Playlists",
            "smart-playlists" => "Smart Playlists",
            _ => "Tracks",
        })
    }
}

async fn index(
    State(products): State<ProductHandles>,
    Extension(auth): Extension<Arc<Authorization>>,
    headers: hyper::HeaderMap,
    Query(mut parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    if parameters.remove("reverse").is_some() {
        parameters.insert(
            "descending".into(),
            (!boolean(&parameters, "descending")?).to_string(),
        );
    }
    let csrf = session(&auth, &headers);
    let mut page = Page {
        connected: csrf.is_some(),
        appearance: if csrf.is_some() {
            catalog::appearance_data(&products)
        } else {
            json!({"theme":"System", "accent":"System", "colors":{}})
        },
        csrf: csrf.unwrap_or_default(),
        error: if parameters.get("login").is_some_and(|v| v == "failed") {
            localization::tr("The API token was not accepted. Please connect again.")
        } else {
            String::new()
        },
        source: String::new(),
        sources: vec![],
        parameters: HashMap::new(),
        content: String::new(),
        previous: None,
        next: None,
        playing: String::new(),
        current_favorite: false,
        artist: String::new(),
        album: String::new(),
        controls: Value::Null,
        position: 0,
        duration: 0,
        queue: vec![],
        queue_total: 0,
        paused: true,
        volume: 1.0,
        folders: vec![],
    };
    if page.connected {
        let configured = products.source.list_sources();
        page.sources = configured
            .sources
            .iter()
            .map(|s| (s.id.to_string(), s.name.clone()))
            .collect();
        if parameters.get("source").is_some_and(|s| s.is_empty()) {
            parameters.remove("source");
        }
        if !parameters.contains_key("source") {
            if let Some(source) = configured
                .selected_source_id
                .as_ref()
                .map(ToString::to_string)
                .or_else(|| page.sources.first().map(|s| s.0.clone()))
            {
                parameters.insert("source".into(), source);
            }
        }
        page.source = parameters.get("source").cloned().unwrap_or_default();
        if let Some(selected) = products
            .source
            .selected_library()
            .filter(|s| s.source_id.as_str() == page.source)
        {
            page.folders = selected
                .music_folders
                .iter()
                .map(|f| (f.object_id.clone(), f.name.clone()))
                .collect();
            if !parameters.contains_key("folder") {
                if let Some(folder) = &selected.music_folder_object_id {
                    parameters.insert("folder".into(), folder.clone());
                }
            }
        }
        parameters.insert("limit".into(), "48".into());
        page.parameters = parameters.clone();
        let current = products.playback.updates.current();
        let playback = playback_json(current.as_deref());
        page.position = playback["position_ms"].as_u64().unwrap_or_default();
        page.duration = playback["duration_ms"].as_u64().unwrap_or_default();
        let queue = queue_json(current.as_deref(), 5);
        page.queue_total = queue["total"].as_u64().unwrap_or_default() as usize;
        page.queue = queue["window"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|row| QueueRow {
                id: row["id"].as_str().unwrap_or_default().into(),
                title: row["track"]["title"].as_str().unwrap_or_default().into(),
                artist: row["track"]["artist"].as_str().unwrap_or_default().into(),
            })
            .collect();
        page.artist = playback["current"]["track"]["artist"]
            .as_str()
            .unwrap_or_default()
            .into();
        page.album = playback["current"]["track"]["album"]
            .as_str()
            .unwrap_or_default()
            .into();
        page.paused = !matches!(
            playback["state"].as_str(),
            Some("playing" | "resolving" | "buffering")
        );
        if let Some(uri) = playback["current"]["track"]["uri"].as_str() {
            page.current_favorite = products
                .library
                .favorite(&library::FavoriteTarget::Track(uri.into()))
                .await
                .map_err(internal)?;
        }
        page.controls = playback.clone();
        page.volume = playback["volume"].as_f64().unwrap_or(1.0);
        page.playing = playback["current"]["track"]["title"]
            .as_str()
            .unwrap_or_default()
            .into();
        if !page.needs_source() {
            match catalog_page(products, parameters.clone()).await {
                Ok(value) => {
                    let count = [
                        "tracks",
                        "entries",
                        "albums",
                        "artists",
                        "playlists",
                        "smart_playlists",
                    ]
                    .iter()
                    .find_map(|key| value[*key].as_array().map(Vec::len))
                    .unwrap_or(0);
                    let offset = number(&parameters, "offset", 0)?;
                    let mut target = parameters.clone();
                    if offset > 0 {
                        target.insert("offset".into(), offset.saturating_sub(48).to_string());
                        page.previous = Some(link(&target));
                    }
                    if count == 48 {
                        target.insert("offset".into(), offset.saturating_add(48).to_string());
                        page.next = Some(link(&target));
                    }
                    page.content = web::render(&parameters, &value, &page.csrf)?;
                }
                Err((_, message)) => {
                    page.error = message.0["error"].as_str().unwrap_or_default().into()
                }
            }
        }
    }
    let html = web::Index {
        version: env!("CARGO_PKG_VERSION"),
        page: &page,
    }
    .render()
    .map_err(internal)?;
    Ok(Response::builder()
        .header("content-type", "text/html; charset=utf-8")
        .header("cache-control", "no-store")
        .header("referrer-policy", "same-origin")
        .body(Body::from(html))
        .expect("page headers"))
}

async fn catalog_page(
    products: ProductHandles,
    mut parameters: HashMap<String, String>,
) -> Result<Value, Error> {
    let view = parameters
        .get("view")
        .cloned()
        .unwrap_or_else(|| "home".into());
    if view == "favorites" {
        parameters.insert("favorites".into(), "true".into());
    }
    let state = State(products);
    let query = Query(parameters);
    let headers = hyper::HeaderMap::new();
    let response = match view.as_str() {
        "home" => catalog::home(state, headers, query).await,
        "tracks" | "favorites" => tracks(state, headers, query).await,
        "albums" => catalog::albums(state, headers, query).await,
        "album" => catalog::album_tracks(state, headers, query).await,
        "artists" => catalog::artists(state, headers, query).await,
        "artist" => catalog::artist_tracks(state, headers, query).await,
        "genre" => catalog::genre_tracks(state, headers, query).await,
        "playlists" => playlists::list(state, headers, query).await,
        "playlist" => playlists::entries(state, headers, query).await,
        "smart-playlists" => catalog::smart_playlists(state, headers, query).await,
        "smart-playlist" => catalog::smart_tracks(state, headers, query).await,
        _ => return Err(bad_request("Unknown library view")),
    }?;
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .map_err(internal)?;
    serde_json::from_slice(&bytes).map_err(internal)
}

async fn action(
    State(products): State<ProductHandles>,
    Extension(auth): Extension<Arc<Authorization>>,
    headers: hyper::HeaderMap,
    Form(form): Form<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    form_session(&auth, &headers, &form)?;
    let command = required(&form, "action")?;
    if command == "refresh" {
        if let Some(source) = form.get("source").filter(|s| !s.is_empty()) {
            source::select(
                State(products),
                Request::builder()
                    .uri("/api/sources/refresh")
                    .body(Body::from(json!({"id":source}).to_string()))
                    .unwrap(),
            )
            .await?;
        }
    } else if command == "favorite" {
        media::favorite(
            State(products),
            Request::new(Body::from(required(&form, "selection")?.to_owned())),
        )
        .await?;
    } else if command == "activate" {
        queue_activate(
            State(products),
            Request::new(Body::from(json!({"id":required(&form, "id")?}).to_string())),
        )
        .await?;
    } else if command == "queue-next" {
        media::play_next(
            State(products),
            Request::new(Body::from(json!({"id":required(&form, "id")?}).to_string())),
        )
        .await?;
    } else if command == "queue-later" {
        media::reorder(
            State(products),
            Request::new(Body::from(
                json!({"ids":[required(&form, "id")?],"before":null}).to_string(),
            )),
        )
        .await?;
    } else if command == "queue-remove" {
        queue_remove(
            State(products),
            Query(HashMap::from([(
                "id".into(),
                required(&form, "id")?.into(),
            )])),
        )
        .await?;
    } else if command == "clear-queue" {
        queue_clear(State(products)).await?;
    } else if command == "source" {
        let id = required(&form, "source")?;
        source::select(
            State(products),
            Request::builder()
                .uri("/api/sources/select")
                .body(Body::from(json!({"id":id}).to_string()))
                .unwrap(),
        )
        .await?;
    } else if command == "folder" {
        let library = form.get("folder").filter(|f| !f.is_empty());
        source::select_library(
            State(products),
            Request::builder()
                .uri("/api/sources/library")
                .body(Body::from(
                    json!({"source":required(&form, "source")?,"library":library}).to_string(),
                ))
                .unwrap(),
        )
        .await?;
    } else if matches!(command, "replace" | "append" | "next") && form.contains_key("selection") {
        let mut selection: Value =
            serde_json::from_str(required(&form, "selection")?).map_err(bad_request)?;
        if command == "replace" && selection["kind"] == "track" {
            let parameters: HashMap<String, String> = url::form_urlencoded::parse(
                form.get("return")
                    .and_then(|v| v.strip_prefix("/?"))
                    .unwrap_or_default()
                    .as_bytes(),
            )
            .into_owned()
            .collect();
            let view = parameters.get("view").map(String::as_str).unwrap_or("home");
            if matches!(
                view,
                "tracks"
                    | "favorites"
                    | "album"
                    | "artist"
                    | "genre"
                    | "playlist"
                    | "smart-playlist"
            ) {
                selection["anchor_uri"] = selection["uris"][0].clone();
                if view == "playlist" {
                    selection["anchor_entry"] = selection["id"].clone();
                }
                selection["kind"] = json!(if matches!(view, "tracks" | "favorites") {
                    "source"
                } else {
                    view
                });
                for key in ["source", "folder", "q", "sort"] {
                    if let Some(value) = parameters.get(key) {
                        selection[key] = json!(value);
                    }
                }
                if let Some(id) = parameters.get("id") {
                    selection["id"] = serde_json::from_str(id).map_err(bad_request)?;
                }
                selection["favorites"] = json!(view == "favorites");
                selection["descending"] =
                    json!(parameters.get("descending").is_some_and(|v| v == "true"));
            }
        }
        let kind = selection["kind"].as_str().unwrap_or_default();
        let path = match kind {
            "track" => "/api/queue".into(),
            "album" | "artist" | "genre" | "playlist" | "smart-playlist" | "source" => {
                format!("/api/queue/{kind}")
            }
            _ => return Err(bad_request("Unknown media kind")),
        };
        selection["mode"] = command.into();
        queue_play(
            State(products),
            Request::builder()
                .uri(path)
                .body(Body::from(selection.to_string()))
                .unwrap(),
        )
        .await?;
    } else if matches!(
        command,
        "play"
            | "pause"
            | "stop"
            | "next"
            | "previous"
            | "volume"
            | "seek"
            | "auto-dj"
            | "shuffle"
            | "repeat"
            | "mute"
    ) {
        let value = if command == "volume" {
            json!({"volume":required(&form, "volume")?.parse::<f64>().map_err(bad_request)?})
        } else {
            let current = playback_json(products.playback.updates.current().as_deref());
            match command {
                "seek" => {
                    json!({"position_ms":required(&form, "position_ms")?.parse::<u64>().map_err(bad_request)?})
                }
                "shuffle" => json!({"enabled":!current["shuffle"].as_bool().unwrap_or(false)}),
                "mute" => json!({"muted":!current["muted"].as_bool().unwrap_or(false)}),
                "repeat" => {
                    json!({"mode":match current["repeat"].as_str() {Some("off")=>"all",Some("all")=>"one",_=>"off"}})
                }
                _ => Value::Null,
            }
        };
        playback_command(
            State(products),
            Request::builder()
                .uri(format!("/api/playback/{command}"))
                .body(Body::from(value.to_string()))
                .unwrap(),
        )
        .await?;
    } else {
        return Err(bad_request("Unknown playback action"));
    }
    let target = form
        .get("return")
        .filter(|v| v.starts_with("/?") && !v.contains(['\r', '\n']))
        .map(String::as_str)
        .unwrap_or("/");
    Ok(Redirect::to(target).into_response())
}
