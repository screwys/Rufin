use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use downloads::DownloadSubject;
use gtk::{gio, glib};
use localization::{msgid, tr};
use playback::SourceSessionEpoch;

use crate::downloads::{OperationFeedback, OperationFeedbackKind};
use crate::interactions::{ADD_TO_PLAYLIST_ICON, ContextMenuSurface, popdown_native_menu};
use crate::settings::ContextMenuItem;
use crate::shell::Shell;

use super::collections::PlaybackTarget;
use super::library_fields::playlist_artwork;
use super::track_selection::TrackSelectionSnapshot;

#[derive(Clone)]
pub(crate) struct PlaylistTrackSource {
    pub(crate) target: PlaybackTarget,
}

impl PlaylistTrackSource {
    pub(crate) fn new(target: PlaybackTarget) -> Self {
        Self { target }
    }
}

#[derive(Clone)]
enum ContextPlaylistSource {
    Target(PlaylistTrackSource),
    Selection(TrackSelectionSnapshot),
}

#[derive(Clone, Debug)]
struct PlaylistChoice {
    key: library::PlaylistKey,
    name: String,
    normalized_name: String,
}

#[derive(Clone)]
struct ReadyPlaylistTracks {
    source_key: library::SourceKey,
    source_session_epoch: SourceSessionEpoch,
    tracks: Rc<[library::TrackKey]>,
    subject: DownloadSubject,
}

pub(crate) fn context_menu_can_add_to_playlist(shell: &Rc<Shell>) -> bool {
    shell.selected_source_operations().is_some()
}

pub(crate) fn refresh_context_playlist_picker(shell: &Rc<Shell>) {
    shell.refresh_playlist_picker();
}

pub(crate) fn append_context_menu_picker(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    source: PlaylistTrackSource,
) {
    append_context_menu_picker_source(surface, shell, ContextPlaylistSource::Target(source));
}

pub(crate) fn append_context_menu_picker_selection(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    selection: TrackSelectionSnapshot,
) {
    append_context_menu_picker_source(surface, shell, ContextPlaylistSource::Selection(selection));
}

fn append_context_menu_picker_source(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    source: ContextPlaylistSource,
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
    open_full_playlist_picker(shell, ContextPlaylistSource::Selection(selection));
}

fn open_full_playlist_picker(shell: &Rc<Shell>, source: ContextPlaylistSource) {
    let Some(selected) = shell.selected_library().as_deref().cloned() else {
        return;
    };
    let runtime = selected.runtime.clone();
    let selected_for_load = selected.clone();
    let task = runtime.spawn(async move {
        let (tracks, subject) = source.resolve(&selected).await?;
        let playlists = load_playlist_rows(&selected).await?;
        Ok::<_, String>((playlists, tracks, subject))
    });
    let shell = Rc::downgrade(shell);
    gtk::glib::spawn_future_local(async move {
        let Some(shell) = shell.upgrade() else {
            return;
        };
        let Some((playlists, tracks, subject)) = task.await.ok().and_then(Result::ok) else {
            return;
        };
        if picker_source_is_current(&shell, &selected_for_load) {
            present_playlist_picker(&shell, selected_for_load, playlists, tracks, subject);
        }
    });
}

fn context_playlist_picker(
    shell: &Rc<Shell>,
    source: ContextPlaylistSource,
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
    source: ContextPlaylistSource,
    popover: &gtk::PopoverMenu,
) {
    let header = gtk::Overlay::new();
    let search = gtk::SearchEntry::new();
    search.add_css_class("context-playlist-submenu-search");
    search.set_placeholder_text(Some(&tr("Search")));
    search.set_hexpand(true);
    header.set_child(Some(&search));
    let skip_duplicates = gtk::CheckButton::new();
    skip_duplicates.set_active(true);
    skip_duplicates.set_halign(gtk::Align::End);
    skip_duplicates.set_valign(gtk::Align::Center);
    skip_duplicates.set_margin_end(6);
    let skip_duplicates_label = tr("Don't duplicate");
    skip_duplicates.set_tooltip_text(Some(&skip_duplicates_label));
    skip_duplicates.update_property(&[gtk::accessible::Property::Label(&skip_duplicates_label)]);
    skip_duplicates.set_visible(
        shell
            .selected_library()
            .as_deref()
            .is_some_and(|selected| selected.playlist_tracks_can_repeat),
    );
    header.add_overlay(&skip_duplicates);
    header.set_measure_overlay(&skip_duplicates, false);
    root.append(&header);

    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let query = Rc::new(RefCell::new(String::new()));
    let filter_query = Rc::clone(&query);
    let filter = gtk::CustomFilter::new(move |item| {
        let Some(item) = item.downcast_ref::<glib::BoxedAnyObject>() else {
            return false;
        };
        let choice = item.borrow::<PlaylistChoice>();
        let query = filter_query.borrow();
        query.is_empty() || choice.normalized_name.contains(query.as_str())
    });
    let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
    let selection = gtk::NoSelection::new(Some(filtered.clone()));
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_margin_top(2);
        label.set_margin_bottom(2);
        label.set_margin_start(4);
        label.set_margin_end(4);
        item.set_child(Some(&label));
    });
    factory.connect_bind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(label) = item
            .child()
            .and_then(|child| child.downcast::<gtk::Label>().ok())
        else {
            return;
        };
        let Some(choice) = item
            .item()
            .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
        else {
            return;
        };
        label.set_label(&choice.borrow::<PlaylistChoice>().name);
    });
    let list = gtk::ListView::new(Some(selection), Some(factory));
    list.add_css_class("context-playlist-submenu-list");
    list.set_single_click_activate(true);
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroller.set_propagate_natural_height(true);
    scroller.set_max_content_height(320);
    scroller.set_child(Some(&list));
    root.append(&scroller);

    let spinner = gtk::Spinner::new();
    spinner.set_margin_top(12);
    spinner.set_margin_bottom(12);
    spinner.start();
    root.append(&spinner);

    let search_filter = filter.clone();
    search.connect_search_changed(move |search| {
        *query.borrow_mut() = search.text().trim().to_lowercase();
        search_filter.changed(gtk::FilterChange::Different);
    });

    let ready = Rc::new(RefCell::new(None::<ReadyPlaylistTracks>));
    let activate_ready = Rc::clone(&ready);
    let activate_shell = Rc::downgrade(shell);
    let activate_model = filtered.clone();
    let activate_skip = skip_duplicates.clone();
    let activate_popover = popover.downgrade();
    list.connect_activate(move |_, position| {
        let Some(shell) = activate_shell.upgrade() else {
            return;
        };
        let Some(ready) = activate_ready.borrow().clone() else {
            return;
        };
        if !ready.is_current(&shell) {
            return;
        }
        let Some(choice) = activate_model
            .item(position)
            .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
            .map(|item| item.borrow::<PlaylistChoice>().clone())
        else {
            return;
        };
        let Some(operations) = shell.selected_source_operations() else {
            return;
        };
        let scheduled = operations.add_playlist_tracks(
            choice.key,
            ready.tracks.to_vec(),
            activate_skip.is_active(),
        );
        if scheduled > 0 {
            shell.show_operation_feedback(&OperationFeedback {
                subject: ready.subject,
                item_count: scheduled,
                kind: OperationFeedbackKind::PlaylistAdded {
                    destination: choice.name,
                },
            });
        }
        if let Some(popover) = activate_popover.upgrade() {
            popdown_native_menu(&popover);
        }
    });

    let Some(selected) = shell.selected_library().as_deref().cloned() else {
        return;
    };
    let runtime = selected.runtime.clone();
    let selected_for_load = selected.clone();
    let task = runtime.spawn(async move {
        let (tracks, subject) = source.resolve(&selected).await?;
        let playlists = load_playlist_destinations(&selected).await?;
        Ok::<_, String>((playlists, tracks, subject))
    });
    let shell = Rc::downgrade(shell);
    let root = root.downgrade();
    let spinner = spinner.downgrade();
    gtk::glib::spawn_future_local(async move {
        let Some(shell) = shell.upgrade() else {
            return;
        };
        if root.upgrade().is_none() {
            return;
        }
        let Some((playlists, tracks, subject)) = task.await.ok().and_then(Result::ok) else {
            return;
        };
        if !picker_source_is_current(&shell, &selected_for_load) {
            return;
        }
        let choices = playlists
            .into_iter()
            .map(|playlist| {
                glib::BoxedAnyObject::new(PlaylistChoice {
                    key: playlist.playlist_key,
                    normalized_name: playlist.name.to_lowercase(),
                    name: playlist.name,
                })
            })
            .collect::<Vec<_>>();
        store.splice(0, 0, &choices);
        ready.replace(Some(ReadyPlaylistTracks {
            source_key: selected_for_load.source_key,
            source_session_epoch: selected_for_load.source_session_epoch,
            tracks: tracks.into(),
            subject,
        }));
        if let Some(spinner) = spinner.upgrade() {
            spinner.stop();
            spinner.set_visible(false);
        }
    });
}

#[derive(Clone)]
struct PickerRow {
    row: gtk::Widget,
    check: gtk::CheckButton,
    playlist: library::PlaylistRow,
    haystack: String,
}

fn present_playlist_picker(
    shell: &Rc<Shell>,
    selected: crate::runtime::SelectedLibrary,
    playlists: Vec<library::PlaylistRow>,
    tracks: Vec<library::TrackKey>,
    subject: DownloadSubject,
) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
    root.add_css_class("context-playlist-picker");
    root.set_margin_top(12);
    root.set_margin_bottom(14);
    root.set_margin_start(18);
    root.set_margin_end(18);
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some(&tr("Type to search or create a new playlist")));
    root.append(&search);
    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let create = gtk::Button::new();
    create.add_css_class("flat");
    create.add_css_class("playlist-picker-create-row");
    create.add_css_class("context-playlist-row");
    create.add_css_class("context-playlist-create-row");
    create.set_halign(gtk::Align::Fill);
    create.set_visible(false);
    list.append(&create);
    let rows = Rc::new(RefCell::new(Vec::<PickerRow>::new()));
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroller.set_vexpand(true);
    scroller.set_child(Some(&list));
    root.append(&scroller);

    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let skip = gtk::CheckButton::with_label(&tr("Don't duplicate"));
    skip.set_active(true);
    skip.set_visible(selected.playlist_tracks_can_repeat);
    footer.append(&skip);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    footer.append(&spacer);
    let cancel = gtk::Button::with_label(&tr("Cancel"));
    let add = gtk::Button::with_label(&tr("Add"));
    add.add_css_class("suggested-action");
    add.set_sensitive(false);
    footer.append(&cancel);
    footer.append(&add);
    root.append(&footer);
    replace_picker_rows(shell, &list, &create, &rows, &add, playlists);

    let dialog = adw::Dialog::builder()
        .title(tr("Add to Playlist"))
        .content_width(700)
        .content_height(510)
        .build();
    let close_shell = Rc::downgrade(shell);
    dialog.connect_closed(move |_| {
        if let Some(shell) = close_shell.upgrade() {
            shell.set_playlist_picker_refresh(None);
        }
    });
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(&tr("Add to Playlist"), "")));
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&root));
    dialog.set_child(Some(&toolbar));
    let close = dialog.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(dialog) = close.upgrade() {
            dialog.close();
        }
    });

    let filter_rows = Rc::clone(&rows);
    let filter_create = create.clone();
    let filter_add = add.clone();
    search.connect_search_changed(move |entry| {
        let text = entry.text();
        let query = text.trim().to_lowercase();
        filter_create.set_label(&format!("+ {} {}", tr("Create"), text.trim()));
        filter_create.set_visible(!query.is_empty());
        for row in filter_rows.borrow().iter() {
            row.row
                .set_visible(query.is_empty() || row.haystack.contains(&query));
        }
        filter_add.set_sensitive(filter_rows.borrow().iter().any(|row| row.check.is_active()));
    });
    let operations = selected.operations.clone();
    let create_shell = Rc::downgrade(shell);
    let create_selected = selected.clone();
    let create_tracks = tracks.clone();
    let create_search = search.clone();
    create.connect_clicked(move |button| {
        let Some(shell) = create_shell.upgrade() else {
            return;
        };
        if !picker_source_is_current(&shell, &create_selected) {
            button.set_sensitive(false);
            return;
        }
        let name = create_search.text().trim().to_string();
        if !name.is_empty() {
            operations.create_playlist(name, create_tracks.clone());
            create_search.set_text("");
        }
    });
    let operations = selected.operations.clone();
    let add_shell = Rc::downgrade(shell);
    let add_selected = selected.clone();
    let add_rows = Rc::clone(&rows);
    let add_dialog = dialog.downgrade();
    add.connect_clicked(move |button| {
        let Some(shell) = add_shell.upgrade() else {
            return;
        };
        if !picker_source_is_current(&shell, &add_selected) {
            button.set_sensitive(false);
            return;
        }
        let mut added = 0;
        let mut destinations = Vec::new();
        for row in add_rows.borrow().iter().filter(|row| row.check.is_active()) {
            let scheduled = operations.add_playlist_tracks(
                row.playlist.playlist_key,
                tracks.clone(),
                skip.is_active(),
            );
            if scheduled > 0 {
                added += scheduled;
                destinations.push(row.playlist.name.clone());
            }
        }
        if added > 0 {
            let destination = match destinations.as_slice() {
                [name] => name.clone(),
                _ => format!("{} {}", destinations.len(), tr("Playlists")),
            };
            shell.show_operation_feedback(&OperationFeedback {
                subject: subject.clone(),
                item_count: added,
                kind: OperationFeedbackKind::PlaylistAdded { destination },
            });
        }
        if let Some(dialog) = add_dialog.upgrade() {
            dialog.close();
        }
    });
    let refresh_dialog = dialog.downgrade();
    let refresh_shell = Rc::downgrade(shell);
    let refresh_selected = selected.clone();
    let refresh_list = list.clone();
    let refresh_create = create.clone();
    let refresh_rows = Rc::clone(&rows);
    let refresh_add = add.clone();
    shell.set_playlist_picker_refresh(Some(Rc::new(move || {
        if refresh_dialog.upgrade().is_none() {
            return;
        }
        let Some(shell_now) = refresh_shell.upgrade() else {
            return;
        };
        if !picker_source_is_current(&shell_now, &refresh_selected) {
            if let Some(dialog) = refresh_dialog.upgrade() {
                dialog.close();
            }
            return;
        }
        let selected = refresh_selected.clone();
        let runtime = selected.runtime.clone();
        let task = runtime.spawn(async move { load_playlist_rows(&selected).await });
        let shell = refresh_shell.clone();
        let list = refresh_list.clone();
        let create = refresh_create.clone();
        let rows = Rc::clone(&refresh_rows);
        let add = refresh_add.clone();
        let current = refresh_selected.clone();
        gtk::glib::spawn_future_local(async move {
            let Some(shell) = shell.upgrade() else {
                return;
            };
            let Some(playlists) = task.await.ok().and_then(Result::ok) else {
                return;
            };
            if picker_source_is_current(&shell, &current) {
                replace_picker_rows(&shell, &list, &create, &rows, &add, playlists);
            }
        });
    })));
    shell.present_selected_dialog(&dialog);
}

fn replace_picker_rows(
    shell: &Rc<Shell>,
    list: &gtk::Box,
    create: &gtk::Button,
    rows: &Rc<RefCell<Vec<PickerRow>>>,
    add: &gtk::Button,
    playlists: Vec<library::PlaylistRow>,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    list.append(create);
    rows.borrow_mut().clear();
    let prefer_server = shell
        .settings
        .current
        .borrow()
        .prefer_server_playlist_covers;
    for playlist in playlists {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.add_css_class("playlist-picker-row");
        row.add_css_class("context-playlist-row");
        row.set_margin_top(4);
        row.set_margin_bottom(4);
        let check = gtk::CheckButton::new();
        row.append(&check);
        let artwork = playlist_artwork(&playlist, prefer_server);
        let cover = shell
            .cover_group_projection_for_artwork(
                &artwork,
                48,
                crate::shell::cover::THUMB_COVER_SIZE as i32,
            )
            .widget();
        cover.add_css_class("context-playlist-cover");
        row.append(&cover);
        let labels = gtk::Box::new(gtk::Orientation::Vertical, 2);
        labels.set_hexpand(true);
        let title = gtk::Label::new(Some(&playlist.name));
        title.add_css_class("context-playlist-title");
        title.set_xalign(0.0);
        labels.append(&title);
        let meta = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        meta.add_css_class("context-playlist-meta");
        meta.append(&playlist_picker_meta(
            "rufin-tracks-symbolic",
            &localization::track_count_text(playlist.track_count.max(0) as u64),
        ));
        meta.append(&playlist_picker_meta(
            "rufin-preferences-system-time-symbolic",
            &crate::format_duration_units((playlist.duration_millis.max(0) / 1_000) as u32),
        ));
        let genres = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        genres.add_css_class("context-playlist-genres");
        for genre in playlist.genres.iter().take(2) {
            let pill = gtk::Label::new(Some(&genre.name));
            pill.add_css_class("album-detail-genre-pill");
            genres.append(&pill);
        }
        genres.set_visible(genres.first_child().is_some());
        meta.append(&genres);
        labels.append(&meta);
        row.append(&labels);
        let widget = row.upcast::<gtk::Widget>();
        list.append(&widget);
        rows.borrow_mut().push(PickerRow {
            haystack: format!(
                "{} {} {}",
                playlist.name,
                playlist.track_count,
                crate::format_duration_units((playlist.duration_millis.max(0) / 1_000) as u32)
            )
            .to_lowercase(),
            playlist,
            row: widget,
            check: check.clone(),
        });
        let rows_for_check = Rc::downgrade(rows);
        let add_for_check = add.downgrade();
        check.connect_toggled(move |_| {
            if let (Some(rows), Some(add)) = (rows_for_check.upgrade(), add_for_check.upgrade()) {
                add.set_sensitive(rows.borrow().iter().any(|row| row.check.is_active()));
            }
        });
    }
}

fn playlist_picker_meta(icon_name: &str, text: &str) -> gtk::Box {
    let item = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.add_css_class("muted");
    icon.set_pixel_size(13);
    item.append(&icon);
    let label = gtk::Label::new(Some(text));
    label.add_css_class("muted");
    label.set_xalign(0.0);
    item.append(&label);
    item
}

impl ContextPlaylistSource {
    async fn resolve(
        &self,
        selected: &crate::runtime::SelectedLibrary,
    ) -> Result<(Vec<library::TrackKey>, DownloadSubject), String> {
        match self {
            Self::Target(source) => {
                let subject = source.target.download_subject();
                let (tracks, _) = source.target.resolve_order(selected).await?;
                Ok((tracks.to_vec(), subject))
            }
            Self::Selection(selection)
                if selection.source_key == selected.source_key
                    && selection.source_session_epoch == selected.source_session_epoch =>
            {
                Ok((selection.tracks.to_vec(), selection.download_subject()))
            }
            Self::Selection(_) => Err("playlist selection is no longer current".to_string()),
        }
    }
}

impl ReadyPlaylistTracks {
    fn is_current(&self, shell: &Shell) -> bool {
        shell.selected_library().as_deref().is_some_and(|selected| {
            selected.source_key == self.source_key
                && selected.source_session_epoch == self.source_session_epoch
        })
    }
}

async fn load_playlist_rows(
    selected: &crate::runtime::SelectedLibrary,
) -> Result<Vec<library::PlaylistRow>, String> {
    let cancellation = library::ReadCancellation::new();
    let order = selected
        .database
        .playlist_order(
            selected.source_key,
            selected.music_folder_key,
            library::PlaylistSort::Title,
            false,
            "",
            &cancellation,
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut playlists = Vec::with_capacity(order.len());
    for keys in order.chunks(128) {
        playlists.extend(
            selected
                .database
                .playlist_rows(
                    selected.source_key,
                    keys,
                    selected.music_folder_key,
                    &cancellation,
                )
                .await
                .map_err(|error| error.to_string())?,
        );
    }
    Ok(playlists)
}

async fn load_playlist_destinations(
    selected: &crate::runtime::SelectedLibrary,
) -> Result<Vec<library::PlaylistDestination>, String> {
    selected
        .database
        .playlist_destinations(
            selected.source_key,
            selected.music_folder_key,
            &library::ReadCancellation::new(),
        )
        .await
        .map_err(|error| error.to_string())
}

fn picker_source_is_current(shell: &Shell, expected: &crate::runtime::SelectedLibrary) -> bool {
    shell.selected_library().as_deref().is_some_and(|selected| {
        selected.source_key == expected.source_key
            && selected.source_session_epoch == expected.source_session_epoch
            && selected.music_folder_key == expected.music_folder_key
    })
}
