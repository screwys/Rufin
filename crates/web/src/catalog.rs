use super::*;
use library::{CatalogSort, FolderKey, ReadCancellation, SourceKey};

pub(super) async fn track_query(
    products: &ProductHandles,
    parameters: &HashMap<String, String>,
    kind: &str,
) -> Result<library::MemberRead, Error> {
    use library::{Collection, MemberRead};
    if parameters.contains_key("folder") && !parameters.contains_key("source") {
        return Err(bad_request("source is required with folder"));
    }
    let (source, folder) = if parameters.contains_key("source")
        && (parameters.contains_key("folder")
            || matches!(
                kind,
                "source" | "tracks" | "smart-playlist" | "smart-tracks"
            )) {
        let (source, folder) = scope(
            products,
            required(parameters, "source")?,
            parameters.get("folder").map(String::as_str),
        )
        .await?;
        (Some(source), folder)
    } else {
        (None, None)
    };
    let scope = match kind {
        "source" | "tracks" => {
            Collection::Source(source.ok_or_else(|| bad_request("Source not found"))?)
        }
        "album" | "album-tracks" => Collection::Album(key(parameters, "id")?),
        "artist" | "artist-tracks" => Collection::Artist {
            key: key(parameters, "id")?,
            album_artist: boolean(parameters, "album_artists")?,
        },
        "genre" | "genre-tracks" => Collection::Genre(key(parameters, "id")?),
        "playlist" | "entries" => Collection::Playlist(key(parameters, "id")?),
        "smart-playlist" | "smart-tracks" => Collection::Smart {
            key: key(parameters, "id")?,
            source,
            now: now(),
        },
        _ => return Err(bad_request("Unknown media kind")),
    };
    let mut query = MemberRead::new(scope);
    query.folder = folder;
    query.filter = parameters.get("q").cloned().unwrap_or_default();
    query.sort = if matches!(query.collection, Collection::Smart { .. }) {
        smart_track_sort(parameters.get("sort").map(String::as_str))?
    } else {
        Some(track_sort(
            parameters
                .get("sort")
                .map(String::as_str)
                .unwrap_or(match query.collection {
                    Collection::Playlist(_) => "position",
                    Collection::Album(_) => "track_number",
                    _ => "title",
                }),
        )?)
    };
    query.descending = boolean(parameters, "descending")?;
    query.favorites_only = boolean(parameters, "favorites")?;
    Ok(query)
}

pub(super) async fn track_total(
    products: &ProductHandles,
    parameters: &HashMap<String, String>,
    query: &library::MemberRead,
) -> Result<Option<i64>, Error> {
    if !boolean(parameters, "total")? {
        return Ok(None);
    }
    (query)
        .count(&products.library, &ReadCancellation::new())
        .await
        .map(|count| Some(count as i64))
        .map_err(internal)
}

pub(super) async fn page_total(
    products: &ProductHandles,
    parameters: &HashMap<String, String>,
    query: &library::ListRead,
) -> Result<Option<i64>, Error> {
    if !boolean(parameters, "total")? {
        return Ok(None);
    }
    query
        .count(&products.library, &ReadCancellation::new())
        .await
        .map(|count| Some(count as i64))
        .map_err(internal)
}

pub(super) async fn scope(
    products: &ProductHandles,
    source: &str,
    folder: Option<&str>,
) -> Result<(SourceKey, Option<FolderKey>), Error> {
    if source.is_empty() {
        return Err(bad_request("source is required"));
    }
    let source = sources::SourceId::new(source);
    let source = products
        .library
        .source_identity_key(&source)
        .await
        .map_err(internal)?
        .ok_or_else(|| bad_request("Source not found"))?;
    let folder = if let Some(folder) = folder {
        Some(
            products
                .library
                .catalog_keys(
                    library::CatalogKind::Folder,
                    Some(source),
                    &[(folder).to_string()],
                    &ReadCancellation::new(),
                )
                .await
                .map(|keys| keys.into_iter().next().map(library::FolderKey::from_raw))
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

pub(super) fn appearance_data(products: &ProductHandles) -> Value {
    let settings = products.settings.load();
    let colors = products.appearance.borrow();
    let theme = match settings.appearance.theme_preference {
        rufin_core::settings::ThemePreference::System => "System",
        rufin_core::settings::ThemePreference::Light => "Light",
        rufin_core::settings::ThemePreference::Dark => "Dark",
        rufin_core::settings::ThemePreference::Named(_) => colors
            .get("color-scheme")
            .map(String::as_str)
            .unwrap_or("System"),
    };
    json!({"theme":theme,"accent":settings.appearance.accent_preference,"colors":*colors})
}

async fn appearance(State(products): State<ProductHandles>) -> Result<Response<Body>, Error> {
    Ok(json_response(StatusCode::OK, appearance_data(&products)))
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
        Some(selected) => selected.source_id,
        None => products
            .source
            .select_source(input.source)
            .await
            .map_err(bad_request)?,
    };
    completion(products.source.refresh_home(selected, input.block)).await?;
    Ok(accepted())
}

fn home_album(products: &ProductHandles, row: &library::HomeAlbumRow) -> Value {
    let album = &row.album;
    json!({"kind":"album","id":album.album_key,"object_id":album.object_id,"uri":album.media_uri,"title":row.title,"artist":album.metadata.artist,"favorite":album.state.favorite,"track_count":album.summary.track_count,"year":album.metadata.year,"duration_ms":album.summary.duration_millis,"artwork_previews":artwork_previews(products, album.artwork_binding.iter())})
}

fn home_track(products: &ProductHandles, row: &library::HomeTrackRow) -> Value {
    let mut value = track_row_json(&row.track);
    value["artwork_previews"] = json!(artwork_previews(products, row.track.artwork_binding.iter()));
    value["kind"] = "track".into();
    value["title"] = row.title.clone().into();
    value
}

fn home_rows(products: &ProductHandles, rows: &library::HomeSectionRows) -> Vec<Value> {
    rows.albums
        .iter()
        .map(|row| home_album(products, row))
        .chain(rows.tracks.iter().map(|row| home_track(products, row)))
        .collect()
}

async fn home(
    State(products): State<ProductHandles>,
    headers: hyper::HeaderMap,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    browser::page(
        &headers,
        &parameters,
        home_data(&products, &parameters).await?,
    )
}

pub(super) async fn home_data(
    products: &ProductHandles,
    parameters: &HashMap<String, String>,
) -> Result<Value, Error> {
    use library::HomeBlockKind;
    let (source, folder) = scope(
        products,
        required(parameters, "source")?,
        parameters.get("folder").map(String::as_str),
    )
    .await?;
    let mut blocks = products.settings.load().application.home_blocks;
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
            HomeBlockKind::Showcase => ("Showcase", page.showcase.iter().map(|row| home_album(products, row)).collect()),
            HomeBlockKind::Explore => ("Explore", page.explore.iter().map(|row| home_album(products, row)).collect()),
            HomeBlockKind::MostPlayed => ("Most played", home_rows(products, &page.most_played)),
            HomeBlockKind::NewlyAdded => ("Newly added", home_rows(products, &page.newly_added)),
            HomeBlockKind::RecentlyPlayed => ("Recently played", home_rows(products, &page.recently_played)),
            HomeBlockKind::RecentlyReleased => ("Recently released", home_rows(products, &page.recently_released)),
            HomeBlockKind::Genres => ("Featured genres", page.genres.iter().map(|row|json!({"kind":"genre","id":row.genre_key,"object_id":row.object_id,"name":row.metadata.name,"track_count":row.summary.track_count})).collect()),
        };
        if !items.is_empty() {
            sections.push(json!({"block":block,"title":title,"items":items}));
        }
    }
    for section in page.provider_sections {
        if parameters.contains_key("block") {
            break;
        }
        let items = home_rows(products, &section.rows);
        if !items.is_empty() {
            sections.push(json!({"title":section.title.unwrap_or_else(||"From your server".into()),"items":items}));
        }
    }
    Ok(json!({"sections":sections}))
}

async fn genre_tracks(
    State(products): State<ProductHandles>,
    headers: hyper::HeaderMap,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    browser::page(
        &headers,
        &parameters,
        genre_tracks_data(&products, &parameters).await?,
    )
}

pub(super) async fn genre_tracks_data(
    products: &ProductHandles,
    parameters: &HashMap<String, String>,
) -> Result<Value, Error> {
    let query = track_query(products, parameters, "genre-tracks").await?;
    let offset = number(parameters, "offset", 0)?;
    let limit = number(parameters, "limit", 48)?.min(256);
    let rows = (&query)
        .rows(&products.library, offset, limit, &ReadCancellation::new())
        .await
        .map(|rows| {
            rows.into_iter()
                .map(library::MemberRow::into_track)
                .collect::<Vec<_>>()
        })
        .map_err(internal)?;
    let total = track_total(products, parameters, &query).await?;
    Ok(
        json!({"activity_sensitive":query.activity_sensitive(),"total":total,"offset":offset,"limit":limit,"tracks":rows.iter().map(track_row_json).collect::<Vec<_>>()}),
    )
}

async fn artists(
    State(products): State<ProductHandles>,
    headers: hyper::HeaderMap,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    browser::page(
        &headers,
        &parameters,
        artists_data(&products, &parameters).await?,
    )
}

pub(super) async fn artists_data(
    products: &ProductHandles,
    parameters: &HashMap<String, String>,
) -> Result<Value, Error> {
    let (source, folder) = scope(
        products,
        required(parameters, "source")?,
        parameters.get("folder").map(String::as_str),
    )
    .await?;
    let sort = match parameters
        .get("sort")
        .map(String::as_str)
        .unwrap_or("title")
    {
        "title" => library::CatalogSort::Title,
        "album_count" => library::CatalogSort::AlbumCount,
        "track_count" => library::CatalogSort::TrackCount,
        "last_played" => library::CatalogSort::LastPlayed,
        "play_count" => library::CatalogSort::PlayCount,
        "rating" => library::CatalogSort::UserRating,
        "favorite" => library::CatalogSort::Favorite,
        _ => return Err(bad_request("Unknown artist sort")),
    };
    let offset = number(parameters, "offset", 0)?;
    let limit = number(parameters, "limit", 48)?.min(128);
    let query = library::ListRead {
        playlist_scope: Default::default(),
        kind: library::ListKind::Artists {
            album_artist: boolean(parameters, "album_artists")?,
        },
        source: Some(source),
        sort: sort,
        folder: folder,
        filter: (parameters.get("q").map(String::as_str).unwrap_or("")).to_string(),
        descending: boolean(parameters, "descending")?,
        favorites_only: boolean(parameters, "favorites")?,
        downloaded_only: false,
        activity: false,
    };
    let rows = query
        .artists(&products.library, offset, limit, &ReadCancellation::new())
        .await
        .map_err(internal)?;
    let total = page_total(products, parameters, &query).await?;
    Ok(
        json!({"activity_sensitive":query.activity_sensitive(),"total":total,"offset":offset,"limit":limit,"artists":rows.iter().map(|row|json!({"id":row.artist_key,"object_id":row.object_id,"album_artists":boolean(parameters,"album_artists").unwrap_or(false),"uri":row.media_uri,"name":row.metadata.name,"favorite":row.state.favorite,"album_count":row.summary.album_count,"track_count":row.summary.track_count,"artwork_previews":artwork_previews(products, row.artwork_binding.iter())})).collect::<Vec<_>>()}),
    )
}

async fn artist_tracks(
    State(products): State<ProductHandles>,
    headers: hyper::HeaderMap,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    browser::page(
        &headers,
        &parameters,
        artist_tracks_data(&products, &parameters).await?,
    )
}

pub(super) async fn artist_tracks_data(
    products: &ProductHandles,
    parameters: &HashMap<String, String>,
) -> Result<Value, Error> {
    let query = track_query(products, parameters, "artist-tracks").await?;
    let offset = number(parameters, "offset", 0)?;
    let limit = number(parameters, "limit", 48)?.min(256);
    let rows = (&query)
        .rows(&products.library, offset, limit, &ReadCancellation::new())
        .await
        .map(|rows| {
            rows.into_iter()
                .map(library::MemberRow::into_track)
                .collect::<Vec<_>>()
        })
        .map_err(internal)?;
    let total = track_total(products, parameters, &query).await?;
    Ok(
        json!({"activity_sensitive":query.activity_sensitive(),"total":total,"offset":offset,"limit":limit,"tracks":rows.iter().map(track_row_json).collect::<Vec<_>>()}),
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
    browser::page(
        &headers,
        &parameters,
        smart_playlists_data(&products, &parameters).await?,
    )
}

pub(super) async fn smart_playlists_data(
    products: &ProductHandles,
    parameters: &HashMap<String, String>,
) -> Result<Value, Error> {
    let (source, folder) = if parameters.contains_key("source") {
        let (s, f) = scope(
            products,
            required(parameters, "source")?,
            parameters.get("folder").map(String::as_str),
        )
        .await?;
        (Some(s), f)
    } else {
        (None, None)
    };
    let sort = match parameters
        .get("sort")
        .map(String::as_str)
        .unwrap_or("position")
    {
        "position" => library::CatalogSort::Position,
        "title" => library::CatalogSort::Title,
        "track_count" => library::CatalogSort::TrackCount,
        "duration" => library::CatalogSort::Duration,
        _ => return Err(bad_request("Unknown smart playlist sort")),
    };
    let offset = number(parameters, "offset", 0)?;
    let limit = number(parameters, "limit", 48)?.min(64);
    let query = library::ListRead {
        playlist_scope: Default::default(),
        kind: library::ListKind::SmartPlaylists { now: now() },
        source: (source).into(),
        sort: sort,
        folder: folder,
        filter: (parameters.get("q").map(String::as_str).unwrap_or("")).to_string(),
        descending: boolean(parameters, "descending")?,
        favorites_only: false,
        downloaded_only: false,
        activity: false,
    };
    let rows = query
        .smart_playlists(&products.library, offset, limit, &ReadCancellation::new())
        .await
        .map_err(internal)?;
    let total = page_total(products, parameters, &query).await?;
    Ok(
        json!({"activity_sensitive":query.activity_sensitive(),"total":total,"offset":offset,"limit":limit,"smart_playlists":rows.iter().map(|row|json!({"id":row.smart_playlist_key,"object_id":row.object_id,"name":rufin_core::playlists::smart_playlist_display_name(row),"track_count":row.track_count,"duration_ms":row.duration_millis,"artwork_count":row.artwork_binding.as_slice().len(),"artwork_previews":artwork_previews(products, row.artwork_binding.as_slice())})).collect::<Vec<_>>()}),
    )
}

async fn smart_tracks(
    State(products): State<ProductHandles>,
    headers: hyper::HeaderMap,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    browser::page(
        &headers,
        &parameters,
        smart_tracks_data(&products, &parameters).await?,
    )
}

pub(super) async fn smart_tracks_data(
    products: &ProductHandles,
    parameters: &HashMap<String, String>,
) -> Result<Value, Error> {
    let query = track_query(products, parameters, "smart-tracks").await?;
    let offset = number(parameters, "offset", 0)?;
    let limit = number(parameters, "limit", 48)?.min(256);
    let rows = (&query)
        .rows(&products.library, offset, limit, &ReadCancellation::new())
        .await
        .map(|rows| {
            rows.into_iter()
                .map(library::MemberRow::into_track)
                .collect::<Vec<_>>()
        })
        .map_err(internal)?;
    let total = track_total(products, parameters, &query).await?;
    Ok(
        json!({"activity_sensitive":query.activity_sensitive(),"total":total,"offset":offset,"limit":limit,"tracks":rows.iter().map(track_row_json).collect::<Vec<_>>()}),
    )
}

async fn albums(
    State(products): State<ProductHandles>,
    headers: hyper::HeaderMap,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    browser::page(
        &headers,
        &parameters,
        albums_data(&products, &parameters).await?,
    )
}

pub(super) async fn albums_data(
    products: &ProductHandles,
    parameters: &HashMap<String, String>,
) -> Result<Value, Error> {
    let offset = number(parameters, "offset", 0)?;
    let limit = number(parameters, "limit", 100)?.min(128);
    let filter = parameters.get("q").map(String::as_str).unwrap_or("");
    let descending = boolean(parameters, "descending")?;
    let favorites = boolean(parameters, "favorites")?;
    let cancellation = ReadCancellation::new();
    let (source, folder) = scope(
        products,
        required(parameters, "source")?,
        parameters.get("folder").map(String::as_str),
    )
    .await?;
    let sort = match parameters
        .get("sort")
        .map(String::as_str)
        .unwrap_or("title")
    {
        "title" => CatalogSort::Title,
        "album_artist" => CatalogSort::AlbumArtist,
        "year" => CatalogSort::Year,
        "release_date" => CatalogSort::ReleaseDate,
        "date_added" => CatalogSort::DateAdded,
        "last_played" => CatalogSort::LastPlayed,
        "play_count" => CatalogSort::PlayCount,
        "rating" => CatalogSort::UserRating,
        "track_count" => CatalogSort::TrackCount,
        "duration" => CatalogSort::Duration,
        "favorite" => CatalogSort::Favorite,
        _ => return Err(bad_request("Unknown album sort")),
    };
    let query = library::ListRead {
        playlist_scope: Default::default(),
        kind: library::ListKind::Albums,
        source: Some(source),
        sort: sort,
        folder: folder,
        filter: (filter).to_string(),
        descending: descending,
        favorites_only: favorites,
        downloaded_only: false,
        activity: false,
    };
    let rows = query
        .albums(&products.library, offset, limit, &cancellation)
        .await
        .map_err(internal)?;
    let total = page_total(products, parameters, &query).await?;
    Ok(
        json!({"activity_sensitive":query.activity_sensitive(),"total":total,"offset":offset,"limit":limit,"albums":rows.iter().map(|row| json!({
                "id":row.album_key,"object_id":row.object_id,"uri":row.media_uri,"title":row.metadata.title,"artist":row.metadata.artist,
                "year":row.metadata.year,"track_count":row.summary.track_count,"duration_ms":row.summary.duration_millis,"favorite":row.state.favorite,"rating":row.state.rating,"artwork_previews":artwork_previews(products, row.artwork_binding.iter())
            })).collect::<Vec<_>>()}),
    )
}

async fn album_tracks(
    State(products): State<ProductHandles>,
    headers: hyper::HeaderMap,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    browser::page(
        &headers,
        &parameters,
        album_tracks_data(&products, &parameters).await?,
    )
}

pub(super) async fn album_tracks_data(
    products: &ProductHandles,
    parameters: &HashMap<String, String>,
) -> Result<Value, Error> {
    let query = track_query(products, parameters, "album-tracks").await?;
    let offset = number(parameters, "offset", 0)?;
    let limit = number(parameters, "limit", 100)?.min(128);
    let rows = (&query)
        .rows(&products.library, offset, limit, &ReadCancellation::new())
        .await
        .map(|rows| {
            rows.into_iter()
                .map(library::MemberRow::into_track)
                .collect::<Vec<_>>()
        })
        .map_err(internal)?;
    let disc_sections = (&query)
        .disc_sections(&products.library, &ReadCancellation::new())
        .await
        .map_err(internal)?;
    let total = track_total(products, parameters, &query).await?;
    Ok(
        json!({"activity_sensitive":query.activity_sensitive(),"album_tracks":true,"disc_sections":disc_sections,"total":total,"offset":offset,"limit":limit,"tracks":rows.iter().map(track_row_json).collect::<Vec<_>>()}),
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
    let (source, folder) = scope(
        products,
        required(parameters, "source")?,
        parameters.get("folder").map(String::as_str),
    )
    .await?;
    let parent = if let Some(parent) = parameters.get("parent") {
        Some(
            products
                .library
                .catalog_keys(
                    library::CatalogKind::Folder,
                    Some(source),
                    std::slice::from_ref(parent),
                    &cancellation,
                )
                .await
                .map(|keys| keys.into_iter().next().map(library::FolderKey::from_raw))
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
        json!({"offset":offset,"limit":limit,"folders":rows.iter().map(|row| json!({"id":row.object_id,"name":row.metadata.name,"track_count":row.track_count})).collect::<Vec<_>>()}),
    ))
}

async fn live_folders(
    State(products): State<ProductHandles>,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let parameters = &parameters;
    let source_id = sources::SourceId::new(required(parameters, "source")?);
    let page = products
        .source
        .browse_folder(
            source_id,
            parameters.get("parent").cloned(),
            parameters.get("folder").cloned(),
        )
        .await
        .map_err(internal)?;
    Ok(json_response(
        StatusCode::OK,
        json!({"folders":page.folders.iter().map(|folder| json!({"id":folder.object_id,"name":folder.name})).collect::<Vec<_>>(),"track_ids":page.tracks}),
    ))
}
