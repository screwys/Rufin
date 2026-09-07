use super::collection_context::{present_album_context_menu, present_artist_context_menu};
use crate::LibraryField;
use crate::favorites::{
    FavoriteControlKey, album_favorite_key, artist_favorite_key, favorite_button_is_active,
    set_favorite_button_active, track_favorite_key,
};
use crate::interactions::install_context_menu_openers;
use crate::shell::Shell;
use crate::shell::cover::{ArtworkTile, LARGE_COVER_SIZE};
use crate::shell::route::{MountedRouteItemNavigation, item_navigation_entry_position};
use ::library::{AlbumRow, ArtistRow, FavoriteTarget};
use adw::prelude::*;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use gtk::{gio, glib};
use localization::msgid;
use playback::QueuePlacement;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use super::cards;
use super::collections::{PlaybackTarget, collection_grid_field_label};
use super::detail_links::{DetailLinkBinding, DetailLinks, album_artist_links};
use super::library_fields::{
    COLLECTION_GRID_CARD_MARGIN, COLLECTION_GRID_MAX_CARD_WIDTH, COLLECTION_GRID_MIN_CARD_WIDTH,
    album_field, artist_field, item_at, item_at_from_item, opaque_artwork,
};
use super::playlist_picker::{MediaDragSource, install_compact_media_drag_source};
use super::sparse_model::{bind_sparse_item, unbind_sparse_item};
use super::track_selection::TrackSelection;

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

fn install_grid_favorite<T: 'static>(
    shell: &Rc<Shell>,
    current: &Rc<RefCell<Option<T>>>,
    button: &gtk::Button,
    target: impl Fn(&T) -> FavoriteTarget + 'static,
    key: impl Fn(&T) -> FavoriteControlKey + 'static,
) {
    let target = Rc::new(target);
    let click_shell = Rc::downgrade(shell);
    let click_current = Rc::clone(current);
    let click_target = Rc::clone(&target);
    button.connect_clicked(move |button| {
        let Some(shell) = click_shell.upgrade() else {
            return;
        };
        let current = click_current.borrow();
        let Some(row) = current.as_ref() else { return };
        shell.set_favorite_with_feedback(
            click_target(row),
            !favorite_button_is_active(button),
            Some(button),
        );
    });
    let favorite_current = Rc::clone(current);
    shell.register_dynamic_favorite_button(
        Rc::new(move || favorite_current.borrow().as_ref().map(&key)),
        button,
    );
}

pub(super) fn install_grid_context<T: Clone + 'static>(
    shell: &Rc<Shell>,
    current: &Rc<RefCell<Option<T>>>,
    menu: &gtk::Button,
    overlay: &gtk::Overlay,
    card: &impl IsA<gtk::Widget>,
    present: impl Fn(&gtk::Widget, &Rc<Shell>, T, Option<(f64, f64)>) + 'static,
) {
    let present = Rc::new(present);
    let menu_shell = Rc::downgrade(shell);
    let menu_current = Rc::clone(current);
    let menu_target = overlay.downgrade();
    let menu_present = Rc::clone(&present);
    menu.connect_clicked(move |_| {
        let (Some(shell), Some(row), Some(target)) = (
            menu_shell.upgrade(),
            menu_current.borrow().as_ref().cloned(),
            menu_target.upgrade(),
        ) else {
            return;
        };
        menu_present(
            target.upcast_ref(),
            &shell,
            row,
            cards::elastic_cover_context_point(&target),
        );
    });
    let context_shell = Rc::downgrade(shell);
    let context_current = Rc::clone(current);
    install_context_menu_openers(
        card,
        Rc::new(move |target, point| {
            if let (Some(shell), Some(row)) = (
                context_shell.upgrade(),
                context_current.borrow().as_ref().cloned(),
            ) {
                present(target, &shell, row, point);
            }
        }),
    );
}

pub(super) fn install_grid_play<T: 'static>(
    shell: &Rc<Shell>,
    current: &Rc<RefCell<Option<T>>>,
    buttons: [gtk::Button; 3],
    play: impl Fn(&Rc<Shell>, &T, QueuePlacement) + 'static,
) {
    let play = Rc::new(play);
    for (button, placement) in buttons.into_iter().zip([
        QueuePlacement::Now,
        QueuePlacement::Next,
        QueuePlacement::Last,
    ]) {
        let shell = Rc::downgrade(shell);
        let current = Rc::clone(current);
        let play = Rc::clone(&play);
        button.connect_clicked(move |_| {
            let Some(shell) = shell.upgrade() else { return };
            let current = current.borrow();
            if let Some(row) = current.as_ref() {
                play(&shell, row, placement);
            }
        });
    }
}

crate::ui_resource::composite_box!(
    pub(crate) CollectionGridCardView,
    collection_grid_card_view_imp,
    "RufinCollectionGridCardView",
    "/io/github/screwys/Rufin/ui/routes/collection_grid_card.ui",
    {
        title_row: gtk::Box,
        title: gtk::Label,
    }
);

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
    row.set_activate_on_single_click(false);
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
    collection_grid_with_selection_and_demand(model, None, fields, make_cell, activate, demand)
}

pub(super) fn collection_grid_with_selection_and_demand<T, Cell, Make, Activate, Demand, M>(
    model: M,
    selection: Option<gtk::SelectionModel>,
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
    let selection =
        selection.unwrap_or_else(|| gtk::MultiSelection::new(Some(model.clone())).upcast());
    let factory = gtk::SignalListItemFactory::new();
    let cells = Rc::new(RefCell::new(Vec::<
        glib::WeakRef<cards::CollectionGridCardInset>,
    >::new()));
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
        child.set_cell(cell);
        item.set_child(Some(&child));
        setup_cells.borrow_mut().push(child.downgrade());
    });
    let demand = Rc::new(demand);
    let bind_demand = Rc::clone(&demand);
    factory.connect_bind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        bind_demand(item.position());
        bind_sparse_item(
            item,
            Rc::new(move |item| {
                with_grid_cell::<Cell, _>(item, |cell| {
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
                });
            }),
        );
    });
    factory.connect_unbind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        unbind_sparse_item(item);
        item.set_activatable(false);
        item.set_selectable(false);
        with_grid_cell::<Cell, _>(item, |cell| {
            cell.clear();
        });
    });
    factory.connect_teardown(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        unbind_sparse_item(item);
        if let Some(cell) = item
            .child()
            .and_then(|child| child.downcast::<cards::CollectionGridCardInset>().ok())
        {
            cell.clear_cell();
        }
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
    grid.set_single_click_activate(false);
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
            selected_position(&navigation_selection),
            model.n_items(),
            direction,
        ) {
            navigation_selection.select_item(position, true);
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
            apply_cells.borrow_mut().retain(|cell| {
                cell.upgrade().is_some_and(|cell| {
                    cell.with_cell::<Cell, _>(|cell| cell.apply_fields(fields))
                        .is_some()
                })
            });
        }),
        cache_bound,
    }
}

fn with_grid_cell<Cell: 'static, R>(
    item: &gtk::ListItem,
    apply: impl FnOnce(&Cell) -> R,
) -> Option<R> {
    item.child()?
        .downcast::<cards::CollectionGridCardInset>()
        .ok()?
        .with_cell(apply)
}

fn selected_position(selection: &gtk::SelectionModel) -> u32 {
    let positions = selection.selection();
    if positions.is_empty() {
        gtk::INVALID_LIST_POSITION
    } else {
        positions.minimum()
    }
}

fn album_playback_target(album_id: String, context: Option<&str>) -> PlaybackTarget {
    let target = PlaybackTarget::Album(album_id);
    context
        .map(|context| target.clone().in_context(context))
        .unwrap_or(target)
}

fn play_one_media_uri(shell: &Shell, media_uri: String, placement: QueuePlacement) {
    let database = Arc::clone(&shell.products.library);
    let queue = shell.products.playback.queue.clone();
    shell.products.runtime.spawn(async move {
        let Ok(mut media) = database
            .queue_items_for_uris(&[media_uri], &library::ReadCancellation::new())
            .await
        else {
            return;
        };
        let Some(media) = media.pop() else {
            return;
        };
        queue.play(playback::PlayRequest::one(media, placement));
    });
}

pub(super) struct TrackGridCell<T: super::library_fields::TrackPresentation = library::TrackRow> {
    body: CollectionGridCardCell,
    shell: std::rc::Weak<Shell>,
    cover: ArtworkTile,
    favorite: gtk::Button,
    current: Rc<RefCell<Option<T>>>,
    position: Rc<Cell<u32>>,
    field: Rc<dyn Fn(u32, &T, LibraryField) -> DetailLinks>,
}

impl<T: super::library_fields::TrackPresentation> TrackGridCell<T> {
    pub(super) fn new(
        shell: &Rc<Shell>,
        fields: &[LibraryField],
        play: Rc<dyn Fn(u32)>,
        field: Rc<dyn Fn(u32, &T, LibraryField) -> DetailLinks>,
        selection: Option<TrackSelection>,
    ) -> Self {
        let current = Rc::new(RefCell::new(None::<T>));
        let position = Rc::new(Cell::new(0));
        let controls = cards::CollectionGridCoverView::new(msgid("Play track"));
        let cover_host = controls.imp().cover_host.get();
        let menu = controls.imp().menu.get();
        let play_now_button = controls.imp().play.get();
        let play_next = controls.imp().play_next.get();
        let play_last = controls.imp().play_last.get();
        let favorite = controls.favorite();
        let body = CollectionGridCardCell::new(shell, fields, controls.clone().upcast());
        let cover = ArtworkTile::new_elastic_square();
        cover_host.append(&cover.widget());
        install_grid_context(
            shell,
            &current,
            &menu,
            &controls.imp().overlay,
            &body.card,
            |target, shell, row, point| {
                super::collection_context::present_track_context_menu(
                    target,
                    shell,
                    row.media_uri().to_string(),
                    point,
                );
            },
        );
        let play_now = Rc::clone(&play);
        let play_position = Rc::clone(&position);
        play_now_button.connect_clicked(move |_| play_now(play_position.get()));
        for (button, placement) in [
            (&play_next, QueuePlacement::Next),
            (&play_last, QueuePlacement::Last),
        ] {
            let action_shell = Rc::downgrade(shell);
            let action_current = Rc::clone(&current);
            button.connect_clicked(move |_| {
                if let (Some(shell), Some(row)) = (
                    action_shell.upgrade(),
                    action_current.borrow().as_ref().cloned(),
                ) {
                    play_one_media_uri(&shell, row.media_uri().to_string(), placement);
                }
            });
        }
        install_grid_favorite(
            shell,
            &current,
            &favorite,
            |row| FavoriteTarget::Track(row.media_uri().to_string()),
            |row| track_favorite_key(row.media_uri()),
        );
        let drag_current = Rc::clone(&current);
        let drag_shell = Rc::downgrade(shell);
        let drag_artwork = cover.drag_paintable_source();
        install_compact_media_drag_source(&body.card, &drag_artwork, move || {
            let current = drag_current.borrow();
            let track = current.as_ref()?;
            let source = match selection.as_ref() {
                Some(selection) => {
                    MediaDragSource::selection(selection.dragged_tracks_for(track.media_uri()))
                }
                None => MediaDragSource::capture_target(
                    drag_shell.upgrade()?.as_ref(),
                    PlaybackTarget::Track(track.media_uri().to_string()),
                ),
            };
            Some((source, track.title().to_string()))
        });
        body.set_download_badge(shell.download_badge(false));
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

impl<T: super::library_fields::TrackPresentation> ReusableCollectionGridCell<T>
    for TrackGridCell<T>
{
    fn widget(&self) -> gtk::Widget {
        self.body.widget()
    }
    fn bind(&self, position: u32, row: T) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        shell.bind_artwork_tile(
            &self.cover,
            opaque_artwork(row.artwork()),
            COLLECTION_GRID_MAX_CARD_WIDTH,
            LARGE_COVER_SIZE,
        );
        self.body
            .bind(row.title(), |value| (self.field)(position, &row, value));
        self.body.set_download_target(&shell, row.downloaded());
        set_grid_favorite(&self.favorite, Some(row.favorite()));
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
                .bind(row.title(), |value| (self.field)(position, row, value));
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
    pub(super) fn enable_card_activation(&self) {
        let click = gtk::GestureClick::new();
        click.set_button(1);
        let shell = self.shell.clone();
        let current = Rc::clone(&self.current);
        click.connect_pressed(move |gesture, presses, _, _| {
            let uri = current
                .borrow()
                .as_ref()
                .map(|album| album.media_uri.clone());
            if presses == 2
                && let Some(shell) = shell.upgrade()
                && let Some(uri) = uri
            {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                shell.navigate(crate::routes::route::Route::AlbumDetail(uri));
            }
        });
        self.body.card.add_controller(click);
    }

    pub(super) fn new(shell: &Rc<Shell>, fields: &[LibraryField], context: Option<String>) -> Self {
        let current = Rc::new(RefCell::new(None::<AlbumRow>));
        let controls = cards::CollectionGridCoverView::new(msgid("Play album"));
        let cover_host = controls.imp().cover_host.get();
        let menu = controls.imp().menu.get();
        let play = controls.imp().play.get();
        let play_next = controls.imp().play_next.get();
        let play_last = controls.imp().play_last.get();
        let favorite = controls.favorite();
        let body = CollectionGridCardCell::new(shell, fields, controls.clone().upcast());
        let cover = ArtworkTile::new_elastic_square();
        cover_host.append(&cover.widget());
        let drag_current = Rc::clone(&current);
        let drag_shell = Rc::downgrade(shell);
        let drag_context = context.clone();
        let drag_artwork = cover.drag_paintable_source();
        install_compact_media_drag_source(&body.card, &drag_artwork, move || {
            let shell = drag_shell.upgrade()?;
            let row = drag_current.borrow();
            let row = row.as_ref()?;
            let target = album_playback_target(row.media_uri.clone(), drag_context.as_deref());
            let source = MediaDragSource::capture_target(&shell, target);
            Some((source, row.title.clone()))
        });
        let menu_context = context.clone();
        install_grid_context(
            shell,
            &current,
            &menu,
            &controls.imp().overlay,
            &body.card,
            move |target, shell, row, point| {
                present_album_context_menu(target, shell, row, menu_context.clone(), None, point)
            },
        );
        install_grid_play(
            shell,
            &current,
            [play, play_next, play_last],
            move |shell, row, placement| {
                album_playback_target(row.media_uri.clone(), context.as_deref())
                    .play(shell, placement);
            },
        );
        install_grid_favorite(
            shell,
            &current,
            &favorite,
            |row| FavoriteTarget::Album(row.media_uri.clone()),
            |row| album_favorite_key(&row.media_uri),
        );
        body.set_download_badge(shell.download_badge(true));
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
        let controls = cards::CollectionGridCoverView::new(msgid("Play artist"));
        let cover_host = controls.imp().cover_host.get();
        let menu = controls.imp().menu.get();
        let play = controls.imp().play.get();
        let play_next = controls.imp().play_next.get();
        let play_last = controls.imp().play_last.get();
        let favorite = controls.favorite();
        let body = CollectionGridCardCell::new(shell, fields, controls.clone().upcast());
        let cover = ArtworkTile::new_elastic_square();
        cover_host.append(&cover.widget());
        let drag_current = Rc::clone(&current);
        let drag_shell = Rc::downgrade(shell);
        let drag_artwork = cover.drag_paintable_source();
        install_compact_media_drag_source(&body.card, &drag_artwork, move || {
            let shell = drag_shell.upgrade()?;
            let row = drag_current.borrow();
            let row = row.as_ref()?;
            let target = if album_artist {
                PlaybackTarget::AlbumArtist(row.media_uri.clone())
            } else {
                PlaybackTarget::Artist(row.media_uri.clone())
            };
            let source = MediaDragSource::capture_target(&shell, target);
            Some((source, row.name.clone()))
        });
        install_grid_context(
            shell,
            &current,
            &menu,
            &controls.imp().overlay,
            &body.card,
            move |target, shell, row, point| {
                present_artist_context_menu(target, shell, row, album_artist, None, point)
            },
        );
        install_grid_play(
            shell,
            &current,
            [play, play_next, play_last],
            move |shell, row, placement| {
                let target = if album_artist {
                    PlaybackTarget::AlbumArtist(row.media_uri.clone())
                } else {
                    PlaybackTarget::Artist(row.media_uri.clone())
                };
                target.play(shell, placement);
            },
        );
        install_grid_favorite(
            shell,
            &current,
            &favorite,
            |row| FavoriteTarget::Artist(row.media_uri.clone()),
            |row| artist_favorite_key(&row.media_uri),
        );
        body.set_download_badge(shell.download_badge(true));
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
    pub(super) card: CollectionGridCardView,
    title: gtk::Label,
    title_row: gtk::Box,
    downloaded: RefCell<Option<gtk::Image>>,
    fields: RefCell<Vec<CollectionGridFieldCell>>,
    ready: Cell<bool>,
}

impl CollectionGridCardCell {
    pub(super) fn new(shell: &Rc<Shell>, fields: &[LibraryField], cover: gtk::Widget) -> Self {
        let card = CollectionGridCardView::new();
        card.prepend(&cover);
        let title = card.imp().title.get();
        connect_label_tooltip(&title);
        let title_row = card.imp().title_row.get();
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
        for field in self.fields.borrow().iter() {
            field.bind(field_value(field.field));
        }
    }

    pub(super) fn clear(&self, shell: &Rc<Shell>) {
        self.title.set_text("");
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

struct CollectionGridFieldCell {
    field: LibraryField,
    widget: gtk::Widget,
    links: DetailLinkBinding,
}

fn connect_label_tooltip(label: &gtk::Label) {
    label.set_has_tooltip(true);
    label.connect_query_tooltip(|label, _, _, _, tooltip| {
        let text = label.text();
        if text.is_empty() {
            return false;
        }
        tooltip.set_text(Some(&text));
        true
    });
}

impl CollectionGridFieldCell {
    fn new(shell: &Rc<Shell>, field: LibraryField) -> Self {
        let (widget, label) = collection_grid_field_label("", field);
        connect_label_tooltip(&label);
        let links = DetailLinkBinding::new(&label, shell);
        Self {
            field,
            widget,
            links,
        }
    }

    fn bind(&self, links: DetailLinks) {
        self.links.bind(links);
    }

    fn clear(&self) {
        self.links.clear();
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
