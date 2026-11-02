use std::{rc::Rc, sync::Arc};

use adw::prelude::*;
use gtk::glib;
use library::{
    AlbumRow, ArtistRow, FavoriteTarget, PlaylistRow, RadioSeed, SmartPlaylistRow, TagRow,
    TrackLinks, TrackRow, TrackView,
};
use rufin_core::collection_context::CollectionTarget;
use rufin_core::playback::{QueuePlacement, RadioPlayRequest};

use crate::widgets::controls::{ADD_ICON, DELETE_ICON, EDIT_ICON, REMOVE_ICON, TRASH_ICON};
use crate::widgets::downloads::{OperationArtwork, OperationFeedback, OperationFeedbackKind};
use crate::widgets::interactions::{
    ALBUM_ICON, ARTIST_ICON, ContextMenuSurface, DOWNLOAD_ICON, GO_TO_ICON,
};
use crate::widgets::ratings::context_rating_row;
use localization::msgid;
use rufin_core::metadata::{MediaDetailsItem, MetadataItemId};
use rufin_core::playlists::SmartPlaylistChange;
use rufin_core::settings::ContextMenuItem;
use rufin_core::settings::SidebarPin;

use crate::widgets::selection::{collection_play_request, track_play_request};
use rufin_core::playback::PlayRequest;
pub type CollectionPlay = Rc<dyn Fn(QueuePlacement, bool)>;

use crate::widgets::selection::{PlaylistEntrySelectionSnapshot, TrackSelectionSnapshot};
use rufin_core::route::Route;

pub fn add_drag_to_playlist(
    menus: &Rc<MediaMenus>,
    playlist_key: library::PlaylistKey,
    playlist_name: String,
    source: crate::widgets::media_drag::MediaDragSource,
) -> bool {
    let catalog = Arc::clone(&menus.library);
    let task = menus
        .runtime
        .spawn(async move { source.playlist_input(&catalog).await });
    let menus = Rc::downgrade(menus);
    glib::spawn_future_local(async move {
        let Some((selection, title, preview_uris)) = task.await.ok().and_then(Result::ok) else {
            return;
        };
        let Some(owner) = menus.upgrade() else { return };
        crate::widgets::playlists::add_media_to_playlist(
            &owner.source,
            playlist_key,
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
                                destination: playlist_name.clone(),
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
    pub current_route: Rc<dyn Fn() -> Route>,
    pub present_dialog: Rc<dyn Fn(&adw::Dialog)>,
    pub artwork: Rc<crate::widgets::artwork::ArtworkState>,
    pub library: Arc<library::Catalog>,
    pub runtime: tokio::runtime::Handle,
    pub source: rufin_core::runtime::SourceHandle,
    pub playback: std::sync::Arc<rufin_core::playback::PlaybackOwner>,
    pub settings: Rc<crate::widgets::settings::SettingsState>,
    pub navigate: Rc<dyn Fn(Route)>,
    pub half_stars_enabled: Rc<dyn Fn(&str, Option<&str>) -> bool>,
    pub set_favorite: Rc<dyn Fn(FavoriteTarget, bool)>,
    pub edit_metadata: Rc<dyn Fn(MetadataItemId)>,
    pub edit_track_metadata: Rc<dyn Fn(library::MemberSelection)>,
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
    pub picker_payload:
        Rc<dyn Fn(&ContextMenuSurface, crate::widgets::media_drag::MediaDragSource)>,
    pub play_target: Rc<dyn Fn(&PlayRequest, QueuePlacement, bool)>,
    pub download: Rc<dyn Fn(&crate::widgets::media_drag::MediaDragSource)>,
    pub remove_download: Rc<dyn Fn(&crate::widgets::media_drag::MediaDragSource)>,
}
fn track_menu_subtitle(artist: &str, album: &str) -> String {
    [artist, album]
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
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
    crate::menus::media_details::append_header(
        &surface,
        target,
        menus,
        crate::menus::media_details::MenuHeader {
            item: MediaDetailsItem::Track(track.media_uri.clone()),
            title: &track.metadata.title,
            subtitle: &track_menu_subtitle(&track.metadata.artist, &track.metadata.album),
            artwork: track.artwork_binding.as_deref(),
            favorite: (
                FavoriteTarget::Track(track.media_uri.clone()),
                track.state.favorite,
            ),
        },
    );
    if let Some((position, occurrence_id)) = queue_entry {
        append_queue_entry_actions(&surface, menus, track.track_key, position, occurrence_id);
    } else {
        append_loaded_play(&surface, menus, track_play_request(track.track_key), None);
    }
    append_radio(
        &surface,
        menus,
        msgid("Track radio"),
        RadioSeed::Track(track.media_uri.clone()),
    );
    append_context_menu_picker(&surface, menus, track_play_request(track.track_key));
    if track.access.cue_path.is_none()
        && (track.source_key.is_some() || track.media_uri.starts_with("file:"))
    {
        let metadata_menus = Rc::clone(menus);
        let media_uri = track.media_uri.clone();
        surface.append_configurable_action(
            ContextMenuItem::EditMetadata,
            msgid("Edit metadata"),
            EDIT_ICON,
            move || (metadata_menus.edit_metadata)(MetadataItemId::Track(media_uri.clone())),
        );
    }
    install_track_navigation(
        &surface,
        menus,
        &track.links,
        &track.media_uri,
        track.source_id.as_deref(),
    );
    let playback = track_play_request(track.track_key);
    install_download_actions(
        &surface,
        menus,
        &crate::widgets::media_drag::MediaDragSource::capture_target(playback),
        Some(&track.media_uri),
        true,
        track.access.is_downloaded,
    );
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
    surface.popup(&menus.settings.current.borrow().application.context_menu);
}

/// A queue entry's Remove from Queue and Play actions: unshuffled plays move the entry; shuffled
/// ones play its track.
fn append_queue_entry_actions(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
    track: library::TrackKey,
    position: usize,
    occurrence_id: Option<u64>,
) {
    let selection = match occurrence_id {
        Some(id) => rufin_core::playback::QueueSelection::Occurrences(vec![id]),
        None => rufin_core::playback::QueueSelection::Positions(vec![position]),
    };
    let remove = selection.clone();
    let queue = menus.playback.clone();
    surface.append_fixed_action(msgid("Remove from Queue"), REMOVE_ICON, move || {
        queue.remove_many(remove.clone())
    });
    let queue = menus.playback.clone();
    let play_menus = Rc::clone(menus);
    surface.append_play_actions(
        &menus.settings.current.borrow().application.context_menu,
        move |placement, shuffled| match (placement, occurrence_id) {
            _ if shuffled => (play_menus.play_target)(&track_play_request(track), placement, true),
            (QueuePlacement::Next, Some(id)) => queue.move_occurrence_after_current(id),
            (QueuePlacement::Next, None) => queue.move_after_current(position),
            (QueuePlacement::End, _) => queue.reorder(
                selection.clone(),
                rufin_core::playback::QueueReorderTarget::End,
            ),
            (_, Some(id)) => queue.activate_occurrence(id),
            (_, None) => queue.activate(position),
        },
    );
}

fn install_track_navigation(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
    links: &TrackLinks,
    media_uri: &str,
    source_id: Option<&str>,
) {
    let album_media_uri = links.album_media_uri.as_deref();
    let (artists, album_artist) = rufin_core::route::detail_links::go_to_artists(links);
    let submenu = go_to_submenu(
        surface,
        menus,
        artists.iter().map(|artist| {
            let key = artist.media_uri.clone();
            let route = if album_artist {
                Route::AlbumArtistDetail(key)
            } else {
                Route::ArtistDetail(key)
            };
            (artist.name.as_str(), route)
        }),
        album_media_uri.map(|album| Route::AlbumDetail(album.to_owned())),
    );
    install_source_navigation(
        surface,
        menus,
        &submenu,
        media_uri,
        source_id,
        album_media_uri,
        false,
    );
    if submenu.n_items() > 0 {
        surface.append_configurable_submenu(
            ContextMenuItem::GoTo,
            msgid("Go to"),
            &submenu,
            GO_TO_ICON,
        );
    }
}

/// The Go to submenu of artists and an album, with the actions that open them.
fn go_to_submenu<'a>(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
    artists: impl IntoIterator<Item = (&'a str, Route)>,
    album: Option<Route>,
) -> gtk::gio::Menu {
    let submenu = gtk::gio::Menu::new();
    let artists = artists.into_iter().map(|(name, route)| {
        (
            localization::tr_with("Go to {artist}", &[("artist", name)]),
            ARTIST_ICON,
            route,
        )
    });
    let album = album.map(|route| (localization::tr(msgid("Go to Album")), ALBUM_ICON, route));
    for (label, icon, route) in artists.chain(album) {
        let menus = Rc::clone(menus);
        submenu.append_item(&surface.item(&label, icon, move || (menus.navigate)(route.clone())));
    }
    submenu
}

#[allow(clippy::too_many_arguments)]
fn install_source_navigation(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
    submenu: &gtk::gio::Menu,
    media_uri: &str,
    source_id: Option<&str>,
    album_uri: Option<&str>,
    album_artist: bool,
) {
    let source_id = source_id
        .map(sources::SourceId::new)
        .or_else(|| library::source_entity_parts(media_uri).map(|(id, _, _)| id));
    let configuration = source_id
        .as_ref()
        .and_then(|id| menus.source.configuration(id));
    let Some(link) = rufin_core::detail_links::navigation_source_link(
        configuration.as_ref(),
        media_uri,
        album_uri,
    ) else {
        return;
    };
    let window = surface
        .popover()
        .root()
        .and_downcast::<gtk::ApplicationWindow>()
        .map(|window| window.downgrade());
    let menus = Rc::clone(menus);
    let media_uri = media_uri.to_owned();
    let (title, icon_name) = (localization::tr(link.title), link.icon_name);
    submenu.append_item(&surface.item(&title, icon_name, move || {
        let Some(window) = window.as_ref().and_then(glib::WeakRef::upgrade) else {
            return;
        };
        let source = menus.source.clone();
        let folder = source
            .selected_library()
            .filter(|selected| source_id.as_ref() == Some(&selected.source_id))
            .and_then(|selected| selected.music_folder_key);
        let media_uri = media_uri.clone();
        let url = link.url.clone();
        let show_error = Rc::clone(&menus.show_error);
        glib::spawn_future_local(async move {
            let result = async {
                if let Some(url) = url {
                    gtk::UriLauncher::new(&url)
                        .launch_future(Some(&window))
                        .await
                        .map_err(|error| error.to_string())
                } else {
                    let uri = source
                        .collection_folder_uri(media_uri, album_artist, folder)
                        .recv()
                        .await
                        .map_err(|error| error.to_string())??;
                    crate::widgets::folder_launcher::open_folder_uri(&window, &uri).await
                }
            }
            .await;
            if let Err(error) = result {
                show_error(error);
            }
        });
    }));
}

pub fn present_playlist_entry_menu(
    target: &gtk::Widget,
    menus: &Rc<MediaMenus>,
    row: TrackRow,
    position: Option<(f64, f64)>,
    _playlist: library::PlaylistKey,
    removal: Option<PlaylistEntrySelectionSnapshot>,
) {
    let surface = ContextMenuSurface::new(target, "playlist-entry", position);
    crate::menus::media_details::append_header(
        &surface,
        target,
        menus,
        crate::menus::media_details::MenuHeader {
            item: MediaDetailsItem::Track(row.media_uri.clone()),
            title: &row.title,
            subtitle: &track_menu_subtitle(&row.artist, &row.album),
            artwork: row.artwork_binding.as_deref(),
            favorite: (
                FavoriteTarget::Track(row.media_uri.clone()),
                row.state.favorite,
            ),
        },
    );
    let queue = menus.playback.clone();
    let media_uri = row.media_uri.clone();
    surface.append_play_actions(
        &menus.settings.current.borrow().application.context_menu,
        move |placement, shuffled| {
            queue.play_selection(
                PlayRequest::uris(vec![media_uri.clone()], placement).shuffled(shuffled),
            )
        },
    );
    if let Some(removal) = removal.filter(|selection| selection.writable) {
        let remove_menus = Rc::downgrade(menus);
        surface.append_fixed_action(msgid("Remove from Playlist"), REMOVE_ICON, move || {
            if let Some(menus) = remove_menus.upgrade() {
                remove_playlist_entry_selection(&menus, removal.clone());
            }
        });
    }
    install_track_navigation(
        &surface,
        menus,
        &row.links,
        &row.media_uri,
        row.source_id.as_deref(),
    );
    install_download_actions(
        &surface,
        menus,
        &crate::widgets::media_drag::MediaDragSource::capture_target(track_play_request(
            row.track_key,
        )),
        Some(&row.media_uri),
        true,
        row.is_downloaded,
    );
    if row.media_uri.starts_with("file:") || library::source_entity_parts(&row.media_uri).is_some()
    {
        let metadata_menus = Rc::clone(menus);
        let media_uri = row.media_uri.clone();
        surface.append_configurable_action(
            ContextMenuItem::EditMetadata,
            msgid("Edit metadata"),
            EDIT_ICON,
            move || (metadata_menus.edit_metadata)(MetadataItemId::Track(media_uri.clone())),
        );
    }
    append_context_menu_picker_media(&surface, menus, row.media_uri.clone());
    if context_menu_rating_visible(menus) {
        let rating_menus = Rc::clone(menus);
        let media_uri = row.media_uri.clone();
        surface.append_fixed_widget(
            "rating",
            &context_rating_row(
                row.state.rating.and_then(|value| u8::try_from(value).ok()),
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
    crate::widgets::media_drag::CollectionTargets,
)> {
    let targets = crate::widgets::media_drag::selected_collection_targets(target, &clicked)?;
    let payload = crate::widgets::media_drag::MediaDragSource::Targets {
        targets: targets.clone(),
    };
    let surface = ContextMenuSurface::new(target, "collection-selection", position);
    let metadata_targets = targets.clone();
    let metadata_menus = Rc::clone(menus);
    let edit_metadata = move || {
        let targets = metadata_targets.clone();
        let menus = Rc::clone(&metadata_menus);
        let catalog = menus.library.clone();
        let task = menus.runtime.spawn(async move {
            targets.resolve(&catalog).await.map(|targets| {
                library::MemberSelection::Concat(
                    targets
                        .into_iter()
                        .filter_map(|target| target.selection())
                        .collect(),
                )
            })
        });
        glib::spawn_future_local(async move {
            match task.await {
                Ok(Ok(selection)) => (menus.edit_track_metadata)(selection),
                Ok(Err(error)) => (menus.show_error)(error),
                Err(error) => (menus.show_error)(error.to_string()),
            }
        });
    };
    surface.append_configurable_action(
        ContextMenuItem::EditMetadata,
        msgid("Edit metadata"),
        EDIT_ICON,
        edit_metadata,
    );
    let play_menus = Rc::clone(menus);
    let play_payload = payload.clone();
    surface.append_play_actions(
        &menus.settings.current.borrow().application.context_menu,
        move |placement, shuffled| {
            let payload = play_payload.clone();
            let catalog = play_menus.library.clone();
            let task = play_menus
                .runtime
                .spawn(async move { payload.play_request(&catalog).await });
            let menus = Rc::clone(&play_menus);
            glib::spawn_future_local(async move {
                match task.await {
                    Ok(Ok(request)) => (menus.play_target)(&request, placement, shuffled),
                    Ok(Err(error)) => (menus.show_error)(error),
                    Err(error) => (menus.show_error)(error.to_string()),
                }
            });
        },
    );
    (menus.picker_payload)(&surface, payload.clone());
    install_selection_download_actions(&surface, menus, payload);
    Some((surface, targets))
}

fn present_playlist_selection_menu(
    target: &gtk::Widget,
    menus: &Rc<MediaMenus>,
    clicked: PlayRequest,
    position: Option<(f64, f64)>,
    in_folder: bool,
) -> bool {
    let Some((surface, targets)) = collection_selection_surface(target, menus, clicked, position)
    else {
        return false;
    };
    let folder_targets = targets.clone();
    let delete_menus = Rc::clone(menus);
    let delete_target = target.downgrade();
    let delete = move || {
        let Some(target) = delete_target.upgrade() else {
            return;
        };
        let menus = Rc::clone(&delete_menus);
        let targets = targets.clone();
        let dialog =
            crate::widgets::playlists::delete_playlists_dialog(targets.count(), move || {
                let targets = targets.clone();
                let source = menus.source.clone();
                let catalog = Arc::clone(&menus.library);
                let task = menus.runtime.spawn(async move {
                    let targets = targets.collections(&catalog).await?;
                    let mut smart = Vec::new();
                    let mut failure = None;
                    for collection in targets {
                        match collection {
                            library::Collection::Playlist(key) => {
                                if let Err(error) =
                                    rufin_core::playlists::delete_playlist(&source, key).await
                                {
                                    failure.get_or_insert_with(|| error.to_string());
                                }
                            }
                            library::Collection::Smart { key, .. } => smart.push(key),
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
    };
    let catalog = menus.library.clone();
    let task = menus
        .runtime
        .spawn(async move { folder_targets.collections(&catalog).await });
    let menus = Rc::clone(menus);
    glib::spawn_future_local(async move {
        match task.await {
            Ok(Ok(collections)) => {
                let members = collections
                    .into_iter()
                    .filter_map(|collection| match collection {
                        library::Collection::Playlist(key) => {
                            Some(library::PlaylistFolderMember::Playlist { key })
                        }
                        library::Collection::Smart { key, .. } => {
                            Some(library::PlaylistFolderMember::SmartPlaylist { key })
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                if !members.is_empty() {
                    crate::menus::playlist_folders::append_folder_actions(
                        &surface, &menus, members, in_folder,
                    );
                }
                surface.append_fixed_action(msgid("Delete"), DELETE_ICON, delete);
                surface.popup(&menus.settings.current.borrow().application.context_menu);
            }
            Ok(Err(error)) => (menus.show_error)(error),
            Err(error) => (menus.show_error)(error.to_string()),
        }
    });
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
        let remove_menus = Rc::clone(menus);
        let removal = selection.clone();
        surface.append_fixed_action(msgid("Remove from Playlist"), REMOVE_ICON, move || {
            remove_playlist_entry_selection(&remove_menus, removal.clone());
        });
    }
    append_playlist_entry_selection_actions(&surface, menus, selection);
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
        let songs = library::MemberSelection::Keys {
            source: None,
            keys: ((tracks).iter().map(|key| key.raw()).collect::<Vec<i64>>())
                .into_iter()
                .map(library::TrackKey::from_raw)
                .collect::<Vec<_>>()
                .into(),
        };
        let catalog = menus.library.clone();
        let preview = songs.clone();
        let artwork = menus
            .runtime
            .spawn(async move {
                catalog
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
                    crate::widgets::playlists::add_media_to_playlist(
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
    let play_menus = Rc::clone(menus);
    let play_selection = selection.clone();
    surface.append_play_actions(
        &menus.settings.current.borrow().application.context_menu,
        move |placement, shuffled| play_selection.play(&play_menus.playback, placement, shuffled),
    );
    append_context_menu_picker_entries(surface, menus, selection.clone());
    append_metadata_selection_action(surface, menus, selection.input.clone());
    let download_menus = Rc::clone(menus);
    let download_payload = crate::widgets::media_drag::MediaDragSource::playlist_entries(selection);
    surface.append_configurable_action(
        ContextMenuItem::Download,
        msgid("Download"),
        DOWNLOAD_ICON,
        move || (download_menus.download)(&download_payload),
    );
}

fn append_track_selection_actions(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
    selection: TrackSelectionSnapshot,
) {
    if selection.count == 0 {
        return;
    }
    let play_menus = Rc::clone(menus);
    let play_selection = selection.clone();
    surface.append_play_actions(
        &menus.settings.current.borrow().application.context_menu,
        move |placement, shuffled| play_selection.play(&play_menus.playback, placement, shuffled),
    );
    append_context_menu_picker_selection(surface, menus, selection.clone());
    append_metadata_selection_action(surface, menus, selection.input.clone());
    install_selection_download_actions(
        surface,
        menus,
        crate::widgets::media_drag::MediaDragSource::selection(selection),
    );
}

fn append_metadata_selection_action(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
    selection: library::MemberSelection,
) {
    let menus = Rc::clone(menus);
    surface.append_configurable_action(
        ContextMenuItem::EditMetadata,
        msgid("Edit metadata"),
        EDIT_ICON,
        move || (menus.edit_track_metadata)(selection.clone()),
    );
}

fn install_selection_download_actions(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
    payload: crate::widgets::media_drag::MediaDragSource,
) {
    let download_menus = Rc::clone(menus);
    let download_payload = payload.clone();
    surface.append_configurable_action(
        ContextMenuItem::Download,
        msgid("Download"),
        DOWNLOAD_ICON,
        move || (download_menus.download)(&download_payload),
    );
    let remove_menus = Rc::clone(menus);
    surface.append_configurable_action(
        ContextMenuItem::Download,
        msgid("Remove Downloads"),
        TRASH_ICON,
        move || (remove_menus.remove_download)(&payload),
    );
}

pub fn present_album_context_menu(
    target: &gtk::Widget,
    menus: &Rc<MediaMenus>,
    album: AlbumRow,
    playback_context: Option<String>,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
) {
    let base = crate::widgets::selection::album_play_request(&album);
    let playback = playback_context
        .map(|context| base.clone().with_context(context.into()))
        .unwrap_or(base);
    if present_collection_selection_menu(target, menus, playback.clone(), position) {
        return;
    }
    let surface = ContextMenuSurface::new(target, "album", position);
    crate::menus::media_details::append_header(
        &surface,
        target,
        menus,
        crate::menus::media_details::MenuHeader {
            item: MediaDetailsItem::Album(album.media_uri.clone()),
            title: &album.title,
            subtitle: &album.artist,
            artwork: album.artwork_binding.as_deref(),
            favorite: (
                FavoriteTarget::Album(album.media_uri.clone()),
                album.state.favorite,
            ),
        },
    );
    append_loaded_play(&surface, menus, playback, play);
    append_radio(
        &surface,
        menus,
        msgid("Album radio"),
        RadioSeed::Album(album.album_key),
    );
    append_context_menu_picker(
        &surface,
        menus,
        crate::widgets::selection::album_play_request(&album),
    );
    append_metadata_action(
        &surface,
        menus,
        MetadataItemId::Album(album.media_uri.clone()),
    );
    install_sidebar_pin_action(&surface, menus, album.pin());
    let submenu = go_to_submenu(
        &surface,
        menus,
        album.links.album_artists.iter().map(|artist| {
            let route = Route::AlbumArtistDetail(artist.media_uri.clone());
            (artist.name.as_str(), route)
        }),
        Some(Route::AlbumDetail(album.media_uri.clone())),
    );
    install_source_navigation(
        &surface,
        menus,
        &submenu,
        &album.media_uri,
        None,
        None,
        false,
    );
    if submenu.n_items() > 0 {
        surface.append_configurable_submenu(
            ContextMenuItem::GoTo,
            msgid("Go to"),
            &submenu,
            GO_TO_ICON,
        );
    }
    install_download_actions(
        &surface,
        menus,
        &crate::widgets::media_drag::MediaDragSource::capture_target(
            crate::widgets::selection::album_play_request(&album),
        ),
        Some(&album.media_uri),
        false,
        album.summary.track_count > 0
            && album.summary.downloaded_count == album.summary.track_count,
    );
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
    artist: ArtistRow,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
) {
    if present_collection_selection_menu(
        target,
        menus,
        crate::widgets::selection::artist_play_request(&artist, artist.album_artist),
        position,
    ) {
        return;
    }
    let surface = ContextMenuSurface::new(target, "artist", position);
    crate::menus::media_details::append_header(
        &surface,
        target,
        menus,
        crate::menus::media_details::MenuHeader {
            item: MediaDetailsItem::Artist {
                media_uri: artist.media_uri.clone(),
                album_artist: artist.album_artist,
            },
            title: &artist.name,
            subtitle: "",
            artwork: artist.artwork_binding.as_deref(),
            favorite: (
                FavoriteTarget::Artist(artist.media_uri.clone()),
                artist.state.favorite,
            ),
        },
    );
    let playback = crate::widgets::selection::artist_play_request(&artist, artist.album_artist);
    append_loaded_play(&surface, menus, playback.clone(), play);
    append_radio(
        &surface,
        menus,
        msgid("Artist radio"),
        if artist.album_artist {
            RadioSeed::AlbumArtist(artist.artist_key)
        } else {
            RadioSeed::Artist(artist.artist_key)
        },
    );
    append_context_menu_picker(&surface, menus, playback.clone());
    append_metadata_action(
        &surface,
        menus,
        MetadataItemId::Artist(artist.media_uri.clone()),
    );
    install_sidebar_pin_action(&surface, menus, artist.pin());
    let submenu = go_to_submenu(
        &surface,
        menus,
        [(artist.name.as_str(), Route::of_artist(&artist))],
        None,
    );
    install_source_navigation(
        &surface,
        menus,
        &submenu,
        &artist.media_uri,
        None,
        None,
        artist.album_artist,
    );
    surface.append_configurable_submenu(
        ContextMenuItem::GoTo,
        msgid("Go to"),
        &submenu,
        GO_TO_ICON,
    );
    install_download_actions(
        &surface,
        menus,
        &crate::widgets::media_drag::MediaDragSource::capture_target(playback),
        Some(&artist.media_uri),
        false,
        artist.summary.track_count > 0
            && artist.summary.downloaded_count == artist.summary.track_count,
    );
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
    genre: TagRow,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
) {
    let key = library::GenreKey::from_raw(genre.key);
    let playback = collection_play_request(
        library::Collection::Genre(key),
        &CollectionTarget::Genre(key),
    );
    if present_collection_selection_menu(target, menus, playback.clone(), position) {
        return;
    }
    let surface = ContextMenuSurface::new(target, "genre", position);
    append_loaded_play(&surface, menus, playback.clone(), play);
    append_radio(&surface, menus, msgid("Genre radio"), RadioSeed::Genre(key));
    install_sidebar_pin_action(&surface, menus, genre.pin());
    install_download_actions(
        &surface,
        menus,
        &crate::widgets::media_drag::MediaDragSource::capture_target(playback),
        None,
        false,
        genre.summary.track_count > 0
            && genre.summary.downloaded_count == genre.summary.track_count,
    );
    surface.popup(&menus.settings.current.borrow().application.context_menu);
}

pub fn present_mood_context_menu(
    target: &gtk::Widget,
    menus: &Rc<MediaMenus>,
    mood: TagRow,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
) {
    let key = library::MoodKey::from_raw(mood.key);
    let playback =
        collection_play_request(library::Collection::Mood(key), &CollectionTarget::Mood(key));
    if present_collection_selection_menu(target, menus, playback.clone(), position) {
        return;
    }
    let surface = ContextMenuSurface::new(target, "mood", position);
    append_loaded_play(&surface, menus, playback.clone(), play);
    install_download_actions(
        &surface,
        menus,
        &crate::widgets::media_drag::MediaDragSource::capture_target(playback.clone()),
        None,
        false,
        mood.summary.track_count > 0 && mood.summary.downloaded_count == mood.summary.track_count,
    );
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
    let in_folder = menus
        .library
        .playlist_member_folder(&library::PlaylistFolderMember::Playlist {
            key: playlist.playlist_key,
        })
        .is_some();
    let playback = collection_play_request(
        library::Collection::Playlist(playlist.playlist_key),
        &CollectionTarget::Playlist(playlist.playlist_key),
    );
    if present_playlist_selection_menu(target, menus, playback.clone(), position, in_folder) {
        return;
    }
    let surface = ContextMenuSurface::new(target, "playlist", position);
    append_loaded_play(&surface, menus, playback.clone(), play);
    append_radio(
        &surface,
        menus,
        msgid("Playlist radio"),
        RadioSeed::Playlist(playlist.playlist_key),
    );
    install_sidebar_pin_action(&surface, menus, Some(playlist.pin()));
    append_export_action(
        &surface,
        menus,
        rufin_core::runtime::source::PlaylistExport::Playlist(playlist.playlist_key),
        &playlist.name,
    );
    let playlist_key = playlist.playlist_key;
    if playlist.metadata_writable {
        let edit_menus = Rc::clone(menus);
        let playlist_name = playlist.name.clone();
        surface.append_fixed_action(msgid("Edit"), EDIT_ICON, move || {
            (edit_menus.edit_playlist_dialog)(playlist_key, playlist_name.clone());
        });
    }
    if playlist.writable {
        let add_menus = Rc::clone(menus);
        surface.append_fixed_action_enabled(
            msgid("Add current"),
            ADD_ICON,
            current.is_some(),
            move || {
                if let Some(media_uri) = current.as_ref() {
                    crate::widgets::playlists::add_media_to_playlist(
                        &add_menus.source,
                        playlist_key,
                        library::MemberSelection::Uris((vec![media_uri.clone()]).into()),
                        false,
                        Rc::new(|_| {}),
                    );
                }
            },
        );
    }
    crate::menus::playlist_folders::append_folder_actions(
        &surface,
        menus,
        vec![library::PlaylistFolderMember::Playlist {
            key: playlist.playlist_key,
        }],
        in_folder,
    );
    if rufin_core::playlists::can_remove_unavailable(&menus.source, &playlist) {
        let cleanup_menus = Rc::downgrade(menus);
        let key = playlist.playlist_key;
        let name = playlist.name.clone();
        let artwork = crate::widgets::library_fields::playlist_artwork(&playlist);
        surface.append_fixed_action(msgid("Remove unavailable tracks"), TRASH_ICON, move || {
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
        let delete_menus = Rc::clone(menus);
        let delete_target = target.downgrade();
        let name = playlist.name.clone();
        surface.append_fixed_action(msgid("Delete"), DELETE_ICON, move || {
            let Some(target) = delete_target.upgrade() else {
                return;
            };
            let menus = Rc::clone(&delete_menus);
            let dialog = crate::widgets::playlists::delete_playlist_dialog(&name, move || {
                let source = menus.source.clone();
                gtk::glib::spawn_future_local(async move {
                    let _ = rufin_core::playlists::delete_playlist(&source, playlist_key).await;
                });
                (menus.navigate)(Route::Playlists);
            });
            dialog.present(Some(&target));
        });
    }
    install_download_actions(
        &surface,
        menus,
        &crate::widgets::media_drag::MediaDragSource::capture_target(playback),
        None,
        false,
        playlist.track_count > 0 && playlist.downloaded_count == playlist.track_count,
    );
    surface.popup(&menus.settings.current.borrow().application.context_menu);
}

pub fn present_smart_playlist_context_menu(
    target: &gtk::Widget,
    menus: &Rc<MediaMenus>,
    playlist: SmartPlaylistRow,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
) {
    let in_folder = menus
        .library
        .playlist_member_folder(&library::PlaylistFolderMember::SmartPlaylist {
            key: playlist.smart_playlist_key,
        })
        .is_some();
    if present_playlist_selection_menu(
        target,
        menus,
        crate::widgets::selection::smart_play_request(playlist.smart_playlist_key),
        position,
        in_folder,
    ) {
        return;
    }
    let surface = ContextMenuSurface::new(target, "smart-playlist", position);
    let playback = crate::widgets::selection::smart_play_request(playlist.smart_playlist_key);
    append_loaded_play(&surface, menus, playback.clone(), play);
    install_sidebar_pin_action(&surface, menus, Some(playlist.pin()));
    append_export_action(
        &surface,
        menus,
        rufin_core::runtime::source::PlaylistExport::Smart(playlist.smart_playlist_key),
        &playlist.name,
    );
    let edit_menus = Rc::clone(menus);
    let edit_row = playlist.clone();
    surface.append_fixed_action(msgid("Edit"), EDIT_ICON, move || {
        (edit_menus.edit_smart_playlist_dialog)(edit_row.clone());
    });
    crate::menus::playlist_folders::append_folder_actions(
        &surface,
        menus,
        vec![library::PlaylistFolderMember::SmartPlaylist {
            key: playlist.smart_playlist_key,
        }],
        in_folder,
    );
    let delete_menus = Rc::clone(menus);
    let key = playlist.smart_playlist_key;
    let delete_target = target.downgrade();
    let name = playlist.name.clone();
    surface.append_fixed_action(msgid("Delete"), DELETE_ICON, move || {
        let Some(target) = delete_target.upgrade() else {
            return;
        };
        let menus = Rc::clone(&delete_menus);
        let dialog = crate::widgets::playlists::delete_playlist_dialog(&name, move || {
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
    install_download_actions(
        &surface,
        menus,
        &crate::widgets::media_drag::MediaDragSource::capture_target(playback),
        None,
        false,
        playlist.track_count > 0 && playlist.downloaded_count == playlist.track_count,
    );
    surface.popup(&menus.settings.current.borrow().application.context_menu);
}

fn append_export_action(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
    export: rufin_core::runtime::source::PlaylistExport,
    name: &str,
) {
    let menus = Rc::clone(menus);
    let name = name.to_owned();
    surface.append_fixed_action(
        msgid("Export playlist"),
        "rufin-document-send-symbolic",
        move || (menus.export_playlist_dialog)(export, &name),
    );
}

fn append_metadata_action(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
    item: MetadataItemId,
) {
    let menus = Rc::clone(menus);
    surface.append_configurable_action(
        ContextMenuItem::EditMetadata,
        msgid("Edit metadata"),
        EDIT_ICON,
        move || (menus.edit_metadata)(item.clone()),
    );
}

pub(crate) fn install_sidebar_pin_action(
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
    let menus = Rc::clone(menus);
    surface.append_configurable_action(
        ContextMenuItem::Pins,
        if pinned {
            msgid("Remove from Pins")
        } else {
            msgid("Add to Pins")
        },
        if pinned { REMOVE_ICON } else { ADD_ICON },
        move || (menus.set_sidebar_pin)(pin.clone(), !pinned),
    );
}

/// The Play submenu of a collection: `play` where the page plays it, else its play request.
fn append_loaded_play(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
    target: PlayRequest,
    play: Option<CollectionPlay>,
) {
    let play_menus = Rc::clone(menus);
    surface.append_play_actions(
        &menus.settings.current.borrow().application.context_menu,
        move |placement, shuffled| match &play {
            Some(play) => play(placement, shuffled),
            None => (play_menus.play_target)(&target, placement, shuffled),
        },
    );
}

pub fn install_download_actions(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
    payload: &crate::widgets::media_drag::MediaDragSource,
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
    let menus = Rc::clone(menus);
    let payload = payload.clone();
    if downloaded {
        surface.append_configurable_action(
            ContextMenuItem::Download,
            if single {
                msgid("Remove Download")
            } else {
                msgid("Remove Downloads")
            },
            TRASH_ICON,
            move || (menus.remove_download)(&payload),
        );
    } else {
        surface.append_configurable_action(
            ContextMenuItem::Download,
            msgid("Download"),
            DOWNLOAD_ICON,
            move || (menus.download)(&payload),
        );
    }
}

fn append_radio(surface: &ContextMenuSurface, menus: &MediaMenus, label: &str, seed: RadioSeed) {
    let radio = menus.playback.clone();
    surface.append_radio(label, move |placement| {
        radio.play_radio(RadioPlayRequest {
            placement,
            seed: seed.clone(),
        })
    });
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
        crate::widgets::media_drag::MediaDragSource::playlist_entries(selection),
    );
}

fn append_context_menu_picker_selection(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
    selection: TrackSelectionSnapshot,
) {
    (menus.picker_payload)(
        surface,
        crate::widgets::media_drag::MediaDragSource::selection(selection),
    );
}

fn append_context_menu_picker_media(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
    media_uri: String,
) {
    (menus.picker_payload)(
        surface,
        crate::widgets::media_drag::MediaDragSource::media_uris([media_uri]),
    );
}

fn append_context_menu_picker(
    surface: &ContextMenuSurface,
    menus: &Rc<MediaMenus>,
    target: PlayRequest,
) {
    (menus.picker_target)(surface, target);
}
