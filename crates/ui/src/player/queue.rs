use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use artwork::ArtworkBinding;
use gtk::{gio, glib};
use library::QueuePageRow;
use localization::tr;
use playback::{OccurrenceId, PlaybackMedia, QueueReorderRequest, QueueReorderTarget};

use crate::favorites::{favorite_icon_button, set_favorite_button_active};
use crate::interactions::install_context_menu_openers;
use crate::layout::allocation_owner;
use crate::routes::collection_context::present_queue_track_context_menu;
use crate::routes::detail_links::{DetailLinkBinding, DetailLinks};
use crate::routes::route::Route;
use crate::shell::Shell;
use crate::shell::cover::{ArtworkTile, THUMB_COVER_SIZE};
use crate::shell::layout::WINDOW_CHROME_MARGIN_END;

const QUEUE_PAGE_SIZE: usize = 100;
const QUEUE_ROW_HEIGHT: i32 = 58;
const QUEUE_OVERLAY_SCROLLBAR_WIDTH: i32 = 9;
const QUEUE_SIDEBAR_END_INSET: i32 = WINDOW_CHROME_MARGIN_END + QUEUE_OVERLAY_SCROLLBAR_WIDTH;
const QUEUE_FULLSCREEN_COLUMN_SPACING: i32 = 16;
const QUEUE_FULLSCREEN_ROW_HORIZONTAL_PADDING: i32 = 12;
const QUEUE_FULLSCREEN_COVER_COLUMN_WIDTH: i32 = 50;
const QUEUE_FULLSCREEN_TITLE_COLUMN_WIDTH: i32 = 320;
const QUEUE_FULLSCREEN_ALBUM_COLUMN_WIDTH: i32 = 260;
const QUEUE_FULLSCREEN_TITLE_MIN_WIDTH: i32 = 160;
const QUEUE_FULLSCREEN_ALBUM_MIN_WIDTH: i32 = 140;
const QUEUE_DURATION_COLUMN_WIDTH: i32 = 82;
const QUEUE_YEAR_COLUMN_WIDTH: i32 = 64;
const QUEUE_FAVORITE_COLUMN_WIDTH: i32 = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct QueueFullscreenColumnWidths {
    title: i32,
    album: i32,
    show_album: bool,
    show_year: bool,
}

struct QueueFullscreenColumnWidgets {
    title: gtk::Widget,
    album: gtk::Widget,
    year: gtk::Widget,
}

#[derive(Default)]
struct QueueDragBinding {
    occurrence: RefCell<Option<OccurrenceId>>,
}

impl QueueDragBinding {
    fn bind(&self, occurrence: OccurrenceId) {
        self.occurrence.replace(Some(occurrence));
    }

    fn clear(&self) {
        self.occurrence.borrow_mut().take();
    }

    fn occurrence(&self) -> Option<OccurrenceId> {
        self.occurrence.borrow().clone()
    }

    fn reorder_request(
        &self,
        occurrence: OccurrenceId,
        after: bool,
    ) -> Option<QueueReorderRequest> {
        let target = self.occurrence()?;
        if occurrence == target {
            return None;
        }
        Some(QueueReorderRequest {
            occurrence,
            target: if after {
                QueueReorderTarget::After(target)
            } else {
                QueueReorderTarget::Before(target)
            },
        })
    }
}

impl QueueFullscreenColumnWidgets {
    fn apply(&self, widths: QueueFullscreenColumnWidths) {
        self.title.set_width_request(widths.title);
        self.album.set_width_request(widths.album.max(1));
        self.album.set_visible(widths.show_album);
        self.year.set_visible(widths.show_year);
    }
}

pub(crate) struct QueueState {
    rows: RefCell<Vec<QueuePageRow>>,
    model: gio::ListStore,
    filter: RefCell<String>,
    generation: Cell<u64>,
    running: Cell<Option<u64>>,
    cancellation: RefCell<Option<(u64, library::ReadCancellation)>>,
    current: RefCell<Option<OccurrenceId>>,
    render_queued: Cell<bool>,
}

impl QueueState {
    pub(crate) fn new() -> Self {
        Self {
            rows: RefCell::new(Vec::new()),
            model: gio::ListStore::new::<glib::BoxedAnyObject>(),
            filter: RefCell::new(String::new()),
            generation: Cell::new(0),
            running: Cell::new(None),
            cancellation: RefCell::new(None),
            current: RefCell::new(None),
            render_queued: Cell::new(false),
        }
    }

    fn begin(&self) -> (u64, library::ReadCancellation) {
        if let Some((_, cancellation)) = self.cancellation.borrow_mut().take() {
            cancellation.cancel();
        }
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        self.running.set(Some(generation));
        let cancellation = library::ReadCancellation::new();
        self.cancellation
            .replace(Some((generation, cancellation.clone())));
        (generation, cancellation)
    }

    fn accept(&self, generation: u64, rows: Vec<QueuePageRow>) -> bool {
        if self.running.get() != Some(generation) || self.generation.get() != generation {
            return false;
        }
        self.running.set(None);
        self.cancellation.borrow_mut().take();
        let changed_positions = changed_queue_positions(&self.rows.borrow(), &rows);
        let previous_ids = self
            .rows
            .borrow()
            .iter()
            .map(|row| row.object_id.clone())
            .collect::<Vec<_>>();
        let mut existing = HashMap::new();
        for position in 0..self.model.n_items() {
            let Some(object) = self
                .model
                .item(position)
                .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
            else {
                continue;
            };
            let object_id = object.borrow::<QueuePageRow>().object_id.clone();
            existing.insert(object_id, object);
        }
        let objects = rows
            .iter()
            .cloned()
            .map(|row| {
                if let Some(object) = existing.remove(&row.object_id) {
                    *object.borrow_mut::<QueuePageRow>() = row;
                    object
                } else {
                    glib::BoxedAnyObject::new(row)
                }
            })
            .collect::<Vec<_>>();
        let next_ids = rows
            .iter()
            .map(|row| row.object_id.as_str())
            .collect::<Vec<_>>();
        let prefix = previous_ids
            .iter()
            .map(String::as_str)
            .zip(next_ids.iter().copied())
            .take_while(|(left, right)| left == right)
            .count();
        let suffix = previous_ids
            .len()
            .saturating_sub(prefix)
            .min(next_ids.len().saturating_sub(prefix));
        let suffix = previous_ids
            .iter()
            .rev()
            .map(String::as_str)
            .zip(next_ids.iter().rev().copied())
            .take(suffix)
            .take_while(|(left, right)| left == right)
            .count();
        let old_middle = previous_ids.len().saturating_sub(prefix + suffix);
        let next_end = objects.len().saturating_sub(suffix);
        if old_middle != 0 || prefix != next_end {
            self.model
                .splice(prefix as u32, old_middle as u32, &objects[prefix..next_end]);
        }
        self.rows.replace(rows);
        for position in changed_positions {
            if position < self.model.n_items() {
                self.model.items_changed(position, 1, 1);
            }
        }
        true
    }

    fn update_current(&self, current: Option<OccurrenceId>) -> bool {
        let previous = self.current.replace(current.clone());
        if previous == current {
            return false;
        }
        for occurrence in [previous, current].into_iter().flatten() {
            if let Some(position) = self
                .rows
                .borrow()
                .iter()
                .position(|row| row.object_id == occurrence.to_string())
            {
                self.model.items_changed(position as u32, 1, 1);
            }
        }
        true
    }

    fn invalidate(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
        self.running.set(None);
        if let Some((_, cancellation)) = self.cancellation.borrow_mut().take() {
            cancellation.cancel();
        }
        self.rows.borrow_mut().clear();
        self.model.remove_all();
        self.current.borrow_mut().take();
    }
}

impl Drop for QueueState {
    fn drop(&mut self) {
        if let Some((_, cancellation)) = self.cancellation.get_mut().take() {
            cancellation.cancel();
        }
    }
}

impl Shell {
    pub(crate) fn schedule_queue_panel_render(self: &Rc<Self>) {
        let Some(queue) = self.selected_queue() else {
            return;
        };
        if queue.render_queued.replace(true) {
            return;
        }
        drop(queue);
        let shell = Rc::clone(self);
        glib::idle_add_local_once(move || {
            if let Some(queue) = shell.selected_queue() {
                queue.render_queued.set(false);
            }
            shell.render_queue_panel();
        });
    }

    pub(crate) fn render_queue_panel(self: &Rc<Self>) {
        let current = self
            .selected_playback()
            .as_deref()
            .and_then(|player| player.queue.current_occurrence.clone());
        let Some(queue) = self.selected_queue() else {
            clear_queue_panel_children(&self.right_panel.queue_panel);
            clear_queue_panel_children(&self.player_view.fullscreen_player.queue_panel);
            return;
        };
        if queue.rows.borrow().is_empty() && queue.running.get().is_none() {
            drop(queue);
            self.request_queue_page();
            return;
        }
        let current_changed = queue.update_current(current);
        let reorderable = queue.filter.borrow().trim().is_empty();
        render_panel(
            self,
            &self.right_panel.queue_panel,
            &queue.model,
            false,
            current_changed,
            reorderable,
        );
        if self.fullscreen_player_visible() {
            render_panel(
                self,
                &self.player_view.fullscreen_player.queue_panel,
                &queue.model,
                true,
                current_changed,
                reorderable,
            );
        }
    }

    pub(crate) fn request_queue_page(self: &Rc<Self>) {
        let Some(selected) = self.selected_library().as_deref().cloned() else {
            return;
        };
        let Some(queue) = self.selected_queue() else {
            return;
        };
        let (generation, cancellation) = queue.begin();
        let filter = queue.filter.borrow().clone();
        drop(queue);
        let current = self
            .selected_playback()
            .as_deref()
            .and_then(|player| player.queue.current_position)
            .unwrap_or_default();
        let after = current.saturating_sub(QUEUE_PAGE_SIZE / 2);
        let after = i64::try_from(after).ok().map(|position| position - 1);
        let database = Arc::clone(&selected.database);
        let task = selected.runtime.spawn(async move {
            database
                .queue_page(
                    selected.source_key,
                    after,
                    &filter,
                    QUEUE_PAGE_SIZE,
                    &cancellation,
                )
                .await
        });
        let shell = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(shell) = shell.upgrade() else {
                return;
            };
            let rows = task.await.ok().and_then(Result::ok);
            let Some(rows) = rows else {
                return;
            };
            let preserve_scroll = shell
                .selected_queue()
                .as_deref()
                .is_some_and(|queue| queue_order_rearranged(&queue.rows.borrow(), &rows));
            let sidebar_scroll = queue_panel_scroller(&shell.right_panel.queue_panel)
                .map(|scroller| (scroller.clone(), scroller.vadjustment().value()));
            let fullscreen_scroll =
                queue_panel_scroller(&shell.player_view.fullscreen_player.queue_panel)
                    .map(|scroller| (scroller.clone(), scroller.vadjustment().value()));
            if shell
                .selected_queue()
                .as_deref()
                .is_some_and(|queue| queue.accept(generation, rows))
            {
                shell.render_queue_panel();
                if preserve_scroll {
                    for (scroller, value) in
                        [sidebar_scroll, fullscreen_scroll].into_iter().flatten()
                    {
                        restore_queue_scroll_later(&scroller, value);
                    }
                } else {
                    let current_row = shell.selected_queue().and_then(|queue| {
                        let current = queue.current.borrow().clone()?;
                        queue
                            .rows
                            .borrow()
                            .iter()
                            .position(|row| row.object_id == current.as_str())
                    });
                    for scroller in [
                        queue_panel_scroller(&shell.right_panel.queue_panel),
                        queue_panel_scroller(&shell.player_view.fullscreen_player.queue_panel),
                    ]
                    .into_iter()
                    .flatten()
                    {
                        reveal_queue_current_row_later(&scroller, current_row);
                    }
                }
            }
        });
    }

    pub(crate) fn position_startup_queue_for_reveal(self: &Rc<Self>) -> bool {
        if !self.right_panel.root.is_visible() {
            return false;
        }
        let Some(scroller) = queue_panel_scroller(&self.right_panel.queue_panel) else {
            self.request_queue_page();
            return true;
        };
        let current_row = self.selected_queue().and_then(|queue| {
            let current = queue.current.borrow().clone()?;
            queue
                .rows
                .borrow()
                .iter()
                .position(|row| row.object_id == current.as_str())
        });
        reveal_queue_current_row(&scroller, current_row)
    }

    pub(crate) fn invalidate_queue_panel_render_state(&self) {
        if let Some(queue) = self.selected_queue() {
            queue.invalidate();
        }
    }
}

fn render_panel(
    shell: &Rc<Shell>,
    panel: &gtk::Box,
    model: &gio::ListStore,
    fullscreen: bool,
    reveal_current: bool,
    reorderable: bool,
) {
    let scroller = queue_panel_scroller(panel).unwrap_or_else(|| {
        let scroller = new_queue_scroller();
        panel.append(&scroller);
        scroller
    });
    let stack = scroller
        .child()
        .and_then(|child| child.downcast::<gtk::Stack>().ok());
    let stack = if let Some(stack) = stack {
        stack
    } else {
        let factory = gtk::SignalListItemFactory::new();
        let drag_bindings = Rc::new(RefCell::new(HashMap::<usize, Rc<QueueDragBinding>>::new()));
        let setup_bindings = Rc::clone(&drag_bindings);
        factory.connect_setup(move |_, item| {
            if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
                item.set_child(Some(&gtk::Box::new(gtk::Orientation::Vertical, 0)));
                setup_bindings
                    .borrow_mut()
                    .insert(item.as_ptr() as usize, Rc::new(QueueDragBinding::default()));
            }
        });
        let bind_shell = Rc::clone(shell);
        let bind_bindings = Rc::clone(&drag_bindings);
        factory.connect_bind(move |_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let Some(object) = item
                .item()
                .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
            else {
                return;
            };
            let row = object.borrow::<QueuePageRow>().clone();
            let Some(binding) = bind_bindings
                .borrow()
                .get(&(item.as_ptr() as usize))
                .cloned()
            else {
                return;
            };
            binding.bind(OccurrenceId::new(row.object_id.clone()));
            let current = bind_shell
                .selected_playback()
                .as_deref()
                .and_then(|player| player.queue.current_occurrence.clone());
            let reorderable = bind_shell
                .selected_queue()
                .as_deref()
                .is_some_and(|queue| queue.filter.borrow().trim().is_empty());
            item.set_child(Some(&queue_row(
                &bind_shell,
                &row,
                current.as_ref(),
                fullscreen,
                reorderable,
                binding,
            )));
        });
        let unbind_bindings = Rc::clone(&drag_bindings);
        factory.connect_unbind(move |_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            if let Some(binding) = unbind_bindings.borrow().get(&(item.as_ptr() as usize)) {
                binding.clear();
            }
        });
        factory.connect_teardown(move |_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            drag_bindings.borrow_mut().remove(&(item.as_ptr() as usize));
        });
        let selection = gtk::NoSelection::new(Some(model.clone()));
        let list = gtk::ListView::new(Some(selection), Some(factory));
        list.add_css_class("queue-list");
        list.set_vscroll_policy(gtk::ScrollablePolicy::Minimum);
        let activate = shell.products.playback.queue.clone();
        let activate_model = model.clone();
        list.connect_activate(move |_, position| {
            let Some(object) = activate_model
                .item(position)
                .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
            else {
                return;
            };
            let occurrence = OccurrenceId::new(object.borrow::<QueuePageRow>().object_id.clone());
            let activate = activate.clone();
            glib::idle_add_local_once(move || activate.activate(occurrence));
        });
        let stack = gtk::Stack::new();
        let empty = gtk::Label::new(Some(&tr("Nothing queued")));
        empty.add_css_class("dim-label");
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&list, Some("list"));
        scroller.set_child(Some(&stack));
        stack
    };

    let has_rows = model.n_items() != 0;
    clear_queue_panel_static_children(panel);
    if has_rows {
        panel.prepend(&queue_header_row(fullscreen));
    }
    if scroller.parent().is_none() {
        panel.append(&scroller);
    }
    stack.set_visible_child_name(if has_rows { "list" } else { "empty" });
    if let Some(empty) = stack
        .child_by_name("empty")
        .and_then(|child| child.downcast::<gtk::Label>().ok())
    {
        empty.set_text(&tr(if reorderable {
            "Nothing queued"
        } else {
            "No matching tracks"
        }));
    }
    if let Some(list) = stack.child_by_name("list") {
        let covered = queue_covered_extent(
            !fullscreen && shell.lyrics.panel_visible.get(),
            has_rows,
            shell.right_panel.lyrics_surface.height_request(),
        );
        list.set_margin_bottom(covered);
    }
    if has_rows && reveal_current {
        let current = shell
            .selected_playback()
            .as_deref()
            .and_then(|player| player.queue.current_occurrence.clone());
        let current_row = current.and_then(|current| {
            shell.selected_queue().and_then(|queue| {
                queue
                    .rows
                    .borrow()
                    .iter()
                    .position(|row| row.object_id == current.as_str())
            })
        });
        reveal_queue_current_row_later(&scroller, current_row);
    }
}

fn new_queue_scroller() -> gtk::ScrolledWindow {
    let scroller = gtk::ScrolledWindow::new();
    scroller.add_css_class("queue-scroller");
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroller.set_vexpand(true);
    scroller
}

fn queue_panel_scroller(panel: &gtk::Box) -> Option<gtk::ScrolledWindow> {
    let mut child = panel.first_child();
    while let Some(widget) = child {
        if let Ok(scroller) = widget.clone().downcast::<gtk::ScrolledWindow>() {
            return Some(scroller);
        }
        child = widget.next_sibling();
    }
    None
}

fn clear_queue_panel_static_children(panel: &gtk::Box) {
    let mut child = panel.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if !widget.is::<gtk::ScrolledWindow>() {
            panel.remove(&widget);
        }
    }
}

fn reveal_queue_current_row_later(scroller: &gtk::ScrolledWindow, current_row: Option<usize>) {
    let scroller = scroller.clone();
    glib::idle_add_local_once(move || {
        reveal_queue_current_row(&scroller, current_row);
    });
}

fn restore_queue_scroll_later(scroller: &gtk::ScrolledWindow, value: f64) {
    let scroller = scroller.clone();
    glib::idle_add_local_once(move || {
        let adjustment = scroller.vadjustment();
        let maximum = (adjustment.upper() - adjustment.page_size()).max(adjustment.lower());
        adjustment.set_value(value.clamp(adjustment.lower(), maximum));
    });
}

fn reveal_queue_current_row(scroller: &gtk::ScrolledWindow, current_row: Option<usize>) -> bool {
    let Some(current_row) = current_row else {
        return false;
    };
    let adjustment = scroller.vadjustment();
    let page_size = adjustment.page_size();
    let Some(target) = queue_reveal_target(
        current_row,
        adjustment.value(),
        page_size,
        adjustment.lower(),
        adjustment.upper(),
    ) else {
        return false;
    };
    adjustment.set_value(target);
    true
}

fn queue_reveal_target(
    current_row: usize,
    value: f64,
    page_size: f64,
    lower: f64,
    upper: f64,
) -> Option<f64> {
    if page_size <= 1.0 {
        return None;
    }
    let row_top = current_row as f64 * f64::from(QUEUE_ROW_HEIGHT);
    let row_bottom = row_top + f64::from(QUEUE_ROW_HEIGHT);
    let comfort_top = value + page_size * 0.25;
    let comfort_bottom = value + page_size * 0.70;
    if row_top >= comfort_top && row_bottom <= comfort_bottom {
        return None;
    }
    let target = row_top - page_size * 0.42;
    let maximum = (upper - page_size).max(lower);
    Some(target.clamp(lower, maximum))
}

fn queue_covered_extent(lyrics_visible: bool, has_rows: bool, requested: i32) -> i32 {
    if lyrics_visible && has_rows {
        requested.max(0)
    } else {
        0
    }
}

fn changed_queue_positions(previous: &[QueuePageRow], next: &[QueuePageRow]) -> Vec<u32> {
    let previous = previous
        .iter()
        .map(|row| (row.object_id.as_str(), row))
        .collect::<HashMap<_, _>>();
    next.iter()
        .enumerate()
        .filter_map(|(position, row)| {
            previous
                .get(row.object_id.as_str())
                .is_some_and(|previous| *previous != row)
                .then_some(position as u32)
        })
        .collect()
}

fn queue_order_rearranged(previous: &[QueuePageRow], next: &[QueuePageRow]) -> bool {
    if previous.len() != next.len()
        || previous
            .iter()
            .zip(next)
            .all(|(left, right)| left.object_id == right.object_id)
    {
        return false;
    }
    let mut previous = previous
        .iter()
        .map(|row| row.object_id.as_str())
        .collect::<Vec<_>>();
    let mut next = next
        .iter()
        .map(|row| row.object_id.as_str())
        .collect::<Vec<_>>();
    previous.sort_unstable();
    next.sort_unstable();
    previous == next
}

fn queue_row(
    shell: &Rc<Shell>,
    row: &QueuePageRow,
    current: Option<&OccurrenceId>,
    fullscreen: bool,
    reorderable: bool,
    drag_binding: Rc<QueueDragBinding>,
) -> gtk::Widget {
    let occurrence = OccurrenceId::new(row.object_id.clone());
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    root.add_css_class("queue-row");
    root.set_height_request(QUEUE_ROW_HEIGHT);
    root.set_hexpand(true);
    root.set_halign(gtk::Align::Fill);
    root.set_valign(gtk::Align::Center);
    root.set_focusable(true);
    if !fullscreen {
        root.set_margin_end(QUEUE_SIDEBAR_END_INSET);
    }
    if current == Some(&occurrence) {
        root.add_css_class("queue-row-current");
    }
    root.update_property(&[gtk::accessible::Property::Label(&format!(
        "{} {}",
        row.title, row.artist
    ))]);

    let content: gtk::Widget = if fullscreen {
        fullscreen_queue_content(shell, row)
    } else {
        sidebar_queue_content(shell, row)
    };
    root.append(&content);

    if reorderable {
        let source = gtk::DragSource::builder()
            .actions(gtk::gdk::DragAction::MOVE)
            .build();
        let source_binding = Rc::clone(&drag_binding);
        source.connect_prepare(move |_, _, _| {
            let occurrence = source_binding.occurrence()?;
            Some(gtk::gdk::ContentProvider::for_value(
                &occurrence.to_string().to_value(),
            ))
        });
        root.add_controller(source);
        let drop = gtk::DropTarget::new(String::static_type(), gtk::gdk::DragAction::MOVE);
        let queue = shell.products.playback.queue.clone();
        let root_for_drop = root.downgrade();
        let target_binding = Rc::clone(&drag_binding);
        drop.connect_drop(move |_, value, _, y| {
            let Ok(object_id) = value.get::<String>() else {
                return false;
            };
            let Some(root) = root_for_drop.upgrade() else {
                return false;
            };
            let Some(request) = target_binding.reorder_request(
                OccurrenceId::new(object_id),
                y > f64::from(root.height()) / 2.0,
            ) else {
                return false;
            };
            queue.reorder(request);
            true
        });
        root.add_controller(drop);
    }

    let context_shell = Rc::clone(shell);
    let context = queue_playback_media(row);
    let primary_artist = row.primary_artist_key;
    let artist_name = row.artist.clone();
    let context_occurrence = occurrence.clone();
    install_context_menu_openers(
        &root,
        Rc::new(move |target, position| {
            present_queue_track_context_menu(
                target,
                &context_shell,
                context.clone(),
                primary_artist,
                &artist_name,
                context_occurrence.clone(),
                position,
            );
        }),
    );
    root.upcast()
}

fn sidebar_queue_content(shell: &Rc<Shell>, row: &QueuePageRow) -> gtk::Widget {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    content.set_hexpand(true);
    let cover = ArtworkTile::new(50);
    shell.bind_artwork_tile(
        &cover,
        row.artwork_binding
            .as_deref()
            .map(ArtworkBinding::opaque)
            .unwrap_or_default(),
        50,
        THUMB_COVER_SIZE,
    );
    content.append(&cover.widget());
    let labels = gtk::Box::new(gtk::Orientation::Vertical, 2);
    labels.set_hexpand(true);
    labels.set_valign(gtk::Align::Center);
    let title = gtk::Label::new(Some(&row.title));
    title.add_css_class("queue-title");
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let artist = queue_link_label(&row.artist);
    DetailLinkBinding::new(&artist, shell).bind(DetailLinks::route(
        &row.artist,
        row.primary_artist_key.map(Route::ArtistDetail),
    ));
    labels.append(&title);
    labels.append(&artist);
    content.append(&labels);
    let year = gtk::Label::new(row.year.map(|year| year.to_string()).as_deref());
    year.add_css_class("muted");
    year.add_css_class("queue-year");
    year.set_xalign(1.0);
    year.set_width_chars(4);
    year.set_halign(gtk::Align::End);
    content.append(&year);
    content.upcast()
}

fn fullscreen_queue_content(shell: &Rc<Shell>, row: &QueuePageRow) -> gtk::Widget {
    let columns = fullscreen_queue_row_box();
    let cover = ArtworkTile::new(QUEUE_FULLSCREEN_COVER_COLUMN_WIDTH);
    shell.bind_artwork_tile(
        &cover,
        row.artwork_binding
            .as_deref()
            .map(ArtworkBinding::opaque)
            .unwrap_or_default(),
        QUEUE_FULLSCREEN_COVER_COLUMN_WIDTH,
        THUMB_COVER_SIZE,
    );
    columns.append(&cover.widget());

    let identity = gtk::Box::new(gtk::Orientation::Vertical, 2);
    identity.set_width_request(1);
    identity.set_valign(gtk::Align::Center);
    let title = gtk::Label::new(Some(&row.title));
    title.add_css_class("queue-title");
    title.set_xalign(0.0);
    title.set_width_request(1);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let artist = queue_link_label(&row.artist);
    artist.set_width_request(1);
    DetailLinkBinding::new(&artist, shell).bind(DetailLinks::route(
        &row.artist,
        row.primary_artist_key.map(Route::ArtistDetail),
    ));
    identity.append(&title);
    identity.append(&artist);
    let album = fullscreen_queue_text_cell(&row.album);
    let duration = fullscreen_queue_fixed_cell(
        &row.duration_millis
            .map(|duration| crate::format_duration((duration.max(0) / 1_000) as u32))
            .unwrap_or_default(),
        QUEUE_DURATION_COLUMN_WIDTH,
    );
    let year = fullscreen_queue_fixed_cell(
        &row.year.map(|year| year.to_string()).unwrap_or_default(),
        QUEUE_YEAR_COLUMN_WIDTH,
    );
    columns.append(&identity);
    columns.append(&album);
    columns.append(&duration);
    columns.append(&year);

    let favorite_cell = gtk::CenterBox::new();
    favorite_cell.add_css_class("queue-favorite-cell");
    favorite_cell.set_width_request(QUEUE_FAVORITE_COLUMN_WIDTH);
    let favorite = favorite_icon_button("Favorite");
    favorite.add_css_class("queue-favorite-button");
    set_favorite_button_active(&favorite, row.favorite.unwrap_or(false));
    if let Some(key) = row.track_key {
        let favorite_shell = Rc::clone(shell);
        favorite.connect_clicked(move |button| {
            favorite_shell.set_favorite_with_feedback(
                library::FavoriteTarget::Track(key),
                !crate::favorites::favorite_button_is_active(button),
                Some(button),
            );
        });
    } else {
        favorite.set_sensitive(false);
    }
    favorite_cell.set_center_widget(Some(&favorite));
    columns.append(&favorite_cell);

    fullscreen_queue_column_owner(
        &columns,
        QueueFullscreenColumnWidgets {
            title: identity.upcast(),
            album: album.upcast(),
            year: year.upcast(),
        },
    )
}

fn queue_link_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("muted");
    label.add_css_class("queue-link");
    label.set_xalign(0.0);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label
}

fn fullscreen_queue_row_box() -> gtk::Box {
    let row = gtk::Box::new(
        gtk::Orientation::Horizontal,
        QUEUE_FULLSCREEN_COLUMN_SPACING,
    );
    row.set_margin_start(QUEUE_FULLSCREEN_ROW_HORIZONTAL_PADDING / 2);
    row.set_margin_end(QUEUE_FULLSCREEN_ROW_HORIZONTAL_PADDING / 2);
    row.set_hexpand(true);
    row.set_halign(gtk::Align::Fill);
    row.set_valign(gtk::Align::Center);
    row
}

fn fullscreen_queue_text_cell(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("muted");
    label.set_xalign(0.0);
    label.set_width_request(1);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label
}

fn fullscreen_queue_fixed_cell(text: &str, width: i32) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("muted");
    label.set_xalign(0.5);
    label.set_width_request(width);
    label
}

fn fullscreen_queue_column_owner(
    root: &gtk::Box,
    columns: QueueFullscreenColumnWidgets,
) -> gtk::Widget {
    let initial = fullscreen_queue_column_widths(1);
    columns.apply(initial);
    let last = Cell::new(initial);
    allocation_owner(root, move |width, _| {
        let widths = fullscreen_queue_column_widths(width.max(1));
        if last.replace(widths) != widths {
            columns.apply(widths);
        }
    })
    .upcast()
}

fn fullscreen_queue_column_widths(available_width: i32) -> QueueFullscreenColumnWidths {
    let full_fixed = fullscreen_queue_fixed_width(true, true);
    let full_variable = available_width.saturating_sub(full_fixed);
    if full_variable >= QUEUE_FULLSCREEN_TITLE_MIN_WIDTH + QUEUE_FULLSCREEN_ALBUM_MIN_WIDTH {
        return split_queue_text_width(full_variable, true);
    }
    let compact_fixed = fullscreen_queue_fixed_width(true, false);
    let compact_variable = available_width.saturating_sub(compact_fixed);
    if compact_variable >= QUEUE_FULLSCREEN_TITLE_MIN_WIDTH + QUEUE_FULLSCREEN_ALBUM_MIN_WIDTH {
        return split_queue_text_width(compact_variable, false);
    }
    QueueFullscreenColumnWidths {
        title: available_width
            .saturating_sub(fullscreen_queue_fixed_width(false, false))
            .max(1),
        album: 0,
        show_album: false,
        show_year: false,
    }
}

fn fullscreen_queue_fixed_width(show_album: bool, show_year: bool) -> i32 {
    let columns = 4 + i32::from(show_album) + i32::from(show_year);
    QUEUE_FULLSCREEN_ROW_HORIZONTAL_PADDING
        + QUEUE_FULLSCREEN_COVER_COLUMN_WIDTH
        + QUEUE_DURATION_COLUMN_WIDTH
        + QUEUE_FAVORITE_COLUMN_WIDTH
        + if show_year {
            QUEUE_YEAR_COLUMN_WIDTH
        } else {
            0
        }
        + (columns - 1) * QUEUE_FULLSCREEN_COLUMN_SPACING
}

fn split_queue_text_width(width: i32, show_year: bool) -> QueueFullscreenColumnWidths {
    let min_total = QUEUE_FULLSCREEN_TITLE_MIN_WIDTH + QUEUE_FULLSCREEN_ALBUM_MIN_WIDTH;
    if width <= min_total {
        return QueueFullscreenColumnWidths {
            title: QUEUE_FULLSCREEN_TITLE_MIN_WIDTH,
            album: QUEUE_FULLSCREEN_ALBUM_MIN_WIDTH,
            show_album: true,
            show_year,
        };
    }
    let base_total = QUEUE_FULLSCREEN_TITLE_COLUMN_WIDTH + QUEUE_FULLSCREEN_ALBUM_COLUMN_WIDTH;
    if width <= base_total {
        let title = ((i64::from(width) * i64::from(QUEUE_FULLSCREEN_TITLE_COLUMN_WIDTH))
            / i64::from(base_total)) as i32;
        return QueueFullscreenColumnWidths {
            title: title.max(QUEUE_FULLSCREEN_TITLE_MIN_WIDTH),
            album: (width - title).max(QUEUE_FULLSCREEN_ALBUM_MIN_WIDTH),
            show_album: true,
            show_year,
        };
    }
    let extra = width - base_total;
    QueueFullscreenColumnWidths {
        title: QUEUE_FULLSCREEN_TITLE_COLUMN_WIDTH + extra / 2,
        album: QUEUE_FULLSCREEN_ALBUM_COLUMN_WIDTH + extra - extra / 2,
        show_album: true,
        show_year,
    }
}

fn queue_header_row(fullscreen: bool) -> gtk::Widget {
    if !fullscreen {
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header.add_css_class("queue-header");
        header.set_margin_end(QUEUE_SIDEBAR_END_INSET);
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_width_request(50);
        header.append(&spacer);
        let title = gtk::Label::new(Some(&tr("Title").to_uppercase()));
        title.add_css_class("muted");
        title.set_xalign(0.0);
        title.set_hexpand(true);
        header.append(&title);
        let year = gtk::Label::new(Some(&tr("Year").to_uppercase()));
        year.add_css_class("muted");
        year.add_css_class("queue-year");
        year.set_xalign(1.0);
        year.set_width_chars(4);
        year.set_halign(gtk::Align::End);
        header.append(&year);
        return header.upcast();
    }
    let header = fullscreen_queue_row_box();
    header.add_css_class("queue-header");
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_width_request(QUEUE_FULLSCREEN_COVER_COLUMN_WIDTH);
    let title = gtk::Label::new(Some(&tr("Title").to_uppercase()));
    title.add_css_class("muted");
    title.set_xalign(0.0);
    let album = gtk::Label::new(Some(&tr("Album").to_uppercase()));
    album.add_css_class("muted");
    album.set_xalign(0.0);
    let duration = gtk::Image::from_icon_name("rufin-preferences-system-time-symbolic");
    duration.add_css_class("muted");
    duration.set_width_request(QUEUE_DURATION_COLUMN_WIDTH);
    let year = gtk::Label::new(Some(&tr("Year").to_uppercase()));
    year.add_css_class("muted");
    year.set_width_request(QUEUE_YEAR_COLUMN_WIDTH);
    let favorite = gtk::Image::from_icon_name("rufin-non-starred-symbolic");
    favorite.add_css_class("muted");
    favorite.set_width_request(QUEUE_FAVORITE_COLUMN_WIDTH);
    header.append(&spacer);
    header.append(&title);
    header.append(&album);
    header.append(&duration);
    header.append(&year);
    header.append(&favorite);
    fullscreen_queue_column_owner(
        &header,
        QueueFullscreenColumnWidgets {
            title: title.upcast(),
            album: album.upcast(),
            year: year.upcast(),
        },
    )
}

fn queue_playback_media(row: &QueuePageRow) -> PlaybackMedia {
    PlaybackMedia {
        source_id: String::new(),
        track_key: row.track_key,
        track_object_id: row.track_object_id.clone(),
        title: row.title.clone(),
        artist: row.artist.clone(),
        album: row.album.clone(),
        album_display_artist: row.album_display_artist.clone(),
        album_key: row.album_key,
        primary_artist_key: row.primary_artist_key,
        media_uri: row.media_uri.clone(),
        artwork_binding: row.artwork_binding.clone(),
        duration_millis: row.duration_millis.unwrap_or_default(),
        disc_number: row.disc_number,
        track_number: row.track_number,
        year: row.year,
        release_date: row.release_date.clone(),
        favorite: row.favorite,
        rating: row.rating,
        is_downloaded: row.is_downloaded,
        source_format: row.source_format.clone(),
        musicbrainz_recording_id: row.musicbrainz_recording_id.clone(),
        musicbrainz_release_track_id: row.musicbrainz_release_track_id.clone(),
        musicbrainz_album_id: row.musicbrainz_album_id.clone(),
        musicbrainz_release_group_id: row.musicbrainz_release_group_id.clone(),
        primary_artist_musicbrainz_id: row.primary_artist_musicbrainz_id.clone(),
        cue_path: row.cue_path.clone(),
        cue_start_millis: row.cue_start_millis,
        cue_end_millis: row.cue_end_millis,
        artist_links: row
            .primary_artist_key
            .map(|artist_key| {
                vec![library::TrackArtistLink {
                    artist_key,
                    name: row.artist.clone(),
                }]
            })
            .unwrap_or_default(),
    }
}

pub(crate) fn clear_queue_panel_children(panel: &gtk::Box) {
    while let Some(child) = panel.first_child() {
        panel.remove(&child);
    }
}

pub(crate) fn connect_queue_panel_controls(shell: &Rc<Shell>) {
    let sidebar_shell = Rc::clone(shell);
    shell
        .right_panel
        .queue_search
        .connect_search_changed(move |entry| {
            if let Some(queue) = sidebar_shell.selected_queue() {
                queue.filter.replace(entry.text().trim().to_string());
            }
            if let Some(scroller) = queue_panel_scroller(&sidebar_shell.right_panel.queue_panel) {
                scroller.vadjustment().set_value(0.0);
            }
            sidebar_shell.request_queue_page();
        });
    let queue = shell.products.playback.queue.clone();
    shell
        .right_panel
        .queue_clear_button
        .connect_clicked(move |_| queue.clear());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, title: &str) -> QueuePageRow {
        QueuePageRow {
            occurrence_key: library::QueueOccurrenceKey::from_raw(1),
            object_id: id.to_string(),
            position: 0,
            traversal_position: 0,
            provenance: library::QueueProvenance::Manual,
            media: library::QueueMedia {
                source_id: String::new(),
                track_key: None,
                track_object_id: format!("track-{id}"),
                title: title.to_string(),
                artist: String::new(),
                album: String::new(),
                album_display_artist: None,
                album_key: None,
                album_object_id: None,
                primary_artist_key: None,
                primary_artist_object_id: None,
                primary_artist_musicbrainz_id: None,
                download_media_uri: None,
                mapping_media_uri: None,
                source_media_uri: None,
                media_uri: None,
                artwork_binding: None,
                duration_millis: None,
                disc_number: None,
                track_number: None,
                year: None,
                release_date: None,
                favorite: None,
                source_format: None,
                musicbrainz_recording_id: None,
                musicbrainz_release_track_id: None,
                musicbrainz_album_id: None,
                musicbrainz_release_group_id: None,
                cue_path: None,
                cue_start_millis: None,
                cue_end_millis: None,
                artist_links: Vec::new(),
            },
            rating: None,
            is_downloaded: false,
        }
    }

    #[test]
    fn queue_page_notifies_only_changed_stable_rows() {
        let previous = vec![row("one", "One"), row("two", "Two"), row("three", "Three")];
        let mut next = previous.clone();
        next[1].favorite = Some(true);
        assert_eq!(changed_queue_positions(&previous, &next), [1]);
        assert!(changed_queue_positions(&next, &next).is_empty());
    }

    #[test]
    fn queue_drag_binding_uses_the_current_recycled_row() {
        let binding = QueueDragBinding::default();
        binding.bind(OccurrenceId::new("old-target"));
        assert_eq!(
            binding
                .reorder_request(OccurrenceId::new("dragged"), false)
                .expect("bound old target")
                .target,
            QueueReorderTarget::Before(OccurrenceId::new("old-target"))
        );

        binding.bind(OccurrenceId::new("new-target"));
        assert_eq!(
            binding
                .reorder_request(OccurrenceId::new("dragged"), true)
                .expect("bound new target")
                .target,
            QueueReorderTarget::After(OccurrenceId::new("new-target"))
        );
        binding.clear();
        assert!(
            binding
                .reorder_request(OccurrenceId::new("dragged"), false)
                .is_none()
        );
    }

    #[test]
    fn queue_current_change_replaces_only_old_and_new_model_rows() {
        let state = QueueState::new();
        let (generation, _) = state.begin();
        assert!(state.accept(
            generation,
            vec![row("one", "One"), row("two", "Two"), row("three", "Three")]
        ));
        let changes = Rc::new(RefCell::new(Vec::new()));
        let observed = Rc::clone(&changes);
        state
            .model
            .connect_items_changed(move |_, position, removed, added| {
                observed.borrow_mut().push((position, removed, added));
            });
        assert!(state.update_current(Some(OccurrenceId::new("one"))));
        changes.borrow_mut().clear();
        assert!(state.update_current(Some(OccurrenceId::new("three"))));
        let positions = changes
            .borrow()
            .iter()
            .map(|(position, _, _)| *position)
            .collect::<Vec<_>>();
        assert_eq!(positions, [0, 2]);
    }

    #[test]
    fn queue_track_and_artwork_changes_keep_stable_row_objects() {
        let state = QueueState::new();
        let (generation, _) = state.begin();
        assert!(state.accept(generation, vec![row("one", "One"), row("two", "Two")]));
        let first = state.model.item(0).expect("first Queue object");
        let second = state.model.item(1).expect("second Queue object");
        let mut next = vec![row("one", "One"), row("two", "Changed")];
        next[1].artwork_binding = Some(vec![1, 2, 3]);
        let (generation, _) = state.begin();
        assert!(state.accept(generation, next));
        assert_eq!(first.as_ptr(), state.model.item(0).unwrap().as_ptr());
        assert_eq!(second.as_ptr(), state.model.item(1).unwrap().as_ptr());
    }

    #[test]
    fn dropping_queue_state_releases_sidebar_and_hidden_fullscreen_model() {
        let state = QueueState::new();
        let weak = state.model.downgrade();
        drop(state);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn lyrics_overlap_adds_one_queue_scroll_extent() {
        assert_eq!(queue_covered_extent(true, true, 240), 240);
        assert_eq!(queue_covered_extent(false, true, 240), 0);
        assert_eq!(queue_covered_extent(true, false, 240), 0);
        assert_eq!(queue_covered_extent(true, true, -1), 0);
    }

    #[test]
    fn queue_current_reveal_prefers_comfort_band_when_possible() {
        assert_eq!(queue_reveal_target(5, 0.0, 500.0, 0.0, 2_000.0), None);
        let target = queue_reveal_target(20, 0.0, 500.0, 0.0, 2_000.0)
            .expect("distant current row needs reveal");
        assert!(target > 0.0);
        assert!(target <= 1_500.0);
    }

    #[test]
    fn fullscreen_queue_columns_never_outgrow_their_allocation() {
        for available in [320, 542, 900] {
            let widths = fullscreen_queue_column_widths(available);
            let used = fullscreen_queue_fixed_width(widths.show_album, widths.show_year)
                + widths.title
                + if widths.show_album { widths.album } else { 0 };
            assert!(used <= available, "{used} > {available}");
            if widths.show_album {
                assert!(widths.title >= QUEUE_FULLSCREEN_TITLE_MIN_WIDTH);
                assert!(widths.album >= QUEUE_FULLSCREEN_ALBUM_MIN_WIDTH);
            }
        }
    }
}
