use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    rc::Rc,
};

use adw::prelude::*;
use localization::{msgid, tr, trn_with};
use sources::{
    AlbumMetadata, AlbumMetadataEdit, AlbumMetadataValues, AlbumMetadataWritable, ArtistMetadata,
    ArtistMetadataEdit, ArtistMetadataValues, ArtistMetadataWritable, SourceMetadataError,
    TrackMetadata, TrackMetadataEdit, TrackMetadataValues, TrackMetadataWritable,
};

use crate::layout::{large_popup_content_height, large_popup_content_width};
use crate::preferences::source::field_layout::{
    compact_field_row_group, install_compact_field_row_responsiveness_at, style_compact_field_row,
};
use crate::shell::Shell;

const EDITOR_WIDTH: i32 = 650;
const EDITOR_MAX_HEIGHT: i32 = 720;
const EDITOR_FIELD_STACK_WIDTH: i32 = 520;
const FIELD_COLUMN_SPACING: i32 = 18;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MetadataItemId {
    Track(library::TrackKey),
    Album(library::AlbumKey),
    Artist(library::ArtistKey),
}

#[derive(Clone)]
#[expect(
    clippy::large_enum_variant,
    reason = "the metadata dialog owns one draft and keeps its concrete editor value inline"
)]
enum MetadataDraft {
    Track(TrackMetadata),
    Album(AlbumMetadata),
    Artist(ArtistMetadata),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum MetadataField {
    Title,
    SortTitle,
    Artist,
    Album,
    AlbumArtist,
    TrackNumber,
    DiscNumber,
    Year,
    Genre,
    Comment,
    Bpm,
    MusicBrainzRecordingId,
    MusicBrainzReleaseTrackId,
    MusicBrainzAlbumId,
    MusicBrainzReleaseGroupId,
    MusicBrainzArtistId,
    Locked,
}

struct MetadataEntry {
    field: MetadataField,
    entry: adw::EntryRow,
    undo: gtk::Button,
}

struct Editor {
    dialog: gtk::glib::WeakRef<adw::Dialog>,
    draft: MetadataDraft,
    entries: Vec<MetadataEntry>,
    locked: Option<adw::SwitchRow>,
    touched: RefCell<HashSet<MetadataField>>,
    token: RefCell<Option<String>>,
    identified_originals: RefCell<HashMap<MetadataField, String>>,
    status: gtk::Label,
    identify: gtk::Button,
    save: gtk::Button,
    cancel: gtk::Button,
    external_lookup_allowed: bool,
}

pub(crate) fn present_metadata_dialog(shell: &Rc<Shell>, item: MetadataItemId) {
    let Some(selected) = shell.selected_library().as_deref().cloned() else {
        return;
    };
    let receiver = match item {
        MetadataItemId::Track(key) => {
            MetadataReceiver::Track(selected.operations.track_metadata(key))
        }
        MetadataItemId::Album(key) => {
            MetadataReceiver::Album(selected.operations.album_metadata(key))
        }
        MetadataItemId::Artist(key) => {
            MetadataReceiver::Artist(selected.operations.artist_metadata(key))
        }
    };
    let shell = Rc::downgrade(shell);
    gtk::glib::spawn_future_local(async move {
        let draft = receiver.recv().await;
        let Some(shell) = shell.upgrade() else { return };
        if !selected_metadata_source_is_current(&shell, &selected) {
            return;
        }
        match draft {
            Ok(draft) => build_dialog(&shell, selected, item, draft),
            Err(SourceMetadataError::LocalAccessRequired { source_path }) => {
                let retry_shell = Rc::downgrade(&shell);
                present_local_access_recovery(
                    &shell,
                    selected,
                    &source_path,
                    Rc::new(move || {
                        if let Some(shell) = retry_shell.upgrade() {
                            present_metadata_dialog(&shell, item);
                        }
                    }),
                );
            }
            Err(SourceMetadataError::Unavailable) => {
                shell.show_feedback_toast(tr(msgid("Metadata editing is no longer available")))
            }
            Err(error) => present_metadata_error(&shell, &error.to_string()),
        }
    });
}

fn present_metadata_error(shell: &Rc<Shell>, message: &str) {
    let resource = crate::ui_resource::METADATA_DIALOG_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    crate::ui_resource::objects!(builder, resource, {
        error_dialog: adw::Dialog,
        error_message: gtk::Label,
    });
    error_dialog.set_content_width(large_popup_content_width(480));
    error_message.set_label(message);
    shell.present_selected_dialog(&error_dialog);
}

enum MetadataReceiver {
    Track(async_channel::Receiver<Result<TrackMetadata, SourceMetadataError>>),
    Album(async_channel::Receiver<Result<AlbumMetadata, SourceMetadataError>>),
    Artist(async_channel::Receiver<Result<ArtistMetadata, SourceMetadataError>>),
}

impl MetadataReceiver {
    async fn recv(self) -> Result<MetadataDraft, SourceMetadataError> {
        match self {
            Self::Track(receiver) => receiver
                .recv()
                .await
                .map_err(|_| SourceMetadataError::Unavailable)?
                .map(MetadataDraft::Track),
            Self::Album(receiver) => receiver
                .recv()
                .await
                .map_err(|_| SourceMetadataError::Unavailable)?
                .map(MetadataDraft::Album),
            Self::Artist(receiver) => receiver
                .recv()
                .await
                .map_err(|_| SourceMetadataError::Unavailable)?
                .map(MetadataDraft::Artist),
        }
    }
}

fn present_local_access_recovery(
    shell: &Rc<Shell>,
    selected: crate::runtime::SelectedLibrary,
    source_path: &str,
    on_success: Rc<dyn Fn()>,
) {
    let resource = crate::ui_resource::METADATA_DIALOG_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    crate::ui_resource::objects!(builder, resource, {
        recovery_dialog: adw::Dialog,
        recovery_content: gtk::Box,
    });
    recovery_dialog.set_content_width(large_popup_content_width(EDITOR_WIDTH));
    let retry_requested = Rc::new(Cell::new(false));
    recovery_dialog.connect_closed({
        let retry_requested = Rc::clone(&retry_requested);
        move |_| {
            if retry_requested.replace(false) {
                let on_success = Rc::clone(&on_success);
                gtk::glib::idle_add_local_once(move || on_success());
            }
        }
    });
    let close = recovery_dialog.downgrade();
    let retry: Rc<dyn Fn()> = Rc::new(move || {
        retry_requested.set(true);
        if let Some(dialog) = close.upgrade() {
            dialog.close();
        }
    });
    crate::preferences::source::local_access::mount_metadata_local_access_mapping(
        shell,
        source_path,
        &selected,
        &recovery_content,
        retry,
    );
    shell.present_selected_dialog(&recovery_dialog);
}

fn build_dialog(
    shell: &Rc<Shell>,
    selected: crate::runtime::SelectedLibrary,
    item: MetadataItemId,
    draft: MetadataDraft,
) {
    let resource = crate::ui_resource::METADATA_DIALOG_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    crate::ui_resource::objects!(builder, resource, {
        editor_dialog: adw::Dialog,
        editor_scroller: gtk::ScrolledWindow,
        fields: gtk::Box,
        identify_scope: gtk::Label,
        identify_spacer: gtk::Box,
        identify: gtk::Button,
        status: gtk::Label,
        cancel: gtk::Button,
        save: gtk::Button,
    });
    editor_dialog.set_content_width(large_popup_content_width(EDITOR_WIDTH));
    let external_lookup_allowed = shell
        .settings
        .current
        .borrow()
        .allows_external_metadata_lookup();
    identify.set_sensitive(draft.source_search() || external_lookup_allowed);
    let mut entries = Vec::new();
    let mut locked = None;
    append_draft_fields(&draft, &mut entries, &mut locked);
    populate_metadata_fields(
        &fields,
        &identify_scope,
        &identify_spacer,
        &draft,
        &entries,
        locked.as_ref(),
    );
    editor_scroller.set_max_content_height(
        large_popup_content_height(shell.chrome.window.height(), EDITOR_MAX_HEIGHT)
            .saturating_sub(64),
    );

    let editor = Rc::new(Editor {
        dialog: editor_dialog.downgrade(),
        draft,
        entries,
        locked,
        touched: RefCell::new(HashSet::new()),
        token: RefCell::new(None),
        identified_originals: RefCell::new(HashMap::new()),
        status,
        identify,
        save,
        cancel,
        external_lookup_allowed,
    });
    connect_editor_changes(&editor);
    seed_rufin_filled(&editor);
    let editor_lifetime = Rc::clone(&editor);
    editor_dialog.connect_closed(move |_| {
        let _ = &editor_lifetime;
    });
    let close = editor_dialog.downgrade();
    editor.cancel.connect_clicked(move |_| {
        if let Some(dialog) = close.upgrade() {
            dialog.close();
        }
    });
    connect_identify(shell, selected.clone(), item, &editor);
    connect_save(shell, selected, item, &editor_dialog, &editor);
    shell.present_selected_dialog(&editor_dialog);
}

impl MetadataDraft {
    fn source_search(&self) -> bool {
        match self {
            Self::Track(value) => value.source_search,
            Self::Album(value) => value.source_search,
            Self::Artist(value) => value.source_search,
        }
    }
    fn revision(&self) -> Option<String> {
        match self {
            Self::Track(value) => value.revision.clone(),
            Self::Album(value) => value.revision.clone(),
            Self::Artist(value) => value.revision.clone(),
        }
    }
    fn track_count(&self) -> usize {
        match self {
            Self::Track(_) => 1,
            Self::Album(value) => value.track_count,
            Self::Artist(value) => value.track_count,
        }
    }

    fn source_value(&self, field: MetadataField) -> String {
        match self {
            Self::Track(value) => track_value(&value.source_values, field),
            Self::Album(value) => album_value(&value.source_values, field),
            Self::Artist(value) => artist_value(&value.source_values, field),
        }
    }

    fn rufin_filled(&self, field: MetadataField) -> bool {
        match self {
            Self::Track(value) => track_writable(&value.rufin_filled, field),
            Self::Album(value) => album_writable(&value.rufin_filled, field),
            Self::Artist(value) => artist_writable(&value.rufin_filled, field),
        }
    }
}

#[derive(Clone, Copy)]
enum FieldLayout {
    Pair(MetadataField, MetadataField),
    Full(MetadataField),
    Lock,
}

const TRACK_LAYOUT: &[FieldLayout] = &[
    FieldLayout::Pair(MetadataField::Title, MetadataField::SortTitle),
    FieldLayout::Pair(MetadataField::Artist, MetadataField::Album),
    FieldLayout::Pair(MetadataField::AlbumArtist, MetadataField::Genre),
    FieldLayout::Pair(MetadataField::TrackNumber, MetadataField::DiscNumber),
    FieldLayout::Pair(MetadataField::Year, MetadataField::Bpm),
    FieldLayout::Full(MetadataField::Comment),
    FieldLayout::Pair(
        MetadataField::MusicBrainzRecordingId,
        MetadataField::MusicBrainzReleaseTrackId,
    ),
    FieldLayout::Pair(
        MetadataField::MusicBrainzAlbumId,
        MetadataField::MusicBrainzReleaseGroupId,
    ),
    FieldLayout::Lock,
];
const ALBUM_LAYOUT: &[FieldLayout] = &[
    FieldLayout::Pair(MetadataField::Title, MetadataField::SortTitle),
    FieldLayout::Pair(MetadataField::Artist, MetadataField::AlbumArtist),
    FieldLayout::Pair(MetadataField::Year, MetadataField::Genre),
    FieldLayout::Full(MetadataField::Comment),
    FieldLayout::Pair(
        MetadataField::MusicBrainzAlbumId,
        MetadataField::MusicBrainzReleaseGroupId,
    ),
    FieldLayout::Lock,
];
const ARTIST_LAYOUT: &[FieldLayout] = &[
    FieldLayout::Pair(MetadataField::Title, MetadataField::SortTitle),
    FieldLayout::Full(MetadataField::Genre),
    FieldLayout::Full(MetadataField::Comment),
    FieldLayout::Full(MetadataField::MusicBrainzArtistId),
    FieldLayout::Lock,
];

fn populate_metadata_fields(
    fields: &gtk::Box,
    identify_scope: &gtk::Label,
    identify_spacer: &gtk::Box,
    draft: &MetadataDraft,
    entries: &[MetadataEntry],
    locked: Option<&adw::SwitchRow>,
) {
    if draft.track_count() > 1 {
        let count = draft.track_count();
        let text = count.to_string();
        identify_scope.set_label(&trn_with(
            "Changes apply to {count} track",
            "Changes apply to {count} tracks",
            count as u64,
            &[("count", text.as_str())],
        ));
        identify_scope.set_visible(true);
        identify_spacer.set_visible(false);
    } else {
        identify_scope.set_visible(false);
        identify_spacer.set_visible(true);
    }

    let layout = match draft {
        MetadataDraft::Track(_) => TRACK_LAYOUT,
        MetadataDraft::Album(_) => ALBUM_LAYOUT,
        MetadataDraft::Artist(_) => ARTIST_LAYOUT,
    };
    for layout in layout {
        match layout {
            FieldLayout::Pair(left, right) => {
                let Some(left) = entries.iter().find(|entry| entry.field == *left) else {
                    continue;
                };
                let Some(right) = entries.iter().find(|entry| entry.field == *right) else {
                    fields.append(&compact_field_row_group(&left.entry));
                    continue;
                };
                let pair = gtk::Box::new(gtk::Orientation::Horizontal, FIELD_COLUMN_SPACING);
                pair.set_homogeneous(true);
                pair.set_hexpand(true);
                pair.append(&compact_field_row_group(&left.entry));
                pair.append(&compact_field_row_group(&right.entry));
                fields.append(&install_compact_field_row_responsiveness_at(
                    &pair,
                    EDITOR_FIELD_STACK_WIDTH,
                ));
            }
            FieldLayout::Full(field) => {
                if let Some(row) = entries.iter().find(|entry| entry.field == *field) {
                    fields.append(&compact_field_row_group(&row.entry));
                }
            }
            FieldLayout::Lock => {
                if let Some(row) = locked {
                    fields.append(&compact_field_row_group(row));
                }
            }
        }
    }
}

fn append_draft_fields(
    draft: &MetadataDraft,
    entries: &mut Vec<MetadataEntry>,
    locked: &mut Option<adw::SwitchRow>,
) {
    match draft {
        MetadataDraft::Track(value) => {
            append_entry(
                entries,
                MetadataField::Title,
                &value.values.title,
                value.writable.title,
                false,
            );
            append_optional(
                entries,
                MetadataField::SortTitle,
                value.values.sort_title.as_deref(),
                value.writable.sort_title,
                false,
            );
            append_optional(
                entries,
                MetadataField::Artist,
                value.values.artist.as_deref(),
                value.writable.artist,
                false,
            );
            append_optional(
                entries,
                MetadataField::Album,
                value.values.album.as_deref(),
                value.writable.album,
                false,
            );
            append_optional(
                entries,
                MetadataField::AlbumArtist,
                value.values.album_artist.as_deref(),
                value.writable.album_artist,
                false,
            );
            append_number(
                entries,
                MetadataField::TrackNumber,
                value.values.track_number,
                value.writable.track_number,
                false,
            );
            append_number(
                entries,
                MetadataField::DiscNumber,
                value.values.disc_number,
                value.writable.disc_number,
                false,
            );
            append_number(
                entries,
                MetadataField::Year,
                value.values.year,
                value.writable.year,
                false,
            );
            append_optional(
                entries,
                MetadataField::Genre,
                value.values.genre.as_deref(),
                value.writable.genre,
                false,
            );
            append_optional(
                entries,
                MetadataField::Comment,
                value.values.comment.as_deref(),
                value.writable.comment,
                false,
            );
            append_number(
                entries,
                MetadataField::Bpm,
                value.values.bpm,
                value.writable.bpm,
                false,
            );
            append_optional(
                entries,
                MetadataField::MusicBrainzRecordingId,
                value.values.musicbrainz_recording_id.as_deref(),
                value.writable.musicbrainz_recording_id,
                false,
            );
            append_optional(
                entries,
                MetadataField::MusicBrainzReleaseTrackId,
                value.values.musicbrainz_release_track_id.as_deref(),
                value.writable.musicbrainz_release_track_id,
                false,
            );
            append_optional(
                entries,
                MetadataField::MusicBrainzAlbumId,
                value.values.musicbrainz_album_id.as_deref(),
                value.writable.musicbrainz_album_id,
                false,
            );
            append_optional(
                entries,
                MetadataField::MusicBrainzReleaseGroupId,
                value.values.musicbrainz_release_group_id.as_deref(),
                value.writable.musicbrainz_release_group_id,
                false,
            );
            append_optional(
                entries,
                MetadataField::MusicBrainzArtistId,
                value.values.musicbrainz_artist_id.as_deref(),
                value.writable.musicbrainz_artist_id,
                false,
            );
            append_lock(locked, value.values.locked, value.writable.locked);
        }
        MetadataDraft::Album(value) => {
            append_entry(
                entries,
                MetadataField::Title,
                &value.values.title,
                value.writable.title,
                value.mixed.title,
            );
            append_optional(
                entries,
                MetadataField::SortTitle,
                value.values.sort_title.as_deref(),
                value.writable.sort_title,
                value.mixed.sort_title,
            );
            append_optional(
                entries,
                MetadataField::Artist,
                value.values.artist.as_deref(),
                value.writable.artist,
                value.mixed.artist,
            );
            append_optional(
                entries,
                MetadataField::AlbumArtist,
                value.values.album_artist.as_deref(),
                value.writable.album_artist,
                value.mixed.album_artist,
            );
            append_number(
                entries,
                MetadataField::Year,
                value.values.year,
                value.writable.year,
                value.mixed.year,
            );
            append_optional(
                entries,
                MetadataField::Genre,
                value.values.genre.as_deref(),
                value.writable.genre,
                value.mixed.genre,
            );
            append_optional(
                entries,
                MetadataField::Comment,
                value.values.comment.as_deref(),
                value.writable.comment,
                value.mixed.comment,
            );
            if let Some(row) = entries.last() {
                row.entry
                    .set_title(&metadata_overview_title(value.mixed.comment));
            }
            append_optional(
                entries,
                MetadataField::MusicBrainzAlbumId,
                value.values.musicbrainz_album_id.as_deref(),
                value.writable.musicbrainz_album_id,
                value.mixed.musicbrainz_album_id,
            );
            append_optional(
                entries,
                MetadataField::MusicBrainzReleaseGroupId,
                value.values.musicbrainz_release_group_id.as_deref(),
                value.writable.musicbrainz_release_group_id,
                value.mixed.musicbrainz_release_group_id,
            );
            append_lock(locked, value.values.locked, value.writable.locked);
        }
        MetadataDraft::Artist(value) => {
            append_entry(
                entries,
                MetadataField::Title,
                &value.values.name,
                value.writable.name,
                value.mixed.name,
            );
            append_optional(
                entries,
                MetadataField::SortTitle,
                value.values.sort_name.as_deref(),
                value.writable.sort_name,
                value.mixed.sort_name,
            );
            append_optional(
                entries,
                MetadataField::Genre,
                value.values.genre.as_deref(),
                value.writable.genre,
                value.mixed.genre,
            );
            append_optional(
                entries,
                MetadataField::Comment,
                value.values.comment.as_deref(),
                value.writable.comment,
                value.mixed.comment,
            );
            if let Some(row) = entries.last() {
                row.entry
                    .set_title(&metadata_overview_title(value.mixed.comment));
            }
            append_optional(
                entries,
                MetadataField::MusicBrainzArtistId,
                value.values.musicbrainz_artist_id.as_deref(),
                value.writable.musicbrainz_artist_id,
                value.mixed.musicbrainz_artist_id,
            );
            append_lock(locked, value.values.locked, value.writable.locked);
        }
    }
}

fn append_optional(
    entries: &mut Vec<MetadataEntry>,
    field: MetadataField,
    value: Option<&str>,
    writable: bool,
    mixed: bool,
) {
    append_entry(entries, field, value.unwrap_or_default(), writable, mixed);
}
fn append_number(
    entries: &mut Vec<MetadataEntry>,
    field: MetadataField,
    value: Option<u16>,
    writable: bool,
    mixed: bool,
) {
    append_entry(
        entries,
        field,
        &value.map(|value| value.to_string()).unwrap_or_default(),
        writable,
        mixed,
    );
    if let Some(row) = entries.last() {
        row.entry.set_input_purpose(gtk::InputPurpose::Digits);
    }
}
fn append_entry(
    entries: &mut Vec<MetadataEntry>,
    field: MetadataField,
    value: &str,
    writable: bool,
    mixed: bool,
) {
    let mut title = tr(field_title(field));
    if mixed {
        title = format!("{title} · {}", tr("Multiple values"));
    }
    let entry = adw::EntryRow::builder().title(title).text(value).build();
    entry.set_sensitive(writable);
    if !writable {
        entry.set_tooltip_text(Some(&tr("This source cannot edit this field")));
    }
    style_compact_field_row(&entry);
    let undo = gtk::Button::from_icon_name("rufin-edit-undo-symbolic");
    undo.add_css_class("flat");
    undo.set_tooltip_text(Some(&tr("Undo identified value")));
    undo.update_property(&[gtk::accessible::Property::Label(&tr(
        "Undo identified value",
    ))]);
    undo.set_valign(gtk::Align::Center);
    undo.set_visible(false);
    entry.add_suffix(&undo);
    entries.push(MetadataEntry { field, entry, undo });
}
fn append_lock(target: &mut Option<adw::SwitchRow>, value: Option<bool>, writable: bool) {
    if !writable {
        return;
    }
    let row = adw::SwitchRow::builder()
        .title(tr("Lock metadata"))
        .subtitle(tr(
            "Prevent automatic metadata refreshes from replacing these values",
        ))
        .active(value.unwrap_or(false))
        .build();
    style_compact_field_row(&row);
    *target = Some(row);
}

fn field_title(field: MetadataField) -> &'static str {
    match field {
        MetadataField::Title => msgid("Title"),
        MetadataField::SortTitle => msgid("Sort title"),
        MetadataField::Artist => msgid("Artists"),
        MetadataField::Album => msgid("Album"),
        MetadataField::AlbumArtist => msgid("Album artists"),
        MetadataField::TrackNumber => msgid("Track number"),
        MetadataField::DiscNumber => msgid("Disc number"),
        MetadataField::Year => msgid("Year"),
        MetadataField::Genre => msgid("Genres"),
        MetadataField::Comment => msgid("Comment"),
        MetadataField::Bpm => msgid("BPM"),
        MetadataField::MusicBrainzRecordingId => msgid("MusicBrainz recording ID"),
        MetadataField::MusicBrainzReleaseTrackId => msgid("MusicBrainz release track ID"),
        MetadataField::MusicBrainzAlbumId => msgid("MusicBrainz release ID"),
        MetadataField::MusicBrainzReleaseGroupId => msgid("MusicBrainz release group ID"),
        MetadataField::MusicBrainzArtistId => msgid("MusicBrainz artist ID"),
        MetadataField::Locked => msgid("Lock metadata"),
    }
}

fn metadata_overview_title(mixed: bool) -> String {
    let title = tr(msgid("Overview"));
    if mixed {
        format!("{title} · {}", tr(msgid("Multiple values")))
    } else {
        title
    }
}

fn connect_editor_changes(editor: &Rc<Editor>) {
    for row in editor.entries.iter() {
        let field = row.field;
        let editor_changed = Rc::downgrade(editor);
        row.entry.connect_changed(move |_| {
            let Some(editor_changed) = editor_changed.upgrade() else {
                return;
            };
            editor_changed.touched.borrow_mut().insert(field);
            refresh_save_state(&editor_changed);
        });
        let field = row.field;
        let editor_undo = Rc::downgrade(editor);
        row.undo.connect_clicked(move |_| {
            let Some(editor_undo) = editor_undo.upgrade() else {
                return;
            };
            let original = editor_undo.identified_originals.borrow_mut().remove(&field);
            if let Some(original) = original {
                editor_undo.entry(field).set_text(&original);
                editor_undo.touched.borrow_mut().remove(&field);
                refresh_identified_field(&editor_undo, field);
                if editor_undo.identified_originals.borrow().is_empty() {
                    editor_undo.token.borrow_mut().take();
                }
                refresh_save_state(&editor_undo);
            }
        });
    }
    if let Some(locked) = &editor.locked {
        let editor = Rc::downgrade(editor);
        locked.connect_active_notify(move |_| {
            let Some(editor) = editor.upgrade() else {
                return;
            };
            editor.touched.borrow_mut().insert(MetadataField::Locked);
            refresh_save_state(&editor);
        });
    }
}

fn seed_rufin_filled(editor: &Editor) {
    for row in editor.entries.iter() {
        if editor.draft.rufin_filled(row.field) && editor.writable(row.field) {
            editor
                .identified_originals
                .borrow_mut()
                .insert(row.field, editor.draft.source_value(row.field));
            editor.touched.borrow_mut().insert(row.field);
            refresh_identified_field(editor, row.field);
        }
    }
    refresh_save_state(editor);
}

fn refresh_identified_field(editor: &Editor, field: MetadataField) {
    let identified = editor.identified_originals.borrow().contains_key(&field);
    let row = editor
        .entries
        .iter()
        .find(|row| row.field == field)
        .expect("identified metadata field belongs to this draft");
    row.undo.set_visible(identified);
    if identified {
        row.entry.add_css_class("metadata-identified-change");
    } else {
        row.entry.remove_css_class("metadata-identified-change");
    }
}
fn refresh_save_state(editor: &Editor) {
    editor
        .save
        .set_sensitive(!editor.touched.borrow().is_empty() || editor.token.borrow().is_some());
    if let Ok(values) = current_values(
        match &editor.draft {
            MetadataDraft::Track(value) => MetadataItemId::Track(value.track_key),
            MetadataDraft::Album(value) => MetadataItemId::Album(value.album_key),
            MetadataDraft::Artist(value) => MetadataItemId::Artist(value.artist_key),
        },
        editor,
    ) {
        editor.identify.set_sensitive(identification_available(
            editor.draft.source_search(),
            editor.external_lookup_allowed,
            &values,
        ));
    }
}

fn identification_available(
    source_search: bool,
    external_lookup_allowed: bool,
    values: &CurrentValues,
) -> bool {
    source_search && !values.title().trim().is_empty()
        || external_lookup_allowed && values.has_exact_musicbrainz_identity()
}

fn connect_identify(
    shell: &Rc<Shell>,
    selected: crate::runtime::SelectedLibrary,
    item: MetadataItemId,
    editor: &Rc<Editor>,
) {
    let shell = Rc::downgrade(shell);
    let identify = editor.identify.clone();
    let editor = Rc::downgrade(editor);
    identify.connect_clicked(move |_| {
        let Some(shell) = shell.upgrade() else {
            return;
        };
        let Some(editor) = editor.upgrade() else {
            return;
        };
        if !selected_metadata_source_is_current(&shell, &selected) {
            editor.force_close();
            return;
        }
        let values = match current_values(item, &editor) {
            Ok(values) => values,
            Err(error) => {
                editor.show_error(&error);
                return;
            }
        };
        if !identification_available(
            editor.draft.source_search(),
            editor.external_lookup_allowed,
            &values,
        ) {
            return;
        }
        editor.set_busy(true, &tr("Identifying..."));
        let receiver = match values {
            CurrentValues::Track(values) => IdentifyReceiver::Track(
                selected
                    .operations
                    .identify_track_metadata(track_key(item), values),
            ),
            CurrentValues::Album(values) => IdentifyReceiver::Album(
                selected
                    .operations
                    .identify_album_metadata(album_key(item), values),
            ),
            CurrentValues::Artist(values) => IdentifyReceiver::Artist(
                selected
                    .operations
                    .identify_artist_metadata(artist_key(item), values),
            ),
        };
        let editor = Rc::downgrade(&editor);
        let shell = Rc::downgrade(&shell);
        let selected = selected.clone();
        gtk::glib::spawn_future_local(async move {
            let response = receiver.recv().await;
            let Some(editor) = editor.upgrade() else {
                return;
            };
            let Some(shell) = shell.upgrade() else {
                return;
            };
            if !selected_metadata_source_is_current(&shell, &selected) {
                editor.force_close();
                return;
            }
            match response {
                Ok(Some(Identified::Track(values, token))) => {
                    apply_track_values(&editor, &values);
                    editor.token.replace(token);
                }
                Ok(Some(Identified::Album(values, token))) => {
                    apply_album_values(&editor, &values);
                    editor.token.replace(token);
                }
                Ok(Some(Identified::Artist(values, token))) => {
                    apply_artist_values(&editor, &values);
                    editor.token.replace(token);
                }
                Ok(None) => {}
                Err(error) => editor.show_error(&error),
            }
            editor.set_busy(false, &tr("Identify"));
            refresh_save_state(&editor);
        });
    });
}

fn connect_save(
    shell: &Rc<Shell>,
    selected: crate::runtime::SelectedLibrary,
    item: MetadataItemId,
    dialog: &adw::Dialog,
    editor: &Rc<Editor>,
) {
    let shell = Rc::downgrade(shell);
    let dialog = dialog.downgrade();
    let save = editor.save.clone();
    let editor = Rc::downgrade(editor);
    save.connect_clicked(move |_| {
        let Some(shell) = shell.upgrade() else {
            return;
        };
        let Some(editor) = editor.upgrade() else {
            return;
        };
        if !selected_metadata_source_is_current(&shell, &selected) {
            editor.force_close();
            return;
        }
        let edit = match metadata_edit(item, &editor) {
            Ok(edit) => edit,
            Err(error) => {
                editor.show_error(&error);
                return;
            }
        };
        editor.set_busy(true, &tr("Saving..."));
        let revision = editor.draft.revision();
        let token = editor.token.borrow().clone();
        let receiver = match edit {
            MetadataEdit::Track(edit) => {
                SaveReceiver::Track(selected.operations.write_reviewed_track_metadata(
                    track_key(item),
                    revision,
                    token,
                    edit,
                ))
            }
            MetadataEdit::Album(edit) => {
                SaveReceiver::Album(selected.operations.write_reviewed_album_metadata(
                    album_key(item),
                    revision,
                    token,
                    edit,
                ))
            }
            MetadataEdit::Artist(edit) => {
                SaveReceiver::Artist(selected.operations.write_reviewed_artist_metadata(
                    artist_key(item),
                    revision,
                    token,
                    edit,
                ))
            }
        };
        let editor = Rc::downgrade(&editor);
        let dialog = dialog.clone();
        let shell = Rc::downgrade(&shell);
        let selected = selected.clone();
        gtk::glib::spawn_future_local(async move {
            let response = receiver.recv().await;
            let Some(editor) = editor.upgrade() else {
                return;
            };
            let Some(shell) = shell.upgrade() else {
                return;
            };
            if !selected_metadata_source_is_current(&shell, &selected) {
                editor.force_close();
                return;
            }
            match response {
                Ok(()) => {
                    if let Some(dialog) = dialog.upgrade() {
                        dialog.force_close();
                    }
                    let shell = Rc::clone(&shell);
                    gtk::glib::idle_add_local_once(move || present_metadata_dialog(&shell, item));
                }
                Err(error @ SourceMetadataError::SavedRefreshFailed(_)) => {
                    editor.show_error(&error.to_string());
                    editor.finish_committed_save();
                }
                Err(error) => {
                    editor.show_error(&error.to_string());
                    editor.set_busy(false, &tr("Save"));
                }
            }
        });
    });
}

fn track_key(item: MetadataItemId) -> library::TrackKey {
    let MetadataItemId::Track(key) = item else {
        unreachable!()
    };
    key
}
fn album_key(item: MetadataItemId) -> library::AlbumKey {
    let MetadataItemId::Album(key) = item else {
        unreachable!()
    };
    key
}
fn artist_key(item: MetadataItemId) -> library::ArtistKey {
    let MetadataItemId::Artist(key) = item else {
        unreachable!()
    };
    key
}

impl Editor {
    fn force_close(&self) {
        if let Some(dialog) = self.dialog.upgrade() {
            dialog.force_close();
        }
    }

    fn entry(&self, field: MetadataField) -> &adw::EntryRow {
        &self
            .entries
            .iter()
            .find(|row| row.field == field)
            .expect("metadata field belongs to this draft")
            .entry
    }
    fn set_busy(&self, busy: bool, label: &str) {
        self.identify
            .set_sensitive(!busy && self.draft.source_search());
        self.save.set_sensitive(
            !busy && (!self.touched.borrow().is_empty() || self.token.borrow().is_some()),
        );
        self.cancel.set_sensitive(!busy);
        for row in self.entries.iter() {
            row.entry.set_sensitive(!busy && self.writable(row.field));
        }
        if let Some(locked) = &self.locked {
            locked.set_sensitive(!busy);
        }
        if busy {
            self.save.set_label(label);
        } else {
            self.save.set_label(&tr("Save"));
            self.identify.set_label(&tr("Identify"));
        }
    }
    fn show_error(&self, message: &str) {
        self.status.set_label(message);
        self.status.set_tooltip_text(Some(message));
        self.status.set_visible(true);
    }

    fn finish_committed_save(&self) {
        self.identify.set_sensitive(false);
        self.save.set_sensitive(false);
        self.save.set_label(&tr("Saved"));
        self.cancel.set_sensitive(true);
        self.cancel.set_label(&tr("Close"));
        for row in self.entries.iter() {
            row.entry.set_sensitive(false);
        }
        if let Some(locked) = &self.locked {
            locked.set_sensitive(false);
        }
    }
    fn writable(&self, field: MetadataField) -> bool {
        match &self.draft {
            MetadataDraft::Track(value) => track_writable(&value.writable, field),
            MetadataDraft::Album(value) => album_writable(&value.writable, field),
            MetadataDraft::Artist(value) => artist_writable(&value.writable, field),
        }
    }
}

enum CurrentValues {
    Track(TrackMetadataValues),
    Album(AlbumMetadataValues),
    Artist(ArtistMetadataValues),
}

impl CurrentValues {
    fn title(&self) -> &str {
        match self {
            Self::Track(values) => &values.title,
            Self::Album(values) => &values.title,
            Self::Artist(values) => &values.name,
        }
    }

    fn has_exact_musicbrainz_identity(&self) -> bool {
        match self {
            Self::Track(values) => {
                values
                    .musicbrainz_recording_id
                    .as_deref()
                    .is_some_and(usable_identity)
                    || values
                        .musicbrainz_release_track_id
                        .as_deref()
                        .is_some_and(usable_identity)
            }
            Self::Album(values) => {
                values
                    .musicbrainz_album_id
                    .as_deref()
                    .is_some_and(usable_identity)
                    || values
                        .musicbrainz_release_group_id
                        .as_deref()
                        .is_some_and(usable_identity)
            }
            Self::Artist(values) => values
                .musicbrainz_artist_id
                .as_deref()
                .is_some_and(usable_identity),
        }
    }
}

fn usable_identity(value: &str) -> bool {
    !value.trim().is_empty()
}
enum MetadataEdit {
    Track(TrackMetadataEdit),
    Album(AlbumMetadataEdit),
    Artist(ArtistMetadataEdit),
}

fn current_values(item: MetadataItemId, editor: &Editor) -> Result<CurrentValues, String> {
    match item {
        MetadataItemId::Track(_) => Ok(CurrentValues::Track(track_values(editor)?)),
        MetadataItemId::Album(_) => Ok(CurrentValues::Album(album_values(editor)?)),
        MetadataItemId::Artist(_) => Ok(CurrentValues::Artist(artist_values(editor)?)),
    }
}
fn metadata_edit(item: MetadataItemId, editor: &Editor) -> Result<MetadataEdit, String> {
    match item {
        MetadataItemId::Track(_) => Ok(MetadataEdit::Track(TrackMetadataEdit {
            values: track_values(editor)?,
            changed: track_changed(editor),
        })),
        MetadataItemId::Album(_) => Ok(MetadataEdit::Album(AlbumMetadataEdit {
            values: album_values(editor)?,
            changed: album_changed(editor),
        })),
        MetadataItemId::Artist(_) => Ok(MetadataEdit::Artist(ArtistMetadataEdit {
            values: artist_values(editor)?,
            changed: artist_changed(editor),
        })),
    }
}

fn track_values(editor: &Editor) -> Result<TrackMetadataValues, String> {
    let MetadataDraft::Track(draft) = &editor.draft else {
        unreachable!()
    };
    let mut values = draft.source_values.clone();
    apply_text(editor, MetadataField::Title, &mut values.title)?;
    apply_optional(editor, MetadataField::SortTitle, &mut values.sort_title);
    apply_optional(editor, MetadataField::Artist, &mut values.artist);
    apply_optional(editor, MetadataField::Album, &mut values.album);
    apply_optional(editor, MetadataField::AlbumArtist, &mut values.album_artist);
    apply_number(editor, MetadataField::TrackNumber, &mut values.track_number)?;
    apply_number(editor, MetadataField::DiscNumber, &mut values.disc_number)?;
    apply_number(editor, MetadataField::Year, &mut values.year)?;
    apply_optional(editor, MetadataField::Genre, &mut values.genre);
    apply_optional(editor, MetadataField::Comment, &mut values.comment);
    apply_number(editor, MetadataField::Bpm, &mut values.bpm)?;
    apply_optional(
        editor,
        MetadataField::MusicBrainzRecordingId,
        &mut values.musicbrainz_recording_id,
    );
    apply_optional(
        editor,
        MetadataField::MusicBrainzReleaseTrackId,
        &mut values.musicbrainz_release_track_id,
    );
    apply_optional(
        editor,
        MetadataField::MusicBrainzAlbumId,
        &mut values.musicbrainz_album_id,
    );
    apply_optional(
        editor,
        MetadataField::MusicBrainzReleaseGroupId,
        &mut values.musicbrainz_release_group_id,
    );
    apply_optional(
        editor,
        MetadataField::MusicBrainzArtistId,
        &mut values.musicbrainz_artist_id,
    );
    if editor.touched.borrow().contains(&MetadataField::Locked) {
        values.locked = editor.locked.as_ref().map(adw::SwitchRow::is_active);
    }
    Ok(values)
}
fn album_values(editor: &Editor) -> Result<AlbumMetadataValues, String> {
    let MetadataDraft::Album(draft) = &editor.draft else {
        unreachable!()
    };
    let mut values = draft.source_values.clone();
    apply_text(editor, MetadataField::Title, &mut values.title)?;
    apply_optional(editor, MetadataField::SortTitle, &mut values.sort_title);
    apply_optional(editor, MetadataField::Artist, &mut values.artist);
    apply_optional(editor, MetadataField::AlbumArtist, &mut values.album_artist);
    apply_number(editor, MetadataField::Year, &mut values.year)?;
    apply_optional(editor, MetadataField::Genre, &mut values.genre);
    apply_optional(editor, MetadataField::Comment, &mut values.comment);
    apply_optional(
        editor,
        MetadataField::MusicBrainzAlbumId,
        &mut values.musicbrainz_album_id,
    );
    apply_optional(
        editor,
        MetadataField::MusicBrainzReleaseGroupId,
        &mut values.musicbrainz_release_group_id,
    );
    if editor.touched.borrow().contains(&MetadataField::Locked) {
        values.locked = editor.locked.as_ref().map(adw::SwitchRow::is_active);
    }
    Ok(values)
}
fn artist_values(editor: &Editor) -> Result<ArtistMetadataValues, String> {
    let MetadataDraft::Artist(draft) = &editor.draft else {
        unreachable!()
    };
    let mut values = draft.source_values.clone();
    apply_text(editor, MetadataField::Title, &mut values.name)?;
    apply_optional(editor, MetadataField::SortTitle, &mut values.sort_name);
    apply_optional(editor, MetadataField::Genre, &mut values.genre);
    apply_optional(editor, MetadataField::Comment, &mut values.comment);
    apply_optional(
        editor,
        MetadataField::MusicBrainzArtistId,
        &mut values.musicbrainz_artist_id,
    );
    if editor.touched.borrow().contains(&MetadataField::Locked) {
        values.locked = editor.locked.as_ref().map(adw::SwitchRow::is_active);
    }
    Ok(values)
}

fn apply_text(editor: &Editor, field: MetadataField, target: &mut String) -> Result<(), String> {
    if !editor.touched.borrow().contains(&field) {
        return Ok(());
    }
    let value = editor.entry(field).text().trim().to_string();
    if value.is_empty() {
        return Err(tr(msgid("Add a title")));
    }
    *target = value;
    Ok(())
}
fn apply_optional(editor: &Editor, field: MetadataField, target: &mut Option<String>) {
    if editor.touched.borrow().contains(&field) {
        let value = editor.entry(field).text().trim().to_string();
        *target = (!value.is_empty()).then_some(value);
    }
}
fn apply_number(
    editor: &Editor,
    field: MetadataField,
    target: &mut Option<u16>,
) -> Result<(), String> {
    if !editor.touched.borrow().contains(&field) {
        return Ok(());
    }
    let value = editor.entry(field).text().trim().to_string();
    *target = if value.is_empty() {
        None
    } else {
        Some(
            value
                .parse()
                .ok()
                .filter(|value| *value > 0)
                .ok_or_else(|| tr(msgid("Use a number above zero")))?,
        )
    };
    Ok(())
}

fn track_changed(editor: &Editor) -> TrackMetadataWritable {
    let touched = editor.touched.borrow();
    let mut changed = TrackMetadataWritable::default();
    for field in touched
        .iter()
        .copied()
        .filter(|field| editor.writable(*field))
    {
        match field {
            MetadataField::Title => changed.title = true,
            MetadataField::SortTitle => changed.sort_title = true,
            MetadataField::Artist => changed.artist = true,
            MetadataField::Album => changed.album = true,
            MetadataField::AlbumArtist => changed.album_artist = true,
            MetadataField::TrackNumber => changed.track_number = true,
            MetadataField::DiscNumber => changed.disc_number = true,
            MetadataField::Year => changed.year = true,
            MetadataField::Genre => changed.genre = true,
            MetadataField::Comment => changed.comment = true,
            MetadataField::Bpm => changed.bpm = true,
            MetadataField::MusicBrainzRecordingId => changed.musicbrainz_recording_id = true,
            MetadataField::MusicBrainzReleaseTrackId => changed.musicbrainz_release_track_id = true,
            MetadataField::MusicBrainzAlbumId => changed.musicbrainz_album_id = true,
            MetadataField::MusicBrainzReleaseGroupId => changed.musicbrainz_release_group_id = true,
            MetadataField::MusicBrainzArtistId => changed.musicbrainz_artist_id = true,
            MetadataField::Locked => changed.locked = true,
        }
    }
    changed
}
fn album_changed(editor: &Editor) -> AlbumMetadataWritable {
    let touched = editor.touched.borrow();
    let mut changed = AlbumMetadataWritable::default();
    for field in touched
        .iter()
        .copied()
        .filter(|field| editor.writable(*field))
    {
        match field {
            MetadataField::Title => changed.title = true,
            MetadataField::SortTitle => changed.sort_title = true,
            MetadataField::Artist => changed.artist = true,
            MetadataField::AlbumArtist => changed.album_artist = true,
            MetadataField::Year => changed.year = true,
            MetadataField::Genre => changed.genre = true,
            MetadataField::Comment => changed.comment = true,
            MetadataField::MusicBrainzAlbumId => changed.musicbrainz_album_id = true,
            MetadataField::MusicBrainzReleaseGroupId => changed.musicbrainz_release_group_id = true,
            MetadataField::Locked => changed.locked = true,
            _ => {}
        }
    }
    changed
}
fn artist_changed(editor: &Editor) -> ArtistMetadataWritable {
    let touched = editor.touched.borrow();
    let mut changed = ArtistMetadataWritable::default();
    for field in touched
        .iter()
        .copied()
        .filter(|field| editor.writable(*field))
    {
        match field {
            MetadataField::Title => changed.name = true,
            MetadataField::SortTitle => changed.sort_name = true,
            MetadataField::Genre => changed.genre = true,
            MetadataField::Comment => changed.comment = true,
            MetadataField::MusicBrainzArtistId => changed.musicbrainz_artist_id = true,
            MetadataField::Locked => changed.locked = true,
            _ => {}
        }
    }
    changed
}

fn track_writable(value: &TrackMetadataWritable, field: MetadataField) -> bool {
    match field {
        MetadataField::Title => value.title,
        MetadataField::SortTitle => value.sort_title,
        MetadataField::Artist => value.artist,
        MetadataField::Album => value.album,
        MetadataField::AlbumArtist => value.album_artist,
        MetadataField::TrackNumber => value.track_number,
        MetadataField::DiscNumber => value.disc_number,
        MetadataField::Year => value.year,
        MetadataField::Genre => value.genre,
        MetadataField::Comment => value.comment,
        MetadataField::Bpm => value.bpm,
        MetadataField::MusicBrainzRecordingId => value.musicbrainz_recording_id,
        MetadataField::MusicBrainzReleaseTrackId => value.musicbrainz_release_track_id,
        MetadataField::MusicBrainzAlbumId => value.musicbrainz_album_id,
        MetadataField::MusicBrainzReleaseGroupId => value.musicbrainz_release_group_id,
        MetadataField::MusicBrainzArtistId => value.musicbrainz_artist_id,
        MetadataField::Locked => value.locked,
    }
}
fn album_writable(value: &AlbumMetadataWritable, field: MetadataField) -> bool {
    match field {
        MetadataField::Title => value.title,
        MetadataField::SortTitle => value.sort_title,
        MetadataField::Artist => value.artist,
        MetadataField::AlbumArtist => value.album_artist,
        MetadataField::Year => value.year,
        MetadataField::Genre => value.genre,
        MetadataField::Comment => value.comment,
        MetadataField::MusicBrainzAlbumId => value.musicbrainz_album_id,
        MetadataField::MusicBrainzReleaseGroupId => value.musicbrainz_release_group_id,
        MetadataField::Locked => value.locked,
        _ => false,
    }
}
fn artist_writable(value: &ArtistMetadataWritable, field: MetadataField) -> bool {
    match field {
        MetadataField::Title => value.name,
        MetadataField::SortTitle => value.sort_name,
        MetadataField::Genre => value.genre,
        MetadataField::Comment => value.comment,
        MetadataField::MusicBrainzArtistId => value.musicbrainz_artist_id,
        MetadataField::Locked => value.locked,
        _ => false,
    }
}

fn track_value(values: &TrackMetadataValues, field: MetadataField) -> String {
    match field {
        MetadataField::Title => values.title.clone(),
        MetadataField::SortTitle => values.sort_title.clone().unwrap_or_default(),
        MetadataField::Artist => values.artist.clone().unwrap_or_default(),
        MetadataField::Album => values.album.clone().unwrap_or_default(),
        MetadataField::AlbumArtist => values.album_artist.clone().unwrap_or_default(),
        MetadataField::TrackNumber => values
            .track_number
            .map(|value| value.to_string())
            .unwrap_or_default(),
        MetadataField::DiscNumber => values
            .disc_number
            .map(|value| value.to_string())
            .unwrap_or_default(),
        MetadataField::Year => values
            .year
            .map(|value| value.to_string())
            .unwrap_or_default(),
        MetadataField::Genre => values.genre.clone().unwrap_or_default(),
        MetadataField::Comment => values.comment.clone().unwrap_or_default(),
        MetadataField::Bpm => values
            .bpm
            .map(|value| value.to_string())
            .unwrap_or_default(),
        MetadataField::MusicBrainzRecordingId => {
            values.musicbrainz_recording_id.clone().unwrap_or_default()
        }
        MetadataField::MusicBrainzReleaseTrackId => values
            .musicbrainz_release_track_id
            .clone()
            .unwrap_or_default(),
        MetadataField::MusicBrainzAlbumId => {
            values.musicbrainz_album_id.clone().unwrap_or_default()
        }
        MetadataField::MusicBrainzReleaseGroupId => values
            .musicbrainz_release_group_id
            .clone()
            .unwrap_or_default(),
        MetadataField::MusicBrainzArtistId => {
            values.musicbrainz_artist_id.clone().unwrap_or_default()
        }
        MetadataField::Locked => String::new(),
    }
}

fn album_value(values: &AlbumMetadataValues, field: MetadataField) -> String {
    match field {
        MetadataField::Title => values.title.clone(),
        MetadataField::SortTitle => values.sort_title.clone().unwrap_or_default(),
        MetadataField::Artist => values.artist.clone().unwrap_or_default(),
        MetadataField::AlbumArtist => values.album_artist.clone().unwrap_or_default(),
        MetadataField::Year => values
            .year
            .map(|value| value.to_string())
            .unwrap_or_default(),
        MetadataField::Genre => values.genre.clone().unwrap_or_default(),
        MetadataField::Comment => values.comment.clone().unwrap_or_default(),
        MetadataField::MusicBrainzAlbumId => {
            values.musicbrainz_album_id.clone().unwrap_or_default()
        }
        MetadataField::MusicBrainzReleaseGroupId => values
            .musicbrainz_release_group_id
            .clone()
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn artist_value(values: &ArtistMetadataValues, field: MetadataField) -> String {
    match field {
        MetadataField::Title => values.name.clone(),
        MetadataField::SortTitle => values.sort_name.clone().unwrap_or_default(),
        MetadataField::Genre => values.genre.clone().unwrap_or_default(),
        MetadataField::Comment => values.comment.clone().unwrap_or_default(),
        MetadataField::MusicBrainzArtistId => {
            values.musicbrainz_artist_id.clone().unwrap_or_default()
        }
        _ => String::new(),
    }
}

fn apply_track_values(editor: &Editor, values: &TrackMetadataValues) {
    set_identified(editor, MetadataField::Title, &values.title);
    set_identified_optional(
        editor,
        MetadataField::SortTitle,
        values.sort_title.as_deref(),
    );
    set_identified_optional(editor, MetadataField::Artist, values.artist.as_deref());
    set_identified_optional(editor, MetadataField::Album, values.album.as_deref());
    set_identified_optional(
        editor,
        MetadataField::AlbumArtist,
        values.album_artist.as_deref(),
    );
    set_identified_number(editor, MetadataField::TrackNumber, values.track_number);
    set_identified_number(editor, MetadataField::DiscNumber, values.disc_number);
    set_identified_number(editor, MetadataField::Year, values.year);
    set_identified_optional(editor, MetadataField::Genre, values.genre.as_deref());
    set_identified_optional(editor, MetadataField::Comment, values.comment.as_deref());
    set_identified_number(editor, MetadataField::Bpm, values.bpm);
    set_identified_optional(
        editor,
        MetadataField::MusicBrainzRecordingId,
        values.musicbrainz_recording_id.as_deref(),
    );
    set_identified_optional(
        editor,
        MetadataField::MusicBrainzReleaseTrackId,
        values.musicbrainz_release_track_id.as_deref(),
    );
    set_identified_optional(
        editor,
        MetadataField::MusicBrainzAlbumId,
        values.musicbrainz_album_id.as_deref(),
    );
    set_identified_optional(
        editor,
        MetadataField::MusicBrainzReleaseGroupId,
        values.musicbrainz_release_group_id.as_deref(),
    );
    set_identified_optional(
        editor,
        MetadataField::MusicBrainzArtistId,
        values.musicbrainz_artist_id.as_deref(),
    );
}
fn apply_album_values(editor: &Editor, values: &AlbumMetadataValues) {
    set_identified(editor, MetadataField::Title, &values.title);
    set_identified_optional(
        editor,
        MetadataField::SortTitle,
        values.sort_title.as_deref(),
    );
    set_identified_optional(editor, MetadataField::Artist, values.artist.as_deref());
    set_identified_optional(
        editor,
        MetadataField::AlbumArtist,
        values.album_artist.as_deref(),
    );
    set_identified_number(editor, MetadataField::Year, values.year);
    set_identified_optional(editor, MetadataField::Genre, values.genre.as_deref());
    set_identified_optional(editor, MetadataField::Comment, values.comment.as_deref());
    set_identified_optional(
        editor,
        MetadataField::MusicBrainzAlbumId,
        values.musicbrainz_album_id.as_deref(),
    );
    set_identified_optional(
        editor,
        MetadataField::MusicBrainzReleaseGroupId,
        values.musicbrainz_release_group_id.as_deref(),
    );
}
fn apply_artist_values(editor: &Editor, values: &ArtistMetadataValues) {
    set_identified(editor, MetadataField::Title, &values.name);
    set_identified_optional(
        editor,
        MetadataField::SortTitle,
        values.sort_name.as_deref(),
    );
    set_identified_optional(editor, MetadataField::Genre, values.genre.as_deref());
    set_identified_optional(editor, MetadataField::Comment, values.comment.as_deref());
    set_identified_optional(
        editor,
        MetadataField::MusicBrainzArtistId,
        values.musicbrainz_artist_id.as_deref(),
    );
}
fn set_identified(editor: &Editor, field: MetadataField, value: &str) {
    if editor.writable(field) {
        let current = editor.entry(field).text().to_string();
        if current == value {
            return;
        }
        editor
            .identified_originals
            .borrow_mut()
            .entry(field)
            .or_insert(current);
        editor.entry(field).set_text(value);
        refresh_identified_field(editor, field);
    }
}
fn set_identified_optional(editor: &Editor, field: MetadataField, value: Option<&str>) {
    set_identified(editor, field, value.unwrap_or_default());
}
fn set_identified_number(editor: &Editor, field: MetadataField, value: Option<u16>) {
    set_identified(
        editor,
        field,
        &value.map(|value| value.to_string()).unwrap_or_default(),
    );
}

enum IdentifyReceiver {
    Track(async_channel::Receiver<Result<Option<(TrackMetadataValues, Option<String>)>, String>>),
    Album(async_channel::Receiver<Result<Option<(AlbumMetadataValues, Option<String>)>, String>>),
    Artist(async_channel::Receiver<Result<Option<(ArtistMetadataValues, Option<String>)>, String>>),
}
enum Identified {
    Track(TrackMetadataValues, Option<String>),
    Album(AlbumMetadataValues, Option<String>),
    Artist(ArtistMetadataValues, Option<String>),
}
impl IdentifyReceiver {
    async fn recv(self) -> Result<Option<Identified>, String> {
        match self {
            Self::Track(receiver) => receiver
                .recv()
                .await
                .map_err(|_| tr(msgid("Metadata editing is no longer available")))?
                .map(|value| value.map(|(values, token)| Identified::Track(values, token))),
            Self::Album(receiver) => receiver
                .recv()
                .await
                .map_err(|_| tr(msgid("Metadata editing is no longer available")))?
                .map(|value| value.map(|(values, token)| Identified::Album(values, token))),
            Self::Artist(receiver) => receiver
                .recv()
                .await
                .map_err(|_| tr(msgid("Metadata editing is no longer available")))?
                .map(|value| value.map(|(values, token)| Identified::Artist(values, token))),
        }
    }
}

enum SaveReceiver {
    Track(async_channel::Receiver<Result<(), SourceMetadataError>>),
    Album(async_channel::Receiver<Result<(), SourceMetadataError>>),
    Artist(async_channel::Receiver<Result<(), SourceMetadataError>>),
}
impl SaveReceiver {
    async fn recv(self) -> Result<(), SourceMetadataError> {
        match self {
            Self::Track(receiver) | Self::Album(receiver) | Self::Artist(receiver) => receiver
                .recv()
                .await
                .map_err(|_| SourceMetadataError::Unavailable)?,
        }
    }
}

fn selected_metadata_source_is_current(
    shell: &Shell,
    expected: &crate::runtime::SelectedLibrary,
) -> bool {
    shell.selected_library().as_deref().is_some_and(|selected| {
        metadata_source_matches(
            expected.source_key,
            expected.source_session_epoch,
            selected.source_key,
            selected.source_session_epoch,
        )
    })
}

fn metadata_source_matches(
    expected_source: library::SourceKey,
    expected_epoch: playback::SourceSessionEpoch,
    actual_source: library::SourceKey,
    actual_epoch: playback::SourceSessionEpoch,
) -> bool {
    expected_source == actual_source && expected_epoch == actual_epoch
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_completion_requires_the_same_source_session() {
        let source = library::SourceKey::from_raw(1);
        let epoch = playback::SourceSessionEpoch::new(7);
        assert!(metadata_source_matches(source, epoch, source, epoch));
        assert!(!metadata_source_matches(
            source,
            epoch,
            library::SourceKey::from_raw(2),
            epoch
        ));
        assert!(!metadata_source_matches(
            source,
            epoch,
            source,
            playback::SourceSessionEpoch::new(8)
        ));
    }

    #[test]
    fn private_mode_keeps_provider_source_search_available() {
        let values = CurrentValues::Track(TrackMetadataValues {
            title: "Track".to_string(),
            ..TrackMetadataValues::default()
        });
        assert!(identification_available(true, false, &values));
        assert!(!identification_available(false, false, &values));
    }
}
