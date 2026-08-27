use std::{cell::RefCell, rc::Rc};

use adw::prelude::*;
use library::{
    AlbumRow, ArtistRow, FavoriteTarget, GenreRow, MoodRow, PlaylistRow, RadioSeed,
    SmartPlaylistRow, TrackArtistLink, TrackKey, TrackRow,
};
use playback::{PlaybackMedia, QueuePlacement, RadioPlayRequest};

use crate::SidebarPin;
use crate::favorites::{FAVORITE_ADD_ICON, FAVORITE_REMOVE_ICON};
use crate::interactions::{
    ADD_TO_PLAYLIST_ICON, ContextMenuSurface, DOWNLOAD_ICON, GO_TO_ICON, RADIO_ICON,
    go_to_context_submenu, install_context_menu_openers, radio_context_submenu,
};
use crate::player::state::current_playback_track;
use crate::preferences::dialogs::SmartPlaylistChange;
use crate::preferences::dialogs::metadata::{MetadataItemId, present_metadata_dialog};
use crate::ratings::context_rating_row;
use crate::settings::ContextMenuItem;
use crate::shell::Shell;
use crate::shell::actions::{
    ADD_ICON, DELETE_ICON, EDIT_ICON, PLAY_ICON, PLAY_LATER_ICON, PLAY_NEXT_ICON, REMOVE_ICON,
    TRASH_ICON,
};
use localization::msgid;

use super::collections::{CollectionPlay, PlaybackTarget};
use super::playlist_picker::{
    PlaylistTrackSource, context_menu_can_add_to_playlist, install_context_menu_picker_action,
};
use super::route::Route;

#[derive(Clone)]
pub(crate) struct TrackContext {
    key: Option<TrackKey>,
    media: PlaybackMedia,
    favorite: bool,
    rating: Option<i64>,
    is_downloaded: bool,
    artists: Vec<TrackArtistLink>,
    artists_are_album_artists: bool,
    album: Option<library::AlbumKey>,
}

impl From<TrackRow> for TrackContext {
    fn from(row: TrackRow) -> Self {
        Self {
            key: Some(row.track_key),
            media: PlaybackMedia::from(row.clone()),
            favorite: row.favorite,
            rating: row.rating,
            is_downloaded: row.is_downloaded,
            artists_are_album_artists: row.artists.is_empty(),
            artists: if row.artists.is_empty() {
                row.album_artists.clone()
            } else {
                row.artists.clone()
            },
            album: row.album_key,
        }
    }
}

impl From<PlaybackMedia> for TrackContext {
    fn from(media: PlaybackMedia) -> Self {
        Self {
            key: media.track_key,
            favorite: media.favorite.unwrap_or(false),
            rating: media.rating,
            is_downloaded: media.is_downloaded,
            artists: Vec::new(),
            artists_are_album_artists: false,
            album: media.album_key,
            media,
        }
    }
}

pub(crate) fn install_dynamic_track_context_menu(
    target: &impl IsA<gtk::Widget>,
    shell: &Rc<Shell>,
    track: Rc<RefCell<Option<TrackRow>>>,
) {
    let shell = Rc::clone(shell);
    install_context_menu_openers(
        target,
        Rc::new(move |target, position| {
            if let Some(track) = track.borrow().clone() {
                present_track_context_menu(target, &shell, track, position);
            }
        }),
    );
}

pub(crate) fn install_dynamic_album_context_menu(
    target: &impl IsA<gtk::Widget>,
    shell: &Rc<Shell>,
    album: Rc<RefCell<Option<AlbumRow>>>,
    playback_context: Option<String>,
) {
    let shell = Rc::clone(shell);
    install_context_menu_openers(
        target,
        Rc::new(move |target, position| {
            if let Some(album) = album.borrow().clone() {
                present_album_context_menu(
                    target,
                    &shell,
                    album,
                    playback_context.clone(),
                    None,
                    position,
                );
            }
        }),
    );
}

pub(crate) fn install_current_track_context_menu(
    target: &impl IsA<gtk::Widget>,
    shell: &Rc<Shell>,
) {
    let shell = Rc::clone(shell);
    install_context_menu_openers(
        target,
        Rc::new(move |target, position| {
            if let Some(track) = current_playback_track(shell.selected_playback().as_deref()) {
                present_track_context_menu(target, &shell, track, position);
            }
        }),
    );
}

pub(crate) fn present_current_track_context_menu(
    target: &impl IsA<gtk::Widget>,
    shell: &Rc<Shell>,
) {
    if let Some(track) = current_playback_track(shell.selected_playback().as_deref()) {
        present_track_context_menu_above(target.as_ref(), shell, track, None);
    }
}

pub(crate) fn present_track_context_menu<T: Into<TrackContext>>(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    track: T,
    position: Option<(f64, f64)>,
) {
    present_track_menu(target, shell, track.into(), position, None, None, None);
}

pub(crate) fn present_playlist_entry_context_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    playlist: library::PlaylistKey,
    entry: library::PlaylistEntryKey,
    track: TrackRow,
    position: Option<(f64, f64)>,
) {
    present_track_menu(
        target,
        shell,
        track.into(),
        position,
        None,
        None,
        Some((playlist, entry)),
    );
}

pub(crate) fn present_queue_track_context_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    media: PlaybackMedia,
    primary_artist: Option<library::ArtistKey>,
    artist_name: &str,
    occurrence: playback::OccurrenceId,
    position: Option<(f64, f64)>,
) {
    let mut context = TrackContext::from(media);
    if let Some(artist_key) = primary_artist {
        context.artists.push(TrackArtistLink {
            artist_key,
            name: artist_name.to_string(),
        });
    }
    present_track_menu(
        target,
        shell,
        context,
        position,
        None,
        Some(occurrence),
        None,
    );
}

pub(crate) fn present_track_context_menu_above<T: Into<TrackContext>>(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    track: T,
    position: Option<(f64, f64)>,
) {
    present_track_menu(
        target,
        shell,
        track.into(),
        position,
        Some(gtk::PositionType::Top),
        None,
        None,
    );
}

fn present_track_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    track: TrackContext,
    position: Option<(f64, f64)>,
    popover_position: Option<gtk::PositionType>,
    queue_occurrence: Option<playback::OccurrenceId>,
    playlist_entry: Option<(library::PlaylistKey, library::PlaylistEntryKey)>,
) {
    let surface = ContextMenuSurface::new(
        target,
        if queue_occurrence.is_some() {
            "queue"
        } else if playlist_entry.is_some() {
            "playlist-entry"
        } else {
            "track"
        },
        position,
    );
    if queue_occurrence.is_some() {
        surface.append_fixed_action(
            msgid("Remove from Queue"),
            "remove-from-queue",
            crate::shell::actions::REMOVE_ICON,
        );
    }
    append_play_actions(&surface);
    if track.key.is_some() {
        surface.append_configurable_submenu(
            ContextMenuItem::PlayRadio,
            msgid("Track radio"),
            &radio_context_submenu("track"),
            RADIO_ICON,
        );
        if context_menu_can_add_to_playlist(shell) {
            surface.append_configurable_action(
                ContextMenuItem::AddToPlaylist,
                msgid("Add to Playlist"),
                "add-to-playlist",
                ADD_TO_PLAYLIST_ICON,
            );
        }
        surface.append_configurable_action(
            ContextMenuItem::Favorites,
            if track.favorite {
                msgid("Remove from Favorites")
            } else {
                msgid("Add to Favorites")
            },
            "favorite",
            if track.favorite {
                FAVORITE_REMOVE_ICON
            } else {
                FAVORITE_ADD_ICON
            },
        );
        if track.media.cue_path.is_none() {
            surface.append_configurable_action(
                ContextMenuItem::EditMetadata,
                msgid("Edit metadata"),
                "edit-metadata",
                EDIT_ICON,
            );
        }
    }
    let artist_names = track
        .artists
        .iter()
        .map(|artist| artist.name.clone())
        .collect::<Vec<_>>();
    if !artist_names.is_empty() || track.album.is_some() {
        surface.append_configurable_submenu(
            ContextMenuItem::GoTo,
            msgid("Go to"),
            &go_to_context_submenu("track", &artist_names, track.album.is_some()),
            GO_TO_ICON,
        );
    }
    if playlist_entry.is_some() {
        surface.append_fixed_action(
            msgid("Remove from Playlist"),
            "remove-from-playlist",
            crate::shell::actions::REMOVE_ICON,
        );
    }
    if let Some(position) = popover_position {
        surface.popover().set_position(position);
    }
    if let Some(key) = track.key {
        let playback = PlaybackTarget::Track(key);
        install_download_actions(&surface, shell, &playback, track.is_downloaded);
        if context_menu_can_add_to_playlist(shell) {
            install_context_menu_picker_action(
                &surface,
                shell,
                PlaylistTrackSource::new(playback.clone()),
            );
        }
        if queue_occurrence.is_none() {
            install_loaded_actions(&surface, shell, playback, false, None);
        }
        install_radio_actions(&surface, shell, RadioSeed::Track(key));
        let shell_favorite = Rc::clone(shell);
        let favorite = track.favorite;
        surface.add_action("favorite", move || {
            shell_favorite.set_favorite_with_feedback(FavoriteTarget::Track(key), !favorite, None);
        });
        if track.media.cue_path.is_none() {
            let metadata_shell = Rc::clone(shell);
            surface.add_action("edit-metadata", move || {
                present_metadata_dialog(&metadata_shell, MetadataItemId::Track(key));
            });
        }
        let artists = &track.artists;
        let album_artist = track.artists_are_album_artists;
        for (index, artist) in artists.iter().enumerate() {
            let action = if artists.len() == 1 {
                "go-artist".to_string()
            } else {
                format!("go-artist-{index}")
            };
            let shell = Rc::clone(shell);
            let key = artist.artist_key;
            surface.add_action(&action, move || {
                shell.navigate(if album_artist {
                    Route::AlbumArtistDetail(key)
                } else {
                    Route::ArtistDetail(key)
                })
            });
        }
        if context_menu_rating_visible(shell) {
            let shell = Rc::clone(shell);
            surface.append_fixed_widget(
                "rating",
                &context_rating_row(
                    track.rating.and_then(|value| u8::try_from(value).ok()),
                    shell.half_stars_enabled(),
                    surface.popover(),
                    move |rating| shell.set_rating(FavoriteTarget::Track(key), rating),
                ),
            );
        }
    } else {
        install_live_track_playback_actions(&surface, shell, track.media.clone());
    }
    if let Some(occurrence) = queue_occurrence {
        let queue = shell.products.playback.queue.clone();
        let remove = occurrence.clone();
        surface.add_action("remove-from-queue", move || queue.remove(remove.clone()));
        let queue = shell.products.playback.queue.clone();
        let activate = occurrence.clone();
        surface.add_action("play-now", move || queue.activate(activate.clone()));
        let queue = shell.products.playback.queue.clone();
        let next = occurrence.clone();
        surface.add_action("play-next", move || queue.move_after_current(next.clone()));
        let queue = shell.products.playback.queue.clone();
        surface.add_action("play-last", move || {
            queue.reorder(playback::QueueReorderRequest {
                occurrence: occurrence.clone(),
                target_index: usize::MAX,
                after: false,
            });
        });
    }
    if let Some((playlist, entry)) = playlist_entry {
        let operations = shell.selected_source_operations();
        surface.add_action("remove-from-playlist", move || {
            if let Some(operations) = operations.as_ref() {
                operations.remove_playlist_entries(playlist, vec![entry]);
            }
        });
    }
    if let Some(album) = track.album {
        let shell = Rc::clone(shell);
        surface.add_action("go-album", move || {
            shell.navigate(Route::AlbumDetail(album))
        });
    }
    surface.popup(&shell.settings.current.borrow().context_menu);
}

pub(crate) fn present_album_context_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    album: AlbumRow,
    playback_context: Option<String>,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
) {
    let base = PlaybackTarget::Album(album.album_key);
    let playback = playback_context
        .map(|context| base.clone().in_context(context))
        .unwrap_or(base);
    let favorite =
        shell.projected_item_favorite(&FavoriteTarget::Album(album.album_key), album.favorite);
    let surface = ContextMenuSurface::new(target, "album", position);
    append_play_actions(&surface);
    surface.append_configurable_submenu(
        ContextMenuItem::PlayRadio,
        msgid("Album radio"),
        &radio_context_submenu("album"),
        RADIO_ICON,
    );
    if context_menu_can_add_to_playlist(shell) {
        surface.append_configurable_action(
            ContextMenuItem::AddToPlaylist,
            msgid("Add to Playlist"),
            "add-to-playlist",
            ADD_TO_PLAYLIST_ICON,
        );
    }
    append_favorite_action(&surface, favorite);
    surface.append_configurable_action(
        ContextMenuItem::EditMetadata,
        msgid("Edit metadata"),
        "edit-metadata",
        EDIT_ICON,
    );
    install_sidebar_pin_action(
        &surface,
        shell,
        sidebar_pin_source(shell).map(|source_id| SidebarPin::Album {
            source_id,
            album_id: album.object_id.clone(),
        }),
    );
    let artist_names = album
        .album_artists
        .iter()
        .map(|artist| artist.name.clone())
        .collect::<Vec<_>>();
    if !artist_names.is_empty() {
        surface.append_configurable_submenu(
            ContextMenuItem::GoTo,
            msgid("Go to"),
            &go_to_context_submenu("album", &artist_names, true),
            GO_TO_ICON,
        );
    }
    install_download_actions(
        &surface,
        shell,
        &playback,
        album.track_count > 0 && album.downloaded_count == album.track_count,
    );
    install_loaded_actions(&surface, shell, playback, true, play);
    if context_menu_can_add_to_playlist(shell) {
        install_context_menu_picker_action(
            &surface,
            shell,
            PlaylistTrackSource::new(PlaybackTarget::Album(album.album_key)),
        );
    }
    install_radio_actions(&surface, shell, RadioSeed::Album(album.album_key));
    add_favorite_action(
        &surface,
        shell,
        FavoriteTarget::Album(album.album_key),
        favorite,
    );
    let album_key = album.album_key;
    let metadata_shell = Rc::clone(shell);
    surface.add_action("edit-metadata", move || {
        present_metadata_dialog(&metadata_shell, MetadataItemId::Album(album_key));
    });
    for (index, artist) in album.album_artists.iter().enumerate() {
        let action = if album.album_artists.len() == 1 {
            "go-artist".to_string()
        } else {
            format!("go-artist-{index}")
        };
        let shell = Rc::clone(shell);
        let key = artist.artist_key;
        surface.add_action(&action, move || {
            shell.navigate(Route::AlbumArtistDetail(key))
        });
    }
    let shell_album = Rc::clone(shell);
    surface.add_action("go-album", move || {
        shell_album.navigate(Route::AlbumDetail(album_key));
    });
    if context_menu_rating_visible(shell) {
        let shell = Rc::clone(shell);
        surface.append_fixed_widget(
            "rating",
            &context_rating_row(
                album.rating.and_then(|value| u8::try_from(value).ok()),
                shell.half_stars_enabled(),
                surface.popover(),
                move |rating| shell.set_rating(FavoriteTarget::Album(album_key), rating),
            ),
        );
    }
    surface.popup(&shell.settings.current.borrow().context_menu);
}

pub(crate) fn present_artist_context_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    artist: ArtistRow,
    album_artist: bool,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
) {
    let favorite =
        shell.projected_item_favorite(&FavoriteTarget::Artist(artist.artist_key), artist.favorite);
    let surface = ContextMenuSurface::new(target, "artist", position);
    append_play_actions(&surface);
    surface.append_configurable_submenu(
        ContextMenuItem::PlayRadio,
        msgid("Artist radio"),
        &radio_context_submenu("artist"),
        RADIO_ICON,
    );
    if context_menu_can_add_to_playlist(shell) {
        surface.append_configurable_action(
            ContextMenuItem::AddToPlaylist,
            msgid("Add to Playlist"),
            "add-to-playlist",
            ADD_TO_PLAYLIST_ICON,
        );
    }
    append_favorite_action(&surface, favorite);
    surface.append_configurable_action(
        ContextMenuItem::EditMetadata,
        msgid("Edit metadata"),
        "edit-metadata",
        EDIT_ICON,
    );
    install_sidebar_pin_action(
        &surface,
        shell,
        sidebar_pin_source(shell).map(|source_id| SidebarPin::Artist {
            source_id,
            artist_id: artist.object_id.clone(),
            album_artist,
        }),
    );
    surface.append_configurable_submenu(
        ContextMenuItem::GoTo,
        msgid("Go to"),
        &go_to_context_submenu("artist", std::slice::from_ref(&artist.name), false),
        GO_TO_ICON,
    );
    let playback = if album_artist {
        PlaybackTarget::AlbumArtist(artist.artist_key)
    } else {
        PlaybackTarget::Artist(artist.artist_key)
    };
    install_download_actions(
        &surface,
        shell,
        &playback,
        artist.track_count > 0 && artist.downloaded_count == artist.track_count,
    );
    install_loaded_actions(&surface, shell, playback, true, play);
    if context_menu_can_add_to_playlist(shell) {
        install_context_menu_picker_action(
            &surface,
            shell,
            PlaylistTrackSource::new(if album_artist {
                PlaybackTarget::AlbumArtist(artist.artist_key)
            } else {
                PlaybackTarget::Artist(artist.artist_key)
            }),
        );
    }
    install_radio_actions(
        &surface,
        shell,
        if album_artist {
            RadioSeed::AlbumArtist(artist.artist_key)
        } else {
            RadioSeed::Artist(artist.artist_key)
        },
    );
    add_favorite_action(
        &surface,
        shell,
        FavoriteTarget::Artist(artist.artist_key),
        favorite,
    );
    let artist_key = artist.artist_key;
    let metadata_shell = Rc::clone(shell);
    surface.add_action("edit-metadata", move || {
        present_metadata_dialog(&metadata_shell, MetadataItemId::Artist(artist_key));
    });
    let shell_artist = Rc::clone(shell);
    surface.add_action("go-artist", move || {
        shell_artist.navigate(if album_artist {
            Route::AlbumArtistDetail(artist_key)
        } else {
            Route::ArtistDetail(artist_key)
        });
    });
    if context_menu_rating_visible(shell) {
        let shell = Rc::clone(shell);
        surface.append_fixed_widget(
            "rating",
            &context_rating_row(
                artist.rating.and_then(|value| u8::try_from(value).ok()),
                shell.half_stars_enabled(),
                surface.popover(),
                move |rating| shell.set_rating(FavoriteTarget::Artist(artist_key), rating),
            ),
        );
    }
    surface.popup(&shell.settings.current.borrow().context_menu);
}

pub(crate) fn present_genre_context_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    genre: GenreRow,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
) {
    let surface = ContextMenuSurface::new(target, "genre", position);
    append_play_actions(&surface);
    surface.append_configurable_submenu(
        ContextMenuItem::PlayRadio,
        msgid("Genre radio"),
        &radio_context_submenu("genre"),
        RADIO_ICON,
    );
    install_sidebar_pin_action(
        &surface,
        shell,
        sidebar_pin_source(shell).map(|source_id| SidebarPin::Genre {
            source_id,
            genre_id: genre.object_id.clone(),
        }),
    );
    let playback = PlaybackTarget::Genre(genre.genre_key);
    install_download_actions(
        &surface,
        shell,
        &playback,
        genre.track_count > 0 && genre.downloaded_count == genre.track_count,
    );
    install_loaded_actions(&surface, shell, playback, true, play);
    install_radio_actions(&surface, shell, RadioSeed::Genre(genre.genre_key));
    surface.popup(&shell.settings.current.borrow().context_menu);
}

pub(crate) fn present_mood_context_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    mood: MoodRow,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
) {
    let surface = ContextMenuSurface::new(target, "mood", position);
    append_play_actions(&surface);
    let playback = PlaybackTarget::Mood(mood.mood_key);
    install_download_actions(
        &surface,
        shell,
        &playback,
        mood.track_count > 0 && mood.downloaded_count == mood.track_count,
    );
    install_loaded_actions(&surface, shell, playback.clone(), true, play);
    if context_menu_can_add_to_playlist(shell) {
        surface.append_configurable_action(
            ContextMenuItem::AddToPlaylist,
            msgid("Add to Playlist"),
            "add-to-playlist",
            ADD_TO_PLAYLIST_ICON,
        );
        install_context_menu_picker_action(&surface, shell, PlaylistTrackSource::new(playback));
    }
    surface.popup(&shell.settings.current.borrow().context_menu);
}

pub(crate) fn present_playlist_context_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    playlist: PlaylistRow,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
) {
    let surface = ContextMenuSurface::new(target, "playlist", position);
    append_play_actions(&surface);
    surface.append_configurable_submenu(
        ContextMenuItem::PlayRadio,
        msgid("Playlist radio"),
        &radio_context_submenu("playlist"),
        RADIO_ICON,
    );
    install_sidebar_pin_action(
        &surface,
        shell,
        sidebar_pin_source(shell).map(|source_id| SidebarPin::Playlist {
            source_id,
            playlist_id: playlist.object_id.clone(),
        }),
    );
    surface.append_fixed_action(msgid("Rename"), "rename", EDIT_ICON);
    surface.append_fixed_action(msgid("Add current"), "add-current", ADD_ICON);
    surface.append_fixed_action(msgid("Delete"), "delete", DELETE_ICON);
    let playback = PlaybackTarget::Playlist(playlist.playlist_key);
    install_download_actions(
        &surface,
        shell,
        &playback,
        playlist.track_count > 0 && playlist.downloaded_count == playlist.track_count,
    );
    install_loaded_actions(&surface, shell, playback, true, play);
    install_radio_actions(&surface, shell, RadioSeed::Playlist(playlist.playlist_key));
    let rename_shell = Rc::clone(shell);
    let playlist_key = playlist.playlist_key;
    let playlist_name = playlist.name.clone();
    surface.add_action("rename", move || {
        rename_shell.rename_playlist_dialog(playlist_key, playlist_name.clone());
    });
    let current = current_playback_track(shell.selected_playback().as_deref())
        .and_then(|track| track.track_key);
    let operations = shell.selected_source_operations();
    let add_shell = Rc::clone(shell);
    surface.add_action_enabled("add-current", current.is_some(), move || {
        if let (Some(operations), Some(track)) = (operations.as_ref(), current) {
            let skip_duplicates = add_shell
                .selected_library()
                .as_deref()
                .is_none_or(|selected| !selected.playlist_tracks_can_repeat);
            operations.add_playlist_tracks(playlist_key, vec![track], skip_duplicates);
        }
    });
    let delete_shell = Rc::clone(shell);
    surface.add_action("delete", move || {
        if let Some(operations) = delete_shell.selected_source_operations() {
            operations.delete_playlist(playlist_key);
        }
        delete_shell.navigate(Route::Playlists);
    });
    surface.popup(&shell.settings.current.borrow().context_menu);
}

pub(crate) fn present_smart_playlist_context_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    playlist: SmartPlaylistRow,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
) {
    let surface = ContextMenuSurface::new(target, "smart-playlist", position);
    append_play_actions(&surface);
    install_sidebar_pin_action(
        &surface,
        shell,
        sidebar_pin_source(shell).map(|source_id| SidebarPin::SmartPlaylist {
            source_id,
            playlist_id: playlist.object_id.clone(),
        }),
    );
    surface.append_fixed_action(msgid("Edit"), "edit-definition", EDIT_ICON);
    surface.append_fixed_action(msgid("Delete"), "delete", DELETE_ICON);
    let playback = PlaybackTarget::SmartPlaylist(playlist.smart_playlist_key);
    install_download_actions(
        &surface,
        shell,
        &playback,
        playlist.track_count > 0 && playlist.downloaded_count == playlist.track_count,
    );
    install_loaded_actions(&surface, shell, playback, true, play);
    let edit_shell = Rc::clone(shell);
    let edit_row = playlist.clone();
    surface.add_action("edit-definition", move || {
        edit_shell.edit_smart_playlist_dialog(edit_row.clone());
    });
    let delete_shell = Rc::clone(shell);
    let key = playlist.smart_playlist_key;
    surface.add_action("delete", move || {
        let navigate = Rc::clone(&delete_shell);
        delete_shell.publish_smart_playlist_change(
            SmartPlaylistChange::Delete(key),
            Some(Rc::new(move |result| {
                if result.is_ok() {
                    navigate.navigate(Route::SmartPlaylists);
                }
            })),
        );
    });
    surface.popup(&shell.settings.current.borrow().context_menu);
}

fn append_play_actions(surface: &ContextMenuSurface) {
    surface.append_configurable_action(ContextMenuItem::Play, msgid("Play"), "play", PLAY_ICON);
    surface.append_configurable_action(
        ContextMenuItem::PlayNext,
        msgid("Play Next"),
        "play-next",
        PLAY_NEXT_ICON,
    );
    surface.append_configurable_action(
        ContextMenuItem::PlayLater,
        msgid("Play Later"),
        "play-last",
        PLAY_LATER_ICON,
    );
}

fn append_favorite_action(surface: &ContextMenuSurface, favorite: bool) {
    surface.append_configurable_action(
        ContextMenuItem::Favorites,
        if favorite {
            msgid("Remove from Favorites")
        } else {
            msgid("Add to Favorites")
        },
        "favorite",
        if favorite {
            FAVORITE_REMOVE_ICON
        } else {
            FAVORITE_ADD_ICON
        },
    );
}

fn add_favorite_action(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    target: FavoriteTarget,
    favorite: bool,
) {
    let shell = Rc::clone(shell);
    surface.add_action("favorite", move || {
        shell.set_favorite_with_feedback(target, !favorite, None);
    });
}

fn install_sidebar_pin_action(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    pin: Option<SidebarPin>,
) {
    let Some(pin) = pin else { return };
    let settings = shell.settings.current.borrow();
    if !sidebar_pin_action_available(&settings) {
        return;
    }
    let pinned = settings.sidebar.is_pinned(&pin);
    drop(settings);
    surface.append_configurable_action(
        ContextMenuItem::Pins,
        if pinned {
            msgid("Remove from Pins")
        } else {
            msgid("Add to Pins")
        },
        "pin",
        if pinned { REMOVE_ICON } else { ADD_ICON },
    );
    let shell = Rc::clone(shell);
    surface.add_action("pin", move || shell.set_sidebar_pin(pin.clone(), !pinned));
}

fn sidebar_pin_source(shell: &Shell) -> Option<sources::SourceId> {
    shell
        .selected_library()
        .as_deref()
        .map(|selected| selected.artwork.source_id.clone())
}

fn sidebar_pin_action_available(settings: &crate::Settings) -> bool {
    settings.sidebar.pins_visible
}

fn install_loaded_actions(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    target: PlaybackTarget,
    shuffled_start: bool,
    play: Option<CollectionPlay>,
) {
    for (action, placement) in [
        ("play", QueuePlacement::Now),
        ("play-next", QueuePlacement::Next),
        ("play-last", QueuePlacement::Last),
    ] {
        let target = target.clone();
        let shell = Rc::clone(shell);
        let play = play.clone();
        surface.add_action(action, move || {
            if let Some(play) = &play {
                play(
                    placement,
                    shuffled_start && placement == QueuePlacement::Now,
                );
            } else {
                target.play(&shell, placement, shuffled_start);
            }
        });
    }
}

fn install_live_track_playback_actions(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    track: PlaybackMedia,
) {
    for (action, placement) in [
        ("play", QueuePlacement::Now),
        ("play-next", QueuePlacement::Next),
        ("play-last", QueuePlacement::Last),
    ] {
        let shell = Rc::clone(shell);
        let track = track.clone();
        surface.add_action(action, move || {
            let Some(selected) = shell.selected_library().as_deref().cloned() else {
                return;
            };
            if let Some(request) = selected.one_track(track.clone(), placement) {
                shell.products.playback.queue.play_loaded(request);
            }
        });
    }
}

pub(crate) fn install_download_actions(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    target: &PlaybackTarget,
    downloaded: bool,
) {
    let Some(selected) = shell.selected_library().as_deref().cloned() else {
        return;
    };
    let remote = shell
        .source
        .configured
        .borrow()
        .sources
        .iter()
        .any(|source| source.id == selected.artwork.source_id && source.kind != "local");
    if !remote {
        return;
    }
    if downloaded {
        surface.append_configurable_action(
            ContextMenuItem::Download,
            if matches!(target, PlaybackTarget::Track(_)) {
                msgid("Remove Download")
            } else {
                msgid("Remove Downloads")
            },
            "remove-downloads",
            TRASH_ICON,
        );
        let remove_shell = Rc::clone(shell);
        let remove_target = target.clone();
        surface.add_action("remove-downloads", move || {
            remove_target.remove_download(&remove_shell)
        });
    } else {
        surface.append_configurable_action(
            ContextMenuItem::Download,
            msgid("Download"),
            "download",
            DOWNLOAD_ICON,
        );
        let download_shell = Rc::clone(shell);
        let download_target = target.clone();
        surface.add_action("download", move || {
            download_target.download(&download_shell)
        });
    }
}

fn install_radio_actions(surface: &ContextMenuSurface, shell: &Shell, seed: RadioSeed) {
    for (action, request) in [
        ("play-radio", RadioPlayRequest::now(seed)),
        ("play-radio-next", RadioPlayRequest::next(seed)),
        ("play-radio-last", RadioPlayRequest::last(seed)),
    ] {
        let radio = shell.products.playback.radio.clone();
        surface.add_action(action, move || radio.play_radio(request.clone()));
    }
}

fn context_menu_rating_visible(shell: &Shell) -> bool {
    shell.settings.current.borrow().context_menu.rating_visible
}

#[cfg(test)]
mod tests {
    use super::sidebar_pin_action_available;
    use crate::Settings;

    #[test]
    fn disabling_sidebar_pins_also_removes_context_menu_pin_actions() {
        let mut settings = Settings::default();
        settings.sidebar.pins_visible = false;
        assert!(!sidebar_pin_action_available(&settings));
    }
}
