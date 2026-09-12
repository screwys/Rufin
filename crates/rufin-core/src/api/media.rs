use super::*;
use crate::lyrics::{CurrentLyrics, CurrentLyricsContent};

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
        .route("/api/favorite", post(favorite))
        .route("/api/queue/radio", post(radio))
}

async fn metadata(
    State(products): State<ProductHandles>,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let uri = required(&parameters, "uri")?;
    let cancel = library::ReadCancellation::new();
    if library::source_entity_parts(uri).is_some_and(|(_, kind, _)| kind == "album") {
        let row = products
            .library
            .album_row_by_media_uri(uri, &cancel)
            .await
            .map_err(internal)?
            .ok_or_else(|| error(StatusCode::NOT_FOUND, "Album unavailable"))?;
        let artists = row.album_artists.iter().map(|link|json!({"id":link.artist_key,"uri":link.media_uri,"name":link.name,"source":library::source_entity_parts(&link.media_uri).map(|(source,_,_)|source),"album_artists":true})).collect::<Vec<_>>();
        return Ok(json_response(
            StatusCode::OK,
            json!({"favorite":row.favorite,"album":null,"artists":artists}),
        ));
    }
    let row = products
        .library
        .smart_playlist_track_rows(&[uri.to_owned()], &cancel)
        .await
        .map_err(internal)?
        .pop();
    let favorite = products
        .library
        .favorite(&library::FavoriteTarget::Track(uri.into()))
        .await
        .map_err(internal)?;
    let mut album = None;
    let mut artists = Vec::new();
    if let Some(row) = row {
        if let Some(uri) = row.album_media_uri
            && let Some(album_row) = products
                .library
                .album_row_by_media_uri(&uri, &cancel)
                .await
                .map_err(internal)?
        {
            album = Some(
                json!({"id":album_row.album_key,"uri":uri,"title":row.album,"source":library::source_entity_parts(&uri).map(|(source,_,_)|source)}),
            );
        }
        let album_artist = row.artists.is_empty();
        let links = if album_artist {
            row.album_artists
        } else {
            row.artists
        };
        artists = links.iter().map(|link|json!({"id":link.artist_key,"uri":link.media_uri,"name":link.name,"source":library::source_entity_parts(&link.media_uri).map(|(source,_,_)|source),"album_artists":album_artist})).collect();
    }
    Ok(json_response(
        StatusCode::OK,
        json!({"favorite":favorite,"album":album,"artists":artists}),
    ))
}

async fn favorite(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    #[derive(Deserialize)]
    struct Input {
        kind: String,
        uri: String,
        favorite: bool,
    }
    let input: Input = body(request).await?;
    let target = match input.kind.as_str() {
        "track" => library::FavoriteTarget::Track(input.uri),
        "album" => library::FavoriteTarget::Album(input.uri),
        "artist" => library::FavoriteTarget::Artist(input.uri),
        _ => return Err(bad_request("Unknown favorite type")),
    };
    let favorite = completion(
        products
            .source
            .set_favorite_with_result(target, input.favorite),
    )
    .await?;
    Ok(json_response(StatusCode::OK, json!({"favorite":favorite})))
}

async fn radio(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    #[derive(Deserialize)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    enum Input {
        Track { uri: String },
        Album { id: library::AlbumKey },
        Artist { id: library::ArtistKey },
        AlbumArtist { id: library::ArtistKey },
        Playlist { id: library::PlaylistKey },
    }
    #[derive(Deserialize)]
    struct RadioInput {
        #[serde(flatten)]
        seed: Input,
        mode: Option<String>,
    }
    let input: RadioInput = body(request).await?;
    let seed = match input.seed {
        Input::Track { uri } => library::RadioSeed::Track(uri),
        Input::Album { id } => library::RadioSeed::Album(id),
        Input::Artist { id } => library::RadioSeed::Artist(id),
        Input::AlbumArtist { id } => library::RadioSeed::AlbumArtist(id),
        Input::Playlist { id } => library::RadioSeed::Playlist(id),
    };
    let request = match input.mode.as_deref().unwrap_or("replace") {
        "replace" => playback::RadioPlayRequest::now(seed),
        "next" => playback::RadioPlayRequest::next(seed),
        "append" => playback::RadioPlayRequest::last(seed),
        _ => return Err(bad_request("Queue mode must be replace, append or next")),
    };
    products.playback.radio.play_radio(request);
    Ok(accepted())
}

async fn random_settings(
    State(products): State<ProductHandles>,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let (source, folder) = catalog::scope(&products, &parameters).await?;
    let rows = products
        .library
        .genre_page(
            source,
            folder,
            parameters.get("q").map(String::as_str).unwrap_or(""),
            library::GenreSort::Title,
            false,
            number(&parameters, "offset", 0)?,
            64,
            &library::ReadCancellation::new(),
        )
        .await
        .map_err(internal)?;
    let settings = products
        .source
        .shared
        .settings
        .load()
        .ui
        .random_play
        .clone();
    let mut selected_genre = None;
    if let Some(object_id) = settings.selected_genre_id(
        &sources::SourceId::new(required(&parameters, "source")?),
        parameters.get("folder").map(String::as_str),
    ) {
        let cancel = library::ReadCancellation::new();
        if let Some(key) = products
            .library
            .genre_key_by_object(source, object_id, &cancel)
            .await
            .map_err(internal)?
        {
            selected_genre = products
                .library
                .genre_rows(source, &[key], folder, &cancel)
                .await
                .map_err(internal)?
                .into_iter()
                .next()
                .map(|row| json!({"id":row.genre_key,"name":row.name}));
        }
    }
    Ok(json_response(
        StatusCode::OK,
        json!({"settings":settings,"selected_genre":selected_genre,"genres":rows.iter().map(|row|json!({"id":row.genre_key,"object_id":row.object_id,"name":row.name})).collect::<Vec<_>>()}),
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
    let mut parameters = HashMap::from([("source".into(), input.source)]);
    if let Some(folder) = input.folder {
        parameters.insert("folder".into(), folder);
    }
    let (source, folder) = catalog::scope(&products, &parameters).await?;
    let placement = match input.mode.as_str() {
        "replace" => playback::QueuePlacement::Now,
        "next" => playback::QueuePlacement::Next,
        "append" => playback::QueuePlacement::Last,
        _ => return Err(bad_request("Queue mode must be replace, append or next")),
    };
    let request = playback::RandomPlayRequest {
        placement,
        requested: input.count.min(500),
        criteria: library::RandomCriteria {
            min_year: input.min_year,
            max_year: input.max_year,
            genre: input.genre,
            played: input.played,
            require_media: false,
            variation: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos() as i64),
        },
    };
    let empty = crate::radio::queue_random(
        &products.library,
        source,
        folder,
        request,
        &products.playback.queue,
    )
    .await
    .map_err(bad_request)?;
    Ok(json_response(StatusCode::OK, json!({"empty":empty})))
}

async fn play_next(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    #[derive(Deserialize)]
    struct Input {
        id: playback::OccurrenceId,
    }
    let input: Input = body(request).await?;
    let queue = products.playback.queue.clone();
    tokio::task::spawn_blocking(move || queue.move_after_current(input.id))
        .await
        .map_err(internal)?;
    Ok(accepted())
}

async fn lyrics(State(products): State<ProductHandles>) -> Result<Response<Body>, Error> {
    Ok(json_response(
        StatusCode::OK,
        lyrics_json(
            &products.lyrics.current().borrow(),
            &products.source.shared.settings.load().ui.lyrics,
        ),
    ))
}

async fn load_lyrics(State(products): State<ProductHandles>) -> Result<Response<Body>, Error> {
    products.lyrics.load_current();
    Ok(accepted())
}

async fn lyrics_events(State(products): State<ProductHandles>) -> Response<Body> {
    let stream = futures_util::stream::unfold(
        (
            products.lyrics.current(),
            products.source.shared.settings.clone(),
            true,
        ),
        |(mut current, settings, first)| async move {
            if !first && current.changed().await.is_err() {
                return None;
            }
            let value = lyrics_json(&current.borrow_and_update(), &settings.load().ui.lyrics);
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
            }) => {
                let readings = document.lines.iter().map(|line| {
                    let supplied = lyrics::pronunciation_line_for(line, pronunciation.as_deref());
                    let reading = if settings.show_furigana { supplied.and_then(|supplied|lyrics::japanese_reading_from_romanization(&line.text,&supplied.text)) } else { None };
                    json!({"segments":reading.map(|reading|reading.segments.into_iter().map(|segment|json!({"surface":segment.surface,"furigana":segment.furigana})).collect::<Vec<_>>()),"romanization":if settings.show_romanization { supplied.map(|line|&line.text) } else { None }})
                }).collect::<Vec<_>>();
                json!({"state":"ready","document":document,"pronunciation":pronunciation,"readings":readings,"show_romanization":settings.show_romanization,"font_size":settings.lyrics_font_size.unwrap_or(19)})
            }
            None => json!({"state":"empty"}),
        },
    };
    if let CurrentLyrics::Loading { media_id } | CurrentLyrics::Ready { media_id, .. } = current {
        value["id"] = json!(media_id.occurrence);
    }
    value
}

async fn reorder(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    #[derive(Deserialize)]
    struct Input {
        ids: Vec<playback::OccurrenceId>,
        before: Option<playback::OccurrenceId>,
    }
    let input: Input = body(request).await?;
    let queue = products.playback.queue.clone();
    tokio::task::spawn_blocking(move || {
        queue.reorder(playback::QueueReorderRequest {
            occurrences: input.ids,
            target: input.before.map_or(
                playback::QueueReorderTarget::End,
                playback::QueueReorderTarget::Before,
            ),
        })
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
    let binding = if parameters.contains_key("playlist") {
        products
            .library
            .playlist_rows(&[key(&parameters, "playlist")?], &cancel)
            .await
            .map_err(internal)?
            .into_iter()
            .next()
            .and_then(|row| {
                row.artwork_binding
                    .or_else(|| row.representative_artwork.into_iter().next())
            })
    } else if parameters.contains_key("smart_playlist") {
        let source = if parameters.contains_key("source") {
            Some(catalog::scope(&products, &parameters).await?.0)
        } else {
            None
        };
        products
            .library
            .smart_playlist_rows(
                source,
                &[key(&parameters, "smart_playlist")?],
                None,
                catalog::now(),
                &cancel,
            )
            .await
            .map_err(internal)?
            .into_iter()
            .next()
            .and_then(|row| row.artwork_bindings.into_iter().next())
    } else {
        let uri = required(&parameters, "uri")?;
        if library::source_entity_parts(uri).is_some_and(|(_, kind, _)| kind == "artist") {
            products
                .library
                .artist_row_by_media_uri(uri, &cancel)
                .await
                .map_err(internal)?
                .and_then(|row| row.artwork_binding)
        } else if library::source_entity_parts(uri).is_some_and(|(_, kind, _)| kind == "album") {
            products
                .library
                .album_row_by_media_uri(uri, &cancel)
                .await
                .map_err(internal)?
                .and_then(|row| row.artwork_binding)
        } else {
            products
                .library
                .queue_artwork_for_uris(&[uri.to_owned()])
                .await
                .map_err(internal)?
                .into_iter()
                .next()
                .and_then(|(_, binding)| binding)
        }
    }
    .ok_or_else(|| error(StatusCode::NOT_FOUND, "No cover image"))?;
    let owner = products.artwork.clone();
    let settings = products.source.shared.settings.load().ui.clone();
    let load = tokio::task::spawn_blocking(move || {
        let external = artwork::ExternalPolicy::new(
            settings.external_metadata_enabled,
            settings.allows_external_metadata_lookup(),
            settings.lastfm_api_key.clone(),
        );
        let request =
            artwork::ArtworkRequest::new(artwork::ArtworkBinding::opaque(&binding), 256, 256)
                .with_external(external);
        owner.request_prepared(owner.prepare(request))
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;
    let image = match load {
        artwork::ArtworkLoad::Ready(image) => image,
        artwork::ArtworkLoad::Pending(pending) => match pending.finish().await {
            artwork::ArtworkOutcome::Ready(image) => image,
            artwork::ArtworkOutcome::Failed(message) => return Err(internal(message)),
            _ => return Err(error(StatusCode::NOT_FOUND, "No cover image")),
        },
        artwork::ArtworkLoad::Missing => {
            return Err(error(StatusCode::NOT_FOUND, "No cover image"));
        }
    };
    let bytes = tokio::task::spawn_blocking(move || {
        use image::ImageEncoder;
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes).write_image(
            image.rgba(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgba8,
        )?;
        Ok::<_, image::ImageError>(bytes)
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;
    Ok(Response::builder()
        .header("content-type", "image/png")
        .header("cache-control", "private, max-age=300")
        .body(Body::from(bytes))
        .expect("image headers"))
}
