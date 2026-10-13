use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use adw::prelude::*;
use localization::{tr, trn_with};
use sources::SourceMetadataError;

use gtk_widgets::field_layout::{compact_field_row_group, style_compact_field_row};
use gtk_widgets::layout::large_popup_content_width;
use std::sync::Arc;

const EDITOR_WIDTH: i32 = 650;

pub use rufin_core::metadata::MetadataDraft;
use rufin_core::metadata::MetadataEditor;

mod artwork;

struct MetadataEntry {
    field: String,
    entry: adw::EntryRow,
    undo: gtk::Button,
}

struct Editor {
    draft: Arc<MetadataDraft>,
    entries: Vec<MetadataEntry>,
    locked: Option<adw::SwitchRow>,
    model: Arc<MetadataEditor>,
    status: gtk::Label,
    identify: gtk::Button,
    save: gtk::Button,
    cancel: gtk::Button,
    busy: Cell<bool>,
    artwork: artwork::ArtworkEditor,
}

pub fn metadata_error_dialog(message: &str) -> adw::Dialog {
    let resource = crate::ui_resource::METADATA_DIALOG_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        error_dialog: adw::Dialog,
        error_message: gtk::Label,
    });
    error_dialog.set_content_width(large_popup_content_width(480));
    error_message.set_label(message);
    error_dialog
}

pub fn build_dialog(model: Arc<MetadataEditor>) -> adw::Dialog {
    let resource = crate::ui_resource::METADATA_DIALOG_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        editor_dialog: adw::Dialog,
        editor_toolbar: adw::ToolbarView,
        editor_pages: gtk::Stack,
        fields: gtk::Box,
        artwork_host: gtk::Box,
        identify_scope: gtk::Label,
        identify_spacer: gtk::Box,
        identify: gtk::Button,
        status: gtk::Label,
        cancel: gtk::Button,
        save: gtk::Button,
    });
    let width = large_popup_content_width(EDITOR_WIDTH);
    editor_dialog.set_content_width(width);
    editor_dialog.set_child(None::<&gtk::Widget>);
    editor_dialog.set_child(Some(&gtk_widgets::layout::preferred_width_owner(
        &editor_toolbar,
        width,
    )));
    let dialog = editor_dialog.downgrade();
    editor_pages.connect_visible_child_name_notify(move |pages| {
        if let Some(dialog) = dialog.upgrade() {
            dialog
                .set_follows_content_size(pages.visible_child_name().as_deref() == Some("artwork"));
        }
    });
    let draft = Arc::clone(&model.draft);
    let external_lookup_allowed = model.snapshot().external_lookup_allowed;
    identify.set_sensitive(model.snapshot().can_identify);
    let mut entries = Vec::new();
    let mut locked = None;
    append_draft_fields(&model, &mut entries, &mut locked);
    populate_metadata_fields(
        &fields,
        &identify_scope,
        &identify_spacer,
        &draft,
        &entries,
        locked.as_ref(),
    );

    let artwork = artwork::ArtworkEditor::new(&artwork_host, external_lookup_allowed);

    let editor = Rc::new(Editor {
        draft,
        entries,
        locked,
        model,
        status,
        identify,
        save,
        cancel,
        busy: Cell::new(false),
        artwork,
    });
    connect_editor_changes(&editor);
    for presented in editor.model.snapshot().fields {
        refresh_identified_field(&editor, &presented.field.key, presented.identified);
    }
    refresh_save_state(&editor);
    let editor_lifetime = RefCell::new(Some(Rc::clone(&editor)));
    editor_dialog.connect_closed(move |_| {
        editor_lifetime.borrow_mut().take();
    });
    let close = editor_dialog.downgrade();
    editor.cancel.connect_clicked(move |_| {
        if let Some(dialog) = close.upgrade() {
            dialog.close();
        }
    });
    connect_identify(&editor);
    artwork::connect(&editor);
    connect_save(&editor_dialog, &editor);
    editor_dialog
}

fn populate_metadata_fields(
    fields: &gtk::Box,
    identify_scope: &gtk::Label,
    identify_spacer: &gtk::Box,
    draft: &MetadataDraft,
    entries: &[MetadataEntry],
    locked: Option<&adw::SwitchRow>,
) {
    if draft.track_count > 1 {
        let count = draft.track_count;
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

    for entry in entries {
        if entry.entry.is_sensitive() || !entry.entry.text().is_empty() {
            fields.append(&compact_field_row_group(&entry.entry));
        }
    }
    if let Some(row) = locked {
        fields.append(&compact_field_row_group(row));
    }
}

fn append_draft_fields(
    model: &MetadataEditor,
    entries: &mut Vec<MetadataEntry>,
    locked: &mut Option<adw::SwitchRow>,
) {
    let snapshot = model.snapshot();
    for presented in snapshot.fields {
        let field = presented.field;
        append_named_entry(
            entries,
            field.key.clone(),
            field.label,
            &field.value,
            field.writable,
            field.mixed,
        );
        let entry = &entries.last().unwrap().entry;
        if field.kind == sources::MetadataFieldKind::Number {
            entry.set_input_purpose(
                if matches!(
                    field.key.as_str(),
                    "track_number" | "disc_number" | "year" | "bpm"
                ) {
                    gtk::InputPurpose::Digits
                } else {
                    gtk::InputPurpose::Number
                },
            );
        }
    }
    append_lock(locked, snapshot.locked, snapshot.lock_writable);
}

fn append_named_entry(
    entries: &mut Vec<MetadataEntry>,
    field: String,
    mut title: String,
    value: &str,
    writable: bool,
    mixed: bool,
) {
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

fn connect_editor_changes(editor: &Rc<Editor>) {
    for row in editor.entries.iter() {
        let field = row.field.clone();
        let writable = editor.writable(&field);
        let editor_changed = Rc::downgrade(editor);
        row.entry.connect_changed(move |entry| {
            if !writable {
                return;
            }
            let Some(editor_changed) = editor_changed.upgrade() else {
                return;
            };
            let text = entry.text().to_string();
            if let Err(error) = editor_changed.model.set_field(field.clone(), text) {
                editor_changed.show_error(&error.to_string());
            }
            refresh_save_state(&editor_changed);
        });
        let field = row.field.clone();
        let editor_undo = Rc::downgrade(editor);
        row.undo.connect_clicked(move |_| {
            let Some(editor_undo) = editor_undo.upgrade() else {
                return;
            };
            if editor_undo.model.undo_identified(field.clone()) {
                refresh_editor_fields(&editor_undo);
                refresh_save_state(&editor_undo);
            }
        });
    }
    if let Some(locked) = &editor.locked {
        let editor = Rc::downgrade(editor);
        locked.connect_active_notify(move |locked| {
            let Some(editor) = editor.upgrade() else {
                return;
            };
            if let Err(error) = editor.model.set_locked(locked.is_active()) {
                editor.show_error(&error.to_string());
            }
            refresh_save_state(&editor);
        });
    }
}

fn refresh_editor_fields(editor: &Editor) {
    let fields = editor.model.snapshot().fields;
    for presented in fields {
        let field = presented.field;
        editor.entry(&field.key).set_text(&field.value);
        refresh_identified_field(editor, &field.key, presented.identified);
    }
}

fn refresh_identified_field(editor: &Editor, field: &str, identified: bool) {
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
    let state = editor.model.snapshot();
    let editable = !editor.busy.get() && state.committed.is_none();
    editor
        .save
        .set_sensitive(editable && !editor.artwork.pending() && state.has_changes);
    editor
        .identify
        .set_sensitive(editable && state.can_identify);
}

fn connect_identify(editor: &Rc<Editor>) {
    let identify = editor.identify.clone();
    let editor = Rc::downgrade(editor);
    identify.connect_clicked(move |_| {
        let Some(editor) = editor.upgrade() else {
            return;
        };
        if !editor.model.snapshot().can_identify {
            return;
        }
        editor.set_busy(true, &tr("Identifying..."));
        let model = editor.model.clone();
        let editor = Rc::downgrade(&editor);
        gtk::glib::spawn_future_local(async move {
            let response = model.identify().await;
            let Some(editor) = editor.upgrade() else {
                return;
            };
            match response {
                Ok(()) => refresh_editor_fields(&editor),
                Err(SourceMetadataError::Write(message)) => editor.show_error(&message),
                Err(error) => editor.show_error(&error.to_string()),
            }
            editor.set_busy(false, &tr("Identify"));
            refresh_save_state(&editor);
        });
    });
}

fn connect_save(dialog: &adw::Dialog, editor: &Rc<Editor>) {
    let dialog = dialog.downgrade();
    let save = editor.save.clone();
    let editor = Rc::downgrade(editor);
    save.connect_clicked(move |_| {
        let Some(editor) = editor.upgrade() else {
            return;
        };
        editor.set_busy(true, &tr("Saving..."));
        let model = editor.model.clone();
        let editor = Rc::downgrade(&editor);
        let dialog = dialog.clone();
        gtk::glib::spawn_future_local(async move {
            let response = model.save().await;
            let Some(editor) = editor.upgrade() else {
                return;
            };
            match response {
                Ok(()) => {
                    if let Some(dialog) = dialog.upgrade() {
                        dialog.force_close();
                    }
                }
                Err(error) => {
                    editor.show_error(&error.to_string());
                    if let Some(fully_saved) = editor.model.snapshot().committed {
                        editor.finish_committed_save(fully_saved);
                    } else {
                        editor.set_busy(false, &tr("Save"));
                        refresh_save_state(&editor);
                    }
                }
            }
        });
    });
}

impl Editor {
    fn entry(&self, field: &str) -> &adw::EntryRow {
        &self
            .entries
            .iter()
            .find(|row| row.field == field)
            .expect("metadata field belongs to this draft")
            .entry
    }
    fn set_busy(&self, busy: bool, label: &str) {
        self.busy.set(busy);
        self.artwork.set_busy(busy);
        self.identify
            .set_sensitive(!busy && self.draft.source_search);
        refresh_save_state(self);
        self.cancel.set_sensitive(!busy);
        for row in self.entries.iter() {
            row.entry.set_sensitive(!busy && self.writable(&row.field));
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

    fn finish_committed_save(&self, fully_saved: bool) {
        self.busy.set(true);
        self.artwork.set_busy(true);
        self.identify.set_sensitive(false);
        self.save.set_sensitive(false);
        self.save
            .set_label(&if fully_saved { tr("Saved") } else { tr("Save") });
        self.cancel.set_sensitive(true);
        self.cancel.set_label(&tr("Close"));
        for row in self.entries.iter() {
            row.entry.set_sensitive(false);
        }
        if let Some(locked) = &self.locked {
            locked.set_sensitive(false);
        }
    }
    fn writable(&self, field: &str) -> bool {
        self.draft
            .fields
            .iter()
            .any(|candidate| candidate.key == field && candidate.writable)
    }
}
