use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use library::{PlaylistEntryKey, PlaylistEntryRow, PlaylistKey};
use localization::msgid;

use crate::favorites::{
    FAVORITE_COLUMN_TITLE, column_favorite_icon_button, favorite_button_is_active,
    set_favorite_button_active, track_favorite_key,
};
use crate::interactions::install_context_menu_openers;
use crate::localization::{bind_search_placeholder, localized_column};
use crate::shell::Shell;
use crate::{LibraryField, LibraryLayout, LibraryListKey, LibraryListSettings};

use super::collection_context::present_playlist_entry_context_menu;
use super::collections::{
    CollectionTableProjection, LibraryCollectionProjection, LibraryPresentationProjection,
    dynamic_collection_table, library_route_inset,
};
use super::columns::{
    TrackRowPlayingIndicator, set_track_row_index_text, track_column_fit_width,
    track_row_index_cell,
};
use super::detail_links::DetailLinks;
use super::grid_cells::{
    CollectionGridCardCell, CollectionGridProjection, ReusableCollectionGridCell,
    collection_grid_with_selection_and_demand,
};
use super::library_fields::{
    COLLECTION_GRID_MAX_CARD_WIDTH, add_field_skeleton_class, count, display_unix_date,
    favorite_text, item_at_from_item, opaque_artwork, optional_track_number, optional_year,
    stored_rating,
};
use super::playlist_entry_model::{PlaylistEntryModel, PlaylistEntryProjectionRequest};
use super::playlist_picker::{
    MediaDragPreviewBinding, MediaDragSource, media_drag_content_provider,
};
use super::recycled_cells::{RecycledArtworkCell, RecycledMergedCell, RecycledTextCell, list_cell};
use super::route_shell::LibraryToolbarProjection;
use super::sparse_model::connect_sparse_bind;
use super::table_sizing::route_column_view_initial_width_with_inset;
use super::track_selection::PlaylistEntrySelection;

fn begin_order_request(generation: &std::cell::Cell<u64>) -> u64 {
    let next = generation.get().wrapping_add(1);
    generation.set(next);
    next
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
    order_generation: Rc<std::cell::Cell<u64>>,
}

impl PlaylistEntriesView {
    pub(crate) fn play(&self, queue: playback::QueueHandle, placement: playback::QueuePlacement) {
        self.model.play(0, queue, placement, true);
    }

    pub(crate) fn widget(&self) -> gtk::Widget {
        self.widget.clone()
    }

    pub(crate) fn item_navigation(&self) -> crate::shell::route::MountedRouteItemNavigation {
        self.collection.item_navigation()
    }

    pub(crate) fn search(&self) -> gtk::SearchEntry {
        self.search.clone()
    }

    pub(crate) fn layout_cycle(&self) -> crate::shell::route::MountedRouteCommand {
        self.toolbar.layout_cycle()
    }

    pub(crate) fn resume_initial_demand(&self) {
        self.model.resume_initial_demand();
    }

    pub(crate) fn update_downloaded(&self, media_uri: &str, downloaded: bool) {
        self.model.update_downloaded(media_uri, downloaded);
    }

    pub(crate) fn connect_search_request(
        &self,
        callback: impl Fn(u64, PlaylistEntryProjectionRequest) + 'static,
    ) {
        let model = self.model.clone();
        let generation = Rc::clone(&self.order_generation);
        self.search.connect_search_changed(move |search| {
            if model.set_query(&search.text()) {
                callback(begin_order_request(&generation), model.projection_request());
            }
        });
    }

    pub(crate) fn begin_order_request(&self) -> (u64, PlaylistEntryProjectionRequest) {
        (
            begin_order_request(&self.order_generation),
            self.model.projection_request(),
        )
    }

    pub(crate) fn replace_order(
        &self,
        generation: u64,
        order: Vec<library::PlaylistEntryKey>,
    ) -> bool {
        if self.order_generation.get() != generation {
            return false;
        }
        let empty = order.is_empty();
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
        playlist: PlaylistKey,
        playlist_name: String,
        order: Vec<library::PlaylistEntryKey>,
        first_row_position: usize,
        first_rows: Vec<PlaylistEntryRow>,
    ) -> PlaylistEntriesView {
        let settings = self
            .settings
            .current
            .borrow()
            .library_list(LibraryListKey::PlaylistTracks);
        let model = PlaylistEntryModel::new(
            self.products.library.clone(),
            self.products.runtime.clone(),
            playlist,
            order,
            first_row_position,
            first_rows,
            settings,
        );
        let wrapper = gtk::Box::new(gtk::Orientation::Vertical, 8);
        wrapper.set_hexpand(true);
        wrapper.set_width_request(1);

        let search = gtk::SearchEntry::new();
        bind_search_placeholder(&search, "Search");
        search.set_hexpand(true);
        search.set_width_request(1);
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
            order_generation: Rc::new(std::cell::Cell::default()),
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
    let current_model = model.clone();
    let current_playing = playing.clone();
    shell.register_current_route_track_selection(Rc::new(move |current| {
        let position = current
            .and_then(|current| {
                let context = current.context.as_ref()?;
                current_model.position_for_current(
                    &current.media_uri,
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
    let setup_shell = Rc::clone(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = track_row_index_cell("");
        let current = list_item_entry(item);
        install_playlist_entry_context(&cell, &setup_shell, playlist, Rc::clone(&current));
        install_playlist_entry_drag(&cell, &setup_shell, playlist, current);
        item.set_child(Some(&cell));
    });
    let bind_playing = playing.clone();
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let entry = item_at_from_item::<PlaylistEntryRow>(item);
        let Some(_entry) = entry else {
            return;
        };
        let Some(cell) = item.child().and_downcast::<gtk::Overlay>() else {
            return;
        };
        set_track_row_index_text(&cell, &playlist_entry_number(item.position(), true));
        bind_playing.bind(cell.upcast_ref(), item.position());
    });
    factory.connect_unbind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(cell) = item.child().and_downcast::<gtk::Overlay>() {
            set_track_row_index_text(&cell, "");
            playing.unbind(cell.upcast_ref());
        }
    });
    let column =
        gtk::ColumnViewColumn::new(Some(super::columns::ROW_INDEX_COLUMN_TITLE), Some(factory));
    column.set_fixed_width(width);
    column
}

fn playlist_entry_image_column(
    shell: &Rc<Shell>,
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
        install_playlist_entry_context(&cell, &setup_shell, playlist, Rc::clone(&current));
        install_playlist_entry_drag(&cell, &setup_shell, playlist, current);
        item.set_child(Some(&cell));
    });
    let bind_shell = Rc::clone(shell);
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let entry = item_at_from_item::<PlaylistEntryRow>(item);
        let Some(entry) = entry else {
            return;
        };
        let Some(cell) = list_cell::<RecycledArtworkCell>(item) else {
            return;
        };
        let cover = cell.artwork();
        bind_shell.bind_artwork_tile(
            &cover,
            opaque_artwork(entry.artwork_binding.as_deref()),
            width,
            crate::shell::cover::THUMB_COVER_SIZE,
        );
    });
    let clear_shell = Rc::clone(shell);
    factory.connect_unbind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(cell) = list_cell::<RecycledArtworkCell>(item) {
            clear_shell.clear_artwork_tile(&cell.artwork());
        }
    });
    let column = localized_column("Image", &factory);
    column.set_fixed_width(width);
    column
}

fn playlist_entry_favorite_column(
    shell: &Rc<Shell>,
    playlist: PlaylistKey,
    width: i32,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let setup_shell = Rc::clone(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let button = column_favorite_icon_button("Favorite track");
        let current = list_item_entry(item);
        install_playlist_entry_context(&button, &setup_shell, playlist, Rc::clone(&current));
        let favorite_current = Rc::clone(&current);
        setup_shell.register_dynamic_favorite_button(
            Rc::new(move || {
                favorite_current()
                    .as_ref()
                    .map(|entry| track_favorite_key(&entry.media_uri))
            }),
            &button,
        );
        let click_shell = Rc::clone(&setup_shell);
        let click_current = current;
        button.connect_clicked(move |button| {
            let Some(entry) = click_current() else {
                return;
            };
            click_shell.set_favorite_with_feedback(
                library::FavoriteTarget::Track(entry.media_uri.clone()),
                !favorite_button_is_active(button),
                Some(button),
            );
        });
        item.set_child(Some(&button));
    });
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let entry = item_at_from_item::<PlaylistEntryRow>(item);
        let Some(entry) = entry else {
            return;
        };
        let Some(button) = item.child().and_downcast::<gtk::Button>() else {
            return;
        };
        set_favorite_button_active(&button, entry.favorite);
        button.set_sensitive(true);
    });
    factory.connect_unbind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(button) = item.child().and_downcast::<gtk::Button>() {
            set_favorite_button_active(&button, false);
            button.set_sensitive(false);
        }
    });
    let column = gtk::ColumnViewColumn::new(Some(FAVORITE_COLUMN_TITLE), Some(factory));
    column.set_fixed_width(width);
    column
}

fn playlist_entry_title_column(
    shell: &Rc<Shell>,
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
        let cell = RecycledMergedCell::new(&setup_shell, 36, true);
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
        install_playlist_entry_context(&cell, &setup_shell, playlist, Rc::clone(&current));
        install_playlist_entry_drag(&cell, &setup_shell, playlist, current);
        item.set_child(Some(&cell));
    });
    let bind_shell = Rc::clone(shell);
    let bind_playing = playing.clone();
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let entry = item_at_from_item::<PlaylistEntryRow>(item);
        let Some(entry) = entry else {
            return;
        };
        let Some(cell) = list_cell::<RecycledMergedCell>(item) else {
            return;
        };
        let cover = cell.cover();
        let title = cell.title();
        bind_shell.bind_artwork_tile(
            &cover,
            opaque_artwork(entry.artwork_binding.as_deref()),
            36,
            crate::shell::cover::THUMB_COVER_SIZE,
        );
        title.set_text(&entry.title);
        cell.bind_subtitle(playlist_entry_field_links(&entry, LibraryField::Artist));
        cell.subtitle().set_visible(true);
        bind_shell.bind_download_badge(
            &cell.downloaded().expect("playlist title badge"),
            entry.is_downloaded,
        );
        bind_playing.bind(title.upcast_ref(), item.position());
    });
    let clear_shell = Rc::clone(shell);
    factory.connect_unbind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(cell) = list_cell::<RecycledMergedCell>(item) {
            let title = cell.title();
            clear_shell.clear_artwork_tile(&cell.cover());
            clear_shell.clear_download_badge(&cell.downloaded().expect("playlist title badge"));
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
    shell: &Rc<Shell>,
    field: LibraryField,
    width: i32,
    playlist: PlaylistKey,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let setup_shell = Rc::clone(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = RecycledTextCell::new();
        let label = cell.label();
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
            cell.enable_links(&setup_shell);
        }
        let current = list_item_entry(item);
        install_playlist_entry_context(&cell, &setup_shell, playlist, Rc::clone(&current));
        install_playlist_entry_drag(&cell, &setup_shell, playlist, current);
        item.set_child(Some(&cell));
    });
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let entry = item_at_from_item::<PlaylistEntryRow>(item);
        let Some(entry) = entry else {
            return;
        };
        let Some(cell) = list_cell::<RecycledTextCell>(item) else {
            return;
        };
        let label = cell.label();
        if matches!(
            field,
            LibraryField::Album | LibraryField::Artist | LibraryField::AlbumArtist
        ) {
            cell.bind_links(playlist_entry_field_links(&entry, field));
        } else {
            label.set_text(&playlist_entry_field(&entry, field));
        }
    });
    factory.connect_unbind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(cell) = list_cell::<RecycledTextCell>(item) {
            cell.clear();
        }
    });
    let column = localized_column(super::columns::track_column_title(field), &factory);
    column.set_fixed_width(width);
    column
}

fn list_item_entry(item: &gtk::ListItem) -> Rc<dyn Fn() -> Option<PlaylistEntryRow>> {
    let item = item.downgrade();
    Rc::new(move || {
        item.upgrade()
            .and_then(|item| item_at_from_item::<PlaylistEntryRow>(&item))
    })
}

fn install_playlist_entry_context(
    target: &impl IsA<gtk::Widget>,
    shell: &Rc<Shell>,
    playlist: PlaylistKey,
    current: Rc<dyn Fn() -> Option<PlaylistEntryRow>>,
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
            present_playlist_entry_context_menu(
                target,
                &shell,
                playlist,
                entry.playlist_entry_key,
                entry,
                position,
            );
        }),
    );
}

fn install_playlist_entry_drag(
    target: &impl IsA<gtk::Widget>,
    shell: &Rc<Shell>,
    playlist: PlaylistKey,
    current: Rc<dyn Fn() -> Option<PlaylistEntryRow>>,
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
        let selection = shell
            .current_playlist_entry_selection(entry.playlist_entry_key)
            .or_else(|| {
                shell
                    .current_playlist_entry_selection_owner()
                    .map(|selection| selection.single_entry(entry.playlist_entry_key))
            })?;
        let mut providers = Vec::new();
        if selection.entries.len() == 1 {
            providers.push(gtk::gdk::ContentProvider::for_value(
                &entry.playlist_entry_key.raw().to_value(),
            ));
        }
        providers.push(media_drag_content_provider(
            MediaDragSource::playlist_entries(selection),
        ));
        drag_preview.prepare_cache_only(&shell, title, artwork);
        match providers.as_slice() {
            [] => None,
            [provider] => Some(provider.clone()),
            providers => Some(gtk::gdk::ContentProvider::new_union(providers)),
        }
    });
    target.as_ref().add_controller(drag_source);

    let move_shell = Rc::clone(shell);
    let drop_target = gtk::DropTarget::new(i64::static_type(), gtk::gdk::DragAction::MOVE);
    drop_target.connect_drop(move |_, value, _, _| {
        let Ok(entry) = value.get::<i64>() else {
            return false;
        };
        let Some(target) = current() else {
            return false;
        };
        let entry = PlaylistEntryKey::from_raw(entry);
        if entry == target.playlist_entry_key {
            return false;
        }
        move_shell.move_media_in_playlist(
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
        let context_current = Rc::clone(&current);
        install_playlist_entry_context(
            &body.card,
            &shell,
            playlist,
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
        self.shell.bind_artwork_tile(
            &self.cover,
            opaque_artwork(entry.artwork_binding.as_deref()),
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
        self.body.bind(&entry.title, |field| {
            playlist_entry_field_links(&entry, field)
        });
        self.body
            .set_download_target(&self.shell, entry.is_downloaded);
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
        if let Some(entry) = self.current.borrow().as_ref() {
            self.body.bind(&entry.title, |field| {
                playlist_entry_field_links(entry, field)
            });
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

fn playlist_entry_field(entry: &PlaylistEntryRow, field: LibraryField) -> String {
    match field {
        LibraryField::Title | LibraryField::TitleMerged => entry.title.clone(),
        LibraryField::Artist => entry.artist.clone(),
        LibraryField::AlbumArtist => entry
            .album_display_artist
            .clone()
            .unwrap_or_else(|| entry.artist.clone()),
        LibraryField::Album => entry.album.clone(),
        LibraryField::Year => optional_year(entry.year),
        LibraryField::ReleaseDate => entry.release_date.clone().unwrap_or_default(),
        LibraryField::DateAdded => entry.date_added.clone().unwrap_or_default(),
        LibraryField::LastPlayed => display_unix_date(entry.last_played),
        LibraryField::PlayCount => count(entry.play_count),
        LibraryField::Genre => entry.genre.clone(),
        LibraryField::Bpm => entry.bpm.map(|value| value.to_string()).unwrap_or_default(),
        LibraryField::UserRating => stored_rating(entry.rating),
        LibraryField::DiscNumber => entry
            .disc_number
            .map(|number| number.to_string())
            .unwrap_or_default(),
        LibraryField::TrackNumber => optional_track_number(entry.disc_number, entry.track_number),
        LibraryField::Duration => {
            crate::format_duration((entry.duration_millis.max(0) / 1_000) as u32)
        }
        LibraryField::Favorite => favorite_text(entry.favorite),
        _ => String::new(),
    }
}

fn playlist_entry_field_links(entry: &PlaylistEntryRow, field: LibraryField) -> DetailLinks {
    super::detail_links::metadata_links(
        field,
        &playlist_entry_field(entry, field),
        entry.album_media_uri.as_deref(),
        &entry.artists,
        &entry.album_artists,
    )
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
        let lane = std::cell::Cell::default();
        let stale = begin_order_request(&lane);
        let current = begin_order_request(&lane);
        assert_ne!(lane.get(), stale);
        assert_eq!(lane.get(), current);
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

    #[test]
    fn playlist_utility_columns_use_the_shared_detail_widths() {
        assert_eq!(playlist_entry_column_width(LibraryField::RowIndex), 48);
        assert_eq!(playlist_entry_column_width(LibraryField::Duration), 48);
        assert_eq!(playlist_entry_column_width(LibraryField::Favorite), 32);
    }
}
