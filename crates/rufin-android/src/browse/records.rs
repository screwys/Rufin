use super::*;
use library::PlaylistListRow;
use rufin_core::settings::app::RecentSearchResult;

#[derive(uniffi::Record)]
pub struct AndroidPlaylistImport {
    pub row: AndroidBrowseRow,
    pub skipped: u64,
}

#[derive(uniffi::Record)]
pub struct AndroidRouteDescriptor {
    pub item: SidebarRouteItem,
    pub id: String,
    pub title: String,
    pub icon_name: String,
    pub selected_icon_name: String,
    pub route: Route,
}

#[derive(uniffi::Record)]
pub struct AndroidRouteSetting {
    pub descriptor: AndroidRouteDescriptor,
    pub visible: bool,
}

#[derive(uniffi::Record)]
pub struct AndroidSortField {
    pub id: LibraryField,
    pub title: String,
}

#[derive(Clone, uniffi::Record)]
pub struct AndroidFieldValue {
    pub id: LibraryField,
    pub text: String,
}

#[derive(uniffi::Record)]
pub struct AndroidBrowseDisplay {
    pub settings: LibraryListSettings,
    pub layouts: Vec<LibraryLayout>,
    /// What the layout's fields are called, and the fields it can show: shown ones first, in
    /// their order.
    pub fields_title: String,
    pub field_choices: Vec<AndroidFieldChoice>,
}

#[derive(uniffi::Record)]
pub struct AndroidFieldChoice {
    pub id: LibraryField,
    pub title: String,
    pub shown: bool,
    pub can_toggle: bool,
}

#[derive(uniffi::Record)]
pub struct AndroidDetailLink {
    pub title: String,
    pub url: Option<String>,
    pub icon_id: String,
}

#[derive(uniffi::Record)]
pub struct AndroidArtistLink {
    pub start: u32,
    pub end: u32,
    pub title: String,
    pub route: Route,
    pub artwork_identity: Option<Vec<u8>>,
}

impl AndroidArtistLink {
    pub(crate) fn from_detail_links(
        links: &rufin_core::route::detail_links::DetailLinks,
    ) -> Result<Vec<Self>, AndroidError> {
        let display = links.display_text();
        links
            .spans()
            .iter()
            .map(|span| {
                let prefix = display
                    .get(..span.range.start)
                    .expect("Detail link text boundary");
                let text = display
                    .get(span.range.clone())
                    .expect("Detail link text boundaries");
                let start = u32::try_from(prefix.encode_utf16().count()).map_err(error)?;
                let end = start + u32::try_from(text.encode_utf16().count()).map_err(error)?;
                Ok(Self {
                    start,
                    end,
                    title: text.to_string(),
                    route: span.route.clone(),
                    artwork_identity: None,
                })
            })
            .collect()
    }
}

#[derive(uniffi::Record)]
pub struct AndroidBrowsePage {
    pub count: u64,
    pub rows: Vec<AndroidBrowseRow>,
    pub summary: Option<AndroidDetailSummary>,
    pub groups: Vec<AndroidArtistReleaseGroup>,
    pub favorite_rows: Vec<AndroidBrowseRow>,
}

#[derive(uniffi::Record)]
pub struct AndroidDetailSummary {
    pub row: AndroidBrowseRow,
    pub artwork_bindings: Vec<Vec<u8>>,
    pub artist_text: String,
    pub artist_links: Vec<AndroidArtistLink>,
    pub label: String,
    pub mode: Route,
    pub album_count: u64,
    pub links: Vec<AndroidDetailLink>,
    pub favorite_route: Option<Route>,
    pub favorite_total: u64,
    pub discography_route: Option<Route>,
    pub tracks_route: Option<Route>,
    pub show_track_images: bool,
    pub show_track_header: bool,
}

#[derive(uniffi::Record)]
pub struct AndroidArtistReleaseGroup {
    pub id: String,
    pub title: String,
    pub total: u64,
    pub index: u64,
}

#[derive(uniffi::Record)]
pub struct AndroidMusicFolder {
    pub id: Option<String>,
    pub title: String,
}

#[derive(uniffi::Record)]
pub struct AndroidMusicFolderState {
    pub selected_id: Option<String>,
    pub choices: Vec<AndroidMusicFolder>,
}

#[derive(uniffi::Record)]
pub struct AndroidBreadcrumb {
    pub title: String,
    pub route: Route,
}

#[derive(uniffi::Record)]
pub struct AndroidScrollSection {
    pub title: String,
    pub index: u64,
}

#[derive(uniffi::Record)]
pub struct AndroidCollectionCategory {
    pub id: String,
    pub title: String,
    pub icon_name: String,
}

#[derive(uniffi::Record)]
pub struct AndroidPinPage {
    pub rows: Vec<AndroidBrowseRow>,
    pub next_offset: Option<u64>,
    pub total: u64,
}

#[derive(Clone, uniffi::Enum)]
#[expect(
    clippy::large_enum_variant,
    reason = "nearly every row is a library row"
)]
pub enum AndroidBrowseItem {
    Library {
        row: library::Row,
    },
    /// A child folder; the row's route opens it.
    Folder {
        artwork: Option<Vec<u8>>,
    },
    Recent {
        row: RecentSearchResult,
    },
}

#[derive(Clone, uniffi::Record)]
pub struct AndroidBrowseRow {
    pub item: AndroidBrowseItem,
    pub fields: Vec<AndroidFieldValue>,
    pub display_title: Option<String>,
    pub lyrics_snippet: Option<library::LyricsSnippet>,
    pub pin: Option<SidebarPin>,
    /// The page the row opens, and the context id a play of that page carries.
    pub route: Option<Route>,
    pub context_id: Option<String>,
    pub section: String,
    pub section_id: String,
    pub section_kind: String,
    pub section_refreshable: bool,
}

impl AndroidBrowseRow {
    pub(crate) fn new(item: AndroidBrowseItem) -> Self {
        Self {
            item,
            fields: Vec::new(),
            display_title: None,
            lyrics_snippet: None,
            pin: None,
            route: None,
            context_id: None,
            section: String::new(),
            section_id: String::new(),
            section_kind: String::new(),
            section_refreshable: false,
        }
    }

    pub(super) fn track(&self) -> Option<&library::TrackRow> {
        match &self.item {
            AndroidBrowseItem::Library {
                row: Row::Track(track) | Row::AlbumTrack { track, .. },
            } => Some(track),
            _ => None,
        }
    }

    /// A row that opens `route`.
    pub(crate) fn opening(item: AndroidBrowseItem, route: Route) -> Self {
        Self {
            context_id: rufin_core::collection_context::CollectionTarget::for_route(&route)
                .map(|target| target.context_id()),
            route: Some(route),
            ..Self::new(item)
        }
    }
}

impl From<Row> for AndroidBrowseRow {
    fn from(row: Row) -> Self {
        let pin = row.pin();
        let display_title = match &row {
            Row::Playlist(PlaylistListRow::SmartPlaylist(playlist)) => {
                Some(rufin_core::playlists::smart_playlist_display_name(
                    &playlist.object_id,
                    &playlist.name,
                ))
            }
            _ => None,
        };
        let route = Route::of_row(&row);
        let item = AndroidBrowseItem::Library { row };
        Self {
            pin,
            display_title,
            ..match route {
                Some(route) => Self::opening(item, route),
                None => Self::new(item),
            }
        }
    }
}

impl From<rufin_core::detail_links::DetailLink> for AndroidDetailLink {
    fn from(link: rufin_core::detail_links::DetailLink) -> Self {
        Self {
            title: link.title.to_string(),
            url: link.url,
            icon_id: link.icon_name.into(),
        }
    }
}

/// `row` showing a committed favorite or rating; None when it shows another item, or this one
/// as it is.
#[uniffi::export]
pub fn show_state(
    mut row: AndroidBrowseRow,
    change: library::StateChange,
) -> Option<AndroidBrowseRow> {
    use library::ShowsState;
    let AndroidBrowseItem::Library { row: library } = &mut row.item else {
        return None;
    };
    let state = library.state_mut(&change)?;
    if change.shown_in(state) {
        return None;
    }
    change.show_in(state);
    for field in &mut row.fields {
        if matches!(field.id, LibraryField::Favorite | LibraryField::UserRating) {
            field.text = rufin_core::settings::presentation::row_field(library, field.id);
        }
    }
    Some(row)
}
