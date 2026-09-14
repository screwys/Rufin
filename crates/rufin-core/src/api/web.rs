use super::*;
use askama::Template;

#[path = "../../../../web/gettext.rs"]
mod web_gettext;

async fn translations() -> Response<Body> {
    static MESSAGES: std::sync::OnceLock<std::collections::BTreeSet<String>> =
        std::sync::OnceLock::new();
    let messages = MESSAGES.get_or_init(|| {
        [
            include_str!("../../../../web/ui.js"),
            include_str!("../../../../web/library.js"),
            include_str!("../../../../web/connection.js"),
            include_str!("../../../../web/player.js"),
            include_str!("../../../../web/queue.js"),
            include_str!("../../../../web/sources.js"),
            include_str!("../../../../web/menus.js"),
            include_str!("../../../../web/random.js"),
            include_str!("../../../../web/pins.js"),
        ]
        .into_iter()
        .flat_map(web_gettext::messages)
        .collect()
    });
    let translated: std::collections::BTreeMap<_, _> = messages
        .iter()
        .map(|message| (message, localization::tr(message)))
        .collect();
    Response::builder()
        .header("content-type", "text/javascript; charset=utf-8")
        .header("cache-control", "no-store")
        .body(Body::from(format!(
            "export const messages = {};",
            serde_json::to_string(&translated).expect("translated strings")
        )))
        .expect("translation headers")
}

#[derive(Template)]
#[template(path = "index.html")]
struct Index<'a> {
    pub version: &'static str,
    pub page: &'a Page,
}

struct Row<'a> {
    data: &'a Value,
    parameters: &'a HashMap<String, String>,
    csrf: &'a str,
    menu_id: String,
}

impl Row<'_> {
    fn href(&self, kind: &str) -> String {
        let mut query = self.parameters.clone();
        query.remove("offset");
        query.remove("q");
        query.remove("sort");
        query.insert("title".into(), self.title().into());
        query.insert("view".into(), kind.into());
        query.insert(
            "id".into(),
            self.data["id"].to_string().trim_matches('"').into(),
        );
        link(&query)
    }
    fn artwork(&self, kind: &str) -> String {
        let mut query = HashMap::new();
        match kind {
            "playlist" => {
                query.insert("playlist".to_owned(), self.data["id"].to_string());
            }
            "smart-playlist" => {
                query = self.parameters.clone();
                query.insert("smart_playlist".into(), self.data["id"].to_string());
            }
            _ => {
                query.insert("uri".into(), self.text("uri").into());
            }
        }
        format!(
            "/api/artwork?{}",
            url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(query)
                .finish()
        )
    }
    fn return_to(&self) -> String {
        link(self.parameters)
    }
    fn selection(&self, kind: &str) -> String {
        let mut value = json!({"kind":kind,"id":self.data["id"],"uri":self.data["uri"],"favorite":!self.favorite()});
        if kind == "track" {
            value["uris"] = json!([self.data["uri"]]);
        }
        for field in ["source", "folder"] {
            if let Some(text) = self.parameters.get(field) {
                value[field] = json!(text);
            }
        }
        value.to_string()
    }

    fn track_count(&self) -> String {
        localization::track_count_text(self.number("track_count"))
    }
    fn text(&self, field: &str) -> &str {
        self.data[field].as_str().unwrap_or_default()
    }

    fn number(&self, field: &str) -> u64 {
        self.data[field].as_u64().unwrap_or_default()
    }

    fn title(&self) -> &str {
        self.data["title"]
            .as_str()
            .unwrap_or_else(|| self.text("name"))
    }

    fn favorite(&self) -> bool {
        self.data["favorite"].as_bool().unwrap_or(false)
    }

    fn duration(&self) -> String {
        let seconds = self.number("duration_ms") / 1000;
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }

    fn long_duration(&self) -> String {
        let seconds = self.number("duration_ms") / 1000;
        [
            (seconds / 3600, "h"),
            (seconds % 3600 / 60, "m"),
            (seconds % 60, "s"),
        ]
        .into_iter()
        .filter(|(value, _)| *value != 0)
        .map(|(value, unit)| format!("{value}{unit}"))
        .collect::<Vec<_>>()
        .join(" ")
    }

    fn palette(&self) -> u32 {
        let seed = self
            .text("object_id")
            .bytes()
            .fold(0x811c9dc5u32, |seed, byte| {
                seed.wrapping_mul(16777619) ^ u32::from(byte)
            });
        (seed ^ seed.rotate_left(13) ^ seed.rotate_right(9)) % 16
    }
}

#[derive(Template)]
#[template(path = "library.html")]
struct Library<'a> {
    rows: Vec<Row<'a>>,
    kind: &'a str,
    offset: usize,
    album_tracks: bool,
    disc_headers: HashMap<usize, String>,
}

impl Library<'_> {
    fn disc_heading(&self, index: &usize) -> Option<&str> {
        self.disc_headers
            .get(&(self.offset + index))
            .map(String::as_str)
    }
}

#[cfg(test)]
mod disc_tests {
    use super::*;

    #[tokio::test]
    async fn album_disc_headers_keep_page_counts_and_track_numbers() {
        let mut headers = hyper::HeaderMap::new();
        headers.insert("HX-Request", "true".parse().unwrap());
        for (offset, sections, has_header) in [
            (0, json!([]), false),
            (0, json!([[0, 1], [2, 2]]), true),
            (1, json!([[0, 1], [2, 2]]), false),
            (2, json!([[0, 1], [2, 2]]), true),
        ] {
            let parameters = HashMap::from([("offset".into(), offset.to_string())]);
            let response = page(
                &headers,
                &parameters,
                json!({
                    "album_tracks": true, "disc_sections": sections,
                    "tracks": [{"uri":"file:///track", "title":"Track", "track_number":7}]
                }),
            )
            .unwrap();
            let html = String::from_utf8(
                axum::body::to_bytes(response.into_body(), usize::MAX)
                    .await
                    .unwrap()
                    .to_vec(),
            )
            .unwrap();
            assert_eq!(html.contains("class=\"disc-header\""), has_header);
            assert!(html.contains("data-count=\"1\""));
            assert_eq!(html.matches("data-row=").count(), 1);
            assert!(html.contains("class=\"number\">7</td>"));
        }
    }
}

struct Section<'a> {
    block: &'a str,
    title: &'a str,
    items: Vec<Row<'a>>,
}

#[derive(Template)]
#[template(path = "home.html")]
struct Home<'a> {
    sections: Vec<Section<'a>>,
}

pub(super) fn render(
    parameters: &HashMap<String, String>,
    value: &Value,
    csrf: &str,
) -> Result<String, Error> {
    let html = if let Some(sections) = value["sections"].as_array() {
        Home {
            sections: sections
                .iter()
                .enumerate()
                .map(|(section_index, section)| Section {
                    block: section["block"].as_str().unwrap_or_default(),
                    title: section["title"].as_str().unwrap_or_default(),
                    items: section["items"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .enumerate()
                        .map(|(index, data)| Row {
                            data,
                            parameters,
                            csrf,
                            menu_id: format!(
                                "media-menu-home-{}-{index}",
                                section["block"]
                                    .as_str()
                                    .filter(|block| !block.is_empty())
                                    .map(str::to_owned)
                                    .unwrap_or_else(|| format!("provider-{section_index}"))
                            ),
                        })
                        .collect(),
                })
                .collect(),
        }
        .render()
    } else {
        let offset = number(parameters, "offset", 0)?;
        let (field, kind) = [
            ("tracks", "track"),
            ("entries", "track"),
            ("albums", "album"),
            ("artists", "artist"),
            ("playlists", "playlist"),
            ("smart_playlists", "smart-playlist"),
        ]
        .into_iter()
        .find(|(field, _)| value[*field].is_array())
        .ok_or_else(|| internal("Missing library rows"))?;
        Library {
            rows: value[field]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
                .map(|(index, data)| Row {
                    data,
                    parameters,
                    csrf,
                    menu_id: format!("media-menu-library-{}", offset + index),
                })
                .collect(),
            kind,
            offset,
            album_tracks: value["album_tracks"].as_bool().unwrap_or(false),
            disc_headers: value["disc_sections"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|section| {
                    let start = section[0].as_u64()? as usize;
                    let disc = section[1].as_i64()?;
                    let title = if disc > 0 {
                        localization::tr("Disc {number}").replace("{number}", &disc.to_string())
                    } else {
                        localization::tr("Unknown disc")
                    };
                    Some((start, title))
                })
                .collect(),
        }
        .render()
    }
    .map_err(internal)?;
    Ok(html)
}

pub(super) fn page(
    headers: &hyper::HeaderMap,
    parameters: &HashMap<String, String>,
    value: Value,
) -> Result<Response<Body>, Error> {
    let mut response = if headers
        .get("HX-Request")
        .is_some_and(|value| value == "true")
    {
        let csrf = headers
            .get("x-rufin-csrf")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        let html = render(parameters, &value, csrf)?;
        axum::response::Html(html).into_response()
    } else {
        json_response(StatusCode::OK, value)
    };
    response
        .headers_mut()
        .insert(hyper::header::VARY, "HX-Request".parse().unwrap());
    response
        .headers_mut()
        .insert(hyper::header::CACHE_CONTROL, "no-store".parse().unwrap());
    Ok(response)
}

pub(super) fn routes() -> Router<ProductHandles> {
    Router::new()
        .route("/", get(index))
        .route("/session", post(login))
        .route("/session/logout", post(logout))
        .route("/translations.js", get(translations))
        .route(
            "/showcase.css",
            get(|| async {
                asset(
                    "text/css; charset=utf-8",
                    include_bytes!("../../../../data/showcase.css"),
                )
            }),
        )
        .route(
            "/app.css",
            get(|| async {
                asset(
                    "text/css; charset=utf-8",
                    include_bytes!("../../../../web/app.css"),
                )
            }),
        )
        .route(
            "/app.js",
            get(|| async {
                asset(
                    "text/javascript; charset=utf-8",
                    include_bytes!("../../../../web/app.js"),
                )
            }),
        )
        .route("/icons/{name}", get(icon))
        .route("/source-icons/{kind}", get(source_icon))
        .route("/{file}", get(script))
        .route("/styles/{file}", get(style))
        .route(
            "/htmx.min.js",
            get(|| async {
                asset(
                    "text/javascript; charset=utf-8",
                    include_bytes!("../../../../web/vendor/htmx.min.js"),
                )
            }),
        )
        .route(
            "/htmx.LICENSE",
            get(|| async {
                asset(
                    "text/plain; charset=utf-8",
                    include_bytes!("../../../../web/vendor/htmx.LICENSE"),
                )
            }),
        )
}

async fn script(
    axum::extract::Path(file): axum::extract::Path<String>,
) -> Result<Response<Body>, Error> {
    macro_rules! file {
        ($name:literal) => {
            (
                $name,
                include_bytes!(concat!("../../../../web/", $name)) as &'static [u8],
            )
        };
    }
    let files = [
        file!("ui.js"),
        file!("connection.js"),
        file!("library.js"),
        file!("player.js"),
        file!("sources.js"),
        file!("menus.js"),
        file!("random.js"),
        file!("queue.js"),
        file!("pins.js"),
        file!("drag.js"),
    ];
    let bytes = files
        .iter()
        .find(|(name, _)| *name == file)
        .map(|(_, bytes)| *bytes)
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "File not found"))?;
    Ok(asset("text/javascript; charset=utf-8", bytes))
}

async fn style(
    axum::extract::Path(file): axum::extract::Path<String>,
) -> Result<Response<Body>, Error> {
    macro_rules! file {
        ($name:literal) => {
            (
                $name,
                include_bytes!(concat!("../../../../web/styles/", $name)) as &'static [u8],
            )
        };
    }
    let files = [
        file!("base.css"),
        file!("layout.css"),
        file!("icons.css"),
        file!("library.css"),
        file!("player.css"),
        file!("sources.css"),
        file!("menus.css"),
        file!("random.css"),
    ];
    let bytes = files
        .iter()
        .find(|(name, _)| *name == file)
        .map(|(_, bytes)| *bytes)
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "File not found"))?;
    Ok(asset("text/css; charset=utf-8", bytes))
}

async fn source_icon(
    axum::extract::Path(kind): axum::extract::Path<String>,
) -> Result<Response<Body>, Error> {
    macro_rules! png {
        ($name:literal) => {
            asset(
                "image/png",
                include_bytes!(concat!(
                    "../../../../data/icons/hicolor/64x64/apps/io.github.screwys.Rufin.source.",
                    $name,
                    ".png"
                )),
            )
        };
    }
    macro_rules! svg {
        ($name:literal) => {
            asset(
                "image/svg+xml",
                include_bytes!(concat!(
                    "../../../../data/icons/hicolor/scalable/apps/io.github.screwys.Rufin.source.",
                    $name,
                    ".svg"
                )),
            )
        };
    }
    Ok(match kind.as_str() {
        "jellyfin" => png!("jellyfin"),
        "navidrome" => png!("navidrome"),
        "subsonic" => png!("opensubsonic"),
        "webdav" => png!("webdav"),
        "smb" => png!("smb"),
        "plex" => svg!("plex"),
        "emby" => svg!("emby"),
        _ => return Err(error(StatusCode::NOT_FOUND, "Source icon not found")),
    })
}

fn asset(kind: &'static str, bytes: &'static [u8]) -> Response<Body> {
    Response::builder()
        .header("content-type", kind)
        .header("cache-control", "no-cache")
        .header("x-content-type-options", "nosniff")
        .body(Body::from(bytes))
        .expect("asset headers")
}

async fn icon(
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Result<Response<Body>, Error> {
    if name == "rufin-symbolic.svg" {
        return Ok(asset(
            "image/svg+xml",
            include_bytes!(
                "../../../../data/icons/hicolor/symbolic/apps/io.github.screwys.Rufin-symbolic.svg"
            ),
        ));
    }
    if name == "rufin.svg" || name == "favicon.svg" {
        let svg = include_str!(
            "../../../../data/icons/hicolor/scalable/apps/io.github.screwys.Rufin.svg"
        )
        .replacen("viewBox=\"0 0 128 128\"", "viewBox=\"0 0 112 112\"", 1)
        .replacen(" transform=\"translate(4,4)\"", "", 1);
        let mut response = asset("image/svg+xml", &[]);
        *response.body_mut() = Body::from(svg);
        return Ok(response);
    }
    macro_rules! icon {
        ($name:literal) => {
            (
                $name,
                include_bytes!(concat!(
                    "../../../../data/icons/hicolor/scalable/actions/",
                    $name
                )) as &'static [u8],
            )
        };
    }
    const ICONS: &[(&str, &[u8])] = &[
        icon!("rufin-application-exit-symbolic.svg"),
        icon!("rufin-audio-only-symbolic.svg"),
        icon!("rufin-playlists-compact-symbolic.svg"),
        icon!("rufin-document-edit-symbolic.svg"),
        icon!("rufin-object-select-symbolic.svg"),
        icon!("rufin-x-office-calendar-symbolic.svg"),
        icon!("rufin-more-symbolic.svg"),
        icon!("rufin-open-menu-symbolic.svg"),
        icon!("rufin-process-stop-symbolic.svg"),
        icon!("rufin-home-symbolic.svg"),
        icon!("rufin-sort-name-symbolic.svg"),
        icon!("rufin-sort-name-descending-symbolic.svg"),
        icon!("rufin-preferences-system-time-symbolic.svg"),
        icon!("rufin-heart-outline-symbolic.svg"),
        icon!("rufin-heart-filled-symbolic.svg"),
        icon!("rufin-mail-forward-symbolic.svg"),
        icon!("rufin-go-last-symbolic.svg"),
        icon!("rufin-artists-symbolic.svg"),
        icon!("rufin-smart-playlists-symbolic.svg"),
        icon!("rufin-shuffle-symbolic.svg"),
        icon!("rufin-external-link-compact-symbolic.svg"),
        (
            "rufin-edit-clear-symbolic.svg",
            include_bytes!(
                "../../../../data/icons/hicolor/16x16/actions/rufin-edit-clear-symbolic.svg"
            ) as &'static [u8],
        ),
        icon!("rufin-window-close-symbolic.svg"),
        icon!("rufin-repeat-symbolic.svg"),
        icon!("rufin-repeat-one-symbolic.svg"),
        icon!("rufin-auto-dj-symbolic.svg"),
        icon!("rufin-random-symbolic.svg"),
        icon!("rufin-preferences-system-symbolic.svg"),
        icon!("rufin-sidebar-hide-symbolic.svg"),
        icon!("rufin-sidebar-collapse-right-symbolic.svg"),
        icon!("rufin-view-more-symbolic.svg"),
        icon!("rufin-player-more-symbolic.svg"),
        icon!("rufin-media-playback-start-symbolic.svg"),
        icon!("rufin-media-playback-pause-symbolic.svg"),
        icon!("rufin-media-skip-backward-symbolic.svg"),
        icon!("rufin-media-skip-forward-symbolic.svg"),
        icon!("rufin-audio-volume-high-symbolic.svg"),
        icon!("rufin-audio-volume-low-symbolic.svg"),
        icon!("rufin-audio-volume-medium-symbolic.svg"),
        icon!("rufin-sidebar-expand-right-symbolic.svg"),
        icon!("rufin-audio-volume-muted-symbolic.svg"),
        icon!("rufin-tracks-symbolic.svg"),
        icon!("rufin-albums-symbolic.svg"),
        icon!("rufin-playlists-symbolic.svg"),
        icon!("rufin-folders-symbolic.svg"),
        icon!("rufin-music-queue-symbolic.svg"),
        icon!("rufin-network-server-symbolic.svg"),
        icon!("rufin-list-add-symbolic.svg"),
        icon!("rufin-list-remove-symbolic.svg"),
        icon!("rufin-view-refresh-symbolic.svg"),
        icon!("rufin-go-previous-symbolic.svg"),
        icon!("rufin-go-next-symbolic.svg"),
        icon!("rufin-sidebar-show-symbolic.svg"),
        icon!("rufin-lyrics-search-symbolic.svg"),
    ];
    let bytes = ICONS
        .iter()
        .find(|(id, _)| *id == name)
        .map(|(_, bytes)| *bytes)
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "Icon not found"))?;
    Ok(asset("image/svg+xml", bytes))
}

struct QueueRow {
    pub id: String,
    pub title: String,
    pub artist: String,
}

struct Page {
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

fn link(parameters: &HashMap<String, String>) -> String {
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
                    page.content = render(&parameters, &value, &page.csrf)?;
                }
                Err((_, message)) => {
                    page.error = message.0["error"].as_str().unwrap_or_default().into()
                }
            }
        }
    }
    let html = Index {
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
    match view.as_str() {
        "home" => catalog::home_data(&products, &parameters).await,
        "tracks" | "favorites" => tracks_data(&products, &parameters).await,
        "albums" => catalog::albums_data(&products, &parameters).await,
        "album" => catalog::album_tracks_data(&products, &parameters).await,
        "artists" => catalog::artists_data(&products, &parameters).await,
        "artist" => catalog::artist_tracks_data(&products, &parameters).await,
        "genre" => catalog::genre_tracks_data(&products, &parameters).await,
        "playlists" => playlists::list_data(&products, &parameters).await,
        "playlist" => playlists::entries_data(&products, &parameters).await,
        "smart-playlists" => catalog::smart_playlists_data(&products, &parameters).await,
        "smart-playlist" => catalog::smart_tracks_data(&products, &parameters).await,
        _ => Err(bad_request("Unknown library view")),
    }
}
