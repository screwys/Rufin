use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};

use adw::prelude::*;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use library::{FolderKey, ReadCancellation};
use localization::{msgid, tr};
use playback::QueuePlacement;

use crate::interactions::install_context_menu_openers;
use crate::{LibraryField, LibraryListKey, shell::Shell, shell::route::MountedRoute};

use super::route::{FolderPathItem, Route};
use super::sparse_model::connect_sparse_bind;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct FolderLink {
    pub(super) object_id: String,
    pub(super) name: String,
}

#[derive(Clone)]
enum FolderTrackSource {
    Live(Arc<[String]>),
    CachedFolder(Option<FolderKey>),
}

pub(crate) struct PreparedFolderRoute {
    folders: Vec<FolderLink>,
    order: Vec<String>,
    source: FolderTrackSource,
    first_tracks: Vec<library::TrackRow>,
}

type FolderResume = Rc<dyn Fn()>;

crate::ui_resource::composite_box!(
    pub(crate) FoldersRouteView,
    folders_route_view_imp,
    "RufinFoldersRouteView",
    "/io/github/screwys/Rufin/ui/routes/folders.ui",
    {
        toolbar_host: gtk::Box,
        breadcrumb_host: gtk::Box,
    }
);

fn folder_table_initial_width(route_width: i32) -> i32 {
    route_width
        .saturating_sub(super::route_layout::PRIMARY_ROUTE_HORIZONTAL_INSET)
        .max(1)
}

impl Shell {
    pub(crate) fn folders_route(
        self: &Rc<Self>,
        path: Vec<FolderPathItem>,
        selected: &crate::runtime::SelectedLibrary,
        prepared: Result<PreparedFolderRoute, String>,
    ) -> MountedRoute {
        let wrapper = FoldersRouteView::new();
        let search = gtk::SearchEntry::new();
        search.set_placeholder_text(Some(&tr("Search current folder")));
        let toolbar = self.library_toolbar_projection_without_detail(
            LibraryListKey::Tracks,
            search.clone(),
            crate::available_sort_fields(LibraryListKey::Tracks),
        );
        toolbar.set_layout_control_visible(false);
        toolbar.set_configure_control_visible(false);
        wrapper.imp().toolbar_host.append(&toolbar.widget());
        wrapper
            .imp()
            .breadcrumb_host
            .append(&folder_path_switcher(self, &path));
        let (resume, download_change) = match prepared {
            Ok(prepared) => {
                let (content, resume, download_change) = folder_page(
                    self,
                    selected,
                    path,
                    search.clone(),
                    prepared.folders,
                    prepared.order,
                    prepared.source,
                    prepared.first_tracks,
                );
                wrapper.append(&content);
                (resume, download_change)
            }
            Err(error) => {
                tracing::warn!(%error, "failed to prepare Folder route");
                wrapper.append(&folder_error_view(self));
                (
                    Rc::new(|| {}) as FolderResume,
                    Rc::new(|_: &downloads::DownloadEvent| {})
                        as crate::shell::route::MountedDownloadChange,
                )
            }
        };
        MountedRoute::new(wrapper.upcast(), resume)
            .with_search(search)
            .with_download_change(download_change)
    }
}

pub(crate) async fn prepare_folder_route(
    selected: &crate::runtime::SelectedLibrary,
    path: &[FolderPathItem],
    settings: &crate::LibraryListSettings,
    cancellation: &ReadCancellation,
) -> Result<PreparedFolderRoute, String> {
    let database = &selected.database;
    let source = selected.source_key;
    let selected_folder = selected.music_folder_key;
    let folder_object_id = path.last().map(|item| item.id.clone());
    let live = (selected.source_id.as_str() != sources::LOCAL_LIBRARY_SOURCE_ID).then(|| {
        selected.operations.folder(
            folder_object_id.clone(),
            selected.music_folder_object_id.clone(),
        )
    });
    if let Some(live) = live {
        match live.recv().await {
            Ok(Ok(page)) => {
                let track_ids = page.tracks;
                let candidates = database
                    .track_media_uris_by_objects(source, &track_ids, cancellation)
                    .await
                    .map_err(|error| error.to_string())?;
                if candidates.len() == track_ids.len() {
                    let order = database
                        .live_folder_track_order(
                            source,
                            &candidates,
                            "",
                            settings.sort_key.track_sort(),
                            settings.descending,
                            cancellation,
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                    let folders: Vec<FolderLink> = page
                        .folders
                        .into_iter()
                        .map(|folder| FolderLink {
                            object_id: folder.object_id,
                            name: folder.name,
                        })
                        .collect();
                    let first_tracks = database
                        .track_rows_by_uri(
                            &order[..order.len().min(64_usize.saturating_sub(folders.len()))],
                            cancellation,
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                    return Ok(PreparedFolderRoute {
                        folders,
                        order,
                        source: FolderTrackSource::Live(candidates.into()),
                        first_tracks,
                    });
                }
                tracing::debug!("live Folder is newer than its accepted cache; using fallback");
            }
            Ok(Err(error)) => {
                tracing::debug!(%error, "live Folder unavailable; using exact cached Folder");
            }
            Err(error) => {
                tracing::debug!(%error, "live Folder ended; using exact cached Folder");
            }
        }
    }
    let exact_folder = exact_cached_folder_scope(
        folder_object_id.is_some(),
        match folder_object_id.as_deref() {
            Some(object_id) => database
                .folder_key_by_object(source, object_id, cancellation)
                .await
                .map_err(|error| error.to_string())?,
            None => None,
        },
        selected_folder,
    )
    .map_err(|error| error.to_string())?;
    let folder_order = database
        .folder_child_order(source, exact_folder, cancellation)
        .await
        .map_err(|error| error.to_string())?;
    let folders: Vec<FolderLink> = database
        .folder_rows(source, &folder_order, cancellation)
        .await
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|folder| FolderLink {
            object_id: folder.object_id,
            name: folder.name,
        })
        .collect();
    let page = database
        .track_route_page(
            source,
            exact_folder,
            false,
            "",
            settings.sort_key.track_sort(),
            settings.descending,
            library::RouteSeedWindow::top(),
            cancellation,
        )
        .await
        .map_err(|error| error.to_string())?;
    let first_tracks = page
        .first_rows
        .into_iter()
        .take(64_usize.saturating_sub(folders.len()))
        .collect();
    Ok(PreparedFolderRoute {
        folders,
        order: page.order,
        source: FolderTrackSource::CachedFolder(exact_folder),
        first_tracks,
    })
}

fn exact_cached_folder_scope(
    nested: bool,
    resolved: Option<FolderKey>,
    root_scope: Option<FolderKey>,
) -> Result<Option<FolderKey>, library::LibraryError> {
    if nested {
        resolved.map(Some).ok_or_else(|| {
            library::LibraryError::InvalidRequest(
                "the exact cached Folder is unavailable".to_string(),
            )
        })
    } else {
        Ok(root_scope)
    }
}

#[derive(Clone)]
#[expect(
    clippy::large_enum_variant,
    reason = "folder table rows are supplied through the bounded sparse-model window"
)]
enum FolderTableRow {
    Folder(FolderLink),
    Track(library::TrackRow),
}

fn folder_page(
    shell: &Rc<Shell>,
    selected: &crate::runtime::SelectedLibrary,
    path: Vec<FolderPathItem>,
    search: gtk::SearchEntry,
    folders: Vec<FolderLink>,
    order: Vec<String>,
    source: FolderTrackSource,
    first_tracks: Vec<library::TrackRow>,
) -> (
    gtk::Widget,
    FolderResume,
    crate::shell::route::MountedDownloadChange,
) {
    search.set_visible(true);
    let route_width = crate::shell::layout::route_content_width(shell);

    let folder_load = Arc::new(move |keys: Vec<FolderLink>, _: ReadCancellation| {
        Box::pin(async move {
            Ok(keys
                .into_iter()
                .map(FolderTableRow::Folder)
                .collect::<Vec<_>>())
        }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
    });
    let folder_sparse = super::sparse_model::SparseRouteModel::new(
        folders,
        48,
        selected.runtime.clone(),
        folder_load,
    );
    let folders = folder_sparse.order();
    folder_sparse.seed(
        folders
            .iter()
            .take(64)
            .cloned()
            .map(FolderTableRow::Folder)
            .collect(),
    );
    let row_database = Arc::clone(&selected.database);
    let track_load = Arc::new(move |keys: Vec<String>, cancellation: ReadCancellation| {
        let database = Arc::clone(&row_database);
        Box::pin(async move {
            let rows = database
                .track_rows_by_uri(&keys, &cancellation)
                .await
                .map_err(|error| error.to_string())?
                .into_iter()
                .map(FolderTableRow::Track)
                .collect::<Vec<_>>();
            Ok(rows)
        }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
    });
    let initial_empty = folders.is_empty() && order.is_empty();
    let track_sparse =
        super::sparse_model::SparseRouteModel::new(order, 48, selected.runtime.clone(), track_load);
    track_sparse.seed_matching(
        first_tracks
            .into_iter()
            .map(FolderTableRow::Track)
            .collect(),
        |row| match row {
            FolderTableRow::Track(track) => track.media_uri.clone(),
            FolderTableRow::Folder(_) => unreachable!(),
        },
    );
    let sections = gtk::gio::ListStore::new::<super::sparse_model::SparseObjectModel>();
    sections.append(&folder_sparse.list_model());
    sections.append(&track_sparse.list_model());
    let rows = gtk::FlattenListModel::new(Some(sections));
    let table_initial_width = folder_table_initial_width(route_width);
    let (table, table_width_fit) = folder_table(
        shell,
        rows,
        Rc::clone(&folder_sparse),
        Rc::clone(&track_sparse),
        path.clone(),
        table_initial_width,
    );

    let table_scroller = gtk::ScrolledWindow::new();
    crate::layout::configure_fill_width_clip(&table_scroller, gtk::PolicyType::Automatic);
    table_scroller.set_hexpand(true);
    table_scroller.set_vexpand(true);
    table_scroller.set_child(Some(&table));
    let table_view = super::route_layout::route_scroller_widget(table_scroller.clone());
    let resize_table_scroller = table_scroller.clone();
    let table_view = crate::layout::width_allocation_owner(&table_view, move |width| {
        table_width_fit.fit_scroller_allocation(&resize_table_scroller, width);
    })
    .upcast::<gtk::Widget>();
    let table_stack = gtk::Stack::new();
    table_stack.set_hexpand(true);
    table_stack.set_vexpand(true);
    table_stack.add_named(&table_view, Some("content"));
    table_stack.add_named(&folder_empty_view(shell), Some("empty"));
    table_stack.set_visible_child_name(if initial_empty { "empty" } else { "content" });

    let generation = Rc::new(Cell::new(0_u64));
    let cancellation = Rc::new(RefCell::new(None::<(u64, ReadCancellation)>));
    let request = {
        let settings_shell = Rc::downgrade(shell);
        let selected = selected.clone();
        let folder_sparse = Rc::clone(&folder_sparse);
        let track_sparse = Rc::clone(&track_sparse);
        let source = source.clone();
        let request_folders = Arc::clone(&folders);
        let generation = Rc::clone(&generation);
        let cancellation = Rc::clone(&cancellation);
        let request_stack = table_stack.clone();
        Rc::new(move |query: String| {
            let settings = settings_shell
                .upgrade()
                .map(|shell| {
                    shell
                        .settings
                        .current
                        .borrow()
                        .library_list(LibraryListKey::Tracks)
                })
                .unwrap_or_else(|| crate::LibraryListSettings::for_key(LibraryListKey::Tracks));
            if let Some((_, previous)) = cancellation.borrow_mut().take() {
                previous.cancel();
            }
            let task_generation = generation.get().wrapping_add(1);
            generation.set(task_generation);
            let read_cancellation = ReadCancellation::new();
            cancellation.replace(Some((task_generation, read_cancellation.clone())));
            let database = Arc::clone(&selected.database);
            let source_key = selected.source_key;
            let source = source.clone();
            let folders = Arc::clone(&request_folders);
            let task = selected.runtime.spawn(async move {
                let tracks = match source {
                    FolderTrackSource::Live(candidates) => {
                        database
                            .live_folder_track_order(
                                source_key,
                                &candidates,
                                &query,
                                settings.sort_key.track_sort(),
                                settings.descending,
                                &read_cancellation,
                            )
                            .await
                    }
                    FolderTrackSource::CachedFolder(folder) => database
                        .track_route_page(
                            source_key,
                            folder,
                            false,
                            &query,
                            settings.sort_key.track_sort(),
                            settings.descending,
                            library::RouteSeedWindow::top(),
                            &read_cancellation,
                        )
                        .await
                        .map(|page| page.order),
                }?;
                let normalized = query.to_lowercase();
                let folders = folders
                    .iter()
                    .filter(|folder| {
                        normalized.is_empty() || folder.name.to_lowercase().contains(&normalized)
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                Ok::<_, library::LibraryError>((folders, tracks))
            });
            let folder_sparse = Rc::clone(&folder_sparse);
            let track_sparse = Rc::clone(&track_sparse);
            let generation = Rc::clone(&generation);
            let cancellation = Rc::clone(&cancellation);
            let stack = request_stack.clone();
            gtk::glib::spawn_future_local(async move {
                if let Some((folders, tracks)) = task.await.ok().and_then(Result::ok)
                    && generation.get() == task_generation
                {
                    stack.set_visible_child_name(if folders.is_empty() && tracks.is_empty() {
                        "empty"
                    } else {
                        "content"
                    });
                    folder_sparse.replace_order(folders);
                    track_sparse.replace_order(tracks);
                }
                if cancellation
                    .borrow()
                    .as_ref()
                    .is_some_and(|(generation, _)| *generation == task_generation)
                {
                    cancellation.borrow_mut().take();
                }
            });
        }) as Rc<dyn Fn(String)>
    };
    let search_request = Rc::clone(&request);
    search.connect_search_changed(move |entry| {
        search_request(entry.text().trim().to_string());
    });
    let resume = Rc::new(move || request(search.text().trim().to_string())) as FolderResume;
    let download_change = Rc::new(move |event: &downloads::DownloadEvent| {
        let downloads::DownloadEvent::Changed {
            media_uri,
            downloaded,
        } = event
        else {
            return;
        };
        let (media_uri, downloaded) = (media_uri.as_str(), *downloaded);
        track_sparse.update_matching(
            |row| {
                matches!(row, FolderTableRow::Track(track)
                if track.media_uri == media_uri && track.is_downloaded != downloaded)
            },
            |row| {
                if let FolderTableRow::Track(track) = row {
                    track.is_downloaded = downloaded;
                }
            },
        );
    });
    (table_stack.upcast(), resume, download_change)
}

fn folder_table(
    shell: &Rc<Shell>,
    rows: gtk::FlattenListModel,
    folders: Rc<super::sparse_model::SparseRouteModel<FolderLink, FolderTableRow>>,
    tracks: Rc<super::sparse_model::SparseRouteModel<String, FolderTableRow>>,
    path: Vec<FolderPathItem>,
    initial_width: i32,
) -> (gtk::ColumnView, super::table_sizing::ColumnViewWidthFit) {
    let selection = gtk::SingleSelection::new(Some(rows));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let table = gtk::ColumnView::new(Some(selection));
    table.add_css_class("folder-table");
    table.add_css_class("folders-table");
    table.add_css_class("data-table");
    table.set_show_column_separators(false);
    table.set_show_row_separators(false);
    table.set_hexpand(true);
    table.set_halign(gtk::Align::Fill);
    table.set_vexpand(true);
    let mut columns = Vec::new();
    if path.is_empty() {
        columns.push(folder_name_column(shell, path.clone()));
        columns.push(folder_detail_column(shell, path.clone()));
    } else {
        columns.push(folder_index_column(Rc::clone(&folders)));
        columns.push(folder_merged_column(shell, path.clone()));
        columns.push(folder_album_column(shell, path.clone()));
        columns.push(folder_year_column(shell, path.clone()));
    }
    let duration = folder_duration_column(shell, path.clone());
    columns.push(duration);
    for column in &columns {
        table.append_column(column);
    }
    let width_fit = super::table_sizing::install_column_view_width_fit(
        &table,
        columns
            .iter()
            .map(|column| (column.clone(), column.fixed_width()))
            .collect(),
        initial_width,
    );
    let activate_shell = Rc::clone(shell);
    let activate_folders = Rc::clone(&folders);
    let activate_tracks = Rc::clone(&tracks);
    table.connect_activate(move |_, position| {
        let folder_count = activate_folders.len();
        let position = position as usize;
        let row = if position < folder_count {
            activate_folders.ready(position as u32)
        } else {
            activate_tracks.ready((position - folder_count) as u32)
        };
        let Some(row) = row else {
            return;
        };
        match row.as_ref() {
            FolderTableRow::Folder(folder) => {
                let mut next = path.clone();
                next.push(FolderPathItem {
                    id: folder.object_id.clone(),
                    name: folder.name.clone(),
                });
                activate_shell.navigate(Route::Folders { path: next });
            }
            FolderTableRow::Track(track) => {
                let anchor = position.saturating_sub(folder_count);
                let order = activate_tracks.order();
                if order.get(anchor) != Some(&track.media_uri) {
                    return;
                }
                activate_shell
                    .products
                    .playback
                    .queue
                    .play(playback::PlayRequest::captured(
                        library::QueueInput::Uris {
                            order,
                            context_id: format!(
                                "{}|result={}",
                                folder_context_id(&path),
                                activate_tracks.order_id()
                            )
                            .into(),
                            source_start: 0,
                        },
                        anchor,
                        QueuePlacement::Now,
                        false,
                    ));
            }
        }
    });
    (table, width_fit)
}

fn folder_name_column(shell: &Rc<Shell>, path: Vec<FolderPathItem>) -> gtk::ColumnViewColumn {
    folder_label_column(shell, path, "Name", 220)
}

fn folder_label_column(
    shell: &Rc<Shell>,
    path: Vec<FolderPathItem>,
    title: &str,
    width: i32,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let setup_shell = Rc::clone(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = super::recycled_cells::RecycledFolderCell::new();
        let weak = item.downgrade();
        let shell = Rc::downgrade(&setup_shell);
        install_context_menu_openers(
            &cell,
            Rc::new(move |target, position| {
                let (Some(item), Some(shell)) = (weak.upgrade(), shell.upgrade()) else {
                    return;
                };
                let Some(FolderTableRow::Track(track)) =
                    super::library_fields::item_at_from_item::<FolderTableRow>(&item)
                else {
                    return;
                };
                super::collection_context::present_track_context_menu(
                    target,
                    &shell,
                    track.media_uri,
                    position,
                );
            }),
        );
        install_folder_cell_activation(&cell, item, &setup_shell, path.clone());
        item.set_child(Some(&cell));
    });
    let bind_shell = Rc::clone(shell);
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(row) = super::library_fields::item_at_from_item::<FolderTableRow>(item) else {
            return;
        };
        let Some(cell) =
            super::recycled_cells::list_cell::<super::recycled_cells::RecycledFolderCell>(item)
        else {
            return;
        };
        let cover = cell.cover();
        match row {
            FolderTableRow::Folder(folder) => {
                bind_shell.clear_artwork_tile(&cover);
                cell.set_cover_visible(false);
                cell.icon().set_visible(true);
                cell.label().set_text(&folder.name);
            }
            FolderTableRow::Track(track) => {
                cell.icon().set_visible(false);
                cell.set_cover_visible(true);
                bind_shell.bind_artwork_tile(
                    &cover,
                    super::library_fields::opaque_artwork(track.artwork_binding.as_deref()),
                    48,
                    crate::shell::cover::THUMB_COVER_SIZE,
                );
                cell.label().set_text(&track.title);
            }
        }
    });
    let clear_shell = Rc::clone(shell);
    factory.connect_unbind(move |_, item| {
        if let Some(cell) = item
            .downcast_ref::<gtk::ListItem>()
            .and_then(super::recycled_cells::list_cell::<super::recycled_cells::RecycledFolderCell>)
        {
            clear_shell.clear_artwork_tile(&cell.cover());
            cell.label().set_text("");
        }
    });
    let column = gtk::ColumnViewColumn::new(Some(&tr(title)), Some(factory));
    configure_folder_column(&column, width);
    column
}

fn folder_index_column(
    folders: Rc<super::sparse_model::SparseRouteModel<FolderLink, FolderTableRow>>,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            item.set_child(Some(&super::columns::track_row_index_cell("")));
        }
    });
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = item
            .child()
            .and_then(|child| child.downcast::<gtk::Overlay>().ok())
        else {
            return;
        };
        let text = match super::library_fields::item_at_from_item::<FolderTableRow>(item) {
            Some(FolderTableRow::Track(_)) => item
                .position()
                .saturating_sub(folders.len().min(u32::MAX as usize) as u32)
                .saturating_add(1)
                .to_string(),
            _ => String::new(),
        };
        super::columns::set_track_row_index_text(&cell, &text);
    });
    factory.connect_unbind(|_, item| {
        if let Some(cell) = item
            .downcast_ref::<gtk::ListItem>()
            .and_then(gtk::ListItem::child)
            .and_then(|child| child.downcast::<gtk::Overlay>().ok())
        {
            super::columns::set_track_row_index_text(&cell, "");
        }
    });
    let column = gtk::ColumnViewColumn::new(Some("#"), Some(factory));
    configure_folder_column(
        &column,
        super::columns::track_column_width(LibraryListKey::Tracks, LibraryField::RowIndex),
    );
    column
}

fn folder_merged_column(shell: &Rc<Shell>, path: Vec<FolderPathItem>) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let setup_shell = Rc::clone(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = super::recycled_cells::RecycledMergedCell::new(&setup_shell, 48, true);
        cell.subtitle().add_css_class("artist-label");
        cell.subtitle().add_css_class("table-link-label");
        let icon = gtk::Image::from_icon_name("rufin-folders-symbolic");
        icon.set_pixel_size(20);
        icon.set_size_request(32, 32);
        icon.set_valign(gtk::Align::Center);
        icon.set_visible(false);
        cell.prepend(&icon);
        let weak = item.downgrade();
        let shell = Rc::downgrade(&setup_shell);
        install_context_menu_openers(
            &cell,
            Rc::new(move |target, position| {
                let (Some(item), Some(shell)) = (weak.upgrade(), shell.upgrade()) else {
                    return;
                };
                let Some(FolderTableRow::Track(track)) =
                    super::library_fields::item_at_from_item::<FolderTableRow>(&item)
                else {
                    return;
                };
                super::collection_context::present_track_context_menu(
                    target,
                    &shell,
                    track.media_uri,
                    position,
                );
            }),
        );
        install_folder_cell_activation(&cell, item, &setup_shell, path.clone());
        item.set_child(Some(&cell));
    });
    let bind_shell = Rc::clone(shell);
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(row) = super::library_fields::item_at_from_item::<FolderTableRow>(item) else {
            return;
        };
        let Some(cell) =
            super::recycled_cells::list_cell::<super::recycled_cells::RecycledMergedCell>(item)
        else {
            return;
        };
        let Some(icon) = cell
            .first_child()
            .and_then(|child| child.downcast::<gtk::Image>().ok())
        else {
            return;
        };
        let cover = cell.cover();
        match row {
            FolderTableRow::Folder(folder) => {
                bind_shell.clear_artwork_tile(&cover);
                cover.widget().set_visible(false);
                icon.set_visible(true);
                cell.title().set_text(&folder.name);
                cell.clear_subtitle();
                if let Some(downloaded) = cell.downloaded() {
                    downloaded.set_visible(false);
                }
            }
            FolderTableRow::Track(track) => {
                icon.set_visible(false);
                cover.widget().set_visible(true);
                bind_shell.bind_artwork_tile(
                    &cover,
                    super::library_fields::opaque_artwork(track.artwork_binding.as_deref()),
                    48,
                    crate::shell::cover::THUMB_COVER_SIZE,
                );
                cell.title().set_text(&track.title);
                cell.bind_subtitle(super::detail_links::track_artist_links(&track));
                cell.subtitle().set_visible(true);
                if let Some(downloaded) = cell.downloaded() {
                    bind_shell.bind_download_badge(&downloaded, track.is_downloaded);
                }
            }
        }
    });
    let clear_shell = Rc::clone(shell);
    factory.connect_unbind(move |_, item| {
        if let Some(cell) = item
            .downcast_ref::<gtk::ListItem>()
            .and_then(super::recycled_cells::list_cell::<super::recycled_cells::RecycledMergedCell>)
        {
            clear_shell.clear_artwork_tile(&cell.cover());
            cell.cover().widget().set_visible(true);
            cell.title().set_text("");
            cell.clear_subtitle();
            if let Some(downloaded) = cell.downloaded() {
                downloaded.set_visible(false);
            }
        }
    });
    let column = gtk::ColumnViewColumn::new(Some(&tr(LibraryField::Title.title())), Some(factory));
    configure_folder_column(
        &column,
        super::columns::track_column_width(LibraryListKey::Tracks, LibraryField::TitleMerged),
    );
    column
}

fn folder_detail_column(shell: &Rc<Shell>, path: Vec<FolderPathItem>) -> gtk::ColumnViewColumn {
    let column = folder_text_column(
        shell,
        path,
        msgid("Artist / Album"),
        18,
        false,
        |row| match row {
            FolderTableRow::Folder(_) => tr("Folder"),
            FolderTableRow::Track(track) => {
                format!("{} / {}", track.artist, track.album)
            }
        },
        |row| match row {
            FolderTableRow::Folder(_) => None,
            FolderTableRow::Track(track) => {
                Some(super::detail_links::track_artist_album_links(track))
            }
        },
    );
    configure_folder_column(&column, 200);
    column
}

fn folder_album_column(shell: &Rc<Shell>, path: Vec<FolderPathItem>) -> gtk::ColumnViewColumn {
    let column = folder_text_column(
        shell,
        path,
        LibraryField::Album.title(),
        24,
        false,
        |row| match row {
            FolderTableRow::Folder(_) => String::new(),
            FolderTableRow::Track(track) => track.album.clone(),
        },
        |row| match row {
            FolderTableRow::Folder(_) => None,
            FolderTableRow::Track(track) => Some(super::detail_links::DetailLinks::route(
                &track.album,
                track.album_media_uri.clone().map(Route::AlbumDetail),
            )),
        },
    );
    configure_folder_column(
        &column,
        super::columns::track_column_width(LibraryListKey::Tracks, LibraryField::Album),
    );
    column
}

fn folder_year_column(shell: &Rc<Shell>, path: Vec<FolderPathItem>) -> gtk::ColumnViewColumn {
    let column = folder_text_column(
        shell,
        path,
        LibraryField::Year.title(),
        8,
        true,
        |row| match row {
            FolderTableRow::Folder(_) => String::new(),
            FolderTableRow::Track(track) => {
                super::library_fields::track_field(track, LibraryField::Year)
            }
        },
        |_| None,
    );
    configure_folder_column(
        &column,
        super::columns::track_column_width(LibraryListKey::Tracks, LibraryField::Year),
    );
    column
}

fn folder_duration_column(shell: &Rc<Shell>, path: Vec<FolderPathItem>) -> gtk::ColumnViewColumn {
    let nested = !path.is_empty();
    let column = folder_text_column(
        shell,
        path,
        super::columns::track_column_title(LibraryField::Duration),
        8,
        true,
        |row| match row {
            FolderTableRow::Folder(_) => String::new(),
            FolderTableRow::Track(track) => {
                crate::format_duration((track.duration_millis.max(0) / 1_000) as u32)
            }
        },
        |_| None,
    );
    configure_folder_column(
        &column,
        if nested {
            super::columns::track_column_width(LibraryListKey::Tracks, LibraryField::Duration)
        } else {
            80
        },
    );
    column
}

fn folder_text_column(
    shell: &Rc<Shell>,
    path: Vec<FolderPathItem>,
    title: &str,
    max_width_chars: i32,
    tabular_numeric: bool,
    value: impl Fn(&FolderTableRow) -> String + 'static,
    links: impl Fn(&FolderTableRow) -> Option<super::detail_links::DetailLinks> + 'static,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let setup_shell = Rc::clone(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = super::recycled_cells::RecycledTextCell::new();
        cell.enable_links(&setup_shell);
        let label = cell.label();
        if tabular_numeric {
            label.add_css_class("tabular-numeric");
        }
        label.set_xalign(0.0);
        label.set_width_request(1);
        label.set_max_width_chars(max_width_chars);
        label.set_hexpand(true);
        label.set_halign(gtk::Align::Fill);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let weak = item.downgrade();
        let shell = Rc::clone(&setup_shell);
        install_context_menu_openers(
            &cell,
            Rc::new(move |target, position| {
                let Some(item) = weak.upgrade() else { return };
                let Some(FolderTableRow::Track(track)) =
                    super::library_fields::item_at_from_item::<FolderTableRow>(&item)
                else {
                    return;
                };
                super::collection_context::present_track_context_menu(
                    target,
                    &shell,
                    track.media_uri,
                    position,
                );
            }),
        );
        install_folder_cell_activation(&cell, item, &setup_shell, path.clone());
        item.set_child(Some(&cell));
    });
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(row) = super::library_fields::item_at_from_item::<FolderTableRow>(item) else {
            return;
        };
        let Some(cell) =
            super::recycled_cells::list_cell::<super::recycled_cells::RecycledTextCell>(item)
        else {
            return;
        };
        let text = value(&row);
        cell.bind_links(
            links(&row).unwrap_or_else(|| super::detail_links::DetailLinks::text(&text)),
        );
    });
    factory.connect_unbind(|_, item| {
        let Some(cell) = item
            .downcast_ref::<gtk::ListItem>()
            .and_then(super::recycled_cells::list_cell::<super::recycled_cells::RecycledTextCell>)
        else {
            return;
        };
        cell.clear();
    });
    gtk::ColumnViewColumn::new(Some(&tr(title)), Some(factory))
}

fn install_folder_cell_activation(
    target: &impl IsA<gtk::Widget>,
    item: &gtk::ListItem,
    shell: &Rc<Shell>,
    path: Vec<FolderPathItem>,
) {
    let item = item.downgrade();
    let shell = Rc::clone(shell);
    let click = gtk::GestureClick::new();
    click.set_button(gtk::gdk::BUTTON_PRIMARY);
    click.connect_released(move |_, presses, _, _| {
        if presses != 1 {
            return;
        }
        let Some(item) = item.upgrade() else { return };
        let Some(FolderTableRow::Folder(folder)) =
            super::library_fields::item_at_from_item::<FolderTableRow>(&item)
        else {
            return;
        };
        let mut next = path.clone();
        next.push(FolderPathItem {
            id: folder.object_id,
            name: folder.name,
        });
        shell.navigate(Route::Folders { path: next });
    });
    target.add_controller(click);
}

fn configure_folder_column(column: &gtk::ColumnViewColumn, width: i32) {
    column.set_fixed_width(width);
    column.set_resizable(false);
}

fn folder_path_switcher(shell: &Rc<Shell>, path: &[FolderPathItem]) -> gtk::Box {
    let switcher = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    switcher.add_css_class("folder-path-switcher");
    switcher.set_hexpand(true);
    switcher.set_halign(gtk::Align::Fill);
    switcher.set_width_request(1);
    let root = folder_path_button(shell, &tr("Folders"), Vec::new(), path.is_empty(), None);
    switcher.append(&root);
    for (index, entry) in path.iter().enumerate() {
        let destination = path.iter().take(index + 1).cloned().collect::<Vec<_>>();
        let separator = gtk::Image::from_icon_name("rufin-go-next-symbolic");
        separator.add_css_class("folder-path-separator");
        separator.set_pixel_size(16);
        separator.set_valign(gtk::Align::Center);
        separator.set_can_target(false);
        switcher.append(&separator);
        switcher.append(&folder_path_button(
            shell,
            &entry.name,
            destination,
            index + 1 == path.len(),
            Some(&root),
        ));
    }
    switcher
}

fn folder_path_button(
    shell: &Rc<Shell>,
    text: &str,
    path: Vec<FolderPathItem>,
    current: bool,
    group: Option<&gtk::ToggleButton>,
) -> gtk::ToggleButton {
    let button = gtk::ToggleButton::new();
    button.add_css_class("flat");
    button.add_css_class("folder-path-segment");
    if let Some(group) = group {
        button.set_group(Some(group));
    }
    button.set_active(current);
    button.set_width_request(1);
    button.set_can_target(!current);
    button.set_focusable(!current);
    let label = gtk::Label::new(Some(text));
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_single_line_mode(true);
    button.set_child(Some(&label));
    button.set_tooltip_text(Some(text));
    let shell = Rc::clone(shell);
    button.connect_clicked(move |_| shell.navigate(Route::Folders { path: path.clone() }));
    button
}

fn folder_context_id(path: &[FolderPathItem]) -> String {
    path.last().map_or_else(
        || "folders:root".to_string(),
        |item| format!("folders:{}", item.id),
    )
}

fn folder_error_view(shell: &Rc<Shell>) -> gtk::Widget {
    folder_status_view(shell, msgid("Couldn't load this folder"), false)
}

fn folder_empty_view(shell: &Rc<Shell>) -> gtk::Widget {
    folder_status_view(shell, msgid("Nothing in this folder"), true)
}

fn folder_status_view(shell: &Rc<Shell>, message: &'static str, empty: bool) -> gtk::Widget {
    let view = gtk::Box::new(gtk::Orientation::Vertical, if empty { 12 } else { 8 });
    if empty {
        view.add_css_class("empty-state");
        view.set_vexpand(true);
        view.set_hexpand(true);
        view.set_valign(gtk::Align::Center);
        view.set_halign(gtk::Align::Center);
        let label = gtk::Label::new(Some(&tr(message)));
        label.add_css_class("muted");
        view.append(&label);
    } else {
        view.append(&shell.route_empty_view(message));
    }
    let refresh = gtk::Button::with_label(&tr(msgid("Refresh")));
    if empty {
        refresh.add_css_class("pill");
    }
    refresh.set_halign(gtk::Align::Center);
    let shell = Rc::clone(shell);
    refresh.connect_clicked(move |_| shell.replace_current_route_when_ready());
    view.append(&refresh);
    view.upcast()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a GTK display"]
    fn folders_route_template_builds() {
        gtk::init().expect("GTK display");
        crate::application::verify_interface_resources().expect("compiled interface resources");
        let view = FoldersRouteView::new();
        assert!(view.imp().toolbar_host.first_child().is_none());
        assert!(view.imp().breadcrumb_host.is_visible());
        assert_eq!(view.imp().toolbar_host.margin_start(), 10);
        assert_eq!(view.imp().toolbar_host.margin_end(), 10);
        assert_eq!(view.imp().breadcrumb_host.margin_start(), 10);
        assert_eq!(view.imp().breadcrumb_host.margin_end(), 10);
    }

    #[test]
    fn missing_nested_folder_never_substitutes_the_root_scope() {
        let root = FolderKey::from_raw(1);
        assert!(exact_cached_folder_scope(true, None, Some(root)).is_err());
        assert_eq!(
            exact_cached_folder_scope(false, None, Some(root)).unwrap(),
            Some(root)
        );
    }

    #[test]
    fn folder_table_uses_the_complete_route_width() {
        assert_eq!(folder_table_initial_width(900), 880);
        assert_eq!(folder_table_initial_width(8), 1);
    }
}
