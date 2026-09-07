use std::{rc::Rc, sync::Arc};

use adw::prelude::*;
use downloads::DownloadSubject;
use gtk::glib;
use library::{
    AlbumRow, ArtistRow, FavoriteTarget, GenreRow, MoodRow, PlaylistRow, RadioSeed,
    SmartPlaylistRow, TrackRow,
};
use playback::{QueuePlacement, RadioPlayRequest};

use crate::SidebarPin;
use crate::downloads::{OperationFeedback, OperationFeedbackKind};
use crate::favorites::{FAVORITE_ADD_ICON, FAVORITE_REMOVE_ICON};
use crate::interactions::{
    ContextMenuSurface, DOWNLOAD_ICON, GO_TO_ICON, RADIO_ICON, go_to_context_submenu,
    install_context_menu_openers, radio_context_submenu,
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
    append_context_menu_picker, append_context_menu_picker_entries,
    append_context_menu_picker_media, append_context_menu_picker_selection,
};
use super::route::Route;
use super::track_selection::{PlaylistEntrySelectionSnapshot, TrackSelectionSnapshot};

pub(crate) fn install_current_track_context_menu(
    target: &impl IsA<gtk::Widget>,
    shell: &Rc<Shell>,
) {
    let shell = Rc::clone(shell);
    install_context_menu_openers(
        target,
        Rc::new(move |target, position| {
            if let Some(track) = current_playback_track(shell.selected_playback().as_deref()) {
                present_playback_media_menu(
                    target,
                    &shell,
                    track.media_uri.clone(),
                    Some(track),
                    position,
                    None,
                    None,
                );
            }
        }),
    );
}

pub(crate) fn present_current_track_context_menu(
    target: &impl IsA<gtk::Widget>,
    shell: &Rc<Shell>,
) {
    if let Some(track) = current_playback_track(shell.selected_playback().as_deref()) {
        present_playback_media_menu(
            target.as_ref(),
            shell,
            track.media_uri.clone(),
            Some(track),
            None,
            Some(gtk::PositionType::Top),
            None,
        );
    }
}

pub(crate) fn present_track_context_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    media_uri: String,
    position: Option<(f64, f64)>,
) {
    if let Some(selection) = shell.current_route_track_selection(&media_uri) {
        present_track_selection_menu(target, shell, selection, position);
        return;
    }
    present_playback_media_menu(target, shell, media_uri, None, position, None, None);
}

pub(crate) fn present_playlist_entry_context_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    playlist: library::PlaylistKey,
    entry: library::PlaylistEntryKey,
    entry_row: library::PlaylistEntryRow,
    position: Option<(f64, f64)>,
) {
    if let Some(selection) = shell.current_playlist_entry_selection(entry) {
        present_playlist_entry_selection_menu(target, shell, selection, position);
        return;
    }
    present_playlist_entry_menu(target, shell, entry_row, position, playlist, entry);
}

pub(crate) fn present_queue_track_context_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    media: library::QueueItem,
    occurrence: playback::OccurrenceId,
    position: Option<(f64, f64)>,
) {
    present_playback_media_menu(
        target,
        shell,
        media.media_uri.clone(),
        Some(media),
        position,
        None,
        Some(occurrence),
    );
}

fn present_catalog_track_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    track: TrackRow,
    position: Option<(f64, f64)>,
    popover_position: Option<gtk::PositionType>,
    playback_media: Option<library::QueueItem>,
    queue_occurrence: Option<playback::OccurrenceId>,
) {
    let action_group = if queue_occurrence.is_some() {
        "queue"
    } else {
        "track"
    };
    let surface = ContextMenuSurface::new(target, action_group, position);
    if queue_occurrence.is_some() {
        surface.append_fixed_action(msgid("Remove from Queue"), "remove-from-queue", REMOVE_ICON);
    }
    append_play_actions(&surface);
    surface.append_configurable_submenu(
        ContextMenuItem::PlayRadio,
        msgid("Track radio"),
        &radio_context_submenu("track"),
        RADIO_ICON,
    );
    append_context_menu_picker(
        &surface,
        shell,
        PlaybackTarget::Track(track.media_uri.clone()),
    );
    let favorite = shell.projected_track_favorite(&track.media_uri, track.favorite);
    append_favorite_action(&surface, favorite);
    if track.cue_path.is_none() {
        surface.append_configurable_action(
            ContextMenuItem::EditMetadata,
            msgid("Edit metadata"),
            "edit-metadata",
            EDIT_ICON,
        );
    }
    let artists = if track.artists.is_empty() {
        &track.album_artists
    } else {
        &track.artists
    };
    let artist_names = artists
        .iter()
        .map(|artist| artist.name.clone())
        .collect::<Vec<_>>();

    if !artist_names.is_empty() || track.album_media_uri.is_some() {
        surface.append_configurable_submenu(
            ContextMenuItem::GoTo,
            msgid("Go to"),
            &go_to_context_submenu("track", &artist_names, track.album_media_uri.is_some()),
            GO_TO_ICON,
        );
    }
    let playback = PlaybackTarget::Track(track.media_uri.clone());
    install_download_actions(&surface, shell, &playback, track.is_downloaded);
    if let Some(media) = playback_media {
        install_live_track_playback_actions(&surface, shell, media);
    } else {
        install_media_uri_playback_actions(&surface, shell, track.media_uri.clone());
    }
    install_radio_actions(&surface, shell, RadioSeed::Track(track.media_uri.clone()));
    add_favorite_action(
        &surface,
        shell,
        FavoriteTarget::Track(track.media_uri.clone()),
        favorite,
    );
    if track.cue_path.is_none() {
        let metadata_shell = Rc::clone(shell);
        let media_uri = track.media_uri.clone();
        surface.add_action("edit-metadata", move || {
            present_metadata_dialog(&metadata_shell, MetadataItemId::Track(media_uri.clone()));
        });
    }
    let album_artist = track.artists.is_empty();
    for (index, artist) in artists.iter().enumerate() {
        let action = if artists.len() == 1 {
            "go-artist".to_string()
        } else {
            format!("go-artist-{index}")
        };
        let shell = Rc::clone(shell);
        let key = artist.media_uri.clone();
        surface.add_action(&action, move || {
            shell.navigate(if album_artist {
                Route::AlbumArtistDetail(key.clone())
            } else {
                Route::ArtistDetail(key.clone())
            })
        });
    }
    if context_menu_rating_visible(shell) {
        let shell = Rc::clone(shell);
        let media_uri = track.media_uri.clone();
        surface.append_fixed_widget(
            "rating",
            &context_rating_row(
                track.rating.and_then(|value| u8::try_from(value).ok()),
                shell.half_stars_enabled(&track.media_uri, Some(&track.source_id)),
                surface.popover(),
                move |rating| shell.set_rating(FavoriteTarget::Track(media_uri.clone()), rating),
            ),
        );
    }
    if let Some(album) = track.album_media_uri.clone() {
        let shell = Rc::clone(shell);
        surface.add_action("go-album", move || {
            shell.navigate(Route::AlbumDetail(album.clone()))
        });
    }
    if let Some(position) = popover_position {
        surface.popover().set_position(position);
    }
    if let Some(occurrence) = queue_occurrence {
        let queue = shell.products.playback.queue.clone();
        let remove = occurrence.clone();
        surface.add_action("remove-from-queue", move || queue.remove(remove.clone()));
        let queue = shell.products.playback.queue.clone();
        let activate = occurrence.clone();
        surface.add_action("play", move || queue.activate(activate.clone()));
        let queue = shell.products.playback.queue.clone();
        let next = occurrence.clone();
        surface.add_action("play-next", move || queue.move_after_current(next.clone()));
        let queue = shell.products.playback.queue.clone();
        surface.add_action("play-last", move || {
            queue.reorder(playback::QueueReorderRequest {
                occurrences: vec![occurrence.clone()],
                target: playback::QueueReorderTarget::End,
            });
        });
    }
    surface.popup(&shell.settings.current.borrow().context_menu);
}

pub(crate) fn present_playback_media_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    media_uri: String,
    media: Option<library::QueueItem>,
    position: Option<(f64, f64)>,
    popover_position: Option<gtk::PositionType>,
    queue_occurrence: Option<playback::OccurrenceId>,
) {
    let database = Arc::clone(&shell.products.library);
    let runtime = shell.products.runtime.clone();
    let task = runtime.spawn(async move {
        let cancellation = library::ReadCancellation::new();
        if let Some(track) = database.track_row_by_uri(&media_uri, &cancellation).await? {
            Ok::<_, library::LibraryError>((Some(track), media, None, false))
        } else {
            let state = database.user_media_state(&media_uri, &cancellation).await?;
            let downloaded = !database
                .retaining_download_rows(std::slice::from_ref(&media_uri), &cancellation)
                .await?
                .is_empty();
            let media = match media {
                Some(media) => Some(media),
                None => database
                    .queue_items_for_uris(&[media_uri], &cancellation)
                    .await?
                    .pop(),
            };
            Ok((None, media, state, downloaded))
        }
    });
    let target = target.clone();
    let shell = Rc::downgrade(shell);
    glib::spawn_future_local(async move {
        let Ok(Ok((track, media, user_state, downloaded))) = task.await else {
            return;
        };
        let Some(shell) = shell.upgrade() else {
            return;
        };
        if let Some(track) = track {
            present_catalog_track_menu(
                &target,
                &shell,
                track,
                position,
                popover_position,
                media,
                queue_occurrence,
            );
        } else if let Some(media) = media {
            present_direct_playback_media_menu(
                &target,
                &shell,
                media,
                user_state,
                downloaded,
                position,
                popover_position,
                queue_occurrence,
            );
        }
    });
}

fn present_direct_playback_media_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    media: library::QueueItem,
    user_state: Option<(Option<bool>, Option<u8>)>,
    downloaded: bool,
    position: Option<(f64, f64)>,
    popover_position: Option<gtk::PositionType>,
    queue_occurrence: Option<playback::OccurrenceId>,
) {
    let action_group = if queue_occurrence.is_some() {
        "queue"
    } else {
        "track"
    };
    let surface = ContextMenuSurface::new(target, action_group, position);
    if queue_occurrence.is_some() {
        surface.append_fixed_action(msgid("Remove from Queue"), "remove-from-queue", REMOVE_ICON);
    }
    append_play_actions(&surface);
    append_context_menu_picker_media(&surface, shell, media.media_uri.clone());
    install_download_actions(
        &surface,
        shell,
        &PlaybackTarget::Track(media.media_uri.clone()),
        downloaded,
    );
    if media.media_uri.starts_with("file:") {
        surface.append_configurable_action(
            ContextMenuItem::EditMetadata,
            msgid("Edit metadata"),
            "edit-metadata",
            EDIT_ICON,
        );
        let metadata_shell = Rc::clone(shell);
        let media_uri = media.media_uri.clone();
        surface.add_action("edit-metadata", move || {
            present_metadata_dialog(&metadata_shell, MetadataItemId::Track(media_uri.clone()))
        });
    }
    let (favorite, rating) = user_state
        .map(|(favorite, rating)| (favorite.unwrap_or(false), rating))
        .unwrap_or((false, None));
    let favorite = shell.projected_track_favorite(&media.media_uri, favorite);
    append_favorite_action(&surface, favorite);
    add_favorite_action(
        &surface,
        shell,
        FavoriteTarget::Track(media.media_uri.clone()),
        favorite,
    );
    if context_menu_rating_visible(shell) {
        let rating_shell = Rc::clone(shell);
        let media_uri = media.media_uri.clone();
        surface.append_fixed_widget(
            "rating",
            &context_rating_row(
                rating,
                rating_shell.half_stars_enabled(&media.media_uri, None),
                surface.popover(),
                move |rating| {
                    rating_shell.set_rating(FavoriteTarget::Track(media_uri.clone()), rating)
                },
            ),
        );
    }
    if let Some(position) = popover_position {
        surface.popover().set_position(position);
    }
    install_live_track_playback_actions(&surface, shell, media);
    if let Some(occurrence) = queue_occurrence {
        let queue = shell.products.playback.queue.clone();
        let remove = occurrence.clone();
        surface.add_action("remove-from-queue", move || queue.remove(remove.clone()));
        let queue = shell.products.playback.queue.clone();
        let activate = occurrence.clone();
        surface.add_action("play", move || queue.activate(activate.clone()));
        let queue = shell.products.playback.queue.clone();
        let next = occurrence.clone();
        surface.add_action("play-next", move || queue.move_after_current(next.clone()));
        let queue = shell.products.playback.queue.clone();
        surface.add_action("play-last", move || {
            queue.reorder(playback::QueueReorderRequest {
                occurrences: vec![occurrence.clone()],
                target: playback::QueueReorderTarget::End,
            });
        });
    }
    surface.popup(&shell.settings.current.borrow().context_menu);
}

fn present_playlist_entry_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    row: library::PlaylistEntryRow,
    position: Option<(f64, f64)>,
    _playlist: library::PlaylistKey,
    entry: library::PlaylistEntryKey,
) {
    let surface = ContextMenuSurface::new(target, "playlist-entry", position);
    append_play_actions(&surface);
    surface.append_fixed_action(
        msgid("Remove from Playlist"),
        "remove-from-playlist",
        REMOVE_ICON,
    );
    append_context_menu_picker_media(&surface, shell, row.media_uri.clone());
    install_download_actions(
        &surface,
        shell,
        &PlaybackTarget::Track(row.media_uri.clone()),
        row.is_downloaded,
    );
    if row.media_uri.starts_with("file:") || library::source_entity_parts(&row.media_uri).is_some()
    {
        surface.append_configurable_action(
            ContextMenuItem::EditMetadata,
            msgid("Edit metadata"),
            "edit-metadata",
            EDIT_ICON,
        );
        let metadata_shell = Rc::clone(shell);
        let media_uri = row.media_uri.clone();
        surface.add_action("edit-metadata", move || {
            present_metadata_dialog(&metadata_shell, MetadataItemId::Track(media_uri.clone()))
        });
    }
    let favorite = shell.projected_track_favorite(&row.media_uri, row.favorite);
    append_favorite_action(&surface, favorite);
    add_favorite_action(
        &surface,
        shell,
        FavoriteTarget::Track(row.media_uri.clone()),
        favorite,
    );
    if context_menu_rating_visible(shell) {
        let rating_shell = Rc::clone(shell);
        let media_uri = row.media_uri.clone();
        surface.append_fixed_widget(
            "rating",
            &context_rating_row(
                row.rating.and_then(|value| u8::try_from(value).ok()),
                rating_shell.half_stars_enabled(&row.media_uri, row.source_id.as_deref()),
                surface.popover(),
                move |rating| {
                    rating_shell.set_rating(FavoriteTarget::Track(media_uri.clone()), rating)
                },
            ),
        );
    }
    install_media_uri_playback_actions(&surface, shell, row.media_uri.clone());
    let removal = shell
        .current_playlist_entry_selection_owner()
        .map(|selection| selection.single_entry(entry));
    let remove_shell = Rc::downgrade(shell);
    surface.add_action("remove-from-playlist", move || {
        if let (Some(shell), Some(removal)) = (remove_shell.upgrade(), removal.as_ref()) {
            remove_playlist_entry_selection(&shell, removal.clone());
        }
    });
    surface.popup(&shell.settings.current.borrow().context_menu);
}

fn present_track_selection_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    selection: TrackSelectionSnapshot,
    position: Option<(f64, f64)>,
) {
    let surface = ContextMenuSurface::new(target, "track-selection", position);
    append_track_selection_actions(&surface, shell, selection);
    surface.popup(&shell.settings.current.borrow().context_menu);
}

fn present_playlist_entry_selection_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    selection: PlaylistEntrySelectionSnapshot,
    position: Option<(f64, f64)>,
) {
    let surface = ContextMenuSurface::new(target, "playlist-entry-selection", position);
    surface.append_fixed_action(
        msgid("Remove from Playlist"),
        "remove-from-playlist",
        REMOVE_ICON,
    );
    append_playlist_entry_selection_actions(&surface, shell, selection.clone());
    let remove_shell = Rc::clone(shell);
    surface.add_action("remove-from-playlist", move || {
        remove_playlist_entry_selection(&remove_shell, selection.clone());
    });
    surface.popup(&shell.settings.current.borrow().context_menu);
}

pub(crate) fn remove_playlist_entry_selection(
    shell: &Rc<Shell>,
    selection: PlaylistEntrySelectionSnapshot,
) -> bool {
    if selection.entries.is_empty() {
        return false;
    }
    let item_count = selection.entries.len();
    let task_selection = selection.clone();
    let database = shell.products.library.clone();
    let task = shell
        .products
        .runtime
        .spawn(async move { task_selection.media_uris(&database).await });
    let shell = Rc::downgrade(shell);
    gtk::glib::spawn_future_local(async move {
        let Ok(Ok(media_uris)) = task.await else {
            return;
        };
        let Some(shell) = shell.upgrade() else { return };
        if media_uris.len() != item_count {
            return;
        }
        shell.remove_media_from_playlist(selection.playlist, selection.entries.to_vec());
        let feedback = OperationFeedback {
            subject: DownloadSubject::for_media_uris("playlist", Some("Playlist"), &media_uris),
            preview_uris: media_uris.iter().take(4).cloned().collect(),
            item_count,
            kind: OperationFeedbackKind::PlaylistRemoved {
                destination: selection.playlist_name.to_string(),
            },
        };
        let shell_for_undo = Rc::downgrade(&shell);
        let playlist = selection.playlist;
        shell.show_undoable_operation_feedback(&feedback, move || {
            let Some(shell) = shell_for_undo.upgrade() else {
                return;
            };
            shell.add_media_to_playlist(playlist, media_uris.clone(), false, Rc::new(|_| {}));
        });
    });
    true
}

fn append_playlist_entry_selection_actions(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    selection: PlaylistEntrySelectionSnapshot,
) {
    if selection.entries.is_empty() {
        return;
    }
    append_play_actions(surface);
    append_context_menu_picker_entries(surface, shell, selection.clone());
    let download_shell = Rc::clone(shell);
    let download_selection = selection.clone();
    surface.append_configurable_action(
        ContextMenuItem::Download,
        msgid("Download"),
        "download",
        DOWNLOAD_ICON,
    );
    surface.add_action("download", move || {
        download_playlist_entry_selection(&download_shell, download_selection.clone());
    });
    for (action, placement) in [
        ("play", QueuePlacement::Now),
        ("play-next", QueuePlacement::Next),
        ("play-last", QueuePlacement::Last),
    ] {
        let shell = Rc::clone(shell);
        let selection = selection.clone();
        surface.add_action(action, move || selection.play(&shell, placement));
    }
}

pub(crate) fn download_playlist_entry_selection(
    shell: &Rc<Shell>,
    selection: PlaylistEntrySelectionSnapshot,
) -> bool {
    let source = shell.products.source.clone();
    let database = shell.products.library.clone();
    shell.products.runtime.spawn(async move {
        let Ok(media_uris) = selection.media_uris(&database).await else {
            return;
        };
        let subject = DownloadSubject::for_media_uris("playlist", Some("Playlist"), &media_uris);
        source.download_media(subject, media_uris);
    });
    true
}

fn append_track_selection_actions(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    selection: TrackSelectionSnapshot,
) {
    if selection.media_uris.is_empty() {
        return;
    }
    append_play_actions(&surface);
    append_context_menu_picker_selection(&surface, shell, selection.clone());
    install_track_selection_download_actions(&surface, shell, selection.clone());
    for (action, placement) in [
        ("play", QueuePlacement::Now),
        ("play-next", QueuePlacement::Next),
        ("play-last", QueuePlacement::Last),
    ] {
        let shell = Rc::clone(shell);
        let selection = selection.clone();
        surface.add_action(action, move || selection.play(&shell, placement));
    }
}

pub(crate) fn install_track_selection_download_actions(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    selection: TrackSelectionSnapshot,
) {
    surface.append_configurable_action(
        ContextMenuItem::Download,
        msgid("Download"),
        "download",
        DOWNLOAD_ICON,
    );
    surface.append_configurable_action(
        ContextMenuItem::Download,
        msgid("Remove Downloads"),
        "remove-downloads",
        TRASH_ICON,
    );
    let download_shell = Rc::clone(shell);
    let download_selection = selection.clone();
    surface.add_action("download", move || {
        download_track_selection(&download_shell, download_selection.clone());
    });
    let remove_shell = Rc::clone(shell);
    surface.add_action("remove-downloads", move || {
        let downloads = remove_shell.products.downloads.clone();
        downloads.remove(selection.media_uris.to_vec(), true);
    });
}

pub(crate) fn download_track_selection(
    shell: &Rc<Shell>,
    selection: TrackSelectionSnapshot,
) -> bool {
    let source = shell.products.source.clone();
    let subject = selection.download_subject();
    let media_uris = selection.media_uris.to_vec();
    source.download_media(subject, media_uris);
    true
}

pub(crate) fn present_album_context_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    album: AlbumRow,
    playback_context: Option<String>,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
) {
    let base = PlaybackTarget::Album(album.media_uri.clone());
    let playback = playback_context
        .map(|context| base.clone().in_context(context))
        .unwrap_or(base);
    let favorite = shell.projected_item_favorite(
        &FavoriteTarget::Album(album.media_uri.clone()),
        album.favorite,
    );
    let surface = ContextMenuSurface::new(target, "album", position);
    append_play_actions(&surface);
    surface.append_configurable_submenu(
        ContextMenuItem::PlayRadio,
        msgid("Album radio"),
        &radio_context_submenu("album"),
        RADIO_ICON,
    );
    append_context_menu_picker(
        &surface,
        shell,
        PlaybackTarget::Album(album.media_uri.clone()),
    );
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
        library::source_entity_parts(&album.media_uri).map(|(source_id, _, _)| SidebarPin::Album {
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
    install_loaded_actions(&surface, shell, playback, play);
    install_radio_actions(&surface, shell, RadioSeed::Album(album.album_key));
    add_favorite_action(
        &surface,
        shell,
        FavoriteTarget::Album(album.media_uri.clone()),
        favorite,
    );
    {
        let metadata_shell = Rc::clone(shell);
        let media_uri = album.media_uri.clone();
        surface.add_action("edit-metadata", move || {
            present_metadata_dialog(&metadata_shell, MetadataItemId::Album(media_uri.clone()));
        });
    }
    for (index, artist) in album.album_artists.iter().enumerate() {
        let action = if album.album_artists.len() == 1 {
            "go-artist".to_string()
        } else {
            format!("go-artist-{index}")
        };
        let shell = Rc::clone(shell);
        let key = artist.media_uri.clone();
        surface.add_action(&action, move || {
            shell.navigate(Route::AlbumArtistDetail(key.clone()))
        });
    }
    let shell_album = Rc::clone(shell);
    let album_uri = album.media_uri.clone();
    surface.add_action("go-album", move || {
        shell_album.navigate(Route::AlbumDetail(album_uri.clone()));
    });
    if context_menu_rating_visible(shell) {
        let shell = Rc::clone(shell);
        let media_uri = album.media_uri.clone();
        surface.append_fixed_widget(
            "rating",
            &context_rating_row(
                album.rating.and_then(|value| u8::try_from(value).ok()),
                shell.half_stars_enabled(&album.media_uri, None),
                surface.popover(),
                move |rating| shell.set_rating(FavoriteTarget::Album(media_uri.clone()), rating),
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
    let favorite = shell.projected_item_favorite(
        &FavoriteTarget::Artist(artist.media_uri.clone()),
        artist.favorite,
    );
    let surface = ContextMenuSurface::new(target, "artist", position);
    append_play_actions(&surface);
    surface.append_configurable_submenu(
        ContextMenuItem::PlayRadio,
        msgid("Artist radio"),
        &radio_context_submenu("artist"),
        RADIO_ICON,
    );
    append_context_menu_picker(
        &surface,
        shell,
        if album_artist {
            PlaybackTarget::AlbumArtist(artist.media_uri.clone())
        } else {
            PlaybackTarget::Artist(artist.media_uri.clone())
        },
    );
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
        library::source_entity_parts(&artist.media_uri).map(|(source_id, _, _)| {
            SidebarPin::Artist {
                source_id,
                artist_id: artist.object_id.clone(),
                album_artist,
            }
        }),
    );
    surface.append_configurable_submenu(
        ContextMenuItem::GoTo,
        msgid("Go to"),
        &go_to_context_submenu("artist", std::slice::from_ref(&artist.name), false),
        GO_TO_ICON,
    );
    let playback = if album_artist {
        PlaybackTarget::AlbumArtist(artist.media_uri.clone())
    } else {
        PlaybackTarget::Artist(artist.media_uri.clone())
    };
    install_download_actions(
        &surface,
        shell,
        &playback,
        artist.track_count > 0 && artist.downloaded_count == artist.track_count,
    );
    install_loaded_actions(&surface, shell, playback, play);
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
        FavoriteTarget::Artist(artist.media_uri.clone()),
        favorite,
    );
    {
        let metadata_shell = Rc::clone(shell);
        let media_uri = artist.media_uri.clone();
        surface.add_action("edit-metadata", move || {
            present_metadata_dialog(&metadata_shell, MetadataItemId::Artist(media_uri.clone()));
        });
    }
    let shell_artist = Rc::clone(shell);
    let artist_uri = artist.media_uri.clone();
    surface.add_action("go-artist", move || {
        shell_artist.navigate(if album_artist {
            Route::AlbumArtistDetail(artist_uri.clone())
        } else {
            Route::ArtistDetail(artist_uri.clone())
        });
    });
    if context_menu_rating_visible(shell) {
        let shell = Rc::clone(shell);
        let media_uri = artist.media_uri.clone();
        surface.append_fixed_widget(
            "rating",
            &context_rating_row(
                artist.rating.and_then(|value| u8::try_from(value).ok()),
                shell.half_stars_enabled(&artist.media_uri, None),
                surface.popover(),
                move |rating| shell.set_rating(FavoriteTarget::Artist(media_uri.clone()), rating),
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
    install_loaded_actions(&surface, shell, playback, play);
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
    install_loaded_actions(&surface, shell, playback.clone(), play);
    append_context_menu_picker(&surface, shell, playback);
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
        if playlist.source_key.is_none() {
            Some(SidebarPin::Playlist {
                source_id: None,
                playlist_id: playlist.object_id.clone(),
            })
        } else {
            sidebar_pin_source(shell).map(|source_id| SidebarPin::Playlist {
                source_id: Some(source_id),
                playlist_id: playlist.object_id.clone(),
            })
        },
    );
    surface.append_fixed_action(
        msgid("Export playlist"),
        "export",
        "rufin-document-send-symbolic",
    );
    let export_shell = Rc::clone(shell);
    let export_key = playlist.playlist_key;
    let export_name = playlist.name.clone();
    surface.add_action("export", move || {
        export_shell.export_playlist_dialog(
            crate::runtime::source::PlaylistExport::Playlist(export_key),
            &export_name,
        )
    });
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
    install_loaded_actions(&surface, shell, playback, play);
    install_radio_actions(&surface, shell, RadioSeed::Playlist(playlist.playlist_key));
    let rename_shell = Rc::clone(shell);
    let playlist_key = playlist.playlist_key;
    let playlist_name = playlist.name.clone();
    surface.add_action("rename", move || {
        rename_shell.rename_playlist_dialog(playlist_key, playlist_name.clone());
    });
    let current =
        current_playback_track(shell.selected_playback().as_deref()).map(|track| track.media_uri);
    let add_shell = Rc::clone(shell);
    surface.add_action_enabled("add-current", current.is_some(), move || {
        if let Some(media_uri) = current.as_ref() {
            add_shell.add_media_to_playlist(
                playlist_key,
                vec![media_uri.clone()],
                false,
                Rc::new(|_| {}),
            );
        }
    });
    let delete_shell = Rc::clone(shell);
    surface.add_action("delete", move || {
        delete_shell.products.source.delete_playlist(playlist_key);
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
        Some(SidebarPin::SmartPlaylist {
            playlist_id: playlist.object_id.clone(),
        }),
    );
    surface.append_fixed_action(
        msgid("Export playlist"),
        "export",
        "rufin-document-send-symbolic",
    );
    let export_shell = Rc::clone(shell);
    let export_key = playlist.smart_playlist_key;
    let export_name = playlist.name.clone();
    surface.add_action("export", move || {
        export_shell.export_playlist_dialog(
            crate::runtime::source::PlaylistExport::Smart(export_key),
            &export_name,
        )
    });
    surface.append_fixed_action(msgid("Edit"), "edit-definition", EDIT_ICON);
    surface.append_fixed_action(msgid("Delete"), "delete", DELETE_ICON);
    let playback = PlaybackTarget::SmartPlaylist(playlist.smart_playlist_key);
    install_download_actions(
        &surface,
        shell,
        &playback,
        playlist.track_count > 0 && playlist.downloaded_count == playlist.track_count,
    );
    install_loaded_actions(&surface, shell, playback, play);
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
        shell.set_favorite_with_feedback(target.clone(), !favorite, None);
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
        .map(|selected| selected.source_id.clone())
}

fn sidebar_pin_action_available(settings: &crate::Settings) -> bool {
    settings.sidebar.pins_visible
}

fn install_loaded_actions(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    target: PlaybackTarget,
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
                play(placement);
            } else {
                target.play(&shell, placement);
            }
        });
    }
}

fn install_live_track_playback_actions(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    track: library::QueueItem,
) {
    for (action, placement) in [
        ("play", QueuePlacement::Now),
        ("play-next", QueuePlacement::Next),
        ("play-last", QueuePlacement::Last),
    ] {
        let shell = Rc::clone(shell);
        let track = track.clone();
        surface.add_action(action, move || {
            shell
                .products
                .playback
                .queue
                .play(playback::PlayRequest::one(track.clone(), placement));
        });
    }
}

fn install_media_uri_playback_actions(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    media_uri: String,
) {
    for (action, placement) in [
        ("play", QueuePlacement::Now),
        ("play-next", QueuePlacement::Next),
        ("play-last", QueuePlacement::Last),
    ] {
        let database = Arc::clone(&shell.products.library);
        let runtime = shell.products.runtime.clone();
        let queue = shell.products.playback.queue.clone();
        let media_uri = media_uri.clone();
        surface.add_action(action, move || {
            let database = Arc::clone(&database);
            let queue = queue.clone();
            let media_uri = media_uri.clone();
            runtime.spawn(async move {
                let media = database
                    .queue_items_for_uris(&[media_uri], &library::ReadCancellation::new())
                    .await
                    .ok()
                    .and_then(|mut media| media.pop());
                if let Some(media) = media {
                    queue.play(playback::PlayRequest::one(media, placement));
                }
            });
        });
    }
}

pub(crate) fn install_download_actions(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    target: &PlaybackTarget,
    downloaded: bool,
) {
    let mut addressed = target;
    while let PlaybackTarget::Contextual { target, .. } = addressed {
        addressed = target;
    }
    let available = match addressed {
        PlaybackTarget::Track(uri)
        | PlaybackTarget::Album(uri)
        | PlaybackTarget::Artist(uri)
        | PlaybackTarget::AlbumArtist(uri) => {
            if let Some((source_id, _, _)) = library::source_entity_parts(uri) {
                shell
                    .source
                    .configured
                    .borrow()
                    .sources
                    .iter()
                    .any(|source| source.id == source_id && source.kind != "local")
            } else {
                library::normalize_direct_media_uri(uri)
                    .is_some_and(|uri| uri.starts_with("https:") || uri.starts_with("http:"))
            }
        }
        _ => true,
    };
    if !downloaded && !available {
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
        ("play-radio", RadioPlayRequest::now(seed.clone())),
        ("play-radio-next", RadioPlayRequest::next(seed.clone())),
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
