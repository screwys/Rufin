use rufin_core::view::OpenOptions;
use std::{cell::RefCell, rc::Rc, time::Duration};

use adw::prelude::*;
use gtk::glib;

use crate::CatalogUi;
use crate::{LibraryLayout, LibraryListKey, LibraryListSettings};
use gtk_widgets::localization::bind_search_placeholder;
use gtk_widgets::mounted_route::{MountedRoute, PageReread, RouteView, route_view};
use localization::msgid;
use rufin_core::collection_context::CollectionTarget;
use rufin_core::view::{Opened, View};

use super::album_detail::AlbumCollectionModels;
use super::collections::{
    album_collection_projection, library_route_inset, track_collection_projection,
};
use super::route_shell::{LibraryPageShellOptions, LibraryToolbarProjection};
use crate::track_model::TrackCollectionModel;
use rufin_core::route::{CollectionCategory, Route};

const TRACK_FILTER_DEBOUNCE: Duration = Duration::from_millis(150);

struct RootTrackRouteOptions {
    key: LibraryListKey,
    empty_body: &'static str,
}

pub struct SearchableTrackOptions {
    pub context_id: String,
    pub content_inset: i32,
    pub fixed_layout: Option<LibraryLayout>,
    pub search: Option<gtk::SearchEntry>,
}

#[derive(Clone)]
pub struct TrackListProjection {
    key: LibraryListKey,
    search: gtk::SearchEntry,
    collection: super::collections::LibraryCollectionProjection,
    model: TrackCollectionModel,
    fixed_layout: Option<LibraryLayout>,
}

impl TrackListProjection {
    pub fn download_change(&self) -> gtk_widgets::mounted_route::MountedDownloadChange {
        let model = self.model.clone();
        Rc::new(move |event| {
            if let downloads::DownloadEvent::Changed {
                media_uri,
                downloaded,
            } = event
            {
                model.update_downloaded(media_uri, *downloaded);
            }
        })
    }

    pub fn search(&self) -> gtk::SearchEntry {
        self.search.clone()
    }

    pub fn scrolling_widget(&self) -> gtk::Widget {
        self.collection.scrolling_widget()
    }

    pub fn item_navigation(&self) -> gtk_widgets::mounted_route::MountedRouteItemNavigation {
        self.collection.item_navigation()
    }

    pub fn mount_in_scroller(&self, scroller: &gtk::ScrolledWindow) -> gtk::Widget {
        self.collection.mount_in_scroller(scroller, 0, 0)
    }

    pub fn source_is_empty(&self) -> bool {
        self.model.source_is_empty()
    }

    pub fn source_count(&self) -> usize {
        self.model.visible_count()
    }

    pub fn play_source(
        &self,
        queue: std::sync::Arc<rufin_core::playback::PlaybackOwner>,
        placement: rufin_core::playback::QueuePlacement,
        context_id: String,
        shuffled: bool,
    ) {
        self.model
            .play_source(queue, placement, context_id, shuffled);
    }

    /// Where a reread starts so the rows on screen stay.
    pub fn live_window(&self) -> library::RouteSeedWindow {
        self.model.live_window()
    }

    pub fn replace_favorites(&self, count: usize, rows: Vec<library::Row>, refresh: bool) {
        self.model.replace_favorites(count, rows, refresh);
    }

    pub fn resume_initial_demand(&self) {
        self.model.resume_initial_demand();
    }

    pub fn apply_library_list_settings(
        &self,
        key: LibraryListKey,
        settings: &crate::LibraryListSettings,
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
}

impl TrackListProjection {
    /// The page's rereads, for this list's search and the inputs `scope` adds. A search reads
    /// at once when it clears and after a pause while typing. Rows read while the route is not
    /// current are dropped.
    pub fn page_reread(
        &self,
        shell: &Rc<CatalogUi>,
        context: &'static str,
        view: &RouteView,
        scope: impl Fn(&mut OpenOptions) + 'static,
        changed: impl Fn(Option<library::Header>) + 'static,
    ) -> Rc<PageReread> {
        let inputs = list_inputs(&self.search);
        let model = self.model.clone();
        let current = Rc::downgrade(shell);
        let reread = PageReread::new(
            shell.runtime.clone(),
            context,
            view,
            move |options| {
                let mut options = inputs(options);
                scope(&mut options);
                options
            },
            move |mut read, previous| {
                let header = read.header.take();
                let replaced = current.upgrade().is_some_and(|shell| (shell.is_current)())
                    && model.replace_page(previous, read);
                if replaced {
                    changed(header);
                }
                replaced
            },
        );
        let search_reread = Rc::downgrade(&reread);
        let pending = Rc::new(RefCell::new(None::<glib::SourceId>));
        self.search.connect_search_changed(move |search| {
            if let Some(source) = pending.borrow_mut().take() {
                source.remove();
            }
            if search.text().trim().is_empty() {
                if let Some(reread) = search_reread.upgrade() {
                    reread.reread();
                }
                return;
            }
            let search_reread = search_reread.clone();
            let pending_timeout = Rc::clone(&pending);
            pending.replace(Some(glib::timeout_add_local_once(
                TRACK_FILTER_DEBOUNCE,
                move || {
                    pending_timeout.borrow_mut().take();
                    if let Some(reread) = search_reread.upgrade() {
                        reread.reread();
                    }
                },
            )));
        });
        reread
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
pub(crate) fn connect_list_search(search: &gtk::SearchEntry, reread: &Rc<PageReread>) {
    let reread = Rc::downgrade(reread);
    search.connect_search_changed(move |_| {
        if let Some(reread) = reread.upgrade() {
            reread.reread();
        }
    });
}

impl CatalogUi {
    pub fn opened_collection_view(self: &Rc<Self>, read: Opened) -> MountedRoute {
        let route = read.view.route().clone();
        let category = read.view.options().category;
        match route {
            Route::History => self.history_route(read),
            Route::Favorites if category == CollectionCategory::Tracks => {
                self.favorites_route(read)
            }
            Route::Tracks => self.library_tracks_route(read),
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
            Route::SmartPlaylistDetail(key) => self.smart_playlist_detail_route(key, read),
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
            Route::Home => self.home_route(read.view.home_sections().unwrap_or_default()),
            Route::Search => self.search_route(),
        }
    }

    pub fn scrolling_track_projection(
        self: &Rc<Self>,
        model: TrackCollectionModel,
        key: LibraryListKey,
        context: &str,
        context_id: String,
    ) -> (gtk::Widget, TrackListProjection, LibraryToolbarProjection) {
        let settings = self.settings.current.borrow().library_list(key);
        let projection = self.build_track_collection(
            model,
            key,
            settings,
            SearchableTrackOptions {
                context_id,
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
        read: Opened,
        favorites_only: bool,
    ) -> MountedRoute {
        let key = LibraryListKey::Albums;
        let view = route_view(&read);
        let models = AlbumCollectionModels::new(&self.runtime, &view, read);
        let models = Rc::new(RefCell::new(models));

        let search = gtk::SearchEntry::new();
        gtk_widgets::controls::configure_search_entry(&search);
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
        let apply = {
            let shell = Rc::downgrade(self);
            let models = models.clone();
            let content = content.clone();
            let page = page.clone();
            let view = Rc::clone(&view);
            move |read: Opened, previous: &View| {
                let Some(shell) = shell.upgrade() else {
                    return false;
                };
                if !(shell.is_current)() {
                    return false;
                }
                let settings = shell.settings.current.borrow().library_list(key);
                let current_models = models.borrow().clone();
                let replace_model = current_models.is_detail() != read.view.album_spans().is_some();
                if replace_model {
                    models.replace(AlbumCollectionModels::new(&shell.runtime, &view, read));
                } else if !current_models.replace_prepared(previous, read) {
                    tracing::warn!("rejected a mismatched prepared Albums page");
                    return false;
                }
                content.apply_settings(&settings);
                page.apply_library_list_settings(key, &settings);
                let models = models.borrow().clone();
                page.set_empty(models.is_empty());
                if replace_model {
                    models.resume_initial_demand();
                }
                true
            }
        };
        let reread = PageReread::new(
            self.runtime.clone(),
            "mounted Albums route",
            &view,
            list_inputs(&search),
            apply,
        );
        connect_list_search(&search, &reread);
        let resume = {
            let shell = Rc::downgrade(self);
            let content = content.clone();
            let models = models.clone();
            let page = page.clone();
            let reread = Rc::clone(&reread);
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else { return };
                let settings = shell.settings.current.borrow().library_list(key);
                if models.borrow().is_detail() == (settings.layout == LibraryLayout::Detail) {
                    content.apply_settings(&settings);
                }
                page.apply_library_list_settings(key, &settings);
                reread.resume(&shell.settings.current.borrow());
            })
        };
        let download_models = models.clone();
        let downloads = crate::collection_download_change(move |target, downloaded| {
            let CollectionTarget::Album(uri) = target else {
                return;
            };
            let rows = download_models.borrow().album_rows();
            if let Some(rows) = rows {
                rows.update_matching(
                    |row| {
                        row.media_uri == *uri
                            && (row.summary.downloaded_count == row.summary.track_count)
                                != downloaded
                    },
                    |row| {
                        row.summary.downloaded_count = if downloaded {
                            row.summary.track_count
                        } else {
                            0
                        }
                    },
                );
            }
        });
        if favorites_only {
            self.install_favorite_category_tabs(CollectionCategory::Albums, &mut page);
        }
        let refresh_models = models.clone();
        page.mounted_route(resume)
            .with_catalog_refresh(Rc::new(move || {
                reread.refresh(refresh_models.borrow().live_window())
            }))
            .with_download_change(downloads)
            .with_item_navigation(content.item_navigation())
            .with_item_scroll(content.item_scroll())
            .with_initial_demand({
                let models = models.clone();
                Rc::new(move || {
                    let models = models.borrow().clone();
                    models.resume_initial_demand();
                })
            })
            .with_states(&view)
    }

    pub fn library_tracks_route(self: &Rc<Self>, read: Opened) -> MountedRoute {
        self.root_track_route(
            RootTrackRouteOptions {
                key: LibraryListKey::Tracks,
                empty_body: msgid("Nothing here yet"),
            },
            read,
            false,
        )
    }

    pub fn favorites_route(self: &Rc<Self>, read: Opened) -> MountedRoute {
        let key = LibraryListKey::FavoriteTracks;
        self.root_track_route(
            RootTrackRouteOptions {
                key,
                empty_body: msgid("No favorites yet"),
            },
            read,
            true,
        )
    }

    fn root_track_route(
        self: &Rc<Self>,
        options: RootTrackRouteOptions,
        read: Opened,
        favorites_only: bool,
    ) -> MountedRoute {
        let context_id = rufin_core::view::context_base(read.view.route());
        let view = route_view(&read);
        let projection = self.searchable_track_collection(
            &view,
            read,
            options.key,
            SearchableTrackOptions {
                context_id,
                content_inset: 0,
                fixed_layout: None,
                search: None,
            },
        );
        let key = options.key;
        let mut page = self.track_page_shell(key, options.empty_body, &projection);
        let apply_page = page.clone();
        let apply_projection = projection.clone();
        let reread = projection.page_reread(
            self,
            "mounted Track route",
            &view,
            |_| {},
            move |_| apply_page.set_empty(apply_projection.source_is_empty()),
        );
        if favorites_only {
            self.install_favorite_category_tabs(CollectionCategory::Tracks, &mut page);
        }
        self.mount_track_page(key, projection, page, reread)
    }

    pub fn history_route(self: &Rc<Self>, read: Opened) -> MountedRoute {
        let key = LibraryListKey::History;
        let view = route_view(&read);
        let context_id = rufin_core::view::context_base(read.view.route());
        let projection = self.searchable_track_collection(
            &view,
            read,
            key,
            SearchableTrackOptions {
                context_id,
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
        let reread = projection.page_reread(
            self,
            "History filter",
            &view,
            move |options| options.history_source.clone_from(&scope.borrow()),
            move |_| apply_page.set_empty(apply_projection.source_is_empty()),
        );
        let filter_reread = Rc::downgrade(&reread);
        let filter_shell = Rc::downgrade(self);
        page.configure_history_filter(self.history_source_name.clone(), move |current_only| {
            source.replace(
                current_only
                    .then(|| {
                        filter_shell
                            .upgrade()
                            .and_then(|shell| (shell.history_source_id)())
                    })
                    .flatten(),
            );
            if let Some(reread) = filter_reread.upgrade() {
                reread.reread();
            }
        });
        self.mount_track_page(key, projection, page, reread)
    }

    pub fn library_artist_list_route(self: &Rc<Self>, read: Opened, route: Route) -> MountedRoute {
        let (key, empty_body) = match route {
            Route::Favorites => (LibraryListKey::Artists, msgid("No favorite artists yet")),
            Route::AlbumArtists => (LibraryListKey::AlbumArtists, msgid("Nothing here yet")),
            _ => (LibraryListKey::Artists, msgid("Nothing here yet")),
        };
        self.collection_list_route(
            key,
            empty_body,
            route == Route::Favorites,
            read,
            |row: &library::ArtistRow| row.artist_key,
        )
    }

    pub fn library_playlists_route(self: &Rc<Self>, read: Opened) -> MountedRoute {
        self.collection_list_route(
            LibraryListKey::Playlists,
            msgid("No playlists yet"),
            false,
            read,
            |row: &library::PlaylistListRow| row.key(),
        )
    }

    pub fn library_smart_playlists_route(self: &Rc<Self>, read: Opened) -> MountedRoute {
        self.collection_list_route(
            LibraryListKey::SmartPlaylists,
            msgid("No smart playlists yet"),
            false,
            read,
            |row: &library::PlaylistListRow| row.key(),
        )
    }

    pub fn searchable_track_collection(
        self: &Rc<Self>,
        view: &RouteView,
        read: Opened,
        key: LibraryListKey,
        options: SearchableTrackOptions,
    ) -> TrackListProjection {
        let model = TrackCollectionModel::new(self.runtime.clone(), view, read);
        self.track_projection(model, key, options)
    }

    pub fn track_projection(
        self: &Rc<Self>,
        model: TrackCollectionModel,
        key: LibraryListKey,
        options: SearchableTrackOptions,
    ) -> TrackListProjection {
        let mut settings = self.settings.current.borrow().library_list(key);
        if let Some(layout) = options.fixed_layout {
            settings.layout = layout;
        }
        self.build_track_collection(model, key, settings, options)
    }

    fn build_track_collection(
        self: &Rc<Self>,
        model: TrackCollectionModel,
        key: LibraryListKey,
        settings: LibraryListSettings,
        options: SearchableTrackOptions,
    ) -> TrackListProjection {
        let persistent_search = options.search.is_some();
        let search = options.search.unwrap_or_else(|| {
            let search = gtk::SearchEntry::new();
            gtk_widgets::controls::configure_search_entry(&search);
            search
        });
        bind_search_placeholder(&search, "Search");
        search.set_hexpand(true);
        if !persistent_search {
            let model = model.clone();
            search.connect_search_changed(move |entry| {
                model.set_query(entry.text().as_str());
            });
        }
        let collection = track_collection_projection(
            self,
            model.clone(),
            key,
            settings,
            options.context_id,
            options.content_inset,
        );
        TrackListProjection {
            key,
            search,
            collection,
            model,
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
                let model = projection.model.clone();
                Rc::new(move || model.visible_count() != 0)
            },
            content: projection.scrolling_widget(),
        })
    }

    fn mount_track_page(
        self: &Rc<Self>,
        key: LibraryListKey,
        projection: TrackListProjection,
        page: super::route_shell::LibraryPageShell,
        reread: Rc<PageReread>,
    ) -> MountedRoute {
        let resume = {
            let shell = Rc::downgrade(self);
            let projection = projection.clone();
            let page = page.clone();
            let reread = Rc::clone(&reread);
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else { return };
                let settings = shell.settings.current.borrow().library_list(key);
                projection.apply_library_list_settings(key, &settings);
                page.apply_library_list_settings(key, &settings);
                reread.resume(&shell.settings.current.borrow());
            })
        };
        let states = Rc::clone(reread.route_view());
        let refresh_projection = projection.clone();
        page.mounted_route(resume)
            .with_catalog_refresh(Rc::new(move || {
                reread.refresh(refresh_projection.live_window())
            }))
            .with_download_change(projection.download_change())
            .with_item_navigation(projection.item_navigation())
            .with_initial_demand({
                let projection = projection.clone();
                Rc::new(move || projection.resume_initial_demand())
            })
            .with_states(&states)
    }
}
