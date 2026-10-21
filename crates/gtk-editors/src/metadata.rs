use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use adw::prelude::*;
use localization::{tr, tr_with};

use gtk_widgets::field_layout::{compact_field_row_group, style_compact_field_row};
use gtk_widgets::layout::large_popup_content_width;
use std::sync::Arc;

const EDITOR_WIDTH: i32 = 850;

pub use rufin_core::metadata::MetadataDraft;
use rufin_core::metadata::{MetadataEditor, mixed_field_label};

mod artwork;
mod batch;
mod lookup;
mod lyrics;
mod navigation;
pub use batch::build_batch_dialog;

struct MetadataEntry {
    field: String,
    group: adw::PreferencesGroup,
    entry: adw::EntryRow,
    undo: gtk::Button,
    clear: gtk::Button,
    original: gtk::Label,
}

struct Editor {
    entries: Vec<MetadataEntry>,
    locked: Option<adw::SwitchRow>,
    model: RefCell<Arc<MetadataEditor>>,
    status: gtk::Label,
    lookup: gtk::Button,
    runtime: tokio::runtime::Handle,
    on_change: Option<Rc<dyn Fn()>>,
    save: gtk::Button,
    cancel: gtk::Button,
    busy: Cell<bool>,
    updating: Cell<bool>,
    scope_controls: Option<gtk::Widget>,
    artwork: artwork::ArtworkEditor,
    lyrics: lyrics::LyricsPane,
    pages: gtk::Stack,
    artwork_page: gtk::StackPage,
    lyrics_page: gtk::StackPage,
    scope_label: gtk::Label,
    scope_spacer: gtk::Box,
    lock_group: adw::PreferencesGroup,
}

pub fn metadata_error_dialog(message: &str) -> adw::Dialog {
    let resource = crate::ui_resource::METADATA_DIALOG_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        error_dialog: adw::Dialog,
        error_header: adw::HeaderBar,
        error_message: gtk::Label,
    });
    gtk_widgets::controls::configure_dialog_header(&error_header);
    error_dialog.set_content_width(large_popup_content_width(480));
    error_message.set_label(message);
    error_dialog
}

struct MetadataView {
    content: gtk::Widget,
    editor: Rc<Editor>,
}

pub fn build_dialog(model: Arc<MetadataEditor>, runtime: tokio::runtime::Handle) -> adw::Dialog {
    let (dialog, view) = build_editor_view(model, None, None, None, runtime);
    dialog.set_child(Some(&view.content));
    let owner = RefCell::new(Some(view));
    dialog.connect_closed(move |_| {
        owner.borrow_mut().take();
    });
    dialog
}

fn build_editor_view(
    model: Arc<MetadataEditor>,
    outer: Option<&adw::Dialog>,
    scope_controls: Option<&gtk::Widget>,
    on_change: Option<Rc<dyn Fn()>>,
    runtime: tokio::runtime::Handle,
) -> (adw::Dialog, MetadataView) {
    let resource = crate::ui_resource::METADATA_DIALOG_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        editor_dialog: adw::Dialog,
        editor_toolbar: adw::ToolbarView,
        editor_header: adw::HeaderBar,
        editor_pages: gtk::Stack,
        fields: gtk::Box,
        artwork_host: gtk::Box,
        lyrics_host: gtk::Box,
        scope_label: gtk::Label,
        scope_spacer: gtk::Box,
        lookup: gtk::Button,
        lookup_content: adw::ButtonContent,
        musicbrainz_lookup_label: gtk::Label,
        status: gtk::Label,
        cancel: gtk::Button,
        save: gtk::Button,
    });
    if model.draft.kind != sources::SourceEntityKind::Track {
        lookup_content.set_label(&musicbrainz_lookup_label.label());
    }
    editor_header.set_visible(outer.is_none());
    gtk_widgets::controls::configure_dialog_header(&editor_header);
    let width = large_popup_content_width(EDITOR_WIDTH);
    editor_dialog.set_content_width(width);
    editor_dialog.set_child(None::<&gtk::Widget>);
    let content =
        gtk_widgets::layout::preferred_width_owner(&editor_toolbar, width).upcast::<gtk::Widget>();
    let dialog = editor_dialog.downgrade();
    editor_pages.connect_visible_child_name_notify(move |pages| {
        if let Some(dialog) = dialog.upgrade() {
            dialog
                .set_follows_content_size(pages.visible_child_name().as_deref() == Some("artwork"));
        }
    });
    let external_lookup_allowed = model.snapshot().external_lookup_allowed;
    lookup.set_sensitive(external_lookup_allowed);
    let mut entries = Vec::new();
    let mut locked = None;
    append_draft_fields(&model, &mut entries, &mut locked);
    for row in &entries {
        fields.append(&row.group);
    }
    let lock_group = compact_field_row_group(locked.as_ref().unwrap());
    fields.append(&lock_group);
    let artwork_page = editor_pages.page(&artwork_host);
    let lyrics_page = editor_pages.page(&lyrics_host);
    editor_pages.set_transition_type(gtk::StackTransitionType::Crossfade);
    editor_pages.set_transition_duration(160);
    let artwork = artwork::ArtworkEditor::new(&artwork_host, external_lookup_allowed);

    let lyrics = lyrics::LyricsPane::new(&lyrics_host);
    let editor = Rc::new(Editor {
        entries,
        locked,
        model: RefCell::new(model),
        pages: editor_pages,
        artwork_page,
        lyrics_page,
        scope_label,
        scope_spacer,
        lock_group,
        status,
        lookup,
        runtime,
        on_change,
        save,
        cancel,
        busy: Cell::new(false),
        updating: Cell::new(false),
        scope_controls: scope_controls.cloned(),
        artwork,
        lyrics,
    });
    connect_editor_changes(&editor);
    refresh_editor_fields(&editor);
    refresh_save_state(&editor);
    let close = outer.unwrap_or(&editor_dialog).downgrade();
    editor.cancel.connect_clicked(move |_| {
        if let Some(dialog) = close.upgrade() {
            dialog.close();
        }
    });
    editor.lookup.set_visible(outer.is_none());
    let parent = outer.unwrap_or(&editor_dialog).downgrade();
    let weak = Rc::downgrade(&editor);
    editor.lookup.connect_clicked(move |_| {
        let Some(editor) = weak.upgrade() else { return };
        artwork::cancel(&editor);
        editor.lyrics.cancel();
        let model = editor.model();
        let parent = parent.clone();
        let weak = Rc::downgrade(&editor);
        editor.set_busy(true, &tr("Save"));
        gtk::glib::spawn_future_local(async move {
            let review = rufin_core::metadata::MetadataLookupReview::open(model).await;
            let Some(editor) = weak.upgrade() else { return };
            editor.set_busy(false, &tr("Save"));
            match review {
                Ok(review) => {
                    if let Some(parent) = parent.upgrade() {
                        let weak = Rc::downgrade(&editor);
                        lookup::present(
                            &parent,
                            review,
                            editor.runtime.clone(),
                            Rc::new(move |accepted| {
                                if accepted && let Some(editor) = weak.upgrade() {
                                    refresh_editor_fields(&editor);
                                    refresh_save_state(&editor);
                                    editor.changed();
                                }
                            }),
                        );
                    }
                }
                Err(error) => editor.show_error(&error.to_string()),
            }
        });
    });
    artwork::connect(&editor);
    lyrics::connect(&editor);
    refresh_pages(&editor);
    connect_save(outer.unwrap_or(&editor_dialog), &editor);
    (editor_dialog, MetadataView { content, editor })
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
            snapshot.track_count,
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
    append_lock(
        locked,
        snapshot.locked,
        snapshot.lock_writable,
        snapshot.lock_mixed,
        snapshot.track_count,
    );
}

fn append_named_entry(
    entries: &mut Vec<MetadataEntry>,
    field: String,
    mut title: String,
    value: &str,
    writable: bool,
    mixed: bool,
    count: u64,
) {
    if mixed {
        title = mixed_field_label(&title, count);
    }
    let resource = crate::ui_resource::METADATA_FIELD_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, { entry: adw::EntryRow, clear: gtk::Button, undo: gtk::Button, original: gtk::Label });
    entry.set_title(&title);
    entry.set_text(if mixed { "" } else { value });
    entry.set_sensitive(writable);
    if writable {
        entry.set_tooltip_text(None);
    }
    style_compact_field_row(&entry);
    entries.push(MetadataEntry {
        field,
        group: compact_field_row_group(&entry),
        entry,
        undo,
        clear,
        original,
    });
}
fn append_lock(
    target: &mut Option<adw::SwitchRow>,
    value: Option<bool>,
    writable: bool,
    mixed: bool,
    count: u64,
) {
    let row = adw::SwitchRow::builder()
        .title(if mixed {
            mixed_field_label(&tr("Lock metadata"), count)
        } else {
            tr("Lock metadata")
        })
        .subtitle(tr(
            "Prevent automatic metadata refreshes from replacing these values",
        ))
        .active(value.unwrap_or(false))
        .build();
    row.set_sensitive(writable);
    style_compact_field_row(&row);
    *target = Some(row);
}

fn connect_editor_changes(editor: &Rc<Editor>) {
    for row in editor.entries.iter() {
        let field = row.field.clone();
        let editor_changed = Rc::downgrade(editor);
        row.entry.connect_changed(move |entry| {
            let Some(editor_changed) = editor_changed.upgrade() else {
                return;
            };
            if editor_changed.updating.get() {
                return;
            }
            let text = entry.text().to_string();
            if let Err(error) = editor_changed.model().set_field(field.clone(), text) {
                editor_changed.show_error(&error.to_string());
            }
            let state = editor_changed.model().snapshot();
            if let Some(presented) = state
                .fields
                .iter()
                .find(|presented| presented.field.key == field)
            {
                entry.set_title(&presented.field.label);
                refresh_identified_field(&editor_changed, &field, presented.identified);
            }
            refresh_save_state(&editor_changed);
        });
        let weak = Rc::downgrade(editor);
        let field = row.field.clone();
        row.clear.connect_clicked(move |_| {
            if let Some(editor) = weak.upgrade() {
                if let Err(error) = editor.model().set_field(field.clone(), String::new()) {
                    editor.show_error(&error.to_string());
                }
                refresh_editor_fields(&editor);
                refresh_save_state(&editor);
            }
        });
        let field = row.field.clone();
        let editor_undo = Rc::downgrade(editor);
        row.undo.connect_clicked(move |_| {
            let Some(editor_undo) = editor_undo.upgrade() else {
                return;
            };
            let model = editor_undo.model().clone();
            let field = field.clone();
            editor_undo.set_busy(true, &tr("Reset"));
            let weak = Rc::downgrade(&editor_undo);
            gtk::glib::spawn_future_local(async move {
                let result = model.reset_field(field).await;
                if let Some(editor) = weak.upgrade() {
                    if let Err(error) = result {
                        editor.show_error(&error.to_string());
                    }
                    refresh_editor_fields(&editor);
                    editor.set_busy(false, &tr("Save"));
                    editor.changed();
                }
            });
        });
    }
    if let Some(locked) = &editor.locked {
        let editor = Rc::downgrade(editor);
        locked.connect_active_notify(move |locked| {
            let Some(editor) = editor.upgrade() else {
                return;
            };
            if editor.updating.get() {
                return;
            }
            if let Err(error) = editor.model().set_locked(locked.is_active()) {
                editor.show_error(&error.to_string());
            }
            refresh_save_state(&editor);
        });
    }
}

fn refresh_editor_fields(editor: &Editor) {
    editor.updating.set(true);
    let snapshot = editor.model().snapshot();
    for row in &editor.entries {
        if let Some(presented) = snapshot
            .fields
            .iter()
            .find(|field| field.field.key == row.field)
        {
            let field = &presented.field;
            row.group
                .set_visible(field.writable || !field.value.is_empty() || field.mixed);
            row.entry.set_title(if field.mixed {
                &presented.mixed_label
            } else {
                &field.label
            });
            let text = if field.mixed { "" } else { &field.value };
            if row.entry.text().as_str() != text {
                row.entry.set_text(text);
            }
            row.entry
                .set_sensitive(!editor.busy.get() && field.writable);
            refresh_identified_field(editor, &field.key, presented.identified);
        } else {
            row.group.set_visible(false);
        }
    }
    editor.lock_group.set_visible(snapshot.lock_writable);
    if let Some(locked) = &editor.locked {
        locked.set_active(snapshot.locked.unwrap_or(false));
        locked.set_sensitive(snapshot.lock_writable && !editor.busy.get());
    }
    if snapshot.track_count > 1 {
        editor.scope_label.set_label(&snapshot.scope_summary);
        editor.scope_label.set_visible(true);
        editor.scope_spacer.set_visible(false);
    } else {
        editor.scope_label.set_visible(false);
        editor.scope_spacer.set_visible(true);
    }
    editor.updating.set(false);
}

fn refresh_pages(editor: &Rc<Editor>) {
    let model = editor.model();
    let single = model.snapshot().selected_count == 1;
    if !single && editor.pages.visible_child_name().as_deref() != Some("tags") {
        editor.pages.set_visible_child_name("tags");
    }
    editor.artwork_page.set_visible(single);
    editor
        .lyrics_page
        .set_visible(single && model.draft.kind == sources::SourceEntityKind::Track);
}

fn refresh_identified_field(editor: &Editor, field: &str, identified: bool) {
    let row = editor
        .entries
        .iter()
        .find(|row| row.field == field)
        .expect("identified metadata field belongs to this draft");
    row.undo.set_visible(identified);
    let snapshot = editor.model().snapshot();
    if let Some(presented) = snapshot
        .fields
        .iter()
        .find(|presented| presented.field.key == field)
    {
        let original = if presented.original_mixed {
            mixed_field_label(&presented.field.label, snapshot.track_count)
        } else {
            presented.original_value.clone()
        };
        let label = tr_with("Original: {value}", &[("value", original.as_str())]);
        row.original.set_label(&label);
        row.original.set_tooltip_text(Some(&label));
    }
    row.original.set_visible(
        identified
            && snapshot.fields.iter().any(|presented| {
                presented.field.key == field
                    && (presented.original_mixed || !presented.original_value.is_empty())
            }),
    );
    if identified {
        row.entry.add_css_class("metadata-identified-change");
    } else {
        row.entry.remove_css_class("metadata-identified-change");
    }
}
fn refresh_save_state(editor: &Editor) {
    let state = editor.model().snapshot();
    let editable = !editor.busy.get() && state.committed.is_none();
    editor
        .save
        .set_sensitive(editable && !editor.artwork.pending() && state.has_changes);
    editor
        .lookup
        .set_sensitive(editable && state.external_lookup_allowed);
}

fn connect_save(dialog: &adw::Dialog, editor: &Rc<Editor>) {
    let dialog = dialog.downgrade();
    let save = editor.save.clone();
    let editor = Rc::downgrade(editor);
    save.connect_clicked(move |_| {
        let Some(editor) = editor.upgrade() else {
            return;
        };
        lyrics::commit(&editor);
        editor.lyrics.cancel();
        editor.set_busy(true, &tr("Saving..."));
        let model = editor.model().clone();
        let editor = Rc::downgrade(&editor);
        let dialog = dialog.clone();
        gtk::glib::spawn_future_local(async move {
            let response = async {
                if model.draft.kind == sources::SourceEntityKind::Track {
                    let summary = model.prepare_save().await?;
                    let Some(current) = editor.upgrade() else {
                        return Ok(false);
                    };
                    current.changed();
                    drop(current);
                    if summary.tracks == 0 {
                        return Ok(false);
                    }
                    let resource = crate::ui_resource::METADATA_DIALOG_RESOURCE;
                    let builder = crate::ui_resource::builder(resource);
                    let confirm: adw::AlertDialog = builder
                        .object("save_confirmation")
                        .expect("metadata save confirmation");
                    confirm.set_heading(Some(&summary.message));
                    let Some(parent) = dialog.upgrade() else {
                        return Ok(false);
                    };
                    if confirm.choose_future(Some(&parent)).await != "save" {
                        return Ok(false);
                    }
                }
                model.save().await.map(|()| true)
            }
            .await;
            let Some(editor) = editor.upgrade() else {
                return;
            };
            match response {
                Ok(true) => {
                    if let Some(dialog) = dialog.upgrade() {
                        dialog.force_close();
                    }
                }
                Ok(false) => {
                    editor.set_busy(false, &tr("Save"));
                    refresh_save_state(&editor);
                }
                Err(error) => {
                    editor.show_error(&error.to_string());
                    if let Some(fully_saved) = editor.model().snapshot().committed {
                        editor.finish_committed_save(fully_saved);
                    } else {
                        refresh_editor_fields(&editor);
                        editor.set_busy(false, &tr("Save"));
                        refresh_save_state(&editor);
                    }
                }
            }
        });
    });
}

impl Editor {
    fn model(&self) -> Arc<MetadataEditor> {
        self.model.borrow().clone()
    }

    fn rebind(self: &Rc<Self>, model: Arc<MetadataEditor>) {
        artwork::cancel(self);
        self.lyrics.cancel();
        self.model.replace(model);
        self.status.set_visible(false);
        refresh_editor_fields(self);
        refresh_save_state(self);
        refresh_pages(self);
        artwork::refresh(self);
        self.lyrics.reset();
        if self.pages.visible_child_name().as_deref() == Some("lyrics") {
            lyrics::activate(self);
        }
    }

    fn changed(&self) {
        if let Some(changed) = &self.on_change {
            changed();
        }
    }

    fn set_busy(&self, busy: bool, label: &str) {
        self.busy.set(busy);
        if let Some(controls) = &self.scope_controls {
            controls.set_sensitive(!busy);
        }
        self.artwork.set_busy(busy);
        self.lyrics.set_busy(busy);
        refresh_save_state(self);
        self.cancel.set_sensitive(!busy);
        for row in self.entries.iter() {
            row.entry.set_sensitive(!busy && self.writable(&row.field));
        }
        if let Some(locked) = &self.locked {
            locked.set_sensitive(!busy && self.model().snapshot().lock_writable);
        }
        if busy {
            self.save.set_label(label);
        } else {
            self.save.set_label(&tr("Save"));
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
        self.lookup.set_sensitive(false);
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
        self.model()
            .draft
            .fields
            .iter()
            .any(|candidate| candidate.key == field && candidate.writable)
    }
}
