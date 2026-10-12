use super::*;
use library::{AlbumSort, FolderKey, ReadCancellation, SourceKey};

pub(super) async fn song_query(
    products: &ProductHandles,
    parameters: &HashMap<String, String>,
    kind: &str,
) -> Result<library::SongQuery, Error> {
    use library::{SongQuery, SongScope};
    if parameters.contains_key("folder") && !parameters.contains_key("source") {
        return Err(bad_request("source is required with folder"));
    }
    let (source, folder) = if parameters.contains_key("source")
        && (parameters.contains_key("folder")
            || matches!(
                kind,
                "source" | "tracks" | "smart-playlist" | "smart-tracks"
            )) {
        let (source, folder) = scope(products, parameters).await?;
        (Some(source), folder)
    } else {
        (None, None)
    };
    let scope = match kind {
        "source" | "tracks" => {
            SongScope::Tracks(source.ok_or_else(|| bad_request("Source not found"))?)
        }
        "album" | "album-tracks" => SongScope::Album(key(parameters, "id")?),
        "artist" | "artist-tracks" => SongScope::Artist {
            key: key(parameters, "id")?,
            album_artist: boolean(parameters, "album_artists")?,
        },
        "genre" | "genre-tracks" => SongScope::Genre(key(parameters, "id")?),
        "playlist" | "entries" => SongScope::Playlist(key(parameters, "id")?),
        "smart-playlist" | "smart-tracks" => SongScope::Smart {
            key: key(parameters, "id")?,
            source,
            now: now(),
        },
        _ => return Err(bad_request("Unknown media kind")),
    };
    let mut query = SongQuery::new(scope);
    query.folder = folder;
    query.filter = parameters.get("q").cloned().unwrap_or_default();
    query.sort = if matches!(query.scope, SongScope::Smart { .. }) {
        smart_track_sort(parameters.get("sort").map(String::as_str))?
    } else {
        Some(track_sort(
            parameters
                .get("sort")
                .map(String::as_str)
                .unwrap_or(match query.scope {
                    SongScope::Playlist(_) => "position",
                    SongScope::Album(_) => "track_number",
                    _ => "title",
                }),
        )?)
    };
    query.descending = boolean(parameters, "descending")?;
    query.favorites_only = boolean(parameters, "favorites")?;
    Ok(query)
}

pub(super) async fn song_total(
    products: &ProductHandles,
    parameters: &HashMap<String, String>,
    query: &library::SongQuery,
) -> Result<Option<i64>, Error> {
    if !boolean(parameters, "total")? {
        return Ok(None);
    }
    products
        .library
        .song_count(query, &ReadCancellation::new())
        .await
        .map(Some)
        .map_err(internal)
}

pub(super) async fn page_total(
    products: &ProductHandles,
    parameters: &HashMap<String, String>,
    kind: &str,
) -> Result<Option<i64>, Error> {
    if !boolean(parameters, "total")? {
        return Ok(None);
    }
    let filter = parameters.get("q").map(String::as_str).unwrap_or("");
    let favorites = boolean(parameters, "favorites")?;
    if kind == "albums"
        && !parameters.contains_key("folder")
        && filter.trim().is_empty()
        && !favorites
        && let Some(source) = parameters.get("source")
        && let Some((albums, _)) = products
            .source
            .cached_source_counts(&sources::SourceId::new(source))
    {
        return Ok(Some(albums as i64));
    }
    let (source, folder) = if parameters.contains_key("source") {
        let (source, folder) = scope(products, parameters).await?;
        (Some(source), folder)
    } else {
        (None, None)
    };
    let cancel = ReadCancellation::new();
    let database = &products.library;
    let required_source = || source.ok_or_else(|| bad_request("Source not found"));
    let count = match kind {
        "albums" => library::CollectionQuery {
            kind: library::CollectionKind::Albums {
                source: required_source()?,
                sort: match parameters.get("sort").map(String::as_str) {
                    Some("track_count") => AlbumSort::TrackCount,
                    Some("duration") => AlbumSort::Duration,
                    _ => AlbumSort::Title,
                },
                inline_tracks: None,
            },
            folder: folder,
            filter: (filter).to_string(),
            descending: false,
            favorites_only: favorites,
        }
        .count(&database, &cancel)
        .await
        .map(|count| count as i64),
        "artists" => library::CollectionQuery {
            kind: library::CollectionKind::Artists {
                source: required_source()?,
                sort: library::ArtistSort::Title,
                album_artist: boolean(parameters, "album_artists")?,
            },
            folder: folder,
            filter: (filter).to_string(),
            descending: false,
            favorites_only: favorites,
        }
        .count(&database, &cancel)
        .await
        .map(|count| count as i64),
        "genres" => library::CollectionQuery {
            kind: library::CollectionKind::Genres {
                source: required_source()?,
                sort: library::GenreSort::Title,
            },
            folder: folder,
            filter: (filter).to_string(),
            descending: false,
            favorites_only: false,
        }
        .count(&database, &cancel)
        .await
        .map(|count| count as i64),
        "playlists" => library::CollectionQuery {
            kind: library::CollectionKind::Playlists {
                source: (source).into(),
                sort: library::PlaylistSort::Title,
            },
            folder: folder,
            filter: (filter).to_string(),
            descending: false,
            favorites_only: false,
        }
        .count(&database, &cancel)
        .await
        .map(|count| count as i64),
        "smart-playlists" => library::CollectionQuery {
            kind: library::CollectionKind::SmartPlaylists {
                source: (source).into(),
                sort: library::SmartPlaylistListSort::Title,
                now: now(),
            },
            folder: folder,
            filter: (filter).to_string(),
            descending: false,
            favorites_only: false,
        }
        .count(&database, &cancel)
        .await
        .map(|count| count as i64),
        _ => unreachable!("Unknown catalog count"),
    }
    .map_err(internal)?;
    Ok(Some(count))
}

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

pub(super) fn appearance_data(products: &ProductHandles) -> Value {
    let settings = products.settings.load();
    let colors = products.appearance.borrow();
    let theme = match settings.theme_preference {
        rufin_core::settings::ThemePreference::System => "System",
        rufin_core::settings::ThemePreference::Light => "Light",
        rufin_core::settings::ThemePreference::Dark => "Dark",
        rufin_core::settings::ThemePreference::Named(_) => colors
            .get("color-scheme")
            .map(String::as_str)
            .unwrap_or("System"),
    };
    json!({"theme":theme,"accent":settings.accent_preference,"colors":*colors})
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
    let (source, folder) = scope(products, parameters).await?;
    let mut blocks = products.settings.load().home_blocks;
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
            HomeBlockKind::Explore => ("Explore", page.explore.iter().map(home_album).collect()),
            HomeBlockKind::MostPlayed => ("Most played", home_rows(&page.most_played)),
            HomeBlockKind::NewlyAdded => ("Newly added", home_rows(&page.newly_added)),
            HomeBlockKind::RecentlyPlayed => ("Recently played", home_rows(&page.recently_played)),
            HomeBlockKind::RecentlyReleased => ("Recently released", home_rows(&page.recently_released)),
            HomeBlockKind::Genres => ("Featured genres", page.genres.iter().map(|row|json!({"kind":"genre","id":row.genre_key,"object_id":row.object_id,"name":row.name,"track_count":row.track_count})).collect()),
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
    let query = song_query(products, parameters, "genre-tracks").await?;
    let offset = number(parameters, "offset", 0)?;
    let limit = number(parameters, "limit", 48)?.min(256);
    let rows = products
        .library
        .song_rows(&query, offset, limit, &ReadCancellation::new())
        .await
        .map_err(internal)?;
    let total = song_total(products, parameters, &query).await?;
    Ok(
        json!({"total":total,"offset":offset,"limit":limit,"tracks":rows.iter().map(track_row_json).collect::<Vec<_>>()}),
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
    let (source, folder) = scope(products, parameters).await?;
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
    let offset = number(parameters, "offset", 0)?;
    let limit = number(parameters, "limit", 48)?.min(128);
    let rows = library::CollectionQuery {
        kind: library::CollectionKind::Artists {
            source: source,
            sort: sort,
            album_artist: boolean(parameters, "album_artists")?,
        },
        folder: folder,
        filter: (parameters.get("q").map(String::as_str).unwrap_or("")).to_string(),
        descending: boolean(parameters, "descending")?,
        favorites_only: boolean(parameters, "favorites")?,
    }
    .rows(&products.library, offset, limit, &ReadCancellation::new())
    .await
    .map(|rows| rows.artists())
    .map_err(internal)?;
    let total = page_total(products, parameters, "artists").await?;
    Ok(
        json!({"total":total,"offset":offset,"limit":limit,"artists":rows.iter().map(|row|json!({"id":row.artist_key,"object_id":row.object_id,"album_artists":boolean(parameters,"album_artists").unwrap_or(false),"uri":row.media_uri,"name":row.name,"favorite":row.favorite,"album_count":row.album_count,"track_count":row.track_count})).collect::<Vec<_>>()}),
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
    let query = song_query(products, parameters, "artist-tracks").await?;
    let offset = number(parameters, "offset", 0)?;
    let limit = number(parameters, "limit", 48)?.min(256);
    let rows = products
        .library
        .song_rows(&query, offset, limit, &ReadCancellation::new())
        .await
        .map_err(internal)?;
    let total = song_total(products, parameters, &query).await?;
    Ok(
        json!({"total":total,"offset":offset,"limit":limit,"tracks":rows.iter().map(track_row_json).collect::<Vec<_>>()}),
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
        let (s, f) = scope(products, parameters).await?;
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
    let offset = number(parameters, "offset", 0)?;
    let limit = number(parameters, "limit", 48)?.min(64);
    let rows = library::CollectionQuery {
        kind: library::CollectionKind::SmartPlaylists {
            source: (source).into(),
            sort: sort,
            now: now(),
        },
        folder: folder,
        filter: (parameters.get("q").map(String::as_str).unwrap_or("")).to_string(),
        descending: boolean(parameters, "descending")?,
        favorites_only: false,
    }
    .rows(&products.library, offset, limit, &ReadCancellation::new())
    .await
    .map(|rows| rows.smart_playlists())
    .map_err(internal)?;
    let total = page_total(products, parameters, "smart-playlists").await?;
    Ok(
        json!({"total":total,"offset":offset,"limit":limit,"smart_playlists":rows.iter().map(|row|json!({"id":row.smart_playlist_key,"object_id":row.object_id,"name":rufin_core::playlists::smart_playlist_display_name(row),"track_count":row.track_count,"duration_ms":row.duration_millis,"artwork_count":row.artwork_bindings.len()})).collect::<Vec<_>>()}),
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
    let query = song_query(products, parameters, "smart-tracks").await?;
    let offset = number(parameters, "offset", 0)?;
    let limit = number(parameters, "limit", 48)?.min(256);
    let rows = products
        .library
        .song_rows(&query, offset, limit, &ReadCancellation::new())
        .await
        .map_err(internal)?;
    let total = song_total(products, parameters, &query).await?;
    Ok(
        json!({"total":total,"offset":offset,"limit":limit,"tracks":rows.iter().map(track_row_json).collect::<Vec<_>>()}),
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
    let rows = library::CollectionQuery {
        kind: library::CollectionKind::Albums {
            source: source,
            sort: sort,
            inline_tracks: None,
        },
        folder: folder,
        filter: (filter).to_string(),
        descending: descending,
        favorites_only: favorites,
    }
    .rows(&products.library, offset, limit, &cancellation)
    .await
    .map(|rows| rows.albums())
    .map_err(internal)?;
    let total = page_total(products, parameters, "albums").await?;
    Ok(
        json!({"total":total,"offset":offset,"limit":limit,"albums":rows.iter().map(|row| json!({
                "id":row.album_key,"object_id":row.object_id,"uri":row.media_uri,"title":row.title,"artist":row.display_artist,
                "year":row.year,"track_count":row.track_count,"duration_ms":row.duration_millis,"favorite":row.favorite,"rating":row.rating
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
    let query = song_query(products, parameters, "album-tracks").await?;
    let offset = number(parameters, "offset", 0)?;
    let limit = number(parameters, "limit", 100)?.min(128);
    let rows = products
        .library
        .song_rows(&query, offset, limit, &ReadCancellation::new())
        .await
        .map_err(internal)?;
    let disc_sections = products
        .library
        .song_disc_sections(&query, &ReadCancellation::new())
        .await
        .map_err(internal)?;
    let total = song_total(products, parameters, &query).await?;
    Ok(
        json!({"album_tracks":true,"disc_sections":disc_sections,"total":total,"offset":offset,"limit":limit,"tracks":rows.iter().map(track_row_json).collect::<Vec<_>>()}),
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
    let page = completion(products.source.browse_folder(
        source_id,
        parameters.get("parent").cloned(),
        parameters.get("folder").cloned(),
    ))
    .await?;
    Ok(json_response(
        StatusCode::OK,
        json!({"folders":page.folders.iter().map(|folder| json!({"id":folder.object_id,"name":folder.name})).collect::<Vec<_>>(),"track_ids":page.tracks}),
    ))
}
