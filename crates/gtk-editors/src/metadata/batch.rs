use super::{MetadataView, build_editor_view};
use adw::prelude::*;
use gtk_widgets::sparse_model::{
    SparseLoad, SparseObjectItem, SparseRouteModel, SparseSource, connect_sparse_bind,
};
use rufin_core::metadata::{MetadataBatch, MetadataEditor, MetadataTrackSummary};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};

struct BatchView {
    batch: Arc<MetadataBatch>,
    dialog: gtk::glib::WeakRef<adw::Dialog>,
    host: gtk::Box,
    sidebar: gtk::Box,
    selection: gtk::MultiSelection,
    current: RefCell<Option<MetadataView>>,
    loading: Cell<bool>,
    active_selection: RefCell<gtk::Bitset>,
    task: RefCell<Option<gtk::glib::JoinHandle<()>>>,
    rows: Rc<SparseRouteModel<u64, MetadataTrackSummary>>,
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
    model: Arc<MetadataEditor>,
    runtime: tokio::runtime::Handle,
) -> adw::Dialog {
    let resource = crate::ui_resource::METADATA_BATCH_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        batch_dialog: adw::Dialog, responsive: adw::BreakpointBin, split: adw::NavigationSplitView,
        sidebar: gtk::Box, editor_host: gtk::Box, tracks: gtk::ListView,
        select_all: gtk::ToggleButton, edit_selected: gtk::Button, show_tracks: gtk::Button,
    });
    let breakpoint = adw::Breakpoint::new(
        adw::BreakpointCondition::parse("max-width: 760sp").expect("metadata breakpoint"),
    );
    breakpoint.add_setter(&split, "collapsed", Some(&true.to_value()));
    responsive.add_breakpoint(breakpoint);
    split
        .bind_property("collapsed", &edit_selected, "visible")
        .sync_create()
        .build();
    select_all.set_active(true);
    split
        .bind_property("collapsed", &show_tracks, "visible")
        .sync_create()
        .build();
    let weak_split = split.downgrade();
    show_tracks.connect_clicked(move |_| {
        if let Some(split) = weak_split.upgrade() {
            split.set_show_content(false);
        }
    });
    let weak_split = split.downgrade();
    edit_selected.connect_clicked(move |_| {
        if let Some(split) = weak_split.upgrade() {
            split.set_show_content(true);
        }
    });
    let weak_split = split.downgrade();
    tracks.connect_activate(move |_, _| {
        if let Some(split) = weak_split.upgrade() {
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
                .map_err(|error| error.to_string())
        })
    });
    let rows = SparseRouteModel::new(
        SparseSource::Query {
            count: batch.track_count() as usize,
        },
        64,
        runtime,
        load,
    );
    let selection = gtk::MultiSelection::new(Some(rows.list_model()));
    selection.select_all();
    tracks.set_model(Some(&selection));
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, object| {
        let item = object.downcast_ref::<gtk::ListItem>().unwrap();
        let resource = crate::ui_resource::METADATA_TRACK_RESOURCE;
        let builder = crate::ui_resource::builder(resource);
        let track: adw::ActionRow = crate::ui_resource::object(&builder, resource, "track");
        item.set_child(Some(&track));
    });
    connect_sparse_bind(&factory, |object| {
        let item = object.downcast_ref::<gtk::ListItem>().unwrap();
        let track = item.child().and_downcast::<adw::ActionRow>().unwrap();
        let row = item
            .item()
            .and_downcast::<SparseObjectItem>()
            .and_then(|item| item.value::<Arc<MetadataTrackSummary>>());
        if let Some(row) = row {
            track.set_title(&row.title);
            track.set_subtitle(row.error.as_deref().unwrap_or(&row.scan_status));
            track.set_tooltip_text(row.error.as_deref().or(Some(&row.title)));
            if row.modified {
                track.add_css_class("heading");
            } else {
                track.remove_css_class("heading");
            }
            if row.error.is_some() {
                track.add_css_class("error");
            } else {
                track.remove_css_class("error");
            }
        } else {
            track.set_title("");
            track.set_subtitle("");
            track.set_tooltip_text(None);
            track.remove_css_class("heading");
            track.remove_css_class("error");
        }
    });
    tracks.set_factory(Some(&factory));
    let active_selection = RefCell::new(selection.selection());
    let view = Rc::new(BatchView {
        batch,
        dialog: batch_dialog.downgrade(),
        host: editor_host,
        sidebar,
        selection,
        current: RefCell::new(None),
        loading: Cell::new(false),
        active_selection,
        task: RefCell::new(None),
        rows,
    });
    view.mount(model);
    let weak = Rc::downgrade(&view);
    let all_button = select_all.downgrade();
    view.selection
        .connect_selection_changed(move |selection, _, _| {
            if let Some(button) = all_button.upgrade() {
                button.set_active(selection.selection().size() == selection.n_items() as u64);
            }
            if let Some(view) = weak.upgrade() {
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
    let anchor = RefCell::new(Some(view));
    batch_dialog.connect_closed(move |_| {
        anchor.borrow_mut().take();
    });
    batch_dialog
}

impl BatchView {
    fn mount(self: &Rc<Self>, model: Arc<MetadataEditor>) {
        let Some(dialog) = self.dialog.upgrade() else {
            return;
        };
        let current = self
            .current
            .borrow()
            .as_ref()
            .map(|view| view.editor.clone());
        if let Some(editor) = current {
            editor.rebind(model);
            return;
        }
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
        );
        self.host.append(&view.content);
        self.current.replace(Some(view));
    }

    fn change_scope(self: &Rc<Self>) {
        if self.loading.get() {
            return;
        }
        let selected = self.selection.selection();
        if selected.is_empty() {
            self.loading.set(true);
            self.selection.set_selection(
                &self.active_selection.borrow(),
                &gtk::Bitset::new_range(0, self.batch.track_count() as u32),
            );
            self.loading.set(false);
            return;
        }
        let indices = if selected.size() == self.batch.track_count() {
            Vec::new()
        } else {
            (0..selected.size())
                .map(|index| selected.nth(index as u32) as u64)
                .collect()
        };
        self.loading.set(true);
        self.sidebar.set_sensitive(false);
        self.host.set_sensitive(false);
        let current = self
            .current
            .borrow()
            .as_ref()
            .expect("metadata view mounted")
            .editor
            .clone();
        super::lyrics::commit(&current);
        let previous = current.model();
        let batch = self.batch.clone();
        let weak = Rc::downgrade(self);
        let task = gtk::glib::spawn_future_local(async move {
            let result = async {
                previous.stage_changes().await?;
                batch.editor(indices).await
            }
            .await;
            if let Some(view) = weak.upgrade() {
                match result {
                    Ok(model) => {
                        view.mount(model);
                        view.active_selection.replace(selected);
                    }
                    Err(error) => {
                        view.selection.set_selection(
                            &view.active_selection.borrow(),
                            &gtk::Bitset::new_range(0, view.batch.track_count() as u32),
                        );
                        if let Some(dialog) = view.dialog.upgrade() {
                            super::metadata_error_dialog(&error.to_string()).present(Some(&dialog));
                        }
                    }
                }
                view.sidebar.set_sensitive(true);
                view.host.set_sensitive(true);
                view.loading.set(false);
                view.refresh_rows();
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
