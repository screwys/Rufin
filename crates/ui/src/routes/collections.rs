use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};

use ::library::{
    AlbumKey, AlbumRow, ArtistKey, ArtistRow, GenreKey, MoodKey, PlaylistKey, PlaylistRow,
    SmartPlaylistKey, SmartPlaylistRow, TrackKey, TrackRow,
};
use adw::prelude::*;
use gtk::{gio, glib};

use crate::layout::{allocation_owner, configure_fill_width_clip, width_allocation_owner};
use crate::preferences::dialogs::SmartPlaylistChange;
use crate::shell::Shell;
use crate::shell::route::{MountedRouteItemNavigation, item_navigation_entry_position};
use crate::{LibraryField, LibraryLayout, LibraryListKey};
use localization::tr;
use playback::{LoadedPlayRequest, QueuePlacement};
use tracing::warn;

use super::album_detail::{AlbumCollectionModels, AlbumDetailVirtualList, album_detail_list};
use super::columns::{
    TrackRowPlayingIndicator, album_column, artist_column, column_fit_width, playlist_column,
    smart_playlist_column, track_column_fit_width, track_column_for_key,
    track_position_text_column,
};
use super::detail_links::{DetailLinks, track_album_artist_links, track_artist_links};
use super::grid_cells::{
    AlbumGridCell, ArtistGridCell, CollectionGridProjection, TrackGridCell,
    collection_grid_with_demand,
};
use super::library_fields::{
    COLLECTION_GRID_CARD_GAP, clear_list_item_child, column_width, compact_header_column_width,
    grid_label_with_label, item_at, item_at_from_item, track_field,
};
use super::route::Route;
use super::route_layout::{PRIMARY_ROUTE_MARGIN_END, PRIMARY_ROUTE_MARGIN_START};
use super::route_shell::restore_single_click_activation_on_primary_press;
use super::sparse_model::connect_sparse_bind;
use super::table_sizing::{
    ColumnViewWidthFit, column_view_initial_width, install_column_view_width_fit,
    route_column_view_initial_width,
};
use super::track_model::TrackCollectionModel;
use crate::runtime::SelectedLibrary;
use downloads::DownloadSubject;

pub(super) const SMART_PLAYLIST_REORDER_WIDTH: i32 = 30;
pub(super) const LIBRARY_TABLE_HEADER_HEIGHT: i32 = 92;
const LIBRARY_TABLE_ROW_HEIGHT: i32 = 58;

// GtkColumnView allocates a 30px header and 64px for each current track row.

pub(crate) type CollectionPlay = Rc<dyn Fn(QueuePlacement, bool)>;

#[derive(Clone, Debug)]
pub(crate) enum PlaybackTarget {
    Track(TrackKey),
    Album(AlbumKey),
    Artist(ArtistKey),
    AlbumArtist(ArtistKey),
    Genre(GenreKey),
    Mood(MoodKey),
    Playlist(PlaylistKey),
    SmartPlaylist(SmartPlaylistKey),
    Contextual {
        target: Box<PlaybackTarget>,
        context_id: String,
    },
}

impl PlaybackTarget {
    pub(crate) fn in_context(self, context_id: impl Into<String>) -> Self {
        Self::Contextual {
            target: Box::new(self),
            context_id: context_id.into(),
        }
    }

    pub(crate) fn download(&self, shell: &Shell) {
        let Some(selected) = shell.selected_library().as_deref().cloned() else {
            return;
        };
        let target = self.clone();
        let downloads = shell.products.downloads.clone();
        let runtime = selected.runtime.clone();
        runtime.spawn(async move {
            match target.resolve_order(&selected).await {
                Ok((order, _)) => downloads.download(
                    selected.artwork.source_id.clone(),
                    target.download_subject(),
                    order.to_vec(),
                ),
                Err(error) => warn!(target = ?target, %error, "failed to identify download tracks"),
            }
        });
    }

    pub(crate) fn remove_download(&self, shell: &Shell) {
        let Some(selected) = shell.selected_library().as_deref().cloned() else {
            return;
        };
        let target = self.clone();
        let downloads = shell.products.downloads.clone();
        let runtime = selected.runtime.clone();
        runtime.spawn(async move {
            match target.resolve_order(&selected).await {
                Ok((order, _)) => {
                    downloads.remove(selected.artwork.source_id.clone(), order.to_vec(), true)
                }
                Err(error) => {
                    warn!(target = ?target, %error, "failed to identify downloaded tracks")
                }
            }
        });
    }

    pub(crate) fn download_subject(&self) -> DownloadSubject {
        match self {
            Self::Track(id) => DownloadSubject::Track(id.clone()),
            Self::Album(id) => DownloadSubject::Album(id.clone()),
            Self::Artist(id) | Self::AlbumArtist(id) => DownloadSubject::Artist(id.clone()),
            Self::Genre(id) => DownloadSubject::Genre(id.clone()),
            Self::Mood(id) => DownloadSubject::Mood(id.clone()),
            Self::Playlist(id) => DownloadSubject::Playlist(id.clone()),
            Self::SmartPlaylist(id) => DownloadSubject::SmartPlaylist(id.clone()),
            Self::Contextual { target, .. } => target.download_subject(),
        }
    }

    fn context_id(&self) -> String {
        match self {
            Self::Track(id) => format!("track:{id}"),
            Self::Album(id) => format!("album:{id}"),
            Self::Artist(id) => format!("artist:{id}"),
            Self::AlbumArtist(id) => format!("album-artist:{id}"),
            Self::Genre(id) => format!("genre:{id}"),
            Self::Mood(id) => format!("mood:{id}"),
            Self::Playlist(id) => format!("playlist:{id}"),
            Self::SmartPlaylist(id) => format!("smart-playlist:{id}"),
            Self::Contextual { context_id, .. } => context_id.clone(),
        }
    }

    pub(crate) fn play(&self, shell: &Shell, placement: QueuePlacement, shuffled_start: bool) {
        let Some(selected) = shell.selected_library().as_deref().cloned() else {
            return;
        };
        let target = self.clone();
        let queue = shell.products.playback.queue.clone();
        let runtime = selected.runtime.clone();
        runtime.spawn(async move {
            let (order, prepared_anchor) = match target.resolve_order(&selected).await {
                Ok(result) => result,
                Err(error) => {
                    warn!(target = ?target, %error, "failed to prepare collection Play order");
                    return;
                }
            };
            let anchor = if let Some(anchor) = prepared_anchor {
                anchor
            } else {
                let Some(key) = order.first().copied() else {
                    return;
                };
                let cancellation = library::ReadCancellation::new();
                let Some(row) = selected
                    .database
                    .track_rows(selected.source_key, &[key], &cancellation)
                    .await
                    .ok()
                    .and_then(|mut rows| rows.pop())
                else {
                    return;
                };
                playback::PlaybackMedia::from(row)
            };
            if let Some(request) = LoadedPlayRequest::context(
                selected.source_key,
                selected.source_session_epoch,
                order,
                anchor,
                0,
                placement,
                target.context_id(),
                shuffled_start && placement == QueuePlacement::Now,
            ) {
                queue.play_loaded(request);
            }
        });
    }

    pub(crate) fn resolve_order<'a>(
        &'a self,
        selected: &'a SelectedLibrary,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<(Arc<[TrackKey]>, Option<playback::PlaybackMedia>), String>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let folder = selected.music_folder_key;
            let cancellation = library::ReadCancellation::new();
            let order = match self {
                Self::Track(key) => vec![*key],
                Self::Album(key) => selected
                    .database
                    .album_track_order(
                        selected.source_key,
                        *key,
                        folder,
                        "",
                        library::TrackSort::TrackNumber,
                        false,
                        &cancellation,
                    )
                    .await
                    .map_err(|error| error.to_string())?,
                Self::Artist(key) => selected
                    .database
                    .artist_track_order(
                        selected.source_key,
                        *key,
                        false,
                        folder,
                        "",
                        library::TrackSort::Title,
                        false,
                        &cancellation,
                    )
                    .await
                    .map_err(|error| error.to_string())?,
                Self::AlbumArtist(key) => selected
                    .database
                    .artist_track_order(
                        selected.source_key,
                        *key,
                        true,
                        folder,
                        "",
                        library::TrackSort::Title,
                        false,
                        &cancellation,
                    )
                    .await
                    .map_err(|error| error.to_string())?,
                Self::Genre(key) => selected
                    .database
                    .genre_track_order(
                        selected.source_key,
                        *key,
                        folder,
                        "",
                        library::TrackSort::Title,
                        false,
                        &cancellation,
                    )
                    .await
                    .map_err(|error| error.to_string())?,
                Self::Mood(key) => selected
                    .database
                    .mood_track_order(
                        selected.source_key,
                        *key,
                        folder,
                        "",
                        library::TrackSort::Title,
                        false,
                        &cancellation,
                    )
                    .await
                    .map_err(|error| error.to_string())?,
                Self::Playlist(key) => selected
                    .database
                    .playlist_track_order(selected.source_key, *key, folder, &cancellation)
                    .await
                    .map_err(|error| error.to_string())?,
                Self::SmartPlaylist(key) => {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |duration| duration.as_secs().min(i64::MAX as u64) as i64);
                    selected
                        .database
                        .smart_playlist_track_order(
                            selected.source_key,
                            *key,
                            folder,
                            now,
                            &cancellation,
                        )
                        .await
                        .map_err(|error| error.to_string())?
                }
                Self::Contextual { target, .. } => return target.resolve_order(selected).await,
            };
            Ok((order.into(), None))
        })
    }
}

struct TrackTablePlayingStateInner {
    model: TrackCollectionModel,
    handler_model: super::sparse_model::SparseObjectModel,
    model_handler: RefCell<Option<glib::SignalHandlerId>>,
    track_id: RefCell<Option<TrackKey>>,
    position: Cell<u32>,
    indicator: TrackRowPlayingIndicator,
}

#[derive(Clone)]
pub(crate) struct TrackTablePlayingState {
    inner: Rc<TrackTablePlayingStateInner>,
}

#[derive(Clone)]
pub(crate) struct CollectionTableProjection {
    table: gtk::ColumnView,
    navigation: MountedRouteItemNavigation,
    fixed_columns: Rc<Vec<(gtk::ColumnViewColumn, i32)>>,
    column_for_field: Rc<dyn Fn(LibraryField) -> (gtk::ColumnViewColumn, i32)>,
    width_for_field: Rc<dyn Fn(LibraryField) -> i32>,
    fields: Rc<RefCell<Vec<LibraryField>>>,
    width_fit: ColumnViewWidthFit,
}

impl CollectionTableProjection {
    pub(crate) fn widget(&self) -> gtk::Widget {
        self.table.clone().upcast()
    }

    pub(crate) fn apply_fields(&self, fields: &[LibraryField]) {
        if self.fields.borrow().as_slice() == fields {
            self.width_fit
                .set_preferred_widths(&self.preferred_widths(fields));
            return;
        }
        while let Some(column) = self
            .table
            .columns()
            .item(0)
            .and_then(|item| item.downcast::<gtk::ColumnViewColumn>().ok())
        {
            self.table.remove_column(&column);
        }
        let mut active = self.fixed_columns.as_ref().clone();
        for (column, _) in self.fixed_columns.iter() {
            self.table.append_column(column);
        }
        for field in fields {
            let (column, width) = (self.column_for_field)(*field);
            self.table.append_column(&column);
            active.push((column, width));
        }
        *self.fields.borrow_mut() = fields.to_vec();
        self.width_fit.replace(active);
    }

    pub(crate) fn fit_scroller_allocation(&self, scroller: &gtk::ScrolledWindow, width: i32) {
        self.width_fit.fit_scroller_allocation(scroller, width);
    }

    fn navigate(&self, direction: gtk::DirectionType) -> glib::Propagation {
        (self.navigation)(direction)
    }

    fn preferred_widths(&self, fields: &[LibraryField]) -> Vec<i32> {
        self.fixed_columns
            .iter()
            .map(|(_, width)| *width)
            .chain(fields.iter().map(|field| (self.width_for_field)(*field)))
            .collect()
    }
}

#[derive(Clone)]
pub(super) enum LibraryPresentationProjection {
    Row(CollectionTableProjection),
    Grid(CollectionGridProjection),
    AlbumDetail(AlbumDetailVirtualList),
}

impl LibraryPresentationProjection {
    fn widget(&self) -> gtk::Widget {
        match self {
            Self::Row(table) => table.widget(),
            Self::Grid(grid) => grid.widget(),
            Self::AlbumDetail(detail) => detail.widget(),
        }
    }

    fn apply_fields(&self, settings: &crate::LibraryListSettings) {
        match self {
            Self::Row(table) => table.apply_fields(&settings.row_fields),
            Self::Grid(grid) => grid.apply_fields(&settings.grid_fields),
            Self::AlbumDetail(_) => {}
        }
    }

    fn navigate(&self, direction: gtk::DirectionType) -> glib::Propagation {
        match self {
            Self::Row(table) => table.navigate(direction),
            Self::Grid(grid) => grid.navigate(direction),
            Self::AlbumDetail(detail) => detail.navigate(direction),
        }
    }

    fn attach_scroller(&self, scroller: &gtk::ScrolledWindow) {
        match self {
            Self::AlbumDetail(detail) => detail.attach_scroller(scroller),
            Self::Row(_) | Self::Grid(_) => {}
        }
    }

    fn fit_scroller_allocation(&self, scroller: &gtk::ScrolledWindow, width: i32) {
        match self {
            Self::Row(table) => table.width_fit.fit_scroller_allocation(scroller, width),
            Self::Grid(grid) => grid.fit_allocation(width),
            Self::AlbumDetail(detail) => detail.fit_allocation(width),
        }
    }
}

#[derive(Clone)]
pub(crate) struct LibraryCollectionProjection {
    host: Rc<RefCell<LibraryCollectionHost>>,
    settings: Rc<RefCell<crate::LibraryListSettings>>,
    presentation: Rc<RefCell<LibraryPresentationProjection>>,
    build: Rc<dyn Fn(LibraryLayout) -> LibraryPresentationProjection>,
}

#[derive(Clone)]
enum LibraryCollectionHost {
    Embedded(gtk::Box),
    Scrolled {
        scroller: gtk::ScrolledWindow,
        margin_start: i32,
        margin_end: i32,
        width_owner: gtk::Widget,
    },
}

impl LibraryCollectionProjection {
    pub(super) fn new(
        settings: crate::LibraryListSettings,
        build: Rc<dyn Fn(LibraryLayout) -> LibraryPresentationProjection>,
    ) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_hexpand(true);
        root.set_vexpand(true);
        let presentation = build(settings.layout);
        presentation.apply_fields(&settings);
        let widget = presentation.widget();
        if widget.parent().is_none() {
            root.append(&widget);
        }
        Self {
            host: Rc::new(RefCell::new(LibraryCollectionHost::Embedded(root))),
            settings: Rc::new(RefCell::new(settings)),
            presentation: Rc::new(RefCell::new(presentation)),
            build,
        }
    }

    pub(crate) fn scrolling_scroller(&self) -> gtk::ScrolledWindow {
        if let LibraryCollectionHost::Scrolled { scroller, .. } = &*self.host.borrow() {
            return scroller.clone();
        }

        let scroller = gtk::ScrolledWindow::new();
        configure_library_route_scroller(&scroller);
        self.mount_in_scroller(
            &scroller,
            PRIMARY_ROUTE_MARGIN_START,
            PRIMARY_ROUTE_MARGIN_END,
        );
        scroller
    }

    pub(crate) fn scrolling_widget(&self) -> gtk::Widget {
        self.scrolling_scroller();
        let LibraryCollectionHost::Scrolled { width_owner, .. } = &*self.host.borrow() else {
            unreachable!("scrolling_scroller installs the scrolled collection host");
        };
        width_owner.clone()
    }

    pub(crate) fn item_navigation(&self) -> MountedRouteItemNavigation {
        let presentation = Rc::clone(&self.presentation);
        Rc::new(move |direction| presentation.borrow().navigate(direction))
    }

    pub(crate) fn mount_in_scroller(
        &self,
        scroller: &gtk::ScrolledWindow,
        margin_start: i32,
        margin_end: i32,
    ) -> gtk::Widget {
        let active = self.presentation.borrow().widget();
        let previous_host = self.host.borrow().clone();
        match previous_host {
            LibraryCollectionHost::Embedded(root) => {
                if active.parent().as_ref() == Some(root.upcast_ref()) {
                    root.remove(&active);
                }
            }
            LibraryCollectionHost::Scrolled {
                scroller: previous, ..
            } => previous.set_child(None::<&gtk::Widget>),
        }
        active.set_margin_start(margin_start);
        active.set_margin_end(margin_end);
        scroller.set_child(Some(&active));
        self.presentation.borrow().attach_scroller(scroller);
        let allocation_only = matches!(
            &*self.presentation.borrow(),
            LibraryPresentationProjection::AlbumDetail(_)
        );
        let presentation = Rc::clone(&self.presentation);
        let resize_scroller = scroller.clone();
        let width_owner = if allocation_only {
            allocation_owner(scroller, move |width, _| {
                presentation
                    .borrow()
                    .fit_scroller_allocation(&resize_scroller, width);
            })
            .upcast::<gtk::Widget>()
        } else {
            width_allocation_owner(scroller, move |width| {
                presentation
                    .borrow()
                    .fit_scroller_allocation(&resize_scroller, width);
            })
            .upcast::<gtk::Widget>()
        };
        self.host.replace(LibraryCollectionHost::Scrolled {
            scroller: scroller.clone(),
            margin_start,
            margin_end,
            width_owner: width_owner.clone(),
        });
        width_owner
    }

    pub(crate) fn apply_settings(&self, settings: &crate::LibraryListSettings) {
        let previous = self.settings.borrow().clone();
        if collection_presentation_needs_rebuild(&previous, settings) {
            let presentation = (self.build)(settings.layout);
            presentation.apply_fields(settings);
            let widget = presentation.widget();
            let host = self.host.borrow().clone();
            if widget.parent().is_some() {
                match host {
                    LibraryCollectionHost::Embedded(root) => {
                        while let Some(child) = root.first_child() {
                            root.remove(&child);
                        }
                    }
                    LibraryCollectionHost::Scrolled { scroller, .. } => {
                        scroller.set_child(None::<&gtk::Widget>);
                        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
                        root.set_hexpand(true);
                        root.set_vexpand(true);
                        self.host.replace(LibraryCollectionHost::Embedded(root));
                    }
                }
            } else {
                match host {
                    LibraryCollectionHost::Embedded(root) => {
                        while let Some(child) = root.first_child() {
                            root.remove(&child);
                        }
                        widget.set_margin_start(0);
                        widget.set_margin_end(0);
                        root.append(&widget);
                    }
                    LibraryCollectionHost::Scrolled {
                        scroller,
                        margin_start,
                        margin_end,
                        ..
                    } => {
                        widget.set_margin_start(margin_start);
                        widget.set_margin_end(margin_end);
                        scroller.set_child(Some(&widget));
                        presentation.attach_scroller(&scroller);
                    }
                }
            }
            self.presentation.replace(presentation);
        } else {
            self.presentation.borrow().apply_fields(settings);
        }
        *self.settings.borrow_mut() = settings.clone();
    }
}

fn collection_presentation_needs_rebuild(
    previous: &crate::LibraryListSettings,
    current: &crate::LibraryListSettings,
) -> bool {
    previous.layout != current.layout
        || (previous.detail_track_fields != current.detail_track_fields
            && current.layout == LibraryLayout::Detail)
}

fn matching_context_rank(
    context: Option<&crate::shell::route::RouteCurrentTrackContext>,
    base: &str,
    visible: &str,
) -> Option<usize> {
    context
        .filter(|context| context.context_id == base || context.context_id == visible)
        .map(|context| context.source_rank)
}

impl TrackTablePlayingState {
    pub(crate) fn new(model: &TrackCollectionModel, indicator: TrackRowPlayingIndicator) -> Self {
        let handler_model = model.list_model();
        let inner = Rc::new(TrackTablePlayingStateInner {
            model: model.clone(),
            handler_model: handler_model.clone(),
            model_handler: RefCell::new(None),
            track_id: RefCell::new(None),
            position: Cell::new(gtk::INVALID_LIST_POSITION),
            indicator,
        });
        let weak_inner = Rc::downgrade(&inner);
        let handler = handler_model.connect_items_changed(move |_, _, _, _| {
            let Some(inner) = weak_inner.upgrade() else {
                return;
            };
            let track_id = inner.track_id.borrow();
            let position = track_id
                .as_ref()
                .and_then(|track_id| {
                    inner
                        .model
                        .selection_position_after_point_change(track_id, inner.position.get())
                })
                .unwrap_or(gtk::INVALID_LIST_POSITION);
            inner.position.set(position);
            inner.indicator.set_position(position);
        });
        inner.model_handler.replace(Some(handler));
        Self { inner }
    }

    fn set_position(&self, position: u32) {
        self.inner.position.set(position);
        self.inner.indicator.set_position(position);
    }

    fn clear_now_playing(&self) {
        self.inner.track_id.borrow_mut().take();
        self.set_position(gtk::INVALID_LIST_POSITION);
    }

    fn set_track_id(&self, track_id: &TrackKey, position: Option<u32>) {
        *self.inner.track_id.borrow_mut() = Some(track_id.clone());
        let position = position
            .or_else(|| self.inner.model.position_for_current(track_id, None))
            .unwrap_or(gtk::INVALID_LIST_POSITION);
        self.set_position(position);
    }

    pub(crate) fn set_now_playing_track(&self, track_id: Option<&TrackKey>, position: Option<u32>) {
        if let Some(track_id) = track_id {
            self.set_track_id(track_id, position);
        } else {
            self.clear_now_playing();
        }
    }

    pub(crate) fn set_paused(&self, paused: bool) {
        self.inner.indicator.set_paused(paused);
    }

    pub(crate) fn is_bound(&self) -> bool {
        true
    }
}

impl Drop for TrackTablePlayingStateInner {
    fn drop(&mut self) {
        if let Some(handler) = self.model_handler.take() {
            self.handler_model.disconnect(handler);
        }
    }
}

pub(crate) fn library_route_inset(child: gtk::Widget) -> gtk::Widget {
    child.set_margin_start(PRIMARY_ROUTE_MARGIN_START);
    child.set_margin_end(PRIMARY_ROUTE_MARGIN_END);
    child.set_hexpand(true);
    child.set_halign(gtk::Align::Fill);
    child
}
pub(crate) fn configure_library_route_scroller(scroller: &gtk::ScrolledWindow) {
    scroller.add_css_class("library-route-scroller");
    scroller.add_css_class("route-scroll-owner");
    configure_fill_width_clip(scroller, gtk::PolicyType::Automatic);
    scroller.set_propagate_natural_height(false);
    scroller.set_overlay_scrolling(true);
    scroller.set_hexpand(true);
    scroller.set_vexpand(true);
}

pub(crate) fn album_collection_projection(
    shell: &Rc<Shell>,
    models: AlbumCollectionModels,
    key: LibraryListKey,
) -> LibraryCollectionProjection {
    let settings = shell.settings.current.borrow().library_list(key);
    let shell = Rc::clone(shell);
    LibraryCollectionProjection::new(
        settings,
        Rc::new(move |layout| match layout {
            LibraryLayout::Row => {
                LibraryPresentationProjection::Row(album_table(&shell, models.albums(), key, None))
            }
            LibraryLayout::Detail if key.supports_layout(LibraryLayout::Detail) => {
                LibraryPresentationProjection::AlbumDetail(album_detail_list(
                    &shell,
                    models.detail(),
                    key,
                ))
            }
            LibraryLayout::Grid | LibraryLayout::Detail => {
                LibraryPresentationProjection::Grid(album_grid(&shell, &models, key))
            }
        }),
    )
}

pub(crate) fn artist_collection_projection(
    shell: &Rc<Shell>,
    sparse: Rc<super::sparse_model::SparseRouteModel<ArtistKey, ArtistRow>>,
    key: LibraryListKey,
) -> LibraryCollectionProjection {
    let settings = shell.settings.current.borrow().library_list(key);
    let shell = Rc::clone(shell);
    LibraryCollectionProjection::new(
        settings,
        Rc::new(move |layout| match layout {
            LibraryLayout::Row => {
                LibraryPresentationProjection::Row(artist_table(&shell, sparse.list_model(), key))
            }
            LibraryLayout::Grid | LibraryLayout::Detail => {
                LibraryPresentationProjection::Grid(artist_grid(&shell, &sparse, key))
            }
        }),
    )
}
pub(crate) fn track_collection_projection(
    shell: &Rc<Shell>,
    model: TrackCollectionModel,
    key: LibraryListKey,
    settings: crate::LibraryListSettings,
    context_id: String,
    content_inset: i32,
) -> LibraryCollectionProjection {
    let shell = Rc::clone(shell);
    let play_model = model.clone();
    let play_queue = shell.products.playback.queue.clone();
    let play_context_id = context_id.clone();
    let play_from_collection = Rc::new(move |position: u32| {
        let Some(request) = play_model.play_request(
            position as usize,
            playback::QueuePlacement::Now,
            &play_context_id,
            false,
        ) else {
            return;
        };
        play_queue.play_loaded(request);
    }) as Rc<dyn Fn(u32)>;
    LibraryCollectionProjection::new(
        settings,
        Rc::new(move |layout| match layout {
            LibraryLayout::Grid => LibraryPresentationProjection::Grid(track_grid(
                &shell,
                model.clone(),
                key,
                Rc::clone(&play_from_collection),
            )),
            LibraryLayout::Row | LibraryLayout::Detail => {
                LibraryPresentationProjection::Row(track_table(
                    &shell,
                    model.clone(),
                    key,
                    TrackTableOptions {
                        detail: false,
                        context_id: context_id.clone(),
                        content_inset,
                    },
                ))
            }
        }),
    )
}

pub(crate) struct TrackTableOptions {
    pub(crate) detail: bool,
    pub(crate) context_id: String,
    pub(crate) content_inset: i32,
}

pub(crate) fn album_grid(
    shell: &Rc<Shell>,
    models: &AlbumCollectionModels,
    key: LibraryListKey,
) -> CollectionGridProjection {
    let settings = shell.settings.current.borrow().library_list(key);
    let fields = settings.grid_fields;
    let sparse = models
        .album_rows()
        .expect("Album grid has an Album row sparse model");
    let cell_shell = Rc::clone(shell);
    let activate_shell = Rc::clone(shell);
    let demand = Rc::clone(&sparse);
    collection_grid_with_demand(
        sparse.list_model(),
        &fields,
        move |fields| AlbumGridCell::new(&cell_shell, fields, None),
        move |_, album: AlbumRow| activate_shell.navigate(Route::AlbumDetail(album.album_key)),
        move |position| demand.demand_positions([position as usize]),
    )
}

pub(crate) fn artist_grid(
    shell: &Rc<Shell>,
    sparse: &Rc<super::sparse_model::SparseRouteModel<ArtistKey, ArtistRow>>,
    key: LibraryListKey,
) -> CollectionGridProjection {
    let fields = shell
        .settings
        .current
        .borrow()
        .library_list(key)
        .grid_fields;
    let album_artist = key == LibraryListKey::AlbumArtists;
    let cell_shell = Rc::clone(shell);
    let activate_shell = Rc::clone(shell);
    let demand = Rc::clone(sparse);
    collection_grid_with_demand(
        sparse.list_model(),
        &fields,
        move |fields| ArtistGridCell::new(&cell_shell, fields, album_artist),
        move |_, artist: ArtistRow| {
            activate_shell.navigate(super::artist::artist_detail_route(
                artist.artist_key,
                album_artist,
            ));
        },
        move |position| demand.demand_positions([position as usize]),
    )
}
pub(crate) fn track_grid(
    shell: &Rc<Shell>,
    model: TrackCollectionModel,
    key: LibraryListKey,
    play_from_collection: Rc<dyn Fn(u32)>,
) -> CollectionGridProjection {
    let fields = shell
        .settings
        .current
        .borrow()
        .library_list(key)
        .grid_fields;
    let sparse = model.sparse_model();
    let field = Rc::new(move |_: u32, track: &TrackRow, field| {
        if key == LibraryListKey::History && field == LibraryField::LastPlayed {
            DetailLinks::text(
                &track
                    .last_played
                    .map(super::track_model::history_played_at_text)
                    .unwrap_or_default(),
            )
        } else {
            track_grid_field_links(track, field)
        }
    });
    let cell_shell = Rc::clone(shell);
    let cell_play = Rc::clone(&play_from_collection);
    let activate = Rc::clone(&play_from_collection);
    let demand = Rc::clone(&sparse);
    collection_grid_with_demand(
        sparse.list_model(),
        &fields,
        move |fields| TrackGridCell::new(&cell_shell, fields, Rc::clone(&cell_play), field.clone()),
        move |position, _: TrackRow| activate(position),
        move |position| demand.demand_positions([position as usize]),
    )
}
pub(crate) fn album_table<M>(
    shell: &Rc<Shell>,
    model: M,
    key: LibraryListKey,
    playback_context: Option<String>,
) -> CollectionTableProjection
where
    M: IsA<gio::ListModel> + Clone + 'static,
{
    let fields = shell.settings.current.borrow().library_list(key).row_fields;
    let activate_shell = Rc::clone(shell);
    let column_shell = Rc::clone(shell);
    let column_playback_context = playback_context;
    dynamic_collection_table(
        shell,
        key,
        model,
        &fields,
        Vec::new(),
        move |field| album_column(&column_shell, field, column_playback_context.clone()),
        |field| column_fit_width(field, column_width(field)),
        true,
        Some(Box::new(move |_, album: AlbumRow| {
            activate_shell.navigate(Route::AlbumDetail(album.album_key));
        })),
        None,
        route_column_view_initial_width(shell),
    )
}
pub(crate) fn artist_table<M>(
    shell: &Rc<Shell>,
    model: M,
    key: LibraryListKey,
) -> CollectionTableProjection
where
    M: IsA<gio::ListModel> + Clone + 'static,
{
    let fields = shell.settings.current.borrow().library_list(key).row_fields;
    let activate_shell = Rc::clone(shell);
    let column_shell = Rc::clone(shell);
    let album_artist = key == LibraryListKey::AlbumArtists;
    dynamic_collection_table(
        shell,
        key,
        model,
        &fields,
        Vec::new(),
        move |field| artist_column(&column_shell, field, album_artist),
        |field| column_fit_width(field, column_width(field)),
        true,
        Some(Box::new(move |_, artist: ArtistRow| {
            activate_shell.navigate(super::artist::artist_detail_route(
                artist.artist_key,
                album_artist,
            ));
        })),
        None,
        route_column_view_initial_width(shell),
    )
}
pub(crate) fn playlist_table<M>(shell: &Rc<Shell>, model: M) -> CollectionTableProjection
where
    M: IsA<gio::ListModel> + Clone + 'static,
{
    let fields = shell
        .settings
        .current
        .borrow()
        .library_list(LibraryListKey::Playlists)
        .row_fields;
    let activate_shell = Rc::clone(shell);
    let column_shell = Rc::clone(shell);
    dynamic_collection_table(
        shell,
        LibraryListKey::Playlists,
        model,
        &fields,
        Vec::new(),
        move |field| playlist_column(&column_shell, field),
        |field| column_fit_width(field, playlist_column_width(field)),
        true,
        Some(Box::new(move |_, playlist: PlaylistRow| {
            activate_shell.navigate(Route::PlaylistDetail(playlist.playlist_key.clone()));
        })),
        None,
        route_column_view_initial_width(shell),
    )
}
pub(crate) fn smart_playlist_table<M>(shell: &Rc<Shell>, model: M) -> CollectionTableProjection
where
    M: IsA<gio::ListModel> + Clone + 'static,
{
    let fields = shell
        .settings
        .current
        .borrow()
        .library_list(LibraryListKey::SmartPlaylists)
        .row_fields;
    let reorder_column = smart_playlist_reorder_column(shell);
    let activate_shell = Rc::clone(shell);
    let column_shell = Rc::clone(shell);
    dynamic_collection_table(
        shell,
        LibraryListKey::SmartPlaylists,
        model,
        &fields,
        vec![(reorder_column, SMART_PLAYLIST_REORDER_WIDTH)],
        move |field| smart_playlist_column(&column_shell, field),
        |field| column_fit_width(field, playlist_column_width(field)),
        true,
        Some(Box::new(move |_, playlist: SmartPlaylistRow| {
            activate_shell.navigate(Route::SmartPlaylistDetail(
                playlist.smart_playlist_key.clone(),
            ));
        })),
        None,
        route_column_view_initial_width(shell),
    )
}

fn playlist_column_width(field: LibraryField) -> i32 {
    if matches!(field, LibraryField::Title | LibraryField::TitleMerged) {
        220
    } else {
        collection_column_width(field)
    }
}

pub(super) fn collection_column_width(field: LibraryField) -> i32 {
    match field {
        LibraryField::AlbumCount | LibraryField::SongCount => {
            compact_header_column_width(field.title(), 96)
        }
        LibraryField::Duration => compact_header_column_width(field.title(), 128),
        _ => column_width(field),
    }
}

pub(super) fn dynamic_collection_table<T, M>(
    _shell: &Rc<Shell>,
    _key: LibraryListKey,
    model: M,
    fields: &[LibraryField],
    fixed_columns: Vec<(gtk::ColumnViewColumn, i32)>,
    column_for_field: impl Fn(LibraryField) -> gtk::ColumnViewColumn + 'static,
    width_for_field: impl Fn(LibraryField) -> i32 + 'static,
    single_click_activate: bool,
    activate: Option<Box<dyn Fn(u32, T)>>,
    selection: Option<gtk::SingleSelection>,
    initial_width: i32,
) -> CollectionTableProjection
where
    T: Clone + 'static,
    M: IsA<gio::ListModel> + Clone + 'static,
{
    let width_for_field = Rc::new(width_for_field) as Rc<dyn Fn(LibraryField) -> i32>;
    let column_width_for_field = Rc::clone(&width_for_field);
    let column_for_field = Rc::new(move |field| {
        let column = column_for_field(field);
        let width = column_width_for_field(field);
        (column, width)
    }) as Rc<dyn Fn(LibraryField) -> (gtk::ColumnViewColumn, i32)>;
    let mut active = fixed_columns.clone();
    active.extend(fields.iter().map(|field| column_for_field(*field)));
    let (table, width_fit, navigation) = collection_table_with_width(
        model,
        active,
        initial_width,
        single_click_activate,
        activate,
        selection,
    );
    let fields = Rc::new(RefCell::new(fields.to_vec()));
    CollectionTableProjection {
        table,
        navigation,
        fixed_columns: Rc::new(fixed_columns),
        column_for_field,
        width_for_field,
        fields,
        width_fit,
    }
}

fn collection_table_with_width<T, M>(
    model: M,
    columns: Vec<(gtk::ColumnViewColumn, i32)>,
    initial_width: i32,
    single_click_activate: bool,
    activate: Option<Box<dyn Fn(u32, T)>>,
    selection: Option<gtk::SingleSelection>,
) -> (
    gtk::ColumnView,
    ColumnViewWidthFit,
    MountedRouteItemNavigation,
)
where
    T: Clone + 'static,
    M: IsA<gio::ListModel> + Clone + 'static,
{
    let selection = selection.unwrap_or_else(|| {
        let selection = gtk::SingleSelection::new(Some(model.clone()));
        selection.set_autoselect(false);
        selection.set_can_unselect(true);
        selection
    });
    let table = gtk::ColumnView::new(Some(selection.clone()));
    table.add_css_class("track-table");
    table.set_single_click_activate(single_click_activate);
    let row_factory = gtk::SignalListItemFactory::new();
    row_factory.connect_setup(|_, row| {
        if let Some(row) = row.downcast_ref::<gtk::ColumnViewRow>() {
            row.set_activatable(true);
        }
    });
    table.set_row_factory(Some(&row_factory));
    if single_click_activate {
        restore_single_click_activation_on_primary_press(&table, |table| {
            table.set_single_click_activate(true);
        });
    }
    table.set_vscroll_policy(gtk::ScrollablePolicy::Minimum);
    table.set_hexpand(true);
    table.set_vexpand(true);
    for (column, _) in &columns {
        table.append_column(column);
    }
    let width_fit = install_column_view_width_fit(&table, columns, initial_width);
    table.connect_activate(move |_, position| {
        if let Some(activate) = activate.as_ref()
            && let Some(value) = item_at::<T>(&model, position)
        {
            activate(position, value);
        }
    });
    let navigation_table = table.clone();
    let navigation_selection = selection;
    let navigation = Rc::new(move |direction| {
        if !matches!(direction, gtk::DirectionType::Up | gtk::DirectionType::Down) {
            return glib::Propagation::Stop;
        }
        if single_click_activate {
            navigation_table.set_single_click_activate(false);
        }
        if navigation_table
            .state_flags()
            .contains(gtk::StateFlags::FOCUS_WITHIN)
        {
            return glib::Propagation::Proceed;
        }
        let Some(model) = navigation_table.model() else {
            return glib::Propagation::Stop;
        };
        if let Some(position) = item_navigation_entry_position(
            navigation_selection.selected(),
            model.n_items(),
            direction,
        ) {
            navigation_selection.set_selected(position);
            navigation_table.scroll_to(position, None, gtk::ListScrollFlags::FOCUS, None);
            navigation_table.grab_focus();
        }
        glib::Propagation::Stop
    }) as MountedRouteItemNavigation;
    (table, width_fit, navigation)
}

pub(crate) fn smart_playlist_reorder_column(shell: &Rc<Shell>) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let shell = Rc::clone(shell);
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(playlist) = item_at_from_item::<SmartPlaylistRow>(item) else {
            return;
        };
        let handle = smart_playlist_drag_handle(&playlist.smart_playlist_key);
        install_smart_playlist_drop_target(&handle, &shell, &playlist.smart_playlist_key);
        item.set_child(Some(&handle));
    });
    factory.connect_unbind(clear_list_item_child);
    let column = gtk::ColumnViewColumn::new(None::<&str>, Some(factory));
    column.set_fixed_width(SMART_PLAYLIST_REORDER_WIDTH);
    column
}
pub(crate) fn track_table(
    shell: &Rc<Shell>,
    model: TrackCollectionModel,
    key: LibraryListKey,
    options: TrackTableOptions,
) -> CollectionTableProjection {
    let selection = gtk::SingleSelection::new(Some(model.list_model()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    selection.set_selected(gtk::INVALID_LIST_POSITION);
    let playing_indicator = TrackRowPlayingIndicator::new();
    let playing_state = TrackTablePlayingState::new(&model, playing_indicator.clone());
    let source_id = shell
        .selected_library()
        .as_deref()
        .map(|selected| selected.source_key);
    let selection_context_base = options.context_id.clone();
    let selection_model = model.clone();
    let current_playing_state = playing_state.clone();
    shell.register_current_route_track_selection(Rc::new(move |current| {
        if !current_playing_state.is_bound() {
            return false;
        }
        let current = current.filter(|current| source_id.as_ref() == Some(&current.source_id));
        let position = current.and_then(|current| {
            let expected_context_id = selection_model.visible_context_id(&selection_context_base);
            let source_rank = matching_context_rank(
                current.context.as_ref(),
                &selection_context_base,
                &expected_context_id,
            );
            selection_model.position_for_current(&current.track_id, source_rank)
        });
        current_playing_state
            .set_now_playing_track(current.map(|current| &current.track_id), position);
        current_playing_state.set_paused(current.is_some_and(|current| current.paused));
        true
    }));
    let fields = if options.detail {
        shell
            .settings
            .current
            .borrow()
            .library_list(key)
            .detail_track_fields
    } else {
        shell.settings.current.borrow().library_list(key).row_fields
    };
    let queue = shell.products.playback.queue.clone();
    let play_model = model.clone();
    let context_id = options.context_id;
    let activate = Box::new(move |position, _: TrackRow| {
        if let Some(request) = play_model.play_request(
            position as usize,
            playback::QueuePlacement::Now,
            &context_id,
            false,
        ) {
            queue.play_loaded(request);
        }
    });
    let column_shell = Rc::clone(shell);
    let column_playing_indicator = playing_indicator;
    let table = dynamic_collection_table(
        shell,
        key,
        model.list_model(),
        &fields,
        Vec::new(),
        move |field| {
            if key == LibraryListKey::History && field == LibraryField::LastPlayed {
                track_position_text_column(
                    &column_shell,
                    field.title(),
                    track_column_fit_width(key, field),
                    0.0,
                    None,
                    move |_, track| {
                        track
                            .last_played
                            .map(super::track_model::history_played_at_text)
                            .unwrap_or_default()
                    },
                )
            } else {
                track_column_for_key(&column_shell, key, field, &column_playing_indicator)
            }
        },
        move |field| track_column_fit_width(key, field),
        false,
        Some(activate),
        Some(selection),
        column_view_initial_width(shell, options.content_inset),
    );
    table.table.add_css_class("track-list");
    table
}
pub(crate) fn set_library_table_content_height(
    scroller: &gtk::ScrolledWindow,
    row_count: usize,
    max_visible_rows: Option<usize>,
) {
    let height = max_visible_rows.map_or_else(
        || library_table_content_height(row_count),
        |max_visible_rows| capped_library_table_content_height(row_count, Some(max_visible_rows)),
    );
    scroller.set_min_content_height(height);
    scroller.set_max_content_height(height);
}
pub(crate) fn library_table_content_height(row_count: usize) -> i32 {
    capped_library_table_content_height(row_count, None)
}
pub(crate) fn capped_library_table_content_height(
    row_count: usize,
    max_visible_rows: Option<usize>,
) -> i32 {
    let max_rows = ((i32::MAX - LIBRARY_TABLE_HEADER_HEIGHT) / LIBRARY_TABLE_ROW_HEIGHT) as usize;
    let visible_rows = row_count.max(1).min(max_visible_rows.unwrap_or(max_rows));
    LIBRARY_TABLE_HEADER_HEIGHT + visible_rows as i32 * LIBRARY_TABLE_ROW_HEIGHT
}
pub(crate) fn smart_playlist_drag_handle(playlist_id: &SmartPlaylistKey) -> gtk::Image {
    let drag = gtk::Image::from_icon_name("rufin-list-drag-handle-symbolic");
    drag.add_css_class("dim-label");
    drag.set_tooltip_text(Some(&tr("Drag to reorder")));
    drag.set_width_request(SMART_PLAYLIST_REORDER_WIDTH);
    drag.set_halign(gtk::Align::Center);
    let source = gtk::DragSource::builder()
        .actions(gtk::gdk::DragAction::MOVE)
        .build();
    let drag_id = playlist_id.to_string();
    source.connect_prepare(move |_, _, _| {
        Some(gtk::gdk::ContentProvider::for_value(&drag_id.to_value()))
    });
    drag.add_controller(source);
    drag
}

pub(crate) fn install_smart_playlist_drop_target(
    target: &impl IsA<gtk::Widget>,
    shell: &Rc<Shell>,
    target_id: &SmartPlaylistKey,
) {
    let widget = target.as_ref().downgrade();
    let shell = Rc::clone(shell);
    let target_id = *target_id;
    let drop_target = gtk::DropTarget::new(String::static_type(), gtk::gdk::DragAction::MOVE);
    drop_target.connect_drop(move |_, value, _, y| {
        let Ok(dragged_id) = value.get::<String>() else {
            return false;
        };
        let Ok(dragged_id) = dragged_id.parse::<i64>() else {
            return false;
        };
        let dragged_id = SmartPlaylistKey::from_raw(dragged_id);
        if dragged_id == target_id {
            return false;
        }
        let Some(widget) = widget.upgrade() else {
            return false;
        };
        let after = y > f64::from(widget.height()) / 2.0;
        shell.publish_smart_playlist_change(
            SmartPlaylistChange::Move {
                dragged: dragged_id,
                target: target_id,
                after,
            },
            None,
        );
        true
    });
    target.add_controller(drop_target);
}
fn collection_grid_field_class(field: LibraryField) -> &'static str {
    match field {
        LibraryField::Artist | LibraryField::AlbumArtist => "artist-label",
        _ => "muted",
    }
}

pub(super) fn collection_grid_field_label(
    value: &str,
    field: LibraryField,
) -> (gtk::Widget, gtk::Label) {
    grid_label_with_label(value, collection_grid_field_class(field))
}

pub(super) fn track_grid_field_links(track: &TrackRow, field: LibraryField) -> DetailLinks {
    match field {
        LibraryField::Artist => track_artist_links(track),
        LibraryField::AlbumArtist => track_album_artist_links(track),
        LibraryField::Album => DetailLinks::route(
            &track.display_album,
            track.album_key.clone().map(Route::AlbumDetail),
        ),
        _ => DetailLinks::text(&track_field(track, field)),
    }
}

pub(super) fn collection_grid_card() -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, COLLECTION_GRID_CARD_GAP);
    card.set_hexpand(true);
    card.set_vexpand(false);
    card.set_halign(gtk::Align::Fill);
    card.set_valign(gtk::Align::Start);
    card
}

#[cfg(test)]
mod layout_policy_tests {
    use super::*;

    #[test]
    fn contextual_playback_target_uses_the_route_context() {
        let target = PlaybackTarget::Album(AlbumKey::from_raw(1)).in_context("artist:1|releases");

        assert_eq!(target.context_id(), "artist:1|releases");
    }

    #[test]
    fn playing_selection_accepts_base_and_decorated_route_contexts() {
        let base = "album:7";
        let visible = "album:7|query=|sort=Position|descending=false";
        for context_id in [base, visible] {
            assert_eq!(
                matching_context_rank(
                    Some(&crate::shell::route::RouteCurrentTrackContext {
                        context_id: context_id.to_string(),
                        source_rank: 3,
                    }),
                    base,
                    visible,
                ),
                Some(3)
            );
        }
    }

    #[test]
    fn collection_presentation_rebuilds_only_for_layout_owned_changes() {
        let row = crate::LibraryListSettings::for_key(LibraryListKey::Tracks);
        assert!(!collection_presentation_needs_rebuild(&row, &row));

        let mut grid = row.clone();
        grid.layout = LibraryLayout::Grid;
        assert!(collection_presentation_needs_rebuild(&row, &grid));

        let mut unused_detail_fields = row.clone();
        unused_detail_fields.detail_track_fields = vec![LibraryField::Title];
        assert!(!collection_presentation_needs_rebuild(
            &row,
            &unused_detail_fields
        ));

        let mut detail = crate::LibraryListSettings::for_key(LibraryListKey::Albums);
        detail.layout = LibraryLayout::Detail;
        let mut changed_detail = detail.clone();
        changed_detail.detail_track_fields = vec![LibraryField::Title];
        assert!(collection_presentation_needs_rebuild(
            &detail,
            &changed_detail
        ));
    }
}
