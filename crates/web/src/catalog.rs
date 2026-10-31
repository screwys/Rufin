use super::*;
use library::{FolderKey, Order, PlaylistListRow, Read, ReadCancellation, Row, SourceKey};

pub(super) async fn track_query(
    products: &ProductHandles,
    parameters: &HashMap<String, String>,
    kind: &str,
) -> Result<library::MemberRead, Error> {
    use library::{Collection, MemberRead};
    if !matches!(kind, "smart-playlist" | "smart-tracks")
        && parameters.contains_key("folder")
        && !parameters.contains_key("source")
    {
        return Err(bad_request("source is required with folder"));
    }
    let (source, folder) = if !matches!(kind, "smart-playlist" | "smart-tracks")
        && parameters.contains_key("source")
        && (parameters.contains_key("folder") || matches!(kind, "source" | "tracks"))
    {
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
            // The page's opening time: it keeps the members it opened with.
            now: match parameters.get("opened") {
                Some(opened) => opened
                    .parse()
                    .map_err(|_| bad_request("Invalid opened time"))?,
                None => now(),
            },
        },
        _ => return Err(bad_request("Unknown media kind")),
    };
    let list = match scope {
        Collection::Album(_) => LibraryListKey::AlbumDetailTracks,
        Collection::Artist { .. } => LibraryListKey::ArtistTracks,
        Collection::Genre(_) => LibraryListKey::GenreTracks,
        Collection::Playlist(_) => LibraryListKey::PlaylistTracks,
        Collection::Smart { .. } => LibraryListKey::SmartPlaylistTracks,
        _ => LibraryListKey::Tracks,
    };
    let mut query = MemberRead::new(scope);
    query.folder = folder;
    query.filter = parameters.get("q").cloned().unwrap_or_default();
    query.sort = sort(parameters, list)?;
    query.descending = boolean(parameters, "descending")?;
    query.favorites_only = boolean(parameters, "favorites")?;
    Ok(query)
}

/// A list's order, and its length when `total` is asked.
pub(super) async fn order(
    products: &ProductHandles,
    parameters: &HashMap<String, String>,
    read: Read,
) -> Result<(Order, Option<i64>), Error> {
    let order = products
        .library
        .order(&read, &ReadCancellation::new())
        .await
        .map_err(internal)?;
    let total = boolean(parameters, "total")?.then_some(order.len() as i64);
    Ok((order, total))
}

pub(super) async fn rows(
    products: &ProductHandles,
    order: &Order,
    offset: usize,
    limit: usize,
    art: bool,
) -> Result<Vec<Row>, Error> {
    products
        .library
        .rows(
            order,
            offset..offset.saturating_add(limit),
            art,
            &ReadCancellation::new(),
        )
        .await
        .map_err(internal)
}

/// A window of a collection's tracks under `list`, with its order for what else the page shows.
pub(super) async fn tracks_page(
    products: &ProductHandles,
    parameters: &HashMap<String, String>,
    kind: &str,
    (default_limit, cap): (usize, usize),
    list: &str,
) -> Result<(Order, Value), Error> {
    let query = track_query(products, parameters, kind).await?;
    let activity_sensitive = query.activity_sensitive();
    let offset = number(parameters, "offset", 0)?;
    let limit = number(parameters, "limit", default_limit)?.min(cap);
    let (order, total) = self::order(products, parameters, Read::Members(query)).await?;
    let tracks = rows(products, &order, offset, limit, false)
        .await?
        .iter()
        .filter_map(|row| match row {
            Row::Track(track) => Some(track_row_json(track)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut value = json!({"activity_sensitive":activity_sensitive,"total":total,"offset":offset,"limit":limit});
    value[list] = tracks.into();
    Ok((order, value))
}

/// A cached folder of a source by object id.
pub(super) async fn folder_key(
    products: &ProductHandles,
    source: SourceKey,
    object: &str,
) -> Result<FolderKey, Error> {
    products
        .library
        .catalog_keys(
            library::CatalogKind::Folder,
            Some(source),
            &[object.to_owned()],
            &ReadCancellation::new(),
        )
        .await
        .map_err(internal)?
        .first()
        .map(|key| FolderKey::from_raw(*key))
        .ok_or_else(|| bad_request("Folder not found"))
}

pub(super) async fn scope(
    products: &ProductHandles,
    source: &str,
    folder: Option<&str>,
) -> Result<(SourceKey, Option<FolderKey>), Error> {
    if source.is_empty() {
        return Err(bad_request("source is required"));
    }
    products.library.loaded().await;
    let source = products
        .library
        .source_key(&sources::SourceId::new(source))
        .ok_or_else(|| bad_request("Source not found"))?;
    let folder = match folder {
        Some(folder) => Some(folder_key(products, source, folder).await?),
        None => None,
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
    let selected = source::ensure_selected(&products, input.source).await?;
    completion(products.source.refresh_home(selected, input.block)).await?;
    Ok(accepted())
}

fn home_album(products: &ProductHandles, row: &library::HomeAlbumRow) -> Value {
    let album = &row.album;
    json!({"kind":"album","id":album.album_key,"object_id":album.object_id,"uri":album.media_uri,"title":row.title,"artist":album.artist,"favorite":album.state.favorite,"track_count":album.summary.track_count,"year":album.year,"duration_ms":album.summary.duration_millis,"artwork_previews":artwork_previews(products, album.artwork_binding.iter())})
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
    use rufin_core::view::HomeSection;
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
    // One Home block shows only that block, without the server's sections.
    let single = parameters.contains_key("block");
    let sections = rufin_core::view::home_sections(&page, &blocks, "")
        .into_iter()
        .filter_map(|section| {
            Some(match section {
                HomeSection::Showcase(row) => {
                    json!({"block":HomeBlockKind::Showcase,"title":"Showcase","items":[home_album(products, &row)]})
                }
                HomeSection::Rows { block, rows } => {
                    let title = match block {
                        HomeBlockKind::Explore => "Explore",
                        HomeBlockKind::MostPlayed => "Most played",
                        HomeBlockKind::NewlyAdded => "Newly added",
                        HomeBlockKind::RecentlyPlayed => "Recently played",
                        HomeBlockKind::RecentlyReleased => "Recently released",
                        HomeBlockKind::Showcase | HomeBlockKind::Genres => {
                            unreachable!("Showcase and Genres have their own sections")
                        }
                    };
                    json!({"block":block,"title":title,"items":home_rows(products, &rows)})
                }
                HomeSection::Genres(genres) => {
                    json!({"block":HomeBlockKind::Genres,"title":"Featured genres","items":genres.iter().map(|row|json!({"kind":"genre","id":row.key,"object_id":row.object_id,"name":row.name,"track_count":row.summary.track_count})).collect::<Vec<_>>()})
                }
                HomeSection::Provider(_) if single => return None,
                HomeSection::Provider(section) => {
                    json!({"title":section.title.unwrap_or_else(||"From your server".into()),"items":home_rows(products, &section.rows)})
                }
            })
        })
        .collect::<Vec<_>>();
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
    Ok(
        tracks_page(products, parameters, "genre-tracks", (48, 256), "tracks")
            .await?
            .1,
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
    let sort = sort(parameters, LibraryListKey::Artists)?.unwrap_or(LibraryField::Title);
    let offset = number(parameters, "offset", 0)?;
    let limit = number(parameters, "limit", 48)?.min(128);
    let album_artists = boolean(parameters, "album_artists")?;
    let query = library::ListRead {
        folder,
        filter: parameters.get("q").cloned().unwrap_or_default(),
        descending: boolean(parameters, "descending")?,
        favorites_only: boolean(parameters, "favorites")?,
        ..library::ListRead::new(
            library::ListKind::Artists {
                album_artist: album_artists,
            },
            Some(source),
            sort,
        )
    };
    let activity_sensitive = query.activity_sensitive();
    let (order, total) = order(products, parameters, Read::List(query)).await?;
    let artists = rows(products, &order, offset, limit, true)
        .await?
        .into_iter()
        .filter_map(|row| match row {
            Row::Artist(row) => Some(json!({"id":row.artist_key,"object_id":row.object_id,"album_artists":album_artists,"uri":row.media_uri,"name":row.name,"favorite":row.state.favorite,"album_count":row.summary.album_count,"track_count":row.summary.track_count,"artwork_previews":artwork_previews(products, row.artwork_binding.iter())})),
            _ => None,
        })
        .collect::<Vec<_>>();
    Ok(
        json!({"activity_sensitive":activity_sensitive,"total":total,"offset":offset,"limit":limit,"artists":artists}),
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
    Ok(
        tracks_page(products, parameters, "artist-tracks", (48, 256), "tracks")
            .await?
            .1,
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
    let sort = sort(parameters, LibraryListKey::SmartPlaylists)?.unwrap_or(LibraryField::RowIndex);
    let offset = number(parameters, "offset", 0)?;
    let limit = number(parameters, "limit", 48)?.min(64);
    let query = library::ListRead {
        folder,
        filter: parameters.get("q").cloned().unwrap_or_default(),
        descending: boolean(parameters, "descending")?,
        ..library::ListRead::new(
            library::ListKind::SmartPlaylists { now: now() },
            source,
            sort,
        )
    };
    let activity_sensitive = query.activity_sensitive();
    let (order, total) = order(products, parameters, Read::List(query)).await?;
    let smart_playlists = rows(products, &order, offset, limit, true)
        .await?
        .into_iter()
        .filter_map(|row| match row {
            Row::Playlist(PlaylistListRow::SmartPlaylist(row)) => Some(json!({"id":row.smart_playlist_key,"object_id":row.object_id,"name":rufin_core::playlists::smart_playlist_display_name(&row.object_id, &row.name),"track_count":row.track_count,"duration_ms":row.duration_millis,"artwork_count":row.artwork_binding.as_slice().len(),"artwork_previews":artwork_previews(products, row.artwork_binding.as_slice())})),
            _ => None,
        })
        .collect::<Vec<_>>();
    Ok(
        json!({"activity_sensitive":activity_sensitive,"total":total,"offset":offset,"limit":limit,"smart_playlists":smart_playlists}),
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
    Ok(
        tracks_page(products, parameters, "smart-tracks", (48, 256), "tracks")
            .await?
            .1,
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
    let (source, folder) = scope(
        products,
        required(parameters, "source")?,
        parameters.get("folder").map(String::as_str),
    )
    .await?;
    let sort = sort(parameters, LibraryListKey::Albums)?.unwrap_or(LibraryField::Title);
    let query = library::ListRead {
        folder,
        filter: parameters.get("q").cloned().unwrap_or_default(),
        descending: boolean(parameters, "descending")?,
        favorites_only: boolean(parameters, "favorites")?,
        ..library::ListRead::new(library::ListKind::Albums, Some(source), sort)
    };
    let activity_sensitive = query.activity_sensitive();
    let (order, total) = order(products, parameters, Read::List(query)).await?;
    let albums = rows(products, &order, offset, limit, true)
        .await?
        .into_iter()
        .filter_map(|row| match row {
            Row::Album(row) => Some(json!({
                "id":row.album_key,"object_id":row.object_id,"uri":row.media_uri,"title":row.title,"artist":row.artist,
                "year":row.year,"track_count":row.summary.track_count,"duration_ms":row.summary.duration_millis,"favorite":row.state.favorite,"rating":row.state.rating,"artwork_previews":artwork_previews(products, row.artwork_binding.iter())
            })),
            _ => None,
        })
        .collect::<Vec<_>>();
    Ok(
        json!({"activity_sensitive":activity_sensitive,"total":total,"offset":offset,"limit":limit,"albums":albums}),
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
    let (order, mut value) =
        tracks_page(products, parameters, "album-tracks", (100, 128), "tracks").await?;
    value["album_tracks"] = true.into();
    value["disc_sections"] = json!(products.library.disc_sections(&order));
    Ok(value)
}

async fn folders(
    State(products): State<ProductHandles>,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let parameters = &parameters;
    let offset = number(parameters, "offset", 0)?;
    let limit = number(parameters, "limit", 100)?.min(128);
    let (source, folder) = scope(
        products,
        required(parameters, "source")?,
        parameters.get("folder").map(String::as_str),
    )
    .await?;
    let parent = match parameters.get("parent") {
        Some(parent) => Some(folder_key(products, source, parent).await?),
        None => folder,
    };
    let read = Read::Folders(library::FolderRead {
        source,
        parent,
        filter: String::new(),
        descending: false,
    });
    let order = products
        .library
        .order(&read, &ReadCancellation::new())
        .await
        .map_err(internal)?;
    let folders = rows(products, &order, offset, limit, false)
        .await?
        .into_iter()
        .filter_map(|row| match row {
            Row::Folder(row) => Some(
                json!({"id":row.object_id,"name":(!row.name.is_empty()).then_some(row.name),"track_count":row.summary.track_count}),
            ),
            _ => None,
        })
        .collect::<Vec<_>>();
    Ok(json_response(
        StatusCode::OK,
        json!({"offset":offset,"limit":limit,"folders":folders}),
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
