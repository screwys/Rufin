use crate::widgets::media_drag::{
    MediaDragSource, install_compact_media_drag_source, install_media_drag_source,
};
use crate::widgets::view_model::item_at_from_item;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

use crate::widgets::selection::PlayTarget;
use adw::prelude::*;
use artwork::ArtworkBinding;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use gtk::{gio, glib};
use library::{ReadCancellation, SearchResults};
use localization::{msgid, tr};
use rufin_core::playback::QueuePlacement;

use crate::favorites::{favorite_button_is_active, set_favorite_button_active};
use crate::library::CatalogUi;
use crate::library::{LibraryField, LibraryLayout};
use crate::widgets::artwork::ArtworkTile;
use crate::widgets::interactions::install_context_menu_openers;
use crate::widgets::mounted_route::{MountedRoute, MountedRouteItemNavigation};

use super::cards;
use super::collection_context::present_track_context_menu;
use super::collections::{
    LibraryCollectionProjection, LibraryPresentationProjection, dynamic_collection_table,
};
use super::grid_cells::{
    CollectionGridCardCell, ReusableCollectionGridCell, collection_grid, install_grid_context,
    install_grid_play,
};
use crate::menus::media_menus::{present_collection_selection_menu, present_row_menu};
use crate::widgets::library_fields::column_fit_width;
use rufin_core::settings::app::{RecentSearchKind, RecentSearchResult};

use super::route_shell::{LibraryPageShell, LibraryToolbarProjection};
use crate::widgets::library_fields::{
    COLLECTION_GRID_MAX_CARD_WIDTH, TrackPresentation, add_field_skeleton_class, column_width,
};
use crate::widgets::recycled_cells::{RecycledMergedCell, RecycledTextCell, list_cell};
use crate::widgets::view_model::{RowItem, connect_row_bind};
use rufin_core::route::CollectionCategory;
use rufin_core::route::Route;
use rufin_core::route::detail_links::DetailLinks;
use rufin_core::route::detail_links::album_row_artist_links;
use rufin_core::settings::presentation::track_row_field;
use rufin_core::settings::presentation::{album_field, artist_field};

const SEARCH_DEBOUNCE: Duration = Duration::from_millis(300);
pub mod session;
use crate::widgets::lyrics_snippet::LyricsSnippetView;

#[derive(Clone, PartialEq)]
#[expect(clippy::large_enum_variant, reason = "bounded prepared Search rows")]
enum SearchItem {
    Track(library::TrackRow, Option<library::LyricsSnippet>),
    Album(library::AlbumRow),
    Artist(library::ArtistRow),
}

impl library::ShowsState for SearchItem {
    fn state(&self, change: &library::StateChange) -> Option<&library::TrackState> {
        match self {
            Self::Track(row, _) => row.state(change),
            Self::Album(row) => row.state(change),
            Self::Artist(row) => row.state(change),
        }
    }
    fn state_mut(&mut self, change: &library::StateChange) -> Option<&mut library::TrackState> {
        match self {
            Self::Track(row, _) => row.state_mut(change),
            Self::Album(row) => row.state_mut(change),
            Self::Artist(row) => row.state_mut(change),
        }
    }
    fn downloads(&self) -> Option<library::Downloads> {
        match self {
            Self::Track(row, _) => row.downloads(),
            Self::Album(row) => row.downloads(),
            Self::Artist(row) => row.downloads(),
        }
    }
    fn show_downloads(&mut self, read: library::Downloads) -> bool {
        match self {
            Self::Track(row, _) => row.show_downloads(read),
            Self::Album(row) => row.show_downloads(read),
            Self::Artist(row) => row.show_downloads(read),
        }
    }
}

impl SearchItem {
    /// The track, album or artist it shows.
    fn uri(&self) -> &str {
        match self {
            Self::Track(row, _) => &row.media_uri,
            Self::Album(row) => &row.media_uri,
            Self::Artist(row) => &row.media_uri,
        }
    }

    fn lyrics(&self) -> Option<&library::LyricsSnippet> {
        match self {
            Self::Track(_, snippet) => snippet.as_ref(),
            _ => None,
        }
    }

    fn recent_result(&self) -> RecentSearchResult {
        let (kind, media_uri, artwork_binding) = match self {
            Self::Track(row, _) => (
                RecentSearchKind::Track,
                &row.media_uri,
                &row.artwork_binding,
            ),
            Self::Album(row) => (
                RecentSearchKind::Album,
                &row.media_uri,
                &row.artwork_binding,
            ),
            Self::Artist(row) => (
                RecentSearchKind::Artist,
                &row.media_uri,
                &row.artwork_binding,
            ),
        };
        RecentSearchResult {
            kind,
            media_uri: media_uri.clone(),
            title: self.title().to_owned(),
            subtitle: self.subtitle().to_owned(),
            artwork_binding: artwork_binding.clone(),
        }
    }
    fn drag_source(&self, _: &CatalogUi) -> Option<MediaDragSource> {
        Some(MediaDragSource::capture_target(self.play_target()))
    }
    fn play_target(&self) -> PlayTarget {
        match self {
            Self::Track(row, _) => PlayTarget::of_track(row),
            Self::Album(row) => PlayTarget::of_album(row),
            Self::Artist(row) => PlayTarget::of_artist(row),
        }
    }

    /// The row it shows.
    fn row(&self) -> library::Row {
        match self {
            Self::Track(row, _) => library::Row::Track(row.clone()),
            Self::Album(row) => library::Row::Album(row.clone()),
            Self::Artist(row) => library::Row::Artist(row.clone()),
        }
    }

    fn media_uri(&self) -> Option<&str> {
        match self {
            Self::Track(row, _) => Some(&row.media_uri),
            _ => None,
        }
    }
    fn title(&self) -> &str {
        match self {
            Self::Track(row, _) => &row.title,
            Self::Album(row) => &row.title,
            Self::Artist(row) => &row.name,
        }
    }
    fn artwork(&self) -> ArtworkBinding {
        let bytes = match self {
            Self::Track(row, _) => &row.artwork_binding,
            Self::Album(row) => &row.artwork_binding,
            Self::Artist(row) => &row.artwork_binding,
        };
        bytes
            .as_deref()
            .map(ArtworkBinding::opaque)
            .unwrap_or_default()
    }
    fn field(&self, field: LibraryField) -> String {
        match self {
            Self::Track(row, _) => track_row_field(row, field),
            Self::Album(row) => album_field(row, field),
            Self::Artist(row) => artist_field(row, field),
        }
    }
    fn field_links(&self, field: LibraryField) -> DetailLinks {
        let text = self.field(field);
        match self {
            Self::Track(row, _) => TrackPresentation::links(row, field),
            Self::Album(row) => match field {
                LibraryField::Artist | LibraryField::AlbumArtist => album_row_artist_links(row),
                LibraryField::Title => {
                    DetailLinks::route(&text, Some(Route::AlbumDetail(row.media_uri.clone())))
                }
                _ => DetailLinks::text(&text),
            },
            Self::Artist(row) if field == LibraryField::Title => {
                DetailLinks::route(&text, Some(Route::ArtistDetail(row.media_uri.clone())))
            }
            _ => DetailLinks::text(&text),
        }
    }
    fn subtitle_links(&self) -> DetailLinks {
        match self {
            Self::Track(row, _) => TrackPresentation::links(row, LibraryField::Artist),
            Self::Album(row) => album_row_artist_links(row),
            _ => DetailLinks::text(self.subtitle()),
        }
    }
    fn subtitle(&self) -> &str {
        match self {
            Self::Track(row, _) => &row.artist,
            Self::Album(row) => &row.artist,
            Self::Artist(_) => "",
        }
    }
    fn is_downloaded(&self) -> bool {
        matches!(self, Self::Track(row, _) if crate::widgets::library_fields::TrackPresentation::download_badge(row))
    }
    fn favorite(&self) -> Option<(library::FavoriteTarget, bool)> {
        Some(match self {
            Self::Track(row, _) => (
                library::FavoriteTarget::Track(row.media_uri.clone()),
                row.state.favorite,
            ),
            Self::Album(row) => (
                library::FavoriteTarget::Album(row.media_uri.clone()),
                row.state.favorite,
            ),
            Self::Artist(row) => (
                library::FavoriteTarget::Artist(row.media_uri.clone()),
                row.state.favorite,
            ),
        })
    }
    fn play(&self, shell: &Rc<CatalogUi>, placement: QueuePlacement) {
        shell
            .settings
            .update_app_settings("recent search", |settings| {
                settings.remember_search_result(self.recent_result())
            });
        shell.search_session.play_row(self.row(), placement, shell);
    }
    fn activate(&self, shell: &Rc<CatalogUi>) {
        if matches!(self, Self::Track(..)) {
            self.play(shell, QueuePlacement::Now);
        } else {
            session::SearchSession::activate_recent(&self.recent_result(), shell);
        }
    }
    fn present_context(
        &self,
        target: &gtk::Widget,
        shell: &Rc<CatalogUi>,
        position: Option<(f64, f64)>,
    ) {
        if present_collection_selection_menu(target, shell.shell(), self.play_target(), position) {
            return;
        }
        match self {
            Self::Track(row, _) => {
                present_track_context_menu(target, shell, row.clone(), None, position);
            }
            _ => present_row_menu(target, shell.shell(), self.row(), None, position),
        }
    }
}

struct SearchGridCell {
    snippet: LyricsSnippetView,
    body: CollectionGridCardCell,
    shell: Rc<CatalogUi>,
    cover: ArtworkTile,
    current: Rc<RefCell<Option<SearchItem>>>,
    favorite: gtk::Button,
}

impl SearchGridCell {
    fn new(shell: Rc<CatalogUi>, fields: &[LibraryField]) -> Self {
        let current = Rc::new(RefCell::new(None::<SearchItem>));
        let controls = cards::CollectionGridCoverView::new(msgid("Play"));
        let cover_host = controls.imp().cover_host.get();
        let favorite = controls.favorite();
        let cover = ArtworkTile::new_elastic_square();
        cover_host.append(&cover.widget());
        let favorite_shell = Rc::clone(&shell);
        let favorite_item = Rc::clone(&current);
        favorite.connect_clicked(move |button| {
            let Some((target, _)) = favorite_item
                .borrow()
                .as_ref()
                .and_then(SearchItem::favorite)
            else {
                return;
            };
            favorite_shell.set_favorite(target, !favorite_button_is_active(button));
        });
        let menu = controls.imp().menu.get();
        let overlay = controls.imp().overlay.get();
        install_grid_play(
            &shell,
            &current,
            [
                controls.imp().play.get(),
                controls.imp().play_next.get(),
                controls.imp().play_last.get(),
            ],
            |shell, item, placement| item.play(shell, placement),
        );
        let body = CollectionGridCardCell::new(&shell, fields, controls.upcast());
        let snippet = LyricsSnippetView::new();
        body.card.append(&snippet);
        let drag_current = Rc::clone(&current);
        let drag_shell = Rc::downgrade(&shell);
        let drag_artwork = cover.drag_paintable_source();
        install_compact_media_drag_source(&body.card, &drag_artwork, move || {
            let shell = drag_shell.upgrade()?;
            let item = drag_current.borrow();
            let item = item.as_ref()?;
            Some((item.drag_source(&shell)?, item.title().to_string()))
        });
        install_grid_context(
            &shell,
            &current,
            &menu,
            &overlay,
            &body.card,
            |target, shell, item, point| item.present_context(target, shell, point),
        );
        Self {
            snippet,
            body,
            shell,
            cover,
            current,
            favorite,
        }
    }

    fn bind_current_artwork(&self) {
        let Some(item) = self.current.borrow().as_ref().cloned() else {
            return;
        };
        self.cover
            .widget()
            .remove_css_class("collection-grid-cover-skeleton");
        self.shell.artwork.bind_artwork_tile(
            &self.cover,
            item.artwork(),
            COLLECTION_GRID_MAX_CARD_WIDTH,
        );
    }

    fn release_artwork(&self) {
        self.shell.artwork.clear_artwork_tile(&self.cover);
        self.cover
            .widget()
            .add_css_class("collection-grid-cover-skeleton");
        self.cover.widget().set_opacity(1.0);
    }
}

impl ReusableCollectionGridCell<SearchItem> for SearchGridCell {
    fn widget(&self) -> gtk::Widget {
        self.body.widget()
    }

    fn bind(&self, _: u32, item: SearchItem) {
        self.snippet.bind(item.lyrics());
        self.body
            .bind_playing_target(&self.shell, item.play_target());
        self.body
            .bind(item.title(), |field| item.field_links(field));
        let favorite = item.favorite();
        set_favorite_button_active(
            &self.favorite,
            favorite.as_ref().is_some_and(|(_, value)| *value),
        );
        self.favorite.set_sensitive(favorite.is_some());
        self.favorite
            .set_opacity(if favorite.is_some() { 1.0 } else { 0.0 });
        *self.current.borrow_mut() = Some(item);
        self.bind_current_artwork();
    }

    fn clear(&self) {
        self.release_artwork();
        self.snippet.bind(None);
        self.body.clear(&self.shell);
        self.favorite.set_visible(false);
        self.favorite.set_sensitive(false);
        self.current.take();
    }

    fn set_ready(&self, ready: bool) {
        self.body.set_ready(ready);
    }

    fn apply_fields(&self, fields: &[LibraryField]) {
        self.body.card.remove(&self.snippet);
        self.body.replace_fields(&self.shell, fields);
        self.body.card.append(&self.snippet);
        if let Some(item) = self.current.borrow().as_ref() {
            self.body
                .bind(item.title(), |field| DetailLinks::text(&item.field(field)));
        }
    }
}

struct SearchRouteProjection {
    root: gtk::Widget,
    shell: Weak<CatalogUi>,
    search: gtk::SearchEntry,
    session: Rc<session::SearchSession>,
    observer: RefCell<Option<Rc<dyn Fn()>>>,
    revision: Cell<Option<u64>>,
    status: gtk::Stack,
    results: adw::ViewStack,
    result_page: gtk::Stack,
    toolbar_host: gtk::Box,
    category_host: gtk::Box,
    search_modes: gtk::Box,
    modes: [gtk::ToggleButton; 2],
    models: [gio::ListStore; 3],
    playing: crate::widgets::recycled_cells::TrackRowPlayingIndicator,
    active: RefCell<Option<SearchPresentation>>,
}

struct SearchPresentation {
    category: CollectionCategory,
    collection: LibraryCollectionProjection,
    toolbar: LibraryToolbarProjection,
    navigation: MountedRouteItemNavigation,
}

crate::composite_box!(
    pub SearchRouteView,
    search_route_view_imp,
    "RufinSearchRouteView",
    "/io/github/screwys/Rufin/ui/routes/search.ui",
    {
        toolbar_host: gtk::Box,
        category_host: gtk::Box,
        search_modes: gtk::Box,
        by_title: gtk::ToggleButton,
        by_lyrics: gtk::ToggleButton,
        status: gtk::Stack,
        result_page: gtk::Stack,
    }
);

impl SearchRouteProjection {
    fn new(shell: &Rc<CatalogUi>) -> Rc<Self> {
        let wrapper = SearchRouteView::new();
        let search = shell.chrome.topbar.search.clone();

        // The page shows the session's results, the one copy the header shows too.
        let models = shell.search_session.models.clone();
        let category_pages =
            CollectionCategory::ALL.map(|_| gtk::Box::new(gtk::Orientation::Vertical, 0).upcast());
        let (results, switcher) =
            collection_category_tabs(category_pages, shell.search_session.category.get());
        wrapper.imp().category_host.append(&switcher);
        let toolbar_host = wrapper.imp().toolbar_host.get();
        let status = wrapper.imp().status.get();
        let result_page = wrapper.imp().result_page.get();
        let category_host = wrapper.imp().category_host.get();
        let search_modes = wrapper.imp().search_modes.get();
        let modes = [wrapper.imp().by_title.get(), wrapper.imp().by_lyrics.get()];

        let projection = Rc::new(Self {
            root: wrapper.upcast(),
            shell: Rc::downgrade(shell),
            search,
            session: Rc::clone(&shell.search_session),
            observer: RefCell::new(None),
            revision: Cell::new(None),
            status,
            results,
            result_page,
            toolbar_host,
            category_host,
            search_modes,
            modes,
            models,
            playing: crate::widgets::recycled_cells::TrackRowPlayingIndicator::new(),
            active: RefCell::new(None),
        });
        projection.connect();
        projection.mount_category(projection.session.category.get());
        projection.register_now_playing(shell);
        projection.sync();
        projection
    }

    fn connect(self: &Rc<Self>) {
        for (button, mode) in self
            .modes
            .iter()
            .zip([library::SearchMode::Title, library::SearchMode::Lyrics])
        {
            let session = Rc::downgrade(&self.session);
            button.connect_toggled(move |button| {
                if button.is_active()
                    && let Some(session) = session.upgrade()
                {
                    session.set_mode(mode);
                }
            });
        }
        let weak = Rc::downgrade(self);
        self.observer.replace(Some(self.session.observe(move || {
            if let Some(projection) = weak.upgrade() {
                projection.sync();
            }
        })));
        let weak = Rc::downgrade(self);
        self.results.connect_visible_child_notify(move |results| {
            let Some(projection) = weak.upgrade() else {
                return;
            };
            let category = results
                .visible_child_name()
                .as_deref()
                .map(CollectionCategory::from_name)
                .unwrap_or_default();
            projection.mount_category(category);
            projection.session.category.set(category);
        });
    }

    fn mount_category(&self, category: CollectionCategory) {
        if self.active.borrow().as_ref().map(|active| active.category) == Some(category) {
            return;
        }
        if let Some(previous) = self.active.borrow_mut().take() {
            self.toolbar_host.remove(&previous.toolbar.widget());
            self.result_page
                .remove(&previous.collection.scrolling_widget());
        }
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        let collection = search_collection(
            &shell,
            self.models[category as usize].clone(),
            category,
            (category == CollectionCategory::Tracks).then(|| self.playing.clone()),
        );
        let toolbar = shell.library_toolbar_projection_without_detail(
            category.key(),
            None,
            rufin_core::settings::available_sort_fields(category.key()),
        );
        let presentation = SearchPresentation {
            category,
            navigation: collection.item_navigation(),
            collection,
            toolbar,
        };
        self.toolbar_host.append(&presentation.toolbar.widget());
        self.result_page
            .add_named(&presentation.collection.scrolling_widget(), Some("results"));
        self.active.replace(Some(presentation));
        self.show_results(self.models[category as usize].n_items() != 0);
    }

    fn register_now_playing(&self, shell: &Rc<CatalogUi>) {
        let model = self.models[CollectionCategory::Tracks as usize].clone();
        let indicator = self.playing.clone();
        shell.register_current_route_track_selection(Rc::new(move |current| {
            let position = current
                .and_then(|current| {
                    (0..model.n_items()).find(|position| {
                        model
                            .item(*position)
                            .and_then(|item| {
                                crate::widgets::view_model::object_item::<SearchItem, _>(
                                    item,
                                    |item| item.media_uri() == Some(current.media_uri.as_str()),
                                )
                            })
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(gtk::INVALID_LIST_POSITION);
            indicator.set_current(current.map(|current| current.media_uri.as_str()), position);
            indicator.set_paused(current.is_some_and(|current| current.paused));
            true
        }));
    }

    fn show_results(&self, has_results: bool) {
        self.result_page
            .set_visible_child_name(if has_results { "results" } else { "empty" });
    }

    fn sync(&self) {
        let lyrics = self.session.mode() == library::SearchMode::Lyrics;
        self.modes[usize::from(lyrics)].set_active(true);
        self.search_modes
            .set_visible(!self.session.query().is_empty());
        self.category_host.set_visible(!lyrics);
        self.results
            .set_visible_child_name(self.session.category.get().name());
        if self.revision.replace(Some(self.session.revision.get()))
            == Some(self.session.revision.get())
        {
            return;
        }
        if let Some(active) = self.active.borrow().as_ref() {
            self.show_results(self.models[active.category as usize].n_items() != 0);
        }
        self.status.set_visible_child_name(self.session.status());
    }

    fn resume(&self) {
        self.sync();
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        if let Some(active) = self.active.borrow().as_ref() {
            let settings = shell
                .settings
                .current
                .borrow()
                .library_list(active.category.key());
            active.collection.apply_settings(&settings);
            active.toolbar.apply(active.category.key(), &settings);
        }
    }

    fn cycle_category(&self) {
        if self.session.mode() == library::SearchMode::Lyrics {
            return;
        }
        let current = self
            .active
            .borrow()
            .as_ref()
            .map_or(CollectionCategory::default(), |presentation| {
                presentation.category
            });
        self.results.set_visible_child_name(current.next().name());
    }

    fn cycle_layout(&self) {
        if let Some(active) = self.active.borrow().as_ref() {
            active.toolbar.cycle_layout();
        }
    }
}

fn collection_category_tabs(
    pages: [gtk::Widget; 3],
    active: CollectionCategory,
) -> (adw::ViewStack, adw::ViewSwitcher) {
    let stack = adw::ViewStack::builder()
        .hexpand(true)
        .vexpand(true)
        .build();
    for (category, page) in CollectionCategory::ALL.into_iter().zip(pages) {
        stack.add_titled_with_icon(
            &page,
            Some(category.name()),
            &tr(category.title()),
            category.icon_name(),
        );
    }
    stack.set_visible_child_name(active.name());
    let switcher = adw::ViewSwitcher::builder()
        .policy(adw::ViewSwitcherPolicy::Wide)
        .stack(&stack)
        .build();
    switcher.set_halign(gtk::Align::Start);
    (stack, switcher)
}

impl CatalogUi {
    pub fn install_favorite_category_tabs(
        self: &Rc<Self>,
        active: CollectionCategory,
        page: &mut LibraryPageShell,
    ) {
        let category_pages =
            CollectionCategory::ALL.map(|_| gtk::Box::new(gtk::Orientation::Vertical, 0).upcast());
        let (pages, switcher) = collection_category_tabs(category_pages, active);
        page.set_category_switcher(&switcher);

        let shell = Rc::downgrade(self);
        pages.connect_visible_child_notify(move |pages| {
            let Some(category) = pages
                .visible_child_name()
                .as_deref()
                .map(CollectionCategory::from_name)
            else {
                return;
            };
            if category != active
                && let Some(shell) = shell.upgrade()
            {
                shell.set_favorite_category(category);
            }
        });
        let shell = Rc::downgrade(self);
        page.set_tab_cycle(Rc::new(move || {
            if let Some(shell) = shell.upgrade() {
                shell.cycle_favorite_category();
            }
        }));
    }

    pub fn search_route(self: &Rc<Self>) -> MountedRoute {
        let projection = SearchRouteProjection::new(self);
        let layout_projection = Rc::downgrade(&projection);
        let layout_cycle = Rc::new(move || {
            if let Some(projection) = layout_projection.upgrade() {
                projection.cycle_layout();
            }
        });
        let tab_projection = Rc::downgrade(&projection);
        let tab_cycle = Rc::new(move || {
            if let Some(projection) = tab_projection.upgrade() {
                projection.cycle_category();
            }
        });
        let resume_projection = Rc::clone(&projection);
        let navigation_projection = Rc::downgrade(&projection);
        let navigation = Rc::new(move |direction| {
            navigation_projection
                .upgrade()
                .and_then(|projection| {
                    projection
                        .active
                        .borrow()
                        .as_ref()
                        .map(|active| (active.navigation)(direction))
                })
                .unwrap_or(glib::Propagation::Stop)
        });

        MountedRoute::new(
            projection.root.clone(),
            Rc::new(move || resume_projection.resume()),
        )
        .with_search(projection.search.clone())
        .with_layout_cycle(layout_cycle)
        .with_tab_cycle(tab_cycle)
        .with_item_navigation(navigation)
    }
}

fn search_collection(
    shell: &Rc<CatalogUi>,
    model: gio::ListStore,
    category: CollectionCategory,
    playing: Option<crate::widgets::recycled_cells::TrackRowPlayingIndicator>,
) -> LibraryCollectionProjection {
    let settings = shell.settings.current.borrow().library_list(category.key());
    let selection = crate::widgets::selection::PositionSelectionModel::new(model.clone());
    let selected_model = model.downgrade();
    selection.set_collection_selection(Box::new(move |clicked, positions| {
        let model = selected_model.upgrade()?;
        let (iter, first) = gtk::BitsetIter::init_first(positions)?;
        let targets = std::iter::once(first)
            .chain(iter)
            .filter_map(|position| {
                crate::widgets::view_model::item_at::<SearchItem>(&model, position)
            })
            .map(|item| item.play_target())
            .collect::<Vec<_>>();
        targets.iter().any(|target| target == clicked).then_some(
            crate::widgets::media_drag::CollectionTargets::Ready(targets),
        )
    }));
    let shell = Rc::clone(shell);
    LibraryCollectionProjection::new(
        settings,
        Rc::new(move |layout| match layout {
            LibraryLayout::Grid => {
                let fields = shell
                    .settings
                    .current
                    .borrow()
                    .library_list(category.key())
                    .grid_fields;
                let cell_shell = Rc::clone(&shell);
                let activate_shell = Rc::clone(&shell);
                LibraryPresentationProjection::Grid(collection_grid(
                    model.clone(),
                    Some(selection.clone().upcast()),
                    &fields,
                    move |fields| SearchGridCell::new(Rc::clone(&cell_shell), fields),
                    move |_, item: SearchItem| item.activate(&activate_shell),
                ))
            }
            LibraryLayout::Row | LibraryLayout::Detail => {
                let fields = shell
                    .settings
                    .current
                    .borrow()
                    .library_list(category.key())
                    .row_fields;
                let activate_shell = Rc::clone(&shell);
                LibraryPresentationProjection::Row(dynamic_collection_table(
                    &shell,
                    category.key(),
                    model.clone(),
                    &fields,
                    Vec::new(),
                    {
                        let column_shell = Rc::clone(&shell);
                        let playing = playing.clone();
                        move |field| search_column(&column_shell, category, field, playing.as_ref())
                    },
                    move |field| {
                        if category == CollectionCategory::Tracks {
                            super::columns::track_column_fit_width(category.key(), field)
                        } else {
                            column_fit_width(field, column_width(field))
                        }
                    },
                    true,
                    Some(Box::new(move |_, item: SearchItem| {
                        item.activate(&activate_shell)
                    })),
                    Some(selection.clone().upcast()),
                    shell.table_initial_width(0),
                ))
            }
        }),
    )
}

fn search_column(
    shell: &Rc<CatalogUi>,
    category: CollectionCategory,
    field: LibraryField,
    playing: Option<&crate::widgets::recycled_cells::TrackRowPlayingIndicator>,
) -> gtk::ColumnViewColumn {
    if field == LibraryField::RowIndex {
        if category == CollectionCategory::Tracks
            && let Some(playing) = playing
        {
            return super::columns::mapped_track_row_index_column_with_width::<SearchItem, _>(
                super::columns::track_column_width(category.key(), field),
                playing.clone(),
                SearchItem::media_uri,
                |position, item| {
                    matches!(item, SearchItem::Track(..)).then(|| (position + 1).to_string())
                },
            );
        }
        return super::columns::row_index_column_with_width(column_width(field));
    }
    if field == LibraryField::Image {
        return super::columns::artwork_column::<SearchItem, _>(
            shell,
            rufin_core::settings::presentation::library_field_title(field),
            column_width(field),
            SearchItem::artwork,
            None,
        );
    }
    if field == LibraryField::TitleMerged {
        let width = if category == CollectionCategory::Tracks {
            super::columns::track_column_fit_width(category.key(), field)
        } else {
            column_width(field)
        };
        return search_merged_column(shell, category, width, playing.cloned());
    }
    if matches!(field, LibraryField::Favorite | LibraryField::Tools) {
        return search_actions_column(shell, field);
    }
    let playing = (field == LibraryField::Title)
        .then(|| playing.cloned())
        .flatten();
    let factory = gtk::SignalListItemFactory::new();
    let setup_shell = Rc::clone(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = RecycledTextCell::new();
        let label = cell.label();
        if field == LibraryField::Title {
            cell.lyrics();
        }
        if field == LibraryField::Title && category == CollectionCategory::Tracks {
            cell.set_spacing(5);
        }
        label.set_halign(gtk::Align::Fill);
        label.set_hexpand(true);
        add_field_skeleton_class(&label, field);
        cell.enable_links(setup_shell.route_navigation());
        let context_shell = Rc::downgrade(&setup_shell);
        let context_item = item.downgrade();
        install_context_menu_openers(
            &cell,
            Rc::new(move |target, position| {
                if let (Some(shell), Some(item)) = (context_shell.upgrade(), context_item.upgrade())
                    && let Some(value) = item_at_from_item::<SearchItem>(&item)
                {
                    value.present_context(target, &shell, position);
                }
            }),
        );
        let drag_item = item.downgrade();
        let drag_shell = Rc::downgrade(&setup_shell);
        install_media_drag_source(&cell, move || {
            let shell = drag_shell.upgrade()?;
            let item = drag_item.upgrade()?;
            let row = item_at_from_item::<SearchItem>(&item)?;
            Some((row.drag_source(&shell)?, row.title().to_string()))
        });
        item.set_child(Some(&cell));
    });
    let bind_playing = playing.clone();
    connect_row_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = list_cell::<RecycledTextCell>(item) else {
            return;
        };
        if let Some(row) = item_at_from_item::<SearchItem>(item) {
            cell.bind_links(row.field_links(field));
            if field == LibraryField::Title {
                cell.lyrics().bind(row.lyrics());
            }
            if let Some(playing) = bind_playing.as_ref() {
                if let Some(uri) = row.media_uri() {
                    playing.bind(cell.label().upcast_ref(), item.position(), &uri);
                }
            }
        } else {
            if let Some(playing) = bind_playing.as_ref() {
                playing.unbind(cell.label().upcast_ref());
            }
            cell.clear();
        }
    });
    factory.connect_unbind(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>()
            && let Some(cell) = list_cell::<RecycledTextCell>(item)
        {
            if let Some(playing) = playing.as_ref() {
                playing.unbind(cell.label().upcast_ref());
            }
            cell.clear();
        }
    });
    gtk::ColumnViewColumn::new(
        Some(rufin_core::settings::presentation::library_field_title(
            field,
        )),
        Some(factory),
    )
}

fn search_merged_column(
    shell: &Rc<CatalogUi>,
    category: CollectionCategory,
    width: i32,
    playing: Option<crate::widgets::recycled_cells::TrackRowPlayingIndicator>,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let setup_shell = Rc::clone(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = RecycledMergedCell::new(
            setup_shell.route_navigation(),
            &setup_shell.downloads,
            48,
            category == CollectionCategory::Tracks,
        );
        cell.lyrics();
        let title = cell.title();
        if category == CollectionCategory::Tracks {
            title.add_css_class("track-list-title");
        }
        let subtitle = cell.subtitle();
        if category == CollectionCategory::Tracks {
            subtitle.add_css_class("artist-label");
            subtitle.add_css_class("table-link-label");
        } else {
            subtitle.add_css_class("muted");
        }
        let context_shell = Rc::downgrade(&setup_shell);
        let context_item = item.downgrade();
        install_context_menu_openers(
            &cell,
            Rc::new(move |target, position| {
                if let (Some(shell), Some(item)) = (context_shell.upgrade(), context_item.upgrade())
                    && let Some(value) = item_at_from_item::<SearchItem>(&item)
                {
                    value.present_context(target, &shell, position);
                }
            }),
        );
        let drag_item = item.downgrade();
        let drag_shell = Rc::downgrade(&setup_shell);
        install_media_drag_source(&cell, move || {
            let shell = drag_shell.upgrade()?;
            let item = drag_item.upgrade()?;
            let row = item_at_from_item::<SearchItem>(&item)?;
            Some((row.drag_source(&shell)?, row.title().to_string()))
        });
        item.set_child(Some(&cell));
    });
    let bind_shell = Rc::clone(shell);
    let bind_playing = playing.clone();
    connect_row_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = list_cell::<RecycledMergedCell>(item) else {
            return;
        };
        let cover = cell.cover();
        let title = cell.title();
        let Some(value) = item_at_from_item::<SearchItem>(item) else {
            bind_shell.artwork.clear_artwork_tile(&cover);
            title.set_text("");
            cell.clear_subtitle();
            cell.lyrics().bind(None);
            if let Some(downloaded) = cell.downloaded() {
                bind_shell.downloads.clear_download_badge(&downloaded);
            }
            if let Some(playing) = bind_playing.as_ref() {
                playing.unbind(title.upcast_ref());
            }
            return;
        };
        bind_shell
            .artwork
            .bind_artwork_tile(&cover, value.artwork(), 48);
        title.set_text(value.title());
        cell.lyrics().bind(value.lyrics());
        cell.bind_subtitle(value.subtitle_links());
        cell.subtitle().set_visible(!value.subtitle().is_empty());
        if let Some(downloaded) = cell.downloaded() {
            bind_shell
                .downloads
                .bind_download_badge(&downloaded, value.is_downloaded());
        }
        if let Some(playing) = bind_playing.as_ref() {
            if let Some(uri) = value.media_uri() {
                playing.bind(title.upcast_ref(), item.position(), &uri);
            }
        }
    });
    let clear_shell = Rc::clone(shell);
    factory.connect_unbind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(cell) = list_cell::<RecycledMergedCell>(item) {
            let title = cell.title();
            clear_shell.artwork.clear_artwork_tile(&cell.cover());
            title.set_text("");
            cell.clear_subtitle();
            cell.lyrics().bind(None);
            if let Some(downloaded) = cell.downloaded() {
                clear_shell.downloads.clear_download_badge(&downloaded);
            }
            if let Some(playing) = playing.as_ref() {
                playing.unbind(title.upcast_ref());
            }
        }
    });
    let column = gtk::ColumnViewColumn::new(Some(&tr("Title")), Some(factory));
    column.set_fixed_width(width);
    column
}

fn search_actions_column(shell: &Rc<CatalogUi>, field: LibraryField) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let setup_shell = Rc::clone(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let actions = crate::widgets::recycled_cells::row_action_cell(field, "Favorite");
        let context_shell = Rc::downgrade(&setup_shell);
        let context_item = item.downgrade();
        install_context_menu_openers(
            &actions,
            Rc::new(move |target, position| {
                if let (Some(shell), Some(item)) = (context_shell.upgrade(), context_item.upgrade())
                    && let Some(value) = item_at_from_item::<SearchItem>(&item)
                    && value.favorite().is_some()
                {
                    value.present_context(target, &shell, position);
                }
            }),
        );
        item.set_child(Some(&actions));
        let Some(button) = crate::widgets::recycled_cells::row_favorite_button(item) else {
            return;
        };
        let click_shell = Rc::clone(&setup_shell);
        let click_item = item.downgrade();
        button.connect_clicked(move |button| {
            let Some((target, _)) = click_item
                .upgrade()
                .and_then(|item| item_at_from_item::<SearchItem>(&item))
                .as_ref()
                .and_then(SearchItem::favorite)
            else {
                return;
            };
            click_shell.set_favorite(target, !favorite_button_is_active(button));
        });
    });
    connect_row_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(value) = item_at_from_item::<SearchItem>(item) else {
            return;
        };
        let Some(button) = crate::widgets::recycled_cells::row_favorite_button(item) else {
            return;
        };
        let favorite = value.favorite();
        set_favorite_button_active(
            &button,
            favorite.as_ref().is_some_and(|(_, active)| *active),
        );
        button.set_sensitive(favorite.is_some());
    });
    factory.connect_unbind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(button) = crate::widgets::recycled_cells::row_favorite_button(item) {
            set_favorite_button_active(&button, false);
            button.set_sensitive(false);
        }
    });
    crate::widgets::recycled_cells::row_action_column(field, &factory)
}

fn cached_items(mut results: SearchResults) -> [Vec<SearchItem>; 3] {
    [
        results
            .tracks
            .into_iter()
            .map(|row| {
                let snippet = results.lyrics.remove(&row.track_key);
                SearchItem::Track(row, snippet)
            })
            .collect(),
        results.albums.into_iter().map(SearchItem::Album).collect(),
        results
            .artists
            .into_iter()
            .map(SearchItem::Artist)
            .collect(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_tabs_cycle_tracks_albums_and_artists() {
        assert_eq!(
            CollectionCategory::Tracks.next(),
            CollectionCategory::Albums
        );
        assert_eq!(
            CollectionCategory::Albums.next(),
            CollectionCategory::Artists
        );
        assert_eq!(
            CollectionCategory::Artists.next(),
            CollectionCategory::Tracks
        );
    }

    #[test]
    fn accepted_search_metadata_keeps_its_exact_route_link() {
        let artist_key = library::ArtistKey::from_raw(7);
        let item = SearchItem::Artist(library::ArtistRow {
            artist_key,
            source_key: library::SourceKey::from_raw(3),
            object_id: "artist-object".to_string(),
            media_uri: library::source_entity_uri(
                &library::SourceId::new("source"),
                "artist",
                "artist-object",
            ),
            artwork_binding: None,
            album_artist: false,
            name: "Performer".to_string(),
            sort_text: "performer".to_string(),
            state: library::TrackState {
                favorite: false,
                rating: None,
                play_count: 0,
                last_played: None,
                ..Default::default()
            },
            summary: library::CollectionSummary {
                album_count: 1,
                track_count: 1,
                duration_millis: 1,
                downloaded_count: 0,
                ..Default::default()
            },
        });
        assert_eq!(
            item.field_links(LibraryField::Title),
            DetailLinks::route(
                "Performer",
                Some(Route::ArtistDetail(library::source_entity_uri(
                    &library::SourceId::new("source"),
                    "artist",
                    "artist-object"
                )))
            )
        );
    }
}
