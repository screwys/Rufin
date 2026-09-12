use super::*;
use library::{AlbumSort, FolderKey, ReadCancellation, SourceKey};

pub(super) async fn scope(
    products: &ProductHandles,
    parameters: &HashMap<String, String>,
) -> Result<(SourceKey, Option<FolderKey>), Error> {
    let source = sources::SourceId::new(required(parameters, "source")?);
    let source = products
        .library
        .source_identity_key(&source)
        .await
        .map_err(internal)?
        .ok_or_else(|| bad_request("Source not found"))?;
    let folder = if let Some(folder) = parameters.get("folder") {
        Some(
            products
                .library
                .folder_key_by_object(source, folder, &ReadCancellation::new())
                .await
                .map_err(internal)?
                .ok_or_else(|| bad_request("Folder not found"))?,
        )
    } else {
        None
    };
    Ok((source, folder))
}

pub(super) fn routes() -> Router<ProductHandles> {
    Router::new()
        .route("/api/home", get(home).post(refresh_home))
        .route("/api/appearance", get(appearance))
        .route("/api/genres/tracks", get(genre_tracks))
        .route("/api/albums", get(albums))
        .route("/api/albums/tracks", get(album_tracks))
        .route("/api/folders", get(folders))
        .route("/api/folders/live", get(live_folders))
        .route("/api/artists", get(artists))
        .route("/api/artists/tracks", get(artist_tracks))
        .route("/api/smart-playlists", get(smart_playlists))
        .route("/api/smart-playlists/tracks", get(smart_tracks))
}

async fn appearance(State(products): State<ProductHandles>) -> Result<Response<Body>, Error> {
    let settings = products.source.shared.settings.load().ui;
    Ok(json_response(
        StatusCode::OK,
        json!({"theme":settings.theme_preference,"accent":settings.accent_preference,"colors":*products.appearance.borrow()}),
    ))
}

async fn refresh_home(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    #[derive(Deserialize)]
    struct Input {
        source: sources::SourceId,
        block: library::HomeSectionKind,
    }
    let input: Input = body(request).await?;
    let selected = match products
        .source
        .selected_library()
        .filter(|selected| selected.source_id == input.source)
    {
        Some(selected) => selected,
        None => completion(products.source.select_source(input.source)).await?,
    };
    completion(selected.operations.refresh_home(input.block)).await?;
    Ok(accepted())
}

fn home_album(row: &library::HomeAlbumRow) -> Value {
    let album = &row.album;
    json!({"kind":"album","id":album.album_key,"object_id":album.object_id,"uri":album.media_uri,"title":row.title,"artist":album.display_artist,"favorite":album.favorite,"track_count":album.track_count,"year":album.year,"duration_ms":album.duration_millis})
}

fn home_track(row: &library::HomeTrackRow) -> Value {
    let mut value = track_row_json(&row.track);
    value["kind"] = "track".into();
    value["title"] = row.title.clone().into();
    value
}

fn home_rows(rows: &library::HomeSectionRows) -> Vec<Value> {
    rows.albums
        .iter()
        .map(home_album)
        .chain(rows.tracks.iter().map(home_track))
        .collect()
}

async fn home(
    State(products): State<ProductHandles>,
    headers: hyper::HeaderMap,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    use library::HomeBlockKind;
    let (source, folder) = scope(&products, &parameters).await?;
    let mut blocks = products.source.shared.settings.load().ui.home_blocks;
    if let Some(block) = parameters.get("block") {
        let selected: HomeBlockKind =
            serde_json::from_value(Value::String(block.clone())).map_err(bad_request)?;
        blocks.retain(|block| *block == selected);
    }
    let (showcase_variation, explore_variation) = products.source.home_variations();
    let page = products
        .library
        .home_page(
            source,
            folder,
            showcase_variation,
            explore_variation,
            &blocks,
            &ReadCancellation::new(),
        )
        .await
        .map_err(internal)?;
    let mut sections = Vec::new();
    for block in blocks {
        let (title, items) = match block {
            HomeBlockKind::Showcase => ("Showcase", page.showcase.iter().map(home_album).collect()),
            HomeBlockKind::Explore => ("Explore", page.explore.iter().map(home_track).collect()),
            HomeBlockKind::MostPlayed => ("Most played", home_rows(&page.most_played)),
            HomeBlockKind::NewlyAdded => ("Newly added", home_rows(&page.newly_added)),
            HomeBlockKind::RecentlyPlayed => ("Recently played", home_rows(&page.recently_played)),
            HomeBlockKind::RecentlyReleased => ("Recently released", home_rows(&page.recently_released)),
            HomeBlockKind::Genres => ("Featured genres", page.genres.iter().map(|row|json!({"kind":"genre","id":row.genre_key,"name":row.name,"track_count":row.track_count})).collect()),
        };
        if !items.is_empty() {
            sections.push(json!({"block":block,"title":title,"items":items}));
        }
    }
    for section in page.provider_sections {
        if parameters.contains_key("block") {
            break;
        }
        let items = home_rows(&section.rows);
        if !items.is_empty() {
            sections.push(json!({"title":section.title.unwrap_or_else(||"From your server".into()),"items":items}));
        }
    }
    web::page(&headers, &parameters, json!({"sections":sections}))
}

async fn genre_tracks(
    State(products): State<ProductHandles>,
    headers: hyper::HeaderMap,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let folder = if parameters.contains_key("folder") {
        scope(&products, &parameters).await?.1
    } else {
        None
    };
    let rows = products
        .library
        .collection_tracks_page(
            &library::QueueCollection::Genre(key(&parameters, "id")?),
            folder,
            parameters.get("q").map(String::as_str).unwrap_or(""),
            track_sort(
                parameters
                    .get("sort")
                    .map(String::as_str)
                    .unwrap_or("title"),
            )?,
            boolean(&parameters, "descending")?,
            false,
            number(&parameters, "offset", 0)?,
            number(&parameters, "limit", 48)?.min(256),
            &ReadCancellation::new(),
        )
        .await
        .map_err(internal)?;
    web::page(
        &headers,
        &parameters,
        json!({"tracks":rows.iter().map(track_row_json).collect::<Vec<_>>()}),
    )
}

async fn artists(
    State(products): State<ProductHandles>,
    headers: hyper::HeaderMap,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let (source, folder) = scope(&products, &parameters).await?;
    let sort = match parameters
        .get("sort")
        .map(String::as_str)
        .unwrap_or("title")
    {
        "title" => library::ArtistSort::Title,
        "album_count" => library::ArtistSort::AlbumCount,
        "track_count" => library::ArtistSort::TrackCount,
        "last_played" => library::ArtistSort::LastPlayed,
        "play_count" => library::ArtistSort::PlayCount,
        "rating" => library::ArtistSort::Rating,
        "favorite" => library::ArtistSort::Favorite,
        _ => return Err(bad_request("Unknown artist sort")),
    };
    let offset = number(&parameters, "offset", 0)?;
    let limit = number(&parameters, "limit", 48)?.min(128);
    let rows = products
        .library
        .artist_page(
            source,
            folder,
            boolean(&parameters, "album_artists")?,
            boolean(&parameters, "favorites")?,
            parameters.get("q").map(String::as_str).unwrap_or(""),
            sort,
            boolean(&parameters, "descending")?,
            offset,
            limit,
            &ReadCancellation::new(),
        )
        .await
        .map_err(internal)?;
    web::page(
        &headers,
        &parameters,
        json!({"offset":offset,"limit":limit,"artists":rows.iter().map(|row|json!({"id":row.artist_key,"uri":row.media_uri,"name":row.name,"favorite":row.favorite,"album_count":row.album_count,"track_count":row.track_count})).collect::<Vec<_>>()}),
    )
}

async fn artist_tracks(
    State(products): State<ProductHandles>,
    headers: hyper::HeaderMap,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let folder = if parameters.contains_key("folder") {
        scope(&products, &parameters).await?.1
    } else {
        None
    };
    let collection = library::QueueCollection::ArtistKey {
        key: key(&parameters, "id")?,
        album_artist: boolean(&parameters, "album_artists")?,
    };
    let offset = number(&parameters, "offset", 0)?;
    let limit = number(&parameters, "limit", 48)?.min(256);
    let rows = products
        .library
        .collection_tracks_page(
            &collection,
            folder,
            parameters.get("q").map(String::as_str).unwrap_or(""),
            track_sort(
                parameters
                    .get("sort")
                    .map(String::as_str)
                    .unwrap_or("title"),
            )?,
            boolean(&parameters, "descending")?,
            boolean(&parameters, "favorites")?,
            offset,
            limit,
            &ReadCancellation::new(),
        )
        .await
        .map_err(internal)?;
    web::page(
        &headers,
        &parameters,
        json!({"offset":offset,"limit":limit,"tracks":rows.iter().map(track_row_json).collect::<Vec<_>>()}),
    )
}

pub(super) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

async fn smart_playlists(
    State(products): State<ProductHandles>,
    headers: hyper::HeaderMap,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let (source, folder) = if parameters.contains_key("source") {
        let (s, f) = scope(&products, &parameters).await?;
        (Some(s), f)
    } else {
        (None, None)
    };
    let sort = match parameters
        .get("sort")
        .map(String::as_str)
        .unwrap_or("position")
    {
        "position" => library::SmartPlaylistListSort::Position,
        "title" => library::SmartPlaylistListSort::Title,
        "track_count" => library::SmartPlaylistListSort::TrackCount,
        "duration" => library::SmartPlaylistListSort::Duration,
        _ => return Err(bad_request("Unknown smart playlist sort")),
    };
    let offset = number(&parameters, "offset", 0)?;
    let limit = number(&parameters, "limit", 48)?.min(64);
    let rows = products
        .library
        .smart_playlist_page(
            source,
            folder,
            sort,
            boolean(&parameters, "descending")?,
            parameters.get("q").map(String::as_str).unwrap_or(""),
            now(),
            offset,
            limit,
            &ReadCancellation::new(),
        )
        .await
        .map_err(internal)?;
    web::page(
        &headers,
        &parameters,
        json!({"offset":offset,"limit":limit,"smart_playlists":rows.iter().map(|row|json!({"id":row.smart_playlist_key,"name":row.name,"track_count":row.track_count,"duration_ms":row.duration_millis})).collect::<Vec<_>>()}),
    )
}

async fn smart_tracks(
    State(products): State<ProductHandles>,
    headers: hyper::HeaderMap,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let (source, folder) = if parameters.contains_key("source") {
        let (s, f) = scope(&products, &parameters).await?;
        (Some(s), f)
    } else {
        (None, None)
    };
    let offset = number(&parameters, "offset", 0)?;
    let limit = number(&parameters, "limit", 48)?.min(256);
    let rows = products
        .library
        .smart_playlist_track_page(
            source,
            key(&parameters, "id")?,
            folder,
            parameters.get("q").map(String::as_str).unwrap_or(""),
            now(),
            offset,
            limit,
            &ReadCancellation::new(),
        )
        .await
        .map_err(internal)?;
    web::page(
        &headers,
        &parameters,
        json!({"offset":offset,"limit":limit,"tracks":rows.iter().map(|row|json!({"uri":row.media_uri,"title":row.title,"artist":row.artist,"album":row.album,"favorite":row.favorite,"duration_ms":row.duration_millis})).collect::<Vec<_>>()}),
    )
}

async fn albums(
    State(products): State<ProductHandles>,
    headers: hyper::HeaderMap,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let parameters = &parameters;
    let offset = number(parameters, "offset", 0)?;
    let limit = number(parameters, "limit", 100)?.min(128);
    let filter = parameters.get("q").map(String::as_str).unwrap_or("");
    let descending = boolean(parameters, "descending")?;
    let favorites = boolean(parameters, "favorites")?;
    let cancellation = ReadCancellation::new();
    let (source, folder) = scope(products, parameters).await?;
    let sort = match parameters
        .get("sort")
        .map(String::as_str)
        .unwrap_or("title")
    {
        "title" => AlbumSort::Title,
        "album_artist" => AlbumSort::AlbumArtist,
        "year" => AlbumSort::Year,
        "release_date" => AlbumSort::ReleaseDate,
        "date_added" => AlbumSort::DateAdded,
        "last_played" => AlbumSort::LastPlayed,
        "play_count" => AlbumSort::PlayCount,
        "rating" => AlbumSort::Rating,
        "track_count" => AlbumSort::TrackCount,
        "duration" => AlbumSort::Duration,
        "favorite" => AlbumSort::Favorite,
        _ => return Err(bad_request("Unknown album sort")),
    };
    let rows = products
        .library
        .album_page(
            source,
            folder,
            favorites,
            filter,
            sort,
            descending,
            offset,
            limit,
            &cancellation,
        )
        .await
        .map_err(internal)?;
    web::page(
        &headers,
        &parameters,
        json!({"offset":offset,"limit":limit,"albums":rows.iter().map(|row| json!({
                "id":row.album_key,"uri":row.media_uri,"title":row.title,"artist":row.display_artist,
                "year":row.year,"track_count":row.track_count,"duration_ms":row.duration_millis,"favorite":row.favorite,"rating":row.rating
            })).collect::<Vec<_>>()}),
    )
}

async fn album_tracks(
    State(products): State<ProductHandles>,
    headers: hyper::HeaderMap,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let parameters = &parameters;
    let offset = number(parameters, "offset", 0)?;
    let limit = number(parameters, "limit", 100)?.min(128);
    let filter = parameters.get("q").map(String::as_str).unwrap_or("");
    let descending = boolean(parameters, "descending")?;
    let favorites = boolean(parameters, "favorites")?;
    let cancellation = ReadCancellation::new();
    let album = key(parameters, "id")?;
    let folder = if parameters.contains_key("folder") {
        scope(products, parameters).await?.1
    } else {
        None
    };
    let sort = track_sort(
        parameters
            .get("sort")
            .map(String::as_str)
            .unwrap_or("track_number"),
    )?;
    let rows = products
        .library
        .collection_tracks_page(
            &library::QueueCollection::AlbumKey(album),
            folder,
            filter,
            sort,
            descending,
            favorites,
            offset,
            limit,
            &cancellation,
        )
        .await
        .map_err(bad_request)?;
    web::page(
        &headers,
        &parameters,
        json!({"offset":offset,"limit":limit,"tracks":rows.iter().map(track_row_json).collect::<Vec<_>>()}),
    )
}

async fn folders(
    State(products): State<ProductHandles>,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let parameters = &parameters;
    let offset = number(parameters, "offset", 0)?;
    let limit = number(parameters, "limit", 100)?.min(128);
    let cancellation = ReadCancellation::new();
    let (source, folder) = scope(products, parameters).await?;
    let parent = if let Some(parent) = parameters.get("parent") {
        Some(
            products
                .library
                .folder_key_by_object(source, parent, &cancellation)
                .await
                .map_err(internal)?
                .ok_or_else(|| bad_request("Folder not found"))?,
        )
    } else {
        folder
    };
    let rows = products
        .library
        .folder_page(source, parent, offset, limit, &cancellation)
        .await
        .map_err(internal)?;
    Ok(json_response(
        StatusCode::OK,
        json!({"offset":offset,"limit":limit,"folders":rows.iter().map(|row| json!({"id":row.object_id,"name":row.name,"track_count":row.track_count})).collect::<Vec<_>>()}),
    ))
}

async fn live_folders(
    State(products): State<ProductHandles>,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let parameters = &parameters;
    let source_id = sources::SourceId::new(required(parameters, "source")?);
    let owner = products.source.clone();
    let source = tokio::task::spawn_blocking(move || owner.client(&source_id))
        .await
        .map_err(internal)?
        .map_err(bad_request)?;
    let page = source
        .browse_folder(
            parameters.get("parent").map(String::as_str),
            parameters.get("folder").map(String::as_str),
        )
        .await
        .map_err(bad_request)?;
    Ok(json_response(
        StatusCode::OK,
        json!({"folders":page.folders.iter().map(|folder| json!({"id":folder.object_id,"name":folder.name})).collect::<Vec<_>>(),"track_ids":page.tracks}),
    ))
}
