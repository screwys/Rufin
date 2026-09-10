//! Optional HTTP access to the same product owners used by native clients.

mod catalog;
mod playlists;
mod source;

use std::collections::HashMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, StreamBody, combinators::BoxBody};
use hyper::{
    Method, Request, Response, StatusCode,
    body::{Frame, Incoming},
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinSet;

use crate::runtime::ProductHandles;

type Body = BoxBody<Bytes, Infallible>;
type Error = (StatusCode, String);

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
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let products = products.clone();
                let authorization = Arc::clone(&authorization);
                connections.spawn(async move {
                    let service = service_fn(move |request| {
                        let products = products.clone();
                        let authorization = Arc::clone(&authorization);
                        async move {
                            let response = match handle(request, &products, &authorization).await {
                                Ok(response) => response,
                                Err((status, message)) => json_response(status, json!({"error": message})),
                            };
                            Ok::<_, Infallible>(response)
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service).await;
                });
            }
            _ = connections.join_next(), if !connections.is_empty() => {}
        }
    }
}

async fn handle(
    request: Request<Incoming>,
    products: &ProductHandles,
    authorization: &str,
) -> Result<Response<Body>, Error> {
    if request
        .headers()
        .get(hyper::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        != Some(authorization)
    {
        return Err((
            StatusCode::UNAUTHORIZED,
            "A valid Bearer token is required".into(),
        ));
    }
    let parameters: HashMap<String, String> =
        url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    let path = request.uri().path().to_owned();
    match (request.method(), path.as_str()) {
        (&Method::GET, "/api") => Ok(json_response(
            StatusCode::OK,
            serde_json::from_str(include_str!("api/help.json")).expect("bundled API help"),
        )),
        (&Method::GET, "/api/playback") => Ok(json_response(
            StatusCode::OK,
            playback_json(products.playback.updates.current().as_deref()),
        )),
        (&Method::GET, "/api/playback/events") => {
            let subscription = products.playback.updates.subscribe();
            let stream = futures_util::stream::unfold(
                subscription,
                |mut subscription| async move {
                    let publication = subscription.recv().await.ok()?;
                    let mut value = playback_json(publication.as_ref().map(|p| &p.view));
                    if let Some(publication) = publication {
                        value["notices"] = publication.notices.iter().map(|notice| match notice {
                        playback::PlaybackNotice::RunStarted(run) => json!({"type":"run_started","run":run.get()}),
                        playback::PlaybackNotice::PositionDiscontinuity(position) => json!({"type":"position_discontinuity","run":position.run.get(),"position_ms":position.position_millis}),
                        playback::PlaybackNotice::OperationFailed(error) => json!({"type":"operation_failed","error":error}),
                        }).collect::<Vec<_>>().into();
                    }
                    let frame =
                        Frame::data(Bytes::from(format!("event: playback\ndata: {value}\n\n")));
                    Some((Ok::<_, Infallible>(frame), subscription))
                },
            );
            Ok(event_response(BodyExt::boxed(StreamBody::new(stream))))
        }
        (_, path) if path == "/api/sources" || path.starts_with("/api/sources/") => {
            source::handle(request, products, &parameters).await
        }
        (_, path) if path == "/api/playlists" || path.starts_with("/api/playlists/") => {
            playlists::handle(request, products, &parameters).await
        }
        (
            &Method::GET,
            "/api/albums" | "/api/albums/tracks" | "/api/folders" | "/api/folders/live",
        ) => catalog::handle(request, products, &parameters).await,
        (&Method::GET, "/api/tracks") => {
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
            Ok(json_response(
                StatusCode::OK,
                json!({"offset":offset,"limit":limit,"tracks":rows}),
            ))
        }
        (&Method::GET, "/api/queue") => {
            let state = products.playback.updates.current();
            let value = state.as_ref().map(|state| json!({
                "total":state.queue.total,"current_index":state.queue.current_index,
                "window":state.queue_window.iter().map(|row| json!({"id":row.occurrence,"track":track_json(&row.item)})).collect::<Vec<_>>()
            })).unwrap_or(json!({"total":0,"current_index":null,"window":[]}));
            Ok(json_response(StatusCode::OK, value))
        }
        (
            &Method::POST,
            "/api/queue" | "/api/queue/album" | "/api/queue/playlist" | "/api/queue/source",
        ) => {
            #[derive(Deserialize)]
            struct Input {
                mode: String,
                #[serde(flatten)]
                selection: Value,
            }
            let input: Input = body(request).await?;
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
                    let input: Uris =
                        serde_json::from_value(input.selection).map_err(bad_request)?;
                    library::QueueInput::MediaUris {
                        order: input.uris.into(),
                        provenance: playback::Provenance::Manual,
                    }
                }
                "/api/queue/album" | "/api/queue/playlist" => {
                    let id = input
                        .selection
                        .get("id")
                        .cloned()
                        .ok_or_else(|| bad_request("id is required"))?;
                    let collection = if path.ends_with("/album") {
                        library::QueueCollection::AlbumKey(
                            serde_json::from_value(id).map_err(bad_request)?,
                        )
                    } else {
                        library::QueueCollection::Playlist(
                            serde_json::from_value(id).map_err(bad_request)?,
                        )
                    };
                    library::QueueInput::Collection {
                        collection,
                        folder: None,
                        context_id: "api".into(),
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
                    }
                    let input: Source =
                        serde_json::from_value(input.selection).map_err(bad_request)?;
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
                        anchor_uri: None,
                    }
                }
            };
            let queue = products.playback.queue.clone();
            let shuffled_start = path != "/api/queue";
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
        (&Method::DELETE, "/api/queue") => {
            let queue = products.playback.queue.clone();
            tokio::task::spawn_blocking(move || queue.clear(true))
                .await
                .map_err(internal)?;
            Ok(accepted())
        }
        (&Method::POST, "/api/queue/activate") => {
            let input: Identifier = body(request).await?;
            if input.id.is_empty() {
                return Err(bad_request("id is required"));
            }
            let queue = products.playback.queue.clone();
            tokio::task::spawn_blocking(move || {
                queue.activate(playback::OccurrenceId::new(input.id))
            })
            .await
            .map_err(internal)?;
            Ok(accepted())
        }
        (&Method::DELETE, "/api/queue/item") => {
            let id = required(&parameters, "id")?.to_owned();
            let queue = products.playback.queue.clone();
            tokio::task::spawn_blocking(move || queue.remove(playback::OccurrenceId::new(id)))
                .await
                .map_err(internal)?;
            Ok(accepted())
        }
        (&Method::POST, path) if path.starts_with("/api/playback/") => {
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
                    "seek" => transport.seek_millis(
                        value
                            .get("position_ms")
                            .and_then(Value::as_u64)
                            .ok_or_else(|| {
                                bad_request("position_ms must be a nonnegative integer")
                            })?,
                    ),
                    "volume" => {
                        let volume = value
                            .get("volume")
                            .and_then(Value::as_f64)
                            .ok_or_else(|| bad_request("volume must be a number"))?;
                        transport.persist_volume(volume);
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
                    "repeat" => {
                        transport.set_repeat(match value.get("mode").and_then(Value::as_str) {
                            Some("off") => playback::RepeatMode::Off,
                            Some("one") => playback::RepeatMode::One,
                            Some("all") => playback::RepeatMode::All,
                            _ => return Err(bad_request("Repeat mode must be off, one or all")),
                        })
                    }
                    _ => return Err((StatusCode::NOT_FOUND, "Route not found".into())),
                }
                Ok(())
            })
            .await
            .map_err(internal)??;
            Ok(accepted())
        }
        _ => Err((StatusCode::NOT_FOUND, "Route not found".into())),
    }
}

#[derive(Deserialize)]
struct Identifier {
    id: String,
}

async fn body<T: serde::de::DeserializeOwned>(request: Request<Incoming>) -> Result<T, Error> {
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
        "state":status,"position_ms":state.transport.position_millis,"duration_ms":state.transport.duration_millis,
        "current":state.transport.current.as_ref().map(|current| json!({"id":current.occurrence.occurrence,"track":track_json(&current.item)})),
        "volume":state.controls.volume,"muted":state.controls.muted,"shuffle":state.controls.shuffle_enabled,
        "repeat":match state.controls.repeat_mode {playback::RepeatMode::Off=>"off",playback::RepeatMode::One=>"one",playback::RepeatMode::All=>"all"},
        "queue_total":state.queue.total,"queue_loading":state.queue_loading,"error":state.transport.error
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
    (StatusCode::BAD_REQUEST, error.to_string())
}
fn internal(error: impl std::fmt::Display) -> Error {
    (StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
}
fn json_response(status: StatusCode, value: Value) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json; charset=utf-8")
        .header("cache-control", "no-store")
        .body(Full::new(Bytes::from(value.to_string())).boxed())
        .expect("static response headers")
}
