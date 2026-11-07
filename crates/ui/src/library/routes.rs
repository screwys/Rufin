use rufin_core::view::OpenOptions;
use std::{cell::RefCell, rc::Rc, time::Duration};

use adw::prelude::*;
use gtk::glib;

use crate::library::CatalogUi;
use crate::library::{LibraryLayout, LibraryListKey, LibraryListSettings};
use crate::widgets::localization::bind_search_placeholder;
use crate::widgets::mounted_route::MountedRoute;
use crate::widgets::view_model::Rows;
use library::Row;
use localization::msgid;
use rufin_core::view::{Opened, Reread};

use super::album_detail::AlbumCollectionModels;
use super::collections::{
    album_collection_projection, library_route_inset, track_collection_projection,
};
use super::route_shell::{LibraryPageShellOptions, LibraryToolbarProjection};
use rufin_core::route::{CollectionCategory, Route};

const TRACK_FILTER_DEBOUNCE: Duration = Duration::from_millis(150);

pub struct SearchableTrackOptions {
    pub content_inset: i32,
    pub fixed_layout: Option<LibraryLayout>,
    pub search: Option<gtk::SearchEntry>,
}

/// What a page reads again, the options it wants read, and what it does with each read.
/// Widgets hold it weakly; the mounted route holds it.
#[derive(Clone)]
pub(crate) struct PageReads(Rc<PageReadsInner>);

pub(crate) struct PageReadsInner {
    rows: Rows<Row>,
    wanted: Box<dyn Fn(&OpenOptions) -> OpenOptions>,
    apply: Rc<dyn Fn(Opened)>,
}

impl PageReads {
    pub fn new(
        rows: &Rows<Row>,
        wanted: impl Fn(&OpenOptions) -> OpenOptions + 'static,
        apply: impl Fn(Opened) + 'static,
    ) -> Self {
        Self(Rc::new(PageReadsInner {
            rows: rows.clone(),
            wanted: Box::new(wanted),
            apply: Rc::new(apply),
        }))
    }

    /// Calls `read` on these reads while the page holds them.
    pub fn weak(&self, read: impl Fn(&Self) + 'static) -> impl Fn() + 'static {
        let reads = Rc::downgrade(&self.0);
        move || {
            if let Some(reads) = reads.upgrade() {
                read(&Self(reads));
            }
        }
    }

    fn wanted(&self) -> Option<OpenOptions> {
        self.0
            .rows
            .view()
            .map(|view| (self.0.wanted)(view.options()))
    }

    fn applied(&self) -> impl FnOnce(Opened) + 'static {
        let apply = Rc::clone(&self.0.apply);
        move |read| apply(read)
    }

    /// Reads from the top, for a new search or filter.
    pub fn reread(&self) {
        if let Some(options) = self.wanted() {
            self.0.rows.reread(
                Reread::Page,
                options,
                library::RouteSeedWindow::top(),
                self.applied(),
            );
        }
    }

    /// Reads an artist page's releases again.
    pub fn reread_releases(&self) {
        if let Some(options) = self.wanted() {
            self.0.rows.reread(
                Reread::Releases,
                options,
                library::RouteSeedWindow::top(),
                self.applied(),
            );
        }
    }

    /// Reads again for a committed change, where the list is.
    pub fn refresh(&self) {
        if let Some(options) = self.wanted() {
            self.0.rows.refresh(options, self.applied());
        }
    }

    /// Reads again what `settings` change.
    pub fn resume(&self, settings: &rufin_core::settings::app::Settings) {
        self.0.rows.resume(settings, self.applied());
    }
}

#[derive(Clone)]
pub struct TrackListProjection {
    key: LibraryListKey,
    search: gtk::SearchEntry,
    collection: super::collections::LibraryCollectionProjection,
    pub(crate) rows: Rows<Row>,
    fixed_layout: Option<LibraryLayout>,
}

impl TrackListProjection {
    pub fn search(&self) -> gtk::SearchEntry {
        self.search.clone()
    }

    pub fn scrolling_widget(&self) -> gtk::Widget {
        self.collection.scrolling_widget()
    }

    pub fn item_navigation(&self) -> crate::widgets::mounted_route::MountedRouteItemNavigation {
        self.collection.item_navigation()
    }

    pub fn mount_in_scroller(&self, scroller: &gtk::ScrolledWindow) -> gtk::Widget {
        self.collection.mount_in_scroller(scroller, 0, 0)
    }

    pub fn source_is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn apply_library_list_settings(
        &self,
        key: LibraryListKey,
        settings: &crate::library::LibraryListSettings,
    ) {
        if key != self.key {
            return;
        }
        let mut settings = settings.clone();
        if let Some(layout) = self.fixed_layout {
            settings.layout = layout;
        }
        self.collection.apply_settings(&settings);
    }

    /// The page's rereads, for this list's search and the inputs `scope` adds. A search reads
    /// at once when it clears and after a pause while typing.
    pub fn page_reads(
        &self,
        scope: impl Fn(&mut OpenOptions) + 'static,
        changed: impl Fn(Option<library::Header>) + 'static,
    ) -> PageReads {
        let inputs = list_inputs(&self.search);
        let reads = PageReads::new(
            &self.rows,
            move |options| {
                let mut options = inputs(options);
                scope(&mut options);
                options
            },
            move |read| changed(read.header),
        );
        let reread: Rc<dyn Fn()> = Rc::new(reads.weak(PageReads::reread));
        let pending = Rc::new(RefCell::new(None::<glib::SourceId>));
        self.search.connect_search_changed(move |search| {
            if let Some(source) = pending.borrow_mut().take() {
                source.remove();
            }
            if search.text().trim().is_empty() {
                reread();
                return;
            }
            let reread = Rc::clone(&reread);
            let pending_timeout = Rc::clone(&pending);
            pending.replace(Some(glib::timeout_add_local_once(
                TRACK_FILTER_DEBOUNCE,
                move || {
                    pending_timeout.borrow_mut().take();
                    reread();
                },
            )));
        });
        reads
    }
}

/// What a list page wants read: the options it opened with and its search.
pub(crate) fn list_inputs(
    search: &gtk::SearchEntry,
) -> impl Fn(&OpenOptions) -> OpenOptions + use<> {
    let search = search.clone();
    move |options| OpenOptions {
        filter: search.text().trim().to_string(),
        ..options.clone()
    }
}

/// Reads a list page again whenever its search changes; lists search as you type.
pub(crate) fn connect_list_search(search: &gtk::SearchEntry, reads: &PageReads) {
    let reread = reads.weak(PageReads::reread);
    search.connect_search_changed(move |_| reread());
}

impl CatalogUi {
    pub fn opened_collection_view(self: &Rc<Self>, read: Opened) -> MountedRoute {
        let route = read.view.route().clone();
        let category = read.view.options().category;
        match route {
            Route::History => self.history_route(read),
            Route::Favorites if category == CollectionCategory::Tracks => self.root_track_route(
                LibraryListKey::FavoriteTracks,
                msgid("No favorites yet"),
                read,
                true,
            ),
            Route::Tracks => self.root_track_route(
                LibraryListKey::Tracks,
                msgid("Nothing here yet"),
                read,
                false,
            ),
            Route::Albums => self.library_albums_route(read, false),
            Route::Favorites if category == CollectionCategory::Albums => {
                self.library_albums_route(read, true)
            }
            Route::Artists | Route::AlbumArtists | Route::Favorites => {
                self.library_artist_list_route(read, route)
            }
            Route::AlbumDetail(_) => self.album_detail_view(read),
            Route::ArtistDetail(_) | Route::AlbumArtistDetail(_) => {
                self.artist_detail_view(matches!(route, Route::AlbumArtistDetail(_)), read)
            }
            Route::ArtistDiscography(_) | Route::AlbumArtistDiscography(_) => {
                self.artist_discography_view(read)
            }
            Route::ArtistTracks(_) | Route::AlbumArtistTracks(_) => {
                self.artist_tracks_view(matches!(route, Route::AlbumArtistTracks(_)), read)
            }
            Route::ArtistFavoriteTracks(_) | Route::AlbumArtistFavoriteTracks(_) => self
                .artist_favorite_tracks_view(
                    matches!(route, Route::AlbumArtistFavoriteTracks(_)),
                    read,
                ),
            Route::PlaylistDetail(key) => self.playlist_detail_route(key, read),
            Route::SmartPlaylistDetail(_) => self.smart_playlist_detail_route(read),
            Route::GenreDetail(key) => {
                self.named_detail_view(super::named_detail::NamedDetailId::Genre(key), read)
            }
            Route::MoodDetail(key) => {
                self.named_detail_view(super::named_detail::NamedDetailId::Mood(key), read)
            }
            Route::Genres | Route::Moods => self.library_tags_route(read, route == Route::Genres),
            Route::Playlists | Route::PlaylistFolder { smart: false, .. } => {
                self.library_playlists_route(read)
            }
            Route::SmartPlaylists | Route::PlaylistFolder { smart: true, .. } => {
                self.library_smart_playlists_route(read)
            }
            Route::Folders { path } => self.folders_route(path, read),
            Route::Home => self
                .home_route(read.view.home_sections().unwrap_or_default())
                .with_view(read.view),
            Route::Search => self.search_route(),
        }
    }

    pub fn scrolling_track_projection(
        self: &Rc<Self>,
        rows: Rows<Row>,
        key: LibraryListKey,
        context: &str,
    ) -> (gtk::Widget, TrackListProjection, LibraryToolbarProjection) {
        let settings = self.settings.current.borrow().library_list(key);
        let projection = self.build_track_collection(
            rows,
            key,
            settings,
            SearchableTrackOptions {
                content_inset: 0,
                fixed_layout: None,
                search: None,
            },
        );
        let wrapper = gtk::Box::new(gtk::Orientation::Vertical, 10);
        wrapper.set_widget_name(context);
        wrapper.set_hexpand(true);
        wrapper.set_vexpand(true);
        let toolbar = self.library_toolbar_projection(key, projection.search());
        wrapper.append(&library_route_inset(toolbar.widget()));
        wrapper.append(&projection.scrolling_widget());
        (wrapper.upcast(), projection, toolbar)
    }

    pub fn library_albums_route(
        self: &Rc<Self>,
        mut read: Opened,
        favorites_only: bool,
    ) -> MountedRoute {
        let key = LibraryListKey::Albums;
        let models = Rc::new(RefCell::new(AlbumCollectionModels::new(
            &self.products.runtime,
            &mut read,
        )));
        let search = gtk::SearchEntry::new();
        crate::widgets::controls::configure_search_entry(&search);
        bind_search_placeholder(&search, "Search");
        let content = album_collection_projection(self, models.clone(), key);
        let mut page = self.library_page_shell(LibraryPageShellOptions {
            key,
            empty: models.borrow().is_empty(),
            empty_body: if favorites_only {
                msgid("No favorite albums yet")
            } else {
                msgid("Nothing here yet")
            },
            search: search.clone(),
            has_visible_results: {
                let models = models.clone();
                Rc::new(move || !models.borrow().is_empty())
            },
            content: content.scrolling_widget(),
        });
        // The rows take each read; a read in the other layout shows them in that layout.
        let apply = {
            let shell = Rc::downgrade(self);
            let models = Rc::downgrade(&models);
            let content = content.clone();
            let page = page.clone();
            move |read: Opened| {
                let (Some(shell), Some(models)) = (shell.upgrade(), models.upgrade()) else {
                    return;
                };
                let other = models.borrow().take(&read);
                if let Some(other) = other {
                    models.replace(other);
                }
                let settings = shell.settings.current.borrow().library_list(key);
                content.apply_settings(&settings);
                page.apply_library_list_settings(key, &settings);
                page.set_empty(models.borrow().is_empty());
            }
        };
        let rows = models.borrow().rows().clone();
        let reads = PageReads::new(&rows, list_inputs(&search), apply);
        connect_list_search(&search, &reads);
        let resume = {
            let shell = Rc::downgrade(self);
            let content = content.clone();
            let models = models.clone();
            let page = page.clone();
            let reads = reads.clone();
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else { return };
                let settings = shell.settings.current.borrow().library_list(key);
                if models.borrow().is_detail() == (settings.layout == LibraryLayout::Detail) {
                    content.apply_settings(&settings);
                }
                page.apply_library_list_settings(key, &settings);
                reads.resume(&shell.settings.current.borrow());
            })
        };
        if favorites_only {
            self.install_favorite_category_tabs(CollectionCategory::Albums, &mut page);
        }
        page.mounted_route(resume)
            .with_catalog_refresh(Rc::new(move || reads.refresh()))
            .with_item_navigation(content.item_navigation())
            .with_item_scroll(content.item_scroll())
            .with_initial_demand({
                let models = models.clone();
                Rc::new(move || models.borrow().rows().resume_initial_demand())
            })
            .with_rows(&rows)
    }

    fn root_track_route(
        self: &Rc<Self>,
        key: LibraryListKey,
        empty_body: &'static str,
        mut read: Opened,
        favorites_only: bool,
    ) -> MountedRoute {
        let projection = self.track_projection(
            Rows::over_view(self.products.runtime.clone(), &mut read),
            key,
            SearchableTrackOptions {
                content_inset: 0,
                fixed_layout: None,
                search: None,
            },
        );
        let mut page = self.track_page_shell(key, empty_body, &projection);
        let apply_page = page.clone();
        let apply_projection = projection.clone();
        let reads = projection.page_reads(
            |_| {},
            move |_| apply_page.set_empty(apply_projection.source_is_empty()),
        );
        if favorites_only {
            self.install_favorite_category_tabs(CollectionCategory::Tracks, &mut page);
        }
        self.mount_track_page(key, projection, page, reads)
    }

    pub fn history_route(self: &Rc<Self>, mut read: Opened) -> MountedRoute {
        let key = LibraryListKey::History;
        let projection = self.track_projection(
            Rows::over_view(self.products.runtime.clone(), &mut read),
            key,
            SearchableTrackOptions {
                content_inset: 0,
                fixed_layout: None,
                search: None,
            },
        );
        let page = self.track_page_shell(key, msgid("Nothing played yet"), &projection);
        let apply_page = page.clone();
        let apply_projection = projection.clone();
        // The current source as the filter was picked; later source changes leave it.
        let source = Rc::new(RefCell::new(None));
        let scope = Rc::clone(&source);
        let reads = projection.page_reads(
            move |options| options.history_source.clone_from(&scope.borrow()),
            move |_| apply_page.set_empty(apply_projection.source_is_empty()),
        );
        let reread = reads.weak(PageReads::reread);
        let filter_shell = Rc::downgrade(self);
        let source_name = {
            let configured = self.source.configured.borrow();
            configured
                .sources
                .iter()
                .find(|source| Some(&source.id) == configured.selected_source_id.as_ref())
                .map(crate::widgets::source_labels::configured_source_display_name)
        };
        page.configure_history_filter(source_name, move |current_only| {
            source.replace(
                current_only
                    .then(|| {
                        filter_shell
                            .upgrade()
                            .and_then(|shell| shell.history_source_id())
                    })
                    .flatten(),
            );
            reread();
        });
        self.mount_track_page(key, projection, page, reads)
    }

    pub fn library_artist_list_route(self: &Rc<Self>, read: Opened, route: Route) -> MountedRoute {
        let (key, empty_body) = match route {
            Route::Favorites => (LibraryListKey::Artists, msgid("No favorite artists yet")),
            Route::AlbumArtists => (LibraryListKey::AlbumArtists, msgid("Nothing here yet")),
            _ => (LibraryListKey::Artists, msgid("Nothing here yet")),
        };
        self.collection_list_route::<library::ArtistRow>(
            key,
            empty_body,
            route == Route::Favorites,
            read,
        )
    }

    pub fn library_playlists_route(self: &Rc<Self>, read: Opened) -> MountedRoute {
        self.collection_list_route::<library::PlaylistListRow>(
            LibraryListKey::Playlists,
            msgid("No playlists yet"),
            false,
            read,
        )
    }

    pub fn library_smart_playlists_route(self: &Rc<Self>, read: Opened) -> MountedRoute {
        self.collection_list_route::<library::PlaylistListRow>(
            LibraryListKey::SmartPlaylists,
            msgid("No smart playlists yet"),
            false,
            read,
        )
    }

    pub fn track_projection(
        self: &Rc<Self>,
        rows: Rows<Row>,
        key: LibraryListKey,
        options: SearchableTrackOptions,
    ) -> TrackListProjection {
        let mut settings = self.settings.current.borrow().library_list(key);
        if let Some(layout) = options.fixed_layout {
            settings.layout = layout;
        }
        self.build_track_collection(rows, key, settings, options)
    }

    fn build_track_collection(
        self: &Rc<Self>,
        rows: Rows<Row>,
        key: LibraryListKey,
        settings: LibraryListSettings,
        options: SearchableTrackOptions,
    ) -> TrackListProjection {
        let search = options.search.unwrap_or_else(|| {
            let search = gtk::SearchEntry::new();
            crate::widgets::controls::configure_search_entry(&search);
            search
        });
        bind_search_placeholder(&search, "Search");
        search.set_hexpand(true);
        let collection =
            track_collection_projection(self, rows.clone(), key, settings, options.content_inset);
        TrackListProjection {
            key,
            search,
            collection,
            rows,
            fixed_layout: options.fixed_layout,
        }
    }

    fn track_page_shell(
        self: &Rc<Self>,
        key: LibraryListKey,
        empty_body: &'static str,
        projection: &TrackListProjection,
    ) -> super::route_shell::LibraryPageShell {
        self.library_page_shell(LibraryPageShellOptions {
            key,
            empty: projection.source_is_empty(),
            empty_body,
            search: projection.search(),
            has_visible_results: {
                let rows = projection.rows.clone();
                Rc::new(move || !rows.is_empty())
            },
            content: projection.scrolling_widget(),
        })
    }

    fn mount_track_page(
        self: &Rc<Self>,
        key: LibraryListKey,
        projection: TrackListProjection,
        page: super::route_shell::LibraryPageShell,
        reads: PageReads,
    ) -> MountedRoute {
        let resume = {
            let shell = Rc::downgrade(self);
            let projection = projection.clone();
            let page = page.clone();
            let reads = reads.clone();
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else { return };
                let settings = shell.settings.current.borrow().library_list(key);
                projection.apply_library_list_settings(key, &settings);
                page.apply_library_list_settings(key, &settings);
                reads.resume(&shell.settings.current.borrow());
            })
        };
        page.mounted_route(resume)
            .with_catalog_refresh(Rc::new(move || reads.refresh()))
            .with_item_navigation(projection.item_navigation())
            .with_initial_demand({
                let rows = projection.rows.clone();
                Rc::new(move || rows.resume_initial_demand())
            })
            .with_rows(&projection.rows)
    }
}
