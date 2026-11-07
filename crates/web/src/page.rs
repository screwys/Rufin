//! Library pages. A page token names a route, the source and music folder it shows and how it
//! opens; every list endpoint maps its documented parameters to one, opens it through core's
//! View and serializes the typed rows it reads.

use super::*;
use library::{Header, Item, PlaylistListRow, ReadCancellation, Row, TagKind};
use rufin_core::route::Route;
use rufin_core::settings::LibraryLayout;
use rufin_core::view::{HomeSection, OpenOptions, View};
use serde::Serialize;

/// A web page: its route, the source and music folder it shows, and how it opens.
#[derive(Clone, Deserialize, PartialEq, Serialize)]
pub(super) struct PageToken {
    pub route: Route,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<sources::SourceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    #[serde(default)]
    pub options: OpenOptions,
}

impl PageToken {
    /// Home of the selected source and music folder, which the pages the player opens keep.
    pub fn selected(products: &ProductHandles) -> Self {
        let selected = products.source.selected_library();
        Self {
            source: selected.as_ref().map(|selected| selected.source_id.clone()),
            folder: selected.and_then(|selected| selected.music_folder_object_id),
            ..Self::new(Route::Home)
        }
    }

    /// The page `route` shows of no source, opened plainly.
    pub fn new(route: Route) -> Self {
        Self {
            route,
            source: None,
            folder: None,
            options: page_options(OpenOptions::default()),
        }
    }

    /// The page `route` shows of this page's source and music folder, opened plainly.
    pub fn at(&self, route: Route) -> Self {
        Self {
            source: self.source.clone(),
            folder: self.folder.clone(),
            ..Self::new(route)
        }
    }

    pub fn json(&self) -> String {
        serde_json::to_string(self).expect("page token")
    }

    /// The list whose settings the page reads with.
    pub fn list(&self) -> LibraryListKey {
        rufin_core::view::library_list_key(&self.route, self.options.category)
    }
}

/// The options the web opens pages with: lists show as cards.
fn page_options(options: OpenOptions) -> OpenOptions {
    OpenOptions {
        layout: Some(LibraryLayout::Grid),
        ..options
    }
}

/// The page the documented parameters of `list` name; `list` is an endpoint under /api
/// (tracks, albums, albums/tracks, artists, artists/tracks, genres/tracks, playlists,
/// playlists/entries, smart-playlists, smart-playlists/tracks, home) or `page`, whose `page`
/// parameter is a token. q, sort, descending, favorites, opened, source and folder apply on top.
pub(super) fn token(
    products: &ProductHandles,
    list: &str,
    parameters: &HashMap<String, String>,
) -> Result<PageToken, Error> {
    let favorites = boolean(parameters, "favorites")?;
    let album_artists = boolean(parameters, "album_artists")?;
    let uri = |item| products.library.uri(item).unwrap_or_default();
    let route = match list {
        "page" => None,
        "home" => Some(Route::Home),
        "tracks" if favorites => Some(Route::Favorites),
        "tracks" => Some(Route::Tracks),
        "albums" => Some(Route::Albums),
        "albums/tracks" => Some(Route::AlbumDetail(uri(Item::Album(key(parameters, "id")?)))),
        "artists" if album_artists => Some(Route::AlbumArtists),
        "artists" => Some(Route::Artists),
        "artists/tracks" => {
            let artist = uri(Item::Artist {
                key: key(parameters, "id")?,
                album_artist: album_artists,
            });
            Some(if album_artists {
                Route::AlbumArtistTracks(artist)
            } else {
                Route::ArtistTracks(artist)
            })
        }
        "genres/tracks" => Some(Route::GenreDetail(key(parameters, "id")?)),
        "playlists" | "smart-playlists" => Some(match parameters.get("playlist_folder") {
            Some(_) => Route::PlaylistFolder {
                key: key(parameters, "playlist_folder")?,
                smart: list == "smart-playlists",
            },
            None if list == "playlists" => Route::Playlists,
            None => Route::SmartPlaylists,
        }),
        "playlists/entries" => Some(Route::PlaylistDetail(key(parameters, "id")?)),
        "smart-playlists/tracks" => Some(Route::SmartPlaylistDetail(key(parameters, "id")?)),
        _ => return Err(bad_request("Unknown library view")),
    };
    let mut token = match route {
        Some(route) => PageToken::new(route),
        None => serde_json::from_str(required(parameters, "page")?).map_err(bad_request)?,
    };
    // The web shows lists as cards.
    token.options.layout = Some(LibraryLayout::Grid);
    if let Some(source) = parameters.get("source") {
        token.source = (!source.is_empty()).then(|| sources::SourceId::new(source));
    }
    if let Some(folder) = parameters.get("folder") {
        token.folder = Some(folder.clone()).filter(|folder| !folder.is_empty());
    }
    let list = token.list();
    let options = &mut token.options;
    if let Some(filter) = parameters.get("q") {
        options.filter.clone_from(filter);
    }
    if parameters.contains_key("sort") {
        options.sort = sort(parameters, list)?;
    }
    if parameters.contains_key("descending") {
        options.descending = Some(boolean(parameters, "descending")?);
    }
    options.favorites_only |= favorites && token.route != Route::Favorites;
    if let Some(opened) = parameters.get("opened") {
        options.smart_opened = Some(
            opened
                .parse()
                .map_err(|_| bad_request("Invalid opened time"))?,
        );
    }
    Ok(token)
}

/// The source and music folder a page or request shows, as a selection of its own.
pub(super) async fn scope(
    products: &ProductHandles,
    source: Option<&sources::SourceId>,
    folder: Option<&str>,
) -> Result<Option<rufin_core::runtime::SelectedLibrary>, Error> {
    let Some(source) = source else {
        return Ok(None);
    };
    let (source_key, music_folder_key) = catalog::scope(products, source.as_str(), folder).await?;
    Ok(Some(rufin_core::runtime::SelectedLibrary {
        source_id: source.clone(),
        source_key,
        music_folder_key,
        music_folder_object_id: folder.map(str::to_owned),
        music_folders: Arc::from([]),
    }))
}

/// What playing `token`'s page whole queues at `placement`, without reading its rows.
pub(super) async fn play(
    products: &ProductHandles,
    token: &PageToken,
    placement: rufin_core::playback::QueuePlacement,
) -> Result<rufin_core::playback::PlayRequest, Error> {
    let selected = scope(products, token.source.as_ref(), token.folder.as_deref()).await?;
    products
        .source
        .play_route(
            &token.route,
            selected.as_ref(),
            &token.options,
            placement,
            false,
            &ReadCancellation::new(),
        )
        .await
        .map_err(read_error)
}

/// An open page and the rows it read at `offset`.
pub(super) struct Opened {
    pub token: PageToken,
    pub view: Arc<View>,
    pub header: Option<Header>,
    pub offset: usize,
    pub rows: Vec<Row>,
    /// What committed while it read, as an update frame shows it on this page; the client
    /// shows it once it shows the rows.
    pub missed: Option<Value>,
}

/// Opens `token` and reads `limit` rows from `offset`.
pub(super) async fn open(
    products: &ProductHandles,
    token: PageToken,
    offset: usize,
    limit: usize,
) -> Result<Opened, Error> {
    let selected = scope(products, token.source.as_ref(), token.folder.as_deref()).await?;
    let reading = products.source.reading();
    let cancel = ReadCancellation::new();
    let opened = products
        .source
        .open(
            token.route.clone(),
            selected,
            token.options.clone(),
            library::RouteSeedWindow::position(offset),
            cards(&token.route),
            &cancel,
        )
        .await
        .map_err(read_error)?;
    let end = offset.saturating_add(limit).min(opened.view.len());
    let start = offset.min(end);
    let seeded = opened.first..opened.first + opened.rows.len();
    let rows = if seeded.start <= start && end <= seeded.end {
        opened.rows[start - seeded.start..end - seeded.start].to_vec()
    } else {
        opened
            .view
            .rows(start..end, &cancel)
            .await
            .map_err(read_error)?
    };
    // A smart playlist page keeps the time it opened at.
    let token = PageToken {
        options: opened.view.options().clone(),
        ..token
    };
    let missed = reading.missed().map(|missed| {
        let mut value = json!({});
        show_changed(products, &mut value, Some(&opened.view), &missed);
        value
    });
    Ok(Opened {
        token,
        view: opened.view,
        header: opened.header,
        offset: start,
        rows,
        missed: missed.filter(|value| value.as_object().is_some_and(|value| !value.is_empty())),
    })
}

pub(super) fn read_error(error: library::LibraryError) -> Error {
    match error {
        library::LibraryError::InvalidRequest(message) => bad_request(message),
        error => internal(error),
    }
}

/// Whether a route lists collections, which show as cards with their pictures.
pub(super) fn cards(route: &Route) -> bool {
    matches!(
        route,
        Route::Albums
            | Route::Artists
            | Route::AlbumArtists
            | Route::Genres
            | Route::Moods
            | Route::Playlists
            | Route::SmartPlaylists
            | Route::PlaylistFolder { .. }
    )
}

/// The name a page's rows go under in its JSON.
fn rows_name(route: &Route) -> &'static str {
    match route {
        Route::Albums => "albums",
        Route::Artists | Route::AlbumArtists => "artists",
        Route::Playlists | Route::PlaylistFolder { smart: false, .. } => "playlists",
        Route::SmartPlaylists | Route::PlaylistFolder { smart: true, .. } => "smart_playlists",
        Route::PlaylistDetail(_) => "entries",
        _ => "tracks",
    }
}

/// The fields a page's rows show: a card list's grid fields, else its list's columns.
pub(super) fn fields(products: &ProductHandles, token: &PageToken) -> Vec<LibraryField> {
    let list = products.settings.load().library_list(token.list());
    if cards(&token.route) {
        list.grid_fields
    } else {
        list.row_fields
    }
}

/// A row as the API lists it, by kind; with `art`, rows read with their pictures carry
/// previews of them. The pages its links open keep the source and music folder of `scope`.
pub(super) fn row_json(
    products: &ProductHandles,
    row: &Row,
    art: bool,
    scope: &PageToken,
) -> Value {
    let previews = |bindings: &[Vec<u8>]| {
        if art {
            artwork_previews(products, bindings)
        } else {
            Vec::new()
        }
    };
    let source = |uri: &str| library::source_entity_parts(uri).map(|(source, _, _)| source);
    let mut value = match row {
        Row::Track(track) | Row::AlbumTrack { track, .. } => {
            let mut value = json!({"kind":"track","uri":track.media_uri,"object_id":track.object_id,"title":track.title,"artist":track.artist,"album":track.album,
                "duration_ms":track.duration_millis,"track_number":track.track_number,"disc_number":track.disc_number,
                "year":track.year,"favorite":track.state.favorite,"rating":track.state.rating,"source_id":track.source_id,"media":media::media_links(scope, &track.links, &track.album)});
            if let Some(entry) = &track.entry {
                value["id"] = json!(entry.key);
                value["position"] = json!(entry.position);
            }
            if let Some(binding) = &track.artwork_binding {
                value["artwork_previews"] = json!(previews(std::slice::from_ref(binding)));
            }
            value
        }
        Row::Album(album) | Row::AlbumHeading(album) => {
            json!({"kind":"album","id":album.album_key,"object_id":album.object_id,"uri":album.media_uri,"source":source(&album.media_uri),"title":album.title,"artist":album.artist,
                "year":album.year,"track_count":album.summary.track_count,"duration_ms":album.summary.duration_millis,"favorite":album.state.favorite,"rating":album.state.rating,
                "artwork_previews":previews(album.artwork_binding.as_slice())})
        }
        Row::Artist(artist) => {
            json!({"kind":"artist","id":artist.artist_key,"object_id":artist.object_id,"album_artists":artist.album_artist,"uri":artist.media_uri,"source":source(&artist.media_uri),"name":artist.name,
                "favorite":artist.state.favorite,"album_count":artist.summary.album_count,"track_count":artist.summary.track_count,"duration_ms":artist.summary.duration_millis,
                "artwork_previews":previews(artist.artwork_binding.as_slice())})
        }
        Row::Tag(tag) => {
            let artwork = tag
                .artwork_binding
                .as_ref()
                .map_or(tag.representative_artwork.as_slice(), std::slice::from_ref);
            json!({"kind":if tag.kind == TagKind::Genre {"genre"} else {"mood"},"id":tag.key,"object_id":tag.object_id,"source":tag.source_id,"name":tag.name,
                "track_count":tag.summary.track_count,"duration_ms":tag.summary.duration_millis,"artwork_count":artwork.len(),"artwork_previews":previews(artwork)})
        }
        Row::Playlist(PlaylistListRow::Playlist(playlist)) => {
            json!({"kind":"playlist","id":playlist.playlist_key,"object_id":playlist.object_id,"source":playlist.source_id,"name":playlist.name,"writable":playlist.writable,
                "metadata_writable":playlist.metadata_writable,
                "track_count":playlist.track_count,"duration_ms":playlist.duration_millis,"artwork_count":playlist.artwork_binding.as_slice().len(),"artwork_previews":previews(playlist.artwork_binding.as_slice())})
        }
        Row::Playlist(PlaylistListRow::SmartPlaylist(smart)) => {
            json!({"kind":"smart-playlist","id":smart.smart_playlist_key,"object_id":smart.object_id,"source":None::<String>,
                "name":rufin_core::playlists::smart_playlist_display_name(&smart.object_id, &smart.name),"track_count":smart.track_count,"duration_ms":smart.duration_millis,
                "artwork_count":smart.artwork_binding.as_slice().len(),"artwork_previews":previews(smart.artwork_binding.as_slice())})
        }
        Row::Playlist(PlaylistListRow::Folder(folder)) => {
            json!({"kind":"playlist-folder","id":folder.folder_key,"object_id":folder.object_id,"name":folder.name,"smart":folder.smart,
                "track_count":folder.track_count,"duration_ms":folder.duration_millis,"artwork_previews":previews(folder.artwork_binding.as_slice())})
        }
        Row::Folder(folder) => {
            json!({"kind":"folder","id":folder.object_id,"name":(!folder.name.is_empty()).then_some(&folder.name),"track_count":folder.summary.track_count})
        }
    };
    if let Some(pin) = row.pin() {
        value["pin"] = json!(pin);
    }
    value
}

/// What a row shows for `fields`, as `[{field, text}]`.
pub(super) fn field_texts(row: &Row, fields: &[LibraryField]) -> Value {
    fields
        .iter()
        .map(|field| {
            json!({"field":field,"text":rufin_core::settings::presentation::row_field(row, *field)})
        })
        .collect()
}

/// A page's title: a collection's name, a playlist folder's, or its list's.
pub(super) fn title(
    products: &ProductHandles,
    token: &PageToken,
    header: Option<&Header>,
) -> String {
    match (&token.route, header) {
        (_, Some(Header::Album(album))) => album.row.title.clone(),
        (_, Some(Header::Artist(artist))) => artist.row.name.clone(),
        (_, Some(Header::Tag(tag))) => tag.name.clone(),
        (_, Some(Header::Playlist(playlist))) => playlist.name.clone(),
        (_, Some(Header::Smart(smart))) => {
            rufin_core::playlists::smart_playlist_display_name(&smart.object_id, &smart.name)
        }
        (Route::PlaylistFolder { key, smart }, None) => products
            .library
            .playlist_folder_choices(*smart, "")
            .into_iter()
            .find_map(|(folder, name)| (folder == *key).then_some(name))
            .unwrap_or_default(),
        (route, None) => rufin_core::settings::SidebarRouteItem::all()
            .into_iter()
            .map(rufin_core::route::sidebar_route_descriptor)
            .find(|descriptor| descriptor.root_route == *route)
            .map(|descriptor| localization::tr(descriptor.title))
            .unwrap_or_default(),
    }
}

/// The page a page's Back button opens.
pub(super) fn parent(token: &PageToken) -> Option<PageToken> {
    Some(token.at(match &token.route {
        Route::AlbumDetail(_) => Route::Albums,
        Route::ArtistTracks(_) => Route::Artists,
        Route::AlbumArtistTracks(_) => Route::AlbumArtists,
        Route::GenreDetail(_) => Route::Home,
        Route::PlaylistDetail(_) | Route::PlaylistFolder { smart: false, .. } => Route::Playlists,
        Route::SmartPlaylistDetail(_) | Route::PlaylistFolder { smart: true, .. } => {
            Route::SmartPlaylists
        }
        _ => return None,
    }))
}

/// The navigation entry a page belongs to.
pub(super) fn section(route: &Route) -> &'static str {
    match route {
        Route::Home | Route::GenreDetail(_) => "Home",
        Route::Favorites => "Favorites",
        Route::Albums | Route::AlbumDetail(_) => "Albums",
        Route::Artists
        | Route::AlbumArtists
        | Route::ArtistTracks(_)
        | Route::AlbumArtistTracks(_) => "Artists",
        Route::Playlists
        | Route::PlaylistDetail(_)
        | Route::PlaylistFolder { smart: false, .. } => "Playlists",
        Route::SmartPlaylists
        | Route::SmartPlaylistDetail(_)
        | Route::PlaylistFolder { smart: true, .. } => "SmartPlaylists",
        _ => "Tracks",
    }
}

/// A page's JSON: its rows under its list's name, the order it read in, and with `total` its
/// length. Track pages carry their columns; album pages their disc sections; playlist pages
/// their name.
pub(super) fn json(products: &ProductHandles, opened: &Opened, limit: usize, total: bool) -> Value {
    let (sort, descending) = opened.view.order();
    let fields = fields(products, &opened.token);
    let rows = opened
        .rows
        .iter()
        .map(|row| {
            let mut value = row_json(products, row, true, &opened.token);
            value["fields"] = field_texts(row, &fields);
            value
        })
        .collect::<Vec<_>>();
    let mut value = json!({
        "activity_sensitive":opened.view.reads_activity(),"sort":sort_name(opened.token.list(), sort),"descending":descending,
        "total":total.then_some(opened.view.len()),"offset":opened.offset,"limit":limit,"page":opened.token,
    });
    value[rows_name(&opened.token.route)] = rows.into();
    if let Some(missed) = &opened.missed {
        value["missed"] = missed.clone();
    }
    if !cards(&opened.token.route) {
        value["columns"] = json!(fields);
    }
    match &opened.token.route {
        Route::AlbumDetail(_) => {
            value["album_tracks"] = true.into();
            value["disc_sections"] = json!(opened.view.disc_sections());
        }
        Route::PlaylistDetail(_) => {
            value["name"] = json!(match &opened.header {
                Some(Header::Playlist(playlist)) => Some(&playlist.name),
                _ => None,
            });
        }
        _ => {}
    }
    value
}

/// Home's sections in core's order and titles, or only `block`'s.
pub(super) fn home_sections(
    view: &View,
    block: Option<library::HomeBlockKind>,
) -> Vec<HomeSection> {
    view.home_sections()
        .unwrap_or_default()
        .into_iter()
        .filter(|section| block.is_none_or(|block| section.block() == Some(block)))
        .collect()
}

/// A Home section's rows, each with the title Home gives it.
pub(super) fn home_rows(section: &HomeSection) -> Vec<(Row, String, &'static [LibraryField])> {
    use rufin_core::settings::layout::{HOME_ALBUM_FIELDS, HOME_TRACK_FIELDS};
    use rufin_core::view::HomeItem;
    match section {
        HomeSection::Showcase(album) => vec![(
            Row::Album(album.album.clone()),
            album.title.clone(),
            &HOME_ALBUM_FIELDS,
        )],
        HomeSection::Genres(genres) => genres
            .iter()
            .map(|genre| (Row::Tag(genre.clone()), genre.name.clone(), &[][..]))
            .collect(),
        _ => section
            .items()
            .into_iter()
            .map(|item| match item {
                HomeItem::Track(track) => {
                    (Row::Track(track.track), track.title, &HOME_TRACK_FIELDS[..])
                }
                HomeItem::Album(album) => {
                    (Row::Album(album.album), album.title, &HOME_ALBUM_FIELDS[..])
                }
            })
            .collect(),
    }
}

/// Home's JSON: `{sections: [{block, title, items}]}`.
pub(super) fn home_json(
    products: &ProductHandles,
    scope: &PageToken,
    sections: &[HomeSection],
) -> Value {
    let sections = sections
        .iter()
        .map(|section| {
            let items = home_rows(section)
                .into_iter()
                .map(|(row, title, fields)| {
                    let mut value = row_json(products, &row, !matches!(row, Row::Tag(_)), scope);
                    value[if matches!(row, Row::Tag(_)) {
                        "name"
                    } else {
                        "title"
                    }] = title.into();
                    value["fields"] = field_texts(&row, fields);
                    value
                })
                .collect::<Vec<_>>();
            let mut value = json!({"title":section.title(),"items":items});
            if let Some(block) = section.block() {
                value["block"] = json!(block);
            }
            value
        })
        .collect::<Vec<_>>();
    json!({"sections":sections})
}
