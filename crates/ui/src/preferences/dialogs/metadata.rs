use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use adw::prelude::*;
use library::{
    MetadataChange, MetadataDraft, MetadataEdit, MetadataError, MetadataField,
    MetadataIdentification, MetadataItemId, MetadataScope, MetadataValues, SourceId,
};
use localization::{msgid, tr, trn_with};
use playback::SourceSessionEpoch;
use tracing::warn;

use crate::layout::{large_popup_content_height, large_popup_content_width};
use crate::preferences::source::field_layout::{
    compact_field_row_group, install_compact_field_row_responsiveness_at, style_compact_field_row,
};
use crate::shell::Shell;

const EDITOR_WIDTH: i32 = 650;
const EDITOR_MAX_HEIGHT: i32 = 720;
const EDITOR_FIELD_STACK_WIDTH: i32 = 520;
const FIELD_COLUMN_SPACING: i32 = 18;
const FIELD_ROW_SPACING: i32 = 14;

#[derive(Clone)]
struct MetadataSource {
    source_id: SourceId,
    source_session_epoch: SourceSessionEpoch,
}

impl MetadataSource {
    fn new(selected: &crate::runtime::SelectedLibrary) -> Self {
        Self {
            source_id: selected.source_id.clone(),
            source_session_epoch: selected.source_session_epoch,
        }
    }

    fn current(&self, shell: &Shell) -> Option<crate::runtime::SelectedLibrary> {
        shell
            .selected_library()
            .as_deref()
            .filter(|selected| {
                selected.source_id == self.source_id
                    && selected.source_session_epoch == self.source_session_epoch
            })
            .cloned()
    }
}

pub(crate) fn present_metadata_dialog(shell: &Rc<Shell>, item_id: MetadataItemId) {
    let source = shell.selected_library().as_deref().map(MetadataSource::new);
    let Some(source) = source else {
        return;
    };
    present_metadata_dialog_for_source(shell, source, item_id);
}

fn present_metadata_dialog_for_source(
    shell: &Rc<Shell>,
    source: MetadataSource,
    item_id: MetadataItemId,
) {
    let Some(selected) = source.current(shell) else {
        return;
    };
    let receiver = selected.operations.metadata(item_id.clone());
    let shell = Rc::downgrade(shell);
    gtk::glib::spawn_future_local(async move {
        let response = receiver.recv().await;
        let Some(shell) = shell.upgrade() else {
            return;
        };
        let Some(selected) = source.current(&shell) else {
            return;
        };
        match response {
            Ok(Ok(draft)) => build_dialog(&shell, source, draft),
            Ok(Err(MetadataError::LocalAccessRequired { source_path })) => {
                present_local_access_recovery(
                    &shell,
                    source,
                    selected,
                    item_id,
                    source_path.as_str(),
                );
            }
            Ok(Err(error)) => present_metadata_error(&shell, &error.to_string()),
            Err(_) => {
                present_metadata_error(&shell, &tr("Metadata editing is no longer available"))
            }
        }
    });
}

fn present_local_access_recovery(
    shell: &Rc<Shell>,
    source: MetadataSource,
    selected: crate::runtime::SelectedLibrary,
    item_id: MetadataItemId,
    source_path: &str,
) {
    let dialog = adw::Dialog::builder()
        .title(tr("Edit metadata"))
        .content_width(large_popup_content_width(EDITOR_WIDTH))
        .build();
    dialog.add_css_class("preferences");
    let dismissed = Rc::new(Cell::new(false));
    dialog.connect_closed({
        let dismissed = Rc::clone(&dismissed);
        move |_| dismissed.set(true)
    });

    let header = adw::HeaderBar::new();
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(true);
    header.set_title_widget(Some(&adw::WindowTitle::new(&tr("Edit metadata"), "")));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);

    let shell_for_reload = Rc::downgrade(shell);
    let source_for_reload = source.clone();
    let item_id_for_reload = item_id.clone();
    let dialog_for_reload = dialog.downgrade();
    let dismissed_for_reload = Rc::clone(&dismissed);
    let on_success: Rc<dyn Fn()> = Rc::new(move || {
        if dismissed_for_reload.get() {
            return;
        }
        let Some(dialog) = dialog_for_reload.upgrade() else {
            return;
        };
        let Some(shell) = shell_for_reload.upgrade() else {
            return;
        };
        if source_for_reload.current(&shell).is_none() {
            return;
        }
        dialog.force_close();
        present_metadata_dialog_for_source(
            &shell,
            source_for_reload.clone(),
            item_id_for_reload.clone(),
        );
    });
    let fields = crate::preferences::source::local_access::metadata_local_access_recovery_form(
        shell,
        source_path,
        &selected,
        item_id,
        on_success,
    );
    toolbar.set_content(Some(&fields));
    dialog.set_child(Some(&toolbar));
    shell.present_selected_dialog(&dialog);
}

#[derive(Default)]
struct MetadataRows {
    fields: Vec<MetadataRow>,
    lock_data: Option<adw::SwitchRow>,
    touched: RefCell<HashSet<MetadataField>>,
}

struct MetadataRow {
    field: MetadataField,
    entry: adw::EntryRow,
    undo: gtk::Button,
}

pub(crate) struct EditorState {
    draft: MetadataDraft,
    rows: MetadataRows,
    dialog: adw::Dialog,
    save: gtk::Button,
    identify: gtk::Button,
    cancel: gtk::Button,
    status: gtk::Label,
    external_lookup_allowed: bool,
    saving: Cell<bool>,
    identifying: Cell<bool>,
    identified_edits: RefCell<HashMap<MetadataField, IdentifiedEdit>>,
    identification: RefCell<Option<MetadataIdentification>>,
}

impl EditorState {
    pub(crate) fn dialog(&self) -> &adw::Dialog {
        &self.dialog
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct IdentifiedEdit {
    original: String,
    identified: String,
    was_touched: bool,
}

fn present_metadata_error(shell: &Rc<Shell>, message: &str) {
    let dialog = adw::Dialog::builder()
        .title(tr("Edit metadata"))
        .content_width(large_popup_content_width(480))
        .build();
    dialog.add_css_class("preferences");

    let header = adw::HeaderBar::new();
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(true);
    header.set_title_widget(Some(&adw::WindowTitle::new(&tr("Edit metadata"), "")));

    let message = gtk::Label::new(Some(message));
    message.set_halign(gtk::Align::Start);
    message.set_wrap(true);
    message.add_css_class("error");
    message.set_margin_start(24);
    message.set_margin_end(24);
    message.set_margin_top(18);
    message.set_margin_bottom(24);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&message));
    dialog.set_child(Some(&toolbar));
    shell.present_selected_dialog(&dialog);
}

fn build_dialog(shell: &Rc<Shell>, source: MetadataSource, draft: MetadataDraft) {
    let dialog = adw::Dialog::builder()
        .title(tr("Edit metadata"))
        .content_width(large_popup_content_width(EDITOR_WIDTH))
        .build();
    dialog.add_css_class("preferences");

    let identify = gtk::Button::with_label(&tr("Identify"));
    identify.add_css_class("destructive-action");
    let external_lookup_allowed = shell
        .settings
        .current
        .borrow()
        .allows_external_metadata_lookup();

    let header = adw::HeaderBar::new();
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(true);
    header.set_title_widget(Some(&adw::WindowTitle::new(&tr("Edit metadata"), "")));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);

    let cancel = gtk::Button::with_label(&tr("Cancel"));
    let save = gtk::Button::with_label(&tr("Save"));
    save.add_css_class("suggested-action");
    save.set_sensitive(false);

    let bottom_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    bottom_actions.set_halign(gtk::Align::End);
    bottom_actions.append(&cancel);
    bottom_actions.append(&save);

    let (fields, rows) = metadata_rows(&draft, &identify);

    let fields_clamp = adw::Clamp::new();
    fields_clamp.set_maximum_size(EDITOR_WIDTH);
    fields_clamp.set_tightening_threshold(EDITOR_FIELD_STACK_WIDTH);
    fields_clamp.set_margin_top(1);
    fields_clamp.set_margin_start(24);
    fields_clamp.set_margin_end(24);
    fields_clamp.set_child(Some(&fields));

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroller.set_propagate_natural_height(true);
    scroller.set_vexpand(false);
    let max_height = large_popup_content_height(shell.chrome.window.height(), EDITOR_MAX_HEIGHT);
    scroller.set_max_content_height(max_height.saturating_sub(64));
    scroller.set_child(Some(&fields_clamp));

    let status = gtk::Label::new(None);
    status.set_halign(gtk::Align::Start);
    status.set_wrap(true);
    status.add_css_class("error");
    status.set_visible(false);
    let footer = gtk::Box::new(gtk::Orientation::Vertical, 12);
    footer.append(&status);
    footer.append(&bottom_actions);
    let footer_clamp = adw::Clamp::new();
    footer_clamp.set_maximum_size(EDITOR_WIDTH);
    footer_clamp.set_margin_start(24);
    footer_clamp.set_margin_end(24);
    footer_clamp.set_margin_bottom(14);
    footer_clamp.set_child(Some(&footer));

    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.append(&scroller);
    body.append(&footer_clamp);
    toolbar.set_content(Some(&body));
    dialog.set_child(Some(&toolbar));

    let state = Rc::new(EditorState {
        draft,
        rows,
        dialog: dialog.clone(),
        save,
        identify,
        cancel,
        status,
        external_lookup_allowed,
        saving: Cell::new(false),
        identifying: Cell::new(false),
        identified_edits: RefCell::new(HashMap::new()),
        identification: RefCell::new(None),
    });
    connect_field_changes(&state);
    connect_cancel(&state);
    connect_identify(shell, source.clone(), &state);
    connect_save(shell, source, &state);
    refresh_save_state(&state);
    shell.own_selected_metadata_editor(Rc::clone(&state));
    shell.present_selected_dialog(&dialog);
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

fn metadata_rows(draft: &MetadataDraft, identify: &gtk::Button) -> (gtk::Box, MetadataRows) {
    let mut rows = MetadataRows::default();
    let fields = gtk::Box::new(gtk::Orientation::Vertical, FIELD_ROW_SPACING);
    fields.set_hexpand(true);

    let identify_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    identify_actions.set_hexpand(true);
    if let MetadataScope::Tracks(count) = draft.scope {
        let count_text = count.to_string();
        let scope = gtk::Label::new(Some(&trn_with(
            "Changes apply to {count} track",
            "Changes apply to {count} tracks",
            count as u64,
            &[("count", count_text.as_str())],
        )));
        scope.set_halign(gtk::Align::Start);
        scope.set_hexpand(true);
        scope.add_css_class("dim-label");
        identify_actions.append(&scope);
    } else {
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        identify_actions.append(&spacer);
    }
    identify_actions.append(identify);
    fields.append(&identify_actions);

    for layout in field_layout(&draft.item_id) {
        match layout {
            FieldLayout::Pair(left, right) => {
                let left = metadata_field(&mut rows, draft, *left);
                let right = metadata_field(&mut rows, draft, *right);
                let pair = gtk::Box::new(gtk::Orientation::Horizontal, FIELD_COLUMN_SPACING);
                pair.set_homogeneous(true);
                pair.set_hexpand(true);
                pair.append(&compact_field_row_group(&left));
                pair.append(&compact_field_row_group(&right));
                fields.append(&install_compact_field_row_responsiveness_at(
                    &pair,
                    EDITOR_FIELD_STACK_WIDTH,
                ));
            }
            FieldLayout::Full(field) => {
                let row = metadata_field(&mut rows, draft, *field);
                fields.append(&compact_field_row_group(&row));
            }
            FieldLayout::Lock => {
                if !draft.editing.includes(MetadataField::LockData) {
                    continue;
                }
                let row = adw::SwitchRow::builder()
                    .title(tr("Lock metadata"))
                    .subtitle(tr(
                        "Prevent automatic metadata refreshes from replacing these values",
                    ))
                    .active(draft.values.lock_data.unwrap_or(false))
                    .build();
                style_compact_field_row(&row);
                fields.append(&compact_field_row_group(&row));
                rows.lock_data = Some(row);
            }
        }
    }

    (fields, rows)
}

fn field_layout(item_id: &MetadataItemId) -> &'static [FieldLayout] {
    match item_id {
        MetadataItemId::Track(_) => TRACK_LAYOUT,
        MetadataItemId::Album(_) => ALBUM_LAYOUT,
        MetadataItemId::Artist(_) => ARTIST_LAYOUT,
    }
}

fn metadata_field(
    rows: &mut MetadataRows,
    draft: &MetadataDraft,
    field: MetadataField,
) -> adw::EntryRow {
    let value = field_value(&draft.values, field).unwrap_or_default();
    let mut title = tr(field_title(&draft.item_id, field));
    if draft.mixed_fields.contains(&field) {
        title = format!("{title} · {}", tr("Multiple values"));
    }
    let row = adw::EntryRow::builder().title(title).text(value).build();
    if is_number_field(field) {
        row.set_input_purpose(gtk::InputPurpose::Digits);
    }
    style_compact_field_row(&row);
    set_editable(&row, draft, field);

    let undo = gtk::Button::from_icon_name("rufin-edit-undo-symbolic");
    undo.add_css_class("flat");
    undo.set_tooltip_text(Some(&tr("Undo identified value")));
    undo.update_property(&[gtk::accessible::Property::Label(&tr(
        "Undo identified value",
    ))]);
    undo.set_valign(gtk::Align::Center);
    undo.set_visible(false);
    row.add_suffix(&undo);

    rows.fields.push(MetadataRow {
        field,
        entry: row.clone(),
        undo,
    });
    row
}

fn field_title(item_id: &MetadataItemId, field: MetadataField) -> &'static str {
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
        MetadataField::Comment if matches!(item_id, MetadataItemId::Track(_)) => msgid("Comment"),
        MetadataField::Comment => msgid("Overview"),
        MetadataField::Bpm => msgid("BPM"),
        MetadataField::MusicBrainzRecordingId => msgid("MusicBrainz recording ID"),
        MetadataField::MusicBrainzReleaseTrackId => msgid("MusicBrainz release track ID"),
        MetadataField::MusicBrainzAlbumId => msgid("MusicBrainz release ID"),
        MetadataField::MusicBrainzReleaseGroupId => msgid("MusicBrainz release group ID"),
        MetadataField::MusicBrainzArtistId => msgid("MusicBrainz artist ID"),
        MetadataField::LockData => unreachable!("lock metadata uses a SwitchRow"),
    }
}

fn is_number_field(field: MetadataField) -> bool {
    matches!(
        field,
        MetadataField::TrackNumber
            | MetadataField::DiscNumber
            | MetadataField::Year
            | MetadataField::Bpm
    )
}

fn field_value(values: &MetadataValues, field: MetadataField) -> Option<String> {
    match field {
        MetadataField::Title => Some(values.title.clone()),
        MetadataField::SortTitle => values.sort_title.clone(),
        MetadataField::Artist => values.artist.clone(),
        MetadataField::Album => values.album.clone(),
        MetadataField::AlbumArtist => values.album_artist.clone(),
        MetadataField::TrackNumber => values.track_number.map(|value| value.to_string()),
        MetadataField::DiscNumber => values.disc_number.map(|value| value.to_string()),
        MetadataField::Year => values.year.map(|value| value.to_string()),
        MetadataField::Genre => values.genre.clone(),
        MetadataField::Comment => values.comment.clone(),
        MetadataField::Bpm => values.bpm.map(|value| value.to_string()),
        MetadataField::MusicBrainzRecordingId => values.musicbrainz_recording_id.clone(),
        MetadataField::MusicBrainzReleaseTrackId => values.musicbrainz_release_track_id.clone(),
        MetadataField::MusicBrainzAlbumId => values.musicbrainz_album_id.clone(),
        MetadataField::MusicBrainzReleaseGroupId => values.musicbrainz_release_group_id.clone(),
        MetadataField::MusicBrainzArtistId => values.musicbrainz_artist_id.clone(),
        MetadataField::LockData => None,
    }
}

fn set_editable(widget: &impl IsA<gtk::Widget>, draft: &MetadataDraft, field: MetadataField) {
    let editable = draft.editing.includes(field);
    widget.set_sensitive(editable);
    if !editable {
        widget.set_tooltip_text(Some(&tr("This source cannot edit this field")));
    }
}

fn connect_field_changes(state: &Rc<EditorState>) {
    for metadata_row in &state.rows.fields {
        let field = metadata_row.field;
        let row = metadata_row.entry.clone();
        let state = Rc::downgrade(state);
        row.connect_changed(move |_| {
            let Some(state) = state.upgrade() else {
                return;
            };
            state.rows.touched.borrow_mut().insert(field);
            refresh_identify_undo_field(&state, field);
            refresh_save_state(&state);
        });
    }
    for metadata_row in &state.rows.fields {
        let field = metadata_row.field;
        let undo = metadata_row.undo.clone();
        let state = Rc::downgrade(state);
        undo.connect_clicked(move |_| {
            let Some(state) = state.upgrade() else {
                return;
            };
            let edit = state.identified_edits.borrow_mut().remove(&field);
            if let (Some(row), Some(edit)) = (state.rows.entry(field), edit) {
                row.set_text(&edit.original);
                restore_touched_state(
                    &mut state.rows.touched.borrow_mut(),
                    field,
                    edit.was_touched,
                );
            }
            if state.identified_edits.borrow().is_empty() {
                state.identification.borrow_mut().take();
            }
            refresh_identify_undo(&state);
            refresh_save_state(&state);
        });
    }
    if let Some(row) = &state.rows.lock_data {
        let state = Rc::downgrade(state);
        row.connect_active_notify(move |_| {
            if let Some(state) = state.upgrade() {
                refresh_save_state(&state);
            }
        });
    }
}

impl MetadataRows {
    fn set_sensitive(&self, sensitive: bool, editing: &library::MetadataEditing) {
        for row in &self.fields {
            row.entry
                .set_sensitive(sensitive && editing.includes(row.field));
        }
        if let Some(row) = &self.lock_data {
            row.set_sensitive(sensitive && editing.includes(MetadataField::LockData));
        }
    }

    fn field_entries(&self) -> impl Iterator<Item = (MetadataField, &adw::EntryRow)> {
        self.fields.iter().map(|row| (row.field, &row.entry))
    }

    fn entry(&self, field: MetadataField) -> Option<&adw::EntryRow> {
        self.row(field).map(|row| &row.entry)
    }

    fn row(&self, field: MetadataField) -> Option<&MetadataRow> {
        self.fields.iter().find(|row| row.field == field)
    }
}

fn refresh_save_state(state: &EditorState) {
    let identification = state.identification.borrow();
    let edit = metadata_edit(&state.draft, &state.rows, identification.as_ref());
    let can_save = match edit {
        Ok(edit) => !edit.changes.is_empty() || edit.application.is_some(),
        Err(_) => true,
    };
    state.dialog.set_can_close(!state.saving.get());
    state
        .save
        .set_sensitive(can_save && !state.saving.get() && !state.identifying.get());
    let can_identify = identification_available(
        &state.draft.item_id,
        state.draft.source_search,
        state.external_lookup_allowed,
        &identification_values(state),
    );
    state
        .identify
        .set_sensitive(can_identify && !state.saving.get() && !state.identifying.get());
}

fn refresh_identify_undo(state: &EditorState) {
    if state.identifying.get() {
        return;
    }
    let identified = state.identified_edits.borrow();
    for row in &state.rows.fields {
        let visible = identified
            .get(&row.field)
            .is_some_and(|edit| edit.identified != edit.original);
        row.undo.set_visible(visible);
        if visible {
            row.entry.add_css_class("metadata-identified-change");
        } else {
            row.entry.remove_css_class("metadata-identified-change");
        }
    }
}

fn refresh_identify_undo_field(state: &EditorState, field: MetadataField) {
    if state.identifying.get() {
        return;
    }
    let Some(row) = state.rows.row(field) else {
        return;
    };
    let mut identified = state.identified_edits.borrow_mut();
    if identified
        .get(&field)
        .is_some_and(|edit| row.entry.text().as_str() == edit.original)
    {
        identified.remove(&field);
    }
    let visible = identified
        .get(&field)
        .is_some_and(|edit| edit.identified != edit.original);
    row.undo.set_visible(visible);
    if visible {
        row.entry.add_css_class("metadata-identified-change");
    } else {
        row.entry.remove_css_class("metadata-identified-change");
    }
    let identification_undone = identified.is_empty();
    drop(identified);
    if identification_undone {
        state.identification.borrow_mut().take();
    }
}

fn connect_cancel(state: &Rc<EditorState>) {
    let dialog = state.dialog.downgrade();
    state.cancel.connect_clicked(move |_| {
        if let Some(dialog) = dialog.upgrade() {
            dialog.close();
        }
    });
}

fn connect_save(shell: &Rc<Shell>, source: MetadataSource, state: &Rc<EditorState>) {
    let shell = Rc::downgrade(shell);
    let state_for_save = Rc::downgrade(state);
    state.save.connect_clicked(move |_| {
        let (Some(shell), Some(state_for_save)) = (shell.upgrade(), state_for_save.upgrade())
        else {
            return;
        };
        let identification = state_for_save.identification.borrow();
        let edit = match metadata_edit(
            &state_for_save.draft,
            &state_for_save.rows,
            identification.as_ref(),
        ) {
            Ok(edit) if !edit.changes.is_empty() || edit.application.is_some() => edit,
            Ok(_) => return,
            Err(error) => {
                show_error(&state_for_save, &error);
                return;
            }
        };
        state_for_save.saving.set(true);
        state_for_save.save.set_sensitive(false);
        state_for_save.cancel.set_sensitive(false);
        state_for_save.identify.set_sensitive(false);
        state_for_save
            .rows
            .set_sensitive(false, &state_for_save.draft.editing);
        state_for_save.save.set_label(&tr("Saving..."));
        let Some(selected) = source.current(&shell) else {
            state_for_save.dialog.force_close();
            return;
        };
        let receiver = selected.operations.edit_metadata(edit);
        let state = Rc::downgrade(&state_for_save);
        gtk::glib::spawn_future_local(async move {
            let response = receiver.recv().await;
            let Some(state) = state.upgrade() else {
                return;
            };
            match response {
                Ok(Ok(())) => state.dialog.force_close(),
                Ok(Err(error @ MetadataError::SavedRefreshFailed(_))) => {
                    finish_committed_save(&state, &error.to_string())
                }
                Ok(Err(error)) => finish_failed_save(&state, &error.to_string()),
                Err(_) => {
                    finish_failed_save(&state, &tr("Metadata editing is no longer available"))
                }
            }
        });
    });
}

fn connect_identify(shell: &Rc<Shell>, source: MetadataSource, state: &Rc<EditorState>) {
    let shell = Rc::downgrade(shell);
    let state_for_identify = Rc::downgrade(state);
    state.identify.connect_clicked(move |_| {
        let (Some(shell), Some(state_for_identify)) =
            (shell.upgrade(), state_for_identify.upgrade())
        else {
            return;
        };
        let values = identification_values(&state_for_identify);
        if !identification_available(
            &state_for_identify.draft.item_id,
            state_for_identify.draft.source_search,
            state_for_identify.external_lookup_allowed,
            &values,
        ) {
            return;
        }
        let before = row_snapshot(&state_for_identify.rows);
        let touched_before = state_for_identify.rows.touched.borrow().clone();
        state_for_identify.identifying.set(true);
        state_for_identify.identify.set_sensitive(false);
        state_for_identify.save.set_sensitive(false);
        state_for_identify.identify.set_label(&tr("Identifying..."));
        state_for_identify
            .rows
            .set_sensitive(false, &state_for_identify.draft.editing);
        let Some(selected) = source.current(&shell) else {
            state_for_identify.dialog.force_close();
            return;
        };
        let receiver = selected.operations.identify_metadata(
            state_for_identify.draft.item_id.clone(),
            state_for_identify.draft.editing.clone(),
            values,
        );
        let state = Rc::downgrade(&state_for_identify);
        gtk::glib::spawn_future_local(async move {
            let response = receiver.recv().await;
            let Some(state) = state.upgrade() else {
                return;
            };
            match response {
                Ok(Ok(Some(identification))) => {
                    apply_identification(&state.rows, &state.draft.editing, &identification.values);
                    remember_identified_changes(&state, &before, &touched_before);
                    *state.identification.borrow_mut() = Some(identification);
                }
                Ok(Ok(None)) => {}
                Ok(Err(error)) => warn!(%error, "metadata identification failed"),
                Err(error) => warn!(%error, "metadata identification ended before completion"),
            }
            state.identifying.set(false);
            state.rows.set_sensitive(true, &state.draft.editing);
            state.identify.set_label(&tr("Identify"));
            refresh_identify_undo(&state);
            refresh_save_state(&state);
        });
    });
}

fn row_snapshot(rows: &MetadataRows) -> HashMap<MetadataField, String> {
    rows.field_entries()
        .map(|(field, row)| (field, row.text().to_string()))
        .collect()
}

fn remember_identified_changes(
    state: &EditorState,
    before: &HashMap<MetadataField, String>,
    touched_before: &HashSet<MetadataField>,
) {
    let after = row_snapshot(&state.rows);
    merge_identified_edits(
        &mut state.identified_edits.borrow_mut(),
        before,
        &after,
        touched_before,
        &state.draft.editing,
    );
}

fn merge_identified_edits(
    edits: &mut HashMap<MetadataField, IdentifiedEdit>,
    before: &HashMap<MetadataField, String>,
    after: &HashMap<MetadataField, String>,
    touched_before: &HashSet<MetadataField>,
    editing: &library::MetadataEditing,
) {
    for (field, previous) in before {
        let Some(identified) = after.get(field).filter(|value| *value != previous) else {
            continue;
        };
        if editing.includes(*field) {
            edits
                .entry(*field)
                .and_modify(|edit| edit.identified.clone_from(identified))
                .or_insert_with(|| IdentifiedEdit {
                    original: previous.clone(),
                    identified: identified.clone(),
                    was_touched: touched_before.contains(field),
                });
        }
    }
}

fn restore_touched_state(
    touched: &mut HashSet<MetadataField>,
    field: MetadataField,
    was_touched: bool,
) {
    if was_touched {
        touched.insert(field);
    } else {
        touched.remove(&field);
    }
}

fn show_error(state: &EditorState, error: &str) {
    state.status.set_text(error);
    state.status.set_visible(true);
}

fn identification_values(state: &EditorState) -> MetadataValues {
    let mut values = state.draft.values.clone();
    for (field, row) in state.rows.field_entries() {
        apply_identification_input(&mut values, field, row.text().as_str());
    }
    if let Some(lock_data) = &state.rows.lock_data {
        values.lock_data = Some(lock_data.is_active());
    }
    values
}

fn apply_identification_input(values: &mut MetadataValues, field: MetadataField, input: &str) {
    let text = || normalized_text(input);
    let number = || input.trim().parse::<u16>().ok().filter(|value| *value > 0);
    match field {
        MetadataField::Title => values.title = input.trim().to_string(),
        MetadataField::SortTitle => values.sort_title = text(),
        MetadataField::Artist => values.artist = text(),
        MetadataField::Album => values.album = text(),
        MetadataField::AlbumArtist => values.album_artist = text(),
        MetadataField::TrackNumber => values.track_number = number(),
        MetadataField::DiscNumber => values.disc_number = number(),
        MetadataField::Year => values.year = number(),
        MetadataField::Genre => values.genre = text(),
        MetadataField::Comment => values.comment = text(),
        MetadataField::Bpm => values.bpm = number(),
        MetadataField::MusicBrainzRecordingId => values.musicbrainz_recording_id = text(),
        MetadataField::MusicBrainzReleaseTrackId => {
            values.musicbrainz_release_track_id = text();
        }
        MetadataField::MusicBrainzAlbumId => values.musicbrainz_album_id = text(),
        MetadataField::MusicBrainzReleaseGroupId => values.musicbrainz_release_group_id = text(),
        MetadataField::MusicBrainzArtistId => values.musicbrainz_artist_id = text(),
        MetadataField::LockData => {}
    }
}

fn identification_available(
    item_id: &MetadataItemId,
    source_search: bool,
    external_lookup_allowed: bool,
    values: &MetadataValues,
) -> bool {
    external_lookup_allowed && item_id.has_exact_musicbrainz_identity(values)
        || source_search && !values.title.trim().is_empty()
}

fn apply_identification(
    rows: &MetadataRows,
    editing: &library::MetadataEditing,
    values: &MetadataValues,
) {
    for (field, row) in rows.field_entries() {
        let value = field_value(values, field)
            .filter(|value| field != MetadataField::Title || !value.is_empty());
        if let Some(value) = value
            && editing.includes(field)
        {
            row.set_text(&value);
        }
    }
}

fn finish_failed_save(state: &EditorState, error: &str) {
    state.saving.set(false);
    state.cancel.set_sensitive(true);
    state.identify.set_sensitive(true);
    state.rows.set_sensitive(true, &state.draft.editing);
    state.save.set_label(&tr("Save"));
    refresh_save_state(state);
    show_error(state, error);
}

fn finish_committed_save(state: &EditorState, error: &str) {
    state.saving.set(false);
    state.dialog.set_can_close(true);
    state.cancel.set_sensitive(true);
    state.cancel.set_label(&tr("Close"));
    state.identify.set_sensitive(false);
    state.rows.set_sensitive(false, &state.draft.editing);
    state.save.set_label(&tr("Saved"));
    state.save.set_sensitive(false);
    state.status.set_text(error);
    state.status.set_visible(true);
}

fn metadata_edit(
    draft: &MetadataDraft,
    rows: &MetadataRows,
    identification: Option<&MetadataIdentification>,
) -> Result<MetadataEdit, String> {
    let compared = identification
        .filter(|identification| identification.application.is_some())
        .map_or(&draft.values, |identification| &identification.values);
    let mut changes = Vec::new();
    for (field, row) in rows.field_entries() {
        if !draft.editing.includes(field) {
            continue;
        }
        let touched = rows.touched.borrow().contains(&field);
        if draft.mixed_fields.contains(&field) && !touched {
            continue;
        }
        let force = draft.mixed_fields.contains(&field) && touched;
        if let Some(change) = metadata_change(compared, field, row, force)? {
            changes.push(change);
        }
    }
    if let Some(row) = &rows.lock_data
        && draft.editing.includes(MetadataField::LockData)
        && Some(row.is_active()) != compared.lock_data
    {
        changes.push(MetadataChange::LockData(row.is_active()));
    }
    let edit = MetadataEdit {
        item_id: draft.item_id.clone(),
        revision: draft.revision.clone(),
        application: identification.and_then(|identification| identification.application.clone()),
        changes,
    };
    edit.validate(&draft.editing)
        .map_err(|error| tr(&error.to_string()))?;
    Ok(edit)
}

fn metadata_change(
    original: &MetadataValues,
    field: MetadataField,
    row: &adw::EntryRow,
    force: bool,
) -> Result<Option<MetadataChange>, String> {
    if field == MetadataField::Title {
        let value = row.text().trim().to_string();
        if value.is_empty() {
            return Err(tr("Add a title"));
        }
        return Ok((force || value != original.title).then_some(MetadataChange::Title(value)));
    }

    if is_number_field(field) {
        let value = row.text();
        let value = value.trim();
        let value = if value.is_empty() {
            None
        } else {
            Some(
                value
                    .parse::<u16>()
                    .ok()
                    .filter(|value| *value > 0)
                    .ok_or_else(|| tr("Use a number above zero"))?,
            )
        };
        let previous = field_value(original, field).and_then(|value| value.parse().ok());
        if !force && value == previous {
            return Ok(None);
        }
        let change = match field {
            MetadataField::TrackNumber => MetadataChange::TrackNumber(value),
            MetadataField::DiscNumber => MetadataChange::DiscNumber(value),
            MetadataField::Year => MetadataChange::Year(value),
            MetadataField::Bpm => MetadataChange::Bpm(value),
            _ => unreachable!("numeric metadata field"),
        };
        return Ok(Some(change));
    }

    let value = normalized_text(row.text().as_str());
    let previous = field_value(original, field).and_then(|value| normalized_text(&value));
    if !force && value == previous {
        return Ok(None);
    }
    let change = match field {
        MetadataField::SortTitle => MetadataChange::SortTitle(value),
        MetadataField::Artist => MetadataChange::Artist(value),
        MetadataField::Album => MetadataChange::Album(value),
        MetadataField::AlbumArtist => MetadataChange::AlbumArtist(value),
        MetadataField::Genre => MetadataChange::Genre(value),
        MetadataField::Comment => MetadataChange::Comment(value),
        MetadataField::MusicBrainzRecordingId => MetadataChange::MusicBrainzRecordingId(value),
        MetadataField::MusicBrainzReleaseTrackId => {
            MetadataChange::MusicBrainzReleaseTrackId(value)
        }
        MetadataField::MusicBrainzAlbumId => MetadataChange::MusicBrainzAlbumId(value),
        MetadataField::MusicBrainzReleaseGroupId => {
            MetadataChange::MusicBrainzReleaseGroupId(value)
        }
        MetadataField::MusicBrainzArtistId => MetadataChange::MusicBrainzArtistId(value),
        MetadataField::Title
        | MetadataField::TrackNumber
        | MetadataField::DiscNumber
        | MetadataField::Year
        | MetadataField::Bpm
        | MetadataField::LockData => unreachable!("metadata field handled separately"),
    };
    Ok(Some(change))
}

fn normalized_text(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_text_uses_empty_values_for_clearing() {
        assert_eq!(normalized_text("  "), None);
        assert_eq!(normalized_text("  Value  "), Some("Value".to_string()));
    }

    #[test]
    fn identification_uses_the_current_visible_metadata_values() {
        let mut values = MetadataValues {
            title: "Old title".to_string(),
            artist: Some("Old artist".to_string()),
            album: Some("Old album".to_string()),
            year: Some(1999),
            ..MetadataValues::default()
        };

        apply_identification_input(&mut values, MetadataField::Title, " New title ");
        apply_identification_input(&mut values, MetadataField::Artist, " New artist ");
        apply_identification_input(&mut values, MetadataField::Album, " New album ");
        apply_identification_input(&mut values, MetadataField::Year, "2026");

        assert_eq!(values.title, "New title");
        assert_eq!(values.artist.as_deref(), Some("New artist"));
        assert_eq!(values.album.as_deref(), Some("New album"));
        assert_eq!(values.year, Some(2026));
    }

    #[test]
    fn identification_undo_remembers_only_changed_writable_fields() {
        let title = MetadataField::Title;
        let artist = MetadataField::Artist;
        let album = MetadataField::Album;
        let mut edits = HashMap::from([(
            title,
            IdentifiedEdit {
                original: "Original title".to_string(),
                identified: "First identified title".to_string(),
                was_touched: true,
            },
        )]);
        let before = HashMap::from([
            (title, "First identified title".to_string()),
            (artist, "Before artist".to_string()),
            (album, "Before album".to_string()),
        ]);
        let after = HashMap::from([
            (title, "Second identified title".to_string()),
            (artist, "After artist".to_string()),
            (album, "After album".to_string()),
        ]);
        let editing = library::MetadataEditing::new(vec![title, artist]);
        let touched_before = HashSet::from([title]);

        merge_identified_edits(&mut edits, &before, &after, &touched_before, &editing);

        assert_eq!(
            edits,
            HashMap::from([
                (
                    title,
                    IdentifiedEdit {
                        original: "Original title".to_string(),
                        identified: "Second identified title".to_string(),
                        was_touched: true,
                    }
                ),
                (
                    artist,
                    IdentifiedEdit {
                        original: "Before artist".to_string(),
                        identified: "After artist".to_string(),
                        was_touched: false,
                    }
                ),
            ])
        );
    }

    #[test]
    fn identification_undo_restores_an_untouched_mixed_field() {
        let field = MetadataField::Artist;
        let mut touched = HashSet::from([field]);

        restore_touched_state(&mut touched, field, false);
        assert!(!touched.contains(&field));

        restore_touched_state(&mut touched, field, true);
        assert!(touched.contains(&field));
    }

    #[test]
    fn private_mode_keeps_source_search_available() {
        const ARTIST_ID: &str = "01234567-89ab-cdef-0123-456789abcdef";
        let item_id = MetadataItemId::Artist(library::ArtistId::fake(1));
        let mut values = MetadataValues {
            title: "Artist".to_string(),
            musicbrainz_artist_id: Some(ARTIST_ID.to_string()),
            ..MetadataValues::default()
        };

        assert!(identification_available(&item_id, true, false, &values));
        assert!(!identification_available(&item_id, false, false, &values));
        assert!(identification_available(&item_id, false, true, &values));

        values.title = " ".to_string();
        values.musicbrainz_artist_id = None;
        assert!(!identification_available(&item_id, true, false, &values));
    }
}
