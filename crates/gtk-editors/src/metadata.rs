use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use adw::prelude::*;
use localization::{tr, trn_with};
use sources::SourceMetadataError;

use gtk_widgets::field_layout::{compact_field_row_group, style_compact_field_row};
use gtk_widgets::layout::large_popup_content_width;
use rufin_core::source::SourceOwner;
use std::sync::Arc;

const EDITOR_WIDTH: i32 = 650;

pub use rufin_core::metadata::{MetadataDraft, MetadataReceiver};
use rufin_core::metadata::{
    MetadataEditor, MetadataField, MetadataItemId, identification_available,
};

mod artwork;

struct MetadataEntry {
    field: MetadataField,
    entry: adw::EntryRow,
    undo: gtk::Button,
}

struct Editor {
    draft: Arc<MetadataDraft>,
    entries: Vec<MetadataEntry>,
    locked: Option<adw::SwitchRow>,
    model: RefCell<MetadataEditor>,
    status: gtk::Label,
    identify: gtk::Button,
    save: gtk::Button,
    cancel: gtk::Button,
    external_lookup_allowed: bool,
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

pub fn build_dialog(
    source: &Arc<SourceOwner>,
    item: MetadataItemId,
    draft: MetadataDraft,
    external_lookup_allowed: bool,
) -> adw::Dialog {
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
    let model = MetadataEditor::new(draft);
    let draft = Arc::clone(&model.draft);
    identify.set_sensitive(draft.source_search() || external_lookup_allowed);
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
        model: RefCell::new(model),
        status,
        identify,
        save,
        cancel,
        external_lookup_allowed,
        busy: Cell::new(false),
        artwork,
    });
    connect_editor_changes(&editor);
    for row in &editor.entries {
        refresh_identified_field(&editor, row.field);
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
    connect_identify(source, item.clone(), &editor);
    artwork::connect(source, &editor);
    connect_save(source, item, &editor_dialog, &editor);
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
    for field in model.fields() {
        append_named_entry(
            entries,
            field.field,
            field.label,
            &field.value,
            field.writable,
            field.mixed,
        );
        let entry = &entries.last().unwrap().entry;
        if field.kind == sources::MetadataFieldKind::Number {
            entry.set_input_purpose(if matches!(field.field, MetadataField::Extra(_)) {
                gtk::InputPurpose::Number
            } else {
                gtk::InputPurpose::Digits
            });
        }
    }
    append_lock(
        locked,
        model.locked(),
        model.writable(MetadataField::Locked),
    );
}

fn append_named_entry(
    entries: &mut Vec<MetadataEntry>,
    field: MetadataField,
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
        let field = row.field;
        let editor_changed = Rc::downgrade(editor);
        row.entry.connect_changed(move |entry| {
            let Some(editor_changed) = editor_changed.upgrade() else {
                return;
            };
            editor_changed
                .model
                .borrow_mut()
                .set_text(field, entry.text().to_string());
            refresh_save_state(&editor_changed);
        });
        let field = row.field;
        let editor_undo = Rc::downgrade(editor);
        row.undo.connect_clicked(move |_| {
            let Some(editor_undo) = editor_undo.upgrade() else {
                return;
            };
            if editor_undo.model.borrow_mut().undo_identified(field) {
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
            editor.model.borrow_mut().set_locked(locked.is_active());
            refresh_save_state(&editor);
        });
    }
}

fn refresh_editor_fields(editor: &Editor) {
    let fields = editor.model.borrow().fields();
    for field in fields {
        editor.entry(field.field).set_text(&field.value);
        refresh_identified_field(editor, field.field);
    }
}

fn refresh_identified_field(editor: &Editor, field: MetadataField) {
    let identified = editor.model.borrow().is_identified(field);
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
    editor.save.set_sensitive(
        !editor.busy.get()
            && !editor.artwork.pending()
            && (editor.model.borrow().has_changes() || editor.artwork.edit().is_some()),
    );
    if let Ok(values) = editor.model.borrow().values() {
        editor.identify.set_sensitive(
            !editor.busy.get()
                && identification_available(
                    editor.draft.source_search(),
                    editor.external_lookup_allowed,
                    &values,
                ),
        );
    }
}

fn connect_identify(source: &Arc<SourceOwner>, item: MetadataItemId, editor: &Rc<Editor>) {
    let source = Arc::downgrade(source);
    let identify = editor.identify.clone();
    let editor = Rc::downgrade(editor);
    identify.connect_clicked(move |_| {
        let Some(source) = source.upgrade() else {
            return;
        };
        let Some(editor) = editor.upgrade() else {
            return;
        };
        let values = match editor.model.borrow().values() {
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
        let receiver = values.identify(&source, item.media_uri().to_string());
        let editor = Rc::downgrade(&editor);
        gtk::glib::spawn_future_local(async move {
            let response = receiver.recv().await;
            let Some(editor) = editor.upgrade() else {
                return;
            };
            match response {
                Ok(Some(identified)) => {
                    editor
                        .model
                        .borrow_mut()
                        .apply_identified(identified.values, identified.token);
                    refresh_editor_fields(&editor);
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
    source: &Arc<SourceOwner>,
    item: MetadataItemId,
    dialog: &adw::Dialog,
    editor: &Rc<Editor>,
) {
    let source = Arc::downgrade(source);
    let dialog = dialog.downgrade();
    let save = editor.save.clone();
    let editor = Rc::downgrade(editor);
    save.connect_clicked(move |_| {
        let Some(source) = source.upgrade() else {
            return;
        };
        let Some(editor) = editor.upgrade() else {
            return;
        };
        let edit = match editor.model.borrow().edit(editor.artwork.edit()) {
            Ok(edit) => edit,
            Err(error) => {
                editor.show_error(&error);
                return;
            }
        };
        editor.set_busy(true, &tr("Saving..."));
        let revision = editor.draft.revision();
        let token = editor.model.borrow().token();
        let receiver = rufin_core::metadata::write_reviewed_metadata(
            &source,
            item.media_uri().to_string(),
            revision,
            token,
            edit,
            editor.draft.artwork().binding.clone(),
        );
        let editor = Rc::downgrade(&editor);
        let dialog = dialog.clone();
        gtk::glib::spawn_future_local(async move {
            let response = receiver
                .recv()
                .await
                .unwrap_or(Err(SourceMetadataError::Unavailable));
            let Some(editor) = editor.upgrade() else {
                return;
            };
            editor.model.borrow_mut().record_save_result(&response);
            match response {
                Ok(()) => {
                    if let Some(dialog) = dialog.upgrade() {
                        dialog.force_close();
                    }
                }
                Err(
                    error @ (SourceMetadataError::SavedRefreshFailed(_)
                    | SourceMetadataError::PartiallySaved { .. }),
                ) => {
                    editor.show_error(&error.to_string());
                    editor.finish_committed_save(editor.model.borrow().committed().unwrap());
                }
                Err(error) => {
                    editor.show_error(&error.to_string());
                    editor.set_busy(false, &tr("Save"));
                    refresh_save_state(&editor);
                }
            }
        });
    });
}

impl Editor {
    fn entry(&self, field: MetadataField) -> &adw::EntryRow {
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
            .set_sensitive(!busy && self.draft.source_search());
        refresh_save_state(self);
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
    fn writable(&self, field: MetadataField) -> bool {
        self.model.borrow().writable(field)
    }
}
