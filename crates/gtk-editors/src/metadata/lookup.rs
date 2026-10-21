use adw::prelude::*;
use gtk_widgets::sparse_model::{SparseLoad, SparseRouteModel, SparseSource};
use rufin_core::metadata::{
    MetadataLookupField, MetadataLookupItem, MetadataLookupReview, MetadataTrackSummary,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};

struct ReviewView {
    review: Arc<MetadataLookupReview>,
    dialog: gtk::glib::WeakRef<adw::Dialog>,
    selection: gtk::SingleSelection,
    rows: Rc<SparseRouteModel<u64, MetadataTrackSummary>>,
    title: gtk::Label,
    query: gtk::SearchEntry,
    status: gtk::Label,
    matches: gtk::ListBox,
    original: adw::ActionRow,
    fields: gtk::gio::ListStore,
    fill_missing: gtk::CheckButton,
    progress: gtk::Label,
    stop: gtk::Button,
    done: gtk::Button,
    cancel: gtk::Button,
    sidebar: gtk::Box,
    accepting: Cell<bool>,
    accepted: Cell<bool>,
    content: gtk::Box,
    busy: Cell<bool>,
    updating: Cell<bool>,
    last: RefCell<Option<MetadataLookupItem>>,
    load: RefCell<Option<gtk::glib::JoinHandle<()>>>,
    action: RefCell<Option<gtk::glib::JoinHandle<()>>>,
    run: RefCell<Option<gtk::glib::JoinHandle<()>>>,
    updates: RefCell<Option<gtk::glib::JoinHandle<()>>>,
}

impl Drop for ReviewView {
    fn drop(&mut self) {
        self.review.cancel();
        for task in [
            &mut self.load,
            &mut self.action,
            &mut self.run,
            &mut self.updates,
        ] {
            if let Some(task) = task.get_mut().take() {
                task.abort();
            }
        }
    }
}

pub(super) fn present(
    parent: &adw::Dialog,
    review: Arc<MetadataLookupReview>,
    runtime: tokio::runtime::Handle,
    on_close: Rc<dyn Fn(bool)>,
) {
    let resource = crate::ui_resource::METADATA_LOOKUP_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        dialog: adw::Dialog, responsive: adw::BreakpointBin, split: adw::NavigationSplitView,
        sidebar: gtk::Box, tracks: gtk::ListView, show_tracks: gtk::Button,
        title: gtk::Label, query: gtk::SearchEntry, status: gtk::Label, matches: gtk::ListBox,
        original: adw::ActionRow, comparison: gtk::ColumnView,
        field_column: gtk::ColumnViewColumn, original_column: gtk::ColumnViewColumn, suggested_column: gtk::ColumnViewColumn,
        fill_missing: gtk::CheckButton, progress: gtk::Label, stop: gtk::Button, cancel: gtk::Button, done: gtk::Button,
        review_content: gtk::Box,
    });
    let breakpoint = adw::Breakpoint::new(
        adw::BreakpointCondition::parse("max-width: 760sp").expect("metadata lookup breakpoint"),
    );
    breakpoint.add_setter(&split, "collapsed", Some(&true.to_value()));
    responsive.add_breakpoint(breakpoint);
    split
        .bind_property("collapsed", &show_tracks, "visible")
        .sync_create()
        .build();
    let weak = split.downgrade();
    show_tracks.connect_clicked(move |_| {
        if let Some(split) = weak.upgrade() {
            split.set_show_content(false);
        }
    });
    let weak = split.downgrade();
    tracks.connect_activate(move |_, _| {
        if let Some(split) = weak.upgrade() {
            split.set_show_content(true);
        }
    });
    let load_review = review.clone();
    let load: SparseLoad<u64, MetadataTrackSummary> = Rc::new(move |_, range, _| {
        let review = load_review.clone();
        Box::pin(async move {
            review
                .tracks(range.start as u64, range.len() as u64)
                .await
                .map_err(|e| e.to_string())
        })
    });
    let rows = SparseRouteModel::new(
        SparseSource::Query {
            count: review.progress().total as usize,
        },
        64,
        runtime,
        load,
    );
    let selection = gtk::SingleSelection::new(Some(rows.list_model()));
    tracks.set_model(Some(&selection));
    tracks.set_factory(Some(&super::batch::track_factory()));
    super::navigation::connect(&sidebar, &tracks, &selection);
    let fields = gtk::gio::ListStore::new::<gtk::glib::BoxedAnyObject>();
    comparison.set_model(Some(&gtk::NoSelection::new(Some(fields.clone()))));
    for (column, value) in [
        (field_column, 0),
        (original_column, 1),
        (suggested_column, 2),
    ] {
        column.set_factory(Some(&field_factory(value)));
    }
    let view = Rc::new(ReviewView {
        review,
        dialog: dialog.downgrade(),
        selection,
        rows,
        title,
        query,
        status,
        matches,
        original,
        fields,
        fill_missing,
        progress,
        stop,
        done,
        cancel: cancel.clone(),
        sidebar,
        accepting: Cell::new(false),
        accepted: Cell::new(false),
        content: review_content,
        busy: Cell::new(false),
        updating: Cell::new(false),
        last: RefCell::new(None),
        load: RefCell::new(None),
        action: RefCell::new(None),
        run: RefCell::new(None),
        updates: RefCell::new(None),
    });
    let weak = Rc::downgrade(&view);
    view.selection.connect_selected_notify(move |_| {
        if let Some(view) = weak.upgrade() {
            if view.busy.get() && !view.accepting.get() {
                if let Some(task) = view.action.borrow_mut().take() {
                    task.abort();
                }
                view.set_busy(false);
            }
            view.matches.set_sensitive(false);
            view.query.set_sensitive(false);
            view.query.set_text("");
            view.last.borrow_mut().take();
            view.load_item();
        }
    });
    let weak = Rc::downgrade(&view);
    view.matches.connect_row_selected(move |_, row| {
        let Some(view) = weak.upgrade() else { return };
        if view.updating.get() || view.busy.get() {
            return;
        }
        let candidate = row.filter(|row| row.index() > 0).and_then(|row| {
            view.last
                .borrow()
                .as_ref()
                .and_then(|item| item.candidates.get(row.index() as usize - 1))
                .cloned()
        });
        let index = view.selection.selected() as u64;
        let review = view.review.clone();
        view.start_action(async move { review.choose(index, candidate).await });
    });
    let weak = Rc::downgrade(&view);
    view.fill_missing.connect_toggled(move |_| {
        if let Some(view) = weak.upgrade() {
            view.load_item();
        }
    });
    let weak = Rc::downgrade(&view);
    view.query.connect_activate(move |query| {
        if let Some(view) = weak.upgrade()
            && !view.busy.get()
        {
            let review = view.review.clone();
            let index = view.selection.selected() as u64;
            let query = query.text().to_string();
            view.start_action(async move { review.search(index, query).await });
        }
    });
    let weak = Rc::downgrade(&view);
    view.stop.connect_clicked(move |_| {
        if let Some(view) = weak.upgrade() {
            view.review.cancel();
        }
    });
    let close = dialog.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(dialog) = close.upgrade() {
            dialog.force_close();
        }
    });
    let weak = Rc::downgrade(&view);
    view.done.connect_clicked(move |_| {
        let Some(view) = weak.upgrade() else { return };
        let review = view.review.clone();
        let fill_missing = view.fill_missing.is_active();
        view.accepting.set(true);
        view.cancel.set_sensitive(false);
        view.sidebar.set_sensitive(false);
        if let Some(dialog) = view.dialog.upgrade() {
            dialog.set_can_close(false);
        }
        view.set_busy(true);
        let weak = Rc::downgrade(&view);
        let task = gtk::glib::spawn_future_local(async move {
            let result = review.accept(fill_missing).await;
            if let Some(view) = weak.upgrade() {
                match result {
                    Ok(()) => {
                        view.accepted.set(true);
                        if let Some(dialog) = view.dialog.upgrade() {
                            dialog.force_close();
                        }
                    }
                    Err(error) => {
                        view.accepting.set(false);
                        view.cancel.set_sensitive(true);
                        view.sidebar.set_sensitive(true);
                        if let Some(dialog) = view.dialog.upgrade() {
                            dialog.set_can_close(true);
                        }
                        view.message(&error.to_string());
                        view.set_busy(false);
                    }
                }
            }
        });
        view.action.replace(Some(task));
    });
    let review = view.review.clone();
    view.run
        .replace(Some(gtk::glib::spawn_future_local(async move {
            review.run().await;
        })));
    let review = view.review.clone();
    let weak = Rc::downgrade(&view);
    view.updates
        .replace(Some(gtk::glib::spawn_future_local(async move {
            while let Some(progress) = review.next_update().await {
                let Some(view) = weak.upgrade() else { return };
                view.show_progress(&progress);
                view.refresh_rows();
                if !view.busy.get() {
                    view.load_item();
                }
                if progress.finished {
                    return;
                }
            }
        })));
    view.show_progress(&view.review.progress());
    view.load_item();
    dialog.present(Some(parent));
    let owner = RefCell::new(Some(view));
    let weak_dialog = dialog.downgrade();
    let parent_handler = RefCell::new(Some(parent.connect_closed(move |_| {
        if let Some(dialog) = weak_dialog.upgrade() {
            dialog.force_close();
        }
    })));
    let parent = parent.downgrade();
    dialog.connect_closed(move |_| {
        if let Some(parent) = parent.upgrade()
            && let Some(handler) = parent_handler.borrow_mut().take()
        {
            parent.disconnect(handler);
        }
        let accepted = owner
            .borrow_mut()
            .take()
            .is_some_and(|view| view.accepted.get());
        on_close(accepted);
    });
}

impl ReviewView {
    fn show_progress(&self, progress: &rufin_core::metadata::MetadataLookupProgress) {
        let label = progress.failure.clone().unwrap_or_else(|| {
            localization::tr_with(
                "{completed} of {total}",
                &[
                    ("completed", &progress.completed.to_string()),
                    ("total", &progress.total.to_string()),
                ],
            )
        });
        self.progress.set_label(&label);
        self.progress.set_tooltip_text(Some(&label));
        self.stop.set_visible(!progress.finished);
        if progress.failure.is_some() {
            self.progress.add_css_class("error");
        } else {
            self.progress.remove_css_class("error");
        }
    }

    fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
        self.content.set_sensitive(!busy);
        self.done.set_sensitive(!busy);
    }

    fn message(&self, message: &str) {
        self.status.set_label(message);
        self.status.set_visible(!message.is_empty());
    }

    fn start_action(
        self: &Rc<Self>,
        operation: impl std::future::Future<Output = Result<(), sources::SourceMetadataError>> + 'static,
    ) {
        if self.busy.get() {
            return;
        }
        if let Some(task) = self.load.borrow_mut().take() {
            task.abort();
        }
        self.set_busy(true);
        let weak = Rc::downgrade(self);
        let task = gtk::glib::spawn_future_local(async move {
            let result = operation.await;
            if let Some(view) = weak.upgrade() {
                view.set_busy(false);
                if result.is_err() {
                    view.last.borrow_mut().take();
                }
                view.load_item();
                view.refresh_rows();
                if let Err(error) = result {
                    view.message(&error.to_string());
                }
            }
        });
        self.action.replace(Some(task));
    }

    fn load_item(self: &Rc<Self>) {
        if self.busy.get() {
            return;
        }
        if let Some(task) = self.load.borrow_mut().take() {
            task.abort();
        }
        let index = self.selection.selected();
        if index == gtk::INVALID_LIST_POSITION {
            return;
        }
        let review = self.review.clone();
        let fill_missing = self.fill_missing.is_active();
        let weak = Rc::downgrade(self);
        let task = gtk::glib::spawn_future_local(async move {
            let item = review.item(index as u64, fill_missing).await;
            if let Some(view) = weak.upgrade()
                && view.selection.selected() == index
            {
                match item {
                    Ok(item) => view.render(item),
                    Err(error) => view.message(&error.to_string()),
                }
            }
        });
        self.load.replace(Some(task));
    }

    fn render(&self, item: MetadataLookupItem) {
        self.title.set_label(&item.title);
        self.message(item.failure.as_deref().unwrap_or(""));
        let finished = self.review.progress().finished;
        if item.failure.is_none() {
            if !item.ready && !finished {
                self.message(&localization::tr("Looking up..."));
            } else if item.ready && item.candidates.is_empty() {
                self.message(&localization::tr("No results"));
            }
        }
        self.query.set_sensitive(item.ready || finished);
        self.matches.set_sensitive(item.ready);
        let (candidates_changed, fields_changed) = {
            let last = self.last.borrow();
            (
                last.as_ref()
                    .is_none_or(|last| last.candidates != item.candidates),
                last.as_ref().is_none_or(|last| last.fields != item.fields),
            )
        };
        self.updating.set(true);
        if candidates_changed {
            self.matches.remove_all();
            self.matches.append(&self.original);
            for candidate in &item.candidates {
                self.matches.append(
                    &adw::ActionRow::builder()
                        .title(&candidate.title)
                        .subtitle(&candidate.detail)
                        .use_markup(false)
                        .activatable(true)
                        .build(),
                );
            }
        }
        let selected = item
            .selected
            .as_ref()
            .and_then(|id| {
                item.candidates
                    .iter()
                    .position(|candidate| &candidate.id == id)
            })
            .map_or(0, |index| index + 1);
        self.matches
            .select_row(self.matches.row_at_index(selected as i32).as_ref());
        if fields_changed {
            self.fields.remove_all();
            for field in &item.fields {
                if !field.original.is_empty() || !field.suggested.is_empty() {
                    self.fields
                        .append(&gtk::glib::BoxedAnyObject::new(field.clone()));
                }
            }
        }
        self.last.replace(Some(item));
        self.updating.set(false);
    }

    fn refresh_rows(&self) {
        if let Some(first) = self.rows.first_live_position() {
            let rows = self.rows.clone();
            let review = self.review.clone();
            gtk::glib::spawn_future_local(async move {
                if let Ok(updated) = review.tracks(first as u64, 192).await {
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

fn field_factory(column: u8) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, object| {
        let item = object.downcast_ref::<gtk::ListItem>().unwrap();
        let label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(28)
            .selectable(true)
            .build();
        item.set_child(Some(&label));
    });
    factory.connect_bind(move |_, object| {
        let item = object.downcast_ref::<gtk::ListItem>().unwrap();
        let label = item.child().and_downcast::<gtk::Label>().unwrap();
        let value = item
            .item()
            .and_downcast::<gtk::glib::BoxedAnyObject>()
            .unwrap();
        let field = value.borrow::<MetadataLookupField>();
        label.set_label(match column {
            0 => &field.label,
            1 => &field.original,
            _ => &field.suggested,
        });
        label.set_tooltip_text(Some(match column {
            0 => &field.label,
            1 => &field.original,
            _ => &field.suggested,
        }));
        if column == 2 && field.changed {
            label.add_css_class("accent");
        } else {
            label.remove_css_class("accent");
        }
    });
    factory
}
