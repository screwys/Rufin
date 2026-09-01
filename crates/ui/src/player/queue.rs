use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use artwork::ArtworkBinding;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use gtk::{gio, glib};
use library::QueuePageRow;
use localization::{msgid, tr};
use playback::{OccurrenceId, PlaybackMedia, QueueReorderRequest, QueueReorderTarget};

use crate::favorites::set_favorite_button_active;
use crate::interactions::{ContextMenuSurface, install_context_menu_openers};
use crate::layout::allocation_owner;
use crate::localization::bind_widget_tooltip;
use crate::routes::collection_context::{
    install_track_selection_download_actions, present_queue_track_context_menu,
};
use crate::routes::detail_links::DetailLinks;
use crate::routes::playlist_picker::{
    PlaylistDragPreviewBinding, PlaylistTrackSource, append_context_menu_picker_selection,
    context_menu_can_add_to_playlist, playlist_drag_content_provider,
};
use crate::routes::route::Route;
use crate::routes::track_selection::TrackSelectionSnapshot;
use crate::routes::{RecycledArtworkCell, RecycledTextCell};
use crate::settings::ContextMenuItem;
use crate::shell::Shell;
use crate::shell::actions::{PLAY_ICON, REMOVE_ICON};
use crate::shell::cover::THUMB_COVER_SIZE;
use crate::shell::layout::WINDOW_CHROME_MARGIN_END;

const QUEUE_PAGE_SIZE: usize = 100;
const QUEUE_ROW_HEIGHT: i32 = 58;
const QUEUE_FULLSCREEN_COVER_COLUMN_WIDTH: i32 = 50;
const QUEUE_FULLSCREEN_SHOW_ALBUM_WIDTH: i32 = 572;
const QUEUE_FULLSCREEN_SHOW_YEAR_WIDTH: i32 = 652;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QueueFullscreenColumnMode {
    TitleOnly,
    Album,
    AlbumAndYear,
}

pub(super) struct QueueFullscreenColumnWidgets {
    pub(super) album: gtk::Widget,
    pub(super) year: gtk::Widget,
}

crate::ui_resource::composite_box!(
    pub(crate) QueueSidebarRow,
    sidebar_row_imp,
    "RufinQueueSidebarRow",
    "/io/github/screwys/Rufin/ui/player/queue_sidebar_row.ui",
    {
        cover: crate::routes::RecycledArtworkCell,
        title: gtk::Label,
        artist: crate::routes::RecycledTextCell,
        year: gtk::Label,
    }
);

impl QueueSidebarRow {
    fn for_item(shell: &Rc<Shell>, item: &gtk::ListItem) -> Self {
        RecycledArtworkCell::ensure_type();
        RecycledTextCell::ensure_type();
        let row = Self::new();
        row.set_margin_end(WINDOW_CHROME_MARGIN_END);
        row.imp().cover.artwork().set_square_size(50);
        row.imp().artist.enable_links(shell);
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

    fn bind(&self, shell: &Rc<Shell>, row: &QueuePageRow, current: Option<&OccurrenceId>) {
        bind_queue_row_root(self.upcast_ref(), row, current);
        bind_queue_artwork(shell, &self.imp().cover, row);
        self.imp().title.set_text(&row.title);
        self.imp().artist.bind_links(DetailLinks::route(
            &row.artist,
            row.primary_artist_key.map(Route::ArtistDetail),
        ));
        self.imp().year.set_text(
            row.year
                .map(|year| year.to_string())
                .as_deref()
                .unwrap_or(""),
        );
    }

    fn clear(&self, shell: &Rc<Shell>) {
        clear_queue_row_root(self.upcast_ref());
        shell.clear_artwork_tile(&self.imp().cover.artwork());
        self.imp().title.set_text("");
        self.imp().artist.clear();
        self.imp().year.set_text("");
    }
}

crate::ui_resource::composite_box!(
    pub(crate) QueueFullscreenRow,
    fullscreen_row_imp,
    "RufinQueueFullscreenRow",
    "/io/github/screwys/Rufin/ui/player/queue_fullscreen_row.ui",
    {
        cover: crate::routes::RecycledArtworkCell,
        title: gtk::Label,
        artist: crate::routes::RecycledTextCell,
        album: gtk::Label,
        duration: gtk::Label,
        year: gtk::Label,
        favorite: gtk::Button,
    }
);

impl QueueFullscreenRow {
    fn for_item(shell: &Rc<Shell>, item: &gtk::ListItem) -> Self {
        RecycledArtworkCell::ensure_type();
        RecycledTextCell::ensure_type();
        let row = Self::new();
        row.imp()
            .cover
            .artwork()
            .set_square_size(QUEUE_FULLSCREEN_COVER_COLUMN_WIDTH);
        row.imp().artist.enable_links(shell);
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
            let Some(key) = row.track_key else {
                return;
            };
            shell.set_favorite_with_feedback(
                library::FavoriteTarget::Track(key),
                !crate::favorites::favorite_button_is_active(button),
                Some(button),
            );
        });
        row
    }

    fn bind(&self, shell: &Rc<Shell>, row: &QueuePageRow, current: Option<&OccurrenceId>) {
        bind_queue_row_root(self.upcast_ref(), row, current);
        bind_queue_artwork(shell, &self.imp().cover, row);
        self.imp().title.set_text(&row.title);
        self.imp().artist.bind_links(DetailLinks::route(
            &row.artist,
            row.primary_artist_key.map(Route::ArtistDetail),
        ));
        self.imp().album.set_text(&row.album);
        self.imp().duration.set_text(
            &row.duration_millis
                .map(|duration| crate::format_duration((duration.max(0) / 1_000) as u32))
                .unwrap_or_default(),
        );
        self.imp().year.set_text(
            row.year
                .map(|year| year.to_string())
                .as_deref()
                .unwrap_or(""),
        );
        set_favorite_button_active(&self.imp().favorite, row.favorite.unwrap_or(false));
        self.imp().favorite.set_sensitive(row.track_key.is_some());
    }

    fn clear(&self, shell: &Rc<Shell>) {
        clear_queue_row_root(self.upcast_ref());
        shell.clear_artwork_tile(&self.imp().cover.artwork());
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
    let occurrence = OccurrenceId::new(row.object_id.clone());
    if current == Some(&occurrence) {
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

fn bind_queue_artwork(shell: &Rc<Shell>, cover: &RecycledArtworkCell, row: &QueuePageRow) {
    shell.bind_artwork_tile(
        &cover.artwork(),
        row.artwork_binding
            .as_deref()
            .map(ArtworkBinding::opaque)
            .unwrap_or_default(),
        50,
        THUMB_COVER_SIZE,
    );
}

fn queue_row_from_item(item: &glib::WeakRef<gtk::ListItem>) -> Option<QueuePageRow> {
    item.upgrade()?
        .item()?
        .downcast::<glib::BoxedAnyObject>()
        .ok()
        .map(|object| object.borrow::<QueuePageRow>().clone())
}

fn install_queue_row_interactions(
    root: &gtk::Widget,
    shell: &Rc<Shell>,
    item: &gtk::ListItem,
    artwork: &gtk::Picture,
) {
    let source = gtk::DragSource::builder()
        .actions(gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::MOVE)
        .build();
    let preview = PlaylistDragPreviewBinding::default();
    preview.connect(&source);
    let drag_preview = preview.clone();
    let weak_artwork = artwork.downgrade();
    let source_item = item.downgrade();
    let source_shell = Rc::downgrade(shell);
    source.connect_prepare(move |_, _, _| {
        drag_preview.clear();
        let shell = source_shell.upgrade()?;
        let row = queue_row_from_item(&source_item)?;
        let occurrence = OccurrenceId::new(row.object_id.clone());
        let selection = shell
            .selected_queue()?
            .dragged_rows_for(&shell, &occurrence)?;
        let artwork = weak_artwork
            .upgrade()
            .and_then(|artwork| artwork.paintable());
        drag_preview.prepare(row.title.clone(), artwork);
        let reorderable = shell
            .selected_queue()
            .as_deref()
            .is_some_and(|queue| queue.filter.borrow().trim().is_empty());
        let mut providers = Vec::new();
        if reorderable && selection.occurrences.len() == 1 {
            providers.push(gtk::gdk::ContentProvider::for_value(
                &occurrence.to_string().to_value(),
            ));
        }
        if let Some(tracks) = selection.tracks {
            providers.push(playlist_drag_content_provider(
                PlaylistTrackSource::selection(tracks),
            ));
        }
        match providers.as_slice() {
            [] => None,
            [provider] => Some(provider.clone()),
            providers => Some(gtk::gdk::ContentProvider::new_union(providers)),
        }
    });
    root.add_controller(source);

    let drop = gtk::DropTarget::new(String::static_type(), gtk::gdk::DragAction::MOVE);
    let queue = shell.products.playback.queue.clone();
    let drop_shell = Rc::downgrade(shell);
    let target_item = item.downgrade();
    drop.connect_drop(move |_, value, _, _| {
        let Ok(object_id) = value.get::<String>() else {
            return false;
        };
        let Some(shell) = drop_shell.upgrade() else {
            return false;
        };
        let Some(state) = shell.selected_queue() else {
            return false;
        };
        if !state.filter.borrow().trim().is_empty() {
            return false;
        }
        let Some(target) =
            queue_row_from_item(&target_item).map(|row| OccurrenceId::new(row.object_id))
        else {
            return false;
        };
        let Some(request) = state.reorder_request(OccurrenceId::new(object_id), target) else {
            return false;
        };
        queue.reorder(request);
        true
    });
    root.add_controller(drop);

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
            let occurrence = OccurrenceId::new(row.object_id.clone());
            if let Some(selection) = shell
                .selected_queue()
                .and_then(|queue| queue.selected_rows_for(&shell, &occurrence))
            {
                present_queue_selection_context_menu(target, &shell, selection, position);
                return;
            }
            present_queue_track_context_menu(
                target,
                &shell,
                queue_playback_media(&row),
                row.primary_artist_key,
                &row.artist,
                occurrence,
                position,
            );
        }),
    );
}

impl QueueFullscreenColumnWidgets {
    fn apply(&self, mode: QueueFullscreenColumnMode) {
        self.album
            .set_visible(mode != QueueFullscreenColumnMode::TitleOnly);
        self.year
            .set_visible(mode == QueueFullscreenColumnMode::AlbumAndYear);
    }
}

pub(crate) struct QueueState {
    rows: RefCell<Vec<QueuePageRow>>,
    model: gio::ListStore,
    selection: RefCell<Option<gtk::MultiSelection>>,
    filter: RefCell<String>,
    generation: Cell<u64>,
    running: Cell<Option<u64>>,
    cancellation: RefCell<Option<(u64, library::ReadCancellation)>>,
    current: RefCell<Option<OccurrenceId>>,
    render_queued: Cell<bool>,
}

impl QueueState {
    pub(crate) fn new() -> Self {
        let model = gio::ListStore::new::<glib::BoxedAnyObject>();
        Self {
            rows: RefCell::new(Vec::new()),
            model,
            selection: RefCell::new(None),
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

    fn selection_model(&self) -> gtk::MultiSelection {
        if let Some(selection) = self.selection.borrow().as_ref() {
            return selection.clone();
        }
        let selection = gtk::MultiSelection::new(Some(self.model.clone()));
        self.selection.replace(Some(selection.clone()));
        selection
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

    pub(crate) fn needs_page_for_current(&self, current: Option<&OccurrenceId>) -> bool {
        self.filter.borrow().trim().is_empty()
            && current.is_some_and(|current| {
                !self
                    .rows
                    .borrow()
                    .iter()
                    .any(|row| row.object_id == current.as_str())
            })
    }

    fn reorder_request(
        &self,
        occurrence: OccurrenceId,
        target: OccurrenceId,
    ) -> Option<QueueReorderRequest> {
        let rows = self.rows.borrow();
        let occurrence_index = rows
            .iter()
            .position(|row| row.object_id == occurrence.as_str())?;
        let target_index = rows
            .iter()
            .position(|row| row.object_id == target.as_str())?;
        if occurrence_index == target_index {
            return None;
        }
        Some(QueueReorderRequest {
            occurrence,
            target: if occurrence_index < target_index {
                QueueReorderTarget::After(target)
            } else {
                QueueReorderTarget::Before(target)
            },
        })
    }

    fn selected_rows_for(
        &self,
        shell: &Shell,
        clicked: &OccurrenceId,
    ) -> Option<QueueSelectionSnapshot> {
        let positions = self.selection.borrow().as_ref()?.selection();
        let rows = queue_rows_at_positions(&self.rows.borrow(), &positions);
        (rows.len() > 1 && rows.iter().any(|row| row.object_id == clicked.as_str()))
            .then(|| QueueSelectionSnapshot::new(shell, rows))
    }

    fn dragged_rows_for(
        &self,
        shell: &Shell,
        clicked: &OccurrenceId,
    ) -> Option<QueueSelectionSnapshot> {
        self.selected_rows_for(shell, clicked).or_else(|| {
            self.rows
                .borrow()
                .iter()
                .find(|row| row.object_id == clicked.as_str())
                .cloned()
                .map(|row| QueueSelectionSnapshot::new(shell, vec![row]))
        })
    }

    pub(crate) fn selected_tracks(&self, shell: &Shell) -> Option<TrackSelectionSnapshot> {
        let positions = self.selection.borrow().as_ref()?.selection();
        let rows = queue_rows_at_positions(&self.rows.borrow(), &positions);
        (!rows.is_empty())
            .then(|| QueueSelectionSnapshot::new(shell, rows))
            .and_then(|selection| selection.tracks)
    }

    pub(crate) fn selected_occurrences(&self) -> Option<Arc<[OccurrenceId]>> {
        let positions = self.selection.borrow().as_ref()?.selection();
        let occurrences = queue_rows_at_positions(&self.rows.borrow(), &positions)
            .into_iter()
            .map(|row| OccurrenceId::new(row.object_id))
            .collect::<Vec<_>>();
        (!occurrences.is_empty()).then(|| occurrences.into())
    }
}

#[derive(Clone)]
struct QueueSelectionSnapshot {
    occurrences: Arc<[OccurrenceId]>,
    tracks: Option<TrackSelectionSnapshot>,
}

impl QueueSelectionSnapshot {
    fn new(shell: &Shell, rows: Vec<QueuePageRow>) -> Self {
        let occurrences = rows
            .iter()
            .map(|row| OccurrenceId::new(row.object_id.clone()))
            .collect::<Vec<_>>()
            .into();
        let tracks = shell.selected_library().as_deref().and_then(|selected| {
            let tracks = rows
                .iter()
                .filter_map(|row| row.track_key)
                .collect::<Vec<_>>();
            (!tracks.is_empty()).then(|| TrackSelectionSnapshot {
                source_key: selected.source_key,
                source_session_epoch: selected.source_session_epoch,
                tracks: tracks.into(),
            })
        });
        Self {
            occurrences,
            tracks,
        }
    }
}

fn queue_rows_at_positions(rows: &[QueuePageRow], positions: &gtk::Bitset) -> Vec<QueuePageRow> {
    let Some((iter, first)) = gtk::BitsetIter::init_first(positions) else {
        return Vec::new();
    };
    queue_rows_for_indexes(rows, std::iter::once(first).chain(iter))
}

fn queue_rows_for_indexes(
    rows: &[QueuePageRow],
    positions: impl Iterator<Item = u32>,
) -> Vec<QueuePageRow> {
    positions
        .filter_map(|position| rows.get(position as usize).cloned())
        .collect()
}

impl Drop for QueueState {
    fn drop(&mut self) {
        if let Some((_, cancellation)) = self.cancellation.get_mut().take() {
            cancellation.cancel();
        }
    }
}

impl Shell {
    pub(crate) fn clear_queue(&self) {
        let include_current = self.settings.current.borrow().clear_queue_includes_current;
        self.products.playback.queue.clear(include_current);
    }

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
            self.right_panel.queue_header_host.set_visible(false);
            self.player_view
                .fullscreen_player
                .queue_header
                .set_visible(false);
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
        let selection = queue.selection_model();
        render_panel(
            self,
            &self.right_panel.queue_panel,
            self.right_panel.queue_header_host.upcast_ref(),
            &queue.model,
            &selection,
            false,
            current_changed,
            reorderable,
        );
        if self.fullscreen_player_visible() {
            render_panel(
                self,
                &self.player_view.fullscreen_player.queue_panel,
                &self.player_view.fullscreen_player.queue_header,
                &queue.model,
                &selection,
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
                    let current_row = current_queue_row(&shell);
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

    pub(crate) fn position_startup_queue_for_reveal(self: &Rc<Self>) {
        if !self.right_panel.root.is_visible() {
            return;
        }
        let Some(scroller) = queue_panel_scroller(&self.right_panel.queue_panel) else {
            self.request_queue_page();
            return;
        };
        reveal_queue_current_row(&scroller, current_queue_row(self));
    }
}

fn render_panel(
    shell: &Rc<Shell>,
    panel: &gtk::Box,
    header: &gtk::Widget,
    model: &gio::ListStore,
    selection: &gtk::MultiSelection,
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
        factory.connect_bind(move |_, item| {
            let Some(shell) = bind_shell.upgrade() else {
                return;
            };
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
    if has_rows && reveal_current {
        reveal_queue_current_row_later(&scroller, current_queue_row(shell));
    }
}

fn fullscreen_queue_row(item: &gtk::ListItem) -> Option<QueueFullscreenRow> {
    item.child()?.first_child()?.downcast().ok()
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

fn reveal_queue_current_row_later(scroller: &gtk::ScrolledWindow, current_row: Option<usize>) {
    let scroller = scroller.clone();
    glib::idle_add_local_once(move || {
        reveal_queue_current_row(&scroller, current_row);
    });
}

fn current_queue_row(shell: &Shell) -> Option<usize> {
    shell.selected_queue().and_then(|queue| {
        let current = queue.current.borrow().clone()?;
        queue
            .rows
            .borrow()
            .iter()
            .position(|row| row.object_id == current.as_str())
    })
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
    let Some(current_row) = current_row.and_then(|position| u32::try_from(position).ok()) else {
        return false;
    };
    if scroller.has_css_class("right-panel-scroller") {
        let adjustment = scroller.vadjustment();
        let row_center = f64::from(current_row) * f64::from(QUEUE_ROW_HEIGHT)
            + f64::from(QUEUE_ROW_HEIGHT) / 2.0;
        let maximum = (adjustment.upper() - adjustment.page_size()).max(adjustment.lower());
        adjustment.set_value(
            (row_center - adjustment.page_size() / 2.0).clamp(adjustment.lower(), maximum),
        );
        return true;
    }
    let Some(list) = scroller
        .child()
        .and_then(|child| child.downcast::<gtk::Stack>().ok())
        .and_then(|stack| stack.child_by_name("list"))
        .and_then(|child| child.downcast::<gtk::ListView>().ok())
    else {
        return false;
    };
    list.scroll_to(current_row, gtk::ListScrollFlags::NONE, None);
    true
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

fn present_queue_selection_context_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    selection: QueueSelectionSnapshot,
    position: Option<(f64, f64)>,
) {
    let surface = ContextMenuSurface::new(target, "queue-selection", position);
    surface.append_fixed_action(msgid("Remove from Queue"), "remove-from-queue", REMOVE_ICON);
    surface.append_configurable_action(ContextMenuItem::Play, msgid("Play"), "play", PLAY_ICON);
    if let Some(tracks) = selection.tracks.as_ref() {
        if context_menu_can_add_to_playlist(shell) {
            append_context_menu_picker_selection(&surface, shell, tracks.clone());
        }
        install_track_selection_download_actions(&surface, shell, tracks.clone());
    }
    let queue = shell.products.playback.queue.clone();
    let remove = Arc::clone(&selection.occurrences);
    surface.add_action("remove-from-queue", move || {
        queue.remove_many(remove.to_vec())
    });
    let queue = shell.products.playback.queue.clone();
    let play = selection.occurrences.first().cloned();
    surface.add_action_enabled("play", play.is_some(), move || {
        if let Some(play) = play.as_ref() {
            queue.activate(play.clone());
        }
    });
    surface.popup(&shell.settings.current.borrow().context_menu);
}

pub(super) fn fullscreen_queue_column_owner(
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

fn fullscreen_queue_column_mode(available_width: i32) -> QueueFullscreenColumnMode {
    if available_width >= QUEUE_FULLSCREEN_SHOW_YEAR_WIDTH {
        QueueFullscreenColumnMode::AlbumAndYear
    } else if available_width >= QUEUE_FULLSCREEN_SHOW_ALBUM_WIDTH {
        QueueFullscreenColumnMode::Album
    } else {
        QueueFullscreenColumnMode::TitleOnly
    }
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
    if let Some(scroller) = queue_panel_scroller(panel) {
        scroller.set_child(None::<&gtk::Widget>);
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
    fn queue_drop_direction_follows_the_existing_order() {
        let state = QueueState::new();
        let (generation, _) = state.begin();
        assert!(state.accept(
            generation,
            vec![row("one", "One"), row("two", "Two"), row("three", "Three")]
        ));

        assert_eq!(
            state
                .reorder_request(OccurrenceId::new("one"), OccurrenceId::new("two"))
                .expect("moving down crosses the target")
                .target,
            QueueReorderTarget::After(OccurrenceId::new("two"))
        );
        assert_eq!(
            state
                .reorder_request(OccurrenceId::new("three"), OccurrenceId::new("two"))
                .expect("moving up crosses the target")
                .target,
            QueueReorderTarget::Before(OccurrenceId::new("two"))
        );
        assert!(
            state
                .reorder_request(OccurrenceId::new("two"), OccurrenceId::new("two"))
                .is_none()
        );
    }

    #[test]
    fn queue_selection_preserves_occurrence_order_and_repeated_tracks() {
        let mut first = row("one", "One");
        let mut second = row("two", "Two");
        let mut third = row("three", "Three");
        first.track_key = Some(library::TrackKey::from_raw(7));
        second.track_key = Some(library::TrackKey::from_raw(7));
        third.track_key = Some(library::TrackKey::from_raw(9));

        let selected = queue_rows_for_indexes(&[first, second, third], [0, 1].into_iter());
        assert_eq!(
            selected
                .iter()
                .map(|row| (row.object_id.as_str(), row.track_key))
                .collect::<Vec<_>>(),
            [
                ("one", Some(library::TrackKey::from_raw(7))),
                ("two", Some(library::TrackKey::from_raw(7))),
            ]
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
    fn current_outside_the_unfiltered_page_needs_a_centered_read() {
        let state = QueueState::new();
        let (generation, _) = state.begin();
        assert!(state.accept(generation, vec![row("one", "One"), row("two", "Two")]));
        assert!(!state.needs_page_for_current(Some(&OccurrenceId::new("one"))));
        assert!(state.needs_page_for_current(Some(&OccurrenceId::new("far"))));
        state.filter.replace("filtered".to_string());
        assert!(!state.needs_page_for_current(Some(&OccurrenceId::new("far"))));
        assert!(!state.needs_page_for_current(None));
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
