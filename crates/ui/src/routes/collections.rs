use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};

use ::library::{
    AlbumRow, ArtistKey, ArtistRow, GenreKey, MoodKey, PlaylistKey, PlaylistRow, SmartPlaylistKey,
    SmartPlaylistRow, TrackRow,
};
use adw::prelude::*;
use gtk::{gio, glib};

use crate::layout::{allocation_owner, configure_fill_width_clip, width_allocation_owner};
use crate::preferences::dialogs::SmartPlaylistChange;
use crate::shell::Shell;
use crate::shell::route::{MountedRouteItemNavigation, item_navigation_entry_position};
use crate::{LibraryField, LibraryLayout, LibraryListKey};
use playback::{PlayRequest, QueuePlacement};
use tracing::warn;

use super::album_detail::{AlbumCollectionModels, AlbumDetailVirtualList, album_detail_list};
use super::columns::{
    TrackRowPlayingIndicator, album_column, artist_column, column_fit_width, playlist_column,
    smart_playlist_column, song_column_for_key, track_column_fit_width,
};
use super::grid_cells::{
    AlbumGridCell, ArtistGridCell, CollectionGridProjection, TrackGridCell,
    collection_grid_with_demand,
};
use super::library_fields::TrackPresentation;
use super::library_fields::{
    column_width, compact_header_column_width, grid_label_with_label, item_at, item_at_from_item,
};
use super::playlist_picker::install_track_drag_source;
use super::route::Route;
use super::route_layout::{PRIMARY_ROUTE_MARGIN_END, PRIMARY_ROUTE_MARGIN_START};
use super::table_sizing::{
    ColumnViewWidthFit, column_view_initial_width, install_column_view_width_fit,
    route_column_view_initial_width,
};
use super::track_model::TrackCollectionModel;
use super::track_selection::TrackSelection;
use downloads::DownloadSubject;

pub(crate) type CollectionPlay = Rc<dyn Fn(QueuePlacement)>;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PlaybackTarget {
    Track(String),
    Album(String),
    AlbumKey(library::AlbumKey),
    Artist(String),
    AlbumArtist(String),
    ArtistKey(library::ArtistKey, bool),
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
        let selected = shell.selected_library();
        let scope = selected.as_ref().map(|selected| selected.source_key);
        let folder = selected
            .as_ref()
            .and_then(|selected| selected.music_folder_key);
        let database = Arc::clone(&shell.products.library);
        let target = self.clone();
        let source = shell.products.source.clone();
        shell.products.runtime.spawn(async move {
            match target.resolve_media_uris(&database, scope, folder).await {
                Ok(media_uris) => {
                    source.download_media(target.download_subject(), media_uris);
                }
                Err(error) => warn!(target = ?target, %error, "failed to identify download tracks"),
            }
        });
    }

    pub(crate) fn remove_download(&self, shell: &Shell) {
        let selected = shell.selected_library();
        let scope = selected.as_ref().map(|selected| selected.source_key);
        let folder = selected
            .as_ref()
            .and_then(|selected| selected.music_folder_key);
        let database = Arc::clone(&shell.products.library);
        let target = self.clone();
        let downloads = shell.products.downloads.clone();
        shell.products.runtime.spawn(async move {
            match target.resolve_media_uris(&database, scope, folder).await {
                Ok(order) => downloads.remove(order, true),
                Err(error) => {
                    warn!(target = ?target, %error, "failed to identify downloaded tracks")
                }
            }
        });
    }

    pub(crate) fn download_subject(&self) -> DownloadSubject {
        let title = match self {
            Self::Track(_) => localization::tr("Track"),
            Self::Album(_) | Self::AlbumKey(_) => localization::tr("Album"),
            Self::Artist(_) | Self::AlbumArtist(_) | Self::ArtistKey(..) => {
                localization::tr("Artist")
            }
            Self::Genre(_) => localization::tr("Genre"),
            Self::Mood(_) => localization::tr("Mood"),
            Self::Playlist(_) => localization::tr("Playlist"),
            Self::SmartPlaylist(_) => localization::tr("Smart Playlist"),
            Self::Contextual { target, .. } => return target.download_subject(),
        };
        DownloadSubject::Prepared {
            context_id: self.context_id(),
            title: Some(title),
        }
    }

    pub(crate) fn context_id(&self) -> String {
        match self {
            Self::Track(id) => format!("track:{id}"),
            Self::Album(id) => format!("album:{id}"),
            Self::AlbumKey(id) => format!("album-key:{id}"),
            Self::Artist(id) => format!("artist:{id}"),
            Self::AlbumArtist(id) => format!("album-artist:{id}"),
            Self::ArtistKey(id, album_artist) => format!("artist-key:{id}:{album_artist}"),
            Self::Genre(id) => format!("genre:{id}"),
            Self::Mood(id) => format!("mood:{id}"),
            Self::Playlist(id) => format!("playlist:{id}"),
            Self::SmartPlaylist(id) => format!("smart-playlist:{id}"),
            Self::Contextual { context_id, .. } => context_id.clone(),
        }
    }

    pub(crate) fn play(&self, shell: &Shell, placement: QueuePlacement) {
        let selected = shell.selected_library();
        let source = selected.as_ref().map(|selected| selected.source_key);
        let folder = selected
            .as_ref()
            .and_then(|selected| selected.music_folder_key);
        shell.products.playback.queue.play(PlayRequest::ordered(
            self.queue_input(source, folder),
            0,
            placement,
            true,
        ));
    }

    pub(crate) async fn resolve_media_uris(
        &self,
        database: &library::Database,
        source_key: Option<library::SourceKey>,
        folder: Option<library::FolderKey>,
    ) -> Result<Vec<String>, String> {
        let cancellation = library::ReadCancellation::new();
        let target = match self {
            Self::Contextual { target, .. } => target.as_ref(),
            target => target,
        };
        match (target, source_key) {
            (Self::AlbumKey(key), Some(source)) => database
                .album_track_route_page(
                    source,
                    *key,
                    folder,
                    "",
                    library::TrackSort::TrackNumber,
                    false,
                    library::RouteSeedWindow::top(),
                    &cancellation,
                )
                .await
                .map(|page| page.order)
                .map_err(|error| error.to_string()),
            (Self::ArtistKey(key, album_artist), Some(source)) => database
                .artist_track_route_page(
                    source,
                    *key,
                    *album_artist,
                    folder,
                    "",
                    library::TrackSort::Title,
                    false,
                    false,
                    library::RouteSeedWindow::top(),
                    &cancellation,
                )
                .await
                .map(|page| page.order)
                .map_err(|error| error.to_string()),
            (Self::Track(media_uri), _) => Ok(vec![media_uri.clone()]),
            (Self::Album(uri), _) => database
                .album_detail(uri, library::TrackSort::TrackNumber, false, &cancellation)
                .await
                .map(|detail| detail.map(|detail| detail.track_order).unwrap_or_default())
                .map_err(|error| error.to_string()),
            (Self::Artist(uri) | Self::AlbumArtist(uri), _) => {
                let Some(row) = database
                    .artist_row_by_media_uri(uri, &cancellation)
                    .await
                    .map_err(|error| error.to_string())?
                else {
                    return Ok(Vec::new());
                };
                database
                    .artist_track_route_page(
                        row.source_key,
                        row.artist_key,
                        matches!(target, Self::AlbumArtist(_)),
                        None,
                        "",
                        library::TrackSort::Title,
                        false,
                        false,
                        library::RouteSeedWindow::top(),
                        &cancellation,
                    )
                    .await
                    .map(|page| page.order)
                    .map_err(|error| error.to_string())
            }
            (Self::Genre(key), Some(source_key)) => database
                .genre_track_route_page(
                    source_key,
                    *key,
                    folder,
                    "",
                    library::TrackSort::Title,
                    false,
                    library::RouteSeedWindow::top(),
                    &cancellation,
                )
                .await
                .map(|page| page.order)
                .map_err(|error| error.to_string()),
            (Self::Mood(key), Some(source_key)) => database
                .mood_track_route_page(
                    source_key,
                    *key,
                    folder,
                    "",
                    library::TrackSort::Title,
                    false,
                    library::RouteSeedWindow::top(),
                    &cancellation,
                )
                .await
                .map(|page| page.order)
                .map_err(|error| error.to_string()),
            (Self::Playlist(key), _) => database
                .playlist_media_uri_order(*key, folder, &cancellation)
                .await
                .map_err(|error| error.to_string()),
            (Self::SmartPlaylist(key), _) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |duration| duration.as_secs().min(i64::MAX as u64) as i64);
                database
                    .smart_playlist_media_uri_order(source_key, *key, folder, now, &cancellation)
                    .await
                    .map_err(|error| error.to_string())
            }
            (Self::Contextual { .. }, _) => unreachable!(),
            (_, None) => Ok(Vec::new()),
        }
    }

    pub(crate) fn queue_input(
        &self,
        source: Option<library::SourceKey>,
        folder: Option<library::FolderKey>,
    ) -> library::QueueInput {
        let target = match self {
            Self::Contextual { target, .. } => target.as_ref(),
            target => target,
        };
        let context_id = self.context_id().into();
        if let Self::SmartPlaylist(key) = target {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs().min(i64::MAX as u64) as i64);
            return library::QueueInput::Smart {
                key: *key,
                source,
                folder,
                now,
                context_id,
            };
        }
        let collection = match target {
            Self::Album(uri) => library::QueueCollection::Album(uri.clone()),
            Self::AlbumKey(key) => library::QueueCollection::AlbumKey(*key),
            Self::ArtistKey(key, album_artist) => library::QueueCollection::ArtistKey {
                key: *key,
                album_artist: *album_artist,
            },
            Self::Artist(uri) | Self::AlbumArtist(uri) => library::QueueCollection::Artist {
                media_uri: uri.clone(),
                album_artist: matches!(target, Self::AlbumArtist(_)),
            },
            Self::Genre(key) => library::QueueCollection::Genre(*key),
            Self::Mood(key) => library::QueueCollection::Mood(*key),
            Self::Playlist(key) => library::QueueCollection::Playlist(*key),
            Self::Track(uri) => {
                return library::QueueInput::Uris {
                    order: Arc::from([uri.clone()]),
                    context_id,
                    source_start: 0,
                };
            }
            Self::SmartPlaylist(_) | Self::Contextual { .. } => unreachable!(),
        };
        library::QueueInput::Collection {
            collection,
            folder,
            context_id,
        }
    }
}

struct TrackTablePlayingStateInner<T: TrackPresentation> {
    model: TrackCollectionModel<T>,
    handler_model: super::sparse_model::SparseObjectModel,
    model_handler: RefCell<Option<glib::SignalHandlerId>>,
    track_id: RefCell<Option<String>>,
    position: Cell<u32>,
    indicator: TrackRowPlayingIndicator,
}

#[derive(Clone)]
pub(crate) struct TrackTablePlayingState<T: TrackPresentation = TrackRow> {
    inner: Rc<TrackTablePlayingStateInner<T>>,
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
        let presentation = self.presentation.borrow();
        let active = presentation.widget();
        let row = matches!(&*presentation, LibraryPresentationProjection::Row(_));
        drop(presentation);
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
        active.set_margin_start(if row { 0 } else { margin_start });
        active.set_margin_end(if row { 0 } else { margin_end });
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
                        let row = matches!(&presentation, LibraryPresentationProjection::Row(_));
                        widget.set_margin_start(if row { 0 } else { margin_start });
                        widget.set_margin_end(if row { 0 } else { margin_end });
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

impl<T: TrackPresentation> TrackTablePlayingState<T> {
    pub(crate) fn new(
        model: &TrackCollectionModel<T>,
        indicator: TrackRowPlayingIndicator,
    ) -> Self {
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

    fn set_track_id(&self, media_uri: &str, position: Option<u32>) {
        *self.inner.track_id.borrow_mut() = Some(media_uri.to_string());
        let position = position
            .or_else(|| self.inner.model.position_for_current_uri(media_uri, None))
            .unwrap_or(gtk::INVALID_LIST_POSITION);
        self.set_position(position);
    }

    pub(crate) fn set_now_playing_track(&self, media_uri: Option<&str>, position: Option<u32>) {
        if let Some(media_uri) = media_uri {
            self.set_track_id(media_uri, position);
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

impl<T: TrackPresentation> Drop for TrackTablePlayingStateInner<T> {
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
pub(crate) fn track_collection_projection<T: TrackPresentation>(
    shell: &Rc<Shell>,
    model: TrackCollectionModel<T>,
    key: LibraryListKey,
    settings: crate::LibraryListSettings,
    context_id: String,
    content_inset: i32,
) -> LibraryCollectionProjection {
    let shell = Rc::clone(shell);
    let selection = TrackSelection::new(model.clone());
    shell.register_current_route_track_selection_owner(selection.clone());
    let play_model = model.clone();
    let play_queue = shell.products.playback.queue.clone();
    let play_context_id = context_id.clone();
    let play_from_collection = Rc::new(move |position: u32| {
        play_model.play(
            play_queue.clone(),
            position as usize,
            playback::QueuePlacement::Now,
            &play_context_id,
            false,
        );
    }) as Rc<dyn Fn(u32)>;
    LibraryCollectionProjection::new(
        settings,
        Rc::new(move |layout| match layout {
            LibraryLayout::Grid => LibraryPresentationProjection::Grid(track_grid(
                &shell,
                model.clone(),
                key,
                Rc::clone(&play_from_collection),
                selection.clone(),
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
                    selection.clone(),
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
        move |_, album: AlbumRow| {
            activate_shell.navigate(Route::AlbumDetail(album.media_uri.clone()))
        },
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
                artist.media_uri.clone(),
                album_artist,
            ));
        },
        move |position| demand.demand_positions([position as usize]),
    )
}
pub(crate) fn track_grid<T: TrackPresentation>(
    shell: &Rc<Shell>,
    model: TrackCollectionModel<T>,
    key: LibraryListKey,
    play_from_collection: Rc<dyn Fn(u32)>,
    selection: TrackSelection,
) -> CollectionGridProjection {
    let sparse = model.sparse_model();
    let demand = Rc::clone(&sparse);
    song_grid::<_, T>(
        shell,
        sparse.list_model(),
        key,
        play_from_collection,
        selection,
        Rc::new(move |position| demand.demand_positions([position as usize])),
    )
}

fn song_grid<M, T: TrackPresentation>(
    shell: &Rc<Shell>,
    model: M,
    key: LibraryListKey,
    play_from_collection: Rc<dyn Fn(u32)>,
    selection: TrackSelection,
    demand: Rc<dyn Fn(u32)>,
) -> CollectionGridProjection
where
    M: IsA<gio::ListModel> + Clone + 'static,
{
    let fields = shell
        .settings
        .current
        .borrow()
        .library_list(key)
        .grid_fields;
    let field = Rc::new(move |_: u32, item: &T, field| item.links(field));
    let cell_shell = Rc::clone(shell);
    let cell_play = Rc::clone(&play_from_collection);
    let activate = Rc::clone(&play_from_collection);
    let bind_demand = Rc::clone(&demand);
    super::grid_cells::collection_grid_with_selection_and_demand(
        model,
        Some(selection.selection_model()),
        &fields,
        move |fields| {
            TrackGridCell::new(
                &cell_shell,
                fields,
                Rc::clone(&cell_play),
                field.clone(),
                Some(selection.clone()),
            )
        },
        move |position, _: T| activate(position),
        move |position| bind_demand(position),
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
            activate_shell.navigate(Route::AlbumDetail(album.media_uri.clone()));
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
                artist.media_uri.clone(),
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
    let activate_shell = Rc::clone(shell);
    let column_shell = Rc::clone(shell);
    dynamic_collection_table(
        shell,
        LibraryListKey::SmartPlaylists,
        model,
        &fields,
        Vec::new(),
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
    _single_click_activate: bool,
    activate: Option<Box<dyn Fn(u32, T)>>,
    selection: Option<gtk::SelectionModel>,
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
    let (table, width_fit, navigation) =
        collection_table_with_width(model, active, initial_width, false, activate, selection);
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
    _single_click_activate: bool,
    activate: Option<Box<dyn Fn(u32, T)>>,
    selection: Option<gtk::SelectionModel>,
) -> (
    gtk::ColumnView,
    ColumnViewWidthFit,
    MountedRouteItemNavigation,
)
where
    T: Clone + 'static,
    M: IsA<gio::ListModel> + Clone + 'static,
{
    let selection =
        selection.unwrap_or_else(|| gtk::MultiSelection::new(Some(model.clone())).upcast());
    let table = gtk::ColumnView::new(Some(selection.clone()));
    table.add_css_class("track-table");
    table.set_single_click_activate(false);
    let row_factory = gtk::SignalListItemFactory::new();
    row_factory.connect_setup(|_, row| {
        if let Some(row) = row.downcast_ref::<gtk::ColumnViewRow>() {
            row.set_activatable(true);
        }
    });
    table.set_row_factory(Some(&row_factory));
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
            selected_position(&navigation_selection),
            model.n_items(),
            direction,
        ) {
            navigation_selection.select_item(position, true);
            navigation_table.scroll_to(position, None, gtk::ListScrollFlags::FOCUS, None);
            navigation_table.grab_focus();
        }
        glib::Propagation::Stop
    }) as MountedRouteItemNavigation;
    (table, width_fit, navigation)
}

pub(crate) fn track_table<T: TrackPresentation>(
    shell: &Rc<Shell>,
    model: TrackCollectionModel<T>,
    key: LibraryListKey,
    options: TrackTableOptions,
    selection: TrackSelection,
) -> CollectionTableProjection {
    let playing_indicator = TrackRowPlayingIndicator::new();
    let playing_state = TrackTablePlayingState::new(&model, playing_indicator.clone());
    let selection_context_base = options.context_id.clone();
    let selection_model = model.clone();
    let current_playing_state = playing_state.clone();
    shell.register_current_route_track_selection(Rc::new(move |current| {
        if !current_playing_state.is_bound() {
            return false;
        }
        let position = current.and_then(|current| {
            let expected_context_id = selection_model.visible_context_id(&selection_context_base);
            let source_rank = matching_context_rank(
                current.context.as_ref(),
                &selection_context_base,
                &expected_context_id,
            );
            selection_model.position_for_current_uri(&current.media_uri, source_rank)
        });
        current_playing_state
            .set_now_playing_track(current.map(|current| current.media_uri.as_str()), position);
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
    let activate = Box::new(move |position, _: T| {
        play_model.play(
            queue.clone(),
            position as usize,
            playback::QueuePlacement::Now,
            &context_id,
            false,
        );
    });
    let column_shell = Rc::clone(shell);
    let column_playing_indicator = playing_indicator;
    let column_selection = selection.clone();
    let selection_model = selection.selection_model();
    let table = dynamic_collection_table(
        shell,
        key,
        model.list_model(),
        &fields,
        Vec::new(),
        move |field| {
            let column =
                song_column_for_key::<T>(&column_shell, key, field, &column_playing_indicator);
            if field != LibraryField::Favorite {
                install_track_column_drag_source::<T>(
                    &column,
                    &column_shell,
                    column_selection.clone(),
                );
            }
            column
        },
        move |field| track_column_fit_width(key, field),
        false,
        Some(activate),
        Some(selection_model),
        column_view_initial_width(shell, options.content_inset),
    );
    table.table.add_css_class("track-list");
    table
}

fn install_track_column_drag_source<T: TrackPresentation>(
    column: &gtk::ColumnViewColumn,
    shell: &Rc<Shell>,
    selection: TrackSelection,
) {
    let Some(factory) = column
        .factory()
        .and_then(|factory| factory.downcast::<gtk::SignalListItemFactory>().ok())
    else {
        return;
    };
    let drag_shell = Rc::downgrade(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(child) = item.child() else {
            return;
        };
        let Some(shell) = drag_shell.upgrade() else {
            return;
        };
        let item = item.downgrade();
        install_track_drag_source(&child, &shell, selection.clone(), move || {
            let item = item.upgrade()?;
            item_at_from_item::<T>(&item).map(|track| {
                let artwork = track
                    .artwork()
                    .map(artwork::ArtworkBinding::opaque)
                    .unwrap_or_default();
                (
                    track.media_uri().to_string(),
                    track.title().to_string(),
                    artwork,
                )
            })
        });
    });
}

fn selected_position(selection: &gtk::SelectionModel) -> u32 {
    let positions = selection.selection();
    if positions.is_empty() {
        gtk::INVALID_LIST_POSITION
    } else {
        positions.minimum()
    }
}
pub(crate) fn install_smart_playlist_reorder(
    target: &impl IsA<gtk::Widget>,
    shell: &Rc<Shell>,
    current: Rc<dyn Fn() -> Option<SmartPlaylistRow>>,
) {
    let shell = Rc::downgrade(shell);
    install_playlist_order_drop(
        target,
        Rc::new(move || current().map(|playlist| playlist.smart_playlist_key.raw())),
        |target| match target {
            PlaybackTarget::SmartPlaylist(key) => Some(key.raw()),
            _ => None,
        },
        Rc::new(move |dragged, target| {
            if let Some(shell) = shell.upgrade() {
                shell.update_library_list_settings(LibraryListKey::SmartPlaylists, |settings| {
                    settings.sort_key = LibraryField::RowIndex;
                    settings.descending = false;
                });
                shell.publish_smart_playlist_change(
                    SmartPlaylistChange::Move {
                        dragged: SmartPlaylistKey::from_raw(dragged),
                        target: SmartPlaylistKey::from_raw(target),
                    },
                    None,
                );
            }
        }),
    );
}

pub(crate) fn install_playlist_reorder(
    target: &impl IsA<gtk::Widget>,
    shell: &Rc<Shell>,
    current: Rc<dyn Fn() -> Option<PlaylistRow>>,
) {
    let shell = Rc::downgrade(shell);
    install_playlist_order_drop(
        target,
        Rc::new(move || current().map(|playlist| playlist.playlist_key.raw())),
        |target| match target {
            PlaybackTarget::Playlist(key) => Some(key.raw()),
            _ => None,
        },
        Rc::new(move |dragged, target| {
            let Some(shell) = shell.upgrade() else { return };
            shell.update_library_list_settings(LibraryListKey::Playlists, |settings| {
                settings.sort_key = LibraryField::RowIndex;
                settings.descending = false;
            });
            let Some(selected) = shell.selected_library().as_deref().cloned() else {
                return;
            };
            let database = Arc::clone(&selected.database);
            let task = selected.runtime.spawn(async move {
                database
                    .move_playlist(
                        selected.source_key,
                        PlaylistKey::from_raw(dragged),
                        PlaylistKey::from_raw(target),
                    )
                    .await
            });
            let shell = Rc::downgrade(&shell);
            glib::spawn_future_local(async move {
                if task.await.ok().and_then(Result::ok) == Some(true)
                    && let Some(shell) = shell.upgrade()
                {
                    shell.refresh_mounted_catalog();
                }
            });
        }),
    );
}

fn install_playlist_order_drop(
    widget: &impl IsA<gtk::Widget>,
    current: Rc<dyn Fn() -> Option<i64>>,
    key: fn(&PlaybackTarget) -> Option<i64>,
    reorder: Rc<dyn Fn(i64, i64)>,
) {
    let drop = gtk::DropTarget::new(
        glib::BoxedAnyObject::static_type(),
        gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::MOVE,
    );
    drop.connect_drop(move |_, value, _, _| {
        use super::playlist_picker::MediaDragSource;
        let Some(source) = super::playlist_picker::media_drag_source(value) else {
            return false;
        };
        let target = match &source {
            MediaDragSource::Target { target, .. } => target,
            MediaDragSource::Targets { targets, .. } if targets.len() == 1 => &targets[0],
            _ => return false,
        };
        let target = match target {
            PlaybackTarget::Contextual { target, .. } => target.as_ref(),
            target => target,
        };
        let (Some(dragged), Some(current)) = (key(target), current()) else {
            return false;
        };
        if dragged == current {
            return false;
        }
        reorder(dragged, current);
        true
    });
    widget.add_controller(drop);
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

#[cfg(test)]
mod layout_policy_tests {
    use super::*;

    #[test]
    fn contextual_playback_target_uses_the_route_context() {
        let target = PlaybackTarget::Album("album:1".to_string()).in_context("artist:1|releases");

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
