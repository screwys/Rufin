use super::*;
use askama::Template;
use library::{PlaylistListRow, Row};
use rufin_core::menu::Menu;

#[path = "../../../web/gettext.rs"]
mod web_gettext;

/// The web client's scripts and styles.
#[derive(rust_embed::Embed)]
#[folder = "../../web"]
#[include = "*.js"]
#[include = "*.css"]
struct WebFiles;

/// The action icons GTK uses, by their file names.
#[derive(rust_embed::Embed)]
#[folder = "../../resources/icons/hicolor/scalable/actions"]
struct Icons;

async fn translations() -> Response<Body> {
    static MESSAGES: std::sync::OnceLock<std::collections::BTreeSet<String>> =
        std::sync::OnceLock::new();
    let messages = MESSAGES.get_or_init(|| {
        WebFiles::iter()
            .filter(|path| path.ends_with(".js"))
            .filter_map(|path| WebFiles::get(&path))
            .flat_map(|file| web_gettext::messages(&String::from_utf8_lossy(&file.data)))
            .collect()
    });
    let translated: std::collections::BTreeMap<_, _> = messages
        .iter()
        .map(|message| (message, localization::tr(message)))
        .collect();
    Response::builder()
        .header("content-type", "text/javascript; charset=utf-8")
        .header("cache-control", "no-store")
        .body(Body::from(format!(
            "export const messages = {};",
            serde_json::to_string(&translated).expect("translated strings")
        )))
        .expect("translation headers")
}

#[derive(Template)]
#[template(path = "index.html")]
struct Index<'a> {
    pub version: &'static str,
    pub page: &'a Page,
}

/// What a page's forms carry back: the browser session's form token and the page to return to.
pub(super) struct Form {
    csrf: String,
    return_to: String,
}

/// A field a list shows: a card's label or a track list's column, with its header's title,
/// class and icon.
struct Column {
    field: LibraryField,
    title: String,
    class: &'static str,
    icon: &'static str,
}

impl Column {
    fn new(field: LibraryField) -> Self {
        let (class, icon) = match field {
            LibraryField::RowIndex => ("number", ""),
            LibraryField::Album => ("album", ""),
            LibraryField::Duration => ("duration", "rufin-preferences-system-time-symbolic"),
            LibraryField::Favorite => ("favorite", "rufin-heart-outline-symbolic"),
            LibraryField::Tools => ("actions", ""),
            _ => ("", ""),
        };
        Self {
            field,
            title: rufin_core::settings::presentation::library_column_title(field)
                .map(localization::tr)
                .unwrap_or_default(),
            class,
            icon,
        }
    }
}

/// A row as the page shows it.
struct Shown<'a> {
    row: Row,
    /// Its title: Home rows have their own.
    title: String,
    /// Its JSON for the script: the row, its menu and the page it opens.
    data: String,
    fields: Vec<(LibraryField, String)>,
    menu: Menu,
    /// The link it opens; empty for tracks.
    href: String,
    /// What its Play buttons post: the list from it, or the page it opens.
    play: String,
    previews: Vec<String>,
    /// Its position in its list.
    index: usize,
    menu_id: String,
    form: &'a Form,
}

/// A row's JSON for the page's script: what the API lists for it, with `art` its pictures'
/// previews, its menu, the page it opens and the page it plays, as core gives them for a row of
/// `view`'s page or for a row outside a page; each keeps the source and music folder of
/// `scope`. A track has no page it plays.
pub(super) fn row_data(
    products: &ProductHandles,
    row: &Row,
    art: bool,
    menu: &Menu,
    scope: &page::PageToken,
    view: Option<&rufin_core::view::View>,
) -> Value {
    use rufin_core::route::Route;
    use rufin_core::view::PlayTarget;
    let mut value = page::row_json(products, row, art, scope);
    value["menu"] = json!(menu);
    let (route, target) = match view {
        Some(view) => (view.row_route(row), view.row_target(row)),
        None => (Route::of_row(row), PlayTarget::of_row(row)),
    };
    if let Some(route) = route {
        value["page"] = json!(scope.at(web_page(route)));
    }
    if let Some(PlayTarget::Route(route)) = target {
        value["play"] = json!(scope.at(route));
    }
    value
}

/// The page the web draws for `route`: an artist's tracks for the artist page.
pub(super) fn web_page(route: rufin_core::route::Route) -> rufin_core::route::Route {
    use rufin_core::route::Route;
    match route {
        Route::ArtistDetail(uri) => Route::ArtistTracks(uri),
        Route::AlbumArtistDetail(uri) => Route::AlbumArtistTracks(uri),
        route => route,
    }
}

impl<'a> Shown<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        products: &ProductHandles,
        settings: &rufin_core::settings::app::Settings,
        list: &page::PageToken,
        view: Option<&rufin_core::view::View>,
        row: Row,
        art: bool,
        title: Option<String>,
        fields: &[LibraryField],
        index: usize,
        menu_id: String,
        form: &'a Form,
    ) -> Self {
        let menu = rufin_core::menu::menu(&products.source, settings, &row);
        let mut data = row_data(products, &row, art, &menu, list, view);
        if let Some(title) = &title {
            data[if matches!(row, Row::Tag(_)) {
                "name"
            } else {
                "title"
            }] = json!(title);
        }
        data["index"] = json!(index);
        let href = match &data["page"] {
            Value::Null => String::new(),
            page => link(&HashMap::from([("page".to_owned(), page.to_string())])),
        };
        // A track names its place on its list, anchored on the track and entry it shows, and
        // core's row rule decides what that plays; another row plays the page core gives it.
        let play = match (&row, &data["play"]) {
            (Row::Track(track) | Row::AlbumTrack { track, .. }, _) => json!({
                "page": list,
                "index": index,
                "anchor_uri": track.media_uri,
                "anchor_entry": track.entry.as_ref().map(|entry| entry.key),
            }),
            (_, Value::Null) => json!({}),
            (_, page) => json!({ "page": page }),
        }
        .to_string();
        let previews = data["artwork_previews"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|preview| preview.as_str().unwrap_or_default().to_owned())
            .collect();
        Self {
            title: title.unwrap_or_else(|| {
                rufin_core::settings::presentation::row_field(&row, LibraryField::Title)
            }),
            fields: fields
                .iter()
                .map(|field| {
                    (
                        *field,
                        rufin_core::settings::presentation::row_field(&row, *field),
                    )
                })
                .collect(),
            menu,
            data: data.to_string(),
            href,
            play,
            previews,
            index,
            menu_id,
            form,
            row,
        }
    }

    fn kind(&self) -> &'static str {
        match &self.row {
            Row::Track(_) | Row::AlbumTrack { .. } => "track",
            Row::Album(_) | Row::AlbumHeading(_) => "album",
            Row::Artist(_) => "artist",
            Row::Tag(tag) if tag.kind == library::TagKind::Genre => "genre",
            Row::Tag(_) => "mood",
            Row::Playlist(PlaylistListRow::Playlist(_)) => "playlist",
            Row::Playlist(PlaylistListRow::SmartPlaylist(_)) => "smart-playlist",
            Row::Playlist(PlaylistListRow::Folder(_)) => "playlist-folder",
            Row::Folder(_) => "folder",
        }
    }

    fn uri(&self) -> &str {
        match &self.row {
            Row::Track(track) | Row::AlbumTrack { track, .. } => &track.media_uri,
            Row::Album(album) | Row::AlbumHeading(album) => &album.media_uri,
            Row::Artist(artist) => &artist.media_uri,
            Row::Tag(tag) => &tag.media_uri,
            _ => "",
        }
    }

    fn favorite(&self) -> bool {
        match &self.row {
            Row::Track(track) | Row::AlbumTrack { track, .. } => track.state.favorite,
            Row::Album(album) | Row::AlbumHeading(album) => album.state.favorite,
            Row::Artist(artist) => artist.state.favorite,
            _ => false,
        }
    }

    /// What its heart posts: the opposite favorite.
    fn favorite_payload(&self) -> String {
        json!({"kind":self.kind(),"uri":self.uri(),"favorite":!self.favorite()}).to_string()
    }

    fn artist(&self) -> &str {
        match &self.row {
            Row::Track(track) | Row::AlbumTrack { track, .. } => &track.artist,
            Row::Album(album) | Row::AlbumHeading(album) => &album.artist,
            _ => "",
        }
    }

    fn track_number(&self) -> i64 {
        match &self.row {
            Row::Track(track) | Row::AlbumTrack { track, .. } => track.track_number,
            _ => 0,
        }
    }

    fn year(&self) -> i64 {
        match &self.row {
            Row::Album(album) | Row::AlbumHeading(album) => album.year.unwrap_or_default(),
            _ => 0,
        }
    }

    fn summary(&self) -> Option<library::CollectionShown<'_>> {
        self.row.collection()
    }

    fn track_count(&self) -> String {
        localization::track_count_text(
            self.summary()
                .map_or(0, |shown| shown.track_count.max(0) as u64),
        )
    }

    fn long_duration(&self) -> String {
        rufin_core::settings::presentation::format_duration_units(
            self.summary()
                .map_or(0, |shown| (shown.duration_millis.max(0) / 1000) as u32),
        )
    }

    /// The text the row shows for `field`.
    fn cell(&self, field: &LibraryField) -> &str {
        self.fields
            .iter()
            .find(|(shown, _)| shown == field)
            .map_or("", |(_, text)| text.as_str())
    }

    /// The fields with text, in its list's order, and its rating, which a change can fill.
    fn labels(&self) -> Vec<(LibraryField, &str)> {
        self.fields
            .iter()
            .filter(|(field, text)| !text.is_empty() || *field == LibraryField::UserRating)
            .map(|(field, text)| (*field, text.as_str()))
            .collect()
    }

    /// Whether its picture is a mosaic of its tracks' covers.
    fn mosaic(&self) -> bool {
        self.data_count() > 1
    }

    fn data_count(&self) -> usize {
        self.summary().map_or(0, |shown| shown.artwork.len())
    }

    fn preview_tiles(&self) -> Vec<&str> {
        let count = self.previews.len().max(1);
        (0..if self.mosaic() { 4 } else { 1 })
            .map(|part| self.previews.get(part % count).map_or("", String::as_str))
            .collect()
    }

    /// Its picture for pages without scripts.
    fn artwork(&self) -> String {
        let query = match &self.row {
            Row::Playlist(PlaylistListRow::Playlist(playlist)) => {
                ("playlist", playlist.playlist_key.raw().to_string())
            }
            Row::Playlist(PlaylistListRow::SmartPlaylist(smart)) => {
                ("smart_playlist", smart.smart_playlist_key.raw().to_string())
            }
            _ => ("uri", self.uri().to_owned()),
        };
        format!(
            "/api/artwork?{}",
            url::form_urlencoded::Serializer::new(String::new())
                .append_pair(query.0, &query.1)
                .finish()
        )
    }
}

/// A showcase: Home's, or a detail page's header. A gradient band with the cover over its
/// edge, the title, the facts, the genres and the buttons.
struct Head<'a> {
    row: Shown<'a>,
    home: bool,
    /// An album's artist, under its title.
    artist: bool,
    facts: Vec<(&'static str, String)>,
    /// Each genre's name and the page it opens.
    genres: Vec<(String, String)>,
    /// The band's gradient stops, from the cover's preview.
    band: String,
}

impl<'a> Head<'a> {
    fn new(
        products: &ProductHandles,
        settings: &rufin_core::settings::app::Settings,
        scope: &page::PageToken,
        row: Row,
        title: Option<String>,
        home: bool,
        form: &'a Form,
    ) -> Self {
        use library::GenreKey;
        let row = Shown::new(
            products,
            settings,
            scope,
            None,
            row,
            true,
            title,
            &[],
            0,
            "media-menu-showcase".to_owned(),
            form,
        );
        let genre = |(name, key): (&str, GenreKey)| {
            let page = scope.at(rufin_core::route::Route::GenreDetail(key));
            (name.to_owned(), page.json())
        };
        let (facts, genres): (Vec<_>, Vec<_>) = match &row.row {
            Row::Album(album) | Row::AlbumHeading(album) => (
                [
                    (row.year() > 0)
                        .then(|| ("rufin-x-office-calendar-symbolic", row.year().to_string())),
                    Some(("rufin-tracks-symbolic", row.track_count())),
                    Some((
                        "rufin-preferences-system-time-symbolic",
                        row.long_duration(),
                    )),
                ]
                .into_iter()
                .flatten()
                .collect(),
                album
                    .links
                    .genres
                    .iter()
                    .map(|link| genre((&link.name, link.genre_key)))
                    .collect(),
            ),
            Row::Artist(artist) => (
                vec![
                    (
                        "rufin-albums-symbolic",
                        localization::album_count_text(artist.summary.album_count.max(0) as u64),
                    ),
                    ("rufin-tracks-symbolic", row.track_count()),
                ],
                Vec::new(),
            ),
            other => (
                vec![
                    ("rufin-tracks-symbolic", row.track_count()),
                    (
                        "rufin-preferences-system-time-symbolic",
                        row.long_duration(),
                    ),
                ],
                match other {
                    Row::Playlist(PlaylistListRow::Playlist(playlist)) => playlist
                        .genres
                        .iter()
                        .map(|link| genre((&link.name, link.genre_key)))
                        .collect(),
                    _ => Vec::new(),
                },
            ),
        };
        let allow_external = settings.application.external_metadata_enabled;
        let [first, middle, last] = row
            .summary()
            .and_then(|shown| shown.artwork.first().copied())
            .and_then(|binding| {
                products
                    .artwork
                    .preview(artwork::ArtworkBinding::opaque(binding), allow_external)
            })
            .map_or_else(
                || artwork::showcase_band(&[], 0, 0, 0),
                |preview| {
                    let image = &preview.image;
                    artwork::showcase_band(
                        image.rgba(),
                        image.width(),
                        image.height(),
                        image.row_stride(),
                    )
                },
            )
            .map(|[r, g, b]| format!("rgb({r} {g} {b})"));
        Self {
            artist: matches!(row.row, Row::Album(_) | Row::AlbumHeading(_)),
            row,
            home,
            facts,
            genres,
            band: format!("--band-start: {first}; --band-middle: {middle}; --band-end: {last}"),
        }
    }
}

/// The header of the page `header` opens.
fn header_row(header: &library::Header) -> Row {
    match header {
        library::Header::Album(album) => Row::Album(album.row.clone()),
        library::Header::Artist(artist) => Row::Artist(artist.row.clone()),
        library::Header::Tag(tag) => Row::Tag(tag.clone()),
        library::Header::Playlist(playlist) => {
            Row::Playlist(PlaylistListRow::Playlist((**playlist).clone()))
        }
        library::Header::Smart(smart) => {
            Row::Playlist(PlaylistListRow::SmartPlaylist((**smart).clone()))
        }
    }
}

#[derive(Template)]
#[template(path = "showcase.html")]
struct ShowcaseFragment<'a> {
    head: Option<Head<'a>>,
}

/// Home's showcase at `showcase`, as the step that landed there answers it.
pub(super) fn showcase(
    products: &ProductHandles,
    scope: &page::PageToken,
    showcase: Option<&library::HomeShowcase>,
    form: &Form,
) -> Result<String, Error> {
    let settings = products.settings.load();
    ShowcaseFragment {
        head: showcase.map(|showcase| {
            Head::new(
                products,
                &settings,
                scope,
                Row::Album(showcase.current.album.clone()),
                Some(showcase.current.title.clone()),
                true,
                form,
            )
        }),
    }
    .render()
    .map_err(internal)
}

#[derive(Template)]
#[template(path = "library.html")]
struct Library<'a> {
    /// The page's header, with its first rows.
    head: Option<Head<'a>>,
    rows: Vec<Shown<'a>>,
    /// A track list's columns: its list's row fields. Track rows carry no artwork, so an
    /// Image field shows no column.
    columns: Vec<Column>,
    cards: bool,
    album_tracks: bool,
    disc_headers: HashMap<usize, String>,
    /// The page as the script reads it.
    page: String,
    title: String,
    back: String,
    section: &'static str,
    sort: &'static str,
    descending: bool,
    sorts: String,
    total: String,
    name: String,
    play: bool,
    writable: bool,
    playlists: bool,
    /// What committed while the page read, as JSON; empty when nothing did.
    missed: String,
}

impl Library<'_> {
    fn disc_heading(&self, index: &usize) -> Option<&str> {
        self.disc_headers.get(index).map(String::as_str)
    }
}

/// An open list page as HTML.
pub(super) fn library(
    products: &ProductHandles,
    opened: &page::Opened,
    total: bool,
    form: &Form,
) -> Result<String, Error> {
    let settings = products.settings.load();
    let token = &opened.token;
    let fields = page::fields(products, token);
    let cards = page::cards(&token.route);
    let rows = opened
        .rows
        .iter()
        .enumerate()
        .map(|(position, row)| {
            let index = opened.offset + position;
            Shown::new(
                products,
                &settings,
                token,
                Some(&opened.view),
                row.clone(),
                true,
                None,
                &fields,
                index,
                format!("media-menu-library-{index}"),
                form,
            )
        })
        .collect();
    let (sort, descending) = opened.view.order();
    let list = token.list();
    let header = opened.header.as_ref();
    Library {
        head: header.filter(|_| opened.offset == 0).map(|header| {
            Head::new(
                products,
                &settings,
                token,
                header_row(header),
                Some(page::title(products, token, Some(header))),
                false,
                form,
            )
        }),
        rows,
        columns: fields
            .iter()
            .filter(|field| !cards && **field != LibraryField::Image)
            .map(|field| Column::new(*field))
            .collect(),
        cards,
        album_tracks: matches!(token.route, rufin_core::route::Route::AlbumDetail(_)),
        disc_headers: opened
            .view
            .disc_sections()
            .iter()
            .map(|(start, disc)| {
                let title = if *disc > 0 {
                    localization::tr("Disc {number}").replace("{number}", &disc.to_string())
                } else {
                    localization::tr("Unknown disc")
                };
                (*start as usize, title)
            })
            .collect(),
        page: token.json(),
        title: page::title(products, token, header),
        back: page::parent(token).map_or_else(String::new, |parent| parent.json()),
        section: page::section(&token.route),
        sort: sort_name(list, sort),
        descending,
        sorts: json!(translated_sorts(list)).to_string(),
        total: total
            .then(|| opened.view.len().to_string())
            .unwrap_or_default(),
        name: match header {
            Some(library::Header::Playlist(playlist)) => playlist.name.clone(),
            _ => String::new(),
        },
        // A page with a header plays from its header.
        play: opened.view.members().is_some() && header.is_none(),
        writable: matches!(header, Some(library::Header::Playlist(playlist)) if playlist.writable),
        playlists: token.route == rufin_core::route::Route::Playlists,
        missed: missed_text(opened),
    }
    .render()
    .map_err(internal)
}

struct Section<'a> {
    block: String,
    title: String,
    /// The showcase's header; other sections show rows.
    head: Option<Head<'a>>,
    rows: Vec<Shown<'a>>,
    /// The rows each column shows: two rows, or one card.
    column_rows: usize,
}

#[derive(Template)]
#[template(path = "home.html")]
struct Home<'a> {
    sections: Vec<Section<'a>>,
    page: String,
    title: String,
    /// What committed while Home read, as JSON; empty when nothing did.
    missed: String,
}

/// What committed while `opened` read, as its page's `data-missed` carries it.
fn missed_text(opened: &page::Opened) -> String {
    opened
        .missed
        .as_ref()
        .map(Value::to_string)
        .unwrap_or_default()
}

/// Home's `sections` of `opened` as HTML.
pub(super) fn home(
    products: &ProductHandles,
    opened: &page::Opened,
    sections: &[rufin_core::view::HomeSection],
    form: &Form,
) -> Result<String, Error> {
    let token = &opened.token;
    let settings = products.settings.load();
    Home {
        sections: sections
            .iter()
            .enumerate()
            .map(|(number, section)| {
                let block = section
                    .block()
                    .and_then(|block| serde_json::to_value(block).ok())
                    .and_then(|block| block.as_str().map(str::to_owned))
                    .unwrap_or_default();
                let id = if block.is_empty() {
                    format!("provider-{number}")
                } else {
                    block.clone()
                };
                if let rufin_core::view::HomeSection::Showcase(showcase) = section {
                    return Section {
                        title: section.title(),
                        head: Some(Head::new(
                            products,
                            &settings,
                            token,
                            Row::Album(showcase.current.album.clone()),
                            Some(showcase.current.title.clone()),
                            true,
                            form,
                        )),
                        rows: Vec::new(),
                        column_rows: 1,
                        block,
                    };
                }
                Section {
                    title: section.title(),
                    head: None,
                    rows: page::home_rows(section)
                        .into_iter()
                        .enumerate()
                        .map(|(index, (row, title, fields))| {
                            Shown::new(
                                products,
                                &settings,
                                token,
                                Some(&opened.view),
                                row,
                                true,
                                Some(title),
                                fields,
                                index,
                                format!("media-menu-home-{id}-{index}"),
                                form,
                            )
                        })
                        .collect(),
                    column_rows: if section.shows_rows() { 2 } else { 1 },
                    block,
                }
            })
            .collect(),
        page: token.json(),
        title: page::title(products, token, None),
        missed: missed_text(opened),
    }
    .render()
    .map_err(internal)
}

/// A library response: HTML for the web page, else JSON.
pub(super) fn page(
    headers: &hyper::HeaderMap,
    json: impl FnOnce() -> Value,
    html: impl FnOnce(&Form) -> Result<String, Error>,
) -> Result<Response<Body>, Error> {
    let mut response = if headers
        .get("HX-Request")
        .is_some_and(|value| value == "true")
    {
        let form = Form {
            csrf: headers
                .get("x-rufin-csrf")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned(),
            return_to: "/".into(),
        };
        axum::response::Html(html(&form)?).into_response()
    } else {
        json_response(StatusCode::OK, json())
    };
    response
        .headers_mut()
        .insert(hyper::header::VARY, "HX-Request".parse().unwrap());
    response
        .headers_mut()
        .insert(hyper::header::CACHE_CONTROL, "no-store".parse().unwrap());
    Ok(response)
}

pub(super) fn routes() -> Router<ProductHandles> {
    Router::new()
        .route("/", get(index))
        .route("/session", post(login))
        .route("/session/logout", post(logout))
        .route("/translations.js", get(translations))
        .route("/icons/{name}", get(icon))
        .route("/source-icons/{kind}", get(source_icon))
        .route("/{file}", get(web_file))
        .route("/styles/{file}", get(web_file))
        .route(
            "/htmx.min.js",
            get(|| async {
                asset(
                    "text/javascript; charset=utf-8",
                    include_bytes!("../../../vendor/htmx/htmx.min.js").as_slice(),
                )
            }),
        )
        .route(
            "/htmx.LICENSE",
            get(|| async {
                asset(
                    "text/plain; charset=utf-8",
                    include_bytes!("../../../vendor/htmx/htmx.LICENSE").as_slice(),
                )
            }),
        )
}

async fn web_file(uri: hyper::Uri) -> Result<Response<Body>, Error> {
    let path = uri.path().trim_start_matches('/');
    let kind = if path.ends_with(".js") {
        "text/javascript; charset=utf-8"
    } else {
        "text/css; charset=utf-8"
    };
    let file = WebFiles::get(path).ok_or_else(|| error(StatusCode::NOT_FOUND, "File not found"))?;
    Ok(asset(kind, file.data))
}

async fn source_icon(
    axum::extract::Path(kind): axum::extract::Path<String>,
) -> Result<Response<Body>, Error> {
    macro_rules! png {
        ($name:literal) => {
            asset(
                "image/png",
                include_bytes!(concat!(
                    "../../../resources/icons/hicolor/64x64/apps/io.github.screwys.Rufin.source.",
                    $name,
                    ".png"
                ))
                .as_slice(),
            )
        };
    }
    Ok(match kind.as_str() {
        "jellyfin" => png!("jellyfin"),
        "navidrome" => png!("navidrome"),
        "subsonic" => png!("opensubsonic"),
        "webdav" => png!("webdav"),
        "smb" => png!("smb"),
        "plex" => png!("plex"),
        "emby" => png!("emby"),
        _ => return Err(error(StatusCode::NOT_FOUND, "Source icon not found")),
    })
}

fn asset(kind: &'static str, bytes: impl Into<Body>) -> Response<Body> {
    Response::builder()
        .header("content-type", kind)
        .header("cache-control", "no-cache")
        .header("x-content-type-options", "nosniff")
        .body(bytes.into())
        .expect("asset headers")
}

async fn icon(
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Result<Response<Body>, Error> {
    if name == "rufin-symbolic.svg" {
        return Ok(asset(
            "image/svg+xml",
            include_bytes!(
                "../../../resources/icons/hicolor/symbolic/apps/io.github.screwys.Rufin-symbolic.svg"
            ).as_slice(),
        ));
    }
    if name == "rufin.svg" || name == "favicon.svg" {
        let svg = include_str!(
            "../../../resources/icons/hicolor/scalable/apps/io.github.screwys.Rufin.svg"
        )
        .replacen("viewBox=\"0 0 128 128\"", "viewBox=\"0 0 112 112\"", 1)
        .replacen(" transform=\"translate(4,4)\"", "", 1);
        return Ok(asset("image/svg+xml", svg));
    }
    let file = name
        .strip_suffix(".svg")
        .and_then(|_| Icons::get(&name))
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "Icon not found"))?;
    Ok(asset("image/svg+xml", file.data))
}

fn translated_sorts(key: LibraryListKey) -> Vec<(&'static str, String)> {
    sort_options(key)
        .into_iter()
        .map(|(name, title)| (name, localization::tr(title)))
        .collect()
}

struct QueueRow {
    pub position: u64,
    pub title: String,
    pub artist: String,
}

/// The sidebar entries GTK shows, in its order, that the web has pages for.
fn navigation_routes(
    sidebar: &rufin_core::settings::SidebarSettings,
) -> Vec<(rufin_core::settings::SidebarRouteItem, bool)> {
    use rufin_core::settings::SidebarRouteItem::*;
    sidebar
        .route_items
        .iter()
        .map(|entry| (entry.item, entry.visible))
        .filter(|(item, _)| {
            matches!(
                item,
                Home | Favorites | Albums | Tracks | Artists | Playlists | SmartPlaylists
            )
        })
        .collect()
}

/// The whole page, for browsers without scripts and the first paint.
struct Page {
    pub connected: bool,
    pub appearance: Value,
    pub csrf: String,
    pub error: String,
    pub source: String,
    pub sources: Vec<(String, String)>,
    /// The page the content shows.
    pub token: Option<page::PageToken>,
    pub title: String,
    pub back: Option<String>,
    pub section: &'static str,
    pub sort: &'static str,
    pub descending: bool,
    /// The orders the page offers: their names, titles and whether the page reads in it.
    pub sorts: Vec<(&'static str, String, bool)>,
    pub play: bool,
    pub playlists: bool,
    pub content: String,
    pub previous: Option<String>,
    pub next: Option<String>,
    pub playing: String,
    pub current_favorite: bool,
    pub artist: String,
    pub album: String,
    pub preview: String,
    pub controls: Value,
    pub position: u64,
    pub duration: u64,
    pub queue: Vec<QueueRow>,
    pub queue_total: usize,
    pub paused: bool,
    pub volume: f64,
    pub folder: String,
    pub folders: Vec<(String, String)>,
    /// The address of this page.
    pub here: String,
    /// The layout settings GTK resolves its sidebars from, as JSON.
    pub layout: String,
    pub private_mode: bool,
    /// The sidebar entries GTK lists, in its order, and whether they show.
    pub routes: Vec<(rufin_core::settings::SidebarRouteItem, bool)>,
}

fn link(parameters: &HashMap<String, String>) -> String {
    format!(
        "/?{}",
        url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(parameters)
            .finish()
    )
}

impl Page {
    pub fn theme(&self) -> String {
        self.appearance["colors"]["color-scheme"]
            .as_str()
            .unwrap_or_else(|| self.appearance["theme"].as_str().unwrap_or("System"))
            .to_lowercase()
    }
    pub fn color_style(&self) -> String {
        self.appearance["colors"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(name, value)| value.as_str().map(|value| format!("{name}:{value};")))
            .collect()
    }
    pub fn needs_source(&self) -> bool {
        self.connected && self.token.is_none()
    }
    pub fn home(&self) -> bool {
        self.section == "Home"
            && self
                .token
                .as_ref()
                .is_none_or(|token| token.route == rufin_core::route::Route::Home)
    }
    pub fn page_json(&self) -> String {
        self.token
            .as_ref()
            .map(page::PageToken::json)
            .unwrap_or_default()
    }
    pub fn query(&self) -> &str {
        self.token
            .as_ref()
            .map_or("", |token| token.options.filter.as_str())
    }
    pub fn current_position(&self) -> String {
        self.controls["current"]["position"]
            .as_u64()
            .map(|position| position.to_string())
            .unwrap_or_default()
    }
    pub fn has_current(&self) -> bool {
        !self.controls["current"].is_null()
    }
    pub fn current_selection(&self) -> String {
        json!({"kind":"track", "uri":self.controls["current"]["track"]["uri"], "favorite":!self.current_favorite}).to_string()
    }
    pub fn position_time(&self) -> String {
        rufin_core::settings::presentation::format_duration((self.position / 1000) as u32)
    }
    pub fn duration_time(&self) -> String {
        rufin_core::settings::presentation::format_duration((self.duration / 1000) as u32)
    }
    /// The navigation entries: their routes, titles, icons, links and whether this page is in
    /// them.
    pub fn navigation(&self) -> Vec<(&'static str, String, &'static str, String, bool, bool)> {
        self.routes
            .iter()
            .map(|(item, visible)| {
                let entry = rufin_core::route::sidebar_route_descriptor(*item);
                (
                    entry.stable_id,
                    localization::tr(entry.title),
                    entry.icon_name,
                    self.nav(entry.stable_id),
                    self.section == entry.stable_id,
                    *visible,
                )
            })
            .collect()
    }

    /// A link to the navigation entry `route`, in this page's source and music folder.
    fn nav(&self, route: &str) -> String {
        let route: rufin_core::route::Route =
            serde_json::from_value(json!(route)).expect("navigation route");
        // Without a page, the entry opens in the selected source and music folder.
        let token = self.token.as_ref().map_or_else(
            || page::PageToken::new(route.clone()),
            |token| token.at(route.clone()),
        );
        link(&HashMap::from([("page".to_owned(), token.json())]))
    }
    pub fn enabled(&self, name: &str) -> bool {
        self.controls[name].as_bool().unwrap_or(false)
    }
    pub fn sleep_minutes(&self) -> String {
        join_counts(&rufin_core::playback::SLEEP_TIMER_MINUTES)
    }
    pub fn sleep_tracks(&self) -> String {
        join_counts(&rufin_core::playback::SLEEP_TIMER_TRACKS)
    }
    pub fn repeat_title(&self) -> String {
        localization::tr(match self.controls["repeat"].as_str() {
            Some("all") => "Repeat all",
            Some("one") => "Repeat one",
            _ => "Repeat off",
        })
    }
}

fn join_counts(counts: &[u32]) -> String {
    counts
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

async fn index(
    State(products): State<ProductHandles>,
    Extension(auth): Extension<Arc<Authorization>>,
    headers: hyper::HeaderMap,
    Query(mut parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let csrf = session(&auth, &headers);
    let mut page = Page {
        connected: csrf.is_some(),
        appearance: if csrf.is_some() {
            catalog::appearance_data(&products)
        } else {
            json!({"theme":"System", "accent":"System", "colors":{}})
        },
        csrf: csrf.unwrap_or_default(),
        error: if parameters.get("login").is_some_and(|v| v == "failed") {
            localization::tr("The API token was not accepted. Please connect again.")
        } else {
            String::new()
        },
        source: String::new(),
        sources: vec![],
        token: None,
        title: localization::tr("Home"),
        back: None,
        section: "Home",
        sort: "title",
        descending: false,
        sorts: Vec::new(),
        play: false,
        playlists: false,
        content: String::new(),
        previous: None,
        next: None,
        playing: String::new(),
        current_favorite: false,
        artist: String::new(),
        album: String::new(),
        preview: String::new(),
        controls: Value::Null,
        position: 0,
        duration: 0,
        queue: vec![],
        queue_total: 0,
        paused: true,
        volume: 1.0,
        folder: String::new(),
        folders: vec![],
        here: link(&parameters),
        layout: String::new(),
        private_mode: false,
        routes: Vec::new(),
    };
    if page.connected {
        let settings = products.settings.load();
        page.layout = serde_json::to_string(&settings.layout).map_err(internal)?;
        page.private_mode = settings.application.private_mode;
        page.routes = navigation_routes(&settings.sidebar);
        drop(settings);
        let configured = products.source.list_sources();
        page.sources = configured
            .sources
            .iter()
            .map(|s| (s.id.to_string(), s.name.clone()))
            .collect();
        let current = products.playback.updates.current();
        let playback = playback_json(&products, current.as_deref(), true);
        page.preview = playback["current"]["track"]["artwork_previews"][0]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        page.position = playback["position_ms"].as_u64().unwrap_or_default();
        page.duration = playback["duration_ms"].as_u64().unwrap_or_default();
        let queue = queue_json(&products, current.as_deref(), 5);
        page.queue_total = queue["total"].as_u64().unwrap_or_default() as usize;
        page.queue = queue["window"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|row| QueueRow {
                position: row["position"].as_u64().unwrap_or_default(),
                title: row["track"]["title"].as_str().unwrap_or_default().into(),
                artist: row["track"]["artist"].as_str().unwrap_or_default().into(),
            })
            .collect();
        page.artist = playback["current"]["track"]["artist"]
            .as_str()
            .unwrap_or_default()
            .into();
        page.album = playback["current"]["track"]["album"]
            .as_str()
            .unwrap_or_default()
            .into();
        page.paused = !matches!(
            playback["state"].as_str(),
            Some("playing" | "resolving" | "buffering")
        );
        page.current_favorite = playback["current"]["track"]["favorite"]
            .as_bool()
            .unwrap_or(false);
        page.controls = playback.clone();
        page.volume = playback["volume"].as_f64().unwrap_or(1.0);
        page.playing = playback["current"]["track"]["title"]
            .as_str()
            .unwrap_or_default()
            .into();
        // Without a page, Home of the selected source; a reversed order flips the current one.
        if !parameters.contains_key("page") {
            parameters.insert(
                "page".into(),
                json!({"route":rufin_core::route::Route::Home}).to_string(),
            );
        }
        let reverse = parameters.remove("reverse").is_some();
        let mut token = page::token(&products, "page", &parameters)?;
        if token.source.is_none() {
            let selected = page::PageToken::selected(&products);
            token.source = selected.source;
            token.folder = selected.folder;
        }
        let selected = products.source.selected_library();
        if reverse {
            let saved = products
                .settings
                .load()
                .library_list(token.list())
                .descending;
            token.options.descending = Some(!token.options.descending.unwrap_or(saved));
        }
        page.source = token
            .source
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        page.folder = token.folder.clone().unwrap_or_default();
        if let Some(selected) = selected.filter(|s| s.source_id.as_str() == page.source) {
            page.folders = selected
                .music_folders
                .iter()
                .map(|f| (f.object_id.clone(), f.name.clone()))
                .collect();
        }
        page.section = page::section(&token.route);
        let offset = number(&parameters, "offset", 0)?;
        let form = Form {
            csrf: page.csrf.clone(),
            return_to: page.here.clone(),
        };
        if token.source.is_some()
            || !token
                .route
                .requires_selected_source(token.options.search_mode)
        {
            match page::open(&products, token, offset, 48).await {
                Ok(opened) => {
                    let list = opened.token.list();
                    let (sort, descending) = opened.view.order();
                    page.title = page::title(&products, &opened.token, opened.header.as_ref());
                    page.back = page::parent(&opened.token)
                        .map(|parent| link(&HashMap::from([("page".to_owned(), parent.json())])));
                    page.sort = sort_name(list, sort);
                    page.descending = descending;
                    page.sorts = translated_sorts(list)
                        .into_iter()
                        .map(|(name, title)| (name, title, name == page.sort))
                        .collect();
                    page.play = opened.view.members().is_some();
                    page.playlists = opened.token.route == rufin_core::route::Route::Playlists;
                    let mut target = HashMap::from([("page".to_owned(), opened.token.json())]);
                    if offset > 0 {
                        target.insert("offset".into(), offset.saturating_sub(48).to_string());
                        page.previous = Some(link(&target));
                    }
                    if opened.offset + opened.rows.len() < opened.view.len() {
                        target.insert("offset".into(), (offset + 48).to_string());
                        page.next = Some(link(&target));
                    }
                    page.content = if opened.token.route == rufin_core::route::Route::Home {
                        home(
                            &products,
                            &opened,
                            &page::home_sections(&opened.view, None),
                            &form,
                        )?
                    } else {
                        library(&products, &opened, true, &form)?
                    };
                    page.token = Some(opened.token);
                }
                Err((_, message)) => {
                    page.error = message.0["error"].as_str().unwrap_or_default().into()
                }
            }
        }
    }
    let html = Index {
        version: env!("CARGO_PKG_VERSION"),
        page: &page,
    }
    .render()
    .map_err(internal)?;
    Ok(Response::builder()
        .header("content-type", "text/html; charset=utf-8")
        .header("cache-control", "no-store")
        .header("referrer-policy", "same-origin")
        .body(Body::from(html))
        .expect("page headers"))
}
