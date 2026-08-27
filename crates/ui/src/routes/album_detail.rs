use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::ops::Range;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;
use library::{AlbumDetailRouteKey, AlbumKey, AlbumRow, TrackRow};
use localization::track_count_text;
use playback::{PlaybackMedia, QueuePlacement};

use crate::favorites::{
    favorite_button_is_active, favorite_icon_button, set_favorite_button_active,
};
use crate::interactions::install_context_menu_openers;
use crate::shell::Shell;
use crate::shell::cover::{ArtworkTile, THUMB_COVER_SIZE};
use crate::shell::layout::route_content_width;
use crate::{LibraryField, LibraryLayout, LibraryListKey};

use super::collection_context::present_track_context_menu;
use super::library_fields::{opaque_artwork, track_field};
use super::sparse_model::{SparseObjectModel, SparseRouteModel};

const ALBUM_TRACK_HEIGHT: i32 = 36;
const ALBUM_DETAIL_INLINE_TRACK_ROWS: usize = 8;
const ALBUM_DETAIL_TRACK_COLUMN_GAP: i32 = 8;
const ALBUM_DETAIL_TRACK_HEADER_HEIGHT: i32 = 26;
const ALBUM_DETAIL_SEPARATOR_WIDTH: i32 = 1;
const ALBUM_DETAIL_ROW_HORIZONTAL_INSET: i32 = 8;
const ALBUM_DETAIL_MAX_COVER: i32 = 168;
const ALBUM_DETAIL_MIN_COVER: i32 = 102;
const ALBUM_DETAIL_NARROW_WIDTH: i32 = 360;
const ALBUM_DETAIL_META_SPACING: i32 = 6;
const ALBUM_DETAIL_META_LABEL_HEIGHT: i32 = 20;
const ALBUM_DETAIL_MAX_RENDERED_ROWS: usize = 96;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AlbumDetailRowMetrics {
    cover_size: i32,
    meta_width: i32,
    spacing: i32,
}

pub(crate) enum AlbumCollectionOrder {
    Rows(Vec<AlbumKey>),
    Detail(Vec<AlbumDetailRouteKey>),
}

#[derive(Clone, Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "album detail rows are supplied through the bounded sparse-model window"
)]
pub(crate) enum AlbumDetailRouteRow {
    Album(AlbumRow),
    Track(TrackRow),
}

#[derive(Clone)]
pub(crate) struct AlbumCollectionModels {
    rows: Option<Rc<SparseRouteModel<AlbumKey, AlbumRow>>>,
    detail: AlbumDetailModel,
}

#[derive(Clone)]
pub(crate) struct AlbumDetailModel {
    sparse: Option<Rc<SparseRouteModel<AlbumDetailRouteKey, AlbumDetailRouteRow>>>,
}

impl AlbumDetailModel {
    fn empty() -> Self {
        Self { sparse: None }
    }

    pub(crate) fn list_model(&self) -> SparseObjectModel {
        self.sparse
            .as_ref()
            .expect("Album detail model is mounted only in Detail layout")
            .list_model()
    }

    pub(crate) fn n_items(&self) -> u32 {
        self.sparse.as_ref().map_or(0, |sparse| sparse.len() as u32)
    }
}

impl AlbumCollectionModels {
    pub(crate) fn new(
        selected: &crate::runtime::SelectedLibrary,
        order: AlbumCollectionOrder,
        first_rows: Vec<AlbumRow>,
        layout: LibraryLayout,
    ) -> Self {
        let source = selected.source_key;
        let folder = selected.music_folder_key;
        if layout == LibraryLayout::Detail {
            let AlbumCollectionOrder::Detail(order) = order else {
                unreachable!("Album detail layout requires its flattened identity order")
            };
            let database = Arc::clone(&selected.database);
            let load = Arc::new(
                move |keys: Vec<AlbumDetailRouteKey>, cancellation: library::ReadCancellation| {
                    let database = Arc::clone(&database);
                    Box::pin(async move {
                        let (albums, tracks) = database
                            .album_detail_route_rows(source, &keys, folder, &cancellation)
                            .await
                            .map_err(|error| error.to_string())?;
                        let mut albums = albums
                            .into_iter()
                            .map(|row| (row.album_key, row))
                            .collect::<BTreeMap<_, _>>();
                        let mut tracks = tracks
                            .into_iter()
                            .map(|row| (row.track_key, row))
                            .collect::<BTreeMap<_, _>>();
                        keys.into_iter()
                            .map(|key| {
                                if let Some(album) = key.album_key() {
                                    albums.remove(&album).map(AlbumDetailRouteRow::Album)
                                } else if let Some(track) = key.track_key() {
                                    tracks.remove(&track).map(AlbumDetailRouteRow::Track)
                                } else {
                                    None
                                }
                                .ok_or_else(|| "Album detail row is no longer current".to_string())
                            })
                            .collect::<Result<Vec<_>, _>>()
                    })
                        as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
                },
            );
            let sparse = SparseRouteModel::new(order, 24, selected.runtime.clone(), load);
            return Self {
                rows: None,
                detail: AlbumDetailModel {
                    sparse: Some(sparse),
                },
            };
        }
        let AlbumCollectionOrder::Rows(order) = order else {
            unreachable!("Album row/grid layout requires its Album key order")
        };
        let database = Arc::clone(&selected.database);
        let load = Arc::new(
            move |keys: Vec<AlbumKey>, cancellation: library::ReadCancellation| {
                let database = Arc::clone(&database);
                Box::pin(async move {
                    database
                        .album_rows(source, &keys, folder, &cancellation)
                        .await
                        .map_err(|error| error.to_string())
                }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
            },
        );
        let sparse = SparseRouteModel::new(order, 32, selected.runtime.clone(), load);
        sparse.seed_matching(first_rows, |row| row.album_key);
        Self {
            rows: Some(sparse),
            detail: AlbumDetailModel::empty(),
        }
    }

    pub(crate) fn albums(&self) -> SparseObjectModel {
        self.rows
            .as_ref()
            .expect("Album row model is mounted only in Row or Grid layout")
            .list_model()
    }

    pub(crate) fn album_rows(&self) -> Option<Rc<SparseRouteModel<AlbumKey, AlbumRow>>> {
        self.rows.as_ref().map(Rc::clone)
    }

    pub(crate) fn detail(&self) -> AlbumDetailModel {
        self.detail.clone()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.rows
            .as_ref()
            .map_or_else(|| self.detail.n_items() == 0, |rows| rows.len() == 0)
    }

    pub(crate) fn replace_order(&self, order: AlbumCollectionOrder) {
        if let Some(rows) = &self.rows {
            if let AlbumCollectionOrder::Rows(order) = order {
                rows.replace_order(order);
            }
        } else if let Some(detail) = &self.detail.sparse
            && let AlbumCollectionOrder::Detail(order) = order
        {
            detail.replace_order(order);
        }
    }

    pub(crate) fn replace_prepared(
        &self,
        order: AlbumCollectionOrder,
        first_rows: Vec<AlbumRow>,
    ) -> bool {
        if let Some(rows) = &self.rows {
            let AlbumCollectionOrder::Rows(order) = order else {
                return false;
            };
            return rows.replace_prepared(order, first_rows, |row| row.album_key);
        }
        if !first_rows.is_empty() {
            return false;
        }
        self.replace_order(order);
        true
    }

    pub(crate) fn update_favorite(&self, album: AlbumKey, favorite: bool) -> bool {
        if let Some(rows) = &self.rows {
            rows.update_ready(&album, |row| row.favorite = favorite)
        } else if let Some(detail) = &self.detail.sparse {
            let Some(key) = detail
                .order()
                .iter()
                .find(|key| key.album_key() == Some(album))
                .copied()
            else {
                return false;
            };
            detail.update_ready(&key, |row| {
                if let AlbumDetailRouteRow::Album(album) = row {
                    album.favorite = favorite;
                }
            })
        } else {
            false
        }
    }
}

pub(crate) fn album_detail_list(
    shell: &Rc<Shell>,
    model: AlbumDetailModel,
    _: LibraryListKey,
) -> AlbumDetailVirtualList {
    AlbumDetailVirtualList::new(shell, model)
}

#[derive(Clone)]
pub(crate) struct AlbumDetailVirtualList {
    inner: Rc<AlbumDetailVirtualListInner>,
}

struct AlbumDetailVirtualListInner {
    shell: Weak<Shell>,
    model: AlbumDetailModel,
    widget: gtk::Box,
    top_spacer: gtk::Box,
    rows_widget: gtk::Box,
    bottom_spacer: gtk::Box,
    layout: RefCell<AlbumDetailLayout>,
    rendered: RefCell<Option<Range<usize>>>,
    keyboard_position: Cell<Option<usize>>,
    selection: AlbumDetailTrackSelection,
    width: Cell<i32>,
    scheduled: Cell<bool>,
    model_handler: RefCell<Option<(SparseObjectModel, glib::SignalHandlerId)>>,
    adjustment_handlers: RefCell<Option<AlbumDetailAdjustmentHandlers>>,
}

struct AlbumDetailAdjustmentHandlers {
    adjustment: glib::WeakRef<gtk::Adjustment>,
    value_changed: glib::SignalHandlerId,
    changed: glib::SignalHandlerId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AlbumDetailSpan {
    album_position: usize,
    first_track_position: usize,
    first_visual_row: usize,
    track_count: usize,
    top: i32,
    lead_height: i32,
}

impl AlbumDetailSpan {
    fn bottom(self) -> i32 {
        self.top
            .saturating_add(self.lead_height)
            .saturating_add(
                i32::try_from(
                    self.track_count
                        .saturating_sub(ALBUM_DETAIL_INLINE_TRACK_ROWS),
                )
                .unwrap_or(i32::MAX)
                .saturating_mul(ALBUM_TRACK_HEIGHT),
            )
            .saturating_add(if self.track_count > ALBUM_DETAIL_INLINE_TRACK_ROWS {
                16
            } else {
                0
            })
    }

    fn visual_row_count(self) -> usize {
        1_usize.saturating_add(
            self.track_count
                .saturating_sub(ALBUM_DETAIL_INLINE_TRACK_ROWS),
        )
    }
}

#[derive(Clone, Debug)]
enum AlbumDetailVisualItem {
    Lead {
        album_position: usize,
        inline_tracks: Range<usize>,
        last_in_album: bool,
    },
    Track {
        track_position: usize,
        index: usize,
        last_in_album: bool,
    },
}

#[derive(Clone, Debug)]
struct PositionedAlbumDetailItem {
    item: AlbumDetailVisualItem,
    top: i32,
    height: i32,
}

impl PositionedAlbumDetailItem {
    fn bottom(&self) -> i32 {
        self.top.saturating_add(self.height)
    }
}

#[derive(Clone, Debug, Default)]
struct AlbumDetailLayout {
    spans: Vec<AlbumDetailSpan>,
    visual_row_count: usize,
    total_height: i32,
}

impl AlbumDetailLayout {
    fn build(model: &AlbumDetailModel, width: i32) -> Self {
        let order = model
            .sparse
            .as_ref()
            .map(|sparse| sparse.order())
            .unwrap_or_else(|| Arc::new([]));
        Self::from_order(&order, width)
    }

    fn from_order(order: &[AlbumDetailRouteKey], width: i32) -> Self {
        let cover_size = album_detail_row_metrics_for_width(width).cover_size;
        let mut position = 0_usize;
        let mut first_visual_row = 0_usize;
        let mut top = 0_i32;
        let mut spans = Vec::new();
        while position < order.len() {
            let album_position = position;
            let Some(album_key) = order[position].album_key() else {
                position = position.saturating_add(1);
                continue;
            };
            position = position.saturating_add(1);
            let first_track_position = position;
            while position < order.len() && order[position].track_key().is_some() {
                position = position.saturating_add(1);
            }
            let track_count = position.saturating_sub(first_track_position);
            let inline_count = track_count.min(ALBUM_DETAIL_INLINE_TRACK_ROWS);
            let lead_height = album_detail_lead_content_height(
                cover_size,
                3 + usize::from(order[album_position].album_has_genres()),
                inline_count,
            )
            .saturating_add(12)
            .saturating_add(if track_count <= ALBUM_DETAIL_INLINE_TRACK_ROWS {
                16
            } else {
                0
            });
            let span = AlbumDetailSpan {
                album_position,
                first_track_position,
                first_visual_row,
                track_count,
                top,
                lead_height,
            };
            first_visual_row = first_visual_row.saturating_add(span.visual_row_count());
            top = span.bottom();
            spans.push(span);
            let _ = album_key;
        }
        Self {
            spans,
            visual_row_count: first_visual_row,
            total_height: top.max(1),
        }
    }

    fn row(&self, visual_position: usize) -> Option<PositionedAlbumDetailItem> {
        if visual_position >= self.visual_row_count {
            return None;
        }
        let span = *self.spans.get(
            self.spans
                .partition_point(|span| span.first_visual_row <= visual_position)
                .checked_sub(1)?,
        )?;
        let offset = visual_position.saturating_sub(span.first_visual_row);
        if offset == 0 {
            let inline_count = span.track_count.min(ALBUM_DETAIL_INLINE_TRACK_ROWS);
            return Some(PositionedAlbumDetailItem {
                item: AlbumDetailVisualItem::Lead {
                    album_position: span.album_position,
                    inline_tracks: span.first_track_position
                        ..span.first_track_position.saturating_add(inline_count),
                    last_in_album: span.track_count <= ALBUM_DETAIL_INLINE_TRACK_ROWS,
                },
                top: span.top,
                height: span.lead_height,
            });
        }
        let continuation = offset.saturating_sub(1);
        let continuation_count = span
            .track_count
            .saturating_sub(ALBUM_DETAIL_INLINE_TRACK_ROWS);
        if continuation >= continuation_count {
            return None;
        }
        let last_in_album = continuation.saturating_add(1) == continuation_count;
        Some(PositionedAlbumDetailItem {
            item: AlbumDetailVisualItem::Track {
                track_position: span
                    .first_track_position
                    .saturating_add(ALBUM_DETAIL_INLINE_TRACK_ROWS)
                    .saturating_add(continuation),
                index: ALBUM_DETAIL_INLINE_TRACK_ROWS.saturating_add(continuation),
                last_in_album,
            },
            top: span.top.saturating_add(span.lead_height).saturating_add(
                i32::try_from(continuation)
                    .unwrap_or(i32::MAX)
                    .saturating_mul(ALBUM_TRACK_HEIGHT),
            ),
            height: ALBUM_TRACK_HEIGHT.saturating_add(if last_in_album { 16 } else { 0 }),
        })
    }

    fn row_at_height(&self, height: f64) -> Option<usize> {
        if self.visual_row_count == 0 {
            return None;
        }
        let height = height.max(0.0).min(f64::from(self.total_height));
        let span = *self.spans.get(
            self.spans
                .partition_point(|span| f64::from(span.bottom()) <= height)
                .min(self.spans.len().saturating_sub(1)),
        )?;
        let within = (height - f64::from(span.top)).max(0.0);
        if within < f64::from(span.lead_height)
            || span.track_count <= ALBUM_DETAIL_INLINE_TRACK_ROWS
        {
            return Some(span.first_visual_row);
        }
        let continuation = ((within - f64::from(span.lead_height)) / f64::from(ALBUM_TRACK_HEIGHT))
            .floor()
            .max(0.0) as usize;
        Some(
            span.first_visual_row
                + 1
                + continuation.min(
                    span.track_count
                        .saturating_sub(ALBUM_DETAIL_INLINE_TRACK_ROWS + 1),
                ),
        )
    }

    fn visible_range(&self, top: f64, bottom: f64) -> Range<usize> {
        if self.visual_row_count == 0 {
            return 0..0;
        }
        let top = top.max(0.0).min(f64::from(self.total_height));
        let bottom = bottom.max(top).min(f64::from(self.total_height));
        let start = self.row_at_height(top).unwrap_or(0);
        let exclusive_bottom = if bottom > top { bottom - 0.5 } else { bottom };
        let end = self
            .row_at_height(exclusive_bottom)
            .map_or(start, |position| position.saturating_add(1))
            .min(self.visual_row_count)
            .min(start.saturating_add(ALBUM_DETAIL_MAX_RENDERED_ROWS));
        start..end
    }

    fn demand_positions(&self, visual: Range<usize>) -> Vec<usize> {
        let mut positions = Vec::with_capacity(visual.len().saturating_mul(2));
        for visual_position in visual {
            let Some(row) = self.row(visual_position) else {
                continue;
            };
            match row.item {
                AlbumDetailVisualItem::Lead {
                    album_position,
                    inline_tracks,
                    ..
                } => {
                    positions.push(album_position);
                    positions.extend(inline_tracks);
                }
                AlbumDetailVisualItem::Track { track_position, .. } => {
                    positions.push(track_position)
                }
            }
        }
        positions.sort_unstable();
        positions.dedup();
        positions
    }

    fn visual_range_depends_on(&self, visual: &Range<usize>, underlying: &Range<usize>) -> bool {
        self.demand_positions(visual.clone())
            .into_iter()
            .any(|position| underlying.contains(&position))
    }
}

#[derive(Clone, Default)]
struct AlbumDetailTrackSelection {
    selected: Rc<RefCell<Option<library::TrackKey>>>,
    paused: Rc<Cell<bool>>,
    visible: Rc<RefCell<Vec<AlbumDetailTrackBinding>>>,
}

struct AlbumDetailTrackBinding {
    track: library::TrackKey,
    row: glib::WeakRef<gtk::Widget>,
}

impl AlbumDetailTrackSelection {
    fn bind(&self, row: &gtk::Widget, track: library::TrackKey) {
        apply_album_detail_track_selection(
            row,
            self.selected.borrow().as_ref() == Some(&track),
            self.paused.get(),
        );
        self.visible.borrow_mut().push(AlbumDetailTrackBinding {
            track,
            row: row.downgrade(),
        });
    }

    fn update(&self, track: Option<library::TrackKey>, paused: bool) {
        self.selected.replace(track);
        self.paused.set(paused);
        self.visible.borrow_mut().retain(|binding| {
            let Some(row) = binding.row.upgrade() else {
                return false;
            };
            apply_album_detail_track_selection(&row, track == Some(binding.track), paused);
            true
        });
    }

    fn clear_visible(&self) {
        self.visible.borrow_mut().clear();
    }
}

fn apply_album_detail_track_selection(row: &gtk::Widget, selected: bool, paused: bool) {
    if selected {
        row.add_css_class("album-detail-track-selected");
        if paused {
            row.add_css_class("track-row-paused");
        } else {
            row.remove_css_class("track-row-paused");
        }
    } else {
        row.remove_css_class("album-detail-track-selected");
        row.remove_css_class("track-row-paused");
    }
}

impl AlbumDetailVirtualList {
    fn new(shell: &Rc<Shell>, model: AlbumDetailModel) -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.add_css_class("track-table");
        widget.add_css_class("album-detail-list");
        widget.set_hexpand(true);
        widget.set_halign(gtk::Align::Fill);
        widget.set_vexpand(false);
        widget.set_focusable(true);
        let top_spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let rows_widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        rows_widget.set_hexpand(true);
        rows_widget.set_halign(gtk::Align::Fill);
        let bottom_spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.append(&top_spacer);
        widget.append(&rows_widget);
        widget.append(&bottom_spacer);

        let width = Cell::new(route_content_width(shell).max(1));
        let selection = AlbumDetailTrackSelection::default();
        let inner = Rc::new(AlbumDetailVirtualListInner {
            shell: Rc::downgrade(shell),
            model: model.clone(),
            widget,
            top_spacer,
            rows_widget,
            bottom_spacer,
            layout: RefCell::new(AlbumDetailLayout::build(&model, width.get())),
            rendered: RefCell::new(None),
            keyboard_position: Cell::new(None),
            selection: selection.clone(),
            width,
            scheduled: Cell::new(false),
            model_handler: RefCell::new(None),
            adjustment_handlers: RefCell::new(None),
        });
        inner.apply_extent();

        let object_model = model.list_model();
        let weak = Rc::downgrade(&inner);
        let handler = object_model.connect_items_changed(move |_, _, _, _| {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            inner.rebuild_layout();
            AlbumDetailVirtualListInner::queue_render(&inner);
        });
        inner.model_handler.replace(Some((object_model, handler)));
        if let Some(sparse) = model.sparse.as_ref() {
            let weak = Rc::downgrade(&inner);
            sparse.connect_ready_changed(move |position, count| {
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                let changed = position as usize..(position as usize).saturating_add(count as usize);
                if inner.rendered.borrow().as_ref().is_some_and(|rendered| {
                    inner
                        .layout
                        .borrow()
                        .visual_range_depends_on(rendered, &changed)
                }) {
                    inner.rendered.borrow_mut().take();
                    AlbumDetailVirtualListInner::queue_render(&inner);
                }
            });
        }

        let selected_source = shell
            .selected_library()
            .as_deref()
            .map(|selected| selected.source_key);
        let weak = Rc::downgrade(&inner);
        shell.register_current_route_track_selection(Rc::new(move |current| {
            let Some(inner) = weak.upgrade() else {
                return false;
            };
            let current = current.filter(|current| selected_source == Some(current.source_id));
            inner.selection.update(
                current.map(|current| current.track_id),
                current.is_some_and(|current| current.paused),
            );
            true
        }));
        Self { inner }
    }

    pub(crate) fn widget(&self) -> gtk::Widget {
        self.inner.widget.clone().upcast()
    }

    pub(crate) fn attach_scroller(&self, scroller: &gtk::ScrolledWindow) {
        self.inner.disconnect_adjustment();
        let adjustment = scroller.vadjustment();
        let weak = Rc::downgrade(&self.inner);
        let value_changed = adjustment.connect_value_changed(move |_| {
            if let Some(inner) = weak.upgrade() {
                AlbumDetailVirtualListInner::queue_render(&inner);
            }
        });
        let weak = Rc::downgrade(&self.inner);
        let changed = adjustment.connect_changed(move |_| {
            if let Some(inner) = weak.upgrade() {
                AlbumDetailVirtualListInner::queue_render(&inner);
            }
        });
        self.inner
            .adjustment_handlers
            .replace(Some(AlbumDetailAdjustmentHandlers {
                adjustment: adjustment.downgrade(),
                value_changed,
                changed,
            }));
        self.inner.render();
        AlbumDetailVirtualListInner::queue_render(&self.inner);
    }

    pub(crate) fn fit_allocation(&self, width: i32) {
        if width <= 1 {
            return;
        }
        let previous_width = self.inner.width.replace(width);
        if previous_width == width {
            return;
        }
        if album_detail_geometry_changes(previous_width, width) {
            self.inner.rebuild_layout();
        } else {
            self.inner.rendered.borrow_mut().take();
        }
        self.inner.render();
    }

    pub(crate) fn navigate(&self, direction: gtk::DirectionType) -> glib::Propagation {
        let forward = matches!(
            direction,
            gtk::DirectionType::Down | gtk::DirectionType::Right
        );
        let backward = matches!(direction, gtk::DirectionType::Up | gtk::DirectionType::Left);
        if !forward && !backward {
            return glib::Propagation::Stop;
        }
        let layout = self.inner.layout.borrow();
        let mut position = match (self.inner.keyboard_position.get(), forward) {
            (None, true) => 0,
            (None, false) => layout.visual_row_count.saturating_sub(1),
            (Some(position), true) => position.saturating_add(1),
            (Some(position), false) => position.saturating_sub(1),
        };
        loop {
            let Some(row) = layout.row(position) else {
                return glib::Propagation::Stop;
            };
            if matches!(row.item, AlbumDetailVisualItem::Track { .. }) {
                self.inner.keyboard_position.set(Some(position));
                if let Some(adjustment) = self.inner.adjustment() {
                    let maximum =
                        (adjustment.upper() - adjustment.page_size()).max(adjustment.lower());
                    adjustment.set_value(f64::from(row.top).clamp(adjustment.lower(), maximum));
                }
                self.inner.widget.grab_focus();
                return glib::Propagation::Stop;
            }
            if forward {
                position = position.saturating_add(1);
            } else if position == 0 {
                return glib::Propagation::Stop;
            } else {
                position -= 1;
            }
        }
    }
}

fn album_detail_geometry_changes(previous_width: i32, width: i32) -> bool {
    album_detail_row_metrics_for_width(previous_width).cover_size
        != album_detail_row_metrics_for_width(width).cover_size
}

impl AlbumDetailVirtualListInner {
    fn rebuild_layout(&self) {
        self.layout
            .replace(AlbumDetailLayout::build(&self.model, self.width.get()));
        self.rendered.borrow_mut().take();
        self.apply_extent();
    }

    fn apply_extent(&self) {
        self.widget
            .set_height_request(self.layout.borrow().total_height);
    }

    fn adjustment(&self) -> Option<gtk::Adjustment> {
        self.adjustment_handlers
            .borrow()
            .as_ref()
            .and_then(|handlers| handlers.adjustment.upgrade())
    }

    fn queue_render(inner: &Rc<Self>) {
        if inner.scheduled.replace(true) {
            return;
        }
        let weak = Rc::downgrade(inner);
        glib::idle_add_local_once(move || {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            inner.scheduled.set(false);
            inner.render();
        });
    }

    fn render(&self) {
        let Some(adjustment) = self.adjustment() else {
            return;
        };
        let layout = self.layout.borrow();
        let overscan = f64::from(album_detail_virtual_overscan_height());
        let top = (adjustment.value() - overscan).max(0.0);
        let bottom = (adjustment.value() + adjustment.page_size() + overscan)
            .min(f64::from(layout.total_height))
            .max(top);
        let visible = layout.visible_range(top, bottom);
        if !album_detail_window_changed(self.rendered.borrow().as_ref(), &visible) {
            return;
        }
        self.rendered.replace(Some(visible.clone()));
        while let Some(child) = self.rows_widget.first_child() {
            self.rows_widget.remove(&child);
        }
        let top_height = layout.row(visible.start).map_or(0, |row| row.top);
        let bottom_start = visible
            .end
            .checked_sub(1)
            .and_then(|position| layout.row(position))
            .map_or(top_height, |row| row.bottom());
        self.top_spacer.set_height_request(top_height.max(0));
        self.bottom_spacer
            .set_height_request(layout.total_height.saturating_sub(bottom_start).max(0));
        let rows = visible
            .clone()
            .filter_map(|position| layout.row(position).map(|row| (position, row)))
            .collect::<Vec<_>>();
        let demand = layout.demand_positions(visible);
        drop(layout);
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        let Some(sparse) = self.model.sparse.as_ref() else {
            return;
        };
        sparse.demand_positions(demand);
        self.selection.clear_visible();
        for (_, row) in rows {
            let child = match &row.item {
                AlbumDetailVisualItem::Lead {
                    album_position,
                    inline_tracks,
                    last_in_album,
                } => {
                    let album =
                        sparse
                            .peek_ready(*album_position)
                            .and_then(|row| match row.as_ref() {
                                AlbumDetailRouteRow::Album(album) => Some(album.clone()),
                                AlbumDetailRouteRow::Track(_) => None,
                            });
                    let tracks = inline_tracks
                        .clone()
                        .map(|position| {
                            sparse
                                .peek_ready(position)
                                .and_then(|row| match row.as_ref() {
                                    AlbumDetailRouteRow::Track(track) => Some(track.clone()),
                                    AlbumDetailRouteRow::Album(_) => None,
                                })
                        })
                        .collect::<Option<Vec<_>>>();
                    match (album, tracks) {
                        (Some(album), Some(tracks)) => album_lead_row(
                            &shell,
                            &album,
                            &tracks,
                            *last_in_album,
                            self.width.get(),
                            &self.selection,
                        ),
                        _ => album_detail_placeholder(row.height),
                    }
                }
                AlbumDetailVisualItem::Track {
                    track_position,
                    index,
                    last_in_album,
                } => match sparse.peek_ready(*track_position).as_deref() {
                    Some(AlbumDetailRouteRow::Track(track)) => album_continuation_row(
                        &shell,
                        track,
                        *index,
                        *last_in_album,
                        self.width.get(),
                        &self.selection,
                    ),
                    _ => album_detail_placeholder(row.height),
                },
            };
            let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
            host.set_height_request(row.height);
            host.set_hexpand(true);
            host.append(&child);
            self.rows_widget.append(&host);
        }
    }

    fn disconnect_adjustment(&self) {
        let Some(handlers) = self.adjustment_handlers.borrow_mut().take() else {
            return;
        };
        let Some(adjustment) = handlers.adjustment.upgrade() else {
            return;
        };
        adjustment.disconnect(handlers.value_changed);
        adjustment.disconnect(handlers.changed);
    }
}

impl Drop for AlbumDetailVirtualListInner {
    fn drop(&mut self) {
        if let Some((model, handler)) = self.model_handler.get_mut().take() {
            model.disconnect(handler);
        }
        self.disconnect_adjustment();
    }
}

fn album_detail_window_changed(rendered: Option<&Range<usize>>, visible: &Range<usize>) -> bool {
    rendered != Some(visible)
}

fn album_detail_virtual_overscan_height() -> i32 {
    ALBUM_TRACK_HEIGHT * 8
}

fn album_detail_placeholder(height: i32) -> gtk::Widget {
    let placeholder = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    placeholder.add_css_class("track-skeleton");
    placeholder.set_height_request(height.max(1));
    placeholder.set_hexpand(true);
    placeholder.set_halign(gtk::Align::Fill);
    placeholder.set_sensitive(false);
    placeholder.set_can_target(false);
    placeholder.upcast()
}

fn album_detail_row_metrics_for_width(route_width: i32) -> AlbumDetailRowMetrics {
    let row_width = route_width
        .saturating_sub(
            super::route_layout::PRIMARY_ROUTE_MARGIN_START
                + super::route_layout::PRIMARY_ROUTE_MARGIN_END,
        )
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

fn album_lead_row(
    shell: &Rc<Shell>,
    album: &AlbumRow,
    inline_tracks: &[TrackRow],
    last_in_album: bool,
    width: i32,
    selection: &AlbumDetailTrackSelection,
) -> gtk::Widget {
    let metrics = album_detail_row_metrics_for_width(width);
    let fields = shell
        .settings
        .current
        .borrow()
        .library_list(LibraryListKey::AlbumDetailTracks)
        .detail_track_fields;
    let track_width = album_track_area_width(width, metrics);
    let field_widths = album_track_field_widths(&fields, track_width);
    let content_height =
        album_detail_ready_height(width, !album.genres.is_empty(), inline_tracks.len());
    let row = gtk::Box::new(gtk::Orientation::Horizontal, metrics.spacing);
    row.add_css_class("album-detail-row");
    row.set_hexpand(true);
    row.set_halign(gtk::Align::Fill);
    row.set_margin_top(12);
    row.set_margin_bottom(if last_in_album { 16 } else { 0 });
    row.set_margin_start(4);
    row.set_margin_end(4);
    row.set_height_request(content_height);
    row.append(&album_lead_meta(shell, album, metrics));

    let track_area = gtk::Box::new(gtk::Orientation::Vertical, 0);
    track_area.set_hexpand(true);
    track_area.set_width_request(track_width);
    if !inline_tracks.is_empty() {
        row.append(&album_detail_separator(content_height));
        track_area.append(&album_track_header(&field_widths));
        for (index, track) in inline_tracks.iter().enumerate() {
            track_area.append(&album_track_cells(
                shell,
                track,
                index,
                &field_widths,
                selection,
            ));
        }
    }
    row.append(&track_area);
    row.upcast()
}

fn album_detail_lead_content_height(
    cover_size: i32,
    meta_label_count: usize,
    inline_count: usize,
) -> i32 {
    let labels = i32::try_from(meta_label_count).unwrap_or(i32::MAX);
    let meta_height = cover_size
        .saturating_add(
            labels.saturating_mul(ALBUM_DETAIL_META_LABEL_HEIGHT + ALBUM_DETAIL_META_SPACING),
        )
        .saturating_add(ALBUM_DETAIL_META_LABEL_HEIGHT);
    let track_height = if inline_count == 0 {
        0
    } else {
        ALBUM_DETAIL_TRACK_HEADER_HEIGHT.saturating_add(
            i32::try_from(inline_count.min(ALBUM_DETAIL_INLINE_TRACK_ROWS))
                .unwrap_or(i32::MAX)
                .saturating_mul(ALBUM_TRACK_HEIGHT),
        )
    };
    meta_height.max(track_height)
}

fn album_detail_ready_height(width: i32, has_genres: bool, inline_count: usize) -> i32 {
    album_detail_lead_content_height(
        album_detail_row_metrics_for_width(width).cover_size,
        3 + usize::from(has_genres),
        inline_count,
    )
}

fn album_continuation_row(
    shell: &Rc<Shell>,
    track: &TrackRow,
    index: usize,
    last_in_album: bool,
    width: i32,
    selection: &AlbumDetailTrackSelection,
) -> gtk::Widget {
    let metrics = album_detail_row_metrics_for_width(width);
    let fields = shell
        .settings
        .current
        .borrow()
        .library_list(LibraryListKey::AlbumDetailTracks)
        .detail_track_fields;
    let field_widths = album_track_field_widths(&fields, album_track_area_width(width, metrics));
    let row = gtk::Box::new(gtk::Orientation::Horizontal, metrics.spacing);
    row.add_css_class("album-detail-row");
    row.set_hexpand(true);
    row.set_halign(gtk::Align::Fill);
    row.set_margin_bottom(if last_in_album { 16 } else { 0 });
    row.set_margin_start(4);
    row.set_margin_end(4);
    row.set_height_request(ALBUM_TRACK_HEIGHT);
    let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    spacer.set_width_request(metrics.meta_width);
    row.append(&spacer);
    row.append(&album_detail_separator(ALBUM_TRACK_HEIGHT));
    row.append(&album_track_cells(
        shell,
        track,
        index,
        &field_widths,
        selection,
    ));
    row.upcast()
}

fn album_lead_meta(
    shell: &Rc<Shell>,
    album: &AlbumRow,
    metrics: AlbumDetailRowMetrics,
) -> gtk::Widget {
    let meta = gtk::Box::new(gtk::Orientation::Vertical, 6);
    meta.set_width_request(metrics.meta_width);
    meta.set_hexpand(false);
    let cover = super::cards::album_cover_overlay(shell, album, metrics.cover_size);
    meta.append(&cover.widget());
    let title = album_meta_label(&album.title, "track-title", metrics.meta_width);
    meta.append(&title);
    let artist = album_meta_label(&album.display_artist, "muted", metrics.meta_width);
    if let Ok(label) = artist
        .child()
        .and_then(|child| child.downcast::<gtk::Label>().ok())
        .ok_or(())
    {
        super::detail_links::DetailLinkBinding::new(&label, shell)
            .bind(super::detail_links::album_artist_links(album));
    }
    meta.append(&artist);
    let mut facts = Vec::new();
    if let Some(year) = album.year {
        facts.push(year.to_string());
    }
    facts.push(track_count_text(album.track_count.max(0) as u64));
    facts.push(crate::format_duration_units(
        (album.duration_millis.max(0) / 1_000) as u32,
    ));
    meta.append(&album_meta_label(
        &facts.join(" • "),
        "muted",
        metrics.meta_width,
    ));
    if !album.genres.is_empty() {
        meta.append(&album_meta_label(
            &album
                .genres
                .iter()
                .map(|genre| genre.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            "muted",
            metrics.meta_width,
        ));
    }
    meta.upcast()
}

fn album_meta_label(text: &str, class: &str, width: i32) -> gtk::ScrolledWindow {
    let label = gtk::Label::new(Some(text));
    label.add_css_class(class);
    label.set_xalign(0.0);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_single_line_mode(true);
    let clip = gtk::ScrolledWindow::new();
    clip.add_css_class("card-label-clip");
    clip.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Never);
    clip.set_overflow(gtk::Overflow::Hidden);
    clip.set_width_request(width);
    clip.set_min_content_width(width);
    clip.set_max_content_width(width);
    clip.set_child(Some(&label));
    clip
}

fn album_detail_separator(height: i32) -> gtk::Widget {
    let separator = gtk::Separator::new(gtk::Orientation::Vertical);
    separator.add_css_class("album-detail-separator");
    separator.set_width_request(ALBUM_DETAIL_SEPARATOR_WIDTH);
    separator.set_height_request(height.max(1));
    separator.set_valign(gtk::Align::Fill);
    separator.upcast()
}

fn album_track_header(field_widths: &[(LibraryField, i32)]) -> gtk::Widget {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, ALBUM_DETAIL_TRACK_COLUMN_GAP);
    row.add_css_class("album-detail-track-cells");
    row.set_height_request(ALBUM_DETAIL_TRACK_HEADER_HEIGHT);
    for (field, width) in field_widths {
        let label = if *field == LibraryField::Duration {
            let image = gtk::Image::from_icon_name("rufin-preferences-system-time-symbolic");
            image.add_css_class("muted");
            image.set_tooltip_text(Some(&localization::tr("Duration")));
            image.upcast()
        } else {
            let label = album_track_label(&localization::tr(field.title()), *field, *width);
            label.add_css_class("muted");
            label.upcast()
        };
        row.append(&fixed_album_track_cell(
            *width,
            ALBUM_DETAIL_TRACK_HEADER_HEIGHT,
            label,
        ));
    }
    row.upcast()
}

fn album_track_cells(
    shell: &Rc<Shell>,
    track: &TrackRow,
    index: usize,
    field_widths: &[(LibraryField, i32)],
    selection: &AlbumDetailTrackSelection,
) -> gtk::Widget {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, ALBUM_DETAIL_TRACK_COLUMN_GAP);
    row.add_css_class("album-detail-track-row");
    row.set_height_request(ALBUM_TRACK_HEIGHT);
    row.set_hexpand(true);
    selection.bind(row.upcast_ref(), track.track_key);
    for (field, width) in field_widths {
        row.append(&album_track_cell(shell, track, index, *field, *width));
    }
    let menu_shell = Rc::clone(shell);
    let menu_track = track.clone();
    install_context_menu_openers(
        &row,
        Rc::new(move |target, position| {
            present_track_context_menu(target, &menu_shell, menu_track.clone(), position);
        }),
    );
    let click_shell = Rc::clone(shell);
    let click_track = track.clone();
    let click = gtk::GestureClick::new();
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    click.set_button(1);
    click.connect_pressed(move |gesture, presses, _, _| {
        if album_detail_play_click(presses) {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            play_album_track(&click_shell, click_track.clone());
        }
    });
    row.add_controller(click);
    row.upcast()
}

fn album_detail_play_click(presses: i32) -> bool {
    presses >= 2 && presses % 2 == 0
}

fn album_track_cell(
    shell: &Rc<Shell>,
    track: &TrackRow,
    index: usize,
    field: LibraryField,
    width: i32,
) -> gtk::Widget {
    match field {
        LibraryField::Favorite => {
            let button = favorite_icon_button("Favorite track");
            set_favorite_button_active(&button, track.favorite);
            shell.register_favorite_button(
                crate::favorites::track_favorite_key(&track.track_key),
                &button,
            );
            let favorite_shell = Rc::clone(shell);
            let key = track.track_key;
            button.connect_clicked(move |button| {
                favorite_shell.set_favorite_with_feedback(
                    library::FavoriteTarget::Track(key),
                    !favorite_button_is_active(button),
                    Some(button),
                );
            });
            fixed_album_track_cell(width, ALBUM_TRACK_HEIGHT, button.upcast())
        }
        LibraryField::Image => {
            let cover = ArtworkTile::new(32);
            shell.bind_artwork_tile(
                &cover,
                opaque_artwork(track.artwork_binding.as_deref()),
                32,
                THUMB_COVER_SIZE,
            );
            fixed_album_track_cell(width, ALBUM_TRACK_HEIGHT, cover.widget())
        }
        LibraryField::RowIndex => fixed_album_track_cell(
            width,
            ALBUM_TRACK_HEIGHT,
            super::columns::track_row_index_cell(&(index + 1).to_string()).upcast(),
        ),
        _ => fixed_album_track_cell(
            width,
            ALBUM_TRACK_HEIGHT,
            album_track_label(&album_detail_track_text(track, index, field), field, width).upcast(),
        ),
    }
}

fn album_track_label(text: &str, field: LibraryField, width: i32) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
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
    cell.set_size_request(width.max(1), height.max(1));
    cell.set_overflow(gtk::Overflow::Hidden);
    cell.append(&child);
    cell.upcast()
}

fn album_track_area_width(width: i32, metrics: AlbumDetailRowMetrics) -> i32 {
    width
        .saturating_sub(
            super::route_layout::PRIMARY_ROUTE_MARGIN_START
                + super::route_layout::PRIMARY_ROUTE_MARGIN_END
                + ALBUM_DETAIL_ROW_HORIZONTAL_INSET,
        )
        .saturating_sub(metrics.meta_width)
        .saturating_sub(ALBUM_DETAIL_SEPARATOR_WIDTH)
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
        let widths = super::table_sizing::fitted_column_widths(
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
        LibraryField::RowIndex => 40,
        LibraryField::TrackNumber => 52,
        LibraryField::DiscNumber => 44,
        LibraryField::Duration => 48,
        LibraryField::Year | LibraryField::Bpm => 52,
        LibraryField::PlayCount => 56,
        LibraryField::UserRating | LibraryField::SongCount | LibraryField::AlbumCount => 64,
        LibraryField::Favorite => 40,
        LibraryField::Image => 56,
        LibraryField::Title | LibraryField::TitleMerged => 320,
        LibraryField::Album
        | LibraryField::Artist
        | LibraryField::AlbumArtist
        | LibraryField::Genre => 160,
        LibraryField::ReleaseDate | LibraryField::DateAdded | LibraryField::LastPlayed => 96,
    }
}

fn play_album_track(shell: &Rc<Shell>, anchor: TrackRow) {
    let Some(selected) = shell.selected_library().as_deref().cloned() else {
        return;
    };
    let Some(album) = anchor.album_key else {
        return;
    };
    let database = Arc::clone(&selected.database);
    let queue = shell.products.playback.queue.clone();
    let settings = shell
        .settings
        .current
        .borrow()
        .library_list(LibraryListKey::AlbumDetailTracks);
    selected.runtime.spawn(async move {
        let cancellation = library::ReadCancellation::new();
        let Ok(order) = database
            .album_track_order(
                selected.source_key,
                album,
                selected.music_folder_key,
                "",
                settings.sort_key.track_sort(),
                settings.descending,
                &cancellation,
            )
            .await
        else {
            return;
        };
        let Some(position) = order.iter().position(|key| *key == anchor.track_key) else {
            return;
        };
        if let Some(request) = playback::LoadedPlayRequest::context(
            selected.source_key,
            selected.source_session_epoch,
            order.into(),
            PlaybackMedia::from(anchor),
            position,
            QueuePlacement::Now,
            format!("album:{album}"),
            false,
        ) {
            queue.play_loaded(request);
        }
    });
}

pub(crate) fn album_detail_track_text(
    track: &TrackRow,
    index: usize,
    field: LibraryField,
) -> String {
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
        let used = track
            + metrics.meta_width
            + ALBUM_DETAIL_SEPARATOR_WIDTH
            + metrics.spacing * 2
            + super::super::route_layout::PRIMARY_ROUTE_MARGIN_START
            + super::super::route_layout::PRIMARY_ROUTE_MARGIN_END
            + ALBUM_DETAIL_ROW_HORIZONTAL_INSET;
        assert!(used <= route_width);
    }

    #[test]
    fn album_detail_repeated_double_clicks_activate() {
        assert!(!album_detail_play_click(1));
        assert!(album_detail_play_click(2));
        assert!(!album_detail_play_click(3));
        assert!(album_detail_play_click(4));
    }

    fn two_album_order(first_tracks: i64, second_tracks: i64) -> Vec<AlbumDetailRouteKey> {
        let mut order = vec![AlbumDetailRouteKey::album(AlbumKey::from_raw(1)).unwrap()];
        order.extend(
            (1..=first_tracks)
                .filter_map(|key| AlbumDetailRouteKey::track(library::TrackKey::from_raw(key))),
        );
        order.push(AlbumDetailRouteKey::album(AlbumKey::from_raw(2)).unwrap());
        order.extend(
            (first_tracks + 1..=first_tracks + second_tracks)
                .filter_map(|key| AlbumDetailRouteKey::track(library::TrackKey::from_raw(key))),
        );
        order
    }

    #[test]
    fn album_detail_layout_keeps_full_order_and_one_compact_span_per_album() {
        let order = two_album_order(240, 3);
        let layout = AlbumDetailLayout::from_order(&order, 720);
        assert_eq!(order.len(), 245);
        assert_eq!(layout.spans.len(), 2);
        assert_eq!(layout.spans[0].album_position, 0);
        assert_eq!(layout.spans[0].first_track_position, 1);
        assert_eq!(layout.spans[0].track_count, 240);
        assert_eq!(layout.spans[1].album_position, 241);
        assert_eq!(layout.spans[1].track_count, 3);
    }

    #[test]
    fn album_detail_ready_and_placeholder_extent_share_the_checkpoint_formula() {
        let width = 720;
        let cover = album_detail_row_metrics_for_width(width).cover_size;
        let expected_meta = cover
            + 4 * (ALBUM_DETAIL_META_LABEL_HEIGHT + ALBUM_DETAIL_META_SPACING)
            + ALBUM_DETAIL_META_LABEL_HEIGHT;
        let expected_tracks = ALBUM_DETAIL_TRACK_HEADER_HEIGHT + 3 * ALBUM_TRACK_HEIGHT;
        let ready_with_genres = album_detail_ready_height(width, true, 3);
        assert_eq!(ready_with_genres, expected_meta.max(expected_tracks));
        assert_eq!(
            album_detail_ready_height(width, false, 3),
            album_detail_ready_height(width, false, 3)
        );
    }

    #[test]
    fn album_detail_layout_preserves_exact_extent_with_arithmetic_continuations() {
        let order = two_album_order(10, 2);
        let width = 720;
        let layout = AlbumDetailLayout::from_order(&order, width);
        assert_eq!(layout.spans.len(), 2);
        assert_eq!(layout.visual_row_count, 4);
        assert_eq!(layout.total_height, layout.spans[1].bottom());
        for pair in layout.spans.windows(2) {
            assert_eq!(pair[0].bottom(), pair[1].top);
        }
        let last_continuation = layout.row(2).expect("last continuation");
        assert!(matches!(
            last_continuation.item,
            AlbumDetailVisualItem::Track {
                track_position: 10,
                index: 9,
                last_in_album: true,
            }
        ));
        assert_eq!(last_continuation.height, ALBUM_TRACK_HEIGHT + 16);
    }

    #[test]
    fn album_detail_visible_range_is_bounded_and_tracks_scroll_height() {
        let order = two_album_order(256, 3);
        let layout = AlbumDetailLayout::from_order(&order, 720);
        let range = layout.visible_range(100.0, 100_000.0);
        assert_eq!(range.len(), ALBUM_DETAIL_MAX_RENDERED_ROWS);
        assert!(range.end <= layout.visual_row_count);

        let exact = layout.visible_range(
            f64::from(layout.row(40).unwrap().top),
            f64::from(layout.row(43).unwrap().bottom()),
        );
        assert_eq!(exact, 40..44);
    }

    #[test]
    fn album_detail_two_window_demand_is_exact_and_each_page_is_at_most_64() {
        let order = two_album_order(130, 1);
        let layout = AlbumDetailLayout::from_order(&order, 720);
        let demanded = layout.demand_positions(54..66);
        let windows = demanded
            .iter()
            .map(|position| position / 64 * 64)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(windows, [0, 64].into_iter().collect());
        assert!(
            windows
                .iter()
                .all(|start| (*start..(*start + 64).min(order.len())).len() <= 64)
        );
        assert!(demanded.windows(2).all(|pair| pair[1] > pair[0]));
    }

    #[test]
    fn album_detail_settled_window_does_not_schedule_an_identical_render() {
        let rendered = 40..64;
        assert!(!album_detail_window_changed(Some(&rendered), &(40..64)));
        assert!(album_detail_window_changed(Some(&rendered), &(41..65)));
        assert!(album_detail_window_changed(None, &(40..64)));
    }

    #[test]
    fn album_detail_layout_uses_current_width_and_same_width_is_settled() {
        let order = two_album_order(8, 2);
        let narrow = AlbumDetailLayout::from_order(&order, 420);
        let wide = AlbumDetailLayout::from_order(&order, 1_200);
        assert_ne!(narrow.total_height, wide.total_height);
        assert!(!album_detail_window_changed(Some(&(0..2)), &(0..2)));
        assert!(!album_detail_geometry_changes(720, 800));
        assert!(album_detail_geometry_changes(420, 1_200));
    }
}
