use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::gio;
use library::{
    GenreKey, GenreRow, MoodKey, MoodRow, PlaylistKey, PlaylistRow, SmartPlaylistKey,
    SmartPlaylistRow,
};
use localization::{album_count_text, msgid, track_count_text};

use crate::shell::Shell;
use crate::shell::cover::{ArtworkTile, LARGE_COVER_SIZE, THUMB_COVER_SIZE};
use crate::shell::route::{LatestMountedRouteRead, MountedRoute};
use crate::{LibraryLayout, LibraryListKey, LibraryListSettings};

use super::collection_context::{
    present_genre_context_menu, present_mood_context_menu, present_playlist_context_menu,
    present_smart_playlist_context_menu,
};
use super::collections::{
    LibraryCollectionProjection, LibraryPresentationProjection, collection_column_width,
    dynamic_collection_table, playlist_table, smart_playlist_table,
};
use super::columns::{artwork_column, column_fit_width, mapped_row_index_column, text_column};
use super::detail_links::DetailLinks;
use super::grid_cells::{
    CollectionGridCardCell, CollectionGridProjection, ReusableCollectionGridCell,
    collection_grid_cover_shell, collection_grid_with_demand,
};
use super::library_fields::{
    item_at_from_item, playlist_artwork, playlist_field, smart_playlist_field,
};
use super::route::Route;
use super::route_shell::LibraryPageShellOptions;
use super::sparse_model::{SparseRouteModel, connect_sparse_bind};
use super::table_sizing::route_column_view_initial_width;
use crate::LibraryField;
use crate::format_duration_units;
use crate::interactions::install_context_menu_openers;

#[derive(Clone)]
pub(super) struct NamedReadRequest {
    pub(super) query: String,
    pub(super) settings: LibraryListSettings,
}

pub(super) type NamedOrderLoad<K, R> = Arc<
    dyn Fn(
            NamedReadRequest,
        ) -> Pin<Box<dyn Future<Output = Result<(Vec<K>, Vec<R>), String>> + Send>>
        + Send
        + Sync,
>;

impl Shell {
    pub(crate) fn library_genres_route(
        self: &Rc<Self>,
        order: Vec<GenreKey>,
        first_rows: Vec<GenreRow>,
        selected: crate::runtime::SelectedLibrary,
    ) -> MountedRoute {
        let source = selected.source_key;
        let folder = selected.music_folder_key;
        let rows_database = Arc::clone(&selected.database);
        let row_load = Arc::new(
            move |keys: Vec<GenreKey>, cancellation: library::ReadCancellation| {
                let database = Arc::clone(&rows_database);
                Box::pin(async move {
                    database
                        .genre_rows(source, &keys, folder, &cancellation)
                        .await
                        .map_err(|error| error.to_string())
                }) as Pin<Box<dyn Future<Output = _> + Send>>
            },
        );
        let order_database = Arc::clone(&selected.database);
        let order_load: NamedOrderLoad<GenreKey, GenreRow> = Arc::new(move |request| {
            let database = Arc::clone(&order_database);
            Box::pin(async move {
                let cancellation = library::ReadCancellation::new();
                database
                    .genre_route_page(
                        source,
                        folder,
                        &request.query,
                        request.settings.sort_key.genre_sort(),
                        request.settings.descending,
                        &cancellation,
                    )
                    .await
                    .map_err(|error| error.to_string())
            })
        });
        self.named_collection_route(
            Route::Genres,
            LibraryListKey::Genres,
            msgid("Nothing here yet"),
            order,
            first_rows,
            |row: &GenreRow| row.genre_key,
            selected,
            row_load,
            order_load,
        )
    }

    pub(crate) fn library_moods_route(
        self: &Rc<Self>,
        order: Vec<MoodKey>,
        first_rows: Vec<MoodRow>,
        selected: crate::runtime::SelectedLibrary,
    ) -> MountedRoute {
        let source = selected.source_key;
        let folder = selected.music_folder_key;
        let rows_database = Arc::clone(&selected.database);
        let row_load = Arc::new(
            move |keys: Vec<MoodKey>, cancellation: library::ReadCancellation| {
                let database = Arc::clone(&rows_database);
                Box::pin(async move {
                    database
                        .mood_rows(source, &keys, folder, &cancellation)
                        .await
                        .map_err(|error| error.to_string())
                }) as Pin<Box<dyn Future<Output = _> + Send>>
            },
        );
        let order_database = Arc::clone(&selected.database);
        let order_load: NamedOrderLoad<MoodKey, MoodRow> = Arc::new(move |request| {
            let database = Arc::clone(&order_database);
            Box::pin(async move {
                let cancellation = library::ReadCancellation::new();
                database
                    .mood_route_page(
                        source,
                        folder,
                        &request.query,
                        request.settings.sort_key.mood_sort(),
                        request.settings.descending,
                        &cancellation,
                    )
                    .await
                    .map_err(|error| error.to_string())
            })
        });
        self.named_collection_route(
            Route::Moods,
            LibraryListKey::Moods,
            msgid("Nothing here yet"),
            order,
            first_rows,
            |row: &MoodRow| row.mood_key,
            selected,
            row_load,
            order_load,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn named_collection_route<K, R>(
        self: &Rc<Self>,
        route: Route,
        key: LibraryListKey,
        empty_body: &'static str,
        order: Vec<K>,
        first_rows: Vec<R>,
        row_key: impl Fn(&R) -> K + 'static,
        selected: crate::runtime::SelectedLibrary,
        row_load: super::sparse_model::SparseLoad<K, R>,
        order_load: NamedOrderLoad<K, R>,
    ) -> MountedRoute
    where
        K: Clone + Eq + Send + Sync + 'static,
        R: NamedCollectionRow<Key = K> + Send + Sync + 'static,
    {
        let sparse = SparseRouteModel::new(order, 32, selected.runtime.clone(), row_load);
        let row_key = Rc::new(row_key);
        sparse.seed_matching(first_rows, {
            let row_key = Rc::clone(&row_key);
            move |row| row_key(row)
        });
        let content = named_collection_projection::<R>(self, Rc::clone(&sparse), key);
        let search = gtk::SearchEntry::new();
        crate::localization::bind_search_placeholder(&search, "Search");
        let page = self.library_page_shell(LibraryPageShellOptions {
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
            let route = route.clone();
            let row_key = Rc::clone(&row_key);
            Rc::new(
                move |request: NamedReadRequest, result: Result<(Vec<K>, Vec<R>), String>| {
                    let Some(shell) = shell.upgrade() else {
                        return;
                    };
                    if shell.navigation.routes.borrow().current() != &route {
                        return;
                    }
                    match result {
                        Ok((order, first_rows)) => {
                            let row_key = Rc::clone(&row_key);
                            if !sparse.replace_prepared(order, first_rows, move |row| row_key(row))
                            {
                                tracing::warn!("rejected a mismatched prepared named page");
                                return;
                            }
                            content.apply_settings(&request.settings);
                            page.apply_library_list_settings(key, &request.settings);
                            page.set_empty(sparse.len() == 0);
                        }
                        Err(error) => tracing::warn!(%error, "failed to refresh named collection"),
                    }
                },
            )
        };
        let read = LatestMountedRouteRead::new_with_request(
            selected.runtime.clone(),
            apply,
            order_load,
            "mounted named collection",
        );
        {
            let read = Rc::downgrade(&read);
            let shell = Rc::downgrade(self);
            search.connect_search_changed(move |search| {
                let (Some(read), Some(shell)) = (read.upgrade(), shell.upgrade()) else {
                    return;
                };
                read.request_with(NamedReadRequest {
                    query: search.text().trim().to_string(),
                    settings: shell.settings.current.borrow().library_list(key),
                });
            });
        }
        let resume = {
            let shell = Rc::downgrade(self);
            let read = Rc::clone(&read);
            let search = search.clone();
            let content = content.clone();
            let page = page.clone();
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else {
                    return;
                };
                let settings = shell.settings.current.borrow().library_list(key);
                content.apply_settings(&settings);
                page.apply_library_list_settings(key, &settings);
                read.request_with(NamedReadRequest {
                    query: search.text().trim().to_string(),
                    settings,
                });
            })
        };
        MountedRoute::new(page.widget(), resume).with_item_navigation(content.item_navigation())
    }
}

pub(super) trait NamedCollectionRow: Clone + PartialEq + 'static {
    type Key: Clone + Eq + Send + Sync + 'static;

    fn name(&self) -> &str;
    fn route(&self) -> Route;
    fn artwork(&self, prefer_server_playlist_covers: bool) -> Vec<artwork::ArtworkBinding>;
    fn playback(&self) -> super::collections::PlaybackTarget;
    fn mosaic(&self) -> bool;
    fn field(&self, field: LibraryField) -> String;
    fn downloaded(&self) -> bool;
    fn present_context(
        &self,
        target: &gtk::Widget,
        shell: &Rc<Shell>,
        position: Option<(f64, f64)>,
    );
    fn install_extra(
        _widget: &gtk::Widget,
        _shell: &Rc<Shell>,
        _current: Rc<std::cell::RefCell<Option<Self>>>,
    ) where
        Self: Sized,
    {
    }
    fn grid(
        shell: &Rc<Shell>,
        sparse: &Rc<SparseRouteModel<Self::Key, Self>>,
        fields: &[LibraryField],
    ) -> CollectionGridProjection
    where
        Self: Send + Sync,
    {
        named_collection_grid_impl(shell, sparse, fields)
    }
    fn table(
        shell: &Rc<Shell>,
        sparse: &Rc<SparseRouteModel<Self::Key, Self>>,
    ) -> super::collections::CollectionTableProjection;
}

impl NamedCollectionRow for GenreRow {
    type Key = GenreKey;

    fn name(&self) -> &str {
        &self.name
    }
    fn route(&self) -> Route {
        Route::GenreDetail(self.genre_key)
    }
    fn artwork(&self, _: bool) -> Vec<artwork::ArtworkBinding> {
        self.artwork_binding
            .as_ref()
            .or_else(|| self.representative_artwork.first())
            .into_iter()
            .map(|binding| artwork::ArtworkBinding::opaque(binding))
            .collect()
    }
    fn field(&self, field: LibraryField) -> String {
        match field {
            LibraryField::Title | LibraryField::TitleMerged => self.name.clone(),
            LibraryField::AlbumCount => album_count_text(self.album_count.max(0) as u64),
            LibraryField::SongCount => track_count_text(self.track_count.max(0) as u64),
            LibraryField::Duration => {
                format_duration_units((self.duration_millis.max(0) / 1_000) as u32)
            }
            _ => String::new(),
        }
    }
    fn playback(&self) -> super::collections::PlaybackTarget {
        super::collections::PlaybackTarget::Genre(self.genre_key)
    }
    fn mosaic(&self) -> bool {
        false
    }
    fn downloaded(&self) -> bool {
        self.track_count > 0 && self.downloaded_count == self.track_count
    }
    fn present_context(
        &self,
        target: &gtk::Widget,
        shell: &Rc<Shell>,
        position: Option<(f64, f64)>,
    ) {
        present_genre_context_menu(target, shell, self.clone(), None, position);
    }
    fn table(
        shell: &Rc<Shell>,
        sparse: &Rc<SparseRouteModel<Self::Key, Self>>,
    ) -> super::collections::CollectionTableProjection {
        named_collection_table::<Self, _>(shell, sparse.list_model(), LibraryListKey::Genres)
    }
}

impl NamedCollectionRow for MoodRow {
    type Key = MoodKey;

    fn name(&self) -> &str {
        &self.name
    }
    fn route(&self) -> Route {
        Route::MoodDetail(self.mood_key)
    }
    fn artwork(&self, _: bool) -> Vec<artwork::ArtworkBinding> {
        self.representative_artwork
            .iter()
            .map(|binding| artwork::ArtworkBinding::opaque(binding))
            .collect()
    }
    fn field(&self, field: LibraryField) -> String {
        match field {
            LibraryField::Title | LibraryField::TitleMerged => self.name.clone(),
            LibraryField::SongCount => track_count_text(self.track_count.max(0) as u64),
            LibraryField::Duration => {
                format_duration_units((self.duration_millis.max(0) / 1_000) as u32)
            }
            _ => String::new(),
        }
    }
    fn playback(&self) -> super::collections::PlaybackTarget {
        super::collections::PlaybackTarget::Mood(self.mood_key)
    }
    fn mosaic(&self) -> bool {
        true
    }
    fn downloaded(&self) -> bool {
        self.track_count > 0 && self.downloaded_count == self.track_count
    }
    fn present_context(
        &self,
        target: &gtk::Widget,
        shell: &Rc<Shell>,
        position: Option<(f64, f64)>,
    ) {
        present_mood_context_menu(target, shell, self.clone(), None, position);
    }
    fn table(
        shell: &Rc<Shell>,
        sparse: &Rc<SparseRouteModel<Self::Key, Self>>,
    ) -> super::collections::CollectionTableProjection {
        named_collection_table::<Self, _>(shell, sparse.list_model(), LibraryListKey::Moods)
    }
}

impl NamedCollectionRow for PlaylistRow {
    type Key = PlaylistKey;

    fn name(&self) -> &str {
        &self.name
    }
    fn route(&self) -> Route {
        Route::PlaylistDetail(self.playlist_key)
    }
    fn artwork(&self, prefer_server_playlist_covers: bool) -> Vec<artwork::ArtworkBinding> {
        playlist_artwork(self, prefer_server_playlist_covers)
    }
    fn field(&self, field: LibraryField) -> String {
        playlist_field(self, field)
    }
    fn playback(&self) -> super::collections::PlaybackTarget {
        super::collections::PlaybackTarget::Playlist(self.playlist_key)
    }
    fn mosaic(&self) -> bool {
        true
    }
    fn downloaded(&self) -> bool {
        self.track_count > 0 && self.downloaded_count == self.track_count
    }
    fn present_context(
        &self,
        target: &gtk::Widget,
        shell: &Rc<Shell>,
        position: Option<(f64, f64)>,
    ) {
        present_playlist_context_menu(target, shell, self.clone(), None, position);
    }
    fn table(
        shell: &Rc<Shell>,
        sparse: &Rc<SparseRouteModel<Self::Key, Self>>,
    ) -> super::collections::CollectionTableProjection {
        playlist_table(shell, sparse.list_model())
    }
}

impl NamedCollectionRow for SmartPlaylistRow {
    type Key = SmartPlaylistKey;

    fn name(&self) -> &str {
        &self.name
    }
    fn route(&self) -> Route {
        Route::SmartPlaylistDetail(self.smart_playlist_key)
    }
    fn artwork(&self, _: bool) -> Vec<artwork::ArtworkBinding> {
        self.artwork_bindings
            .iter()
            .map(|binding| artwork::ArtworkBinding::opaque(binding))
            .collect()
    }
    fn field(&self, field: LibraryField) -> String {
        smart_playlist_field(self, field)
    }
    fn playback(&self) -> super::collections::PlaybackTarget {
        super::collections::PlaybackTarget::SmartPlaylist(self.smart_playlist_key)
    }
    fn mosaic(&self) -> bool {
        true
    }
    fn downloaded(&self) -> bool {
        false
    }
    fn present_context(
        &self,
        target: &gtk::Widget,
        shell: &Rc<Shell>,
        position: Option<(f64, f64)>,
    ) {
        present_smart_playlist_context_menu(target, shell, self.clone(), None, position);
    }
    fn table(
        shell: &Rc<Shell>,
        sparse: &Rc<SparseRouteModel<Self::Key, Self>>,
    ) -> super::collections::CollectionTableProjection {
        smart_playlist_table(shell, sparse.list_model())
    }
}

fn named_collection_projection<T>(
    shell: &Rc<Shell>,
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
            LibraryLayout::Row => LibraryPresentationProjection::Row(T::table(&shell, &sparse)),
            LibraryLayout::Grid | LibraryLayout::Detail => LibraryPresentationProjection::Grid(
                named_collection_grid::<T>(&shell, &sparse, key),
            ),
        }),
    )
}

fn named_collection_grid<T>(
    shell: &Rc<Shell>,
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
    T::grid(shell, sparse, &fields)
}

fn named_collection_grid_impl<T>(
    shell: &Rc<Shell>,
    sparse: &Rc<SparseRouteModel<T::Key, T>>,
    fields: &[LibraryField],
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
        move |fields| NamedCollectionGridCell::<T>::new(Rc::clone(&cell_shell), fields),
        move |_, row: T| activate_shell.navigate(row.route()),
        move |position| demand.demand_positions([position as usize]),
    )
}

struct NamedCollectionGridCell<T: NamedCollectionRow> {
    body: CollectionGridCardCell,
    shell: Rc<Shell>,
    cover_stack: gtk::Stack,
    single: ArtworkTile,
    quadrants: Vec<ArtworkTile>,
    current: Rc<std::cell::RefCell<Option<T>>>,
}

impl<T: NamedCollectionRow> NamedCollectionGridCell<T> {
    fn new(shell: Rc<Shell>, fields: &[LibraryField]) -> Self {
        let current = Rc::new(std::cell::RefCell::new(None::<T>));
        let overlay = super::cards::elastic_cover_overlay();
        let cover_button = collection_grid_cover_shell();
        let cover_stack = gtk::Stack::new();
        cover_stack.set_hexpand(true);
        cover_stack.set_vexpand(true);
        let single = ArtworkTile::new_elastic_square();
        cover_stack.add_named(&single.widget(), Some("single"));
        let mosaic = gtk::Grid::new();
        mosaic.add_css_class("cover-tile");
        mosaic.add_css_class("card");
        mosaic.set_overflow(gtk::Overflow::Hidden);
        mosaic.set_row_homogeneous(true);
        mosaic.set_column_homogeneous(true);
        mosaic.set_hexpand(true);
        mosaic.set_vexpand(true);
        let quadrants = (0..4)
            .map(|index| {
                let tile = ArtworkTile::new_elastic_square();
                mosaic.attach(&tile.widget(), index % 2, index / 2, 1, 1);
                tile
            })
            .collect::<Vec<_>>();
        cover_stack.add_named(&mosaic, Some("mosaic"));
        cover_button.set_child(Some(&cover_stack));
        let open_shell = Rc::clone(&shell);
        let open_item = Rc::clone(&current);
        cover_button.connect_clicked(move |_| {
            if let Some(item) = open_item.borrow().as_ref() {
                open_shell.navigate(item.route());
            }
        });
        overlay.set_child(Some(&cover_button));

        let controls = super::cards::cover_play_hover_controls(0, msgid("Play"));
        for (button, placement) in [
            (&controls.play, playback::QueuePlacement::Now),
            (&controls.play_next, playback::QueuePlacement::Next),
            (&controls.play_last, playback::QueuePlacement::Last),
        ] {
            let play_shell = Rc::clone(&shell);
            let play_item = Rc::clone(&current);
            button.connect_clicked(move |_| {
                if let Some(item) = play_item.borrow().as_ref() {
                    item.playback().play(
                        &play_shell,
                        placement,
                        placement == playback::QueuePlacement::Now,
                    );
                }
            });
        }
        controls.add_to_overlay(&overlay);
        controls.connect_hover(&overlay);
        let cover = super::cards::square_cover_frame(&overlay, &controls.transport);
        let body = CollectionGridCardCell::new(&shell, fields, cover.upcast());
        body.set_download_badge(shell.download_badge(true));
        let menu_shell = Rc::clone(&shell);
        let menu_item = Rc::clone(&current);
        install_context_menu_openers(
            &body.card,
            Rc::new(move |target, position| {
                if let Some(item) = menu_item.borrow().as_ref() {
                    item.present_context(target, &menu_shell, position);
                }
            }),
        );
        T::install_extra(body.card.upcast_ref(), &shell, Rc::clone(&current));
        Self {
            body,
            shell,
            cover_stack,
            single,
            quadrants,
            current,
        }
    }

    fn clear_artwork(&self) {
        self.shell.clear_artwork_tile(&self.single);
        for tile in &self.quadrants {
            self.shell.clear_artwork_tile(tile);
        }
    }
}

impl<T: NamedCollectionRow> ReusableCollectionGridCell<T> for NamedCollectionGridCell<T> {
    fn widget(&self) -> gtk::Widget {
        self.body.widget()
    }

    fn bind(&self, _: u32, item: T) {
        self.clear_artwork();
        let artwork = item.artwork(
            self.shell
                .settings
                .current
                .borrow()
                .prefer_server_playlist_covers,
        );
        if item.mosaic() && artwork.len() > 1 {
            for (index, tile) in self.quadrants.iter().enumerate() {
                self.shell.bind_artwork_tile(
                    tile,
                    artwork[index % artwork.len()].clone(),
                    super::library_fields::COLLECTION_GRID_MAX_CARD_WIDTH / 2,
                    THUMB_COVER_SIZE,
                );
            }
            self.cover_stack.set_visible_child_name("mosaic");
        } else {
            self.shell.bind_artwork_tile(
                &self.single,
                artwork.into_iter().next().unwrap_or_default(),
                super::library_fields::COLLECTION_GRID_MAX_CARD_WIDTH,
                LARGE_COVER_SIZE,
            );
            self.cover_stack.set_visible_child_name("single");
        }
        self.body
            .bind(item.name(), |field| DetailLinks::text(&item.field(field)));
        self.body
            .set_download_target(&self.shell, item.downloaded());
        self.current.replace(Some(item));
    }

    fn clear(&self) {
        self.clear_artwork();
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
    shell: &Rc<Shell>,
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
            move |field| named_collection_column::<T>(&column_shell, field)
        },
        |field| column_fit_width(field, collection_column_width(field)),
        true,
        Some(Box::new(move |_, item: T| {
            activate_shell.navigate(item.route())
        })),
        None,
        route_column_view_initial_width(shell),
    )
}

fn named_collection_column<T: NamedCollectionRow>(
    shell: &Rc<Shell>,
    field: LibraryField,
) -> gtk::ColumnViewColumn {
    match field {
        LibraryField::RowIndex => mapped_row_index_column::<T>(collection_column_width(field)),
        LibraryField::Image => {
            let prefer_server_playlist_covers = shell
                .settings
                .current
                .borrow()
                .prefer_server_playlist_covers;
            artwork_column::<T, _>(
                shell,
                field.title(),
                collection_column_width(field),
                move |item| {
                    item.artwork(prefer_server_playlist_covers)
                        .into_iter()
                        .next()
                        .unwrap_or_default()
                },
            )
        }
        LibraryField::Title | LibraryField::TitleMerged => {
            let factory = gtk::SignalListItemFactory::new();
            let setup_shell = Rc::clone(shell);
            factory.connect_setup(move |_, item| {
                let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                    return;
                };
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 5);
                row.set_hexpand(true);
                let label = gtk::Label::new(None);
                label.set_xalign(0.0);
                label.set_ellipsize(gtk::pango::EllipsizeMode::End);
                label.set_single_line_mode(true);
                row.append(&label);
                let downloaded = setup_shell.download_badge(true);
                row.append(&downloaded);
                let weak = item.downgrade();
                let shell = Rc::clone(&setup_shell);
                install_context_menu_openers(
                    &row,
                    Rc::new(move |target, position| {
                        let Some(item) = weak.upgrade() else { return };
                        if let Some(value) = item_at_from_item::<T>(&item) {
                            value.present_context(target, &shell, position);
                        }
                    }),
                );
                item.set_child(Some(&row));
            });
            let bind_shell = Rc::clone(shell);
            connect_sparse_bind(&factory, move |item| {
                let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                    return;
                };
                let Some(row) = item_at_from_item::<T>(item) else {
                    return;
                };
                let Some(row_widget) = item
                    .child()
                    .and_then(|child| child.downcast::<gtk::Box>().ok())
                else {
                    return;
                };
                let Some(label) = row_widget
                    .first_child()
                    .and_then(|child| child.downcast::<gtk::Label>().ok())
                else {
                    return;
                };
                let Some(downloaded) = label
                    .next_sibling()
                    .and_then(|child| child.downcast::<gtk::Image>().ok())
                else {
                    return;
                };
                label.set_text(row.name());
                bind_shell.bind_download_badge(&downloaded, row.downloaded());
            });
            factory.connect_unbind(|_, item| {
                let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                    return;
                };
                let Some(row) = item
                    .child()
                    .and_then(|child| child.downcast::<gtk::Box>().ok())
                else {
                    return;
                };
                if let Some(label) = row
                    .first_child()
                    .and_then(|child| child.downcast::<gtk::Label>().ok())
                {
                    label.set_text("");
                    if let Some(downloaded) = label.next_sibling() {
                        downloaded.set_visible(false);
                    }
                }
            });
            gtk::ColumnViewColumn::new(Some(field.title()), Some(factory))
        }
        _ => {
            let column = text_column::<T, _>(field, collection_column_width(field), move |row| {
                row.field(field)
            });
            column
        }
    }
}
