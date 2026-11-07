//! Optional HTTP access to the same product owners used by native clients.

#[cfg(feature = "bindings")]
uniffi::setup_scaffolding!();

mod browser;
mod catalog;
mod connect;
mod controller;
mod equalizer;
mod media;
mod page;
mod pins;
mod playlists;
mod source;
pub use controller::{Controller, ControllerStatus, controller_addresses};

use axum::{
    Extension, Router,
    body::Body,
    extract::{Query, State},
    middleware::{self, Next},
    response::IntoResponse,
    routing::{delete, get, post},
};
use bytes::Bytes;
use futures_util::StreamExt;
use http_body_util::{BodyExt, StreamBody};
use hyper::{Request, Response, StatusCode, body::Frame};
use hyper_util::{rt::TokioIo, service::TowerToHyperService};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::task::JoinSet;

use rufin_core::runtime::ProductHandles;
use rufin_core::settings::{LibraryField, LibraryListKey, available_sort_fields};

type Error = (StatusCode, axum::Json<Value>);

fn artwork_previews<'a>(
    products: &ProductHandles,
    bindings: impl IntoIterator<Item = &'a Vec<u8>>,
) -> Vec<Option<String>> {
    use base64::Engine;
    let allow_external = products
        .settings
        .load()
        .application
        .external_metadata_enabled;
    bindings
        .into_iter()
        .map(|binding| {
            products
                .artwork
                .preview(artwork::ArtworkBinding::opaque(binding), allow_external)
                .map(|preview| {
                    format!(
                        "data:image/png;base64,{}",
                        base64::engine::general_purpose::STANDARD.encode(preview.png.as_slice())
                    )
                })
        })
        .collect()
}

/// The host binds the requested address and owns this future. Dropping it closes
/// the listener and its connections, including live event subscriptions.
pub async fn serve(
    listener: TcpListener,
    products: ProductHandles,
    token: String,
) -> std::io::Result<()> {
    if token.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "An API token is required",
        ));
    }
    let authorization = Arc::new(Authorization::new(&token));
    let router = routes()
        .merge(media::routes())
        .route_layer(middleware::from_fn_with_state(
            authorization.clone(),
            authorize,
        ))
        .merge(browser::routes())
        .layer(Extension(authorization))
        .layer(Extension(Pages::default()))
        .with_state(products);
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, peer) = accepted?;
                let service = TowerToHyperService::new(router.clone().layer(Extension(peer)));
                connections.spawn(async move {
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service).await;
                });
            }
            _ = connections.join_next(), if !connections.is_empty() => {}
        }
    }
}

fn routes() -> Router<ProductHandles> {
    Router::new()
        .route("/api", get(help))
        .route("/api/playback", get(playback_state))
        .route("/api/playback/events", get(playback_events))
        .route("/api/events", get(events))
        .route("/api/page", list_route("page", (48, 128)))
        .route(
            "/api/home",
            list_route("home", (0, 0)).post(catalog::refresh_home),
        )
        .route("/api/tracks", list_route("tracks", (100, 256)))
        .route("/api/albums", list_route("albums", (100, 128)))
        .route(
            "/api/albums/tracks",
            list_route("albums/tracks", (100, 128)),
        )
        .route("/api/artists", list_route("artists", (48, 128)))
        .route(
            "/api/artists/tracks",
            list_route("artists/tracks", (48, 256)),
        )
        .route("/api/genres/tracks", list_route("genres/tracks", (48, 256)))
        .route(
            "/api/playlists",
            list_route("playlists", (100, 128))
                .post(playlists::create)
                .patch(playlists::rename)
                .delete(playlists::delete),
        )
        .route(
            "/api/playlists/entries",
            list_route("playlists/entries", (100, 256))
                .post(playlists::add_entries)
                .delete(playlists::remove_entries)
                .patch(playlists::move_entry),
        )
        .route(
            "/api/smart-playlists",
            list_route("smart-playlists", (48, 64)),
        )
        .route(
            "/api/smart-playlists/tracks",
            list_route("smart-playlists/tracks", (48, 256)),
        )
        .route(
            "/api/queue",
            get(queue_state).post(queue_play).delete(queue_clear),
        )
        .route("/api/queue/album", post(queue_play))
        .route("/api/queue/playlist", post(queue_play))
        .route("/api/queue/artist", post(queue_play))
        .route("/api/queue/genre", post(queue_play))
        .route("/api/queue/smart-playlist", post(queue_play))
        .route("/api/queue/source", post(queue_play))
        .route("/api/queue/activate", post(queue_activate))
        .route("/api/queue/item", delete(queue_remove).post(queue_remove))
        .route("/api/queue/clear", post(queue_clear))
        .route("/api/playback/{action}", post(playback_command))
        .merge(source::routes())
        .merge(playlists::routes())
        .merge(pins::routes())
        .merge(catalog::routes())
        .merge(connect::routes())
        .merge(equalizer::routes())
}

async fn help() -> Result<Response<Body>, Error> {
    let mut help: Value =
        serde_json::from_str(include_str!("help.json")).expect("bundled API help");
    for (choice, key) in [
        ("track_sort", LibraryListKey::Tracks),
        ("album_sort", LibraryListKey::Albums),
        ("playlist_sort", LibraryListKey::Playlists),
        ("artist_sort", LibraryListKey::Artists),
    ] {
        help["choices"][choice] = sort_options(key)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
    }
    Ok(json_response(StatusCode::OK, help))
}

async fn playback_state(State(products): State<ProductHandles>) -> Result<Response<Body>, Error> {
    let products = &products;
    Ok(json_response(
        StatusCode::OK,
        playback_json(
            products,
            products.playback.updates.current().as_deref(),
            true,
        ),
    ))
}

async fn playback_events(State(products): State<ProductHandles>) -> Result<Response<Body>, Error> {
    let subscription = products.playback.updates.subscribe();
    let stream = futures_util::stream::unfold(
        (subscription, None, products),
        |(mut subscription, previous, products)| async move {
            let publication = subscription.recv().await.ok()?;
            let current = publication
                .as_ref()
                .and_then(|p| p.view.transport.current.clone());
            let value = publication_json(&products, publication.as_ref(), current != previous);
            let frame = Frame::data(Bytes::from(format!("event: playback\ndata: {value}\n\n")));
            Some((
                Ok::<_, Infallible>(frame),
                (subscription, current, products),
            ))
        },
    );
    Ok(event_response(Body::new(StreamBody::new(stream))))
}

async fn queue_state(
    State(products): State<ProductHandles>,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let limit = number(&parameters, "limit", library::QUEUE_CONTEXT_LIMIT)?
        .min(library::QUEUE_CONTEXT_LIMIT);
    let state = products.playback.updates.current();
    Ok(json_response(
        StatusCode::OK,
        queue_json(products, state.as_deref(), limit),
    ))
}

fn queue_json(
    products: &ProductHandles,
    state: Option<&playback::PlaybackView>,
    limit: usize,
) -> Value {
    let scope = page::PageToken::selected(products);
    state.map(|state| {
        let current = state.queue_window.iter().position(|row| Some(row.position) == state.queue.current_index).unwrap_or(0);
        let start = current.saturating_sub(limit / 2).min(state.queue_window.len().saturating_sub(limit));
        let rows = state.queue_window.iter().skip(start).take(limit);
        let offset = rows.clone().next().map_or(0, |row| row.position);
        json!({
            "total": state.queue.total, "current_index": state.queue.current_index, "loading": state.queue_loading,
            "offset": offset, "window": rows.map(|row| json!({"position": row.position, "track": track_json(products, &scope, &row.track, false)})).collect::<Vec<_>>()
        })
    }).unwrap_or(json!({"total": 0, "current_index": null, "loading": false, "offset": 0, "window": []}))
}

/// A list endpoint: the page its parameters name, as HTML for the web page or as JSON.
fn list_route(
    list: &'static str,
    limits: (usize, usize),
) -> axum::routing::MethodRouter<ProductHandles> {
    get(
        move |State(products): State<ProductHandles>,
              Extension(pages): Extension<Pages>,
              headers: hyper::HeaderMap,
              Query(parameters): Query<HashMap<String, String>>| async move {
            list_page(&products, &pages, &headers, list, &parameters, limits).await
        },
    )
}

async fn list_page(
    products: &ProductHandles,
    pages: &Pages,
    headers: &hyper::HeaderMap,
    list: &str,
    parameters: &HashMap<String, String>,
    (default, cap): (usize, usize),
) -> Result<Response<Body>, Error> {
    let token = page::token(products, list, parameters)?;
    let limit = number(parameters, "limit", default)?.min(cap);
    let read = pages.read(headers, &token);
    let mut opened = page::open(products, token, number(parameters, "offset", 0)?, limit).await?;
    if let Some(read) = read {
        pages.show(read, &opened);
        // A stream that followed this page all along carries what committed meanwhile.
        opened.missed = opened.missed.filter(|_| !read.2);
    }
    if opened.token.route == rufin_core::route::Route::Home {
        let block = parameters
            .get("block")
            .map(|block| serde_json::from_value(Value::String(block.clone())))
            .transpose()
            .map_err(bad_request)?;
        let sections = page::home_sections(&opened.view, block);
        return browser::page(
            headers,
            || page::home_json(products, &opened.token, &sections),
            |csrf| browser::home(products, &opened, &sections, csrf),
        );
    }
    let total = boolean(parameters, "total")?;
    browser::page(
        headers,
        || page::json(products, &opened, limit, total),
        |csrf| browser::library(products, &opened, total, csrf),
    )
}

async fn queue_play(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    // The collection routes name a page by the list parameters of its endpoint.
    let list = match request.uri().path().strip_prefix("/api/queue/") {
        Some("album") => Some("albums/tracks"),
        Some("artist") => Some("artists/tracks"),
        Some("genre") => Some("genres/tracks"),
        Some("playlist") => Some("playlists/entries"),
        Some("smart-playlist") => Some("smart-playlists/tracks"),
        Some("source") => Some("tracks"),
        _ => None,
    };
    #[derive(Deserialize)]
    struct Input {
        mode: String,
        before: Option<usize>,
        after: Option<usize>,
        uris: Option<Vec<String>>,
        page: Option<page::PageToken>,
        ranges: Option<Vec<std::ops::Range<usize>>>,
        index: Option<usize>,
        anchor_uri: Option<String>,
        anchor_entry: Option<library::PlaylistEntryKey>,
        #[serde(flatten)]
        parameters: HashMap<String, Value>,
    }
    let input: Input = body(request).await?;
    use rufin_core::playback::QueuePlacement;
    let placement = match input.mode.as_str() {
        "replace" => QueuePlacement::Replace { anchor_index: 0 },
        "append" | "insert" => QueuePlacement::End,
        "next" => QueuePlacement::AfterCurrent,
        _ => {
            return Err(bad_request(
                "Queue mode must be replace, append, next or insert",
            ));
        }
    };
    let request = if let Some(uris) = input
        .uris
        .filter(|_| list.is_none() && input.page.is_none())
    {
        let keys = products
            .library
            .remember_media_uris(&uris)
            .await
            .map_err(internal)?;
        rufin_core::playback::PlayRequest::tracks(
            library::MemberSelection::Keys {
                source: None,
                keys: keys.into(),
            },
            placement,
        )
    } else {
        let token = match (input.page, list) {
            (Some(token), _) => token,
            (None, Some(list)) => {
                let parameters = input
                    .parameters
                    .iter()
                    .filter(|(_, value)| !value.is_null())
                    .map(|(key, value)| {
                        let text = value
                            .as_str()
                            .map_or_else(|| value.to_string(), str::to_owned);
                        (key.clone(), text)
                    })
                    .collect();
                page::token(products, list, &parameters)?
            }
            (None, None) => return Err(bad_request("uris or page is required")),
        };
        let gone = || bad_request("Nothing to play here");
        let anchor = match (&input.anchor_uri, input.anchor_entry, &token.route) {
            (None, None, _) if input.index.is_some() => {
                return Err(bad_request(
                    "A row play names its anchor_uri or anchor_entry",
                ));
            }
            (None, None, _) => None,
            (Some(uri), entry, _) => products
                .library
                .track_keys(std::slice::from_ref(uri), &library::ReadCancellation::new())
                .await
                .map_err(internal)?
                .first()
                .map(|key| (*key, entry)),
            (None, Some(entry), rufin_core::route::Route::PlaylistDetail(playlist)) => products
                .library
                .playlist_entry_track(*playlist, entry)
                .map(|key| (key, Some(entry))),
            (None, Some(_), _) => return Err(gone()),
        };
        match (anchor, input.ranges) {
            // Selected rows play in the page's order.
            (_, Some(ranges)) => page::selection(products, token, Some(ranges))
                .await?
                .map(|selection| rufin_core::playback::PlayRequest::tracks(selection, placement)),
            // A page without a track plays whole, as its header and the card that opens it do.
            (None, None) => Some(page::play(products, &token, placement).await?),
            (Some((key, entry)), None) => page::open(products, token, 0, 0).await?.view.play_track(
                key,
                entry,
                input.index.unwrap_or(0),
                placement,
                false,
            ),
        }
        .ok_or_else(gone)?
    };
    let queue = products.playback.clone();
    tokio::task::spawn_blocking(move || {
        if input.mode == "insert" {
            if let Some(selection) = request.selection() {
                queue.insert(selection, queue_drop_target(input.before, input.after));
            }
            return;
        }
        queue.play_selection(request)
    })
    .await
    .map_err(internal)?;
    Ok(accepted())
}

fn queue_drop_target(
    before: Option<usize>,
    after: Option<usize>,
) -> rufin_core::playback::QueueReorderTarget {
    match (before, after) {
        (Some(id), _) => rufin_core::playback::QueueReorderTarget::Before(id),
        (_, Some(id)) => rufin_core::playback::QueueReorderTarget::After(id),
        _ => rufin_core::playback::QueueReorderTarget::End,
    }
}

async fn queue_clear(State(products): State<ProductHandles>) -> Result<Response<Body>, Error> {
    let products = &products;
    let queue = products.playback.clone();
    let include_current = products
        .settings
        .load()
        .player_display
        .clear_queue_includes_current;
    tokio::task::spawn_blocking(move || queue.clear(include_current))
        .await
        .map_err(internal)?;
    Ok(accepted())
}

async fn queue_activate(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let input: QueuePosition = body(request).await?;
    let queue = products.playback.clone();
    tokio::task::spawn_blocking(move || queue.activate(input.position))
        .await
        .map_err(internal)?;
    Ok(accepted())
}

async fn queue_remove(
    State(products): State<ProductHandles>,
    Query(parameters): Query<HashMap<String, String>>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let parameters = &parameters;
    let position = if request.method() == hyper::Method::POST {
        body::<QueuePosition>(request).await?.position
    } else {
        required(&parameters, "position")?
            .parse::<usize>()
            .map_err(bad_request)?
    };
    let queue = products.playback.clone();
    tokio::task::spawn_blocking(move || queue.remove(position))
        .await
        .map_err(internal)?;
    Ok(accepted())
}

async fn playback_command(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let path = request.uri().path().to_owned();
    let transport = products.playback.clone();
    let action = path.trim_start_matches("/api/playback/").to_owned();
    let value: Value = if matches!(
        action.as_str(),
        "play" | "pause" | "stop" | "next" | "previous"
    ) {
        Value::Null
    } else {
        body(request).await?
    };
    tokio::task::spawn_blocking(move || -> Result<(), Error> {
        match action.as_str() {
            "play" => transport.play(),
            "pause" => transport.pause(),
            "stop" => transport.stop(),
            "next" => transport.next(),
            "previous" => transport.previous(),
            "auto-dj" => transport.toggle_auto_dj(),
            "seek" => transport.seek_millis(
                value
                    .get("position_ms")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| bad_request("position_ms must be a nonnegative integer"))?,
            ),
            "volume" => {
                let volume = value
                    .get("volume")
                    .and_then(Value::as_f64)
                    .ok_or_else(|| bad_request("volume must be a number"))?;
                if value
                    .get("persist")
                    .and_then(Value::as_bool)
                    .unwrap_or(true)
                {
                    transport.persist_volume(volume);
                } else {
                    transport.set_volume(volume);
                }
            }
            "mute" => transport.set_muted(
                value
                    .get("muted")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| bad_request("muted must be true or false"))?,
            ),
            "shuffle" => transport.set_shuffle(
                value
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| bad_request("enabled must be true or false"))?,
            ),
            "repeat" => transport.set_repeat(match value.get("mode").and_then(Value::as_str) {
                Some("off") => playback::QueueRepeatMode::Off,
                Some("one") => playback::QueueRepeatMode::One,
                Some("all") => playback::QueueRepeatMode::All,
                _ => return Err(bad_request("Repeat mode must be off, one or all")),
            }),
            _ => return Err(error(StatusCode::NOT_FOUND, "Route not found")),
        }
        Ok(())
    })
    .await
    .map_err(internal)??;
    Ok(accepted())
}

#[derive(Deserialize)]
struct Identifier {
    id: String,
}

#[derive(Deserialize)]
struct QueuePosition {
    position: usize,
}

async fn body<T: serde::de::DeserializeOwned>(mut request: Request<Body>) -> Result<T, Error> {
    if let Some(SubmittedForm(fields)) = request.extensions_mut().remove::<SubmittedForm>() {
        let mut value = match fields.get("payload") {
            Some(payload) => {
                match serde_json::from_str::<serde_json::Map<String, Value>>(payload) {
                    Ok(value) => Value::Object(value),
                    Err(error) => return Err(bad_request(error)),
                }
            }
            None => json!({}),
        };
        for (name, text) in &fields {
            if matches!(name.as_str(), "csrf" | "return" | "payload") {
                continue;
            }
            value[name] = if matches!(
                name.as_str(),
                "volume" | "position_ms" | "position" | "before" | "after"
            ) {
                match serde_json::from_str(text) {
                    Ok(value) => value,
                    Err(error) => return Err(bad_request(error)),
                }
            } else if name == "library" && text.is_empty() {
                Value::Null
            } else {
                Value::String(text.clone())
            };
        }
        return serde_json::from_value(value).map_err(bad_request);
    }
    let bytes = request
        .into_body()
        .collect()
        .await
        .map_err(bad_request)?
        .to_bytes();
    serde_json::from_slice(&bytes).map_err(bad_request)
}

fn required<'a>(parameters: &'a HashMap<String, String>, key: &str) -> Result<&'a str, Error> {
    parameters
        .get(key)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| bad_request(format!("{key} is required")))
}

fn number(parameters: &HashMap<String, String>, key: &str, default: usize) -> Result<usize, Error> {
    parameters
        .get(key)
        .map(|value| value.parse().map_err(bad_request))
        .unwrap_or(Ok(default))
}

fn boolean(parameters: &HashMap<String, String>, key: &str) -> Result<bool, Error> {
    parameters
        .get(key)
        .map(|value| value.parse().map_err(bad_request))
        .unwrap_or(Ok(false))
}

/// The `sort` names help.json documents, and the field each one names.
const SORTS: [(&str, LibraryField); 18] = [
    ("position", LibraryField::RowIndex),
    ("title", LibraryField::Title),
    ("track_number", LibraryField::TrackNumber),
    ("artist", LibraryField::Artist),
    ("album_artist", LibraryField::AlbumArtist),
    ("album", LibraryField::Album),
    ("year", LibraryField::Year),
    ("release_date", LibraryField::ReleaseDate),
    ("date_added", LibraryField::DateAdded),
    ("last_played", LibraryField::LastPlayed),
    ("play_count", LibraryField::PlayCount),
    ("rating", LibraryField::UserRating),
    ("genre", LibraryField::Genre),
    ("bpm", LibraryField::Bpm),
    ("duration", LibraryField::Duration),
    ("favorite", LibraryField::Favorite),
    ("track_count", LibraryField::SongCount),
    ("album_count", LibraryField::AlbumCount),
];

/// The `sort` names a list takes and their titles, as GTK titles them.
fn sort_options(key: LibraryListKey) -> Vec<(&'static str, &'static str)> {
    available_sort_fields(key)
        .iter()
        .filter_map(|field| {
            let (name, _) = SORTS.iter().find(|(_, sorted)| sorted == field)?;
            Some((
                *name,
                rufin_core::settings::presentation::library_sort_title(key, *field),
            ))
        })
        .collect()
}

/// Reads `sort` as a documented name for a field `key` sorts by. `position` and `definition`
/// name a list's own order: a playlist's, or a smart playlist's saved one.
fn sort(
    parameters: &HashMap<String, String>,
    key: LibraryListKey,
) -> Result<Option<LibraryField>, Error> {
    let Some(name) = parameters.get("sort") else {
        return Ok(None);
    };
    if matches!(name.as_str(), "position" | "definition") {
        return Ok(Some(LibraryField::RowIndex));
    }
    SORTS
        .iter()
        .find(|(api, field)| api == name && available_sort_fields(key).contains(field))
        .map(|(_, field)| Some(*field))
        .ok_or_else(|| bad_request("Unknown sort"))
}

/// The documented name of the order a list reads in.
fn sort_name(key: LibraryListKey, sort: LibraryField) -> &'static str {
    if key == LibraryListKey::SmartPlaylistTracks && sort == LibraryField::RowIndex {
        return "definition";
    }
    SORTS
        .iter()
        .find(|(_, field)| *field == sort)
        .map_or("title", |(name, _)| name)
}

/// A queued or playing track, whose links open pages of the selected library `scope`.
fn track_json(
    products: &ProductHandles,
    scope: &page::PageToken,
    track: &library::TrackView,
    include_preview: bool,
) -> Value {
    let mut row = json!({"uri":track.media_uri,"source_id":track.source_id,"title":track.metadata.title,"artist":track.metadata.artist,"album":track.metadata.album,"duration_ms":track.metadata.duration_millis,"favorite":track.state.favorite,"media":media::media_links(scope, &track.links, &track.metadata.album)});
    if include_preview {
        row["artwork_previews"] = json!(artwork_previews(products, track.artwork_binding.iter()));
    }
    row
}
fn playback_json(
    products: &ProductHandles,
    state: Option<&playback::PlaybackView>,
    include_preview: bool,
) -> Value {
    let Some(state) = state else {
        return Value::Null;
    };
    let status = match state.transport.effective_state() {
        playback::TransportStatus::Stopped => "stopped",
        playback::TransportStatus::Resolving => "resolving",
        playback::TransportStatus::Buffering => "buffering",
        playback::TransportStatus::Playing => "playing",
        playback::TransportStatus::Paused => "paused",
        playback::TransportStatus::Failed => "failed",
    };
    json!({
        "state":status,"position_ms":state.transport.position_millis,"duration_ms":state.transport.duration_millis,"can_seek":state.transport.can_seek,
        "current":state.transport.current.as_ref().map(|current| json!({"position":state.queue.current_index,"media_uri":current.id.media_uri,"run":current.id.run.map(playback::RunId::get),"track":track_json(products, &page::PageToken::selected(products), &current.row.track, include_preview)})),
        "volume":state.controls.volume,"muted":state.controls.muted,"shuffle":state.controls.shuffle_enabled,"auto_dj":state.controls.auto_dj_enabled,
        "repeat":match state.controls.repeat_mode {playback::QueueRepeatMode::Off=>"off",playback::QueueRepeatMode::One=>"one",playback::QueueRepeatMode::All=>"all"},
        "queue_total":state.queue.total,"queue_loading":state.queue_loading,"refilling":state.transport.refilling(),"error":state.transport.error,
        "queue_revision":state.queue.revision
    })
}

fn accepted() -> Response<Body> {
    json_response(StatusCode::ACCEPTED, json!({"accepted":true}))
}
async fn completion<T>(receiver: async_channel::Receiver<Result<T, String>>) -> Result<T, Error> {
    receiver
        .recv()
        .await
        .map_err(internal)?
        .map_err(bad_request)
}
fn key<T: serde::de::DeserializeOwned>(
    parameters: &HashMap<String, String>,
    name: &str,
) -> Result<T, Error> {
    serde_json::from_str(required(parameters, name)?).map_err(bad_request)
}
fn event_response(body: Body) -> Response<Body> {
    Response::builder()
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-store")
        .body(body)
        .expect("static response headers")
}
fn bad_request(error: impl std::fmt::Display) -> Error {
    self::error(StatusCode::BAD_REQUEST, error)
}
fn internal(error: impl std::fmt::Display) -> Error {
    self::error(StatusCode::INTERNAL_SERVER_ERROR, error)
}
fn json_response(status: StatusCode, value: Value) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json; charset=utf-8")
        .header("cache-control", "no-store")
        .body(Body::from(value.to_string()))
        .expect("static response headers")
}

fn error(status: StatusCode, message: impl std::fmt::Display) -> Error {
    (status, axum::Json(json!({"error":message.to_string()})))
}
struct Authorization {
    token: String,
    failures: Mutex<VecDeque<(IpAddr, Instant, u8)>>,
    browser_key: [u8; 32],
}

#[cfg(test)]
mod authentication_tests {
    use super::*;

    #[test]
    fn failed_attempts_expire_and_are_separate_for_each_address() {
        let authorization = Authorization::new("valid");
        let address = "127.0.0.1".parse().unwrap();
        let now = Instant::now();
        for _ in 0..10 {
            assert_eq!(
                authorization
                    .check(address, None, now)
                    .unwrap_err()
                    .status(),
                StatusCode::UNAUTHORIZED
            );
        }
        let rejected = authorization
            .check(address, Some("Bearer valid"), now)
            .unwrap_err();
        assert_eq!(rejected.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(rejected.headers()["retry-after"], "60");
        assert!(
            authorization
                .check("::1".parse().unwrap(), Some("Bearer valid"), now)
                .is_ok()
        );
        let rejected = authorization
            .check(address, None, now + Duration::from_secs(59))
            .unwrap_err();
        assert_eq!(rejected.headers()["retry-after"], "1");
        assert!(
            authorization
                .check(address, Some("Bearer valid"), now + Duration::from_secs(60))
                .is_ok()
        );
    }

    #[test]
    fn successful_authentication_clears_previous_failures() {
        let authorization = Authorization::new("valid");
        let address = "127.0.0.1".parse().unwrap();
        let now = Instant::now();
        for _ in 0..2 {
            for _ in 0..9 {
                assert_eq!(
                    authorization
                        .check(address, None, now)
                        .unwrap_err()
                        .status(),
                    StatusCode::UNAUTHORIZED
                );
            }
            assert!(
                authorization
                    .check(address, Some("Bearer valid"), now)
                    .is_ok()
            );
        }
    }
}

impl Authorization {
    fn new(token: &str) -> Self {
        Self {
            token: format!("Bearer {token}"),
            failures: Mutex::new(VecDeque::new()),
            browser_key: blake3::derive_key("Rufin browser authentication v1", token.as_bytes()),
        }
    }

    fn check(
        &self,
        address: IpAddr,
        token: Option<&str>,
        now: Instant,
    ) -> Result<(), Box<Response<Body>>> {
        let window = Duration::from_secs(60);
        let mut failures = self.failures.lock().expect("authentication failures");
        failures.retain(|(_, start, _)| now.duration_since(*start) < window);
        let previous = failures.iter().position(|(ip, _, _)| *ip == address);
        if let Some(index) = previous {
            let (_, start, count) = failures[index];
            if count >= 10 {
                let seconds = (window - now.duration_since(start)).as_secs().max(1);
                let mut response = error(
                    StatusCode::TOO_MANY_REQUESTS,
                    "Too many failed authentication attempts. Try again shortly.",
                )
                .into_response();
                response
                    .headers_mut()
                    .insert(hyper::header::RETRY_AFTER, seconds.into());
                return Err(Box::new(response));
            }
        }
        if token.is_some_and(|token| {
            constant_time_eq::constant_time_eq(token.as_bytes(), self.token.as_bytes())
        }) {
            if let Some(index) = previous {
                failures.remove(index);
            }
            return Ok(());
        }
        if let Some(index) = previous {
            let (_, start, count) = &mut failures[index];
            *count += 1;
            if *count == 10 {
                *start = now;
            }
        } else {
            // Keep failed requests from growing the address history without bound.
            if failures.len() == 1024 {
                failures.pop_front();
            }
            failures.push_back((address, now, 1));
        }
        Err(Box::new(
            error(StatusCode::UNAUTHORIZED, "A valid Bearer token is required").into_response(),
        ))
    }
}

#[derive(Clone)]
struct SubmittedForm(HashMap<String, String>);

async fn authorize(
    State(authorization): State<Arc<Authorization>>,
    Extension(peer): Extension<SocketAddr>,
    mut request: Request<Body>,
    next: Next,
) -> Response<Body> {
    let is_form = request
        .headers()
        .get(hyper::header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| h.split(';').next() == Some("application/x-www-form-urlencoded"));
    let form = if is_form {
        let body = std::mem::replace(request.body_mut(), Body::empty());
        let fields = match axum::body::to_bytes(body, usize::MAX).await {
            Ok(bytes) => url::form_urlencoded::parse(&bytes)
                .into_owned()
                .collect::<HashMap<String, String>>(),
            Err(error) => return bad_request(error).into_response(),
        };
        Some(SubmittedForm(fields))
    } else {
        None
    };
    let token = request
        .headers()
        .get(hyper::header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok());
    if let Some(csrf) = token
        .is_none()
        .then(|| session(&authorization, request.headers()))
        .flatten()
    {
        if !matches!(*request.method(), hyper::Method::GET | hyper::Method::HEAD) {
            let submitted = form
                .as_ref()
                .and_then(|form| form.0.get("csrf").map(String::as_str))
                .or_else(|| {
                    request
                        .headers()
                        .get("x-rufin-csrf")
                        .and_then(|v| v.to_str().ok())
                });
            if submitted != Some(csrf.as_str()) {
                return error(StatusCode::FORBIDDEN, "Invalid form token").into_response();
            }
        }
    } else if let Err(response) = authorization.check(peer.ip(), token, Instant::now()) {
        return *response;
    }
    let target = form.as_ref().map(|form| {
        form.0
            .get("return")
            .filter(|v| v.starts_with("/?") && !v.contains(['\r', '\n']))
            .cloned()
            .unwrap_or_else(|| "/".into())
    });
    if let Some(form) = form {
        request.extensions_mut().insert(form);
    }
    let response = next.run(request).await;
    if response.status().is_success() {
        if let Some(target) = target {
            return axum::response::Redirect::to(&target).into_response();
        }
    }
    response
}

fn publication_json(
    products: &ProductHandles,
    publication: Option<&playback::PlaybackProjection>,
    include_preview: bool,
) -> Value {
    let mut value = playback_json(products, publication.map(|p| &p.view), include_preview);
    if let Some(publication) = publication {
        value["notices"] = publication.notices.iter().map(|notice| match notice {
                        playback::PlaybackNotice::RunStarted(run) => json!({"type":"run_started","run":run.get()}),
                        playback::PlaybackNotice::PositionDiscontinuity(position) => json!({"type":"position_discontinuity","run":position.run.get(),"position_ms":position.position_millis}),
                        playback::PlaybackNotice::OperationFailed(error) => json!({"type":"operation_failed","error":error}),
                        }).collect::<Vec<_>>().into();
    }
    value
}

/// The page each live events stream shows, for the refresh verdicts its frames carry, after
/// the page reads it got. A page read sent with `X-Rufin-Events: <stream>` becomes that
/// stream's page unless the stream got a later read.
#[derive(Clone, Default)]
struct Pages(Arc<Mutex<HashMap<u64, (u64, Option<Shown>)>>>);

type Shown = (page::PageToken, Arc<rufin_core::view::View>);

impl Pages {
    /// Counts a read of `token` for the stream the headers name: the stream, the read's place
    /// and whether the stream follows that page already.
    fn read(
        &self,
        headers: &hyper::HeaderMap,
        token: &page::PageToken,
    ) -> Option<(u64, u64, bool)> {
        let stream = headers.get("x-rufin-events")?.to_str().ok()?.parse().ok()?;
        let mut pages = self.0.lock().expect("event pages");
        let (reads, shown) = pages.get_mut(&stream)?;
        *reads += 1;
        Some((
            stream,
            *reads,
            shown.as_ref().is_some_and(|(page, _)| page == token),
        ))
    }

    fn show(&self, (stream, place, _): (u64, u64, bool), opened: &page::Opened) {
        let mut pages = self.0.lock().expect("event pages");
        if let Some((reads, shown)) = pages.get_mut(&stream)
            && *reads == place
        {
            *shown = Some((opened.token.clone(), Arc::clone(&opened.view)));
        }
    }

    fn page(&self, stream: u64) -> Option<Arc<rufin_core::view::View>> {
        let pages = self.0.lock().expect("event pages");
        pages
            .get(&stream)
            .and_then(|(_, shown)| Some(Arc::clone(&shown.as_ref()?.1)))
    }
}

/// An events stream's place in [`Pages`], which it leaves when the stream closes.
struct Stream {
    id: u64,
    pages: Pages,
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.pages.0.lock().expect("event pages").remove(&self.id);
    }
}

async fn events(
    State(products): State<ProductHandles>,
    Extension(pages): Extension<Pages>,
    Query(parameters): Query<HashMap<String, String>>,
) -> Response<Body> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // Changes from before the page opens on, so none committed while it opens is lost.
    let catalog = products.source.changes();
    // The page the client shows when it connects; one that does not open has nothing to follow.
    let page = match page::token(&products, "page", &parameters) {
        Ok(token) => page::open(&products, token, 0, 0).await.ok(),
        Err(_) => None,
    };
    let page = page.map(|opened| (opened.token, opened.view));
    pages.0.lock().expect("event pages").insert(id, (0, page));
    let stream = Stream { id, pages };
    let receivers = (
        products.playback.updates.subscribe(),
        products.source.operation(),
        products.lyrics.current(),
        products.appearance.subscribe(),
        products.settings.changes(),
        catalog,
        products.clone(),
        true,
        None,
        rufin_core::settings::SidebarSettings::default(),
        stream,
    );
    let stream = futures_util::stream::unfold(
        receivers,
        |(
            mut playback,
            mut source,
            mut lyrics,
            mut appearance,
            mut sidebar,
            catalog,
            products,
            first,
            mut previous,
            mut pins,
            stream,
        )| async move {
            let values = if first {
                let publication = playback.recv().await.ok()?;
                let mut value = playback_event_json(&products, publication, &mut previous, true);
                value["stream"] = json!(stream.id);
                value["source"] = json!(*source.borrow_and_update());
                value["lyrics"] = media::lyrics_json(
                    &lyrics.borrow_and_update(),
                    &products.settings.load().lyrics,
                );
                value["appearance"] = json!(*appearance.borrow_and_update());
                pins = sidebar.borrow_and_update().ui.sidebar.clone();
                value["pins_changed"] = json!(true);
                vec![value]
            } else {
                tokio::select! {
                    publication = playback.recv() => vec![playback_event_json(&products, publication.ok()?, &mut previous, false)],
                    changed = source.changed() => { changed.ok()?; vec![json!({"source":*source.borrow_and_update()})] },
                    changed = lyrics.changed() => { changed.ok()?; vec![json!({"lyrics":media::lyrics_json(&lyrics.borrow_and_update(), &products.settings.load().lyrics)})] },
                    changed = appearance.changed() => { changed.ok()?; vec![json!({"appearance":*appearance.borrow_and_update()})] },
                    changed = sidebar.changed() => {
                        changed.ok()?;
                        let now = sidebar.borrow_and_update().ui.sidebar.clone();
                        if now == pins { Vec::new() } else { pins = now; vec![json!({"pins_changed":true})] }
                    },
                    changed = catalog.next() => changed_json(&products, stream.pages.page(stream.id).as_deref(), &changed?),
                }
            };
            Some((
                values,
                (
                    playback, source, lyrics, appearance, sidebar, catalog, products, false,
                    previous, pins, stream,
                ),
            ))
        },
    )
    .flat_map(|values| {
        futures_util::stream::iter(values.into_iter().map(|value| {
            Ok::<_, Infallible>(Frame::data(Bytes::from(format!(
                "event: update\ndata: {value}\n\n"
            ))))
        }))
    });
    event_response(Body::new(StreamBody::new(stream)))
}

/// The update frames of one catalog change. The first carries the flags and what the change
/// shows on the stream's page ([`show_changed`]); then the first changed playlist, or null when
/// any may have changed. Each further playlist gets a frame of its own.
fn changed_json(
    products: &ProductHandles,
    shown: Option<&rufin_core::view::View>,
    changed: &rufin_core::source::feed::Changed,
) -> Vec<Value> {
    let mut first = json!({
        "catalog_acquired": changed.all || !changed.sources.is_empty(),
        "pins_changed": rufin_core::view::pins_refresh(&products.source, changed),
        "activity_changed": changed.listened,
    });
    show_changed(products, &mut first, shown, changed);
    if changed.playlist_lists {
        first["playlists"] = Value::Null;
        return vec![first];
    }
    let mut playlists = changed.playlists.iter();
    if let Some(playlist) = playlists.next() {
        first["playlists"] = json!(playlist);
    }
    std::iter::once(first)
        .chain(playlists.map(|playlist| {
            json!({"catalog_acquired":false,"pins_changed":false,"activity_changed":false,"playlists":playlist})
        }))
        .collect()
}

/// What `changed` shows on `shown`, the page a client shows: whether it reads the page again
/// (`page_refresh`: reread, or reopen in the selected source and music folder) and the
/// favorites and ratings that changed, which the rows that show them patch in.
fn show_changed(
    products: &ProductHandles,
    value: &mut Value,
    shown: Option<&rufin_core::view::View>,
    changed: &rufin_core::source::feed::Changed,
) {
    use rufin_core::view::Refresh;
    let current = products.source.selected_library();
    match shown.map(|view| view.refresh(current.as_ref(), changed)) {
        Some(Refresh::Reread) => value["page_refresh"] = json!("reread"),
        Some(Refresh::Reopen) => value["page_refresh"] = json!("reopen"),
        Some(Refresh::Nothing) | None => {}
    }
    if !changed.items.is_empty() {
        value["states"] = changed
            .items
            .iter()
            .map(|state| {
                let (library::FavoriteTarget::Track(uri)
                | library::FavoriteTarget::Album(uri)
                | library::FavoriteTarget::Artist(uri)) = &state.target;
                json!({"uri":uri,"favorite":state.favorite,"rating":state.rating})
            })
            .collect();
    }
}

fn playback_event_json(
    products: &ProductHandles,
    publication: Option<playback::PlaybackProjection>,
    previous: &mut Option<playback::PlaybackView>,
    first: bool,
) -> Value {
    let current = publication.as_ref().map(|publication| &publication.view);
    let changed = match (previous.as_ref(), current) {
        (Some(previous), Some(current)) => {
            previous.queue != current.queue
                || previous.queue_loading != current.queue_loading
                || previous.queue_window != current.queue_window
        }
        (None, None) => false,
        _ => true,
    };
    let preview_changed = first
        || previous
            .as_ref()
            .and_then(|view| view.transport.current.as_ref())
            != current.and_then(|view| view.transport.current.as_ref());
    let mut value =
        json!({"playback": publication_json(products, publication.as_ref(), preview_changed)});
    if first || changed {
        value["queue"] = queue_json(products, current, library::QUEUE_CONTEXT_LIMIT);
    }
    *previous = publication.map(|publication| publication.view);
    value
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

fn session(auth: &Authorization, headers: &hyper::HeaderMap) -> Option<String> {
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

const BROWSER_SESSION_AGE: u64 = 365 * 24 * 60 * 60;

async fn login(
    Extension(auth): Extension<Arc<Authorization>>,
    Extension(peer): Extension<SocketAddr>,
    headers: hyper::HeaderMap,
    axum::Form(form): axum::Form<HashMap<String, String>>,
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
            return axum::response::Redirect::to("/?login=failed").into_response();
        }
        return *response;
    }
    let csrf = match session(&auth, &headers) {
        Some(csrf) => csrf,
        None => match random_secret() {
            Ok(nonce) => nonce,
            Err(error) => return error.into_response(),
        },
    };
    let signature = blake3::keyed_hash(&auth.browser_key, csrf.as_bytes());
    let value = format!("{csrf}.{}", signature.to_hex());
    let mut response = if headers
        .get(hyper::header::ACCEPT)
        .is_some_and(|v| v == "application/json")
    {
        json_response(StatusCode::OK, json!({"csrf":csrf}))
    } else {
        axum::response::Redirect::to("/").into_response()
    };
    set_cookie(&mut response, &headers, &value, Some(BROWSER_SESSION_AGE));
    response
}

async fn logout(
    Extension(auth): Extension<Arc<Authorization>>,
    headers: hyper::HeaderMap,
    axum::Form(form): axum::Form<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    form_session(&auth, &headers, &form)?;
    let mut response = axum::response::Redirect::to("/").into_response();
    set_cookie(&mut response, &headers, "", Some(0));
    Ok(response)
}
