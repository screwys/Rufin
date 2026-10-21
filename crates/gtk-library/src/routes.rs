use rufin_core::browse::{MemberPage, OpenedCollection};
use std::{cell::RefCell, rc::Rc, sync::Arc, time::Duration};

use adw::prelude::*;
use gtk::glib;
use tracing::warn;

use crate::CatalogUi;
use crate::{LibraryLayout, LibraryListKey, LibraryListSettings};
use gtk_widgets::localization::bind_search_placeholder;
use gtk_widgets::mounted_route::{LatestMountedRouteRead, MountedRoute};
use localization::msgid;

use super::album_detail::AlbumCollectionModels;
use super::collections::{
    album_collection_projection, library_route_inset, track_collection_projection,
};
use super::route_shell::{LibraryPageShellOptions, LibraryToolbarProjection};
use crate::track_model::TrackCollectionModel;
use gtk_widgets::route::{CollectionCategory, Route};

const TRACK_FILTER_DEBOUNCE: Duration = Duration::from_millis(150);

struct RootTrackRouteOptions {
    key: LibraryListKey,
    context: &'static str,
    empty_body: &'static str,
}

pub(crate) type ListPageLoad<R> = Arc<
    dyn Fn(
            library::ListRead,
            library::ReadCancellation,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<R, String>> + Send>>
        + Send
        + Sync,
>;

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

    pub fn query(&self) -> library::MemberRead {
        self.model.query()
    }

    pub fn connect_search_request(&self, callback: impl Fn(library::MemberRead) + 'static) {
        let model = self.model.clone();
        let callback = Rc::new(callback);
        let pending = Rc::new(RefCell::new(None::<glib::SourceId>));
        self.search.connect_search_changed(move |_| {
            if let Some(source) = pending.borrow_mut().take() {
                source.remove();
            }
            let request = model.query();
            if request.filter.is_empty() {
                callback(request);
                return;
            }
            let callback = Rc::clone(&callback);
            let model = model.clone();
            let pending_timeout = Rc::clone(&pending);
            pending.replace(Some(glib::timeout_add_local_once(
                TRACK_FILTER_DEBOUNCE,
                move || {
                    pending_timeout.borrow_mut().take();
                    callback(model.query());
                },
            )));
        });
    }

    pub fn replace_page(&self, prepared: rufin_core::browse::MemberPage) -> bool {
        self.model.replace_page(prepared)
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
        self.model.apply_settings(&settings);
        self.collection.apply_settings(&settings);
    }
}

impl TrackListProjection {
    fn page_read(
        &self,
        shell: &Rc<CatalogUi>,
        context: &'static str,
        changed: impl Fn() + 'static,
    ) -> Rc<
        LatestMountedRouteRead<Result<rufin_core::browse::MemberPage, String>, library::MemberRead>,
    > {
        let runtime = shell.runtime.clone();
        let database = Arc::clone(&shell.library);
        let shell = Rc::downgrade(shell);
        let model = self.model.clone();
        let apply = Rc::new(move |_, result| {
            let Some(shell) = shell.upgrade() else { return };
            if !(shell.is_current)() {
                return;
            }
            match result {
                Ok(page) => {
                    if model.replace_page(page) {
                        changed();
                    }
                }
                Err(error) => warn!(%error, context, "failed to read Track page"),
            }
        });
        let load = Arc::new(
            move |query: library::MemberRead, cancellation: library::ReadCancellation| {
                let database = Arc::clone(&database);
                Box::pin(async move {
                    MemberPage::load(
                        &database,
                        query,
                        library::RouteSeedWindow::top(),
                        &cancellation,
                    )
                    .await
                    .map_err(|error| error.to_string())
                }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
            },
        );
        LatestMountedRouteRead::new_with_request(runtime, apply, load, context)
    }

    pub fn connect_read(&self, shell: &Rc<CatalogUi>, context: &'static str) -> Rc<dyn Fn()> {
        let read = self.page_read(shell, context, || {});
        let search_read = Rc::downgrade(&read);
        self.connect_search_request(move |query| {
            if let Some(read) = search_read.upgrade() {
                read.request_with(query);
            }
        });
        let model = self.model.clone();
        Rc::new(move || read.request_with(model.query()))
    }
}

impl CatalogUi {
    pub fn opened_collection_view(
        self: &Rc<Self>,
        route: Route,
        opened: OpenedCollection,
        selected: Option<rufin_core::runtime::SelectedLibrary>,
    ) -> MountedRoute {
        match opened {
            OpenedCollection::Tracks(page) => match route {
                Route::History => self.history_route(page),
                Route::Favorites => {
                    self.favorites_route(page, selected.expect("source collection"))
                }
                _ => self.library_tracks_route(page, selected.expect("source collection")),
            },
            OpenedCollection::Detail(detail) => {
                match route {
                    Route::AlbumDetail(_) => self.album_detail_view(detail),
                    Route::ArtistDetail(_) | Route::AlbumArtistDetail(_) => self
                        .artist_detail_view(matches!(route, Route::AlbumArtistDetail(_)), detail),
                    Route::ArtistDiscography(_) | Route::AlbumArtistDiscography(_) => {
                        self.artist_discography_view(detail)
                    }
                    Route::ArtistTracks(_) | Route::AlbumArtistTracks(_) => self
                        .artist_tracks_view(matches!(route, Route::AlbumArtistTracks(_)), detail),
                    Route::ArtistFavoriteTracks(_) | Route::AlbumArtistFavoriteTracks(_) => self
                        .artist_favorite_tracks_view(
                            matches!(route, Route::AlbumArtistFavoriteTracks(_)),
                            detail,
                        ),
                    Route::PlaylistDetail(key) => self.playlist_detail_route(key, detail),
                    Route::SmartPlaylistDetail(key) => {
                        self.smart_playlist_detail_route(key, detail)
                    }
                    Route::GenreDetail(key) => self
                        .named_detail_view(super::named_detail::NamedDetailId::Genre(key), detail),
                    Route::MoodDetail(key) => self
                        .named_detail_view(super::named_detail::NamedDetailId::Mood(key), detail),
                    _ => unreachable!("detail renderer"),
                }
            }
            OpenedCollection::Albums(page) => self.library_albums_route(page),
            OpenedCollection::Artists { read, page } => self.library_artist_list_route(read, page),
            OpenedCollection::Genres { read, page } => self.library_genres_route(read, page),
            OpenedCollection::Moods { read, page } => self.library_moods_route(read, page),
            OpenedCollection::Playlists { read, page } => self.library_playlists_route(read, page),
            OpenedCollection::SmartPlaylists { read, page } => {
                self.library_smart_playlists_route(read, page)
            }
            OpenedCollection::Folders(page) => {
                let Route::Folders { path } = route else {
                    unreachable!("folder renderer")
                };
                self.folders_route(path, selected.as_ref().expect("folder source"), Ok(page))
            }
            OpenedCollection::Home(page) => self.home_route(selected.expect("home source"), *page),
            OpenedCollection::Search(_) => self.search_route(),
        }
    }

    pub(crate) fn collection_reload<T: Send + 'static>(
        self: &Rc<Self>,
        key: LibraryListKey,
        search: &gtk::SearchEntry,
        captured: library::ListRead,
        read: &Rc<LatestMountedRouteRead<T, library::ListRead>>,
        apply_settings: impl Fn(&LibraryListSettings) -> bool + 'static,
    ) -> Rc<dyn Fn()> {
        let shell = Rc::downgrade(self);
        let search_read = Rc::downgrade(read);
        let search_captured = captured.clone();
        search.connect_search_changed(move |search| {
            let (Some(shell), Some(read)) = (shell.upgrade(), search_read.upgrade()) else {
                return;
            };
            let settings = shell.settings.current.borrow().library_list(key);
            read.request_with(rufin_core::browse::list_display(
                &search_captured,
                &search.text(),
                &settings,
            ));
        });
        let shell = Rc::downgrade(self);
        let read = Rc::clone(read);
        let search = search.clone();
        Rc::new(move || {
            let Some(shell) = shell.upgrade() else { return };
            let settings = shell.settings.current.borrow().library_list(key);
            if apply_settings(&settings) {
                read.request_with(rufin_core::browse::list_display(
                    &captured,
                    &search.text(),
                    &settings,
                ));
            }
        })
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
        opened: rufin_core::browse::AlbumListPage,
    ) -> MountedRoute {
        let captured = opened.read;
        let favorites_only = captured.favorites_only;
        let key = LibraryListKey::Albums;
        let settings = self.settings.current.borrow().library_list(key);
        let applied_settings = Rc::new(RefCell::new(settings.clone()));
        let applied_query = Rc::new(RefCell::new(captured.clone()));
        let models = AlbumCollectionModels::new(
            &self.library,
            &self.runtime,
            opened.content,
            Rc::clone(&applied_query),
        );
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
            let applied_settings = Rc::clone(&applied_settings);
            Rc::new(
                move |request: library::ListRead,
                      result: Result<rufin_core::browse::AlbumListContent,String>| {
                    let Some(shell) = shell.upgrade() else {
                        return;
                    };
                    if !(shell.is_current)() {
                        return;
                    }
                    match result {
                        Ok(albums) => {
                            applied_query.replace(request);
                            let settings = shell.settings.current.borrow().library_list(key);
                            let current_models = models.borrow().clone();
                            let replace_model = current_models.is_detail() != matches!(&albums,rufin_core::browse::AlbumListContent::Detail {..});
                            if replace_model {
                                models.replace(AlbumCollectionModels::new(&shell.library,&shell.runtime,albums,Rc::clone(&applied_query)));
                            } else if !current_models.replace_prepared(albums) {
                                warn!("rejected a mismatched prepared Albums page");return;
                            }
                            content.apply_settings(&settings);
                            page.apply_library_list_settings(key, &settings);
                            let models = models.borrow().clone();
                            page.set_empty(models.is_empty());
                            if replace_model {
                                models.resume_initial_demand();
                            }
                            applied_settings.replace(settings);
                        }
                        Err(error) => warn!(%error, "failed to refresh Albums order"),
                    }
                },
            )
        };
        let source_owner = self.source.clone();
        let reload_read = captured.clone();
        let favorite_captured = captured.clone();
        let load: ListPageLoad<rufin_core::browse::AlbumListContent> =
            Arc::new(move |read, cancel| {
                let source_owner = source_owner.clone();
                Box::pin(async move {
                    source_owner
                        .album_list_page(read, library::RouteSeedWindow::top(), &cancel)
                        .await
                        .map(|opened| opened.content)
                        .map_err(|error| error.to_string())
                })
            });
        let read = LatestMountedRouteRead::new_with_request(
            self.runtime.clone(),
            apply,
            load,
            "mounted Albums route",
        );
        let resume = self.collection_reload(key, &search, reload_read, &read, {
            let content = content.clone();
            let models = models.clone();
            let page = page.clone();
            let applied_settings = Rc::clone(&applied_settings);
            move |settings| {
                let previous = applied_settings.borrow().clone();
                if models.borrow().is_detail() == (settings.layout == LibraryLayout::Detail) {
                    content.apply_settings(settings);
                }
                page.apply_library_list_settings(key, settings);
                let reload = (previous.layout == LibraryLayout::Detail)
                    != (settings.layout == LibraryLayout::Detail)
                    || previous.sort_key != settings.sort_key
                    || previous.descending != settings.descending
                    || rufin_core::browse::uses_activity(&Route::Albums, &previous)
                        != rufin_core::browse::uses_activity(&Route::Albums, settings);
                applied_settings.replace(settings.clone());
                reload
            }
        });
        let refresh = {
            let read = Rc::clone(&read);
            let search = search.clone();
            let shell = Rc::downgrade(self);
            let captured = captured.clone();
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else { return };
                let settings = shell.settings.current.borrow().library_list(key);
                read.request_with(rufin_core::browse::list_display(
                    &captured,
                    &search.text(),
                    &settings,
                ));
            })
        };
        let favorite_models = models.clone();

        let download_models = models.clone();
        let downloads = crate::collection_download_change(move |identity, downloaded| {
            let Some(uri) = identity.strip_prefix("album:") else {
                return;
            };
            let rows = download_models.borrow().album_rows();
            if let Some(rows) = rows {
                rows.update_matching(
                    |row| {
                        row.media_uri == uri
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
        let favorite_read = Rc::clone(&read);
        let favorite_search = search.clone();
        if favorites_only {
            self.install_favorite_category_tabs(CollectionCategory::Albums, &mut page);
        }
        page.mounted_route(resume)
            .with_catalog_refresh(refresh)
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
            .with_favorite_settlement(Rc::new(move |settlement| {
                let library::FavoriteTarget::Album(album) = settlement.target else {
                    return;
                };
                let models = favorite_models.borrow().clone();
                models.update_favorite(&album, settlement.effective);
                let settings = applied_settings.borrow().clone();
                if favorites_only || settings.sort_key == crate::LibraryField::Favorite {
                    favorite_read.request_with(rufin_core::browse::list_display(
                        &favorite_captured,
                        &favorite_search.text(),
                        &settings,
                    ));
                }
            }))
    }

    pub fn library_tracks_route(
        self: &Rc<Self>,
        page: rufin_core::browse::MemberPage,
        selected: rufin_core::runtime::SelectedLibrary,
    ) -> MountedRoute {
        self.root_track_route(
            RootTrackRouteOptions {
                key: LibraryListKey::Tracks,
                context: "tracks",
                empty_body: msgid("Nothing here yet"),
            },
            page,
            selected,
            false,
        )
    }

    pub fn favorites_route(
        self: &Rc<Self>,
        page: rufin_core::browse::MemberPage,
        selected: rufin_core::runtime::SelectedLibrary,
    ) -> MountedRoute {
        let key = LibraryListKey::FavoriteTracks;
        self.root_track_route(
            RootTrackRouteOptions {
                key,
                context: "favorite-tracks",
                empty_body: msgid("No favorites yet"),
            },
            page,
            selected,
            true,
        )
    }

    fn root_track_route(
        self: &Rc<Self>,
        options: RootTrackRouteOptions,
        page: rufin_core::browse::MemberPage,
        selected: rufin_core::runtime::SelectedLibrary,
        favorites_only: bool,
    ) -> MountedRoute {
        let context_id = selected.music_folder_key.as_ref().map_or_else(
            || format!("{}:all", options.context),
            |folder_key| format!("{}:{folder_key}", options.context),
        );
        let projection = self.searchable_track_collection(
            page,
            options.key,
            SearchableTrackOptions {
                context_id,
                content_inset: 0,
                fixed_layout: None,
                search: None,
            },
        );
        self.track_page_route(options, projection, favorites_only)
    }

    pub fn history_route(self: &Rc<Self>, initial: rufin_core::browse::MemberPage) -> MountedRoute {
        let key = LibraryListKey::History;
        let projection = self.searchable_track_collection(
            initial,
            key,
            SearchableTrackOptions {
                context_id: "history:all".to_string(),
                content_inset: 0,
                fixed_layout: None,
                search: None,
            },
        );
        let page = self.track_page_shell(key, msgid("Nothing played yet"), &projection);
        let apply_page = page.clone();
        let apply_projection = projection.clone();
        let read = projection.page_read(self, "History filter", move || {
            apply_page.set_empty(apply_projection.source_is_empty());
        });
        let filter_read = Rc::downgrade(&read);
        let filter_projection = projection.clone();
        let filter_shell = Rc::downgrade(self);
        let current_source_name = self.history_source_name.clone();
        page.configure_history_filter(current_source_name, move |current_only| {
            let source = current_only
                .then(|| {
                    filter_shell
                        .upgrade()
                        .and_then(|shell| (shell.history_source_id)())
                })
                .flatten();
            filter_projection
                .model
                .set_scope(library::Collection::History {
                    source: source.clone(),
                });
            let Some(read) = filter_read.upgrade() else {
                return;
            };
            read.request_with(filter_projection.query());
        });
        self.mount_track_page(key, projection, page, read, None)
    }

    pub fn library_artist_list_route(
        self: &Rc<Self>,
        captured: library::ListRead,
        initial: library::Page<library::ArtistView>,
    ) -> MountedRoute {
        let key = if matches!(
            captured.kind,
            library::ListKind::Artists { album_artist: true }
        ) {
            LibraryListKey::AlbumArtists
        } else {
            LibraryListKey::Artists
        };
        self.collection_list_route(
            key,
            if captured.favorites_only {
                msgid("No favorite artists yet")
            } else {
                msgid("Nothing here yet")
            },
            initial,
            |row: &library::ArtistView| row.artist_key,
            captured,
        )
    }

    pub fn library_playlists_route(
        self: &Rc<Self>,
        read: library::ListRead,
        page: library::Page<library::PlaylistRow>,
    ) -> MountedRoute {
        self.collection_list_route(
            LibraryListKey::Playlists,
            msgid("No playlists yet"),
            page,
            |row: &library::PlaylistRow| row.playlist_key,
            read,
        )
    }

    pub fn library_smart_playlists_route(
        self: &Rc<Self>,
        read: library::ListRead,
        page: library::Page<library::SmartPlaylistRow>,
    ) -> MountedRoute {
        self.collection_list_route(
            LibraryListKey::SmartPlaylists,
            msgid("No smart playlists yet"),
            page,
            |row: &library::SmartPlaylistRow| row.smart_playlist_key,
            read,
        )
    }

    pub fn searchable_track_collection(
        self: &Rc<Self>,
        page: rufin_core::browse::MemberPage,
        key: LibraryListKey,
        options: SearchableTrackOptions,
    ) -> TrackListProjection {
        let mut settings = self.settings.current.borrow().library_list(key);
        if let Some(layout) = options.fixed_layout {
            settings.layout = layout;
        }
        let model =
            TrackCollectionModel::new(Arc::clone(&self.library), self.runtime.clone(), page);
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

    fn track_page_route(
        self: &Rc<Self>,
        options: RootTrackRouteOptions,
        projection: TrackListProjection,
        favorites_only: bool,
    ) -> MountedRoute {
        let key = options.key;
        let mut page = self.track_page_shell(key, options.empty_body, &projection);
        let apply_page = page.clone();
        let apply_projection = projection.clone();
        let read = projection.page_read(self, "mounted Track route", move || {
            apply_page.set_empty(apply_projection.source_is_empty());
        });
        if favorites_only {
            self.install_favorite_category_tabs(CollectionCategory::Tracks, &mut page);
        }
        self.mount_track_page(key, projection, page, read, Some(favorites_only))
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
        read: Rc<
            LatestMountedRouteRead<
                Result<rufin_core::browse::MemberPage, String>,
                library::MemberRead,
            >,
        >,
        favorite_requery: Option<bool>,
    ) -> MountedRoute {
        let search_read = Rc::downgrade(&read);
        projection.connect_search_request(move |projection_request| {
            if let Some(read) = search_read.upgrade() {
                read.request_with(projection_request);
            }
        });
        let resume = {
            let shell = Rc::downgrade(self);
            let projection = projection.clone();
            let page = page.clone();
            let read = Rc::clone(&read);
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else { return };
                let settings = shell.settings.current.borrow().library_list(key);
                let previous = projection.query();
                projection.apply_library_list_settings(key, &settings);
                page.apply_library_list_settings(key, &settings);
                let request = projection.query();
                if previous != request {
                    read.request_with(request);
                }
            })
        };
        let refresh = {
            let projection = projection.clone();
            let read = Rc::clone(&read);
            Rc::new(move || read.request_with(projection.query()))
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
                let projection_request = favorite_projection.query();
                if favorite_requery.is_some_and(|favorites_only| {
                    favorites_only
                        || projection_request.sort == Some(library::CatalogSort::Favorite)
                }) {
                    favorite_read.request_with(projection_request);
                }
            }))
    }
}
