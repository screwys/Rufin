use adw::prelude::*;
use artwork::ArtworkBinding;
use gtk::{gio, glib};
use library::QueuePageRow;
use localization::tr;
use playback::{OccurrenceId, QueueReorderRequest, QueueReorderTarget};
use rufin_core::settings::layout::{LibraryField, LibraryListKey, LibraryListSettings};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    sync::Arc,
};
use ui_shared::{
    artwork::{ArtworkTileWeak, THUMB_COVER_SIZE},
    favorites::{favorite_button_is_active, row_favorite_icon_button, set_favorite_button_active},
    interactions::install_context_menu_openers,
    library_fields::TrackPresentation,
    media_drag::{
        MediaDragPreviewBinding, MediaDragSource, media_drag_content_provider, media_drag_source,
    },
    recycled_cells::{RecycledMergedCell, set_track_row_index_text, track_list_row_index_cell},
    sparse_model::{SparseObjectItem, connect_sparse_bind},
};

const ROW_INDEX_COLUMN_TITLE: &str = "\u{2003}#";

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
        let changed = previous != current;
        for occurrence in [previous.filter(|_| changed), current]
            .into_iter()
            .flatten()
        {
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
        changed
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
    cover: Option<ArtworkTileWeak>,
) {
    let source = gtk::DragSource::builder()
        .actions(gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::MOVE)
        .build();
    let preview = MediaDragPreviewBinding::default();
    preview.connect(&source);
    let drag_preview = preview.clone();
    let weak_cover = cover;
    let source_item = item.downgrade();
    let source_shell = Rc::downgrade(shell);
    source.connect_prepare(move |_, _, _| {
        drag_preview.clear();
        let shell = source_shell.upgrade()?;
        let row = queue_row_from_item(&source_item)?;
        let occurrence = row.occurrence.clone();
        let selection = shell.selected_queue()?.dragged_rows_for(&occurrence)?;
        let artwork = weak_cover
            .as_ref()
            .and_then(|weak| weak.upgrade())
            .and_then(|tile| tile.drag_paintable_source().paintable());
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
    ui_shared::media_drag::style_media_drop_target(root);
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

fn queue_index_column(shell: &Rc<crate::PlayerUi>) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let setup_shell = Rc::downgrade(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = track_list_row_index_cell(item);
        if let Some(shell) = setup_shell.upgrade() {
            install_queue_row_interactions(cell.upcast_ref(), &shell, item, None);
        }
        item.set_child(Some(&cell));
    });
    let bind_shell = Rc::downgrade(shell);
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = item.child().and_downcast::<gtk::Overlay>() else {
            return;
        };
        set_track_row_index_text(&cell, &(item.position() + 1).to_string());
        if let Some(shell) = bind_shell.upgrade() {
            let row = queue_row_from_item(&item.downgrade());
            let player = shell.selected_playback();
            let current = row
                .as_ref()
                .zip(player.as_ref())
                .is_some_and(|(row, player)| {
                    player.queue.current_occurrence.as_ref() == Some(&row.occurrence)
                });
            let paused = player
                .as_ref()
                .is_some_and(|player| !player.transport.desired_playing);
            ui_shared::recycled_cells::set_track_playing(cell.upcast_ref(), current, paused);
        }
    });
    factory.connect_unbind(|_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>()
            && let Some(cell) = item.child().and_downcast::<gtk::Overlay>()
        {
            ui_shared::recycled_cells::set_track_playing(cell.upcast_ref(), false, false);
            set_track_row_index_text(&cell, "");
        }
    });
    let column = gtk::ColumnViewColumn::new(Some(ROW_INDEX_COLUMN_TITLE), Some(factory));
    column.set_fixed_width(ui_shared::library_fields::column_width(
        LibraryField::RowIndex,
    ));
    column
}

fn queue_title_column(shell: &Rc<crate::PlayerUi>) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let setup_shell = Rc::downgrade(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(shell) = setup_shell.upgrade() else {
            return;
        };
        let cell = RecycledMergedCell::without_downloads(Rc::clone(&shell.navigate), 48);
        let title = cell.title();
        title.add_css_class("track-list-title");
        title.add_css_class("queue-title");
        let subtitle = cell.subtitle();
        subtitle.add_css_class("artist-label");
        subtitle.add_css_class("table-link-label");
        install_queue_row_interactions(
            cell.upcast_ref(),
            &shell,
            item,
            Some(cell.cover().downgrade()),
        );
        item.set_child(Some(&cell));
    });
    let bind_shell = Rc::downgrade(shell);
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = item.child().and_downcast::<RecycledMergedCell>() else {
            return;
        };
        let Some(object) = item
            .item()
            .and_then(|item| item.downcast::<SparseObjectItem>().ok())
        else {
            return;
        };
        let row = object.value::<Arc<QueuePageRow>>().expect("Queue row");
        let Some(shell) = bind_shell.upgrade() else {
            return;
        };
        let current = shell
            .selected_playback()
            .as_deref()
            .and_then(|player| player.queue.current_occurrence.clone());
        let is_current = current.as_ref() == Some(&row.occurrence);
        let title = cell.title();
        title.set_text(&row.title);
        let paused = shell
            .selected_playback()
            .as_ref()
            .is_some_and(|p| !p.transport.desired_playing);
        ui_shared::recycled_cells::set_track_playing(title.upcast_ref(), is_current, paused);
        let subtitle = cell.subtitle();
        subtitle.set_text(&row.artist);
        subtitle.set_visible(!row.artist.is_empty());
        cell.bind_subtitle(row.links(LibraryField::Artist));
        let cover = cell.cover();
        shell.artwork.bind_artwork_tile(
            &cover,
            row.artwork_binding
                .as_deref()
                .map(ArtworkBinding::opaque)
                .unwrap_or_default(),
            48,
            THUMB_COVER_SIZE,
        );
    });
    let unbind_shell = Rc::downgrade(shell);
    factory.connect_unbind(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>()
            && let Some(cell) = item.child().and_downcast::<RecycledMergedCell>()
        {
            cell.title().set_text("");
            cell.title().remove_css_class("track-row-playing");
            cell.clear_subtitle();
            if let Some(shell) = unbind_shell.upgrade() {
                shell.artwork.clear_artwork_tile(&cell.cover());
            }
        }
    });
    let column = gtk::ColumnViewColumn::new(Some(&tr("Title")), Some(factory));
    column.set_fixed_width(ui_shared::library_fields::column_width(
        LibraryField::TitleMerged,
    ));
    column
}

fn queue_duration_column(shell: &Rc<crate::PlayerUi>) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let setup_shell = Rc::downgrade(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = ui_shared::recycled_cells::RecycledTextCell::new();
        if let Some(shell) = setup_shell.upgrade() {
            install_queue_row_interactions(cell.upcast_ref(), &shell, item, None);
        }
        item.set_child(Some(&cell));
    });
    connect_sparse_bind(&factory, |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(label) = item
            .child()
            .and_downcast::<ui_shared::recycled_cells::RecycledTextCell>()
        else {
            return;
        };
        let Some(object) = item
            .item()
            .and_then(|item| item.downcast::<SparseObjectItem>().ok())
        else {
            return;
        };
        let row = object.value::<Arc<QueuePageRow>>().expect("Queue row");
        let text = ui_shared::format_duration((row.duration_millis.max(0) / 1000) as u32);
        label.label().set_text(&text);
    });
    factory.connect_unbind(|_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>()
            && let Some(label) = item
                .child()
                .and_downcast::<ui_shared::recycled_cells::RecycledTextCell>()
        {
            label.label().set_text("");
        }
    });
    let column = gtk::ColumnViewColumn::new(Some("◷"), Some(factory));
    column.set_fixed_width(ui_shared::library_fields::column_width(
        LibraryField::Duration,
    ));
    column
}

fn queue_favorite_column(shell: &Rc<crate::PlayerUi>) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let setup_shell = Rc::downgrade(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(shell) = setup_shell.upgrade() else {
            return;
        };
        let button = row_favorite_icon_button("Favorite track");
        let actions = ui_shared::recycled_cells::RowActions::with_favorite(&button);
        install_queue_row_interactions(actions.upcast_ref(), &shell, item, None);
        let click_item = item.downgrade();
        let click_shell = Rc::downgrade(&shell);
        button.connect_clicked(move |button| {
            let Some(click_shell) = click_shell.upgrade() else {
                return;
            };
            let Some(row) = queue_row_from_item(&click_item) else {
                return;
            };
            let favorite = !favorite_button_is_active(button);
            (click_shell.set_track_favorite)(row.media_uri.clone(), favorite, button);
            set_favorite_button_active(button, favorite);
        });
        item.set_child(Some(&actions));
    });
    connect_sparse_bind(&factory, |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(button) = ui_shared::recycled_cells::row_favorite_button(item) else {
            return;
        };
        let Some(object) = item
            .item()
            .and_then(|item| item.downcast::<SparseObjectItem>().ok())
        else {
            return;
        };
        let row = object.value::<Arc<QueuePageRow>>().expect("Queue row");
        set_favorite_button_active(&button, row.favorite);
    });
    factory.connect_unbind(|_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>()
            && let Some(button) = ui_shared::recycled_cells::row_favorite_button(item)
        {
            set_favorite_button_active(&button, false);
        }
    });
    ui_shared::recycled_cells::row_actions_column(&factory)
}

fn queue_year_column(shell: &Rc<crate::PlayerUi>) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let setup_shell = Rc::downgrade(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::new(None);
        label.add_css_class("muted");
        label.add_css_class("tabular-numeric");
        label.add_css_class("queue-year");
        label.set_xalign(1.0);
        label.set_hexpand(true);
        if let Some(shell) = setup_shell.upgrade() {
            install_queue_row_interactions(label.upcast_ref(), &shell, item, None);
        }
        item.set_child(Some(&label));
    });
    connect_sparse_bind(&factory, |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(label) = item.child().and_downcast::<gtk::Label>() else {
            return;
        };
        let Some(object) = item
            .item()
            .and_then(|item| item.downcast::<SparseObjectItem>().ok())
        else {
            return;
        };
        let row = object.value::<Arc<QueuePageRow>>().expect("Queue row");
        label.set_text(&row.year.map(|y| y.to_string()).unwrap_or_default());
    });
    factory.connect_unbind(|_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>()
            && let Some(label) = item.child().and_downcast::<gtk::Label>()
        {
            label.set_text("");
        }
    });
    let column = gtk::ColumnViewColumn::new(Some(&tr("Year")), Some(factory));
    column.set_fixed_width(62);
    column
}

fn build_queue_table(
    shell: &Rc<crate::PlayerUi>,
    model: &gio::ListStore,
    selection: &gtk::MultiSelection,
    fullscreen: bool,
) -> (gtk::ColumnView, ui_shared::table_sizing::ColumnViewWidthFit) {
    let table = gtk::ColumnView::new(Some(selection.clone()));
    table.add_css_class("track-table");
    table.add_css_class("track-list");
    table.add_css_class("queue-list");
    table.set_single_click_activate(false);
    let row_factory = gtk::SignalListItemFactory::new();
    row_factory.connect_setup(|_, row| {
        if let Some(row) = row.downcast_ref::<gtk::ColumnViewRow>() {
            row.set_activatable(true);
        }
    });
    table.set_row_factory(Some(&row_factory));
    table.set_vscroll_policy(gtk::ScrollablePolicy::Minimum);
    table.set_hexpand(true);
    table.set_vexpand(true);

    let width_fit = if fullscreen {
        use ui_shared::library_fields::column_width;
        let index = queue_index_column(shell);
        let title = queue_title_column(shell);
        let duration = queue_duration_column(shell);
        let mut columns = vec![
            (index.clone(), column_width(LibraryField::RowIndex)),
            (
                title.clone(),
                column_width(LibraryField::TitleMerged).saturating_add(72),
            ),
            (duration.clone(), column_width(LibraryField::Duration)),
        ];
        table.append_column(&index);
        table.append_column(&title);
        table.append_column(&duration);
        if shell
            .settings
            .current
            .borrow()
            .library_list(LibraryListKey::Queue)
            .row_fields
            .contains(&LibraryField::Tools)
        {
            let tools = queue_favorite_column(shell);
            table.append_column(&tools);
            columns.push((tools, ui_shared::recycled_cells::ROW_ACTIONS_WIDTH));
        }
        ui_shared::table_sizing::install_column_view_width_fit(&table, columns, 1)
    } else {
        ui_shared::table_sizing::install_column_view_width_fit(
            &table,
            configure_queue_columns(shell, &table),
            1,
        )
    };

    let activate = shell.playback_handles.queue.clone();
    let activate_model = model.clone();
    table.connect_activate(move |_, position| {
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
    table.add_controller(end_drop);

    (table, width_fit)
}

fn configure_queue_columns(
    shell: &Rc<crate::PlayerUi>,
    table: &gtk::ColumnView,
) -> Vec<(gtk::ColumnViewColumn, i32)> {
    while let Some(column) = table
        .columns()
        .item(0)
        .and_downcast::<gtk::ColumnViewColumn>()
    {
        table.remove_column(&column);
    }
    shell
        .settings
        .current
        .borrow()
        .library_list(LibraryListKey::Queue)
        .row_fields
        .iter()
        .map(|field| {
            let column = match field {
                LibraryField::TitleMerged => queue_title_column(shell),
                LibraryField::RowIndex => queue_index_column(shell),
                LibraryField::Duration => queue_duration_column(shell),
                LibraryField::Tools => queue_favorite_column(shell),
                LibraryField::Year => queue_year_column(shell),
                _ => queue_text_column(shell, *field),
            };
            let width = if *field == LibraryField::Tools {
                ui_shared::recycled_cells::ROW_ACTIONS_WIDTH
            } else if *field == LibraryField::Year {
                62
            } else {
                ui_shared::library_fields::column_width(*field)
            };
            table.append_column(&column);
            (column, width)
        })
        .collect()
}

fn queue_text_column(shell: &Rc<crate::PlayerUi>, field: LibraryField) -> gtk::ColumnViewColumn {
    use ui_shared::recycled_cells::RecycledTextCell;
    let factory = gtk::SignalListItemFactory::new();
    let weak = Rc::downgrade(shell);
    factory.connect_setup(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = RecycledTextCell::new();
        if let Some(shell) = weak.upgrade() {
            cell.enable_links(Rc::clone(&shell.navigate));
            install_queue_row_interactions(cell.upcast_ref(), &shell, item, None);
        }
        item.set_child(Some(&cell));
    });
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = item.child().and_downcast::<RecycledTextCell>() else {
            return;
        };
        let Some(row) = queue_row_from_item(&item.downgrade()) else {
            return;
        };
        cell.bind_links(row.links(field));
    });
    factory.connect_unbind(|_, object| {
        if let Some(item) = object.downcast_ref::<gtk::ListItem>()
            && let Some(cell) = item.child().and_downcast::<RecycledTextCell>()
        {
            cell.clear();
        }
    });
    gtk::ColumnViewColumn::new(
        Some(&tr(ui_shared::settings::library_field_title(field))),
        Some(factory),
    )
}

impl crate::PlayerUi {
    fn present_queue_settings(self: &Rc<Self>) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let resource = crate::ui_resource::QUEUE_SETTINGS_RESOURCE;
        let builder = ui_shared::ui_resource::builder(resource);
        ui_shared::objects!(builder, resource, {
            dialog: adw::PreferencesDialog,
            fields_group: adw::PreferencesGroup,
            reset: gtk::Button,
        });
        dialog.set_content_width(ui_shared::layout::large_popup_content_width(560));
        dialog.set_content_height(ui_shared::layout::large_popup_content_height(
            window.height(),
            640,
        ));
        let rows = Rc::new(RefCell::new(Vec::new()));
        let weak = Rc::downgrade(self);
        let changed: Rc<dyn Fn()> = Rc::new(move || {
            let Some(shell) = weak.upgrade() else {
                return;
            };
            let Some(table) = queue_panel_scroller(&shell.right_panel.queue_panel)
                .and_then(|scroller| scroller.child())
                .and_downcast::<gtk::ColumnView>()
            else {
                return;
            };
            let columns = configure_queue_columns(&shell, &table);
            if let Some(fit) = shell.right_panel.queue_width_fit.borrow().as_ref() {
                fit.replace(columns);
            }
        });
        ui_shared::library_field_editor::populate_library_field_rows(
            &self.settings,
            Rc::clone(&changed),
            LibraryListKey::Queue,
            &fields_group,
            &rows,
        );
        let settings = Rc::clone(&self.settings);
        let fields_group = fields_group.downgrade();
        reset.connect_clicked(move |_| {
            let Some(fields_group) = fields_group.upgrade() else {
                return;
            };
            if settings.update_library_list_settings(LibraryListKey::Queue, |settings| {
                *settings = LibraryListSettings::for_key(LibraryListKey::Queue);
            }) {
                changed();
            }
            ui_shared::library_field_editor::populate_library_field_rows(
                &settings,
                Rc::clone(&changed),
                LibraryListKey::Queue,
                &fields_group,
                &rows,
            );
        });
        ui_shared::popup::present_light_dismiss_dialog(&dialog, &window);
    }
}

fn queue_overlay_empty_label(
    scroller: &gtk::ScrolledWindow,
    shell: &Rc<crate::PlayerUi>,
) -> Option<gtk::Label> {
    let overlay = scroller.parent().and_downcast::<gtk::Overlay>()?;
    let mut child = overlay.first_child();
    while let Some(widget) = child {
        if widget.has_css_class("queue-empty-label") {
            return widget.downcast::<gtk::Label>().ok();
        }
        child = widget.next_sibling();
    }
    let empty = gtk::Label::new(Some(&tr("Nothing queued")));
    empty.add_css_class("dim-label");
    empty.add_css_class("queue-empty-label");
    empty.set_halign(gtk::Align::Center);
    empty.set_valign(gtk::Align::Center);
    let empty_drop = gtk::DropTarget::new(
        glib::BoxedAnyObject::static_type(),
        gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::MOVE,
    );
    let empty_shell = Rc::downgrade(shell);
    empty_drop.connect_drop(move |_, value, _, _| {
        enqueue_media_drop(&empty_shell, value, QueueReorderTarget::End)
    });
    empty.add_css_class("media-drop-target");
    empty.add_controller(empty_drop);
    overlay.add_overlay(&empty);
    Some(empty)
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
            clear_queue_panel_children(&self.right_panel.queue_panel);
            clear_queue_panel_children(&self.views.fullscreen_player.queue_panel);
            return;
        };
        if queue.generation.get() == 0 {
            drop(queue);
            self.refresh_queue_window();
            return;
        }
        let loading = self
            .selected_playback()
            .is_some_and(|player| player.queue_loading)
            || (queue.hydration.borrow().is_some() && !self.queue_has_current());
        self.set_queue_loading(false, loading);
        self.set_queue_loading(true, loading);
        if loading && queue.window.borrow().is_empty() {
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
            &queue.model,
            &selection,
            false,
            reorderable,
        );
        if self.fullscreen_player_visible() {
            render_panel(
                self,
                &self.views.fullscreen_player.queue_panel,
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
        let loading = self.selected_playback().is_some_and(|player| {
            player.queue_loading || (player.queue.total > 0 && !self.queue_has_current())
        });
        self.set_queue_loading(false, loading);
        self.set_queue_loading(true, loading);
        if self
            .selected_playback()
            .is_some_and(|player| player.queue_loading)
        {
            drop(queue);
            self.render_queue_panel();
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
        self.render_queue_panel();
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
            let replacing = loading && !self.queue_has_current();
            scroller.set_opacity(if replacing { 0.0 } else { 1.0 });
            scroller.set_sensitive(!replacing);
        }
    }

    fn queue_has_current(&self) -> bool {
        let current = self
            .selected_playback()
            .and_then(|player| player.queue.current_occurrence.clone());
        self.selected_queue().is_some_and(|queue| {
            let rows = queue.window.borrow();
            current.as_ref().map_or(!rows.is_empty(), |current| {
                rows.iter().any(|row| &row.occurrence == current)
            })
        })
    }

    fn reveal_queue_current(self: &Rc<Self>, scroller: &gtk::ScrolledWindow, fullscreen: bool) {
        let Some(queue) = self.selected_queue() else {
            return;
        };
        let pending = &queue.reveal_current[usize::from(fullscreen)];
        let loading = self.queue_loading_icon(fullscreen).is_visible();
        if !pending.get() && !loading {
            return;
        }
        let Some(occurrence) = queue.current.borrow().clone() else {
            pending.set(false);
            self.set_queue_loading(fullscreen, false);
            return;
        };
        let Some(position) = queue
            .rows
            .borrow()
            .iter()
            .position(|row| row.occurrence == occurrence)
        else {
            pending.set(false);
            self.set_queue_loading(fullscreen, loading);
            return;
        };
        if reveal_queue_current_row(scroller, position) {
            pending.set(false);
            self.set_queue_loading(fullscreen, loading);
        }
    }
}

fn render_panel(
    shell: &Rc<crate::PlayerUi>,
    panel: &gtk::Box,
    model: &gio::ListStore,
    selection: &gtk::MultiSelection,
    fullscreen: bool,
    reorderable: bool,
) {
    let Some(scroller) = queue_panel_scroller(panel) else {
        return;
    };
    let table = if let Some(table) = scroller.child().and_downcast::<gtk::ColumnView>() {
        table
    } else {
        let (table, width_fit) = build_queue_table(shell, model, selection, fullscreen);
        if !fullscreen {
            shell
                .right_panel
                .queue_width_fit
                .replace(Some(width_fit.clone()));
        }
        ui_shared::layout::configure_fill_width_clip(&scroller, gtk::PolicyType::Automatic);
        scroller.set_child(Some(&table));
        let overlay = scroller
            .parent()
            .and_downcast::<gtk::Overlay>()
            .expect("queue overlay");
        panel.remove(&overlay);
        let weak = scroller.downgrade();
        let previous_height = Cell::new(0);
        let resize_offset = Rc::new(Cell::new(None));
        let before_offset = Rc::clone(&resize_offset);
        let owner = ui_shared::layout::allocation_owner(&overlay, move |width, height| {
            if let Some(scroller) = weak.upgrade() {
                let previous = previous_height.replace(height);
                before_offset.set(
                    (previous > 0 && previous != height).then(|| scroller.vadjustment().value()),
                );
                width_fit.fit_scroller_allocation(&scroller, width);
            }
        });
        let weak = scroller.downgrade();
        owner.set_after_allocate_callback(move || {
            if let Some(value) = resize_offset.take()
                && let Some(scroller) = weak.upgrade()
            {
                // GtkColumnView otherwise keeps a row at a fractional viewport
                // position, which slides the tracks when a divider changes height.
                let adjustment = scroller.vadjustment();
                let maximum = (adjustment.upper() - adjustment.page_size()).max(adjustment.lower());
                adjustment.set_value(value.clamp(adjustment.lower(), maximum));
            }
        });
        panel.append(&owner);
        table
    };

    let has_rows = model.n_items() != 0;
    table.set_visible(has_rows);
    if let Some(empty) = queue_overlay_empty_label(&scroller, shell) {
        empty.set_visible(!has_rows);
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

fn reveal_queue_current_row(scroller: &gtk::ScrolledWindow, position: usize) -> bool {
    let Some(table) = scroller.child().and_downcast::<gtk::ColumnView>() else {
        return false;
    };
    if table.width() <= 1 && table.height() <= 1 {
        return false;
    }
    table.scroll_to(position as u32, None, gtk::ListScrollFlags::NONE, None);
    true
}

pub fn clear_queue_panel_children(panel: &gtk::Box) {
    if let Some(scroller) = queue_panel_scroller(panel) {
        if let Some(table) = scroller.child().and_downcast::<gtk::ColumnView>() {
            table.set_visible(false);
        }
        if let Some(overlay) = scroller.parent().and_downcast::<gtk::Overlay>() {
            let mut child = overlay.first_child();
            while let Some(widget) = child {
                if widget.has_css_class("queue-empty-label") {
                    widget.set_visible(false);
                }
                child = widget.next_sibling();
            }
        }
    }
}

pub fn connect_queue_panel_controls(shell: &Rc<crate::PlayerUi>) {
    let weak = Rc::downgrade(shell);
    shell
        .right_panel
        .queue_customize_button
        .connect_clicked(move |_| {
            if let Some(shell) = weak.upgrade() {
                shell.present_queue_settings();
            }
        });
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
        let generation = state.begin();
        assert!(state.accept(generation, window));
        assert!(state.reveal_current[0].get());
        assert!(state.reveal_current[1].get());
        let updated = state.model.item(10).unwrap();
        assert_eq!(object, updated);
    }
}
