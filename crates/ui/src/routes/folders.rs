use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};

use adw::prelude::*;
use library::{FolderKey, ReadCancellation, TrackKey};
use localization::{msgid, tr};
use playback::QueuePlacement;

use crate::interactions::install_context_menu_openers;
use crate::{LibraryListKey, shell::Shell, shell::route::MountedRoute};

use super::route::{FolderPathItem, Route};

#[derive(Clone)]
struct FolderLink {
    object_id: String,
    name: String,
}

#[derive(Clone)]
enum FolderTrackSource {
    LivePage(Arc<[TrackKey]>),
    CachedFolder(Option<FolderKey>),
}

type FolderResume = Rc<dyn Fn()>;

const FOLDER_TREE_MIN_WIDTH: i32 = 132;
const FOLDER_TREE_HIDE_WIDTH: i32 = 550;
const FOLDER_TABLE_MIN_WIDTH: i32 = 240;

fn folder_tree_visible(width: i32) -> bool {
    width >= FOLDER_TREE_HIDE_WIDTH
}

fn folder_tree_position(width: i32, preferred: i32) -> i32 {
    if !folder_tree_visible(width) {
        return 0;
    }
    preferred.clamp(
        FOLDER_TREE_MIN_WIDTH,
        (width - FOLDER_TABLE_MIN_WIDTH).max(FOLDER_TREE_MIN_WIDTH),
    )
}

impl Shell {
    pub(crate) fn folders_route(
        self: &Rc<Self>,
        path: Vec<FolderPathItem>,
        selected: &crate::runtime::SelectedLibrary,
    ) -> MountedRoute {
        let wrapper = gtk::Box::new(gtk::Orientation::Vertical, 12);
        wrapper.add_css_class("route-content");
        wrapper.add_css_class("folders-route");
        wrapper.set_margin_top(super::route_layout::ROUTE_TOP_MARGIN);
        wrapper.set_margin_bottom(28);
        wrapper.set_hexpand(true);
        wrapper.set_width_request(1);
        wrapper.set_vexpand(true);
        if !path.is_empty() {
            wrapper.append(&super::collections::library_route_inset(
                folder_breadcrumbs(self, &path).upcast(),
            ));
        }
        let search = gtk::SearchEntry::new();
        search.add_css_class("folder-search");
        search.set_placeholder_text(Some(&tr("Search current folder")));
        search.set_visible(false);
        self.set_route_search(Some(search.clone()));
        wrapper.append(&super::collections::library_route_inset(
            search.clone().upcast(),
        ));
        let stack = gtk::Stack::new();
        stack.set_hexpand(true);
        stack.set_vexpand(true);
        stack.add_named(
            &self.placeholder_view(msgid("Folders"), msgid("Loading folders...")),
            Some("loading"),
        );
        stack.set_visible_child_name("loading");
        wrapper.append(&stack);
        let resume_slot = Rc::new(RefCell::new(None::<FolderResume>));
        if selected.artwork.source_id.as_str() == sources::LOCAL_LIBRARY_SOURCE_ID {
            load_cached_folder(self, selected, path, &search, &stack, &resume_slot);
            let resume = Rc::new(move || {
                if let Some(resume) = resume_slot.borrow().as_ref() {
                    resume();
                }
            });
            return MountedRoute::new(wrapper.upcast(), resume);
        }
        let receiver = selected.operations.folder(
            path.last().map(|item| item.id.clone()),
            selected.music_folder_object_id.clone(),
        );
        let shell = Rc::downgrade(self);
        let stack_for_load = stack.clone();
        let selected = selected.clone();
        let resume_for_load = Rc::clone(&resume_slot);
        let search_for_load = search.clone();
        gtk::glib::spawn_future_local(async move {
            let Some(shell) = shell.upgrade() else { return };
            match receiver.recv().await {
                Ok(Ok(page)) => {
                    let track_ids = page
                        .tracks
                        .iter()
                        .map(|track| track.object_id.clone())
                        .collect::<Vec<_>>();
                    let database = Arc::clone(&selected.database);
                    let source = selected.source_key;
                    let task = selected.runtime.spawn(async move {
                        database
                            .track_keys_by_objects(source, &track_ids, &ReadCancellation::new())
                            .await
                    });
                    let order = task.await.ok().and_then(Result::ok).unwrap_or_default();
                    let folders = page
                        .folders
                        .into_iter()
                        .map(|folder| FolderLink {
                            object_id: folder.object_id,
                            name: folder.name,
                        })
                        .collect();
                    let (content, resume) = folder_page(
                        &shell,
                        &selected,
                        path,
                        search_for_load.clone(),
                        folders,
                        order.clone(),
                        FolderTrackSource::LivePage(order.into()),
                    );
                    resume_for_load.replace(Some(resume));
                    stack_for_load.add_named(&content, Some("content"));
                    stack_for_load.set_visible_child_name("content");
                }
                Ok(Err(error)) => {
                    tracing::debug!(%error, "live Folder unavailable; using exact cached Folder");
                    load_cached_folder(
                        &shell,
                        &selected,
                        path,
                        &search_for_load,
                        &stack_for_load,
                        &resume_for_load,
                    );
                }
                Err(error) => {
                    tracing::debug!(%error, "live Folder ended; using exact cached Folder");
                    load_cached_folder(
                        &shell,
                        &selected,
                        path,
                        &search_for_load,
                        &stack_for_load,
                        &resume_for_load,
                    );
                }
            }
        });
        let resume = Rc::new(move || {
            if let Some(resume) = resume_slot.borrow().as_ref() {
                resume();
            }
        });
        MountedRoute::new(wrapper.upcast(), resume)
    }
}

fn load_cached_folder(
    shell: &Rc<Shell>,
    selected: &crate::runtime::SelectedLibrary,
    path: Vec<FolderPathItem>,
    search: &gtk::SearchEntry,
    stack: &gtk::Stack,
    resume_slot: &Rc<RefCell<Option<FolderResume>>>,
) {
    let database = Arc::clone(&selected.database);
    let source = selected.source_key;
    let selected_folder = selected.music_folder_key;
    let folder_object_id = path.last().map(|item| item.id.clone());
    let settings = shell
        .settings
        .current
        .borrow()
        .library_list(LibraryListKey::Tracks);
    let task = selected.runtime.spawn(async move {
        let cancellation = ReadCancellation::new();
        let exact_folder = if let Some(object_id) = folder_object_id.as_deref() {
            exact_cached_folder_scope(
                true,
                database
                    .folder_key_by_object(source, object_id, &cancellation)
                    .await?,
                selected_folder,
            )?
        } else {
            exact_cached_folder_scope(false, None, selected_folder)?
        };
        let folder_order = database
            .folder_child_order(source, exact_folder, &cancellation)
            .await?;
        let folders = database
            .folder_rows(source, &folder_order, &cancellation)
            .await?;
        let order = database
            .track_route_order(
                source,
                exact_folder,
                false,
                "",
                settings.sort_key.track_sort(),
                settings.descending,
                &cancellation,
            )
            .await?;
        Ok::<_, library::LibraryError>((exact_folder, folders, order))
    });
    let shell = Rc::downgrade(shell);
    let stack = stack.clone();
    let selected = selected.clone();
    let resume_slot = Rc::clone(resume_slot);
    let search = search.clone();
    gtk::glib::spawn_future_local(async move {
        let Some(shell) = shell.upgrade() else { return };
        let Some((exact_folder, folders, order)) = task.await.ok().and_then(Result::ok) else {
            stack.add_named(&folder_error_view(&shell, path), Some("error"));
            stack.set_visible_child_name("error");
            return;
        };
        let folders = folders
            .into_iter()
            .map(|folder| FolderLink {
                object_id: folder.object_id,
                name: folder.name,
            })
            .collect();
        let (content, resume) = folder_page(
            &shell,
            &selected,
            path,
            search,
            folders,
            order,
            FolderTrackSource::CachedFolder(exact_folder),
        );
        resume_slot.replace(Some(resume));
        stack.add_named(&content, Some("cached"));
        stack.set_visible_child_name("cached");
    });
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

#[derive(Clone, Debug, Eq, PartialEq)]
enum FolderRouteKey {
    Folder(usize),
    Track(TrackKey),
}

#[derive(Clone)]
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
    order: Vec<TrackKey>,
    source: FolderTrackSource,
) -> (gtk::Widget, FolderResume) {
    const TREE_DEFAULT: i32 = 260;
    search.set_visible(true);
    let saved_tree_width = shell
        .settings
        .current
        .borrow()
        .folder_view
        .tree_width
        .unwrap_or(TREE_DEFAULT);

    let folders = Arc::new(folders);
    let row_database = Arc::clone(&selected.database);
    let source_key = selected.source_key;
    let folder_rows = Arc::clone(&folders);
    let load = Arc::new(
        move |keys: Vec<FolderRouteKey>, cancellation: ReadCancellation| {
            let database = Arc::clone(&row_database);
            let folders = Arc::clone(&folder_rows);
            Box::pin(async move {
                let track_keys = keys
                    .iter()
                    .filter_map(|key| match key {
                        FolderRouteKey::Track(key) => Some(*key),
                        FolderRouteKey::Folder(_) => None,
                    })
                    .collect::<Vec<_>>();
                let mut tracks = database
                    .track_rows(source_key, &track_keys, &cancellation)
                    .await
                    .map_err(|error| error.to_string())?
                    .into_iter()
                    .map(|row| (row.track_key, row))
                    .collect::<std::collections::BTreeMap<_, _>>();
                keys.into_iter()
                    .map(|key| match key {
                        FolderRouteKey::Folder(index) => folders
                            .get(index)
                            .cloned()
                            .map(FolderTableRow::Folder)
                            .ok_or_else(|| "Folder row is no longer current".to_string()),
                        FolderRouteKey::Track(key) => tracks
                            .remove(&key)
                            .map(FolderTableRow::Track)
                            .ok_or_else(|| "Folder Track is no longer current".to_string()),
                    })
                    .collect::<Result<Vec<_>, _>>()
            }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
        },
    );
    let initial_empty = folders.is_empty() && order.is_empty();
    let keys = (0..folders.len())
        .map(FolderRouteKey::Folder)
        .chain(order.iter().copied().map(FolderRouteKey::Track))
        .collect();
    let sparse =
        super::sparse_model::SparseRouteModel::new(keys, 48, selected.runtime.clone(), load);
    let (table, table_width_fit) =
        folder_table(shell, Rc::clone(&sparse), selected.clone(), path.clone());

    let tree = gtk::ListBox::new();
    tree.add_css_class("folder-tree");
    tree.set_selection_mode(gtk::SelectionMode::None);
    tree.append(&folder_tree_navigation_row(
        shell,
        "Folders",
        Vec::new(),
        0,
        path.is_empty(),
        true,
    ));
    for (index, item) in path.iter().enumerate() {
        tree.append(&folder_tree_navigation_row(
            shell,
            &item.name,
            path.iter().take(index + 1).cloned().collect(),
            index + 1,
            index + 1 == path.len(),
            false,
        ));
    }
    for folder in folders.iter() {
        tree.append(&folder_tree_row(shell, &path, folder, path.len() + 1));
    }
    let tree_scroller = gtk::ScrolledWindow::new();
    tree_scroller.add_css_class("folders-tree-pane");
    tree_scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    tree_scroller.set_min_content_width(FOLDER_TREE_MIN_WIDTH);
    tree_scroller.set_width_request(saved_tree_width);
    tree_scroller.set_vexpand(true);
    tree_scroller.set_child(Some(&tree));

    let table_scroller = gtk::ScrolledWindow::new();
    crate::layout::configure_fill_width_clip(&table_scroller, gtk::PolicyType::Automatic);
    table_scroller.set_hexpand(true);
    table_scroller.set_vexpand(true);
    table_scroller.set_child(Some(&super::collections::library_route_inset(
        table.upcast(),
    )));
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
    table_stack.add_named(&folder_empty_view(shell, &path), Some("empty"));
    table_stack.set_visible_child_name(if initial_empty { "empty" } else { "content" });

    let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
    paned.add_css_class("folders-split");
    paned.set_position(saved_tree_width);
    paned.set_wide_handle(false);
    paned.set_hexpand(true);
    paned.set_vexpand(true);
    paned.set_start_child(Some(&tree_scroller));
    paned.set_resize_start_child(false);
    paned.set_shrink_start_child(false);
    paned.set_end_child(Some(&table_stack));
    paned.set_resize_end_child(true);
    paned.set_shrink_end_child(true);
    let persist_shell = Rc::clone(shell);
    paned.connect_position_notify(move |paned| {
        if !paned.start_child().is_some_and(|child| child.is_visible()) {
            return;
        }
        let width = paned.position().max(FOLDER_TREE_MIN_WIDTH);
        persist_shell.update_app_settings("Folder tree width", move |settings| {
            if settings.folder_view.tree_width == Some(width) {
                return false;
            }
            settings.folder_view.tree_width = Some(width);
            true
        });
    });
    let tree_visibility = tree_scroller.clone();
    let tree_paned = paned.clone();
    let visible_tree_width = Rc::new(Cell::new(saved_tree_width));
    let allocation_tree_width = Rc::clone(&visible_tree_width);
    let paned_owner = crate::layout::width_allocation_owner(&paned, move |width| {
        let visible = folder_tree_visible(width);
        if !visible && tree_visibility.is_visible() {
            allocation_tree_width.set(tree_paned.position().max(FOLDER_TREE_MIN_WIDTH));
        }
        tree_visibility.set_visible(visible);
        if visible {
            let position = if tree_paned.position() <= 0 {
                allocation_tree_width.get()
            } else {
                tree_paned.position()
            };
            tree_paned.set_position(folder_tree_position(width, position));
        } else {
            tree_paned.set_position(0);
        }
    });

    let generation = Rc::new(Cell::new(0_u64));
    let cancellation = Rc::new(RefCell::new(None::<(u64, ReadCancellation)>));
    let request = {
        let settings_shell = Rc::downgrade(shell);
        let selected = selected.clone();
        let sparse = Rc::clone(&sparse);
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
                    FolderTrackSource::LivePage(candidates) => {
                        database
                            .track_subset_order(
                                source_key,
                                &candidates,
                                &query,
                                settings.sort_key.track_sort(),
                                settings.descending,
                                &read_cancellation,
                            )
                            .await
                    }
                    FolderTrackSource::CachedFolder(folder) => {
                        database
                            .track_route_order(
                                source_key,
                                folder,
                                false,
                                &query,
                                settings.sort_key.track_sort(),
                                settings.descending,
                                &read_cancellation,
                            )
                            .await
                    }
                }?;
                let normalized = query.to_lowercase();
                let folder_keys = folders
                    .iter()
                    .enumerate()
                    .filter(|(_, folder)| {
                        normalized.is_empty() || folder.name.to_lowercase().contains(&normalized)
                    })
                    .map(|(index, _)| FolderRouteKey::Folder(index));
                Ok::<_, library::LibraryError>(
                    folder_keys
                        .chain(tracks.into_iter().map(FolderRouteKey::Track))
                        .collect::<Vec<_>>(),
                )
            });
            let sparse = Rc::clone(&sparse);
            let generation = Rc::clone(&generation);
            let cancellation = Rc::clone(&cancellation);
            let stack = request_stack.clone();
            gtk::glib::spawn_future_local(async move {
                if let Some(keys) = task.await.ok().and_then(Result::ok)
                    && generation.get() == task_generation
                {
                    stack.set_visible_child_name(if keys.is_empty() { "empty" } else { "content" });
                    sparse.replace_order(keys);
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
    (paned_owner.upcast(), resume)
}

fn folder_table(
    shell: &Rc<Shell>,
    sparse: Rc<super::sparse_model::SparseRouteModel<FolderRouteKey, FolderTableRow>>,
    selected: crate::runtime::SelectedLibrary,
    path: Vec<FolderPathItem>,
) -> (gtk::ColumnView, super::table_sizing::ColumnViewWidthFit) {
    let selection = gtk::SingleSelection::new(Some(sparse.list_model()));
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
    let folder_settings = shell.settings.current.borrow().folder_view.clone();
    let name = folder_name_column_restored(shell);
    configure_folder_column(
        shell,
        &name,
        folder_settings.name_column_width.unwrap_or(220),
    );
    table.append_column(&name);
    let detail = folder_detail_column(shell);
    configure_folder_column(
        shell,
        &detail,
        folder_settings.detail_column_width.unwrap_or(200),
    );
    table.append_column(&detail);
    let duration = folder_duration_column(shell);
    configure_folder_column(
        shell,
        &duration,
        folder_settings.duration_column_width.unwrap_or(80),
    );
    table.append_column(&duration);
    let width_fit = super::table_sizing::install_column_view_width_fit(
        &table,
        vec![
            (name.clone(), name.fixed_width()),
            (detail.clone(), detail.fixed_width()),
            (duration.clone(), duration.fixed_width()),
        ],
        super::table_sizing::route_column_view_initial_width(shell),
    );
    let save_shell = Rc::clone(shell);
    super::table_sizing::connect_column_width_save(&table, &width_fit, move |widths| {
        let [name, detail, duration] = widths.as_slice() else {
            return;
        };
        let (name, detail, duration) = (*name, *detail, *duration);
        save_shell.update_app_settings("Folder table column widths", move |settings| {
            let next = (Some(name), Some(detail), Some(duration));
            let current = (
                settings.folder_view.name_column_width,
                settings.folder_view.detail_column_width,
                settings.folder_view.duration_column_width,
            );
            if current == next {
                return false;
            }
            settings.folder_view.name_column_width = next.0;
            settings.folder_view.detail_column_width = next.1;
            settings.folder_view.duration_column_width = next.2;
            true
        });
    });

    let activate_shell = Rc::clone(shell);
    let activate_sparse = Rc::clone(&sparse);
    table.connect_activate(move |_, position| {
        let Some(row) = activate_sparse.ready(position) else {
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
                let order = activate_sparse
                    .order()
                    .iter()
                    .filter_map(|key| match key {
                        FolderRouteKey::Track(key) => Some(*key),
                        FolderRouteKey::Folder(_) => None,
                    })
                    .collect::<Vec<_>>();
                let Some(anchor) = order.iter().position(|key| *key == track.track_key) else {
                    return;
                };
                let media = playback::PlaybackMedia::from(track.clone());
                if let Some(request) = playback::LoadedPlayRequest::context(
                    selected.source_key,
                    selected.source_session_epoch,
                    order.into(),
                    media,
                    anchor,
                    QueuePlacement::Now,
                    folder_context_id(&path),
                    false,
                ) {
                    activate_shell.products.playback.queue.play_loaded(request);
                }
            }
        }
    });
    (table, width_fit)
}

fn folder_name_column_restored(shell: &Rc<Shell>) -> gtk::ColumnViewColumn {
    #[derive(Clone)]
    struct Cell {
        icon: gtk::Image,
        cover: crate::shell::cover::ArtworkTile,
        label: gtk::Label,
    }
    let factory = gtk::SignalListItemFactory::new();
    let cells = super::factory_cells::FactoryCells::<Cell>::new();
    let setup_shell = Rc::clone(shell);
    let setup_cells = cells.clone();
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.add_css_class("folder-table-row");
        let icon = gtk::Image::from_icon_name("rufin-folders-symbolic");
        icon.set_pixel_size(28);
        row.append(&icon);
        let cover = crate::shell::cover::ArtworkTile::new(28);
        cover.widget().set_visible(false);
        row.append(&cover.widget());
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        label.set_hexpand(true);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        row.append(&label);
        let weak = item.downgrade();
        let shell = Rc::clone(&setup_shell);
        install_context_menu_openers(
            &row,
            Rc::new(move |target, position| {
                let Some(item) = weak.upgrade() else { return };
                let Some(FolderTableRow::Track(track)) =
                    super::library_fields::item_at_from_item::<FolderTableRow>(&item)
                else {
                    return;
                };
                super::collection_context::present_track_context_menu(
                    target, &shell, track, position,
                );
            }),
        );
        item.set_child(Some(&row));
        setup_cells.insert(item, Cell { icon, cover, label });
    });
    let bind_shell = Rc::clone(shell);
    let bind_cells = cells.clone();
    factory.connect_bind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(row) = super::library_fields::item_at_from_item::<FolderTableRow>(item) else {
            return;
        };
        let Some(cell) = bind_cells.get(item) else {
            return;
        };
        match row {
            FolderTableRow::Folder(folder) => {
                bind_shell.clear_artwork_tile(&cell.cover);
                cell.cover.widget().set_visible(false);
                cell.icon.set_visible(true);
                cell.label.set_text(&folder.name);
            }
            FolderTableRow::Track(track) => {
                cell.icon.set_visible(false);
                cell.cover.widget().set_visible(true);
                bind_shell.bind_artwork_tile(
                    &cell.cover,
                    super::library_fields::opaque_artwork(track.artwork_binding.as_deref()),
                    28,
                    crate::shell::cover::THUMB_COVER_SIZE,
                );
                cell.label.set_text(&track.title);
            }
        }
    });
    let clear_shell = Rc::clone(shell);
    let clear_cells = cells.clone();
    factory.connect_unbind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(cell) = clear_cells.get(item) {
            clear_shell.clear_artwork_tile(&cell.cover);
            cell.label.set_text("");
        }
    });
    let teardown_cells = cells.clone();
    factory.connect_teardown(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            teardown_cells.remove(item);
        }
    });
    gtk::ColumnViewColumn::new(Some(&tr("Name")), Some(factory))
}

fn folder_detail_column(shell: &Rc<Shell>) -> gtk::ColumnViewColumn {
    folder_text_column(shell, "Artist / Album", 18, |row| match row {
        FolderTableRow::Folder(_) => tr("Folder"),
        FolderTableRow::Track(track) => {
            format!("{} / {}", track.display_artist, track.display_album)
        }
    })
}

fn folder_duration_column(shell: &Rc<Shell>) -> gtk::ColumnViewColumn {
    folder_text_column(shell, "Duration", 8, |row| match row {
        FolderTableRow::Folder(_) => String::new(),
        FolderTableRow::Track(track) => {
            crate::format_duration((track.duration_millis.max(0) / 1_000) as u32)
        }
    })
}

fn folder_text_column(
    shell: &Rc<Shell>,
    title: &str,
    max_width_chars: i32,
    value: impl Fn(FolderTableRow) -> String + 'static,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let setup_shell = Rc::clone(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        label.set_width_request(1);
        label.set_max_width_chars(max_width_chars);
        label.set_hexpand(true);
        label.set_halign(gtk::Align::Fill);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let weak = item.downgrade();
        let shell = Rc::clone(&setup_shell);
        install_context_menu_openers(
            &label,
            Rc::new(move |target, position| {
                let Some(item) = weak.upgrade() else { return };
                let Some(FolderTableRow::Track(track)) =
                    super::library_fields::item_at_from_item::<FolderTableRow>(&item)
                else {
                    return;
                };
                super::collection_context::present_track_context_menu(
                    target, &shell, track, position,
                );
            }),
        );
        item.set_child(Some(&label));
    });
    factory.connect_bind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(row) = super::library_fields::item_at_from_item::<FolderTableRow>(item) else {
            return;
        };
        if let Some(label) = item
            .child()
            .and_then(|child| child.downcast::<gtk::Label>().ok())
        {
            label.set_text(&value(row));
        }
    });
    factory.connect_unbind(|_, item| {
        let Some(label) = item
            .downcast_ref::<gtk::ListItem>()
            .and_then(gtk::ListItem::child)
            .and_then(|child| child.downcast::<gtk::Label>().ok())
        else {
            return;
        };
        label.set_text("");
        label.set_tooltip_text(None);
    });
    gtk::ColumnViewColumn::new(Some(&tr(title)), Some(factory))
}

fn configure_folder_column(_: &Rc<Shell>, column: &gtk::ColumnViewColumn, width: i32) {
    column.set_fixed_width(width);
    column.set_resizable(true);
}

fn folder_tree_row(
    shell: &Rc<Shell>,
    path: &[FolderPathItem],
    folder: &FolderLink,
    depth: usize,
) -> gtk::Widget {
    let button = gtk::Button::new();
    button.add_css_class("flat");
    button.add_css_class("folder-tree-row");
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_margin_start(i32::try_from(depth).unwrap_or(i32::MAX).saturating_mul(12));
    row.append(&gtk::Image::from_icon_name("rufin-folders-symbolic"));
    let label = gtk::Label::new(Some(&folder.name));
    label.set_xalign(0.0);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    row.append(&label);
    button.set_child(Some(&row));
    let mut next = path.to_vec();
    next.push(FolderPathItem {
        id: folder.object_id.clone(),
        name: folder.name.clone(),
    });
    let shell = Rc::clone(shell);
    button.connect_clicked(move |_| shell.navigate(Route::Folders { path: next.clone() }));
    button.upcast()
}

fn folder_tree_navigation_row(
    shell: &Rc<Shell>,
    name: &str,
    path: Vec<FolderPathItem>,
    depth: usize,
    current: bool,
    localized: bool,
) -> gtk::Widget {
    let button = gtk::Button::new();
    button.add_css_class("flat");
    button.add_css_class("folder-tree-row");
    if current {
        button.add_css_class("active");
    }
    button.set_sensitive(!current);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_margin_start(i32::try_from(depth).unwrap_or(i32::MAX).saturating_mul(12));
    row.append(&gtk::Image::from_icon_name("rufin-folders-symbolic"));
    let text = if localized {
        tr(name)
    } else {
        name.to_string()
    };
    let label = gtk::Label::new(Some(&text));
    label.set_xalign(0.0);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    row.append(&label);
    button.set_child(Some(&row));
    let shell = Rc::clone(shell);
    button.connect_clicked(move |_| shell.navigate(Route::Folders { path: path.clone() }));
    button.upcast()
}

fn folder_breadcrumbs(shell: &Rc<Shell>, path: &[FolderPathItem]) -> gtk::Box {
    let breadcrumbs = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    breadcrumbs.add_css_class("folder-breadcrumbs");
    breadcrumbs.set_hexpand(true);
    breadcrumbs.set_width_request(1);
    breadcrumbs.append(&breadcrumb_button(
        shell,
        "Folders",
        Vec::new(),
        path.is_empty(),
        true,
    ));
    for (index, entry) in path.iter().enumerate() {
        breadcrumbs.append(&gtk::Label::new(Some("/")));
        breadcrumbs.append(&breadcrumb_button(
            shell,
            &entry.name,
            path.iter().take(index + 1).cloned().collect(),
            index + 1 == path.len(),
            false,
        ));
    }
    breadcrumbs
}

fn breadcrumb_button(
    shell: &Rc<Shell>,
    label: &str,
    path: Vec<FolderPathItem>,
    current: bool,
    localized: bool,
) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("flat");
    button.add_css_class("folder-breadcrumb");
    button.set_width_request(1);
    button.set_sensitive(!current);
    let text = if localized {
        tr(label)
    } else {
        label.to_string()
    };
    let label_widget = gtk::Label::new(Some(&text));
    label_widget.set_xalign(0.0);
    label_widget.set_ellipsize(gtk::pango::EllipsizeMode::End);
    button.set_child(Some(&label_widget));
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

fn folder_error_view(shell: &Rc<Shell>, path: Vec<FolderPathItem>) -> gtk::Widget {
    let view = gtk::Box::new(gtk::Orientation::Vertical, 8);
    view.append(&shell.route_empty_view(msgid("Couldn't load this folder")));
    let retry = gtk::Button::with_label(&tr(msgid("Refresh")));
    retry.set_halign(gtk::Align::Center);
    let shell = Rc::clone(shell);
    retry.connect_clicked(move |_| shell.navigate(Route::Folders { path: path.clone() }));
    view.append(&retry);
    view.upcast()
}

fn folder_empty_view(shell: &Rc<Shell>, path: &[FolderPathItem]) -> gtk::Widget {
    let view = gtk::Box::new(gtk::Orientation::Vertical, 12);
    view.add_css_class("empty-state");
    view.set_vexpand(true);
    view.set_hexpand(true);
    view.set_valign(gtk::Align::Center);
    view.set_halign(gtk::Align::Center);
    let empty = gtk::Label::new(Some(&tr(msgid("Nothing in this folder"))));
    empty.add_css_class("muted");
    view.append(&empty);
    let refresh = gtk::Button::with_label(&tr(msgid("Refresh")));
    refresh.add_css_class("pill");
    refresh.set_halign(gtk::Align::Center);
    let shell = Rc::clone(shell);
    let path = path.to_vec();
    refresh.connect_clicked(move |_| shell.navigate(Route::Folders { path: path.clone() }));
    view.append(&refresh);
    view.upcast()
}

#[cfg(test)]
mod tests {
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
    fn folders_hide_tree_at_tiny_width() {
        assert!(!folder_tree_visible(FOLDER_TREE_HIDE_WIDTH - 1));
        assert_eq!(folder_tree_position(FOLDER_TREE_HIDE_WIDTH - 1, 260), 0);
        assert!(folder_tree_visible(FOLDER_TREE_HIDE_WIDTH));
    }

    #[test]
    fn folder_tree_position_keeps_saved_width_inside_real_bounds() {
        assert_eq!(folder_tree_position(900, 10), FOLDER_TREE_MIN_WIDTH);
        assert_eq!(folder_tree_position(900, 400), 400);
        assert_eq!(folder_tree_position(600, 900), 360);
    }
}
