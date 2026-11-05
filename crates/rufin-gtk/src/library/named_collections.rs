use crate::widgets::media_drag::{
    MediaDragSource, install_compact_media_drag_source, install_media_drag_source,
};
use crate::widgets::view_model::item_at_from_item;
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use library::{ArtistRow, Row, TagKind, TagRow};
use localization::msgid;
use rufin_core::collection_context::CollectionTarget;

use crate::library::CatalogUi;
use crate::library::{LibraryLayout, LibraryListKey};
use crate::widgets::artwork::CoverGroupProjection;
use crate::widgets::mounted_route::MountedRoute;
use crate::widgets::view_model::Rows;
use rufin_core::view::Opened;

use super::collections::{
    LibraryCollectionProjection, LibraryPresentationProjection, collection_column_width,
    dynamic_collection_table,
};
use super::columns::{artwork_column, mapped_row_index_column, text_column};
use super::grid_cells::{
    CollectionGridCardCell, CollectionGridProjection, ReusableCollectionGridCell,
    collection_grid_with_demand, install_grid_context, install_grid_play,
};
use crate::menus::media_menus::{
    present_genre_context_menu, present_mood_context_menu, present_smart_playlist_context_menu,
};
use crate::widgets::library_fields::column_fit_width;

use super::route_shell::LibraryPageShellOptions;
use crate::library::LibraryField;
use crate::widgets::interactions::install_context_menu_openers;
use crate::widgets::library_fields::playlist_artwork;
use crate::widgets::recycled_cells::{RecycledBadgedTextCell, list_cell};
use crate::widgets::view_model::connect_row_bind;
use rufin_core::route::Route;
use rufin_core::route::detail_links::DetailLinks;
use rufin_core::settings::presentation::{
    playlist_field, playlist_folder_field, smart_playlist_field, tag_field,
};

impl CatalogUi {
    pub fn library_tags_route(self: &Rc<Self>, read: Opened, genres: bool) -> MountedRoute {
        self.collection_list_route::<TagRow>(
            if genres {
                LibraryListKey::Genres
            } else {
                LibraryListKey::Moods
            },
            msgid("Nothing here yet"),
            false,
            read,
        )
    }

    pub(crate) fn collection_list_route<R: CollectionRow>(
        self: &Rc<Self>,
        key: LibraryListKey,
        empty_body: &'static str,
        favorites_only: bool,
        mut read: Opened,
    ) -> MountedRoute {
        let rows = Rows::over_view(self.products.runtime.clone(), &mut read);
        crate::widgets::media_drag::install_parent_selection(&rows);
        let content = R::projection(self, rows.clone(), key);
        let search = gtk::SearchEntry::new();
        crate::widgets::controls::configure_search_entry(&search);
        crate::widgets::localization::bind_search_placeholder(&search, "Search");
        let mut page = self.library_page_shell(LibraryPageShellOptions {
            key,
            empty: rows.is_empty(),
            empty_body,
            search: search.clone(),
            has_visible_results: {
                let rows = rows.clone();
                Rc::new(move || !rows.is_empty())
            },
            content: content.scrolling_widget(),
        });
        let apply = {
            let shell = Rc::downgrade(self);
            let rows = rows.clone();
            let page = page.clone();
            let content = content.clone();
            move |_| {
                let Some(shell) = shell.upgrade() else {
                    return;
                };
                let settings = shell.settings.current.borrow().library_list(key);
                content.apply_settings(&settings);
                page.apply_library_list_settings(key, &settings);
                page.set_empty(rows.is_empty());
            }
        };
        let reads = crate::library::routes::PageReads::new(
            &rows,
            crate::library::routes::list_inputs(&search),
            apply,
        );
        crate::library::routes::connect_list_search(&search, &reads);
        let resume = {
            let shell = Rc::downgrade(self);
            let content = content.clone();
            let page = page.clone();
            let reads = reads.clone();
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else { return };
                let settings = shell.settings.current.borrow().library_list(key);
                content.apply_settings(&settings);
                page.apply_library_list_settings(key, &settings);
                reads.resume(&shell.settings.current.borrow());
            })
        };
        if favorites_only {
            self.install_favorite_category_tabs(
                rufin_core::route::CollectionCategory::Artists,
                &mut page,
            );
        }
        page.mounted_route(resume)
            .with_catalog_refresh(Rc::new(move || reads.refresh()))
            .with_rows(&rows)
            .with_item_navigation(content.item_navigation())
            .with_initial_demand({
                let rows = rows.clone();
                Rc::new(move || rows.resume_initial_demand())
            })
    }
}

pub trait CollectionRow: library::ShowsState + Clone + PartialEq + Send + Sync + 'static {
    fn playback(&self, key: LibraryListKey) -> Option<rufin_core::playback::PlayRequest>;
    fn downloaded(&self) -> bool;
    fn projection(
        shell: &Rc<CatalogUi>,
        rows: Rows<Row>,
        key: LibraryListKey,
    ) -> LibraryCollectionProjection;
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
        sparse: &Rows<Row>,
        fields: &[LibraryField],
        key: LibraryListKey,
    ) -> CollectionGridProjection
    where
        Self: Send + Sync,
    {
        named_collection_grid_impl::<Self>(shell, sparse, fields, key)
    }
    fn table(
        shell: &Rc<CatalogUi>,
        sparse: &Rows<Row>,
        key: LibraryListKey,
    ) -> super::collections::CollectionTableProjection;
}

impl CollectionRow for ArtistRow {
    fn playback(&self, key: LibraryListKey) -> Option<rufin_core::playback::PlayRequest> {
        Some(crate::widgets::selection::artist_play_request(
            self,
            key == LibraryListKey::AlbumArtists,
        ))
    }
    fn downloaded(&self) -> bool {
        self.summary.downloaded()
    }
    fn projection(
        shell: &Rc<CatalogUi>,
        sparse: Rows<Row>,
        key: LibraryListKey,
    ) -> LibraryCollectionProjection {
        super::collections::artist_collection_projection(shell, sparse, key)
    }
}

impl CollectionRow for TagRow {
    fn playback(&self, _key: LibraryListKey) -> Option<rufin_core::playback::PlayRequest> {
        let (scope, target) = match self.kind {
            TagKind::Genre => {
                let key = library::GenreKey::from_raw(self.key);
                (
                    library::Collection::Genre(key),
                    CollectionTarget::Genre(key),
                )
            }
            TagKind::Mood => {
                let key = library::MoodKey::from_raw(self.key);
                (library::Collection::Mood(key), CollectionTarget::Mood(key))
            }
        };
        Some(crate::widgets::selection::collection_play_request(
            scope, &target,
        ))
    }

    fn downloaded(&self) -> bool {
        self.summary.downloaded()
    }
    fn projection(
        shell: &Rc<CatalogUi>,
        sparse: Rows<Row>,
        key: LibraryListKey,
    ) -> LibraryCollectionProjection {
        named_collection_projection::<Self>(shell, sparse, key)
    }
}

impl NamedCollectionRow for TagRow {
    fn name(&self) -> &str {
        &self.name
    }
    fn route(&self) -> Route {
        match self.kind {
            TagKind::Genre => Route::GenreDetail(library::GenreKey::from_raw(self.key)),
            TagKind::Mood => Route::MoodDetail(library::MoodKey::from_raw(self.key)),
        }
    }
    fn artwork(&self) -> Vec<artwork::ArtworkBinding> {
        self.artwork_binding
            .as_ref()
            .filter(|_| self.kind == TagKind::Genre)
            .map(std::slice::from_ref)
            .unwrap_or(&self.representative_artwork)
            .iter()
            .map(|binding| artwork::ArtworkBinding::opaque(binding))
            .collect()
    }
    fn field(&self, field: LibraryField) -> String {
        tag_field(self, field)
    }

    fn present_context(
        &self,
        target: &gtk::Widget,
        shell: &Rc<CatalogUi>,
        position: Option<(f64, f64)>,
    ) {
        match self.kind {
            TagKind::Genre => {
                present_genre_context_menu(target, shell.shell(), self.clone(), None, position)
            }
            TagKind::Mood => {
                present_mood_context_menu(target, shell.shell(), self.clone(), None, position)
            }
        }
    }
    fn table(
        shell: &Rc<CatalogUi>,
        sparse: &Rows<Row>,
        key: LibraryListKey,
    ) -> super::collections::CollectionTableProjection {
        named_collection_table::<Self, _>(shell, sparse.list_model(), key)
    }
}

impl CollectionRow for library::PlaylistListRow {
    fn playback(&self, _key: LibraryListKey) -> Option<rufin_core::playback::PlayRequest> {
        match self {
            Self::Playlist(row) => Some(crate::widgets::selection::collection_play_request(
                library::Collection::Playlist(row.playlist_key),
                &CollectionTarget::Playlist(row.playlist_key),
            )),
            Self::SmartPlaylist(row) => Some(crate::widgets::selection::smart_play_request(
                row.smart_playlist_key,
            )),
            Self::Folder(_) => None,
        }
    }
    fn downloaded(&self) -> bool {
        library::PlaylistListRow::downloaded(self)
    }
    fn projection(
        shell: &Rc<CatalogUi>,
        sparse: Rows<Row>,
        key: LibraryListKey,
    ) -> LibraryCollectionProjection {
        named_collection_projection::<Self>(shell, sparse, key)
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
            Self::Playlist(row) => playlist_field(row, field),
            Self::SmartPlaylist(row) => smart_playlist_field(row, field),
            Self::Folder(row) => playlist_folder_field(row, field),
        }
    }
    fn route(&self) -> Route {
        match self {
            Self::Playlist(row) => Route::PlaylistDetail(row.playlist_key),
            Self::SmartPlaylist(row) => Route::SmartPlaylistDetail(row.smart_playlist_key),
            Self::Folder(row) => Route::PlaylistFolder {
                key: row.folder_key,
                smart: row.smart,
            },
        }
    }
    fn artwork(&self) -> Vec<artwork::ArtworkBinding> {
        let binding = match self {
            Self::Playlist(row) => return playlist_artwork(row),
            Self::SmartPlaylist(row) => &row.artwork_binding,
            Self::Folder(row) => &row.artwork_binding,
        };
        binding
            .iter()
            .map(|binding| artwork::ArtworkBinding::opaque(binding))
            .collect()
    }
    fn present_context(
        &self,
        target: &gtk::Widget,
        shell: &Rc<CatalogUi>,
        position: Option<(f64, f64)>,
    ) {
        match self {
            Self::Playlist(row) => crate::menus::media_menus::present_playlist_context_menu(
                target,
                shell,
                row.clone(),
                None,
                position,
            ),
            Self::SmartPlaylist(row) => present_smart_playlist_context_menu(
                target,
                shell.shell(),
                row.clone(),
                None,
                position,
            ),
            Self::Folder(row) => crate::menus::playlist_folders::present_folder_context_menu(
                target,
                shell.shell(),
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
        let menus = Rc::downgrade(shell.shell());
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
                .and_then(crate::widgets::media_drag::media_drag_source)
            else {
                return;
            };
            let accepts = match accept_current() {
                Some(library::PlaylistListRow::Folder(folder)) => {
                    crate::menus::playlist_folders::playlist_drag_matches(&source, folder.smart)
                }
                Some(library::PlaylistListRow::Playlist(_)) => {
                    !crate::menus::playlist_folders::single_regular_playlist_drag(&source)
                }
                _ => false,
            };
            if !accepts {
                drop.reject();
            }
        });
        crate::widgets::media_drag::style_order_drop_target(&drop, false, |_| false);
        drop.connect_drop(move |_, value, _, _| {
            let (Some(menus), Some(row), Some(source)) = (
                menus.upgrade(),
                current(),
                crate::widgets::media_drag::media_drag_source(value),
            ) else {
                return false;
            };
            match row {
                library::PlaylistListRow::Playlist(row) => {
                    if crate::menus::playlist_folders::single_regular_playlist_drag(&source) {
                        return false;
                    }
                    crate::menus::media_menus::add_drag_to_playlist(
                        &menus,
                        row.playlist_key,
                        row.name,
                        source,
                    )
                }
                library::PlaylistListRow::Folder(row) => {
                    crate::menus::playlist_folders::drop_playlist_on_folder(
                        &menus,
                        row.folder_key,
                        row.smart,
                        source,
                    )
                }
                library::PlaylistListRow::SmartPlaylist(_) => false,
            }
        });
        widget.add_controller(drop);
    }
    fn table(
        shell: &Rc<CatalogUi>,
        sparse: &Rows<Row>,
        key: LibraryListKey,
    ) -> super::collections::CollectionTableProjection {
        named_collection_table::<Self, _>(shell, sparse.list_model(), key)
    }
}

fn named_collection_projection<T>(
    shell: &Rc<CatalogUi>,
    sparse: Rows<Row>,
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
    sparse: &Rows<Row>,
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
    sparse: &Rows<Row>,
    fields: &[LibraryField],
    key: LibraryListKey,
) -> CollectionGridProjection
where
    T: NamedCollectionRow + Send + Sync,
{
    let cell_shell = Rc::clone(shell);
    let activate_shell = Rc::clone(shell);
    let demand = sparse.clone();
    collection_grid_with_demand(
        sparse.list_model(),
        fields,
        move |fields| NamedCollectionGridCell::<T>::new(Rc::clone(&cell_shell), fields, key),
        move |_, row: T| activate_shell.open_route(row.route()),
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
            crate::widgets::library_fields::COLLECTION_GRID_MAX_CARD_WIDTH,
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
                if let Some(target) = item.playback(key) {
                    crate::menus::media_menus::play_target(
                        &shell.products.playback,
                        &target,
                        placement,
                        false,
                    );
                }
            },
        );
        let body = CollectionGridCardCell::new(&shell, fields, controls.upcast());
        body.set_download_badge(shell.downloads.download_badge(true));
        let drag_current = Rc::clone(&current);
        install_compact_media_drag_source(&body.card, &cover.widget(), move || {
            let item = drag_current.borrow();
            let item = item.as_ref()?;
            let source = MediaDragSource::capture_target(item.playback(key)?);
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
        if let Some(target) = item.playback(self.key) {
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
            .set_download_target(&self.shell, !self.shell.local_source() && item.downloaded());
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
            activate_shell.open_route(item.route())
        })),
        None,
        shell.table_initial_width(0),
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
    let column = match field {
        LibraryField::Tools => named_actions_column::<T>(shell),
        LibraryField::RowIndex => {
            mapped_row_index_column::<T>(shell, named_column_width(key, field), move |row| {
                row.playback(key)
            })
        }
        LibraryField::Image => artwork_column::<T, _>(
            shell,
            rufin_core::settings::presentation::library_field_title(field),
            named_column_width(key, field),
            move |item| item.artwork().into_iter().next().unwrap_or_default(),
            Some(Rc::new(move |item| {
                Some((item.playback(key)?, item.field(LibraryField::Title)))
            })),
        ),
        LibraryField::Title | LibraryField::TitleMerged => {
            let factory = gtk::SignalListItemFactory::new();
            super::columns::bind_collection_title::<T>(&factory, shell, move |row| {
                row.playback(key)
            });
            let setup_shell = Rc::clone(shell);
            factory.connect_setup(move |_, item| {
                let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                    return;
                };
                let cell = RecycledBadgedTextCell::with_downloads(&setup_shell.downloads);
                let drag_item = item.downgrade();
                install_media_drag_source(&cell, move || {
                    let item = drag_item.upgrade()?;
                    let row = item_at_from_item::<T>(&item)?;
                    let source = MediaDragSource::capture_target(row.playback(key)?);
                    Some((source, row.field(LibraryField::Title)))
                });
                item.set_child(Some(&cell));
            });
            let bind_shell = Rc::clone(shell);
            connect_row_bind(&factory, move |item| {
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
                Some(rufin_core::settings::presentation::library_field_title(
                    field,
                )),
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
        let actions = crate::widgets::recycled_cells::RowActions::menu_only();
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
    crate::widgets::recycled_cells::row_actions_column(&factory)
}
