//! Which actions a row's menu offers, as data. Each client labels, orders and draws them;
//! the saved menu settings hide the configurable ones.

use library::{PlaylistListRow, RadioSeed, Row, TagKind};

use crate::settings::app::Settings;
use crate::settings::{ContextMenuItem, SidebarPin};
use crate::source::SourceOwner;

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize)]
pub struct Menu {
    pub play: bool,
    pub play_next: bool,
    pub play_later: bool,
    pub radio: Option<RadioSeed>,
    pub add_to_playlist: bool,
    /// Add to or Remove from Favorites.
    pub favorite: bool,
    /// The row's pin and whether it is pinned.
    pub pin: Option<(SidebarPin, bool)>,
    pub go_to: bool,
    /// A playlist's or smart playlist's export to a file.
    pub export: bool,
    /// A playlist's name and file settings.
    pub edit: bool,
    pub remove_unavailable: bool,
    pub delete: bool,
}

/// The menu of `row` under `settings`.
pub fn menu(owner: &SourceOwner, settings: &Settings, row: &Row) -> Menu {
    let shown = |item| settings.application.context_menu.is_visible(item);
    let mut menu = Menu {
        play: shown(ContextMenuItem::Play),
        play_next: shown(ContextMenuItem::PlayNext),
        play_later: shown(ContextMenuItem::PlayLater),
        ..Menu::default()
    };
    let (radio, add_to_playlist, favorite, go_to) = match row {
        Row::Track(track) | Row::AlbumTrack { track, .. } => (
            Some(RadioSeed::Track(track.media_uri.clone())),
            true,
            true,
            track.links.album_key.is_some() || !track.links.artists.is_empty(),
        ),
        Row::Album(album) | Row::AlbumHeading(album) => (
            Some(RadioSeed::Album(album.album_key)),
            true,
            true,
            !album.links.album_artists.is_empty(),
        ),
        Row::Artist(artist) => (
            Some(if artist.album_artist {
                RadioSeed::AlbumArtist(artist.artist_key)
            } else {
                RadioSeed::Artist(artist.artist_key)
            }),
            true,
            true,
            true,
        ),
        Row::Tag(tag) if tag.kind == TagKind::Genre => (
            Some(RadioSeed::Genre(library::GenreKey::from_raw(tag.key))),
            false,
            false,
            false,
        ),
        Row::Tag(_) => (None, true, false, false),
        Row::Playlist(PlaylistListRow::Playlist(playlist)) => {
            menu.export = true;
            menu.edit = playlist.metadata_writable;
            menu.delete = playlist.metadata_writable;
            menu.remove_unavailable = crate::playlists::can_remove_unavailable(owner, playlist);
            (
                Some(RadioSeed::Playlist(playlist.playlist_key)),
                false,
                false,
                false,
            )
        }
        Row::Playlist(PlaylistListRow::SmartPlaylist(_)) => {
            menu.export = true;
            (None, false, false, false)
        }
        Row::Playlist(PlaylistListRow::Folder(_)) => (None, false, false, false),
        Row::Folder(_) => return Menu::default(),
    };
    menu.radio = radio.filter(|_| shown(ContextMenuItem::PlayRadio));
    menu.add_to_playlist = add_to_playlist && shown(ContextMenuItem::AddToPlaylist);
    menu.favorite = favorite && shown(ContextMenuItem::Favorites);
    menu.go_to = go_to && shown(ContextMenuItem::GoTo);
    menu.pin = row
        .pin()
        .filter(|_| settings.sidebar.pins_visible && shown(ContextMenuItem::Pins))
        .map(|pin| {
            let pinned = settings.sidebar.is_pinned(&pin);
            (pin, pinned)
        });
    menu
}
