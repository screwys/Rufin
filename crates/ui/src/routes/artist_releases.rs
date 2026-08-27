use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::layout::{configure_fill_width_clip, width_allocation_owner};
use crate::shell::Shell;
use crate::{LibraryField, LibraryLayout, LibraryListKey, LibraryListSettings};

use super::cards;
use super::collections::{CollectionTableProjection, album_table, library_route_inset};
use super::grid_cells::{AlbumGridCell, ReusableCollectionGridCell, collection_grid_column_count};
use super::library_fields::{COLLECTION_GRID_MIN_CARD_WIDTH, item_at};
use super::route_layout::{
    PRIMARY_ROUTE_HORIZONTAL_INSET, ROUTE_TOP_MARGIN, detail_route_scroller,
};
use super::route_shell::LibraryToolbarProjection;
use super::sparse_model::SparseRouteModel;

const ARTIST_RELEASE_SECTION_GAP: i32 = 18;
const ARTIST_RELEASE_HEADER_GAP: i32 = 10;

#[derive(Clone)]
pub(super) struct ArtistRouteSearchTarget {
    pub(super) search: gtk::SearchEntry,
    pub(super) focus: Rc<dyn Fn()>,
}

pub(super) struct ArtistReleaseRoutePreamble {
    pub(super) header: gtk::Widget,
    pub(super) favorite: Option<(gtk::Widget, gtk::SearchEntry)>,
    pub(super) favorite_present: bool,
    pub(super) empty: gtk::Widget,
}

#[derive(Clone)]
pub(super) struct ArtistReleaseProjections {
    sections: Rc<Vec<Rc<ArtistAlbumProjection>>>,
    orders: Rc<RefCell<[Vec<library::AlbumKey>; 6]>>,
    sparse: Rc<SparseRouteModel<library::AlbumKey, library::AlbumRow>>,
    lane: Rc<super::named_detail::NamedOrderLane>,
    surface: gtk::Widget,
    layout: Rc<Cell<LibraryLayout>>,
    favorite: Option<gtk::Widget>,
    favorite_present: Rc<Cell<bool>>,
    search_targets: Rc<HashMap<ArtistRouteTarget, ArtistRouteSearchTarget>>,
    apply_grid_fields: Rc<dyn Fn(&[LibraryField])>,
    grid_fields: Rc<RefCell<Vec<LibraryField>>>,
    empty: gtk::Widget,
}

struct ArtistAlbumProjection {
    sparse: Rc<SparseRouteModel<library::AlbumKey, library::AlbumRow>>,
    model: gtk::SliceListModel,
    start: Cell<usize>,
    count: Cell<usize>,
    source_present: Cell<bool>,
    search: gtk::SearchEntry,
    header: gtk::Widget,
    toolbar: LibraryToolbarProjection,
    rows: gio::ListStore,
    row_table: RefCell<Option<CollectionTableProjection>>,
    row_surface: RefCell<Option<gtk::Widget>>,
    body_layout: Cell<Option<LibraryLayout>>,
    layout: Rc<Cell<LibraryLayout>>,
    columns: Rc<Cell<usize>>,
    applied_settings: RefCell<LibraryListSettings>,
    shell: Weak<Shell>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum ArtistRouteTarget {
    Favorite,
    Release(usize),
}

#[derive(Clone)]
enum ArtistRouteRow {
    Static {
        widget: gtk::Widget,
    },
    AlbumGrid {
        section: Weak<ArtistAlbumProjection>,
        start: usize,
        len: usize,
        columns: usize,
        margin_bottom: i32,
    },
}

struct ArtistRouteListCell {
    root: gtk::Box,
    grid_cells: Vec<ArtistGridSlot>,
    grid_columns: usize,
    grid_mode: bool,
}

struct ArtistGridSlot {
    cell: AlbumGridCell,
    widget: gtk::Widget,
}

impl ArtistAlbumProjection {
    fn new(
        shell: &Rc<Shell>,
        title: &'static str,
        sparse: Rc<SparseRouteModel<library::AlbumKey, library::AlbumRow>>,
        start: usize,
        count: usize,
        layout: Rc<Cell<LibraryLayout>>,
        columns: Rc<Cell<usize>>,
    ) -> Rc<Self> {
        let search = gtk::SearchEntry::new();
        search.set_placeholder_text(Some(&localization::tr("Search")));
        search.set_hexpand(true);
        let toolbar =
            shell.library_toolbar_projection(LibraryListKey::ArtistAlbums, search.clone());
        let header = gtk::Box::new(gtk::Orientation::Vertical, ARTIST_RELEASE_HEADER_GAP);
        header.set_hexpand(true);
        header.set_halign(gtk::Align::Fill);
        let heading = crate::localization::localized_label(title);
        heading.add_css_class("section-heading");
        heading.set_xalign(0.0);
        header.append(&heading);
        header.append(&toolbar.widget());
        let header: gtk::Widget = header.upcast();
        let rows = static_artist_route_model(header.clone());

        let settings = shell
            .settings
            .current
            .borrow()
            .library_list(LibraryListKey::ArtistAlbums);
        let source_present = count != 0;
        let model = gtk::SliceListModel::new(
            Some(sparse.list_model()),
            start.min(u32::MAX as usize) as u32,
            count.min(u32::MAX as usize) as u32,
        );
        let projection = Rc::new(Self {
            sparse,
            model,
            start: Cell::new(start),
            count: Cell::new(count),
            source_present: Cell::new(source_present),
            search,
            header,
            toolbar,
            rows,
            row_table: RefCell::new(None),
            row_surface: RefCell::new(None),
            body_layout: Cell::new(None),
            layout,
            columns,
            applied_settings: RefCell::new(settings),
            shell: Rc::downgrade(shell),
        });
        let weak = Rc::downgrade(&projection);
        projection
            .sparse
            .connect_ready_changed(move |position, count| {
                if let Some(projection) = weak.upgrade() {
                    projection.refresh_grid_range(position, count);
                }
            });
        projection.refresh_body();
        projection
    }

    fn source_is_empty(&self) -> bool {
        !self.source_present.get()
    }

    fn search(&self) -> gtk::SearchEntry {
        self.search.clone()
    }

    fn rows(&self) -> gio::ListStore {
        self.rows.clone()
    }

    fn set_range(self: &Rc<Self>, start: usize, count: usize, authoritative: bool) {
        if authoritative {
            self.source_present.set(count != 0);
        }
        self.start.set(start);
        self.count.set(count);
        self.model.set_offset(start.min(u32::MAX as usize) as u32);
        self.model.set_size(count.min(u32::MAX as usize) as u32);
        self.refresh_body();
    }

    fn row_widget(&self, settings: &LibraryListSettings) -> gtk::Widget {
        if let Some(surface) = self.row_surface.borrow().as_ref() {
            if let Some(table) = self.row_table.borrow().as_ref() {
                table.apply_fields(&settings.row_fields);
            }
            return surface.clone();
        }
        let Some(shell) = self.shell.upgrade() else {
            return gtk::Box::new(gtk::Orientation::Vertical, 0).upcast();
        };
        let table = album_table(
            &shell,
            self.model.clone(),
            LibraryListKey::ArtistAlbums,
            None,
        );
        table.apply_fields(&settings.row_fields);
        let clip = non_propagating_width_scroller();
        clip.set_child(Some(&table.widget()));
        let resize_table = table.clone();
        let resize_clip = clip.clone();
        let surface = width_allocation_owner(&clip, move |width| {
            resize_table.fit_scroller_allocation(&resize_clip, width);
        })
        .upcast::<gtk::Widget>();
        self.row_table.replace(Some(table));
        self.row_surface.replace(Some(surface.clone()));
        surface
    }

    fn refresh_body(self: &Rc<Self>) {
        if self.source_is_empty() {
            self.header.set_visible(false);
            self.header.set_margin_bottom(0);
            replace_artist_release_body(&self.rows, Vec::new());
            self.body_layout.set(None);
            return;
        }
        self.header.set_visible(true);
        if self.layout.get() == LibraryLayout::Row
            && self.body_layout.get() == Some(LibraryLayout::Row)
        {
            self.header.set_margin_bottom(ARTIST_RELEASE_HEADER_GAP);
            let settings = self.applied_settings.borrow().clone();
            let row = self.row_widget(&settings);
            row.set_margin_bottom(ARTIST_RELEASE_SECTION_GAP);
            return;
        }
        match self.layout.get() {
            LibraryLayout::Row => {
                self.header.set_margin_bottom(ARTIST_RELEASE_HEADER_GAP);
                let settings = self.applied_settings.borrow().clone();
                let row = self.row_widget(&settings);
                row.set_margin_bottom(ARTIST_RELEASE_SECTION_GAP);
                replace_artist_release_body(
                    &self.rows,
                    vec![ArtistRouteRow::Static { widget: row }],
                );
            }
            LibraryLayout::Grid | LibraryLayout::Detail => {
                self.header.set_margin_bottom(if self.count.get() == 0 {
                    ARTIST_RELEASE_SECTION_GAP
                } else {
                    ARTIST_RELEASE_HEADER_GAP
                });
                replace_artist_release_body(&self.rows, self.grid_rows(0, self.grid_row_count()));
            }
        }
        self.body_layout.set(Some(self.layout.get()));
    }

    fn grid_row_count(&self) -> usize {
        self.count.get().div_ceil(self.columns.get().max(1))
    }

    fn grid_rows(self: &Rc<Self>, start_row: usize, end_row: usize) -> Vec<ArtistRouteRow> {
        let columns = self.columns.get().max(1);
        let count = self.count.get();
        let row_count = self.grid_row_count();
        (start_row..end_row.min(row_count))
            .map(|row| {
                let local_start = row * columns;
                ArtistRouteRow::AlbumGrid {
                    section: Rc::downgrade(self),
                    start: self.start.get() + local_start,
                    len: (count - local_start).min(columns),
                    columns,
                    margin_bottom: if row + 1 == row_count {
                        ARTIST_RELEASE_SECTION_GAP
                    } else {
                        0
                    },
                }
            })
            .collect()
    }

    fn refresh_grid_range(self: &Rc<Self>, position: u32, count: u32) {
        if self.layout.get() != LibraryLayout::Grid || self.source_is_empty() {
            return;
        }
        let section_start = self.start.get();
        let section_end = section_start.saturating_add(self.count.get());
        let change_start = position as usize;
        let change_end = change_start.saturating_add(count as usize);
        if change_end <= section_start || change_start >= section_end {
            return;
        }
        let columns = self.columns.get().max(1);
        let row_count = self.grid_row_count();
        if row_count == 0 {
            return;
        }
        let local_start = change_start.saturating_sub(section_start);
        let local_end = change_end.min(section_end).saturating_sub(section_start);
        let first = (local_start / columns).min(row_count - 1);
        let end = local_end
            .max(local_start.saturating_add(1))
            .div_ceil(columns)
            .min(row_count)
            .max(first + 1);
        let additions = self
            .grid_rows(first, end)
            .into_iter()
            .map(glib::BoxedAnyObject::new)
            .collect::<Vec<_>>();
        self.rows
            .splice((first + 1) as u32, (end - first) as u32, &additions);
    }

    fn apply_settings(self: &Rc<Self>, settings: &LibraryListSettings) {
        self.toolbar.apply(LibraryListKey::ArtistAlbums, settings);
        self.applied_settings.replace(settings.clone());
        self.refresh_body();
    }
}

impl ArtistReleaseProjections {
    pub(super) fn new(
        shell: &Rc<Shell>,
        selected: &crate::runtime::SelectedLibrary,
        preamble: ArtistReleaseRoutePreamble,
        titles: [&'static str; 6],
        orders: [Vec<library::AlbumKey>; 6],
        first_rows: Vec<library::AlbumRow>,
    ) -> Self {
        let settings = shell
            .settings
            .current
            .borrow()
            .library_list(LibraryListKey::ArtistAlbums);
        let layout = Rc::new(Cell::new(normalized_artist_layout(settings.layout)));
        let columns = Rc::new(Cell::new(1));
        let flat_order = orders.iter().flatten().copied().collect::<Vec<_>>();
        let database = Arc::clone(&selected.database);
        let source = selected.source_key;
        let folder = selected.music_folder_key;
        let load = Arc::new(
            move |keys: Vec<library::AlbumKey>, cancellation: library::ReadCancellation| {
                let database = Arc::clone(&database);
                Box::pin(async move {
                    database
                        .album_rows(source, &keys, folder, &cancellation)
                        .await
                        .map_err(|error| error.to_string())
                }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
            },
        );
        let sparse = SparseRouteModel::new(flat_order, 32, selected.runtime.clone(), load);
        sparse.seed_matching(first_rows, |row| row.album_key);
        let orders = Rc::new(RefCell::new(orders));
        let mut start = 0_usize;
        let sections = Rc::new(
            titles
                .into_iter()
                .enumerate()
                .map(|(index, title)| {
                    let count = orders.borrow()[index].len();
                    let section = ArtistAlbumProjection::new(
                        shell,
                        title,
                        Rc::clone(&sparse),
                        start,
                        count,
                        Rc::clone(&layout),
                        Rc::clone(&columns),
                    );
                    start += count;
                    section
                })
                .collect::<Vec<_>>(),
        );
        let ArtistReleaseRoutePreamble {
            header,
            favorite,
            favorite_present,
            empty,
        } = preamble;
        header.set_margin_bottom(ARTIST_RELEASE_SECTION_GAP);
        let header_rows = static_artist_route_model(header);
        let favorite_search = favorite.as_ref().map(|(_, search)| search.clone());
        let favorite_widget = favorite.map(|(widget, _)| {
            widget.set_margin_bottom(ARTIST_RELEASE_SECTION_GAP);
            widget.set_visible(favorite_present);
            widget
        });
        let favorite_rows = favorite_widget
            .as_ref()
            .map_or_else(gio::ListStore::new::<glib::BoxedAnyObject>, |widget| {
                static_artist_route_model(widget.clone())
            });
        empty.set_visible(
            !favorite_present && sections.iter().all(|section| section.source_is_empty()),
        );
        let empty_rows = static_artist_route_model(empty.clone());
        let mut section_models = Vec::with_capacity(sections.len() + 3);
        section_models.push(header_rows);
        section_models.push(favorite_rows);
        section_models.extend(sections.iter().map(|section| section.rows()));
        section_models.push(empty_rows);
        let section_models = Rc::new(section_models);
        let model_sections = gio::ListStore::new::<gio::ListStore>();
        for model in section_models.iter() {
            model_sections.append(model);
        }
        let rows = gtk::FlattenListModel::new(Some(model_sections));
        let grid_fields = Rc::new(RefCell::new(settings.grid_fields.clone()));
        let (list, apply_grid_fields) = artist_route_list(shell, rows, Rc::clone(&grid_fields));
        list.set_margin_top(ROUTE_TOP_MARGIN);
        let resize_columns = Rc::clone(&columns);
        let resize_layout = Rc::clone(&layout);
        let resize_sections = Rc::clone(&sections);
        let scroller = detail_route_scroller(library_route_inset(list.clone().upcast()));
        let owner = width_allocation_owner(&scroller, move |width| {
            if resize_layout.get() == LibraryLayout::Row {
                return;
            }
            let next =
                collection_grid_column_count(width.saturating_sub(PRIMARY_ROUTE_HORIZONTAL_INSET));
            if resize_columns.replace(next) != next {
                for section in resize_sections.iter() {
                    section.refresh_body();
                }
            }
        });
        let mut search_targets = HashMap::new();
        if let Some(search) = favorite_search {
            search_targets.insert(
                ArtistRouteTarget::Favorite,
                virtual_search_target(&list, Rc::clone(&section_models), 1, search),
            );
        }
        for (index, section) in sections.iter().enumerate() {
            search_targets.insert(
                ArtistRouteTarget::Release(index),
                virtual_search_target(
                    &list,
                    Rc::clone(&section_models),
                    index + 2,
                    section.search(),
                ),
            );
        }
        Self {
            sections,
            orders,
            sparse,
            lane: Rc::new(super::named_detail::NamedOrderLane::new()),
            surface: owner.upcast(),
            layout,
            favorite: favorite_widget,
            favorite_present: Rc::new(Cell::new(favorite_present)),
            search_targets: Rc::new(search_targets),
            apply_grid_fields,
            grid_fields,
            empty,
        }
    }

    pub(super) fn widget(&self) -> gtk::Widget {
        self.surface.clone()
    }

    pub(super) fn section_search(&self, index: usize) -> Option<gtk::SearchEntry> {
        self.sections.get(index).map(|section| section.search())
    }

    pub(super) fn section_lane(
        &self,
        index: usize,
    ) -> Option<Rc<super::named_detail::NamedOrderLane>> {
        self.sections.get(index).map(|_| Rc::clone(&self.lane))
    }

    pub(super) fn lane(&self) -> Rc<super::named_detail::NamedOrderLane> {
        Rc::clone(&self.lane)
    }

    pub(super) fn replace_section_order(
        &self,
        index: usize,
        order: Vec<library::AlbumKey>,
        authoritative: bool,
    ) {
        if index >= self.sections.len() {
            return;
        }
        self.orders.borrow_mut()[index] = order;
        self.publish_orders(authoritative, None);
    }

    pub(super) fn replace_orders(
        &self,
        orders: [Vec<library::AlbumKey>; 6],
        first_rows: Vec<library::AlbumRow>,
        authoritative: bool,
    ) {
        self.orders.replace(orders);
        self.publish_orders(authoritative, Some(first_rows));
    }

    fn publish_orders(&self, authoritative: bool, first_rows: Option<Vec<library::AlbumRow>>) {
        let orders = self.orders.borrow();
        let flat = orders.iter().flatten().copied().collect::<Vec<_>>();
        if let Some(first_rows) = first_rows {
            if !self
                .sparse
                .replace_prepared(flat.clone(), first_rows, |row| row.album_key)
            {
                self.sparse.replace_order(flat);
            }
        } else {
            self.sparse.replace_order(flat);
        }
        let mut start = 0;
        for (section, order) in self.sections.iter().zip(orders.iter()) {
            section.set_range(start, order.len(), authoritative);
            start += order.len();
        }
        drop(orders);
        self.sync_empty();
    }

    pub(super) fn set_favorite_present(&self, present: bool) {
        self.favorite_present.set(present);
        if let Some(favorite) = self.favorite.as_ref() {
            favorite.set_visible(present);
        }
        self.sync_empty();
    }

    fn sync_empty(&self) {
        self.empty.set_visible(
            !self.favorite_present.get()
                && self
                    .sections
                    .iter()
                    .all(|section| section.source_is_empty()),
        );
    }

    pub(super) fn primary_search(&self) -> Option<ArtistRouteSearchTarget> {
        let target = if self.favorite_present.get() {
            Some(ArtistRouteTarget::Favorite)
        } else {
            self.sections
                .iter()
                .position(|section| !section.source_is_empty())
                .map(ArtistRouteTarget::Release)
        }?;
        self.search_targets.get(&target).cloned()
    }

    pub(super) fn apply_library_list_settings(&self, settings: &LibraryListSettings) {
        let next_layout = normalized_artist_layout(settings.layout);
        let previous_fields = self.grid_fields.borrow().clone();
        self.layout.set(next_layout);
        for section in self.sections.iter() {
            section.apply_settings(settings);
        }
        if previous_fields != settings.grid_fields {
            self.grid_fields.replace(settings.grid_fields.clone());
            (self.apply_grid_fields)(&settings.grid_fields);
        }
    }
}

fn normalized_artist_layout(layout: LibraryLayout) -> LibraryLayout {
    match layout {
        LibraryLayout::Row => LibraryLayout::Row,
        LibraryLayout::Grid | LibraryLayout::Detail => LibraryLayout::Grid,
    }
}

fn static_artist_route_model(widget: gtk::Widget) -> gio::ListStore {
    let model = gio::ListStore::new::<glib::BoxedAnyObject>();
    model.append(&glib::BoxedAnyObject::new(ArtistRouteRow::Static {
        widget,
    }));
    model
}

fn replace_artist_release_body(model: &gio::ListStore, rows: Vec<ArtistRouteRow>) {
    let additions = rows
        .into_iter()
        .map(glib::BoxedAnyObject::new)
        .collect::<Vec<_>>();
    replace_artist_release_objects(model, &additions);
}

fn replace_artist_release_objects(model: &gio::ListStore, additions: &[glib::BoxedAnyObject]) {
    model.splice(1, model.n_items().saturating_sub(1), additions);
}

fn artist_route_list(
    shell: &Rc<Shell>,
    model: gtk::FlattenListModel,
    fields: Rc<RefCell<Vec<LibraryField>>>,
) -> (gtk::ListView, Rc<dyn Fn(&[LibraryField])>) {
    let selection = gtk::NoSelection::new(Some(model));
    let factory = gtk::SignalListItemFactory::new();
    let cells = Rc::new(RefCell::new(HashMap::<usize, ArtistRouteListCell>::new()));
    let setup_cells = Rc::clone(&cells);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.set_hexpand(true);
        root.set_halign(gtk::Align::Fill);
        root.set_vexpand(false);
        root.set_valign(gtk::Align::Start);
        item.set_child(Some(&root));
        setup_cells.borrow_mut().insert(
            item.as_ptr() as usize,
            ArtistRouteListCell {
                root,
                grid_cells: Vec::new(),
                grid_columns: 0,
                grid_mode: false,
            },
        );
    });
    let bind_cells = Rc::clone(&cells);
    let bind_fields = Rc::clone(&fields);
    let bind_shell = Rc::clone(shell);
    factory.connect_bind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(row) = item
            .item()
            .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
            .map(|item| item.borrow::<ArtistRouteRow>().clone())
        else {
            return;
        };
        let mut states = bind_cells.borrow_mut();
        let Some(state) = states.get_mut(&(item.as_ptr() as usize)) else {
            return;
        };
        match row {
            ArtistRouteRow::Static { widget } => {
                clear_grid_state(state, true);
                state.root.set_orientation(gtk::Orientation::Vertical);
                state.root.set_homogeneous(false);
                state.root.set_margin_bottom(0);
                if widget.parent().as_ref() != Some(state.root.upcast_ref()) {
                    if let Some(parent) = widget.parent().and_downcast::<gtk::Box>() {
                        parent.remove(&widget);
                    }
                    state.root.append(&widget);
                }
            }
            ArtistRouteRow::AlbumGrid {
                section,
                start,
                len,
                columns,
                margin_bottom,
            } => {
                let Some(section) = section.upgrade() else {
                    return;
                };
                if !state.grid_mode || state.grid_columns != columns {
                    clear_grid_state(state, true);
                    state.root.set_orientation(gtk::Orientation::Horizontal);
                    state.root.set_homogeneous(true);
                    state.root.add_css_class("album-grid");
                    for _ in 0..columns {
                        let cell = AlbumGridCell::new(&bind_shell, &bind_fields.borrow(), None);
                        let widget = cell.widget();
                        let wrapper = cards::collection_grid_card_inset(
                            &widget,
                            COLLECTION_GRID_MIN_CARD_WIDTH,
                        );
                        state.root.append(&wrapper);
                        state.grid_cells.push(ArtistGridSlot { cell, widget });
                    }
                    state.grid_columns = columns;
                    state.grid_mode = true;
                }
                state.root.set_margin_bottom(margin_bottom);
                let model = section.sparse.list_model();
                for (offset, slot) in state.grid_cells.iter().enumerate() {
                    if offset >= len {
                        slot.cell.clear();
                        slot.widget.set_visible(false);
                        continue;
                    }
                    slot.widget.set_visible(true);
                    let position = start + offset;
                    if let Some(album) = item_at::<library::AlbumRow>(&model, position as u32) {
                        slot.cell.bind(position as u32, album);
                    } else {
                        slot.cell.clear();
                    }
                }
            }
        }
    });
    let unbind_cells = Rc::clone(&cells);
    factory.connect_unbind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(state) = unbind_cells.borrow_mut().get_mut(&(item.as_ptr() as usize)) {
            if state.grid_mode {
                clear_grid_state(state, false);
            } else {
                remove_box_children(&state.root);
            }
        }
    });
    let teardown_cells = Rc::clone(&cells);
    factory.connect_teardown(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(mut state) = teardown_cells
            .borrow_mut()
            .remove(&(item.as_ptr() as usize))
        {
            clear_grid_state(&mut state, true);
        }
        item.set_child(None::<&gtk::Widget>);
    });
    let list = gtk::ListView::new(Some(selection), Some(factory));
    list.add_css_class("artist-release-list");
    list.set_single_click_activate(false);
    list.set_hexpand(true);
    list.set_halign(gtk::Align::Fill);
    list.set_vexpand(true);
    let apply_cells = Rc::clone(&cells);
    let apply_fields = Rc::new(move |fields: &[LibraryField]| {
        for state in apply_cells.borrow().values() {
            for slot in &state.grid_cells {
                slot.cell.apply_fields(fields);
            }
        }
    }) as Rc<dyn Fn(&[LibraryField])>;
    (list, apply_fields)
}

fn clear_grid_state(state: &mut ArtistRouteListCell, remove: bool) {
    for slot in &state.grid_cells {
        slot.cell.clear();
        slot.widget.set_visible(false);
    }
    if remove {
        remove_box_children(&state.root);
        state.grid_cells.clear();
        state.grid_columns = 0;
        state.grid_mode = false;
        state.root.remove_css_class("album-grid");
    }
}

fn remove_box_children(root: &gtk::Box) {
    while let Some(child) = root.first_child() {
        root.remove(&child);
    }
}

fn non_propagating_width_scroller() -> gtk::ScrolledWindow {
    let clip = gtk::ScrolledWindow::new();
    clip.add_css_class("non-propagating-width-clip");
    configure_fill_width_clip(&clip, gtk::PolicyType::Never);
    clip.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Never);
    clip.set_propagate_natural_height(true);
    clip.set_hexpand(true);
    clip.set_halign(gtk::Align::Fill);
    clip
}

fn virtual_search_target(
    list: &gtk::ListView,
    models: Rc<Vec<gio::ListStore>>,
    model_index: usize,
    search: gtk::SearchEntry,
) -> ArtistRouteSearchTarget {
    let pending = Rc::new(Cell::new(false));
    let mapped_pending = Rc::clone(&pending);
    search.connect_map(move |search| {
        if mapped_pending.replace(false) {
            search.grab_focus();
        }
    });
    let weak_list = list.downgrade();
    let weak_search = search.downgrade();
    let focus = Rc::new(move || {
        let Some(search) = weak_search.upgrade() else {
            return;
        };
        if search.is_mapped() {
            search.grab_focus();
            return;
        }
        pending.set(true);
        let position = models
            .iter()
            .take(model_index)
            .map(gio::prelude::ListModelExt::n_items)
            .sum();
        if let Some(list) = weak_list.upgrade() {
            list.scroll_to(position, gtk::ListScrollFlags::FOCUS, None);
        }
    }) as Rc<dyn Fn()>;
    ArtistRouteSearchTarget { search, focus }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_rows_preserve_every_album_position_at_each_column_count() {
        for columns in 1..=6 {
            let count = 17_usize;
            let positions = (0..count.div_ceil(columns))
                .flat_map(|row| {
                    let start = row * columns;
                    start..(start + (count - start).min(columns))
                })
                .collect::<Vec<_>>();
            assert_eq!(positions, (0..count).collect::<Vec<_>>());
        }
    }

    #[test]
    fn section_body_updates_leave_header_item_in_place() {
        let rows = gio::ListStore::new::<glib::BoxedAnyObject>();
        rows.append(&glib::BoxedAnyObject::new(0_u8));
        let identity = rows.item(0).unwrap();
        replace_artist_release_objects(&rows, &[glib::BoxedAnyObject::new(1_u8)]);
        assert_eq!(rows.item(0).unwrap(), identity);
        assert_eq!(rows.n_items(), 2);
        replace_artist_release_objects(&rows, &[]);
        assert_eq!(rows.item(0).unwrap(), identity);
        assert_eq!(rows.n_items(), 1);
    }
}
