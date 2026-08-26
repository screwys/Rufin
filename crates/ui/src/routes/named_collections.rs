use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::gio;
use library::{GenreKey, GenreRow, MoodKey, MoodRow};
use localization::{album_count_text, msgid, track_count_text};

use crate::shell::Shell;
use crate::shell::route::{LatestMountedRouteRead, MountedRoute};
use crate::{LibraryLayout, LibraryListKey, LibraryListSettings};

use super::collection_context::{present_genre_context_menu, present_mood_context_menu};
use super::collections::{
    LibraryCollectionProjection, LibraryPresentationProjection, PlaybackTarget,
    collection_column_width, dynamic_collection_table,
};
use super::columns::{artwork_column, column_fit_width, mapped_row_index_column, text_column};
use super::detail_links::DetailLinks;
use super::grid_cells::{
    CollectionGridCardCell, CollectionGridProjection, ReusableCollectionGridCell, collection_grid,
    collection_grid_cover_shell,
};
use super::library_fields::item_at_from_item;
use super::route::Route;
use super::route_shell::LibraryPageShellOptions;
use super::sparse_model::SparseRouteModel;
use super::table_sizing::route_column_view_initial_width;
use crate::LibraryField;
use crate::format_duration_units;
use crate::interactions::install_context_menu_openers;
use crate::shell::cover::THUMB_COVER_SIZE;
use playback::QueuePlacement;

#[derive(Clone)]
struct NamedReadRequest {
    query: String,
    settings: LibraryListSettings,
}

type NamedOrderLoad<K> = Arc<
    dyn Fn(NamedReadRequest) -> Pin<Box<dyn Future<Output = Result<Vec<K>, String>> + Send>>
        + Send
        + Sync,
>;

#[derive(Clone, Copy)]
enum NamedCollectionKind {
    Genres,
    Moods,
}

impl Shell {
    pub(crate) fn library_genres_route(
        self: &Rc<Self>,
        order: Vec<GenreKey>,
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
        let order_load: NamedOrderLoad<GenreKey> = Arc::new(move |request| {
            let database = Arc::clone(&order_database);
            Box::pin(async move {
                let cancellation = library::ReadCancellation::new();
                if request.query.is_empty() {
                    database
                        .genre_order(
                            source,
                            folder,
                            request.settings.sort_key.genre_sort(),
                            request.settings.descending,
                            &cancellation,
                        )
                        .await
                } else {
                    database
                        .genre_filter_order(source, folder, &request.query, &cancellation)
                        .await
                }
                .map_err(|error| error.to_string())
            })
        });
        self.named_collection_route(
            Route::Genres,
            LibraryListKey::Genres,
            NamedCollectionKind::Genres,
            order,
            selected,
            row_load,
            order_load,
        )
    }

    pub(crate) fn library_moods_route(
        self: &Rc<Self>,
        order: Vec<MoodKey>,
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
        let order_load: NamedOrderLoad<MoodKey> = Arc::new(move |request| {
            let database = Arc::clone(&order_database);
            Box::pin(async move {
                let cancellation = library::ReadCancellation::new();
                if request.query.is_empty() {
                    database
                        .mood_order(
                            source,
                            folder,
                            request.settings.sort_key.mood_sort(),
                            request.settings.descending,
                            &cancellation,
                        )
                        .await
                } else {
                    database
                        .mood_filter_order(source, folder, &request.query, &cancellation)
                        .await
                }
                .map_err(|error| error.to_string())
            })
        });
        self.named_collection_route(
            Route::Moods,
            LibraryListKey::Moods,
            NamedCollectionKind::Moods,
            order,
            selected,
            row_load,
            order_load,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn named_collection_route<K, R>(
        self: &Rc<Self>,
        route: Route,
        key: LibraryListKey,
        kind: NamedCollectionKind,
        order: Vec<K>,
        selected: crate::runtime::SelectedLibrary,
        row_load: super::sparse_model::SparseLoad<K, R>,
        order_load: NamedOrderLoad<K>,
    ) -> MountedRoute
    where
        K: Clone + Eq + Send + Sync + 'static,
        R: NamedCollectionRow + Send + Sync + 'static,
    {
        let sparse = SparseRouteModel::new(order, 32, selected.runtime.clone(), row_load);
        let model = sparse.list_model();
        let content = named_collection_projection::<R, _>(self, model.clone(), key, kind);
        let search = gtk::SearchEntry::new();
        search.set_placeholder_text(Some("Search"));
        let page_shell = self.library_page_shell(LibraryPageShellOptions {
            key,
            empty: sparse.len() == 0,
            empty_body: msgid("Nothing here yet"),
            search: search.clone(),
            has_visible_results: {
                let sparse = Rc::clone(&sparse);
                Rc::new(move || sparse.len() != 0)
            },
            content: content.scrolling_widget(),
        });
        let apply = {
            let shell = Rc::clone(self);
            let sparse = Rc::clone(&sparse);
            let page_shell = page_shell.clone();
            let content = content.clone();
            let route = route.clone();
            Rc::new(
                move |request: NamedReadRequest, result: Result<Vec<K>, String>| {
                    if shell.navigation.routes.borrow().current() != &route {
                        return;
                    }
                    match result {
                        Ok(order) => {
                            sparse.replace_order(order);
                            content.apply_settings(&request.settings);
                            page_shell.apply_library_list_settings(key, &request.settings);
                            page_shell.set_empty(sparse.len() == 0);
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
            let shell = Rc::clone(self);
            search.connect_search_changed(move |entry| {
                let Some(read) = read.upgrade() else {
                    return;
                };
                read.request_with(NamedReadRequest {
                    query: entry.text().trim().to_string(),
                    settings: shell.settings.current.borrow().library_list(key),
                });
            });
        }
        let resume = {
            let shell = Rc::clone(self);
            let read = Rc::clone(&read);
            let search = search.clone();
            let content = content.clone();
            let page_shell = page_shell.clone();
            Rc::new(move || {
                let settings = shell.settings.current.borrow().library_list(key);
                content.apply_settings(&settings);
                page_shell.apply_library_list_settings(key, &settings);
                read.request_with(NamedReadRequest {
                    query: search.text().trim().to_string(),
                    settings,
                });
            })
        };
        page_shell.mounted_route(resume, content.item_navigation())
    }
}

trait NamedCollectionRow: Clone + 'static {
    fn name(&self) -> &str;
    fn route(&self) -> Route;
    fn artwork(&self) -> Vec<artwork::ArtworkBinding>;
    fn field(&self, field: LibraryField) -> String;
    fn playback(&self) -> PlaybackTarget;
    fn downloaded(&self) -> bool;
    fn present_context(
        &self,
        target: &gtk::Widget,
        shell: &Rc<Shell>,
        position: Option<(f64, f64)>,
    );
}

impl NamedCollectionRow for GenreRow {
    fn name(&self) -> &str {
        &self.name
    }
    fn route(&self) -> Route {
        Route::GenreDetail(self.genre_key)
    }
    fn artwork(&self) -> Vec<artwork::ArtworkBinding> {
        self.artwork_binding
            .iter()
            .chain(&self.representative_artwork)
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
    fn playback(&self) -> PlaybackTarget {
        PlaybackTarget::Genre(self.genre_key)
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
}

impl NamedCollectionRow for MoodRow {
    fn name(&self) -> &str {
        &self.name
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
            LibraryField::Title | LibraryField::TitleMerged => self.name.clone(),
            LibraryField::SongCount => track_count_text(self.track_count.max(0) as u64),
            LibraryField::Duration => {
                format_duration_units((self.duration_millis.max(0) / 1_000) as u32)
            }
            _ => String::new(),
        }
    }
    fn playback(&self) -> PlaybackTarget {
        PlaybackTarget::Mood(self.mood_key)
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
}

fn named_collection_projection<T, M>(
    shell: &Rc<Shell>,
    model: M,
    key: LibraryListKey,
    kind: NamedCollectionKind,
) -> LibraryCollectionProjection
where
    T: NamedCollectionRow,
    M: IsA<gio::ListModel> + Clone + 'static,
{
    let settings = shell.settings.current.borrow().library_list(key);
    let shell = Rc::clone(shell);
    LibraryCollectionProjection::new(
        settings,
        Rc::new(move |layout| match layout {
            LibraryLayout::Row => LibraryPresentationProjection::Row(
                named_collection_table::<T, _>(&shell, model.clone(), key),
            ),
            LibraryLayout::Grid | LibraryLayout::Detail => LibraryPresentationProjection::Grid(
                named_collection_grid::<T, _>(&shell, model.clone(), key, kind),
            ),
        }),
    )
}

fn named_collection_grid<T, M>(
    shell: &Rc<Shell>,
    model: M,
    key: LibraryListKey,
    kind: NamedCollectionKind,
) -> CollectionGridProjection
where
    T: NamedCollectionRow,
    M: IsA<gio::ListModel> + Clone + 'static,
{
    let fields = shell
        .settings
        .current
        .borrow()
        .library_list(key)
        .grid_fields;
    let cell_shell = Rc::clone(shell);
    let activate_shell = Rc::clone(shell);
    collection_grid(
        model,
        &fields,
        move |fields| NamedCollectionGridCell::<T>::new(Rc::clone(&cell_shell), kind, fields),
        move |_, item: T| activate_shell.navigate(item.route()),
    )
}

struct NamedCollectionGridCell<T: NamedCollectionRow> {
    body: CollectionGridCardCell,
    shell: Rc<Shell>,
    cover_button: gtk::Button,
    current: Rc<std::cell::RefCell<Option<T>>>,
}

impl<T: NamedCollectionRow> NamedCollectionGridCell<T> {
    fn new(shell: Rc<Shell>, kind: NamedCollectionKind, fields: &[LibraryField]) -> Self {
        let current = Rc::new(std::cell::RefCell::new(None::<T>));
        let overlay = super::cards::elastic_cover_overlay();
        let cover_button = collection_grid_cover_shell();
        let open_shell = Rc::clone(&shell);
        let open_item = Rc::clone(&current);
        cover_button.connect_clicked(move |_| {
            if let Some(item) = open_item.borrow().as_ref() {
                open_shell.navigate(item.route());
            }
        });
        overlay.set_child(Some(&cover_button));

        let play_label = match kind {
            NamedCollectionKind::Genres => msgid("Play genre"),
            NamedCollectionKind::Moods => msgid("Play mood"),
        };
        let controls = super::cards::cover_play_hover_controls(0, play_label);
        let play_shell = Rc::clone(&shell);
        let play_item = Rc::clone(&current);
        controls.play.connect_clicked(move |_| {
            if let Some(item) = play_item.borrow().as_ref() {
                item.playback().play(&play_shell, QueuePlacement::Now, true);
            }
        });
        let next_shell = Rc::clone(&shell);
        let next_item = Rc::clone(&current);
        controls.play_next.connect_clicked(move |_| {
            if let Some(item) = next_item.borrow().as_ref() {
                item.playback()
                    .play(&next_shell, QueuePlacement::Next, false);
            }
        });
        let last_shell = Rc::clone(&shell);
        let last_item = Rc::clone(&current);
        controls.play_last.connect_clicked(move |_| {
            if let Some(item) = last_item.borrow().as_ref() {
                item.playback()
                    .play(&last_shell, QueuePlacement::Last, false);
            }
        });
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
        Self {
            body,
            shell,
            cover_button,
            current,
        }
    }
}

impl<T: NamedCollectionRow> ReusableCollectionGridCell<T> for NamedCollectionGridCell<T> {
    fn widget(&self) -> gtk::Widget {
        self.body.widget()
    }
    fn bind(&self, _: u32, item: T) {
        self.cover_button.set_child(Some(
            &self
                .shell
                .elastic_cover_group_tile_for_artwork(&item.artwork(), THUMB_COVER_SIZE),
        ));
        self.body
            .bind(item.name(), |field| DetailLinks::text(&item.field(field)));
        self.body
            .set_download_target(&self.shell, item.downloaded());
        self.current.replace(Some(item));
    }
    fn clear(&self) {
        self.cover_button.set_child(None::<&gtk::Widget>);
        self.body.clear(&self.shell);
        self.current.take();
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
        LibraryField::Image => artwork_column::<T, _>(
            shell,
            field.title(),
            collection_column_width(field),
            |item| item.artwork().into_iter().next().unwrap_or_default(),
        ),
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
            factory.connect_bind(move |_, item| {
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
            let column =
                text_column::<T, _>(field.title(), collection_column_width(field), move |row| {
                    row.field(field)
                });
            column
        }
    }
}
