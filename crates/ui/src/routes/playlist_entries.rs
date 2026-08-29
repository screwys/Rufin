use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use library::{PlaylistEntryKey, PlaylistEntryRow, PlaylistKey};
use localization::msgid;

use crate::favorites::{
    FAVORITE_COLUMN_TITLE, favorite_button_is_active, row_favorite_icon_button,
    set_favorite_button_active, track_favorite_key,
};
use crate::interactions::install_context_menu_openers;
use crate::localization::{bind_search_placeholder, localized_column};
use crate::shell::Shell;
use crate::{LibraryField, LibraryLayout, LibraryListKey, LibraryListSettings};

use super::collection_context::present_playlist_entry_context_menu;
use super::collections::{
    CollectionTableProjection, LibraryCollectionProjection, LibraryPresentationProjection,
    dynamic_collection_table, library_route_inset, track_grid_field_links,
};
use super::columns::{
    TrackRowPlayingIndicator, set_track_row_index_text, track_column_fit_width,
    track_row_index_cell,
};
use super::detail_links::{
    DetailLinkBinding, DetailLinks, track_album_artist_links, track_artist_links,
};
use super::factory_cells::FactoryCells;
use super::grid_cells::{
    CollectionGridCardCell, CollectionGridProjection, ReusableCollectionGridCell,
    collection_grid_with_selection_and_demand,
};
use super::library_fields::{
    COLLECTION_GRID_MAX_CARD_WIDTH, add_field_skeleton_class, item_at_from_item, opaque_artwork,
    track_field,
};
use super::playlist_entry_model::{PlaylistEntryModel, PlaylistEntryProjectionRequest};
use super::playlist_picker::{
    PlaylistDragPreviewBinding, PlaylistTrackSource, playlist_drag_content_provider,
};
use super::route_shell::LibraryToolbarProjection;
use super::sparse_model::connect_sparse_bind;
use super::table_sizing::route_column_view_initial_width_with_inset;
use super::track_selection::PlaylistEntrySelection;

#[derive(Default)]
struct PlaylistEntryOrderLane(std::cell::Cell<u64>);

impl PlaylistEntryOrderLane {
    fn begin(&self) -> u64 {
        let generation = self.0.get().wrapping_add(1);
        self.0.set(generation);
        generation
    }

    fn accepts(&self, generation: u64) -> bool {
        self.0.get() == generation
    }
}

#[derive(Clone)]
pub(crate) struct PlaylistEntriesView {
    widget: gtk::Widget,
    collection: LibraryCollectionProjection,
    toolbar: LibraryToolbarProjection,
    model: PlaylistEntryModel,
    search: gtk::SearchEntry,
    stack: gtk::Stack,
    toolbar_widget: gtk::Widget,
    order_generation: Rc<PlaylistEntryOrderLane>,
}

impl PlaylistEntriesView {
    pub(crate) fn widget(&self) -> gtk::Widget {
        self.widget.clone()
    }

    pub(crate) fn item_navigation(&self) -> crate::shell::route::MountedRouteItemNavigation {
        self.collection.item_navigation()
    }

    pub(crate) fn connect_search_request(
        &self,
        callback: impl Fn(u64, PlaylistEntryProjectionRequest) + 'static,
    ) {
        let model = self.model.clone();
        let generation = Rc::clone(&self.order_generation);
        self.search.connect_search_changed(move |search| {
            if model.set_query(&search.text()) {
                callback(generation.begin(), model.projection_request());
            }
        });
    }

    pub(crate) fn begin_order_request(&self) -> (u64, PlaylistEntryProjectionRequest) {
        (
            self.order_generation.begin(),
            self.model.projection_request(),
        )
    }

    pub(crate) fn replace_order(
        &self,
        generation: u64,
        order: library::PlaylistEntryOrder,
    ) -> bool {
        if !self.order_generation.accepts(generation) {
            return false;
        }
        let empty = order.entries.is_empty();
        self.model.replace_order(order);
        self.toolbar_widget.set_visible(!empty);
        self.stack
            .set_visible_child_name(if empty { "empty" } else { "content" });
        true
    }

    pub(crate) fn apply_library_list_settings(
        &self,
        key: LibraryListKey,
        settings: &LibraryListSettings,
    ) {
        if key != LibraryListKey::PlaylistTracks {
            return;
        }
        self.model.apply_settings(settings.clone());
        self.collection.apply_settings(settings);
        self.toolbar.apply(key, settings);
    }
}

impl Shell {
    pub(crate) fn playlist_entries_view(
        self: &Rc<Self>,
        selected: &crate::runtime::SelectedLibrary,
        playlist: PlaylistKey,
        playlist_name: String,
        order: library::PlaylistEntryOrder,
        first_rows: Vec<PlaylistEntryRow>,
    ) -> PlaylistEntriesView {
        let settings = self
            .settings
            .current
            .borrow()
            .library_list(LibraryListKey::PlaylistTracks);
        let model = PlaylistEntryModel::new(selected, playlist, order, first_rows, settings);
        let wrapper = gtk::Box::new(gtk::Orientation::Vertical, 8);
        wrapper.set_hexpand(true);
        wrapper.set_width_request(1);

        let search = gtk::SearchEntry::new();
        bind_search_placeholder(&search, "Search");
        search.set_hexpand(true);
        search.set_width_request(1);
        self.set_route_search(Some(search.clone()));
        let toolbar =
            self.library_toolbar_projection(LibraryListKey::PlaylistTracks, search.clone());
        let toolbar_widget = library_route_inset(toolbar.widget());
        toolbar_widget.set_visible(!model.source_is_empty());
        wrapper.append(&toolbar_widget);

        let collection = playlist_entry_collection(self, model.clone(), playlist, playlist_name);
        let stack = gtk::Stack::new();
        stack.set_hexpand(true);
        stack.set_vexpand(true);
        stack.add_named(
            &library_route_inset(self.placeholder_view("Tracks", msgid("No tracks here yet"))),
            Some("empty"),
        );
        stack.add_named(&collection.scrolling_widget(), Some("content"));
        stack.set_visible_child_name(if model.source_is_empty() {
            "empty"
        } else {
            "content"
        });
        wrapper.append(&stack);

        PlaylistEntriesView {
            widget: wrapper.upcast(),
            collection,
            toolbar,
            model,
            search,
            stack,
            toolbar_widget,
            order_generation: Rc::new(PlaylistEntryOrderLane::default()),
        }
    }
}

fn playlist_entry_collection(
    shell: &Rc<Shell>,
    model: PlaylistEntryModel,
    playlist: PlaylistKey,
    playlist_name: String,
) -> LibraryCollectionProjection {
    let settings = shell
        .settings
        .current
        .borrow()
        .library_list(LibraryListKey::PlaylistTracks);
    let playing = TrackRowPlayingIndicator::new();
    let selection = PlaylistEntrySelection::new(model.clone(), playlist_name);
    shell.set_current_playlist_entry_selection(selection.clone());
    let selected_source = shell
        .selected_library()
        .as_deref()
        .map(|selected| selected.source_key);
    let current_model = model.clone();
    let current_playing = playing.clone();
    shell.register_current_route_track_selection(Rc::new(move |current| {
        let position = current
            .filter(|current| selected_source == Some(current.source_id))
            .and_then(|current| {
                let context = current.context.as_ref()?;
                current_model.position_for_current(
                    current.track_id,
                    &context.context_id,
                    context.source_rank,
                )
            })
            .unwrap_or(gtk::INVALID_LIST_POSITION);
        current_playing.set_position(position);
        current_playing.set_paused(current.is_some_and(|current| current.paused));
        true
    }));
    let shell = Rc::clone(shell);
    LibraryCollectionProjection::new(
        settings,
        Rc::new(move |layout| match layout {
            LibraryLayout::Row => LibraryPresentationProjection::Row(playlist_entry_table(
                &shell,
                model.clone(),
                playlist,
                playing.clone(),
                selection.clone(),
            )),
            LibraryLayout::Grid | LibraryLayout::Detail => LibraryPresentationProjection::Grid(
                playlist_entry_grid(&shell, model.clone(), playlist, selection.clone()),
            ),
        }),
    )
}

fn playlist_entry_table(
    shell: &Rc<Shell>,
    model: PlaylistEntryModel,
    playlist: PlaylistKey,
    playing: TrackRowPlayingIndicator,
    selection: PlaylistEntrySelection,
) -> CollectionTableProjection {
    let fields = shell
        .settings
        .current
        .borrow()
        .library_list(LibraryListKey::PlaylistTracks)
        .row_fields;
    let column_shell = Rc::clone(shell);
    let activate_model = model.clone();
    let queue = shell.products.playback.queue.clone();
    let table = dynamic_collection_table(
        shell,
        LibraryListKey::PlaylistTracks,
        model.list_model(),
        &fields,
        Vec::new(),
        move |field| playlist_entry_column(&column_shell, field, playlist, playing.clone()),
        playlist_entry_column_width,
        false,
        Some(Box::new(move |position, _: PlaylistEntryRow| {
            activate_model.activate(position, queue.clone());
        })),
        Some(selection.selection_model()),
        route_column_view_initial_width_with_inset(
            shell,
            super::route_layout::PRIMARY_ROUTE_HORIZONTAL_INSET,
        ),
    );
    table.widget().add_css_class("track-table");
    table.widget().add_css_class("playlist-entry-table");
    table
}

fn playlist_entry_column(
    shell: &Rc<Shell>,
    field: LibraryField,
    playlist: PlaylistKey,
    playing: TrackRowPlayingIndicator,
) -> gtk::ColumnViewColumn {
    let width = playlist_entry_column_width(field);
    match field {
        LibraryField::RowIndex => playlist_entry_number_column(shell, playlist, width, playing),
        LibraryField::Image => playlist_entry_image_column(shell, playlist, width),
        LibraryField::TitleMerged => playlist_entry_title_column(shell, playlist, width, playing),
        LibraryField::Favorite => playlist_entry_favorite_column(shell, playlist, width),
        _ => playlist_entry_text_column(shell, field, width, playlist),
    }
}

fn playlist_entry_number_column(
    shell: &Rc<Shell>,
    playlist: PlaylistKey,
    width: i32,
    playing: TrackRowPlayingIndicator,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let cells = FactoryCells::<PlaylistEntryNumberCell>::new();
    let setup_shell = Rc::clone(shell);
    let setup_cells = cells.clone();
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let current = Rc::new(RefCell::new(None));
        let cell = track_row_index_cell("");
        install_playlist_entry_context(&cell, &setup_shell, playlist, Rc::clone(&current));
        install_playlist_entry_drag(&cell, &setup_shell, playlist, Rc::clone(&current));
        item.set_child(Some(&cell));
        setup_cells.insert(item, PlaylistEntryNumberCell { cell, current });
    });
    let bind_cells = cells.clone();
    let bind_playing = playing.clone();
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let entry = item_at_from_item::<PlaylistEntryRow>(item);
        let Some(entry) = entry else {
            return;
        };
        let Some(cell) = bind_cells.get(item) else {
            return;
        };
        set_track_row_index_text(&cell.cell, &playlist_entry_number(item.position(), true));
        bind_playing.bind(cell.cell.upcast_ref(), item.position());
        cell.current.replace(Some(entry));
    });
    let unbind_cells = cells.clone();
    factory.connect_unbind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(cell) = unbind_cells.get(item) {
            set_track_row_index_text(&cell.cell, "");
            playing.unbind(cell.cell.upcast_ref());
            cell.current.take();
        }
    });
    let teardown_cells = cells.clone();
    factory.connect_teardown(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            teardown_cells.remove(item);
        }
    });
    let column =
        gtk::ColumnViewColumn::new(Some(super::columns::ROW_INDEX_COLUMN_TITLE), Some(factory));
    column.set_fixed_width(width);
    column
}

#[derive(Clone)]
struct PlaylistEntryImageCell {
    cover: crate::shell::cover::ArtworkTile,
    current: Rc<RefCell<Option<PlaylistEntryRow>>>,
}

fn playlist_entry_image_column(
    shell: &Rc<Shell>,
    playlist: PlaylistKey,
    width: i32,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let cells = FactoryCells::<PlaylistEntryImageCell>::new();
    let setup_shell = Rc::clone(shell);
    let setup_cells = cells.clone();
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let current = Rc::new(RefCell::new(None));
        let cover = crate::shell::cover::ArtworkTile::new(width);
        let widget = cover.widget();
        install_playlist_entry_context(&widget, &setup_shell, playlist, Rc::clone(&current));
        install_playlist_entry_drag(&widget, &setup_shell, playlist, Rc::clone(&current));
        item.set_child(Some(&widget));
        setup_cells.insert(item, PlaylistEntryImageCell { cover, current });
    });
    let bind_shell = Rc::clone(shell);
    let bind_cells = cells.clone();
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let entry = item_at_from_item::<PlaylistEntryRow>(item);
        let Some(entry) = entry else {
            return;
        };
        let Some(cell) = bind_cells.get(item) else {
            return;
        };
        if let Some(track) = entry.track.as_ref() {
            bind_shell.bind_artwork_tile(
                &cell.cover,
                opaque_artwork(track.artwork_binding.as_deref()),
                width,
                crate::shell::cover::THUMB_COVER_SIZE,
            );
        } else {
            bind_shell.clear_artwork_tile(&cell.cover);
        }
        cell.current.replace(Some(entry));
    });
    let clear_shell = Rc::clone(shell);
    let clear_cells = cells.clone();
    factory.connect_unbind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(cell) = clear_cells.get(item) {
            clear_shell.clear_artwork_tile(&cell.cover);
            cell.current.take();
        }
    });
    let teardown_cells = cells.clone();
    factory.connect_teardown(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            teardown_cells.remove(item);
        }
    });
    let column = localized_column("Image", &factory);
    column.set_fixed_width(width);
    column
}

#[derive(Clone)]
struct PlaylistEntryFavoriteCell {
    button: gtk::Button,
    current: Rc<RefCell<Option<PlaylistEntryRow>>>,
}

fn playlist_entry_favorite_column(
    shell: &Rc<Shell>,
    playlist: PlaylistKey,
    width: i32,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let cells = FactoryCells::<PlaylistEntryFavoriteCell>::new();
    let setup_shell = Rc::clone(shell);
    let setup_cells = cells.clone();
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let current = Rc::new(RefCell::new(None::<PlaylistEntryRow>));
        let button = row_favorite_icon_button("Favorite track");
        install_playlist_entry_context(&button, &setup_shell, playlist, Rc::clone(&current));
        let favorite_current = Rc::clone(&current);
        setup_shell.register_dynamic_favorite_button(
            Rc::new(move || {
                favorite_current
                    .borrow()
                    .as_ref()
                    .and_then(|entry| entry.track.as_ref())
                    .map(|track| track_favorite_key(&track.track_key))
            }),
            &button,
        );
        let click_shell = Rc::clone(&setup_shell);
        let click_current = Rc::clone(&current);
        button.connect_clicked(move |button| {
            let Some(track) = click_current
                .borrow()
                .as_ref()
                .and_then(|entry| entry.track.clone())
            else {
                return;
            };
            click_shell.set_favorite_with_feedback(
                library::FavoriteTarget::Track(track.track_key),
                !favorite_button_is_active(button),
                Some(button),
            );
        });
        item.set_child(Some(&button));
        setup_cells.insert(item, PlaylistEntryFavoriteCell { button, current });
    });
    let bind_cells = cells.clone();
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let entry = item_at_from_item::<PlaylistEntryRow>(item);
        let Some(entry) = entry else {
            return;
        };
        let Some(cell) = bind_cells.get(item) else {
            return;
        };
        let track = entry.track.as_ref();
        set_favorite_button_active(&cell.button, track.is_some_and(|track| track.favorite));
        cell.button.set_sensitive(track.is_some());
        cell.current.replace(Some(entry));
    });
    let clear_cells = cells.clone();
    factory.connect_unbind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(cell) = clear_cells.get(item) {
            set_favorite_button_active(&cell.button, false);
            cell.button.set_sensitive(false);
            cell.current.take();
        }
    });
    let teardown_cells = cells.clone();
    factory.connect_teardown(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            teardown_cells.remove(item);
        }
    });
    let column = gtk::ColumnViewColumn::new(Some(FAVORITE_COLUMN_TITLE), Some(factory));
    column.set_fixed_width(width);
    column
}

#[derive(Clone)]
struct PlaylistEntryTitleCell {
    cover: crate::shell::cover::ArtworkTile,
    title: gtk::Label,
    artist: gtk::Label,
    artist_links: DetailLinkBinding,
    downloaded: gtk::Image,
    current: Rc<RefCell<Option<PlaylistEntryRow>>>,
}

fn playlist_entry_title_column(
    shell: &Rc<Shell>,
    playlist: PlaylistKey,
    width: i32,
    playing: TrackRowPlayingIndicator,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let cells = FactoryCells::<PlaylistEntryTitleCell>::new();
    let setup_shell = Rc::clone(shell);
    let setup_cells = cells.clone();
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let current = Rc::new(RefCell::new(None::<PlaylistEntryRow>));
        let cover = crate::shell::cover::ArtworkTile::new(36);
        let labels = gtk::Box::new(gtk::Orientation::Vertical, 2);
        labels.set_hexpand(true);
        labels.set_halign(gtk::Align::Fill);
        labels.set_width_request(1);
        let title = playlist_entry_label("", "playlist-entry-title", 44);
        let title_row = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        title_row.append(&title);
        let downloaded = setup_shell.download_badge(false);
        title_row.append(&downloaded);
        let artist = playlist_entry_label("", "muted", 44);
        artist.add_css_class("table-link-label");
        let artist_links = DetailLinkBinding::new(&artist, &setup_shell);
        labels.append(&title_row);
        labels.append(&artist);
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        root.set_hexpand(true);
        root.set_halign(gtk::Align::Fill);
        root.set_width_request(1);
        root.append(&cover.widget());
        root.append(&labels);
        install_playlist_entry_context(&root, &setup_shell, playlist, Rc::clone(&current));
        install_playlist_entry_drag(&root, &setup_shell, playlist, Rc::clone(&current));
        item.set_child(Some(&root));
        setup_cells.insert(
            item,
            PlaylistEntryTitleCell {
                cover,
                title,
                artist,
                artist_links,
                downloaded,
                current,
            },
        );
    });
    let bind_shell = Rc::clone(shell);
    let bind_cells = cells.clone();
    let bind_playing = playing.clone();
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let entry = item_at_from_item::<PlaylistEntryRow>(item);
        let Some(entry) = entry else {
            return;
        };
        let Some(cell) = bind_cells.get(item) else {
            return;
        };
        if let Some(track) = entry.track.as_ref() {
            bind_shell.bind_artwork_tile(
                &cell.cover,
                opaque_artwork(track.artwork_binding.as_deref()),
                36,
                crate::shell::cover::THUMB_COVER_SIZE,
            );
            cell.title.set_text(&track.title);
            cell.artist_links.bind(track_artist_links(track));
            bind_shell.bind_download_badge(&cell.downloaded, track.is_downloaded);
            bind_playing.bind(cell.title.upcast_ref(), item.position());
        } else {
            bind_shell.clear_artwork_tile(&cell.cover);
            cell.title.set_text("");
            cell.artist_links.clear();
            bind_shell.clear_download_badge(&cell.downloaded);
            bind_playing.unbind(cell.title.upcast_ref());
        }
        cell.current.replace(Some(entry));
    });
    let clear_shell = Rc::clone(shell);
    let clear_cells = cells.clone();
    factory.connect_unbind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(cell) = clear_cells.get(item) {
            clear_shell.clear_artwork_tile(&cell.cover);
            clear_shell.clear_download_badge(&cell.downloaded);
            cell.title.set_text("");
            cell.artist.set_text("");
            cell.artist_links.clear();
            playing.unbind(cell.title.upcast_ref());
            cell.current.take();
        }
    });
    let teardown_cells = cells.clone();
    factory.connect_teardown(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            teardown_cells.remove(item);
        }
    });
    let column = localized_column("Title", &factory);
    column.set_fixed_width(width);
    column
}

fn playlist_entry_label(text: &str, css_class: &str, max_width_chars: i32) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    if !css_class.is_empty() {
        label.add_css_class(css_class);
    }
    label.set_xalign(0.0);
    label.set_width_chars(1);
    label.set_max_width_chars(max_width_chars);
    label.set_wrap(false);
    label.set_single_line_mode(true);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label
}

fn playlist_entry_text_column(
    shell: &Rc<Shell>,
    field: LibraryField,
    width: i32,
    playlist: PlaylistKey,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let cells = FactoryCells::<PlaylistEntryTextCell>::new();
    let setup_shell = Rc::clone(shell);
    let setup_cells = cells.clone();
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let current = Rc::new(RefCell::new(None::<PlaylistEntryRow>));
        let label = gtk::Label::new(None);
        add_field_skeleton_class(&label, field);
        label.add_css_class("muted");
        label.set_xalign(0.0);
        label.set_halign(gtk::Align::Fill);
        label.set_hexpand(true);
        label.set_width_request(1);
        label.set_wrap(false);
        label.set_single_line_mode(true);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let links = matches!(
            field,
            LibraryField::Album | LibraryField::Artist | LibraryField::AlbumArtist
        )
        .then(|| {
            label.add_css_class("table-link-label");
            DetailLinkBinding::new(&label, &setup_shell)
        });
        install_playlist_entry_context(&label, &setup_shell, playlist, Rc::clone(&current));
        install_playlist_entry_drag(&label, &setup_shell, playlist, Rc::clone(&current));
        item.set_child(Some(&label));
        setup_cells.insert(
            item,
            PlaylistEntryTextCell {
                label,
                links,
                current,
            },
        );
    });
    let bind_cells = cells.clone();
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let entry = item_at_from_item::<PlaylistEntryRow>(item);
        let Some(entry) = entry else {
            return;
        };
        let Some(cell) = bind_cells.get(item) else {
            return;
        };
        if let Some(track) = entry.track.as_ref() {
            let links = match field {
                LibraryField::Album => Some(DetailLinks::route(
                    &track.display_album,
                    track.album_key.map(super::route::Route::AlbumDetail),
                )),
                LibraryField::Artist => Some(track_artist_links(track)),
                LibraryField::AlbumArtist => Some(track_album_artist_links(track)),
                _ => None,
            };
            if let (Some(binding), Some(links)) = (cell.links.as_ref(), links) {
                binding.bind(links);
            } else {
                cell.label.set_text(&track_field(track, field));
            }
        } else if let Some(binding) = cell.links.as_ref() {
            binding.clear();
        } else {
            cell.label.set_text("");
        }
        cell.current.replace(Some(entry));
    });
    let unbind_cells = cells.clone();
    factory.connect_unbind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(cell) = unbind_cells.get(item) {
            cell.label.set_text("");
            if let Some(binding) = cell.links.as_ref() {
                binding.clear();
            }
            cell.current.take();
        }
    });
    let teardown_cells = cells.clone();
    factory.connect_teardown(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            teardown_cells.remove(item);
        }
    });
    let column = localized_column(field.title(), &factory);
    column.set_fixed_width(width);
    column
}

fn install_playlist_entry_context(
    target: &impl IsA<gtk::Widget>,
    shell: &Rc<Shell>,
    playlist: PlaylistKey,
    current: Rc<RefCell<Option<PlaylistEntryRow>>>,
) {
    let shell = Rc::clone(shell);
    install_context_menu_openers(
        target,
        Rc::new(move |target, position| {
            let Some(entry) = current.borrow().clone() else {
                return;
            };
            let Some(track) = entry.track else {
                return;
            };
            present_playlist_entry_context_menu(
                target,
                &shell,
                playlist,
                entry.playlist_entry_key,
                track,
                position,
            );
        }),
    );
}

fn install_playlist_entry_drag(
    target: &impl IsA<gtk::Widget>,
    shell: &Rc<Shell>,
    playlist: PlaylistKey,
    current: Rc<RefCell<Option<PlaylistEntryRow>>>,
) {
    target.as_ref().set_valign(gtk::Align::Fill);
    let drag_source = gtk::DragSource::builder()
        .actions(gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::MOVE)
        .build();
    drag_source.set_propagation_phase(gtk::PropagationPhase::Capture);
    let preview = PlaylistDragPreviewBinding::default();
    preview.connect(&drag_source);
    let drag_preview = preview.clone();
    let drag_shell = Rc::downgrade(shell);
    let drag_current = Rc::clone(&current);
    drag_source.connect_prepare(move |_, _, _| {
        drag_preview.clear();
        let shell = drag_shell.upgrade()?;
        let entry = drag_current.borrow().clone()?;
        let track = entry.track.as_ref()?;
        let title = track.title.clone();
        let artwork = track
            .artwork_binding
            .as_deref()
            .map(artwork::ArtworkBinding::opaque)
            .unwrap_or_default();
        let selection = shell
            .current_playlist_entry_selection(entry.playlist_entry_key)
            .or_else(|| {
                shell
                    .current_playlist_entry_selection_owner()
                    .map(|selection| {
                        selection.single_entry(entry.playlist_entry_key, entry.track_key)
                    })
            })?;
        let mut providers = Vec::new();
        if selection.entries.len() == 1 {
            providers.push(gtk::gdk::ContentProvider::for_value(
                &entry.playlist_entry_key.raw().to_value(),
            ));
        }
        if !selection.tracks.tracks.is_empty() {
            providers.push(playlist_drag_content_provider(
                PlaylistTrackSource::selection(selection.tracks),
            ));
        }
        drag_preview.prepare_cache_only(&shell, title, artwork);
        match providers.as_slice() {
            [] => None,
            [provider] => Some(provider.clone()),
            providers => Some(gtk::gdk::ContentProvider::new_union(providers)),
        }
    });
    target.as_ref().add_controller(drag_source);

    let operations = shell.selected_source_operations();
    let drop_target = gtk::DropTarget::new(i64::static_type(), gtk::gdk::DragAction::MOVE);
    drop_target.connect_drop(move |_, value, _, _| {
        let Ok(entry) = value.get::<i64>() else {
            return false;
        };
        let Some(target) = current.borrow().clone() else {
            return false;
        };
        let Some(operations) = operations.as_ref() else {
            return false;
        };
        let entry = PlaylistEntryKey::from_raw(entry);
        if entry == target.playlist_entry_key {
            return false;
        }
        operations.move_playlist_entry(
            playlist,
            entry,
            playlist_entry_drop_position(target.position),
        );
        true
    });
    target.as_ref().add_controller(drop_target);
}

struct PlaylistEntryGridCell {
    body: CollectionGridCardCell,
    shell: Rc<Shell>,
    cover: crate::shell::cover::ArtworkTile,
    current: Rc<RefCell<Option<PlaylistEntryRow>>>,
}

impl PlaylistEntryGridCell {
    fn new(shell: Rc<Shell>, fields: &[LibraryField], playlist: PlaylistKey) -> Self {
        let cover = crate::shell::cover::ArtworkTile::new_elastic_square();
        let cover_frame = super::cards::square_cover_frame(&cover.widget(), None);
        let body = CollectionGridCardCell::new(&shell, fields, cover_frame.upcast());
        body.set_download_badge(shell.download_badge(true));
        let current = Rc::new(RefCell::new(None));
        install_playlist_entry_context(&body.card, &shell, playlist, Rc::clone(&current));
        install_playlist_entry_drag(&body.card, &shell, playlist, Rc::clone(&current));
        Self {
            body,
            shell,
            cover,
            current,
        }
    }

    fn bind_current_artwork(&self) {
        let current = self.current.borrow();
        let Some(track) = current.as_ref().and_then(|entry| entry.track.as_ref()) else {
            return;
        };
        self.cover
            .widget()
            .remove_css_class("collection-grid-cover-skeleton");
        self.shell.bind_artwork_tile(
            &self.cover,
            opaque_artwork(track.artwork_binding.as_deref()),
            COLLECTION_GRID_MAX_CARD_WIDTH,
            crate::shell::cover::LARGE_COVER_SIZE,
        );
    }

    fn release_artwork(&self) {
        self.shell.clear_artwork_tile(&self.cover);
        self.cover
            .widget()
            .add_css_class("collection-grid-cover-skeleton");
        self.cover.widget().set_opacity(1.0);
    }
}

impl ReusableCollectionGridCell<PlaylistEntryRow> for PlaylistEntryGridCell {
    fn widget(&self) -> gtk::Widget {
        self.body.widget()
    }

    fn bind(&self, _: u32, entry: PlaylistEntryRow) {
        let Some(track) = entry.track.as_ref() else {
            return;
        };
        self.body
            .bind(&track.title, |field| track_grid_field_links(track, field));
        self.body
            .set_download_target(&self.shell, track.is_downloaded);
        self.current.replace(Some(entry));
        self.bind_current_artwork();
    }

    fn clear(&self) {
        self.release_artwork();
        self.body.clear(&self.shell);
        self.current.take();
    }

    fn set_ready(&self, ready: bool) {
        self.body.set_ready(ready);
    }

    fn apply_fields(&self, fields: &[LibraryField]) {
        self.body.replace_fields(&self.shell, fields);
        if let Some(track) = self
            .current
            .borrow()
            .as_ref()
            .and_then(|entry| entry.track.as_ref())
        {
            self.body
                .bind(&track.title, |field| track_grid_field_links(track, field));
        }
    }
}

fn playlist_entry_grid(
    shell: &Rc<Shell>,
    model: PlaylistEntryModel,
    playlist: PlaylistKey,
    selection: PlaylistEntrySelection,
) -> CollectionGridProjection {
    let fields = shell
        .settings
        .current
        .borrow()
        .library_list(LibraryListKey::PlaylistTracks)
        .grid_fields;
    let cell_shell = Rc::clone(shell);
    let activate_model = model.clone();
    let queue = shell.products.playback.queue.clone();
    collection_grid_with_selection_and_demand(
        model.list_model(),
        Some(selection.selection_model()),
        &fields,
        move |fields| PlaylistEntryGridCell::new(Rc::clone(&cell_shell), fields, playlist),
        move |position, _: PlaylistEntryRow| activate_model.activate(position, queue.clone()),
        |_| {},
    )
}

#[derive(Clone)]
struct PlaylistEntryTextCell {
    label: gtk::Label,
    links: Option<DetailLinkBinding>,
    current: Rc<RefCell<Option<PlaylistEntryRow>>>,
}

#[derive(Clone)]
struct PlaylistEntryNumberCell {
    cell: gtk::Overlay,
    current: Rc<RefCell<Option<PlaylistEntryRow>>>,
}

fn playlist_entry_number(position: u32, ready: bool) -> String {
    ready
        .then(|| (position + 1).to_string())
        .unwrap_or_default()
}

fn playlist_entry_drop_position(target_position: i64) -> usize {
    usize::try_from(target_position.max(0)).unwrap_or_default()
}

fn playlist_entry_column_width(field: LibraryField) -> i32 {
    match field {
        LibraryField::RowIndex => 24,
        LibraryField::Image => 36,
        LibraryField::Title | LibraryField::TitleMerged => 320,
        LibraryField::Album => 220,
        _ => track_column_fit_width(LibraryListKey::PlaylistTracks, field),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_playlist_update_rejects_a_stale_request() {
        let lane = PlaylistEntryOrderLane::default();
        let stale = lane.begin();
        let current = lane.begin();
        assert!(!lane.accepts(stale));
        assert!(lane.accepts(current));
    }

    #[test]
    fn playlist_placeholder_keeps_number_and_actions_inert_until_ready() {
        assert_eq!(playlist_entry_number(7, false), "");
        assert_eq!(playlist_entry_number(7, true), "8");
    }

    #[test]
    fn playlist_drop_uses_the_crossed_target_position() {
        assert_eq!(playlist_entry_drop_position(3), 3);
        assert_eq!(playlist_entry_drop_position(-1), 0);
    }
}
