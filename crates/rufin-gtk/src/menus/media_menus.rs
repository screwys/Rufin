use std::{rc::Rc, sync::Arc};

use crate::shell::shell::Shell;
use crate::widgets::media_drag::MediaDragSource;

use adw::prelude::*;
use gtk::glib;
use library::{
    AlbumRow, ArtistRow, FavoriteTarget, PlaylistRow, RadioSeed, SmartPlaylistRow, TagRow,
    TrackLinks, TrackRow, TrackView,
};
use rufin_core::collection_context::CollectionTarget;
use rufin_core::playback::{QueuePlacement, RadioPlayRequest};

use crate::downloads::{OperationArtwork, OperationFeedback, OperationFeedbackKind};
use crate::ratings::context_rating_row;
use crate::widgets::controls::{ADD_ICON, DELETE_ICON, EDIT_ICON, REMOVE_ICON, TRASH_ICON};
use crate::widgets::interactions::{
    ALBUM_ICON, ARTIST_ICON, ContextMenuSurface, DOWNLOAD_ICON, GO_TO_ICON,
};
use localization::msgid;
use rufin_core::metadata::{MediaDetailsItem, MetadataItemId};
use rufin_core::playlists::SmartPlaylistChange;
use rufin_core::settings::ContextMenuItem;
use rufin_core::settings::SidebarPin;

use crate::widgets::selection::collection_play_request;
use rufin_core::playback::PlayRequest;
pub type CollectionPlay = Rc<dyn Fn(QueuePlacement, bool)>;

use crate::widgets::selection::{PlaylistEntrySelectionSnapshot, TrackSelectionSnapshot};
use rufin_core::route::Route;

pub fn add_drag_to_playlist(
    shell: &Rc<Shell>,
    playlist_key: library::PlaylistKey,
    playlist_name: String,
    source: crate::widgets::media_drag::MediaDragSource,
) -> bool {
    let catalog = Arc::clone(&shell.products.library);
    let task = shell
        .products
        .runtime
        .spawn(async move { source.playlist_input(&catalog).await });
    let shell = Rc::downgrade(shell);
    glib::spawn_future_local(async move {
        let Some((selection, title, preview_uris)) = task.await.ok().and_then(Result::ok) else {
            return;
        };
        let Some(owner) = shell.upgrade() else { return };
        crate::playlists::picker::add_media_to_playlist(
            &owner.products.source,
            playlist_key,
            selection,
            false,
            Rc::new(move |accepted| {
                if accepted > 0
                    && let Some(shell) = shell.upgrade()
                {
                    shell.show_operation_feedback(&OperationFeedback {
                        title: title.clone(),
                        artwork: OperationArtwork::MediaUris(preview_uris.clone()),
                        item_count: accepted,
                        kind: OperationFeedbackKind::PlaylistAdded {
                            destination: playlist_name.clone(),
                        },
                    });
                }
            }),
        );
    });
    true
}

/// Plays `target` at `placement`, shuffled when asked.
pub(crate) fn play_target(
    playback: &rufin_core::playback::PlaybackOwner,
    target: &PlayRequest,
    placement: QueuePlacement,
    shuffled: bool,
) {
    let mut request = target.clone().shuffled(shuffled);
    request.placement = placement;
    playback.play_selection(request);
}

pub(crate) fn download(shell: &Shell, payload: &crate::widgets::media_drag::MediaDragSource) {
    let payload = payload.clone();
    let catalog = shell.products.library.clone();
    let source = shell.products.source.clone();
    shell.products.runtime.spawn(async move {
        match payload.resolve(&catalog).await {
            Ok((uris, subject)) => source.download_media(subject, uris),
            Err(error) => tracing::warn!(%error, "failed to identify download tracks"),
        }
    });
}

fn remove_download(shell: &Shell, payload: &crate::widgets::media_drag::MediaDragSource) {
    let payload = payload.clone();
    let catalog = shell.products.library.clone();
    let source = shell.products.source.clone();
    shell.products.runtime.spawn(async move {
        match payload.resolve(&catalog).await {
            Ok((uris, _)) => source.remove_download_media(uris),
            Err(error) => tracing::warn!(%error, "failed to identify downloaded tracks"),
        }
    });
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
    shell: &Rc<Shell>,
    track: TrackView,
    position: Option<(f64, f64)>,
    popover_position: Option<gtk::PositionType>,
    queue_entry: Option<(usize, Option<u64>)>,
) {
    if queue_entry.is_none()
        && present_collection_selection_menu(
            target,
            shell,
            PlayRequest::track(track.track_key, QueuePlacement::Now),
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
        shell,
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
        append_queue_entry_actions(&surface, shell, track.track_key, position, occurrence_id);
    } else {
        append_loaded_play(
            &surface,
            shell,
            PlayRequest::track(track.track_key, QueuePlacement::Now),
            None,
        );
    }
    append_radio(
        &surface,
        shell,
        msgid("Track radio"),
        RadioSeed::Track(track.media_uri.clone()),
    );
    crate::playlists::picker::append_to_menu(
        &surface,
        shell,
        MediaDragSource::capture_target(PlayRequest::track(track.track_key, QueuePlacement::Now)),
    );
    if track.access.cue_path.is_none()
        && (track.source_key.is_some() || track.media_uri.starts_with("file:"))
    {
        let metadata_shell = Rc::clone(shell);
        let media_uri = track.media_uri.clone();
        surface.append_configurable_action(
            ContextMenuItem::EditMetadata,
            msgid("Edit metadata"),
            EDIT_ICON,
            move || {
                crate::editors::metadata::present_metadata_dialog(
                    &metadata_shell,
                    MetadataItemId::Track(media_uri.clone()),
                )
            },
        );
    }
    install_track_navigation(
        &surface,
        shell,
        &track.links,
        &track.media_uri,
        track.source_id.as_deref(),
    );
    let playback = PlayRequest::track(track.track_key, QueuePlacement::Now);
    install_download_actions(
        &surface,
        shell,
        &crate::widgets::media_drag::MediaDragSource::capture_target(playback),
        Some(&track.media_uri),
        true,
        track.access.is_downloaded,
    );
    if context_menu_rating_visible(shell) {
        let shell = Rc::clone(shell);
        let media_uri = track.media_uri.clone();
        surface.append_fixed_widget(
            "rating",
            &context_rating_row(
                track
                    .state
                    .rating
                    .and_then(|value| u8::try_from(value).ok()),
                shell.half_stars_enabled(&track.media_uri, track.source_id.as_deref()),
                surface.popover(),
                move |rating| {
                    shell
                        .products
                        .source
                        .set_rating(FavoriteTarget::Track(media_uri.clone()), rating)
                },
            ),
        );
    }
    if let Some(position) = popover_position {
        surface.popover().set_position(position);
    }
    surface.popup(&shell.settings.current.borrow().application.context_menu);
}

/// A queue entry's Remove from Queue and Play actions: unshuffled plays move the entry; shuffled
/// ones play its track.
fn append_queue_entry_actions(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    track: library::TrackKey,
    position: usize,
    occurrence_id: Option<u64>,
) {
    let selection = match occurrence_id {
        Some(id) => rufin_core::playback::QueueSelection::Occurrences(vec![id]),
        None => rufin_core::playback::QueueSelection::Positions(vec![position]),
    };
    let remove = selection.clone();
    let queue = shell.products.playback.clone();
    surface.append_fixed_action(msgid("Remove from Queue"), REMOVE_ICON, move || {
        queue.remove_many(remove.clone())
    });
    let queue = shell.products.playback.clone();
    let play_shell = Rc::clone(shell);
    surface.append_play_actions(
        &shell.settings.current.borrow().application.context_menu,
        move |placement, shuffled| match (placement, occurrence_id) {
            _ if shuffled => play_target(
                &play_shell.products.playback,
                &PlayRequest::track(track, QueuePlacement::Now),
                placement,
                true,
            ),
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
    shell: &Rc<Shell>,
    links: &TrackLinks,
    media_uri: &str,
    source_id: Option<&str>,
) {
    let album_media_uri = links.album_media_uri.as_deref();
    let (artists, album_artist) = rufin_core::route::detail_links::go_to_artists(links);
    let submenu = go_to_submenu(
        surface,
        shell,
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
        shell,
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
    shell: &Rc<Shell>,
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
        let shell = Rc::clone(shell);
        submenu.append_item(&surface.item(&label, icon, move || shell.open_route(route.clone())));
    }
    submenu
}

#[allow(clippy::too_many_arguments)]
fn install_source_navigation(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
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
        .and_then(|id| shell.products.source.configuration(id));
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
    let shell = Rc::clone(shell);
    let media_uri = media_uri.to_owned();
    let (title, icon_name) = (localization::tr(link.title), link.icon_name);
    submenu.append_item(&surface.item(&title, icon_name, move || {
        let Some(window) = window.as_ref().and_then(glib::WeakRef::upgrade) else {
            return;
        };
        let source = shell.products.source.clone();
        let folder = source
            .selected_library()
            .filter(|selected| source_id.as_ref() == Some(&selected.source_id))
            .and_then(|selected| selected.music_folder_key);
        let media_uri = media_uri.clone();
        let url = link.url.clone();
        let feedback = Rc::clone(&shell.control_feedback);
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
                feedback.show_feedback_toast(error);
            }
        });
    }));
}

pub fn present_playlist_entry_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    row: TrackRow,
    position: Option<(f64, f64)>,
    _playlist: library::PlaylistKey,
    removal: Option<PlaylistEntrySelectionSnapshot>,
) {
    let surface = ContextMenuSurface::new(target, "playlist-entry", position);
    crate::menus::media_details::append_header(
        &surface,
        target,
        shell,
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
    let queue = shell.products.playback.clone();
    let media_uri = row.media_uri.clone();
    surface.append_play_actions(
        &shell.settings.current.borrow().application.context_menu,
        move |placement, shuffled| {
            queue.play_selection(
                PlayRequest::uris(vec![media_uri.clone()], placement).shuffled(shuffled),
            )
        },
    );
    if let Some(removal) = removal.filter(|selection| selection.writable) {
        let remove_shell = Rc::downgrade(shell);
        surface.append_fixed_action(msgid("Remove from Playlist"), REMOVE_ICON, move || {
            if let Some(shell) = remove_shell.upgrade() {
                remove_playlist_entry_selection(&shell, removal.clone());
            }
        });
    }
    install_track_navigation(
        &surface,
        shell,
        &row.links,
        &row.media_uri,
        row.source_id.as_deref(),
    );
    install_download_actions(
        &surface,
        shell,
        &crate::widgets::media_drag::MediaDragSource::capture_target(PlayRequest::track(
            row.track_key,
            QueuePlacement::Now,
        )),
        Some(&row.media_uri),
        true,
        row.is_downloaded,
    );
    if row.media_uri.starts_with("file:") || library::source_entity_parts(&row.media_uri).is_some()
    {
        let metadata_shell = Rc::clone(shell);
        let media_uri = row.media_uri.clone();
        surface.append_configurable_action(
            ContextMenuItem::EditMetadata,
            msgid("Edit metadata"),
            EDIT_ICON,
            move || {
                crate::editors::metadata::present_metadata_dialog(
                    &metadata_shell,
                    MetadataItemId::Track(media_uri.clone()),
                )
            },
        );
    }
    crate::playlists::picker::append_to_menu(
        &surface,
        shell,
        MediaDragSource::media_uris([row.media_uri.clone()]),
    );
    if context_menu_rating_visible(shell) {
        let rating_shell = Rc::clone(shell);
        let media_uri = row.media_uri.clone();
        surface.append_fixed_widget(
            "rating",
            &context_rating_row(
                row.state.rating.and_then(|value| u8::try_from(value).ok()),
                rating_shell.half_stars_enabled(&row.media_uri, row.source_id.as_deref()),
                surface.popover(),
                move |rating| {
                    rating_shell
                        .products
                        .source
                        .set_rating(FavoriteTarget::Track(media_uri.clone()), rating)
                },
            ),
        );
    }
    surface.popup(&shell.settings.current.borrow().application.context_menu);
}

pub fn present_track_selection_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    selection: TrackSelectionSnapshot,
    position: Option<(f64, f64)>,
) {
    let surface = ContextMenuSurface::new(target, "track-selection", position);
    append_track_selection_actions(&surface, shell, selection);
    surface.popup(&shell.settings.current.borrow().application.context_menu);
}

pub fn present_collection_selection_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    clicked: PlayRequest,
    position: Option<(f64, f64)>,
) -> bool {
    let Some((surface, _)) = collection_selection_surface(target, shell, clicked, position) else {
        return false;
    };
    surface.popup(&shell.settings.current.borrow().application.context_menu);
    true
}

fn collection_selection_surface(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
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
    let metadata_shell = Rc::clone(shell);
    let edit_metadata = move || {
        let targets = metadata_targets.clone();
        let shell = Rc::clone(&metadata_shell);
        let catalog = shell.products.library.clone();
        let task = shell.products.runtime.spawn(async move {
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
                Ok(Ok(selection)) => {
                    crate::editors::metadata::present_track_metadata_dialog(&shell, selection)
                }
                Ok(Err(error)) => shell.control_feedback.show_feedback_toast(error),
                Err(error) => shell
                    .control_feedback
                    .show_feedback_toast(error.to_string()),
            }
        });
    };
    surface.append_configurable_action(
        ContextMenuItem::EditMetadata,
        msgid("Edit metadata"),
        EDIT_ICON,
        edit_metadata,
    );
    let play_shell = Rc::clone(shell);
    let play_payload = payload.clone();
    surface.append_play_actions(
        &shell.settings.current.borrow().application.context_menu,
        move |placement, shuffled| {
            let payload = play_payload.clone();
            let catalog = play_shell.products.library.clone();
            let task = play_shell
                .products
                .runtime
                .spawn(async move { payload.play_request(&catalog).await });
            let shell = Rc::clone(&play_shell);
            glib::spawn_future_local(async move {
                match task.await {
                    Ok(Ok(request)) => {
                        play_target(&shell.products.playback, &request, placement, shuffled)
                    }
                    Ok(Err(error)) => shell.control_feedback.show_feedback_toast(error),
                    Err(error) => shell
                        .control_feedback
                        .show_feedback_toast(error.to_string()),
                }
            });
        },
    );
    crate::playlists::picker::append_to_menu(&surface, shell, payload.clone());
    install_selection_download_actions(&surface, shell, payload);
    Some((surface, targets))
}

fn present_playlist_selection_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    clicked: PlayRequest,
    position: Option<(f64, f64)>,
    in_folder: bool,
) -> bool {
    let Some((surface, targets)) = collection_selection_surface(target, shell, clicked, position)
    else {
        return false;
    };
    let folder_targets = targets.clone();
    let delete_shell = Rc::clone(shell);
    let delete_target = target.downgrade();
    let delete = move || {
        let Some(target) = delete_target.upgrade() else {
            return;
        };
        let shell = Rc::clone(&delete_shell);
        let targets = targets.clone();
        let dialog = crate::playlists::delete_playlists_dialog(targets.count(), move || {
            let targets = targets.clone();
            let source = shell.products.source.clone();
            let catalog = Arc::clone(&shell.products.library);
            let task = shell.products.runtime.spawn(async move {
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
            let shell = Rc::clone(&shell);
            glib::spawn_future_local(async move {
                match task.await {
                    Ok(Ok((smart, failure))) => {
                        for key in smart {
                            shell.publish_smart_playlist_change(
                                SmartPlaylistChange::Delete(key),
                                None,
                                None,
                            );
                        }
                        if let Some(error) = failure {
                            shell.control_feedback.show_feedback_toast(error);
                        }
                    }
                    Ok(Err(error)) => shell.control_feedback.show_feedback_toast(error),
                    Err(error) => shell
                        .control_feedback
                        .show_feedback_toast(error.to_string()),
                }
            });
        });
        dialog.present(Some(&target));
    };
    let catalog = shell.products.library.clone();
    let task = shell
        .products
        .runtime
        .spawn(async move { folder_targets.collections(&catalog).await });
    let shell = Rc::clone(shell);
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
                        &surface, &shell, members, in_folder,
                    );
                }
                surface.append_fixed_action(msgid("Delete"), DELETE_ICON, delete);
                surface.popup(&shell.settings.current.borrow().application.context_menu);
            }
            Ok(Err(error)) => shell.control_feedback.show_feedback_toast(error),
            Err(error) => shell
                .control_feedback
                .show_feedback_toast(error.to_string()),
        }
    });
    true
}

pub fn present_playlist_entry_selection_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    selection: PlaylistEntrySelectionSnapshot,
    position: Option<(f64, f64)>,
) {
    let surface = ContextMenuSurface::new(target, "playlist-entry-selection", position);
    if selection.writable {
        let remove_shell = Rc::clone(shell);
        let removal = selection.clone();
        surface.append_fixed_action(msgid("Remove from Playlist"), REMOVE_ICON, move || {
            remove_playlist_entry_selection(&remove_shell, removal.clone());
        });
    }
    append_playlist_entry_selection_actions(&surface, shell, selection);
    surface.popup(&shell.settings.current.borrow().application.context_menu);
}

pub fn remove_playlist_entry_selection(
    shell: &Rc<Shell>,
    selection: PlaylistEntrySelectionSnapshot,
) -> bool {
    if !selection.writable || selection.count == 0 {
        return false;
    }
    let result = rufin_core::playlists::remove_playlist_entries(
        &shell.products.source,
        selection.playlist,
        selection.input,
    );
    let shell = Rc::downgrade(shell);
    gtk::glib::spawn_future_local(async move {
        let Ok(Ok(tracks)) = result.recv().await else {
            return;
        };
        if tracks.is_empty() {
            return;
        }
        let Some(shell) = shell.upgrade() else {
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
        let catalog = shell.products.library.clone();
        let preview = songs.clone();
        let artwork = shell
            .products
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
        let source = shell.products.source.clone();
        shell.show_undoable_operation_feedback(&feedback, move || {
            crate::playlists::picker::add_media_to_playlist(
                &source,
                selection.playlist,
                songs.clone(),
                false,
                Rc::new(|_| {}),
            );
        });
    });
    true
}

fn append_playlist_entry_selection_actions(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    selection: PlaylistEntrySelectionSnapshot,
) {
    if selection.count == 0 {
        return;
    }
    let play_shell = Rc::clone(shell);
    let play_selection = selection.clone();
    surface.append_play_actions(
        &shell.settings.current.borrow().application.context_menu,
        move |placement, shuffled| {
            play_selection.play(&play_shell.products.playback, placement, shuffled)
        },
    );
    crate::playlists::picker::append_to_menu(
        surface,
        shell,
        MediaDragSource::playlist_entries(selection.clone()),
    );
    append_metadata_selection_action(surface, shell, selection.input.clone());
    let download_shell = Rc::clone(shell);
    let download_payload = crate::widgets::media_drag::MediaDragSource::playlist_entries(selection);
    surface.append_configurable_action(
        ContextMenuItem::Download,
        msgid("Download"),
        DOWNLOAD_ICON,
        move || download(&download_shell, &download_payload),
    );
}

fn append_track_selection_actions(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    selection: TrackSelectionSnapshot,
) {
    if selection.count == 0 {
        return;
    }
    let play_shell = Rc::clone(shell);
    let play_selection = selection.clone();
    surface.append_play_actions(
        &shell.settings.current.borrow().application.context_menu,
        move |placement, shuffled| {
            play_selection.play(&play_shell.products.playback, placement, shuffled)
        },
    );
    crate::playlists::picker::append_to_menu(
        surface,
        shell,
        MediaDragSource::selection(selection.clone()),
    );
    append_metadata_selection_action(surface, shell, selection.input.clone());
    install_selection_download_actions(
        surface,
        shell,
        crate::widgets::media_drag::MediaDragSource::selection(selection),
    );
}

fn append_metadata_selection_action(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    selection: library::MemberSelection,
) {
    let shell = Rc::clone(shell);
    surface.append_configurable_action(
        ContextMenuItem::EditMetadata,
        msgid("Edit metadata"),
        EDIT_ICON,
        move || crate::editors::metadata::present_track_metadata_dialog(&shell, selection.clone()),
    );
}

fn install_selection_download_actions(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    payload: crate::widgets::media_drag::MediaDragSource,
) {
    let download_shell = Rc::clone(shell);
    let download_payload = payload.clone();
    surface.append_configurable_action(
        ContextMenuItem::Download,
        msgid("Download"),
        DOWNLOAD_ICON,
        move || download(&download_shell, &download_payload),
    );
    let remove_shell = Rc::clone(shell);
    surface.append_configurable_action(
        ContextMenuItem::Download,
        msgid("Remove Downloads"),
        TRASH_ICON,
        move || remove_download(&remove_shell, &payload),
    );
}

pub fn present_album_context_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    album: AlbumRow,
    playback_context: Option<String>,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
) {
    let base = crate::widgets::selection::album_play_request(&album);
    let playback = playback_context
        .map(|context| base.clone().with_context(context.into()))
        .unwrap_or(base);
    if present_collection_selection_menu(target, shell, playback.clone(), position) {
        return;
    }
    let surface = ContextMenuSurface::new(target, "album", position);
    crate::menus::media_details::append_header(
        &surface,
        target,
        shell,
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
    append_loaded_play(&surface, shell, playback, play);
    append_radio(
        &surface,
        shell,
        msgid("Album radio"),
        RadioSeed::Album(album.album_key),
    );
    crate::playlists::picker::append_to_menu(
        &surface,
        shell,
        MediaDragSource::capture_target(crate::widgets::selection::album_play_request(&album)),
    );
    append_metadata_action(
        &surface,
        shell,
        MetadataItemId::Album(album.media_uri.clone()),
    );
    install_sidebar_pin_action(&surface, shell, album.pin());
    let submenu = go_to_submenu(
        &surface,
        shell,
        album.links.album_artists.iter().map(|artist| {
            let route = Route::AlbumArtistDetail(artist.media_uri.clone());
            (artist.name.as_str(), route)
        }),
        Some(Route::AlbumDetail(album.media_uri.clone())),
    );
    install_source_navigation(
        &surface,
        shell,
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
        shell,
        &crate::widgets::media_drag::MediaDragSource::capture_target(
            crate::widgets::selection::album_play_request(&album),
        ),
        Some(&album.media_uri),
        false,
        album.summary.downloaded(),
    );
    if context_menu_rating_visible(shell) {
        let shell = Rc::clone(shell);
        let media_uri = album.media_uri.clone();
        surface.append_fixed_widget(
            "rating",
            &context_rating_row(
                album
                    .state
                    .rating
                    .and_then(|value| u8::try_from(value).ok()),
                shell.half_stars_enabled(&album.media_uri, None),
                surface.popover(),
                move |rating| {
                    shell
                        .products
                        .source
                        .set_rating(FavoriteTarget::Album(media_uri.clone()), rating)
                },
            ),
        );
    }
    surface.popup(&shell.settings.current.borrow().application.context_menu);
}

pub fn present_artist_context_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    artist: ArtistRow,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
) {
    if present_collection_selection_menu(
        target,
        shell,
        crate::widgets::selection::artist_play_request(&artist, artist.album_artist),
        position,
    ) {
        return;
    }
    let surface = ContextMenuSurface::new(target, "artist", position);
    crate::menus::media_details::append_header(
        &surface,
        target,
        shell,
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
    append_loaded_play(&surface, shell, playback.clone(), play);
    append_radio(
        &surface,
        shell,
        msgid("Artist radio"),
        if artist.album_artist {
            RadioSeed::AlbumArtist(artist.artist_key)
        } else {
            RadioSeed::Artist(artist.artist_key)
        },
    );
    crate::playlists::picker::append_to_menu(
        &surface,
        shell,
        MediaDragSource::capture_target(playback.clone()),
    );
    append_metadata_action(
        &surface,
        shell,
        MetadataItemId::Artist(artist.media_uri.clone()),
    );
    install_sidebar_pin_action(&surface, shell, artist.pin());
    let submenu = go_to_submenu(
        &surface,
        shell,
        [(artist.name.as_str(), Route::of_artist(&artist))],
        None,
    );
    install_source_navigation(
        &surface,
        shell,
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
        shell,
        &crate::widgets::media_drag::MediaDragSource::capture_target(playback),
        Some(&artist.media_uri),
        false,
        artist.summary.downloaded(),
    );
    if context_menu_rating_visible(shell) {
        let shell = Rc::clone(shell);
        let media_uri = artist.media_uri.clone();
        surface.append_fixed_widget(
            "rating",
            &context_rating_row(
                artist
                    .state
                    .rating
                    .and_then(|value| u8::try_from(value).ok()),
                shell.half_stars_enabled(&artist.media_uri, None),
                surface.popover(),
                move |rating| {
                    shell
                        .products
                        .source
                        .set_rating(FavoriteTarget::Artist(media_uri.clone()), rating)
                },
            ),
        );
    }
    surface.popup(&shell.settings.current.borrow().application.context_menu);
}

pub fn present_genre_context_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    genre: TagRow,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
) {
    let key = library::GenreKey::from_raw(genre.key);
    let playback = collection_play_request(
        library::Collection::Genre(key),
        &CollectionTarget::Genre(key),
    );
    if present_collection_selection_menu(target, shell, playback.clone(), position) {
        return;
    }
    let surface = ContextMenuSurface::new(target, "genre", position);
    append_loaded_play(&surface, shell, playback.clone(), play);
    append_radio(&surface, shell, msgid("Genre radio"), RadioSeed::Genre(key));
    install_sidebar_pin_action(&surface, shell, genre.pin());
    install_download_actions(
        &surface,
        shell,
        &crate::widgets::media_drag::MediaDragSource::capture_target(playback),
        None,
        false,
        genre.summary.downloaded(),
    );
    surface.popup(&shell.settings.current.borrow().application.context_menu);
}

pub fn present_mood_context_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    mood: TagRow,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
) {
    let key = library::MoodKey::from_raw(mood.key);
    let playback =
        collection_play_request(library::Collection::Mood(key), &CollectionTarget::Mood(key));
    if present_collection_selection_menu(target, shell, playback.clone(), position) {
        return;
    }
    let surface = ContextMenuSurface::new(target, "mood", position);
    append_loaded_play(&surface, shell, playback.clone(), play);
    install_download_actions(
        &surface,
        shell,
        &crate::widgets::media_drag::MediaDragSource::capture_target(playback.clone()),
        None,
        false,
        mood.summary.downloaded(),
    );
    crate::playlists::picker::append_to_menu(
        &surface,
        shell,
        MediaDragSource::capture_target(playback),
    );
    surface.popup(&shell.settings.current.borrow().application.context_menu);
}

pub fn present_playlist_context_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    playlist: PlaylistRow,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
) {
    let current = crate::player::state::current_playback_track(
        shell.player_ui.selected_playback().as_deref(),
    )
    .map(|track| track.media_uri);
    let in_folder = shell
        .products
        .library
        .playlist_member_folder(&library::PlaylistFolderMember::Playlist {
            key: playlist.playlist_key,
        })
        .is_some();
    let playback = collection_play_request(
        library::Collection::Playlist(playlist.playlist_key),
        &CollectionTarget::Playlist(playlist.playlist_key),
    );
    if present_playlist_selection_menu(target, shell, playback.clone(), position, in_folder) {
        return;
    }
    let surface = ContextMenuSurface::new(target, "playlist", position);
    append_loaded_play(&surface, shell, playback.clone(), play);
    append_radio(
        &surface,
        shell,
        msgid("Playlist radio"),
        RadioSeed::Playlist(playlist.playlist_key),
    );
    install_sidebar_pin_action(&surface, shell, Some(playlist.pin()));
    append_export_action(
        &surface,
        shell,
        rufin_core::runtime::source::PlaylistExport::Playlist(playlist.playlist_key),
        &playlist.name,
    );
    let playlist_key = playlist.playlist_key;
    if playlist.metadata_writable {
        let edit_shell = Rc::clone(shell);
        let playlist_name = playlist.name.clone();
        surface.append_fixed_action(msgid("Edit"), EDIT_ICON, move || {
            edit_shell.edit_playlist_dialog(playlist_key, playlist_name.clone());
        });
    }
    if playlist.writable {
        let add_shell = Rc::clone(shell);
        surface.append_fixed_action_enabled(
            msgid("Add current"),
            ADD_ICON,
            current.is_some(),
            move || {
                if let Some(media_uri) = current.as_ref() {
                    crate::playlists::picker::add_media_to_playlist(
                        &add_shell.products.source,
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
        shell,
        vec![library::PlaylistFolderMember::Playlist {
            key: playlist.playlist_key,
        }],
        in_folder,
    );
    if rufin_core::playlists::can_remove_unavailable(&shell.products.source, &playlist) {
        let cleanup_shell = Rc::downgrade(shell);
        let key = playlist.playlist_key;
        let name = playlist.name.clone();
        let artwork = crate::widgets::library_fields::playlist_artwork(&playlist);
        surface.append_fixed_action(msgid("Remove unavailable tracks"), TRASH_ICON, move || {
            let Some(shell) = cleanup_shell.upgrade() else {
                return;
            };
            let result =
                rufin_core::playlists::remove_unavailable_tracks(&shell.products.source, key);
            let shell = Rc::downgrade(&shell);
            let name = name.clone();
            let artwork = artwork.clone();
            gtk::glib::spawn_future_local(async move {
                let Ok(result) = result.recv().await else {
                    return;
                };
                let Some(shell) = shell.upgrade() else {
                    return;
                };
                let removed = match result {
                    Ok(removed) => removed,
                    Err(error) => {
                        shell.control_feedback.show_feedback_toast(error);
                        return;
                    }
                };
                shell.show_operation_feedback(&OperationFeedback {
                    title: name.clone(),
                    artwork: OperationArtwork::Bindings(artwork),
                    item_count: removed,
                    kind: OperationFeedbackKind::PlaylistRemoved { destination: name },
                });
            });
        });
    }
    if playlist.metadata_writable {
        let delete_shell = Rc::clone(shell);
        let delete_target = target.downgrade();
        let name = playlist.name.clone();
        surface.append_fixed_action(msgid("Delete"), DELETE_ICON, move || {
            let Some(target) = delete_target.upgrade() else {
                return;
            };
            let shell = Rc::clone(&delete_shell);
            let dialog = crate::playlists::delete_playlist_dialog(&name, move || {
                let source = shell.products.source.clone();
                gtk::glib::spawn_future_local(async move {
                    let _ = rufin_core::playlists::delete_playlist(&source, playlist_key).await;
                });
                shell.open_route(Route::Playlists);
            });
            dialog.present(Some(&target));
        });
    }
    install_download_actions(
        &surface,
        shell,
        &crate::widgets::media_drag::MediaDragSource::capture_target(playback),
        None,
        false,
        playlist.downloaded(),
    );
    surface.popup(&shell.settings.current.borrow().application.context_menu);
}

pub fn present_smart_playlist_context_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    playlist: SmartPlaylistRow,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
) {
    let in_folder = shell
        .products
        .library
        .playlist_member_folder(&library::PlaylistFolderMember::SmartPlaylist {
            key: playlist.smart_playlist_key,
        })
        .is_some();
    if present_playlist_selection_menu(
        target,
        shell,
        crate::widgets::selection::smart_play_request(playlist.smart_playlist_key),
        position,
        in_folder,
    ) {
        return;
    }
    let surface = ContextMenuSurface::new(target, "smart-playlist", position);
    let playback = crate::widgets::selection::smart_play_request(playlist.smart_playlist_key);
    append_loaded_play(&surface, shell, playback.clone(), play);
    install_sidebar_pin_action(&surface, shell, Some(playlist.pin()));
    append_export_action(
        &surface,
        shell,
        rufin_core::runtime::source::PlaylistExport::Smart(playlist.smart_playlist_key),
        &playlist.name,
    );
    let edit_shell = Rc::clone(shell);
    let edit_row = playlist.clone();
    surface.append_fixed_action(msgid("Edit"), EDIT_ICON, move || {
        edit_shell.edit_smart_playlist_dialog(edit_row.clone());
    });
    crate::menus::playlist_folders::append_folder_actions(
        &surface,
        shell,
        vec![library::PlaylistFolderMember::SmartPlaylist {
            key: playlist.smart_playlist_key,
        }],
        in_folder,
    );
    let delete_shell = Rc::clone(shell);
    let key = playlist.smart_playlist_key;
    let delete_target = target.downgrade();
    let name = playlist.name.clone();
    surface.append_fixed_action(msgid("Delete"), DELETE_ICON, move || {
        let Some(target) = delete_target.upgrade() else {
            return;
        };
        let shell = Rc::clone(&delete_shell);
        let dialog = crate::playlists::delete_playlist_dialog(&name, move || {
            let navigate = Rc::clone(&shell);
            shell.publish_smart_playlist_change(
                SmartPlaylistChange::Delete(key),
                None,
                Some(Rc::new(move |result| {
                    if result.is_ok() {
                        navigate.open_route(Route::SmartPlaylists);
                    }
                })),
            );
        });
        dialog.present(Some(&target));
    });
    install_download_actions(
        &surface,
        shell,
        &crate::widgets::media_drag::MediaDragSource::capture_target(playback),
        None,
        false,
        playlist.downloaded(),
    );
    surface.popup(&shell.settings.current.borrow().application.context_menu);
}

fn append_export_action(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    export: rufin_core::runtime::source::PlaylistExport,
    name: &str,
) {
    let shell = Rc::clone(shell);
    let name = name.to_owned();
    surface.append_fixed_action(
        msgid("Export playlist"),
        "rufin-document-send-symbolic",
        move || shell.export_playlist_dialog(export, &name),
    );
}

fn append_metadata_action(surface: &ContextMenuSurface, shell: &Rc<Shell>, item: MetadataItemId) {
    let shell = Rc::clone(shell);
    surface.append_configurable_action(
        ContextMenuItem::EditMetadata,
        msgid("Edit metadata"),
        EDIT_ICON,
        move || crate::editors::metadata::present_metadata_dialog(&shell, item.clone()),
    );
}

pub(crate) fn install_sidebar_pin_action(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    pin: Option<SidebarPin>,
) {
    let Some(pin) = pin else { return };
    let settings = shell.settings.current.borrow();
    if !settings.sidebar.pins_visible {
        return;
    }
    let pinned = settings.sidebar.is_pinned(&pin);
    drop(settings);
    let shell = Rc::clone(shell);
    surface.append_configurable_action(
        ContextMenuItem::Pins,
        if pinned {
            msgid("Remove from Pins")
        } else {
            msgid("Add to Pins")
        },
        if pinned { REMOVE_ICON } else { ADD_ICON },
        move || shell.set_sidebar_pin(pin.clone(), !pinned),
    );
}

/// The Play submenu of a collection: `play` where the page plays it, else its play request.
fn append_loaded_play(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    target: PlayRequest,
    play: Option<CollectionPlay>,
) {
    let play_shell = Rc::clone(shell);
    surface.append_play_actions(
        &shell.settings.current.borrow().application.context_menu,
        move |placement, shuffled| match &play {
            Some(play) => play(placement, shuffled),
            None => play_target(&play_shell.products.playback, &target, placement, shuffled),
        },
    );
}

pub fn install_download_actions(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    payload: &crate::widgets::media_drag::MediaDragSource,
    media_uri: Option<&str>,
    single: bool,
    downloaded: bool,
) {
    let available = media_uri.is_none_or(|uri| {
        if let Some((source_id, _, _)) = library::source_entity_parts(uri) {
            shell.source_download_available(&source_id)
        } else {
            library::normalize_direct_media_uri(uri)
                .is_some_and(|uri| uri.starts_with("https:") || uri.starts_with("http:"))
        }
    });
    if !downloaded && !available {
        return;
    }
    let shell = Rc::clone(shell);
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
            move || remove_download(&shell, &payload),
        );
    } else {
        surface.append_configurable_action(
            ContextMenuItem::Download,
            msgid("Download"),
            DOWNLOAD_ICON,
            move || download(&shell, &payload),
        );
    }
}

fn append_radio(surface: &ContextMenuSurface, shell: &Shell, label: &str, seed: RadioSeed) {
    let radio = shell.products.playback.clone();
    surface.append_radio(label, move |placement| {
        radio.play_radio(RadioPlayRequest {
            placement,
            seed: seed.clone(),
        })
    });
}

fn context_menu_rating_visible(shell: &Shell) -> bool {
    shell
        .settings
        .current
        .borrow()
        .application
        .context_menu
        .rating_visible
}
