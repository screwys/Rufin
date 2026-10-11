use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;
use library::{AlbumKey, AlbumRow, TrackRow};
use localization::track_count_text;
use playback::QueuePlacement;

use crate::CatalogUi;
use crate::{LibraryField, LibraryLayout, LibraryListKey};
use gtk_widgets::artwork::{ArtworkTile, THUMB_COVER_SIZE};
use gtk_widgets::favorites::{
    favorite_button_is_active, row_favorite_icon_button, set_favorite_button_active,
};
use gtk_widgets::interactions::install_context_menu_openers;

use super::collection_context::present_track_context_menu;
use gtk_widgets::library_fields::{opaque_artwork, track_field};
use gtk_widgets::sparse_model::{SparseObjectModel, SparseRouteModel, SparseSource};

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

use library::AlbumDetailRow;

type AlbumDetailKey = (bool, i64);

fn detail_key(row: &AlbumDetailRow) -> AlbumDetailKey {
    match row {
        AlbumDetailRow::Album { album, .. } => (true, album.album_key.raw()),
        AlbumDetailRow::Track { track, .. } => (false, track.track_key.raw()),
    }
}

#[derive(Clone)]
pub struct AlbumCollectionModels {
    pub rows: Option<Rc<SparseRouteModel<AlbumKey, AlbumRow>>>,
    detail: AlbumDetailModel,
}

#[derive(Clone)]
pub struct AlbumDetailModel {
    sparse: Option<Rc<SparseRouteModel<AlbumDetailKey, AlbumDetailRow>>>,
}

impl AlbumDetailModel {
    fn empty() -> Self {
        Self { sparse: None }
    }

    pub fn list_model(&self) -> SparseObjectModel {
        self.sparse
            .as_ref()
            .expect("Album detail model is mounted only in Detail layout")
            .list_model()
    }

    pub fn n_items(&self) -> u32 {
        self.sparse.as_ref().map_or(0, |sparse| sparse.len() as u32)
    }
}

impl AlbumCollectionModels {
    pub fn new(
        selected: &rufin_core::runtime::SelectedLibrary,
        count: usize,
        first_row_position: usize,
        first_rows: Vec<AlbumRow>,
        first_detail_rows: Vec<AlbumDetailRow>,
        layout: LibraryLayout,
        favorites_only: bool,
        request: Rc<RefCell<crate::routes::CollectionReadRequest>>,
    ) -> Self {
        let source = selected.source_key;
        let folder = selected.music_folder_key;
        if layout == LibraryLayout::Detail {
            let database = Arc::clone(&selected.database);
            let load = Rc::new(
                move |_: Vec<AlbumDetailKey>,
                      range: std::ops::Range<usize>,
                      cancellation: library::ReadCancellation| {
                    let database = Arc::clone(&database);
                    let request = request.borrow().clone();
                    Box::pin(async move {
                        database
                            .album_detail_page(
                                source,
                                folder,
                                favorites_only,
                                &request.query,
                                request.settings.sort_key.album_sort(),
                                request.settings.descending,
                                ALBUM_DETAIL_INLINE_TRACK_ROWS,
                                range.start,
                                range.len(),
                                &cancellation,
                            )
                            .await
                            .map_err(|error| error.to_string())
                    })
                        as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
                },
            );
            let sparse = SparseRouteModel::new(
                SparseSource::Query { count },
                24,
                selected.runtime.clone(),
                load,
            );
            sparse.seed_matching_at(first_row_position, first_detail_rows, detail_key);
            return Self {
                rows: None,
                detail: AlbumDetailModel {
                    sparse: Some(sparse),
                },
            };
        }
        let database = Arc::clone(&selected.database);
        let load: gtk_widgets::media_drag::CollectionRowsLoad<
            crate::routes::CollectionReadRequest,
            AlbumRow,
        > = Arc::new(
            move |request: crate::routes::CollectionReadRequest,
                  range: std::ops::Range<usize>,
                  cancellation: library::ReadCancellation| {
                let database = Arc::clone(&database);
                Box::pin(async move {
                    database
                        .album_page(
                            source,
                            folder,
                            favorites_only,
                            &request.query,
                            request.settings.sort_key.album_sort(),
                            request.settings.descending,
                            range.start,
                            range.len(),
                            &cancellation,
                        )
                        .await
                        .map_err(|error| error.to_string())
                }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
            },
        );
        let sparse = SparseRouteModel::new(
            SparseSource::Query { count },
            32,
            selected.runtime.clone(),
            {
                let load = Arc::clone(&load);
                let request = Rc::clone(&request);
                Rc::new(move |_, range, cancellation| {
                    let request = request.borrow().clone();
                    load(request, range, cancellation)
                })
            },
        );
        gtk_widgets::media_drag::install_collection_selection(&sparse, request, load, |row| {
            gtk_widgets::selection::collection_play_request(library::SongScope::Album(
                row.album_key,
            ))
        });
        sparse.seed_matching_at(first_row_position, first_rows, |row| row.album_key);
        Self {
            rows: Some(sparse),
            detail: AlbumDetailModel::empty(),
        }
    }

    pub fn albums(&self) -> SparseObjectModel {
        self.rows
            .as_ref()
            .expect("Album row model is mounted only in Row or Grid layout")
            .list_model()
    }

    pub fn album_rows(&self) -> Option<Rc<SparseRouteModel<AlbumKey, AlbumRow>>> {
        self.rows.as_ref().map(Rc::clone)
    }

    pub fn detail(&self) -> AlbumDetailModel {
        self.detail.clone()
    }

    pub fn is_empty(&self) -> bool {
        self.rows
            .as_ref()
            .map_or_else(|| self.detail.n_items() == 0, |rows| rows.len() == 0)
    }

    pub fn resume_initial_demand(&self) {
        if let Some(sparse) = &self.rows {
            sparse.resume_initial_demand();
        }
        if let Some(sparse) = &self.detail.sparse {
            sparse.resume_initial_demand();
        }
    }

    pub fn replace_prepared(
        &self,
        count: usize,
        first_row_position: usize,
        first_rows: Vec<AlbumRow>,
        first_detail_rows: Vec<AlbumDetailRow>,
    ) -> bool {
        if let Some(rows) = &self.rows {
            return rows.replace_prepared_at(
                SparseSource::Query { count },
                first_row_position,
                first_rows,
                Vec::new(),
                |row| row.album_key,
            );
        }
        let Some(detail) = &self.detail.sparse else {
            return false;
        };
        detail.replace_prepared_at(
            SparseSource::Query { count },
            first_row_position,
            first_detail_rows,
            Vec::new(),
            detail_key,
        )
    }

    pub fn update_favorite(&self, media_uri: &str, favorite: bool) -> bool {
        if let Some(rows) = &self.rows {
            let Some(position) = rows.ready_position(|row| row.media_uri == media_uri) else {
                return false;
            };
            rows.update_ready(position as usize, |row| row.favorite = favorite)
        } else if let Some(detail) = &self.detail.sparse {
            let Some(position) = detail.ready_position(|row| {
                matches!(row, AlbumDetailRow::Album { album, .. } if album.media_uri == media_uri)
            })
            else {
                return false;
            };
            detail.update_ready(position as usize, |row| {
                if let AlbumDetailRow::Album { album, .. } = row {
                    album.favorite = favorite;
                }
            })
        } else {
            false
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
pub struct AlbumDetailVirtualList {
    inner: Rc<AlbumDetailVirtualListInner>,
}

struct AlbumDetailVirtualListInner {
    shell: Weak<CatalogUi>,
    list: gtk::ListView,
    fields: Vec<LibraryField>,
    width: Cell<i32>,
    display: RefCell<rufin_core::settings::layout::DisplaySettings>,
    playing: RefCell<(Option<String>, bool)>,
    cells: RefCell<HashMap<usize, Rc<AlbumDetailCell>>>,
}

impl AlbumDetailVirtualListInner {
    fn cells(&self) -> Vec<Rc<AlbumDetailCell>> {
        self.cells.borrow().values().cloned().collect()
    }
}

impl AlbumDetailVirtualList {
    fn new(shell: &Rc<CatalogUi>, model: AlbumDetailModel, fields: Vec<LibraryField>) -> Self {
        let factory = gtk::SignalListItemFactory::new();
        let list = gtk::ListView::new(None::<gtk::NoSelection>, None::<gtk::SignalListItemFactory>);
        list.add_css_class("track-table");
        list.add_css_class("album-detail-list");
        list.set_hexpand(true);
        let inner = Rc::new(AlbumDetailVirtualListInner {
            shell: Rc::downgrade(shell),
            list,
            fields,
            width: Cell::new((shell.route_width)().max(1)),
            display: RefCell::new(detail_display(shell)),
            playing: RefCell::new((None, false)),
            cells: RefCell::new(HashMap::new()),
        });
        let weak = Rc::downgrade(&inner);
        factory.connect_setup(move |_, object| {
            let (Some(inner), Some(item)) =
                (weak.upgrade(), object.downcast_ref::<gtk::ListItem>())
            else {
                return;
            };
            let Some(shell) = inner.shell.upgrade() else {
                return;
            };
            item.set_selectable(false);
            item.set_activatable(false);
            let cell = Rc::new(AlbumDetailCell::new(&shell, &inner.fields));
            cell.item.replace(Some(item.downgrade()));
            let display = inner.display.borrow().clone();
            cell.resize(inner.width.get(), &display);
            item.set_child(Some(&cell.widget));
            inner
                .cells
                .borrow_mut()
                .insert(item.as_ptr() as usize, cell);
        });
        let weak = Rc::downgrade(&inner);
        gtk_widgets::sparse_model::connect_sparse_bind(&factory, move |object| {
            let (Some(inner), Some(item)) =
                (weak.upgrade(), object.downcast_ref::<gtk::ListItem>())
            else {
                return;
            };
            let cell = inner.cells.borrow().get(&(item.as_ptr() as usize)).cloned();
            if let Some(cell) = cell
                && let Some(shell) = inner.shell.upgrade()
            {
                cell.bind(
                    &shell,
                    gtk_widgets::sparse_model::item_at_from_item::<AlbumDetailRow>(item),
                );
                let display = inner.display.borrow().clone();
                cell.resize(inner.width.get(), &display);
                let playing = inner.playing.borrow().clone();
                cell.playing(playing.0.as_deref(), playing.1);
            }
        });
        let weak = Rc::downgrade(&inner);
        factory.connect_unbind(move |_, object| {
            let Some(inner) = weak.upgrade() else { return };
            let cell = inner
                .cells
                .borrow()
                .get(&(object.as_ptr() as usize))
                .cloned();
            if let Some(cell) = cell
                && let Some(shell) = inner.shell.upgrade()
            {
                cell.bind(&shell, None);
            }
        });
        let weak = Rc::downgrade(&inner);
        factory.connect_teardown(move |_, object| {
            if let Some(inner) = weak.upgrade() {
                inner.cells.borrow_mut().remove(&(object.as_ptr() as usize));
            }
        });
        let weak = Rc::downgrade(&inner);
        shell.register_current_route_track_selection(Rc::new(move |current| {
            let Some(inner) = weak.upgrade() else {
                return false;
            };
            let uri = current.map(|current| current.media_uri.clone());
            let paused = current.is_some_and(|current| current.paused);
            inner.playing.replace((uri.clone(), paused));
            for cell in inner.cells() {
                cell.playing(uri.as_deref(), paused);
            }
            true
        }));
        inner.list.set_factory(Some(&factory));
        inner
            .list
            .set_model(Some(&gtk::NoSelection::new(Some(model.list_model()))));
        Self { inner }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.inner.list.clone().upcast()
    }

    pub fn apply_display(&self, display: &rufin_core::settings::layout::DisplaySettings) {
        if *self.inner.display.borrow() != *display {
            self.inner.display.replace(display.clone());
            for cell in self.inner.cells() {
                cell.resize(self.inner.width.get(), display);
            }
        }
    }

    pub fn fit_allocation(&self, width: i32) {
        if width > 1 && self.inner.width.replace(width) != width {
            let display = self.inner.display.borrow().clone();
            for cell in self.inner.cells() {
                cell.resize(width, &display);
            }
        }
    }

    pub fn item_scroll(&self) -> gtk_widgets::mounted_route::NativeListScroll {
        let inner = Rc::downgrade(&self.inner);
        gtk_widgets::mounted_route::NativeListScroll::new(
            self.inner.list.clone(),
            Rc::new(move || {
                let Some(inner) = inner.upgrade() else {
                    return Vec::new();
                };
                inner
                    .cells()
                    .into_iter()
                    .filter_map(|cell| {
                        let item = cell.item.borrow().as_ref()?.upgrade()?;
                        item.item()?;
                        Some((item.position(), cell.widget.clone().upcast()))
                    })
                    .collect()
            }),
            true,
        )
    }

    pub fn navigate(&self, direction: gtk::DirectionType) -> glib::Propagation {
        if self
            .inner
            .list
            .state_flags()
            .contains(gtk::StateFlags::FOCUS_WITHIN)
        {
            return glib::Propagation::Proceed;
        }
        let Some(model) = self.inner.list.model() else {
            return glib::Propagation::Stop;
        };
        if let Some(position) = gtk_widgets::mounted_route::item_navigation_entry_position(
            gtk::INVALID_LIST_POSITION,
            model.n_items(),
            direction,
        ) {
            self.inner
                .list
                .scroll_to(position, gtk::ListScrollFlags::FOCUS, None);
            self.inner.list.grab_focus();
        }
        glib::Propagation::Stop
    }
}

struct AlbumDetailCell {
    widget: gtk::Box,
    item: RefCell<Option<glib::WeakRef<gtk::ListItem>>>,
    meta: gtk::Box,
    meta_contents: gtk::Box,
    cover: super::cards::AlbumCover,
    labels: Vec<(gtk::ScrolledWindow, gtk::Label)>,
    artist_links: gtk_widgets::detail_links::DetailLinkBinding,
    separator: gtk::Widget,
    track_area: gtk::Box,
    header: gtk::Box,
    header_cells: Vec<gtk::Widget>,
    tracks: Vec<AlbumTrackSlot>,
    fields: Vec<LibraryField>,
    lead: Cell<bool>,
    has_genres: Cell<bool>,
    last: Cell<bool>,
    track_count: Cell<usize>,
}

impl AlbumDetailCell {
    fn new(shell: &Rc<CatalogUi>, fields: &[LibraryField]) -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        widget.add_css_class("album-detail-row");
        widget.set_hexpand(true);
        widget.set_halign(gtk::Align::Fill);
        widget.set_margin_start(4);
        widget.set_margin_end(4);
        let meta = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let meta_contents = gtk::Box::new(gtk::Orientation::Vertical, ALBUM_DETAIL_META_SPACING);
        let cover = super::cards::AlbumCover::new(shell);
        meta_contents.append(&cover.widget);
        let labels = ["track-title", "muted", "muted", "muted"]
            .into_iter()
            .map(|class| {
                let clip = album_meta_label("", class, 1);
                let label = clip
                    .child()
                    .and_downcast::<gtk::Label>()
                    .expect("album metadata label");
                meta_contents.append(&clip);
                (clip, label)
            })
            .collect::<Vec<_>>();
        let artist_links = gtk_widgets::detail_links::DetailLinkBinding::new(
            &labels[1].1,
            shell.route_navigation(),
        );
        meta.append(&meta_contents);
        widget.append(&meta);
        let separator = album_detail_separator(1);
        widget.append(&separator);
        let track_area = gtk::Box::new(gtk::Orientation::Vertical, 0);
        track_area.set_hexpand(true);
        let (header, header_cells) = album_track_header(fields);
        track_area.append(&header);
        let tracks = (0..ALBUM_DETAIL_INLINE_TRACK_ROWS)
            .map(|_| {
                let slot = AlbumTrackSlot::new(shell, fields);
                track_area.append(&slot.widget);
                slot
            })
            .collect();
        widget.append(&track_area);
        Self {
            widget,
            item: RefCell::new(None),
            meta,
            meta_contents,
            cover,
            labels,
            artist_links,
            separator,
            track_area,
            header,
            header_cells,
            tracks,
            fields: fields.to_vec(),
            lead: Cell::new(false),
            has_genres: Cell::new(false),
            last: Cell::new(false),
            track_count: Cell::new(0),
        }
    }

    fn bind(&self, shell: &CatalogUi, row: Option<AlbumDetailRow>) {
        let mut values = Vec::new();
        self.lead.set(false);
        self.has_genres.set(false);
        self.last.set(false);
        match row {
            Some(AlbumDetailRow::Album {
                album,
                tracks,
                last,
            }) => {
                self.lead.set(true);
                self.last.set(last);
                self.has_genres.set(!album.genres.is_empty());
                self.cover.bind(shell, Some(&album));
                self.labels[0].1.set_text(&album.title);
                self.artist_links
                    .bind(gtk_widgets::detail_links::album_artist_links(&album));
                let mut facts = album
                    .year
                    .map(|year| year.to_string())
                    .into_iter()
                    .collect::<Vec<_>>();
                facts.push(track_count_text(album.track_count.max(0) as u64));
                facts.push(gtk_widgets::format_duration_units(
                    (album.duration_millis.max(0) / 1000) as u32,
                ));
                self.labels[2].1.set_text(&facts.join(" • "));
                self.labels[3].1.set_text(
                    &album
                        .genres
                        .iter()
                        .map(|genre| genre.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                );
                values.extend(tracks.into_iter().enumerate());
            }
            Some(AlbumDetailRow::Track { track, index, last }) => {
                self.last.set(last);
                self.cover.bind(shell, None);
                values.push((index, track));
            }
            None => self.cover.bind(shell, None),
        }
        self.track_count.set(values.len());
        self.meta_contents.set_visible(self.lead.get());
        self.labels[3].0.set_visible(self.has_genres.get());
        self.separator.set_visible(!values.is_empty());
        self.widget
            .set_margin_top(if self.lead.get() { 12 } else { 0 });
        self.widget
            .set_margin_bottom(if self.last.get() { 16 } else { 0 });
        let mut values = values.into_iter();
        for slot in &self.tracks {
            slot.bind(shell, values.next());
        }
    }

    fn resize(&self, width: i32, display: &rufin_core::settings::layout::DisplaySettings) {
        let metrics = album_detail_row_metrics_for_width(width);
        self.widget.set_spacing(metrics.spacing);
        self.meta.set_width_request(metrics.meta_width);
        self.cover.resize(metrics.cover_size);
        for (clip, _) in &self.labels {
            clip.set_width_request(metrics.meta_width);
            clip.set_min_content_width(metrics.meta_width);
            clip.set_max_content_width(metrics.meta_width);
        }
        let widths = album_track_field_widths(&self.fields, album_track_area_width(width, metrics));
        self.track_area
            .set_width_request(album_track_area_width(width, metrics));
        self.header
            .set_visible(self.lead.get() && self.track_count.get() > 0 && display.show_header);
        for (cell, (_, width)) in self.header_cells.iter().zip(&widths) {
            cell.set_width_request(*width);
        }
        for slot in &self.tracks {
            slot.resize(&widths, display_track_height(display));
        }
        let height = if self.lead.get() {
            album_detail_ready_height(
                width,
                self.has_genres.get(),
                self.track_count.get(),
                display,
            )
        } else {
            display_track_height(display)
        };
        self.widget.set_height_request(height);
        self.separator.set_height_request(height);
    }

    fn playing(&self, uri: Option<&str>, paused: bool) {
        for slot in &self.tracks {
            slot.playing(uri, paused);
        }
    }
}

struct AlbumTrackSlot {
    widget: gtk::Box,
    current: Rc<RefCell<Option<Rc<TrackRow>>>>,
    cells: Vec<(gtk::Widget, AlbumTrackField)>,
}

enum AlbumTrackField {
    Text(LibraryField, gtk::Label),
    Index(gtk::Overlay),
    Image(ArtworkTile),
    Favorite(gtk::Button),
}

impl AlbumTrackSlot {
    fn new(shell: &Rc<CatalogUi>, fields: &[LibraryField]) -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Horizontal, ALBUM_DETAIL_TRACK_COLUMN_GAP);
        widget.add_css_class("album-detail-track-row");
        widget.set_hexpand(true);
        let current = Rc::new(RefCell::new(None::<Rc<TrackRow>>));
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
                play_album_track(&shell, track.album_key, track.track_key);
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
                                play_album_track(&shell, track.album_key, track.track_key);
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

    fn bind(&self, shell: &CatalogUi, value: Option<(usize, TrackRow)>) {
        self.widget.set_visible(value.is_some());
        let index = value.as_ref().map_or(0, |(index, _)| *index);
        self.current.replace(value.map(|(_, track)| Rc::new(track)));
        if index % 2 == 1 {
            self.widget.add_css_class("display-even");
        } else {
            self.widget.remove_css_class("display-even");
        }
        let current = self.current.borrow().clone();
        for (_, cell) in &self.cells {
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
                    current.as_ref().is_some_and(|track| track.favorite),
                ),
                AlbumTrackField::Image(tile) => {
                    if let Some(track) = current.as_ref() {
                        shell.artwork.bind_artwork_tile(
                            tile,
                            opaque_artwork(track.artwork_binding.as_deref()),
                            32,
                            THUMB_COVER_SIZE,
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
        .saturating_sub(
            crate::route_layout::PRIMARY_ROUTE_MARGIN_START
                + crate::route_layout::PRIMARY_ROUTE_MARGIN_END,
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

fn album_detail_lead_content_height(
    cover_size: i32,
    meta_label_count: usize,
    inline_count: usize,
    display: &rufin_core::settings::layout::DisplaySettings,
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
        (if display.show_header {
            ALBUM_DETAIL_TRACK_HEADER_HEIGHT
        } else {
            0
        })
        .saturating_add(
            i32::try_from(inline_count.min(ALBUM_DETAIL_INLINE_TRACK_ROWS))
                .unwrap_or(i32::MAX)
                .saturating_mul(display_track_height(display)),
        )
    };
    meta_height.max(track_height)
}

fn album_detail_ready_height(
    width: i32,
    has_genres: bool,
    inline_count: usize,
    display: &rufin_core::settings::layout::DisplaySettings,
) -> i32 {
    album_detail_lead_content_height(
        album_detail_row_metrics_for_width(width).cover_size,
        3 + usize::from(has_genres),
        inline_count,
        display,
    )
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

fn album_track_header(fields: &[LibraryField]) -> (gtk::Box, Vec<gtk::Widget>) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, ALBUM_DETAIL_TRACK_COLUMN_GAP);
    row.add_css_class("album-detail-track-cells");
    row.add_css_class("table-header");
    row.set_height_request(ALBUM_DETAIL_TRACK_HEADER_HEIGHT);
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
                label.upcast()
            };
            let cell = fixed_album_track_cell(1, ALBUM_DETAIL_TRACK_HEADER_HEIGHT, label);
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
    cell.set_size_request(width.max(1), height.max(1));
    cell.set_overflow(gtk::Overflow::Hidden);
    cell.append(&child);
    cell.upcast()
}

fn album_track_area_width(width: i32, metrics: AlbumDetailRowMetrics) -> i32 {
    width
        .saturating_sub(
            crate::route_layout::PRIMARY_ROUTE_MARGIN_START
                + crate::route_layout::PRIMARY_ROUTE_MARGIN_END
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
) {
    let Some(selected) = shell.selected_library().as_deref().cloned() else {
        return;
    };
    let Some(album) = album else {
        return;
    };
    let queue = shell.queue.clone();
    let settings = shell
        .settings
        .current
        .borrow()
        .library_list(LibraryListKey::AlbumDetailTracks);
    queue.play(
        playback::PlayRequest::ordered(
            library::SongQuery {
                folder: selected.music_folder_key,
                sort: Some(settings.sort_key.track_sort()),
                descending: settings.descending,
                ..library::SongQuery::new(library::SongScope::Album(album))
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

pub fn album_detail_track_text(track: &TrackRow, index: usize, field: LibraryField) -> String {
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
            + crate::route_layout::PRIMARY_ROUTE_MARGIN_START
            + crate::route_layout::PRIMARY_ROUTE_MARGIN_END
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
    fn album_detail_header_keeps_space_for_genres_and_inline_tracks() {
        let width = 720;
        let cover = album_detail_row_metrics_for_width(width).cover_size;
        let expected_meta = cover
            + 4 * (ALBUM_DETAIL_META_LABEL_HEIGHT + ALBUM_DETAIL_META_SPACING)
            + ALBUM_DETAIL_META_LABEL_HEIGHT;
        let expected_tracks = ALBUM_DETAIL_TRACK_HEADER_HEIGHT + 3 * ALBUM_TRACK_HEIGHT;
        assert_eq!(
            album_detail_ready_height(width, true, 3, &Default::default()),
            expected_meta.max(expected_tracks),
        );
    }
}

pub async fn load_album_collection(
    database: &library::Database,
    source_key: library::SourceKey,
    folder: Option<library::FolderKey>,
    settings: &rufin_core::settings::LibraryListSettings,
    favorites_only: bool,
    filter: &str,
    window: library::RouteSeedWindow,
    cancellation: &library::ReadCancellation,
) -> library::LibraryResult<(usize, usize, Vec<AlbumRow>, Vec<AlbumDetailRow>)> {
    if settings.layout == crate::LibraryLayout::Detail {
        let count = database
            .album_detail_count(
                source_key,
                folder,
                favorites_only,
                filter,
                ALBUM_DETAIL_INLINE_TRACK_ROWS,
                cancellation,
            )
            .await?
            .max(0) as usize;
        let seed = window.range(count);
        let first_detail_rows = database
            .album_detail_page(
                source_key,
                folder,
                favorites_only,
                filter,
                settings.sort_key.album_sort(),
                settings.descending,
                ALBUM_DETAIL_INLINE_TRACK_ROWS,
                seed.start,
                seed.len(),
                cancellation,
            )
            .await?;
        Ok((count, seed.start, Vec::new(), first_detail_rows))
    } else {
        let (order, first_row_position, first_rows) = database
            .album_route_page(
                source_key,
                folder,
                favorites_only,
                filter,
                settings.sort_key.album_sort(),
                settings.descending,
                window,
                cancellation,
            )
            .await?;
        Ok((order, first_row_position, first_rows, Vec::new()))
    }
}
