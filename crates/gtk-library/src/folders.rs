use std::{cell::RefCell, rc::Rc, sync::Arc};

use adw::prelude::*;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use library::{FolderKey, ReadCancellation};
use localization::{msgid, tr};
use playback::QueuePlacement;

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

#[derive(Clone)]
enum FolderTrackSource {
    Live(Arc<[String]>),
    CachedFolder(Option<FolderKey>),
}

pub struct PreparedFolderRoute {
    folders: Vec<FolderLink>,
    order: SparseSource<String>,
    source: FolderTrackSource,
    first_tracks: Vec<library::TrackRow>,
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
        let (resume, refresh, download_change) = match prepared {
            Ok(prepared) => {
                let (content, resume, refresh, download_change) = folder_page(
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
                (resume, refresh, download_change)
            }
            Err(error) => {
                tracing::warn!(%error, "failed to prepare Folder route");
                wrapper.append(&folder_error_view(self));
                (
                    Rc::new(|| {}) as FolderResume,
                    Rc::new(|| {}) as FolderResume,
                    Rc::new(|_: &downloads::DownloadEvent| {})
                        as gtk_widgets::mounted_route::MountedDownloadChange,
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
    }
}

pub async fn prepare_folder_route(
    selected: &rufin_core::runtime::SelectedLibrary,
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
                    let mut folders: Vec<FolderLink> = page
                        .folders
                        .into_iter()
                        .map(|folder| FolderLink {
                            object_id: folder.object_id,
                            name: folder.name,
                        })
                        .collect();
                    folders.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
                    if settings.descending {
                        folders.reverse();
                    }
                    let first_tracks = database
                        .track_rows_by_uri(
                            &order[..order.len().min(64_usize.saturating_sub(folders.len()))],
                            cancellation,
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                    return Ok(PreparedFolderRoute {
                        folders,
                        order: order.into(),
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
    let mut folders: Vec<FolderLink> = database
        .folder_rows(source, &folder_order, cancellation)
        .await
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|folder| FolderLink {
            object_id: folder.object_id,
            name: folder.name,
        })
        .collect();
    if settings.descending {
        folders.reverse();
    }
    let page = database
        .query_track_route_page(
            &library::TrackQuery {
                source: source,
                collection: None,
                folder: exact_folder,
                favorites_only: false,
            },
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
        order: SparseSource::Query { count: page.count },
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

#[derive(Clone, PartialEq)]
#[expect(
    clippy::large_enum_variant,
    reason = "folder table rows are supplied through the bounded sparse-model window"
)]
enum FolderTableRow {
    Folder(FolderLink),
    Track(library::TrackRow),
}

fn folder_page(
    shell: &Rc<CatalogUi>,
    selected: &rufin_core::runtime::SelectedLibrary,
    path: Vec<FolderPathItem>,
    search: gtk::SearchEntry,
    folders: Vec<FolderLink>,
    order: SparseSource<String>,
    source: FolderTrackSource,
    first_tracks: Vec<library::TrackRow>,
) -> (
    gtk::Widget,
    FolderResume,
    FolderResume,
    gtk_widgets::mounted_route::MountedDownloadChange,
) {
    search.set_visible(true);
    let route_width = (shell.route_width)();

    let folder_load = Rc::new(
        move |keys: Vec<FolderLink>, _: std::ops::Range<usize>, _: ReadCancellation| {
            Box::pin(async move {
                Ok(keys
                    .into_iter()
                    .map(FolderTableRow::Folder)
                    .collect::<Vec<_>>())
            }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
        },
    );
    let folder_sparse = gtk_widgets::sparse_model::SparseRouteModel::new(
        folders,
        48,
        selected.runtime.clone(),
        folder_load,
    );
    let folders = folder_sparse.order().keys().expect("folder keys").clone();
    folder_sparse.seed(
        folders
            .iter()
            .take(64)
            .cloned()
            .map(FolderTableRow::Folder)
            .collect(),
    );
    let settings = shell
        .settings
        .current
        .borrow()
        .library_list(LibraryListKey::Folders);
    let queue_input = Rc::new(RefCell::new(match &source {
        FolderTrackSource::Live(_) => None,
        FolderTrackSource::CachedFolder(folder) => Some(library::QueueInput::Query {
            query: library::QueueQuery::Tracks {
                source: selected.source_key,
                favorites_only: false,
                recursive: false,
            },
            folder: *folder,
            filter: String::new(),
            sort: settings.sort_key.track_sort(),
            descending: settings.descending,
            context_id: folder_context_id(&path).into(),
            anchor_uri: None,
        }),
    }));
    let row_database = Arc::clone(&selected.database);
    let load_input = Rc::clone(&queue_input);
    let track_load = Rc::new(
        move |keys: Vec<String>, range: std::ops::Range<usize>, cancellation: ReadCancellation| {
            let database = Arc::clone(&row_database);
            let input = load_input.borrow().clone();
            Box::pin(async move {
                let rows = match input {
                    Some(library::QueueInput::Query {
                        query:
                            library::QueueQuery::Tracks {
                                source,
                                favorites_only,
                                ..
                            },
                        folder,
                        filter,
                        sort,
                        descending,
                        ..
                    }) => {
                        database
                            .track_page(
                                source,
                                folder,
                                favorites_only,
                                &filter,
                                sort,
                                descending,
                                range.start,
                                range.len(),
                                &cancellation,
                            )
                            .await
                    }
                    _ => database.track_rows_by_uri(&keys, &cancellation).await,
                }
                .map_err(|error| error.to_string())?
                .into_iter()
                .map(FolderTableRow::Track)
                .collect::<Vec<_>>();
                Ok(rows)
            }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
        },
    );
    let initial_empty = folders.is_empty() && order.is_empty();
    let track_sparse = gtk_widgets::sparse_model::SparseRouteModel::new(
        order,
        48,
        selected.runtime.clone(),
        track_load,
    );
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
        Rc::clone(&queue_input),
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
            move |request: crate::track_model::TrackProjectionRequest,
                  result: library::LibraryResult<(
                Vec<FolderLink>,
                SparseSource<String>,
                Vec<library::TrackRow>,
            )>| {
                let Ok((folders, tracks, first_rows)) = result else {
                    return;
                };
                stack.set_visible_child_name(if folders.is_empty() && tracks.is_empty() {
                    "empty"
                } else {
                    "content"
                });
                if let Some(library::QueueInput::Query {
                    filter,
                    sort,
                    descending,
                    ..
                }) = &mut *queue_input.borrow_mut()
                {
                    *filter = request.query;
                    *sort = request.settings.sort_key.track_sort();
                    *descending = request.settings.descending;
                }
                folder_sparse.replace_order(folders);
                track_sparse.replace_prepared(
                    tracks,
                    first_rows.into_iter().map(FolderTableRow::Track).collect(),
                    |row| match row {
                        FolderTableRow::Track(track) => track.media_uri.clone(),
                        FolderTableRow::Folder(_) => unreachable!(),
                    },
                );
            },
        )
    };
    let database = Arc::clone(&selected.database);
    let source_key = selected.source_key;
    let mut canonical_folders = folders.to_vec();
    canonical_folders.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    let canonical_folders: Arc<[FolderLink]> = canonical_folders.into();
    let load = Arc::new(
        move |request: crate::track_model::TrackProjectionRequest,
              cancellation: ReadCancellation| {
            let database = Arc::clone(&database);
            let source = source.clone();
            let folders = Arc::clone(&canonical_folders);
            Box::pin(async move {
                let (tracks, first_rows) = match source {
                    FolderTrackSource::Live(candidates) => (
                        database
                            .live_folder_track_order(
                                source_key,
                                &candidates,
                                &request.query,
                                request.settings.sort_key.track_sort(),
                                request.settings.descending,
                                &cancellation,
                            )
                            .await?
                            .into(),
                        Vec::new(),
                    ),
                    FolderTrackSource::CachedFolder(folder) => {
                        let page = database
                            .query_track_route_page(
                                &library::TrackQuery {
                                    source: source_key,
                                    collection: None,
                                    folder: folder,
                                    favorites_only: false,
                                },
                                &request.query,
                                request.settings.sort_key.track_sort(),
                                request.settings.descending,
                                library::RouteSeedWindow::top(),
                                &cancellation,
                            )
                            .await?;
                        (SparseSource::Query { count: page.count }, page.first_rows)
                    }
                };
                let normalized = request.query.to_lowercase();
                let mut folders: Vec<FolderLink> = folders
                    .iter()
                    .filter(|folder| {
                        normalized.is_empty() || folder.name.to_lowercase().contains(&normalized)
                    })
                    .cloned()
                    .collect();
                if request.settings.descending {
                    folders.reverse();
                }
                Ok((folders, tracks, first_rows))
            }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
        },
    );
    let read = LatestMountedRouteRead::new_with_request(
        selected.runtime.clone(),
        apply,
        load,
        "mounted Folder route",
    );
    let request = {
        let shell = Rc::downgrade(shell);
        let folder_sparse = Rc::clone(&folder_sparse);
        let previous = RefCell::new(crate::track_model::TrackProjectionRequest {
            query: String::new(),
            settings,
        });
        Rc::new(move |query: String, refresh: bool| {
            let Some(shell) = shell.upgrade() else { return };
            let request = crate::track_model::TrackProjectionRequest {
                query,
                settings: shell
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::Folders),
            };
            let name_changed = folder_name_field(&previous.borrow().settings.row_fields)
                != folder_name_field(&request.settings.row_fields);
            table.apply_fields(&request.settings.row_fields);
            if name_changed {
                folder_sparse.update_matching(|_| true, |_| {});
            }
            let query_changed = !previous.borrow().same_query(&request);
            previous.replace(request.clone());
            if refresh || query_changed {
                read.request_with(request);
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
    (table_stack.upcast(), resume, refresh, download_change)
}

fn folder_table(
    shell: &Rc<CatalogUi>,
    rows: gtk::FlattenListModel,
    folders: Rc<gtk_widgets::sparse_model::SparseRouteModel<FolderLink, FolderTableRow>>,
    tracks: Rc<gtk_widgets::sparse_model::SparseRouteModel<String, FolderTableRow>>,
    path: Vec<FolderPathItem>,
    initial_width: i32,
    queue_input: Rc<RefCell<Option<library::QueueInput>>>,
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
                let mut input =
                    queue_input
                        .borrow()
                        .clone()
                        .unwrap_or_else(|| library::QueueInput::Uris {
                            order: activate_tracks
                                .order()
                                .keys()
                                .expect("live folder track keys")
                                .clone(),
                            context_id: format!(
                                "{}|result={}",
                                folder_context_id(&path),
                                activate_tracks.order_id()
                            )
                            .into(),
                            source_start: 0,
                        });
                if let library::QueueInput::Query {
                    anchor_uri,
                    context_id,
                    ..
                } = &mut input
                {
                    *anchor_uri = Some(track.media_uri.clone());
                    *context_id = format!(
                        "{}|result={}",
                        folder_context_id(&path),
                        activate_tracks.order_id()
                    )
                    .into();
                }
                activate_shell.queue.play(playback::PlayRequest::captured(
                    input,
                    anchor,
                    QueuePlacement::Now,
                    false,
                ));
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
                    gtk_widgets::artwork::THUMB_COVER_SIZE,
                );
                cell.title().set_text(&track.title);
                cell.bind_subtitle(gtk_widgets::detail_links::track_artist_links(&track));
                cell.subtitle().set_visible(true);
                if let Some(downloaded) = cell.downloaded() {
                    bind_shell.downloads.bind_download_badge(
                        &downloaded,
                        gtk_widgets::downloads::media_download_badge(
                            &track.media_uri,
                            track.is_downloaded,
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
                    FolderTableRow::Track(track) => Some((track.media_uri.clone(), track.favorite)),
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
