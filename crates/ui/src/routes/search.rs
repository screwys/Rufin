use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;
use artwork::ArtworkBinding;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use gtk::{gio, glib};
use library::{ReadCancellation, SearchRequest, SearchResults};
use localization::{msgid, tr};
use playback::QueuePlacement;
use sources::LiveSearchResults;

use crate::favorites::{favorite_button_is_active, set_favorite_button_active};
use crate::interactions::install_context_menu_openers;
use crate::localization::bind_search_placeholder;
use crate::runtime::SelectedLibrary;
use crate::shell::Shell;
use crate::shell::cover::{ArtworkTile, LARGE_COVER_SIZE};
use crate::shell::route::{MountedRoute, MountedRouteItemNavigation};
use crate::{LibraryField, LibraryLayout, LibraryListKey};

use super::cards;
use super::collection_context::{
    present_album_context_menu, present_artist_context_menu, present_track_context_menu,
};
use super::collections::{
    LibraryCollectionProjection, LibraryPresentationProjection, PlaybackTarget,
    dynamic_collection_table,
};
use super::columns::column_fit_width;
use super::detail_links::{
    DetailLinks, album_artist_links, track_album_artist_links, track_artist_links,
};
use super::grid_cells::{
    CollectionGridCardCell, ReusableCollectionGridCell, collection_grid, install_grid_context,
    install_grid_open, install_grid_play,
};
use super::library_fields::{
    COLLECTION_GRID_MAX_CARD_WIDTH, add_field_skeleton_class, album_field, artist_field,
    column_width, item_at_from_item, track_field,
};
use super::recycled_cells::{RecycledMergedCell, RecycledTextCell, list_cell};
use super::route::CollectionCategory;
use super::route::Route;
use super::route_shell::{LibraryPageShell, LibraryToolbarProjection};
use super::sparse_model::connect_sparse_bind;
use super::table_sizing::route_column_view_initial_width;

const SEARCH_DEBOUNCE: Duration = Duration::from_millis(300);

impl CollectionCategory {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Tracks => "tracks",
            Self::Albums => "albums",
            Self::Artists => "artists",
        }
    }

    pub(crate) fn from_name(name: &str) -> Self {
        match name {
            "albums" => Self::Albums,
            "artists" => Self::Artists,
            _ => Self::Tracks,
        }
    }

    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::Tracks => msgid("Tracks"),
            Self::Albums => msgid("Albums"),
            Self::Artists => msgid("Artists"),
        }
    }

    pub(crate) const fn icon_name(self) -> &'static str {
        match self {
            Self::Tracks => "rufin-tracks-symbolic",
            Self::Albums => "rufin-albums-symbolic",
            Self::Artists => "rufin-artists-symbolic",
        }
    }

    pub(crate) const fn key(self) -> LibraryListKey {
        match self {
            Self::Tracks => LibraryListKey::Tracks,
            Self::Albums => LibraryListKey::Albums,
            Self::Artists => LibraryListKey::Artists,
        }
    }

    fn sort_fields(self) -> &'static [LibraryField] {
        match self {
            Self::Tracks => crate::available_sort_fields(LibraryListKey::Tracks),
            Self::Albums => &[
                LibraryField::Title,
                LibraryField::AlbumArtist,
                LibraryField::Year,
                LibraryField::ReleaseDate,
                LibraryField::DateAdded,
                LibraryField::LastPlayed,
                LibraryField::PlayCount,
                LibraryField::UserRating,
                LibraryField::Favorite,
            ],
            Self::Artists => &[
                LibraryField::Title,
                LibraryField::LastPlayed,
                LibraryField::PlayCount,
                LibraryField::UserRating,
                LibraryField::Favorite,
            ],
        }
    }
}

#[derive(Clone)]
#[expect(
    clippy::large_enum_variant,
    reason = "provider Search results are bounded and keep final result values inline"
)]
enum SearchItem {
    Track {
        media: playback::PlaybackMedia,
        row: Option<library::TrackRow>,
        artwork: Option<Vec<u8>>,
    },
    Album {
        object_id: String,
        title: String,
        artist: String,
        row: Option<library::AlbumRow>,
        artwork: Option<Vec<u8>>,
    },
    Artist {
        object_id: String,
        name: String,
        row: Option<library::ArtistRow>,
        artwork: Option<Vec<u8>>,
    },
}

impl SearchItem {
    fn title(&self) -> &str {
        match self {
            Self::Track { media, .. } => &media.title,
            Self::Album { title, .. } => title,
            Self::Artist { name, .. } => name,
        }
    }

    fn artwork(&self) -> ArtworkBinding {
        let bytes = match self {
            Self::Track { artwork, row, .. } => artwork
                .as_deref()
                .or_else(|| row.as_ref()?.artwork_binding.as_deref()),
            Self::Album { artwork, row, .. } => artwork
                .as_deref()
                .or_else(|| row.as_ref()?.artwork_binding.as_deref()),
            Self::Artist { artwork, row, .. } => artwork
                .as_deref()
                .or_else(|| row.as_ref()?.artwork_binding.as_deref()),
        };
        bytes.map(ArtworkBinding::opaque).unwrap_or_default()
    }

    fn field(&self, field: LibraryField) -> String {
        match self {
            Self::Track { media, row, .. } => row.as_ref().map_or_else(
                || match field {
                    LibraryField::Title | LibraryField::TitleMerged => media.title.clone(),
                    LibraryField::Artist => media.artist.clone(),
                    LibraryField::Album => media.album.clone(),
                    _ => String::new(),
                },
                |row| track_field(row, field),
            ),
            Self::Album {
                title, artist, row, ..
            } => row.as_ref().map_or_else(
                || match field {
                    LibraryField::Title | LibraryField::TitleMerged => title.clone(),
                    LibraryField::AlbumArtist => artist.clone(),
                    _ => String::new(),
                },
                |row| album_field(row, field),
            ),
            Self::Artist { name, row, .. } => row.as_ref().map_or_else(
                || match field {
                    LibraryField::Title | LibraryField::TitleMerged => name.clone(),
                    _ => String::new(),
                },
                |row| artist_field(row, field),
            ),
        }
    }

    fn field_links(&self, field: LibraryField) -> DetailLinks {
        let text = self.field(field);
        match self {
            Self::Track { row: Some(row), .. } => match field {
                LibraryField::Artist => track_artist_links(row),
                LibraryField::AlbumArtist => track_album_artist_links(row),
                LibraryField::Album => {
                    DetailLinks::route(&text, row.album_key.map(Route::AlbumDetail))
                }
                _ => DetailLinks::text(&text),
            },
            Self::Album { row: Some(row), .. } => match field {
                LibraryField::Artist | LibraryField::AlbumArtist => album_artist_links(row),
                LibraryField::Title => {
                    DetailLinks::route(&text, Some(Route::AlbumDetail(row.album_key)))
                }
                _ => DetailLinks::text(&text),
            },
            Self::Artist { row: Some(row), .. } if field == LibraryField::Title => {
                DetailLinks::route(&text, Some(Route::ArtistDetail(row.artist_key)))
            }
            _ => DetailLinks::text(&text),
        }
    }

    fn subtitle_links(&self) -> DetailLinks {
        match self {
            Self::Track { row: Some(row), .. } => track_artist_links(row),
            Self::Album { row: Some(row), .. } => album_artist_links(row),
            _ => DetailLinks::text(self.subtitle()),
        }
    }

    fn subtitle(&self) -> &str {
        match self {
            Self::Track { media, .. } => &media.artist,
            Self::Album { artist, .. } => artist,
            Self::Artist { .. } => "",
        }
    }

    fn is_downloaded(&self) -> bool {
        matches!(self, Self::Track { row: Some(row), .. } if row.is_downloaded)
    }

    fn numeric_field(&self, field: LibraryField) -> Option<i64> {
        match self {
            Self::Track { media, row, .. } => match field {
                LibraryField::Year => row.as_ref().and_then(|row| row.year).or(media.year),
                LibraryField::Duration => Some(
                    row.as_ref()
                        .map_or(media.duration_millis, |row| row.duration_millis),
                ),
                LibraryField::PlayCount => row.as_ref().map(|row| row.play_count),
                LibraryField::UserRating => {
                    row.as_ref().and_then(|row| row.rating).or(media.rating)
                }
                LibraryField::Favorite => Some(i64::from(
                    row.as_ref()
                        .map_or_else(|| media.favorite.unwrap_or(false), |row| row.favorite),
                )),
                _ => None,
            },
            Self::Album { row, .. } => row.as_ref().and_then(|row| match field {
                LibraryField::Year => row.year,
                LibraryField::Duration => Some(row.duration_millis),
                LibraryField::PlayCount => Some(row.play_count),
                LibraryField::UserRating => row.rating,
                LibraryField::SongCount => Some(row.track_count),
                LibraryField::Favorite => Some(i64::from(row.favorite)),
                _ => None,
            }),
            Self::Artist { row, .. } => row.as_ref().and_then(|row| match field {
                LibraryField::Duration => Some(row.duration_millis),
                LibraryField::PlayCount => Some(row.play_count),
                LibraryField::AlbumCount => Some(row.album_count),
                LibraryField::SongCount => Some(row.track_count),
                LibraryField::Favorite => Some(i64::from(row.favorite)),
                _ => None,
            }),
        }
    }

    fn date_field(&self, field: LibraryField) -> Option<&str> {
        match self {
            Self::Track { row, .. } => row.as_ref().and_then(|row| match field {
                LibraryField::ReleaseDate => row.release_date.as_deref(),
                LibraryField::DateAdded => row.date_added.as_deref(),
                _ => None,
            }),
            Self::Album { row, .. } => row.as_ref().and_then(|row| match field {
                LibraryField::ReleaseDate => row.release_date.as_deref(),
                LibraryField::DateAdded => row.date_added.as_deref(),
                _ => None,
            }),
            Self::Artist { .. } => None,
        }
    }

    fn route(&self) -> Option<Route> {
        match self {
            Self::Album { row: Some(row), .. } => Some(Route::AlbumDetail(row.album_key)),
            Self::Artist { row: Some(row), .. } => Some(Route::ArtistDetail(row.artist_key)),
            _ => None,
        }
    }

    fn favorite(&self) -> Option<(library::FavoriteTarget, bool)> {
        match self {
            Self::Track { row: Some(row), .. } => {
                Some((library::FavoriteTarget::Track(row.track_key), row.favorite))
            }
            Self::Album { row: Some(row), .. } => {
                Some((library::FavoriteTarget::Album(row.album_key), row.favorite))
            }
            Self::Artist { row: Some(row), .. } => Some((
                library::FavoriteTarget::Artist(row.artist_key),
                row.favorite,
            )),
            _ => None,
        }
    }

    fn play(&self, shell: &Rc<Shell>, placement: QueuePlacement) {
        match self {
            Self::Track { row: Some(row), .. } => {
                PlaybackTarget::Track(row.track_key).play(shell, placement, false);
            }
            Self::Track { media, .. } => {
                if let Some(selected) = shell.selected_library().as_deref().cloned()
                    && let Some(request) = selected.one_track(media.clone(), placement)
                {
                    shell.products.playback.queue.play_loaded(request);
                }
            }
            Self::Album { row: Some(row), .. } => {
                PlaybackTarget::Album(row.album_key).play(
                    shell,
                    placement,
                    placement == QueuePlacement::Now,
                );
            }
            Self::Artist { row: Some(row), .. } => {
                PlaybackTarget::Artist(row.artist_key).play(
                    shell,
                    placement,
                    placement == QueuePlacement::Now,
                );
            }
            Self::Album { object_id, .. } => {
                if let Some(operations) = shell.selected_source_operations() {
                    operations.play_live_search_collection(
                        crate::runtime::source::LiveSearchCollectionTarget::Album(
                            object_id.clone(),
                        ),
                        placement,
                    );
                }
            }
            Self::Artist { object_id, .. } => {
                if let Some(operations) = shell.selected_source_operations() {
                    operations.play_live_search_collection(
                        crate::runtime::source::LiveSearchCollectionTarget::Artist(
                            object_id.clone(),
                        ),
                        placement,
                    );
                }
            }
        }
    }

    fn activate(&self, shell: &Rc<Shell>) {
        if let Some(route) = self.route() {
            shell.navigate(route);
        } else {
            self.play(shell, QueuePlacement::Now);
        }
    }

    fn present_context(
        &self,
        target: &gtk::Widget,
        shell: &Rc<Shell>,
        position: Option<(f64, f64)>,
    ) {
        match self {
            Self::Track { row: Some(row), .. } => {
                present_track_context_menu(target, shell, row.clone(), position)
            }
            Self::Track { media, .. } => {
                present_track_context_menu(target, shell, media.clone(), position)
            }
            Self::Album { row: Some(row), .. } => {
                present_album_context_menu(target, shell, row.clone(), None, None, position)
            }
            Self::Artist { row: Some(row), .. } => {
                present_artist_context_menu(target, shell, row.clone(), false, None, position)
            }
            _ => {}
        }
    }
}

struct SearchGridCell {
    body: CollectionGridCardCell,
    shell: Rc<Shell>,
    cover: ArtworkTile,
    cover_button: gtk::Button,
    current: Rc<RefCell<Option<SearchItem>>>,
    favorite: gtk::Button,
}

impl SearchGridCell {
    fn new(shell: Rc<Shell>, fields: &[LibraryField]) -> Self {
        let current = Rc::new(RefCell::new(None::<SearchItem>));
        let controls = cards::CollectionGridCoverView::new(msgid("Play"));
        let cover_button = controls.imp().cover_button.get();
        let favorite = controls.favorite();
        let cover = ArtworkTile::new_elastic_square();
        cover_button.set_child(Some(&cover.widget()));
        install_grid_open(&shell, &current, &cover_button, |shell, item| {
            item.activate(shell);
        });

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
            favorite_shell.set_favorite_with_feedback(
                target,
                !favorite_button_is_active(button),
                Some(button),
            );
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
        install_grid_context(
            &shell,
            &current,
            &menu,
            &overlay,
            &body.card,
            |target, shell, item, point| item.present_context(target, shell, point),
        );
        Self {
            body,
            shell,
            cover,
            cover_button,
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
        self.shell.bind_artwork_tile(
            &self.cover,
            item.artwork(),
            COLLECTION_GRID_MAX_CARD_WIDTH,
            LARGE_COVER_SIZE,
        );
    }

    fn release_artwork(&self) {
        self.shell.clear_artwork_tile(&self.cover);
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
        self.cover_button.set_can_target(true);
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
        self.body.clear(&self.shell);
        self.cover_button.set_can_target(false);
        self.favorite.set_visible(false);
        self.favorite.set_sensitive(false);
        self.current.take();
    }

    fn set_ready(&self, ready: bool) {
        self.body.set_ready(ready);
    }

    fn apply_fields(&self, fields: &[LibraryField]) {
        self.body.replace_fields(&self.shell, fields);
        if let Some(item) = self.current.borrow().as_ref() {
            self.body
                .bind(item.title(), |field| DetailLinks::text(&item.field(field)));
        }
    }
}

struct SearchRouteProjection {
    root: gtk::Widget,
    shell: Weak<Shell>,
    selected: SelectedLibrary,
    search: gtk::SearchEntry,
    status: gtk::Stack,
    results: adw::ViewStack,
    result_page: gtk::Stack,
    toolbar_host: gtk::Box,
    models: [gio::ListStore; 3],
    playing: super::columns::TrackRowPlayingIndicator,
    active: RefCell<Option<SearchPresentation>>,
    generation: Cell<u64>,
    debounce: RefCell<Option<glib::SourceId>>,
    cancellation: RefCell<Option<ReadCancellation>>,
}

struct SearchPresentation {
    category: CollectionCategory,
    collection: LibraryCollectionProjection,
    toolbar: LibraryToolbarProjection,
    navigation: MountedRouteItemNavigation,
}

crate::ui_resource::composite_box!(
    pub(crate) SearchRouteView,
    search_route_view_imp,
    "RufinSearchRouteView",
    "/io/github/screwys/Rufin/ui/routes/search.ui",
    {
        toolbar_host: gtk::Box,
        category_host: gtk::Box,
        status: gtk::Stack,
        result_page: gtk::Stack,
    }
);

impl SearchRouteProjection {
    fn new(shell: &Rc<Shell>, selected: &SelectedLibrary) -> Rc<Self> {
        let wrapper = SearchRouteView::new();
        let search = gtk::SearchEntry::new();
        search.set_hexpand(true);
        search.set_width_request(1);
        bind_search_placeholder(&search, "Search");

        let models = CollectionCategory::ALL.map(|_| gio::ListStore::new::<glib::BoxedAnyObject>());
        let category_pages =
            CollectionCategory::ALL.map(|_| gtk::Box::new(gtk::Orientation::Vertical, 0).upcast());
        let (results, switcher) =
            collection_category_tabs(category_pages, CollectionCategory::default());
        wrapper.imp().category_host.append(&switcher);
        let toolbar_host = wrapper.imp().toolbar_host.get();
        let status = wrapper.imp().status.get();
        let result_page = wrapper.imp().result_page.get();

        let projection = Rc::new(Self {
            root: wrapper.upcast(),
            shell: Rc::downgrade(shell),
            selected: selected.clone(),
            search,
            status,
            results,
            result_page,
            toolbar_host,
            models,
            playing: super::columns::TrackRowPlayingIndicator::new(),
            active: RefCell::new(None),
            generation: Cell::new(0),
            debounce: RefCell::new(None),
            cancellation: RefCell::new(None),
        });
        projection.connect();
        projection.mount_category(CollectionCategory::default());
        projection.register_now_playing(shell);
        projection
    }

    fn connect(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.search.connect_search_changed(move |entry| {
            let Some(projection) = weak.upgrade() else {
                return;
            };
            if let Some(source) = projection.debounce.borrow_mut().take() {
                source.remove();
            }
            let query = entry.text().trim().to_string();
            if query.is_empty() {
                projection.reset();
                return;
            }
            let weak = Rc::downgrade(&projection);
            projection
                .debounce
                .replace(Some(glib::timeout_add_local_once(
                    SEARCH_DEBOUNCE,
                    move || {
                        if let Some(projection) = weak.upgrade() {
                            projection.debounce.borrow_mut().take();
                            projection.submit(query);
                        }
                    },
                )));
        });
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
        });
    }

    fn mount_category(&self, category: CollectionCategory) {
        if self.active.borrow().as_ref().map(|active| active.category) == Some(category) {
            return;
        }
        if let Some(previous) = self.active.borrow_mut().take() {
            if let Some(search_host) = self.search.parent().and_downcast::<gtk::Box>() {
                search_host.remove(&self.search);
            }
            self.toolbar_host.remove(&previous.toolbar.widget());
            self.result_page
                .remove(&previous.collection.scrolling_widget());
        }
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        self.sort_model(category);
        let collection = search_collection(
            &shell,
            self.models[category as usize].clone(),
            category,
            (category == CollectionCategory::Tracks).then(|| self.playing.clone()),
        );
        let toolbar = shell.library_toolbar_projection_without_detail(
            category.key(),
            self.search.clone(),
            category.sort_fields(),
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

    fn register_now_playing(&self, shell: &Rc<Shell>) {
        let model = self.models[CollectionCategory::Tracks as usize].clone();
        let indicator = self.playing.clone();
        let source = self.selected.source_key;
        shell.register_current_route_track_selection(Rc::new(move |current| {
            let current = current.filter(|current| current.source_id == source);
            let position = current
                .and_then(|current| {
                    (0..model.n_items()).find(|position| {
                        matches!(
                            super::library_fields::item_at::<SearchItem>(&model, *position),
                            Some(SearchItem::Track { media, .. })
                                if media.track_key == Some(current.track_id)
                        )
                    })
                })
                .unwrap_or(gtk::INVALID_LIST_POSITION);
            indicator.set_position(position);
            indicator.set_paused(current.is_some_and(|current| current.paused));
            true
        }));
    }

    fn show_results(&self, has_results: bool) {
        self.result_page
            .set_visible_child_name(if has_results { "results" } else { "empty" });
    }

    fn reset(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
        if let Some(cancellation) = self.cancellation.borrow_mut().take() {
            cancellation.cancel();
        }
        for model in &self.models {
            model.remove_all();
        }
        self.show_results(false);
        self.status.set_visible_child_name("initial");
    }

    fn submit(self: &Rc<Self>, query: String) {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        if let Some(cancellation) = self.cancellation.borrow_mut().take() {
            cancellation.cancel();
        }
        let cancellation = ReadCancellation::new();
        self.cancellation.replace(Some(cancellation.clone()));
        self.status.set_visible_child_name("loading");
        let selected = self.selected.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let prepared = acquire_search(&selected, query, cancellation).await;
            let Some(projection) = weak.upgrade() else {
                return;
            };
            if projection.generation.get() != generation {
                return;
            }
            projection.cancellation.borrow_mut().take();
            match prepared {
                Ok(items) => projection.apply(items),
                Err(error) => {
                    tracing::warn!(%error, "failed to load Search results");
                    projection.status.set_visible_child_name("error");
                }
            }
        });
    }

    fn apply(&self, mut items: [Vec<SearchItem>; 3]) {
        for index in 0..3 {
            if let Some(shell) = self.shell.upgrade() {
                let category = CollectionCategory::ALL[index];
                let settings = shell.settings.current.borrow().library_list(category.key());
                sort_search_items(&mut items[index], settings.sort_key, settings.descending);
            }
            let additions = items[index]
                .iter()
                .cloned()
                .map(glib::BoxedAnyObject::new)
                .collect::<Vec<_>>();
            self.models[index].splice(0, self.models[index].n_items(), &additions);
        }
        if let Some(active) = self.active.borrow().as_ref() {
            self.show_results(self.models[active.category as usize].n_items() != 0);
        }
        self.status.set_visible_child_name("results");
    }

    fn resume(&self) {
        for category in CollectionCategory::ALL {
            self.sort_model(category);
        }
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

    fn sort_model(&self, category: CollectionCategory) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        let settings = shell.settings.current.borrow().library_list(category.key());
        let model = &self.models[category as usize];
        let mut items = (0..model.n_items())
            .filter_map(|position| super::library_fields::item_at::<SearchItem>(model, position))
            .collect::<Vec<_>>();
        sort_search_items(&mut items, settings.sort_key, settings.descending);
        let additions = items
            .into_iter()
            .map(glib::BoxedAnyObject::new)
            .collect::<Vec<_>>();
        model.splice(0, model.n_items(), &additions);
    }

    fn cycle_category(&self) {
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

impl Drop for SearchRouteProjection {
    fn drop(&mut self) {
        if let Some(source) = self.debounce.get_mut().take() {
            source.remove();
        }
        if let Some(cancellation) = self.cancellation.get_mut().take() {
            cancellation.cancel();
        }
    }
}

fn sort_search_items(items: &mut [SearchItem], field: LibraryField, descending: bool) {
    items.sort_by(|left, right| {
        compare_optional(left.numeric_field(field), right.numeric_field(field))
            .then_with(|| compare_optional(left.date_field(field), right.date_field(field)))
            .then_with(|| {
                left.field(field)
                    .to_lowercase()
                    .cmp(&right.field(field).to_lowercase())
            })
            .then_with(|| left.title().cmp(right.title()))
    });
    if descending {
        items.reverse();
    }
}

fn compare_optional<T: Ord>(left: Option<T>, right: Option<T>) -> std::cmp::Ordering {
    left.cmp(&right)
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

impl Shell {
    pub(crate) fn install_favorite_category_tabs(
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
                shell.set_favorite_category(shell.navigation.favorite_category.get().next());
            }
        }));
    }

    pub(crate) fn search_route(self: &Rc<Self>, selected: &SelectedLibrary) -> MountedRoute {
        let projection = SearchRouteProjection::new(self, selected);
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
    shell: &Rc<Shell>,
    model: gio::ListStore,
    category: CollectionCategory,
    playing: Option<super::columns::TrackRowPlayingIndicator>,
) -> LibraryCollectionProjection {
    let settings = shell.settings.current.borrow().library_list(category.key());
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
                    None,
                    route_column_view_initial_width(&shell),
                ))
            }
        }),
    )
}

fn search_column(
    shell: &Rc<Shell>,
    category: CollectionCategory,
    field: LibraryField,
    playing: Option<&super::columns::TrackRowPlayingIndicator>,
) -> gtk::ColumnViewColumn {
    if field == LibraryField::RowIndex {
        if category == CollectionCategory::Tracks
            && let Some(playing) = playing
        {
            return super::columns::mapped_track_row_index_column_with_width::<SearchItem, _>(
                super::columns::track_column_width(category.key(), field),
                playing.clone(),
                |item| matches!(item, SearchItem::Track { .. }),
            );
        }
        return super::columns::row_index_column_with_width(column_width(field));
    }
    if field == LibraryField::Image {
        return super::columns::artwork_column::<SearchItem, _>(
            shell,
            field.title(),
            column_width(field),
            SearchItem::artwork,
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
    if field == LibraryField::Favorite {
        return search_favorite_column(shell);
    }
    let factory = gtk::SignalListItemFactory::new();
    let setup_shell = Rc::clone(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = RecycledTextCell::new();
        let label = cell.label();
        label.set_halign(gtk::Align::Fill);
        label.set_hexpand(true);
        add_field_skeleton_class(&label, field);
        cell.enable_links(&setup_shell);
        item.set_child(Some(&cell));
    });
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = list_cell::<RecycledTextCell>(item) else {
            return;
        };
        if let Some(row) = item_at_from_item::<SearchItem>(item) {
            cell.bind_links(row.field_links(field));
        } else {
            cell.clear();
        }
    });
    factory.connect_unbind(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>()
            && let Some(cell) = list_cell::<RecycledTextCell>(item)
        {
            cell.clear();
        }
    });
    gtk::ColumnViewColumn::new(Some(field.title()), Some(factory))
}

fn search_merged_column(
    shell: &Rc<Shell>,
    category: CollectionCategory,
    width: i32,
    playing: Option<super::columns::TrackRowPlayingIndicator>,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let setup_shell = Rc::clone(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell =
            RecycledMergedCell::new(&setup_shell, 48, category == CollectionCategory::Tracks);
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
        if category == CollectionCategory::Tracks {
            let context_shell = Rc::downgrade(&setup_shell);
            let context_item = item.downgrade();
            install_context_menu_openers(
                &cell,
                Rc::new(move |target, position| {
                    if let (Some(shell), Some(item)) =
                        (context_shell.upgrade(), context_item.upgrade())
                        && let Some(value) = item_at_from_item::<SearchItem>(&item)
                    {
                        value.present_context(target, &shell, position);
                    }
                }),
            );
        }
        item.set_child(Some(&cell));
    });
    let bind_shell = Rc::clone(shell);
    let bind_playing = playing.clone();
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = list_cell::<RecycledMergedCell>(item) else {
            return;
        };
        let cover = cell.cover();
        let title = cell.title();
        let Some(value) = item_at_from_item::<SearchItem>(item) else {
            bind_shell.clear_artwork_tile(&cover);
            title.set_text("");
            cell.clear_subtitle();
            if let Some(downloaded) = cell.downloaded() {
                bind_shell.clear_download_badge(&downloaded);
            }
            if let Some(playing) = bind_playing.as_ref() {
                playing.unbind(title.upcast_ref());
            }
            return;
        };
        bind_shell.bind_artwork_tile(
            &cover,
            value.artwork(),
            48,
            crate::shell::cover::THUMB_COVER_SIZE,
        );
        title.set_text(value.title());
        cell.bind_subtitle(value.subtitle_links());
        cell.subtitle().set_visible(!value.subtitle().is_empty());
        if let Some(downloaded) = cell.downloaded() {
            bind_shell.bind_download_badge(&downloaded, value.is_downloaded());
        }
        if let Some(playing) = bind_playing.as_ref() {
            playing.bind(title.upcast_ref(), item.position());
        }
    });
    let clear_shell = Rc::clone(shell);
    factory.connect_unbind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(cell) = list_cell::<RecycledMergedCell>(item) {
            let title = cell.title();
            clear_shell.clear_artwork_tile(&cell.cover());
            title.set_text("");
            cell.clear_subtitle();
            if let Some(downloaded) = cell.downloaded() {
                clear_shell.clear_download_badge(&downloaded);
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

fn search_favorite_column(shell: &Rc<Shell>) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let setup_shell = Rc::clone(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let button = crate::favorites::column_favorite_icon_button("Favorite");
        let context_shell = Rc::downgrade(&setup_shell);
        let context_item = item.downgrade();
        install_context_menu_openers(
            &button,
            Rc::new(move |target, position| {
                if let (Some(shell), Some(item)) = (context_shell.upgrade(), context_item.upgrade())
                    && let Some(value) = item_at_from_item::<SearchItem>(&item)
                    && value.favorite().is_some()
                {
                    value.present_context(target, &shell, position);
                }
            }),
        );
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
            click_shell.set_favorite_with_feedback(
                target,
                !favorite_button_is_active(button),
                Some(button),
            );
        });
        item.set_child(Some(&button));
    });
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(value) = item_at_from_item::<SearchItem>(item) else {
            return;
        };
        let Some(button) = item.child().and_downcast::<gtk::Button>() else {
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
        if let Some(button) = item.child().and_downcast::<gtk::Button>() {
            set_favorite_button_active(&button, false);
            button.set_sensitive(false);
        }
    });
    let column =
        gtk::ColumnViewColumn::new(Some(crate::favorites::FAVORITE_COLUMN_TITLE), Some(factory));
    column.set_fixed_width(column_width(LibraryField::Favorite));
    column
}

async fn acquire_search(
    selected: &SelectedLibrary,
    query: String,
    cancellation: ReadCancellation,
) -> Result<[Vec<SearchItem>; 3], String> {
    if selected.artwork.source_id.as_str() == sources::LOCAL_LIBRARY_SOURCE_ID {
        let database = Arc::clone(&selected.database);
        let source = selected.source_key;
        let folder = selected.music_folder_key;
        let task = selected.runtime.spawn(async move {
            database
                .search(
                    source,
                    folder,
                    false,
                    &SearchRequest::with_limit(query, 60),
                    &cancellation,
                )
                .await
        });
        return task
            .await
            .map_err(|error| error.to_string())?
            .map(cached_items)
            .map_err(|error| error.to_string());
    }
    let receiver = selected.operations.search(query.clone(), 60);
    match receiver.recv().await {
        Ok(Ok(live)) => {
            let resolved = resolve_live_rows(selected, &live, &cancellation)
                .await
                .unwrap_or_default();
            Ok(live_items(
                live,
                resolved,
                selected.artwork.source_id.as_str(),
            ))
        }
        _ => {
            let database = Arc::clone(&selected.database);
            let source = selected.source_key;
            let folder = selected.music_folder_key;
            selected
                .runtime
                .spawn(async move {
                    database
                        .search(
                            source,
                            folder,
                            false,
                            &SearchRequest::with_limit(query, 60),
                            &cancellation,
                        )
                        .await
                })
                .await
                .map_err(|error| error.to_string())?
                .map(cached_items)
                .map_err(|error| error.to_string())
        }
    }
}

async fn resolve_live_rows(
    selected: &SelectedLibrary,
    live: &LiveSearchResults,
    cancellation: &ReadCancellation,
) -> Option<SearchResults> {
    let database = Arc::clone(&selected.database);
    let source = selected.source_key;
    let folder = selected.music_folder_key;
    let tracks = live
        .tracks
        .iter()
        .map(|row| row.object_id.clone())
        .collect::<Vec<_>>();
    let albums = live
        .albums
        .iter()
        .map(|row| row.object_id.clone())
        .collect::<Vec<_>>();
    let artists = live
        .artists
        .iter()
        .map(|row| row.object_id.clone())
        .collect::<Vec<_>>();
    let cancellation = cancellation.clone();
    selected
        .runtime
        .spawn(async move {
            database
                .search_rows_by_objects(
                    source,
                    folder,
                    false,
                    &tracks,
                    &albums,
                    &artists,
                    &cancellation,
                )
                .await
        })
        .await
        .ok()
        .and_then(Result::ok)
}

fn cached_items(results: SearchResults) -> [Vec<SearchItem>; 3] {
    [
        results
            .tracks
            .into_iter()
            .map(|row| SearchItem::Track {
                media: playback::PlaybackMedia::from(row.clone()),
                artwork: row.artwork_binding.clone(),
                row: Some(row),
            })
            .collect(),
        results
            .albums
            .into_iter()
            .map(|row| SearchItem::Album {
                object_id: row.object_id.clone(),
                title: row.title.clone(),
                artist: row.display_artist.clone(),
                artwork: row.artwork_binding.clone(),
                row: Some(row),
            })
            .collect(),
        results
            .artists
            .into_iter()
            .map(|row| SearchItem::Artist {
                object_id: row.object_id.clone(),
                name: row.name.clone(),
                artwork: row.artwork_binding.clone(),
                row: Some(row),
            })
            .collect(),
    ]
}

fn live_items(
    live: LiveSearchResults,
    resolved: SearchResults,
    source_id: &str,
) -> [Vec<SearchItem>; 3] {
    let tracks = live
        .tracks
        .into_iter()
        .map(|live| {
            let row = resolved
                .tracks
                .iter()
                .find(|row| row.object_id == live.object_id)
                .cloned();
            let media = row
                .as_ref()
                .cloned()
                .map(playback::PlaybackMedia::from)
                .unwrap_or_else(|| live_track_media(live.clone(), source_id));
            SearchItem::Track {
                media,
                row,
                artwork: live.artwork_binding,
            }
        })
        .collect();
    let albums = live
        .albums
        .into_iter()
        .map(|live| SearchItem::Album {
            row: resolved
                .albums
                .iter()
                .find(|row| row.object_id == live.object_id)
                .cloned(),
            object_id: live.object_id,
            title: live.title,
            artist: live.artist,
            artwork: live.artwork_binding,
        })
        .collect();
    let artists = live
        .artists
        .into_iter()
        .map(|live| SearchItem::Artist {
            row: resolved
                .artists
                .iter()
                .find(|row| row.object_id == live.object_id)
                .cloned(),
            object_id: live.object_id,
            name: live.name,
            artwork: live.artwork_binding,
        })
        .collect();
    [tracks, albums, artists]
}

fn live_track_media(track: sources::LiveSearchTrack, source_id: &str) -> playback::PlaybackMedia {
    playback::PlaybackMedia {
        source_id: source_id.to_string(),
        track_key: None,
        track_object_id: track.object_id,
        title: track.title,
        artist: track.artist,
        album: track.album,
        album_display_artist: None,
        album_key: None,
        primary_artist_key: None,
        media_uri: None,
        artwork_binding: track.artwork_binding,
        duration_millis: 0,
        disc_number: None,
        track_number: None,
        year: None,
        release_date: None,
        favorite: None,
        rating: None,
        is_downloaded: false,
        source_format: None,
        musicbrainz_recording_id: None,
        musicbrainz_release_track_id: None,
        musicbrainz_album_id: None,
        musicbrainz_release_group_id: None,
        primary_artist_musicbrainz_id: None,
        cue_path: None,
        cue_start_millis: None,
        cue_end_millis: None,
        artist_links: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a GTK display"]
    fn search_route_template_builds() {
        gtk::init().expect("GTK display");
        crate::application::verify_interface_resources().expect("compiled interface resources");
        let view = SearchRouteView::new();
        assert_eq!(
            view.imp().status.visible_child_name().as_deref(),
            Some("initial")
        );
        assert_eq!(
            view.imp().result_page.visible_child_name().as_deref(),
            Some("empty")
        );
    }

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
    fn provider_only_track_keeps_a_playable_source_identity() {
        let media = live_track_media(
            sources::LiveSearchTrack {
                object_id: "provider-track".to_string(),
                title: "Track".to_string(),
                artist: "Artist".to_string(),
                album: "Album".to_string(),
                artwork_binding: None,
            },
            "source",
        );
        assert_eq!(media.track_object_id, "provider-track");
        assert!(media.track_key.is_none());
    }

    #[test]
    fn optional_search_values_follow_native_sort_direction() {
        assert_eq!(
            compare_optional::<i64>(None, Some(1)),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            compare_optional(Some(1_i64), None),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            compare_optional(Some(1_i64), Some(2)),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn accepted_search_metadata_keeps_its_exact_route_link() {
        let artist_key = library::ArtistKey::from_raw(7);
        let item = SearchItem::Artist {
            object_id: "artist-object".to_string(),
            name: "Performer".to_string(),
            artwork: None,
            row: Some(library::ArtistRow {
                artist_key,
                source_key: library::SourceKey::from_raw(3),
                object_id: "artist-object".to_string(),
                name: "Performer".to_string(),
                musicbrainz_artist_id: None,
                artwork_binding: None,
                favorite: false,
                rating: None,
                play_count: 0,
                last_played: None,
                album_count: 1,
                track_count: 1,
                duration_millis: 1,
                downloaded_count: 0,
            }),
        };
        assert_eq!(
            item.field_links(LibraryField::Title),
            DetailLinks::route("Performer", Some(Route::ArtistDetail(artist_key)))
        );
    }
}
