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
struct Index {
    version: &'static str,
}

struct Row<'a> {
    data: &'a Value,
}

impl Row<'_> {
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

pub(super) fn page(
    headers: &hyper::HeaderMap,
    parameters: &HashMap<String, String>,
    value: Value,
) -> Result<Response<Body>, Error> {
    let mut response = if headers
        .get("HX-Request")
        .is_some_and(|value| value == "true")
    {
        let html = if let Some(sections) = value["sections"].as_array() {
            Home {
                sections: sections
                    .iter()
                    .map(|section| Section {
                        block: section["block"].as_str().unwrap_or_default(),
                        title: section["title"].as_str().unwrap_or_default(),
                        items: section["items"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|data| Row { data })
                            .collect(),
                    })
                    .collect(),
            }
            .render()
        } else {
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
                    .map(|data| Row { data })
                    .collect(),
                kind,
                offset: number(parameters, "offset", 0)?,
            }
            .render()
        }
        .map_err(internal)?;
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
        .route("/translations.js", get(translations))
        .route("/", get(index))
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

async fn index() -> Result<Response<Body>, Error> {
    let html = Index {
        version: env!("CARGO_PKG_VERSION"),
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
