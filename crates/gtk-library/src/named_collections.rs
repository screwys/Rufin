use gtk_widgets::media_drag::{
    MediaDragSource, install_compact_media_drag_source, install_media_drag_source,
};
use gtk_widgets::sparse_model::item_at_from_item;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::gio;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use library::{
    ArtistKey, ArtistView, GenreKey, GenreView, MoodKey, MoodView, PlaylistKey, PlaylistRow,
    SmartPlaylistKey, SmartPlaylistRow,
};
use localization::{album_count_text, msgid, track_count_text};

use crate::CatalogUi;
use crate::{LibraryLayout, LibraryListKey};
use gtk_widgets::artwork::CoverGroupProjection;
use gtk_widgets::mounted_route::{LatestMountedRouteRead, MountedRoute};

use super::collections::{
    LibraryCollectionProjection, LibraryPresentationProjection, collection_column_width,
    dynamic_collection_table,
};
use super::columns::{artwork_column, column_fit_width, mapped_row_index_column, text_column};
use super::grid_cells::{
    CollectionGridCardCell, CollectionGridProjection, ReusableCollectionGridCell,
    collection_grid_with_demand, install_grid_context, install_grid_play,
};
use gtk_media_menus::media_menus::{
    present_genre_context_menu, present_mood_context_menu, present_smart_playlist_context_menu,
};

use super::route_shell::LibraryPageShellOptions;
use super::table_sizing::route_column_view_initial_width;
use crate::LibraryField;
use gtk_widgets::detail_links::DetailLinks;
use gtk_widgets::format_duration_units;
use gtk_widgets::interactions::install_context_menu_openers;
use gtk_widgets::library_fields::{playlist_artwork, playlist_field, smart_playlist_field};
use gtk_widgets::recycled_cells::{RecycledBadgedTextCell, list_cell};
use gtk_widgets::route::Route;
use gtk_widgets::sparse_model::{SparseRouteModel, SparseSource, connect_sparse_bind};

impl CatalogUi {
    pub fn library_genres_route(
        self: &Rc<Self>,
        read: library::ListRead,
        page: library::Page<GenreView>,
    ) -> MountedRoute {
        self.collection_list_route(
            LibraryListKey::Genres,
            msgid("Nothing here yet"),
            page,
            |row: &GenreView| row.genre_key,
            read,
        )
    }

    pub fn library_moods_route(
        self: &Rc<Self>,
        read: library::ListRead,
        page: library::Page<MoodView>,
    ) -> MountedRoute {
        self.collection_list_route(
            LibraryListKey::Moods,
            msgid("Nothing here yet"),
            page,
            |row: &MoodView| row.mood_key,
            read,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn collection_list_route<K, R>(
        self: &Rc<Self>,
        key: LibraryListKey,
        empty_body: &'static str,
        initial: library::Page<R>,
        row_key: impl Fn(&R) -> K + 'static,
        captured: library::ListRead,
    ) -> MountedRoute
    where
        K: Clone + Eq + Send + Sync + 'static,
        R: CollectionRow<Key = K>,
    {
        let library::Page {
            count,
            first_row_position,
            rows: first_rows,
        } = initial;
        let reload_read = captured.clone();
        let favorites_only = captured.favorites_only;
        let favorite_read = captured.clone();
        let database = self.library.clone();
        let order_load: crate::routes::ListPageLoad<library::Page<R>> =
            Arc::new(move |request, cancel| {
                let read = request;
                let database = database.clone();
                Box::pin(async move {
                    let page = R::page(&read, &database, library::RouteSeedWindow::top(), &cancel)
                        .await
                        .map_err(|error| error.to_string())?;
                    Ok(page)
                })
            });
        let applied = Rc::new(RefCell::new(captured.clone()));
        let load_request = Rc::clone(&applied);
        let row_database = self.library.clone();
        let sparse = SparseRouteModel::new(
            SparseSource::Query { count },
            32,
            self.runtime.clone(),
            Rc::new(move |_, range, cancellation| {
                let request = load_request.borrow().clone();
                let database = row_database.clone();
                Box::pin(async move {
                    R::read(&request, &database, range.start, range.len(), &cancellation)
                        .await
                        .map_err(|error| error.to_string())
                })
            }),
        );
        let row_key = Rc::new(row_key);
        gtk_widgets::media_drag::install_parent_selection(&sparse, Rc::clone(&applied), {
            let selected = self.selected.clone();
            move |row| row.playback(key, selected.as_ref())
        });
        sparse.seed_matching_at(first_row_position, first_rows, {
            let row_key = Rc::clone(&row_key);
            move |row| row_key(row)
        });
        let content = R::projection(self, Rc::clone(&sparse), key);
        let search = gtk::SearchEntry::new();
        gtk_widgets::controls::configure_search_entry(&search);
        gtk_widgets::localization::bind_search_placeholder(&search, "Search");
        let mut page = self.library_page_shell(LibraryPageShellOptions {
            key,
            empty: sparse.len() == 0,
            empty_body,
            search: search.clone(),
            has_visible_results: {
                let sparse = Rc::clone(&sparse);
                Rc::new(move || sparse.len() != 0)
            },
            content: content.scrolling_widget(),
        });
        let apply = {
            let shell = Rc::downgrade(self);
            let sparse = Rc::clone(&sparse);
            let page = page.clone();
            let content = content.clone();
            let row_key = Rc::clone(&row_key);
            Rc::new(
                move |request: library::ListRead, result: Result<library::Page<R>, String>| {
                    let Some(shell) = shell.upgrade() else {
                        return;
                    };
                    if !(shell.is_current)() {
                        return;
                    }
                    match result {
                        Ok(library::Page {
                            count,
                            first_row_position,
                            rows: first_rows,
                        }) => {
                            applied.replace(request.clone());
                            let row_key = Rc::clone(&row_key);
                            if !sparse.replace_prepared_at(
                                SparseSource::Query { count },
                                first_row_position,
                                first_rows,
                                Vec::new(),
                                move |row| row_key(row),
                            ) {
                                tracing::warn!("rejected a mismatched prepared named page");
                                return;
                            }
                            let settings = shell.settings.current.borrow().library_list(key);
                            content.apply_settings(&settings);
                            page.apply_library_list_settings(key, &settings);
                            page.set_empty(sparse.len() == 0);
                        }
                        Err(error) => tracing::warn!(%error, "failed to refresh named collection"),
                    }
                },
            )
        };
        let read = LatestMountedRouteRead::new_with_request(
            self.runtime.clone(),
            apply,
            order_load,
            "mounted named collection",
        );
        let resume = self.collection_reload(key, &search, reload_read, &read, {
            let content = content.clone();
            let page = page.clone();
            move |settings| {
                content.apply_settings(settings);
                page.apply_library_list_settings(key, settings);
                true
            }
        });

        let download_rows = Rc::clone(&sparse);
        let downloads = crate::collection_download_change(move |identity, downloaded| {
            download_rows.update_matching(
                |row| {
                    row.playback(key, None)
                        .is_some_and(|target| target.context() == Some(identity))
                        && row.downloaded() != downloaded
                },
                |row| row.set_downloaded(downloaded),
            );
        });
        if favorites_only {
            self.install_favorite_category_tabs(
                rufin_core::route::CollectionCategory::Artists,
                &mut page,
            );
        }
        let favorites = Rc::clone(&sparse);
        let shell = Rc::downgrade(self);
        let favorite_search = search.clone();
        page.mounted_route(resume)
            .with_favorite_settlement(Rc::new(move |settlement| {
                if !R::FAVORITES {
                    return;
                }
                favorites.update_matching(
                    |row| row.matches_favorite(&settlement.target),
                    |row| row.set_favorite(settlement.effective),
                );
                let Some(shell) = shell.upgrade() else { return };
                let settings = shell.settings.current.borrow().library_list(key);
                if favorites_only || settings.sort_key == LibraryField::Favorite {
                    read.request_with(rufin_core::browse::list_display(
                        &favorite_read,
                        &favorite_search.text(),
                        &settings,
                    ));
                }
            }))
            .with_download_change(downloads)
            .with_item_navigation(content.item_navigation())
            .with_initial_demand({
                let sparse = Rc::clone(&sparse);
                Rc::new(move || sparse.resume_initial_demand())
            })
    }
}

pub trait CollectionRow: Clone + PartialEq + Send + Sync + 'static {
    type Key: Clone + Eq + Send + Sync + 'static;
    const FAVORITES: bool = false;
    fn read<'a>(
        read: &'a library::ListRead,
        database: &'a library::Database,
        offset: usize,
        limit: usize,
        cancel: &'a library::ReadCancellation,
    ) -> impl std::future::Future<Output = library::LibraryResult<Vec<Self>>> + Send + 'a;
    fn page<'a>(
        read: &'a library::ListRead,
        database: &'a library::Database,
        window: library::RouteSeedWindow,
        cancel: &'a library::ReadCancellation,
    ) -> impl std::future::Future<Output = library::LibraryResult<library::Page<Self>>> + Send + 'a;

    fn playback(
        &self,
        key: LibraryListKey,
        _selected: Option<&rufin_core::runtime::SelectedLibrary>,
    ) -> Option<rufin_core::playback::PlayRequest>;
    fn downloaded(&self) -> bool;
    fn set_downloaded(&mut self, downloaded: bool);
    fn projection(
        shell: &Rc<CatalogUi>,
        sparse: Rc<SparseRouteModel<Self::Key, Self>>,
        key: LibraryListKey,
    ) -> LibraryCollectionProjection;
    fn matches_favorite(&self, _target: &library::FavoriteTarget) -> bool {
        false
    }
    fn set_favorite(&mut self, _favorite: bool) {}
}

pub trait NamedCollectionRow: CollectionRow {
    const MOSAIC: bool = true;

    fn name(&self) -> &str;
    fn folder(&self) -> bool {
        false
    }
    fn field(&self, field: LibraryField) -> String;
    fn route(&self) -> Route;
    fn artwork(&self) -> Vec<artwork::ArtworkBinding>;
    fn present_context(
        &self,
        target: &gtk::Widget,
        shell: &Rc<CatalogUi>,
        position: Option<(f64, f64)>,
    );
    fn install_extra(
        _widget: &gtk::Widget,
        _shell: &Rc<CatalogUi>,
        _current: Rc<dyn Fn() -> Option<Self>>,
    ) where
        Self: Sized,
    {
    }
    fn grid(
        shell: &Rc<CatalogUi>,
        sparse: &Rc<SparseRouteModel<Self::Key, Self>>,
        fields: &[LibraryField],
        key: LibraryListKey,
    ) -> CollectionGridProjection
    where
        Self: Send + Sync,
    {
        named_collection_grid_impl(shell, sparse, fields, key)
    }
    fn table(
        shell: &Rc<CatalogUi>,
        sparse: &Rc<SparseRouteModel<Self::Key, Self>>,
        key: LibraryListKey,
    ) -> super::collections::CollectionTableProjection;
}

impl CollectionRow for ArtistView {
    type Key = ArtistKey;
    const FAVORITES: bool = true;
    fn read<'a>(
        read: &'a library::ListRead,
        database: &'a library::Database,
        offset: usize,
        limit: usize,
        cancel: &'a library::ReadCancellation,
    ) -> impl std::future::Future<Output = library::LibraryResult<Vec<Self>>> + Send + 'a {
        read.artists(database, offset, limit, cancel)
    }
    fn page<'a>(
        read: &'a library::ListRead,
        database: &'a library::Database,
        window: library::RouteSeedWindow,
        cancel: &'a library::ReadCancellation,
    ) -> impl std::future::Future<Output = library::LibraryResult<library::Page<Self>>> + Send + 'a
    {
        read.page_artists(database, window, cancel)
    }
    fn playback(
        &self,
        key: LibraryListKey,
        _selected: Option<&rufin_core::runtime::SelectedLibrary>,
    ) -> Option<rufin_core::playback::PlayRequest> {
        Some(gtk_widgets::selection::artist_play_request(
            self,
            key == LibraryListKey::AlbumArtists,
        ))
    }
    fn downloaded(&self) -> bool {
        self.summary.track_count > 0 && self.summary.downloaded_count == self.summary.track_count
    }
    fn set_downloaded(&mut self, downloaded: bool) {
        self.summary.downloaded_count = if downloaded {
            self.summary.track_count
        } else {
            0
        };
    }
    fn projection(
        shell: &Rc<CatalogUi>,
        sparse: Rc<SparseRouteModel<Self::Key, Self>>,
        key: LibraryListKey,
    ) -> LibraryCollectionProjection {
        super::collections::artist_collection_projection(shell, sparse, key)
    }
    fn matches_favorite(&self, target: &library::FavoriteTarget) -> bool {
        matches!(target,library::FavoriteTarget::Artist(uri) if uri == &self.media_uri)
    }
    fn set_favorite(&mut self, favorite: bool) {
        self.state.favorite = favorite;
    }
}

impl CollectionRow for GenreView {
    type Key = GenreKey;
    fn read<'a>(
        read: &'a library::ListRead,
        database: &'a library::Database,
        offset: usize,
        limit: usize,
        cancel: &'a library::ReadCancellation,
    ) -> impl std::future::Future<Output = library::LibraryResult<Vec<Self>>> + Send + 'a {
        read.genres(database, offset, limit, cancel)
    }

    fn page<'a>(
        read: &'a library::ListRead,
        database: &'a library::Database,
        window: library::RouteSeedWindow,
        cancel: &'a library::ReadCancellation,
    ) -> impl std::future::Future<Output = library::LibraryResult<library::Page<Self>>> + Send + 'a
    {
        read.page_genres(database, window, cancel)
    }

    fn set_downloaded(&mut self, downloaded: bool) {
        self.summary.downloaded_count = if downloaded {
            self.summary.track_count
        } else {
            0
        };
    }

    fn playback(
        &self,
        _key: LibraryListKey,
        _selected: Option<&rufin_core::runtime::SelectedLibrary>,
    ) -> Option<rufin_core::playback::PlayRequest> {
        Some(gtk_widgets::selection::collection_play_request(
            library::Collection::Genre(self.genre_key),
        ))
    }

    fn downloaded(&self) -> bool {
        self.summary.track_count > 0 && self.summary.downloaded_count == self.summary.track_count
    }
    fn projection(
        shell: &Rc<CatalogUi>,
        sparse: Rc<SparseRouteModel<Self::Key, Self>>,
        key: LibraryListKey,
    ) -> LibraryCollectionProjection {
        named_collection_projection(shell, sparse, key)
    }
}

impl NamedCollectionRow for GenreView {
    fn name(&self) -> &str {
        &self.metadata.name
    }
    fn route(&self) -> Route {
        Route::GenreDetail(self.genre_key)
    }
    fn artwork(&self) -> Vec<artwork::ArtworkBinding> {
        self.artwork_binding
            .as_ref()
            .map(std::slice::from_ref)
            .unwrap_or(&self.representative_artwork)
            .iter()
            .map(|binding| artwork::ArtworkBinding::opaque(binding))
            .collect()
    }
    fn field(&self, field: LibraryField) -> String {
        match field {
            LibraryField::Title | LibraryField::TitleMerged => self.metadata.name.clone(),
            LibraryField::AlbumCount => album_count_text(self.summary.album_count.max(0) as u64),
            LibraryField::SongCount => track_count_text(self.summary.track_count.max(0) as u64),
            LibraryField::Duration => {
                format_duration_units((self.summary.duration_millis.max(0) / 1_000) as u32)
            }
            _ => String::new(),
        }
    }

    fn present_context(
        &self,
        target: &gtk::Widget,
        shell: &Rc<CatalogUi>,
        position: Option<(f64, f64)>,
    ) {
        present_genre_context_menu(target, &shell.media_menus, self.clone(), None, position);
    }
    fn table(
        shell: &Rc<CatalogUi>,
        sparse: &Rc<SparseRouteModel<Self::Key, Self>>,
        _key: LibraryListKey,
    ) -> super::collections::CollectionTableProjection {
        named_collection_table::<Self, _>(shell, sparse.list_model(), LibraryListKey::Genres)
    }
}

impl CollectionRow for MoodView {
    type Key = MoodKey;
    fn read<'a>(
        read: &'a library::ListRead,
        database: &'a library::Database,
        offset: usize,
        limit: usize,
        cancel: &'a library::ReadCancellation,
    ) -> impl std::future::Future<Output = library::LibraryResult<Vec<Self>>> + Send + 'a {
        read.moods(database, offset, limit, cancel)
    }

    fn page<'a>(
        read: &'a library::ListRead,
        database: &'a library::Database,
        window: library::RouteSeedWindow,
        cancel: &'a library::ReadCancellation,
    ) -> impl std::future::Future<Output = library::LibraryResult<library::Page<Self>>> + Send + 'a
    {
        read.page_moods(database, window, cancel)
    }

    fn set_downloaded(&mut self, downloaded: bool) {
        self.summary.downloaded_count = if downloaded {
            self.summary.track_count
        } else {
            0
        };
    }

    fn playback(
        &self,
        _key: LibraryListKey,
        _selected: Option<&rufin_core::runtime::SelectedLibrary>,
    ) -> Option<rufin_core::playback::PlayRequest> {
        Some(gtk_widgets::selection::collection_play_request(
            library::Collection::Mood(self.mood_key),
        ))
    }

    fn downloaded(&self) -> bool {
        self.summary.track_count > 0 && self.summary.downloaded_count == self.summary.track_count
    }
    fn projection(
        shell: &Rc<CatalogUi>,
        sparse: Rc<SparseRouteModel<Self::Key, Self>>,
        key: LibraryListKey,
    ) -> LibraryCollectionProjection {
        named_collection_projection(shell, sparse, key)
    }
}

impl NamedCollectionRow for MoodView {
    fn name(&self) -> &str {
        &self.metadata.name
    }
    fn route(&self) -> Route {
        Route::MoodDetail(self.mood_key)
    }
    fn artwork(&self) -> Vec<artwork::ArtworkBinding> {
        self.representative_artwork
            .iter()
            .map(|binding| artwork::ArtworkBinding::opaque(binding))
            .collect()
    }
    fn field(&self, field: LibraryField) -> String {
        match field {
            LibraryField::Title | LibraryField::TitleMerged => self.metadata.name.clone(),
            LibraryField::SongCount => track_count_text(self.summary.track_count.max(0) as u64),
            LibraryField::Duration => {
                format_duration_units((self.summary.duration_millis.max(0) / 1_000) as u32)
            }
            _ => String::new(),
        }
    }

    fn present_context(
        &self,
        target: &gtk::Widget,
        shell: &Rc<CatalogUi>,
        position: Option<(f64, f64)>,
    ) {
        present_mood_context_menu(target, &shell.media_menus, self.clone(), None, position);
    }
    fn table(
        shell: &Rc<CatalogUi>,
        sparse: &Rc<SparseRouteModel<Self::Key, Self>>,
        _key: LibraryListKey,
    ) -> super::collections::CollectionTableProjection {
        named_collection_table::<Self, _>(shell, sparse.list_model(), LibraryListKey::Moods)
    }
}

impl CollectionRow for PlaylistRow {
    type Key = PlaylistKey;
    fn read<'a>(
        read: &'a library::ListRead,
        database: &'a library::Database,
        offset: usize,
        limit: usize,
        cancel: &'a library::ReadCancellation,
    ) -> impl std::future::Future<Output = library::LibraryResult<Vec<Self>>> + Send + 'a {
        read.playlists(database, offset, limit, cancel)
    }

    fn page<'a>(
        read: &'a library::ListRead,
        database: &'a library::Database,
        window: library::RouteSeedWindow,
        cancel: &'a library::ReadCancellation,
    ) -> impl std::future::Future<Output = library::LibraryResult<library::Page<Self>>> + Send + 'a
    {
        read.page_playlists(database, window, cancel)
    }

    fn set_downloaded(&mut self, downloaded: bool) {
        self.downloaded_count = if downloaded { self.track_count } else { 0 };
    }

    fn playback(
        &self,
        _key: LibraryListKey,
        _selected: Option<&rufin_core::runtime::SelectedLibrary>,
    ) -> Option<rufin_core::playback::PlayRequest> {
        Some(gtk_widgets::selection::collection_play_request(
            library::Collection::Playlist(self.playlist_key),
        ))
    }

    fn downloaded(&self) -> bool {
        self.track_count > 0 && self.downloaded_count == self.track_count
    }
    fn projection(
        shell: &Rc<CatalogUi>,
        sparse: Rc<SparseRouteModel<Self::Key, Self>>,
        key: LibraryListKey,
    ) -> LibraryCollectionProjection {
        named_collection_projection(shell, sparse, key)
    }
}

impl NamedCollectionRow for PlaylistRow {
    const MOSAIC: bool = false;

    fn name(&self) -> &str {
        &self.name
    }
    fn route(&self) -> Route {
        Route::PlaylistDetail(self.playlist_key)
    }
    fn artwork(&self) -> Vec<artwork::ArtworkBinding> {
        playlist_artwork(self)
    }
    fn field(&self, field: LibraryField) -> String {
        playlist_field(self, field)
    }

    fn present_context(
        &self,
        target: &gtk::Widget,
        shell: &Rc<CatalogUi>,
        position: Option<(f64, f64)>,
    ) {
        (shell.playlist_context_menu)(target, self.clone(), None, position);
    }
    fn install_extra(
        widget: &gtk::Widget,
        shell: &Rc<CatalogUi>,
        current: Rc<dyn Fn() -> Option<Self>>,
    ) {
        super::collections::install_playlist_drop(widget, shell, current);
    }
    fn table(
        shell: &Rc<CatalogUi>,
        sparse: &Rc<SparseRouteModel<Self::Key, Self>>,
        _key: LibraryListKey,
    ) -> super::collections::CollectionTableProjection {
        named_collection_table::<Self, _>(shell, sparse.list_model(), LibraryListKey::Playlists)
    }
}

impl CollectionRow for SmartPlaylistRow {
    type Key = SmartPlaylistKey;
    fn read<'a>(
        read: &'a library::ListRead,
        database: &'a library::Database,
        offset: usize,
        limit: usize,
        cancel: &'a library::ReadCancellation,
    ) -> impl std::future::Future<Output = library::LibraryResult<Vec<Self>>> + Send + 'a {
        read.smart_playlists(database, offset, limit, cancel)
    }

    fn page<'a>(
        read: &'a library::ListRead,
        database: &'a library::Database,
        window: library::RouteSeedWindow,
        cancel: &'a library::ReadCancellation,
    ) -> impl std::future::Future<Output = library::LibraryResult<library::Page<Self>>> + Send + 'a
    {
        read.page_smart_playlists(database, window, cancel)
    }

    fn set_downloaded(&mut self, downloaded: bool) {
        self.downloaded_count = if downloaded { self.track_count } else { 0 };
    }

    fn playback(
        &self,
        _key: LibraryListKey,
        _selected: Option<&rufin_core::runtime::SelectedLibrary>,
    ) -> Option<rufin_core::playback::PlayRequest> {
        Some(gtk_widgets::selection::smart_play_request(
            self.smart_playlist_key,
            _selected,
        ))
    }

    fn downloaded(&self) -> bool {
        self.track_count > 0 && self.downloaded_count == self.track_count
    }
    fn projection(
        shell: &Rc<CatalogUi>,
        sparse: Rc<SparseRouteModel<Self::Key, Self>>,
        key: LibraryListKey,
    ) -> LibraryCollectionProjection {
        named_collection_projection(shell, sparse, key)
    }
}

impl NamedCollectionRow for SmartPlaylistRow {
    const MOSAIC: bool = false;

    fn name(&self) -> &str {
        &self.name
    }
    fn route(&self) -> Route {
        Route::SmartPlaylistDetail(self.smart_playlist_key)
    }
    fn artwork(&self) -> Vec<artwork::ArtworkBinding> {
        self.artwork_binding
            .iter()
            .map(|binding| artwork::ArtworkBinding::opaque(binding))
            .collect()
    }
    fn field(&self, field: LibraryField) -> String {
        smart_playlist_field(self, field)
    }

    fn present_context(
        &self,
        target: &gtk::Widget,
        shell: &Rc<CatalogUi>,
        position: Option<(f64, f64)>,
    ) {
        present_smart_playlist_context_menu(
            target,
            &shell.media_menus,
            self.clone(),
            None,
            position,
        );
    }
    fn table(
        shell: &Rc<CatalogUi>,
        sparse: &Rc<SparseRouteModel<Self::Key, Self>>,
        _key: LibraryListKey,
    ) -> super::collections::CollectionTableProjection {
        named_collection_table::<Self, _>(
            shell,
            sparse.list_model(),
            LibraryListKey::SmartPlaylists,
        )
    }
}

impl CollectionRow for library::PlaylistListRow {
    type Key = (u8, i64);
    fn read<'a>(
        read: &'a library::ListRead,
        database: &'a library::Database,
        offset: usize,
        limit: usize,
        cancel: &'a library::ReadCancellation,
    ) -> impl std::future::Future<Output = library::LibraryResult<Vec<Self>>> + Send + 'a {
        read.playlist_items(database, offset, limit, cancel)
    }
    fn page<'a>(
        read: &'a library::ListRead,
        database: &'a library::Database,
        window: library::RouteSeedWindow,
        cancel: &'a library::ReadCancellation,
    ) -> impl std::future::Future<Output = library::LibraryResult<library::Page<Self>>> + Send + 'a
    {
        read.page_playlist_items(database, window, cancel)
    }
    fn playback(
        &self,
        key: LibraryListKey,
        selected: Option<&rufin_core::runtime::SelectedLibrary>,
    ) -> Option<rufin_core::playback::PlayRequest> {
        match self {
            Self::Playlist(row) => row.playback(key, selected),
            Self::SmartPlaylist(row) => row.playback(key, selected),
            Self::Folder(_) => None,
        }
    }
    fn downloaded(&self) -> bool {
        match self {
            Self::Playlist(row) => row.downloaded(),
            Self::SmartPlaylist(row) => row.downloaded(),
            Self::Folder(_) => false,
        }
    }
    fn set_downloaded(&mut self, downloaded: bool) {
        match self {
            Self::Playlist(row) => row.set_downloaded(downloaded),
            Self::SmartPlaylist(row) => row.set_downloaded(downloaded),
            Self::Folder(_) => {}
        }
    }
    fn projection(
        shell: &Rc<CatalogUi>,
        sparse: Rc<SparseRouteModel<Self::Key, Self>>,
        key: LibraryListKey,
    ) -> LibraryCollectionProjection {
        named_collection_projection(shell, sparse, key)
    }
}
impl NamedCollectionRow for library::PlaylistListRow {
    const MOSAIC: bool = false;
    fn name(&self) -> &str {
        match self {
            Self::Playlist(row) => &row.name,
            Self::SmartPlaylist(row) => &row.name,
            Self::Folder(row) => &row.name,
        }
    }
    fn folder(&self) -> bool {
        matches!(self, Self::Folder(_))
    }
    fn field(&self, field: LibraryField) -> String {
        match self {
            Self::Playlist(row) => row.field(field),
            Self::SmartPlaylist(row) => row.field(field),
            Self::Folder(row) => match field {
                LibraryField::Title | LibraryField::TitleMerged => row.name.clone(),
                LibraryField::SongCount => track_count_text(row.track_count.max(0) as u64),
                LibraryField::Duration => {
                    format_duration_units((row.duration_millis.max(0) / 1_000) as u32)
                }
                _ => String::new(),
            },
        }
    }
    fn route(&self) -> Route {
        match self {
            Self::Playlist(row) => row.route(),
            Self::SmartPlaylist(row) => row.route(),
            Self::Folder(row) => Route::PlaylistFolder {
                key: row.folder_key,
                smart: row.smart,
            },
        }
    }
    fn artwork(&self) -> Vec<artwork::ArtworkBinding> {
        match self {
            Self::Playlist(row) => row.artwork(),
            Self::SmartPlaylist(row) => row.artwork(),
            Self::Folder(row) => row
                .artwork_binding
                .iter()
                .map(|binding| artwork::ArtworkBinding::opaque(binding))
                .collect(),
        }
    }
    fn present_context(
        &self,
        target: &gtk::Widget,
        shell: &Rc<CatalogUi>,
        position: Option<(f64, f64)>,
    ) {
        match self {
            Self::Playlist(row) => row.present_context(target, shell, position),
            Self::SmartPlaylist(row) => row.present_context(target, shell, position),
            Self::Folder(row) => gtk_media_menus::playlist_folders::present_folder_context_menu(
                target,
                &shell.media_menus,
                row.clone(),
                position,
            ),
        }
    }
    fn install_extra(
        widget: &gtk::Widget,
        shell: &Rc<CatalogUi>,
        current: Rc<dyn Fn() -> Option<Self>>,
    ) {
        let menus = Rc::downgrade(&shell.media_menus);
        let drop = gtk::DropTarget::new(
            gtk::glib::BoxedAnyObject::static_type(),
            gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::MOVE,
        );
        let accept_current = Rc::clone(&current);
        drop.set_preload(true);
        drop.connect_notify_local(Some("value"), move |drop, _| {
            let Some(source) = drop
                .value()
                .as_ref()
                .and_then(gtk_widgets::media_drag::media_drag_source)
            else {
                return;
            };
            let accepts = match accept_current() {
                Some(library::PlaylistListRow::Folder(folder)) => {
                    gtk_media_menus::playlist_folders::playlist_drag_matches(&source, folder.smart)
                }
                Some(library::PlaylistListRow::Playlist(_)) => {
                    !gtk_media_menus::playlist_folders::single_regular_playlist_drag(&source)
                }
                _ => false,
            };
            if !accepts {
                drop.reject();
            }
        });
        gtk_widgets::media_drag::style_order_drop_target(&drop, false, |_| false);
        drop.connect_drop(move |_, value, _, _| {
            let (Some(menus), Some(row), Some(source)) = (
                menus.upgrade(),
                current(),
                gtk_widgets::media_drag::media_drag_source(value),
            ) else {
                return false;
            };
            match row {
                library::PlaylistListRow::Playlist(row) => {
                    if gtk_media_menus::playlist_folders::single_regular_playlist_drag(&source) {
                        return false;
                    }
                    gtk_media_menus::media_menus::add_drag_to_playlist(&menus, row, source)
                }
                library::PlaylistListRow::Folder(row) => {
                    gtk_media_menus::playlist_folders::drop_playlist_on_folder(&menus, row, source)
                }
                library::PlaylistListRow::SmartPlaylist(_) => false,
            }
        });
        widget.add_controller(drop);
    }
    fn table(
        shell: &Rc<CatalogUi>,
        sparse: &Rc<SparseRouteModel<Self::Key, Self>>,
        key: LibraryListKey,
    ) -> super::collections::CollectionTableProjection {
        named_collection_table::<Self, _>(shell, sparse.list_model(), key)
    }
}

fn named_collection_projection<T>(
    shell: &Rc<CatalogUi>,
    sparse: Rc<SparseRouteModel<T::Key, T>>,
    key: LibraryListKey,
) -> LibraryCollectionProjection
where
    T: NamedCollectionRow + Send + Sync,
{
    let settings = shell.settings.current.borrow().library_list(key);
    let shell = Rc::clone(shell);
    LibraryCollectionProjection::new(
        settings,
        Rc::new(move |layout| match layout {
            LibraryLayout::Row => {
                LibraryPresentationProjection::Row(T::table(&shell, &sparse, key))
            }
            LibraryLayout::Grid | LibraryLayout::Detail => LibraryPresentationProjection::Grid(
                named_collection_grid::<T>(&shell, &sparse, key),
            ),
        }),
    )
}

fn named_collection_grid<T>(
    shell: &Rc<CatalogUi>,
    sparse: &Rc<SparseRouteModel<T::Key, T>>,
    key: LibraryListKey,
) -> CollectionGridProjection
where
    T: NamedCollectionRow + Send + Sync,
{
    let fields = shell
        .settings
        .current
        .borrow()
        .library_list(key)
        .grid_fields;
    T::grid(shell, sparse, &fields, key)
}

fn named_collection_grid_impl<T>(
    shell: &Rc<CatalogUi>,
    sparse: &Rc<SparseRouteModel<T::Key, T>>,
    fields: &[LibraryField],
    key: LibraryListKey,
) -> CollectionGridProjection
where
    T: NamedCollectionRow + Send + Sync,
{
    let cell_shell = Rc::clone(shell);
    let activate_shell = Rc::clone(shell);
    let demand = Rc::clone(sparse);
    collection_grid_with_demand(
        sparse.list_model(),
        fields,
        move |fields| NamedCollectionGridCell::<T>::new(Rc::clone(&cell_shell), fields, key),
        move |_, row: T| activate_shell.navigate(row.route()),
        move |position| demand.demand_positions([position as usize]),
    )
}

struct NamedCollectionGridCell<T: NamedCollectionRow> {
    body: CollectionGridCardCell,
    key: LibraryListKey,
    shell: Rc<CatalogUi>,
    cover: CoverGroupProjection,
    current: Rc<std::cell::RefCell<Option<T>>>,
    play_buttons: [gtk::Button; 3],
}

impl<T: NamedCollectionRow> NamedCollectionGridCell<T> {
    fn new(shell: Rc<CatalogUi>, fields: &[LibraryField], key: LibraryListKey) -> Self {
        let current = Rc::new(std::cell::RefCell::new(None::<T>));
        let controls = super::cards::CollectionGridCoverView::without_favorite(msgid("Play"));
        let cover_host = controls.imp().cover_host.get();
        let cover = shell.artwork.elastic_cover_group_projection_for_artwork(
            &[],
            gtk_widgets::library_fields::COLLECTION_GRID_MAX_CARD_WIDTH,
            T::MOSAIC,
        );
        cover_host.append(&cover.widget());
        let menu = controls.imp().menu.get();
        let overlay = controls.imp().overlay.get();
        let play_buttons = [
            controls.imp().play.get(),
            controls.imp().play_next.get(),
            controls.imp().play_last.get(),
        ];
        install_grid_play(
            &shell,
            &current,
            play_buttons.clone(),
            move |shell, item, placement| {
                if let Some(target) = item.playback(key, shell.selected_library()) {
                    (shell.media_menus.play_target)(&target, placement, false);
                }
            },
        );
        let body = CollectionGridCardCell::new(&shell, fields, controls.upcast());
        body.set_download_badge(shell.downloads.download_badge(true));
        let drag_current = Rc::clone(&current);
        let drag_shell = Rc::downgrade(&shell);
        install_compact_media_drag_source(&body.card, &cover.widget(), move || {
            let shell = drag_shell.upgrade()?;
            let item = drag_current.borrow();
            let item = item.as_ref()?;
            let source =
                MediaDragSource::capture_target(item.playback(key, shell.selected_library())?);
            Some((source, item.field(LibraryField::Title)))
        });
        install_grid_context(
            &shell,
            &current,
            &menu,
            &overlay,
            &body.card,
            |target, shell, item, point| item.present_context(target, shell, point),
        );
        T::install_extra(body.card.upcast_ref(), &shell, {
            let current = Rc::downgrade(&current);
            Rc::new(move || current.upgrade()?.borrow().clone())
        });
        Self {
            body,
            key,
            shell,
            cover,
            current,
            play_buttons,
        }
    }
}

impl<T: NamedCollectionRow> ReusableCollectionGridCell<T> for NamedCollectionGridCell<T> {
    fn widget(&self) -> gtk::Widget {
        self.body.widget()
    }

    fn bind(&self, _: u32, item: T) {
        if let Some(target) = item.playback(self.key, self.shell.selected_library()) {
            self.body.bind_playing_target(&self.shell, target);
        }
        for button in &self.play_buttons {
            button.set_visible(!item.folder());
        }
        self.body.set_folder(item.folder());
        let artwork = item.artwork();
        self.cover.replace(&self.shell.artwork, &artwork);
        self.body
            .bind(item.name(), |field| DetailLinks::text(&item.field(field)));
        self.body
            .set_download_target(&self.shell, !self.shell.local_source && item.downloaded());
        self.current.replace(Some(item));
    }

    fn clear(&self) {
        self.cover.clear(&self.shell.artwork);
        self.body.clear(&self.shell);
        self.current.take();
    }

    fn set_ready(&self, ready: bool) {
        self.body.set_ready(ready);
    }

    fn apply_fields(&self, fields: &[LibraryField]) {
        self.body.replace_fields(&self.shell, fields);
        if let Some(item) = self.current.borrow().as_ref() {
            self.body
                .bind(item.name(), |field| DetailLinks::text(&item.field(field)));
        }
    }
}

fn named_collection_table<T, M>(
    shell: &Rc<CatalogUi>,
    model: M,
    key: LibraryListKey,
) -> super::collections::CollectionTableProjection
where
    T: NamedCollectionRow,
    M: IsA<gio::ListModel> + Clone + 'static,
{
    let fields = shell.settings.current.borrow().library_list(key).row_fields;
    let activate_shell = Rc::clone(shell);
    dynamic_collection_table(
        shell,
        key,
        model,
        &fields,
        Vec::new(),
        {
            let column_shell = Rc::clone(shell);
            move |field| named_collection_column::<T>(&column_shell, field, key)
        },
        move |field| column_fit_width(field, named_column_width(key, field)),
        true,
        Some(Box::new(move |_, item: T| {
            activate_shell.navigate(item.route())
        })),
        None,
        route_column_view_initial_width(shell),
    )
}

fn named_column_width(key: LibraryListKey, field: LibraryField) -> i32 {
    if matches!(
        key,
        LibraryListKey::Playlists | LibraryListKey::SmartPlaylists
    ) && matches!(field, LibraryField::Title | LibraryField::TitleMerged)
    {
        220
    } else {
        collection_column_width(field)
    }
}

fn named_collection_column<T: NamedCollectionRow>(
    shell: &Rc<CatalogUi>,
    field: LibraryField,
    key: LibraryListKey,
) -> gtk::ColumnViewColumn {
    let selected = shell.selected.clone();
    let column = match field {
        LibraryField::Tools => named_actions_column::<T>(shell),
        LibraryField::RowIndex => {
            mapped_row_index_column::<T>(shell, named_column_width(key, field), move |row| {
                row.playback(key, selected.as_ref())
            })
        }
        LibraryField::Image => artwork_column::<T, _>(
            shell,
            gtk_widgets::settings::library_field_title(field),
            named_column_width(key, field),
            move |item| item.artwork().into_iter().next().unwrap_or_default(),
            Some(Rc::new(move |item| {
                Some((
                    item.playback(key, selected.as_ref())?,
                    item.field(LibraryField::Title),
                ))
            })),
        ),
        LibraryField::Title | LibraryField::TitleMerged => {
            let factory = gtk::SignalListItemFactory::new();
            super::columns::bind_collection_title::<T>(&factory, shell, move |row| {
                row.playback(key, selected.as_ref())
            });
            let setup_shell = Rc::clone(shell);
            factory.connect_setup(move |_, item| {
                let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                    return;
                };
                let cell = RecycledBadgedTextCell::with_downloads(&setup_shell.downloads);
                let drag_item = item.downgrade();
                let drag_shell = Rc::downgrade(&setup_shell);
                install_media_drag_source(&cell, move || {
                    let shell = drag_shell.upgrade()?;
                    let item = drag_item.upgrade()?;
                    let row = item_at_from_item::<T>(&item)?;
                    let source = MediaDragSource::capture_target(
                        row.playback(key, shell.selected_library())?,
                    );
                    Some((source, row.field(LibraryField::Title)))
                });
                item.set_child(Some(&cell));
            });
            let bind_shell = Rc::clone(shell);
            connect_sparse_bind(&factory, move |item| {
                let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                    return;
                };
                let Some(cell) = list_cell::<RecycledBadgedTextCell>(item) else {
                    return;
                };
                let Some(row) = item_at_from_item::<T>(item) else {
                    cell.clear();
                    return;
                };
                cell.label().set_text(&row.field(LibraryField::Title));
                cell.set_folder(row.folder());
                bind_shell.bind_download_badge(&cell.downloaded(), row.downloaded());
            });
            factory.connect_unbind(|_, item| {
                if let Some(item) = item.downcast_ref::<gtk::ListItem>()
                    && let Some(cell) = list_cell::<RecycledBadgedTextCell>(item)
                {
                    cell.clear();
                }
            });
            gtk::ColumnViewColumn::new(
                Some(gtk_widgets::settings::library_field_title(field)),
                Some(factory),
            )
        }
        _ => {
            let column = text_column::<T, _>(field, named_column_width(key, field), move |row| {
                row.field(field)
            });
            column
        }
    };
    if field != LibraryField::Tools {
        let factory = column
            .factory()
            .and_downcast::<gtk::SignalListItemFactory>()
            .expect("Collection column factory");
        let shell = Rc::downgrade(shell);
        factory.connect_setup(move |_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let child = item.child().expect("Collection column child");
            if let Some(shell) = shell.upgrade() {
                T::install_extra(&child, &shell, {
                    let item = item.downgrade();
                    Rc::new(move || {
                        item.upgrade()
                            .and_then(|item| item_at_from_item::<T>(&item))
                    })
                });
            }
            let item = item.downgrade();
            let shell = shell.clone();
            install_context_menu_openers(
                &child,
                Rc::new(move |target, position| {
                    if let (Some(shell), Some(item)) = (shell.upgrade(), item.upgrade())
                        && let Some(row) = item_at_from_item::<T>(&item)
                    {
                        row.present_context(target, &shell, position);
                    }
                }),
            );
        });
    }
    column
}

pub fn named_actions_column<T: NamedCollectionRow>(shell: &Rc<CatalogUi>) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let shell = Rc::clone(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let actions = gtk_widgets::recycled_cells::RowActions::menu_only();
        let weak = item.downgrade();
        let shell = Rc::clone(&shell);
        install_context_menu_openers(
            &actions,
            Rc::new(move |target, position| {
                if let Some(row) = weak
                    .upgrade()
                    .and_then(|item| item_at_from_item::<T>(&item))
                {
                    row.present_context(target, &shell, position);
                }
            }),
        );
        item.set_child(Some(&actions));
    });
    gtk_widgets::recycled_cells::row_actions_column(&factory)
}
