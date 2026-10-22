use super::{MetadataView, build_editor_view};
use adw::prelude::*;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use gtk_widgets::sparse_model::{
    SparseLoad, SparseObjectItem, SparseRouteModel, SparseSource, connect_sparse_bind,
};
use rufin_core::metadata::{
    MetadataBatch, MetadataEditor, MetadataLookupReview, MetadataTrackSummary,
};
use sources::SourceMetadataError;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};

gtk_widgets::composite_box!(pub(crate) MetadataTrackRow, track_row, "RufinMetadataTrackRow",
"/io/github/screwys/Rufin/ui/preferences/dialogs/metadata_track.ui", {
    title: gtk::Label, status: gtk::Label,
});

struct BatchView {
    batch: Arc<MetadataBatch>,
    scope_button: gtk::ToggleButton,
    scope_selected: Cell<bool>,
    dialog: gtk::glib::WeakRef<adw::Dialog>,
    host: gtk::Box,
    content: gtk::Stack,
    lookup: gtk::Button,
    sidebar: gtk::Box,
    selection: gtk::MultiSelection,
    current: RefCell<Option<MetadataView>>,
    active_selection: RefCell<gtk::Bitset>,
    task: RefCell<Option<gtk::glib::JoinHandle<()>>>,
    rows: Rc<SparseRouteModel<u64, MetadataTrackSummary>>,
    runtime: tokio::runtime::Handle,
    on_error: Rc<dyn Fn(SourceMetadataError, Rc<dyn Fn()>)>,
    opening_lookup: Cell<bool>,
    restoring_selection: Cell<bool>,
}

impl Drop for BatchView {
    fn drop(&mut self) {
        if let Some(task) = self.task.get_mut().take() {
            task.abort();
        }
    }
}

pub fn build_batch_dialog(
    batch: Arc<MetadataBatch>,
    runtime: tokio::runtime::Handle,
    on_error: Rc<dyn Fn(SourceMetadataError, Rc<dyn Fn()>)>,
) -> adw::Dialog {
    let resource = crate::ui_resource::METADATA_BATCH_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        batch_dialog: adw::Dialog, responsive: adw::BreakpointBin, split: adw::NavigationSplitView,
        header: adw::HeaderBar,
        sidebar: gtk::Box, editor_host: gtk::Box, tracks: gtk::ListView, content: gtk::Stack, lookup: gtk::Button,
        scope_button: gtk::ToggleButton, scope_name: gtk::Label, select_all: gtk::ToggleButton, edit_selected: gtk::Button, show_tracks: gtk::Button,
    });
    gtk_widgets::controls::configure_dialog_header(&header);
    let breakpoint = adw::Breakpoint::new(
        adw::BreakpointCondition::parse("max-width: 760sp").expect("metadata breakpoint"),
    );
    breakpoint.add_setter(&split, "collapsed", Some(&true.to_value()));
    responsive.add_breakpoint(breakpoint);
    split
        .bind_property("collapsed", &edit_selected, "visible")
        .sync_create()
        .build();
    split
        .bind_property("collapsed", &show_tracks, "visible")
        .sync_create()
        .build();
    for (button, show) in [(show_tracks, false), (edit_selected, true)] {
        let weak = split.downgrade();
        button.connect_clicked(move |_| {
            if let Some(split) = weak.upgrade() {
                split.set_show_content(show);
            }
        });
    }
    let weak = split.downgrade();
    tracks.connect_activate(move |_, _| {
        if let Some(split) = weak.upgrade() {
            split.set_show_content(true);
        }
    });
    let load_batch = batch.clone();
    let load: SparseLoad<u64, MetadataTrackSummary> = Rc::new(move |_, range, _| {
        let batch = load_batch.clone();
        Box::pin(async move {
            batch
                .tracks(range.start as u64, range.len() as u64)
                .await
                .map_err(|e| e.to_string())
        })
    });
    let rows = SparseRouteModel::new(
        SparseSource::Query {
            count: batch.track_count() as usize,
        },
        64,
        runtime.clone(),
        load,
    );
    let selection = gtk::MultiSelection::new(Some(rows.list_model()));
    selection.select_all();
    select_all.set_active(true);
    tracks.set_model(Some(&selection));
    tracks.set_factory(Some(&track_factory()));
    super::navigation::connect(&batch_dialog, &tracks, &selection);
    lookup.set_sensitive(batch.external_lookup_allowed());
    let active_selection = RefCell::new(selection.selection());
    let scope_title = batch.scope_title();
    scope_name.set_label(scope_title.as_deref().unwrap_or_default());
    scope_button.set_visible(scope_title.is_some());
    scope_button.set_active(scope_title.is_some());
    if scope_title.is_some() {
        selection.unselect_all();
    }
    let view = Rc::new(BatchView {
        batch,
        scope_button,
        scope_selected: Cell::new(scope_title.is_some()),
        dialog: batch_dialog.downgrade(),
        host: editor_host,
        content,
        lookup,
        sidebar,
        selection,
        current: RefCell::new(None),
        active_selection,
        task: RefCell::new(None),
        rows,
        runtime,
        on_error,
        opening_lookup: Cell::new(false),
        restoring_selection: Cell::new(false),
    });
    let weak = Rc::downgrade(&view);
    let all_button = select_all.downgrade();
    view.selection
        .connect_selection_changed(move |selection, _, _| {
            if let Some(button) = all_button.upgrade() {
                button.set_active(selection.selection().size() == selection.n_items() as u64);
            }
            if let Some(view) = weak.upgrade() {
                if !selection.selection().is_empty() {
                    view.scope_selected.set(false);
                    view.scope_button.set_active(false);
                }
                view.change_scope();
            }
        });
    let weak = Rc::downgrade(&view);
    view.scope_button.connect_clicked(move |button| {
        if let Some(view) = weak.upgrade() {
            button.set_active(true);
            if view.scope_selected.replace(true) {
                return;
            }
            view.restoring_selection.set(true);
            view.selection.unselect_all();
            view.restoring_selection.set(false);
            view.change_scope();
        }
    });
    let weak = Rc::downgrade(&view);
    select_all.connect_clicked(move |button| {
        if let Some(view) = weak.upgrade() {
            view.selection.select_all();
            button.set_active(true);
        }
    });
    let weak = Rc::downgrade(&view);
    view.lookup.connect_clicked(move |_| {
        if let Some(view) = weak.upgrade() {
            view.open_lookup();
        }
    });
    view.change_scope();
    let owner = RefCell::new(Some(view));
    batch_dialog.connect_closed(move |_| {
        owner.borrow_mut().take();
    });
    batch_dialog
}

impl BatchView {
    fn indices(&self) -> Vec<u64> {
        let selected = self.selection.selection();
        if selected.size() == self.batch.track_count() {
            Vec::new()
        } else {
            (0..selected.size())
                .map(|index| selected.nth(index as u32) as u64)
                .collect()
        }
    }

    fn current(&self) -> Option<Rc<super::Editor>> {
        self.current
            .borrow()
            .as_ref()
            .map(|view| view.editor.clone())
    }

    fn mount(self: &Rc<Self>, model: Arc<MetadataEditor>) {
        if let Some(editor) = self.current() {
            if editor.model().draft.kind == model.draft.kind {
                editor.rebind(model);
                return;
            }
            if let Some(view) = self.current.borrow_mut().take() {
                self.host.remove(&view.content);
            }
        }
        let Some(dialog) = self.dialog.upgrade() else {
            return;
        };
        let weak = Rc::downgrade(self);
        let changed = Rc::new(move || {
            if let Some(view) = weak.upgrade() {
                view.refresh_rows();
            }
        });
        let (_, view) = build_editor_view(
            model,
            Some(&dialog),
            Some(self.sidebar.upcast_ref()),
            Some(changed),
            self.runtime.clone(),
        );
        view.editor
            .lookup
            .bind_property("sensitive", &self.lookup, "sensitive")
            .sync_create()
            .build();
        let header = self.lookup.downgrade();
        view.editor
            .pages
            .connect_visible_child_name_notify(move |pages| {
                if let Some(header) = header.upgrade() {
                    header.set_visible(pages.visible_child_name().as_deref() == Some("tags"));
                }
            });
        self.host.append(&view.content);
        self.current.replace(Some(view));
    }

    fn change_scope(self: &Rc<Self>) {
        if self.opening_lookup.get() || self.restoring_selection.get() {
            return;
        }
        let selected = self.selection.selection();
        if selected.is_empty() && !self.scope_selected.get() {
            let previous = self.active_selection.borrow().clone();
            self.restore_selection(&previous);
            return;
        }
        if let Some(task) = self.task.borrow_mut().take() {
            task.abort();
        }
        let indices = self.indices();
        let scope_selected = self.scope_selected.get();
        self.host.set_sensitive(false);
        let previous = self.current().map(|editor| {
            super::lyrics::commit(&editor);
            super::artwork::cancel(&editor);
            editor.lyrics.cancel();
            editor.model()
        });
        let batch = self.batch.clone();
        let weak = Rc::downgrade(self);
        let task = gtk::glib::spawn_future_local(async move {
            let result = async {
                if let Some(previous) = previous {
                    previous.stage_changes().await?;
                }
                if scope_selected {
                    batch.scope_editor().await
                } else {
                    batch.editor(indices).await
                }
            }
            .await;
            if let Some(view) = weak.upgrade() {
                match result {
                    Ok(model) => {
                        view.mount(model);
                        view.active_selection.replace(selected.clone());
                        view.content.set_visible_child_name("editor");
                    }
                    Err(error) => {
                        let current = view.current();
                        if let Some(current) = &current {
                            let previous_scope =
                                current.model().draft.kind != sources::SourceEntityKind::Track;
                            view.scope_selected.set(previous_scope);
                            view.scope_button.set_active(previous_scope);
                            let active = view.active_selection.borrow().clone();
                            view.restore_selection(&active);
                        }
                        if matches!(&error, SourceMetadataError::LocalAccessRequired { .. })
                            || current.is_none()
                        {
                            let weak = Rc::downgrade(&view);
                            let retry = Rc::new(move || {
                                if let Some(view) = weak.upgrade() {
                                    view.scope_selected.set(scope_selected);
                                    view.scope_button.set_active(scope_selected);
                                    view.restore_selection(&selected);
                                    view.change_scope();
                                }
                            });
                            (view.on_error)(error, retry);
                        } else if let Some(current) = current {
                            current.show_error(&error.to_string());
                        }
                    }
                }
                view.host.set_sensitive(true);
                view.refresh_rows();
            }
        });
        self.task.replace(Some(task));
    }

    fn restore_selection(&self, selected: &gtk::Bitset) {
        self.restoring_selection.set(true);
        self.selection.set_selection(
            selected,
            &gtk::Bitset::new_range(0, self.batch.track_count() as u32),
        );
        self.restoring_selection.set(false);
    }

    fn open_lookup(self: &Rc<Self>) {
        if self.opening_lookup.replace(true) {
            return;
        }
        if let Some(task) = self.task.borrow_mut().take() {
            task.abort();
        }
        self.sidebar.set_sensitive(false);
        self.host.set_sensitive(false);
        self.lookup.set_sensitive(false);
        let previous = self.current().map(|editor| {
            super::lyrics::commit(&editor);
            super::artwork::cancel(&editor);
            editor.lyrics.cancel();
            editor.model()
        });
        let batch = self.batch.clone();
        let indices = self.indices();
        let weak = Rc::downgrade(self);
        let task = gtk::glib::spawn_future_local(async move {
            let result = async {
                if let Some(previous) = previous {
                    previous.stage_changes().await?;
                }
                MetadataLookupReview::open_batch(batch, indices).await
            }
            .await;
            if let Some(view) = weak.upgrade() {
                view.opening_lookup.set(false);
                view.sidebar.set_sensitive(true);
                view.host.set_sensitive(true);
                view.lookup
                    .set_sensitive(view.batch.external_lookup_allowed());
                match result {
                    Ok(review) => {
                        if let Some(dialog) = view.dialog.upgrade() {
                            let weak = Rc::downgrade(&view);
                            super::lookup::present(
                                &dialog,
                                review,
                                view.runtime.clone(),
                                Rc::new(move |accepted| {
                                    if let Some(view) = weak.upgrade() {
                                        if accepted
                                            || view.current().is_none()
                                            || view.selection.selection()
                                                != *view.active_selection.borrow()
                                        {
                                            view.change_scope();
                                        }
                                    }
                                }),
                            );
                        }
                    }
                    Err(error) => {
                        if let Some(current) = view.current() {
                            let active = view.active_selection.borrow().clone();
                            view.restore_selection(&active);
                            view.host.set_sensitive(true);
                            current.show_error(&error.to_string());
                        } else {
                            let weak = Rc::downgrade(&view);
                            (view.on_error)(
                                error,
                                Rc::new(move || {
                                    if let Some(view) = weak.upgrade() {
                                        view.change_scope();
                                    }
                                }),
                            );
                        }
                    }
                }
            }
        });
        self.task.replace(Some(task));
    }

    fn refresh_rows(&self) {
        if let Some(first) = self.rows.first_live_position() {
            let batch = self.batch.clone();
            let rows = self.rows.clone();
            gtk::glib::spawn_future_local(async move {
                if let Ok(updated) = batch.tracks(first as u64, 192).await {
                    for row in updated {
                        if rows
                            .peek_ready(row.index as usize)
                            .is_some_and(|current| current.as_ref() == &row)
                        {
                            continue;
                        }
                        rows.update_ready(row.index as usize, |current| *current = row.clone());
                    }
                }
            });
        }
    }
}

pub(super) fn track_factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, object| {
        let item = object.downcast_ref::<gtk::ListItem>().unwrap();
        let track = MetadataTrackRow::new();
        item.set_child(Some(&track));
    });
    connect_sparse_bind(&factory, |object| {
        let item = object.downcast_ref::<gtk::ListItem>().unwrap();
        let track = item.child().and_downcast::<MetadataTrackRow>().unwrap();
        let row = item
            .item()
            .and_downcast::<SparseObjectItem>()
            .and_then(|item| item.value::<Arc<MetadataTrackSummary>>());
        if let Some(row) = row {
            track.imp().title.set_label(&row.title);
            track
                .imp()
                .status
                .set_label(row.error.as_deref().unwrap_or(&row.scan_status));
            track.set_tooltip_text(row.error.as_deref().or(Some(&row.title)));
            if row.modified {
                track.imp().title.add_css_class("heading");
                track.imp().title.add_css_class("accent");
            } else {
                track.imp().title.remove_css_class("heading");
                track.imp().title.remove_css_class("accent");
            }
            if row.error.is_some() {
                track.add_css_class("error");
            } else {
                track.remove_css_class("error");
            }
        } else {
            track.imp().title.set_label("");
            track.imp().status.set_label("");
            track.set_tooltip_text(None);
            track.imp().title.remove_css_class("heading");
            track.imp().title.remove_css_class("accent");
            track.remove_css_class("error");
        }
    });
    factory
}
