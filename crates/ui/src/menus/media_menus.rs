use std::{rc::Rc, sync::Arc};

use crate::shell::shell::Shell;
use crate::widgets::media_drag::MediaDragSource;

use adw::prelude::*;
use gtk::glib;
use library::{FavoriteTarget, PlaylistListRow, RadioSeed, Row, TrackRow, TrackView};
use rufin_core::menu::{GoTo, Menu};
use rufin_core::playback::{
    PlaybackOwner, QueuePlacement, QueueReorderTarget, QueueSelection, RadioPlayRequest,
};

use crate::downloads::{OperationArtwork, OperationFeedback, OperationFeedbackKind};
use crate::ratings::context_rating_row;
use crate::widgets::controls::{ADD_ICON, DELETE_ICON, EDIT_ICON, REMOVE_ICON, TRASH_ICON};
use crate::widgets::interactions::{
    ALBUM_ICON, ARTIST_ICON, ContextMenuSurface, DOWNLOAD_ICON, GO_TO_ICON, SHARE_ICON,
};
use localization::msgid;
use rufin_core::metadata::{MediaDetailsItem, MetadataItemId};
use rufin_core::playlists::SmartPlaylistChange;
use rufin_core::settings::ContextMenuItem;
use rufin_core::settings::SidebarPin;
use rufin_core::sharing::{Share, ShareDialog, ShareExpiry, ShareOptions, ShareTarget};

use crate::widgets::selection::PlayTarget;
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
    let owner = shell.products.source.clone();
    let task = shell
        .products
        .runtime
        .spawn(async move { source.playlist_input(&catalog, &owner).await });
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

/// Plays `target` at `placement`, shuffled when asked: a collection row plays the page it
/// opens, as that page's header does.
pub(crate) fn play_target(
    products: &rufin_core::runtime::ProductHandles,
    target: &PlayTarget,
    placement: QueuePlacement,
    shuffled: bool,
) {
    let target = target.clone();
    let source = products.source.clone();
    let playback = products.playback.clone();
    products.runtime.spawn(async move {
        let selected = source.selected_library();
        let cancel = library::ReadCancellation::new();
        match source
            .play_target(&target, selected.as_ref(), placement, shuffled, &cancel)
            .await
        {
            Ok(request) => playback.play_selection(request),
            Err(error) => tracing::warn!(%error, "could not play the collection"),
        }
    });
}

/// Downloads the tracks of `payload`, or removes their downloads.
pub(crate) fn download(shell: &Shell, payload: &MediaDragSource, remove: bool) {
    let payload = payload.clone();
    let source = shell.products.source.clone();
    shell.products.runtime.spawn(async move {
        let result = async {
            let selection = payload
                .track_selection(&source, rufin_core::view::SelectionScope::Downloads)
                .await?;
            let reply = if remove {
                source.remove_download_selection(selection)
            } else {
                source.download_selection(selection)
            };
            reply.recv().await.map_err(|error| error.to_string())?
        }
        .await;
        if let Err(error) = result {
            tracing::warn!(%error, "could not change the downloads");
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

/// What a row menu adds for where the row is shown.
enum Place {
    List,
    /// A queue entry: its position and occurrence.
    Queue(usize, Option<u64>),
    /// A playlist entry, with the removal its selection allows.
    Entry(Option<PlaylistEntrySelectionSnapshot>),
}

/// The menu of `row`, as core's Menu offers it, or of the selection it belongs to. `play`
/// plays the row where its page does.
pub fn present_row_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    row: Row,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
) {
    let playback = PlayTarget::of_row(&row);
    present_menu(target, shell, row, playback, play, position);
}

/// The menu of `row` on `view`'s page, which plays what [`View::row_target`] gives: a child
/// folder plays the folder it opens.
///
/// [`View::row_target`]: rufin_core::view::View::row_target
pub fn present_view_row_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    view: &rufin_core::view::View,
    row: Row,
    position: Option<(f64, f64)>,
) {
    let playback = view.row_target(&row);
    present_menu(target, shell, row, playback, None, position);
}

fn present_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    row: Row,
    playback: Option<PlayTarget>,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
) {
    let menu = row_menu(shell, &row);
    if let Some(clicked) = playback.clone() {
        let selected = match &row {
            Row::Playlist(_) => present_playlist_selection_menu(
                target,
                shell,
                clicked,
                position,
                menu.folder.unwrap_or(false),
            ),
            _ => present_collection_selection_menu(target, shell, clicked, position),
        };
        if selected {
            return;
        }
    }
    present_row_surface(
        target,
        shell,
        &row,
        playback,
        &menu,
        play,
        position,
        Place::List,
    );
}

/// The menu of the player's or the queue's `track`; `queue_entry` is its queue position and
/// occurrence.
pub fn present_track_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    track: TrackView,
    position: Option<(f64, f64)>,
    popover_position: Option<gtk::PositionType>,
    queue_entry: Option<(usize, Option<u64>)>,
) {
    let row = Row::Track(track.into());
    let menu = row_menu(shell, &row);
    let place = match queue_entry {
        Some((position, occurrence)) => Place::Queue(position, occurrence),
        None => Place::List,
    };
    let playback = PlayTarget::of_row(&row);
    if let Some(surface) = row_surface(target, shell, &row, playback, &menu, None, position, place)
    {
        if let Some(position) = popover_position {
            surface.popover().set_position(position);
        }
        surface.popup();
    }
}

pub fn present_playlist_entry_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    row: TrackRow,
    position: Option<(f64, f64)>,
    removal: Option<PlaylistEntrySelectionSnapshot>,
) {
    let row = Row::Track(row);
    let menu = row_menu(shell, &row);
    present_row_surface(
        target,
        shell,
        &row,
        PlayTarget::of_row(&row),
        &menu,
        None,
        position,
        Place::Entry(removal),
    );
}

fn row_menu(shell: &Shell, row: &Row) -> Menu {
    rufin_core::menu::menu(
        &shell.products.source,
        &shell.settings.current.borrow(),
        row,
    )
}

#[allow(clippy::too_many_arguments)]
fn present_row_surface(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    row: &Row,
    playback: Option<PlayTarget>,
    menu: &Menu,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
    place: Place,
) {
    if let Some(surface) = row_surface(target, shell, row, playback, menu, play, position, place) {
        surface.popup();
    }
}

#[allow(clippy::too_many_arguments)]
fn row_surface(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    row: &Row,
    playback: Option<PlayTarget>,
    menu: &Menu,
    play: Option<CollectionPlay>,
    position: Option<(f64, f64)>,
    place: Place,
) -> Option<ContextMenuSurface> {
    let group = match (&place, row) {
        (Place::Queue(..), _) => "queue",
        (Place::Entry(_), _) => "playlist-entry",
        (_, Row::Track(_) | Row::AlbumTrack { .. }) => "track",
        (_, Row::Album(_) | Row::AlbumHeading(_)) => "album",
        (_, Row::Artist(_)) => "artist",
        (_, Row::Tag(tag)) if tag.kind == library::TagKind::Genre => "genre",
        (_, Row::Tag(_)) => "mood",
        (_, Row::Playlist(PlaylistListRow::Playlist(_))) => "playlist",
        (_, Row::Playlist(PlaylistListRow::SmartPlaylist(_))) => "smart-playlist",
        (_, Row::Playlist(PlaylistListRow::Folder(_))) => "playlist-folder",
        (_, Row::Folder(_)) => "folder",
    };
    let surface = ContextMenuSurface::new(target, group, position);
    let rated = rated(row);
    let header = header(row);
    if let Some((_, title, subtitle, artwork)) = &header {
        crate::menus::media_details::append_header(
            &surface,
            shell,
            crate::menus::media_details::MenuHeader {
                title,
                subtitle,
                artwork: *artwork,
            },
        );
    }
    match (place, &playback) {
        (Place::Queue(position, occurrence), Some(PlayTarget::Track(track))) => {
            append_queue_entry_actions(&surface, shell, menu, *track, position, occurrence);
        }
        (place, Some(playback)) => {
            append_loaded_play(&surface, shell, menu, playback.clone(), play);
            if let Place::Entry(Some(removal)) = place
                && removal.writable
            {
                let remove_shell = Rc::downgrade(shell);
                surface.append_fixed_action(
                    msgid("Remove from Playlist"),
                    REMOVE_ICON,
                    move || {
                        if let Some(shell) = remove_shell.upgrade() {
                            remove_playlist_entry_selection(&shell, removal.clone());
                        }
                    },
                );
            }
        }
        _ => {}
    }
    if let Some(seed) = &menu.radio {
        append_radio(&surface, shell, radio_label(seed), seed.clone());
    }
    let payload = playback.map(MediaDragSource::capture_target);
    if menu.add_to_playlist
        && let Some(payload) = &payload
    {
        crate::playlists::picker::append_to_menu(&surface, shell, payload.clone());
    }
    if menu.favorite
        && let Some(rated) = &rated
    {
        let shell = Rc::clone(shell);
        let (target, favorite) = (rated.target.clone(), rated.favorite);
        surface.append_configurable_action(
            ContextMenuItem::Favorites,
            if favorite {
                msgid("Remove from Favorites")
            } else {
                msgid("Add to Favorites")
            },
            if favorite {
                crate::favorites::FAVORITE_REMOVE_ICON
            } else {
                crate::favorites::FAVORITE_ADD_ICON
            },
            move || shell.set_favorite(target.clone(), !favorite),
        );
    }
    if menu.edit_metadata
        && let Some(item) = metadata_item(row)
    {
        let shell = Rc::clone(shell);
        surface.append_configurable_action(
            ContextMenuItem::EditMetadata,
            msgid("Edit metadata"),
            EDIT_ICON,
            move || crate::editors::metadata::present_metadata_dialog(&shell, item.clone()),
        );
    }
    if menu.go_to {
        append_go_to(&surface, shell, row);
    }
    if let Some(share) = &menu.share {
        append_share(&surface, shell, share.clone());
    }
    match row {
        Row::Playlist(PlaylistListRow::Playlist(playlist)) => {
            append_playlist_actions(&surface, shell, target, menu, playlist);
        }
        Row::Playlist(PlaylistListRow::SmartPlaylist(playlist)) => {
            append_smart_playlist_actions(&surface, shell, target, menu, playlist);
        }
        Row::Playlist(PlaylistListRow::Folder(folder)) => {
            crate::menus::playlist_folders::append_folder_row_actions(
                &surface, shell, menu, folder,
            );
        }
        _ => {}
    }
    if let Some(payload) = payload {
        let single = matches!(row, Row::Track(_) | Row::AlbumTrack { .. });
        append_download_actions(&surface, shell, menu, &payload, single);
    }
    // Details and the pin close the menu, just above the rating.
    if let Some((item, ..)) = header {
        crate::menus::media_details::append_details_action(&surface, target, shell, item);
    }
    if let Some(pin) = &menu.pin {
        append_pin_action(&surface, shell, pin.clone(), menu.pinned);
    }
    if menu.rating
        && let Some(rated) = rated
    {
        let shell = Rc::clone(shell);
        let half_stars = shell.half_stars_enabled(rated.media_uri, rated.source_id);
        let target = rated.target;
        surface.append_fixed_widget(
            "rating",
            &context_rating_row(
                rated.rating.and_then(|value| u8::try_from(value).ok()),
                half_stars,
                surface.popover(),
                move |rating| shell.products.source.set_rating(target.clone(), rating),
            ),
        );
    }
    Some(surface)
}

/// The header a track, album or artist menu starts with: what Details opens, its title,
/// subtitle and artwork.
fn header(row: &Row) -> Option<(MediaDetailsItem, &str, String, Option<&[u8]>)> {
    Some(match row {
        Row::Track(track) | Row::AlbumTrack { track, .. } => (
            MediaDetailsItem::Track(track.media_uri.clone()),
            &track.title,
            track_menu_subtitle(&track.artist, &track.album),
            track.artwork_binding.as_deref(),
        ),
        Row::Album(album) | Row::AlbumHeading(album) => (
            MediaDetailsItem::Album(album.media_uri.clone()),
            &album.title,
            album.artist.clone(),
            album.artwork_binding.as_deref(),
        ),
        Row::Artist(artist) => (
            MediaDetailsItem::Artist {
                media_uri: artist.media_uri.clone(),
                album_artist: artist.album_artist,
            },
            &artist.name,
            String::new(),
            artist.artwork_binding.as_deref(),
        ),
        _ => return None,
    })
}

/// What a track, album or artist row's favorite and rating mark.
struct Rated<'a> {
    target: FavoriteTarget,
    favorite: bool,
    rating: Option<i64>,
    media_uri: &'a str,
    source_id: Option<&'a str>,
}

/// What a favorite of `row` changes, and whether it is a favorite now.
pub(crate) fn row_favorite(row: &Row) -> Option<(FavoriteTarget, bool)> {
    rated(row).map(|rated| (rated.target, rated.favorite))
}

fn rated(row: &Row) -> Option<Rated<'_>> {
    let (target, state, media_uri, source_id) = match row {
        Row::Track(track) | Row::AlbumTrack { track, .. } => (
            FavoriteTarget::Track(track.media_uri.clone()),
            &track.state,
            &track.media_uri,
            track.source_id.as_deref(),
        ),
        Row::Album(album) | Row::AlbumHeading(album) => (
            FavoriteTarget::Album(album.media_uri.clone()),
            &album.state,
            &album.media_uri,
            None,
        ),
        Row::Artist(artist) => (
            FavoriteTarget::Artist(artist.media_uri.clone()),
            &artist.state,
            &artist.media_uri,
            None,
        ),
        _ => return None,
    };
    Some(Rated {
        target,
        favorite: state.favorite,
        rating: state.rating,
        media_uri,
        source_id,
    })
}

fn metadata_item(row: &Row) -> Option<MetadataItemId> {
    Some(match row {
        Row::Track(track) | Row::AlbumTrack { track, .. } => {
            MetadataItemId::Track(track.media_uri.clone())
        }
        Row::Album(album) | Row::AlbumHeading(album) => {
            MetadataItemId::Album(album.media_uri.clone())
        }
        Row::Artist(artist) => MetadataItemId::Artist(artist.media_uri.clone()),
        _ => return None,
    })
}

fn radio_label(seed: &RadioSeed) -> &'static str {
    match seed {
        RadioSeed::Track(_) => msgid("Track radio"),
        RadioSeed::Album(_) => msgid("Album radio"),
        RadioSeed::Artist(_) | RadioSeed::AlbumArtist(_) => msgid("Artist radio"),
        RadioSeed::Genre(_) => msgid("Genre radio"),
        RadioSeed::Playlist(_) => msgid("Playlist radio"),
    }
}

/// A queue entry's Remove from Queue and Play actions: unshuffled plays move the entry; shuffled
/// ones play its track.
fn append_queue_entry_actions(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    menu: &Menu,
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
    let play_shell = Rc::clone(shell);
    surface.append_play_actions(menu, move |placement, shuffled| match shuffled {
        true => play_target(
            &play_shell.products,
            &PlayTarget::Track(track),
            placement,
            true,
        ),
        false => place_queue_selection(&play_shell.products.playback, selection.clone(), placement),
    });
}

/// Play Next and Play Later move the queue entries `selection`; other placements play the first.
pub fn place_queue_selection(
    queue: &PlaybackOwner,
    selection: QueueSelection,
    placement: QueuePlacement,
) {
    match (placement, selection) {
        (QueuePlacement::Next, selection) => queue.move_selection_after_current(selection),
        (QueuePlacement::End, selection) => queue.reorder(selection, QueueReorderTarget::End),
        (_, QueueSelection::Occurrences(ids)) => queue.activate_occurrence(ids[0]),
        (_, QueueSelection::Positions(positions)) => queue.activate(positions[0]),
    }
}

/// The Go to submenu of what `row` links to, with the actions that open them.
fn append_go_to(surface: &ContextMenuSurface, shell: &Rc<Shell>, row: &Row) {
    let submenu = gtk::gio::Menu::new();
    for target in rufin_core::menu::go_to(&shell.products.source, row) {
        let item = match target {
            GoTo::Artist(name, route) => {
                let label = localization::tr_with("Go to {artist}", &[("artist", &name)]);
                let shell = Rc::clone(shell);
                surface.item(&label, ARTIST_ICON, move || shell.open_route(route.clone()))
            }
            GoTo::Album(route) => {
                let shell = Rc::clone(shell);
                surface.item(
                    &localization::tr(msgid("Go to Album")),
                    ALBUM_ICON,
                    move || shell.open_route(route.clone()),
                )
            }
            GoTo::Source(link) => source_item(surface, shell, row, link),
        };
        submenu.append_item(&item);
    }
    surface.append_configurable_submenu(
        ContextMenuItem::GoTo,
        msgid("Go to"),
        &submenu,
        GO_TO_ICON,
    );
}

/// Share: copies the link to the row on its server. A link the server makes asks first how
/// long it lasts, what it says and, where the server lets Rufin say so, whether it downloads.
fn append_share(surface: &ContextMenuSurface, shell: &Rc<Shell>, share: Share) {
    let shell = Rc::downgrade(shell);
    surface.append_configurable_action(
        ContextMenuItem::Share,
        msgid("Share"),
        SHARE_ICON,
        move || {
            let Some(shell) = shell.upgrade() else { return };
            match share.dialog {
                Some(dialog) => present_share_dialog(&shell, share.target.clone(), dialog),
                None => copy_share_link(&shell, share.target.clone(), ShareOptions::default()),
            }
        },
    );
}

/// The dialog's Expires choices, in its order.
const SHARE_EXPIRIES: [ShareExpiry; 5] = [
    ShareExpiry::ServerDefault,
    ShareExpiry::Day,
    ShareExpiry::Week,
    ShareExpiry::Month,
    ShareExpiry::Year,
];

fn present_share_dialog(shell: &Rc<Shell>, target: ShareTarget, offered: ShareDialog) {
    let resource = "preferences/dialogs/share.ui";
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, {
        share_dialog: adw::Dialog, share_expiry: adw::ComboRow, share_description: adw::EntryRow,
        share_downloadable: adw::SwitchRow, share_cancel: gtk::Button, share_create: gtk::Button,
    });
    share_downloadable.set_visible(offered.downloadable);
    let dialog = share_dialog.downgrade();
    share_cancel.connect_clicked(move |_| {
        if let Some(dialog) = dialog.upgrade() {
            dialog.close();
        }
    });
    let dialog = share_dialog.downgrade();
    let owner = Rc::downgrade(shell);
    share_create.connect_clicked(move |_| {
        let options = ShareOptions {
            expiry: SHARE_EXPIRIES
                .get(share_expiry.selected() as usize)
                .copied()
                .unwrap_or_default(),
            description: share_description.text().into(),
            downloadable: share_downloadable.is_active(),
        };
        if let Some(dialog) = dialog.upgrade() {
            dialog.close();
        }
        if let Some(shell) = owner.upgrade() {
            copy_share_link(&shell, target.clone(), options);
        }
    });
    shell.present_selected_dialog(&share_dialog);
}

fn copy_share_link(shell: &Rc<Shell>, target: ShareTarget, options: ShareOptions) {
    let link = rufin_core::sharing::share(&shell.products.source, target, options);
    let shell = Rc::downgrade(shell);
    glib::spawn_future_local(async move {
        let (Ok(link), Some(shell)) = (link.recv().await, shell.upgrade()) else {
            return;
        };
        let message = match link {
            Ok(url) => {
                if let Some(display) = gtk::gdk::Display::default() {
                    display.clipboard().set_text(&url);
                }
                localization::tr(msgid("Link copied"))
            }
            Err(error) => localization::tr_with(
                "Could not share: {error}",
                &[("error", &localization::tr(&error))],
            ),
        };
        shell.control_feedback.show_feedback_toast(message);
    });
}

/// The item that opens `row`'s page on its server, or its folder.
fn source_item(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    row: &Row,
    link: rufin_core::detail_links::DetailLink,
) -> gtk::gio::MenuItem {
    let (media_uri, album_artist) = match row {
        Row::Track(track) | Row::AlbumTrack { track, .. } => (&track.media_uri, false),
        Row::Album(album) | Row::AlbumHeading(album) => (&album.media_uri, false),
        Row::Artist(artist) => (&artist.media_uri, artist.album_artist),
        _ => unreachable!("Go to leads to a source only from tracks, albums and artists"),
    };
    let window = surface
        .popover()
        .root()
        .and_downcast::<gtk::ApplicationWindow>()
        .map(|window| window.downgrade());
    let shell = Rc::clone(shell);
    let media_uri = media_uri.clone();
    let (title, icon_name) = (localization::tr(link.title), link.icon_name);
    surface.item(&title, icon_name, move || {
        let Some(window) = window.as_ref().and_then(glib::WeakRef::upgrade) else {
            return;
        };
        let source = shell.products.source.clone();
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
                        .collection_folder_uri(media_uri, album_artist)
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
    })
}

/// A playlist's Export, Edit, Add current, folder, Remove unavailable tracks and Delete
/// actions, as `menu` offers them.
fn append_playlist_actions(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    target: &gtk::Widget,
    menu: &Menu,
    playlist: &library::PlaylistRow,
) {
    let playlist_key = playlist.playlist_key;
    if menu.export {
        append_export_action(
            surface,
            shell,
            rufin_core::runtime::source::PlaylistExport::Playlist(playlist_key),
            &playlist.name,
        );
    }
    if menu.edit {
        let edit_shell = Rc::clone(shell);
        let playlist_name = playlist.name.clone();
        surface.append_fixed_action(msgid("Edit"), EDIT_ICON, move || {
            edit_shell.edit_playlist_dialog(playlist_key, playlist_name.clone());
        });
    }
    if menu.add_current {
        let current = crate::player::state::current_playback_track(
            shell.player_ui.selected_playback().as_deref(),
        )
        .map(|track| track.media_uri);
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
    if let Some(in_folder) = menu.folder {
        crate::menus::playlist_folders::append_folder_actions(
            surface,
            shell,
            vec![library::PlaylistFolderMember::Playlist { key: playlist_key }],
            in_folder,
        );
    }
    if menu.remove_unavailable {
        let cleanup_shell = Rc::downgrade(shell);
        let name = playlist.name.clone();
        let artwork = crate::widgets::library_fields::playlist_artwork(playlist);
        surface.append_fixed_action(msgid("Remove unavailable tracks"), TRASH_ICON, move || {
            let Some(shell) = cleanup_shell.upgrade() else {
                return;
            };
            let result = rufin_core::playlists::remove_unavailable_tracks(
                &shell.products.source,
                playlist_key,
            );
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
    if menu.delete {
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
}

/// A smart playlist's Export, Edit, folder and Delete actions, as `menu` offers them.
fn append_smart_playlist_actions(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    target: &gtk::Widget,
    menu: &Menu,
    playlist: &library::SmartPlaylistRow,
) {
    let key = playlist.smart_playlist_key;
    if menu.export {
        append_export_action(
            surface,
            shell,
            rufin_core::runtime::source::PlaylistExport::Smart(key),
            &playlist.name,
        );
    }
    if menu.edit {
        let edit_shell = Rc::clone(shell);
        let edit_row = playlist.clone();
        surface.append_fixed_action(msgid("Edit"), EDIT_ICON, move || {
            edit_shell.edit_smart_playlist_dialog(edit_row.clone());
        });
    }
    if let Some(in_folder) = menu.folder {
        crate::menus::playlist_folders::append_folder_actions(
            surface,
            shell,
            vec![library::PlaylistFolderMember::SmartPlaylist { key }],
            in_folder,
        );
    }
    if menu.delete {
        let delete_shell = Rc::clone(shell);
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
    }
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

fn append_pin_action(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    pin: SidebarPin,
    pinned: bool,
) {
    let shell = Rc::clone(shell);
    surface.append_configurable_action(
        ContextMenuItem::Pins,
        if pinned {
            msgid("Remove from Saved")
        } else {
            msgid("Add to Saved")
        },
        "rufin-bookmark-symbolic",
        move || shell.set_sidebar_pin(pin.clone(), !pinned),
    );
}

/// The Play submenu of a row: `play` where the page plays it, else its play request.
fn append_loaded_play(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    menu: &Menu,
    target: PlayTarget,
    play: Option<CollectionPlay>,
) {
    let play_shell = Rc::clone(shell);
    surface.append_play_actions(menu, move |placement, shuffled| match &play {
        Some(play) => play(placement, shuffled),
        None => play_target(&play_shell.products, &target, placement, shuffled),
    });
}

/// Download or Remove Download(s) of `payload`, as `menu` offers them; `single` names one
/// track.
fn append_download_actions(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    menu: &Menu,
    payload: &MediaDragSource,
    single: bool,
) {
    if menu.download {
        let shell = Rc::clone(shell);
        let payload = payload.clone();
        surface.append_configurable_action(
            ContextMenuItem::Download,
            msgid("Download"),
            DOWNLOAD_ICON,
            move || download(&shell, &payload, false),
        );
    }
    if menu.remove_download {
        let shell = Rc::clone(shell);
        let payload = payload.clone();
        surface.append_configurable_action(
            ContextMenuItem::Download,
            if single {
                msgid("Remove Download")
            } else {
                msgid("Remove Downloads")
            },
            TRASH_ICON,
            move || download(&shell, &payload, true),
        );
    }
    if menu.delete_permanently {
        let shell = Rc::clone(shell);
        let payload = payload.clone();
        let target = surface.target().downgrade();
        surface.append_destructive_action(msgid("Delete permanently"), TRASH_ICON, move || {
            if let Some(target) = target.upgrade() {
                confirm_delete(&shell, &target, payload.clone());
            }
        });
    }
}

/// Asks before Delete permanently, then deletes the files of the tracks of `payload`.
fn confirm_delete(shell: &Rc<Shell>, target: &gtk::Widget, payload: MediaDragSource) {
    let tr = |text| localization::tr(text);
    let dialog = adw::AlertDialog::builder()
        .heading(tr(msgid("Delete Permanently?")))
        .body(tr(msgid(
            "The files are deleted where they are stored. This cannot be undone.",
        )))
        .build();
    dialog.add_response("cancel", &tr(msgid("Cancel")));
    dialog.add_response("delete", &tr(msgid("Delete")));
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    let shell = Rc::clone(shell);
    dialog.connect_response(Some("delete"), move |_, _| {
        let payload = payload.clone();
        let source = shell.products.source.clone();
        let task = shell.products.runtime.spawn(async move {
            let selection = payload
                .track_selection(&source, rufin_core::view::SelectionScope::Downloads)
                .await?;
            let reply = source.delete_selection(selection);
            reply.recv().await.map_err(|error| error.to_string())?
        });
        let shell = Rc::clone(&shell);
        glib::spawn_future_local(async move {
            let error = match task.await {
                Ok(Ok(())) => return,
                Ok(Err(error)) => error,
                Err(error) => error.to_string(),
            };
            shell.control_feedback.show_feedback_toast(error);
        });
    });
    dialog.present(Some(target));
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

fn selection_menu(shell: &Shell) -> Menu {
    rufin_core::menu::selection(&shell.settings.current.borrow())
}

pub fn present_track_selection_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    selection: TrackSelectionSnapshot,
    position: Option<(f64, f64)>,
) {
    let surface = ContextMenuSurface::new(target, "track-selection", position);
    append_track_selection_actions(&surface, shell, selection);
    surface.popup();
}

pub fn present_collection_selection_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    clicked: PlayTarget,
    position: Option<(f64, f64)>,
) -> bool {
    let Some((surface, _)) = collection_selection_surface(target, shell, clicked, position) else {
        return false;
    };
    surface.popup();
    true
}

fn collection_selection_surface(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    clicked: PlayTarget,
    position: Option<(f64, f64)>,
) -> Option<(
    ContextMenuSurface,
    crate::widgets::media_drag::CollectionTargets,
)> {
    let targets = crate::widgets::media_drag::selected_collection_targets(target, &clicked)?;
    let payload = MediaDragSource::Targets {
        targets: targets.clone(),
    };
    let menu = selection_menu(shell);
    let surface = ContextMenuSurface::new(target, "collection-selection", position);
    if menu.edit_metadata {
        let metadata_payload = payload.clone();
        let metadata_shell = Rc::clone(shell);
        let edit_metadata = move || {
            let payload = metadata_payload.clone();
            let shell = Rc::clone(&metadata_shell);
            let source = shell.products.source.clone();
            let task = shell.products.runtime.spawn(async move {
                payload
                    .track_selection(&source, rufin_core::view::SelectionScope::Page)
                    .await
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
    }
    let play_shell = Rc::clone(shell);
    let play_payload = payload.clone();
    surface.append_play_actions(&menu, move |placement, shuffled| {
        let payload = play_payload.clone();
        let source = play_shell.products.source.clone();
        let playback = play_shell.products.playback.clone();
        let task = play_shell.products.runtime.spawn(async move {
            let mut request = payload.play_request(&source).await?.shuffled(shuffled);
            request.placement = placement;
            playback.play_selection(request);
            Ok::<_, String>(())
        });
        let shell = Rc::clone(&play_shell);
        glib::spawn_future_local(async move {
            match task.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => shell.control_feedback.show_feedback_toast(error),
                Err(error) => shell
                    .control_feedback
                    .show_feedback_toast(error.to_string()),
            }
        });
    });
    if menu.add_to_playlist {
        crate::playlists::picker::append_to_menu(&surface, shell, payload.clone());
    }
    append_download_actions(&surface, shell, &menu, &payload, false);
    Some((surface, targets))
}

fn present_playlist_selection_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    clicked: PlayTarget,
    position: Option<(f64, f64)>,
    in_folder: bool,
) -> bool {
    let Some((surface, targets)) = collection_selection_surface(target, shell, clicked, position)
    else {
        return false;
    };
    let catalog = shell.products.library.clone();
    let task = shell
        .products
        .runtime
        .spawn(async move { targets.collections(&catalog).await });
    let (shell, target) = (Rc::clone(shell), target.clone());
    glib::spawn_future_local(async move {
        match task.await {
            Ok(Ok(collections)) => {
                let members = collections
                    .iter()
                    .filter_map(|collection| match *collection {
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
                let delete_shell = Rc::clone(&shell);
                let delete_target = target.downgrade();
                let delete = move || {
                    let Some(target) = delete_target.upgrade() else {
                        return;
                    };
                    let shell = Rc::clone(&delete_shell);
                    let targets = collections.clone();
                    let dialog =
                        crate::playlists::delete_playlists_dialog(targets.len(), move || {
                            let targets = targets.clone();
                            let source = shell.products.source.clone();
                            let task = shell.products.runtime.spawn(async move {
                                let mut smart = Vec::new();
                                let mut failure = None;
                                for collection in targets {
                                    match collection {
                                        library::Collection::Playlist(key) => {
                                            if let Err(error) =
                                                rufin_core::playlists::delete_playlist(&source, key)
                                                    .await
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
                                    Ok(Err(error)) => {
                                        shell.control_feedback.show_feedback_toast(error)
                                    }
                                    Err(error) => shell
                                        .control_feedback
                                        .show_feedback_toast(error.to_string()),
                                }
                            });
                        });
                    dialog.present(Some(&target));
                };
                surface.append_fixed_action(msgid("Delete"), DELETE_ICON, delete);
                surface.popup();
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
    surface.popup();
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
    let menu = selection_menu(shell);
    let play_shell = Rc::clone(shell);
    let play_selection = selection.clone();
    surface.append_play_actions(&menu, move |placement, shuffled| {
        play_selection.play(&play_shell.products.playback, placement, shuffled)
    });
    if menu.add_to_playlist {
        crate::playlists::picker::append_to_menu(
            surface,
            shell,
            MediaDragSource::playlist_entries(selection.clone()),
        );
    }
    append_metadata_selection_action(surface, shell, &menu, selection.input.clone());
    let payload = MediaDragSource::playlist_entries(selection);
    append_download_actions(surface, shell, &menu, &payload, false);
}

fn append_track_selection_actions(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    selection: TrackSelectionSnapshot,
) {
    if selection.count == 0 {
        return;
    }
    let menu = selection_menu(shell);
    let play_shell = Rc::clone(shell);
    let play_selection = selection.clone();
    surface.append_play_actions(&menu, move |placement, shuffled| {
        play_selection.play(&play_shell.products.playback, placement, shuffled)
    });
    if menu.add_to_playlist {
        crate::playlists::picker::append_to_menu(
            surface,
            shell,
            MediaDragSource::selection(selection.clone()),
        );
    }
    append_metadata_selection_action(surface, shell, &menu, selection.input.clone());
    append_download_actions(
        surface,
        shell,
        &menu,
        &MediaDragSource::selection(selection),
        false,
    );
}

fn append_metadata_selection_action(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    menu: &Menu,
    selection: library::MemberSelection,
) {
    if !menu.edit_metadata {
        return;
    }
    let shell = Rc::clone(shell);
    surface.append_configurable_action(
        ContextMenuItem::EditMetadata,
        msgid("Edit metadata"),
        EDIT_ICON,
        move || crate::editors::metadata::present_track_metadata_dialog(&shell, selection.clone()),
    );
}
