use std::{cell::RefCell, rc::Rc};

use crate::downloads::{OperationFeedback, OperationFeedbackKind};
use crate::interactions::ContextMenuSurface;
use crate::shell::Shell;
use adw::prelude::*;
use downloads::DownloadSubject;
use localization::tr;

use super::collections::PlaybackTarget;
use super::library_fields::playlist_artwork;

#[derive(Clone)]
pub(crate) struct PlaylistTrackSource {
    pub(crate) target: PlaybackTarget,
}

impl PlaylistTrackSource {
    pub(crate) fn new(target: PlaybackTarget) -> Self {
        Self { target }
    }
}

pub(crate) fn refresh_context_playlist_picker(shell: &Rc<Shell>) {
    shell.refresh_playlist_picker();
}

pub(crate) fn context_menu_can_add_to_playlist(shell: &Rc<Shell>) -> bool {
    shell.selected_source_operations().is_some()
}

pub(crate) fn install_context_menu_picker_action(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    source: PlaylistTrackSource,
) {
    let shell = Rc::clone(shell);
    surface.add_action("add-to-playlist", move || {
        let Some(selected) = shell.selected_library().as_deref().cloned() else {
            return;
        };
        let target = source.target.clone();
        let subject = target.download_subject();
        let runtime = selected.runtime.clone();
        let selected_for_load = selected.clone();
        let task = runtime.spawn(async move {
            let (tracks, _) = target.resolve_order(&selected).await?;
            let playlists = load_playlist_rows(&selected).await?;
            Ok::<_, String>((playlists, tracks.to_vec()))
        });
        let shell = Rc::downgrade(&shell);
        gtk::glib::spawn_future_local(async move {
            let Some(shell) = shell.upgrade() else { return };
            let Some((playlists, tracks)) = task.await.ok().and_then(Result::ok) else {
                return;
            };
            if picker_source_is_current(&shell, &selected_for_load) {
                present_playlist_picker(&shell, selected_for_load, playlists, tracks, subject);
            }
        });
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
            let Some(shell) = shell.upgrade() else { return };
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

fn picker_source_is_current(shell: &Shell, expected: &crate::runtime::SelectedLibrary) -> bool {
    shell.selected_library().as_deref().is_some_and(|selected| {
        selected.source_key == expected.source_key
            && selected.source_session_epoch == expected.source_session_epoch
            && selected.music_folder_key == expected.music_folder_key
    })
}
