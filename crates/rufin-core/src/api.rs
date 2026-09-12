//! Optional HTTP access to the same product owners used by native clients.

mod catalog;
mod controller;
mod media;
mod playlists;
mod source;
mod web;
pub use controller::{Controller, ControllerSettings, ControllerStatus};

use std::collections::HashMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    Router,
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
    let authorization = Arc::new(format!("Bearer {token}"));
    let router = routes()
        .merge(media::routes())
        .route_layer(middleware::from_fn_with_state(authorization, authorize))
        .merge(web::routes())
        .with_state(products);
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let service = TowerToHyperService::new(router.clone());
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
        .route("/api/queue/item", delete(queue_remove))
        .route("/api/playback/{action}", post(playback_command))
        .merge(source::routes())
        .merge(playlists::routes())
        .merge(catalog::routes())
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
    let products = &products;
    let parameters = &parameters;
    let offset = number(&parameters, "offset", 0)?;
    let limit = number(&parameters, "limit", 100)?.min(256);
    let rows = products
        .source
        .tracks(
            sources::SourceId::new(required(&parameters, "source")?),
            parameters.get("folder").cloned(),
            parameters.get("q").cloned().unwrap_or_default(),
            boolean(&parameters, "favorites")?,
            track_sort(
                parameters
                    .get("sort")
                    .map(String::as_str)
                    .unwrap_or("title"),
            )?,
            boolean(&parameters, "descending")?,
            offset,
            limit,
        )
        .recv()
        .await
        .map_err(internal)?
        .map_err(bad_request)?;
    let rows = rows.iter().map(track_row_json).collect::<Vec<_>>();
    web::page(
        &headers,
        &parameters,
        json!({"offset":offset,"limit":limit,"tracks":rows}),
    )
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
    let path = request.uri().path().to_owned();
    #[derive(Deserialize)]
    struct Input {
        mode: String,
        #[serde(flatten)]
        selection: Value,
    }
    let input: Input = body(request).await?;
    let folder = if path != "/api/queue/source" {
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
    let shuffled_start = path != "/api/queue"
        && input.selection.get("anchor_uri").is_none_or(Value::is_null)
        && input
            .selection
            .get("anchor_entry")
            .is_none_or(Value::is_null);
    let placement = match input.mode.as_str() {
        "replace" => playback::QueuePlacement::Replace { anchor_index: 0 },
        "append" => playback::QueuePlacement::End,
        "next" => playback::QueuePlacement::AfterCurrent,
        _ => return Err(bad_request("Queue mode must be replace, append or next")),
    };
    let selection = match path.as_str() {
        "/api/queue" => {
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
        "/api/queue/album" | "/api/queue/playlist" | "/api/queue/artist" | "/api/queue/genre" => {
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
            if !path.ends_with("/playlist") {
                let collection = if path.ends_with("/artist") {
                    library::QueueCollection::ArtistKey {
                        key: serde_json::from_value(input.id).map_err(bad_request)?,
                        album_artist: input.album_artists,
                    }
                } else if path.ends_with("/genre") {
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
                    sort: track_sort(input.sort.as_deref().unwrap_or(
                        if path.ends_with("/artist") {
                            "title"
                        } else {
                            "track_number"
                        },
                    ))?,
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
        "/api/queue/smart-playlist" => {
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
        _ => {
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
    };
    let queue = products.playback.queue.clone();
    tokio::task::spawn_blocking(move || {
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
) -> Result<Response<Body>, Error> {
    let products = &products;
    let parameters = &parameters;
    let id = required(&parameters, "id")?.to_owned();
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

async fn body<T: serde::de::DeserializeOwned>(request: Request<Body>) -> Result<T, Error> {
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
async fn authorize(
    State(authorization): State<Arc<String>>,
    request: Request<Body>,
    next: Next,
) -> Response<Body> {
    if request
        .headers()
        .get(hyper::header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        != Some(authorization.as_str())
    {
        return error(StatusCode::UNAUTHORIZED, "A valid Bearer token is required").into_response();
    }
    next.run(request).await
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
        products.source.shared.settings.clone(),
        true,
        None,
    );
    let stream = futures_util::stream::unfold(
        receivers,
        |(mut playback, mut source, mut lyrics, mut appearance, settings, first, mut previous)| async move {
            let value = if first {
                let publication = playback.recv().await.ok()?;
                let mut value = playback_event_json(publication, &mut previous, true);
                value["source"] = json!(*source.borrow_and_update());
                value["lyrics"] =
                    media::lyrics_json(&lyrics.borrow_and_update(), &settings.load().ui.lyrics);
                value["appearance"] = json!(*appearance.borrow_and_update());
                value
            } else {
                tokio::select! {
                    publication = playback.recv() => playback_event_json(publication.ok()?, &mut previous, false),
                    changed = source.changed() => { changed.ok()?; json!({"source":*source.borrow_and_update()}) },
                    changed = lyrics.changed() => { changed.ok()?; json!({"lyrics":media::lyrics_json(&lyrics.borrow_and_update(), &settings.load().ui.lyrics)}) },
                    changed = appearance.changed() => { changed.ok()?; json!({"appearance":*appearance.borrow_and_update()}) },
                }
            };
            Some((
                Ok::<_, Infallible>(Frame::data(Bytes::from(format!(
                    "event: update\ndata: {value}\n\n"
                )))),
                (
                    playback, source, lyrics, appearance, settings, false, previous,
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
