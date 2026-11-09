//! Which actions a row's menu offers, as data. Each client labels, orders and draws them;
//! the saved menu settings hide the configurable ones.

use library::{PlaylistFolderMember, PlaylistListRow, RadioSeed, Row, TagKind};
use sources::{SourceConfiguration, SourceId};

use crate::detail_links::DetailLink;
use crate::route::Route;
use crate::settings::app::Settings;
use crate::settings::{ContextMenuItem, SidebarPin};
use crate::source::SourceOwner;

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Menu {
    pub play: bool,
    pub play_next: bool,
    pub play_later: bool,
    pub radio: Option<RadioSeed>,
    pub add_to_playlist: bool,
    /// Add to or Remove from Favorites.
    pub favorite: bool,
    /// The rating stars.
    pub rating: bool,
    pub edit_metadata: bool,
    /// The row's pin, and whether it is pinned.
    pub pin: Option<SidebarPin>,
    pub pinned: bool,
    /// Whether Go to has a destination; [`go_to`] lists them.
    pub go_to: bool,
    pub download: bool,
    /// Remove Download, or Remove Downloads of a collection.
    pub remove_download: bool,
    /// Share: the link to the row on its server.
    pub share: Option<crate::sharing::Share>,
    /// Add current: the playing track into a writable playlist.
    pub add_current: bool,
    /// A playlist's or smart playlist's folder actions, and whether it is in a folder.
    pub folder: Option<bool>,
    /// A playlist's or smart playlist's export to a file.
    pub export: bool,
    /// A playlist's name and file settings, a smart playlist's rules, a folder's name.
    pub edit: bool,
    pub remove_unavailable: bool,
    pub delete: bool,
}

/// Where a row's Go to leads.
pub enum GoTo {
    /// An artist, by name, and the page that shows them.
    Artist(String, Route),
    Album(Route),
    /// The row's folder, or its page on its server.
    Source(DetailLink),
}

/// The menu of `row` under `settings`.
pub fn menu(owner: &SourceOwner, settings: &Settings, row: &Row) -> Menu {
    offered(owner, settings, row, |item| {
        settings.application.context_menu.is_visible(item)
    })
}

/// What `row` offers, every configurable entry shown: what a swipe on it can do.
pub fn swipes(owner: &SourceOwner, settings: &Settings, row: &Row) -> Menu {
    offered(owner, settings, row, |_| true)
}

/// The radio a play of `row` seeds.
pub fn radio_seed(row: &Row) -> Option<RadioSeed> {
    Some(match row {
        Row::Track(track) | Row::AlbumTrack { track, .. } => {
            RadioSeed::Track(track.media_uri.clone())
        }
        Row::Album(album) | Row::AlbumHeading(album) => RadioSeed::Album(album.album_key),
        Row::Artist(artist) if artist.album_artist => RadioSeed::AlbumArtist(artist.artist_key),
        Row::Artist(artist) => RadioSeed::Artist(artist.artist_key),
        Row::Tag(tag) if tag.kind == TagKind::Genre => {
            RadioSeed::Genre(library::GenreKey::from_raw(tag.key))
        }
        Row::Playlist(PlaylistListRow::Playlist(playlist)) => {
            RadioSeed::Playlist(playlist.playlist_key)
        }
        _ => return None,
    })
}

fn offered(
    owner: &SourceOwner,
    settings: &Settings,
    row: &Row,
    shown: impl Fn(ContextMenuItem) -> bool,
) -> Menu {
    let stored = owner.shared.settings.load();
    let source = row_source(owner, &stored.sources, row);
    let plays = crate::view::PlayTarget::plays(row);
    let mut menu = Menu {
        play: plays && shown(ContextMenuItem::Play),
        play_next: plays && shown(ContextMenuItem::PlayNext),
        play_later: plays && shown(ContextMenuItem::PlayLater),
        ..Menu::default()
    };
    let (add_to_playlist, rated) = match row {
        Row::Track(track) | Row::AlbumTrack { track, .. } => {
            menu.edit_metadata =
                !track.cue && (track.source_key.is_some() || track.media_uri.starts_with("file:"));
            (true, true)
        }
        Row::Album(_) | Row::AlbumHeading(_) | Row::Artist(_) => {
            menu.edit_metadata = true;
            (true, true)
        }
        Row::Tag(tag) => (tag.kind != TagKind::Genre, false),
        Row::Playlist(PlaylistListRow::Playlist(playlist)) => {
            menu.export = true;
            menu.edit = playlist.metadata_writable;
            menu.delete = playlist.metadata_writable;
            menu.add_current = playlist.writable;
            menu.folder = Some(in_folder(
                owner,
                PlaylistFolderMember::Playlist {
                    key: playlist.playlist_key,
                },
            ));
            menu.remove_unavailable = crate::playlists::can_remove_unavailable(owner, playlist);
            (false, false)
        }
        Row::Playlist(PlaylistListRow::SmartPlaylist(playlist)) => {
            menu.export = true;
            menu.edit = true;
            menu.delete = true;
            menu.folder = Some(in_folder(
                owner,
                PlaylistFolderMember::SmartPlaylist {
                    key: playlist.smart_playlist_key,
                },
            ));
            (false, false)
        }
        Row::Playlist(PlaylistListRow::Folder(_)) => {
            menu.edit = true;
            menu.delete = true;
            (false, false)
        }
        // A child folder plays the folder it opens, as a collection row plays its page.
        Row::Folder(_) => (true, false),
    };
    menu.radio = radio_seed(row).filter(|_| shown(ContextMenuItem::PlayRadio));
    menu.add_to_playlist = add_to_playlist && shown(ContextMenuItem::AddToPlaylist);
    menu.favorite = rated && shown(ContextMenuItem::Favorites);
    menu.rating = rated && settings.application.context_menu.rating_visible;
    menu.edit_metadata &= shown(ContextMenuItem::EditMetadata);
    menu.go_to = shown(ContextMenuItem::GoTo) && targets(source, row).next().is_some();
    let downloaded = match row {
        Row::Folder(folder) => folder.summary.downloaded(),
        row => row.downloaded(),
    };
    menu.download = !downloaded && downloadable(source, row) && shown(ContextMenuItem::Download);
    menu.remove_download = downloaded && shown(ContextMenuItem::Download);
    menu.share = crate::sharing::offered(owner, row).filter(|_| shown(ContextMenuItem::Share));
    menu.pin = row
        .pin()
        .filter(|_| settings.sidebar.pins_visible && shown(ContextMenuItem::Pins));
    menu.pinned = menu
        .pin
        .as_ref()
        .is_some_and(|pin| settings.sidebar.is_pinned(pin));
    menu
}

/// The menu of several selected rows: their plays, playlists, metadata and downloads.
pub fn selection(settings: &Settings) -> Menu {
    let shown = |item| settings.application.context_menu.is_visible(item);
    Menu {
        play: shown(ContextMenuItem::Play),
        play_next: shown(ContextMenuItem::PlayNext),
        play_later: shown(ContextMenuItem::PlayLater),
        add_to_playlist: shown(ContextMenuItem::AddToPlaylist),
        edit_metadata: shown(ContextMenuItem::EditMetadata),
        download: shown(ContextMenuItem::Download),
        remove_download: shown(ContextMenuItem::Download),
        ..Menu::default()
    }
}

/// Where Go to on `row` leads: its artists, its album and its folder or server page.
pub fn go_to(owner: &SourceOwner, row: &Row) -> Vec<GoTo> {
    let stored = owner.shared.settings.load();
    targets(row_source(owner, &stored.sources, row), row).collect()
}

/// Go to's destinations, each built when it is asked for.
fn targets<'a>(
    source: Option<&'a SourceConfiguration>,
    row: &'a Row,
) -> impl Iterator<Item = GoTo> + 'a {
    type Targets<'a> = Box<dyn Iterator<Item = GoTo> + 'a>;
    let (targets, media_uri, album_uri): (Targets<'a>, _, _) = match row {
        Row::Track(track) | Row::AlbumTrack { track, .. } => {
            let (artists, album_artist) = crate::route::detail_links::go_to_artists(&track.links);
            let album = track.links.album_media_uri.as_deref();
            let artists = artists.iter().map(move |artist| {
                let uri = artist.media_uri.clone();
                let route = if album_artist {
                    Route::AlbumArtistDetail(uri)
                } else {
                    Route::ArtistDetail(uri)
                };
                GoTo::Artist(artist.name.clone(), route)
            });
            let album_target = album.map(|uri| GoTo::Album(Route::AlbumDetail(uri.to_owned())));
            (
                Box::new(artists.chain(album_target)),
                Some(track.media_uri.as_str()),
                album,
            )
        }
        Row::Album(album) | Row::AlbumHeading(album) => (
            Box::new(
                album
                    .links
                    .album_artists
                    .iter()
                    .map(|artist| {
                        GoTo::Artist(
                            artist.name.clone(),
                            Route::AlbumArtistDetail(artist.media_uri.clone()),
                        )
                    })
                    .chain(std::iter::once_with(|| {
                        GoTo::Album(Route::AlbumDetail(album.media_uri.clone()))
                    })),
            ),
            Some(album.media_uri.as_str()),
            None,
        ),
        Row::Artist(artist) => (
            Box::new(std::iter::once_with(|| {
                GoTo::Artist(artist.name.clone(), Route::of_artist(artist))
            })),
            Some(artist.media_uri.as_str()),
            None,
        ),
        _ => (Box::new(std::iter::empty()), None, None),
    };
    targets.chain(media_uri.into_iter().flat_map(move |media_uri| {
        crate::detail_links::navigation_source_link(source, media_uri, album_uri).map(GoTo::Source)
    }))
}

/// The configured source of the track, album or artist `row` shows, or of the child folder,
/// which the selected source's Folders lists.
fn row_source<'a>(
    owner: &SourceOwner,
    sources: &'a crate::settings::SourceSettings,
    row: &Row,
) -> Option<&'a SourceConfiguration> {
    let id = match row {
        Row::Track(track) | Row::AlbumTrack { track, .. } => match &track.source_id {
            Some(id) => SourceId::new(id),
            None => library::source_entity_parts(&track.media_uri)?.0,
        },
        Row::Album(album) | Row::AlbumHeading(album) => {
            library::source_entity_parts(&album.media_uri)?.0
        }
        Row::Artist(artist) => library::source_entity_parts(&artist.media_uri)?.0,
        Row::Folder(_) => owner.shared.selected()?.source_id,
        _ => return None,
    };
    sources
        .configured
        .iter()
        .map(|item| &item.configuration)
        .find(|configuration| configuration.source_id == id)
}

/// Whether Download applies: tracks of a remote source or a web address, child folders of a
/// remote source, and every genre, mood and playlist. Local files are already here.
fn downloadable(source: Option<&SourceConfiguration>, row: &Row) -> bool {
    let remote =
        || source.is_some_and(|source| source.source_kind() != Some(library::SourceKind::Local));
    let uri = match row {
        Row::Track(track) | Row::AlbumTrack { track, .. } => &track.media_uri,
        Row::Album(album) | Row::AlbumHeading(album) => &album.media_uri,
        Row::Artist(artist) => &artist.media_uri,
        Row::Tag(_)
        | Row::Playlist(PlaylistListRow::Playlist(_) | PlaylistListRow::SmartPlaylist(_)) => {
            return true;
        }
        Row::Folder(_) => return remote(),
        Row::Playlist(PlaylistListRow::Folder(_)) => return false,
    };
    if library::source_entity_parts(uri).is_some() {
        remote()
    } else {
        library::normalize_direct_media_uri(uri)
            .is_some_and(|uri| uri.starts_with("https:") || uri.starts_with("http:"))
    }
}

fn in_folder(owner: &SourceOwner, member: PlaylistFolderMember) -> bool {
    owner
        .shared
        .catalog
        .playlist_member_folder(&member)
        .is_some()
}
