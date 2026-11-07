use crate::widgets::media_drag::{
    MediaDragPreviewBinding, MediaDragSource, media_drag_content_provider,
};
use crate::widgets::view_model::item_at_from_item;
use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use library::{PlaylistKey, TrackRow};
use localization::msgid;

use crate::favorites::{favorite_button_is_active, set_favorite_button_active};
use crate::library::CatalogUi;
use crate::library::{LibraryField, LibraryLayout, LibraryListKey, LibraryListSettings};
use crate::widgets::interactions::install_context_menu_openers;
use crate::widgets::localization::{bind_search_placeholder, localized_column};

use super::collection_context::present_playlist_entry_context_menu;
use super::collections::{
    CollectionTableProjection, LibraryCollectionProjection, LibraryPresentationProjection,
    dynamic_collection_table, library_route_inset,
};
use super::columns::track_column_fit_width;
use super::grid_cells::{
    CollectionGridCardCell, CollectionGridProjection, ReusableCollectionGridCell, collection_grid,
};
use crate::widgets::recycled_cells::{TrackRowPlayingIndicator, set_track_row_index_text};

use super::route_shell::LibraryToolbarProjection;
use crate::library::track_selection::PlaylistEntrySelection;
use crate::widgets::library_fields::{
    COLLECTION_GRID_MAX_CARD_WIDTH, add_field_skeleton_class, opaque_artwork,
};
use crate::widgets::recycled_cells::{
    RecycledArtworkCell, RecycledMergedCell, RecycledTextCell, list_cell,
};
use crate::widgets::view_model::{Rows, connect_row_bind};
use library::Row;
use rufin_core::route::detail_links::DetailLinks;
use rufin_core::settings::presentation::track_row_field;

fn entry_key(row: &TrackRow) -> Option<library::PlaylistEntryKey> {
    row.entry.as_ref().map(|entry| entry.key)
}

#[derive(Clone)]
pub struct PlaylistEntriesView {
    widget: gtk::Widget,
    collection: LibraryCollectionProjection,
    toolbar: LibraryToolbarProjection,
    pub(crate) rows: Rows<Row>,
    search: gtk::SearchEntry,
    stack: gtk::Stack,
    toolbar_widget: gtk::Widget,
}

impl PlaylistEntriesView {
    pub fn play(
        &self,
        queue: &rufin_core::playback::PlaybackOwner,
        placement: rufin_core::playback::QueuePlacement,
        shuffled: bool,
    ) {
        self.rows.play_all(queue, placement, shuffled);
    }

    pub fn widget(&self) -> gtk::Widget {
        self.widget.clone()
    }

    pub fn item_navigation(&self) -> crate::widgets::mounted_route::MountedRouteItemNavigation {
        self.collection.item_navigation()
    }

    pub fn search(&self) -> gtk::SearchEntry {
        self.search.clone()
    }

    pub fn layout_cycle(&self) -> crate::widgets::mounted_route::MountedRouteCommand {
        self.toolbar.layout_cycle()
    }

    /// Shows whether the playlist is empty: a search that matches nothing keeps its toolbar.
    pub fn show_empty(&self) {
        let empty = self.rows.is_empty()
            && self
                .rows
                .view()
                .is_none_or(|view| view.options().filter.is_empty());
        self.toolbar_widget.set_visible(!empty);
        self.stack
            .set_visible_child_name(if empty { "empty" } else { "content" });
    }

    pub fn apply_library_list_settings(&self, key: LibraryListKey, settings: &LibraryListSettings) {
        if key != LibraryListKey::PlaylistTracks {
            return;
        }
        self.collection.apply_settings(settings);
        self.toolbar.apply(key, settings);
    }
}

impl CatalogUi {
    pub fn playlist_entries_view(
        self: &Rc<Self>,
        playlist: PlaylistKey,
        playlist_name: String,
        writable: bool,
        rows: Rows<Row>,
    ) -> PlaylistEntriesView {
        let model = rows;
        let wrapper = gtk::Box::new(gtk::Orientation::Vertical, 8);
        wrapper.set_hexpand(true);
        wrapper.set_width_request(1);

        let search = gtk::SearchEntry::new();
        crate::widgets::controls::configure_search_entry(&search);
        bind_search_placeholder(&search, "Search");
        search.set_hexpand(true);
        search.set_width_request(1);
        let toolbar =
            self.library_toolbar_projection(LibraryListKey::PlaylistTracks, search.clone());
        let toolbar_widget = library_route_inset(toolbar.widget());
        wrapper.append(&toolbar_widget);

        let collection =
            playlist_entry_collection(self, model.clone(), playlist, playlist_name, writable);
        let stack = gtk::Stack::new();
        stack.set_hexpand(true);
        stack.set_vexpand(true);
        stack.add_named(
            &library_route_inset(crate::library::route_layout::placeholder_view(
                "Tracks",
                msgid("No tracks here yet"),
            )),
            Some("empty"),
        );
        stack.add_named(&collection.scrolling_widget(), Some("content"));
        wrapper.append(&stack);

        let view = PlaylistEntriesView {
            widget: wrapper.upcast(),
            collection,
            toolbar,
            rows: model,
            search,
            stack,
            toolbar_widget,
        };
        view.show_empty();
        view
    }
}

fn playlist_entry_collection(
    shell: &Rc<CatalogUi>,
    model: Rows<Row>,
    playlist: PlaylistKey,
    playlist_name: String,
    writable: bool,
) -> LibraryCollectionProjection {
    let settings = shell
        .settings
        .current
        .borrow()
        .library_list(LibraryListKey::PlaylistTracks);
    let playing = TrackRowPlayingIndicator::new();
    let selection = PlaylistEntrySelection::new(&model, playlist, playlist_name, writable);
    shell.set_current_playlist_entry_selection(selection.clone());
    shell.register_playing_rows(&model, &playing);
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
            LibraryLayout::Grid | LibraryLayout::Detail => {
                LibraryPresentationProjection::Grid(playlist_entry_grid(
                    &shell,
                    model.clone(),
                    playlist,
                    selection.clone(),
                    playing.clone(),
                ))
            }
        }),
    )
}

fn playlist_entry_table(
    shell: &Rc<CatalogUi>,
    model: Rows<Row>,
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
    let products = shell.products.clone();
    let table = dynamic_collection_table(
        shell,
        LibraryListKey::PlaylistTracks,
        model.list_model(),
        &fields,
        Vec::new(),
        move |field| playlist_entry_column(&column_shell, field, playlist, playing.clone()),
        playlist_entry_column_width,
        false,
        Some(Box::new(move |position, _: TrackRow| {
            activate_model.play(
                &products,
                position as usize,
                rufin_core::playback::QueuePlacement::Now,
                false,
            );
        })),
        Some(selection.selection_model()),
        shell.table_initial_width(crate::library::route_layout::PRIMARY_ROUTE_HORIZONTAL_INSET),
    );
    table.widget().add_css_class("track-table");
    table.widget().add_css_class("playlist-entry-table");
    table
}

fn playlist_entry_column(
    shell: &Rc<CatalogUi>,
    field: LibraryField,
    playlist: PlaylistKey,
    playing: TrackRowPlayingIndicator,
) -> gtk::ColumnViewColumn {
    let width = playlist_entry_column_width(field);
    match field {
        LibraryField::RowIndex => playlist_entry_number_column(shell, playlist, width, playing),
        LibraryField::Image => playlist_entry_image_column(shell, playlist, width),
        LibraryField::TitleMerged => playlist_entry_title_column(shell, playlist, width, playing),
        LibraryField::Favorite | LibraryField::Tools => playlist_entry_actions_column(shell, field),
        _ => playlist_entry_text_column(shell, field, width, playlist, playing),
    }
}

fn playlist_entry_number_column(
    shell: &Rc<CatalogUi>,
    playlist: PlaylistKey,
    width: i32,
    playing: TrackRowPlayingIndicator,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let setup_shell = Rc::clone(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = crate::widgets::recycled_cells::track_list_row_index_cell(item);
        let current = list_item_entry(item);
        install_playlist_entry_context(&cell, &setup_shell, Rc::clone(&current));
        install_playlist_entry_drag(&cell, &setup_shell, playlist, current);
        item.set_child(Some(&cell));
    });
    let bind_playing = playing.clone();
    connect_row_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let entry = item_at_from_item::<TrackRow>(item);
        let Some(entry) = entry else {
            return;
        };
        let Some(cell) = item.child().and_downcast::<gtk::Overlay>() else {
            return;
        };
        set_track_row_index_text(&cell, &playlist_entry_number(item.position(), true));
        bind_playing.bind(cell.upcast_ref(), item.position(), &entry.media_uri);
    });
    factory.connect_unbind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(cell) = item.child().and_downcast::<gtk::Overlay>() {
            playing.unbind(cell.upcast_ref());
            set_track_row_index_text(&cell, "");
        }
    });
    let column = gtk::ColumnViewColumn::new(Some("#"), Some(factory));
    column.set_fixed_width(width);
    column
}

fn playlist_entry_image_column(
    shell: &Rc<CatalogUi>,
    playlist: PlaylistKey,
    width: i32,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let setup_shell = Rc::clone(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = RecycledArtworkCell::new(width);
        let current = list_item_entry(item);
        install_playlist_entry_context(&cell, &setup_shell, Rc::clone(&current));
        install_playlist_entry_drag(&cell, &setup_shell, playlist, current);
        item.set_child(Some(&cell));
    });
    let bind_shell = Rc::clone(shell);
    connect_row_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let entry = item_at_from_item::<TrackRow>(item);
        let Some(entry) = entry else {
            return;
        };
        let Some(cell) = list_cell::<RecycledArtworkCell>(item) else {
            return;
        };
        let cover = cell.artwork();
        bind_shell.artwork.bind_artwork_tile(
            &cover,
            opaque_artwork(entry.artwork_binding.as_deref()),
            width,
        );
    });
    let clear_shell = Rc::clone(shell);
    factory.connect_unbind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(cell) = list_cell::<RecycledArtworkCell>(item) {
            clear_shell.artwork.clear_artwork_tile(&cell.artwork());
        }
    });
    let column = localized_column("Image", &factory);
    column.set_fixed_width(width);
    column
}

fn playlist_entry_actions_column(
    shell: &Rc<CatalogUi>,
    field: LibraryField,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let setup_shell = Rc::clone(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let actions = crate::widgets::recycled_cells::row_action_cell(field, "Favorite track");
        let current = list_item_entry(item);
        install_playlist_entry_context(&actions, &setup_shell, Rc::clone(&current));
        item.set_child(Some(&actions));
        let Some(button) = crate::widgets::recycled_cells::row_favorite_button(item) else {
            return;
        };
        let click_shell = Rc::clone(&setup_shell);
        let click_current = current;
        button.connect_clicked(move |button| {
            let Some(entry) = click_current() else {
                return;
            };
            click_shell.set_favorite(
                library::FavoriteTarget::Track(entry.media_uri.clone()),
                !favorite_button_is_active(button),
            );
        });
    });
    connect_row_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let entry = item_at_from_item::<TrackRow>(item);
        let Some(entry) = entry else {
            return;
        };
        let Some(button) = crate::widgets::recycled_cells::row_favorite_button(item) else {
            return;
        };
        set_favorite_button_active(&button, entry.state.favorite);
        button.set_sensitive(true);
    });
    factory.connect_unbind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(button) = crate::widgets::recycled_cells::row_favorite_button(item) {
            set_favorite_button_active(&button, false);
            button.set_sensitive(false);
        }
    });
    crate::widgets::recycled_cells::row_action_column(field, &factory)
}

fn playlist_entry_title_column(
    shell: &Rc<CatalogUi>,
    playlist: PlaylistKey,
    width: i32,
    playing: TrackRowPlayingIndicator,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let setup_shell = Rc::clone(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = RecycledMergedCell::new(
            setup_shell.route_navigation(),
            &setup_shell.downloads,
            36,
            true,
        );
        cell.set_hexpand(true);
        cell.set_halign(gtk::Align::Fill);
        cell.set_width_request(1);
        let title = cell.title();
        title.add_css_class("playlist-entry-title");
        title.set_width_chars(1);
        title.set_max_width_chars(44);
        let artist = cell.subtitle();
        artist.add_css_class("muted");
        artist.add_css_class("table-link-label");
        artist.set_max_width_chars(44);
        let current = list_item_entry(item);
        install_playlist_entry_context(&cell, &setup_shell, Rc::clone(&current));
        install_playlist_entry_drag(&cell, &setup_shell, playlist, current);
        item.set_child(Some(&cell));
    });
    let bind_shell = Rc::clone(shell);
    let bind_playing = playing.clone();
    connect_row_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let entry = item_at_from_item::<TrackRow>(item);
        let Some(entry) = entry else {
            return;
        };
        let Some(cell) = list_cell::<RecycledMergedCell>(item) else {
            return;
        };
        let cover = cell.cover();
        let title = cell.title();
        bind_shell.artwork.bind_artwork_tile(
            &cover,
            opaque_artwork(entry.artwork_binding.as_deref()),
            36,
        );
        title.set_text(&entry.title);
        cell.bind_subtitle(playlist_entry_field_links(&entry, LibraryField::Artist));
        cell.subtitle().set_visible(true);
        bind_shell.downloads.bind_download_badge(
            &cell.downloaded().expect("playlist title badge"),
            crate::downloads::media_download_badge(&entry.media_uri, entry.is_downloaded),
        );
        bind_playing.bind(title.upcast_ref(), item.position(), &entry.media_uri);
    });
    let clear_shell = Rc::clone(shell);
    factory.connect_unbind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(cell) = list_cell::<RecycledMergedCell>(item) {
            let title = cell.title();
            clear_shell.artwork.clear_artwork_tile(&cell.cover());
            clear_shell
                .downloads
                .clear_download_badge(&cell.downloaded().expect("playlist title badge"));
            title.set_text("");
            cell.clear_subtitle();
            playing.unbind(title.upcast_ref());
        }
    });
    let column = localized_column("Title", &factory);
    column.set_fixed_width(width);
    column
}

fn playlist_entry_text_column(
    shell: &Rc<CatalogUi>,
    field: LibraryField,
    width: i32,
    playlist: PlaylistKey,
    playing: TrackRowPlayingIndicator,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let setup_shell = Rc::clone(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = RecycledTextCell::new();
        let label = cell.label();
        if field == LibraryField::Title {
            cell.set_spacing(5);
        }
        add_field_skeleton_class(&label, field);
        label.add_css_class("muted");
        label.set_xalign(0.0);
        label.set_halign(gtk::Align::Fill);
        label.set_hexpand(true);
        label.set_width_request(1);
        label.set_wrap(false);
        label.set_single_line_mode(true);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        if matches!(
            field,
            LibraryField::Album | LibraryField::Artist | LibraryField::AlbumArtist
        ) {
            label.add_css_class("table-link-label");
            cell.enable_links(setup_shell.route_navigation());
        }
        let current = list_item_entry(item);
        install_playlist_entry_context(&cell, &setup_shell, Rc::clone(&current));
        install_playlist_entry_drag(&cell, &setup_shell, playlist, current);
        item.set_child(Some(&cell));
    });
    let bind_playing = playing.clone();
    connect_row_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let entry = item_at_from_item::<TrackRow>(item);
        let Some(entry) = entry else {
            return;
        };
        let Some(cell) = list_cell::<RecycledTextCell>(item) else {
            return;
        };
        let label = cell.label();
        if field == LibraryField::Title {
            bind_playing.bind(label.upcast_ref(), item.position(), &entry.media_uri);
        }
        if matches!(
            field,
            LibraryField::Album | LibraryField::Artist | LibraryField::AlbumArtist
        ) {
            cell.bind_links(playlist_entry_field_links(&entry, field));
        } else {
            label.set_text(&track_row_field(&entry, field));
        }
    });
    factory.connect_unbind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(cell) = list_cell::<RecycledTextCell>(item) {
            if field == LibraryField::Title {
                playing.unbind(cell.label().upcast_ref());
            }
            cell.clear();
        }
    });
    let column = localized_column(super::columns::track_column_title(field), &factory);
    column.set_fixed_width(width);
    column
}

fn list_item_entry(item: &gtk::ListItem) -> Rc<dyn Fn() -> Option<TrackRow>> {
    let item = item.downgrade();
    Rc::new(move || {
        item.upgrade()
            .and_then(|item| item_at_from_item::<TrackRow>(&item))
    })
}

fn install_playlist_entry_context(
    target: &impl IsA<gtk::Widget>,
    shell: &Rc<CatalogUi>,
    current: Rc<dyn Fn() -> Option<TrackRow>>,
) {
    let shell = Rc::downgrade(shell);
    install_context_menu_openers(
        target,
        Rc::new(move |target, position| {
            let Some(shell) = shell.upgrade() else {
                return;
            };
            let Some(entry) = current() else {
                return;
            };
            let Some(key) = entry_key(&entry) else {
                return;
            };
            present_playlist_entry_context_menu(target, &shell, key, entry, position);
        }),
    );
}

fn install_playlist_entry_drag(
    target: &impl IsA<gtk::Widget>,
    shell: &Rc<CatalogUi>,
    playlist: PlaylistKey,
    current: Rc<dyn Fn() -> Option<TrackRow>>,
) {
    target.as_ref().set_valign(gtk::Align::Fill);
    let drag_source = gtk::DragSource::builder()
        .actions(gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::MOVE)
        .build();
    drag_source.set_propagation_phase(gtk::PropagationPhase::Capture);
    let preview = MediaDragPreviewBinding::default();
    preview.connect(&drag_source);
    let drag_preview = preview.clone();
    let drag_shell = Rc::downgrade(shell);
    let drag_current = Rc::clone(&current);
    drag_source.connect_prepare(move |_, _, _| {
        drag_preview.clear();
        let shell = drag_shell.upgrade()?;
        let entry = drag_current()?;
        let title = entry.title.clone();
        let artwork = entry
            .artwork_binding
            .as_deref()
            .map(artwork::ArtworkBinding::opaque)
            .unwrap_or_default();
        let dragged = entry.entry.clone()?;
        let selection = shell
            .current_playlist_entry_selection(dragged.key)
            .or_else(|| {
                shell
                    .current_playlist_entry_selection_owner()
                    .map(|selection| selection.single_entry(dragged))
            })?;
        drag_preview.prepare_cache_only(&shell.artwork, title, artwork);
        Some(media_drag_content_provider(
            MediaDragSource::playlist_entries(selection),
        ))
    });
    target.as_ref().add_controller(drag_source);

    let move_shell = Rc::downgrade(shell);
    let drop_target = gtk::DropTarget::new(
        glib::BoxedAnyObject::static_type(),
        gtk::gdk::DragAction::MOVE,
    );
    drop_target.set_preload(true);
    drop_target.connect_notify_local(Some("value"), move |drop, _| {
        let Some(value) = drop.value() else { return };
        if !matches!(crate::widgets::media_drag::media_drag_source(&value), Some(MediaDragSource::PlaylistEntries(selection)) if selection.playlist == playlist && selection.writable && selection.count == 1) {
            drop.reject();
        }
    });
    drop_target.connect_drop(move |drop, value, x, y| {
        let Some(MediaDragSource::PlaylistEntries(selection)) =
            crate::widgets::media_drag::media_drag_source(value)
        else {
            return false;
        };
        if selection.playlist != playlist || !selection.writable {
            return false;
        }
        let (Some(moved), Some(target)) = (selection.entry, current().and_then(|row| row.entry))
        else {
            return false;
        };
        if moved.key == target.key {
            return false;
        }
        let Some(shell) = move_shell.upgrade() else {
            return false;
        };
        let (entry, moved, target) = (
            moved.key,
            i64::from(moved.position),
            i64::from(target.position),
        );
        let after = drop
            .widget()
            .is_some_and(|widget| crate::widgets::media_drag::order_drop_after(&widget, x, y));
        let settings = shell
            .settings
            .current
            .borrow()
            .library_list(LibraryListKey::PlaylistTracks);
        let after = after != (settings.descending && settings.sort_key == LibraryField::RowIndex);
        let position = target + i64::from(after) - i64::from(moved < target + i64::from(after));
        if position != moved {
            rufin_core::playlists::move_playlist_entry(
                &shell.products.source,
                playlist,
                entry,
                position.max(0) as usize,
            );
        }
        true
    });
    crate::widgets::media_drag::style_order_drop_target(&drop_target, true, |_| true);
    target.as_ref().add_controller(drop_target);
}

struct PlaylistEntryGridCell {
    body: CollectionGridCardCell,
    shell: Rc<CatalogUi>,
    cover: crate::widgets::artwork::ArtworkTile,
    current: Rc<RefCell<Option<TrackRow>>>,
    playing: TrackRowPlayingIndicator,
}

impl PlaylistEntryGridCell {
    fn new(
        shell: Rc<CatalogUi>,
        fields: &[LibraryField],
        playlist: PlaylistKey,
        playing: TrackRowPlayingIndicator,
    ) -> Self {
        let cover = crate::widgets::artwork::ArtworkTile::new_elastic_square();
        let cover_frame = super::cards::square_cover_frame(&cover.widget(), None);
        let body = CollectionGridCardCell::new(&shell, fields, cover_frame.upcast());
        body.set_download_badge(shell.downloads.download_badge(true));
        let current = Rc::new(RefCell::new(None));
        let context_current = Rc::clone(&current);
        install_playlist_entry_context(
            &body.card,
            &shell,
            Rc::new(move || context_current.borrow().clone()),
        );
        let drag_current = Rc::clone(&current);
        install_playlist_entry_drag(
            &body.card,
            &shell,
            playlist,
            Rc::new(move || drag_current.borrow().clone()),
        );
        Self {
            body,
            shell,
            cover,
            current,
            playing,
        }
    }

    fn bind_current_artwork(&self) {
        let current = self.current.borrow();
        let Some(entry) = current.as_ref() else {
            return;
        };
        self.cover
            .widget()
            .remove_css_class("collection-grid-cover-skeleton");
        self.shell.artwork.bind_artwork_tile(
            &self.cover,
            opaque_artwork(entry.artwork_binding.as_deref()),
            COLLECTION_GRID_MAX_CARD_WIDTH,
        );
    }

    fn release_artwork(&self) {
        self.shell.artwork.clear_artwork_tile(&self.cover);
        self.cover
            .widget()
            .add_css_class("collection-grid-cover-skeleton");
        self.cover.widget().set_opacity(1.0);
    }
}

impl ReusableCollectionGridCell<TrackRow> for PlaylistEntryGridCell {
    fn widget(&self) -> gtk::Widget {
        self.body.widget()
    }

    fn bind(&self, position: u32, entry: TrackRow) {
        self.playing
            .bind(self.body.title.upcast_ref(), position, &entry.media_uri);
        self.body.bind(&entry.title, |field| {
            playlist_entry_field_links(&entry, field)
        });
        self.body.set_download_target(
            &self.shell,
            crate::downloads::media_download_badge(&entry.media_uri, entry.is_downloaded),
        );
        self.current.replace(Some(entry));
        self.bind_current_artwork();
    }

    fn clear(&self) {
        self.playing.unbind(self.body.title.upcast_ref());
        self.release_artwork();
        self.body.clear(&self.shell);
        self.current.take();
    }

    fn set_ready(&self, ready: bool) {
        self.body.set_ready(ready);
    }

    fn apply_fields(&self, fields: &[LibraryField]) {
        self.body.replace_fields(&self.shell, fields);
        if let Some(entry) = self.current.borrow().as_ref() {
            self.body.bind(&entry.title, |field| {
                playlist_entry_field_links(entry, field)
            });
        }
    }
}

fn playlist_entry_grid(
    shell: &Rc<CatalogUi>,
    model: Rows<Row>,
    playlist: PlaylistKey,
    selection: PlaylistEntrySelection,
    playing: TrackRowPlayingIndicator,
) -> CollectionGridProjection {
    let fields = shell
        .settings
        .current
        .borrow()
        .library_list(LibraryListKey::PlaylistTracks)
        .grid_fields;
    let cell_shell = Rc::clone(shell);
    let activate_model = model.clone();
    let products = shell.products.clone();
    collection_grid(
        model.list_model(),
        Some(selection.selection_model()),
        &fields,
        move |fields| {
            PlaylistEntryGridCell::new(Rc::clone(&cell_shell), fields, playlist, playing.clone())
        },
        move |position, _: TrackRow| {
            activate_model.play(
                &products,
                position as usize,
                rufin_core::playback::QueuePlacement::Now,
                false,
            )
        },
    )
}

fn playlist_entry_field_links(entry: &TrackRow, field: LibraryField) -> DetailLinks {
    rufin_core::route::detail_links::metadata_links(
        field,
        &track_row_field(&entry, field),
        entry.links.album_media_uri.as_deref(),
        &entry.links.artists,
        &entry.links.album_artists,
    )
}

fn playlist_entry_number(position: u32, ready: bool) -> String {
    ready
        .then(|| (position + 1).to_string())
        .unwrap_or_default()
}

fn playlist_entry_column_width(field: LibraryField) -> i32 {
    match field {
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
    fn playlist_placeholder_keeps_number_and_actions_inert_until_ready() {
        assert_eq!(playlist_entry_number(7, false), "");
        assert_eq!(playlist_entry_number(7, true), "8");
    }
}
