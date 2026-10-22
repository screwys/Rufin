use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use adw::prelude::*;
use glib::subclass::prelude::*;
use gtk::{gio, glib};
use library::{AlbumKey, AlbumView, TrackView};
use localization::track_count_text;
use rufin_core::playback::QueuePlacement;

use crate::CatalogUi;
use crate::{LibraryField, LibraryListKey};
use gtk_widgets::artwork::ArtworkTile;
use gtk_widgets::favorites::{
    favorite_button_is_active, row_favorite_icon_button, set_favorite_button_active,
};
use gtk_widgets::interactions::install_context_menu_openers;

use super::collection_context::present_track_context_menu;
use gtk_widgets::library_fields::{opaque_artwork, track_field};
use gtk_widgets::mounted_route::{ItemScrollPosition, NativeListScroll};
use gtk_widgets::sparse_model::{
    SparseObjectItem, SparseObjectModel, SparseRouteModel, SparseSource, connect_sparse_bind,
};

const ALBUM_TRACK_HEIGHT: i32 = 36;

fn display_track_height(display: &rufin_core::settings::layout::DisplaySettings) -> i32 {
    use rufin_core::settings::layout::DisplaySize;
    match display.size {
        DisplaySize::Compact => 32,
        DisplaySize::Default => ALBUM_TRACK_HEIGHT,
        DisplaySize::Large => 48,
    }
}

fn detail_display(shell: &CatalogUi) -> rufin_core::settings::layout::DisplaySettings {
    shell
        .settings
        .current
        .borrow()
        .library_list(LibraryListKey::Albums)
        .display
}
const ALBUM_DETAIL_TRACK_COLUMN_GAP: i32 = 8;
const ALBUM_DETAIL_ROW_HORIZONTAL_INSET: i32 = 24;
const ALBUM_DETAIL_MAX_COVER: i32 = 240;
const ALBUM_DETAIL_MIN_COVER: i32 = 102;
const ALBUM_DETAIL_NARROW_WIDTH: i32 = 360;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AlbumDetailRowMetrics {
    cover_size: i32,
    meta_width: i32,
    spacing: i32,
}

use library::AlbumDetailRow;

#[derive(Clone)]
pub enum AlbumCollectionModels {
    Rows(Rc<SparseRouteModel<AlbumKey, AlbumView>>),
    Detail(AlbumDetailModel),
}
#[derive(Clone)]
pub struct AlbumDetailModel {
    inner: Rc<AlbumDetailModelInner>,
}
struct AlbumDetailModelInner {
    native: DetailRowsModel,
    rows: Rc<SparseRouteModel<usize, AlbumDetailRow>>,
    initial_position: Cell<usize>,
    folder: Option<library::FolderKey>,
}
impl AlbumCollectionModels {
    pub fn new(
        database: &Arc<library::Database>,
        runtime: &tokio::runtime::Handle,
        content: rufin_core::browse::AlbumListContent,
        request: Rc<RefCell<library::ListRead>>,
    ) -> Self {
        let page = match content {
            rufin_core::browse::AlbumListContent::Detail { index, page } => {
                let native = DetailRowsModel::new(Arc::new(DetailLayout::new(index, [0, 0])));
                let reader = native.clone();
                let database = database.clone();
                let rows = SparseRouteModel::new(
                    SparseSource::Query { count: page.count },
                    32,
                    runtime.clone(),
                    Rc::new(move |_, range, cancel| {
                        let index = reader.layout().index.clone();
                        let database = database.clone();
                        Box::pin(async move {
                            index
                                .rows(&database, range.start, range.len(), &cancel)
                                .await
                                .map_err(|error| error.to_string())
                        })
                    }),
                );
                rows.seed_matching_at(page.first_row_position, page.rows, |_| 0);
                native
                    .imp()
                    .source
                    .set(rows.list_model())
                    .expect("Detail source");
                return Self::Detail(AlbumDetailModel {
                    inner: Rc::new(AlbumDetailModelInner {
                        native,
                        rows,
                        initial_position: Cell::new(page.first_row_position),
                        folder: request.borrow().folder,
                    }),
                });
            }

            rufin_core::browse::AlbumListContent::Rows(page) => page,
        };
        let library::Page {
            count,
            first_row_position,
            rows: first_rows,
        } = page;
        let read_request = request.clone();
        let reader = database.clone();
        let rows = SparseRouteModel::new(
            SparseSource::Query { count },
            32,
            runtime.clone(),
            Rc::new(move |_, range, cancel| {
                let read = read_request.borrow().clone();
                let database = reader.clone();
                Box::pin(async move {
                    read.albums(&database, range.start, range.len(), &cancel)
                        .await
                        .map_err(|error| error.to_string())
                })
            }),
        );
        gtk_widgets::media_drag::install_parent_selection(&rows, request, |row| {
            Some(gtk_widgets::selection::collection_play_request(
                library::Collection::Album(row.album_key),
            ))
        });
        rows.seed_matching_at(first_row_position, first_rows, |row| row.album_key);
        Self::Rows(rows)
    }
    pub fn is_detail(&self) -> bool {
        matches!(self, Self::Detail(_))
    }
    pub fn albums(&self) -> SparseObjectModel {
        self.album_rows().expect("Album rows").list_model()
    }
    pub fn album_rows(&self) -> Option<Rc<SparseRouteModel<AlbumKey, AlbumView>>> {
        if let Self::Rows(rows) = self {
            Some(rows.clone())
        } else {
            None
        }
    }
    pub fn detail(&self) -> AlbumDetailModel {
        let Self::Detail(detail) = self else {
            unreachable!("Detail layout")
        };
        detail.clone()
    }
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Rows(rows) => rows.len() == 0,
            Self::Detail(detail) => detail.inner.native.layout().index.is_empty(),
        }
    }
    pub fn resume_initial_demand(&self) {
        match self {
            Self::Rows(rows) => rows.resume_initial_demand(),
            Self::Detail(detail) => detail.inner.rows.resume_initial_demand(),
        }
    }
    pub fn replace_prepared(&self, content: rufin_core::browse::AlbumListContent) -> bool {
        match (self, content) {
            (Self::Rows(rows), rufin_core::browse::AlbumListContent::Rows(page)) => rows
                .replace_prepared_at(
                    SparseSource::Query { count: page.count },
                    page.first_row_position,
                    page.rows,
                    Vec::new(),
                    |row| row.album_key,
                ),
            (
                Self::Detail(detail),
                rufin_core::browse::AlbumListContent::Detail { index, page },
            ) => {
                let inner = &detail.inner;
                let previous = inner.native.layout();
                let layout = Arc::new(DetailLayout::new(index, previous.minimum));
                inner.initial_position.set(page.first_row_position);
                inner.native.imp().layout.replace(Some(layout));
                let replaced = inner.rows.replace_prepared_at(
                    SparseSource::Query { count: page.count },
                    page.first_row_position,
                    page.rows,
                    Vec::new(),
                    |_| 0,
                );
                inner.native.changed(previous.count);
                replaced
            }
            _ => false,
        }
    }
    pub fn update_favorite(&self, uri: &str, favorite: bool) -> bool {
        match self {
            Self::Rows(rows) => {
                let Some(position) = rows.ready_position(|row| row.media_uri == uri) else {
                    return false;
                };
                rows.update_ready(position as usize, |row| row.state.favorite = favorite)
            }
            Self::Detail(detail) => {
                detail.inner.rows.update_matching(
                    |row| matches!(row, AlbumDetailRow::Album { album, .. } if album.media_uri == uri),
                    |row| {
                        if let AlbumDetailRow::Album { album, .. } = row {
                            album.state.favorite = favorite;
                        }
                    },
                );
                true
            }
        }
    }
}

pub fn album_detail_list(
    shell: &Rc<CatalogUi>,
    model: AlbumDetailModel,
    key: LibraryListKey,
) -> AlbumDetailVirtualList {
    let fields = shell
        .settings
        .current
        .borrow()
        .library_list(key)
        .detail_track_fields;
    AlbumDetailVirtualList::new(shell, model, fields)
}

#[derive(Clone)]
struct DetailLayout {
    index: Arc<library::AlbumDetailIndex>,
    starts: Vec<usize>,
    count: usize,
    minimum: [usize; 2],
}
impl DetailLayout {
    fn new(index: Arc<library::AlbumDetailIndex>, minimum: [usize; 2]) -> Self {
        let mut count = 0;
        let starts = index
            .spans()
            .map(|(_, tracks, genres, _)| {
                let start = count;
                count += 1 + tracks.max(minimum[usize::from(genres)]);
                start
            })
            .collect();
        Self {
            index,
            starts,
            count,
            minimum,
        }
    }
    fn span(&self, position: usize) -> Option<(usize, usize, usize, bool, usize)> {
        if position >= self.count {
            return None;
        }
        let ordinal = self
            .starts
            .partition_point(|first| *first <= position)
            .saturating_sub(1);
        let (_, tracks, genres, flat) = self.index.spans().nth(ordinal)?;
        Some((ordinal, self.starts[ordinal], tracks, genres, flat))
    }
    fn flat(&self, position: usize) -> Option<usize> {
        let (_, first, tracks, _, flat) = self.span(position)?;
        (position - first <= tracks).then_some(flat + position - first)
    }
    fn native(&self, flat: usize) -> usize {
        let mut low = 0;
        let mut high = self.starts.len();
        while low < high {
            let middle = (low + high) / 2;
            if self
                .index
                .spans()
                .nth(middle)
                .is_some_and(|(_, _, _, first)| first <= flat)
            {
                low = middle + 1
            } else {
                high = middle
            }
        }
        let ordinal = low.saturating_sub(1);
        let Some((_, tracks, _, first)) = self.index.spans().nth(ordinal) else {
            return 0;
        };
        self.starts[ordinal] + flat.saturating_sub(first).min(tracks)
    }
    fn focus(&self, position: usize, forward: bool) -> usize {
        let Some((ordinal, first, tracks, _, _)) = self.span(position) else {
            return self.count.saturating_sub(1);
        };
        if position <= first + tracks {
            return position;
        }
        if forward {
            self.starts
                .get(ordinal + 1)
                .copied()
                .unwrap_or(first + tracks)
        } else {
            first + tracks
        }
    }
}

mod detail_rows_imp {
    use super::*;
    use gio::subclass::prelude::*;

    #[derive(Default)]
    pub(super) struct DetailRowsModel {
        pub(super) source: std::cell::OnceCell<SparseObjectModel>,
        pub(super) layout: RefCell<Option<Arc<DetailLayout>>>,
        pub(super) spacers: RefCell<BTreeMap<u32, glib::WeakRef<SparseObjectItem>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DetailRowsModel {
        const NAME: &'static str = "RufinAlbumDetailRowsModel";
        type Type = super::DetailRowsModel;
        type Interfaces = (gio::ListModel,);
    }
    impl ObjectImpl for DetailRowsModel {}
    impl ListModelImpl for DetailRowsModel {
        fn item_type(&self) -> glib::Type {
            SparseObjectItem::static_type()
        }
        fn n_items(&self) -> u32 {
            self.layout
                .borrow()
                .as_ref()
                .map_or(0, |layout| layout.count as u32)
        }
        fn item(&self, position: u32) -> Option<glib::Object> {
            let layout = self.layout.borrow().clone()?;
            if position as usize >= layout.count {
                return None;
            }
            if let Some(flat) = layout.flat(position as usize) {
                return self.source.get()?.item(flat as u32);
            }
            let mut spacers = self.spacers.borrow_mut();
            spacers.retain(|_, item| item.upgrade().is_some());
            if let Some(item) = spacers.get(&position).and_then(glib::WeakRef::upgrade) {
                return Some(item.upcast());
            }
            let item = SparseObjectItem::new((), true);
            spacers.insert(position, item.downgrade());
            Some(item.upcast())
        }
    }
}

glib::wrapper! {
    struct DetailRowsModel(ObjectSubclass<detail_rows_imp::DetailRowsModel>) @implements gio::ListModel;
}
impl DetailRowsModel {
    fn new(layout: Arc<DetailLayout>) -> Self {
        let model: Self = glib::Object::new();
        model.imp().layout.replace(Some(layout));
        model
    }
    fn layout(&self) -> Arc<DetailLayout> {
        self.imp()
            .layout
            .borrow()
            .as_ref()
            .expect("Detail layout")
            .clone()
    }
    fn changed(&self, old_count: usize) {
        self.imp().spacers.borrow_mut().clear();
        self.items_changed(0, old_count as u32, self.n_items());
    }
}

struct NativeDetailCell {
    item: glib::WeakRef<gtk::ListItem>,
    root: gtk::Box,
    gap: gtk::Box,
    stack: gtk::Stack,
    header_cells: Vec<gtk::Widget>,
    track: AlbumTrackSlot,
}

impl NativeDetailCell {
    fn new(
        shell: &Rc<CatalogUi>,
        fields: &[LibraryField],
        folder: Option<library::FolderKey>,
    ) -> Rc<Self> {
        let builder =
            gtk_widgets::ui_resource::builder(crate::ui_resource::ALBUM_DETAIL_ROW_RESOURCE);
        gtk_widgets::objects!(
            &builder,
            crate::ui_resource::ALBUM_DETAIL_ROW_RESOURCE,
            {
                root: gtk::Box,
                gap: gtk::Box,
                content: gtk::Stack,
                header: gtk::Box,
                track: gtk::Box
            }
        );
        let (titles, header_cells) = album_track_header(fields);
        header.append(&titles);
        let slot = AlbumTrackSlot::new(shell, fields, folder);
        track.append(&slot.widget);
        Rc::new(Self {
            item: glib::WeakRef::new(),
            root,
            gap,
            stack: content,
            header_cells,
            track: slot,
        })
    }
}

#[derive(Clone)]
pub struct AlbumDetailVirtualList {
    inner: Rc<AlbumDetailListInner>,
}
struct AlbumDetailListInner {
    shell: Weak<CatalogUi>,
    model: Rc<AlbumDetailModelInner>,
    list: gtk::ListView,
    cells: RefCell<BTreeMap<usize, Rc<NativeDetailCell>>>,
    panels: RefCell<BTreeMap<usize, Rc<AlbumSidePanel>>>,
    overlay: RefCell<glib::WeakRef<gtk::Overlay>>,
    scroll_handler: RefCell<Option<(gtk::Adjustment, glib::SignalHandlerId)>>,
    overlay_handler: RefCell<Option<glib::SignalHandlerId>>,
    model_handler: RefCell<Option<glib::SignalHandlerId>>,
    overlay_keys: RefCell<Option<gtk::EventControllerKey>>,
    display: RefCell<rufin_core::settings::layout::DisplaySettings>,
    fields: Vec<LibraryField>,
    panel_probe: [Rc<AlbumSidePanel>; 2],
    free_panels: RefCell<Vec<Rc<AlbumSidePanel>>>,
    panel_heights: Cell<[i32; 2]>,
    width: Cell<i32>,
    row_height: Cell<i32>,
    pending_restore: Cell<Option<ItemScrollPosition>>,
    pending_panel: Cell<Option<(usize, bool)>>,
    playing: RefCell<(Option<String>, bool)>,
}
impl AlbumDetailListInner {
    fn visible_cells(&self) -> Vec<(usize, Rc<NativeDetailCell>)> {
        self.cells
            .borrow()
            .values()
            .filter_map(|cell| {
                let item = cell.item.upgrade()?;
                cell.root
                    .is_mapped()
                    .then_some((item.position() as usize, cell.clone()))
            })
            .collect()
    }
    fn scroll(self: &Rc<Self>) -> NativeListScroll {
        let owner = Rc::downgrade(self);
        let items = Rc::new(move || {
            let Some(inner) = owner.upgrade() else {
                return Vec::new();
            };
            let layout = inner.model.native.layout();
            let panels = inner.panels.borrow();
            let mut items = inner
                .visible_cells()
                .into_iter()
                .filter_map(|(position, cell)| {
                    if panels.contains_key(&position) {
                        return None;
                    }
                    Some((layout.flat(position)? as u32, cell.root.clone().upcast()))
                })
                .collect::<Vec<_>>();
            items.extend(panels.iter().filter_map(|(first, panel)| {
                Some((layout.flat(*first)? as u32, panel.root.clone().upcast()))
            }));
            items
        });
        let owner = Rc::downgrade(self);
        NativeListScroll::new(self.list.clone(), items, true).with_native_position(Rc::new(
            move |position| {
                owner.upgrade().map_or(0, |inner| {
                    inner.model.native.layout().native(position as usize) as u32
                })
            },
        ))
    }
    fn refresh_layout(self: &Rc<Self>, width: i32) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        let cells = self.cells.borrow().values().cloned().collect::<Vec<_>>();
        let Some(sample) = cells.first() else { return };
        let metrics = album_detail_row_metrics_for_width(width);
        let display = self.display.borrow().clone();
        let widths = album_track_field_widths(&self.fields, album_track_area_width(width, metrics));
        let width_changed = self.width.get() != width;
        if width_changed {
            for cell in &cells {
                cell.gap
                    .set_width_request(metrics.meta_width + metrics.spacing);
                cell.stack.set_margin_start(metrics.spacing);
                cell.track.resize(&widths, display_track_height(&display));
                for (widget, (_, width)) in cell.header_cells.iter().zip(&widths) {
                    widget.set_width_request(*width)
                }
            }
            for probe in &self.panel_probe {
                probe.resize(metrics);
            }
        }
        let height = sample
            .root
            .measure(gtk::Orientation::Vertical, width)
            .1
            .max(1);
        let [plain, genres] = self.panel_probe.each_ref().map(|probe| {
            probe
                .root
                .measure(gtk::Orientation::Vertical, metrics.meta_width)
                .1
        });
        let minimum = [plain, genres].map(|panel| {
            (panel.max(1) as usize)
                .div_ceil(height as usize)
                .saturating_sub(1)
        });
        self.width.set(width);
        self.row_height.set(height);
        self.panel_heights.set([plain, genres]);
        let previous = self.model.native.layout();
        if previous.minimum != minimum {
            let saved = self
                .pending_restore
                .get()
                .or_else(|| self.scroll().capture());
            let layout = Arc::new(DetailLayout::new(previous.index.clone(), minimum));
            self.model.native.imp().layout.replace(Some(layout));
            self.model.native.changed(previous.count);
            if let Some(saved) = saved {
                self.pending_restore.set(Some(saved));
                self.scroll().scroll_to(saved.position);
            }
        }
        let capacity = self.resize_panels(&shell, width_changed);
        self.sync_panels(&shell, capacity);
    }
    fn resize_panels(&self, shell: &Rc<CatalogUi>, width_changed: bool) -> usize {
        let span = (self
            .model
            .native
            .layout()
            .minimum
            .into_iter()
            .min()
            .unwrap_or(0)
            + 1)
            * self.row_height.get().max(1) as usize;
        let capacity = (self.list.height().max(0) as usize).div_ceil(span) + 2;
        let active = self.panels.borrow();
        let mut free = self.free_panels.borrow_mut();
        while active.len() + free.len() < capacity {
            let panel = Rc::new(AlbumSidePanel::new(shell));
            panel.resize(album_detail_row_metrics_for_width(self.width.get()));
            free.push(panel);
        }
        if width_changed {
            for panel in active.values().chain(free.iter()) {
                panel.resize(album_detail_row_metrics_for_width(self.width.get()));
            }
        }
        capacity
    }
    fn bind_cell(&self, cell: &NativeDetailCell, item: &gtk::ListItem) {
        let position = item.position() as usize;
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        let layout = self.model.native.layout();
        let Some((ordinal, first, tracks, _, _)) = layout.span(position) else {
            return;
        };
        let end = layout
            .starts
            .get(ordinal + 1)
            .copied()
            .unwrap_or(layout.count);
        if position + 1 == end {
            cell.root.add_css_class("album-detail-block-end");
        } else {
            cell.root.remove_css_class("album-detail-block-end");
        }
        let row = item
            .item()
            .and_downcast::<SparseObjectItem>()
            .and_then(|item| item.value::<Arc<AlbumDetailRow>>());
        let local = position - first;
        let pending = row.is_none();
        if local == 0 {
            cell.stack
                .set_visible_child_name(if self.display.borrow().show_header {
                    "header"
                } else {
                    "blank"
                });
        } else if local <= tracks {
            cell.stack.set_visible_child_name("track");
        } else {
            cell.stack.set_visible_child_name("blank");
        }
        let track = row.as_ref().and_then(|row| match row.as_ref() {
            AlbumDetailRow::Track { track, index, .. } => Some((*index, track.clone())),
            _ => None,
        });
        cell.track
            .bind(&shell, track, pending && local > 0 && local <= tracks);
        item.set_selectable(false);
        item.set_activatable(false);
        item.set_focusable(local <= tracks);
        item.set_accessible_label(
            &row.as_ref()
                .map(|row| match row.as_ref() {
                    AlbumDetailRow::Album { album, .. } => album.metadata.title.clone(),
                    AlbumDetailRow::Track { track, .. } => track.metadata.title.clone(),
                })
                .unwrap_or_default(),
        );
        let playing = self.playing.borrow();
        cell.track.playing(playing.0.as_deref(), playing.1);
    }
    fn sync_panels(&self, shell: &CatalogUi, capacity: usize) {
        let Some(overlay) = self.overlay.borrow().upgrade() else {
            return;
        };
        let layout = self.model.native.layout();
        let mut wanted = BTreeMap::new();
        for (position, cell) in self.visible_cells() {
            let Some((_, first, _, genres, _)) = layout.span(position) else {
                continue;
            };
            // GTK owns mapped row parents; factory bind can still see floating rows.
            if let Some(owner) = cell.root.parent() {
                owner.update_state(&[gtk::accessible::State::Hidden(
                    layout.flat(position).is_none(),
                )]);
            }
            let Some(bounds) = cell.root.compute_bounds(&self.list) else {
                continue;
            };
            let top = bounds.y() - (position - first) as f32 * self.row_height.get() as f32;
            let panel_height = self.panel_heights.get()[usize::from(genres)];
            if top < self.list.height() as f32 && top + panel_height as f32 > 0.0 {
                wanted.insert(first, (top, genres));
            }
        }
        let mut panels = self.panels.borrow_mut();
        let removed = panels
            .keys()
            .filter(|first| !wanted.contains_key(first))
            .copied()
            .collect::<Vec<_>>();
        for first in removed {
            if let Some(panel) = panels.remove(&first) {
                if panel
                    .root
                    .state_flags()
                    .contains(gtk::StateFlags::FOCUS_WITHIN)
                {
                    self.list.grab_focus();
                }
                overlay.remove_overlay(&panel.root);
                panel.bind(shell, None, false);
                self.free_panels.borrow_mut().push(panel);
            }
        }
        for (first, (_, genres)) in wanted {
            let panel = if let Some(panel) = panels.get(&first) {
                panel.clone()
            } else {
                let Some(panel) = self.free_panels.borrow_mut().pop() else {
                    continue;
                };
                overlay.add_overlay(&panel.root);
                overlay.set_clip_overlay(&panel.root, true);
                overlay.set_measure_overlay(&panel.root, false);
                panels.insert(first, panel.clone());
                panel
            };
            let item = layout.flat(first).and_then(|flat| {
                self.model
                    .rows
                    .list_model()
                    .item(flat as u32)
                    .and_downcast::<SparseObjectItem>()
            });
            panel.bind(shell, item, genres);
        }
        self.free_panels
            .borrow_mut()
            .truncate(capacity.saturating_sub(panels.len()));
        drop(panels);
        if let Some((position, backward)) = self.pending_panel.get()
            && let Some(panel) = self.panels.borrow().get(&position)
            && panel.ready.get()
        {
            self.pending_panel.set(None);
            panel.root.child_focus(if backward {
                gtk::DirectionType::TabBackward
            } else {
                gtk::DirectionType::TabForward
            });
        }
    }
    fn key(&self, key: gtk::gdk::Key, shift: bool) -> glib::Propagation {
        let focused = self.visible_cells().into_iter().find(|(_, cell)| {
            cell.root.parent().is_some_and(|row| {
                row.has_focus() || row.state_flags().contains(gtk::StateFlags::FOCUS_WITHIN)
            })
        });
        let panel = self
            .panels
            .borrow()
            .iter()
            .find(|(_, panel)| {
                panel
                    .root
                    .state_flags()
                    .contains(gtk::StateFlags::FOCUS_WITHIN)
            })
            .map(|(first, panel)| (*first, panel.clone()));
        let tab = matches!(key, gtk::gdk::Key::Tab | gtk::gdk::Key::ISO_Left_Tab);
        let forward = !shift && key != gtk::gdk::Key::ISO_Left_Tab;
        if tab
            && let Some((_, cell)) = &focused
            && cell.root.child_focus(if forward {
                gtk::DirectionType::TabForward
            } else {
                gtk::DirectionType::TabBackward
            })
        {
            return glib::Propagation::Stop;
        }
        if tab
            && let Some((_, panel)) = &panel
            && panel.root.child_focus(if forward {
                gtk::DirectionType::TabForward
            } else {
                gtk::DirectionType::TabBackward
            })
        {
            return glib::Propagation::Stop;
        }
        if let Some(direction) = match key {
            gtk::gdk::Key::Up => Some(gtk::DirectionType::Up),
            gtk::gdk::Key::Down => Some(gtk::DirectionType::Down),
            _ => None,
        } {
            if focused.as_ref().is_some_and(|(_, cell)| {
                cell.root
                    .state_flags()
                    .contains(gtk::StateFlags::FOCUS_WITHIN)
                    && cell.root.child_focus(direction)
            }) || panel
                .as_ref()
                .is_some_and(|(_, panel)| panel.root.child_focus(direction))
            {
                return glib::Propagation::Stop;
            }
        }
        let layout = self.model.native.layout();
        let current = focused
            .as_ref()
            .map(|(position, _)| *position)
            .or_else(|| panel.as_ref().map(|(first, _)| *first))
            .unwrap_or(0);
        let Some((ordinal, first, tracks, _, _)) = layout.span(current) else {
            return glib::Propagation::Proceed;
        };
        let mut next = match key {
            gtk::gdk::Key::Up => current.saturating_sub(1),
            gtk::gdk::Key::Down => (current + 1).min(layout.count.saturating_sub(1)),
            gtk::gdk::Key::Home => 0,
            gtk::gdk::Key::End => layout.count.saturating_sub(1),
            gtk::gdk::Key::Page_Up => {
                current.saturating_sub((self.list.height() / self.row_height.get()).max(1) as usize)
            }
            gtk::gdk::Key::Page_Down => (current
                + (self.list.height() / self.row_height.get()).max(1) as usize)
                .min(layout.count.saturating_sub(1)),
            gtk::gdk::Key::Tab | gtk::gdk::Key::ISO_Left_Tab => {
                if panel.is_some() {
                    if forward && tracks > 0 {
                        first + 1
                    } else if forward && ordinal + 1 < layout.starts.len() {
                        layout.starts[ordinal + 1]
                    } else if !forward && ordinal > 0 {
                        first - 1
                    } else {
                        return glib::Propagation::Proceed;
                    }
                } else if current == first && forward {
                    self.pending_panel.set(Some((first, false)));
                    if let Some(overlay) = self.overlay.borrow().upgrade() {
                        overlay.queue_allocate();
                    }
                    return glib::Propagation::Stop;
                } else if current == first && ordinal == 0 {
                    return glib::Propagation::Proceed;
                } else if !forward && current == first + 1 {
                    first
                } else if forward && current == first + tracks {
                    if ordinal + 1 < layout.starts.len() {
                        layout.starts[ordinal + 1]
                    } else {
                        return glib::Propagation::Proceed;
                    }
                } else if forward {
                    current + 1
                } else {
                    current.saturating_sub(1)
                }
            }
            _ => return glib::Propagation::Proceed,
        };
        next = layout.focus(
            next,
            !matches!(
                key,
                gtk::gdk::Key::Up | gtk::gdk::Key::Page_Up | gtk::gdk::Key::Home
            ) && (!tab || forward),
        );
        if tab && layout.starts.binary_search(&next).is_ok() {
            self.pending_panel.set(Some((next, !forward)));
            self.list
                .scroll_to(next as u32, gtk::ListScrollFlags::NONE, None);
        } else {
            self.list
                .scroll_to(next as u32, gtk::ListScrollFlags::FOCUS, None);
        }
        if let Some(overlay) = self.overlay.borrow().upgrade() {
            overlay.queue_allocate();
        }
        glib::Propagation::Stop
    }
}
impl AlbumDetailListInner {
    fn detach(&self) {
        if let Some((adjustment, handler)) = self.scroll_handler.borrow_mut().take() {
            adjustment.disconnect(handler);
        }
        let previous = self.overlay.replace(glib::WeakRef::new());
        if let Some(overlay) = previous.upgrade() {
            if let Some(handler) = self.overlay_handler.borrow_mut().take() {
                overlay.disconnect(handler);
            }
            if let Some(keys) = self.overlay_keys.borrow_mut().take() {
                overlay.remove_controller(&keys);
            }
            for panel in std::mem::take(&mut *self.panels.borrow_mut()).into_values() {
                overlay.remove_overlay(&panel.root);
                self.free_panels.borrow_mut().push(panel);
            }
            for probe in self
                .panel_probe
                .iter()
                .map(|probe| probe.root.upcast_ref::<gtk::Widget>())
            {
                overlay.remove_overlay(probe);
            }
        }
    }
}
impl Drop for AlbumDetailListInner {
    fn drop(&mut self) {
        if let Some(handler) = self.model_handler.get_mut().take() {
            self.model.native.disconnect(handler);
        }
        self.detach();
    }
}

impl AlbumDetailVirtualList {
    fn new(shell: &Rc<CatalogUi>, model: AlbumDetailModel, fields: Vec<LibraryField>) -> Self {
        let model = model.inner;
        let rows = model.rows.clone();
        let panel_probe = std::array::from_fn(|index| {
            let panel = Rc::new(AlbumSidePanel::new(shell));
            panel.title.set_text(" ");
            panel.artist_label.set_text(" ");
            panel.release.set_text(" ");
            panel.facts.set_text(" ");
            panel.genres.set_text(" ");
            panel.genres.set_visible(index != 0);
            panel
        });
        let flat_initial = model.initial_position.get();
        let initial = model.native.layout().native(flat_initial);
        let factory = gtk::SignalListItemFactory::new();
        let list = gtk::ListView::new(
            Some(gtk::NoSelection::new(Some(model.native.clone()))),
            Some(factory.clone()),
        );
        list.add_css_class("track-table");
        list.add_css_class("album-detail-list");
        list.set_single_click_activate(false);
        let inner = Rc::new(AlbumDetailListInner {
            shell: Rc::downgrade(shell),
            model: model.clone(),
            list: list.clone(),
            cells: RefCell::new(BTreeMap::new()),
            panels: RefCell::new(BTreeMap::new()),
            overlay: RefCell::new(glib::WeakRef::new()),
            scroll_handler: RefCell::new(None),
            overlay_handler: RefCell::new(None),
            model_handler: RefCell::new(None),
            overlay_keys: RefCell::new(None),
            display: RefCell::new(detail_display(shell)),
            fields,
            panel_probe,
            free_panels: RefCell::new(Vec::new()),
            panel_heights: Cell::new([0, 0]),
            width: Cell::new(0),
            row_height: Cell::new(1),
            pending_restore: Cell::new(None),
            pending_panel: Cell::new(None),
            playing: RefCell::new((None, false)),
        });
        let weak = Rc::downgrade(&inner);
        let setup_shell = Rc::downgrade(shell);
        factory.connect_setup(move |_, item| {
            let (Some(inner), Some(shell), Some(item)) = (
                weak.upgrade(),
                setup_shell.upgrade(),
                item.downcast_ref::<gtk::ListItem>(),
            ) else {
                return;
            };
            let cell = NativeDetailCell::new(&shell, &inner.fields, inner.model.folder);
            cell.item.set(Some(item));
            item.set_child(Some(&cell.root));
            inner
                .cells
                .borrow_mut()
                .insert(item.as_ptr() as usize, cell.clone());
            let metrics = album_detail_row_metrics_for_width(inner.width.get());
            let widths = album_track_field_widths(
                &inner.fields,
                album_track_area_width(inner.width.get(), metrics),
            );
            cell.gap
                .set_width_request(metrics.meta_width + metrics.spacing);
            cell.stack.set_margin_start(metrics.spacing);
            cell.track
                .resize(&widths, display_track_height(&inner.display.borrow()));
            for (widget, (_, width)) in cell.header_cells.iter().zip(&widths) {
                widget.set_width_request(*width);
            }
        });
        let weak = Rc::downgrade(&inner);
        connect_sparse_bind(&factory, move |item| {
            let (Some(inner), Some(item)) = (weak.upgrade(), item.downcast_ref::<gtk::ListItem>())
            else {
                return;
            };
            let cell = inner.cells.borrow().get(&(item.as_ptr() as usize)).cloned();
            if let Some(cell) = cell {
                inner.bind_cell(&cell, item);
            }
            if let Some(overlay) = inner.overlay.borrow().upgrade() {
                overlay.queue_allocate();
            }
        });
        let weak = Rc::downgrade(&inner);
        factory.connect_teardown(move |_, item| {
            if let Some(inner) = weak.upgrade() {
                inner.cells.borrow_mut().remove(&(item.as_ptr() as usize));
            }
        });
        let weak = Rc::downgrade(&inner);
        let handler = model.native.connect_items_changed(move |_, _, _, _| {
            if let Some(inner) = weak.upgrade()
                && let Some(overlay) = inner.overlay.borrow().upgrade()
            {
                overlay.queue_allocate();
            }
        });
        inner.model_handler.replace(Some(handler));
        let weak = Rc::downgrade(&inner);
        rows.connect_ready_changed(move |_, _| {
            if let Some(inner) = weak.upgrade()
                && let Some(overlay) = inner.overlay.borrow().upgrade()
            {
                overlay.queue_allocate();
            }
        });
        let weak = Rc::downgrade(&inner);
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(move |_, key, _, state| {
            if state
                .intersects(gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::ALT_MASK)
            {
                return glib::Propagation::Proceed;
            }
            weak.upgrade().map_or(glib::Propagation::Proceed, |inner| {
                inner.key(key, state.contains(gtk::gdk::ModifierType::SHIFT_MASK))
            })
        });
        list.add_controller(keys);
        let weak = Rc::downgrade(&inner);
        shell.register_current_route_track_selection(Rc::new(move |current| {
            let Some(inner) = weak.upgrade() else {
                return false;
            };
            let (uri, paused) = current.map_or((None, false), |current| {
                (Some(current.media_uri.clone()), current.paused)
            });
            inner.playing.replace((uri.clone(), paused));
            for cell in inner.cells.borrow().values() {
                cell.track.playing(uri.as_deref(), paused);
            }
            true
        }));
        if initial > 0 {
            list.scroll_to(initial as u32, gtk::ListScrollFlags::NONE, None);
        }
        Self { inner }
    }
    pub fn widget(&self) -> gtk::Widget {
        self.inner.list.clone().upcast()
    }
    pub fn attach_scroller(&self, scroller: &gtk::ScrolledWindow, overlay: &gtk::Overlay) {
        if self.inner.overlay.borrow().upgrade().as_ref() == Some(overlay) {
            return;
        }
        self.inner.detach();
        self.inner.overlay.replace(overlay.downgrade());
        for probe in self
            .inner
            .panel_probe
            .iter()
            .map(|probe| probe.root.upcast_ref::<gtk::Widget>())
        {
            overlay.add_overlay(probe);
            probe.set_child_visible(false);
            probe.set_sensitive(false);
            overlay.set_measure_overlay(probe, false);
        }
        let weak = Rc::downgrade(&self.inner);
        let position_handler = overlay.connect_get_child_position(move |_, widget| {
            let inner = weak.upgrade()?;
            let first = *inner
                .panels
                .borrow()
                .iter()
                .find(|(_, panel)| panel.root.upcast_ref::<gtk::Widget>() == widget)?
                .0;
            let layout = inner.model.native.layout();
            let (position, cell) = inner.visible_cells().into_iter().find(|(position, _)| {
                layout
                    .span(*position)
                    .is_some_and(|(_, start, _, _, _)| start == first)
            })?;
            let bounds = cell
                .root
                .compute_bounds(&inner.overlay.borrow().upgrade()?)?;
            let gap = cell
                .gap
                .compute_bounds(&inner.overlay.borrow().upgrade()?)?;
            let metrics = album_detail_row_metrics_for_width(inner.width.get());
            let x = gap.x() as i32
                + if inner.list.direction() == gtk::TextDirection::Rtl {
                    metrics.spacing
                } else {
                    0
                };
            let y = bounds.y() as i32 - (position - first) as i32 * inner.row_height.get();
            let (_, _, _, genres, _) = layout.span(first)?;
            Some(gtk::gdk::Rectangle::new(
                x,
                y,
                metrics.meta_width,
                inner.panel_heights.get()[usize::from(genres)],
            ))
        });
        self.inner.overlay_handler.replace(Some(position_handler));
        let weak = Rc::downgrade(&self.inner);
        let adjustment = scroller.vadjustment();
        let handler = adjustment.connect_value_changed(move |_| {
            if let Some(inner) = weak.upgrade()
                && let Some(overlay) = inner.overlay.borrow().upgrade()
            {
                overlay.queue_allocate();
            }
        });
        self.inner
            .scroll_handler
            .replace(Some((adjustment, handler)));
        let weak = Rc::downgrade(&self.inner);
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(move |_, key, _, state| {
            if state
                .intersects(gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::ALT_MASK)
            {
                return glib::Propagation::Proceed;
            }

            let Some(inner) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if inner.panels.borrow().values().any(|panel| {
                panel
                    .root
                    .state_flags()
                    .contains(gtk::StateFlags::FOCUS_WITHIN)
            }) {
                inner.key(key, state.contains(gtk::gdk::ModifierType::SHIFT_MASK))
            } else {
                glib::Propagation::Proceed
            }
        });
        overlay.add_controller(keys.clone());
        self.inner.overlay_keys.replace(Some(keys));
    }
    pub fn restore(&self, saved: ItemScrollPosition) {
        self.inner.pending_restore.set(Some(saved));
    }
    pub fn after_allocate(&self) {
        self.inner.refresh_layout(self.inner.list.width());
        if let Some(saved) = self.inner.pending_restore.get() {
            let scroll = self.inner.scroll();
            if scroll.restore_offset(saved) {
                self.inner.pending_restore.set(None);
            } else {
                scroll.scroll_to(saved.position);
            }
        }
    }
    pub fn apply_display(&self, display: &rufin_core::settings::layout::DisplaySettings) {
        self.inner.display.replace(display.clone());
        self.inner.width.set(0);
        self.inner.list.queue_resize();
    }
    pub fn fit_allocation(&self, width: i32) {
        self.inner.refresh_layout(width);
    }
    pub fn item_scroll(&self) -> NativeListScroll {
        self.inner.scroll()
    }
    pub fn navigate(&self, direction: gtk::DirectionType) -> glib::Propagation {
        self.inner.key(
            match direction {
                gtk::DirectionType::Up => gtk::gdk::Key::Up,
                gtk::DirectionType::Down => gtk::gdk::Key::Down,
                _ => return glib::Propagation::Proceed,
            },
            false,
        )
    }
}

struct AlbumSidePanel {
    root: gtk::Box,
    cover: super::cards::AlbumCover,
    cover_host: gtk::Stack,
    binding: RefCell<Option<Arc<AlbumDetailRow>>>,
    item: RefCell<Option<SparseObjectItem>>,
    skeleton: gtk::Box,
    title: gtk::Label,
    artist: gtk_widgets::detail_links::DetailLinkBinding,
    artist_label: gtk::Label,
    release: gtk::Label,
    facts: gtk::Label,
    genres: gtk::Label,
    ready: Cell<bool>,
}
impl AlbumSidePanel {
    fn new(shell: &Rc<CatalogUi>) -> Self {
        let builder =
            gtk_widgets::ui_resource::builder(crate::ui_resource::ALBUM_DETAIL_PANEL_RESOURCE);
        gtk_widgets::objects!(
            &builder,
            crate::ui_resource::ALBUM_DETAIL_PANEL_RESOURCE,
            {
                root: gtk::Box,
                cover_host: gtk::Stack,
                skeleton: gtk::Box,
                title: gtk::Label,
                artist: gtk::Label,
                release: gtk::Label,
                facts: gtk::Label,
                genres: gtk::Label
            }
        );
        let cover = super::cards::AlbumCover::new(shell);
        cover_host.add_named(&cover.widget, Some("cover"));
        cover_host.set_visible_child_name("pending");
        root.set_sensitive(false);
        let artist_label = artist.clone();
        let artist =
            gtk_widgets::detail_links::DetailLinkBinding::new(&artist, shell.route_navigation());
        Self {
            root,
            cover,
            cover_host,
            binding: RefCell::new(None),
            item: RefCell::new(None),
            skeleton,
            title,
            artist,
            artist_label,
            release,
            facts,
            genres,
            ready: Cell::new(false),
        }
    }
    fn resize(&self, metrics: AlbumDetailRowMetrics) {
        self.root.set_width_request(metrics.meta_width);
        self.cover.resize(metrics.cover_size);
        self.skeleton
            .set_size_request(metrics.cover_size, metrics.cover_size);
    }
    fn bind(&self, shell: &CatalogUi, item: Option<SparseObjectItem>, genres: bool) {
        self.genres.set_visible(genres);
        let row = item
            .as_ref()
            .filter(|item| item.is_ready())
            .and_then(|item| item.value::<Arc<AlbumDetailRow>>());
        self.item.replace(item);
        let same = match (self.binding.borrow().as_ref(), row.as_ref()) {
            (Some(old), Some(new)) => Arc::ptr_eq(old, new),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        let album = row.as_ref().and_then(|row| match row.as_ref() {
            AlbumDetailRow::Album { album, .. } => Some(album),
            _ => None,
        });
        self.ready.set(album.is_some());
        self.cover.bind(shell, album);
        self.cover_host
            .set_visible_child_name(if album.is_some() { "cover" } else { "pending" });
        if let Some(album) = album {
            self.title.set_text(&album.metadata.title);
            self.artist
                .bind(gtk_widgets::detail_links::album_artist_links(album));
            self.release.set_text(
                &album
                    .metadata
                    .year
                    .map(|year| year.to_string())
                    .unwrap_or_default(),
            );
            let facts = [
                track_count_text(album.summary.track_count.max(0) as u64),
                gtk_widgets::format_duration_units(
                    (album.summary.duration_millis.max(0) / 1000) as u32,
                ),
            ];
            self.facts.set_text(&facts.join(" • "));
            self.genres.set_text(
                &album
                    .links
                    .genres
                    .iter()
                    .map(|genre| genre.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        } else {
            self.title.set_text("");
            self.artist.clear();
            self.release.set_text("");
            self.facts.set_text("");
            self.genres.set_text("");
        }
        self.root.set_sensitive(album.is_some());
        let ready = album.is_some();
        self.binding.replace(row);
        for label in [&self.title, &self.release, &self.facts, &self.genres] {
            if ready {
                label.remove_css_class("track-skeleton");
            } else {
                label.add_css_class("track-skeleton");
            }
        }
    }
}

struct AlbumTrackSlot {
    widget: gtk::Box,
    current: Rc<RefCell<Option<Rc<TrackView>>>>,
    cells: Vec<(gtk::Widget, AlbumTrackField)>,
}

enum AlbumTrackField {
    Text(LibraryField, gtk::Label),
    Index(gtk::Overlay),
    Image(ArtworkTile),
    Favorite(gtk::Button),
}

impl AlbumTrackSlot {
    fn new(
        shell: &Rc<CatalogUi>,
        fields: &[LibraryField],
        folder: Option<library::FolderKey>,
    ) -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Horizontal, ALBUM_DETAIL_TRACK_COLUMN_GAP);
        widget.add_css_class("album-detail-track-row");
        widget.set_hexpand(true);
        let motion = gtk::EventControllerMotion::new();
        let update_hover = |motion: &gtk::EventControllerMotion, x, y| {
            if let Some(widget) = motion.widget() {
                // Grab restoration can report enter outside the row at (-1, -1).
                if widget.contains(x, y) {
                    widget.add_css_class("album-detail-track-hover");
                } else {
                    widget.remove_css_class("album-detail-track-hover");
                }
            }
        };
        motion.connect_enter(update_hover);
        motion.connect_motion(update_hover);
        motion.connect_leave(|motion| {
            if let Some(widget) = motion.widget() {
                widget.remove_css_class("album-detail-track-hover");
            }
        });
        widget.add_controller(motion);
        let current = Rc::new(RefCell::new(None::<Rc<TrackView>>));
        let menu_shell = Rc::downgrade(shell);
        let row = Rc::clone(&current);
        let menu: gtk_widgets::interactions::ContextMenuOpen = Rc::new(move |target, position| {
            let row = row.borrow().clone();
            if let Some(shell) = menu_shell.upgrade()
                && let Some(track) = row
            {
                present_track_context_menu(target, &shell, track.media_uri.clone(), position);
            }
        });
        install_context_menu_openers(&widget, Rc::clone(&menu));
        let row = Rc::clone(&current);
        let click_shell = Rc::downgrade(shell);
        let click = gtk::GestureClick::new();
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        click.set_button(1);
        click.connect_pressed(move |gesture, presses, _, _| {
            let row = row.borrow().clone();
            if album_detail_play_click(presses)
                && let Some(shell) = click_shell.upgrade()
                && let Some(track) = row
            {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                play_album_track(&shell, track.links.album_key, track.track_key, folder);
            }
        });
        widget.add_controller(click);
        let cells = fields
            .iter()
            .map(|field| {
                let (child, binding) = match field {
                    LibraryField::Tools => {
                        let button = row_favorite_icon_button("Favorite track");
                        let actions =
                            gtk_widgets::recycled_cells::RowActions::with_favorite(&button);
                        install_context_menu_openers(&actions, Rc::clone(&menu));
                        let row = Rc::downgrade(&current);
                        shell.register_dynamic_favorite_button(
                            Rc::new(move || {
                                Some(gtk_widgets::favorites::track_favorite_key(
                                    &row.upgrade()?.borrow().as_ref()?.media_uri,
                                ))
                            }),
                            &button,
                        );
                        let row = Rc::clone(&current);
                        let favorite_shell = Rc::downgrade(shell);
                        button.connect_clicked(move |button| {
                            let row = row.borrow().clone();
                            if let Some(shell) = favorite_shell.upgrade()
                                && let Some(track) = row
                            {
                                shell.set_favorite_with_feedback(
                                    library::FavoriteTarget::Track(track.media_uri.clone()),
                                    !favorite_button_is_active(button),
                                    Some(button),
                                );
                            }
                        });
                        (actions.upcast(), AlbumTrackField::Favorite(button))
                    }
                    LibraryField::Image => {
                        let tile = ArtworkTile::new(32);
                        (tile.widget(), AlbumTrackField::Image(tile))
                    }
                    LibraryField::RowIndex => {
                        let row = Rc::clone(&current);
                        let play_shell = Rc::downgrade(shell);
                        let cell = super::columns::track_row_index_cell("", move |_| {
                            let row = row.borrow().clone();
                            if let Some(shell) = play_shell.upgrade()
                                && let Some(track) = row
                            {
                                play_album_track(
                                    &shell,
                                    track.links.album_key,
                                    track.track_key,
                                    folder,
                                );
                            }
                        });
                        (cell.clone().upcast(), AlbumTrackField::Index(cell))
                    }
                    field => {
                        let label = album_track_label("", *field, 1);
                        label.set_hexpand(true);
                        (label.clone().upcast(), AlbumTrackField::Text(*field, label))
                    }
                };
                let cell = fixed_album_track_cell(1, ALBUM_TRACK_HEIGHT, child);
                widget.append(&cell);
                (cell, binding)
            })
            .collect();
        Self {
            widget,
            current,
            cells,
        }
    }

    fn bind(&self, shell: &CatalogUi, value: Option<(usize, TrackView)>, pending: bool) {
        if self.current.borrow().as_ref().map(|track| track.track_key)
            != value.as_ref().map(|(_, track)| track.track_key)
        {
            self.widget.remove_css_class("album-detail-track-hover");
        }
        let index = value.as_ref().map_or(0, |(index, _)| *index);
        self.current.replace(value.map(|(_, track)| Rc::new(track)));
        if index % 2 == 1 {
            self.widget.add_css_class("display-even");
        } else {
            self.widget.remove_css_class("display-even");
        }
        let current = self.current.borrow().clone();
        for (widget, cell) in &self.cells {
            if pending && !matches!(cell, AlbumTrackField::Favorite(_)) {
                widget.add_css_class("track-skeleton");
            } else {
                widget.remove_css_class("track-skeleton");
            }
            match cell {
                AlbumTrackField::Text(field, label) => {
                    label.set_text(&current.as_ref().map_or_else(String::new, |track| {
                        album_detail_track_text(track, index, *field)
                    }))
                }
                AlbumTrackField::Index(cell) => super::columns::set_track_row_index_text(
                    cell,
                    &current
                        .as_ref()
                        .map_or_else(String::new, |_| (index + 1).to_string()),
                ),
                AlbumTrackField::Favorite(button) => set_favorite_button_active(
                    button,
                    current.as_ref().is_some_and(|track| track.state.favorite),
                ),
                AlbumTrackField::Image(tile) => {
                    if let Some(track) = current.as_ref() {
                        shell.artwork.bind_artwork_tile(
                            tile,
                            opaque_artwork(track.artwork_binding.as_deref()),
                            32,
                        );
                    } else {
                        shell.artwork.clear_artwork_tile(tile);
                    }
                }
            }
        }
    }

    fn resize(&self, widths: &[(LibraryField, i32)], height: i32) {
        self.widget.set_height_request(height);
        for ((cell, _), (_, width)) in self.cells.iter().zip(widths) {
            cell.set_size_request(*width, height);
        }
    }

    fn playing(&self, uri: Option<&str>, paused: bool) {
        let playing = self
            .current
            .borrow()
            .as_ref()
            .is_some_and(|track| uri == Some(track.media_uri.as_str()));
        for (_, cell) in &self.cells {
            let widget = match cell {
                AlbumTrackField::Index(cell) => cell.upcast_ref(),
                AlbumTrackField::Text(LibraryField::Title | LibraryField::TitleMerged, label) => {
                    label.upcast_ref()
                }
                _ => continue,
            };
            gtk_widgets::recycled_cells::set_track_playing(widget, playing, paused);
        }
    }
}

fn album_detail_play_click(presses: i32) -> bool {
    presses >= 2 && presses % 2 == 0
}

fn album_detail_row_metrics_for_width(route_width: i32) -> AlbumDetailRowMetrics {
    let row_width = route_width
        .saturating_sub(ALBUM_DETAIL_ROW_HORIZONTAL_INSET)
        .max(1);
    let spacing = if row_width < ALBUM_DETAIL_NARROW_WIDTH {
        8
    } else {
        12
    };
    let target = if row_width < ALBUM_DETAIL_NARROW_WIDTH {
        row_width * 38 / 100
    } else {
        row_width * 42 / 100
    };
    let cover_size = if row_width < ALBUM_DETAIL_MIN_COVER {
        row_width
    } else {
        target.clamp(ALBUM_DETAIL_MIN_COVER, ALBUM_DETAIL_MAX_COVER)
    }
    .min(row_width)
    .max(1);
    AlbumDetailRowMetrics {
        cover_size,
        meta_width: cover_size,
        spacing,
    }
}

fn album_track_header(fields: &[LibraryField]) -> (gtk::Box, Vec<gtk::Widget>) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, ALBUM_DETAIL_TRACK_COLUMN_GAP);
    row.add_css_class("album-detail-track-cells");
    row.add_css_class("table-header");
    let cells = fields
        .iter()
        .map(|field| {
            let label = if *field == LibraryField::Duration {
                let image = gtk::Image::from_icon_name("rufin-preferences-system-time-symbolic");
                image.set_tooltip_text(Some(&localization::tr("Duration")));
                image.upcast()
            } else if *field == LibraryField::Tools {
                gtk::Box::new(gtk::Orientation::Horizontal, 0).upcast()
            } else {
                let label = album_track_label(
                    &localization::tr(gtk_widgets::settings::library_field_title(*field)),
                    *field,
                    1,
                );
                label.remove_css_class("track-list-title");
                label.set_hexpand(true);
                label.upcast()
            };
            let cell = fixed_album_track_cell(1, -1, label);
            row.append(&cell);
            cell
        })
        .collect();
    (row, cells)
}

fn album_track_label(text: &str, field: LibraryField, width: i32) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    if field == LibraryField::Duration {
        label.add_css_class("tabular-numeric");
    }
    if matches!(field, LibraryField::Title | LibraryField::TitleMerged) {
        label.add_css_class("track-list-title");
    }
    label.set_xalign(if field == LibraryField::RowIndex {
        0.5
    } else {
        0.0
    });
    label.set_halign(gtk::Align::Fill);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_single_line_mode(true);
    label.set_width_request(width);
    label
}

fn fixed_album_track_cell(width: i32, height: i32, child: gtk::Widget) -> gtk::Widget {
    let cell = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    cell.set_hexpand(false);
    cell.set_size_request(width.max(1), height.max(1));
    cell.set_overflow(gtk::Overflow::Hidden);
    cell.append(&child);
    cell.upcast()
}

fn album_track_area_width(width: i32, metrics: AlbumDetailRowMetrics) -> i32 {
    width
        .saturating_sub(ALBUM_DETAIL_ROW_HORIZONTAL_INSET)
        .saturating_sub(metrics.meta_width)
        .saturating_sub(metrics.spacing * 2)
        .max(1)
}

fn album_track_field_widths(
    fields: &[LibraryField],
    available_width: i32,
) -> Vec<(LibraryField, i32)> {
    let gaps = ALBUM_DETAIL_TRACK_COLUMN_GAP
        * i32::try_from(fields.len().saturating_sub(1)).unwrap_or(i32::MAX);
    let available = available_width
        .saturating_sub(gaps)
        .max(fields.len() as i32);
    let base = fields
        .iter()
        .map(|field| (*field, album_track_column_width(*field)))
        .collect::<Vec<_>>();
    let fixed = base
        .iter()
        .filter(|(field, _)| !matches!(field, LibraryField::Title | LibraryField::TitleMerged))
        .map(|(_, width)| *width)
        .sum::<i32>();
    let title_count = base
        .iter()
        .filter(|(field, _)| matches!(field, LibraryField::Title | LibraryField::TitleMerged))
        .count() as i32;
    if title_count == 0 || fixed.saturating_add(title_count) > available {
        let widths = gtk_widgets::table_sizing::fitted_column_widths(
            &base.iter().map(|(_, width)| *width).collect::<Vec<_>>(),
            available,
        );
        return base
            .into_iter()
            .zip(widths)
            .map(|((field, _), width)| (field, width))
            .collect();
    }
    base.into_iter()
        .map(|(field, width)| {
            if matches!(field, LibraryField::Title | LibraryField::TitleMerged) {
                (field, available.saturating_sub(fixed).max(1))
            } else {
                (field, width.min(available).max(1))
            }
        })
        .collect()
}

fn album_track_column_width(field: LibraryField) -> i32 {
    match field {
        LibraryField::Bitrate
        | LibraryField::SampleRate
        | LibraryField::BitDepth
        | LibraryField::Channels
        | LibraryField::Format
        | LibraryField::FilePath
        | LibraryField::Source => gtk_widgets::library_fields::column_width(field),
        LibraryField::RowIndex => 40,
        LibraryField::TrackNumber => 52,
        LibraryField::DiscNumber => 44,
        LibraryField::Duration => 48,
        LibraryField::Year | LibraryField::Bpm => 52,
        LibraryField::PlayCount => 56,
        LibraryField::UserRating | LibraryField::SongCount | LibraryField::AlbumCount => 64,
        LibraryField::Tools => gtk_widgets::recycled_cells::ROW_ACTIONS_WIDTH,
        LibraryField::Favorite => gtk_widgets::favorites::FAVORITE_COLUMN_WIDTH,
        LibraryField::Image => 56,
        LibraryField::Title | LibraryField::TitleMerged => 320,
        LibraryField::Album
        | LibraryField::Artist
        | LibraryField::AlbumArtist
        | LibraryField::Genre => 160,
        LibraryField::ReleaseDate | LibraryField::DateAdded => 96,
        LibraryField::LastPlayed => 148,
    }
}

fn play_album_track(
    shell: &Rc<CatalogUi>,
    album: Option<library::AlbumKey>,
    track_key: library::TrackKey,
    folder: Option<library::FolderKey>,
) {
    let Some(album) = album else {
        return;
    };
    let queue = shell.playback.clone();
    let settings = shell
        .settings
        .current
        .borrow()
        .library_list(LibraryListKey::AlbumDetailTracks);
    queue.play_selection(
        rufin_core::playback::PlayRequest::ordered(
            library::MemberRead {
                sort: Some(settings.sort_key.catalog_sort()),
                folder,
                descending: settings.descending,
                ..library::MemberRead::new(library::Collection::Album(album))
            }
            .selection(),
            0,
            QueuePlacement::Now,
            false,
        )
        .with_context(format!("album:{album}").into())
        .with_anchor(track_key, None),
    );
}

pub fn album_detail_track_text(track: &TrackView, index: usize, field: LibraryField) -> String {
    if field == LibraryField::RowIndex {
        (index + 1).to_string()
    } else {
        track_field(track, field)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn album_detail_meta_shrinks_with_route() {
        let narrow = album_detail_row_metrics_for_width(320);
        let wide = album_detail_row_metrics_for_width(1_200);
        assert!(narrow.meta_width < wide.meta_width);
        assert!(wide.meta_width <= ALBUM_DETAIL_MAX_COVER);
        assert_eq!(narrow.meta_width, narrow.cover_size);
    }

    #[test]
    fn album_detail_track_width_stays_in_narrow_budget() {
        let metrics = album_detail_row_metrics_for_width(420);
        let available = album_track_area_width(420, metrics);
        let fields = [
            LibraryField::RowIndex,
            LibraryField::Title,
            LibraryField::Artist,
            LibraryField::Duration,
        ];
        let widths = album_track_field_widths(&fields, available);
        let used = widths.iter().map(|(_, width)| *width).sum::<i32>()
            + ALBUM_DETAIL_TRACK_COLUMN_GAP * (fields.len() as i32 - 1);
        assert!(used <= available, "{used} > {available}");
    }

    #[test]
    fn album_detail_width_respects_route_side_insets() {
        let route_width = 760;
        let metrics = album_detail_row_metrics_for_width(route_width);
        let track = album_track_area_width(route_width, metrics);
        let used =
            track + metrics.meta_width + metrics.spacing * 2 + ALBUM_DETAIL_ROW_HORIZONTAL_INSET;
        assert!(used <= route_width);
    }

    #[test]
    fn album_detail_repeated_double_clicks_activate() {
        assert!(!album_detail_play_click(1));
        assert!(album_detail_play_click(2));
        assert!(!album_detail_play_click(3));
        assert!(album_detail_play_click(4));
    }
}
