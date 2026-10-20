use std::{rc::Rc, sync::Arc};

use adw::prelude::*;
use downloads::DownloadSubject;
use gtk::glib;
use library::{
    AlbumView, ArtistView, FavoriteTarget, GenreView, MoodView, PlaylistRow, RadioSeed,
    SmartPlaylistRow, TrackView,
};
use rufin_core::playback::{QueuePlacement, RadioPlayRequest};

use gtk_widgets::controls::{ADD_ICON, DELETE_ICON, EDIT_ICON, REMOVE_ICON, TRASH_ICON};
use gtk_widgets::downloads::{OperationArtwork, OperationFeedback, OperationFeedbackKind};
use gtk_widgets::favorites::{FAVORITE_ADD_ICON, FAVORITE_REMOVE_ICON};
use gtk_widgets::interactions::{
    ContextMenuSurface, DOWNLOAD_ICON, GO_TO_ICON, RADIO_ICON, go_to_context_submenu,
    radio_context_submenu,
};
use gtk_widgets::ratings::context_rating_row;
use localization::msgid;
use rufin_core::metadata::MetadataItemId;
use rufin_core::playlists::SmartPlaylistChange;
use rufin_core::settings::ContextMenuItem;
use rufin_core::settings::SidebarPin;

use gtk_widgets::selection::{collection_play_request, track_play_request};
use rufin_core::playback::PlayRequest;
const PLAY_ACTIONS: [(&str, QueuePlacement, bool); 6] = [
    ("play", QueuePlacement::Now, false),
    ("play-next", QueuePlacement::Next, false),
    ("play-last", QueuePlacement::Last, false),
    ("play-shuffled", QueuePlacement::Now, true),
    ("play-next-shuffled", QueuePlacement::Next, true),
    ("play-last-shuffled", QueuePlacement::Last, true),
];
pub type CollectionPlay = Rc<dyn Fn(QueuePlacement, bool)>;

use gtk_widgets::route::Route;
use gtk_widgets::selection::{PlaylistEntrySelectionSnapshot, TrackSelectionSnapshot};

pub fn add_drag_to_playlist(
    menus: &Rc<MediaMenus>,
    playlist: PlaylistRow,
    source: gtk_widgets::media_drag::MediaDragSource,
) -> bool {
    let database = Arc::clone(&menus.library);
    let task = menus
        .runtime
        .spawn(async move { source.playlist_input(&database).await });
    let menus = Rc::downgrade(menus);
    glib::spawn_future_local(async move {
        let Some((selection, title, preview_uris)) = task.await.ok().and_then(Result::ok) else {
            return;
        };
        let Some(owner) = menus.upgrade() else { return };
        gtk_widgets::playlists::add_media_to_playlist(
            &owner.source,
            playlist.playlist_key,
            selection,
            false,
            Rc::new(move |accepted| {
                if accepted > 0
                    && let Some(menus) = menus.upgrade()
                {
                    (menus.operation_feedback)(
                        &OperationFeedback {
                            title: title.clone(),
                            artwork: OperationArtwork::MediaUris(preview_uris.clone()),
                            item_count: accepted,
                            kind: OperationFeedbackKind::PlaylistAdded {
                                destination: playlist.name.clone(),
                            },
                        },
                        None,
                    );
                }
            }),
        );
    });
    true
}

pub struct MediaMenus {
    pub library: Arc<library::Database>,
    pub runtime: tokio::runtime::Handle,
    pub source: rufin_core::runtime::SourceHandle,
    pub playback: std::sync::Arc<rufin_core::playback::PlaybackOwner>,
    pub downloads: downloads::Downloads,
    pub settings: Rc<gtk_widgets::settings::SettingsState>,
    pub navigate: Rc<dyn Fn(Route)>,
    pub projected_item_favorite: Rc<dyn Fn(&FavoriteTarget, bool) -> bool>,
    pub half_stars_enabled: Rc<dyn Fn(&str, Option<&str>) -> bool>,
    pub set_favorite: Rc<dyn Fn(FavoriteTarget, bool)>,
    pub edit_metadata: Rc<dyn Fn(MetadataItemId)>,
    pub set_sidebar_pin: Rc<dyn Fn(SidebarPin, bool)>,
    pub source_download_available: Rc<dyn Fn(&sources::SourceId) -> bool>,
    pub export_playlist_dialog: Rc<dyn Fn(rufin_core::runtime::source::PlaylistExport, &str)>,
    pub edit_playlist_dialog: Rc<dyn Fn(library::PlaylistKey, String)>,
    pub edit_smart_playlist_dialog: Rc<dyn Fn(SmartPlaylistRow)>,
    pub publish_smart_playlist_change:
        Rc<dyn Fn(SmartPlaylistChange, Option<Rc<dyn Fn(Result<(), String>)>>)>,
    pub operation_feedback: Rc<dyn Fn(&OperationFeedback, Option<Box<dyn FnOnce()>>)>,
    pub show_error: Rc<dyn Fn(String)>,
    pub picker_target: Rc<dyn Fn(&ContextMenuSurface, PlayRequest)>,
    pub picker_payload: Rc<dyn Fn(&ContextMenuSurface, gtk_widgets::media_drag::MediaDragSource)>,
    pub play_target: Rc<dyn Fn(&PlayRequest, QueuePlacement, bool)>,
    pub download: Rc<dyn Fn(&gtk_widgets::media_drag::MediaDragSource)>,
    pub remove_download: Rc<dyn Fn(&gtk_widgets::media_drag::MediaDragSource)>,
}
pub fn present_track_menu(
    target: &gtk::Widget,
    menus: &Rc<MediaMenus>,
    track: TrackView,
    position: Option<(f64, f64)>,
    popover_position: Option<gtk::PositionType>,
    queue_entry: Option<(usize, Option<u64>)>,
) {
    if queue_entry.is_none()
        && present_collection_selection_menu(
            target,
            menus,
            track_play_request(track.track_key),
            position,
        )
    {
        return;
    }
    let action_group = if queue_entry.is_some() {
        "queue"
    } else {
        "track"
    };
    let surface = ContextMenuSurface::new(target, action_group, position);
    if queue_entry.is_some() {
        surface.append_fixed_action(msgid("Remove from Queue"), "remove-from-queue", REMOVE_ICON);
    }
    surface.append_play_actions(&menus.settings.current.borrow().application.context_menu);
    surface.append_configurable_submenu(
        ContextMenuItem::PlayRadio,
        msgid("Track radio"),
        &radio_context_submenu(action_group),
        RADIO_ICON,
    );
    let favorite = menus.projected_track_favorite(&track.media_uri, track.state.favorite);
    append_favorite_action(&surface, favorite);
    append_context_menu_picker(&surface, menus, track_play_request(track.track_key));
    if track.access.cue_path.is_none()
        && (track.source_key.is_some() || track.media_uri.starts_with("file:"))
    {
        surface.append_configurable_action(
            ContextMenuItem::EditMetadata,
            msgid("Edit metadata"),
            "edit-metadata",
            EDIT_ICON,
        );
    }
    install_track_navigation(
        &surface,
        menus,
        action_group,
        &track.links.artists,
        &track.links.album_artists,
        track.links.album_media_uri.as_deref(),
    );
    let playback = track_play_request(track.track_key);
    install_download_actions(
        &surface,
        menus,
        &gtk_widgets::media_drag::MediaDragSource::capture_target(playback),
        Some(&track.media_uri),
        true,
        track.access.is_downloaded,
    );
    install_loaded_actions(&surface, menus, track_play_request(track.track_key), None);
    install_radio_actions(&surface, menus, RadioSeed::Track(track.media_uri.clone()));
    add_favorite_action(
        &surface,
        menus,
        FavoriteTarget::Track(track.media_uri.clone()),
        favorite,
    );
    if track.access.cue_path.is_none()
        && (track.source_key.is_some() || track.media_uri.starts_with("file:"))
    {
        let metadata_menus = Rc::clone(menus);
        let media_uri = track.media_uri.clone();
        surface.add_action("edit-metadata", move || {
            (metadata_menus.edit_metadata)(MetadataItemId::Track(media_uri.clone()));
        });
    }
    if context_menu_rating_visible(menus) {
        let menus = Rc::clone(menus);
        let media_uri = track.media_uri.clone();
        surface.append_fixed_widget(
            "rating",
            &context_rating_row(
                track
                    .state
                    .rating
                    .and_then(|value| u8::try_from(value).ok()),
                (menus.half_stars_enabled)(&track.media_uri, track.source_id.as_deref()),
                surface.popover(),
                move |rating| {
                    menus
                        .source
                        .set_rating(FavoriteTarget::Track(media_uri.clone()), rating)
                },
            ),
        );
    }
    if let Some(position) = popover_position {
        surface.popover().set_position(position);
    }
    if let Some((position, occurrence_id)) = queue_entry {
        install_queue_entry_actions(&surface, menus, position, occurrence_id);
    }
    surface.popup(&menus.settings.current.borrow().application.context_menu);
}

fn install_queue_entry_actions(
    surface: &ContextMenuSurface,
    menus: &MediaMenus,
    position: usize,
    occurrence_id: Option<u64>,
) {
    let selection = match occurrence_id {
        Some(id) => rufin_core::playback::QueueSelection::Occurrences(vec![id]),
        None => rufin_core::playback::QueueSelection::Positions(vec![position]),
    };
    let remove = selection.clone();
    let queue = menus.playback.clone();
    surface.add_action("remove-from-queue", move || {
        queue.remove_many(remove.clone())
    });
    let queue = menus.playback.clone();
    surface.add_action("play", move || match occurrence_id {
        Some(id) => queue.activate_occurrence(id),
        None => queue.activate(position),
    });
    let queue = menus.playback.clone();
    surface.add_action("play-next", move || match occurrence_id {
        Some(id) => queue.move_occurrence_after_current(id),
        None => queue.move_after_current(position),
    });
    let queue = menus.playback.clone();
    surface.add_action("play-last", move || {
        queue.reorder(
            selection.clone(),
            rufin_core::playback::QueueReorderTarget::End,
        );
    });
}

fn install_track_navigation(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
    action_group: &str,
    track_artists: &[library::TrackArtistLink],
    album_artists: &[library::TrackArtistLink],
    album_media_uri: Option<&str>,
) {
    let artists = if track_artists.is_empty() {
        album_artists
    } else {
        track_artists
    };
    let artist_names = artists
        .iter()
        .map(|artist| artist.name.clone())
        .collect::<Vec<_>>();

    if !artist_names.is_empty() || album_media_uri.is_some() {
        surface.append_configurable_submenu(
            ContextMenuItem::GoTo,
            msgid("Go to"),
            &go_to_context_submenu(action_group, &artist_names, album_media_uri.is_some()),
            GO_TO_ICON,
        );
    }
    let album_artist = track_artists.is_empty();
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
    if let Some(album) = album_media_uri.map(str::to_owned) {
        let menus = Rc::clone(menus);
        surface.add_action("go-album", move || {
            (menus.navigate)(Route::AlbumDetail(album.clone()))
        });
    }
}

pub fn present_playlist_entry_menu(
    target: &gtk::Widget,
    menus: &Rc<MediaMenus>,
    row: library::PlaylistEntryView,
    position: Option<(f64, f64)>,
    _playlist: library::PlaylistKey,
    removal: Option<PlaylistEntrySelectionSnapshot>,
) {
    let surface = ContextMenuSurface::new(target, "playlist-entry", position);
    surface.append_play_actions(&menus.settings.current.borrow().application.context_menu);
    if removal.as_ref().is_some_and(|selection| selection.writable) {
        surface.append_fixed_action(
            msgid("Remove from Playlist"),
            "remove-from-playlist",
            REMOVE_ICON,
        );
    }
    install_track_navigation(
        &surface,
        menus,
        "playlist-entry",
        &row.track.links.artists,
        &row.track.links.album_artists,
        row.track.links.album_media_uri.as_deref(),
    );
    let favorite = menus.projected_track_favorite(&row.track.media_uri, row.track.state.favorite);
    append_favorite_action(&surface, favorite);
    install_download_actions(
        &surface,
        menus,
        &gtk_widgets::media_drag::MediaDragSource::capture_target(track_play_request(
            row.track.track_key,
        )),
        Some(&row.track.media_uri),
        true,
        row.track.access.is_downloaded,
    );
    if row.track.media_uri.starts_with("file:")
        || library::source_entity_parts(&row.track.media_uri).is_some()
    {
        surface.append_configurable_action(
            ContextMenuItem::EditMetadata,
            msgid("Edit metadata"),
            "edit-metadata",
            EDIT_ICON,
        );
        let metadata_menus = Rc::clone(menus);
        let media_uri = row.track.media_uri.clone();
        surface.add_action("edit-metadata", move || {
            (metadata_menus.edit_metadata)(MetadataItemId::Track(media_uri.clone()))
        });
    }
    append_context_menu_picker_media(&surface, menus, row.track.media_uri.clone());
    add_favorite_action(
        &surface,
        menus,
        FavoriteTarget::Track(row.track.media_uri.clone()),
        favorite,
    );
    if context_menu_rating_visible(menus) {
        let rating_menus = Rc::clone(menus);
        let media_uri = row.track.media_uri.clone();
        surface.append_fixed_widget(
            "rating",
            &context_rating_row(
                row.track
                    .state
                    .rating
                    .and_then(|value| u8::try_from(value).ok()),
                (rating_menus.half_stars_enabled)(
                    &row.track.media_uri,
                    row.track.source_id.as_deref(),
                ),
                surface.popover(),
                move |rating| {
                    rating_menus
                        .source
                        .set_rating(FavoriteTarget::Track(media_uri.clone()), rating)
                },
            ),
        );
    }
    install_media_uri_playback_actions(&surface, menus, row.track.media_uri.clone());
    let remove_menus = Rc::downgrade(menus);
    surface.add_action("remove-from-playlist", move || {
        if let (Some(menus), Some(removal)) = (remove_menus.upgrade(), removal.as_ref()) {
            remove_playlist_entry_selection(&menus, removal.clone());
        }
    });
    surface.popup(&menus.settings.current.borrow().application.context_menu);
}

pub fn present_track_selection_menu(
    target: &gtk::Widget,
    menus: &Rc<MediaMenus>,
    selection: TrackSelectionSnapshot,
    position: Option<(f64, f64)>,
) {
    let surface = ContextMenuSurface::new(target, "track-selection", position);
    append_track_selection_actions(&surface, menus, selection);
    surface.popup(&menus.settings.current.borrow().application.context_menu);
}

pub fn present_collection_selection_menu(
    target: &gtk::Widget,
    menus: &Rc<MediaMenus>,
    clicked: PlayRequest,
    position: Option<(f64, f64)>,
) -> bool {
    let Some((surface, _)) = collection_selection_surface(target, menus, clicked, position) else {
        return false;
    };
    surface.popup(&menus.settings.current.borrow().application.context_menu);
    true
}

fn collection_selection_surface(
    target: &gtk::Widget,
    menus: &Rc<MediaMenus>,
    clicked: PlayRequest,
    position: Option<(f64, f64)>,
) -> Option<(
    ContextMenuSurface,
    gtk_widgets::media_drag::CollectionTargets,
)> {
    let targets = gtk_widgets::media_drag::selected_collection_targets(target, &clicked)?;
    let payload = gtk_widgets::media_drag::MediaDragSource::Targets {
        targets: targets.clone(),
    };
    let surface = ContextMenuSurface::new(target, "collection-selection", position);
    surface.append_play_actions(&menus.settings.current.borrow().application.context_menu);
    (menus.picker_payload)(&surface, payload.clone());
    for (action, placement, shuffled) in PLAY_ACTIONS {
        let menus = Rc::clone(menus);
        let payload = payload.clone();
        surface.add_action(action, move || {
            let payload = payload.clone();
            let database = menus.library.clone();
            let task = menus
                .runtime
                .spawn(async move { payload.play_request(&database).await });
            let menus = Rc::clone(&menus);
            glib::spawn_future_local(async move {
                match task.await {
                    Ok(Ok(request)) => (menus.play_target)(&request, placement, shuffled),
                    Ok(Err(error)) => (menus.show_error)(error),
                    Err(error) => (menus.show_error)(error.to_string()),
                }
            });
        });
    }
    surface.append_configurable_action(
        ContextMenuItem::Download,
        msgid("Download"),
        "download",
        DOWNLOAD_ICON,
    );
    let download_menus = Rc::clone(menus);
    let download_payload = payload.clone();
    surface.add_action("download", move || {
        (download_menus.download)(&download_payload)
    });
    surface.append_configurable_action(
        ContextMenuItem::Download,
        msgid("Remove Downloads"),
        "remove-downloads",
        TRASH_ICON,
    );
    let remove_menus = Rc::clone(menus);
    surface.add_action("remove-downloads", move || {
        (remove_menus.remove_download)(&payload)
    });
    Some((surface, targets))
}

fn present_playlist_selection_menu(
    target: &gtk::Widget,
    menus: &Rc<MediaMenus>,
    clicked: PlayRequest,
    position: Option<(f64, f64)>,
) -> bool {
    let Some((surface, targets)) = collection_selection_surface(target, menus, clicked, position)
    else {
        return false;
    };
    surface.append_fixed_action(msgid("Delete"), "delete", DELETE_ICON);
    let delete_menus = Rc::clone(menus);
    let delete_target = target.downgrade();
    surface.add_action("delete", move || {
        let Some(target) = delete_target.upgrade() else {
            return;
        };
        let menus = Rc::clone(&delete_menus);
        let targets = targets.clone();
        let dialog = gtk_widgets::playlists::delete_playlists_dialog(targets.count(), move || {
            let targets = targets.clone();
            let source = menus.source.clone();
            let task = menus.runtime.spawn(async move {
                let targets = targets.resolve().await?;
                let mut smart = Vec::new();
                let mut failure = None;
                for target in targets {
                    let Some(library::TrackSelection::Query {
                        query:
                            library::CatalogQuery {
                                subject: library::CatalogSubject::Tracks { scope, .. },
                                ..
                            },
                        ..
                    }) = target.selection()
                    else {
                        unreachable!("playlist selection contains playlist queries");
                    };
                    match scope {
                        library::TrackScope::Playlist(key) => {
                            if let Err(error) =
                                rufin_core::playlists::delete_playlist(&source, key).await
                            {
                                failure.get_or_insert_with(|| error.to_string());
                            }
                        }
                        library::TrackScope::Smart { key, .. } => smart.push(key),
                        _ => unreachable!("playlist selection contains playlists"),
                    }
                }
                Ok::<_, String>((smart, failure))
            });
            let menus = Rc::clone(&menus);
            glib::spawn_future_local(async move {
                match task.await {
                    Ok(Ok((smart, failure))) => {
                        for key in smart {
                            (menus.publish_smart_playlist_change)(
                                SmartPlaylistChange::Delete(key),
                                None,
                            );
                        }
                        if let Some(error) = failure {
                            (menus.show_error)(error);
                        }
                    }
                    Ok(Err(error)) => (menus.show_error)(error),
                    Err(error) => (menus.show_error)(error.to_string()),
                }
            });
        });
        dialog.present(Some(&target));
    });
    surface.popup(&menus.settings.current.borrow().application.context_menu);
    true
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
    surface.popup(&menus.settings.current.borrow().application.context_menu);
}

pub fn remove_playlist_entry_selection(
    menus: &Rc<MediaMenus>,
    selection: PlaylistEntrySelectionSnapshot,
) -> bool {
    if !selection.writable || selection.count == 0 {
        return false;
    }
    let result = rufin_core::playlists::remove_playlist_entries(
        &menus.source,
        selection.playlist,
        selection.input,
    );
    let menus = Rc::downgrade(menus);
    gtk::glib::spawn_future_local(async move {
        let Ok(Ok(tracks)) = result.recv().await else {
            return;
        };
        if tracks.is_empty() {
            return;
        }
        let Some(menus) = menus.upgrade() else {
            return;
        };
        let item_count = tracks.len();
        let songs = library::CatalogQuery::keys(
            library::CatalogKind::Track,
            None,
            (tracks).iter().map(|key| key.raw()).collect(),
        )
        .selection();
        let database = menus.library.clone();
        let preview = songs.clone();
        let artwork = menus
            .runtime
            .spawn(async move {
                database
                    .selected_track_uris(&preview, Some(4), &library::ReadCancellation::new())
                    .await
            })
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
        let feedback = OperationFeedback {
            title: localization::tr("Playlist"),
            artwork: OperationArtwork::MediaUris(artwork),
            item_count,
            kind: OperationFeedbackKind::PlaylistRemoved {
                destination: selection.playlist_name.to_string(),
            },
        };
        let menus_for_undo = Rc::downgrade(&menus);
        (menus.operation_feedback)(
            &feedback,
            Some(Box::new(move || {
                if let Some(menus) = menus_for_undo.upgrade() {
                    gtk_widgets::playlists::add_media_to_playlist(
                        &menus.source,
                        selection.playlist,
                        songs.clone(),
                        false,
                        Rc::new(|_| {}),
                    );
                }
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
    if selection.count == 0 {
        return;
    }
    surface.append_play_actions(&menus.settings.current.borrow().application.context_menu);
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
    for (action, placement, shuffled) in PLAY_ACTIONS {
        let menus = Rc::clone(menus);
        let selection = selection.clone();
        surface.add_action(action, move || {
            selection.play(&menus.playback, placement, shuffled)
        });
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
    if selection.count == 0 {
        return;
    }
    surface.append_play_actions(&menus.settings.current.borrow().application.context_menu);
    append_context_menu_picker_selection(&surface, menus, selection.clone());
    install_track_selection_download_actions(&surface, menus, selection.clone());
    for (action, placement, shuffled) in PLAY_ACTIONS {
        let menus = Rc::clone(menus);
        let selection = selection.clone();
        surface.add_action(action, move || {
            selection.play(&menus.playback, placement, shuffled)
        });
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
        let database = remove_menus.library.clone();
        let selection = selection.clone();
        let task = remove_menus
            .runtime
            .spawn(async move { selection.media_uris(&database).await });
        glib::spawn_future_local(async move {
            if let Ok(Ok(uris)) = task.await {
                downloads.remove(uris, true);
            }
        });
    });
}

pub fn download_track_selection(menus: &Rc<MediaMenus>, selection: TrackSelectionSnapshot) -> bool {
    let source = menus.source.clone();
    let database = menus.library.clone();
    let task = menus
        .runtime
        .spawn(async move { selection.media_uris(&database).await });
    glib::spawn_future_local(async move {
        if let Ok(Ok(uris)) = task.await {
            let subject = DownloadSubject::for_media_uris("track-selection", None, &uris);
            source.download_media(subject, uris);
        }
    });
    true
}

pub fn present_album_context_menu(
    target: &gtk::Widget,
    menus: &Rc<MediaMenus>,
    album: AlbumView,
    playback_context: Option<String>,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
) {
    let base = gtk_widgets::selection::album_play_request(&album);
    let playback = playback_context
        .map(|context| base.clone().with_context(context.into()))
        .unwrap_or(base);
    if present_collection_selection_menu(target, menus, playback.clone(), position) {
        return;
    }
    let favorite = (menus.projected_item_favorite)(
        &FavoriteTarget::Album(album.media_uri.clone()),
        album.state.favorite,
    );
    let surface = ContextMenuSurface::new(target, "album", position);
    surface.append_play_actions(&menus.settings.current.borrow().application.context_menu);
    surface.append_configurable_submenu(
        ContextMenuItem::PlayRadio,
        msgid("Album radio"),
        &radio_context_submenu("album"),
        RADIO_ICON,
    );
    append_favorite_action(&surface, favorite);
    append_context_menu_picker(
        &surface,
        menus,
        gtk_widgets::selection::album_play_request(&album),
    );
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
        .links
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
        &gtk_widgets::media_drag::MediaDragSource::capture_target(
            gtk_widgets::selection::album_play_request(&album),
        ),
        Some(&album.media_uri),
        false,
        album.summary.track_count > 0
            && album.summary.downloaded_count == album.summary.track_count,
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
    for (index, artist) in album.links.album_artists.iter().enumerate() {
        let action = if album.links.album_artists.len() == 1 {
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
                album
                    .state
                    .rating
                    .and_then(|value| u8::try_from(value).ok()),
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
    surface.popup(&menus.settings.current.borrow().application.context_menu);
}

pub fn present_artist_context_menu(
    target: &gtk::Widget,
    menus: &Rc<MediaMenus>,
    artist: ArtistView,
    album_artist: bool,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
) {
    if present_collection_selection_menu(
        target,
        menus,
        gtk_widgets::selection::artist_play_request(&artist, album_artist),
        position,
    ) {
        return;
    }
    let favorite = (menus.projected_item_favorite)(
        &FavoriteTarget::Artist(artist.media_uri.clone()),
        artist.state.favorite,
    );
    let surface = ContextMenuSurface::new(target, "artist", position);
    surface.append_play_actions(&menus.settings.current.borrow().application.context_menu);
    surface.append_configurable_submenu(
        ContextMenuItem::PlayRadio,
        msgid("Artist radio"),
        &radio_context_submenu("artist"),
        RADIO_ICON,
    );
    append_favorite_action(&surface, favorite);
    append_context_menu_picker(
        &surface,
        menus,
        if album_artist {
            gtk_widgets::selection::artist_play_request(&artist, true)
        } else {
            gtk_widgets::selection::artist_play_request(&artist, false)
        },
    );
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
        &go_to_context_submenu("artist", std::slice::from_ref(&artist.metadata.name), false),
        GO_TO_ICON,
    );
    let playback = if album_artist {
        gtk_widgets::selection::artist_play_request(&artist, true)
    } else {
        gtk_widgets::selection::artist_play_request(&artist, false)
    };
    install_download_actions(
        &surface,
        menus,
        &gtk_widgets::media_drag::MediaDragSource::capture_target(playback.clone()),
        Some(&artist.media_uri),
        false,
        artist.summary.track_count > 0
            && artist.summary.downloaded_count == artist.summary.track_count,
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
                artist
                    .state
                    .rating
                    .and_then(|value| u8::try_from(value).ok()),
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
    surface.popup(&menus.settings.current.borrow().application.context_menu);
}

pub fn present_genre_context_menu(
    target: &gtk::Widget,
    menus: &Rc<MediaMenus>,
    genre: GenreView,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
) {
    if present_collection_selection_menu(
        target,
        menus,
        collection_play_request(library::TrackScope::Genre(genre.genre_key)),
        position,
    ) {
        return;
    }
    let surface = ContextMenuSurface::new(target, "genre", position);
    surface.append_play_actions(&menus.settings.current.borrow().application.context_menu);
    surface.append_configurable_submenu(
        ContextMenuItem::PlayRadio,
        msgid("Genre radio"),
        &radio_context_submenu("genre"),
        RADIO_ICON,
    );
    install_sidebar_pin_action(
        &surface,
        menus,
        Some(SidebarPin::Genre {
            source_id: sources::SourceId::new(genre.source_id.clone()),
            genre_id: genre.object_id.clone(),
        }),
    );
    let playback = collection_play_request(library::TrackScope::Genre(genre.genre_key));
    install_download_actions(
        &surface,
        menus,
        &gtk_widgets::media_drag::MediaDragSource::capture_target(playback.clone()),
        None,
        false,
        genre.summary.track_count > 0
            && genre.summary.downloaded_count == genre.summary.track_count,
    );
    install_loaded_actions(&surface, menus, playback, play);
    install_radio_actions(&surface, menus, RadioSeed::Genre(genre.genre_key));
    surface.popup(&menus.settings.current.borrow().application.context_menu);
}

pub fn present_mood_context_menu(
    target: &gtk::Widget,
    menus: &Rc<MediaMenus>,
    mood: MoodView,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
) {
    if present_collection_selection_menu(
        target,
        menus,
        collection_play_request(library::TrackScope::Mood(mood.mood_key)),
        position,
    ) {
        return;
    }
    let surface = ContextMenuSurface::new(target, "mood", position);
    surface.append_play_actions(&menus.settings.current.borrow().application.context_menu);
    let playback = collection_play_request(library::TrackScope::Mood(mood.mood_key));
    install_download_actions(
        &surface,
        menus,
        &gtk_widgets::media_drag::MediaDragSource::capture_target(playback.clone()),
        None,
        false,
        mood.summary.track_count > 0 && mood.summary.downloaded_count == mood.summary.track_count,
    );
    install_loaded_actions(&surface, menus, playback.clone(), play);
    append_context_menu_picker(&surface, menus, playback);
    surface.popup(&menus.settings.current.borrow().application.context_menu);
}

pub fn present_playlist_context_menu(
    target: &gtk::Widget,
    menus: &Rc<MediaMenus>,
    playlist: PlaylistRow,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
    current: Option<String>,
) {
    if present_playlist_selection_menu(
        target,
        menus,
        collection_play_request(library::TrackScope::Playlist(playlist.playlist_key)),
        position,
    ) {
        return;
    }
    let surface = ContextMenuSurface::new(target, "playlist", position);
    surface.append_play_actions(&menus.settings.current.borrow().application.context_menu);
    surface.append_configurable_submenu(
        ContextMenuItem::PlayRadio,
        msgid("Playlist radio"),
        &radio_context_submenu("playlist"),
        RADIO_ICON,
    );
    install_sidebar_pin_action(
        &surface,
        menus,
        Some(SidebarPin::Playlist {
            source_id: playlist.source_id.clone().map(sources::SourceId::new),
            playlist_id: playlist.object_id.clone(),
        }),
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
    if playlist.metadata_writable {
        surface.append_fixed_action(msgid("Edit"), "edit", EDIT_ICON);
    }
    if playlist.writable {
        surface.append_fixed_action(msgid("Add current"), "add-current", ADD_ICON);
    }
    if rufin_core::playlists::can_remove_unavailable(&menus.source, &playlist) {
        surface.append_fixed_action(
            msgid("Remove unavailable tracks"),
            "remove-unavailable",
            TRASH_ICON,
        );
        let cleanup_menus = Rc::downgrade(menus);
        let key = playlist.playlist_key;
        let name = playlist.name.clone();
        let artwork = gtk_widgets::library_fields::playlist_artwork(&playlist);
        surface.add_action("remove-unavailable", move || {
            let Some(menus) = cleanup_menus.upgrade() else {
                return;
            };
            let result = rufin_core::playlists::remove_unavailable_tracks(&menus.source, key);
            let menus = Rc::downgrade(&menus);
            let name = name.clone();
            let artwork = artwork.clone();
            gtk::glib::spawn_future_local(async move {
                let Ok(result) = result.recv().await else {
                    return;
                };
                let Some(menus) = menus.upgrade() else {
                    return;
                };
                let removed = match result {
                    Ok(removed) => removed,
                    Err(error) => {
                        (menus.show_error)(error);
                        return;
                    }
                };
                (menus.operation_feedback)(
                    &OperationFeedback {
                        title: name.clone(),
                        artwork: OperationArtwork::Bindings(artwork),
                        item_count: removed,
                        kind: OperationFeedbackKind::PlaylistRemoved { destination: name },
                    },
                    None,
                );
            });
        });
    }
    if playlist.metadata_writable {
        surface.append_fixed_action(msgid("Delete"), "delete", DELETE_ICON);
    }
    let playback = collection_play_request(library::TrackScope::Playlist(playlist.playlist_key));
    install_download_actions(
        &surface,
        menus,
        &gtk_widgets::media_drag::MediaDragSource::capture_target(playback.clone()),
        None,
        false,
        playlist.track_count > 0 && playlist.downloaded_count == playlist.track_count,
    );
    install_loaded_actions(&surface, menus, playback, play);
    install_radio_actions(&surface, menus, RadioSeed::Playlist(playlist.playlist_key));
    let edit_menus = Rc::clone(menus);
    let playlist_key = playlist.playlist_key;
    let playlist_name = playlist.name.clone();
    surface.add_action("edit", move || {
        (edit_menus.edit_playlist_dialog)(playlist_key, playlist_name.clone());
    });
    let add_menus = Rc::clone(menus);
    surface.add_action_enabled("add-current", current.is_some(), move || {
        if let Some(media_uri) = current.as_ref() {
            gtk_widgets::playlists::add_media_to_playlist(
                &add_menus.source,
                playlist_key,
                library::CatalogQuery::uris(library::CatalogKind::Track, vec![media_uri.clone()])
                    .selection(),
                false,
                Rc::new(|_| {}),
            );
        }
    });
    let delete_menus = Rc::clone(menus);
    let delete_target = target.downgrade();
    surface.add_action("delete", move || {
        let Some(target) = delete_target.upgrade() else {
            return;
        };
        let menus = Rc::clone(&delete_menus);
        let dialog = gtk_widgets::playlists::delete_playlist_dialog(&playlist.name, move || {
            let source = menus.source.clone();
            gtk::glib::spawn_future_local(async move {
                let _ = rufin_core::playlists::delete_playlist(&source, playlist_key).await;
            });
            (menus.navigate)(Route::Playlists);
        });
        dialog.present(Some(&target));
    });
    surface.popup(&menus.settings.current.borrow().application.context_menu);
}

pub fn present_smart_playlist_context_menu(
    target: &gtk::Widget,
    menus: &Rc<MediaMenus>,
    playlist: SmartPlaylistRow,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
) {
    if present_playlist_selection_menu(
        target,
        menus,
        gtk_widgets::selection::smart_play_request(
            playlist.smart_playlist_key,
            menus.source.selected_library().as_ref(),
        ),
        position,
    ) {
        return;
    }
    let surface = ContextMenuSurface::new(target, "smart-playlist", position);
    surface.append_play_actions(&menus.settings.current.borrow().application.context_menu);
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
    let playback = gtk_widgets::selection::smart_play_request(
        playlist.smart_playlist_key,
        menus.source.selected_library().as_ref(),
    );
    install_download_actions(
        &surface,
        menus,
        &gtk_widgets::media_drag::MediaDragSource::capture_target(playback.clone()),
        None,
        false,
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
    let delete_target = target.downgrade();
    surface.add_action("delete", move || {
        let Some(target) = delete_target.upgrade() else {
            return;
        };
        let menus = Rc::clone(&delete_menus);
        let dialog = gtk_widgets::playlists::delete_playlist_dialog(&playlist.name, move || {
            let navigate = Rc::clone(&menus);
            (menus.publish_smart_playlist_change)(
                SmartPlaylistChange::Delete(key),
                Some(Rc::new(move |result| {
                    if result.is_ok() {
                        (navigate.navigate)(Route::SmartPlaylists);
                    }
                })),
            );
        });
        dialog.present(Some(&target));
    });
    surface.popup(&menus.settings.current.borrow().application.context_menu);
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

fn install_loaded_actions(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
    target: PlayRequest,
    play: Option<CollectionPlay>,
) {
    for (action, placement, shuffled) in PLAY_ACTIONS {
        let target = target.clone();
        let menus = Rc::clone(menus);
        let play = play.clone();
        surface.add_action(action, move || {
            if let Some(play) = &play {
                play(placement, shuffled);
            } else {
                (menus.play_target)(&target, placement, shuffled);
            }
        });
    }
}

fn install_media_uri_playback_actions(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
    media_uri: String,
) {
    for (action, placement, shuffled) in PLAY_ACTIONS {
        let queue = menus.playback.clone();
        let media_uri = media_uri.clone();
        surface.add_action(action, move || {
            queue.play_selection(
                PlayRequest::uris(vec![media_uri.clone()], placement).shuffled(shuffled),
            )
        });
    }
}

pub fn install_download_actions(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
    payload: &gtk_widgets::media_drag::MediaDragSource,
    media_uri: Option<&str>,
    single: bool,
    downloaded: bool,
) {
    let available = media_uri.is_none_or(|uri| {
        if let Some((source_id, _, _)) = library::source_entity_parts(uri) {
            (menus.source_download_available)(&source_id)
        } else {
            library::normalize_direct_media_uri(uri)
                .is_some_and(|uri| uri.starts_with("https:") || uri.starts_with("http:"))
        }
    });
    if !downloaded && !available {
        return;
    }
    if downloaded {
        surface.append_configurable_action(
            ContextMenuItem::Download,
            if single {
                msgid("Remove Download")
            } else {
                msgid("Remove Downloads")
            },
            "remove-downloads",
            TRASH_ICON,
        );
        let menus = Rc::clone(menus);
        let payload = payload.clone();
        surface.add_action("remove-downloads", move || {
            (menus.remove_download)(&payload)
        });
    } else {
        surface.append_configurable_action(
            ContextMenuItem::Download,
            msgid("Download"),
            "download",
            DOWNLOAD_ICON,
        );
        let menus = Rc::clone(menus);
        let payload = payload.clone();
        surface.add_action("download", move || (menus.download)(&payload));
    }
}

fn install_radio_actions(surface: &ContextMenuSurface, menus: &MediaMenus, seed: RadioSeed) {
    for (action, request) in [
        ("play-radio", RadioPlayRequest::now(seed.clone())),
        ("play-radio-next", RadioPlayRequest::next(seed.clone())),
        ("play-radio-last", RadioPlayRequest::last(seed)),
    ] {
        let radio = menus.playback.clone();
        surface.add_action(action, move || radio.play_radio(request.clone()));
    }
}

fn context_menu_rating_visible(menus: &MediaMenus) -> bool {
    menus
        .settings
        .current
        .borrow()
        .application
        .context_menu
        .rating_visible
}

fn append_context_menu_picker_entries(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
    selection: PlaylistEntrySelectionSnapshot,
) {
    (menus.picker_payload)(
        surface,
        gtk_widgets::media_drag::MediaDragSource::playlist_entries(selection),
    );
}

fn append_context_menu_picker_selection(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
    selection: TrackSelectionSnapshot,
) {
    (menus.picker_payload)(
        surface,
        gtk_widgets::media_drag::MediaDragSource::selection(selection),
    );
}

fn append_context_menu_picker_media(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
    media_uri: String,
) {
    (menus.picker_payload)(
        surface,
        gtk_widgets::media_drag::MediaDragSource::media_uris([media_uri]),
    );
}

fn append_context_menu_picker(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
    target: PlayRequest,
) {
    (menus.picker_target)(surface, target);
}
impl MediaMenus {
    fn projected_track_favorite(&self, media_uri: &str, fallback: bool) -> bool {
        (self.projected_item_favorite)(&FavoriteTarget::Track(media_uri.to_string()), fallback)
    }
}
