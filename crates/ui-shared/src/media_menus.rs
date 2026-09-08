use std::{rc::Rc, sync::Arc};

use adw::prelude::*;
use downloads::DownloadSubject;
use gtk::glib;
use library::{
    AlbumRow, ArtistRow, FavoriteTarget, GenreRow, MoodRow, PlaylistRow, RadioSeed,
    SmartPlaylistRow, TrackRow,
};
use playback::{QueuePlacement, RadioPlayRequest};

use crate::controls::{
    ADD_ICON, DELETE_ICON, EDIT_ICON, PLAY_ICON, PLAY_LATER_ICON, PLAY_NEXT_ICON, REMOVE_ICON,
    TRASH_ICON,
};
use crate::downloads::{OperationFeedback, OperationFeedbackKind};
use crate::favorites::{FAVORITE_ADD_ICON, FAVORITE_REMOVE_ICON};
use crate::interactions::{
    ContextMenuSurface, DOWNLOAD_ICON, GO_TO_ICON, RADIO_ICON, go_to_context_submenu,
    radio_context_submenu,
};
use crate::metadata::MetadataItemId;
use crate::ratings::context_rating_row;
use crate::smart_playlist::SmartPlaylistChange;
use localization::msgid;
use rufin_core::settings::ContextMenuItem;
use rufin_core::settings::SidebarPin;

use rufin_core::playback::PlaybackTarget;
pub type CollectionPlay = Rc<dyn Fn(QueuePlacement)>;

use crate::route::Route;
use crate::selection::{PlaylistEntrySelectionSnapshot, TrackSelectionSnapshot};

pub struct MediaMenus {
    pub library: Arc<library::Database>,
    pub runtime: tokio::runtime::Handle,
    pub source: rufin_core::runtime::SourceHandle,
    pub queue: playback::QueueHandle,
    pub radio: playback::RadioHandle,
    pub downloads: downloads::Downloads,
    pub settings: Rc<crate::settings::SettingsState>,
    pub navigate: Rc<dyn Fn(Route)>,
    pub projected_item_favorite: Rc<dyn Fn(&FavoriteTarget, bool) -> bool>,
    pub half_stars_enabled: Rc<dyn Fn(&str, Option<&str>) -> bool>,
    pub set_favorite: Rc<dyn Fn(FavoriteTarget, bool)>,
    pub edit_metadata: Rc<dyn Fn(MetadataItemId)>,
    pub set_sidebar_pin: Rc<dyn Fn(SidebarPin, bool)>,
    pub sidebar_pin_source: Rc<dyn Fn() -> Option<sources::SourceId>>,
    pub source_download_available: Rc<dyn Fn(&sources::SourceId) -> bool>,
    pub export_playlist_dialog: Rc<dyn Fn(rufin_core::runtime::source::PlaylistExport, &str)>,
    pub rename_playlist_dialog: Rc<dyn Fn(library::PlaylistKey, String)>,
    pub edit_smart_playlist_dialog: Rc<dyn Fn(SmartPlaylistRow)>,
    pub publish_smart_playlist_change:
        Rc<dyn Fn(SmartPlaylistChange, Option<Rc<dyn Fn(Result<(), String>)>>)>,
    pub operation_feedback: Rc<dyn Fn(&OperationFeedback, Option<Box<dyn FnOnce()>>)>,
    pub picker_target: Rc<dyn Fn(&ContextMenuSurface, PlaybackTarget)>,
    pub picker_payload: Rc<dyn Fn(&ContextMenuSurface, crate::media_drag::MediaDragSource)>,
    pub play_target: Rc<dyn Fn(&PlaybackTarget, QueuePlacement)>,
    pub download: Rc<dyn Fn(&PlaybackTarget)>,
    pub remove_download: Rc<dyn Fn(&PlaybackTarget)>,
}
fn present_catalog_track_menu(
    target: &gtk::Widget,
    menus: &Rc<MediaMenus>,
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
        menus,
        PlaybackTarget::Track(track.media_uri.clone()),
    );
    let favorite = menus.projected_track_favorite(&track.media_uri, track.favorite);
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
    install_download_actions(&surface, menus, &playback, track.is_downloaded);
    if let Some(media) = playback_media {
        install_live_track_playback_actions(&surface, menus, media);
    } else {
        install_media_uri_playback_actions(&surface, menus, track.media_uri.clone());
    }
    install_radio_actions(&surface, menus, RadioSeed::Track(track.media_uri.clone()));
    add_favorite_action(
        &surface,
        menus,
        FavoriteTarget::Track(track.media_uri.clone()),
        favorite,
    );
    if track.cue_path.is_none() {
        let metadata_menus = Rc::clone(menus);
        let media_uri = track.media_uri.clone();
        surface.add_action("edit-metadata", move || {
            (metadata_menus.edit_metadata)(MetadataItemId::Track(media_uri.clone()));
        });
    }
    let album_artist = track.artists.is_empty();
    for (index, artist) in artists.iter().enumerate() {
        let action = if artists.len() == 1 {
            "go-artist".to_string()
        } else {
            format!("go-artist-{index}")
        };
        let menus = Rc::clone(menus);
        let key = artist.media_uri.clone();
        surface.add_action(&action, move || {
            (menus.navigate)(if album_artist {
                Route::AlbumArtistDetail(key.clone())
            } else {
                Route::ArtistDetail(key.clone())
            })
        });
    }
    if context_menu_rating_visible(menus) {
        let menus = Rc::clone(menus);
        let media_uri = track.media_uri.clone();
        surface.append_fixed_widget(
            "rating",
            &context_rating_row(
                track.rating.and_then(|value| u8::try_from(value).ok()),
                (menus.half_stars_enabled)(&track.media_uri, Some(&track.source_id)),
                surface.popover(),
                move |rating| {
                    menus
                        .source
                        .set_rating(FavoriteTarget::Track(media_uri.clone()), rating)
                },
            ),
        );
    }
    if let Some(album) = track.album_media_uri.clone() {
        let menus = Rc::clone(menus);
        surface.add_action("go-album", move || {
            (menus.navigate)(Route::AlbumDetail(album.clone()))
        });
    }
    if let Some(position) = popover_position {
        surface.popover().set_position(position);
    }
    if let Some(occurrence) = queue_occurrence {
        let queue = menus.queue.clone();
        let remove = occurrence.clone();
        surface.add_action("remove-from-queue", move || queue.remove(remove.clone()));
        let queue = menus.queue.clone();
        let activate = occurrence.clone();
        surface.add_action("play", move || queue.activate(activate.clone()));
        let queue = menus.queue.clone();
        let next = occurrence.clone();
        surface.add_action("play-next", move || queue.move_after_current(next.clone()));
        let queue = menus.queue.clone();
        surface.add_action("play-last", move || {
            queue.reorder(playback::QueueReorderRequest {
                occurrences: vec![occurrence.clone()],
                target: playback::QueueReorderTarget::End,
            });
        });
    }
    surface.popup(&menus.settings.current.borrow().context_menu);
}

pub fn present_playback_media_menu(
    target: &gtk::Widget,
    menus: &Rc<MediaMenus>,
    media_uri: String,
    media: Option<library::QueueItem>,
    position: Option<(f64, f64)>,
    popover_position: Option<gtk::PositionType>,
    queue_occurrence: Option<playback::OccurrenceId>,
) {
    let database = Arc::clone(&menus.library);
    let runtime = menus.runtime.clone();
    let occurrence = queue_occurrence.clone();
    let task = runtime.spawn(async move {
        let cancellation = library::ReadCancellation::new();
        let media = match (media, occurrence) {
            (None, Some(occurrence)) => database.queue_item_for_occurrence(&occurrence).await?,
            (media, _) => media,
        };
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
    let menus = Rc::downgrade(menus);
    glib::spawn_future_local(async move {
        let Ok(Ok((track, media, user_state, downloaded))) = task.await else {
            return;
        };
        let Some(menus) = menus.upgrade() else {
            return;
        };
        if let Some(track) = track {
            present_catalog_track_menu(
                &target,
                &menus,
                track,
                position,
                popover_position,
                media,
                queue_occurrence,
            );
        } else if let Some(media) = media {
            present_direct_playback_media_menu(
                &target,
                &menus,
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
    menus: &Rc<MediaMenus>,
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
    append_context_menu_picker_media(&surface, menus, media.media_uri.clone());
    install_download_actions(
        &surface,
        menus,
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
        let metadata_menus = Rc::clone(menus);
        let media_uri = media.media_uri.clone();
        surface.add_action("edit-metadata", move || {
            (metadata_menus.edit_metadata)(MetadataItemId::Track(media_uri.clone()))
        });
    }
    let (favorite, rating) = user_state
        .map(|(favorite, rating)| (favorite.unwrap_or(false), rating))
        .unwrap_or((false, None));
    let favorite = menus.projected_track_favorite(&media.media_uri, favorite);
    append_favorite_action(&surface, favorite);
    add_favorite_action(
        &surface,
        menus,
        FavoriteTarget::Track(media.media_uri.clone()),
        favorite,
    );
    if context_menu_rating_visible(menus) {
        let rating_menus = Rc::clone(menus);
        let media_uri = media.media_uri.clone();
        surface.append_fixed_widget(
            "rating",
            &context_rating_row(
                rating,
                (rating_menus.half_stars_enabled)(&media.media_uri, None),
                surface.popover(),
                move |rating| {
                    rating_menus
                        .source
                        .set_rating(FavoriteTarget::Track(media_uri.clone()), rating)
                },
            ),
        );
    }
    if let Some(position) = popover_position {
        surface.popover().set_position(position);
    }
    install_live_track_playback_actions(&surface, menus, media);
    if let Some(occurrence) = queue_occurrence {
        let queue = menus.queue.clone();
        let remove = occurrence.clone();
        surface.add_action("remove-from-queue", move || queue.remove(remove.clone()));
        let queue = menus.queue.clone();
        let activate = occurrence.clone();
        surface.add_action("play", move || queue.activate(activate.clone()));
        let queue = menus.queue.clone();
        let next = occurrence.clone();
        surface.add_action("play-next", move || queue.move_after_current(next.clone()));
        let queue = menus.queue.clone();
        surface.add_action("play-last", move || {
            queue.reorder(playback::QueueReorderRequest {
                occurrences: vec![occurrence.clone()],
                target: playback::QueueReorderTarget::End,
            });
        });
    }
    surface.popup(&menus.settings.current.borrow().context_menu);
}

pub fn present_playlist_entry_menu(
    target: &gtk::Widget,
    menus: &Rc<MediaMenus>,
    row: library::PlaylistEntryRow,
    position: Option<(f64, f64)>,
    _playlist: library::PlaylistKey,
    removal: Option<PlaylistEntrySelectionSnapshot>,
) {
    let surface = ContextMenuSurface::new(target, "playlist-entry", position);
    append_play_actions(&surface);
    if removal.as_ref().is_some_and(|selection| selection.writable) {
        surface.append_fixed_action(
            msgid("Remove from Playlist"),
            "remove-from-playlist",
            REMOVE_ICON,
        );
    }
    append_context_menu_picker_media(&surface, menus, row.media_uri.clone());
    install_download_actions(
        &surface,
        menus,
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
        let metadata_menus = Rc::clone(menus);
        let media_uri = row.media_uri.clone();
        surface.add_action("edit-metadata", move || {
            (metadata_menus.edit_metadata)(MetadataItemId::Track(media_uri.clone()))
        });
    }
    let favorite = menus.projected_track_favorite(&row.media_uri, row.favorite);
    append_favorite_action(&surface, favorite);
    add_favorite_action(
        &surface,
        menus,
        FavoriteTarget::Track(row.media_uri.clone()),
        favorite,
    );
    if context_menu_rating_visible(menus) {
        let rating_menus = Rc::clone(menus);
        let media_uri = row.media_uri.clone();
        surface.append_fixed_widget(
            "rating",
            &context_rating_row(
                row.rating.and_then(|value| u8::try_from(value).ok()),
                (rating_menus.half_stars_enabled)(&row.media_uri, row.source_id.as_deref()),
                surface.popover(),
                move |rating| {
                    rating_menus
                        .source
                        .set_rating(FavoriteTarget::Track(media_uri.clone()), rating)
                },
            ),
        );
    }
    install_media_uri_playback_actions(&surface, menus, row.media_uri.clone());
    let remove_menus = Rc::downgrade(menus);
    surface.add_action("remove-from-playlist", move || {
        if let (Some(menus), Some(removal)) = (remove_menus.upgrade(), removal.as_ref()) {
            remove_playlist_entry_selection(&menus, removal.clone());
        }
    });
    surface.popup(&menus.settings.current.borrow().context_menu);
}

pub fn present_track_selection_menu(
    target: &gtk::Widget,
    menus: &Rc<MediaMenus>,
    selection: TrackSelectionSnapshot,
    position: Option<(f64, f64)>,
) {
    let surface = ContextMenuSurface::new(target, "track-selection", position);
    append_track_selection_actions(&surface, menus, selection);
    surface.popup(&menus.settings.current.borrow().context_menu);
}

pub fn present_playlist_entry_selection_menu(
    target: &gtk::Widget,
    menus: &Rc<MediaMenus>,
    selection: PlaylistEntrySelectionSnapshot,
    position: Option<(f64, f64)>,
) {
    let surface = ContextMenuSurface::new(target, "playlist-entry-selection", position);
    if selection.writable {
        surface.append_fixed_action(
            msgid("Remove from Playlist"),
            "remove-from-playlist",
            REMOVE_ICON,
        );
    }
    append_playlist_entry_selection_actions(&surface, menus, selection.clone());
    let remove_menus = Rc::clone(menus);
    surface.add_action("remove-from-playlist", move || {
        remove_playlist_entry_selection(&remove_menus, selection.clone());
    });
    surface.popup(&menus.settings.current.borrow().context_menu);
}

pub fn remove_playlist_entry_selection(
    menus: &Rc<MediaMenus>,
    selection: PlaylistEntrySelectionSnapshot,
) -> bool {
    if !selection.writable || selection.entries.is_empty() {
        return false;
    }
    let item_count = selection.entries.len();
    let task_selection = selection.clone();
    let database = menus.library.clone();
    let task = menus
        .runtime
        .spawn(async move { task_selection.media_uris(&database).await });
    let menus = Rc::downgrade(menus);
    gtk::glib::spawn_future_local(async move {
        let Ok(Ok(media_uris)) = task.await else {
            return;
        };
        let Some(menus) = menus.upgrade() else { return };
        if media_uris.len() != item_count {
            return;
        }
        rufin_core::playlists::remove_playlist_entries(
            &menus.source,
            selection.playlist,
            selection.entries.to_vec(),
        );
        let feedback = OperationFeedback {
            subject: DownloadSubject::for_media_uris("playlist", Some("Playlist"), &media_uris),
            preview_uris: media_uris.iter().take(4).cloned().collect(),
            item_count,
            kind: OperationFeedbackKind::PlaylistRemoved {
                destination: selection.playlist_name.to_string(),
            },
        };
        let menus_for_undo = Rc::downgrade(&menus);
        let playlist = selection.playlist;
        (menus.operation_feedback)(
            &feedback,
            Some(Box::new(move || {
                let Some(menus) = menus_for_undo.upgrade() else {
                    return;
                };
                crate::playlists::add_media_to_playlist(
                    &menus.source,
                    playlist,
                    media_uris.clone(),
                    false,
                    Rc::new(|_| {}),
                );
            })),
        );
    });
    true
}

fn append_playlist_entry_selection_actions(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
    selection: PlaylistEntrySelectionSnapshot,
) {
    if selection.entries.is_empty() {
        return;
    }
    append_play_actions(surface);
    append_context_menu_picker_entries(surface, menus, selection.clone());
    let download_menus = Rc::clone(menus);
    let download_selection = selection.clone();
    surface.append_configurable_action(
        ContextMenuItem::Download,
        msgid("Download"),
        "download",
        DOWNLOAD_ICON,
    );
    surface.add_action("download", move || {
        download_playlist_entry_selection(&download_menus, download_selection.clone());
    });
    for (action, placement) in [
        ("play", QueuePlacement::Now),
        ("play-next", QueuePlacement::Next),
        ("play-last", QueuePlacement::Last),
    ] {
        let menus = Rc::clone(menus);
        let selection = selection.clone();
        surface.add_action(action, move || selection.play(&menus.queue, placement));
    }
}

pub fn download_playlist_entry_selection(
    menus: &Rc<MediaMenus>,
    selection: PlaylistEntrySelectionSnapshot,
) -> bool {
    let source = menus.source.clone();
    let database = menus.library.clone();
    menus.runtime.spawn(async move {
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
    menus: &Rc<MediaMenus>,
    selection: TrackSelectionSnapshot,
) {
    if selection.media_uris.is_empty() {
        return;
    }
    append_play_actions(&surface);
    append_context_menu_picker_selection(&surface, menus, selection.clone());
    install_track_selection_download_actions(&surface, menus, selection.clone());
    for (action, placement) in [
        ("play", QueuePlacement::Now),
        ("play-next", QueuePlacement::Next),
        ("play-last", QueuePlacement::Last),
    ] {
        let menus = Rc::clone(menus);
        let selection = selection.clone();
        surface.add_action(action, move || selection.play(&menus.queue, placement));
    }
}

pub fn install_track_selection_download_actions(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
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
    let download_menus = Rc::clone(menus);
    let download_selection = selection.clone();
    surface.add_action("download", move || {
        download_track_selection(&download_menus, download_selection.clone());
    });
    let remove_menus = Rc::clone(menus);
    surface.add_action("remove-downloads", move || {
        let downloads = remove_menus.downloads.clone();
        downloads.remove(selection.media_uris.to_vec(), true);
    });
}

pub fn download_track_selection(menus: &Rc<MediaMenus>, selection: TrackSelectionSnapshot) -> bool {
    let source = menus.source.clone();
    let subject = selection.download_subject();
    let media_uris = selection.media_uris.to_vec();
    source.download_media(subject, media_uris);
    true
}

pub fn present_album_context_menu(
    target: &gtk::Widget,
    menus: &Rc<MediaMenus>,
    album: AlbumRow,
    playback_context: Option<String>,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
) {
    let base = PlaybackTarget::Album(album.media_uri.clone());
    let playback = playback_context
        .map(|context| base.clone().in_context(context))
        .unwrap_or(base);
    let favorite = (menus.projected_item_favorite)(
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
        menus,
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
        menus,
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
        menus,
        &playback,
        album.track_count > 0 && album.downloaded_count == album.track_count,
    );
    install_loaded_actions(&surface, menus, playback, play);
    install_radio_actions(&surface, menus, RadioSeed::Album(album.album_key));
    add_favorite_action(
        &surface,
        menus,
        FavoriteTarget::Album(album.media_uri.clone()),
        favorite,
    );
    {
        let metadata_menus = Rc::clone(menus);
        let media_uri = album.media_uri.clone();
        surface.add_action("edit-metadata", move || {
            (metadata_menus.edit_metadata)(MetadataItemId::Album(media_uri.clone()));
        });
    }
    for (index, artist) in album.album_artists.iter().enumerate() {
        let action = if album.album_artists.len() == 1 {
            "go-artist".to_string()
        } else {
            format!("go-artist-{index}")
        };
        let menus = Rc::clone(menus);
        let key = artist.media_uri.clone();
        surface.add_action(&action, move || {
            (menus.navigate)(Route::AlbumArtistDetail(key.clone()))
        });
    }
    let menus_album = Rc::clone(menus);
    let album_uri = album.media_uri.clone();
    surface.add_action("go-album", move || {
        (menus_album.navigate)(Route::AlbumDetail(album_uri.clone()));
    });
    if context_menu_rating_visible(menus) {
        let menus = Rc::clone(menus);
        let media_uri = album.media_uri.clone();
        surface.append_fixed_widget(
            "rating",
            &context_rating_row(
                album.rating.and_then(|value| u8::try_from(value).ok()),
                (menus.half_stars_enabled)(&album.media_uri, None),
                surface.popover(),
                move |rating| {
                    menus
                        .source
                        .set_rating(FavoriteTarget::Album(media_uri.clone()), rating)
                },
            ),
        );
    }
    surface.popup(&menus.settings.current.borrow().context_menu);
}

pub fn present_artist_context_menu(
    target: &gtk::Widget,
    menus: &Rc<MediaMenus>,
    artist: ArtistRow,
    album_artist: bool,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
) {
    let favorite = (menus.projected_item_favorite)(
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
        menus,
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
        menus,
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
        menus,
        &playback,
        artist.track_count > 0 && artist.downloaded_count == artist.track_count,
    );
    install_loaded_actions(&surface, menus, playback, play);
    install_radio_actions(
        &surface,
        menus,
        if album_artist {
            RadioSeed::AlbumArtist(artist.artist_key)
        } else {
            RadioSeed::Artist(artist.artist_key)
        },
    );
    add_favorite_action(
        &surface,
        menus,
        FavoriteTarget::Artist(artist.media_uri.clone()),
        favorite,
    );
    {
        let metadata_menus = Rc::clone(menus);
        let media_uri = artist.media_uri.clone();
        surface.add_action("edit-metadata", move || {
            (metadata_menus.edit_metadata)(MetadataItemId::Artist(media_uri.clone()));
        });
    }
    let menus_artist = Rc::clone(menus);
    let artist_uri = artist.media_uri.clone();
    surface.add_action("go-artist", move || {
        (menus_artist.navigate)(if album_artist {
            Route::AlbumArtistDetail(artist_uri.clone())
        } else {
            Route::ArtistDetail(artist_uri.clone())
        });
    });
    if context_menu_rating_visible(menus) {
        let menus = Rc::clone(menus);
        let media_uri = artist.media_uri.clone();
        surface.append_fixed_widget(
            "rating",
            &context_rating_row(
                artist.rating.and_then(|value| u8::try_from(value).ok()),
                (menus.half_stars_enabled)(&artist.media_uri, None),
                surface.popover(),
                move |rating| {
                    menus
                        .source
                        .set_rating(FavoriteTarget::Artist(media_uri.clone()), rating)
                },
            ),
        );
    }
    surface.popup(&menus.settings.current.borrow().context_menu);
}

pub fn present_genre_context_menu(
    target: &gtk::Widget,
    menus: &Rc<MediaMenus>,
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
        menus,
        sidebar_pin_source(menus).map(|source_id| SidebarPin::Genre {
            source_id,
            genre_id: genre.object_id.clone(),
        }),
    );
    let playback = PlaybackTarget::Genre(genre.genre_key);
    install_download_actions(
        &surface,
        menus,
        &playback,
        genre.track_count > 0 && genre.downloaded_count == genre.track_count,
    );
    install_loaded_actions(&surface, menus, playback, play);
    install_radio_actions(&surface, menus, RadioSeed::Genre(genre.genre_key));
    surface.popup(&menus.settings.current.borrow().context_menu);
}

pub fn present_mood_context_menu(
    target: &gtk::Widget,
    menus: &Rc<MediaMenus>,
    mood: MoodRow,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
) {
    let surface = ContextMenuSurface::new(target, "mood", position);
    append_play_actions(&surface);
    let playback = PlaybackTarget::Mood(mood.mood_key);
    install_download_actions(
        &surface,
        menus,
        &playback,
        mood.track_count > 0 && mood.downloaded_count == mood.track_count,
    );
    install_loaded_actions(&surface, menus, playback.clone(), play);
    append_context_menu_picker(&surface, menus, playback);
    surface.popup(&menus.settings.current.borrow().context_menu);
}

pub fn present_playlist_context_menu(
    target: &gtk::Widget,
    menus: &Rc<MediaMenus>,
    playlist: PlaylistRow,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
    current: Option<String>,
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
        menus,
        if playlist.source_key.is_none() {
            Some(SidebarPin::Playlist {
                source_id: None,
                playlist_id: playlist.object_id.clone(),
            })
        } else {
            sidebar_pin_source(menus).map(|source_id| SidebarPin::Playlist {
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
    let export_menus = Rc::clone(menus);
    let export_key = playlist.playlist_key;
    let export_name = playlist.name.clone();
    surface.add_action("export", move || {
        (export_menus.export_playlist_dialog)(
            rufin_core::runtime::source::PlaylistExport::Playlist(export_key),
            &export_name,
        )
    });
    if playlist.writable {
        surface.append_fixed_action(msgid("Rename"), "rename", EDIT_ICON);
        surface.append_fixed_action(msgid("Add current"), "add-current", ADD_ICON);
        surface.append_fixed_action(msgid("Delete"), "delete", DELETE_ICON);
    }
    let playback = PlaybackTarget::Playlist(playlist.playlist_key);
    install_download_actions(
        &surface,
        menus,
        &playback,
        playlist.track_count > 0 && playlist.downloaded_count == playlist.track_count,
    );
    install_loaded_actions(&surface, menus, playback, play);
    install_radio_actions(&surface, menus, RadioSeed::Playlist(playlist.playlist_key));
    let rename_menus = Rc::clone(menus);
    let playlist_key = playlist.playlist_key;
    let playlist_name = playlist.name.clone();
    surface.add_action("rename", move || {
        (rename_menus.rename_playlist_dialog)(playlist_key, playlist_name.clone());
    });
    let add_menus = Rc::clone(menus);
    surface.add_action_enabled("add-current", current.is_some(), move || {
        if let Some(media_uri) = current.as_ref() {
            crate::playlists::add_media_to_playlist(
                &add_menus.source,
                playlist_key,
                vec![media_uri.clone()],
                false,
                Rc::new(|_| {}),
            );
        }
    });
    let delete_menus = Rc::clone(menus);
    surface.add_action("delete", move || {
        rufin_core::playlists::delete_playlist(&delete_menus.source, playlist_key);
        (delete_menus.navigate)(Route::Playlists);
    });
    surface.popup(&menus.settings.current.borrow().context_menu);
}

pub fn present_smart_playlist_context_menu(
    target: &gtk::Widget,
    menus: &Rc<MediaMenus>,
    playlist: SmartPlaylistRow,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
) {
    let surface = ContextMenuSurface::new(target, "smart-playlist", position);
    append_play_actions(&surface);
    install_sidebar_pin_action(
        &surface,
        menus,
        Some(SidebarPin::SmartPlaylist {
            playlist_id: playlist.object_id.clone(),
        }),
    );
    surface.append_fixed_action(
        msgid("Export playlist"),
        "export",
        "rufin-document-send-symbolic",
    );
    let export_menus = Rc::clone(menus);
    let export_key = playlist.smart_playlist_key;
    let export_name = playlist.name.clone();
    surface.add_action("export", move || {
        (export_menus.export_playlist_dialog)(
            rufin_core::runtime::source::PlaylistExport::Smart(export_key),
            &export_name,
        )
    });
    surface.append_fixed_action(msgid("Edit"), "edit-definition", EDIT_ICON);
    surface.append_fixed_action(msgid("Delete"), "delete", DELETE_ICON);
    let playback = PlaybackTarget::SmartPlaylist(playlist.smart_playlist_key);
    install_download_actions(
        &surface,
        menus,
        &playback,
        playlist.track_count > 0 && playlist.downloaded_count == playlist.track_count,
    );
    install_loaded_actions(&surface, menus, playback, play);
    let edit_menus = Rc::clone(menus);
    let edit_row = playlist.clone();
    surface.add_action("edit-definition", move || {
        (edit_menus.edit_smart_playlist_dialog)(edit_row.clone());
    });
    let delete_menus = Rc::clone(menus);
    let key = playlist.smart_playlist_key;
    surface.add_action("delete", move || {
        let navigate = Rc::clone(&delete_menus);
        (delete_menus.publish_smart_playlist_change)(
            SmartPlaylistChange::Delete(key),
            Some(Rc::new(move |result| {
                if result.is_ok() {
                    (navigate.navigate)(Route::SmartPlaylists);
                }
            })),
        );
    });
    surface.popup(&menus.settings.current.borrow().context_menu);
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
    menus: &Rc<MediaMenus>,
    target: FavoriteTarget,
    favorite: bool,
) {
    let menus = Rc::clone(menus);
    surface.add_action("favorite", move || {
        (menus.set_favorite)(target.clone(), !favorite);
    });
}

fn install_sidebar_pin_action(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
    pin: Option<SidebarPin>,
) {
    let Some(pin) = pin else { return };
    let settings = menus.settings.current.borrow();
    if !settings.sidebar.pins_visible {
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
    let menus = Rc::clone(menus);
    surface.add_action("pin", move || (menus.set_sidebar_pin)(pin.clone(), !pinned));
}

fn sidebar_pin_source(menus: &MediaMenus) -> Option<sources::SourceId> {
    (menus.sidebar_pin_source)()
}

fn install_loaded_actions(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
    target: PlaybackTarget,
    play: Option<CollectionPlay>,
) {
    for (action, placement) in [
        ("play", QueuePlacement::Now),
        ("play-next", QueuePlacement::Next),
        ("play-last", QueuePlacement::Last),
    ] {
        let target = target.clone();
        let menus = Rc::clone(menus);
        let play = play.clone();
        surface.add_action(action, move || {
            if let Some(play) = &play {
                play(placement);
            } else {
                (menus.play_target)(&target, placement);
            }
        });
    }
}

fn install_live_track_playback_actions(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
    track: library::QueueItem,
) {
    for (action, placement) in [
        ("play", QueuePlacement::Now),
        ("play-next", QueuePlacement::Next),
        ("play-last", QueuePlacement::Last),
    ] {
        let menus = Rc::clone(menus);
        let track = track.clone();
        surface.add_action(action, move || {
            menus
                .queue
                .play(playback::PlayRequest::one(track.clone(), placement));
        });
    }
}

fn install_media_uri_playback_actions(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
    media_uri: String,
) {
    for (action, placement) in [
        ("play", QueuePlacement::Now),
        ("play-next", QueuePlacement::Next),
        ("play-last", QueuePlacement::Last),
    ] {
        let database = Arc::clone(&menus.library);
        let runtime = menus.runtime.clone();
        let queue = menus.queue.clone();
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

pub fn install_download_actions(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
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
                (menus.source_download_available)(&source_id)
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
        let remove_menus = Rc::clone(menus);
        let remove_target = target.clone();
        surface.add_action("remove-downloads", move || {
            (remove_menus.remove_download)(&remove_target)
        });
    } else {
        surface.append_configurable_action(
            ContextMenuItem::Download,
            msgid("Download"),
            "download",
            DOWNLOAD_ICON,
        );
        let download_menus = Rc::clone(menus);
        let download_target = target.clone();
        surface.add_action("download", move || {
            (download_menus.download)(&download_target)
        });
    }
}

fn install_radio_actions(surface: &ContextMenuSurface, menus: &MediaMenus, seed: RadioSeed) {
    for (action, request) in [
        ("play-radio", RadioPlayRequest::now(seed.clone())),
        ("play-radio-next", RadioPlayRequest::next(seed.clone())),
        ("play-radio-last", RadioPlayRequest::last(seed)),
    ] {
        let radio = menus.radio.clone();
        surface.add_action(action, move || radio.play_radio(request.clone()));
    }
}

fn context_menu_rating_visible(menus: &MediaMenus) -> bool {
    menus.settings.current.borrow().context_menu.rating_visible
}

fn append_context_menu_picker_entries(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
    selection: PlaylistEntrySelectionSnapshot,
) {
    (menus.picker_payload)(
        surface,
        crate::media_drag::MediaDragSource::playlist_entries(selection),
    );
}

fn append_context_menu_picker_selection(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
    selection: TrackSelectionSnapshot,
) {
    (menus.picker_payload)(
        surface,
        crate::media_drag::MediaDragSource::selection(selection),
    );
}

fn append_context_menu_picker_media(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
    media_uri: String,
) {
    (menus.picker_payload)(
        surface,
        crate::media_drag::MediaDragSource::media_uris([media_uri]),
    );
}

fn append_context_menu_picker(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
    target: PlaybackTarget,
) {
    (menus.picker_target)(surface, target);
}
impl MediaMenus {
    fn projected_track_favorite(&self, media_uri: &str, fallback: bool) -> bool {
        (self.projected_item_favorite)(&FavoriteTarget::Track(media_uri.to_string()), fallback)
    }
}
