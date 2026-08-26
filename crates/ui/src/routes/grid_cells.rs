use super::collection_context::{
    install_dynamic_album_context_menu, install_dynamic_track_context_menu,
};
use super::collection_context::{
    present_album_context_menu, present_artist_context_menu, present_playlist_context_menu,
    present_smart_playlist_context_menu, present_track_context_menu,
};
use crate::LibraryField;
use crate::favorites::{
    album_favorite_key, artist_favorite_key, favorite_button_is_active, set_favorite_button_active,
    track_favorite_key,
};
use crate::interactions::install_context_menu_openers;
use crate::shell::Shell;
use crate::shell::cover::{ArtworkTile, LARGE_COVER_SIZE, THUMB_COVER_SIZE};
use crate::shell::route::{MountedRouteItemNavigation, item_navigation_entry_position};
use ::library::{
    AlbumKey, AlbumRow, ArtistRow, PlaylistRow, SmartPlaylistKey, SmartPlaylistRow, TrackRow,
};
use adw::prelude::*;
use artwork::ArtworkBinding;
use gtk::{gio, glib};
use localization::msgid;
use playback::QueuePlacement;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use crate::preferences::dialogs::SmartPlaylistChange;

use super::cards;
use super::collections::{PlaybackTarget, collection_grid_card, collection_grid_field_label};
use super::detail_links::{DetailLinkBinding, DetailLinks, album_artist_links};
use super::library_fields::{
    COLLECTION_GRID_CARD_MARGIN, COLLECTION_GRID_MAX_CARD_WIDTH, COLLECTION_GRID_MIN_CARD_WIDTH,
    album_field, artist_field, grid_title_with_label, item_at, item_at_from_item, opaque_artwork,
    playlist_field, smart_playlist_display_name, smart_playlist_field,
};
use super::route::Route;
use super::route_shell::restore_single_click_activation_on_primary_press;

fn collection_is_downloaded(track_count: i64, downloaded_count: i64) -> bool {
    track_count > 0 && downloaded_count == track_count
}

pub(super) trait ReusableCollectionGridCell<T>: 'static {
    fn widget(&self) -> gtk::Widget;
    fn activatable(&self, _: &T) -> bool {
        true
    }
    fn bind(&self, position: u32, value: T);
    fn clear(&self);
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
        let child =
            cards::collection_grid_card_inset(&cell.widget(), COLLECTION_GRID_MIN_CARD_WIDTH);
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
        let Some(value) = item_at_from_item::<T>(item) else {
            return;
        };
        if let Some(cell) = bind_cells.borrow().get(&(item.as_ptr() as usize)) {
            let activatable = cell.activatable(&value);
            item.set_activatable(activatable);
            item.set_selectable(activatable);
            cell.bind(item.position(), value);
        }
    });
    let unbind_cells = Rc::clone(&cells);
    factory.connect_unbind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        item.set_activatable(false);
        item.set_selectable(false);
        if let Some(cell) = unbind_cells.borrow().get(&(item.as_ptr() as usize)) {
            cell.clear();
        }
    });
    let teardown_cells = Rc::clone(&cells);
    factory.connect_teardown(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        teardown_cells
            .borrow_mut()
            .remove(&(item.as_ptr() as usize));
        item.set_child(None::<&gtk::Widget>);
    });

    let grid = gtk::GridView::new(Some(selection.clone()), Some(factory));
    grid.add_css_class("album-grid");
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

fn play_loaded_tracks(
    shell: &Shell,
    target: PlaybackTarget,
    placement: QueuePlacement,
    shuffled_start: bool,
) {
    target.play(shell, placement, shuffled_start);
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
    shell: Rc<Shell>,
    cover_tile: ArtworkTile,
    favorite: gtk::Button,
    current_track: Rc<RefCell<Option<TrackRow>>>,
    current_position: Rc<Cell<u32>>,
    field_value: Rc<dyn Fn(u32, &TrackRow, LibraryField) -> DetailLinks>,
}

impl TrackGridCell {
    pub(super) fn new_with_field_value(
        shell: Rc<Shell>,
        fields: &[LibraryField],
        play_from_collection: Rc<dyn Fn(u32)>,
        field_value: Rc<dyn Fn(u32, &TrackRow, LibraryField) -> DetailLinks>,
    ) -> Self {
        let current_track = Rc::new(RefCell::new(None::<TrackRow>));
        let current_position = Rc::new(Cell::new(0));

        let overlay = cards::elastic_cover_overlay();
        let (cover_button, cover_tile) = collection_grid_cover_button();
        let button_position = Rc::clone(&current_position);
        let button_play = Rc::clone(&play_from_collection);
        cover_button.connect_clicked(move |_| {
            button_play(button_position.get());
        });
        overlay.set_child(Some(&cover_button));

        let (mut controls, favorite) =
            cards::cover_hover_controls_with_favorite(0, msgid("Play track"), false);
        let menu = controls.add_context_button();
        let menu_shell = Rc::clone(&shell);
        let menu_track = Rc::clone(&current_track);
        let menu_target = overlay.downgrade();
        menu.connect_clicked(move |_| {
            let Some(track) = menu_track.borrow().as_ref().cloned() else {
                return;
            };
            let Some(menu_target) = menu_target.upgrade() else {
                return;
            };
            present_track_context_menu(
                menu_target.upcast_ref(),
                &menu_shell,
                track,
                cards::elastic_cover_context_point(&menu_target),
            );
        });

        let play_position = Rc::clone(&current_position);
        let play_action = Rc::clone(&play_from_collection);
        controls.play.connect_clicked(move |_| {
            play_action(play_position.get());
        });

        let next_shell = Rc::clone(&shell);
        let next_track = Rc::clone(&current_track);
        controls.play_next.connect_clicked(move |_| {
            if let Some(track) = next_track.borrow().as_ref().cloned() {
                play_one_track(&next_shell, track, QueuePlacement::Next);
            }
        });

        let last_shell = Rc::clone(&shell);
        let last_track = Rc::clone(&current_track);
        controls.play_last.connect_clicked(move |_| {
            if let Some(track) = last_track.borrow().as_ref().cloned() {
                play_one_track(&last_shell, track, QueuePlacement::Last);
            }
        });

        let favorite_key_track = Rc::clone(&current_track);
        shell.register_dynamic_favorite_button(
            Rc::new(move || {
                favorite_key_track
                    .borrow()
                    .as_ref()
                    .map(|track| track_favorite_key(&track.track_key))
            }),
            &favorite,
        );
        let favorite_shell = Rc::clone(&shell);
        let favorite_track = Rc::clone(&current_track);
        favorite.connect_clicked(move |button| {
            let Some(track) = favorite_track.borrow().as_ref().cloned() else {
                return;
            };
            let favorite = !favorite_button_is_active(button);
            favorite_shell.set_favorite_with_feedback(
                library::FavoriteTarget::Track(track.track_key),
                favorite,
                Some(button),
            );
        });
        controls.add_to_overlay(&overlay);
        controls.connect_hover(&overlay);

        let cover = cards::square_cover_frame(&overlay, &controls.transport);
        let body = CollectionGridCardCell::new(&shell, fields, cover.upcast());
        body.set_download_badge(shell.download_badge(false));
        install_dynamic_track_context_menu(&body.card, &shell, Rc::clone(&current_track));
        body.widget().add_css_class("collection-grid-text-skeleton");
        cover_tile
            .widget()
            .add_css_class("collection-grid-cover-skeleton");

        Self {
            body,
            shell,
            cover_tile,
            favorite,
            current_track,
            current_position,
            field_value,
        }
    }
}

impl ReusableCollectionGridCell<TrackRow> for TrackGridCell {
    fn widget(&self) -> gtk::Widget {
        self.body.widget()
    }

    fn bind(&self, position: u32, track: TrackRow) {
        self.body
            .widget()
            .remove_css_class("collection-grid-text-skeleton");
        self.cover_tile
            .widget()
            .remove_css_class("collection-grid-cover-skeleton");
        let artwork = track
            .artwork_binding
            .as_deref()
            .map(ArtworkBinding::opaque)
            .unwrap_or_default();
        self.shell.bind_artwork_tile(
            &self.cover_tile,
            artwork,
            COLLECTION_GRID_MAX_CARD_WIDTH,
            LARGE_COVER_SIZE,
        );
        self.body.bind(&track.title, |field| {
            (self.field_value)(position, &track, field)
        });
        self.body
            .set_download_target(&self.shell, track.is_downloaded);
        set_favorite_button_active(&self.favorite, track.favorite);
        *self.current_track.borrow_mut() = Some(track);
        self.current_position.set(position);
    }

    fn clear(&self) {
        self.shell.clear_artwork_tile(&self.cover_tile);
        self.body.clear(&self.shell);
        self.body
            .widget()
            .add_css_class("collection-grid-text-skeleton");
        self.cover_tile
            .widget()
            .add_css_class("collection-grid-cover-skeleton");
        *self.current_track.borrow_mut() = None;
    }

    fn apply_fields(&self, fields: &[LibraryField]) {
        self.body.replace_fields(&self.shell, fields);
        if let Some(track) = self.current_track.borrow().as_ref().cloned() {
            let position = self.current_position.get();
            self.body.bind(&track.title, |field| {
                (self.field_value)(position, &track, field)
            });
        }
    }
}

pub(super) struct AlbumGridCell {
    body: CollectionGridCardCell,
    shell: Rc<Shell>,
    cover_tile: ArtworkTile,
    favorite: gtk::Button,
    current_album: Rc<RefCell<Option<AlbumRow>>>,
}

impl AlbumGridCell {
    pub(super) fn new(
        shell: Rc<Shell>,
        fields: &[LibraryField],
        playback_context: Option<String>,
    ) -> Self {
        let current_album = Rc::new(RefCell::new(None::<AlbumRow>));

        let overlay = cards::elastic_cover_overlay();
        let (album_button, cover_tile) = collection_grid_cover_button();
        let open_shell = Rc::clone(&shell);
        let open_album = Rc::clone(&current_album);
        album_button.connect_clicked(move |_| {
            let Some(album) = open_album.borrow().as_ref().cloned() else {
                return;
            };
            open_shell.navigate(Route::AlbumDetail(album.album_key.clone()));
        });
        overlay.set_child(Some(&album_button));

        let (mut controls, favorite) =
            cards::cover_hover_controls_with_favorite(0, msgid("Play album"), false);
        let menu = controls.add_context_button();
        let menu_shell = Rc::clone(&shell);
        let menu_album = Rc::clone(&current_album);
        let menu_target = overlay.downgrade();
        let menu_playback_context = playback_context.clone();
        menu.connect_clicked(move |_| {
            let Some(album) = menu_album.borrow().as_ref().cloned() else {
                return;
            };
            let Some(menu_target) = menu_target.upgrade() else {
                return;
            };
            present_album_context_menu(
                menu_target.upcast_ref(),
                &menu_shell,
                album,
                menu_playback_context.clone(),
                None,
                cards::elastic_cover_context_point(&menu_target),
            );
        });

        let play_shell = Rc::clone(&shell);
        let play_album = Rc::clone(&current_album);
        let play_context = playback_context.clone();
        controls.play.connect_clicked(move |_| {
            let Some(album) = play_album.borrow().as_ref().cloned() else {
                return;
            };
            play_loaded_tracks(
                &play_shell,
                album_playback_target(album.album_key.clone(), play_context.as_deref()),
                QueuePlacement::Now,
                true,
            );
        });

        let next_shell = Rc::clone(&shell);
        let next_album = Rc::clone(&current_album);
        let next_context = playback_context.clone();
        controls.play_next.connect_clicked(move |_| {
            let Some(album) = next_album.borrow().as_ref().cloned() else {
                return;
            };
            play_loaded_tracks(
                &next_shell,
                album_playback_target(album.album_key.clone(), next_context.as_deref()),
                QueuePlacement::Next,
                false,
            );
        });

        let last_shell = Rc::clone(&shell);
        let last_album = Rc::clone(&current_album);
        let last_context = playback_context.clone();
        controls.play_last.connect_clicked(move |_| {
            let Some(album) = last_album.borrow().as_ref().cloned() else {
                return;
            };
            play_loaded_tracks(
                &last_shell,
                album_playback_target(album.album_key.clone(), last_context.as_deref()),
                QueuePlacement::Last,
                false,
            );
        });

        let favorite_key_album = Rc::clone(&current_album);
        shell.register_dynamic_favorite_button(
            Rc::new(move || {
                favorite_key_album
                    .borrow()
                    .as_ref()
                    .map(|album| album_favorite_key(&album.album_key))
            }),
            &favorite,
        );
        let favorite_shell = Rc::clone(&shell);
        let favorite_album = Rc::clone(&current_album);
        favorite.connect_clicked(move |button| {
            let Some(album) = favorite_album.borrow().as_ref().cloned() else {
                return;
            };
            let favorite = !favorite_button_is_active(button);
            favorite_shell.set_favorite_with_feedback(
                library::FavoriteTarget::Album(album.album_key.clone()),
                favorite,
                Some(button),
            );
        });
        controls.add_to_overlay(&overlay);
        controls.connect_hover(&overlay);

        let cover = cards::square_cover_frame(&overlay, &controls.transport);
        let body = CollectionGridCardCell::new(&shell, fields, cover.upcast());
        body.set_download_badge(shell.download_badge(true));
        install_dynamic_album_context_menu(
            &body.card,
            &shell,
            Rc::clone(&current_album),
            playback_context,
        );

        Self {
            body,
            shell,
            cover_tile,
            favorite,
            current_album,
        }
    }
}

impl ReusableCollectionGridCell<AlbumRow> for AlbumGridCell {
    fn widget(&self) -> gtk::Widget {
        self.body.widget()
    }

    fn bind(&self, _: u32, album: AlbumRow) {
        self.shell.bind_artwork_tile(
            &self.cover_tile,
            opaque_artwork(album.artwork_binding.as_deref()),
            COLLECTION_GRID_MAX_CARD_WIDTH,
            LARGE_COVER_SIZE,
        );
        self.body.bind(&album.title, |field| {
            let value = album_field(&album, field);
            if value.is_empty()
                || !matches!(field, LibraryField::Artist | LibraryField::AlbumArtist)
            {
                DetailLinks::text(&value)
            } else {
                album_artist_links(&album)
            }
        });
        self.body.set_download_target(
            &self.shell,
            collection_is_downloaded(album.track_count, album.downloaded_count),
        );
        set_favorite_button_active(&self.favorite, album.favorite);
        *self.current_album.borrow_mut() = Some(album);
    }

    fn clear(&self) {
        self.shell.clear_artwork_tile(&self.cover_tile);
        self.body.clear(&self.shell);
        *self.current_album.borrow_mut() = None;
    }

    fn apply_fields(&self, fields: &[LibraryField]) {
        self.body.replace_fields(&self.shell, fields);
        if let Some(album) = self.current_album.borrow().as_ref().cloned() {
            self.body.bind(&album.title, |field| {
                let value = album_field(&album, field);
                if value.is_empty()
                    || !matches!(field, LibraryField::Artist | LibraryField::AlbumArtist)
                {
                    DetailLinks::text(&value)
                } else {
                    album_artist_links(&album)
                }
            });
        }
    }
}

pub(super) struct ArtistGridCell {
    body: CollectionGridCardCell,
    shell: Rc<Shell>,
    cover_tile: ArtworkTile,
    favorite: gtk::Button,
    current_artist: Rc<RefCell<Option<ArtistRow>>>,
}

impl ArtistGridCell {
    pub(super) fn new(shell: Rc<Shell>, fields: &[LibraryField], album_artist: bool) -> Self {
        let current_artist = Rc::new(RefCell::new(None::<ArtistRow>));

        let overlay = cards::elastic_cover_overlay();
        let (artist_button, cover_tile) = collection_grid_cover_button();
        let open_shell = Rc::clone(&shell);
        let open_artist = Rc::clone(&current_artist);
        artist_button.connect_clicked(move |_| {
            let Some(artist) = open_artist.borrow().as_ref().cloned() else {
                return;
            };
            open_shell.navigate(if album_artist {
                Route::AlbumArtistDetail(artist.artist_key)
            } else {
                Route::ArtistDetail(artist.artist_key)
            });
        });
        overlay.set_child(Some(&artist_button));

        let (mut controls, favorite) =
            cards::cover_hover_controls_with_favorite(0, msgid("Play artist"), false);
        let menu = controls.add_context_button();
        let menu_shell = Rc::clone(&shell);
        let menu_artist = Rc::clone(&current_artist);
        let menu_target = overlay.downgrade();
        menu.connect_clicked(move |_| {
            let Some(artist) = menu_artist.borrow().as_ref().cloned() else {
                return;
            };
            let Some(menu_target) = menu_target.upgrade() else {
                return;
            };
            present_artist_context_menu(
                menu_target.upcast_ref(),
                &menu_shell,
                artist,
                album_artist,
                None,
                cards::elastic_cover_context_point(&menu_target),
            );
        });

        let play_shell = Rc::clone(&shell);
        let play_artist = Rc::clone(&current_artist);
        controls.play.connect_clicked(move |_| {
            let Some(artist) = play_artist.borrow().as_ref().cloned() else {
                return;
            };
            play_loaded_tracks(
                &play_shell,
                if album_artist {
                    PlaybackTarget::AlbumArtist(artist.artist_key)
                } else {
                    PlaybackTarget::Artist(artist.artist_key)
                },
                QueuePlacement::Now,
                true,
            );
        });

        let next_shell = Rc::clone(&shell);
        let next_artist = Rc::clone(&current_artist);
        controls.play_next.connect_clicked(move |_| {
            let Some(artist) = next_artist.borrow().as_ref().cloned() else {
                return;
            };
            play_loaded_tracks(
                &next_shell,
                if album_artist {
                    PlaybackTarget::AlbumArtist(artist.artist_key)
                } else {
                    PlaybackTarget::Artist(artist.artist_key)
                },
                QueuePlacement::Next,
                false,
            );
        });

        let last_shell = Rc::clone(&shell);
        let last_artist = Rc::clone(&current_artist);
        controls.play_last.connect_clicked(move |_| {
            let Some(artist) = last_artist.borrow().as_ref().cloned() else {
                return;
            };
            play_loaded_tracks(
                &last_shell,
                if album_artist {
                    PlaybackTarget::AlbumArtist(artist.artist_key)
                } else {
                    PlaybackTarget::Artist(artist.artist_key)
                },
                QueuePlacement::Last,
                false,
            );
        });

        let favorite_key_artist = Rc::clone(&current_artist);
        shell.register_dynamic_favorite_button(
            Rc::new(move || {
                favorite_key_artist
                    .borrow()
                    .as_ref()
                    .map(|artist| artist_favorite_key(&artist.artist_key))
            }),
            &favorite,
        );
        let favorite_shell = Rc::clone(&shell);
        let favorite_artist = Rc::clone(&current_artist);
        favorite.connect_clicked(move |button| {
            let Some(artist) = favorite_artist.borrow().as_ref().cloned() else {
                return;
            };
            let favorite = !favorite_button_is_active(button);
            favorite_shell.set_favorite_with_feedback(
                library::FavoriteTarget::Artist(artist.artist_key.clone()),
                favorite,
                Some(button),
            );
        });
        controls.add_to_overlay(&overlay);
        controls.connect_hover(&overlay);

        let cover = cards::square_cover_frame(&overlay, &controls.transport);
        let body = CollectionGridCardCell::new(&shell, fields, cover.upcast());
        body.set_download_badge(shell.download_badge(true));
        install_dynamic_artist_context_menu(
            &body.card,
            &shell,
            Rc::clone(&current_artist),
            album_artist,
        );

        Self {
            body,
            shell,
            cover_tile,
            favorite,
            current_artist,
        }
    }
}

impl ReusableCollectionGridCell<ArtistRow> for ArtistGridCell {
    fn widget(&self) -> gtk::Widget {
        self.body.widget()
    }

    fn bind(&self, _: u32, artist: ArtistRow) {
        self.shell.bind_artwork_tile(
            &self.cover_tile,
            opaque_artwork(artist.artwork_binding.as_deref()),
            COLLECTION_GRID_MAX_CARD_WIDTH,
            LARGE_COVER_SIZE,
        );
        self.body.bind(&artist.name, |field| {
            DetailLinks::text(&artist_field(&artist, field))
        });
        self.body.set_download_target(
            &self.shell,
            collection_is_downloaded(artist.track_count, artist.downloaded_count),
        );
        set_favorite_button_active(&self.favorite, artist.favorite);
        *self.current_artist.borrow_mut() = Some(artist);
    }

    fn clear(&self) {
        self.shell.clear_artwork_tile(&self.cover_tile);
        self.body.clear(&self.shell);
        *self.current_artist.borrow_mut() = None;
    }

    fn apply_fields(&self, fields: &[LibraryField]) {
        self.body.replace_fields(&self.shell, fields);
        if let Some(artist) = self.current_artist.borrow().as_ref().cloned() {
            self.body.bind(&artist.name, |field| {
                DetailLinks::text(&artist_field(&artist, field))
            });
        }
    }
}

pub(super) trait PlaylistCardRow: Clone + 'static {
    fn name(&self) -> String;
    fn route(&self) -> Route;
    fn target(&self) -> PlaybackTarget;
    fn artwork(&self) -> Vec<ArtworkBinding>;
    fn field(&self, field: LibraryField) -> String;
    fn downloaded(&self) -> bool;
    fn present_context(
        &self,
        target: &gtk::Widget,
        shell: &Rc<Shell>,
        position: Option<(f64, f64)>,
    );
    fn install_extra(_widget: &gtk::Widget, _shell: &Rc<Shell>, _current: Rc<RefCell<Option<Self>>>)
    where
        Self: Sized,
    {
    }
}

impl PlaylistCardRow for PlaylistRow {
    fn name(&self) -> String {
        self.name.clone()
    }
    fn route(&self) -> Route {
        Route::PlaylistDetail(self.playlist_key)
    }
    fn target(&self) -> PlaybackTarget {
        PlaybackTarget::Playlist(self.playlist_key)
    }
    fn artwork(&self) -> Vec<ArtworkBinding> {
        self.artwork_binding
            .iter()
            .chain(&self.representative_artwork)
            .map(|binding| ArtworkBinding::opaque(binding))
            .collect()
    }
    fn field(&self, field: LibraryField) -> String {
        playlist_field(self, field)
    }
    fn downloaded(&self) -> bool {
        collection_is_downloaded(self.track_count, self.downloaded_count)
    }
    fn present_context(
        &self,
        target: &gtk::Widget,
        shell: &Rc<Shell>,
        position: Option<(f64, f64)>,
    ) {
        present_playlist_context_menu(target, shell, self.clone(), None, position);
    }
}

impl PlaylistCardRow for SmartPlaylistRow {
    fn name(&self) -> String {
        smart_playlist_display_name(self)
    }
    fn route(&self) -> Route {
        Route::SmartPlaylistDetail(self.smart_playlist_key)
    }
    fn target(&self) -> PlaybackTarget {
        PlaybackTarget::SmartPlaylist(self.smart_playlist_key)
    }
    fn artwork(&self) -> Vec<ArtworkBinding> {
        self.artwork_bindings
            .iter()
            .map(|binding| ArtworkBinding::opaque(binding))
            .collect()
    }
    fn field(&self, field: LibraryField) -> String {
        smart_playlist_field(self, field)
    }
    fn downloaded(&self) -> bool {
        collection_is_downloaded(self.track_count, self.downloaded_count)
    }
    fn present_context(
        &self,
        target: &gtk::Widget,
        shell: &Rc<Shell>,
        position: Option<(f64, f64)>,
    ) {
        present_smart_playlist_context_menu(target, shell, self.clone(), None, position);
    }
    fn install_extra(widget: &gtk::Widget, shell: &Rc<Shell>, current: Rc<RefCell<Option<Self>>>) {
        install_dynamic_smart_playlist_drop_target(widget, shell, Rc::clone(&current));
    }
}

pub(super) struct PlaylistVisualGridCell<T: PlaylistCardRow> {
    body: CollectionGridCardCell,
    shell: Rc<Shell>,
    cover_button: gtk::Button,
    current: Rc<RefCell<Option<T>>>,
}

pub(super) type PlaylistGridCell = PlaylistVisualGridCell<PlaylistRow>;
pub(super) type SmartPlaylistGridCell = PlaylistVisualGridCell<SmartPlaylistRow>;

impl<T: PlaylistCardRow> PlaylistVisualGridCell<T> {
    pub(super) fn new(shell: Rc<Shell>, fields: &[LibraryField]) -> Self {
        let current = Rc::new(RefCell::new(None::<T>));
        let overlay = cards::elastic_cover_overlay();
        let cover_button = collection_grid_cover_shell();
        let open_shell = Rc::clone(&shell);
        let open_item = Rc::clone(&current);
        cover_button.connect_clicked(move |_| {
            if let Some(item) = open_item.borrow().as_ref() {
                open_shell.navigate(item.route());
            }
        });
        overlay.set_child(Some(&cover_button));

        let mut controls = cards::cover_play_hover_controls(0, msgid("Play playlist"));
        let menu = controls.add_context_button();
        let menu_shell = Rc::clone(&shell);
        let menu_item = Rc::clone(&current);
        let menu_target = overlay.downgrade();
        menu.connect_clicked(move |_| {
            let Some(item) = menu_item.borrow().as_ref().cloned() else {
                return;
            };
            let Some(target) = menu_target.upgrade() else {
                return;
            };
            item.present_context(
                target.upcast_ref(),
                &menu_shell,
                cards::elastic_cover_context_point(&target),
            );
        });
        for (button, placement, shuffled) in [
            (&controls.play, QueuePlacement::Now, true),
            (&controls.play_next, QueuePlacement::Next, false),
            (&controls.play_last, QueuePlacement::Last, false),
        ] {
            let play_shell = Rc::clone(&shell);
            let play_item = Rc::clone(&current);
            button.connect_clicked(move |_| {
                if let Some(item) = play_item.borrow().as_ref() {
                    play_loaded_tracks(&play_shell, item.target(), placement, shuffled);
                }
            });
        }
        controls.add_to_overlay(&overlay);
        controls.connect_hover(&overlay);
        let cover = cards::square_cover_frame(&overlay, &controls.transport);
        let body = CollectionGridCardCell::new(&shell, fields, cover.upcast());
        body.set_download_badge(shell.download_badge(true));
        let widget = body.widget();
        let context_shell = Rc::clone(&shell);
        let context_item = Rc::clone(&current);
        install_context_menu_openers(
            &widget,
            Rc::new(move |target, position| {
                if let Some(item) = context_item.borrow().as_ref() {
                    item.present_context(target, &context_shell, position);
                }
            }),
        );
        T::install_extra(&widget, &shell, Rc::clone(&current));
        Self {
            body,
            shell,
            cover_button,
            current,
        }
    }
}

impl<T: PlaylistCardRow> ReusableCollectionGridCell<T> for PlaylistVisualGridCell<T> {
    fn widget(&self) -> gtk::Widget {
        self.body.widget()
    }
    fn bind(&self, _: u32, item: T) {
        self.cover_button.set_child(Some(
            &self
                .shell
                .elastic_cover_group_tile_for_artwork(&item.artwork(), THUMB_COVER_SIZE),
        ));
        self.body
            .bind(&item.name(), |field| DetailLinks::text(&item.field(field)));
        self.body
            .set_download_target(&self.shell, item.downloaded());
        self.current.replace(Some(item));
    }
    fn clear(&self) {
        self.cover_button.set_child(None::<&gtk::Widget>);
        self.body.clear(&self.shell);
        self.current.take();
    }
    fn apply_fields(&self, fields: &[LibraryField]) {
        self.body.replace_fields(&self.shell, fields);
        if let Some(item) = self.current.borrow().as_ref() {
            self.body
                .bind(&item.name(), |field| DetailLinks::text(&item.field(field)));
        }
    }
}

fn install_dynamic_artist_context_menu(
    target: &impl IsA<gtk::Widget>,
    shell: &Rc<Shell>,
    artist: Rc<RefCell<Option<ArtistRow>>>,
    album_artist: bool,
) {
    let shell = Rc::clone(shell);
    install_context_menu_openers(
        target,
        Rc::new(move |target, position| {
            let Some(artist) = artist.borrow().clone() else {
                return;
            };
            present_artist_context_menu(target, &shell, artist, album_artist, None, position);
        }),
    );
}

fn install_dynamic_smart_playlist_drop_target(
    target: &impl IsA<gtk::Widget>,
    shell: &Rc<Shell>,
    playlist: Rc<RefCell<Option<SmartPlaylistRow>>>,
) {
    let widget = target.as_ref().downgrade();
    let shell = Rc::clone(shell);
    let drop_target = gtk::DropTarget::new(String::static_type(), gtk::gdk::DragAction::MOVE);
    drop_target.connect_drop(move |_, value, _, y| {
        let Ok(dragged_id) = value.get::<String>() else {
            return false;
        };
        let Some(target_id) = playlist
            .borrow()
            .as_ref()
            .map(|playlist| playlist.smart_playlist_key.clone())
        else {
            return false;
        };
        let Ok(dragged_id) = dragged_id.parse::<i64>() else {
            return false;
        };
        let dragged_id = SmartPlaylistKey::from_raw(dragged_id);
        if dragged_id == target_id {
            return false;
        }
        let Some(widget) = widget.upgrade() else {
            return false;
        };
        let after = y > f64::from(widget.height()) / 2.0;
        shell.publish_smart_playlist_change(
            SmartPlaylistChange::Move {
                dragged: dragged_id,
                target: target_id,
                after,
            },
            None,
        );
        true
    });
    target.add_controller(drop_target);
}

pub(super) struct CollectionGridCardCell {
    pub(super) card: gtk::Box,
    title: gtk::Label,
    title_row: gtk::Box,
    downloaded: RefCell<Option<gtk::Image>>,
    fields: RefCell<Vec<CollectionGridFieldCell>>,
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
    cover_button
}

fn collection_grid_cover_button() -> (gtk::Button, ArtworkTile) {
    let cover_button = collection_grid_cover_shell();
    let cover_tile = ArtworkTile::new_elastic_square();
    cover_button.set_child(Some(&cover_tile.widget()));
    (cover_button, cover_tile)
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
