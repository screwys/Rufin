use std::sync::Arc;
mod commands;
mod details;
mod folders;
mod projection;
mod queries;
mod records;
mod search;
pub use records::{
    AndroidArtistLink, AndroidArtistReleaseGroup, AndroidBreadcrumb, AndroidBrowseDisplay,
    AndroidBrowseRow, AndroidCollectionCategory, AndroidDetailSummary, AndroidFieldValue,
    AndroidMusicFolder, AndroidMusicFolderState, AndroidPinPage, AndroidPlaylistImport,
    AndroidRatingState, AndroidRouteDescriptor, AndroidRouteSetting, AndroidScrollSection,
    AndroidSortField, AndroidSortSelection,
};

use library::{ReadCancellation, SongQuery, SongScope, SongSelection};
use rufin_core::source::folders::FolderProjection;
use rufin_core::{
    route::{CollectionCategory, Route, sidebar_route_descriptor},
    runtime::SelectedLibrary,
    settings::{
        SidebarPin, SidebarRouteItem,
        layout::{DisplaySize, LibraryField, LibraryLayout, LibraryListKey, LibraryListSettings},
    },
};

use crate::{
    host::{AndroidError, error},
    library::AndroidLibrary,
};

const ARTIST_RELEASE_GROUPS: [(&str, &str); 6] = [
    ("Album", "Albums"),
    ("Ep", "EPs"),
    ("Single", "Singles"),
    ("Collection", "Collections"),
    ("Other", "Other releases"),
    ("AppearsOn", "Appears On"),
];

#[derive(Clone, Copy)]
enum ArtistReleaseSection {
    Class(library::AlbumReleaseClass),
    AppearsOn,
}

fn release_section(id: &str) -> Result<ArtistReleaseSection, AndroidError> {
    use library::AlbumReleaseClass;
    Ok(ArtistReleaseSection::Class(match id {
        "Album" => AlbumReleaseClass::Album,
        "Ep" => AlbumReleaseClass::Ep,
        "Single" => AlbumReleaseClass::Single,
        "Collection" => AlbumReleaseClass::Collection,
        "Other" => AlbumReleaseClass::Other,
        "AppearsOn" => return Ok(ArtistReleaseSection::AppearsOn),
        _ => return Err(error("Unknown release section")),
    }))
}

fn release_index(class: library::AlbumReleaseClass) -> usize {
    use library::AlbumReleaseClass;
    match class {
        AlbumReleaseClass::Album => 0,
        AlbumReleaseClass::Ep => 1,
        AlbumReleaseClass::Single => 2,
        AlbumReleaseClass::Collection => 3,
        AlbumReleaseClass::Other => 4,
    }
}

fn browse_display(key: LibraryListKey, settings: &LibraryListSettings) -> AndroidBrowseDisplay {
    let id = |value: serde_json::Value| value.as_str().expect("Display setting name").to_string();
    let layouts = [
        LibraryLayout::Row,
        LibraryLayout::Grid,
        LibraryLayout::Detail,
    ]
    .into_iter()
    .filter(|layout| key.supports_layout(*layout))
    .map(|layout| AndroidSortField {
        id: id(serde_json::to_value(layout).expect("Layout serialization")),
        title: localization::tr(match layout {
            LibraryLayout::Row => "Rows",
            LibraryLayout::Grid => "Grid",
            LibraryLayout::Detail => "Detail",
        }),
    })
    .collect();
    let sizes = [
        DisplaySize::Compact,
        DisplaySize::Default,
        DisplaySize::Large,
    ]
    .into_iter()
    .map(|size| AndroidSortField {
        id: id(serde_json::to_value(size).expect("Size serialization")),
        title: localization::tr(match size {
            DisplaySize::Compact => "Compact",
            DisplaySize::Default => "Default",
            DisplaySize::Large => "Large",
        }),
    })
    .collect();
    let grid_spacings = [
        DisplaySize::Compact,
        DisplaySize::Default,
        DisplaySize::Large,
    ]
    .into_iter()
    .map(|size| AndroidSortField {
        id: id(serde_json::to_value(size).expect("Spacing serialization")),
        title: localization::tr(match size {
            DisplaySize::Compact => "Small",
            DisplaySize::Default => "Default",
            DisplaySize::Large => "Wide",
        }),
    })
    .collect();
    AndroidBrowseDisplay {
        layout: id(serde_json::to_value(settings.layout).expect("Layout serialization")),
        show_header: settings.display.show_header,
        layouts,
        size: id(serde_json::to_value(settings.display.size).expect("Size serialization")),
        grid_spacing: id(
            serde_json::to_value(settings.display.grid_spacing).expect("Spacing serialization")
        ),
        sizes,
        grid_spacings,
    }
}

fn queue_placement(value: &str) -> Result<playback::QueuePlacement, AndroidError> {
    match value {
        "now" => Ok(playback::QueuePlacement::Now),
        "next" => Ok(playback::QueuePlacement::Next),
        "append" => Ok(playback::QueuePlacement::Last),
        _ => Err(error("Unknown queue placement")),
    }
}

fn route_json(route: Route) -> String {
    serde_json::to_string(&route).expect("Route serialization")
}

fn context_caption(
    route: &Route,
    title: Option<String>,
) -> Option<Arc<library::QueueContextTitle>> {
    let kind = match route {
        Route::AlbumDetail(_) => Some("Album"),
        Route::ArtistDetail(_)
        | Route::ArtistDiscography(_)
        | Route::ArtistTracks(_)
        | Route::ArtistFavoriteTracks(_)
        | Route::AlbumArtistDetail(_)
        | Route::AlbumArtistDiscography(_)
        | Route::AlbumArtistTracks(_)
        | Route::AlbumArtistFavoriteTracks(_) => Some("Artist"),
        Route::PlaylistDetail(_) | Route::SmartPlaylistDetail(_) => Some("Playlist"),
        Route::GenreDetail(_) => Some("Genre"),
        Route::MoodDetail(_) => Some("Mood"),
        _ => None,
    };
    title.map(|title| {
        Arc::new(library::QueueContextTitle {
            kind: kind.map(Arc::from),
            title: title.into(),
        })
    })
}

fn core_route_kind(route: &Route) -> String {
    match serde_json::to_value(route).expect("Route serialization") {
        serde_json::Value::String(kind) => kind,
        serde_json::Value::Object(value) => value.into_iter().next().expect("Route variant").0,
        _ => unreachable!("Core route variant"),
    }
}

fn library_list_key(route: &Route, category: CollectionCategory) -> LibraryListKey {
    match route {
        Route::Albums => LibraryListKey::Albums,
        Route::Artists => LibraryListKey::Artists,
        Route::AlbumArtists => LibraryListKey::AlbumArtists,
        Route::Genres => LibraryListKey::Genres,
        Route::Moods => LibraryListKey::Moods,
        Route::Playlists => LibraryListKey::Playlists,
        Route::SmartPlaylists => LibraryListKey::SmartPlaylists,
        Route::PlaylistDetail(_) => LibraryListKey::PlaylistTracks,
        Route::SmartPlaylistDetail(_) => LibraryListKey::SmartPlaylistTracks,
        Route::AlbumDetail(_) => LibraryListKey::AlbumDetailTracks,
        Route::ArtistDiscography(_) | Route::AlbumArtistDiscography(_) => {
            LibraryListKey::ArtistAlbums
        }
        Route::ArtistDetail(_) | Route::AlbumArtistDetail(_)
            if category == CollectionCategory::Albums =>
        {
            LibraryListKey::ArtistAlbums
        }
        Route::ArtistDetail(_)
        | Route::AlbumArtistDetail(_)
        | Route::ArtistTracks(_)
        | Route::AlbumArtistTracks(_)
        | Route::ArtistFavoriteTracks(_)
        | Route::AlbumArtistFavoriteTracks(_) => LibraryListKey::ArtistTracks,
        Route::GenreDetail(_) => LibraryListKey::GenreTracks,
        Route::MoodDetail(_) => LibraryListKey::MoodTracks,
        Route::History => LibraryListKey::History,
        Route::Folders { .. } => LibraryListKey::Folders,
        Route::Favorites if category == CollectionCategory::Tracks => {
            LibraryListKey::FavoriteTracks
        }
        Route::Favorites => category.key(),
        _ => LibraryListKey::Tracks,
    }
}

fn home_section(row: &mut AndroidBrowseRow, block: library::HomeBlockKind) {
    row.section_id = serde_json::to_value(block)
        .expect("Home block serialization")
        .as_str()
        .expect("Home block name")
        .to_string();
    row.section = localization::tr(rufin_core::settings::presentation::home_block_title(block));
    row.section_kind = if block == library::HomeBlockKind::Showcase {
        "showcase"
    } else if block == library::HomeBlockKind::Genres {
        "genres"
    } else {
        "section"
    }
    .into();
    row.section_refreshable = block.section_kind().is_some();
}

fn artist_row(row: library::ArtistRow, album_artist: bool) -> AndroidBrowseRow {
    let pin = library::source_entity_parts(&row.media_uri).map(|(source_id, _, artist_id)| {
        SidebarPin::Artist {
            source_id,
            artist_id,
            album_artist,
        }
    });
    AndroidBrowseRow {
        track_key: None,
        kind: "artist".into(),
        playback_context_id: Some(format!(
            "{}:{}",
            if album_artist {
                "album-artist"
            } else {
                "artist"
            },
            row.media_uri
        )),
        fields: Vec::new(),
        artist: row.name.clone(),
        album: String::new(),
        key: row.object_id,
        detail_route: Some(route_json(if album_artist {
            Route::AlbumArtistDetail(row.media_uri.clone())
        } else {
            Route::ArtistDetail(row.media_uri.clone())
        })),
        media_uri: row.media_uri,
        title: row.name,
        subtitle: String::new(),
        favorite: row.favorite,
        duration_millis: row.duration_millis.max(0) as u64,
        pin: pin.map(|pin| serde_json::to_string(&pin).expect("Pin serialization")),
        section: String::new(),
        section_id: String::new(),
        section_kind: String::new(),
        section_refreshable: false,
        year: None,
        track_count: 0,
        source_id: None,
        source_name: String::new(),
        last_played: None,
        writable: false,
        downloaded: row.track_count > 0 && row.downloaded_count == row.track_count,
        artwork_identity: row.artwork_binding,
    }
}

#[derive(uniffi::Object)]
pub struct AndroidBrowseQuery {
    selected: Option<SelectedLibrary>,
    database: Arc<library::Database>,
    runtime: tokio::runtime::Handle,
    queue: playback::QueueHandle,
    route: Route,
    category: CollectionCategory,
    filter: String,
    sort: LibraryField,
    descending: bool,
    now: i64,
    blocks: Vec<library::HomeBlockKind>,
    history_current_only: bool,
    favorites_only: bool,
    downloaded_only: bool,
    home_variations: (i64, i64),
    home_snapshot: tokio::sync::OnceCell<Vec<AndroidBrowseRow>>,
    display: LibraryListSettings,
    album_tracks: LibraryListSettings,
    artist_tracks: LibraryListSettings,
    source: rufin_core::runtime::SourceHandle,
    settings: rufin_core::SettingsHandle,
    release_section: Option<ArtistReleaseSection>,
    release_counts: tokio::sync::OnceCell<[i64; 6]>,
    folder_projection: tokio::sync::OnceCell<FolderProjection>,
    songs: tokio::sync::OnceCell<Option<SongQuery>>,
}

fn search_rows(rows: library::SearchResults) -> Vec<AndroidBrowseRow> {
    rows.tracks
        .into_iter()
        .map(Into::into)
        .chain(rows.albums.into_iter().map(Into::into))
        .chain(rows.artists.into_iter().map(|row| artist_row(row, false)))
        .collect()
}

impl AndroidBrowseQuery {
    fn selected(&self) -> &SelectedLibrary {
        self.selected
            .as_ref()
            .expect("Source-scoped query has a selected library")
    }

    fn artist_release_identity(&self) -> Option<(&str, bool)> {
        match &self.route {
            Route::ArtistDiscography(uri) => Some((uri, false)),
            Route::AlbumArtistDiscography(uri) => Some((uri, true)),
            Route::ArtistDetail(uri) => Some((uri, false)),
            Route::AlbumArtistDetail(uri) => Some((uri, true)),
            _ => None,
        }
    }

    async fn release_counts(&self) -> Result<[i64; 6], AndroidError> {
        self.release_counts
            .get_or_try_init(|| async {
                let Some((uri, album_artist)) = self.artist_release_identity() else {
                    return Ok([0; 6]);
                };
                let Some(artist) = self
                    .database
                    .artist_row_by_media_uri(uri, &ReadCancellation::new())
                    .await
                    .map_err(error)?
                else {
                    return Ok([0; 6]);
                };
                self.database
                    .artist_release_counts(
                        artist.source_key,
                        artist.artist_key,
                        album_artist,
                        self.selected().music_folder_key,
                        &self.filter,
                        &ReadCancellation::new(),
                    )
                    .await
                    .map_err(error)
            })
            .await
            .copied()
    }

    fn detail_track_settings(&self) -> &LibraryListSettings {
        if self.detail_identity().is_some() && !matches!(self.route, Route::AlbumDetail(_)) {
            &self.artist_tracks
        } else {
            &self.display
        }
    }
    fn detail_identity(&self) -> Option<(&str, bool)> {
        match &self.route {
            Route::AlbumDetail(uri)
            | Route::ArtistDetail(uri)
            | Route::ArtistDiscography(uri)
            | Route::ArtistTracks(uri)
            | Route::ArtistFavoriteTracks(uri) => Some((uri, false)),
            Route::AlbumArtistDetail(uri)
            | Route::AlbumArtistDiscography(uri)
            | Route::AlbumArtistTracks(uri)
            | Route::AlbumArtistFavoriteTracks(uri) => Some((uri, true)),
            _ => None,
        }
    }
    fn album_detail_layout(&self) -> bool {
        self.display.layout == LibraryLayout::Detail
            && library_list_key(&self.route, self.category) == LibraryListKey::Albums
    }
    async fn song_query(&self) -> library::LibraryResult<Option<&SongQuery>> {
        self.songs
            .get_or_try_init(|| async {
                let cancel = ReadCancellation::new();
                let mut folder = self
                    .selected
                    .as_ref()
                    .and_then(|selected| selected.music_folder_key);
                let scope = match &self.route {
                    Route::Tracks => SongScope::Tracks(self.selected().source_key),
                    Route::Favorites if self.category == CollectionCategory::Tracks => {
                        SongScope::Tracks(self.selected().source_key)
                    }
                    Route::AlbumDetail(uri) => {
                        let Some(row) = self.database.album_row_by_media_uri(uri, &cancel).await?
                        else {
                            return Ok(None);
                        };
                        SongScope::Album(row.album_key)
                    }
                    Route::ArtistDetail(_) | Route::AlbumArtistDetail(_)
                        if self.category == CollectionCategory::Albums =>
                    {
                        return Ok(None);
                    }
                    Route::ArtistDetail(uri)
                    | Route::ArtistTracks(uri)
                    | Route::ArtistFavoriteTracks(uri)
                    | Route::AlbumArtistDetail(uri)
                    | Route::AlbumArtistTracks(uri)
                    | Route::AlbumArtistFavoriteTracks(uri) => {
                        let Some(row) = self.database.artist_row_by_media_uri(uri, &cancel).await?
                        else {
                            return Ok(None);
                        };
                        SongScope::Artist {
                            key: row.artist_key,
                            album_artist: matches!(
                                self.route,
                                Route::AlbumArtistDetail(_)
                                    | Route::AlbumArtistTracks(_)
                                    | Route::AlbumArtistFavoriteTracks(_)
                            ),
                        }
                    }
                    Route::GenreDetail(key) => SongScope::Genre(*key),
                    Route::MoodDetail(key) => SongScope::Mood(*key),
                    Route::PlaylistDetail(key) => SongScope::Playlist(*key),
                    Route::SmartPlaylistDetail(key) => SongScope::Smart {
                        key: *key,
                        source: self.selected.as_ref().map(|selected| selected.source_key),
                        now: self.now,
                    },
                    Route::Folders { .. } => {
                        let query = self
                            .folder_projection()
                            .await?
                            .song_query(self.selected().source_key);
                        folder = query.folder;
                        query.scope
                    }
                    Route::History => SongScope::History {
                        source: self
                            .history_current_only
                            .then(|| self.selected().source_id.clone()),
                    },
                    _ => return Ok(None),
                };
                let mut query = SongQuery::new(scope);
                query.folder = folder;
                query.filter = self.filter.clone();
                query.sort = Some(self.sort.track_sort());
                query.descending = self.descending;
                query.favorites_only = self.favorites();
                query.downloaded_only = self.downloaded();
                Ok(Some(query))
            })
            .await
            .map(Option::as_ref)
    }

    async fn target_selection(&self) -> Result<SongSelection, AndroidError> {
        if matches!(self.route, Route::Folders { .. }) {
            return self.folder_selection().await;
        }
        let mut query = self
            .song_query()
            .await
            .map_err(error)?
            .cloned()
            .ok_or_else(|| error("Choose a collection"))?;
        if !matches!(query.scope, SongScope::Smart { .. }) {
            query.folder = None;
        }
        query.sort = None;
        query.descending = false;
        Ok(query.selection())
    }

    fn context_id(&self) -> Arc<str> {
        let context = match &self.route {
            Route::AlbumDetail(uri) => format!("album:{uri}"),
            Route::ArtistDetail(uri)
            | Route::ArtistTracks(uri)
            | Route::ArtistFavoriteTracks(uri)
            | Route::ArtistDiscography(uri) => format!("artist:{uri}"),
            Route::AlbumArtistDetail(uri)
            | Route::AlbumArtistTracks(uri)
            | Route::AlbumArtistFavoriteTracks(uri)
            | Route::AlbumArtistDiscography(uri) => format!("album-artist:{uri}"),
            Route::GenreDetail(key) => format!("genre:{key}"),
            Route::MoodDetail(key) => format!("mood:{key}"),
            Route::PlaylistDetail(key) => format!("playlist:{key}"),
            Route::SmartPlaylistDetail(key) => format!("smart-playlist:{key}"),
            _ => format!("android:{}", route_json(self.route.clone())),
        };
        format!("{context}|query={}|source={}|folder={:?}|sort={:?}|descending={}|favorites={}|downloaded={}",
            self.filter, self.selected.as_ref().map_or("rufin", |selected| selected.source_id.as_str()),
            self.selected.as_ref().and_then(|selected| selected.music_folder_key), self.sort, self.descending, self.favorites(), self.downloaded()).into()
    }

    fn downloaded(&self) -> bool {
        self.downloaded_only && self.supports_downloaded_filter()
    }

    fn favorites(&self) -> bool {
        (self.favorites_only && self.supports_favorite_filter())
            || matches!(
                self.route,
                Route::Favorites
                    | Route::ArtistFavoriteTracks(_)
                    | Route::AlbumArtistFavoriteTracks(_)
            )
    }
}
