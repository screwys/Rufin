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
use super::sparse_model::connect_sparse_bind;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct FolderLink {
    pub(super) object_id: String,
    pub(super) name: String,
}

#[derive(Clone)]
enum FolderTrackSource {
    Live(Arc<[TrackKey]>),
    CachedFolder(Option<FolderKey>),
}

type FolderResume = Rc<dyn Fn()>;

const FOLDER_TREE_MIN_WIDTH: i32 = 132;
const FOLDER_TREE_MAX_WIDTH: i32 = 480;
const FOLDER_TREE_HIDE_WIDTH: i32 = 550;
const FOLDER_TABLE_MIN_WIDTH: i32 = 240;
const FOLDER_SPLIT_SEPARATOR_WIDTH: i32 = 17;
const FOLDER_TREE_WIDTH: i32 = 260;

fn folder_tree_visible(width: i32) -> bool {
    width >= FOLDER_TREE_HIDE_WIDTH
}

fn folder_tree_position(width: i32, preferred: i32) -> i32 {
    if !folder_tree_visible(width) {
        return 0;
    }
    preferred.clamp(
        FOLDER_TREE_MIN_WIDTH,
        width
            .saturating_sub(FOLDER_SPLIT_SEPARATOR_WIDTH)
            .saturating_sub(FOLDER_TABLE_MIN_WIDTH)
            .clamp(FOLDER_TREE_MIN_WIDTH, FOLDER_TREE_MAX_WIDTH),
    )
}

fn folder_tree_width(width: i32) -> i32 {
    if width < 760 {
        (width / 3).clamp(FOLDER_TREE_MIN_WIDTH, FOLDER_TREE_WIDTH)
    } else {
        FOLDER_TREE_WIDTH
    }
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
        wrapper.set_halign(gtk::Align::Fill);
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
            &super::collections::library_route_inset(
                self.route_empty_view(msgid("Loading folders...")),
            ),
            Some("loading"),
        );
        stack.set_visible_child_name("loading");
        wrapper.append(&stack);
        let resume_slot = Rc::new(RefCell::new(None::<FolderResume>));
        let live =
            (selected.artwork.source_id.as_str() != sources::LOCAL_LIBRARY_SOURCE_ID).then(|| {
                selected.operations.folder(
                    path.last().map(|item| item.id.clone()),
                    selected.music_folder_object_id.clone(),
                )
            });
        let settings = self
            .settings
            .current
            .borrow()
            .library_list(LibraryListKey::Tracks);
        let selected_for_load = selected.clone();
        let path_for_load = path.clone();
        let task = selected.runtime.spawn(async move {
            prepare_folder(&selected_for_load, &path_for_load, live, &settings).await
        });
        let shell = Rc::downgrade(self);
        let stack_for_load = stack.clone();
        let selected = selected.clone();
        let resume_for_load = Rc::clone(&resume_slot);
        let search_for_load = search.clone();
        gtk::glib::spawn_future_local(async move {
            let Some(shell) = shell.upgrade() else { return };
            let Ok(Ok((folders, order, source, first_tracks))) = task.await else {
                stack_for_load.add_named(&folder_error_view(&shell), Some("error"));
                stack_for_load.set_visible_child_name("error");
                return;
            };
            let (content, resume) = folder_page(
                &shell,
                &selected,
                path,
                search_for_load,
                folders,
                order,
                source,
                first_tracks,
            );
            resume_for_load.replace(Some(resume));
            stack_for_load.add_named(&content, Some("content"));
            stack_for_load.set_visible_child_name("content");
        });
        let resume = Rc::new(move || {
            if let Some(resume) = resume_slot.borrow().as_ref() {
                resume();
            }
        });
        MountedRoute::new(wrapper.upcast(), resume)
    }
}

async fn prepare_folder(
    selected: &crate::runtime::SelectedLibrary,
    path: &[FolderPathItem],
    live: Option<async_channel::Receiver<Result<sources::LiveFolderPage, String>>>,
    settings: &crate::LibraryListSettings,
) -> Result<
    (
        Vec<FolderLink>,
        Vec<TrackKey>,
        FolderTrackSource,
        Vec<library::TrackRow>,
    ),
    String,
> {
    let database = &selected.database;
    let source = selected.source_key;
    let selected_folder = selected.music_folder_key;
    let folder_object_id = path.last().map(|item| item.id.clone());
    let cancellation = ReadCancellation::new();
    if let Some(live) = live {
        match live.recv().await {
            Ok(Ok(page)) => {
                let track_ids = page.tracks;
                let candidates = database
                    .track_keys_by_objects(source, &track_ids, &cancellation)
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
                            &cancellation,
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
                        .track_rows(
                            source,
                            &order[..order.len().min(64_usize.saturating_sub(folders.len()))],
                            &cancellation,
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                    return Ok((
                        folders,
                        order,
                        FolderTrackSource::Live(candidates.into()),
                        first_tracks,
                    ));
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
                .folder_key_by_object(source, object_id, &cancellation)
                .await
                .map_err(|error| error.to_string())?,
            None => None,
        },
        selected_folder,
    )
    .map_err(|error| error.to_string())?;
    let folder_order = database
        .folder_child_order(source, exact_folder, &cancellation)
        .await
        .map_err(|error| error.to_string())?;
    let folders: Vec<FolderLink> = database
        .folder_rows(source, &folder_order, &cancellation)
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
            &cancellation,
        )
        .await
        .map_err(|error| error.to_string())?;
    let first_tracks = page
        .first_rows
        .into_iter()
        .take(64_usize.saturating_sub(folders.len()))
        .collect();
    Ok((
        folders,
        page.order,
        FolderTrackSource::CachedFolder(exact_folder),
        first_tracks,
    ))
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
    order: Vec<TrackKey>,
    source: FolderTrackSource,
    first_tracks: Vec<library::TrackRow>,
) -> (gtk::Widget, FolderResume) {
    search.set_visible(true);
    let route_width = crate::shell::layout::route_content_width(shell);
    let preferred_tree_width = Rc::new(Cell::new(
        shell
            .settings
            .current
            .borrow()
            .folder_view
            .tree_width
            .unwrap_or_else(|| folder_tree_width(route_width)),
    ));
    let tree_visible = folder_tree_visible(route_width);
    let tree_width = if tree_visible {
        folder_tree_position(route_width, preferred_tree_width.get())
    } else {
        0
    };

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
    let source_key = selected.source_key;
    let track_load = Arc::new(move |keys: Vec<TrackKey>, cancellation: ReadCancellation| {
        let database = Arc::clone(&row_database);
        Box::pin(async move {
            let rows = database
                .track_rows(source_key, &keys, &cancellation)
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
            FolderTableRow::Track(track) => track.track_key,
            FolderTableRow::Folder(_) => unreachable!(),
        },
    );
    let sections = gtk::gio::ListStore::new::<super::sparse_model::SparseObjectModel>();
    sections.append(&folder_sparse.list_model());
    sections.append(&track_sparse.list_model());
    let rows = gtk::FlattenListModel::new(Some(sections));
    let table_initial_width = route_width
        .saturating_sub(tree_width)
        .saturating_sub(if tree_visible {
            FOLDER_SPLIT_SEPARATOR_WIDTH
        } else {
            0
        })
        .saturating_sub(super::route_layout::PRIMARY_ROUTE_HORIZONTAL_INSET)
        .max(1);
    let (table, table_width_fit) = folder_table(
        shell,
        rows,
        Rc::clone(&folder_sparse),
        Rc::clone(&track_sparse),
        selected.clone(),
        path.clone(),
        table_initial_width,
    );

    let tree = gtk::ListBox::new();
    tree.add_css_class("folder-tree");
    tree.set_selection_mode(gtk::SelectionMode::None);
    tree.set_width_request(1);
    tree.set_hexpand(true);
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
            1,
            index + 1 == path.len(),
            false,
        ));
    }
    for folder in folders.iter() {
        tree.append(&folder_tree_row(
            shell,
            &path,
            folder,
            path.len().saturating_add(1).min(3),
        ));
    }
    let tree_scroller = gtk::ScrolledWindow::new();
    tree_scroller.add_css_class("folders-tree-pane");
    tree_scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    tree_scroller.set_min_content_width(FOLDER_TREE_MIN_WIDTH);
    tree_scroller.set_hexpand(false);
    tree_scroller.set_vexpand(true);
    tree_scroller.set_visible(tree_visible);
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
    table_stack.add_named(&folder_empty_view(shell), Some("empty"));
    table_stack.set_visible_child_name(if initial_empty { "empty" } else { "content" });

    let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
    paned.add_css_class("folders-split");
    paned.set_position(tree_width);
    paned.set_wide_handle(false);
    paned.set_hexpand(true);
    paned.set_vexpand(true);
    paned.set_start_child(Some(&tree_scroller));
    paned.set_resize_start_child(false);
    paned.set_shrink_start_child(false);
    paned.set_end_child(Some(&table_stack));
    paned.set_resize_end_child(true);
    paned.set_shrink_end_child(true);
    let applying_tree_width = Rc::new(Cell::new(false));
    let tree_width_changed = Rc::new(Cell::new(false));
    let position_tree_scroller = tree_scroller.clone();
    let position_preferred = Rc::clone(&preferred_tree_width);
    let position_applying = Rc::clone(&applying_tree_width);
    let position_changed = Rc::clone(&tree_width_changed);
    paned.connect_position_notify(move |paned| {
        if position_applying.get() || !position_tree_scroller.is_visible() {
            return;
        }
        let position = paned.position();
        if position >= FOLDER_TREE_MIN_WIDTH && position_preferred.replace(position) != position {
            position_changed.set(true);
        }
    });
    connect_folder_tree_width_save(
        shell,
        &paned,
        Rc::clone(&preferred_tree_width),
        tree_width_changed,
    );

    let resize_tree_scroller = tree_scroller.clone();
    let resize_paned = paned.clone();
    let resize_preferred = Rc::clone(&preferred_tree_width);
    let resize_applying = Rc::clone(&applying_tree_width);
    let allocated_width = Cell::new(route_width);
    let paned_owner = crate::layout::width_allocation_owner(&paned, move |width| {
        if width <= 1 || allocated_width.replace(width) == width {
            return;
        }
        apply_folder_width(
            &resize_paned,
            &resize_tree_scroller,
            width,
            resize_preferred.get(),
            &resize_applying,
        );
    });

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
    (paned_owner.upcast(), resume)
}

fn apply_folder_width(
    paned: &gtk::Paned,
    tree_scroller: &gtk::ScrolledWindow,
    width: i32,
    preferred_tree_width: i32,
    applying: &Cell<bool>,
) {
    let tree_visible = folder_tree_visible(width);
    let tree_width = if tree_visible {
        folder_tree_position(width, preferred_tree_width)
    } else {
        0
    };
    applying.set(true);
    tree_scroller.set_visible(tree_visible);
    paned.set_position(tree_width);
    applying.set(false);
}

fn connect_folder_tree_width_save(
    shell: &Rc<Shell>,
    paned: &gtk::Paned,
    preferred_width: Rc<Cell<i32>>,
    changed: Rc<Cell<bool>>,
) {
    let events = gtk::EventControllerLegacy::new();
    events.set_propagation_phase(gtk::PropagationPhase::Capture);
    let shell = Rc::downgrade(shell);
    events.connect_event(move |_, event| {
        if matches!(
            event.event_type(),
            gtk::gdk::EventType::ButtonRelease | gtk::gdk::EventType::KeyRelease
        ) {
            let shell = shell.clone();
            let preferred_width = Rc::clone(&preferred_width);
            let changed = Rc::clone(&changed);
            gtk::glib::idle_add_local_once(move || {
                if !changed.replace(false) {
                    return;
                }
                if let Some(shell) = shell.upgrade() {
                    let width = preferred_width.get();
                    shell.update_app_settings("Folder tree width", move |settings| {
                        if settings.folder_view.tree_width == Some(width) {
                            return false;
                        }
                        settings.folder_view.tree_width = Some(width);
                        true
                    });
                }
            });
        }
        gtk::glib::Propagation::Proceed
    });
    paned.add_controller(events);
}

fn folder_table(
    shell: &Rc<Shell>,
    rows: gtk::FlattenListModel,
    folders: Rc<super::sparse_model::SparseRouteModel<FolderLink, FolderTableRow>>,
    tracks: Rc<super::sparse_model::SparseRouteModel<TrackKey, FolderTableRow>>,
    selected: crate::runtime::SelectedLibrary,
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
    let folder_settings = shell.settings.current.borrow().folder_view.clone();
    let name = folder_name_column_restored(shell, path.clone());
    configure_folder_column(
        shell,
        &name,
        folder_settings.name_column_width.unwrap_or(220),
    );
    table.append_column(&name);
    let detail = folder_detail_column(shell, path.clone());
    configure_folder_column(
        shell,
        &detail,
        folder_settings.detail_column_width.unwrap_or(200),
    );
    table.append_column(&detail);
    let duration = folder_duration_column(shell, path.clone());
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
                if order.get(anchor) != Some(&track.track_key) {
                    return;
                }
                let media = playback::PlaybackMedia::from(track.clone());
                if let Some(request) = playback::LoadedPlayRequest::context(
                    selected.source_key,
                    selected.source_session_epoch,
                    order,
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

fn folder_name_column_restored(
    shell: &Rc<Shell>,
    path: Vec<FolderPathItem>,
) -> gtk::ColumnViewColumn {
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
        row.append(&icon);
        let cover = crate::shell::cover::ArtworkTile::new(28);
        cover.widget().set_visible(false);
        row.append(&cover.widget());
        let label = gtk::Label::new(None);
        label.add_css_class("folder-row-label");
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
        install_folder_cell_activation(&row, item, &setup_shell, path.clone());
        item.set_child(Some(&row));
        setup_cells.insert(item, Cell { icon, cover, label });
    });
    let bind_shell = Rc::clone(shell);
    let bind_cells = cells.clone();
    connect_sparse_bind(&factory, move |item| {
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

fn folder_detail_column(shell: &Rc<Shell>, path: Vec<FolderPathItem>) -> gtk::ColumnViewColumn {
    folder_text_column(shell, path, msgid("Artist / Album"), 18, |row| match row {
        FolderTableRow::Folder(_) => tr("Folder"),
        FolderTableRow::Track(track) => {
            format!("{} / {}", track.display_artist, track.display_album)
        }
    })
}

fn folder_duration_column(shell: &Rc<Shell>, path: Vec<FolderPathItem>) -> gtk::ColumnViewColumn {
    folder_text_column(shell, path, "Duration", 8, |row| match row {
        FolderTableRow::Folder(_) => String::new(),
        FolderTableRow::Track(track) => {
            crate::format_duration((track.duration_millis.max(0) / 1_000) as u32)
        }
    })
}

fn folder_text_column(
    shell: &Rc<Shell>,
    path: Vec<FolderPathItem>,
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
        install_folder_cell_activation(&label, item, &setup_shell, path.clone());
        item.set_child(Some(&label));
    });
    connect_sparse_bind(&factory, move |item| {
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

fn configure_folder_column(_: &Rc<Shell>, column: &gtk::ColumnViewColumn, width: i32) {
    column.set_fixed_width(width);
    column.set_resizable(false);
}

fn folder_tree_row(
    shell: &Rc<Shell>,
    path: &[FolderPathItem],
    folder: &FolderLink,
    depth: usize,
) -> gtk::Widget {
    let mut next = path.to_vec();
    next.push(FolderPathItem {
        id: folder.object_id.clone(),
        name: folder.name.clone(),
    });
    folder_tree_navigation_row(shell, &folder.name, next, depth, false, false)
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
    button.set_hexpand(true);
    button.set_halign(gtk::Align::Fill);
    if current {
        button.add_css_class("active");
    }
    button.set_sensitive(!current);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_hexpand(true);
    row.set_margin_start(i32::try_from(depth).unwrap_or(i32::MAX).saturating_mul(12));
    row.append(&gtk::Image::from_icon_name("rufin-folders-symbolic"));
    let text = if localized {
        tr(name)
    } else {
        name.to_string()
    };
    let label = gtk::Label::new(Some(&text));
    label.set_xalign(0.0);
    label.set_hexpand(true);
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
    breadcrumbs.set_halign(gtk::Align::Fill);
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
    button.set_can_target(!current);
    button.set_focusable(!current);
    let text = if localized {
        tr(label)
    } else {
        label.to_string()
    };
    let label_widget = gtk::Label::new(Some(&text));
    label_widget.set_xalign(0.0);
    label_widget.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label_widget.set_single_line_mode(true);
    button.set_child(Some(&label_widget));
    button.set_tooltip_text(Some(&text));
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
        assert_eq!(folder_tree_position(550, 900), 293);
        assert_eq!(folder_tree_position(1_500, 900), FOLDER_TREE_MAX_WIDTH);
    }
}
