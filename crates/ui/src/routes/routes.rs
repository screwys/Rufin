use std::{cell::RefCell, rc::Rc, sync::Arc, time::Duration};

use adw::prelude::*;
use gtk::glib;
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
use super::library_fields::TrackPresentation;
use super::named_collections::{NamedOrderLoad, NamedReadRequest};
use super::route::{CollectionCategory, Route};
use super::route_shell::{LibraryPageShellOptions, LibraryToolbarProjection};
use super::track_model::{PreparedTrackProjection, TrackCollectionModel, TrackProjectionRequest};

const TRACK_FILTER_DEBOUNCE: Duration = Duration::from_millis(150);

struct RootTrackRouteOptions {
    key: LibraryListKey,
    route: Route,
    context: &'static str,
    empty_body: &'static str,
}

#[derive(Clone)]
struct CollectionReadRequest {
    query: String,
    settings: LibraryListSettings,
}

pub(crate) struct SearchableTrackOptions {
    pub(crate) context_id: String,
    pub(crate) content_inset: i32,
    pub(crate) fixed_layout: Option<LibraryLayout>,
    pub(crate) search: Option<gtk::SearchEntry>,
}

#[derive(Clone)]
pub(crate) struct TrackListProjection<T: TrackPresentation = library::TrackRow> {
    key: LibraryListKey,
    search: gtk::SearchEntry,
    collection: super::collections::LibraryCollectionProjection,
    model: TrackCollectionModel<T>,
    fixed_layout: Option<LibraryLayout>,
}

impl<T: TrackPresentation> TrackListProjection<T> {
    pub(crate) fn set_queue_source(
        &self,
        query: library::QueueQuery,
        folder: Option<library::FolderKey>,
    ) {
        self.model.set_queue_source(query, folder);
    }
    pub(crate) fn download_change(&self) -> crate::shell::route::MountedDownloadChange {
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
    ) {
        self.model.play_source(queue, placement, context_id);
    }

    pub(crate) fn projection_request(&self) -> TrackProjectionRequest {
        self.model.projection_request()
    }

    pub(crate) fn connect_search_request(
        &self,
        callback: impl Fn(TrackProjectionRequest) + 'static,
    ) {
        let model = self.model.clone();
        let callback = Rc::new(callback);
        let pending = Rc::new(RefCell::new(None::<glib::SourceId>));
        self.search.connect_search_changed(move |_| {
            if let Some(source) = pending.borrow_mut().take() {
                source.remove();
            }
            let request = model.projection_request();
            if request.query.is_empty() {
                callback(request);
                return;
            }
            let callback = Rc::clone(&callback);
            let pending_timeout = Rc::clone(&pending);
            pending.replace(Some(glib::timeout_add_local_once(
                TRACK_FILTER_DEBOUNCE,
                move || {
                    pending_timeout.borrow_mut().take();
                    callback(request);
                },
            )));
        });
    }

    pub(crate) fn replace_prepared(&self, prepared: PreparedTrackProjection<T>) -> bool {
        self.model.replace_prepared(prepared)
    }

    pub(crate) fn resume_initial_demand(&self) {
        self.model.resume_initial_demand();
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
        self.model.apply_settings(settings.clone());
        self.collection.apply_settings(&settings);
    }
}

impl Shell {
    pub(crate) fn scrolling_track_projection<T: TrackPresentation>(
        self: &Rc<Self>,
        model: TrackCollectionModel<T>,
        key: LibraryListKey,
        context: &str,
        context_id: String,
    ) -> (
        gtk::Widget,
        TrackListProjection<T>,
        LibraryToolbarProjection,
    ) {
        let settings = model.settings();
        let projection = self.searchable_song_collection(
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

    pub(crate) fn library_albums_route(
        self: &Rc<Self>,
        route: Route,
        favorites_only: bool,
        order: AlbumCollectionOrder,
        first_row_position: usize,
        first_rows: Vec<library::AlbumRow>,
        first_detail_rows: Vec<super::album_detail::AlbumDetailRouteRow>,
        selected: crate::runtime::SelectedLibrary,
    ) -> MountedRoute {
        let key = LibraryListKey::Albums;
        let settings = self.settings.current.borrow().library_list(key);
        let applied_settings = Rc::new(RefCell::new(settings.clone()));
        let models = AlbumCollectionModels::new(
            &selected,
            order,
            first_row_position,
            first_rows,
            first_detail_rows,
            settings.layout,
        );

        let search = gtk::SearchEntry::new();
        bind_search_placeholder(&search, "Search");
        let query = Rc::new(RefCell::new(String::new()));
        let content = album_collection_projection(self, models.clone(), key);
        let mut page = self.library_page_shell(LibraryPageShellOptions {
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
                      result: Result<
                    (AlbumCollectionOrder, usize, Vec<library::AlbumRow>),
                    String,
                >| {
                    let Some(shell) = shell.upgrade() else {
                        return;
                    };
                    if shell.navigation.routes.borrow().current() != &route {
                        return;
                    }
                    match result {
                        Ok((order, first_row_position, first_rows)) => {
                            if !models.replace_prepared(order, first_row_position, first_rows) {
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
                        .map(|(media_uris, albums, albums_with_genres)| {
                            (
                                AlbumCollectionOrder::Detail {
                                    media_uris,
                                    albums,
                                    albums_with_genres,
                                },
                                0,
                                Vec::new(),
                            )
                        })
                        .map_err(|error| error.to_string())
                } else {
                    let (order, first_row_position, first_rows) = database
                        .album_route_page(
                            source,
                            folder,
                            favorites_only,
                            &request.query,
                            request.settings.sort_key.album_sort(),
                            request.settings.descending,
                            library::RouteSeedWindow::top(),
                            &cancellation,
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                    Ok((
                        AlbumCollectionOrder::Rows(order),
                        first_row_position,
                        first_rows,
                    ))
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

        let download_rows = models.rows.clone();
        let downloads = self.collection_download_change(move |identity, downloaded| {
            let Some(uri) = identity.strip_prefix("album:") else {
                return;
            };
            if let Some(rows) = &download_rows {
                rows.update_matching(
                    |row| {
                        row.media_uri == uri
                            && (row.downloaded_count == row.track_count) != downloaded
                    },
                    |row| row.downloaded_count = if downloaded { row.track_count } else { 0 },
                );
            }
        });
        let favorite_read = Rc::clone(&read);
        let favorite_query = Rc::clone(&query);
        if favorites_only {
            self.install_favorite_category_tabs(CollectionCategory::Albums, &mut page);
        }
        page.mounted_route(resume)
            .with_download_change(downloads)
            .with_item_navigation(content.item_navigation())
            .with_initial_demand({
                let models = models.clone();
                Rc::new(move || models.resume_initial_demand())
            })
            .with_favorite_settlement(Rc::new(move |settlement| {
                let library::FavoriteTarget::Album(album) = settlement.target else {
                    return;
                };
                favorite_models.update_favorite(&album, settlement.effective);
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
        order: Vec<String>,
        first_row_position: usize,
        first_rows: Vec<library::TrackRow>,
        selected: crate::runtime::SelectedLibrary,
    ) -> MountedRoute {
        self.root_track_route(
            RootTrackRouteOptions {
                key: LibraryListKey::Tracks,
                route: Route::Tracks,
                context: "tracks",
                empty_body: msgid("Nothing here yet"),
            },
            order,
            first_row_position,
            first_rows,
            selected,
            false,
        )
    }

    pub(crate) fn favorites_route(
        self: &Rc<Self>,
        order: Vec<String>,
        first_row_position: usize,
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
            },
            order,
            first_row_position,
            first_rows,
            selected,
            true,
        )
    }

    fn root_track_route(
        self: &Rc<Self>,
        options: RootTrackRouteOptions,
        order: Vec<String>,
        first_row_position: usize,
        first_rows: Vec<library::TrackRow>,
        selected: crate::runtime::SelectedLibrary,
        favorites_only: bool,
    ) -> MountedRoute {
        let context_id = selected.music_folder_key.as_ref().map_or_else(
            || format!("{}:all", options.context),
            |folder_key| format!("{}:{folder_key}", options.context),
        );
        let projection = self.searchable_track_collection(
            order,
            first_row_position,
            first_rows,
            options.key,
            SearchableTrackOptions {
                context_id,
                content_inset: 0,
                fixed_layout: None,
                search: None,
            },
        );
        projection.set_queue_source(
            library::QueueQuery::Tracks {
                source: selected.source_key,
                favorites_only,
                recursive: true,
            },
            selected.music_folder_key,
        );
        self.track_page_route(options, projection, selected, favorites_only)
    }

    pub(crate) fn history_route(self: &Rc<Self>, rows: Vec<library::HistoryRow>) -> MountedRoute {
        let key = LibraryListKey::History;
        let database = Arc::clone(&self.products.library);
        let runtime = self.products.runtime.clone();
        let order = rows.iter().map(|row| row.media_uri.clone()).collect();
        let settings = self.settings.current.borrow().library_list(key);
        let rows_database = database.clone();
        let load = Arc::new(
            move |uris: Vec<String>, cancellation: library::ReadCancellation| {
                let database = rows_database.clone();
                Box::pin(async move {
                    database
                        .history_rows_by_uri(&uris, &cancellation)
                        .await
                        .map_err(|error| error.to_string())
                }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
            },
        );
        let model = TrackCollectionModel::with_load(
            runtime.clone(),
            order,
            0,
            rows,
            settings.clone(),
            load,
        );
        let projection = self.searchable_song_collection(
            model,
            key,
            settings,
            SearchableTrackOptions {
                context_id: "history:all".to_string(),
                content_inset: 0,
                fixed_layout: None,
                search: None,
            },
        );
        let page = self.track_page_shell(key, msgid("Nothing played yet"), &projection);
        let current = Rc::new(RefCell::new(None::<library::SourceId>));
        let apply_page = page.clone();
        let apply_projection = projection.clone();
        let apply = Rc::new(move |_, result| {
            let Ok(prepared) = result else { return };
            if apply_projection.replace_prepared(prepared) {
                apply_page.set_empty(apply_projection.source_is_empty());
            }
        });
        let load = Arc::new(
            move |request: (TrackProjectionRequest, Option<library::SourceId>)| {
                let database = Arc::clone(&database);
                Box::pin(async move {
                    let rows = database
                        .activity_history(
                            request.1.as_ref(),
                            &request.0.query,
                            &library::ReadCancellation::new(),
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                    Ok::<_, String>(PreparedTrackProjection {
                        order: rows.iter().map(|row| row.media_uri.clone()).collect(),
                        first_row_position: 0,
                        first_rows: rows,
                        request: request.0,
                    })
                }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
            },
        );
        let read = LatestMountedRouteRead::new_with_request(
            self.products.runtime.clone(),
            apply,
            load,
            "History filter",
        );
        let search_read = Rc::downgrade(&read);
        let search_current = Rc::clone(&current);
        projection.connect_search_request(move |request| {
            if let Some(read) = search_read.upgrade() {
                read.request_with((request, search_current.borrow().clone()));
            }
        });
        let filter_read = Rc::downgrade(&read);
        let filter_projection = projection.clone();
        let filter_current = Rc::clone(&current);
        let filter_shell = Rc::downgrade(self);
        let current_source_name = {
            let configured = self.source.configured.borrow();
            configured
                .sources
                .iter()
                .find(|source| Some(&source.id) == configured.selected_source_id.as_ref())
                .map(crate::preferences::source::configured_source_display_name)
        };
        page.configure_history_filter(current_source_name, move |current_only| {
            let source = current_only
                .then(|| {
                    filter_shell.upgrade().and_then(|shell| {
                        shell.source.configured.borrow().selected_source_id.clone()
                    })
                })
                .flatten();
            *filter_current.borrow_mut() = source.clone();
            let Some(read) = filter_read.upgrade() else {
                return;
            };
            read.request_with((filter_projection.projection_request(), source));
        });
        let request_current = Rc::clone(&current);
        self.mount_track_page(
            key,
            projection,
            page,
            read,
            Rc::new(move |request| (request, request_current.borrow().clone())),
            None,
        )
    }

    pub(crate) fn library_artist_list_route(
        self: &Rc<Self>,
        route: Route,
        album_artist: bool,
        favorites_only: bool,
        order: Vec<library::ArtistKey>,
        first_row_position: usize,
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
        sparse.seed_matching_at(first_row_position, first_rows, |row| row.artist_key);
        let content = artist_collection_projection(self, Rc::clone(&sparse), key);
        let search = gtk::SearchEntry::new();
        bind_search_placeholder(&search, "Search");
        let query = Rc::new(RefCell::new(String::new()));
        let mut page = self.library_page_shell(LibraryPageShellOptions {
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
                    (Vec<library::ArtistKey>, usize, Vec<library::ArtistRow>),
                    String,
                >| {
                    let Some(shell) = shell.upgrade() else {
                        return;
                    };
                    if shell.navigation.routes.borrow().current() != &route {
                        return;
                    }
                    match result {
                        Ok((order, first_row_position, first_rows)) => {
                            if !sparse.replace_prepared_at(
                                order,
                                first_row_position,
                                first_rows,
                                |row| row.artist_key,
                            ) {
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
                        library::RouteSeedWindow::top(),
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

        let download_rows = Rc::clone(&sparse);
        let downloads = self.collection_download_change(move |identity, downloaded| {
            let prefix = if album_artist {
                "album-artist:"
            } else {
                "artist:"
            };
            let Some(uri) = identity.strip_prefix(prefix) else {
                return;
            };
            download_rows.update_matching(
                |row| {
                    row.media_uri == uri && (row.downloaded_count == row.track_count) != downloaded
                },
                |row| row.downloaded_count = if downloaded { row.track_count } else { 0 },
            );
        });
        let favorite_read = Rc::clone(&read);
        let favorite_query = Rc::clone(&query);
        let favorite_shell = Rc::downgrade(self);
        if favorites_only {
            self.install_favorite_category_tabs(CollectionCategory::Artists, &mut page);
        }
        page.mounted_route(resume)
            .with_download_change(downloads)
            .with_item_navigation(content.item_navigation())
            .with_initial_demand({
                let sparse = Rc::clone(&sparse);
                Rc::new(move || sparse.resume_initial_demand())
            })
            .with_favorite_settlement(Rc::new(move |settlement| {
                let Some(favorite_shell) = favorite_shell.upgrade() else {
                    return;
                };
                let library::FavoriteTarget::Artist(artist) = settlement.target else {
                    return;
                };
                if let Some(position) =
                    favorite_sparse.ready_position(|row| row.media_uri == artist)
                    && let Some(key) = favorite_sparse.order().get(position as usize).copied()
                {
                    favorite_sparse.update_ready(&key, |row: &mut library::ArtistRow| {
                        row.favorite = settlement.effective;
                    });
                }
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
        first_row_position: usize,
        first_rows: Vec<library::PlaylistRow>,
        source: Option<library::SourceKey>,
        folder: Option<library::FolderKey>,
    ) -> MountedRoute {
        let key = LibraryListKey::Playlists;
        let database = Arc::clone(&self.products.library);
        let row_database = Arc::clone(&database);
        let row_load = Arc::new(
            move |keys: Vec<library::PlaylistKey>, cancellation: library::ReadCancellation| {
                let database = Arc::clone(&row_database);
                Box::pin(async move {
                    database
                        .playlist_rows(&keys, &cancellation)
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
                            library::RouteSeedWindow::top(),
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
            first_row_position,
            first_rows,
            |row: &library::PlaylistRow| row.playlist_key,
            row_load,
            order_load,
        )
    }

    pub(crate) fn library_smart_playlists_route(
        self: &Rc<Self>,
        order: Vec<library::SmartPlaylistKey>,
        first_row_position: usize,
        first_rows: Vec<library::SmartPlaylistRow>,
        source: Option<library::SourceKey>,
        folder: Option<library::FolderKey>,
    ) -> MountedRoute {
        let key = LibraryListKey::SmartPlaylists;
        let database = Arc::clone(&self.products.library);
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
                            library::RouteSeedWindow::top(),
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
            first_row_position,
            first_rows,
            |row: &library::SmartPlaylistRow| row.smart_playlist_key,
            row_load,
            order_load,
        )
    }

    pub(crate) fn searchable_track_collection(
        self: &Rc<Self>,
        order: Vec<String>,
        first_row_position: usize,
        first_rows: Vec<library::TrackRow>,
        key: LibraryListKey,
        options: SearchableTrackOptions,
    ) -> TrackListProjection {
        let mut settings = self.settings.current.borrow().library_list(key);
        if let Some(layout) = options.fixed_layout {
            settings.layout = layout;
        }
        let model = TrackCollectionModel::new(
            Arc::clone(&self.products.library),
            self.products.runtime.clone(),
            order,
            first_row_position,
            first_rows,
            settings.clone(),
        );
        self.searchable_song_collection(model, key, settings, options)
    }

    fn searchable_song_collection<T: TrackPresentation>(
        self: &Rc<Self>,
        model: TrackCollectionModel<T>,
        key: LibraryListKey,
        settings: LibraryListSettings,
        options: SearchableTrackOptions,
    ) -> TrackListProjection<T> {
        let persistent_search = options.search.is_some();
        let search = options.search.unwrap_or_else(gtk::SearchEntry::new);
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

    fn track_page_route(
        self: &Rc<Self>,
        options: RootTrackRouteOptions,
        projection: TrackListProjection,
        selected: crate::runtime::SelectedLibrary,
        favorites_only: bool,
    ) -> MountedRoute {
        let key = options.key;
        let mut page = self.track_page_shell(key, options.empty_body, &projection);
        let apply = {
            let shell = Rc::downgrade(self);
            let projection = projection.clone();
            let page = page.clone();
            let route = options.route.clone();
            let source_key = selected.source_key;
            Rc::new(move |_request: TrackProjectionRequest, result| {
                let Some(shell) = shell.upgrade() else {
                    return;
                };
                if shell.navigation.routes.borrow().current() != &route
                    || !shell
                        .selected_library()
                        .as_deref()
                        .is_some_and(|selected| selected.source_key == source_key)
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
        let load = Arc::new(move |request: TrackProjectionRequest| {
            let database = Arc::clone(&database);
            Box::pin(async move {
                let cancellation = library::ReadCancellation::new();
                let page = database
                    .track_route_page(
                        source_key,
                        folder,
                        favorites_only,
                        &request.query,
                        request.settings.sort_key.track_sort(),
                        request.settings.descending,
                        library::RouteSeedWindow::top(),
                        &cancellation,
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                Ok::<PreparedTrackProjection, String>(PreparedTrackProjection {
                    order: page.order,
                    first_row_position: page.first_row_position,
                    first_rows: page.first_rows,
                    request,
                })
            }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
        });
        let read = LatestMountedRouteRead::new_with_request(
            selected.runtime.clone(),
            apply,
            load,
            "mounted Track route",
        );
        if favorites_only {
            self.install_favorite_category_tabs(CollectionCategory::Tracks, &mut page);
        }
        self.mount_track_page(
            key,
            projection,
            page,
            read,
            Rc::new(|request| request),
            Some(favorites_only),
        )
    }

    fn track_page_shell<T: TrackPresentation>(
        self: &Rc<Self>,
        key: LibraryListKey,
        empty_body: &'static str,
        projection: &TrackListProjection<T>,
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

    fn mount_track_page<Q, T: TrackPresentation>(
        self: &Rc<Self>,
        key: LibraryListKey,
        projection: TrackListProjection<T>,
        page: super::route_shell::LibraryPageShell,
        read: Rc<LatestMountedRouteRead<Result<PreparedTrackProjection<T>, String>, Q>>,
        map_request: Rc<dyn Fn(TrackProjectionRequest) -> Q>,
        favorite_requery: Option<bool>,
    ) -> MountedRoute
    where
        Q: Clone + Send + 'static,
    {
        let search_read = Rc::downgrade(&read);
        let search_request = Rc::clone(&map_request);
        projection.connect_search_request(move |projection_request| {
            if let Some(read) = search_read.upgrade() {
                read.request_with(search_request(projection_request));
            }
        });
        let resume = {
            let shell = Rc::downgrade(self);
            let projection = projection.clone();
            let page = page.clone();
            let read = Rc::clone(&read);
            let map_request = Rc::clone(&map_request);
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else { return };
                let settings = shell.settings.current.borrow().library_list(key);
                let previous = projection.projection_request();
                projection.apply_library_list_settings(key, &settings);
                page.apply_library_list_settings(key, &settings);
                let request = projection.projection_request();
                if !previous.same_query(&request) {
                    read.request_with(map_request(request));
                }
            })
        };
        let refresh = {
            let projection = projection.clone();
            let read = Rc::clone(&read);
            let map_request = Rc::clone(&map_request);
            Rc::new(move || read.request_with(map_request(projection.projection_request())))
        };
        let favorite_projection = projection.clone();
        let favorite_read = read;
        page.mounted_route(resume)
            .with_catalog_refresh(refresh)
            .with_download_change(projection.download_change())
            .with_item_navigation(projection.item_navigation())
            .with_initial_demand({
                let projection = projection.clone();
                Rc::new(move || projection.resume_initial_demand())
            })
            .with_favorite_settlement(Rc::new(move |settlement| {
                let library::FavoriteTarget::Track(track) = settlement.target else {
                    return;
                };
                favorite_projection
                    .model
                    .update_favorite(&track, settlement.effective);
                let projection_request = favorite_projection.projection_request();
                if favorite_requery.is_some_and(|favorites_only| {
                    favorites_only
                        || projection_request.settings.sort_key == crate::LibraryField::Favorite
                }) {
                    favorite_read.request_with(map_request(projection_request));
                }
            }))
    }
}
