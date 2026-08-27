use super::collection_context::{
    present_album_context_menu, present_artist_context_menu, present_track_context_menu,
};
use crate::LibraryField;
use crate::favorites::{
    album_favorite_key, artist_favorite_key, favorite_button_is_active, set_favorite_button_active,
    track_favorite_key,
};
use crate::interactions::install_context_menu_openers;
use crate::shell::Shell;
use crate::shell::cover::{ArtworkTile, LARGE_COVER_SIZE};
use crate::shell::route::{MountedRouteItemNavigation, item_navigation_entry_position};
use ::library::{AlbumKey, AlbumRow, ArtistRow, FavoriteTarget, TrackRow};
use adw::prelude::*;
use gtk::{gio, glib};
use localization::msgid;
use playback::QueuePlacement;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use super::cards;
use super::collections::{PlaybackTarget, collection_grid_card, collection_grid_field_label};
use super::detail_links::{DetailLinkBinding, DetailLinks, album_artist_links};
use super::library_fields::{
    COLLECTION_GRID_CARD_MARGIN, COLLECTION_GRID_MAX_CARD_WIDTH, COLLECTION_GRID_MIN_CARD_WIDTH,
    album_field, artist_field, grid_title_with_label, item_at, item_at_from_item, opaque_artwork,
};
use super::route::Route;
use super::route_shell::restore_single_click_activation_on_primary_press;
use super::sparse_model::{bind_sparse_item, unbind_sparse_item};

fn collection_is_downloaded(track_count: i64, downloaded_count: i64) -> bool {
    track_count > 0 && downloaded_count == track_count
}

fn set_grid_favorite(button: &gtk::Button, favorite: Option<bool>) {
    set_favorite_button_active(button, favorite.unwrap_or(false));
    button.set_sensitive(favorite.is_some());
    if favorite.is_none() {
        button.set_visible(false);
    }
}

pub(super) trait ReusableCollectionGridCell<T>: 'static {
    fn widget(&self) -> gtk::Widget;
    fn activatable(&self, _: &T) -> bool {
        true
    }
    fn bind(&self, position: u32, value: T);
    fn clear(&self);
    fn set_ready(&self, _ready: bool) {}
    fn apply_fields(&self, fields: &[LibraryField]);
}

#[derive(Clone)]
pub(crate) struct CollectionGridProjection {
    surface: gtk::Widget,
    navigation: MountedRouteItemNavigation,
    fields: Rc<RefCell<Vec<LibraryField>>>,
    apply_fields: Rc<dyn Fn(&[LibraryField])>,
    cache_bound: CollectionGridCacheBound,
}

#[derive(Clone)]
struct CollectionGridCacheBound {
    grid: glib::WeakRef<gtk::GridView>,
}

impl CollectionGridCacheBound {
    fn fit_allocation(&self, allocation_width: i32) {
        let Some(grid) = self.grid.upgrade() else {
            return;
        };
        let maximum_columns =
            collection_grid_column_limit(allocation_width, grid.margin_start(), grid.margin_end());
        if grid.max_columns() == maximum_columns {
            return;
        }
        grid.set_max_columns(maximum_columns);
    }
}

fn collection_grid_column_limit(allocation_width: i32, margin_start: i32, margin_end: i32) -> u32 {
    let available_width = allocation_width
        .saturating_sub(margin_start)
        .saturating_sub(margin_end)
        .max(1);
    collection_grid_column_count(available_width).min(u32::MAX as usize) as u32
}

pub(super) fn collection_grid_column_count(available_width: i32) -> usize {
    let minimum_slot_width = COLLECTION_GRID_MIN_CARD_WIDTH
        .max(1)
        .saturating_add(COLLECTION_GRID_CARD_MARGIN.saturating_mul(2));
    let maximum_slot_width = COLLECTION_GRID_MAX_CARD_WIDTH
        .max(COLLECTION_GRID_MIN_CARD_WIDTH)
        .max(1)
        .saturating_add(COLLECTION_GRID_CARD_MARGIN.saturating_mul(2));
    let available_width = available_width.max(1);
    let maximum_fitting_columns = (available_width / minimum_slot_width).max(1);
    let minimum_needed_columns =
        available_width.saturating_add(maximum_slot_width - 1) / maximum_slot_width;
    minimum_needed_columns.max(1).min(maximum_fitting_columns) as usize
}

#[derive(Clone)]
pub(crate) struct FixedPageCollectionRow {
    row: gtk::FlowBox,
    page_size: Rc<Cell<usize>>,
}

impl FixedPageCollectionRow {
    pub(crate) fn widget(&self) -> gtk::FlowBox {
        self.row.clone()
    }

    pub(crate) fn set_page_size(&self, page_size: usize) {
        let page_size = page_size.max(1);
        if self.page_size.replace(page_size) == page_size {
            return;
        }
        self.row
            .set_max_children_per_line(page_size.min(u32::MAX as usize) as u32);
        self.row
            .set_min_children_per_line(page_size.min(u32::MAX as usize) as u32);
    }
}

#[derive(Clone)]
enum FixedPageSlot<T> {
    Item { position: u32, value: T },
    Empty,
}

fn refill_fixed_page_slots<T: Clone + 'static>(
    source: &gio::ListStore,
    presentation: &gio::ListStore,
    page_size: usize,
) {
    let additions = (0..page_size.max(1))
        .map(|index| {
            let slot = item_at::<T>(source, index as u32).map_or(FixedPageSlot::Empty, |value| {
                FixedPageSlot::Item {
                    position: index as u32,
                    value,
                }
            });
            glib::BoxedAnyObject::new(slot)
        })
        .collect::<Vec<_>>();
    presentation.splice(0, presentation.n_items(), &additions);
}

pub(super) fn fixed_page_collection_row<T, Make, Activate>(
    model: gio::ListStore,
    columns: usize,
    make_widget: Make,
    activate: Activate,
) -> FixedPageCollectionRow
where
    T: Clone + 'static,
    Make: Fn(u32, T) -> gtk::Widget + 'static,
    Activate: Fn(u32, T) + 'static,
{
    let columns = columns.max(1);
    let maximum_columns = columns.min(u32::MAX as usize) as u32;
    let row = gtk::FlowBox::new();
    row.add_css_class("album-grid");
    row.set_homogeneous(true);
    row.set_column_spacing(0);
    row.set_min_children_per_line(maximum_columns);
    row.set_max_children_per_line(maximum_columns);
    row.set_selection_mode(gtk::SelectionMode::None);
    row.set_activate_on_single_click(true);
    row.set_hexpand(true);
    row.set_vexpand(false);
    row.set_halign(gtk::Align::Fill);
    row.set_valign(gtk::Align::Start);

    let page_size = Rc::new(Cell::new(columns));
    let presentation = gio::ListStore::new::<glib::BoxedAnyObject>();
    row.bind_model(Some(&presentation), move |item| {
        let item = item
            .downcast_ref::<glib::BoxedAnyObject>()
            .expect("fixed Home row item type");
        match item.borrow::<FixedPageSlot<T>>().clone() {
            FixedPageSlot::Item { position, value } => {
                let widget = make_widget(position, value);
                cards::collection_grid_card_inset(&widget, COLLECTION_GRID_MIN_CARD_WIDTH).upcast()
            }
            FixedPageSlot::Empty => {
                let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
                spacer.set_can_target(false);
                spacer.set_focusable(false);
                spacer.set_sensitive(false);
                spacer.set_accessible_role(gtk::AccessibleRole::Presentation);
                cards::collection_grid_card_inset(&spacer, COLLECTION_GRID_MIN_CARD_WIDTH).upcast()
            }
        }
    });
    let activate_presentation = presentation.clone();
    row.connect_child_activated(move |_, child| {
        let position = child.index();
        if position < 0 {
            return;
        }
        let Some(item) = activate_presentation.item(position as u32) else {
            return;
        };
        let item = item
            .downcast_ref::<glib::BoxedAnyObject>()
            .expect("fixed Home activation item type");
        if let FixedPageSlot::Item { position, value } = item.borrow::<FixedPageSlot<T>>().clone() {
            activate(position, value);
        }
    });

    let change_presentation = presentation.clone();
    let change_page_size = Rc::clone(&page_size);
    model.connect_items_changed(move |source, _, _, _| {
        refill_fixed_page_slots::<T>(source, &change_presentation, change_page_size.get());
    });
    refill_fixed_page_slots::<T>(&model, &presentation, columns);
    FixedPageCollectionRow { row, page_size }
}

impl CollectionGridProjection {
    pub(crate) fn widget(&self) -> gtk::Widget {
        self.surface.clone()
    }

    pub(crate) fn apply_fields(&self, fields: &[LibraryField]) {
        if self.fields.borrow().as_slice() == fields {
            return;
        }
        *self.fields.borrow_mut() = fields.to_vec();
        (self.apply_fields)(fields);
    }

    pub(crate) fn fit_allocation(&self, width: i32) {
        self.cache_bound.fit_allocation(width);
    }

    pub(crate) fn navigate(&self, direction: gtk::DirectionType) -> glib::Propagation {
        (self.navigation)(direction)
    }
}

pub(super) fn collection_grid<T, Cell, Make, Activate, M>(
    model: M,
    fields: &[LibraryField],
    make_cell: Make,
    activate: Activate,
) -> CollectionGridProjection
where
    T: Clone + 'static,
    Cell: ReusableCollectionGridCell<T>,
    Make: Fn(&[LibraryField]) -> Cell + 'static,
    Activate: Fn(u32, T) + 'static,
    M: IsA<gio::ListModel> + Clone + 'static,
{
    collection_grid_with_demand(model, fields, make_cell, activate, |_| {})
}

pub(super) fn collection_grid_with_demand<T, Cell, Make, Activate, Demand, M>(
    model: M,
    fields: &[LibraryField],
    make_cell: Make,
    activate: Activate,
    demand: Demand,
) -> CollectionGridProjection
where
    T: Clone + 'static,
    Cell: ReusableCollectionGridCell<T>,
    Make: Fn(&[LibraryField]) -> Cell + 'static,
    Activate: Fn(u32, T) + 'static,
    Demand: Fn(u32) + 'static,
    M: IsA<gio::ListModel> + Clone + 'static,
{
    let selection = gtk::SingleSelection::new(Some(model.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    selection.set_selected(gtk::INVALID_LIST_POSITION);
    let factory = gtk::SignalListItemFactory::new();
    let cells = Rc::new(RefCell::new(HashMap::<usize, Cell>::new()));
    let fields = Rc::new(RefCell::new(fields.to_vec()));
    let setup_cells = Rc::clone(&cells);
    let setup_fields = Rc::clone(&fields);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = make_cell(&setup_fields.borrow());
        cell.clear();
        cell.set_ready(false);
        let child =
            cards::collection_grid_card_inset(&cell.widget(), COLLECTION_GRID_MIN_CARD_WIDTH);
        child.set_sensitive(false);
        child.set_can_target(false);
        item.set_child(Some(&child));
        let mut cells = setup_cells.borrow_mut();
        cells.insert(item.as_ptr() as usize, cell);
    });
    let bind_cells = Rc::clone(&cells);
    let demand = Rc::new(demand);
    let bind_demand = Rc::clone(&demand);
    factory.connect_bind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        bind_demand(item.position());
        let ready_cells = Rc::clone(&bind_cells);
        bind_sparse_item(
            item,
            Rc::new(move |item| {
                let cells = ready_cells.borrow();
                let Some(cell) = cells.get(&(item.as_ptr() as usize)) else {
                    return;
                };
                if let Some(value) = item_at_from_item::<T>(item) {
                    cell.set_ready(true);
                    if let Some(child) = item.child() {
                        child.set_sensitive(true);
                        child.set_can_target(true);
                    }
                    cell.widget().set_sensitive(true);
                    cell.widget().set_can_target(true);
                    let activatable = cell.activatable(&value);
                    item.set_activatable(activatable);
                    item.set_selectable(activatable);
                    cell.bind(item.position(), value);
                } else {
                    item.set_activatable(false);
                    item.set_selectable(false);
                    cell.clear();
                    cell.set_ready(false);
                    if let Some(child) = item.child() {
                        child.set_sensitive(false);
                        child.set_can_target(false);
                    }
                    cell.widget().set_sensitive(false);
                    cell.widget().set_can_target(false);
                }
            }),
        );
    });
    let unbind_cells = Rc::clone(&cells);
    factory.connect_unbind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        unbind_sparse_item(item);
        item.set_activatable(false);
        item.set_selectable(false);
        if let Some(cell) = unbind_cells.borrow().get(&(item.as_ptr() as usize)) {
            cell.clear();
            cell.set_ready(false);
            if let Some(child) = item.child() {
                child.set_sensitive(false);
                child.set_can_target(false);
            }
            cell.widget().set_sensitive(false);
            cell.widget().set_can_target(false);
        }
    });
    let teardown_cells = Rc::clone(&cells);
    factory.connect_teardown(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        unbind_sparse_item(item);
        teardown_cells
            .borrow_mut()
            .remove(&(item.as_ptr() as usize));
        item.set_child(None::<&gtk::Widget>);
    });

    let grid = gtk::GridView::new(Some(selection.clone()), Some(factory));
    grid.add_css_class("album-grid");
    grid.set_focus_on_click(false);
    grid.set_min_columns(1);
    // GtkGridView keeps up to 30 rows times max-columns alive. Start with the
    // smallest cache bound; the allocation owner raises this to the number of
    // cards in the allocated width before the first allocation.
    grid.set_max_columns(1);
    grid.set_single_click_activate(true);
    restore_single_click_activation_on_primary_press(&grid, |grid| {
        grid.set_single_click_activate(true);
    });
    grid.set_hexpand(true);
    grid.set_vexpand(true);
    grid.connect_activate(move |_, position| {
        demand(position);
        if let Some(value) = item_at::<T>(&model, position) {
            activate(position, value);
        }
    });
    let navigation_grid = grid.clone();
    let navigation_selection = selection;
    let navigation = Rc::new(move |direction| {
        navigation_grid.set_single_click_activate(false);
        if navigation_grid
            .state_flags()
            .contains(gtk::StateFlags::FOCUS_WITHIN)
        {
            return glib::Propagation::Proceed;
        }
        let Some(model) = navigation_grid.model() else {
            return glib::Propagation::Stop;
        };
        if let Some(position) = item_navigation_entry_position(
            navigation_selection.selected(),
            model.n_items(),
            direction,
        ) {
            navigation_selection.set_selected(position);
            navigation_grid.scroll_to(position, gtk::ListScrollFlags::FOCUS, None);
            navigation_grid.grab_focus();
        }
        glib::Propagation::Stop
    }) as MountedRouteItemNavigation;
    let cache_bound = CollectionGridCacheBound {
        grid: grid.downgrade(),
    };
    let apply_cells = Rc::clone(&cells);
    CollectionGridProjection {
        surface: grid.upcast(),
        navigation,
        fields,
        apply_fields: Rc::new(move |fields| {
            for cell in apply_cells.borrow().values() {
                cell.apply_fields(fields);
            }
        }),
        cache_bound,
    }
}

fn album_playback_target(album_id: AlbumKey, context: Option<&str>) -> PlaybackTarget {
    let target = PlaybackTarget::Album(album_id);
    context
        .map(|context| target.clone().in_context(context))
        .unwrap_or(target)
}

fn play_one_track(shell: &Shell, track: TrackRow, placement: QueuePlacement) {
    let Some(selected) = shell.selected_library().as_deref().cloned() else {
        return;
    };
    if let Some(request) = selected.one_track(playback::PlaybackMedia::from(track), placement) {
        shell.products.playback.queue.play_loaded(request);
    }
}

pub(super) struct TrackGridCell {
    body: CollectionGridCardCell,
    shell: std::rc::Weak<Shell>,
    cover: ArtworkTile,
    favorite: gtk::Button,
    current: Rc<RefCell<Option<TrackRow>>>,
    position: Rc<Cell<u32>>,
    field: Rc<dyn Fn(u32, &TrackRow, LibraryField) -> DetailLinks>,
}

impl TrackGridCell {
    pub(super) fn new(
        shell: &Rc<Shell>,
        fields: &[LibraryField],
        play: Rc<dyn Fn(u32)>,
        field: Rc<dyn Fn(u32, &TrackRow, LibraryField) -> DetailLinks>,
    ) -> Self {
        let current = Rc::new(RefCell::new(None::<TrackRow>));
        let position = Rc::new(Cell::new(0));
        let overlay = cards::elastic_cover_overlay();
        let cover_button = collection_grid_cover_shell();
        let cover = ArtworkTile::new_elastic_square();
        cover_button.set_child(Some(&cover.widget()));
        let open_play = Rc::clone(&play);
        let open_position = Rc::clone(&position);
        cover_button.connect_clicked(move |_| open_play(open_position.get()));
        overlay.set_child(Some(&cover_button));
        let (mut controls, favorite) =
            cards::cover_hover_controls_with_favorite(0, msgid("Play track"), false);
        let menu = controls.add_context_button();
        let menu_shell = Rc::downgrade(shell);
        let menu_current = Rc::clone(&current);
        let menu_target = overlay.downgrade();
        menu.connect_clicked(move |_| {
            let (Some(shell), Some(row), Some(target)) = (
                menu_shell.upgrade(),
                menu_current.borrow().as_ref().cloned(),
                menu_target.upgrade(),
            ) else {
                return;
            };
            present_track_context_menu(
                target.upcast_ref(),
                &shell,
                row,
                cards::elastic_cover_context_point(&target),
            );
        });
        let play_now = Rc::clone(&play);
        let play_position = Rc::clone(&position);
        controls
            .play
            .connect_clicked(move |_| play_now(play_position.get()));
        for (button, placement) in [
            (&controls.play_next, QueuePlacement::Next),
            (&controls.play_last, QueuePlacement::Last),
        ] {
            let action_shell = Rc::downgrade(shell);
            let action_current = Rc::clone(&current);
            button.connect_clicked(move |_| {
                if let (Some(shell), Some(row)) = (
                    action_shell.upgrade(),
                    action_current.borrow().as_ref().cloned(),
                ) {
                    play_one_track(&shell, row, placement);
                }
            });
        }
        let favorite_shell = Rc::downgrade(shell);
        let favorite_current = Rc::clone(&current);
        favorite.connect_clicked(move |button| {
            let (Some(shell), Some(row)) = (
                favorite_shell.upgrade(),
                favorite_current.borrow().as_ref().cloned(),
            ) else {
                return;
            };
            shell.set_favorite_with_feedback(
                FavoriteTarget::Track(row.track_key),
                !favorite_button_is_active(button),
                Some(button),
            );
        });
        let favorite_key = Rc::clone(&current);
        shell.register_dynamic_favorite_button(
            Rc::new(move || {
                favorite_key
                    .borrow()
                    .as_ref()
                    .map(|row| track_favorite_key(&row.track_key))
            }),
            &favorite,
        );
        controls.add_to_overlay(&overlay);
        controls.connect_hover(&overlay);
        let framed = cards::square_cover_frame(&overlay, &controls.transport);
        let body = CollectionGridCardCell::new(shell, fields, framed.upcast());
        body.set_download_badge(shell.download_badge(false));
        let context_shell = Rc::downgrade(shell);
        let context_current = Rc::clone(&current);
        install_context_menu_openers(
            &body.card,
            Rc::new(move |target, point| {
                if let (Some(shell), Some(row)) = (
                    context_shell.upgrade(),
                    context_current.borrow().as_ref().cloned(),
                ) {
                    present_track_context_menu(target, &shell, row, point);
                }
            }),
        );
        Self {
            body,
            shell: Rc::downgrade(shell),
            cover,
            favorite,
            current,
            position,
            field,
        }
    }
}

impl ReusableCollectionGridCell<TrackRow> for TrackGridCell {
    fn widget(&self) -> gtk::Widget {
        self.body.widget()
    }
    fn bind(&self, position: u32, row: TrackRow) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        shell.bind_artwork_tile(
            &self.cover,
            opaque_artwork(row.artwork_binding.as_deref()),
            COLLECTION_GRID_MAX_CARD_WIDTH,
            LARGE_COVER_SIZE,
        );
        self.body
            .bind(&row.title, |value| (self.field)(position, &row, value));
        self.body.set_download_target(&shell, row.is_downloaded);
        set_grid_favorite(&self.favorite, Some(row.favorite));
        self.current.replace(Some(row));
        self.position.set(position);
    }
    fn clear(&self) {
        if let Some(shell) = self.shell.upgrade() {
            shell.clear_artwork_tile(&self.cover);
            self.body.clear(&shell);
        }
        set_grid_favorite(&self.favorite, None);
        self.current.take();
    }
    fn set_ready(&self, ready: bool) {
        self.body.set_ready(ready);
    }
    fn apply_fields(&self, fields: &[LibraryField]) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        self.body.replace_fields(&shell, fields);
        if let Some(row) = self.current.borrow().as_ref() {
            let position = self.position.get();
            self.body
                .bind(&row.title, |value| (self.field)(position, row, value));
        }
    }
}

pub(super) struct AlbumGridCell {
    body: CollectionGridCardCell,
    shell: std::rc::Weak<Shell>,
    cover: ArtworkTile,
    favorite: gtk::Button,
    current: Rc<RefCell<Option<AlbumRow>>>,
}

impl AlbumGridCell {
    pub(super) fn new(shell: &Rc<Shell>, fields: &[LibraryField], context: Option<String>) -> Self {
        let current = Rc::new(RefCell::new(None::<AlbumRow>));
        let overlay = cards::elastic_cover_overlay();
        let cover_button = collection_grid_cover_shell();
        let cover = ArtworkTile::new_elastic_square();
        cover_button.set_child(Some(&cover.widget()));
        let open_shell = Rc::downgrade(shell);
        let open_current = Rc::clone(&current);
        cover_button.connect_clicked(move |_| {
            if let (Some(shell), Some(row)) = (
                open_shell.upgrade(),
                open_current.borrow().as_ref().cloned(),
            ) {
                shell.navigate(Route::AlbumDetail(row.album_key));
            }
        });
        overlay.set_child(Some(&cover_button));
        let (mut controls, favorite) =
            cards::cover_hover_controls_with_favorite(0, msgid("Play album"), false);
        let menu = controls.add_context_button();
        let menu_shell = Rc::downgrade(shell);
        let menu_current = Rc::clone(&current);
        let menu_target = overlay.downgrade();
        let menu_context = context.clone();
        menu.connect_clicked(move |_| {
            let (Some(shell), Some(row), Some(target)) = (
                menu_shell.upgrade(),
                menu_current.borrow().as_ref().cloned(),
                menu_target.upgrade(),
            ) else {
                return;
            };
            present_album_context_menu(
                target.upcast_ref(),
                &shell,
                row,
                menu_context.clone(),
                None,
                cards::elastic_cover_context_point(&target),
            );
        });
        for (button, placement) in [
            (&controls.play, QueuePlacement::Now),
            (&controls.play_next, QueuePlacement::Next),
            (&controls.play_last, QueuePlacement::Last),
        ] {
            let action_shell = Rc::downgrade(shell);
            let action_current = Rc::clone(&current);
            let action_context = context.clone();
            button.connect_clicked(move |_| {
                if let (Some(shell), Some(row)) = (
                    action_shell.upgrade(),
                    action_current.borrow().as_ref().cloned(),
                ) {
                    album_playback_target(row.album_key, action_context.as_deref()).play(
                        &shell,
                        placement,
                        placement == QueuePlacement::Now,
                    );
                }
            });
        }
        let favorite_shell = Rc::downgrade(shell);
        let favorite_current = Rc::clone(&current);
        favorite.connect_clicked(move |button| {
            let (Some(shell), Some(row)) = (
                favorite_shell.upgrade(),
                favorite_current.borrow().as_ref().cloned(),
            ) else {
                return;
            };
            shell.set_favorite_with_feedback(
                FavoriteTarget::Album(row.album_key),
                !favorite_button_is_active(button),
                Some(button),
            );
        });
        let favorite_key = Rc::clone(&current);
        shell.register_dynamic_favorite_button(
            Rc::new(move || {
                favorite_key
                    .borrow()
                    .as_ref()
                    .map(|row| album_favorite_key(&row.album_key))
            }),
            &favorite,
        );
        controls.add_to_overlay(&overlay);
        controls.connect_hover(&overlay);
        let framed = cards::square_cover_frame(&overlay, &controls.transport);
        let body = CollectionGridCardCell::new(shell, fields, framed.upcast());
        body.set_download_badge(shell.download_badge(true));
        let context_shell = Rc::downgrade(shell);
        let context_current = Rc::clone(&current);
        let body_context = context.clone();
        install_context_menu_openers(
            &body.card,
            Rc::new(move |target, point| {
                if let (Some(shell), Some(row)) = (
                    context_shell.upgrade(),
                    context_current.borrow().as_ref().cloned(),
                ) {
                    present_album_context_menu(
                        target,
                        &shell,
                        row,
                        body_context.clone(),
                        None,
                        point,
                    );
                }
            }),
        );
        Self {
            body,
            shell: Rc::downgrade(shell),
            cover,
            favorite,
            current,
        }
    }
}

pub(super) struct ArtistGridCell {
    body: CollectionGridCardCell,
    shell: std::rc::Weak<Shell>,
    cover: ArtworkTile,
    favorite: gtk::Button,
    current: Rc<RefCell<Option<ArtistRow>>>,
}

impl ArtistGridCell {
    pub(super) fn new(shell: &Rc<Shell>, fields: &[LibraryField], album_artist: bool) -> Self {
        let current = Rc::new(RefCell::new(None::<ArtistRow>));
        let overlay = cards::elastic_cover_overlay();
        let cover_button = collection_grid_cover_shell();
        let cover = ArtworkTile::new_elastic_square();
        cover_button.set_child(Some(&cover.widget()));
        let open_shell = Rc::downgrade(shell);
        let open_current = Rc::clone(&current);
        cover_button.connect_clicked(move |_| {
            if let (Some(shell), Some(row)) = (
                open_shell.upgrade(),
                open_current.borrow().as_ref().cloned(),
            ) {
                shell.navigate(super::artist::artist_detail_route(
                    row.artist_key,
                    album_artist,
                ));
            }
        });
        overlay.set_child(Some(&cover_button));
        let (mut controls, favorite) =
            cards::cover_hover_controls_with_favorite(0, msgid("Play artist"), false);
        let menu = controls.add_context_button();
        let menu_shell = Rc::downgrade(shell);
        let menu_current = Rc::clone(&current);
        let menu_target = overlay.downgrade();
        menu.connect_clicked(move |_| {
            let (Some(shell), Some(row), Some(target)) = (
                menu_shell.upgrade(),
                menu_current.borrow().as_ref().cloned(),
                menu_target.upgrade(),
            ) else {
                return;
            };
            present_artist_context_menu(
                target.upcast_ref(),
                &shell,
                row,
                album_artist,
                None,
                cards::elastic_cover_context_point(&target),
            );
        });
        for (button, placement) in [
            (&controls.play, QueuePlacement::Now),
            (&controls.play_next, QueuePlacement::Next),
            (&controls.play_last, QueuePlacement::Last),
        ] {
            let action_shell = Rc::downgrade(shell);
            let action_current = Rc::clone(&current);
            button.connect_clicked(move |_| {
                if let (Some(shell), Some(row)) = (
                    action_shell.upgrade(),
                    action_current.borrow().as_ref().cloned(),
                ) {
                    let target = if album_artist {
                        PlaybackTarget::AlbumArtist(row.artist_key)
                    } else {
                        PlaybackTarget::Artist(row.artist_key)
                    };
                    target.play(&shell, placement, placement == QueuePlacement::Now);
                }
            });
        }
        let favorite_shell = Rc::downgrade(shell);
        let favorite_current = Rc::clone(&current);
        favorite.connect_clicked(move |button| {
            let (Some(shell), Some(row)) = (
                favorite_shell.upgrade(),
                favorite_current.borrow().as_ref().cloned(),
            ) else {
                return;
            };
            shell.set_favorite_with_feedback(
                FavoriteTarget::Artist(row.artist_key),
                !favorite_button_is_active(button),
                Some(button),
            );
        });
        let favorite_key = Rc::clone(&current);
        shell.register_dynamic_favorite_button(
            Rc::new(move || {
                favorite_key
                    .borrow()
                    .as_ref()
                    .map(|row| artist_favorite_key(&row.artist_key))
            }),
            &favorite,
        );
        controls.add_to_overlay(&overlay);
        controls.connect_hover(&overlay);
        let framed = cards::square_cover_frame(&overlay, &controls.transport);
        let body = CollectionGridCardCell::new(shell, fields, framed.upcast());
        body.set_download_badge(shell.download_badge(true));
        let context_shell = Rc::downgrade(shell);
        let context_current = Rc::clone(&current);
        install_context_menu_openers(
            &body.card,
            Rc::new(move |target, point| {
                if let (Some(shell), Some(row)) = (
                    context_shell.upgrade(),
                    context_current.borrow().as_ref().cloned(),
                ) {
                    present_artist_context_menu(target, &shell, row, album_artist, None, point);
                }
            }),
        );
        Self {
            body,
            shell: Rc::downgrade(shell),
            cover,
            favorite,
            current,
        }
    }
}

impl ReusableCollectionGridCell<ArtistRow> for ArtistGridCell {
    fn widget(&self) -> gtk::Widget {
        self.body.widget()
    }
    fn bind(&self, _: u32, row: ArtistRow) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        shell.bind_artwork_tile(
            &self.cover,
            opaque_artwork(row.artwork_binding.as_deref()),
            COLLECTION_GRID_MAX_CARD_WIDTH,
            LARGE_COVER_SIZE,
        );
        self.body.bind(&row.name, |field| {
            DetailLinks::text(&artist_field(&row, field))
        });
        self.body.set_download_target(
            &shell,
            collection_is_downloaded(row.track_count, row.downloaded_count),
        );
        set_grid_favorite(&self.favorite, Some(row.favorite));
        self.current.replace(Some(row));
    }
    fn clear(&self) {
        if let Some(shell) = self.shell.upgrade() {
            shell.clear_artwork_tile(&self.cover);
            self.body.clear(&shell);
        }
        set_grid_favorite(&self.favorite, None);
        self.current.take();
    }
    fn set_ready(&self, ready: bool) {
        self.body.set_ready(ready);
    }
    fn apply_fields(&self, fields: &[LibraryField]) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        self.body.replace_fields(&shell, fields);
        if let Some(row) = self.current.borrow().as_ref() {
            self.body.bind(&row.name, |field| {
                DetailLinks::text(&artist_field(row, field))
            });
        }
    }
}

impl ReusableCollectionGridCell<AlbumRow> for AlbumGridCell {
    fn widget(&self) -> gtk::Widget {
        self.body.widget()
    }
    fn bind(&self, _: u32, row: AlbumRow) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        shell.bind_artwork_tile(
            &self.cover,
            opaque_artwork(row.artwork_binding.as_deref()),
            COLLECTION_GRID_MAX_CARD_WIDTH,
            LARGE_COVER_SIZE,
        );
        self.body.bind(&row.title, |field| {
            let value = album_field(&row, field);
            if value.is_empty()
                || !matches!(field, LibraryField::Artist | LibraryField::AlbumArtist)
            {
                DetailLinks::text(&value)
            } else {
                album_artist_links(&row)
            }
        });
        self.body.set_download_target(
            &shell,
            collection_is_downloaded(row.track_count, row.downloaded_count),
        );
        set_grid_favorite(&self.favorite, Some(row.favorite));
        self.current.replace(Some(row));
    }
    fn clear(&self) {
        if let Some(shell) = self.shell.upgrade() {
            shell.clear_artwork_tile(&self.cover);
            self.body.clear(&shell);
        }
        set_grid_favorite(&self.favorite, None);
        self.current.take();
    }
    fn set_ready(&self, ready: bool) {
        self.body.set_ready(ready);
    }
    fn apply_fields(&self, fields: &[LibraryField]) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        self.body.replace_fields(&shell, fields);
        if let Some(row) = self.current.borrow().as_ref() {
            self.body.bind(&row.title, |field| {
                DetailLinks::text(&album_field(row, field))
            });
        }
    }
}

pub(super) struct CollectionGridCardCell {
    pub(super) card: gtk::Box,
    title: gtk::Label,
    title_row: gtk::Box,
    downloaded: RefCell<Option<gtk::Image>>,
    fields: RefCell<Vec<CollectionGridFieldCell>>,
    ready: Cell<bool>,
}

impl CollectionGridCardCell {
    pub(super) fn new(shell: &Rc<Shell>, fields: &[LibraryField], cover: gtk::Widget) -> Self {
        let card = collection_grid_card();
        card.append(&cover);
        let (title_widget, title) = grid_title_with_label("", "track-title");
        title.set_halign(gtk::Align::Start);
        title.set_hexpand(false);
        let title_row = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        title_row.set_hexpand(true);
        title_row.append(&title_widget);
        card.append(&title_row);
        let field_cells = fields
            .iter()
            .copied()
            .map(|field| CollectionGridFieldCell::new(shell, field))
            .collect::<Vec<_>>();
        for field in &field_cells {
            card.append(&field.widget);
        }
        let cell = Self {
            card,
            title,
            title_row,
            downloaded: RefCell::new(None),
            fields: RefCell::new(field_cells),
            ready: Cell::new(false),
        };
        cell
    }

    pub(super) fn set_download_badge(&self, badge: gtk::Image) {
        if let Some(previous) = self.downloaded.replace(Some(badge.clone())) {
            self.title_row.remove(&previous);
        }
        self.title_row.append(&badge);
    }

    pub(super) fn set_download_target(&self, shell: &Rc<Shell>, downloaded: bool) {
        if let Some(badge) = self.downloaded.borrow().as_ref() {
            shell.bind_download_badge(badge, downloaded);
        }
    }

    pub(super) fn widget(&self) -> gtk::Widget {
        self.card.clone().upcast()
    }

    pub(super) fn bind(
        &self,
        title: &str,
        mut field_value: impl FnMut(LibraryField) -> DetailLinks,
    ) {
        self.title.set_text(title);
        self.title
            .set_tooltip_text((!title.is_empty()).then_some(title));
        for field in self.fields.borrow().iter() {
            field.bind(field_value(field.field));
        }
    }

    pub(super) fn clear(&self, shell: &Rc<Shell>) {
        self.title.set_text("");
        self.title.set_tooltip_text(None);
        if let Some(downloaded) = self.downloaded.borrow().as_ref() {
            shell.clear_download_badge(downloaded);
        }
        for field in self.fields.borrow().iter() {
            field.clear();
        }
    }

    pub(super) fn set_ready(&self, ready: bool) {
        self.ready.set(ready);
        for widget in std::iter::once(self.title.clone().upcast::<gtk::Widget>()).chain(
            self.fields
                .borrow()
                .iter()
                .map(|field| field.widget.clone()),
        ) {
            if ready {
                widget.remove_css_class("collection-grid-text-skeleton");
            } else {
                widget.add_css_class("collection-grid-text-skeleton");
            }
        }
    }

    pub(super) fn replace_fields(&self, shell: &Rc<Shell>, fields: &[LibraryField]) {
        if self
            .fields
            .borrow()
            .iter()
            .map(|field| field.field)
            .eq(fields.iter().copied())
        {
            return;
        }
        for field in self.fields.take() {
            self.card.remove(&field.widget);
        }
        let next = fields
            .iter()
            .copied()
            .map(|field| CollectionGridFieldCell::new(shell, field))
            .collect::<Vec<_>>();
        if !self.ready.get() {
            for field in &next {
                field.widget.add_css_class("collection-grid-text-skeleton");
            }
        }
        for field in &next {
            self.card.append(&field.widget);
        }
        self.fields.replace(next);
    }
}

pub(super) fn collection_grid_cover_shell() -> gtk::Button {
    let cover_button = gtk::Button::new();
    cover_button.add_css_class("album-cover-button");
    cover_button.add_css_class("flat");
    cards::clip_cover(&cover_button);
    cover_button.set_width_request(1);
    cover_button.set_height_request(1);
    cover_button.set_hexpand(true);
    cover_button.set_vexpand(true);
    cover_button.set_halign(gtk::Align::Fill);
    cover_button.set_valign(gtk::Align::Fill);
    cover_button.set_focus_on_click(false);
    cover_button
}

struct CollectionGridFieldCell {
    field: LibraryField,
    widget: gtk::Widget,
    label: gtk::Label,
    links: DetailLinkBinding,
}

impl CollectionGridFieldCell {
    fn new(shell: &Rc<Shell>, field: LibraryField) -> Self {
        let (widget, label) = collection_grid_field_label("", field);
        let links = DetailLinkBinding::new(&label, shell);
        Self {
            field,
            widget,
            label,
            links,
        }
    }

    fn bind(&self, links: DetailLinks) {
        self.links.bind(links);
        let value = self.label.text();
        self.label
            .set_tooltip_text((!value.is_empty()).then_some(value.as_str()));
    }

    fn clear(&self) {
        self.links.clear();
        self.label.set_tooltip_text(None);
        self.widget.set_cursor_from_name(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_grid_columns_follow_the_shared_card_width_band() {
        let minimum_slot = COLLECTION_GRID_MIN_CARD_WIDTH + COLLECTION_GRID_CARD_MARGIN * 2;
        let maximum_slot = COLLECTION_GRID_MAX_CARD_WIDTH + COLLECTION_GRID_CARD_MARGIN * 2;
        assert_eq!(collection_grid_column_count(435), 3);
        assert_eq!(collection_grid_column_count(450), 3);
        assert_eq!(collection_grid_column_count(496), 3);
        assert_eq!(collection_grid_column_count(minimum_slot * 3 - 1), 2);
        assert_eq!(collection_grid_column_count(maximum_slot * 2 + 1), 3);
        assert_eq!(collection_grid_column_limit(600, 0, 0), 3);
        assert_eq!(collection_grid_column_count(1_600), 8);
        assert_eq!(collection_grid_column_count(3_200), 16);
    }
}
