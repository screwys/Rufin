use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use ::library::{
    LibraryQueryResult, MusicFolderId, PlaylistEdit, PlaylistSummary, PlaylistTrackAdd, SourceId,
    TrackId, TrackSelection,
};
use adw::prelude::*;
use artwork::ArtworkBinding;
use downloads::DownloadSubject;
use gtk::{gio, glib};
use playback::SourceSessionEpoch;
use tracing::warn;

use crate::downloads::{OperationFeedback, OperationFeedbackKind};
use crate::format_duration_units;
use crate::interactions::{ContextMenuSurface, close_context_surface, context_menu_scroll_page};
use crate::preferences::dialogs::popup::present_light_dismiss_dialog;
use crate::runtime::{SelectedLibrary, SelectedSourceHandle};
use crate::shell::Shell;
use crate::shell::cover::THUMB_COVER_SIZE;
use localization::tr;
use localization::track_count_text;

const CONTEXT_PLAYLIST_ROW_COVER_SIZE: i32 = 48;
const ADD_TO_PLAYLIST_DIALOG_WIDTH: i32 = 700;
const ADD_TO_PLAYLIST_DIALOG_HEIGHT: i32 = 510;

#[derive(Clone)]
struct PlaylistPickerRow {
    playlist: PlaylistSummary,
    row: gtk::Widget,
    check: gtk::CheckButton,
    haystack: String,
}
#[derive(Clone)]
pub(crate) struct PlaylistPickerHandle {
    source: PlaylistSourceIdentity,
    list: gtk::Box,
    rows: Rc<RefCell<Vec<PlaylistPickerRow>>>,
    create: gtk::Button,
    search: gtk::SearchEntry,
    add_button: gtk::Button,
    can_create: bool,
}

#[derive(Default)]
pub(crate) struct PlaylistPickerState {
    pub(crate) active: RefCell<Option<PlaylistPickerHandle>>,
}

impl PlaylistPickerState {
    fn close_active(&self) {
        let Some(handle) = self.active.borrow_mut().take() else {
            return;
        };
        close_context_surface(&handle.list);
    }
}

impl Drop for PlaylistPickerState {
    fn drop(&mut self) {
        self.close_active();
    }
}

#[derive(Clone)]
struct PlaylistSourceIdentity {
    source_id: SourceId,
    source_session_epoch: SourceSessionEpoch,
    music_folder_id: Option<MusicFolderId>,
    loaded_instance: usize,
    operations: SelectedSourceHandle,
}

impl PlaylistSourceIdentity {
    fn selected(selected: &SelectedLibrary) -> Self {
        Self {
            source_id: selected.source_id.clone(),
            source_session_epoch: selected.source_session_epoch,
            music_folder_id: selected.music_folder_id.clone(),
            loaded_instance: Arc::as_ptr(&selected.library) as usize,
            operations: selected.operations.clone(),
        }
    }

    fn is_current(&self, shell: &Shell) -> bool {
        shell.selected_library().as_deref().is_some_and(|selected| {
            selected.source_id == self.source_id
                && selected.source_session_epoch == self.source_session_epoch
                && selected.music_folder_id == self.music_folder_id
                && Arc::as_ptr(&selected.library) as usize == self.loaded_instance
        })
    }
}

#[derive(Clone)]
pub(crate) struct PlaylistTrackSource {
    source: PlaylistSourceIdentity,
    subject: DownloadSubject,
    tracks: PlaylistTracks,
}

#[derive(Clone)]
enum PlaylistTracks {
    Ready(Arc<[TrackId]>),
    Loaded(TrackSelection),
}

impl PlaylistTrackSource {
    pub(crate) fn ready(
        selected: &SelectedLibrary,
        subject: DownloadSubject,
        track_ids: Arc<[TrackId]>,
    ) -> Self {
        Self {
            source: PlaylistSourceIdentity::selected(selected),
            subject,
            tracks: PlaylistTracks::Ready(track_ids),
        }
    }

    pub(crate) fn loaded(
        selected: &SelectedLibrary,
        subject: DownloadSubject,
        tracks: TrackSelection,
    ) -> Self {
        Self {
            source: PlaylistSourceIdentity::selected(selected),
            subject,
            tracks: PlaylistTracks::Loaded(tracks),
        }
    }
}

fn present_context_playlist_picker_dialog(
    shell: &Rc<Shell>,
    source: PlaylistSourceIdentity,
    subject: DownloadSubject,
    track_ids: Arc<[TrackId]>,
) {
    let content = context_playlist_picker(shell, source, subject, track_ids);
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(&tr("Add to Playlist"), "")));
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&content));

    let dialog = adw::Dialog::builder()
        .title(tr("Add to Playlist"))
        .content_width(ADD_TO_PLAYLIST_DIALOG_WIDTH)
        .content_height(ADD_TO_PLAYLIST_DIALOG_HEIGHT)
        .child(&toolbar)
        .build();
    let shell_for_close = Rc::downgrade(shell);
    dialog.connect_closed(move |_| {
        if let Some(shell) = shell_for_close.upgrade()
            && let Some(picker) = shell.selected_playlist_picker()
        {
            picker.active.borrow_mut().take();
        }
    });
    present_light_dismiss_dialog(&dialog, &shell.chrome.window);
}
fn context_playlist_picker(
    shell: &Rc<Shell>,
    source_identity: PlaylistSourceIdentity,
    subject: DownloadSubject,
    track_ids: Arc<[TrackId]>,
) -> gtk::Box {
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
    let rows = Rc::new(RefCell::new(Vec::<PlaylistPickerRow>::new()));
    let create = playlist_create_row("");
    create.set_visible(false);
    list.append(&create);
    let add_button = gtk::Button::with_label(&tr("Add"));
    add_button.add_css_class("suggested-action");
    add_button.set_sensitive(false);
    let scroller = context_menu_scroll_page(&list);
    root.append(&scroller);

    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let skip = gtk::CheckButton::with_label(&tr("Don't duplicate"));
    skip.set_active(true);
    skip.set_visible(
        shell
            .selected_library()
            .as_deref()
            .is_none_or(|selected| selected.playlist_tracks_can_repeat),
    );
    footer.append(&skip);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    footer.append(&spacer);
    let cancel = gtk::Button::with_label(&tr("Cancel"));
    cancel.connect_clicked(close_context_surface);
    footer.append(&cancel);
    footer.append(&add_button);
    root.append(&footer);

    let handle = PlaylistPickerHandle {
        source: source_identity.clone(),
        list: list.clone(),
        rows: Rc::clone(&rows),
        create: create.clone(),
        search: search.clone(),
        add_button: add_button.clone(),
        can_create: shell.selected_library().is_some(),
    };
    refresh_playlist_picker_rows(shell, &handle, &context_menu_playlists(shell));
    if let Some(picker) = shell.selected_playlist_picker() {
        *picker.active.borrow_mut() = Some(handle.clone());
    }

    let create_for_search = create.downgrade();
    let rows_for_search = Rc::clone(&rows);
    let add_button_for_search = add_button.downgrade();
    let can_create = handle.can_create;
    search.connect_search_changed(move |entry| {
        let (Some(create), Some(add_button)) =
            (create_for_search.upgrade(), add_button_for_search.upgrade())
        else {
            return;
        };
        let text = entry.text();
        let label = create_playlist_label(text.trim());
        let query = text.trim().to_lowercase();
        create.set_label(&label);
        sync_playlist_picker_filter(&create, &rows_for_search, &add_button, can_create, &query);
    });

    let source = source_identity.operations.clone();
    let shell_for_create = Rc::downgrade(shell);
    let source_identity_for_create = source_identity.clone();
    let track_ids_for_create = Arc::clone(&track_ids);
    let search_for_create = search.downgrade();
    create.connect_clicked(move |button| {
        let Some(shell) = shell_for_create.upgrade() else {
            return;
        };
        let Some(search) = search_for_create.upgrade() else {
            return;
        };
        if !source_identity_for_create.is_current(&shell) {
            close_context_surface(button);
            return;
        }
        let name = search.text().trim().to_string();
        if !name.is_empty() {
            source.edit_playlist(PlaylistEdit::Create {
                name,
                track_ids: track_ids_for_create.to_vec(),
            });
            search.set_text("");
        }
    });

    let rows_for_add = Rc::clone(&rows);
    let source = source_identity.operations.clone();
    let shell_for_add = Rc::downgrade(shell);
    let source_identity_for_add = source_identity.clone();
    let feedback_subject = subject.clone();
    add_button.connect_clicked(move |button| {
        let Some(shell) = shell_for_add.upgrade() else {
            return;
        };
        if !source_identity_for_add.is_current(&shell) {
            close_context_surface(button);
            return;
        }
        let mut added_tracks = 0;
        let mut changed_playlist_count = 0;
        let mut changed_playlist_entries = Vec::new();
        if track_ids.is_empty() {
            close_context_surface(button);
            return;
        }
        for row in rows_for_add
            .borrow()
            .iter()
            .filter(|row| row.check.is_active())
        {
            let scheduled = source.add_playlist_tracks(PlaylistTrackAdd {
                playlist_id: row.playlist.playlist.id.clone(),
                track_ids: track_ids.to_vec(),
                skip_duplicates: skip.is_active(),
            });
            if scheduled > 0 {
                added_tracks += scheduled;
                changed_playlist_count += 1;
                changed_playlist_entries.push((
                    row.playlist.playlist.id.clone(),
                    row.playlist.playlist.name.clone(),
                ));
            }
        }
        if added_tracks > 0 {
            let destination = match changed_playlist_entries.as_slice() {
                [(_, name)] => name.clone(),
                _ => format!("{changed_playlist_count} {}", tr("Playlists")),
            };
            shell.show_operation_feedback(&OperationFeedback {
                subject: feedback_subject.clone(),
                item_count: added_tracks,
                kind: OperationFeedbackKind::PlaylistAdded { destination },
            });
        }
        close_context_surface(button);
    });

    root
}
pub(crate) fn refresh_context_playlist_picker(shell: &Rc<Shell>) {
    let Some(picker) = shell.selected_playlist_picker() else {
        return;
    };
    let Some(handle) = picker.active.borrow().clone() else {
        return;
    };
    drop(picker);
    if !handle.source.is_current(shell) {
        close_context_surface(&handle.list);
        return;
    }
    refresh_playlist_picker_rows(shell, &handle, &context_menu_playlists(shell));
}
fn refresh_playlist_picker_rows(
    shell: &Rc<Shell>,
    handle: &PlaylistPickerHandle,
    playlists: &[PlaylistSummary],
) {
    while let Some(child) = handle.list.first_child() {
        handle.list.remove(&child);
    }
    handle.list.append(&handle.create);
    handle.rows.borrow_mut().clear();
    for playlist in playlists {
        let (row, check, haystack) = playlist_picker_row(shell, playlist);
        handle.list.append(&row);
        handle.rows.borrow_mut().push(PlaylistPickerRow {
            playlist: playlist.clone(),
            row: row.upcast::<gtk::Widget>(),
            check: check.clone(),
            haystack,
        });
        let rows_for_check = Rc::downgrade(&handle.rows);
        let add_for_check = handle.add_button.downgrade();
        check.connect_toggled(move |_| {
            if let (Some(rows), Some(add)) = (rows_for_check.upgrade(), add_for_check.upgrade()) {
                update_playlist_picker_add_button(&rows, &add);
            }
        });
    }
    let query = handle.search.text().trim().to_lowercase();
    sync_playlist_picker_filter(
        &handle.create,
        &handle.rows,
        &handle.add_button,
        handle.can_create,
        &query,
    );
}
fn sync_playlist_picker_filter(
    create: &gtk::Button,
    rows: &Rc<RefCell<Vec<PlaylistPickerRow>>>,
    add_button: &gtk::Button,
    can_create: bool,
    query: &str,
) {
    create.set_visible(show_create_playlist_row(query, can_create));
    for row in rows.borrow().iter() {
        row.row
            .set_visible(query.is_empty() || row.haystack.contains(query));
    }
    update_playlist_picker_add_button(rows, add_button);
}
fn playlist_create_row(name: &str) -> gtk::Button {
    let button = gtk::Button::with_label(&create_playlist_label(name));
    button.add_css_class("flat");
    button.add_css_class("context-playlist-row");
    button.add_css_class("context-playlist-create-row");
    button.set_halign(gtk::Align::Fill);
    button
}
fn create_playlist_label(name: &str) -> String {
    format!("+ {} {}", tr("Create"), name)
}
fn show_create_playlist_row(query: &str, can_create: bool) -> bool {
    can_create && !query.trim().is_empty()
}
fn playlist_picker_row(
    shell: &Rc<Shell>,
    playlist: &PlaylistSummary,
) -> (gtk::Box, gtk::CheckButton, String) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row.add_css_class("context-playlist-row");
    row.set_margin_top(4);
    row.set_margin_bottom(4);

    let check = gtk::CheckButton::new();
    row.append(&check);
    row.append(&playlist_picker_cover(shell, playlist));

    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_hexpand(true);
    let title = gtk::Label::new(Some(&playlist.playlist.name));
    title.add_css_class("context-playlist-title");
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    text.append(&title);

    let meta = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    meta.add_css_class("context-playlist-meta");
    meta.append(&playlist_picker_meta(
        "rufin-tracks-symbolic",
        &track_count_text(playlist.track_count.into()),
    ));
    meta.append(&playlist_picker_meta(
        "rufin-preferences-system-time-symbolic",
        &format_duration_units(playlist.duration_seconds),
    ));
    let genres = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    genres.add_css_class("context-playlist-genres");
    for genre in playlist.genres.iter().take(2) {
        genres.append(&playlist_genre_pill(&genre.name));
    }
    genres.set_visible(genres.first_child().is_some());
    meta.append(&genres);
    text.append(&meta);
    row.append(&text);

    let haystack = format!(
        "{} {} {}",
        playlist.playlist.name,
        playlist.track_count,
        format_duration_units(playlist.duration_seconds)
    )
    .to_lowercase();
    (row, check, haystack)
}
fn playlist_genre_pill(name: &str) -> gtk::Label {
    let pill = gtk::Label::new(Some(name));
    pill.add_css_class("album-detail-genre-pill");
    pill
}
fn playlist_picker_cover(shell: &Rc<Shell>, playlist: &PlaylistSummary) -> gtk::Widget {
    let settings = shell.settings.current.borrow();
    let cover = shell.cover_tile_for_candidates(
        ArtworkBinding::playlist(
            &playlist.playlist,
            &playlist.representative_albums,
            settings.prefer_server_playlist_covers,
        ),
        CONTEXT_PLAYLIST_ROW_COVER_SIZE,
        THUMB_COVER_SIZE,
    );
    cover.add_css_class("context-playlist-cover");
    cover
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
fn update_playlist_picker_add_button(
    rows: &Rc<RefCell<Vec<PlaylistPickerRow>>>,
    button: &gtk::Button,
) {
    button.set_sensitive(rows.borrow().iter().any(|row| row.check.is_active()));
}
pub(crate) fn install_context_menu_picker_action(
    surface: &ContextMenuSurface,
    shell: &Rc<Shell>,
    track_source: PlaylistTrackSource,
) {
    let shell = Rc::clone(shell);
    surface.add_action("add-to-playlist", move || {
        let PlaylistTrackSource {
            source,
            subject,
            tracks,
        } = track_source.clone();
        match tracks {
            PlaylistTracks::Ready(track_ids) => {
                if source.is_current(&shell) && !track_ids.is_empty() {
                    present_context_playlist_picker_dialog(&shell, source, subject, track_ids);
                }
            }
            PlaylistTracks::Loaded(tracks) => {
                let shell = Rc::clone(&shell);
                glib::spawn_future_local(async move {
                    let result =
                        gio::spawn_blocking(move || -> LibraryQueryResult<Arc<[TrackId]>> {
                            tracks.prepare()?.track_ids()
                        })
                        .await;
                    match result {
                        Ok(Ok(track_ids)) if source.is_current(&shell) && !track_ids.is_empty() => {
                            present_context_playlist_picker_dialog(
                                &shell, source, subject, track_ids,
                            );
                        }
                        Ok(Ok(_)) => {}
                        Ok(Err(error)) => {
                            warn!(%error, "could not prepare tracks for the playlist picker");
                        }
                        Err(_) => {
                            warn!("playlist picker preparation task panicked");
                        }
                    }
                });
            }
        }
    });
}

pub(crate) fn context_menu_can_add_to_playlist(shell: &Rc<Shell>) -> bool {
    shell.selected_library().is_some()
}
fn context_menu_playlists(shell: &Rc<Shell>) -> Vec<PlaylistSummary> {
    let selected = shell.selected_library();
    let Some(selected) = selected.as_ref() else {
        return Vec::new();
    };
    selected
        .library
        .playlists()
        .map(|playlists| playlists.to_vec())
        .unwrap_or_default()
}
