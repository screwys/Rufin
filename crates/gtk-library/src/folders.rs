use std::{cell::RefCell, rc::Rc, sync::Arc};

use adw::prelude::*;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use library::ReadCancellation;
use localization::{msgid, tr};
use rufin_core::playback::QueuePlacement;
use rufin_core::source::folders::FolderProjection;

use crate::{CatalogUi, LibraryField, LibraryListKey};
use gtk_widgets::interactions::install_context_menu_openers;
use gtk_widgets::mounted_route::{LatestMountedRouteRead, MountedRoute};

use gtk_widgets::route::{FolderPathItem, Route};
use gtk_widgets::sparse_model::{SparseSource, connect_sparse_bind};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FolderLink {
    pub object_id: String,
    pub name: String,
}

pub struct PreparedFolderRoute {
    source: FolderProjection,
    page: FolderPageData,
}

struct FolderPageData {
    query: library::CatalogQuery,
    folder_count: usize,
    track_count: usize,
    first_position: usize,
    first_folders: Vec<FolderLink>,
    first_tracks: Vec<library::TrackView>,
}

type FolderResume = Rc<dyn Fn()>;

gtk_widgets::composite_box!(
    pub FoldersRouteView,
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
        .saturating_sub(crate::route_layout::PRIMARY_ROUTE_HORIZONTAL_INSET)
        .max(1)
}

impl CatalogUi {
    pub fn folders_route(
        self: &Rc<Self>,
        path: Vec<FolderPathItem>,
        selected: &rufin_core::runtime::SelectedLibrary,
        prepared: Result<PreparedFolderRoute, String>,
    ) -> MountedRoute {
        let wrapper = FoldersRouteView::new();
        let search = gtk::SearchEntry::new();
        gtk_widgets::controls::configure_search_entry(&search);
        search.set_placeholder_text(Some(&tr("Search current folder")));
        let toolbar = self.library_toolbar_projection_without_detail(
            LibraryListKey::Folders,
            Some(search.clone()),
            crate::available_sort_fields(LibraryListKey::Folders),
        );
        toolbar.set_layout_control_visible(false);
        wrapper.imp().toolbar_host.append(&toolbar.widget());
        wrapper
            .imp()
            .breadcrumb_host
            .append(&folder_path_switcher(self, &path));
        let (resume, refresh, download_change, initial_demand) = match prepared {
            Ok(prepared) => {
                let (content, resume, refresh, download_change, initial_demand) =
                    folder_page(self, selected, path, search.clone(), prepared);
                wrapper.append(&content);
                (resume, refresh, download_change, initial_demand)
            }
            Err(error) => {
                tracing::warn!(%error, "failed to prepare Folder route");
                wrapper.append(&folder_error_view(self));
                (
                    Rc::new(|| {}) as FolderResume,
                    Rc::new(|| {}) as FolderResume,
                    Rc::new(|_: &downloads::DownloadEvent| {})
                        as gtk_widgets::mounted_route::MountedDownloadChange,
                    Rc::new(|| {}) as FolderResume,
                )
            }
        };
        let shell = Rc::downgrade(self);
        let resume = Rc::new(move || {
            let Some(shell) = shell.upgrade() else { return };
            toolbar.apply(
                LibraryListKey::Folders,
                &shell
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::Folders),
            );
            resume();
        });
        MountedRoute::new(wrapper.upcast(), resume)
            .with_catalog_refresh(refresh)
            .with_search(search)
            .with_download_change(download_change)
            .with_initial_demand(initial_demand)
    }
}

pub async fn prepare_folder_route(
    owner: &rufin_core::source::SourceOwner,
    database: &library::Database,
    selected: &rufin_core::runtime::SelectedLibrary,
    path: &[FolderPathItem],
    settings: &crate::LibraryListSettings,
    window: library::RouteSeedWindow,
    cancellation: &ReadCancellation,
) -> Result<PreparedFolderRoute, String> {
    let source = rufin_core::source::folders::resolve_folder(
        owner,
        database,
        selected,
        path.last().map(|item| item.id.as_str()),
        cancellation,
    )
    .await
    .map_err(|error| error.to_string())?;
    let route = Route::Folders {
        path: path.to_vec(),
    };
    let query = rufin_core::browse::BrowseRequest::new(
        &route,
        rufin_core::route::CollectionCategory::Tracks,
        Some(selected.source_key),
        selected.music_folder_key,
        settings,
        8,
    )
    .resolve(database, Some(&source), cancellation)
    .await
    .map_err(|error| error.to_string())?
    .tracks()
    .expect("folder song route")
    .clone();
    let page = load_folder_page(
        database,
        selected.source_key,
        &source,
        query,
        window,
        cancellation,
    )
    .await
    .map_err(|error| error.to_string())?;
    Ok(PreparedFolderRoute { source, page })
}

async fn load_child_folders(
    database: &library::Database,
    source_key: library::SourceKey,
    source: &FolderProjection,
    query: &library::CatalogQuery,
    range: std::ops::Range<usize>,
    cancellation: &ReadCancellation,
) -> library::LibraryResult<(usize, Vec<FolderLink>)> {
    let (count, rows) = source
        .child_page(database, source_key, query, range, cancellation)
        .await?;
    Ok((
        count,
        rows.into_iter()
            .map(|row| FolderLink {
                object_id: row.object_id,
                name: row.name,
            })
            .collect(),
    ))
}

async fn load_folder_page(
    database: &library::Database,
    source_key: library::SourceKey,
    source: &FolderProjection,
    query: library::CatalogQuery,
    window: library::RouteSeedWindow,
    cancellation: &ReadCancellation,
) -> library::LibraryResult<FolderPageData> {
    let folder_count = load_child_folders(database, source_key, source, &query, 0..0, cancellation)
        .await?
        .0;
    let track_count = (&query).count(&database, cancellation).await? as usize;
    let range = window.range(folder_count + track_count);
    let first_folders = if range.start < folder_count {
        load_child_folders(
            database,
            source_key,
            source,
            &query,
            range.clone(),
            cancellation,
        )
        .await?
        .1
    } else {
        Vec::new()
    };
    let first_tracks = (&query)
        .rows(
            &database,
            range.start.saturating_sub(folder_count),
            range.len().saturating_sub(first_folders.len()),
            cancellation,
        )
        .await
        .map(|rows| match rows {
            library::CatalogRows::Tracks(rows) => rows,
            _ => unreachable!("typed song projection"),
        })?;
    Ok(FolderPageData {
        query,
        folder_count,
        track_count,
        first_position: range.start,
        first_folders,
        first_tracks,
    })
}

#[derive(Clone, PartialEq)]
#[expect(
    clippy::large_enum_variant,
    reason = "folder table rows are supplied through the bounded sparse-model window"
)]
enum FolderTableRow {
    Folder(FolderLink),
    Track(library::TrackView),
}

fn folder_page(
    shell: &Rc<CatalogUi>,
    selected: &rufin_core::runtime::SelectedLibrary,
    path: Vec<FolderPathItem>,
    search: gtk::SearchEntry,
    prepared: PreparedFolderRoute,
) -> (
    gtk::Widget,
    FolderResume,
    FolderResume,
    gtk_widgets::mounted_route::MountedDownloadChange,
    FolderResume,
) {
    search.set_visible(true);
    let route_width = (shell.route_width)();

    let PreparedFolderRoute { source, page } = prepared;
    let FolderPageData {
        query,
        folder_count,
        track_count,
        first_position,
        first_folders,
        first_tracks,
    } = page;
    let applied_query = Rc::new(RefCell::new(query.clone()));
    let database = Arc::clone(&shell.library);
    let source_key = selected.source_key;
    let folder_source = source.clone();
    let folder_query = Rc::clone(&applied_query);
    let folder_load = Rc::new(
        move |_: Vec<FolderLink>, range: std::ops::Range<usize>, cancellation: ReadCancellation| {
            let database = Arc::clone(&database);
            let source = folder_source.clone();
            let query = folder_query.borrow().clone();
            Box::pin(async move {
                Ok(
                    load_child_folders(
                        &database,
                        source_key,
                        &source,
                        &query,
                        range,
                        &cancellation,
                    )
                    .await
                    .map_err(|error| error.to_string())?
                    .1
                    .into_iter()
                    .map(FolderTableRow::Folder)
                    .collect::<Vec<_>>(),
                )
            }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
        },
    );
    let folder_sparse = gtk_widgets::sparse_model::SparseRouteModel::new(
        SparseSource::Query {
            count: folder_count,
        },
        48,
        shell.runtime.clone(),
        folder_load,
    );
    folder_sparse.seed_matching_at(
        first_position.min(folder_count),
        first_folders
            .into_iter()
            .map(FolderTableRow::Folder)
            .collect(),
        |row| match row {
            FolderTableRow::Folder(folder) => folder.clone(),
            _ => unreachable!(),
        },
    );
    let settings = shell
        .settings
        .current
        .borrow()
        .library_list(LibraryListKey::Folders);
    let row_database = Arc::clone(&shell.library);
    let load_input = Rc::clone(&applied_query);
    let track_load = Rc::new(
        move |_: Vec<library::TrackKey>,
              range: std::ops::Range<usize>,
              cancellation: ReadCancellation| {
            let database = Arc::clone(&row_database);
            let query = load_input.borrow().clone();
            Box::pin(async move {
                Ok((&query)
                    .rows(&database, range.start, range.len(), &cancellation)
                    .await
                    .map(|rows| match rows {
                        library::CatalogRows::Tracks(rows) => rows,
                        _ => unreachable!("typed song projection"),
                    })
                    .map_err(|error| error.to_string())?
                    .into_iter()
                    .map(FolderTableRow::Track)
                    .collect::<Vec<_>>())
            }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
        },
    );
    let initial_empty = folder_count + track_count == 0;
    let track_sparse = gtk_widgets::sparse_model::SparseRouteModel::new(
        SparseSource::Query { count: track_count },
        48,
        shell.runtime.clone(),
        track_load,
    );
    track_sparse.seed_matching_at(
        first_position.saturating_sub(folder_count),
        first_tracks
            .into_iter()
            .map(FolderTableRow::Track)
            .collect(),
        |row| match row {
            FolderTableRow::Track(track) => track.track_key,
            _ => unreachable!(),
        },
    );
    let sections = gtk::gio::ListStore::new::<gtk_widgets::sparse_model::SparseObjectModel>();
    sections.append(&folder_sparse.list_model());
    sections.append(&track_sparse.list_model());
    let rows = gtk::FlattenListModel::new(Some(sections));
    let table_initial_width = folder_table_initial_width(route_width);
    let table = folder_table(
        shell,
        rows,
        Rc::clone(&folder_sparse),
        Rc::clone(&track_sparse),
        path.clone(),
        table_initial_width,
        Rc::clone(&applied_query),
        &settings.row_fields,
    );

    let table_scroller = gtk::ScrolledWindow::new();
    gtk_widgets::layout::configure_fill_width_clip(&table_scroller, gtk::PolicyType::Automatic);
    table_scroller.set_hexpand(true);
    table_scroller.set_vexpand(true);
    table_scroller.set_child(Some(&table.widget()));
    let table_view = crate::route_layout::route_scroller_widget(table_scroller.clone());
    let resize_table_scroller = table_scroller.clone();
    let resize_table = table.clone();
    let table_view = gtk_widgets::layout::width_allocation_owner(&table_view, move |width| {
        resize_table.fit_scroller_allocation(&resize_table_scroller, width);
    })
    .upcast::<gtk::Widget>();
    let table_stack = gtk::Stack::new();
    table_stack.set_hexpand(true);
    table_stack.set_vexpand(true);
    table_stack.add_named(&table_view, Some("content"));
    table_stack.add_named(&folder_empty_view(shell), Some("empty"));
    table_stack.set_visible_child_name(if initial_empty { "empty" } else { "content" });

    let apply = {
        let folder_sparse = Rc::clone(&folder_sparse);
        let track_sparse = Rc::clone(&track_sparse);
        let stack = table_stack.clone();
        Rc::new(
            move |_: (library::CatalogQuery, library::RouteSeedWindow),
                  result: library::LibraryResult<FolderPageData>| {
                let Ok(page) = result else { return };
                stack.set_visible_child_name(if page.folder_count + page.track_count == 0 {
                    "empty"
                } else {
                    "content"
                });
                applied_query.replace(page.query);
                folder_sparse.replace_prepared_at(
                    SparseSource::Query {
                        count: page.folder_count,
                    },
                    page.first_position.min(page.folder_count),
                    page.first_folders
                        .into_iter()
                        .map(FolderTableRow::Folder)
                        .collect(),
                    Vec::new(),
                    |row| match row {
                        FolderTableRow::Folder(folder) => folder.clone(),
                        _ => unreachable!(),
                    },
                );
                track_sparse.replace_prepared_at(
                    SparseSource::Query {
                        count: page.track_count,
                    },
                    page.first_position.saturating_sub(page.folder_count),
                    page.first_tracks
                        .into_iter()
                        .map(FolderTableRow::Track)
                        .collect(),
                    Vec::new(),
                    |row| match row {
                        FolderTableRow::Track(track) => track.track_key,
                        _ => unreachable!(),
                    },
                );
            },
        )
    };
    let database = Arc::clone(&shell.library);
    let load = Arc::new(
        move |(query, window): (library::CatalogQuery, library::RouteSeedWindow),
              cancellation: ReadCancellation| {
            let database = Arc::clone(&database);
            let source = source.clone();
            Box::pin(async move {
                load_folder_page(&database, source_key, &source, query, window, &cancellation).await
            }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
        },
    );
    let read = LatestMountedRouteRead::new_with_request(
        shell.runtime.clone(),
        apply,
        load,
        "mounted Folder route",
    );
    let request = {
        let shell = Rc::downgrade(shell);
        let folder_sparse = Rc::clone(&folder_sparse);
        let track_sparse = Rc::clone(&track_sparse);
        let previous = RefCell::new(query);
        let previous_fields = RefCell::new(settings.row_fields);
        Rc::new(move |query: String, refresh: bool| {
            let Some(shell) = shell.upgrade() else { return };
            let settings = shell
                .settings
                .current
                .borrow()
                .library_list(LibraryListKey::Folders);
            let mut request = previous.borrow().clone();
            request.filter = query;
            request.descending = settings.descending;
            if let library::CatalogSubject::Tracks { sort, .. } = &mut request.subject {
                *sort = Some(settings.sort_key.catalog_sort());
            }
            let name_changed = folder_name_field(&previous_fields.borrow())
                != folder_name_field(&settings.row_fields);
            table.apply_fields(&settings.row_fields);
            gtk_widgets::display::apply(&table.widget(), &settings.display);
            if name_changed {
                folder_sparse.update_matching(|_| true, |_| {});
            }
            previous_fields.replace(settings.row_fields);
            let query_changed = *previous.borrow() != request;
            previous.replace(request.clone());
            if refresh || query_changed {
                let position = folder_sparse
                    .first_live_position()
                    .or_else(|| {
                        track_sparse
                            .first_live_position()
                            .map(|position| folder_sparse.len() + position)
                    })
                    .unwrap_or(0);
                let total = folder_sparse.len() + track_sparse.len();
                let window = if query_changed {
                    library::RouteSeedWindow::top()
                } else {
                    library::RouteSeedWindow::relative(
                        position as f64 / total.saturating_sub(1).max(1) as f64,
                    )
                };
                read.request_with((request, window));
            }
        })
    };
    let search_request = Rc::downgrade(&request);
    search.connect_search_changed(move |entry| {
        if let Some(request) = search_request.upgrade() {
            request(entry.text().trim().to_string(), false);
        }
    });
    let refresh = {
        let request = Rc::clone(&request);
        let search = search.clone();
        Rc::new(move || request(search.text().trim().to_string(), true)) as FolderResume
    };
    let resume = Rc::new(move || request(search.text().trim().to_string(), false)) as FolderResume;
    let initial_demand = {
        let tracks = Rc::clone(&track_sparse);
        Rc::new(move || {
            folder_sparse.resume_initial_demand();
            tracks.resume_initial_demand();
        }) as FolderResume
    };
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
                if track.media_uri == media_uri && track.access.is_downloaded != downloaded)
            },
            |row| {
                if let FolderTableRow::Track(track) = row {
                    track.access.is_downloaded = downloaded;
                }
            },
        );
    });
    (
        table_stack.upcast(),
        resume,
        refresh,
        download_change,
        initial_demand,
    )
}

fn folder_table(
    shell: &Rc<CatalogUi>,
    rows: gtk::FlattenListModel,
    folders: Rc<gtk_widgets::sparse_model::SparseRouteModel<FolderLink, FolderTableRow>>,
    tracks: Rc<gtk_widgets::sparse_model::SparseRouteModel<library::TrackKey, FolderTableRow>>,
    path: Vec<FolderPathItem>,
    initial_width: i32,
    applied_query: Rc<RefCell<library::CatalogQuery>>,
    fields: &[LibraryField],
) -> crate::collections::CollectionTableProjection {
    let selection = gtk::SingleSelection::new(Some(rows.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let playing = super::columns::TrackRowPlayingIndicator::new();
    let current_playing = playing.clone();
    shell.register_current_route_track_selection(Rc::new(move |current| {
        current_playing.set_current(
            current.map(|current| current.media_uri.as_str()),
            gtk::INVALID_LIST_POSITION,
        );
        current_playing.set_paused(current.is_some_and(|current| current.paused));
        true
    }));
    let column_shell = Rc::clone(shell);
    let column_path = path.clone();
    let column_folders = Rc::clone(&folders);
    let activate_shell = Rc::clone(shell);
    let activate_folders = Rc::clone(&folders);
    let activate_tracks = Rc::clone(&tracks);
    let activate = move |position: u32, row: FolderTableRow| {
        let folder_count = activate_folders.len();
        let position = position as usize;
        match row {
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
                let input = applied_query.borrow().clone().selection();
                activate_shell.playback.play_selection(
                    rufin_core::playback::PlayRequest::captured(
                        input,
                        anchor,
                        QueuePlacement::Now,
                        false,
                    )
                    .with_context(
                        format!(
                            "{}|result={}",
                            folder_context_id(&path),
                            activate_tracks.order_id()
                        )
                        .into(),
                    )
                    .with_anchor(track.track_key, None),
                );
            }
        }
    };
    let table = crate::collections::dynamic_collection_table(
        shell,
        LibraryListKey::Folders,
        rows,
        fields,
        Vec::new(),
        move |field| {
            folder_column(
                &column_shell,
                column_path.clone(),
                Rc::clone(&column_folders),
                &playing,
                field,
            )
        },
        |field| super::columns::track_column_width(LibraryListKey::Folders, field),
        false,
        Some(Box::new(activate)),
        Some(selection.upcast()),
        initial_width,
    );
    let widget = table.widget();
    widget.add_css_class("folder-table");
    widget.add_css_class("folders-table");
    widget.add_css_class("data-table");
    table
}

fn folder_index_column(
    folders: Rc<gtk_widgets::sparse_model::SparseRouteModel<FolderLink, FolderTableRow>>,
    playing: super::columns::TrackRowPlayingIndicator,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            item.set_child(Some(
                &gtk_widgets::recycled_cells::track_list_row_index_cell(item),
            ));
        }
    });
    let bind_playing = playing.clone();
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
        let text = match gtk_widgets::sparse_model::item_at_from_item::<FolderTableRow>(item) {
            Some(FolderTableRow::Track(track)) => {
                bind_playing.bind(cell.upcast_ref(), item.position(), &track.media_uri);
                item.position()
                    .saturating_sub(folders.len().min(u32::MAX as usize) as u32)
                    .saturating_add(1)
                    .to_string()
            }
            _ => {
                bind_playing.unbind(cell.upcast_ref());
                String::new()
            }
        };
        super::columns::set_track_row_index_text(&cell, &text);
    });
    factory.connect_unbind(move |_, item| {
        if let Some(cell) = item
            .downcast_ref::<gtk::ListItem>()
            .and_then(gtk::ListItem::child)
            .and_then(|child| child.downcast::<gtk::Overlay>().ok())
        {
            super::columns::set_track_row_index_text(&cell, "");
            playing.unbind(cell.upcast_ref());
        }
    });
    let column =
        gtk::ColumnViewColumn::new(Some(super::columns::ROW_INDEX_COLUMN_TITLE), Some(factory));
    configure_folder_column(
        &column,
        super::columns::track_column_width(LibraryListKey::Folders, LibraryField::RowIndex),
    );
    column
}

fn folder_merged_column(shell: &Rc<CatalogUi>, path: Vec<FolderPathItem>) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let setup_shell = Rc::clone(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = gtk_widgets::recycled_cells::RecycledMergedCell::new(
            setup_shell.route_navigation(),
            &setup_shell.downloads,
            48,
            true,
        );
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
                    gtk_widgets::sparse_model::item_at_from_item::<FolderTableRow>(&item)
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
        let Some(row) = gtk_widgets::sparse_model::item_at_from_item::<FolderTableRow>(item) else {
            return;
        };
        let Some(cell) = gtk_widgets::recycled_cells::list_cell::<
            gtk_widgets::recycled_cells::RecycledMergedCell,
        >(item) else {
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
                bind_shell.artwork.clear_artwork_tile(&cover);
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
                bind_shell.artwork.bind_artwork_tile(
                    &cover,
                    gtk_widgets::library_fields::opaque_artwork(track.artwork_binding.as_deref()),
                    48,
                );
                cell.title().set_text(&track.metadata.title);
                cell.bind_subtitle(gtk_widgets::detail_links::track_artist_links(&track));
                cell.subtitle().set_visible(true);
                if let Some(downloaded) = cell.downloaded() {
                    bind_shell.downloads.bind_download_badge(
                        &downloaded,
                        gtk_widgets::downloads::media_download_badge(
                            &track.media_uri,
                            track.access.is_downloaded,
                        ),
                    );
                }
            }
        }
    });
    let clear_shell = Rc::clone(shell);
    factory.connect_unbind(move |_, item| {
        if let Some(cell) =
            item.downcast_ref::<gtk::ListItem>().and_then(
                gtk_widgets::recycled_cells::list_cell::<
                    gtk_widgets::recycled_cells::RecycledMergedCell,
                >,
            )
        {
            clear_shell.artwork.clear_artwork_tile(&cell.cover());
            cell.cover().widget().set_visible(true);
            cell.title().set_text("");
            cell.clear_subtitle();
            if let Some(downloaded) = cell.downloaded() {
                downloaded.set_visible(false);
            }
        }
    });
    let column = gtk::ColumnViewColumn::new(
        Some(&tr(gtk_widgets::settings::library_field_title(
            LibraryField::Title,
        ))),
        Some(factory),
    );
    configure_folder_column(
        &column,
        super::columns::track_column_width(LibraryListKey::Folders, LibraryField::TitleMerged),
    );
    column
}

fn folder_column(
    shell: &Rc<CatalogUi>,
    path: Vec<FolderPathItem>,
    folders: Rc<gtk_widgets::sparse_model::SparseRouteModel<FolderLink, FolderTableRow>>,
    playing: &super::columns::TrackRowPlayingIndicator,
    field: LibraryField,
) -> gtk::ColumnViewColumn {
    use gtk_widgets::library_fields::TrackPresentation;
    let width = super::columns::track_column_width(LibraryListKey::Folders, field);
    match field {
        LibraryField::RowIndex => return folder_index_column(folders, playing.clone()),
        LibraryField::TitleMerged => return folder_merged_column(shell, path),
        LibraryField::Image => {
            return super::columns::mapped_track_image_column::<FolderTableRow, _, _>(
                shell,
                gtk_widgets::settings::library_field_title(field),
                width,
                |row| match row {
                    FolderTableRow::Track(track) => Some(track.media_uri.clone()),
                    FolderTableRow::Folder(_) => None,
                },
                |row| match row {
                    FolderTableRow::Track(track) => gtk_widgets::library_fields::opaque_artwork(
                        track.artwork_binding.as_deref(),
                    ),
                    FolderTableRow::Folder(_) => Default::default(),
                },
            );
        }
        LibraryField::Tools => {
            return super::columns::mapped_track_favorite_column::<FolderTableRow, _, _>(
                shell,
                |row| match row {
                    FolderTableRow::Track(track) => Some(track.media_uri.clone()),
                    FolderTableRow::Folder(_) => None,
                },
                |row| match row {
                    FolderTableRow::Track(track) => {
                        Some((track.media_uri.clone(), track.state.favorite))
                    }
                    FolderTableRow::Folder(_) => None,
                },
            );
        }
        _ => {}
    }
    let value_shell = Rc::clone(shell);
    let column = folder_text_column(
        shell,
        path,
        super::columns::track_column_title(field),
        24,
        !matches!(
            field,
            LibraryField::Title
                | LibraryField::Artist
                | LibraryField::AlbumArtist
                | LibraryField::Album
                | LibraryField::Genre
        ),
        move |row| match row {
            FolderTableRow::Folder(folder) => {
                let fields = value_shell
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::Folders)
                    .row_fields;
                if folder_name_field(&fields) == Some(field) {
                    folder.name.clone()
                } else {
                    String::new()
                }
            }
            FolderTableRow::Track(track) => track.field(field),
        },
        move |row| match row {
            FolderTableRow::Folder(_) => None,
            FolderTableRow::Track(track) => Some(track.links(field)),
        },
    );
    configure_folder_column(&column, width);
    column
}

fn folder_name_field(fields: &[LibraryField]) -> Option<LibraryField> {
    [LibraryField::TitleMerged, LibraryField::Title]
        .into_iter()
        .find(|field| fields.contains(field))
        .or_else(|| {
            fields.iter().copied().find(|field| {
                !matches!(
                    field,
                    LibraryField::RowIndex | LibraryField::Image | LibraryField::Tools
                )
            })
        })
}

fn folder_text_column(
    shell: &Rc<CatalogUi>,
    path: Vec<FolderPathItem>,
    title: &str,
    max_width_chars: i32,
    tabular_numeric: bool,
    value: impl Fn(&FolderTableRow) -> String + 'static,
    links: impl Fn(&FolderTableRow) -> Option<gtk_widgets::detail_links::DetailLinks> + 'static,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let setup_shell = Rc::clone(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = gtk_widgets::recycled_cells::RecycledTextCell::new();
        cell.enable_links(setup_shell.route_navigation());
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
                    gtk_widgets::sparse_model::item_at_from_item::<FolderTableRow>(&item)
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
        let Some(row) = gtk_widgets::sparse_model::item_at_from_item::<FolderTableRow>(item) else {
            return;
        };
        let Some(cell) = gtk_widgets::recycled_cells::list_cell::<
            gtk_widgets::recycled_cells::RecycledTextCell,
        >(item) else {
            return;
        };
        let text = value(&row);
        cell.bind_links(
            links(&row).unwrap_or_else(|| gtk_widgets::detail_links::DetailLinks::text(&text)),
        );
    });
    factory.connect_unbind(|_, item| {
        let Some(cell) = item.downcast_ref::<gtk::ListItem>().and_then(
            gtk_widgets::recycled_cells::list_cell::<gtk_widgets::recycled_cells::RecycledTextCell>,
        ) else {
            return;
        };
        cell.clear();
    });
    gtk::ColumnViewColumn::new(Some(&tr(title)), Some(factory))
}

fn install_folder_cell_activation(
    target: &impl IsA<gtk::Widget>,
    item: &gtk::ListItem,
    shell: &Rc<CatalogUi>,
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
            gtk_widgets::sparse_model::item_at_from_item::<FolderTableRow>(&item)
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

fn folder_path_switcher(shell: &Rc<CatalogUi>, path: &[FolderPathItem]) -> gtk::Box {
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
    shell: &Rc<CatalogUi>,
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

fn folder_error_view(shell: &Rc<CatalogUi>) -> gtk::Widget {
    folder_status_view(shell, msgid("Couldn't load this folder"), false)
}

fn folder_empty_view(shell: &Rc<CatalogUi>) -> gtk::Widget {
    folder_status_view(shell, msgid("Nothing in this folder"), true)
}

fn folder_status_view(shell: &Rc<CatalogUi>, message: &'static str, empty: bool) -> gtk::Widget {
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
        view.append(&crate::route_layout::route_empty_view(message));
    }
    let refresh = gtk::Button::with_label(&tr(msgid("Refresh")));
    if empty {
        refresh.add_css_class("pill");
    }
    refresh.set_halign(gtk::Align::Center);
    let shell = Rc::clone(shell);
    refresh.connect_clicked(move |_| (shell.refresh)());
    view.append(&refresh);
    view.upcast()
}

#[cfg(test)]
mod tests {
    #[test]
    fn folder_keys_use_shared_ready_row_extraction() {
        use gtk::prelude::*;
        use gtk_widgets::sparse_model::{SparseObjectModel, item_at};
        let model = SparseObjectModel::new::<super::FolderLink, String>(
            vec![super::FolderLink {
                object_id: "folder:music".into(),
                name: "Music".into(),
            }],
            64,
        );
        let placeholder = model.item(0).expect("folder placeholder");
        let page = model
            .hydrate::<super::FolderLink, String>(0..1)
            .expect("folder hydration");
        assert!(model.accept::<super::FolderLink, String>(&page, vec!["Music".into()]));
        assert_eq!(model.item(0).as_ref(), Some(&placeholder));
        assert_eq!(item_at::<String>(&model, 0).as_deref(), Some("Music"));
    }
    use super::*;

    #[test]
    fn folder_table_uses_the_complete_route_width() {
        assert_eq!(folder_table_initial_width(900), 880);
        assert_eq!(folder_table_initial_width(8), 1);
    }
}
