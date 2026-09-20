//! Optional HTTP access to the same product owners used by native clients.

mod catalog;
mod connect;
mod controller;
mod media;
mod pins;
mod playlists;
mod source;
mod web;
pub use controller::{Controller, ControllerSettings, ControllerStatus};

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    Extension, Router,
    body::Body,
    extract::{Query, State},
    middleware::{self, Next},
    response::IntoResponse,
    routing::{delete, get, post},
};
use bytes::Bytes;
use http_body_util::{BodyExt, StreamBody};
use hyper::{Request, Response, StatusCode, body::Frame};
use hyper_util::{rt::TokioIo, service::TowerToHyperService};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinSet;

use crate::runtime::ProductHandles;

type Error = (StatusCode, axum::Json<Value>);

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
        .merge(web::routes())
        .layer(Extension(authorization))
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

pub(super) fn routes() -> Router<ProductHandles> {
    Router::new()
        .route("/api", get(help))
        .route("/api/playback", get(playback_state))
        .route("/api/playback/events", get(playback_events))
        .route("/api/events", get(events))
        .route("/api/tracks", get(tracks))
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
}

async fn help() -> Result<Response<Body>, Error> {
    Ok(json_response(
        StatusCode::OK,
        serde_json::from_str(include_str!("api/help.json")).expect("bundled API help"),
    ))
}

async fn playback_state(State(products): State<ProductHandles>) -> Result<Response<Body>, Error> {
    let products = &products;
    Ok(json_response(
        StatusCode::OK,
        playback_json(products.playback.updates.current().as_deref()),
    ))
}

async fn playback_events(State(products): State<ProductHandles>) -> Result<Response<Body>, Error> {
    let products = &products;
    let subscription = products.playback.updates.subscribe();
    let stream = futures_util::stream::unfold(subscription, |mut subscription| async move {
        let publication = subscription.recv().await.ok()?;
        let value = publication_json(publication.as_ref());
        let frame = Frame::data(Bytes::from(format!("event: playback\ndata: {value}\n\n")));
        Some((Ok::<_, Infallible>(frame), subscription))
    });
    Ok(event_response(Body::new(StreamBody::new(stream))))
}

async fn tracks(
    State(products): State<ProductHandles>,
    headers: hyper::HeaderMap,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    web::page(
        &headers,
        &parameters,
        tracks_data(&products, &parameters).await?,
    )
}

async fn tracks_data(
    products: &ProductHandles,
    parameters: &HashMap<String, String>,
) -> Result<Value, Error> {
    let offset = number(parameters, "offset", 0)?;
    let limit = number(parameters, "limit", 100)?.min(256);
    let rows = products
        .source
        .tracks(
            sources::SourceId::new(required(parameters, "source")?),
            parameters.get("folder").cloned(),
            parameters.get("q").cloned().unwrap_or_default(),
            boolean(parameters, "favorites")?,
            track_sort(
                parameters
                    .get("sort")
                    .map(String::as_str)
                    .unwrap_or("title"),
            )?,
            boolean(parameters, "descending")?,
            offset,
            limit,
        )
        .recv()
        .await
        .map_err(internal)?
        .map_err(bad_request)?;
    let rows = rows.iter().map(track_row_json).collect::<Vec<_>>();
    Ok(json!({"offset":offset,"limit":limit,"tracks":rows}))
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
        queue_json(state.as_deref(), limit),
    ))
}

fn queue_json(state: Option<&playback::PlaybackView>, limit: usize) -> Value {
    state.map(|state| {
        let current=state.queue_window.iter().position(|row|Some(&row.occurrence)==state.queue.current_occurrence.as_ref()).unwrap_or(0);
        let start=current.saturating_sub(limit/2).min(state.queue_window.len().saturating_sub(limit));
        let offset=state.queue.current_index.unwrap_or(0).saturating_sub(current)+start;
        json!({
                "total":state.queue.total,"current_index":state.queue.current_index,"current_id":state.queue.current_occurrence,"loading":state.queue_loading,
                "offset":offset,"window":state.queue_window.iter().skip(start).take(limit).map(|row| json!({"id":row.occurrence,"track":track_json(&row.item)})).collect::<Vec<_>>()
            })}).unwrap_or(json!({"total":0,"current_index":null,"current_id":null,"loading":false,"offset":0,"window":[]}))
}

async fn queue_play(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let form_return = request
        .extensions()
        .get::<SubmittedForm>()
        .map(|form| form.0.get("return").cloned().unwrap_or_default());
    let mut kind = request
        .uri()
        .path()
        .strip_prefix("/api/queue/")
        .unwrap_or("track")
        .to_owned();
    #[derive(Deserialize)]
    struct Input {
        mode: String,
        before: Option<playback::OccurrenceId>,
        after: Option<playback::OccurrenceId>,
        #[serde(flatten)]
        selection: Value,
    }
    let mut input: Input = body(request).await?;
    if form_return.is_some() {
        kind = input.selection["kind"].as_str().unwrap_or(&kind).to_owned();
    }
    if let Some(form_return) = form_return.filter(|_| input.mode == "replace") {
        let parameters: HashMap<String, String> = url::form_urlencoded::parse(
            form_return
                .strip_prefix("/?")
                .unwrap_or_default()
                .as_bytes(),
        )
        .into_owned()
        .collect();
        let selection = &mut input.selection;
        if selection["kind"] == "track" {
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
        kind = selection["kind"].as_str().unwrap_or(&kind).to_owned();
    }
    let folder = if kind != "source" {
        if let Some(folder) = input.selection.get("folder").and_then(Value::as_str) {
            let source = input
                .selection
                .get("source")
                .and_then(Value::as_str)
                .ok_or_else(|| bad_request("source is required with folder"))?;
            catalog::scope(
                products,
                &HashMap::from([
                    ("source".into(), source.into()),
                    ("folder".into(), folder.into()),
                ]),
            )
            .await?
            .1
        } else {
            None
        }
    } else {
        None
    };
    let shuffled_start = kind != "track"
        && input.selection.get("anchor_uri").is_none_or(Value::is_null)
        && input
            .selection
            .get("anchor_entry")
            .is_none_or(Value::is_null);
    let placement = match input.mode.as_str() {
        "replace" => playback::QueuePlacement::Replace { anchor_index: 0 },
        "append" | "insert" => playback::QueuePlacement::End,
        "next" => playback::QueuePlacement::AfterCurrent,
        _ => {
            return Err(bad_request(
                "Queue mode must be replace, append, next or insert",
            ));
        }
    };
    let selection = match kind.as_str() {
        "track" => {
            #[derive(Deserialize)]
            struct Uris {
                uris: Vec<String>,
            }
            let input: Uris = serde_json::from_value(input.selection).map_err(bad_request)?;
            library::QueueInput::MediaUris {
                order: input.uris.into(),
                provenance: playback::Provenance::Manual,
            }
        }
        "album" | "playlist" | "artist" | "genre" => {
            #[derive(Deserialize)]
            struct Collection {
                id: Value,
                #[serde(default)]
                q: String,
                sort: Option<String>,
                #[serde(default)]
                descending: bool,
                anchor_uri: Option<String>,
                anchor_entry: Option<library::PlaylistEntryKey>,
                #[serde(default)]
                album_artists: bool,
            }
            let input: Collection = serde_json::from_value(input.selection).map_err(bad_request)?;
            if !(kind == "playlist") {
                let collection = if kind == "artist" {
                    library::QueueCollection::ArtistKey {
                        key: serde_json::from_value(input.id).map_err(bad_request)?,
                        album_artist: input.album_artists,
                    }
                } else if kind == "genre" {
                    library::QueueCollection::Genre(
                        serde_json::from_value(input.id).map_err(bad_request)?,
                    )
                } else {
                    library::QueueCollection::AlbumKey(
                        serde_json::from_value(input.id).map_err(bad_request)?,
                    )
                };
                library::QueueInput::Query {
                    query: library::QueueQuery::Collection {
                        collection,
                        favorites_only: false,
                    },
                    folder,
                    context_id: "api".into(),
                    filter: input.q,
                    sort: track_sort(input.sort.as_deref().unwrap_or(if kind == "artist" {
                        "title"
                    } else {
                        "track_number"
                    }))?,
                    descending: input.descending,
                    anchor_uri: input.anchor_uri,
                }
            } else {
                library::QueueInput::PlaylistQuery {
                    key: serde_json::from_value(input.id).map_err(bad_request)?,
                    folder,
                    context_id: "api".into(),
                    filter: input.q,
                    sort: playlists::entry_sort(input.sort.as_deref().unwrap_or("position"))?,
                    descending: input.descending,
                    anchor_entry: input.anchor_entry,
                    anchor_uri: input.anchor_uri,
                }
            }
        }
        "smart-playlist" => {
            #[derive(Deserialize)]
            struct Smart {
                id: library::SmartPlaylistKey,
                source: Option<String>,
                anchor_uri: Option<String>,
                #[serde(default)]
                q: String,
            }
            let input: Smart = serde_json::from_value(input.selection).map_err(bad_request)?;
            let source = if let Some(source) = input.source {
                Some(
                    catalog::scope(products, &HashMap::from([("source".into(), source)]))
                        .await?
                        .0,
                )
            } else {
                None
            };
            library::QueueInput::Query {
                query: library::QueueQuery::Smart {
                    key: input.id,
                    source,
                    now: catalog::now(),
                },
                folder,
                filter: input.q,
                sort: library::TrackSort::Title,
                descending: false,
                context_id: "api".into(),
                anchor_uri: input.anchor_uri,
            }
        }
        "source" => {
            #[derive(Deserialize)]
            struct Source {
                source: String,
                folder: Option<String>,
                #[serde(default)]
                q: String,
                sort: Option<String>,
                #[serde(default)]
                descending: bool,
                #[serde(default)]
                favorites: bool,
                anchor_uri: Option<String>,
            }
            let input: Source = serde_json::from_value(input.selection).map_err(bad_request)?;
            let mut scope = HashMap::from([("source".into(), input.source)]);
            if let Some(folder) = input.folder {
                scope.insert("folder".into(), folder);
            }
            let (source, folder) = catalog::scope(products, &scope).await?;
            library::QueueInput::Query {
                query: library::QueueQuery::Tracks {
                    source,
                    favorites_only: input.favorites,
                    recursive: true,
                },
                folder,
                filter: input.q,
                sort: track_sort(input.sort.as_deref().unwrap_or("title"))?,
                descending: input.descending,
                context_id: "api".into(),
                anchor_uri: input.anchor_uri,
            }
        }
        _ => return Err(bad_request("Unknown media kind")),
    };
    let queue = products.playback.queue.clone();
    tokio::task::spawn_blocking(move || {
        if input.mode == "insert" {
            queue.insert(selection, queue_drop_target(input.before, input.after));
            return;
        }
        queue.play(playback::PlayRequest::ordered(
            selection,
            0,
            placement,
            shuffled_start,
        ))
    })
    .await
    .map_err(internal)?;
    Ok(accepted())
}

fn queue_drop_target(
    before: Option<playback::OccurrenceId>,
    after: Option<playback::OccurrenceId>,
) -> playback::QueueReorderTarget {
    match (before, after) {
        (Some(id), _) => playback::QueueReorderTarget::Before(id),
        (_, Some(id)) => playback::QueueReorderTarget::After(id),
        _ => playback::QueueReorderTarget::End,
    }
}

async fn queue_clear(State(products): State<ProductHandles>) -> Result<Response<Body>, Error> {
    let products = &products;
    let queue = products.playback.queue.clone();
    let include_current = products
        .source
        .shared
        .settings
        .load()
        .ui
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
    let input: Identifier = body(request).await?;
    if input.id.is_empty() {
        return Err(bad_request("id is required"));
    }
    let queue = products.playback.queue.clone();
    tokio::task::spawn_blocking(move || queue.activate(playback::OccurrenceId::new(input.id)))
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
    let id = if request.method() == hyper::Method::POST {
        body::<Identifier>(request).await?.id
    } else {
        required(&parameters, "id")?.to_owned()
    };
    let queue = products.playback.queue.clone();
    tokio::task::spawn_blocking(move || queue.remove(playback::OccurrenceId::new(id)))
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
    let transport = products.playback.transport.clone();
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
                Some("off") => playback::RepeatMode::Off,
                Some("one") => playback::RepeatMode::One,
                Some("all") => playback::RepeatMode::All,
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
            value[name] = if matches!(name.as_str(), "volume" | "position_ms") {
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

fn track_sort(value: &str) -> Result<library::TrackSort, Error> {
    use library::TrackSort::*;
    match value {
        "title" => Ok(Title),
        "track_number" => Ok(TrackNumber),
        "artist" => Ok(Artist),
        "album_artist" => Ok(AlbumArtist),
        "album" => Ok(Album),
        "year" => Ok(Year),
        "release_date" => Ok(ReleaseDate),
        "date_added" => Ok(DateAdded),
        "last_played" => Ok(LastPlayed),
        "play_count" => Ok(PlayCount),
        "rating" => Ok(UserRating),
        "genre" => Ok(Genre),
        "bpm" => Ok(Bpm),
        "duration" => Ok(Duration),
        "favorite" => Ok(Favorite),
        _ => Err(bad_request("Unknown track sort")),
    }
}

fn track_json(track: &playback::QueueItem) -> Value {
    json!({"uri":track.media_uri,"title":track.title,"artist":track.artist,"album":track.album,"duration_ms":track.duration_millis})
}
fn track_row_json(row: &library::TrackRow) -> Value {
    json!({"uri":row.media_uri,"object_id":row.object_id,"title":row.title,"artist":row.artist,"album":row.album,
        "duration_ms":row.duration_millis,"track_number":row.track_number,"disc_number":row.disc_number,
        "year":row.year,"favorite":row.favorite,"rating":row.rating})
}

fn playback_json(state: Option<&playback::PlaybackView>) -> Value {
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
        "current":state.transport.current.as_ref().map(|current| json!({"id":current.occurrence.occurrence,"track":track_json(&current.item)})),
        "volume":state.controls.volume,"muted":state.controls.muted,"shuffle":state.controls.shuffle_enabled,"auto_dj":state.controls.auto_dj_enabled,
        "repeat":match state.controls.repeat_mode {playback::RepeatMode::Off=>"off",playback::RepeatMode::One=>"one",playback::RepeatMode::All=>"all"},
        "queue_total":state.queue.total,"queue_loading":state.queue_loading,"error":state.transport.error,
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
        if token == Some(self.token.as_str()) {
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

fn publication_json(publication: Option<&playback::PlaybackProjection>) -> Value {
    let mut value = playback_json(publication.map(|p| &p.view));
    if let Some(publication) = publication {
        value["notices"] = publication.notices.iter().map(|notice| match notice {
                        playback::PlaybackNotice::RunStarted(run) => json!({"type":"run_started","run":run.get()}),
                        playback::PlaybackNotice::PositionDiscontinuity(position) => json!({"type":"position_discontinuity","run":position.run.get(),"position_ms":position.position_millis}),
                        playback::PlaybackNotice::OperationFailed(error) => json!({"type":"operation_failed","error":error}),
                        }).collect::<Vec<_>>().into();
    }
    value
}

async fn events(State(products): State<ProductHandles>) -> Response<Body> {
    let receivers = (
        products.playback.updates.subscribe(),
        products.source.operation(),
        products.lyrics.current(),
        products.appearance.subscribe(),
        products.source.shared.settings.sidebar_changes(),
        products.source.catalog_changes(),
        products.source.shared.settings.clone(),
        true,
        None,
    );
    let stream = futures_util::stream::unfold(
        receivers,
        |(
            mut playback,
            mut source,
            mut lyrics,
            mut appearance,
            mut sidebar,
            mut catalog,
            settings,
            first,
            mut previous,
        )| async move {
            let value = if first {
                let publication = playback.recv().await.ok()?;
                let mut value = playback_event_json(publication, &mut previous, true);
                value["source"] = json!(*source.borrow_and_update());
                value["lyrics"] =
                    media::lyrics_json(&lyrics.borrow_and_update(), &settings.load().ui.lyrics);
                value["appearance"] = json!(*appearance.borrow_and_update());
                sidebar.borrow_and_update();
                catalog.borrow_and_update();
                value["pins_changed"] = json!(true);
                value
            } else {
                tokio::select! {
                    publication = playback.recv() => playback_event_json(publication.ok()?, &mut previous, false),
                    changed = source.changed() => { changed.ok()?; json!({"source":*source.borrow_and_update()}) },
                    changed = lyrics.changed() => { changed.ok()?; json!({"lyrics":media::lyrics_json(&lyrics.borrow_and_update(), &settings.load().ui.lyrics)}) },
                    changed = appearance.changed() => { changed.ok()?; json!({"appearance":*appearance.borrow_and_update()}) },
                    changed = sidebar.changed() => { changed.ok()?; json!({"pins_changed":true}) },
                    changed = catalog.changed() => { changed.ok()?; json!({"pins_changed":true}) },
                }
            };
            Some((
                Ok::<_, Infallible>(Frame::data(Bytes::from(format!(
                    "event: update\ndata: {value}\n\n"
                )))),
                (
                    playback, source, lyrics, appearance, sidebar, catalog, settings, false,
                    previous,
                ),
            ))
        },
    );
    event_response(Body::new(StreamBody::new(stream)))
}

fn playback_event_json(
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
    let mut value = json!({"playback": publication_json(publication.as_ref())});
    if first || changed {
        value["queue"] = queue_json(current, library::QUEUE_CONTEXT_LIMIT);
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
