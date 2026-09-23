use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use adw::prelude::*;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use gtk::{gio, glib};

use crate::CatalogUi;
use crate::{LibraryField, LibraryLayout, LibraryListKey, LibraryListSettings};
use gtk_widgets::layout::width_allocation_owner;
use gtk_widgets::mounted_route::MountedRouteSearchTarget;

use super::cards;
use super::collections::library_route_inset;
use super::columns::AlbumTableCell;
use super::grid_cells::{AlbumGridCell, ReusableCollectionGridCell, collection_grid_column_count};
use super::route_shell::LibraryToolbarProjection;
use crate::route_layout::{
    PRIMARY_ROUTE_HORIZONTAL_INSET, ROUTE_TOP_MARGIN, detail_route_scroller,
};
use gtk_widgets::library_fields::COLLECTION_GRID_MIN_CARD_WIDTH;
use gtk_widgets::sparse_model::{SparseObjectItem, SparseRouteModel, connect_sparse_bind};

const ARTIST_RELEASE_SECTION_GAP: i32 = 18;
const ARTIST_RELEASE_HEADER_GAP: i32 = 10;

pub struct ArtistReleaseRoutePreamble {
    pub header: gtk::Widget,
    pub favorite: Option<(gtk::Widget, gtk::SearchEntry)>,
    pub favorite_present: bool,
    pub empty: gtk::Widget,
}

pub struct ArtistReleaseProjections {
    sections: Rc<Vec<Rc<ArtistAlbumProjection>>>,
    orders: RefCell<[Vec<library::AlbumKey>; 6]>,
    pub sparse: Rc<SparseRouteModel<library::AlbumKey, library::AlbumRow>>,
    lane: Rc<super::named_detail::NamedOrderLane>,
    surface: gtk::Widget,
    layout: Rc<Cell<LibraryLayout>>,
    favorite: Option<gtk::Widget>,
    favorite_present: Cell<bool>,
    search_targets: HashMap<ArtistRouteTarget, MountedRouteSearchTarget>,
    apply_grid_fields: Rc<dyn Fn(&[LibraryField])>,
    grid_fields: Rc<RefCell<Vec<LibraryField>>>,
    empty: gtk::Widget,
}

struct ArtistAlbumProjection {
    sparse: Rc<SparseRouteModel<library::AlbumKey, library::AlbumRow>>,
    start: Cell<usize>,
    count: Cell<usize>,
    source_present: Cell<bool>,
    search: gtk::SearchEntry,
    header: gtk::Widget,
    toolbar: LibraryToolbarProjection,
    rows: ArtistRowsModel,
    layout: Rc<Cell<LibraryLayout>>,
    columns: Rc<Cell<usize>>,
    applied_settings: RefCell<LibraryListSettings>,
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
    TableHeader {
        section: Weak<ArtistAlbumProjection>,
    },
    AlbumTable {
        section: Weak<ArtistAlbumProjection>,
        position: usize,
        last: bool,
    },
    AlbumGrid {
        section: Weak<ArtistAlbumProjection>,
        start: usize,
        len: usize,
        columns: usize,
        margin_bottom: i32,
    },
}

struct ArtistGridSlot {
    cell: AlbumGridCell,
    widget: gtk::Widget,
}

mod artist_rows_model_imp {
    use super::*;
    use gio::subclass::prelude::*;
    #[derive(Default)]
    pub(super) struct ArtistRowsModel {
        pub(super) owner: RefCell<Weak<ArtistAlbumProjection>>,
        pub(super) count: Cell<u32>,
        pub(super) items: RefCell<HashMap<u32, glib::WeakRef<SparseObjectItem>>>,
    }
    #[glib::object_subclass]
    impl ObjectSubclass for ArtistRowsModel {
        const NAME: &'static str = "RufinArtistRowsModel";
        type Type = super::ArtistRowsModel;
        type Interfaces = (gio::ListModel,);
    }
    impl ObjectImpl for ArtistRowsModel {}
    impl ListModelImpl for ArtistRowsModel {
        fn item_type(&self) -> glib::Type {
            SparseObjectItem::static_type()
        }
        fn n_items(&self) -> u32 {
            self.count.get()
        }
        #[expect(
            clippy::arc_with_non_send_sync,
            reason = "GTK presentation rows stay on the main thread"
        )]
        fn item(&self, position: u32) -> Option<glib::Object> {
            if position >= self.count.get() {
                return None;
            }
            let mut items = self.items.borrow_mut();
            items.retain(|_, item| item.upgrade().is_some());
            if let Some(item) = items.get(&position).and_then(glib::WeakRef::upgrade) {
                return Some(item.upcast());
            }
            let item = SparseObjectItem::new(
                Arc::new(self.owner.borrow().upgrade()?.row_at(position)),
                true,
            );
            items.insert(position, item.downgrade());
            Some(item.upcast())
        }
    }
}

glib::wrapper! {
    struct ArtistRowsModel(ObjectSubclass<artist_rows_model_imp::ArtistRowsModel>) @implements gio::ListModel;
}

impl ArtistRowsModel {
    fn refresh(&self, count: u32) {
        let old = self.imp().count.replace(count);
        if old != count {
            self.imp()
                .items
                .borrow_mut()
                .retain(|position, _| *position == 0);
            let start = u32::from(old != 0 && count != 0);
            self.items_changed(start, old - start, count - start);
        } else {
            self.refresh_items(|_| true);
        }
    }
    #[expect(
        clippy::arc_with_non_send_sync,
        reason = "GTK presentation rows stay on the main thread"
    )]
    fn refresh_items(&self, changed: impl Fn(&ArtistRouteRow) -> bool) {
        let Some(owner) = self.imp().owner.borrow().upgrade() else {
            return;
        };
        let items = self
            .imp()
            .items
            .borrow()
            .iter()
            .filter_map(|(position, item)| Some((*position, item.upgrade()?)))
            .collect::<Vec<_>>();
        for (position, item) in items {
            let row = owner.row_at(position);
            if changed(&row) {
                item.replace(Arc::new(row), true);
            }
        }
    }
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum ArtistCellKind {
    #[default]
    Static,
    Grid,
    Table,
    Header,
}

mod artist_route_list_cell_imp {
    use super::{ArtistCellKind, ArtistGridSlot, LibraryField};
    use std::cell::{Cell, RefCell};

    use gtk::glib;
    use gtk::subclass::prelude::*;

    #[derive(Default)]
    pub(super) struct ArtistRouteListCell {
        pub(super) grid_cells: RefCell<Vec<ArtistGridSlot>>,
        pub(super) grid_columns: Cell<usize>,
        pub(super) kind: Cell<ArtistCellKind>,
        pub(super) table_fields: RefCell<Vec<LibraryField>>,
        pub(super) table_cells: RefCell<Vec<gtk::Widget>>,
        pub(super) bound_items: RefCell<Vec<gtk_widgets::sparse_model::SparseObjectItem>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ArtistRouteListCell {
        const NAME: &'static str = "RufinArtistRouteListCell";
        type Type = super::ArtistRouteListCell;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for ArtistRouteListCell {}
    impl WidgetImpl for ArtistRouteListCell {}
    impl BoxImpl for ArtistRouteListCell {}
}

glib::wrapper! {
    struct ArtistRouteListCell(ObjectSubclass<artist_route_list_cell_imp::ArtistRouteListCell>)
        @extends gtk::Widget, gtk::Box,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

mod artist_table_layout_imp {
    use super::*;
    use gtk::subclass::prelude::*;

    #[derive(Default)]
    pub(super) struct ArtistTableLayout;

    #[glib::object_subclass]
    impl ObjectSubclass for ArtistTableLayout {
        const NAME: &'static str = "RufinArtistTableLayout";
        type Type = super::ArtistTableLayout;
        type ParentType = gtk::LayoutManager;
    }

    impl ObjectImpl for ArtistTableLayout {}

    impl LayoutManagerImpl for ArtistTableLayout {
        fn request_mode(&self, _: &gtk::Widget) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(
            &self,
            widget: &gtk::Widget,
            orientation: gtk::Orientation,
            for_size: i32,
        ) -> (i32, i32, i32, i32) {
            let row = widget.downcast_ref::<ArtistRouteListCell>().unwrap();
            if orientation == gtk::Orientation::Horizontal {
                let minimum = row
                    .imp()
                    .table_cells
                    .borrow()
                    .iter()
                    .map(|cell| cell.measure(orientation, -1).0)
                    .sum();
                let natural = row.column_widths(-1).iter().sum::<i32>().max(minimum);
                return (minimum, natural, -1, -1);
            }
            let widths = row.column_widths(for_size);
            let mut minimum = 0;
            let mut natural = 0;
            for (cell, width) in row.imp().table_cells.borrow().iter().zip(widths) {
                let width = width.max(cell.measure(gtk::Orientation::Horizontal, -1).0);
                let (min, nat, _, _) = cell.measure(orientation, width);
                minimum = minimum.max(min);
                natural = natural.max(nat);
            }
            (minimum, natural, -1, -1)
        }

        fn allocate(&self, widget: &gtk::Widget, width: i32, height: i32, baseline: i32) {
            let row = widget.downcast_ref::<ArtistRouteListCell>().unwrap();
            let widths = row.column_widths(width);
            let rtl = widget.direction() == gtk::TextDirection::Rtl;
            let mut x = 0;
            for (cell, cell_width) in row.imp().table_cells.borrow().iter().zip(widths) {
                let position = if rtl { width - x - cell_width } else { x };
                cell.allocate(
                    cell_width,
                    height,
                    baseline,
                    Some(
                        gtk::gsk::Transform::new()
                            .translate(&gtk::graphene::Point::new(position as f32, 0.0)),
                    ),
                );
                x += cell_width;
            }
        }
    }
}

glib::wrapper! {
    struct ArtistTableLayout(ObjectSubclass<artist_table_layout_imp::ArtistTableLayout>)
        @extends gtk::LayoutManager;
}

impl ArtistRouteListCell {
    fn new() -> Self {
        glib::Object::builder()
            .property("orientation", gtk::Orientation::Horizontal)
            .property("hexpand", true)
            .property("halign", gtk::Align::Fill)
            .property("vexpand", false)
            .property("valign", gtk::Align::Start)
            .build()
    }

    fn clear_content(&self, remove: bool) {
        let imp = self.imp();
        for slot in imp.grid_cells.borrow().iter() {
            slot.cell.clear();
            slot.widget.set_visible(false);
        }
        imp.bound_items.borrow_mut().clear();
        for widget in imp.table_cells.borrow().iter() {
            if let Some(cell) = widget.downcast_ref::<AlbumTableCell>() {
                cell.bind(0, None);
            }
        }
        if remove {
            if matches!(
                imp.kind.get(),
                ArtistCellKind::Table | ArtistCellKind::Header
            ) {
                self.set_layout_manager(Some(gtk::BoxLayout::new(gtk::Orientation::Horizontal)));
            }
            remove_box_children(self);
            imp.table_cells.borrow_mut().clear();
            imp.table_fields.borrow_mut().clear();
            imp.grid_cells.borrow_mut().clear();
            imp.grid_columns.set(0);
            imp.kind.set(ArtistCellKind::Static);
            self.remove_css_class("album-grid");
            self.remove_css_class("artist-album-table-row");
            self.remove_css_class("artist-album-table-header");
        }
    }

    fn table(&self, shell: &Rc<CatalogUi>, fields: &[LibraryField], header: bool) {
        let kind = if header {
            ArtistCellKind::Header
        } else {
            ArtistCellKind::Table
        };
        let imp = self.imp();
        if imp.kind.get() == kind && imp.table_fields.borrow().as_slice() == fields {
            return;
        }
        self.clear_content(true);
        self.set_orientation(gtk::Orientation::Horizontal);
        self.set_homogeneous(false);
        self.add_css_class(if header {
            "artist-album-table-header"
        } else {
            "artist-album-table-row"
        });
        for field in fields {
            let widget = if header {
                let label = album_table_title(*field).map_or_else(
                    || gtk::Label::new(None),
                    gtk_widgets::localization::localized_label,
                );
                label.add_css_class("table-header");
                label.set_xalign(0.0);
                label.set_ellipsize(gtk::pango::EllipsizeMode::End);
                label.upcast::<gtk::Widget>()
            } else {
                AlbumTableCell::new(shell, *field, None).upcast()
            };
            self.append(&widget);
            imp.table_cells.borrow_mut().push(widget);
        }
        imp.table_fields.replace(fields.to_vec());
        imp.kind.set(kind);
        self.set_layout_manager(Some(glib::Object::new::<ArtistTableLayout>()));
    }

    fn column_widths(&self, width: i32) -> Vec<i32> {
        let imp = self.imp();
        let fields = imp.table_fields.borrow();
        let preferred = fields
            .iter()
            .map(|field| {
                super::columns::column_fit_width(
                    *field,
                    gtk_widgets::library_fields::column_width(*field),
                )
            })
            .collect::<Vec<_>>();
        if width < 0 {
            return preferred;
        }
        let minimum = fields
            .iter()
            .zip(&preferred)
            .map(|(field, preferred)| {
                let title = album_table_title(*field).map(localization::tr);
                gtk_widgets::table_sizing::column_header_minimum_width(title.as_deref(), *preferred)
            })
            .collect::<Vec<_>>();
        gtk_widgets::table_sizing::fitted_column_widths_with_minimums(
            &preferred,
            &minimum,
            width - gtk_widgets::table_sizing::FITTED_TABLE_WIDTH_PADDING,
        )
    }

    fn apply_fields(&self, fields: &[LibraryField]) {
        for slot in self.imp().grid_cells.borrow().iter() {
            slot.cell.apply_fields(fields);
        }
    }
}

fn album_table_title(field: LibraryField) -> Option<&'static str> {
    match field {
        LibraryField::Tools => None,
        LibraryField::RowIndex => Some(super::columns::ROW_INDEX_COLUMN_TITLE),
        LibraryField::TitleMerged => Some("Title"),
        _ => Some(gtk_widgets::settings::library_field_title(field)),
    }
}

impl ArtistAlbumProjection {
    fn new(
        shell: &Rc<CatalogUi>,
        title: &'static str,
        sparse: Rc<SparseRouteModel<library::AlbumKey, library::AlbumRow>>,
        start: usize,
        count: usize,
        layout: Rc<Cell<LibraryLayout>>,
        columns: Rc<Cell<usize>>,
    ) -> Rc<Self> {
        let search = gtk::SearchEntry::new();
        gtk_widgets::controls::configure_search_entry(&search);
        search.set_placeholder_text(Some(&localization::tr("Search")));
        search.set_hexpand(true);
        let toolbar =
            shell.library_toolbar_projection(LibraryListKey::ArtistAlbums, search.clone());
        let header = gtk::Box::new(gtk::Orientation::Vertical, ARTIST_RELEASE_HEADER_GAP);
        header.set_hexpand(true);
        header.set_halign(gtk::Align::Fill);
        let heading = gtk_widgets::localization::localized_label(title);
        heading.add_css_class("section-heading");
        heading.set_xalign(0.0);
        header.append(&heading);
        header.append(&toolbar.widget());
        let header: gtk::Widget = header.upcast();
        let rows: ArtistRowsModel = glib::Object::new();

        let settings = shell
            .settings
            .current
            .borrow()
            .library_list(LibraryListKey::ArtistAlbums);
        let source_present = count != 0;
        let projection = Rc::new(Self {
            sparse,
            start: Cell::new(start),
            count: Cell::new(count),
            source_present: Cell::new(source_present),
            search,
            header,
            toolbar,
            rows,
            layout,
            columns,
            applied_settings: RefCell::new(settings),
        });
        projection
            .rows
            .imp()
            .owner
            .replace(Rc::downgrade(&projection));
        projection.refresh_body();
        projection
    }

    fn source_is_empty(&self) -> bool {
        !self.source_present.get()
    }

    fn search(&self) -> gtk::SearchEntry {
        self.search.clone()
    }

    fn rows(&self) -> gio::ListModel {
        self.rows.clone().upcast()
    }

    fn set_range(self: &Rc<Self>, start: usize, count: usize, authoritative: bool) {
        if authoritative {
            self.source_present.set(count != 0);
        }
        self.start.set(start);
        self.count.set(count);
        self.refresh_body();
    }

    fn refresh_body(self: &Rc<Self>) {
        self.header.set_visible(!self.source_is_empty());
        self.header.set_margin_bottom(if self.source_is_empty() {
            0
        } else if self.count.get() == 0 {
            ARTIST_RELEASE_SECTION_GAP
        } else {
            ARTIST_RELEASE_HEADER_GAP
        });
        let count = if self.source_is_empty() {
            1
        } else if self.layout.get() == LibraryLayout::Row {
            2 + self.count.get()
        } else {
            1 + self.count.get().div_ceil(self.columns.get().max(1))
        };
        self.rows.refresh(count as u32);
    }

    fn row_at(self: &Rc<Self>, position: u32) -> ArtistRouteRow {
        if position == 0 {
            return ArtistRouteRow::Static {
                widget: self.header.clone(),
            };
        }
        let local = position as usize - 1;
        if self.layout.get() == LibraryLayout::Row {
            if local == 0 {
                return ArtistRouteRow::TableHeader {
                    section: Rc::downgrade(self),
                };
            }
            return ArtistRouteRow::AlbumTable {
                section: Rc::downgrade(self),
                position: self.start.get() + local - 1,
                last: local == self.count.get(),
            };
        }
        let columns = self.columns.get().max(1);
        let start = local * columns;
        ArtistRouteRow::AlbumGrid {
            section: Rc::downgrade(self),
            start: self.start.get() + start,
            len: (self.count.get() - start).min(columns),
            columns,
            margin_bottom: if start + columns >= self.count.get() {
                ARTIST_RELEASE_SECTION_GAP
            } else {
                0
            },
        }
    }

    fn refresh_grid_range(self: &Rc<Self>, position: u32, count: u32) {
        let end = position.saturating_add(count);
        self.rows.refresh_items(|row| match row {
            ArtistRouteRow::AlbumGrid { start, len, .. } => {
                *start < end as usize && start + len > position as usize
            }
            ArtistRouteRow::AlbumTable { position: at, .. } => {
                (*at as u32) >= position && (*at as u32) < end
            }
            _ => false,
        });
    }

    fn apply_settings(self: &Rc<Self>, settings: &LibraryListSettings) {
        if *self.applied_settings.borrow() == *settings {
            return;
        }
        self.toolbar.apply(LibraryListKey::ArtistAlbums, settings);
        self.applied_settings.replace(settings.clone());
        self.refresh_body();
    }
}

impl ArtistReleaseProjections {
    pub fn new(
        shell: &Rc<CatalogUi>,
        source: library::SourceKey,
        preamble: ArtistReleaseRoutePreamble,
        titles: [&'static str; 6],
        orders: [Vec<library::AlbumKey>; 6],
        first_row_position: usize,
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
        let database = Arc::clone(&shell.library);
        let folder = None;
        let load = Rc::new(
            move |keys: Vec<library::AlbumKey>,
                  _: std::ops::Range<usize>,
                  cancellation: library::ReadCancellation| {
                let database = Arc::clone(&database);
                Box::pin(async move {
                    database
                        .album_rows(source, &keys, folder, &cancellation)
                        .await
                        .map_err(|error| error.to_string())
                }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
            },
        );
        let sparse = SparseRouteModel::new(flat_order, 32, shell.runtime.clone(), load);
        sparse.seed_matching_at(first_row_position, first_rows, |row| row.album_key);
        let orders = RefCell::new(orders);
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
        let ready_sections = Rc::downgrade(&sections);
        sparse.connect_ready_changed(move |position, count| {
            if let Some(sections) = ready_sections.upgrade() {
                for section in sections.iter() {
                    section.refresh_grid_range(position, count);
                }
            }
        });
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
            .map_or_else(gio::ListStore::new::<SparseObjectItem>, |widget| {
                static_artist_route_model(widget.clone())
            });
        empty.set_visible(
            !favorite_present && sections.iter().all(|section| section.source_is_empty()),
        );
        let empty_rows = static_artist_route_model(empty.clone());
        let mut section_models = Vec::with_capacity(sections.len() + 3);
        section_models.push(header_rows.upcast::<gio::ListModel>());
        section_models.push(favorite_rows.upcast::<gio::ListModel>());
        section_models.extend(sections.iter().map(|section| section.rows()));
        section_models.push(empty_rows.upcast::<gio::ListModel>());
        let section_models = Rc::new(section_models);
        let model_sections = gio::ListStore::new::<gio::ListModel>();
        for model in section_models.iter() {
            model_sections.append(model);
        }
        let rows = gtk::FlattenListModel::new(Some(model_sections));
        let grid_fields = Rc::new(RefCell::new(settings.grid_fields.clone()));
        let (list, apply_grid_fields) = artist_route_list(shell, rows, Rc::clone(&grid_fields));
        let selection = list.model().expect("Artist selection").downgrade();
        sparse
            .list_model()
            .connect_items_changed(move |_, _, _, _| {
                if let Some(selection) = selection.upgrade() {
                    selection.unselect_all();
                }
            });
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
            favorite_present: Cell::new(favorite_present),
            search_targets,
            apply_grid_fields,
            grid_fields,
            empty,
        }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.surface.clone()
    }

    pub fn section_search(&self, index: usize) -> Option<gtk::SearchEntry> {
        self.sections.get(index).map(|section| section.search())
    }

    pub fn lane(&self) -> Rc<super::named_detail::NamedOrderLane> {
        Rc::clone(&self.lane)
    }

    pub fn resume_initial_demand(&self) {
        self.sparse.resume_initial_demand();
    }

    pub fn replace_section_order(
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

    pub fn replace_orders(
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

    pub fn set_favorite_present(&self, present: bool) {
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

    pub fn primary_search(&self) -> Option<MountedRouteSearchTarget> {
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

    pub fn layout_cycle(&self) -> gtk_widgets::mounted_route::MountedRouteCommand {
        self.sections
            .first()
            .expect("Artist releases have a section")
            .toolbar
            .layout_cycle()
    }

    pub fn apply_library_list_settings(&self, settings: &LibraryListSettings) -> bool {
        let query_changed = self.sections.first().is_some_and(|section| {
            let previous = section.applied_settings.borrow();
            previous.sort_key != settings.sort_key || previous.descending != settings.descending
        });
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
        query_changed
    }
}

fn normalized_artist_layout(layout: LibraryLayout) -> LibraryLayout {
    match layout {
        LibraryLayout::Row => LibraryLayout::Row,
        LibraryLayout::Grid | LibraryLayout::Detail => LibraryLayout::Grid,
    }
}

#[expect(
    clippy::arc_with_non_send_sync,
    reason = "GTK-only presentation rows reuse the existing SparseItem notification owner"
)]
fn static_artist_route_model(widget: gtk::Widget) -> gio::ListStore {
    let model = gio::ListStore::new::<SparseObjectItem>();
    model.append(&SparseObjectItem::new(
        Arc::new(ArtistRouteRow::Static { widget }),
        true,
    ));
    model
}

fn artist_route_list(
    shell: &Rc<CatalogUi>,
    model: gtk::FlattenListModel,
    fields: Rc<RefCell<Vec<LibraryField>>>,
) -> (gtk::ListView, Rc<dyn Fn(&[LibraryField])>) {
    let activate_model = model.clone();
    let selection_models = model.model().expect("Artist section models");
    let selection = gtk_widgets::selection::PositionSelectionModel::new(model);
    selection.set_collection_selection(Box::new(move |target, positions| {
        let rufin_core::playback::PlaybackTarget::Album(uri) = target else {
            return None;
        };
        let selected = gtk::Bitset::new_empty();
        let mut sparse = None;
        let mut offset = 0;
        for index in 0..selection_models.n_items() {
            let model = selection_models
                .item(index)?
                .downcast::<gio::ListModel>()
                .ok()?;
            if let Some(rows) = model.downcast_ref::<ArtistRowsModel>()
                && let Some(section) = rows.imp().owner.borrow().upgrade()
                && section.layout.get() == LibraryLayout::Row
                && !section.source_is_empty()
            {
                let section_positions = positions.copy();
                section_positions.intersect(&gtk::Bitset::new_range(
                    offset + 2,
                    section.count.get() as u32,
                ));
                section_positions.shift_left(offset + 2);
                section_positions.shift_right(section.start.get() as u32);
                selected.union(&section_positions);
                sparse = Some(Rc::clone(&section.sparse));
            }
            offset += model.n_items();
        }
        let sparse = sparse?;
        let dragged = sparse.ready_position(|row| row.media_uri == *uri)?;
        if selected.size() < 2 || !selected.contains(dragged) {
            return None;
        }
        Some(gtk_widgets::media_drag::CollectionTargets::Ready(
            gtk_widgets::selection::selected_values(
                sparse.order().keys().expect("artist release keys"),
                &selected,
            )
            .into_iter()
            .map(rufin_core::playback::PlaybackTarget::AlbumKey)
            .collect(),
        ))
    }));
    let factory = gtk::SignalListItemFactory::new();
    let cells = Rc::new(RefCell::new(
        Vec::<glib::WeakRef<ArtistRouteListCell>>::new(),
    ));
    let setup_cells = Rc::clone(&cells);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let root = ArtistRouteListCell::new();
        item.set_activatable(false);
        item.set_selectable(false);
        item.set_child(Some(&root));
        setup_cells.borrow_mut().push(root.downgrade());
    });
    let bind_fields = Rc::clone(&fields);
    let bind_shell = Rc::clone(shell);
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(row) = gtk_widgets::sparse_model::item_at_from_item::<ArtistRouteRow>(item) else {
            return;
        };
        let Some(state) = item.child().and_downcast::<ArtistRouteListCell>() else {
            return;
        };
        state.imp().bound_items.borrow_mut().clear();
        let table = matches!(row, ArtistRouteRow::AlbumTable { .. });
        item.set_selectable(table);
        item.set_activatable(table);
        match row {
            ArtistRouteRow::TableHeader { section } => {
                let Some(section) = section.upgrade() else {
                    return;
                };
                state.table(
                    &bind_shell,
                    &section.applied_settings.borrow().row_fields,
                    true,
                );
                state.set_margin_bottom(0);
            }
            ArtistRouteRow::AlbumTable {
                section,
                position,
                last,
            } => {
                let Some(section) = section.upgrade() else {
                    return;
                };
                state.table(
                    &bind_shell,
                    &section.applied_settings.borrow().row_fields,
                    false,
                );
                state.set_margin_bottom(if last { ARTIST_RELEASE_SECTION_GAP } else { 0 });
                let value = section
                    .sparse
                    .list_model()
                    .item(position as u32)
                    .and_downcast::<SparseObjectItem>();
                let row = value
                    .as_ref()
                    .and_then(|item| item.value::<Arc<library::AlbumRow>>());
                for cell in state.imp().table_cells.borrow().iter() {
                    cell.downcast_ref::<AlbumTableCell>()
                        .unwrap()
                        .bind((position - section.start.get()) as u32, row.clone());
                }
                state.imp().bound_items.borrow_mut().extend(value);
            }
            ArtistRouteRow::Static { widget } => {
                state.clear_content(true);
                state.set_orientation(gtk::Orientation::Vertical);
                state.set_homogeneous(false);
                state.set_margin_bottom(0);
                if widget.parent().as_ref() != Some(state.upcast_ref()) {
                    if let Some(parent) = widget.parent().and_downcast::<gtk::Box>() {
                        parent.remove(&widget);
                    }
                    state.append(&widget);
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
                let imp = state.imp();
                if imp.kind.get() != ArtistCellKind::Grid || imp.grid_columns.get() != columns {
                    state.clear_content(true);
                    state.set_orientation(gtk::Orientation::Horizontal);
                    state.set_homogeneous(true);
                    state.add_css_class("album-grid");
                    for _ in 0..columns {
                        let cell = AlbumGridCell::new(&bind_shell, &bind_fields.borrow(), None);
                        cell.enable_card_activation();
                        let widget = cell.widget();
                        let wrapper = cards::collection_grid_card_inset(
                            &widget,
                            COLLECTION_GRID_MIN_CARD_WIDTH,
                        );
                        state.append(&wrapper);
                        imp.grid_cells
                            .borrow_mut()
                            .push(ArtistGridSlot { cell, widget });
                    }
                    imp.grid_columns.set(columns);
                    imp.kind.set(ArtistCellKind::Grid);
                }
                state.set_margin_bottom(margin_bottom);
                let model = section.sparse.list_model();
                for (offset, slot) in imp.grid_cells.borrow().iter().enumerate() {
                    if offset >= len {
                        slot.cell.clear();
                        slot.widget.set_visible(false);
                        continue;
                    }
                    slot.widget.set_visible(true);
                    let position = start + offset;
                    let value = model
                        .item(position as u32)
                        .and_downcast::<SparseObjectItem>();
                    if let Some(album) = value
                        .as_ref()
                        .and_then(|item| item.value::<Arc<library::AlbumRow>>())
                    {
                        slot.cell.bind(position as u32, (*album).clone());
                    } else {
                        slot.cell.clear();
                    }
                    imp.bound_items.borrow_mut().extend(value);
                }
            }
        }
    });
    factory.connect_unbind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(state) = item.child().and_downcast::<ArtistRouteListCell>() {
            state.clear_content(state.imp().kind.get() == ArtistCellKind::Static);
        }
    });
    factory.connect_teardown(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(state) = item.child().and_downcast::<ArtistRouteListCell>() {
            state.clear_content(true);
        }
        item.set_child(None::<&gtk::Widget>);
    });
    let list = gtk::ListView::new(Some(selection), Some(factory));
    list.add_css_class("artist-release-list");
    list.set_single_click_activate(false);
    let activate_shell = Rc::downgrade(shell);
    list.connect_activate(move |_, position| {
        let Some(shell) = activate_shell.upgrade() else {
            return;
        };
        let Some(ArtistRouteRow::AlbumTable {
            section, position, ..
        }) = gtk_widgets::sparse_model::item_at::<ArtistRouteRow>(&activate_model, position)
        else {
            return;
        };
        if let Some(row) = section
            .upgrade()
            .and_then(|section| section.sparse.peek_ready(position))
        {
            shell.navigate(gtk_widgets::route::Route::AlbumDetail(
                row.media_uri.clone(),
            ));
        }
    });
    list.set_hexpand(true);
    list.set_halign(gtk::Align::Fill);
    list.set_vexpand(true);
    let apply_cells = Rc::clone(&cells);
    let apply_fields = Rc::new(move |fields: &[LibraryField]| {
        apply_cells.borrow_mut().retain(|cell| {
            cell.upgrade().is_some_and(|cell| {
                cell.apply_fields(fields);
                true
            })
        });
    }) as Rc<dyn Fn(&[LibraryField])>;
    (list, apply_fields)
}

fn remove_box_children(root: &impl IsA<gtk::Box>) {
    let root = root.as_ref();
    while let Some(child) = root.first_child() {
        root.remove(&child);
    }
}

fn virtual_search_target(
    list: &gtk::ListView,
    models: Rc<Vec<gio::ListModel>>,
    model_index: usize,
    search: gtk::SearchEntry,
) -> MountedRouteSearchTarget {
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
    MountedRouteSearchTarget {
        search,
        focus: Some(focus),
    }
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
        let rows: ArtistRowsModel = glib::Object::new();
        let header = SparseObjectItem::new(0_u8, true);
        rows.imp().count.set(1);
        rows.imp().items.borrow_mut().insert(0, header.downgrade());
        let identity = rows.item(0).unwrap();
        rows.refresh(2);
        assert_eq!(rows.item(0).unwrap(), identity);
        assert_eq!(rows.n_items(), 2);
        rows.refresh(1);
        assert_eq!(rows.item(0).unwrap(), identity);
        assert_eq!(rows.n_items(), 1);
    }
}
