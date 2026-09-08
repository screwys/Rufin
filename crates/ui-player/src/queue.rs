use adw::prelude::*;
use gtk::{gio, glib};
use library::QueuePageRow;
use playback::{OccurrenceId, QueueReorderRequest, QueueReorderTarget};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    sync::Arc,
};
use ui_shared::sparse_model::SparseObjectItem;
pub struct QueueState {
    pub window: RefCell<Vec<Arc<QueuePageRow>>>,
    pub rows: RefCell<Vec<Arc<QueuePageRow>>>,
    pub model: gio::ListStore,
    pub selection: RefCell<Option<gtk::MultiSelection>>,
    pub filter: RefCell<String>,
    pub generation: Cell<u64>,
    pub current: RefCell<Option<OccurrenceId>>,
    pub render_queued: Cell<bool>,
    reveal_current: [Cell<bool>; 2],
    hydration: RefCell<Option<tokio::task::AbortHandle>>,
}

impl QueueState {
    pub fn new() -> Self {
        let model = gio::ListStore::new::<SparseObjectItem>();
        Self {
            window: RefCell::new(Vec::new()),
            rows: RefCell::new(Vec::new()),
            model,
            selection: RefCell::new(None),
            filter: RefCell::new(String::new()),
            generation: Cell::new(0),
            current: RefCell::new(None),
            render_queued: Cell::new(false),
            reveal_current: Default::default(),
            hydration: RefCell::new(None),
        }
    }

    pub fn begin(&self) -> u64 {
        if let Some(task) = self.hydration.take() {
            task.abort();
        }
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        generation
    }

    pub fn selection_model(&self) -> gtk::MultiSelection {
        if let Some(selection) = self.selection.borrow().as_ref() {
            return selection.clone();
        }
        let selection = gtk::MultiSelection::new(Some(self.model.clone()));
        self.selection.replace(Some(selection.clone()));
        selection
    }

    pub fn accept(&self, generation: u64, rows: Vec<QueuePageRow>) -> bool {
        if self.generation.get() != generation {
            return false;
        }
        // Playback can publish the selected occurrence before the surrounding
        // queue arrives. Reveal again when that window changes, even if the
        // playing occurrence itself is unchanged. Metadata refreshes don't scroll.
        if self
            .window
            .borrow()
            .iter()
            .map(|row| &row.occurrence)
            .ne(rows.iter().map(|row| &row.occurrence))
        {
            for pending in &self.reveal_current {
                pending.set(true);
            }
        }
        let mut previous = self
            .window
            .take()
            .into_iter()
            .map(|row| (row.occurrence.clone(), row))
            .collect::<HashMap<_, _>>();
        self.window.replace(
            rows.into_iter()
                .map(|row| {
                    previous
                        .remove(&row.occurrence)
                        .filter(|previous| previous.as_ref() == &row)
                        .unwrap_or_else(|| Arc::new(row))
                })
                .collect(),
        );
        self.apply_filter();
        true
    }

    pub fn apply_filter(&self) {
        let filter = self.filter.borrow().clone();
        let rows = filter_queue_window(&self.window.borrow(), &filter);
        let selected = self.selection.borrow().as_ref().map(|selection| {
            queue_rows_at_positions(&self.rows.borrow(), &selection.selection())
                .into_iter()
                .map(|row| row.occurrence.clone())
                .collect::<Vec<_>>()
        });
        let previous = (0..self.model.n_items())
            .map(|position| {
                self.model
                    .item(position)
                    .and_downcast::<SparseObjectItem>()
                    .expect("Queue row object")
            })
            .collect::<Vec<_>>();
        let mut existing = previous
            .iter()
            .map(|object| {
                let row = object.value::<Arc<QueuePageRow>>().expect("Queue row");
                (row.occurrence.clone(), object.clone())
            })
            .collect::<HashMap<_, _>>();
        // Reserve surviving occurrences before recycling slots from removed entries.
        let reserved = rows
            .iter()
            .map(|row| existing.remove(&row.occurrence))
            .collect::<Vec<_>>();
        let mut retired = previous.iter().filter(|object| {
            let row = object.value::<Arc<QueuePageRow>>().expect("Queue row");
            existing.contains_key(&row.occurrence)
        });
        let objects = rows
            .iter()
            .zip(reserved)
            .map(|(row, object)| {
                let object = object
                    .or_else(|| retired.next().cloned())
                    .unwrap_or_else(|| SparseObjectItem::new(Arc::clone(row), true));
                let previous = object.value::<Arc<QueuePageRow>>().expect("Queue row");
                if !Arc::ptr_eq(&previous, row) {
                    object.replace(Arc::clone(row), true);
                }
                object
            })
            .collect::<Vec<_>>();
        let prefix = previous
            .iter()
            .zip(&objects)
            .take_while(|(left, right)| left == right)
            .count();
        let suffix = previous
            .len()
            .saturating_sub(prefix)
            .min(objects.len().saturating_sub(prefix));
        let suffix = previous
            .iter()
            .rev()
            .zip(objects.iter().rev())
            .take(suffix)
            .take_while(|(left, right)| left == right)
            .count();
        let old_middle = previous.len().saturating_sub(prefix + suffix);
        let next_end = objects.len().saturating_sub(suffix);
        if old_middle != 0 || prefix != next_end {
            self.model
                .splice(prefix as u32, old_middle as u32, &objects[prefix..next_end]);
        }
        self.rows.replace(rows);
        if let Some(selected) = selected {
            let selection = self.selection.borrow().as_ref().cloned().unwrap();
            let positions = gtk::Bitset::new_empty();
            for (position, row) in self.rows.borrow().iter().enumerate() {
                if selected.contains(&row.occurrence) {
                    positions.add(position as u32);
                }
            }
            selection.set_selection(&positions, &gtk::Bitset::new_range(0, self.model.n_items()));
        }
    }

    pub fn update_current(&self, current: Option<OccurrenceId>) -> bool {
        let previous = self.current.replace(current.clone());
        if previous == current {
            return false;
        }
        for occurrence in [previous, current].into_iter().flatten() {
            let row = self
                .rows
                .borrow()
                .iter()
                .enumerate()
                .find(|(_, row)| row.occurrence == occurrence)
                .map(|(position, row)| (position, row.clone()));
            if let Some((position, row)) = row {
                let object = self
                    .model
                    .item(position as u32)
                    .and_downcast::<SparseObjectItem>()
                    .expect("Queue row object");
                object.replace(row, true);
            }
        }
        true
    }

    pub fn selected_rows_for(&self, clicked: &OccurrenceId) -> Option<QueueSelectionSnapshot> {
        let positions = self.selection.borrow().as_ref()?.selection();
        let rows = queue_rows_at_positions(&self.rows.borrow(), &positions);
        (rows.len() > 1 && rows.iter().any(|row| &row.occurrence == clicked))
            .then(|| QueueSelectionSnapshot::new(rows))
    }

    pub fn dragged_rows_for(&self, clicked: &OccurrenceId) -> Option<QueueSelectionSnapshot> {
        self.selected_rows_for(clicked).or_else(|| {
            self.rows
                .borrow()
                .iter()
                .find(|row| &row.occurrence == clicked)
                .cloned()
                .map(|row| QueueSelectionSnapshot::new(vec![row]))
        })
    }

    pub fn selected_media_uris(&self) -> Option<Arc<[String]>> {
        let positions = self.selection.borrow().as_ref()?.selection();
        let rows = queue_rows_at_positions(&self.rows.borrow(), &positions);
        let media_uris = rows
            .into_iter()
            .map(|row| row.media_uri.clone())
            .collect::<Vec<_>>();
        (!media_uris.is_empty()).then(|| media_uris.into())
    }

    pub fn selected_occurrences(&self) -> Option<Arc<[OccurrenceId]>> {
        let positions = self.selection.borrow().as_ref()?.selection();
        let occurrences = queue_rows_at_positions(&self.rows.borrow(), &positions)
            .into_iter()
            .map(|row| row.occurrence.clone())
            .collect::<Vec<_>>();
        (!occurrences.is_empty()).then(|| occurrences.into())
    }
}

impl Drop for QueueState {
    fn drop(&mut self) {
        if let Some(task) = self.hydration.get_mut().take() {
            task.abort();
        }
    }
}

#[derive(Clone)]
pub struct QueueSelectionSnapshot {
    pub occurrences: Arc<[OccurrenceId]>,
    pub media_uris: Arc<[String]>,
}

impl QueueSelectionSnapshot {
    pub fn new(rows: Vec<Arc<QueuePageRow>>) -> Self {
        let occurrences = rows
            .iter()
            .map(|row| row.occurrence.clone())
            .collect::<Vec<_>>()
            .into();
        let media_uris = rows
            .iter()
            .map(|row| row.media_uri.clone())
            .collect::<Vec<_>>()
            .into();
        Self {
            occurrences,
            media_uris,
        }
    }
}

fn queue_rows_at_positions(
    rows: &[Arc<QueuePageRow>],
    positions: &gtk::Bitset,
) -> Vec<Arc<QueuePageRow>> {
    let Some((iter, first)) = gtk::BitsetIter::init_first(positions) else {
        return Vec::new();
    };
    queue_rows_for_indexes(rows, std::iter::once(first).chain(iter))
}

pub fn queue_rows_for_indexes(
    rows: &[Arc<QueuePageRow>],
    positions: impl Iterator<Item = u32>,
) -> Vec<Arc<QueuePageRow>> {
    positions
        .filter_map(|position| rows.get(position as usize).cloned())
        .collect()
}

fn filter_queue_window(rows: &[Arc<QueuePageRow>], filter: &str) -> Vec<Arc<QueuePageRow>> {
    let filter: String = filter.trim().chars().take(256).collect();
    if filter.is_empty() {
        return rows.to_vec();
    }
    let pattern = regex::RegexBuilder::new(&regex::escape(&filter))
        .case_insensitive(true)
        .build()
        .expect("Literal queue search");
    rows.iter()
        .filter(|row| {
            [&row.title, &row.artist, &row.album]
                .iter()
                .any(|text| pattern.is_match(text))
        })
        .cloned()
        .collect()
}

use ui_shared::layout::allocation_owner;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueueFullscreenColumnMode {
    TitleOnly,
    Album,
    AlbumAndYear,
}
pub struct QueueFullscreenColumnWidgets {
    pub album: gtk::Widget,
    pub year: gtk::Widget,
}
impl QueueFullscreenColumnWidgets {
    pub fn apply(&self, mode: QueueFullscreenColumnMode) {
        self.album
            .set_visible(mode != QueueFullscreenColumnMode::TitleOnly);
        self.year
            .set_visible(mode == QueueFullscreenColumnMode::AlbumAndYear);
    }
}
pub fn fullscreen_queue_column_owner(
    root: &gtk::Box,
    columns: QueueFullscreenColumnWidgets,
) -> gtk::Widget {
    let initial = fullscreen_queue_column_mode(1);
    columns.apply(initial);
    let last = Cell::new(initial);
    allocation_owner(root, move |width, _| {
        let mode = fullscreen_queue_column_mode(width.max(1));
        if last.replace(mode) != mode {
            columns.apply(mode);
        }
    })
    .upcast()
}
pub fn fullscreen_queue_column_mode(available_width: i32) -> QueueFullscreenColumnMode {
    if available_width >= QUEUE_FULLSCREEN_SHOW_YEAR_WIDTH {
        QueueFullscreenColumnMode::AlbumAndYear
    } else if available_width >= QUEUE_FULLSCREEN_SHOW_ALBUM_WIDTH {
        QueueFullscreenColumnMode::Album
    } else {
        QueueFullscreenColumnMode::TitleOnly
    }
}
pub const QUEUE_FULLSCREEN_SHOW_ALBUM_WIDTH: i32 = 572;
pub const QUEUE_FULLSCREEN_SHOW_YEAR_WIDTH: i32 = 652;

use artwork::ArtworkBinding;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use localization::tr;
use ui_shared::{
    artwork::THUMB_COVER_SIZE,
    detail_links::DetailLinks,
    favorites::set_favorite_button_active,
    interactions::install_context_menu_openers,
    layout::WINDOW_CHROME_MARGIN_END,
    localization::bind_widget_tooltip,
    media_drag::{
        MediaDragPreviewBinding, MediaDragSource, media_drag_content_provider, media_drag_source,
    },
    recycled_cells::{RecycledArtworkCell, RecycledTextCell},
    route::Route,
    sparse_model::connect_sparse_bind,
};
const QUEUE_FULLSCREEN_COVER_COLUMN_WIDTH: i32 = 50;

ui_shared::composite_box!(
    pub QueueSidebarRow,
    sidebar_row_imp,
    "RufinQueueSidebarRow",
    "/io/github/screwys/Rufin/ui/player/queue_sidebar_row.ui",
    {
        cover: ui_shared::recycled_cells::RecycledArtworkCell,
        title: gtk::Label,
        artist: ui_shared::recycled_cells::RecycledTextCell,
        year: gtk::Label,
    }
);

impl QueueSidebarRow {
    fn for_item(shell: &Rc<crate::PlayerUi>, item: &gtk::ListItem) -> Self {
        RecycledArtworkCell::ensure_type();
        RecycledTextCell::ensure_type();
        let row = Self::new();
        row.set_margin_end(WINDOW_CHROME_MARGIN_END);
        row.imp().cover.artwork().set_square_size(50);
        row.imp().artist.enable_links(Rc::clone(&shell.navigate));
        row.imp().artist.label().add_css_class("muted");
        row.imp().artist.label().add_css_class("queue-link");
        install_queue_row_interactions(
            row.upcast_ref(),
            shell,
            item,
            &row.imp().cover.artwork().drag_paintable_source(),
        );
        row
    }

    fn bind(
        &self,
        shell: &Rc<crate::PlayerUi>,
        row: &QueuePageRow,
        current: Option<&OccurrenceId>,
    ) {
        bind_queue_row_root(self.upcast_ref(), row, current);
        bind_queue_artwork(shell, &self.imp().cover, row);
        self.imp().title.set_text(&row.title);
        self.imp().artist.bind_links(DetailLinks::route(
            &row.artist,
            row.primary_artist_media_uri
                .clone()
                .map(Route::ArtistDetail),
        ));
        self.imp().year.set_text(
            row.year
                .map(|year| year.to_string())
                .as_deref()
                .unwrap_or(""),
        );
    }

    fn clear(&self, shell: &Rc<crate::PlayerUi>) {
        clear_queue_row_root(self.upcast_ref());
        shell
            .artwork
            .clear_artwork_tile(&self.imp().cover.artwork());
        self.imp().title.set_text("");
        self.imp().artist.clear();
        self.imp().year.set_text("");
    }
}

ui_shared::composite_box!(
    pub QueueFullscreenRow,
    fullscreen_row_imp,
    "RufinQueueFullscreenRow",
    "/io/github/screwys/Rufin/ui/player/queue_fullscreen_row.ui",
    {
        cover: ui_shared::recycled_cells::RecycledArtworkCell,
        title: gtk::Label,
        artist: ui_shared::recycled_cells::RecycledTextCell,
        album: gtk::Label,
        duration: gtk::Label,
        year: gtk::Label,
        favorite: gtk::Button,
    }
);

impl QueueFullscreenRow {
    fn for_item(shell: &Rc<crate::PlayerUi>, item: &gtk::ListItem) -> Self {
        RecycledArtworkCell::ensure_type();
        RecycledTextCell::ensure_type();
        let row = Self::new();
        row.imp()
            .cover
            .artwork()
            .set_square_size(QUEUE_FULLSCREEN_COVER_COLUMN_WIDTH);
        row.imp().artist.enable_links(Rc::clone(&shell.navigate));
        row.imp().artist.label().add_css_class("muted");
        row.imp().artist.label().add_css_class("queue-link");
        bind_widget_tooltip(&row.imp().favorite.get(), "Favorite");
        install_queue_row_interactions(
            row.upcast_ref(),
            shell,
            item,
            &row.imp().cover.artwork().drag_paintable_source(),
        );
        let favorite_item = item.downgrade();
        let favorite_shell = Rc::downgrade(shell);
        row.imp().favorite.connect_clicked(move |button| {
            let Some(shell) = favorite_shell.upgrade() else {
                return;
            };
            let Some(row) = queue_row_from_item(&favorite_item) else {
                return;
            };
            (shell.set_track_favorite)(
                row.media_uri.clone(),
                !ui_shared::favorites::favorite_button_is_active(button),
                button,
            );
        });
        row
    }

    fn bind(
        &self,
        shell: &Rc<crate::PlayerUi>,
        row: &QueuePageRow,
        current: Option<&OccurrenceId>,
    ) {
        bind_queue_row_root(self.upcast_ref(), row, current);
        bind_queue_artwork(shell, &self.imp().cover, row);
        self.imp().title.set_text(&row.title);
        self.imp().artist.bind_links(DetailLinks::route(
            &row.artist,
            row.primary_artist_media_uri
                .clone()
                .map(Route::ArtistDetail),
        ));
        self.imp().album.set_text(&row.album);
        let duration = if row.duration_millis > 0 {
            ui_shared::format_duration((row.duration_millis / 1_000) as u32)
        } else {
            String::new()
        };
        self.imp().duration.set_text(&duration);
        self.imp().year.set_text(
            row.year
                .map(|year| year.to_string())
                .as_deref()
                .unwrap_or(""),
        );
        set_favorite_button_active(&self.imp().favorite, row.favorite);
        self.imp().favorite.set_sensitive(true);
    }

    fn clear(&self, shell: &Rc<crate::PlayerUi>) {
        clear_queue_row_root(self.upcast_ref());
        shell
            .artwork
            .clear_artwork_tile(&self.imp().cover.artwork());
        self.imp().title.set_text("");
        self.imp().artist.clear();
        self.imp().album.set_text("");
        self.imp().duration.set_text("");
        self.imp().year.set_text("");
        set_favorite_button_active(&self.imp().favorite, false);
        self.imp().favorite.set_sensitive(false);
    }

    fn allocation_widget(&self) -> gtk::Widget {
        fullscreen_queue_column_owner(
            self.upcast_ref(),
            QueueFullscreenColumnWidgets {
                album: self.imp().album.get().upcast(),
                year: self.imp().year.get().upcast(),
            },
        )
    }
}

fn bind_queue_row_root(root: &gtk::Widget, row: &QueuePageRow, current: Option<&OccurrenceId>) {
    if current == Some(&row.occurrence) {
        root.add_css_class("queue-row-current");
    } else {
        root.remove_css_class("queue-row-current");
    }
    root.update_property(&[gtk::accessible::Property::Label(&format!(
        "{} {}",
        row.title, row.artist
    ))]);
}

fn clear_queue_row_root(root: &gtk::Widget) {
    root.remove_css_class("queue-row-current");
    root.update_property(&[gtk::accessible::Property::Label("")]);
}

fn bind_queue_artwork(
    shell: &Rc<crate::PlayerUi>,
    cover: &RecycledArtworkCell,
    row: &QueuePageRow,
) {
    shell.artwork.bind_artwork_tile(
        &cover.artwork(),
        row.artwork_binding
            .as_deref()
            .map(ArtworkBinding::opaque)
            .unwrap_or_default(),
        50,
        THUMB_COVER_SIZE,
    );
}

fn queue_row_from_item(item: &glib::WeakRef<gtk::ListItem>) -> Option<Arc<QueuePageRow>> {
    item.upgrade()?
        .item()?
        .downcast::<SparseObjectItem>()
        .ok()
        .and_then(|object| object.value::<Arc<QueuePageRow>>())
}

fn install_queue_row_interactions(
    root: &gtk::Widget,
    shell: &Rc<crate::PlayerUi>,
    item: &gtk::ListItem,
    artwork: &gtk::Picture,
) {
    let source = gtk::DragSource::builder()
        .actions(gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::MOVE)
        .build();
    let preview = MediaDragPreviewBinding::default();
    preview.connect(&source);
    let drag_preview = preview.clone();
    let weak_artwork = artwork.downgrade();
    let source_item = item.downgrade();
    let source_shell = Rc::downgrade(shell);
    source.connect_prepare(move |_, _, _| {
        drag_preview.clear();
        let shell = source_shell.upgrade()?;
        let row = queue_row_from_item(&source_item)?;
        let occurrence = row.occurrence.clone();
        let selection = shell.selected_queue()?.dragged_rows_for(&occurrence)?;
        let artwork = weak_artwork
            .upgrade()
            .and_then(|artwork| artwork.paintable());
        drag_preview.prepare(row.title.clone(), artwork);
        Some(media_drag_content_provider(MediaDragSource::Queue {
            occurrences: selection.occurrences,
            media_uris: selection.media_uris,
        }))
    });
    root.add_controller(source);

    let media_drop = gtk::DropTarget::new(
        glib::BoxedAnyObject::static_type(),
        gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::MOVE,
    );
    let media_shell = Rc::downgrade(shell);
    let media_item = item.downgrade();
    let media_root = root.downgrade();
    media_drop.connect_drop(move |_, value, _, y| {
        let Some(occurrence) = queue_row_from_item(&media_item).map(|row| row.occurrence.clone())
        else {
            return false;
        };
        let Some(root) = media_root.upgrade() else {
            return false;
        };
        let queue_target = queue_row_drop_target(occurrence, y, root.height());
        enqueue_media_drop(&media_shell, value, queue_target)
    });
    root.add_controller(media_drop);

    let context_shell = Rc::downgrade(shell);
    let context_item = item.downgrade();
    install_context_menu_openers(
        root,
        Rc::new(move |target, position| {
            let Some(shell) = context_shell.upgrade() else {
                return;
            };
            let Some(row) = queue_row_from_item(&context_item) else {
                return;
            };
            let occurrence = row.occurrence.clone();
            let selection = shell
                .selected_queue()
                .and_then(|queue| queue.selected_rows_for(&occurrence));
            (shell.open_queue_menu)(target, row.as_ref().clone(), selection, position);
        }),
    );
}

fn queue_row_drop_target(occurrence: OccurrenceId, y: f64, height: i32) -> QueueReorderTarget {
    if y < f64::from(height) / 2.0 {
        QueueReorderTarget::Before(occurrence)
    } else {
        QueueReorderTarget::After(occurrence)
    }
}

fn enqueue_media_drop(
    shell: &std::rc::Weak<crate::PlayerUi>,
    value: &glib::Value,
    target: QueueReorderTarget,
) -> bool {
    let Some(source) = media_drag_source(value) else {
        return false;
    };
    let Some(shell) = shell.upgrade() else {
        return false;
    };
    let queue = shell.playback_handles.queue.clone();
    if let MediaDragSource::Queue { occurrences, .. } = source {
        queue.reorder(QueueReorderRequest {
            occurrences: occurrences.to_vec(),
            target,
        });
        return true;
    }
    shell.runtime.spawn(async move {
        let Ok(input) = source.queue_input().await else {
            return;
        };
        queue.insert(input, target);
    });
    true
}

impl crate::PlayerUi {
    pub fn clear_queue(&self) {
        let include_current = self.settings.current.borrow().clear_queue_includes_current;
        self.playback_handles.queue.clear(include_current);
    }

    pub fn schedule_queue_panel_render(self: &Rc<Self>) {
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

    pub fn render_queue_panel(self: &Rc<Self>) {
        let current = self
            .selected_playback()
            .as_deref()
            .and_then(|player| player.queue.current_occurrence.clone());
        let Some(queue) = self.selected_queue() else {
            self.right_panel.queue_header_host.set_visible(false);
            self.views.fullscreen_player.queue_header.set_visible(false);
            clear_queue_panel_children(&self.right_panel.queue_panel);
            clear_queue_panel_children(&self.views.fullscreen_player.queue_panel);
            return;
        };
        if queue.generation.get() == 0 {
            drop(queue);
            self.refresh_queue_window();
            return;
        }
        // The accepted window owns the row positions used for highlighting and reveal.
        if queue.hydration.borrow().is_some()
            || self
                .selected_playback()
                .is_some_and(|player| player.queue_loading)
        {
            return;
        }
        if queue.update_current(current) {
            for pending in &queue.reveal_current {
                pending.set(true);
            }
        }
        let reorderable = queue.filter.borrow().trim().is_empty();
        let selection = queue.selection_model();
        render_panel(
            self,
            &self.right_panel.queue_panel,
            self.right_panel.queue_header_host.upcast_ref(),
            &queue.model,
            &selection,
            false,
            reorderable,
        );
        if self.fullscreen_player_visible() {
            render_panel(
                self,
                &self.views.fullscreen_player.queue_panel,
                &self.views.fullscreen_player.queue_header,
                &queue.model,
                &selection,
                true,
                reorderable,
            );
        }
    }

    pub fn refresh_queue_window(self: &Rc<Self>) {
        let Some(queue) = self.selected_queue() else {
            return;
        };
        let generation = queue.begin();
        self.set_queue_loading(false, true);
        self.set_queue_loading(true, true);
        if self
            .selected_playback()
            .is_some_and(|player| player.queue_loading)
        {
            return;
        }
        let window = self
            .selected_playback()
            .as_deref()
            .map(|player| player.queue_window.clone())
            .unwrap_or_default();
        let database = Arc::clone(&self.database);
        let task = self
            .runtime
            .spawn(async move { database.prepared_queue_page(&window).await });
        queue.hydration.replace(Some(task.abort_handle()));
        drop(queue);
        let shell = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let rows = task.await.ok().and_then(Result::ok);
            let Some(shell) = shell.upgrade() else {
                return;
            };
            let Some(queue) = shell.selected_queue() else {
                return;
            };
            if queue.generation.get() != generation {
                return;
            }
            queue.hydration.take();
            if let Some(rows) = rows {
                queue.accept(generation, rows);
            }
            drop(queue);
            shell.render_queue_panel();
        });
    }

    fn queue_loading_icon(&self, fullscreen: bool) -> &adw::Spinner {
        if fullscreen {
            &self.views.fullscreen_player.queue_loading
        } else {
            &self.right_panel.queue_loading
        }
    }

    fn set_queue_loading(&self, fullscreen: bool, loading: bool) {
        self.queue_loading_icon(fullscreen).set_visible(loading);
        let panel = if fullscreen {
            &self.views.fullscreen_player.queue_panel
        } else {
            &self.right_panel.queue_panel
        };
        if let Some(scroller) = queue_panel_scroller(panel) {
            // Keep the same mapped list allocating underneath the spinner.
            scroller.set_opacity(if loading { 0.0 } else { 1.0 });
            scroller.set_sensitive(!loading);
        }
    }

    fn reveal_queue_current(&self, scroller: &gtk::ScrolledWindow, fullscreen: bool) {
        let Some(queue) = self.selected_queue() else {
            return;
        };
        let pending = &queue.reveal_current[usize::from(fullscreen)];
        if queue.hydration.borrow().is_some()
            || self
                .selected_playback()
                .is_some_and(|player| player.queue_loading)
        {
            return;
        }
        if !pending.get() {
            self.set_queue_loading(fullscreen, false);
            return;
        }
        let position = queue.current.borrow().as_ref().and_then(|current| {
            queue
                .rows
                .borrow()
                .iter()
                .position(|row| &row.occurrence == current)
        });
        let Some(position) = position else {
            pending.set(false);
            self.set_queue_loading(fullscreen, false);
            return;
        };
        if reveal_queue_current_row(scroller, position) {
            pending.set(false);
            self.set_queue_loading(fullscreen, false);
        }
    }
}

fn render_panel(
    shell: &Rc<crate::PlayerUi>,
    panel: &gtk::Box,
    header: &gtk::Widget,
    model: &gio::ListStore,
    selection: &gtk::MultiSelection,
    fullscreen: bool,
    reorderable: bool,
) {
    let scroller = queue_panel_scroller(panel).expect("Queue controls are connected before render");
    let stack = scroller
        .child()
        .and_downcast::<gtk::Viewport>()
        .and_then(|viewport| viewport.child())
        .and_then(|child| child.downcast::<gtk::Stack>().ok());
    let stack = if let Some(stack) = stack {
        stack
    } else {
        let factory = gtk::SignalListItemFactory::new();
        let setup_shell = Rc::downgrade(shell);
        factory.connect_setup(move |_, item| {
            let Some(shell) = setup_shell.upgrade() else {
                return;
            };
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            if fullscreen {
                let row = QueueFullscreenRow::for_item(&shell, item);
                item.set_child(Some(&row.allocation_widget()));
            } else {
                item.set_child(Some(&QueueSidebarRow::for_item(&shell, item)));
            }
        });
        let bind_shell = Rc::downgrade(shell);
        connect_sparse_bind(&factory, move |item| {
            let Some(shell) = bind_shell.upgrade() else {
                return;
            };
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let Some(object) = item
                .item()
                .and_then(|item| item.downcast::<SparseObjectItem>().ok())
            else {
                return;
            };
            let row = object.value::<Arc<QueuePageRow>>().expect("Queue row");
            let current = shell
                .selected_playback()
                .as_deref()
                .and_then(|player| player.queue.current_occurrence.clone());
            if fullscreen {
                if let Some(row_view) = fullscreen_queue_row(item) {
                    row_view.bind(&shell, &row, current.as_ref());
                }
            } else if let Some(row_view) = item.child().and_downcast::<QueueSidebarRow>() {
                row_view.bind(&shell, &row, current.as_ref());
            }
        });
        let unbind_shell = Rc::downgrade(shell);
        factory.connect_unbind(move |_, item| {
            let Some(shell) = unbind_shell.upgrade() else {
                return;
            };
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            if fullscreen {
                if let Some(row_view) = fullscreen_queue_row(item) {
                    row_view.clear(&shell);
                }
            } else if let Some(row_view) = item.child().and_downcast::<QueueSidebarRow>() {
                row_view.clear(&shell);
            }
        });
        let list = gtk::ListView::new(Some(selection.clone()), Some(factory));
        list.add_css_class("queue-list");
        list.set_focus_on_click(false);
        list.set_vscroll_policy(gtk::ScrollablePolicy::Minimum);
        let activate = shell.playback_handles.queue.clone();
        let activate_model = model.clone();
        list.connect_activate(move |_, position| {
            let Some(object) = activate_model
                .item(position)
                .and_then(|item| item.downcast::<SparseObjectItem>().ok())
            else {
                return;
            };
            let occurrence = object
                .value::<Arc<QueuePageRow>>()
                .expect("Queue row")
                .occurrence
                .clone();
            let activate = activate.clone();
            glib::idle_add_local_once(move || activate.activate(occurrence));
        });
        let end_drop = gtk::DropTarget::new(
            glib::BoxedAnyObject::static_type(),
            gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::MOVE,
        );
        let end_shell = Rc::downgrade(shell);
        end_drop.connect_drop(move |_, value, _, _| {
            enqueue_media_drop(&end_shell, value, QueueReorderTarget::End)
        });
        list.add_controller(end_drop);
        let stack = gtk::Stack::new();
        let empty = gtk::Label::new(Some(&tr("Nothing queued")));
        empty.add_css_class("dim-label");
        let empty_drop = gtk::DropTarget::new(
            glib::BoxedAnyObject::static_type(),
            gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::MOVE,
        );
        let empty_shell = Rc::downgrade(shell);
        empty_drop.connect_drop(move |_, value, _, _| {
            enqueue_media_drop(&empty_shell, value, QueueReorderTarget::End)
        });
        empty.add_controller(empty_drop);
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&list, Some("list"));
        scroller.set_child(Some(&stack));
        stack
    };

    let has_rows = model.n_items() != 0;
    header.set_visible(has_rows);
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
    if shell
        .selected_queue()
        .is_some_and(|queue| queue.reveal_current[usize::from(fullscreen)].get())
        || shell.queue_loading_icon(fullscreen).is_visible()
    {
        reveal_queue_after_layout(shell, &scroller, fullscreen);
    }
}

fn fullscreen_queue_row(item: &gtk::ListItem) -> Option<QueueFullscreenRow> {
    item.child()?.first_child()?.downcast().ok()
}

fn queue_panel_scroller(panel: &gtk::Box) -> Option<gtk::ScrolledWindow> {
    let mut child = panel.first_child();
    while let Some(widget) = child {
        if let Some(scroller) = queue_scroller_in(&widget) {
            return Some(scroller);
        }
        child = widget.next_sibling();
    }
    None
}

fn queue_scroller_in(widget: &gtk::Widget) -> Option<gtk::ScrolledWindow> {
    if let Ok(scroller) = widget.clone().downcast::<gtk::ScrolledWindow>() {
        return Some(scroller);
    }
    let mut child = widget.first_child();
    while let Some(widget) = child {
        if let Some(scroller) = queue_scroller_in(&widget) {
            return Some(scroller);
        }
        child = widget.next_sibling();
    }
    None
}

fn reveal_queue_after_layout(
    shell: &Rc<crate::PlayerUi>,
    scroller: &gtk::ScrolledWindow,
    fullscreen: bool,
) {
    let shell = Rc::downgrade(shell);
    // Tick callbacks wait for mapping and run before layout. Connect after the
    // layout handlers for this frame, then disconnect: no resize-driven follow.
    scroller.add_tick_callback(move |scroller, clock| {
        let scroller = scroller.downgrade();
        let shell = shell.clone();
        let handler = Rc::new(RefCell::new(None));
        let disconnect = handler.clone();
        *handler.borrow_mut() = Some(clock.connect_local("layout", true, move |values| {
            let clock = values[0].get::<gtk::gdk::FrameClock>().unwrap();
            clock.disconnect(disconnect.take().unwrap());
            if let (Some(shell), Some(scroller)) = (shell.upgrade(), scroller.upgrade()) {
                shell.reveal_queue_current(&scroller, fullscreen);
            }
            None
        }));
        clock.request_phase(gtk::gdk::FrameClockPhase::LAYOUT);
        glib::ControlFlow::Break
    });
}

fn reveal_queue_current_row(scroller: &gtk::ScrolledWindow, current_row: usize) -> bool {
    let Some(list) = scroller
        .child()
        .and_downcast::<gtk::Viewport>()
        .and_then(|viewport| viewport.child())
        .and_then(|child| child.downcast::<gtk::Stack>().ok())
        .and_then(|stack| stack.child_by_name("list"))
        .and_then(|child| child.downcast::<gtk::ListView>().ok())
    else {
        return false;
    };
    let adjustment = scroller.vadjustment();
    let extent = f64::from(list.measure(gtk::Orientation::Vertical, scroller.width()).0);
    let count = list.model().map_or(0, |model| model.n_items()).max(1);
    let row_center = (current_row as f64 + 0.5) * extent / f64::from(count);
    let maximum = (adjustment.upper() - adjustment.page_size()).max(adjustment.lower());
    adjustment
        .set_value((row_center - adjustment.page_size() / 2.0).clamp(adjustment.lower(), maximum));
    true
}

pub fn clear_queue_panel_children(panel: &gtk::Box) {
    if let Some(scroller) = queue_panel_scroller(panel) {
        scroller.set_child(None::<&gtk::Widget>);
    }
}

pub fn connect_queue_panel_controls(shell: &Rc<crate::PlayerUi>) {
    let sidebar_shell = Rc::downgrade(shell);
    shell
        .right_panel
        .queue_search
        .connect_search_changed(move |entry| {
            let Some(sidebar_shell) = sidebar_shell.upgrade() else {
                return;
            };
            if let Some(queue) = sidebar_shell.selected_queue() {
                queue.filter.replace(entry.text().trim().to_string());
                queue.apply_filter();
            }
            if let Some(scroller) = queue_panel_scroller(&sidebar_shell.right_panel.queue_panel) {
                scroller.vadjustment().set_value(0.0);
            }
            sidebar_shell.render_queue_panel();
        });
    let clear_shell = Rc::downgrade(shell);
    shell
        .right_panel
        .queue_clear_button
        .connect_clicked(move |_| {
            if let Some(shell) = clear_shell.upgrade() {
                shell.clear_queue();
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, title: &str) -> QueuePageRow {
        QueuePageRow {
            occurrence: OccurrenceId::new(id),
            position: 0,
            primary_artist_media_uri: None,
            media_uri: format!("https://example.test/{id}"),
            title: title.to_string(),
            artist: String::new(),
            album: String::new(),
            artwork_binding: None,
            duration_millis: 0,
            year: None,
            favorite: false,
        }
    }

    #[test]
    fn queue_search_filters_the_retained_window_and_keeps_duplicate_entries() {
        let state = QueueState::new();
        let mut first = row("one", "Été [live]");
        first.artist = "Artist".to_string();
        let mut second = first.clone();
        second.occurrence = OccurrenceId::new("two");
        let mut third = row("three", "Other");
        third.album = "Summer".to_string();
        let generation = state.begin();
        assert!(state.accept(
            generation,
            vec![first.clone(), second.clone(), third.clone()]
        ));
        state.filter.replace("  ÉTÉ [live]  ".to_string());
        state.apply_filter();
        assert_eq!(
            *state.rows.borrow(),
            vec![Arc::new(first), Arc::new(second)]
        );
        for position in 0..2 {
            let window = state.window.borrow();
            let rows = state.rows.borrow();
            let object = state
                .model
                .item(position as u32)
                .and_downcast::<SparseObjectItem>()
                .unwrap();
            let payload = object.value::<Arc<QueuePageRow>>().unwrap();
            assert!(Arc::ptr_eq(&window[position], &rows[position]));
            assert!(Arc::ptr_eq(&rows[position], &payload));
        }
        state.filter.replace("SUMMER".to_string());
        state.apply_filter();
        assert_eq!(*state.rows.borrow(), vec![Arc::new(third)]);
        state.filter.replace(String::new());
        state.apply_filter();
        assert_eq!(state.rows.borrow().len(), 3);
    }

    #[test]
    fn stale_window_projection_cannot_replace_newer_rows() {
        let state = QueueState::new();
        let stale = state.begin();
        let current = state.begin();
        assert!(state.accept(current, vec![row("new", "New")]));
        assert!(!state.accept(stale, vec![row("old", "Old")]));
        assert_eq!(state.rows.borrow()[0].occurrence, OccurrenceId::new("new"));
    }

    #[test]
    fn expanding_the_queue_reveals_the_same_playing_occurrence_again() {
        let state = QueueState::new();
        let current = row("playing", "Playing");
        let generation = state.begin();
        assert!(state.accept(generation, vec![current.clone()]));
        state.update_current(Some(current.occurrence.clone()));
        let object = state.model.item(0).unwrap();
        for pending in &state.reveal_current {
            pending.set(false);
        }

        let mut window = (0..10)
            .map(|i| row(&format!("previous-{i}"), "Previous"))
            .collect::<Vec<_>>();
        window.push(current.clone());
        window.push(row("next", "Next"));
        let generation = state.begin();
        assert!(state.accept(generation, window.clone()));
        assert!(!state.update_current(Some(current.occurrence.clone())));
        assert_eq!(state.model.item(10).unwrap(), object);
        assert!(state.reveal_current.iter().all(Cell::get));

        for pending in &state.reveal_current {
            pending.set(false);
        }
        window[10].title = "Updated metadata".into();
        let generation = state.begin();
        assert!(state.accept(generation, window));
        assert!(state.reveal_current.iter().all(|pending| !pending.get()));
    }

    #[test]
    fn external_queue_drop_uses_the_target_row_half() {
        let occurrence = OccurrenceId::new("target");
        assert_eq!(
            queue_row_drop_target(occurrence.clone(), 28.9, 58),
            QueueReorderTarget::Before(occurrence.clone())
        );
        assert_eq!(
            queue_row_drop_target(occurrence.clone(), 29.0, 58),
            QueueReorderTarget::After(occurrence)
        );
    }

    #[test]
    fn queue_selection_preserves_occurrence_order_and_repeated_tracks() {
        let mut first = row("one", "One");
        let mut second = row("two", "Two");
        let mut third = row("three", "Three");
        first.media_uri = "track:7".to_string();
        second.media_uri = "track:7".to_string();
        third.media_uri = "track:9".to_string();

        let selected = queue_rows_for_indexes(
            &[Arc::new(first), Arc::new(second), Arc::new(third)],
            [0, 1].into_iter(),
        );
        assert_eq!(
            selected
                .iter()
                .map(|row| (row.occurrence.as_str(), row.media_uri.as_str()))
                .collect::<Vec<_>>(),
            [("one", "track:7"), ("two", "track:7"),]
        );
    }

    #[test]
    fn queue_current_change_preserves_model_rows() {
        let state = QueueState::new();
        let generation = state.begin();
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
        let first = state.model.item(0).unwrap();
        let third = state.model.item(2).unwrap();
        changes.borrow_mut().clear();
        assert!(state.update_current(Some(OccurrenceId::new("three"))));
        assert!(changes.borrow().is_empty());
        assert_eq!(state.model.item(0).unwrap(), first);
        assert_eq!(state.model.item(2).unwrap(), third);
        assert!(!state.update_current(Some(OccurrenceId::new("three"))));
    }

    #[test]
    fn queue_track_and_artwork_changes_keep_stable_row_objects() {
        let state = QueueState::new();
        let generation = state.begin();
        assert!(state.accept(generation, vec![row("one", "One"), row("two", "Two")]));
        let first = state.model.item(0).expect("first Queue object");
        let second = state.model.item(1).expect("second Queue object");
        let mut next = vec![row("one", "One"), row("two", "Changed")];
        next[1].artwork_binding = Some(vec![1, 2, 3]);
        let generation = state.begin();
        assert!(state.accept(generation, next));
        assert_eq!(first.as_ptr(), state.model.item(0).unwrap().as_ptr());
        assert_eq!(second.as_ptr(), state.model.item(1).unwrap().as_ptr());
    }

    #[test]
    fn queue_replacement_updates_actions_without_replacing_presentation_objects() {
        let state = QueueState::new();
        assert!(state.accept(state.begin(), vec![row("one", "One"), row("two", "Two")]));
        let previous = (0..2)
            .map(|position| state.model.item(position).unwrap())
            .collect::<Vec<_>>();
        let changes = Rc::new(Cell::new(0));
        let observed = Rc::clone(&changes);
        state.model.connect_items_changed(move |_, _, _, _| {
            observed.set(observed.get() + 1);
        });

        for replacement in 0..3 {
            let mut next = vec![
                row(&format!("replacement-{replacement}-one"), "Next one"),
                row(&format!("replacement-{replacement}-two"), "Next two"),
            ];
            next[0].media_uri = "track:replacement".to_string();
            next[0].favorite = true;
            assert!(state.accept(state.begin(), next.clone()));
            assert_eq!(changes.get(), 0);
            for (position, expected) in next.iter().enumerate() {
                let object = state
                    .model
                    .item(position as u32)
                    .and_downcast::<SparseObjectItem>()
                    .unwrap();
                assert_eq!(object.upcast_ref::<glib::Object>(), &previous[position]);
                let payload = object.value::<Arc<QueuePageRow>>().unwrap();
                assert_eq!(payload.as_ref(), expected);
                let dragged = state.dragged_rows_for(&payload.occurrence).unwrap();
                assert_eq!(
                    &*dragged.occurrences,
                    std::slice::from_ref(&expected.occurrence)
                );
                assert_eq!(
                    &*dragged.media_uris,
                    std::slice::from_ref(&expected.media_uri)
                );
            }
        }
        assert!(state.dragged_rows_for(&OccurrenceId::new("one")).is_none());
    }

    #[test]
    fn queue_reorder_reserves_surviving_duplicate_occurrences_before_reusing_slots() {
        let state = QueueState::new();
        let mut first = row("one", "Repeated track");
        first.media_uri = "track:7".to_string();
        let mut second = first.clone();
        second.occurrence = OccurrenceId::new("two");
        assert!(state.accept(
            state.begin(),
            vec![first.clone(), row("removed", "Removed"), second.clone()]
        ));
        let previous = (0..3)
            .map(|position| state.model.item(position).unwrap())
            .collect::<Vec<_>>();
        let next = vec![row("new", "New"), second, first];
        assert!(state.accept(state.begin(), next.clone()));
        for (position, previous_position) in [1, 2, 0].into_iter().enumerate() {
            assert_eq!(
                state.model.item(position as u32).unwrap(),
                previous[previous_position]
            );
        }
        let selected = QueueSelectionSnapshot::new(queue_rows_for_indexes(
            &state.rows.borrow(),
            [1, 2].into_iter(),
        ));
        assert_eq!(
            &*selected.occurrences,
            &[OccurrenceId::new("two"), OccurrenceId::new("one")]
        );
        assert_eq!(&*selected.media_uris, &["track:7", "track:7"]);
        for (position, expected) in next.iter().enumerate() {
            let object = state
                .model
                .item(position as u32)
                .and_downcast::<SparseObjectItem>()
                .unwrap();
            assert_eq!(
                object.value::<Arc<QueuePageRow>>().unwrap().as_ref(),
                expected
            );
        }
    }

    #[test]
    fn unchanged_queue_hydration_keeps_the_shared_payload() {
        let state = QueueState::new();
        let rows = vec![row("one", "One")];
        assert!(state.accept(state.begin(), rows.clone()));
        let previous = Arc::clone(&state.window.borrow()[0]);
        assert!(state.accept(state.begin(), rows));
        let object = state
            .model
            .item(0)
            .and_downcast::<SparseObjectItem>()
            .unwrap();
        assert!(Arc::ptr_eq(&previous, &state.window.borrow()[0]));
        assert!(Arc::ptr_eq(&previous, &state.rows.borrow()[0]));
        assert!(Arc::ptr_eq(
            &previous,
            &object.value::<Arc<QueuePageRow>>().unwrap()
        ));
    }

    #[test]
    fn queue_hydration_is_cancelled_when_superseded_or_its_owner_drops() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        runtime.block_on(async {
            let state = QueueState::new();
            let superseded = tokio::spawn(std::future::pending::<()>());
            state.hydration.replace(Some(superseded.abort_handle()));
            state.begin();
            assert!(superseded.await.unwrap_err().is_cancelled());

            let detached = tokio::spawn(std::future::pending::<()>());
            state.hydration.replace(Some(detached.abort_handle()));
            drop(state);
            assert!(detached.await.unwrap_err().is_cancelled());
        });
    }

    #[test]
    fn dropping_queue_state_releases_sidebar_and_hidden_fullscreen_model() {
        let state = QueueState::new();
        let weak = state.model.downgrade();
        drop(state);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn fullscreen_queue_columns_change_only_at_semantic_breakpoints() {
        assert_eq!(
            fullscreen_queue_column_mode(QUEUE_FULLSCREEN_SHOW_ALBUM_WIDTH - 1),
            QueueFullscreenColumnMode::TitleOnly
        );
        assert_eq!(
            fullscreen_queue_column_mode(QUEUE_FULLSCREEN_SHOW_ALBUM_WIDTH),
            QueueFullscreenColumnMode::Album
        );
        assert_eq!(
            fullscreen_queue_column_mode(QUEUE_FULLSCREEN_SHOW_YEAR_WIDTH - 1),
            QueueFullscreenColumnMode::Album
        );
        assert_eq!(
            fullscreen_queue_column_mode(QUEUE_FULLSCREEN_SHOW_YEAR_WIDTH),
            QueueFullscreenColumnMode::AlbumAndYear
        );
    }
}
