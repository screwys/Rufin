use std::collections::BTreeMap;
use std::rc::Rc;
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
use crate::{LibraryField, LibraryLayout, LibraryListKey};

use super::collection_context::present_track_context_menu;
use super::library_fields::{opaque_artwork, track_field};
use super::sparse_model::{SparseItem, SparseObjectModel, SparseRouteModel};

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
pub(crate) enum AlbumDetailRouteRow {
    Album {
        album: AlbumRow,
        inline_tracks: Vec<TrackRow>,
        last_in_album: bool,
    },
    Track {
        track: TrackRow,
        index: usize,
        last_in_album: bool,
    },
}

#[derive(Clone)]
pub(crate) struct AlbumCollectionModels {
    albums: SparseObjectModel,
    rows: Option<Rc<SparseRouteModel<AlbumKey, AlbumRow>>>,
    detail: AlbumDetailModel,
}

#[derive(Clone)]
pub(crate) struct AlbumDetailModel {
    model: SparseObjectModel,
    sparse: Option<Rc<SparseRouteModel<AlbumDetailRouteKey, AlbumDetailRouteRow>>>,
    order_layout: Option<Arc<std::sync::RwLock<AlbumDetailOrderLayout>>>,
}

#[derive(Default)]
struct AlbumDetailOrderLayout {
    presentation: Vec<AlbumDetailRouteKey>,
    inline_tracks: BTreeMap<AlbumKey, Vec<library::TrackKey>>,
    album_has_continuation: BTreeMap<AlbumKey, bool>,
    track_indexes: BTreeMap<library::TrackKey, (usize, bool)>,
    track_albums: BTreeMap<library::TrackKey, AlbumKey>,
}

fn collapse_album_detail_order(order: &[AlbumDetailRouteKey]) -> AlbumDetailOrderLayout {
    let mut layout = AlbumDetailOrderLayout {
        presentation: Vec::with_capacity(order.len()),
        ..AlbumDetailOrderLayout::default()
    };
    let mut current_album = None;
    let mut current_index = 0_usize;
    for (position, key) in order.iter().copied().enumerate() {
        if let Some(album) = key.album_key() {
            current_album = Some(album);
            current_index = 0;
            layout.presentation.push(key);
            continue;
        }
        let Some(track) = key.track_key() else {
            continue;
        };
        if let Some(album) = current_album {
            layout.track_albums.insert(track, album);
        }
        let last_in_album = order
            .get(position + 1)
            .is_none_or(|next| next.album_key().is_some());
        if current_index < ALBUM_DETAIL_INLINE_TRACK_ROWS {
            if let Some(album) = current_album {
                layout.inline_tracks.entry(album).or_default().push(track);
            }
        } else {
            layout.presentation.push(key);
            layout
                .track_indexes
                .insert(track, (current_index, last_in_album));
            if let Some(album) = current_album {
                layout.album_has_continuation.insert(album, true);
            }
        }
        current_index = current_index.saturating_add(1);
    }
    layout
}

impl AlbumDetailModel {
    fn empty() -> Self {
        Self {
            model: SparseObjectModel::new::<AlbumDetailRouteKey, AlbumDetailRouteRow>(
                Vec::new(),
                0,
            ),
            sparse: None,
            order_layout: None,
        }
    }

    pub(crate) fn list_model(&self) -> SparseObjectModel {
        self.model.clone()
    }

    pub(crate) fn n_items(&self) -> u32 {
        self.model.n_items()
    }
}

impl AlbumCollectionModels {
    pub(crate) fn new(
        selected: &crate::runtime::SelectedLibrary,
        order: AlbumCollectionOrder,
        layout: LibraryLayout,
    ) -> Self {
        let source = selected.source_key;
        let folder = selected.music_folder_key;
        if layout == LibraryLayout::Detail {
            let AlbumCollectionOrder::Detail(order) = order else {
                unreachable!("Album detail layout requires its flattened identity order")
            };
            let order_layout =
                Arc::new(std::sync::RwLock::new(collapse_album_detail_order(&order)));
            let presentation_order = order_layout
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .presentation
                .clone();
            let database = Arc::clone(&selected.database);
            let load_layout = Arc::clone(&order_layout);
            let load = Arc::new(
                move |keys: Vec<AlbumDetailRouteKey>, cancellation: library::ReadCancellation| {
                    let database = Arc::clone(&database);
                    let order_layout = Arc::clone(&load_layout);
                    Box::pin(async move {
                        let (inline_tracks, album_has_continuation, track_indexes) = {
                            let layout = order_layout
                                .read()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            (
                                layout.inline_tracks.clone(),
                                layout.album_has_continuation.clone(),
                                layout.track_indexes.clone(),
                            )
                        };
                        let mut load_keys = keys.clone();
                        for album in keys.iter().filter_map(|key| key.album_key()) {
                            load_keys.extend(
                                inline_tracks
                                    .get(&album)
                                    .into_iter()
                                    .flatten()
                                    .filter_map(|track| AlbumDetailRouteKey::track(*track)),
                            );
                        }
                        let (albums, tracks) = database
                            .album_detail_route_rows(source, &load_keys, folder, &cancellation)
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
                                    albums.remove(&album).map(|album_row| {
                                        let inline_tracks = inline_tracks
                                            .get(&album)
                                            .into_iter()
                                            .flatten()
                                            .filter_map(|track| tracks.remove(track))
                                            .collect::<Vec<_>>();
                                        AlbumDetailRouteRow::Album {
                                            album: album_row,
                                            last_in_album: !album_has_continuation
                                                .get(&album)
                                                .copied()
                                                .unwrap_or(false),
                                            inline_tracks,
                                        }
                                    })
                                } else if let Some(track) = key.track_key() {
                                    tracks.remove(&track).map(|row| {
                                        let (index, last_in_album) =
                                            track_indexes.get(&track).copied().unwrap_or_default();
                                        AlbumDetailRouteRow::Track {
                                            track: row,
                                            index,
                                            last_in_album,
                                        }
                                    })
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
            let sparse =
                SparseRouteModel::new(presentation_order, 24, selected.runtime.clone(), load);
            return Self {
                albums: SparseObjectModel::new::<AlbumKey, AlbumRow>(Vec::new(), 0),
                rows: None,
                detail: AlbumDetailModel {
                    model: sparse.list_model(),
                    sparse: Some(sparse),
                    order_layout: Some(order_layout),
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
        Self {
            albums: sparse.list_model(),
            rows: Some(sparse),
            detail: AlbumDetailModel::empty(),
        }
    }

    pub(crate) fn albums(&self) -> SparseObjectModel {
        self.albums.clone()
    }

    pub(crate) fn detail(&self) -> AlbumDetailModel {
        self.detail.clone()
    }

    pub(crate) fn replace_order(&self, order: AlbumCollectionOrder) {
        if let Some(rows) = &self.rows {
            if let AlbumCollectionOrder::Rows(order) = order {
                rows.replace_order(order);
            }
        } else if let Some(detail) = &self.detail.sparse
            && let AlbumCollectionOrder::Detail(order) = order
        {
            let collapsed = collapse_album_detail_order(&order);
            let presentation = collapsed.presentation.clone();
            if let Some(layout) = &self.detail.order_layout {
                *layout
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = collapsed;
            }
            detail.replace_order(presentation);
        }
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
                if let AlbumDetailRouteRow::Album { album, .. } = row {
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
    list: gtk::ListView,
    selection: gtk::SingleSelection,
    sparse: Option<Rc<SparseRouteModel<AlbumDetailRouteKey, AlbumDetailRouteRow>>>,
    width: Rc<std::cell::Cell<i32>>,
}

impl AlbumDetailVirtualList {
    fn new(shell: &Rc<Shell>, model: AlbumDetailModel) -> Self {
        let selection = gtk::SingleSelection::new(Some(model.list_model()));
        selection.set_autoselect(false);
        selection.set_can_unselect(true);
        let sparse = model.sparse.clone();
        let current_selection = selection.clone();
        let current_sparse = sparse.clone();
        let current_layout = model.order_layout.clone();
        let source = shell
            .selected_library()
            .as_deref()
            .map(|selected| selected.source_key);
        shell.register_current_route_track_selection(Rc::new(move |current| {
            let track = current
                .filter(|current| source == Some(current.source_id))
                .map(|current| current.track_id);
            let position = track.and_then(AlbumDetailRouteKey::track).and_then(|key| {
                current_sparse.as_ref().and_then(|sparse| {
                    sparse
                        .order()
                        .iter()
                        .position(|candidate| *candidate == key)
                })
            });
            if position.is_none()
                && let (Some(track), Some(layout), Some(sparse)) =
                    (track, current_layout.as_ref(), current_sparse.as_ref())
            {
                let layout = layout
                    .read()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(album) = layout.track_albums.get(&track)
                    && let Some(album_position) = sparse
                        .order()
                        .iter()
                        .position(|candidate| candidate.album_key() == Some(*album))
                {
                    sparse
                        .list_model()
                        .items_changed(album_position as u32, 1, 1);
                }
            }
            let position = position
                .and_then(|position| u32::try_from(position).ok())
                .unwrap_or(gtk::INVALID_LIST_POSITION);
            current_selection.set_selected(position);
            true
        }));
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            item.set_child(Some(&gtk::Box::new(gtk::Orientation::Vertical, 0)));
        });
        let bind_shell = Rc::clone(shell);
        let width = Rc::new(std::cell::Cell::new(1));
        let bind_width = Rc::clone(&width);
        let placeholder_layout = model.order_layout.clone();
        factory.connect_bind(move |_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let Some(host) = item
                .child()
                .and_then(|child| child.downcast::<gtk::Box>().ok())
            else {
                return;
            };
            while let Some(child) = host.first_child() {
                host.remove(&child);
            }
            let Some(boxed) = item
                .item()
                .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
            else {
                return;
            };
            let row = boxed.borrow::<SparseItem<AlbumDetailRouteKey, AlbumDetailRouteRow>>();
            let child = match &*row {
                SparseItem::Placeholder(key) => {
                    album_detail_placeholder(*key, bind_width.get(), placeholder_layout.as_ref())
                }
                SparseItem::Ready(row) => match row.as_ref() {
                    AlbumDetailRouteRow::Album {
                        album,
                        inline_tracks,
                        last_in_album,
                    } => album_lead_row(
                        &bind_shell,
                        album,
                        inline_tracks,
                        *last_in_album,
                        bind_width.get(),
                    ),
                    AlbumDetailRouteRow::Track {
                        track,
                        index,
                        last_in_album,
                    } => album_continuation_row(
                        &bind_shell,
                        track,
                        *index,
                        *last_in_album,
                        bind_width.get(),
                    ),
                },
            };
            host.append(&child);
        });
        factory.connect_unbind(|_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            if let Some(host) = item
                .child()
                .and_then(|child| child.downcast::<gtk::Box>().ok())
            {
                while let Some(child) = host.first_child() {
                    host.remove(&child);
                }
            }
        });
        let list = gtk::ListView::new(Some(selection.clone()), Some(factory));
        list.add_css_class("album-detail-list");
        list.set_hexpand(true);
        list.set_vexpand(true);
        Self {
            list,
            selection,
            sparse,
            width,
        }
    }

    pub(crate) fn widget(&self) -> gtk::Widget {
        self.list.clone().upcast()
    }

    pub(crate) fn attach_scroller(&self, scroller: &gtk::ScrolledWindow) {
        let list = self.list.downgrade();
        scroller.vadjustment().connect_value_changed(move |_| {
            if let Some(list) = list.upgrade() {
                list.queue_draw();
            }
        });
    }

    pub(crate) fn fit_allocation(&self, width: i32) {
        if width <= 1 || self.width.replace(width) == width {
            return;
        }
        if let Some(model) = self.list.model() {
            let count = model.n_items();
            if count != 0 {
                model.items_changed(0, count, count);
            }
        }
        self.list.queue_allocate();
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
        let current = self.selection.selected();
        let order = self
            .sparse
            .as_ref()
            .map(|sparse| sparse.order())
            .unwrap_or_else(|| Arc::new([]));
        let mut position = if current == gtk::INVALID_LIST_POSITION {
            if forward {
                0
            } else {
                order.len().saturating_sub(1)
            }
        } else if forward {
            current as usize + 1
        } else {
            (current as usize).saturating_sub(1)
        };
        loop {
            let Some(key) = order.get(position) else {
                return glib::Propagation::Stop;
            };
            if key.track_key().is_some() {
                self.selection.set_selected(position as u32);
                self.list.grab_focus();
                return glib::Propagation::Stop;
            }
            if forward {
                position += 1;
            } else if position == 0 {
                return glib::Propagation::Stop;
            } else {
                position -= 1;
            }
        }
    }
}

fn album_detail_placeholder(
    key: AlbumDetailRouteKey,
    width: i32,
    layout: Option<&Arc<std::sync::RwLock<AlbumDetailOrderLayout>>>,
) -> gtk::Widget {
    let placeholder = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    if let Some(album) = key.album_key() {
        let inline_count = layout
            .map(|layout| {
                layout
                    .read()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .inline_tracks
                    .get(&album)
                    .map_or(0, Vec::len)
            })
            .unwrap_or(0);
        placeholder.set_height_request(album_detail_placeholder_height(
            width,
            key.album_has_genres(),
            inline_count,
        ));
    } else {
        placeholder.set_height_request(ALBUM_TRACK_HEIGHT);
    }
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
            track_area.append(&album_track_cells(shell, track, index, &field_widths));
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

fn album_detail_placeholder_height(width: i32, has_genres: bool, inline_count: usize) -> i32 {
    album_detail_lead_content_height(
        album_detail_row_metrics_for_width(width).cover_size,
        3 + usize::from(has_genres),
        inline_count,
    )
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
    row.append(&album_track_cells(shell, track, index, &field_widths));
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
) -> gtk::Widget {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, ALBUM_DETAIL_TRACK_COLUMN_GAP);
    row.add_css_class("album-detail-track-row");
    row.set_height_request(ALBUM_TRACK_HEIGHT);
    row.set_hexpand(true);
    if shell
        .selected_playback()
        .as_deref()
        .and_then(|playback| playback.transport.current.as_ref())
        .and_then(|media| media.track.track_key)
        == Some(track.track_key)
    {
        row.add_css_class("album-detail-track-selected");
    }
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

    #[test]
    fn album_detail_layout_retains_one_lead_row_per_album_after_refresh() {
        let mut order = vec![AlbumDetailRouteKey::album(AlbumKey::from_raw(1)).unwrap()];
        order.extend(
            (1..=10).filter_map(|key| AlbumDetailRouteKey::track(library::TrackKey::from_raw(key))),
        );
        order.push(AlbumDetailRouteKey::album(AlbumKey::from_raw(2)).unwrap());
        order.extend(
            (11..=13)
                .filter_map(|key| AlbumDetailRouteKey::track(library::TrackKey::from_raw(key))),
        );
        let initial = collapse_album_detail_order(&order);
        let refreshed = collapse_album_detail_order(&order);
        assert_eq!(
            initial
                .presentation
                .iter()
                .filter(|key| key.album_key().is_some())
                .count(),
            2
        );
        assert_eq!(initial.presentation, refreshed.presentation);
        assert_eq!(initial.inline_tracks, refreshed.inline_tracks);
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
        let placeholder_with_genres = album_detail_placeholder_height(width, true, 3);
        assert_eq!(ready_with_genres, expected_meta.max(expected_tracks));
        assert_eq!(placeholder_with_genres, ready_with_genres);
        assert_eq!(
            album_detail_placeholder_height(width, false, 3),
            album_detail_ready_height(width, false, 3)
        );
    }
}
