use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
    sync::Arc,
};

use super::collection_context::install_track_context_menu;
use super::route::{FolderPathItem, Route};
use crate::layout::{configure_fill_width_clip, width_allocation_owner};
use crate::localization::{
    bind_label_text, bind_search_placeholder, bind_widget_tooltip, localized_column,
    localized_label,
};
use crate::runtime::{SelectedLibrary, SelectedSourceHandle};
use crate::shell::Shell;
use crate::shell::cover::THUMB_COVER_SIZE;
use crate::shell::layout::route_content_width;
use crate::shell::route::MountedRoute;
use crate::{LibraryListKey, format_duration};
use ::library::{Folder, FolderContents, Track};
use adw::prelude::*;
use artwork::ArtworkBinding;
use gtk::{gio, glib};
use localization::{msgid, tr};
use playback::{LoadedPlayRequest, QueuePlacement, SourceSessionEpoch};
use tracing::warn;

use super::collections::library_route_inset;
use super::library_fields::sort_tracks;
use super::models::track_matches_query;
use super::route_layout::{
    PRIMARY_ROUTE_HORIZONTAL_INSET, ROUTE_TOP_MARGIN, route_scroller_widget,
};
use super::table_sizing::{
    ColumnViewWidthFit, connect_column_width_save, install_column_view_width_fit,
};

const FOLDER_TREE_WIDTH: i32 = 260;
const FOLDER_TREE_MIN_WIDTH: i32 = 132;
const FOLDER_TREE_MAX_WIDTH: i32 = 480;
const FOLDER_TABLE_MIN_WIDTH: i32 = 240;
const FOLDER_TREE_HIDE_WIDTH: i32 = 550;
const FOLDER_SPLIT_SEPARATOR_WIDTH: i32 = 17;
const FOLDER_NAME_COLUMN_WIDTH: i32 = 220;
const FOLDER_DETAIL_COLUMN_WIDTH: i32 = 200;
const FOLDER_DURATION_COLUMN_WIDTH: i32 = 80;
const FOLDER_ROW_ARTWORK_SIZE: i32 = 28;

#[derive(Clone)]
enum FolderTableRow {
    Folder {
        name: String,
        path: Vec<FolderPathItem>,
    },
    Track {
        track: Track,
        position: usize,
    },
    Empty,
}

enum FolderCellText {
    Plain(String),
    Localized(&'static str),
}

impl FolderCellText {
    fn resolve(self) -> (String, Option<&'static str>) {
        match self {
            Self::Plain(text) => (text, None),
            Self::Localized(message) => (tr(message), Some(message)),
        }
    }
}

#[derive(Clone)]
struct FolderPlayContext {
    source_id: ::library::SourceId,
    source_session_epoch: SourceSessionEpoch,
    context_id: String,
}

#[derive(Clone)]
struct FolderTableModel {
    rows: gio::ListStore,
    tracks: Rc<RefCell<Arc<[Track]>>>,
    play: Rc<FolderPlayContext>,
}

impl FolderTableModel {
    fn new(play: Rc<FolderPlayContext>) -> Self {
        Self {
            rows: gio::ListStore::new::<glib::BoxedAnyObject>(),
            tracks: Rc::new(RefCell::new(Arc::from([]))),
            play,
        }
    }

    fn replace(&self, rows: Vec<FolderTableRow>, tracks: Arc<[Track]>) {
        let additions = rows
            .into_iter()
            .map(glib::BoxedAnyObject::new)
            .collect::<Vec<_>>();
        self.tracks.replace(tracks);
        self.rows.splice(0, self.rows.n_items(), &additions);
    }
}

pub(crate) struct FolderRouteProjection {
    root: gtk::Widget,
    shell: Weak<Shell>,
    pub(crate) path: Vec<FolderPathItem>,
    source: SelectedSourceHandle,
    folder_id: Option<::library::FolderId>,
    music_folder_id: Option<::library::MusicFolderId>,
    status: gtk::Stack,
    search: gtk::SearchEntry,
    tree: gtk::ListBox,
    table_model: FolderTableModel,
    contents: RefCell<Option<FolderContents>>,
}

impl FolderRouteProjection {
    pub(crate) fn new(
        shell: &Rc<Shell>,
        path: Vec<FolderPathItem>,
        selected: &SelectedLibrary,
    ) -> Rc<Self> {
        let wrapper = gtk::Box::new(gtk::Orientation::Vertical, 12);
        wrapper.add_css_class("route-content");
        wrapper.add_css_class("folders-route");
        wrapper.set_margin_top(ROUTE_TOP_MARGIN);
        wrapper.set_margin_bottom(28);
        wrapper.set_hexpand(true);
        wrapper.set_halign(gtk::Align::Fill);
        wrapper.set_width_request(1);
        wrapper.set_vexpand(true);

        if !path.is_empty() {
            wrapper.append(&library_route_inset(
                folder_breadcrumbs(shell, &path).upcast(),
            ));
        }

        let search = gtk::SearchEntry::new();
        search.add_css_class("folder-search");
        bind_search_placeholder(&search, "Search current folder");
        shell.set_route_search(Some(search.clone()));

        let route_width = route_content_width(shell);
        let saved_tree_width = shell.settings.current.borrow().folder_view.tree_width;
        let preferred_tree_width = Rc::new(Cell::new(
            saved_tree_width.unwrap_or_else(|| folder_tree_width(route_width)),
        ));
        let tree_visible = folder_tree_visible(route_width);
        let tree_width = if tree_visible {
            folder_tree_position(route_width, preferred_tree_width.get())
        } else {
            0
        };

        let play = Rc::new(FolderPlayContext {
            source_id: selected.source_id.clone(),
            source_session_epoch: selected.source_session_epoch,
            context_id: folder_context_id(&path),
        });
        let table_model = FolderTableModel::new(play);
        let table_initial_width = route_width
            .saturating_sub(tree_width)
            .saturating_sub(if tree_visible {
                FOLDER_SPLIT_SEPARATOR_WIDTH
            } else {
                0
            })
            .saturating_sub(PRIMARY_ROUTE_HORIZONTAL_INSET)
            .max(1);
        let (table, table_width_fit) = folder_table(shell, &table_model, table_initial_width);

        let table_scroller = gtk::ScrolledWindow::new();
        configure_fill_width_clip(&table_scroller, gtk::PolicyType::Automatic);
        table_scroller.set_hexpand(true);
        table_scroller.set_vexpand(true);
        table_scroller.set_child(Some(&library_route_inset(table.clone().upcast())));
        let table_view = route_scroller_widget(table_scroller.clone());
        let resize_table_scroller = table_scroller.clone();
        let table_view = width_allocation_owner(&table_view, move |width| {
            table_width_fit.fit_scroller_allocation(&resize_table_scroller, width);
        })
        .upcast::<gtk::Widget>();

        let tree = gtk::ListBox::new();
        tree.add_css_class("folder-tree");
        tree.set_selection_mode(gtk::SelectionMode::None);
        tree.set_width_request(1);
        tree.set_hexpand(true);

        let tree_scroller = gtk::ScrolledWindow::new();
        tree_scroller.add_css_class("folders-tree-pane");
        tree_scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        tree_scroller.set_min_content_width(FOLDER_TREE_MIN_WIDTH);
        tree_scroller.set_hexpand(false);
        tree_scroller.set_vexpand(true);
        tree_scroller.set_visible(tree_visible);
        tree_scroller.set_child(Some(&tree));

        let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
        paned.add_css_class("folders-split");
        paned.set_position(tree_width);
        paned.set_wide_handle(false);
        paned.set_hexpand(true);
        paned.set_vexpand(true);
        paned.set_start_child(Some(&tree_scroller));
        paned.set_resize_start_child(false);
        paned.set_shrink_start_child(false);
        paned.set_end_child(Some(&table_view));
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
            if position >= FOLDER_TREE_MIN_WIDTH && position_preferred.replace(position) != position
            {
                position_changed.set(true);
            }
        });
        connect_folder_tree_width_save(
            shell,
            &paned,
            Rc::clone(&preferred_tree_width),
            tree_width_changed,
        );

        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_hexpand(true);
        content.set_vexpand(true);
        content.append(&library_route_inset(search.clone().upcast()));
        let resize_tree_scroller = tree_scroller.clone();
        let resize_paned = paned.clone();
        let resize_preferred = Rc::clone(&preferred_tree_width);
        let resize_applying = Rc::clone(&applying_tree_width);
        let allocated_width = Cell::new(route_width);
        let paned_owner = width_allocation_owner(&paned, move |width| {
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
        content.append(&paned_owner);

        let status = gtk::Stack::new();
        status.set_hexpand(true);
        status.set_vexpand(true);
        status.add_named(&content, Some("content"));
        status.add_named(
            &library_route_inset(shell.route_empty_view(msgid("Loading folders..."))),
            Some("loading"),
        );
        let empty = gtk::Box::new(gtk::Orientation::Vertical, 12);
        empty.add_css_class("empty-state");
        empty.set_vexpand(true);
        empty.set_hexpand(true);
        empty.set_valign(gtk::Align::Center);
        empty.set_halign(gtk::Align::Center);
        let empty_label = localized_label("Nothing in this folder");
        empty_label.add_css_class("muted");
        let retry = gtk::Button::with_label(&tr("Refresh"));
        retry.add_css_class("pill");
        retry.set_halign(gtk::Align::Center);
        empty.append(&empty_label);
        empty.append(&retry);
        status.add_named(&library_route_inset(empty.upcast()), Some("empty"));
        status.set_visible_child_name("loading");
        wrapper.append(&status);

        let projection = Rc::new(Self {
            root: wrapper.upcast(),
            shell: Rc::downgrade(shell),
            source: selected.operations.clone(),
            folder_id: path.last().map(|item| item.id.clone()),
            music_folder_id: selected.music_folder_id.clone(),
            path,
            status,
            search,
            tree,
            table_model,
            contents: RefCell::new(None),
        });

        let search_projection = Rc::downgrade(&projection);
        projection.search.connect_search_changed(move |_| {
            if let Some(projection) = search_projection.upgrade() {
                projection.publish_table();
            }
        });
        let retry_projection = Rc::downgrade(&projection);
        retry.connect_clicked(move |_| {
            if let Some(projection) = retry_projection.upgrade() {
                projection.request();
            }
        });
        projection
    }

    pub(crate) fn widget(&self) -> gtk::Widget {
        self.root.clone()
    }

    pub(crate) fn publish(&self) {
        if self.contents.borrow().is_some() {
            self.publish_tree();
            self.publish_table();
        }
        let page = if self.contents.borrow().is_none() {
            "empty"
        } else {
            "content"
        };
        self.status.set_visible_child_name(page);
    }

    fn begin_refresh(&self) {
        if self.contents.borrow().is_none() {
            self.status.set_visible_child_name("loading");
        }
    }

    fn apply_loaded(&self, result: Result<FolderContents, String>) {
        match result {
            Ok(contents) => {
                *self.contents.borrow_mut() = Some(contents);
            }
            Err(error) => {
                warn!(%error, path = ?self.path, "folder load failed");
                self.contents.borrow_mut().take();
            }
        }
        self.publish();
    }

    fn request(self: &Rc<Self>) {
        if self.shell.upgrade().is_none() {
            return;
        }
        self.begin_refresh();
        let receiver = self
            .source
            .folder(self.folder_id.clone(), self.music_folder_id.clone());
        let projection = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = receiver
                .recv()
                .await
                .unwrap_or_else(|_| Err(tr("Couldn't load this folder")));
            if let Some(projection) = projection.upgrade() {
                projection.apply_loaded(result);
            }
        });
    }

    fn publish_tree(&self) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        let contents = self.contents.borrow();
        let Some(contents) = contents.as_ref() else {
            return;
        };
        populate_folder_tree(&shell, &self.tree, &self.path, contents);
    }

    fn publish_table(&self) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        let contents = self.contents.borrow();
        let Some(contents) = contents.as_ref() else {
            return;
        };
        populate_folder_table(
            &shell,
            &self.table_model,
            &self.path,
            contents,
            self.search.text().as_str(),
        );
    }
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
            glib::idle_add_local_once(move || {
                if !changed.replace(false) {
                    return;
                }
                if let Some(shell) = shell.upgrade() {
                    shell.save_folder_tree_width(preferred_width.get());
                }
            });
        }
        glib::Propagation::Proceed
    });
    paned.add_controller(events);
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
    translate: bool,
) -> gtk::Button {
    let text = display_label(label, translate);
    let button = gtk::Button::new();
    button.add_css_class("flat");
    button.add_css_class("folder-breadcrumb");
    button.set_width_request(1);
    button.set_sensitive(!current);
    let label = gtk::Label::new(Some(&text));
    label.set_xalign(0.0);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_single_line_mode(true);
    button.set_child(Some(&label));
    button.set_tooltip_text(Some(&text));
    let shell = Rc::clone(shell);
    button.connect_clicked(move |_| {
        shell.navigate(Route::Folders { path: path.clone() });
    });
    button
}

fn populate_folder_tree(
    shell: &Rc<Shell>,
    tree: &gtk::ListBox,
    path: &[FolderPathItem],
    contents: &FolderContents,
) {
    while let Some(child) = tree.first_child() {
        tree.remove(&child);
    }
    tree.append(&tree_row(
        shell,
        "Folders",
        Vec::new(),
        path.is_empty(),
        0,
        true,
    ));

    for (index, entry) in path.iter().enumerate() {
        tree.append(&tree_row(
            shell,
            &entry.name,
            path.iter().take(index + 1).cloned().collect(),
            index + 1 == path.len(),
            1,
            false,
        ));
    }

    let mut folders = contents.folders.to_vec();
    folders.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.id.cmp(&right.id))
    });
    for folder in folders {
        tree.append(&tree_row(
            shell,
            &folder.name,
            path_for_child(path, &folder),
            false,
            path.len().saturating_add(1).min(3),
            false,
        ));
    }
}

fn tree_row(
    shell: &Rc<Shell>,
    label: &str,
    path: Vec<FolderPathItem>,
    current: bool,
    depth: usize,
    translate: bool,
) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("flat");
    button.add_css_class("folder-tree-row");
    button.set_hexpand(true);
    button.set_halign(gtk::Align::Fill);
    if current {
        button.add_css_class("active");
    }
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    content.set_hexpand(true);
    content.set_margin_start((depth as i32) * 12);
    content.append(&gtk::Image::from_icon_name("rufin-folders-symbolic"));
    let text = gtk::Label::new(Some(&display_label(label, translate)));
    text.set_xalign(0.0);
    text.set_hexpand(true);
    text.set_ellipsize(gtk::pango::EllipsizeMode::End);
    content.append(&text);
    button.set_child(Some(&content));

    let shell = Rc::clone(shell);
    button.connect_clicked(move |_| {
        shell.navigate(Route::Folders { path: path.clone() });
    });
    button
}

fn populate_folder_table(
    shell: &Rc<Shell>,
    model: &FolderTableModel,
    path: &[FolderPathItem],
    contents: &FolderContents,
    query: &str,
) {
    let query = query.trim().to_lowercase();
    let mut folders = contents
        .folders
        .iter()
        .filter(|folder| query.is_empty() || folder.name.to_lowercase().contains(&query))
        .cloned()
        .collect::<Vec<_>>();
    folders.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.id.cmp(&right.id))
    });

    let mut tracks = contents
        .tracks
        .iter()
        .filter(|track| query.is_empty() || track_matches_query(track, &query))
        .cloned()
        .collect::<Vec<_>>();
    let settings = shell
        .settings
        .current
        .borrow()
        .library_list(LibraryListKey::Tracks);
    sort_tracks(&mut tracks, &settings);

    let visible_tracks: Arc<[Track]> = tracks.into();
    let mut rows = folders
        .into_iter()
        .map(|folder| FolderTableRow::Folder {
            name: folder.name.clone(),
            path: path_for_child(path, &folder),
        })
        .collect::<Vec<_>>();
    rows.extend(
        visible_tracks
            .iter()
            .enumerate()
            .map(|(position, track)| FolderTableRow::Track {
                track: track.clone(),
                position,
            }),
    );
    if rows.is_empty() {
        rows.push(FolderTableRow::Empty);
    }
    model.replace(rows, visible_tracks);
}

fn folder_table(
    shell: &Rc<Shell>,
    model: &FolderTableModel,
    initial_width: i32,
) -> (gtk::ColumnView, ColumnViewWidthFit) {
    let selection = gtk::SingleSelection::new(Some(model.rows.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    selection.set_selected(gtk::INVALID_LIST_POSITION);

    let table = gtk::ColumnView::new(Some(selection.clone()));
    table.add_css_class("folders-table");
    table.add_css_class("data-table");
    table.set_vscroll_policy(gtk::ScrollablePolicy::Minimum);
    table.set_hexpand(true);
    table.set_halign(gtk::Align::Fill);
    table.set_vexpand(true);

    let folder_view = shell.settings.current.borrow().folder_view.clone();
    let columns = vec![
        (
            folder_name_column(shell),
            folder_view
                .name_column_width
                .unwrap_or(FOLDER_NAME_COLUMN_WIDTH),
        ),
        (
            folder_detail_column(shell),
            folder_view
                .detail_column_width
                .unwrap_or(FOLDER_DETAIL_COLUMN_WIDTH),
        ),
        (
            folder_duration_column(shell),
            folder_view
                .duration_column_width
                .unwrap_or(FOLDER_DURATION_COLUMN_WIDTH),
        ),
    ];
    for (column, width) in &columns {
        column.set_fixed_width(*width);
        column.set_resizable(true);
        table.append_column(column);
    }
    let width_fit = install_column_view_width_fit(&table, columns, initial_width);
    let save_shell = Rc::downgrade(shell);
    connect_column_width_save(&table, &width_fit, move |widths| {
        let [name, detail, duration] = widths.as_slice() else {
            return;
        };
        if let Some(shell) = save_shell.upgrade() {
            shell.save_folder_column_widths([*name, *detail, *duration]);
        }
    });

    let row_factory = gtk::SignalListItemFactory::new();
    row_factory.connect_bind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ColumnViewRow>() else {
            return;
        };
        let Some(row) = folder_table_row_from_object(item.item()) else {
            return;
        };
        let active = !matches!(&row, FolderTableRow::Empty);
        item.set_selectable(active);
        item.set_activatable(active);
        item.set_accessible_label(&row.accessible_label());
    });
    row_factory.connect_unbind(|_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ColumnViewRow>() {
            item.set_selectable(true);
            item.set_activatable(true);
            item.set_accessible_label("");
        }
    });
    table.set_row_factory(Some(&row_factory));

    let activate_shell = Rc::clone(shell);
    let activate_model = model.clone();
    table.connect_activate(move |_, position| {
        let Some(row) = folder_table_row_at(&activate_model.rows, position) else {
            return;
        };
        activate_folder_table_row(&activate_shell, &activate_model, &row);
    });

    (table, width_fit)
}

fn folder_name_column(shell: &Rc<Shell>) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        item.set_child(Some(&folder_table_cell()));
    });
    let shell = Rc::clone(shell);
    factory.connect_bind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(row) = folder_table_row_from_object(item.item()) else {
            return;
        };
        let Some(cell) = item
            .child()
            .and_then(|child| child.downcast::<gtk::Box>().ok())
        else {
            return;
        };
        clear_folder_table_cell(&cell);
        let content = folder_table_cell_content();

        match &row {
            FolderTableRow::Folder { name, .. } => {
                content.append(&gtk::Image::from_icon_name("rufin-folders-symbolic"));
                content.append(&folder_table_label(name, 20));
            }
            FolderTableRow::Track { track, .. } => {
                content.append(&shell.cover_tile_for_candidates(
                    ArtworkBinding::track(track),
                    FOLDER_ROW_ARTWORK_SIZE,
                    THUMB_COVER_SIZE,
                ));
                content.append(&folder_table_label(&track.title, 20));
            }
            FolderTableRow::Empty => {
                let label = folder_table_label("", 28);
                bind_label_text(&label, "Nothing in this folder");
                bind_widget_tooltip(&label, "Nothing in this folder");
                label.add_css_class("muted");
                content.append(&label);
            }
        }
        install_folder_table_cell_interactions(&content, &shell, &row);
        cell.append(&content);
    });
    factory.connect_unbind(|_, item| {
        if let Some(cell) = item
            .downcast_ref::<gtk::ListItem>()
            .and_then(gtk::ListItem::child)
            .and_then(|child| child.downcast::<gtk::Box>().ok())
        {
            clear_folder_table_cell(&cell);
        }
    });
    localized_column("Name", &factory)
}

fn folder_detail_column(shell: &Rc<Shell>) -> gtk::ColumnViewColumn {
    folder_text_column(shell, msgid("Artist / Album"), 18, |row| match row {
        FolderTableRow::Folder { .. } => FolderCellText::Localized(msgid("Folder")),
        FolderTableRow::Track { track, .. } => {
            FolderCellText::Plain(format!("{} / {}", track.artist, track.album))
        }
        FolderTableRow::Empty => FolderCellText::Plain(String::new()),
    })
}

fn folder_duration_column(shell: &Rc<Shell>) -> gtk::ColumnViewColumn {
    folder_text_column(shell, "Duration", 8, |row| match row {
        FolderTableRow::Track { track, .. } => {
            FolderCellText::Plain(format_duration(track.duration_seconds))
        }
        FolderTableRow::Folder { .. } | FolderTableRow::Empty => {
            FolderCellText::Plain(String::new())
        }
    })
}

fn folder_text_column(
    shell: &Rc<Shell>,
    title: &str,
    max_width_chars: i32,
    text: impl Fn(&FolderTableRow) -> FolderCellText + 'static,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let text = Rc::new(text);
    factory.connect_setup(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        item.set_child(Some(&folder_table_cell()));
    });
    let shell = Rc::clone(shell);
    let text_for_bind = Rc::clone(&text);
    factory.connect_bind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(row) = folder_table_row_from_object(item.item()) else {
            return;
        };
        let Some(cell) = item
            .child()
            .and_then(|child| child.downcast::<gtk::Box>().ok())
        else {
            return;
        };
        clear_folder_table_cell(&cell);
        let content = folder_table_cell_content();
        let (text, localized_message) = text_for_bind(&row).resolve();
        let label = folder_table_label(&text, max_width_chars);
        if let Some(message) = localized_message {
            bind_label_text(&label, message);
            bind_widget_tooltip(&label, message);
        }
        content.append(&label);
        install_folder_table_cell_interactions(&content, &shell, &row);
        cell.append(&content);
    });
    factory.connect_unbind(|_, item| {
        if let Some(cell) = item
            .downcast_ref::<gtk::ListItem>()
            .and_then(gtk::ListItem::child)
            .and_then(|child| child.downcast::<gtk::Box>().ok())
        {
            clear_folder_table_cell(&cell);
        }
    });
    localized_column(title, &factory)
}

fn folder_table_cell() -> gtk::Box {
    let cell = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    cell.add_css_class("folder-table-row");
    cell.set_width_request(1);
    cell.set_hexpand(true);
    cell.set_halign(gtk::Align::Fill);
    cell
}

fn folder_table_cell_content() -> gtk::Box {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    content.set_width_request(1);
    content.set_hexpand(true);
    content.set_halign(gtk::Align::Fill);
    content
}

fn folder_table_label(text: &str, max_width_chars: i32) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_width_request(1);
    label.set_hexpand(true);
    label.set_halign(gtk::Align::Fill);
    label.set_wrap(false);
    label.set_single_line_mode(true);
    label.set_width_chars(1);
    label.set_max_width_chars(max_width_chars);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    if !text.is_empty() {
        label.set_tooltip_text(Some(text));
    }
    label
}

fn clear_folder_table_cell(cell: &gtk::Box) {
    while let Some(child) = cell.first_child() {
        cell.remove(&child);
    }
}

fn install_folder_table_cell_interactions(
    target: &gtk::Box,
    shell: &Rc<Shell>,
    row: &FolderTableRow,
) {
    match row {
        FolderTableRow::Folder { path, .. } => {
            let path = path.clone();
            let shell = Rc::clone(shell);
            let click = gtk::GestureClick::new();
            click.set_button(gtk::gdk::BUTTON_PRIMARY);
            click.connect_released(move |_, n_press, _, _| {
                if n_press == 1 {
                    shell.navigate(Route::Folders { path: path.clone() });
                }
            });
            target.add_controller(click);
        }
        FolderTableRow::Track { track, .. } => {
            install_track_context_menu(target, shell, track.clone());
        }
        FolderTableRow::Empty => {}
    }
}

fn activate_folder_table_row(shell: &Rc<Shell>, model: &FolderTableModel, row: &FolderTableRow) {
    match row {
        FolderTableRow::Folder { path, .. } => {
            shell.navigate(Route::Folders { path: path.clone() });
        }
        FolderTableRow::Track { position, .. } => {
            if let Some(request) = LoadedPlayRequest::context(
                model.play.source_id.clone(),
                model.play.source_session_epoch,
                Arc::clone(&model.tracks.borrow()),
                *position,
                QueuePlacement::Now,
                model.play.context_id.clone(),
                false,
            ) {
                shell.products.playback.queue.play_loaded(request);
            }
        }
        FolderTableRow::Empty => {}
    }
}

fn folder_table_row_at(model: &gio::ListStore, position: u32) -> Option<FolderTableRow> {
    folder_table_row_from_object(model.item(position))
}

fn folder_table_row_from_object(item: Option<glib::Object>) -> Option<FolderTableRow> {
    item.and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
        .map(|item| item.borrow::<FolderTableRow>().clone())
}

impl FolderTableRow {
    fn accessible_label(&self) -> String {
        match self {
            Self::Folder { name, .. } => name.clone(),
            Self::Track { track, .. } => {
                format!("{} — {} — {}", track.title, track.artist, track.album)
            }
            Self::Empty => tr("Nothing in this folder"),
        }
    }
}

fn folder_tree_width(route_width: i32) -> i32 {
    if route_width < 760 {
        (route_width / 3).clamp(FOLDER_TREE_MIN_WIDTH, FOLDER_TREE_WIDTH)
    } else {
        FOLDER_TREE_WIDTH
    }
}

fn folder_tree_position(route_width: i32, preferred_width: i32) -> i32 {
    let maximum = route_width
        .saturating_sub(FOLDER_SPLIT_SEPARATOR_WIDTH)
        .saturating_sub(FOLDER_TABLE_MIN_WIDTH)
        .clamp(FOLDER_TREE_MIN_WIDTH, FOLDER_TREE_MAX_WIDTH);
    preferred_width.clamp(FOLDER_TREE_MIN_WIDTH, maximum)
}

fn folder_tree_visible(route_width: i32) -> bool {
    route_width >= FOLDER_TREE_HIDE_WIDTH
}

fn display_label(label: &str, translate: bool) -> String {
    if translate {
        tr(label)
    } else {
        label.to_string()
    }
}

fn path_for_child(path: &[FolderPathItem], folder: &Folder) -> Vec<FolderPathItem> {
    let mut next = path.to_vec();
    next.push(FolderPathItem {
        id: folder.id.clone(),
        name: folder.name.clone(),
    });
    next
}

fn folder_context_id(path: &[FolderPathItem]) -> String {
    let mut context = String::from("folder");
    for item in path {
        context.push(':');
        context.push_str(item.id.as_str());
    }
    context
}

#[cfg(test)]
mod tests {
    #[test]
    fn folders_hide_tree_at_tiny_width() {
        assert!(!super::folder_tree_visible(450));
        assert!(!super::folder_tree_visible(549));
        assert!(super::folder_tree_visible(550));
    }

    #[test]
    fn folder_tree_position_keeps_the_saved_width_inside_real_bounds() {
        assert_eq!(super::folder_tree_position(900, 200), 200);
        assert_eq!(
            super::folder_tree_position(900, 40),
            super::FOLDER_TREE_MIN_WIDTH
        );
        assert_eq!(super::folder_tree_position(550, 480), 293);
        assert_eq!(
            super::folder_tree_position(1_500, 900),
            super::FOLDER_TREE_MAX_WIDTH
        );
    }
}

impl Shell {
    pub(crate) fn folders_route(
        self: &Rc<Self>,
        path: Vec<FolderPathItem>,
        selected: &SelectedLibrary,
    ) -> MountedRoute {
        let projection = FolderRouteProjection::new(self, path, selected);
        projection.request();

        let resume_projection = Rc::clone(&projection);
        let route = MountedRoute::new(
            projection.widget(),
            Rc::new(move || {
                resume_projection.publish_table();
            }),
        );
        let update_projection = Rc::clone(&projection);
        route.with_library_update(Rc::new(move |update| {
            if !update.change.local_folders_changed {
                return;
            }
            update_projection.request();
        }))
    }
}
