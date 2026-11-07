use super::*;
use rufin_core::lyrics::{CurrentLyrics, CurrentLyricsContent};

pub(super) fn routes() -> Router<ProductHandles> {
    Router::new()
        .route("/api/lyrics", get(lyrics).post(load_lyrics))
        .route("/api/lyrics/events", get(lyrics_events))
        .route("/api/artwork", get(artwork))
        .route("/api/queue/reorder", post(reorder))
        .route("/api/queue/next", post(play_next))
        .route("/api/random", get(random_settings))
        .route("/api/queue/random", post(random))
        .route("/api/media", get(metadata))
        .route("/api/favorite", get(selection_favorite).post(favorite))
        .route("/api/queue/radio", post(radio))
}

async fn metadata(
    State(products): State<ProductHandles>,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let uri = required(&parameters, "uri")?;
    let cancel = library::ReadCancellation::new();
    // The pages its links open keep the source and music folder of the page that asks, or the
    // selected ones for the player's queue.
    let scope = match parameters.get("page") {
        Some(page) => serde_json::from_str::<page::PageToken>(page).map_err(bad_request)?,
        None => page::PageToken::selected(&products),
    };
    if library::source_entity_parts(uri).is_some_and(|(_, kind, _)| kind == "album") {
        let row = products
            .library
            .album(uri, false, &cancel)
            .await
            .map_err(internal)?
            .ok_or_else(|| error(StatusCode::NOT_FOUND, "Album unavailable"))?
            .row;
        let artists = row
            .links
            .album_artists
            .iter()
            .map(|link| artist_link(&scope, link.artist_key, &link.media_uri, &link.name, true))
            .collect::<Vec<_>>();
        return Ok(json_response(
            StatusCode::OK,
            json!({"favorite":row.state.favorite,"album":null,"artists":artists}),
        ));
    }
    let row = products
        .library
        .track(uri, &cancel)
        .await
        .map_err(internal)?;
    let (favorite, mut links) = if let Some(row) = row {
        (
            row.state.favorite,
            media_links(&scope, &row.links, &row.metadata.album),
        )
    } else {
        let favorite = products
            .library
            .favorite(&library::FavoriteTarget::Track(uri.into()))
            .await
            .map_err(internal)?;
        (favorite, json!({"album":null,"artists":[]}))
    };
    links["favorite"] = json!(favorite);
    Ok(json_response(StatusCode::OK, links))
}

/// The album and artists a track's Go to menu opens, as `{album, artists}`, each with the page
/// it opens in the source and music folder of `scope`.
pub(super) fn media_links(
    scope: &page::PageToken,
    links: &library::TrackLinks,
    album: &str,
) -> Value {
    let (artists, album_artists) = rufin_core::route::detail_links::go_to_artists(links);
    let album = links
        .album_media_uri
        .as_ref()
        .zip(links.album_key)
        .map(|(uri, id)| {
            let source = library::source_entity_parts(uri).map(|(source, _, _)| source);
            let page = scope.at(rufin_core::route::Route::AlbumDetail(uri.clone()));
            json!({"id":id,"uri":uri,"title":album,"source":source,"page":page})
        });
    let artists = artists
        .iter()
        .map(|link| {
            artist_link(
                scope,
                link.artist_key,
                &link.media_uri,
                &link.name,
                album_artists,
            )
        })
        .collect::<Vec<_>>();
    json!({"album":album,"artists":artists})
}

fn artist_link(
    scope: &page::PageToken,
    id: library::ArtistKey,
    uri: &str,
    name: &str,
    album_artists: bool,
) -> Value {
    use rufin_core::route::Route;
    let source = library::source_entity_parts(uri).map(|(source, _, _)| source);
    let page = scope.at(if album_artists {
        Route::AlbumArtistTracks(uri.to_owned())
    } else {
        Route::ArtistTracks(uri.to_owned())
    });
    json!({"id":id,"uri":uri,"name":name,"source":source,"album_artists":album_artists,"page":page})
}

pub(super) async fn favorite(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    #[derive(Deserialize)]
    struct Input {
        kind: String,
        uri: Option<String>,
        favorite: bool,
        page: Option<page::PageToken>,
        ranges: Option<Vec<std::ops::Range<usize>>>,
    }
    let input: Input = body(request).await?;
    if let (Some(page), Some(ranges)) = (input.page, input.ranges) {
        for uri in selected_uris(&products, page, ranges).await? {
            let target = library::FavoriteTarget::Track(uri);
            let source = &products.source;
            source
                .set_favorite_with_result(target, input.favorite)
                .await
                .map_err(bad_request)?;
        }
        return Ok(json_response(
            StatusCode::OK,
            json!({"favorite":input.favorite}),
        ));
    }
    let uri = input
        .uri
        .ok_or_else(|| bad_request("uri, or page and ranges, is required"))?;
    let target = match input.kind.as_str() {
        "track" => library::FavoriteTarget::Track(uri),
        "album" => library::FavoriteTarget::Album(uri),
        "artist" => library::FavoriteTarget::Artist(uri),
        _ => return Err(bad_request("Unknown favorite type")),
    };
    let favorite = products
        .source
        .set_favorite_with_result(target, input.favorite)
        .await
        .map_err(bad_request)?;
    Ok(json_response(StatusCode::OK, json!({"favorite":favorite})))
}

/// The tracks of the rows at `ranges` of `page`, by URI.
async fn selected_uris(
    products: &ProductHandles,
    page: page::PageToken,
    ranges: Vec<std::ops::Range<usize>>,
) -> Result<Vec<String>, Error> {
    let Some(selection) = page::selection(products, page, Some(ranges)).await? else {
        return Ok(Vec::new());
    };
    let cancel = library::ReadCancellation::new();
    let uris = products
        .library
        .selected_track_uris(&selection, None, &cancel);
    uris.await.map_err(internal)
}

/// Whether every track of the rows at `ranges` of `page` is a favorite.
async fn selection_favorite(
    State(products): State<ProductHandles>,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let page = serde_json::from_str(required(&parameters, "page")?).map_err(bad_request)?;
    let ranges = serde_json::from_str(required(&parameters, "ranges")?).map_err(bad_request)?;
    let uris = selected_uris(&products, page, ranges).await?;
    let states = products.library.user_states(Some(&uris)).await;
    let states = states.map_err(internal)?;
    let favorites: std::collections::HashSet<_> = (states.iter())
        .filter_map(|(state, _)| (state.favorite == Some(true)).then_some(&state.media_uri))
        .collect();
    let favorite = uris.iter().all(|uri| favorites.contains(uri));
    Ok(json_response(StatusCode::OK, json!({"favorite":favorite})))
}

async fn radio(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    #[derive(Deserialize)]
    struct RadioInput {
        #[serde(flatten)]
        seed: library::RadioSeed,
        mode: Option<String>,
    }
    let input: RadioInput = body(request).await?;
    let seed = input.seed;
    let request = match input.mode.as_deref().unwrap_or("replace") {
        "replace" => rufin_core::playback::RadioPlayRequest::now(seed),
        "next" => rufin_core::playback::RadioPlayRequest::next(seed),
        "append" => rufin_core::playback::RadioPlayRequest::last(seed),
        _ => return Err(bad_request("Queue mode must be replace, append or next")),
    };
    products.playback.play_radio(request);
    Ok(accepted())
}

/// The source and music folder a Random Play request names, as a selection of its own.
async fn random_library(
    products: &ProductHandles,
    source: &str,
    folder: Option<&str>,
) -> Result<rufin_core::runtime::SelectedLibrary, Error> {
    page::scope(products, Some(&sources::SourceId::new(source)), folder)
        .await?
        .ok_or_else(|| bad_request("source is required"))
}

async fn random_settings(
    State(products): State<ProductHandles>,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let selected = random_library(
        &products,
        required(&parameters, "source")?,
        parameters.get("folder").map(String::as_str),
    )
    .await?;
    let offset = number(&parameters, "offset", 0)?;
    let (total, rows) = rufin_core::radio::random_genres(
        &products.library,
        &selected,
        parameters.get("q").map(String::as_str).unwrap_or(""),
        offset..offset.saturating_add(64),
        &library::ReadCancellation::new(),
    )
    .await
    .map_err(internal)?;
    let settings = products.settings.load().random_play.clone();
    let selected_genre =
        rufin_core::radio::saved_random_genre(&products.library, &settings, &selected)
            .await
            .map_err(internal)?
            .map(|row| json!({"id":row.key,"name":row.name}));
    let total = boolean(&parameters, "total")?.then_some(total as i64);
    Ok(json_response(
        StatusCode::OK,
        json!({"total":total,"settings":settings,"selected_genre":selected_genre,"genres":rows.iter().map(|row|json!({"id":row.genre_key,"object_id":row.object_id,"name":row.name})).collect::<Vec<_>>()}),
    ))
}

async fn random(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    #[derive(Deserialize)]
    struct Input {
        source: String,
        folder: Option<String>,
        count: usize,
        min_year: Option<i64>,
        max_year: Option<i64>,
        genre: Option<library::GenreKey>,
        #[serde(default)]
        played: library::PlayedFilter,
        mode: String,
    }
    let input: Input = body(request).await?;
    let selected = random_library(&products, &input.source, input.folder.as_deref()).await?;
    let placement = match input.mode.as_str() {
        "replace" => rufin_core::playback::QueuePlacement::Now,
        "next" => rufin_core::playback::QueuePlacement::Next,
        "append" => rufin_core::playback::QueuePlacement::Last,
        _ => return Err(bad_request("Queue mode must be replace, append or next")),
    };
    let request = rufin_core::playback::RandomPlayRequest {
        placement,
        requested: input.count.min(500),
        criteria: library::RandomCriteria {
            min_year: input.min_year,
            max_year: input.max_year,
            genre: input.genre,
            played: input.played,
            variation: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos() as i64),
        },
    };
    let empty =
        rufin_core::radio::queue_random(&products.library, &selected, request, &products.playback)
            .await
            .map_err(bad_request)?;
    Ok(json_response(StatusCode::OK, json!({"empty":empty})))
}

pub(super) async fn play_next(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    #[derive(Deserialize)]
    struct Input {
        position: usize,
    }
    let input: Input = body(request).await?;
    let queue = products.playback.clone();
    tokio::task::spawn_blocking(move || queue.move_after_current(input.position))
        .await
        .map_err(internal)?;
    Ok(accepted())
}

async fn lyrics(State(products): State<ProductHandles>) -> Result<Response<Body>, Error> {
    Ok(json_response(
        StatusCode::OK,
        lyrics_json(
            &products.lyrics.current().borrow(),
            &products.settings.load().lyrics,
        ),
    ))
}

async fn load_lyrics(State(products): State<ProductHandles>) -> Result<Response<Body>, Error> {
    products.lyrics.load_current();
    Ok(accepted())
}

async fn lyrics_events(State(products): State<ProductHandles>) -> Response<Body> {
    let stream = futures_util::stream::unfold(
        (products.lyrics.current(), products.settings.clone(), true),
        |(mut current, settings, first)| async move {
            if !first && current.changed().await.is_err() {
                return None;
            }
            let value = lyrics_json(&current.borrow_and_update(), &settings.load().lyrics);
            Some((
                Ok::<_, Infallible>(Frame::data(Bytes::from(format!(
                    "event: lyrics\ndata: {value}\n\n"
                )))),
                (current, settings, false),
            ))
        },
    );
    event_response(Body::new(StreamBody::new(stream)))
}

pub(super) fn lyrics_json(current: &CurrentLyrics, settings: &lyrics::Settings) -> Value {
    let mut value = match current {
        CurrentLyrics::Cleared => json!({"state":"empty"}),
        CurrentLyrics::Loading { .. } => json!({"state":"loading"}),
        CurrentLyrics::Ready { content, .. } => match content {
            Some(CurrentLyricsContent::Instrumental) => json!({"state":"instrumental"}),
            Some(CurrentLyricsContent::Document {
                document,
                pronunciation,
                ..
            }) => {
                let readings = document.lines.iter().map(|line| {
                    let supplied = lyrics::pronunciation_line_for(line, pronunciation.as_deref());
                    let reading = if settings.preferences.show_furigana { supplied.and_then(|supplied|lyrics::japanese_reading_from_romanization(&line.text,&supplied.text)) } else { None };
                    json!({"segments":reading.map(|reading|reading.segments.into_iter().map(|segment|json!({"surface":segment.surface,"furigana":segment.furigana})).collect::<Vec<_>>()),"romanization":if settings.preferences.show_romanization { supplied.map(|line|&line.text) } else { None }})
                }).collect::<Vec<_>>();
                json!({"state":"ready","document":document,"pronunciation":pronunciation,"readings":readings,"show_romanization":settings.preferences.show_romanization,"font_size":settings.preferences.lyrics_font_size.unwrap_or(19)})
            }
            None => json!({"state":"empty"}),
        },
    };
    if let CurrentLyrics::Loading { media_id } | CurrentLyrics::Ready { media_id, .. } = current {
        value["media_uri"] = json!(media_id.media_uri);
        value["run"] = json!(media_id.run.map(playback::RunId::get));
    }
    value
}

pub(super) async fn reorder(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    #[derive(Deserialize)]
    struct Input {
        positions: Vec<usize>,
        before: Option<usize>,
        after: Option<usize>,
    }
    let input: Input = body(request).await?;
    let queue = products.playback.clone();
    tokio::task::spawn_blocking(move || {
        queue.reorder(
            rufin_core::playback::QueueSelection::Positions(input.positions),
            queue_drop_target(input.before, input.after),
        )
    })
    .await
    .map_err(internal)?;
    Ok(accepted())
}

async fn artwork(
    State(products): State<ProductHandles>,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let cancel = library::ReadCancellation::new();
    let binding = if parameters.contains_key("genre") {
        let (source, _) = catalog::scope(
            &products,
            required(&parameters, "source")?,
            parameters.get("folder").map(String::as_str),
        )
        .await?;
        products
            .library
            .rows_of(
                &[library::Item::Genre(key(&parameters, "genre")?)],
                true,
                &cancel,
            )
            .await
            .map_err(internal)?
            .into_iter()
            .find_map(|row| match row {
                library::Row::Tag(row) if row.source_key == source => Some(row),
                _ => None,
            })
            .and_then(|row| {
                row.artwork_binding
                    .as_ref()
                    .map(std::slice::from_ref)
                    .unwrap_or(&row.representative_artwork)
                    .get(
                        parameters
                            .get("part")
                            .and_then(|part| part.parse::<usize>().ok())
                            .unwrap_or(0),
                    )
                    .cloned()
            })
    } else if parameters.contains_key("playlist") {
        products
            .library
            .playlists(
                &[key::<library::PlaylistKey>(&parameters, "playlist")?],
                &cancel,
            )
            .await
            .map_err(internal)?
            .into_iter()
            .next()
            .and_then(|row| row.artwork_binding)
    } else if parameters.contains_key("smart_playlist") {
        products
            .library
            .rows_of(
                &[library::Item::Smart(key(&parameters, "smart_playlist")?)],
                true,
                &cancel,
            )
            .await
            .map_err(internal)?
            .into_iter()
            .find_map(|row| match row {
                library::Row::Playlist(library::PlaylistListRow::SmartPlaylist(row)) => {
                    row.artwork_binding
                }
                _ => None,
            })
    } else {
        let uri = required(&parameters, "uri")?;
        products
            .library
            .artwork_bindings(&[uri.to_owned()], &cancel)
            .await
            .map_err(internal)?
            .into_iter()
            .next()
            .flatten()
    }
    .ok_or_else(|| error(StatusCode::NOT_FOUND, "No cover image"))?;
    let external = products.settings.load().artwork_policy();
    let binding = artwork::ArtworkBinding::opaque(&binding);
    let size = parameters
        .get("size")
        .map(|value| value.parse::<u32>())
        .transpose()
        .map_err(bad_request)?
        .unwrap_or(256)
        .max(1);
    let original = parameters
        .get("original")
        .is_some_and(|value| value == "true");
    let request = if original {
        artwork::ArtworkRequest::original(binding, size)
    } else {
        artwork::ArtworkRequest::new(binding, size)
    }
    .with_external(external);
    let (bytes, mime) = if original {
        let bytes = products
            .artwork
            .image_bytes(request)
            .await
            .map_err(internal)?
            .ok_or_else(|| error(StatusCode::NOT_FOUND, "No cover image"))?;
        let mime = artwork::image_mime(&bytes)
            .ok_or_else(|| error(StatusCode::NOT_FOUND, "No cover image"))?;
        (bytes, mime)
    } else {
        let image = match products.artwork.load(request).finish().await {
            artwork::ArtworkOutcome::Ready(image) => image,
            artwork::ArtworkOutcome::Missing | artwork::ArtworkOutcome::Invalidated => {
                return Err(error(StatusCode::NOT_FOUND, "No cover image"));
            }
            artwork::ArtworkOutcome::Failed(message) => return Err(internal(message)),
        };
        let png =
            tokio::task::spawn_blocking(move || image.pixels().resized_to_fit(size)?.encode_png())
                .await
                .map_err(internal)?
                .map_err(internal)?;
        (png, "image/png")
    };
    Ok(Response::builder()
        .header("content-type", mime)
        .header("cache-control", "private, max-age=300")
        .body(Body::from(bytes))
        .expect("image headers"))
}
