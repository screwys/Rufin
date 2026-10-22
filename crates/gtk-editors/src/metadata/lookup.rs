use adw::prelude::*;
use gtk_widgets::sparse_model::{SparseLoad, SparseRouteModel, SparseSource};
use rufin_core::metadata::{
    MetadataLookupField, MetadataLookupItem, MetadataLookupReview, MetadataMatchKind,
    MetadataTrackSummary,
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
    current: Cell<u32>,
    rows: Rc<SparseRouteModel<u64, MetadataTrackSummary>>,
    title: gtk::Label,
    query: gtk::SearchEntry,
    status: gtk::Label,
    matches: gtk::ListBox,
    original: adw::ActionRow,
    fields: gtk::gio::ListStore,
    fill_missing: gtk::CheckButton,
    progress: adw::WindowTitle,
    progress_bar: gtk::ProgressBar,
    kind_labels: [gtk::glib::GString; 4],
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
        header: adw::HeaderBar,
        scope_title: gtk::Label, track_navigation: gtk::Box, sidebar: gtk::Box, tracks: gtk::ListView, show_tracks: gtk::Button,
        title: gtk::Label, refresh: gtk::Button, query: gtk::SearchEntry, status: gtk::Label, matches: gtk::ListBox,
        original: adw::ActionRow, comparison: gtk::ColumnView,
        field_column: gtk::ColumnViewColumn, original_column: gtk::ColumnViewColumn, suggested_column: gtk::ColumnViewColumn,
        fill_missing: gtk::CheckButton, progress: adw::WindowTitle, progress_bar: gtk::ProgressBar,
        recording_label: gtk::Label, release_label: gtk::Label, release_group_label: gtk::Label, artist_label: gtk::Label,
        stop: gtk::Button, cancel: gtk::Button, done: gtk::Button,
        review_content: gtk::Box,
    });
    gtk_widgets::controls::configure_dialog_header(&header);
    let scope = review.scope_title();
    scope_title.set_label(scope.as_deref().unwrap_or_default());
    scope_title.set_visible(scope.is_some());
    if review.progress().total > 1 || scope.is_some() {
        let breakpoint = adw::Breakpoint::new(
            adw::BreakpointCondition::parse("max-width: 760sp")
                .expect("metadata lookup breakpoint"),
        );
        breakpoint.add_setter(&split, "collapsed", Some(&true.to_value()));
        responsive.add_breakpoint(breakpoint);
        split
            .bind_property("collapsed", &show_tracks, "visible")
            .sync_create()
            .build();
    } else {
        split.set_collapsed(true);
        split.set_show_content(true);
        track_navigation.set_visible(false);
        show_tracks.set_visible(false);
    }
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
    super::navigation::connect(&dialog, &tracks, &selection);
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
        current: Cell::new(0),
        rows,
        title,
        query,
        status,
        matches,
        original,
        fields,
        fill_missing,
        progress,
        progress_bar,
        kind_labels: [
            recording_label.label(),
            release_label.label(),
            release_group_label.label(),
            artist_label.label(),
        ],
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
    view.selection.connect_selected_notify(move |selection| {
        if let Some(view) = weak.upgrade() {
            let current = selection.selected();
            if current == view.current.replace(current) {
                return;
            }
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
        let index = view.current.get() as u64;
        let review = view.review.clone();
        let stage = if candidate.is_some() {
            localization::tr("Fetching metadata...")
        } else {
            localization::tr("Reading tags...")
        };
        view.start_action(&stage, async move { review.choose(index, candidate).await });
    });
    let weak = Rc::downgrade(&view);
    view.query.connect_activate(move |_| {
        if let Some(view) = weak.upgrade() {
            view.search(false);
        }
    });
    let weak = Rc::downgrade(&view);
    view.query.connect_changed(move |_| {
        if let Some(view) = weak.upgrade() {
            view.search(true);
        }
    });
    let weak = Rc::downgrade(&view);
    refresh.connect_clicked(move |_| {
        if let Some(view) = weak.upgrade() {
            let review = view.review.clone();
            let index = view.current.get() as u64;
            let query = view.query.text().trim().to_string();
            view.start_action(&localization::tr("Searching MusicBrainz..."), async move {
                review.refresh(index, query).await
            });
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
            let mut restoring = true;
            let mut previous_active = None;
            while let Some(progress) = review.next_update().await {
                let Some(view) = weak.upgrade() else { return };
                let phase_changed = restoring != progress.restoring_cache;
                let visited_start = if phase_changed {
                    0
                } else {
                    previous_active.unwrap_or(0)
                };
                let visited_end = progress.active_track.unwrap_or(visited_start);
                if progress.finished
                    || phase_changed
                    || view.rows.first_live_position().is_some_and(|first| {
                        let first = first as u64;
                        let end = first + 192;
                        visited_start < end && visited_end >= first
                    })
                {
                    view.refresh_rows();
                }
                if !view.busy.get() {
                    view.show_progress(&progress);
                    let current = u64::from(view.current.get());
                    if progress.finished
                        || phase_changed
                        || (current >= visited_start && current <= visited_end)
                    {
                        view.load_item();
                    }
                }
                restoring = progress.restoring_cache;
                previous_active = progress.active_track;
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
            let values = [
                ("stage", progress.stage.as_str()),
                ("completed", &progress.completed.to_string()),
                ("total", &progress.total.to_string()),
                ("cached", &progress.cached.to_string()),
            ];
            if progress.stage.is_empty() {
                localization::tr_with("{completed} of {total} ({cached} cached)", &values)
            } else {
                localization::tr_with("{stage} ({completed} of {total}, {cached} cached)", &values)
            }
        });
        self.progress.set_subtitle(&label);
        self.progress.set_tooltip_text(Some(&label));
        self.progress_bar
            .set_fraction(progress.completed as f64 / progress.total as f64);
        self.progress_bar
            .set_opacity(if progress.finished { 0.0 } else { 1.0 });
        self.stop.set_visible(!progress.finished);
        if progress.failure.is_some() {
            self.progress.add_css_class("error");
        } else {
            self.progress.remove_css_class("error");
        }
    }

    fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
        self.content.set_sensitive(!self.accepting.get());
        self.matches.set_sensitive(!busy);
        self.fill_missing.set_sensitive(!busy);
        self.done.set_sensitive(!busy);
    }

    fn search(self: &Rc<Self>, delayed: bool) {
        if self.accepting.get() || !self.query.is_sensitive() {
            return;
        }
        let query = self.query.text().trim().to_string();
        if query.is_empty() {
            if let Some(task) = self.action.borrow_mut().take() {
                task.abort();
            }
            self.set_busy(false);
            self.show_progress(&self.review.progress());
            self.load_item();
            return;
        }
        let review = self.review.clone();
        let index = self.current.get() as u64;
        self.start_action(&localization::tr("Searching MusicBrainz..."), async move {
            if delayed {
                gtk::glib::timeout_future(std::time::Duration::from_millis(600)).await;
            }
            review.search(index, query).await
        });
    }

    fn message(&self, message: &str) {
        self.status.set_label(message);
        self.status.set_visible(!message.is_empty());
    }

    fn start_action(
        self: &Rc<Self>,
        stage: &str,
        operation: impl std::future::Future<Output = Result<(), sources::SourceMetadataError>> + 'static,
    ) {
        if self.accepting.get() {
            return;
        }
        if let Some(task) = self.action.borrow_mut().take() {
            task.abort();
        }
        if let Some(task) = self.load.borrow_mut().take() {
            task.abort();
        }
        self.set_busy(true);
        self.progress.set_subtitle(stage);
        self.progress_bar.set_opacity(1.0);
        self.progress_bar.pulse();
        let weak = Rc::downgrade(self);
        let task = gtk::glib::spawn_future_local(async move {
            let result = operation.await;
            if let Some(view) = weak.upgrade() {
                view.set_busy(false);
                view.show_progress(&view.review.progress());
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
        let index = self.current.get();
        if index == gtk::INVALID_LIST_POSITION {
            return;
        }
        let review = self.review.clone();
        let weak = Rc::downgrade(self);
        let task = gtk::glib::spawn_future_local(async move {
            let item = review.item(index as u64).await;
            if let Some(view) = weak.upgrade()
                && view.current.get() == index
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
        if item.failure.is_none() && item.ready && item.candidates.is_empty() {
            self.message(&localization::tr("No results"));
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
                let row = adw::ActionRow::builder()
                    .title(&candidate.title)
                    .subtitle(&candidate.detail)
                    .use_markup(false)
                    .activatable(true)
                    .build();
                let kind = match candidate.kind {
                    MetadataMatchKind::Recording => 0,
                    MetadataMatchKind::Release => 1,
                    MetadataMatchKind::ReleaseGroup => 2,
                    MetadataMatchKind::Artist => 3,
                };
                row.add_suffix(
                    &gtk::Label::builder()
                        .label(self.kind_labels[kind].as_str())
                        .css_classes(["caption", "dim-label"])
                        .build(),
                );
                self.matches.append(&row);
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
            .hexpand(true)
            .vexpand(true)
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
        if field.changed {
            label.add_css_class("metadata-lookup-changed");
        } else {
            label.remove_css_class("metadata-lookup-changed");
        }
    });
    factory
}
