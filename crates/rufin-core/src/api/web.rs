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
pub(super) struct Index<'a> {
    pub version: &'static str,
    pub page: &'a browser::Page,
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
        browser::link(&query)
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
        browser::link(self.parameters)
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
