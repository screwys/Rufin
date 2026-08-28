use std::{cell::RefCell, rc::Rc, sync::Arc};

use adw::prelude::*;
use tracing::warn;

use crate::localization::bind_search_placeholder;
use crate::shell::Shell;
use crate::shell::route::{LatestMountedRouteRead, MountedRoute};
use crate::{LibraryLayout, LibraryListKey, LibraryListSettings};
use localization::msgid;

use super::album_detail::{AlbumCollectionModels, AlbumCollectionOrder};
use super::collections::{
    album_collection_projection, artist_collection_projection, library_route_inset,
    track_collection_projection,
};
use super::named_collections::{NamedOrderLoad, NamedReadRequest};
use super::route::{CollectionCategory, Route};
use super::route_layout::PRIMARY_ROUTE_HORIZONTAL_INSET;
use super::route_shell::{LibraryPageShellOptions, LibraryToolbarProjection};
use super::track_model::{PreparedTrackProjection, TrackCollectionModel, TrackProjectionRequest};

struct RootTrackRouteOptions {
    key: LibraryListKey,
    route: Route,
    context: &'static str,
    empty_body: &'static str,
    history: bool,
}

#[derive(Clone)]
struct TrackRouteReadRequest {
    tracks: TrackProjectionRequest,
}

#[derive(Clone)]
struct CollectionReadRequest {
    query: String,
    settings: LibraryListSettings,
}

pub(crate) struct SearchableTrackOptions {
    pub(crate) on_visible_count_changed: Option<Rc<dyn Fn(usize)>>,
    pub(crate) context_id: String,
    pub(crate) content_inset: i32,
    pub(crate) fixed_layout: Option<LibraryLayout>,
    pub(crate) search: Option<gtk::SearchEntry>,
}

#[derive(Clone)]
pub(crate) struct TrackListProjection {
    key: LibraryListKey,
    search: gtk::SearchEntry,
    collection: super::collections::LibraryCollectionProjection,
    model: TrackCollectionModel,
    on_visible_count_changed: Option<Rc<dyn Fn(usize)>>,
    fixed_layout: Option<LibraryLayout>,
}

impl TrackListProjection {
    pub(crate) fn search(&self) -> gtk::SearchEntry {
        self.search.clone()
    }

    pub(crate) fn scrolling_widget(&self) -> gtk::Widget {
        self.collection.scrolling_widget()
    }

    pub(crate) fn item_navigation(&self) -> crate::shell::route::MountedRouteItemNavigation {
        self.collection.item_navigation()
    }

    pub(crate) fn mount_in_scroller(&self, scroller: &gtk::ScrolledWindow) -> gtk::Widget {
        self.collection.mount_in_scroller(scroller, 0, 0)
    }

    pub(crate) fn source_is_empty(&self) -> bool {
        self.model.source_is_empty()
    }

    pub(crate) fn source_count(&self) -> usize {
        self.model.visible_count()
    }

    pub(crate) fn play_source(
        &self,
        queue: playback::QueueHandle,
        placement: playback::QueuePlacement,
        context_id: String,
        shuffled_start: bool,
    ) {
        self.model
            .play_source(queue, placement, context_id, shuffled_start);
    }

    pub(crate) fn projection_request(&self) -> TrackProjectionRequest {
        self.model.projection_request()
    }

    pub(crate) fn connect_search_request(
        &self,
        callback: impl Fn(TrackProjectionRequest) + 'static,
    ) {
        let model = self.model.clone();
        self.search
            .connect_search_changed(move |_| callback(model.projection_request()));
    }

    pub(crate) fn replace_prepared(&self, prepared: PreparedTrackProjection) -> bool {
        let changed = self.model.replace_prepared(prepared);
        if changed {
            self.notify_visible_count();
        }
        changed
    }

    pub(crate) fn apply_library_list_settings(
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
        let previous = self.model.settings();
        self.model.apply_settings(settings.clone());
        if previous.sort_key != settings.sort_key || previous.descending != settings.descending {
            self.notify_visible_count();
        }
        self.collection.apply_settings(&settings);
    }

    fn notify_visible_count(&self) {
        if let Some(on_visible_count_changed) = self.on_visible_count_changed.as_ref() {
            on_visible_count_changed(self.model.visible_count());
        }
    }
}

impl Shell {
    pub(crate) fn scrolling_track_projection(
        self: &Rc<Self>,
        selected: &crate::runtime::SelectedLibrary,
        order: Vec<library::TrackKey>,
        first_rows: Vec<library::TrackRow>,
        key: LibraryListKey,
        context: &str,
        context_id: String,
    ) -> (gtk::Widget, TrackListProjection, LibraryToolbarProjection) {
        let projection = self.searchable_track_collection(
            selected,
            order,
            first_rows,
            key,
            SearchableTrackOptions {
                on_visible_count_changed: None,
                context_id,
                content_inset: PRIMARY_ROUTE_HORIZONTAL_INSET,
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
        self.set_route_search(Some(projection.search()));
        wrapper.append(&projection.scrolling_widget());
        (wrapper.upcast(), projection, toolbar)
    }

    pub(crate) fn library_albums_route(
        self: &Rc<Self>,
        route: Route,
        favorites_only: bool,
        order: AlbumCollectionOrder,
        first_rows: Vec<library::AlbumRow>,
        selected: crate::runtime::SelectedLibrary,
    ) -> MountedRoute {
        let key = LibraryListKey::Albums;
        let settings = self.settings.current.borrow().library_list(key);
        let applied_settings = Rc::new(RefCell::new(settings.clone()));
        let models = AlbumCollectionModels::new(&selected, order, first_rows, settings.layout);

        let search = gtk::SearchEntry::new();
        bind_search_placeholder(&search, "Search");
        let query = Rc::new(RefCell::new(String::new()));
        let content = album_collection_projection(self, models.clone(), key);
        let page = self.library_page_shell(LibraryPageShellOptions {
            key,
            empty: models.is_empty(),
            empty_body: if favorites_only {
                msgid("No favorite albums yet")
            } else {
                msgid("Nothing here yet")
            },
            search: search.clone(),
            has_visible_results: {
                let models = models.clone();
                Rc::new(move || !models.is_empty())
            },
            content: content.scrolling_widget(),
        });
        let apply = {
            let shell = Rc::downgrade(self);
            let models = models.clone();
            let content = content.clone();
            let page = page.clone();
            let applied_settings = Rc::clone(&applied_settings);
            Rc::new(
                move |request: CollectionReadRequest,
                      result: Result<(AlbumCollectionOrder, Vec<library::AlbumRow>), String>| {
                    let Some(shell) = shell.upgrade() else {
                        return;
                    };
                    if shell.navigation.routes.borrow().current() != &route {
                        return;
                    }
                    match result {
                        Ok((order, first_rows)) => {
                            if !models.replace_prepared(order, first_rows) {
                                warn!("rejected a mismatched prepared Albums page");
                                return;
                            }
                            content.apply_settings(&request.settings);
                            page.apply_library_list_settings(key, &request.settings);
                            page.set_empty(models.is_empty());
                            applied_settings.replace(request.settings);
                        }
                        Err(error) => warn!(%error, "failed to refresh Albums order"),
                    }
                },
            )
        };
        let database = Arc::clone(&selected.database);
        let source = selected.source_key;
        let folder = selected.music_folder_key;
        let load = Arc::new(move |request: CollectionReadRequest| {
            let database = Arc::clone(&database);
            Box::pin(async move {
                let cancellation = library::ReadCancellation::new();
                if request.settings.layout == LibraryLayout::Detail {
                    database
                        .album_detail_route_order(
                            source,
                            folder,
                            favorites_only,
                            &request.query,
                            request.settings.sort_key.album_sort(),
                            request.settings.descending,
                            &cancellation,
                        )
                        .await
                        .map(|order| (AlbumCollectionOrder::Detail(order), Vec::new()))
                        .map_err(|error| error.to_string())
                } else {
                    let (order, first_rows) = database
                        .album_route_page(
                            source,
                            folder,
                            favorites_only,
                            &request.query,
                            request.settings.sort_key.album_sort(),
                            request.settings.descending,
                            &cancellation,
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                    Ok((AlbumCollectionOrder::Rows(order), first_rows))
                }
            }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
        });
        let read = LatestMountedRouteRead::new_with_request(
            selected.runtime.clone(),
            apply,
            load,
            "mounted Albums route",
        );
        {
            let read = Rc::downgrade(&read);
            let shell = Rc::downgrade(self);
            let query_state = Rc::clone(&query);
            search.connect_search_changed(move |search| {
                let (Some(read), Some(shell)) = (read.upgrade(), shell.upgrade()) else {
                    return;
                };
                let text = search.text().trim().to_string();
                query_state.replace(text.clone());
                let settings = shell.settings.current.borrow().library_list(key);
                read.request_with(CollectionReadRequest {
                    query: text,
                    settings,
                });
            });
        }
        let resume = {
            let shell = Rc::downgrade(self);
            let query = Rc::clone(&query);
            let content = content.clone();
            let page = page.clone();
            let applied_settings = Rc::clone(&applied_settings);
            let read = Rc::clone(&read);
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else {
                    return;
                };
                let settings = shell.settings.current.borrow().library_list(key);
                let previous = applied_settings.borrow().clone();
                if previous.layout != settings.layout {
                    shell.replace_current_route_when_ready();
                    return;
                }
                content.apply_settings(&settings);
                page.apply_library_list_settings(key, &settings);
                if previous.sort_key != settings.sort_key
                    || previous.descending != settings.descending
                {
                    read.request_with(CollectionReadRequest {
                        query: query.borrow().clone(),
                        settings: settings.clone(),
                    });
                }
                applied_settings.replace(settings);
            })
        };
        let favorite_models = models.clone();
        let favorite_read = Rc::clone(&read);
        let favorite_query = Rc::clone(&query);
        if favorites_only {
            self.install_favorite_category_tabs(CollectionCategory::Albums, &page);
        }
        MountedRoute::new(page.widget(), resume)
            .with_item_navigation(content.item_navigation())
            .with_favorite_settlement(Rc::new(move |settlement| {
                let library::FavoriteTarget::Album(album) = settlement.target else {
                    return;
                };
                favorite_models.update_favorite(album, settlement.effective);
                let settings = applied_settings.borrow().clone();
                if favorites_only || settings.sort_key == crate::LibraryField::Favorite {
                    favorite_read.request_with(CollectionReadRequest {
                        query: favorite_query.borrow().clone(),
                        settings,
                    });
                }
            }))
    }

    pub(crate) fn library_tracks_route(
        self: &Rc<Self>,
        order: Vec<library::TrackKey>,
        first_rows: Vec<library::TrackRow>,
        selected: crate::runtime::SelectedLibrary,
    ) -> MountedRoute {
        self.root_track_route(
            RootTrackRouteOptions {
                key: LibraryListKey::Tracks,
                route: Route::Tracks,
                context: "tracks",
                empty_body: msgid("Nothing here yet"),
                history: false,
            },
            order,
            first_rows,
            selected,
            false,
        )
    }

    pub(crate) fn favorites_route(
        self: &Rc<Self>,
        order: Vec<library::TrackKey>,
        first_rows: Vec<library::TrackRow>,
        selected: crate::runtime::SelectedLibrary,
    ) -> MountedRoute {
        let key = LibraryListKey::FavoriteTracks;
        self.root_track_route(
            RootTrackRouteOptions {
                key,
                route: Route::Favorites,
                context: "favorite-tracks",
                empty_body: msgid("No favorites yet"),
                history: false,
            },
            order,
            first_rows,
            selected,
            true,
        )
    }

    fn root_track_route(
        self: &Rc<Self>,
        options: RootTrackRouteOptions,
        order: Vec<library::TrackKey>,
        first_rows: Vec<library::TrackRow>,
        selected: crate::runtime::SelectedLibrary,
        favorites_only: bool,
    ) -> MountedRoute {
        let context_id = selected.music_folder_key.as_ref().map_or_else(
            || format!("{}:all", options.context),
            |folder_key| format!("{}:{folder_key}", options.context),
        );
        let projection = self.searchable_track_collection(
            &selected,
            order,
            first_rows,
            options.key,
            SearchableTrackOptions {
                on_visible_count_changed: None,
                context_id,
                content_inset: 0,
                fixed_layout: None,
                search: None,
            },
        );
        self.track_page_route(options, projection, selected, favorites_only)
    }

    pub(crate) fn history_route(
        self: &Rc<Self>,
        order: Vec<library::TrackKey>,
        first_rows: Vec<library::TrackRow>,
        selected: crate::runtime::SelectedLibrary,
    ) -> MountedRoute {
        self.root_track_route(
            RootTrackRouteOptions {
                key: LibraryListKey::History,
                route: Route::History,
                context: "history",
                empty_body: msgid("Nothing played yet"),
                history: true,
            },
            order,
            first_rows,
            selected,
            false,
        )
    }

    pub(crate) fn library_artist_list_route(
        self: &Rc<Self>,
        route: Route,
        album_artist: bool,
        favorites_only: bool,
        order: Vec<library::ArtistKey>,
        first_rows: Vec<library::ArtistRow>,
        selected: crate::runtime::SelectedLibrary,
    ) -> MountedRoute {
        let key = if album_artist {
            LibraryListKey::AlbumArtists
        } else {
            LibraryListKey::Artists
        };
        let database = Arc::clone(&selected.database);
        let source = selected.source_key;
        let folder = selected.music_folder_key;
        let row_database = Arc::clone(&database);
        let row_load = Arc::new(
            move |keys: Vec<library::ArtistKey>, cancellation: library::ReadCancellation| {
                let database = Arc::clone(&row_database);
                Box::pin(async move {
                    database
                        .artist_rows(source, &keys, album_artist, folder, &cancellation)
                        .await
                        .map_err(|error| error.to_string())
                }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
            },
        );
        let sparse = super::sparse_model::SparseRouteModel::new(
            order,
            32,
            selected.runtime.clone(),
            row_load,
        );
        sparse.seed_matching(first_rows, |row| row.artist_key);
        let content = artist_collection_projection(self, Rc::clone(&sparse), key);
        let search = gtk::SearchEntry::new();
        bind_search_placeholder(&search, "Search");
        let query = Rc::new(RefCell::new(String::new()));
        let page = self.library_page_shell(LibraryPageShellOptions {
            key,
            empty: sparse.len() == 0,
            empty_body: if favorites_only {
                msgid("No favorite artists yet")
            } else {
                msgid("Nothing here yet")
            },
            search: search.clone(),
            has_visible_results: {
                let sparse = Rc::clone(&sparse);
                Rc::new(move || sparse.len() != 0)
            },
            content: content.scrolling_widget(),
        });
        let apply = {
            let sparse = Rc::clone(&sparse);
            let content = content.clone();
            let page = page.clone();
            let shell = Rc::downgrade(self);
            Rc::new(
                move |request: CollectionReadRequest,
                      result: Result<
                    (Vec<library::ArtistKey>, Vec<library::ArtistRow>),
                    String,
                >| {
                    let Some(shell) = shell.upgrade() else {
                        return;
                    };
                    if shell.navigation.routes.borrow().current() != &route {
                        return;
                    }
                    match result {
                        Ok((order, first_rows)) => {
                            if !sparse.replace_prepared(order, first_rows, |row| row.artist_key) {
                                warn!("rejected a mismatched prepared Artist page");
                                return;
                            }
                            content.apply_settings(&request.settings);
                            page.apply_library_list_settings(key, &request.settings);
                            page.set_empty(sparse.len() == 0);
                        }
                        Err(error) => warn!(%error, "failed to refresh Artist order"),
                    }
                },
            )
        };
        let order_database = Arc::clone(&database);
        let load = Arc::new(move |request: CollectionReadRequest| {
            let database = Arc::clone(&order_database);
            Box::pin(async move {
                let cancellation = library::ReadCancellation::new();
                database
                    .artist_route_page(
                        source,
                        folder,
                        album_artist,
                        favorites_only,
                        &request.query,
                        request.settings.sort_key.artist_sort(),
                        request.settings.descending,
                        &cancellation,
                    )
                    .await
                    .map_err(|error| error.to_string())
            }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
        });
        let read = LatestMountedRouteRead::new_with_request(
            selected.runtime.clone(),
            apply,
            load,
            "mounted Artist route",
        );
        {
            let read = Rc::downgrade(&read);
            let shell = Rc::downgrade(self);
            let query_state = Rc::clone(&query);
            search.connect_search_changed(move |search| {
                let (Some(read), Some(shell)) = (read.upgrade(), shell.upgrade()) else {
                    return;
                };
                let text = search.text().trim().to_string();
                query_state.replace(text.clone());
                read.request_with(CollectionReadRequest {
                    query: text,
                    settings: shell.settings.current.borrow().library_list(key),
                });
            });
        }
        let resume = {
            let shell = Rc::downgrade(self);
            let read = Rc::clone(&read);
            let query = Rc::clone(&query);
            let content = content.clone();
            let page = page.clone();
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else {
                    return;
                };
                let settings = shell.settings.current.borrow().library_list(key);
                content.apply_settings(&settings);
                page.apply_library_list_settings(key, &settings);
                read.request_with(CollectionReadRequest {
                    query: query.borrow().clone(),
                    settings,
                });
            })
        };
        let favorite_sparse = Rc::clone(&sparse);
        let favorite_read = Rc::clone(&read);
        let favorite_query = Rc::clone(&query);
        let favorite_shell = Rc::downgrade(self);
        if favorites_only {
            self.install_favorite_category_tabs(CollectionCategory::Artists, &page);
        }
        MountedRoute::new(page.widget(), resume)
            .with_item_navigation(content.item_navigation())
            .with_favorite_settlement(Rc::new(move |settlement| {
                let Some(favorite_shell) = favorite_shell.upgrade() else {
                    return;
                };
                let library::FavoriteTarget::Artist(artist) = settlement.target else {
                    return;
                };
                favorite_sparse.update_ready(&artist, |row: &mut library::ArtistRow| {
                    row.favorite = settlement.effective;
                });
                let settings = favorite_shell.settings.current.borrow().library_list(key);
                if favorites_only || settings.sort_key == crate::LibraryField::Favorite {
                    favorite_read.request_with(CollectionReadRequest {
                        query: favorite_query.borrow().clone(),
                        settings,
                    });
                }
            }))
    }

    pub(crate) fn library_playlists_route(
        self: &Rc<Self>,
        order: Vec<library::PlaylistKey>,
        first_rows: Vec<library::PlaylistRow>,
        selected: crate::runtime::SelectedLibrary,
    ) -> MountedRoute {
        let key = LibraryListKey::Playlists;
        let database = Arc::clone(&selected.database);
        let source = selected.source_key;
        let folder = selected.music_folder_key;
        let row_database = Arc::clone(&database);
        let row_load = Arc::new(
            move |keys: Vec<library::PlaylistKey>, cancellation: library::ReadCancellation| {
                let database = Arc::clone(&row_database);
                Box::pin(async move {
                    database
                        .playlist_rows(source, &keys, folder, &cancellation)
                        .await
                        .map_err(|error| error.to_string())
                }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
            },
        );
        let order_database = Arc::clone(&database);
        let order_load: NamedOrderLoad<library::PlaylistKey, library::PlaylistRow> =
            Arc::new(move |request: NamedReadRequest| {
                let database = Arc::clone(&order_database);
                Box::pin(async move {
                    let cancellation = library::ReadCancellation::new();
                    database
                        .playlist_route_page(
                            source,
                            folder,
                            request.settings.sort_key.playlist_sort(),
                            request.settings.descending,
                            &request.query,
                            &cancellation,
                        )
                        .await
                        .map_err(|error| error.to_string())
                })
            });
        self.named_collection_route(
            Route::Playlists,
            key,
            msgid("Nothing here yet"),
            order,
            first_rows,
            |row: &library::PlaylistRow| row.playlist_key,
            selected,
            row_load,
            order_load,
        )
    }

    pub(crate) fn library_smart_playlists_route(
        self: &Rc<Self>,
        order: Vec<library::SmartPlaylistKey>,
        first_rows: Vec<library::SmartPlaylistRow>,
        selected: crate::runtime::SelectedLibrary,
    ) -> MountedRoute {
        let key = LibraryListKey::SmartPlaylists;
        let database = Arc::clone(&selected.database);
        let source = selected.source_key;
        let folder = selected.music_folder_key;
        let row_database = Arc::clone(&database);
        let row_load = Arc::new(
            move |keys: Vec<library::SmartPlaylistKey>, cancellation: library::ReadCancellation| {
                let database = Arc::clone(&row_database);
                Box::pin(async move {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |duration| duration.as_secs().min(i64::MAX as u64) as i64);
                    database
                        .smart_playlist_rows(source, &keys, folder, now, &cancellation)
                        .await
                        .map_err(|error| error.to_string())
                }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
            },
        );
        let order_database = Arc::clone(&database);
        let order_load: NamedOrderLoad<library::SmartPlaylistKey, library::SmartPlaylistRow> =
            Arc::new(move |request: NamedReadRequest| {
                let database = Arc::clone(&order_database);
                Box::pin(async move {
                    let cancellation = library::ReadCancellation::new();
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |duration| duration.as_secs().min(i64::MAX as u64) as i64);
                    database
                        .smart_playlist_route_page(
                            source,
                            folder,
                            request.settings.sort_key.smart_playlist_sort(),
                            request.settings.descending,
                            now,
                            &cancellation,
                        )
                        .await
                        .map_err(|error| error.to_string())
                })
            });
        self.named_collection_route(
            Route::SmartPlaylists,
            key,
            msgid("No smart playlists yet"),
            order,
            first_rows,
            |row: &library::SmartPlaylistRow| row.smart_playlist_key,
            selected,
            row_load,
            order_load,
        )
    }

    pub(crate) fn searchable_track_collection(
        self: &Rc<Self>,
        selected: &crate::runtime::SelectedLibrary,
        order: Vec<library::TrackKey>,
        first_rows: Vec<library::TrackRow>,
        key: LibraryListKey,
        options: SearchableTrackOptions,
    ) -> TrackListProjection {
        let mut settings = self.settings.current.borrow().library_list(key);
        if let Some(layout) = options.fixed_layout {
            settings.layout = layout;
        }
        let model = TrackCollectionModel::new(
            selected.source_key,
            selected.source_session_epoch,
            Arc::clone(&selected.database),
            selected.runtime.clone(),
            order,
            first_rows,
            settings.clone(),
        );
        if let Some(on_visible_count_changed) = options.on_visible_count_changed.as_ref() {
            on_visible_count_changed(model.visible_count());
        }
        let persistent_search = options.search.is_some();
        let search = options.search.unwrap_or_else(gtk::SearchEntry::new);
        bind_search_placeholder(&search, "Search");
        search.set_hexpand(true);
        if !persistent_search {
            let model = model.clone();
            let on_visible_count_changed = options.on_visible_count_changed.clone();
            search.connect_search_changed(move |entry| {
                if model.set_query(entry.text().as_str())
                    && let Some(on_visible_count_changed) = on_visible_count_changed.as_ref()
                {
                    on_visible_count_changed(model.visible_count());
                }
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
            on_visible_count_changed: options.on_visible_count_changed,
            fixed_layout: options.fixed_layout,
        }
    }

    fn track_page_route(
        self: &Rc<Self>,
        options: RootTrackRouteOptions,
        projection: TrackListProjection,
        selected: crate::runtime::SelectedLibrary,
        favorites_only: bool,
    ) -> MountedRoute {
        let key = options.key;
        let page = self.library_page_shell(LibraryPageShellOptions {
            key,
            empty: projection.source_is_empty(),
            empty_body: options.empty_body,
            search: projection.search(),
            has_visible_results: {
                let model = projection.model.clone();
                Rc::new(move || model.visible_count() != 0)
            },
            content: projection.scrolling_widget(),
        });
        let apply = {
            let shell = Rc::downgrade(self);
            let projection = projection.clone();
            let page = page.clone();
            let route = options.route.clone();
            let source_key = selected.source_key;
            let epoch = selected.source_session_epoch;
            Rc::new(move |_request: TrackRouteReadRequest, result| {
                let Some(shell) = shell.upgrade() else {
                    return;
                };
                if shell.navigation.routes.borrow().current() != &route
                    || !shell.selected_library().as_deref().is_some_and(|selected| {
                        selected.source_key == source_key && selected.source_session_epoch == epoch
                    })
                {
                    return;
                }
                let prepared = match result {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        warn!(%error, "failed to read a mounted Track route");
                        return;
                    }
                };
                if projection.replace_prepared(prepared) {
                    page.set_empty(projection.source_is_empty());
                }
            })
        };
        let database = Arc::clone(&selected.database);
        let source_key = selected.source_key;
        let folder = selected.music_folder_key;
        let load = Arc::new(move |request: TrackRouteReadRequest| {
            let database = Arc::clone(&database);
            Box::pin(async move {
                let cancellation = library::ReadCancellation::new();
                let page = if options.history {
                    database
                        .history_track_page(
                            source_key,
                            folder,
                            &request.tracks.query,
                            &cancellation,
                        )
                        .await
                } else {
                    database
                        .track_route_page(
                            source_key,
                            folder,
                            favorites_only,
                            &request.tracks.query,
                            request.tracks.settings.sort_key.track_sort(),
                            request.tracks.settings.descending,
                            &cancellation,
                        )
                        .await
                }
                .map_err(|error| error.to_string())?;
                Ok::<PreparedTrackProjection, String>(PreparedTrackProjection {
                    order: page.order,
                    first_rows: page.first_rows,
                    request: request.tracks,
                })
            }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
        });
        let read = LatestMountedRouteRead::new_with_request(
            selected.runtime.clone(),
            apply,
            load,
            "mounted Track route",
        );
        {
            let read = Rc::downgrade(&read);
            projection.connect_search_request(move |tracks| {
                let Some(read) = read.upgrade() else {
                    return;
                };
                read.request_with(TrackRouteReadRequest { tracks });
            });
        }
        let resume = {
            let shell = Rc::downgrade(self);
            let projection = projection.clone();
            let page = page.clone();
            let read = Rc::clone(&read);
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else {
                    return;
                };
                let settings = shell.settings.current.borrow().library_list(key);
                projection.apply_library_list_settings(key, &settings);
                page.apply_library_list_settings(key, &settings);
                read.request_with(TrackRouteReadRequest {
                    tracks: projection.projection_request(),
                });
            })
        };
        let favorite_projection = projection.clone();
        let favorite_read = Rc::clone(&read);
        if favorites_only {
            self.install_favorite_category_tabs(CollectionCategory::Tracks, &page);
        }
        MountedRoute::new(page.widget(), resume)
            .with_item_navigation(projection.item_navigation())
            .with_favorite_settlement(Rc::new(move |settlement| {
                let library::FavoriteTarget::Track(track) = settlement.target else {
                    return;
                };
                favorite_projection
                    .model
                    .update_favorite(track, settlement.effective);
                let request = favorite_projection.projection_request();
                if favorites_only || request.settings.sort_key == crate::LibraryField::Favorite {
                    favorite_read.request_with(TrackRouteReadRequest { tracks: request });
                }
            }))
    }
}
