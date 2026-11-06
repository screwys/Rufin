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

impl AndroidSortField {
    pub(super) fn of(field: LibraryField) -> Self {
        Self {
            id: field,
            title: localization::tr(rufin_core::settings::presentation::library_field_title(
                field,
            )),
        }
    }
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

/// An opened view and what it read when it opened.
#[derive(uniffi::Record)]
pub struct AndroidOpened {
    pub view: Arc<super::AndroidView>,
    pub count: u64,
    pub rows: Vec<AndroidBrowseRow>,
    pub summary: Option<AndroidDetailSummary>,
    pub groups: Vec<AndroidArtistReleaseGroup>,
    pub favorite_rows: Vec<AndroidBrowseRow>,
    pub home: Vec<AndroidHomeSection>,
    /// What committed while the page read.
    pub reading: Arc<AndroidReading>,
}

/// A window of a page's rows and what committed while it read them.
#[derive(uniffi::Record)]
pub struct AndroidRows {
    pub rows: Vec<AndroidBrowseRow>,
    /// What committed while it read, which the page handles once it holds the rows.
    pub reading: Arc<AndroidReading>,
}

/// What committed while a page read, which the page handles as any other change once it shows
/// what it read.
#[derive(uniffi::Object)]
pub struct AndroidReading(std::sync::Mutex<Option<rufin_core::shown::Reading>>);

impl AndroidReading {
    pub(super) fn new(reading: rufin_core::shown::Reading) -> Arc<Self> {
        Arc::new(Self(std::sync::Mutex::new(Some(reading))))
    }
}

#[uniffi::export]
impl AndroidReading {
    /// Everything committed since the page started reading, everything when it fell behind;
    /// None when nothing was, or when it was taken already.
    pub fn missed(&self) -> Option<rufin_core::source::feed::Changed> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()?
            .missed()
    }
}

#[derive(uniffi::Enum)]
pub enum AndroidHomeSectionKind {
    Showcase,
    Rows,
    Genres,
}

/// One Home section: its title, its rows and the block a refresh reads again.
#[derive(uniffi::Record)]
pub struct AndroidHomeSection {
    pub title: String,
    pub kind: AndroidHomeSectionKind,
    pub refresh: Option<library::HomeBlockKind>,
    pub rows: Vec<AndroidBrowseRow>,
}

/// What a detail page's header shows besides its row: its pictures, credited artists, label
/// and links, and an artist's favorite tracks count.
#[derive(uniffi::Record)]
pub struct AndroidDetailSummary {
    pub row: AndroidBrowseRow,
    pub artwork_bindings: Vec<Vec<u8>>,
    pub artist_text: String,
    pub artist_links: Vec<AndroidArtistLink>,
    /// What the collection is, untranslated: an album's release class, Artist, Playlist.
    pub label: String,
    pub links: Vec<AndroidDetailLink>,
    pub favorite_total: u64,
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
    pub category: CollectionCategory,
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
    /// A track with its copy, or a collection whose tracks are all downloaded.
    pub downloaded: bool,
    /// What a track or collection row shows of its length and picture: one for a track.
    pub track_count: u64,
    pub duration_millis: u64,
    /// The duration in core's format: a track's clock time, a collection's units.
    pub duration_text: String,
    pub artwork_identity: Option<Vec<u8>>,
}

impl AndroidBrowseRow {
    pub(crate) fn new(item: AndroidBrowseItem) -> Self {
        use rufin_core::settings::presentation::{format_duration, format_duration_units};
        let (track_count, duration_millis, duration_text, artwork_identity) = match &item {
            AndroidBrowseItem::Library {
                row: Row::Track(track) | Row::AlbumTrack { track, .. },
            } => {
                let millis = track.duration_millis.max(0) as u64;
                let text = format_duration((millis / 1_000) as u32);
                (1, millis, text, track.artwork_binding.clone())
            }
            AndroidBrowseItem::Library { row } => row.collection().map_or_else(
                || (0, 0, String::new(), None),
                |shown| {
                    let millis = shown.duration_millis.max(0) as u64;
                    (
                        shown.track_count.max(0) as u64,
                        millis,
                        format_duration_units((millis / 1_000) as u32),
                        shown.artwork.first().map(|binding| binding.to_vec()),
                    )
                },
            ),
            AndroidBrowseItem::Folder { artwork } => (0, 0, String::new(), artwork.clone()),
            AndroidBrowseItem::Recent { row } => (0, 0, String::new(), row.artwork_binding.clone()),
        };
        Self {
            track_count,
            duration_millis,
            duration_text,
            artwork_identity,
            item,
            fields: Vec::new(),
            display_title: None,
            lyrics_snippet: None,
            pin: None,
            route: None,
            context_id: None,
            downloaded: false,
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
        let downloaded = row.downloaded();
        let item = AndroidBrowseItem::Library { row };
        Self {
            pin,
            display_title,
            downloaded,
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

/// A row a page shows that a committed change shows differently, by its index among the rows
/// the page passed.
#[derive(uniffi::Record)]
pub struct AndroidShownRow {
    pub index: u64,
    pub row: AndroidBrowseRow,
}

/// What a committed change shows in a page's rows: their favorites and ratings now, their
/// downloads once read.
#[derive(uniffi::Record)]
pub struct AndroidShowing {
    pub states: Vec<AndroidShownRow>,
    pub downloads: Option<Arc<AndroidDownloads>>,
}

type DownloadRead = std::pin::Pin<
    Box<dyn std::future::Future<Output = Option<Vec<(usize, library::Downloads)>>> + Send>,
>;

/// The downloads a committed change shows in a page's rows, read in the background.
#[derive(uniffi::Object)]
pub struct AndroidDownloads {
    read: std::sync::Mutex<Option<DownloadRead>>,
    /// The rows passed, by the index core reads them at.
    rows: Vec<(usize, AndroidBrowseRow)>,
}

#[uniffi::export]
impl AndroidDownloads {
    /// The rows whose downloads changed, showing them as the Catalog counts them; nothing when
    /// a newer change of the same page superseded it. A page takes each into the row it shows
    /// now with [`AndroidLibrary::take_downloads`].
    pub async fn read(&self) -> Vec<AndroidShownRow> {
        let read = self
            .read
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let Some(read) = read else {
            return Vec::new();
        };
        read.await
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(at, read)| {
                use library::ShowsState;
                let (index, row) = &self.rows[at];
                let AndroidBrowseItem::Library { row: library } = &row.item else {
                    return None;
                };
                let mut library = library.clone();
                library.show_downloads(read);
                Some(AndroidShownRow {
                    index: *index as u64,
                    row: row.showing(library),
                })
            })
            .collect()
    }
}

impl AndroidBrowseRow {
    /// The row showing `library`, the same row with other favorites, ratings or downloads.
    fn showing(&self, library: library::Row) -> Self {
        let mut row = self.clone();
        for field in &mut row.fields {
            if matches!(field.id, LibraryField::Favorite | LibraryField::UserRating) {
                field.text = rufin_core::settings::presentation::row_field(&library, field.id);
            }
        }
        row.downloaded = library.downloaded();
        row.item = AndroidBrowseItem::Library { row: library };
        row
    }
}

#[uniffi::export]
impl AndroidLibrary {
    /// Shows `changed` in `rows`, the rows a page holds now, through core's
    /// [`rufin_core::shown::ShownChanges`]: the rows whose favorites and ratings it changes at
    /// once, and the downloads to read. `view` is the page that read them; a newer change of
    /// the same page supersedes the download read of an older one.
    pub fn show_changes(
        &self,
        rows: Vec<AndroidBrowseRow>,
        changed: rufin_core::source::feed::Changed,
        view: Option<Arc<super::AndroidView>>,
    ) -> AndroidShowing {
        let library = rows
            .into_iter()
            .enumerate()
            .filter(|(_, row)| matches!(row.item, AndroidBrowseItem::Library { .. }))
            .collect::<Vec<_>>();
        let shown = library
            .iter()
            .filter_map(|(_, row)| match &row.item {
                AndroidBrowseItem::Library { row } => Some(row),
                _ => None,
            })
            .collect::<Vec<_>>();
        let changes = view
            .as_ref()
            .map_or_else(|| self.changes.clone(), |view| view.changes.clone());
        let showing = changes.show(
            &self.source,
            &shown,
            &changed,
            view.as_ref().map(|view| &*view.view),
        );
        AndroidShowing {
            states: showing
                .states
                .into_iter()
                .map(|(at, row)| {
                    let (index, shown) = &library[at];
                    AndroidShownRow {
                        index: *index as u64,
                        row: shown.showing(row),
                    }
                })
                .collect(),
            downloads: showing.downloads.map(|read| {
                Arc::new(AndroidDownloads {
                    read: std::sync::Mutex::new(Some(Box::pin(read))),
                    rows: library,
                })
            }),
        }
    }

    /// `row`, the row a page shows now, with the downloads `read` shows when it is the same row
    /// read again; None when it is another row.
    pub fn take_downloads(
        &self,
        row: AndroidBrowseRow,
        read: AndroidBrowseRow,
    ) -> Option<AndroidBrowseRow> {
        use library::ShowsState;
        let (AndroidBrowseItem::Library { row: shown }, AndroidBrowseItem::Library { row: read }) =
            (&row.item, read.item)
        else {
            return None;
        };
        let mut shown = shown.clone();
        shown
            .show_downloads(read.downloads()?)
            .then(|| row.showing(shown))
    }
}
