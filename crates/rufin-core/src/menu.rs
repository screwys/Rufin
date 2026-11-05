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
    let shown = |item| settings.application.context_menu.is_visible(item);
    let source = row_source(owner, row);
    let mut menu = Menu {
        play: shown(ContextMenuItem::Play),
        play_next: shown(ContextMenuItem::PlayNext),
        play_later: shown(ContextMenuItem::PlayLater),
        ..Menu::default()
    };
    let (radio, add_to_playlist, rated) = match row {
        Row::Track(track) | Row::AlbumTrack { track, .. } => {
            menu.edit_metadata =
                !track.cue && (track.source_key.is_some() || track.media_uri.starts_with("file:"));
            (Some(RadioSeed::Track(track.media_uri.clone())), true, true)
        }
        Row::Album(album) | Row::AlbumHeading(album) => {
            menu.edit_metadata = true;
            (Some(RadioSeed::Album(album.album_key)), true, true)
        }
        Row::Artist(artist) => {
            menu.edit_metadata = true;
            let seed = if artist.album_artist {
                RadioSeed::AlbumArtist(artist.artist_key)
            } else {
                RadioSeed::Artist(artist.artist_key)
            };
            (Some(seed), true, true)
        }
        Row::Tag(tag) if tag.kind == TagKind::Genre => (
            Some(RadioSeed::Genre(library::GenreKey::from_raw(tag.key))),
            false,
            false,
        ),
        Row::Tag(_) => (None, true, false),
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
            (
                Some(RadioSeed::Playlist(playlist.playlist_key)),
                false,
                false,
            )
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
            (None, false, false)
        }
        Row::Playlist(PlaylistListRow::Folder(_)) => {
            menu.edit = true;
            menu.delete = true;
            menu.play = false;
            menu.play_next = false;
            menu.play_later = false;
            (None, false, false)
        }
        Row::Folder(_) => return Menu::default(),
    };
    menu.radio = radio.filter(|_| shown(ContextMenuItem::PlayRadio));
    menu.add_to_playlist = add_to_playlist && shown(ContextMenuItem::AddToPlaylist);
    menu.favorite = rated && shown(ContextMenuItem::Favorites);
    menu.rating = rated && settings.application.context_menu.rating_visible;
    menu.edit_metadata &= shown(ContextMenuItem::EditMetadata);
    menu.go_to = shown(ContextMenuItem::GoTo) && !targets(source.as_ref(), row).is_empty();
    let downloaded = row.downloaded();
    menu.download =
        !downloaded && downloadable(source.as_ref(), row) && shown(ContextMenuItem::Download);
    menu.remove_download = downloaded && shown(ContextMenuItem::Download);
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
    targets(row_source(owner, row).as_ref(), row)
}

fn targets(source: Option<&SourceConfiguration>, row: &Row) -> Vec<GoTo> {
    let (mut targets, media_uri, album_uri) = match row {
        Row::Track(track) | Row::AlbumTrack { track, .. } => {
            let (artists, album_artist) = crate::route::detail_links::go_to_artists(&track.links);
            let album = track.links.album_media_uri.as_deref();
            let mut targets = artists
                .iter()
                .map(|artist| {
                    let uri = artist.media_uri.clone();
                    let route = if album_artist {
                        Route::AlbumArtistDetail(uri)
                    } else {
                        Route::ArtistDetail(uri)
                    };
                    GoTo::Artist(artist.name.clone(), route)
                })
                .collect::<Vec<_>>();
            targets.extend(album.map(|uri| GoTo::Album(Route::AlbumDetail(uri.to_owned()))));
            (targets, &track.media_uri, album)
        }
        Row::Album(album) | Row::AlbumHeading(album) => {
            let mut targets = album
                .links
                .album_artists
                .iter()
                .map(|artist| {
                    GoTo::Artist(
                        artist.name.clone(),
                        Route::AlbumArtistDetail(artist.media_uri.clone()),
                    )
                })
                .collect::<Vec<_>>();
            targets.push(GoTo::Album(Route::AlbumDetail(album.media_uri.clone())));
            (targets, &album.media_uri, None)
        }
        Row::Artist(artist) => (
            vec![GoTo::Artist(artist.name.clone(), Route::of_artist(artist))],
            &artist.media_uri,
            None,
        ),
        _ => return Vec::new(),
    };
    targets.extend(
        crate::detail_links::navigation_source_link(source, media_uri, album_uri).map(GoTo::Source),
    );
    targets
}

/// The configured source of the track, album or artist `row` shows.
fn row_source(owner: &SourceOwner, row: &Row) -> Option<SourceConfiguration> {
    let id = match row {
        Row::Track(track) | Row::AlbumTrack { track, .. } => match &track.source_id {
            Some(id) => SourceId::new(id),
            None => library::source_entity_parts(&track.media_uri)?.0,
        },
        Row::Album(album) | Row::AlbumHeading(album) => {
            library::source_entity_parts(&album.media_uri)?.0
        }
        Row::Artist(artist) => library::source_entity_parts(&artist.media_uri)?.0,
        _ => return None,
    };
    owner.configuration(&id)
}

/// Whether Download applies: tracks of a remote source or a web address, and every genre,
/// mood and playlist. Local files are already here.
fn downloadable(source: Option<&SourceConfiguration>, row: &Row) -> bool {
    let uri = match row {
        Row::Track(track) | Row::AlbumTrack { track, .. } => &track.media_uri,
        Row::Album(album) | Row::AlbumHeading(album) => &album.media_uri,
        Row::Artist(artist) => &artist.media_uri,
        Row::Tag(_)
        | Row::Playlist(PlaylistListRow::Playlist(_) | PlaylistListRow::SmartPlaylist(_)) => {
            return true;
        }
        Row::Playlist(PlaylistListRow::Folder(_)) | Row::Folder(_) => return false,
    };
    if library::source_entity_parts(uri).is_some() {
        source.is_some_and(|source| source.source_kind() != Some(library::SourceKind::Local))
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
