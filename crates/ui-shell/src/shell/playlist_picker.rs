use std::cell::Cell;
use std::rc::Rc;
use ui_shared::media_drag::MediaDragSource;

use adw::prelude::*;
use downloads::DownloadSubject;
use localization::msgid;

use crate::shell::Shell;
use rufin_core::settings::ContextMenuItem;
use ui_shared::downloads::OperationFeedback;
use ui_shared::interactions::{ADD_TO_PLAYLIST_ICON, ContextMenuSurface};

use rufin_core::playback::PlaybackTarget;
use ui_shared::selection::{PlaylistEntrySelectionSnapshot, TrackSelectionSnapshot};

pub(crate) fn refresh_context_playlist_picker(shell: &Rc<Shell>) {
    shell.refresh_playlist_picker();
}

pub(crate) fn append_context_menu_picker(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    target: PlaybackTarget,
) {
    let source = MediaDragSource::capture_target(shell.selected_library().as_deref(), target);
    append_context_menu_picker_source(surface, shell, source);
}

pub(crate) fn append_context_menu_picker_media_uris(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    media_uris: impl IntoIterator<Item = String>,
) {
    append_context_menu_picker_source(surface, shell, MediaDragSource::media_uris(media_uris));
}

pub(crate) fn present_playlist_picker_media_uris(
    shell: &Rc<Shell>,
    media_uris: impl IntoIterator<Item = String>,
) {
    open_full_playlist_picker(shell, MediaDragSource::media_uris(media_uris));
}

pub(crate) fn append_context_menu_picker_source(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    source: MediaDragSource,
) {
    let picker = context_playlist_picker(shell, source.clone(), surface.popover());
    let click_shell = Rc::downgrade(shell);
    surface.append_configurable_widget_submenu(
        ContextMenuItem::AddToPlaylist,
        msgid("Add to Playlist"),
        "playlist-picker",
        &picker,
        ADD_TO_PLAYLIST_ICON,
        move || {
            if let Some(shell) = click_shell.upgrade() {
                open_full_playlist_picker(&shell, source.clone());
            }
        },
    );
}

pub(crate) fn present_playlist_picker_selection(
    shell: &Rc<Shell>,
    selection: TrackSelectionSnapshot,
) {
    open_full_playlist_picker(shell, MediaDragSource::selection(selection));
}

pub(crate) fn present_playlist_picker_entries(
    shell: &Rc<Shell>,
    selection: PlaylistEntrySelectionSnapshot,
) {
    open_full_playlist_picker(shell, MediaDragSource::playlist_entries(selection));
}

fn open_full_playlist_picker(shell: &Rc<Shell>, source: MediaDragSource) {
    let source_key = shell
        .selected_library()
        .as_deref()
        .map(|selected| selected.source_key);
    let database = shell.products.library.clone();
    let task = shell.products.runtime.spawn(async move {
        let (tracks, subject) = source.resolve(&database).await?;
        let playlists = database
            .playlist_destinations(source_key, &library::ReadCancellation::new())
            .await
            .map_err(|error| error.to_string())?;
        Ok::<_, String>((playlists, tracks, subject))
    });
    let shell = Rc::downgrade(shell);
    gtk::glib::spawn_future_local(async move {
        let Some((playlists, tracks, subject)) = task.await.ok().and_then(Result::ok) else {
            return;
        };
        if let Some(shell) = shell.upgrade() {
            present_playlist_picker(&shell, source_key, playlists, tracks, subject);
        }
    });
}

fn context_playlist_picker(
    shell: &Rc<Shell>,
    source: MediaDragSource,
    popover: &gtk::PopoverMenu,
) -> gtk::Box {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("context-playlist-submenu");
    root.set_hexpand(true);

    let started = Cell::new(false);
    let picker_shell = Rc::downgrade(shell);
    let picker_popover = popover.downgrade();
    root.connect_map(move |root| {
        if started.replace(true) {
            return;
        }
        let (Some(shell), Some(popover)) = (picker_shell.upgrade(), picker_popover.upgrade())
        else {
            return;
        };
        populate_context_playlist_picker(root, &shell, source.clone(), &popover);
    });
    root
}

fn populate_context_playlist_picker(
    root: &gtk::Box,
    shell: &Rc<Shell>,
    source: MediaDragSource,
    popover: &gtk::PopoverMenu,
) {
    let update = ui_shared::playlist_picker::populate_context_playlist_picker(
        root,
        shell.products.source.clone(),
        picker_feedback(shell),
        popover,
    );
    let source_key = shell
        .selected_library()
        .as_deref()
        .map(|selected| selected.source_key);
    let database = shell.products.library.clone();
    let task = shell.products.runtime.spawn(async move {
        let (tracks, subject) = source.resolve(&database).await?;
        let playlists = database
            .playlist_destinations(source_key, &library::ReadCancellation::new())
            .await
            .map_err(|error| error.to_string())?;
        Ok::<_, String>((playlists, tracks, subject))
    });
    gtk::glib::spawn_future_local(async move {
        if let Some((playlists, tracks, subject)) = task.await.ok().and_then(Result::ok) {
            update(playlists, tracks, subject);
        }
    });
}

fn picker_feedback(shell: &Rc<Shell>) -> Rc<dyn Fn(&OperationFeedback)> {
    let shell = Rc::downgrade(shell);
    Rc::new(move |feedback| {
        if let Some(shell) = shell.upgrade() {
            shell.show_operation_feedback(feedback);
        }
    })
}

fn present_playlist_picker(
    shell: &Rc<Shell>,
    source_key: Option<library::SourceKey>,
    playlists: Vec<library::PlaylistRow>,
    media_uris: Vec<String>,
    subject: DownloadSubject,
) {
    let create_shell = Rc::downgrade(shell);
    let (dialog, update) = ui_shared::playlist_picker::playlist_picker_dialog(
        shell.products.source.clone(),
        Rc::clone(&shell.artwork),
        Rc::clone(&shell.settings),
        Rc::new(move |name, tracks| {
            if let Some(shell) = create_shell.upgrade() {
                shell.new_playlist_dialog_with(name, tracks);
            }
        }),
        picker_feedback(shell),
        playlists,
        media_uris,
        subject,
    );
    let close_shell = Rc::downgrade(shell);
    dialog.connect_closed(move |_| {
        if let Some(shell) = close_shell.upgrade() {
            shell.set_playlist_picker_refresh(None);
        }
    });
    let refresh_dialog = dialog.downgrade();
    let refresh_shell = Rc::downgrade(shell);
    shell.set_playlist_picker_refresh(Some(Rc::new(move || {
        if refresh_dialog.upgrade().is_none() {
            return;
        }
        let Some(shell_now) = refresh_shell.upgrade() else {
            return;
        };
        let database = shell_now.products.library.clone();
        let task = shell_now.products.runtime.spawn(async move {
            database
                .playlist_destinations(source_key, &library::ReadCancellation::new())
                .await
        });
        let shell = refresh_shell.clone();
        let update = Rc::clone(&update);
        let dialog = refresh_dialog.clone();
        gtk::glib::spawn_future_local(async move {
            let Some(playlists) = task.await.ok().and_then(Result::ok) else {
                return;
            };
            if dialog.upgrade().is_some() && shell.upgrade().is_some() {
                update(playlists);
            }
        });
    })));
    shell.present_selected_dialog(&dialog);
}
