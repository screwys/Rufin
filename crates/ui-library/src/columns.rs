use std::{cell::RefCell, collections::HashMap, rc::Rc};
use ui_shared::media_drag::{MediaDragSource, install_media_drag_source};
use ui_shared::sparse_model::{item_at_from_item, object_item};

use ::library::{AlbumRow, ArtistRow, PlaylistRow, SmartPlaylistRow};
use adw::prelude::*;
use artwork::ArtworkBinding;
use gtk::glib;

use super::collection_context::present_track_context_menu;
use crate::CatalogUi;
use crate::{LibraryField, LibraryListKey};
use ui_shared::artwork::{ArtworkTile, THUMB_COVER_SIZE};
use ui_shared::favorites::{
    FAVORITE_COLUMN_WIDTH, album_favorite_key, artist_favorite_key, favorite_button_is_active,
    row_favorite_icon_button, set_favorite_button_active, track_favorite_key,
};
use ui_shared::interactions::install_context_menu_openers;
use ui_shared::localization::localized_column;
use ui_shared::media_menus::{present_album_context_menu, present_artist_context_menu};

use super::collections::{install_playlist_reorder, install_smart_playlist_reorder};
use rufin_core::playback::PlaybackTarget;

use super::table_links::mapped_track_link_column;
use ui_shared::detail_links::{DetailLinks, album_artist_links};
use ui_shared::library_fields::{
    add_field_skeleton_class, album_field, artist_field, column_width, opaque_artwork,
    play_count_column_width, playlist_artwork, playlist_field, smart_playlist_display_name,
    smart_playlist_field,
};
use ui_shared::recycled_cells::{
    RecycledArtworkCell, RecycledBadgedTextCell, RecycledMergedCell, RecycledTextCell, list_cell,
};
use ui_shared::sparse_model::connect_sparse_bind;

pub const ROW_INDEX_COLUMN_TITLE: &str = "\u{2003}#";
const DETAIL_TRACK_UTILITY_COLUMN_WIDTH: i32 = 48;

pub fn bind_collection_title<T: Clone + 'static>(
    factory: &gtk::SignalListItemFactory,
    shell: &Rc<CatalogUi>,
    target: impl Fn(&T) -> PlaybackTarget + 'static,
) {
    let bindings = Rc::new(RefCell::new(HashMap::<
        usize,
        Rc<super::grid_cells::MediaPlayingBinding>,
    >::new()));
    let bind_cells = Rc::clone(&bindings);
    let shell = Rc::clone(shell);
    connect_sparse_bind(factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = list_cell::<RecycledBadgedTextCell>(item) else {
            return;
        };
        let binding = bind_cells
            .borrow_mut()
            .entry(item.as_ptr() as usize)
            .or_insert_with(|| super::grid_cells::MediaPlayingBinding::new(&shell, &cell.label()))
            .clone();
        let target = item
            .item()
            .and_then(|object| object_item::<T, _>(object, &target));
        binding.bind(&shell, target);
    });
    factory.connect_unbind(move |_, item| {
        if let Some(binding) = bindings.borrow_mut().remove(&(item.as_ptr() as usize)) {
            binding.refresh(None);
        }
    });
}

mod collection_index_cell_imp {
    use super::*;
    use gtk::subclass::prelude::*;

    #[derive(Default)]
    pub struct CollectionIndexCell {
        pub(super) overlay: std::cell::OnceCell<gtk::Overlay>,
        pub(super) playing: std::cell::OnceCell<Rc<super::super::grid_cells::MediaPlayingBinding>>,
        pub(super) target: RefCell<Option<PlaybackTarget>>,
        pub(super) shell: RefCell<std::rc::Weak<CatalogUi>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CollectionIndexCell {
        const NAME: &'static str = "RufinCollectionIndexCell";
        type Type = super::CollectionIndexCell;
        type ParentType = gtk::Widget;

        fn class_init(class: &mut Self::Class) {
            class.set_layout_manager_type::<gtk::BinLayout>();
        }
    }

    impl ObjectImpl for CollectionIndexCell {
        fn dispose(&self) {
            if let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for CollectionIndexCell {}
}

glib::wrapper! {
    pub struct CollectionIndexCell(ObjectSubclass<collection_index_cell_imp::CollectionIndexCell>)
        @extends gtk::Widget, @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl CollectionIndexCell {
    fn new(shell: &Rc<CatalogUi>) -> Self {
        use gtk::subclass::prelude::ObjectSubclassIsExt;
        let cell: Self = glib::Object::new();
        cell.imp().shell.replace(Rc::downgrade(shell));
        let weak = cell.downgrade();
        let overlay = track_row_index_cell("", move |_| {
            let Some(cell) = weak.upgrade() else { return };
            let target = cell.imp().target.borrow().clone();
            if let (Some(shell), Some(target)) = (cell.imp().shell.borrow().upgrade(), target) {
                (shell.media_menus.play_target)(
                    &target,
                    library::QueuePlacement::Replace { anchor_index: 0 },
                    false,
                );
            }
        });
        let binding = super::grid_cells::MediaPlayingBinding::new(shell, &overlay);
        cell.imp().playing.set(binding).ok().unwrap();
        overlay.set_parent(&cell);
        cell.imp().overlay.set(overlay).unwrap();
        cell
    }

    fn bind(&self, position: u32, target: Option<PlaybackTarget>) {
        use gtk::subclass::prelude::ObjectSubclassIsExt;
        let imp = self.imp();
        set_track_row_index_text(
            imp.overlay.get().unwrap(),
            &target
                .as_ref()
                .map_or_else(String::new, |_| (position + 1).to_string()),
        );
        imp.target.replace(target.clone());
        if let Some(shell) = imp.shell.borrow().upgrade() {
            imp.playing.get().unwrap().bind(&shell, target);
        }
    }
}

fn collection_is_downloaded(track_count: i64, downloaded_count: i64) -> bool {
    track_count > 0 && downloaded_count == track_count
}

fn set_cover_placeholder(shell: &Rc<CatalogUi>, cover: &ArtworkTile, placeholder: bool) {
    if placeholder {
        shell.artwork.clear_artwork_tile(cover);
        cover
            .widget()
            .add_css_class("collection-grid-cover-skeleton");
        cover.widget().set_opacity(1.0);
    } else {
        cover
            .widget()
            .remove_css_class("collection-grid-cover-skeleton");
    }
    cover.widget().set_sensitive(!placeholder);
}

fn clear_merged_artwork(shell: &Rc<CatalogUi>, cover: &ArtworkTile) {
    shell.artwork.clear_artwork_tile(cover);
    cover
        .widget()
        .remove_css_class("collection-grid-cover-skeleton");
}

fn set_placeholder_favorite(button: &gtk::Button, favorite: Option<bool>) {
    set_favorite_button_active(button, favorite.unwrap_or(false));
    button.set_visible(favorite.is_some());
    button.set_sensitive(favorite.is_some());
}

pub fn album_column(
    shell: &Rc<CatalogUi>,
    field: LibraryField,
    playback_context: Option<String>,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let shell = Rc::clone(shell);
    factory.connect_setup(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            item.set_child(Some(&AlbumTableCell::new(
                &shell,
                field,
                playback_context.clone(),
            )));
        }
    });
    connect_sparse_bind(&factory, |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(cell) = list_cell::<AlbumTableCell>(item) {
            let row = item
                .item()
                .and_downcast::<ui_shared::sparse_model::SparseObjectItem>()
                .and_then(|item| item.value::<std::sync::Arc<AlbumRow>>());
            cell.bind(item.position(), row);
        }
    });
    factory.connect_unbind(|_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>()
            && let Some(cell) = list_cell::<AlbumTableCell>(item)
        {
            cell.bind(0, None);
        }
    });
    if field == LibraryField::Tools {
        return ui_shared::recycled_cells::row_actions_column(&factory);
    }
    let title = if field == LibraryField::RowIndex {
        ROW_INDEX_COLUMN_TITLE
    } else if field == LibraryField::TitleMerged {
        "Title"
    } else {
        ui_shared::settings::library_field_title(field)
    };
    let column = localized_column(title, &factory);
    column.set_fixed_width(column_width(field));
    column
}

pub(super) enum AlbumCellView {
    Index(CollectionIndexCell),
    Image(RecycledArtworkCell),
    Text(RecycledBadgedTextCell),
    Merged(RecycledMergedCell),
    Tools(ui_shared::recycled_cells::RowActions),
}

impl AlbumCellView {
    fn widget(&self) -> &gtk::Widget {
        match self {
            Self::Index(cell) => cell.upcast_ref(),
            Self::Image(cell) => cell.upcast_ref(),
            Self::Text(cell) => cell.upcast_ref(),
            Self::Merged(cell) => cell.upcast_ref(),
            Self::Tools(cell) => cell.upcast_ref(),
        }
    }
}

mod album_table_cell_imp {
    use super::*;
    use gtk::subclass::prelude::*;
    #[derive(Default)]
    pub struct AlbumTableCell {
        pub(super) view: RefCell<Option<AlbumCellView>>,
        pub(super) row: RefCell<Option<std::sync::Arc<AlbumRow>>>,
        pub(super) field: std::cell::Cell<Option<LibraryField>>,
        pub(super) shell: RefCell<std::rc::Weak<CatalogUi>>,
        pub(super) playback_context: RefCell<Option<String>>,
        pub(super) title_playing:
            RefCell<Option<Rc<super::super::grid_cells::MediaPlayingBinding>>>,
    }
    #[glib::object_subclass]
    impl ObjectSubclass for AlbumTableCell {
        const NAME: &'static str = "RufinAlbumTableCell";
        type Type = super::AlbumTableCell;
        type ParentType = gtk::Widget;
        fn class_init(class: &mut Self::Class) {
            class.set_layout_manager_type::<gtk::BinLayout>();
        }
    }
    impl ObjectImpl for AlbumTableCell {
        fn dispose(&self) {
            if let Some(view) = self.view.take() {
                view.widget().unparent();
            }
            self.row.take();
        }
    }
    impl WidgetImpl for AlbumTableCell {}
}

glib::wrapper! {
    pub struct AlbumTableCell(ObjectSubclass<album_table_cell_imp::AlbumTableCell>)
        @extends gtk::Widget, @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl AlbumTableCell {
    pub fn new(
        shell: &Rc<CatalogUi>,
        field: LibraryField,
        playback_context: Option<String>,
    ) -> Self {
        use gtk::subclass::prelude::ObjectSubclassIsExt;
        let cell: Self = glib::Object::new();
        cell.imp().field.set(Some(field));
        cell.imp().shell.replace(Rc::downgrade(shell));
        cell.imp()
            .playback_context
            .replace(playback_context.clone());
        let view = match field {
            LibraryField::RowIndex => AlbumCellView::Index(CollectionIndexCell::new(shell)),
            LibraryField::Image => AlbumCellView::Image(RecycledArtworkCell::new(48)),
            LibraryField::TitleMerged => {
                let view =
                    RecycledMergedCell::new(shell.route_navigation(), &shell.downloads, 48, true);
                view.subtitle().add_css_class("artist-label");
                view.subtitle().set_visible(false);
                AlbumCellView::Merged(view)
            }
            LibraryField::Tools => {
                let button = row_favorite_icon_button("Favorite album");
                let weak = cell.downgrade();
                shell.register_dynamic_favorite_button(
                    Rc::new(move || {
                        weak.upgrade()?
                            .imp()
                            .row
                            .borrow()
                            .as_ref()
                            .map(|row| album_favorite_key(&row.media_uri))
                    }),
                    &button,
                );
                let weak = cell.downgrade();
                button.connect_clicked(move |button| {
                    let Some(cell) = weak.upgrade() else {
                        return;
                    };
                    let row = cell.imp().row.borrow().clone();
                    if let (Some(shell), Some(row)) = (cell.imp().shell.borrow().upgrade(), row) {
                        shell.set_favorite_with_feedback(
                            library::FavoriteTarget::Album(row.media_uri.clone()),
                            !favorite_button_is_active(button),
                            Some(button),
                        );
                    }
                });
                AlbumCellView::Tools(ui_shared::recycled_cells::RowActions::with_favorite(
                    &button,
                ))
            }
            _ => {
                let view = RecycledBadgedTextCell::with_downloads(&shell.downloads);
                add_field_skeleton_class(&view, field);
                AlbumCellView::Text(view)
            }
        };
        if field != LibraryField::RowIndex {
            let weak = cell.downgrade();
            let context = playback_context.clone();
            install_media_drag_source(view.widget(), move || {
                let cell = weak.upgrade()?;
                let shell = cell.imp().shell.borrow().upgrade()?;
                let row = cell.imp().row.borrow().clone()?;
                let target = PlaybackTarget::Album(row.media_uri.clone());
                let target = context
                    .as_ref()
                    .map_or(target.clone(), |context| target.in_context(context));
                Some((
                    MediaDragSource::capture_target(shell.selected_library().as_deref(), target),
                    row.title.clone(),
                ))
            });
            let weak = cell.downgrade();
            install_context_menu_openers(
                view.widget(),
                Rc::new(move |target, position| {
                    let Some(cell) = weak.upgrade() else {
                        return;
                    };
                    let row = cell.imp().row.borrow().clone();
                    if let (Some(shell), Some(row)) = (cell.imp().shell.borrow().upgrade(), row) {
                        present_album_context_menu(
                            target,
                            &shell.media_menus,
                            (*row).clone(),
                            playback_context.clone(),
                            None,
                            position,
                        );
                    }
                }),
            );
        }
        let title = match &view {
            AlbumCellView::Merged(view) => Some(view.title()),
            AlbumCellView::Text(view) if field == LibraryField::Title => Some(view.label()),
            _ => None,
        };
        if let Some(title) = title {
            cell.imp()
                .title_playing
                .replace(Some(super::grid_cells::MediaPlayingBinding::new(
                    shell, &title,
                )));
        }
        view.widget().set_parent(&cell);
        cell.imp().view.replace(Some(view));
        cell.bind(0, None);
        cell
    }

    pub fn bind(&self, position: u32, row: Option<std::sync::Arc<AlbumRow>>) {
        use gtk::subclass::prelude::ObjectSubclassIsExt;
        let imp = self.imp();
        imp.row.replace(row.clone());
        let Some(shell) = imp.shell.borrow().upgrade() else {
            return;
        };
        let view = imp.view.borrow();
        let target = row.as_ref().map(|row| {
            let target = PlaybackTarget::Album(row.media_uri.clone());
            imp.playback_context
                .borrow()
                .as_ref()
                .map_or(target.clone(), |context| target.in_context(context))
        });
        if let Some(playing) = imp.title_playing.borrow().as_ref() {
            playing.bind(&shell, target.clone());
        }
        match view.as_ref().unwrap() {
            AlbumCellView::Index(cell) => cell.bind(position, target),
            AlbumCellView::Image(cell) => {
                let cover = cell.artwork();
                set_cover_placeholder(&shell, &cover, row.is_none());
                if let Some(row) = row {
                    shell.artwork.bind_artwork_tile(
                        &cover,
                        opaque_artwork(row.artwork_binding.as_deref()),
                        48,
                        THUMB_COVER_SIZE,
                    );
                }
            }
            AlbumCellView::Text(cell) => {
                if let Some(row) = row {
                    cell.label()
                        .set_text(&album_field(&row, imp.field.get().unwrap()));
                    shell.bind_download_badge(
                        &cell.downloaded(),
                        collection_is_downloaded(row.track_count, row.downloaded_count),
                    );
                } else {
                    cell.clear();
                }
            }
            AlbumCellView::Merged(cell) => {
                let cover = cell.cover();
                if let Some(row) = row {
                    set_cover_placeholder(&shell, &cover, false);
                    shell.artwork.bind_artwork_tile(
                        &cover,
                        opaque_artwork(row.artwork_binding.as_deref()),
                        48,
                        THUMB_COVER_SIZE,
                    );
                    cell.title().set_text(&row.title);
                    cell.bind_subtitle(album_artist_links(&row));
                    cell.subtitle()
                        .set_visible(!row.display_artist.trim().is_empty());
                    shell.bind_download_badge(
                        &cell.downloaded().unwrap(),
                        collection_is_downloaded(row.track_count, row.downloaded_count),
                    );
                } else {
                    cell.title().set_text("");
                    cell.downloaded().unwrap().set_visible(false);
                    cell.clear_subtitle();
                    clear_merged_artwork(&shell, &cover);
                }
            }
            AlbumCellView::Tools(cell) => set_placeholder_favorite(
                &cell.favorite().unwrap(),
                row.as_ref().map(|row| row.favorite),
            ),
        }
    }
}

pub fn artist_column(
    shell: &Rc<CatalogUi>,
    field: LibraryField,
    album_artist: bool,
) -> gtk::ColumnViewColumn {
    match field {
        LibraryField::RowIndex => {
            mapped_row_index_column::<ArtistRow>(shell, column_width(field), move |row| {
                if album_artist {
                    PlaybackTarget::AlbumArtist(row.media_uri.clone())
                } else {
                    PlaybackTarget::Artist(row.media_uri.clone())
                }
            })
        }
        LibraryField::Image => artist_image_column(shell, album_artist),
        LibraryField::TitleMerged | LibraryField::Title => {
            artist_text_column(shell, field, "Title", 220, album_artist, |artist| {
                artist.name.clone()
            })
        }
        LibraryField::Tools => artist_favorite_column(shell, album_artist),
        _ => artist_text_column(
            shell,
            field,
            ui_shared::settings::library_field_title(field),
            column_width(field),
            album_artist,
            move |artist| artist_field(artist, field),
        ),
    }
}
pub fn playlist_column(shell: &Rc<CatalogUi>, field: LibraryField) -> gtk::ColumnViewColumn {
    match field {
        LibraryField::Tools => super::named_collections::named_actions_column::<PlaylistRow>(shell),
        LibraryField::RowIndex => mapped_row_index_column::<PlaylistRow>(
            shell,
            column_width(field),
            super::named_collections::NamedCollectionRow::playback,
        ),
        LibraryField::Image => {
            let settings_shell = Rc::clone(shell);
            artwork_column::<PlaylistRow, _>(
                shell,
                "Image",
                column_width(LibraryField::Image),
                move |playlist| {
                    let prefer_server_cover = settings_shell
                        .settings
                        .current
                        .borrow()
                        .prefer_server_playlist_covers;
                    playlist_artwork(playlist, prefer_server_cover)
                        .into_iter()
                        .next()
                        .unwrap_or_default()
                },
                Some(|playlist| {
                    (
                        PlaybackTarget::Playlist(playlist.playlist_key),
                        playlist.name,
                    )
                }),
            )
        }
        LibraryField::Title | LibraryField::TitleMerged => {
            playlist_title_column(shell, "Title", 220, |playlist| playlist.name.clone())
        }
        _ => text_column::<PlaylistRow, _>(field, column_width(field), move |playlist| {
            playlist_field(playlist, field)
        }),
    }
}

fn playlist_title_column<F>(
    shell: &Rc<CatalogUi>,
    title: &str,
    width: i32,
    value: F,
) -> gtk::ColumnViewColumn
where
    F: Fn(&PlaylistRow) -> String + 'static,
{
    let factory = gtk::SignalListItemFactory::new();
    bind_collection_title::<PlaylistRow>(
        &factory,
        shell,
        super::named_collections::NamedCollectionRow::playback,
    );
    let value = Rc::new(value);
    let setup_shell = Rc::clone(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = RecycledBadgedTextCell::with_downloads(&setup_shell.downloads);
        item.set_child(Some(&cell));
        let weak_item = item.downgrade();
        install_playlist_reorder(
            &cell,
            &setup_shell,
            Rc::new(move || {
                weak_item
                    .upgrade()
                    .and_then(|item| item_at_from_item::<PlaylistRow>(&item))
            }),
        );
        install_collection_cell_drag(
            &cell,
            &setup_shell,
            item,
            Rc::new(|playlist: PlaylistRow| {
                (
                    PlaybackTarget::Playlist(playlist.playlist_key),
                    playlist.name,
                )
            }),
        );
    });
    let bind_shell = Rc::clone(shell);
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = list_cell::<RecycledBadgedTextCell>(item) else {
            return;
        };
        let Some((text, downloaded)) = item.item().and_then(|object| {
            object_item::<PlaylistRow, _>(object, |row| {
                (
                    value(row),
                    collection_is_downloaded(row.track_count, row.downloaded_count),
                )
            })
        }) else {
            cell.clear();
            return;
        };
        cell.label().set_text(&text);
        bind_shell.bind_download_badge(&cell.downloaded(), downloaded);
    });
    factory.connect_unbind(|_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>()
            && let Some(cell) = list_cell::<RecycledBadgedTextCell>(item)
        {
            cell.clear();
        }
    });
    let column = localized_column(title, &factory);
    column.set_fixed_width(width);
    column
}

pub fn smart_playlist_column(shell: &Rc<CatalogUi>, field: LibraryField) -> gtk::ColumnViewColumn {
    match field {
        LibraryField::Tools => {
            super::named_collections::named_actions_column::<SmartPlaylistRow>(shell)
        }
        LibraryField::RowIndex => mapped_row_index_column::<SmartPlaylistRow>(
            shell,
            column_width(field),
            super::named_collections::NamedCollectionRow::playback,
        ),
        LibraryField::Image => artwork_column::<SmartPlaylistRow, _>(
            shell,
            "Image",
            column_width(LibraryField::Image),
            |playlist| {
                playlist
                    .artwork_bindings
                    .first()
                    .map(|binding| ArtworkBinding::opaque(binding))
                    .unwrap_or_default()
            },
            Some(|playlist| {
                let name = smart_playlist_display_name(&playlist);
                (
                    PlaybackTarget::SmartPlaylist(playlist.smart_playlist_key),
                    name,
                )
            }),
        ),
        LibraryField::Title | LibraryField::TitleMerged => {
            smart_playlist_title_column(shell, "Title", 220, |playlist| {
                smart_playlist_display_name(&playlist)
            })
        }
        _ => text_column::<SmartPlaylistRow, _>(field, column_width(field), move |playlist| {
            smart_playlist_field(playlist, field)
        }),
    }
}

fn smart_playlist_title_column<F>(
    shell: &Rc<CatalogUi>,
    title: &str,
    width: i32,
    value: F,
) -> gtk::ColumnViewColumn
where
    F: Fn(&SmartPlaylistRow) -> String + 'static,
{
    let factory = gtk::SignalListItemFactory::new();
    bind_collection_title::<SmartPlaylistRow>(
        &factory,
        shell,
        super::named_collections::NamedCollectionRow::playback,
    );
    let value = Rc::new(value);
    let setup_shell = Rc::clone(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = RecycledBadgedTextCell::with_downloads(&setup_shell.downloads);
        item.set_child(Some(&cell));
        let weak_item = item.downgrade();
        install_smart_playlist_reorder(
            &cell,
            &setup_shell,
            Rc::new(move || {
                weak_item
                    .upgrade()
                    .and_then(|item| item_at_from_item::<SmartPlaylistRow>(&item))
            }),
        );
        install_collection_cell_drag(
            &cell,
            &setup_shell,
            item,
            Rc::new(|playlist: SmartPlaylistRow| {
                let name = smart_playlist_display_name(&playlist);
                (
                    PlaybackTarget::SmartPlaylist(playlist.smart_playlist_key),
                    name,
                )
            }),
        );
    });
    let bind_shell = Rc::clone(shell);
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = list_cell::<RecycledBadgedTextCell>(item) else {
            return;
        };
        let Some((text, downloaded)) = item.item().and_then(|object| {
            object_item::<SmartPlaylistRow, _>(object, |row| {
                (
                    value(row),
                    collection_is_downloaded(row.track_count, row.downloaded_count),
                )
            })
        }) else {
            cell.clear();
            return;
        };
        cell.label().set_text(&text);
        bind_shell.bind_download_badge(&cell.downloaded(), downloaded);
    });
    factory.connect_unbind(|_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>()
            && let Some(cell) = list_cell::<RecycledBadgedTextCell>(item)
        {
            cell.clear();
        }
    });
    let column = localized_column(title, &factory);
    column.set_fixed_width(width);
    column
}

pub fn song_column_for_key<T: ui_shared::library_fields::TrackPresentation>(
    shell: &Rc<CatalogUi>,
    key: LibraryListKey,
    field: LibraryField,
    playing: &TrackRowPlayingIndicator,
) -> gtk::ColumnViewColumn {
    let width = track_column_width(key, field);
    match field {
        LibraryField::RowIndex => mapped_track_row_index_column_with_width::<T, _>(
            width,
            playing.clone(),
            |item| Some(item.media_uri()),
            move |position, item| {
                Some(if key == LibraryListKey::AlbumDetailTracks {
                    item.track_number()
                        .filter(|number| *number > 0)
                        .map(|number| number.to_string())
                        .unwrap_or_else(|| (position + 1).to_string())
                } else {
                    (position + 1).to_string()
                })
            },
        ),
        LibraryField::Image => mapped_track_image_column::<T, _, _>(
            shell,
            "Image",
            width,
            |item| Some(item.media_uri().to_string()),
            |item| opaque_artwork(item.artwork()),
        ),
        LibraryField::TitleMerged => track_merged_column(
            shell,
            "Title",
            width,
            playing.clone(),
            TrackMergedColumnValues {
                track: |item: &T| Some(item.media_uri().to_string()),
                downloaded: |item: &T| item.download_badge(),
                artwork: |item: &T| opaque_artwork(item.artwork()),
                title: |item: &T| item.title().to_string(),
                subtitle: |item: &T| item.artist().to_string(),
                subtitle_links: |item: &T| Some(item.links(LibraryField::Artist)),
                context_menu: true,
            },
        ),
        LibraryField::Title => {
            let column = mapped_track_position_text_column(
                shell,
                field,
                "Title",
                width,
                0.0,
                Some(playing.clone()),
                |item: &T| Some(item.media_uri().to_string()),
                |item: &T| item.download_badge(),
                |_, item: &T| item.title().to_string(),
            );
            if matches!(
                key,
                LibraryListKey::PlaylistTracks | LibraryListKey::SmartPlaylistTracks
            ) && let Some(factory) = column
                .factory()
                .and_then(|factory| factory.downcast::<gtk::SignalListItemFactory>().ok())
            {
                factory.connect_setup(|_, item| {
                    if let Some(label) = item
                        .downcast_ref::<gtk::ListItem>()
                        .and_then(gtk::ListItem::child)
                    {
                        label.add_css_class("playlist-entry-title");
                    }
                });
            }
            column
        }
        LibraryField::Tools => mapped_track_favorite_column(
            shell,
            |item: &T| Some(item.media_uri().to_string()),
            |item: &T| Some((item.media_uri().to_string(), item.favorite())),
        ),
        LibraryField::Artist => mapped_track_link_column(
            shell,
            "Artist",
            width,
            |item: &T| Some(item.media_uri().to_string()),
            |item: &T| item.links(LibraryField::Artist),
        ),
        LibraryField::AlbumArtist => mapped_track_link_column(
            shell,
            ui_shared::settings::library_field_title(LibraryField::AlbumArtist),
            width,
            |item: &T| Some(item.media_uri().to_string()),
            |item: &T| item.links(LibraryField::AlbumArtist),
        ),
        LibraryField::Album => mapped_track_link_column(
            shell,
            "Album",
            width,
            |item: &T| Some(item.media_uri().to_string()),
            |item: &T| item.links(LibraryField::Album),
        ),
        _ => mapped_track_position_text_column(
            shell,
            field,
            track_column_title(field),
            width,
            0.0,
            None,
            |item: &T| Some(item.media_uri().to_string()),
            |item: &T| item.download_badge(),
            move |_, item: &T| {
                if key == LibraryListKey::AlbumDetailTracks && field == LibraryField::TrackNumber {
                    item.track_number()
                        .map(|number| number.to_string())
                        .unwrap_or_default()
                } else {
                    item.field(field)
                }
            },
        ),
    }
}

pub fn track_column_title(field: LibraryField) -> &'static str {
    if field == LibraryField::Duration {
        "◷"
    } else {
        ui_shared::settings::library_field_title(field)
    }
}

pub fn track_column_fit_width(key: LibraryListKey, field: LibraryField) -> i32 {
    column_fit_width(field, track_column_width(key, field))
}
pub fn track_column_width(key: LibraryListKey, field: LibraryField) -> i32 {
    if matches!(
        key,
        LibraryListKey::AlbumDetailTracks
            | LibraryListKey::ArtistTracks
            | LibraryListKey::GenreTracks
            | LibraryListKey::MoodTracks
            | LibraryListKey::PlaylistTracks
            | LibraryListKey::SmartPlaylistTracks
    ) {
        match field {
            LibraryField::RowIndex => {
                return DETAIL_TRACK_UTILITY_COLUMN_WIDTH;
            }
            LibraryField::Duration => return track_list_column_width(field),
            LibraryField::Favorite => return FAVORITE_COLUMN_WIDTH,
            _ => {}
        }
    }
    if key == LibraryListKey::History && field == LibraryField::LastPlayed {
        return 148;
    }

    match key {
        LibraryListKey::ArtistTracks
        | LibraryListKey::GenreTracks
        | LibraryListKey::PlaylistTracks => return track_list_column_width(field),
        LibraryListKey::SmartPlaylistTracks => {}
        _ => return column_width(field),
    }

    match field {
        LibraryField::RowIndex => 44,
        LibraryField::Title | LibraryField::TitleMerged => 212,
        LibraryField::Album
        | LibraryField::Artist
        | LibraryField::AlbumArtist
        | LibraryField::Genre => 180,
        LibraryField::PlayCount => play_count_column_width(),
        LibraryField::UserRating | LibraryField::SongCount | LibraryField::AlbumCount => 82,
        LibraryField::ReleaseDate | LibraryField::DateAdded | LibraryField::LastPlayed => 108,
        LibraryField::Year
        | LibraryField::DiscNumber
        | LibraryField::TrackNumber
        | LibraryField::Bpm => 62,
        LibraryField::Duration => 70,
        LibraryField::Image => column_width(LibraryField::Image),
        LibraryField::Favorite => FAVORITE_COLUMN_WIDTH,
        LibraryField::Tools => ui_shared::recycled_cells::ROW_ACTIONS_WIDTH,
    }
}
pub fn column_fit_width(field: LibraryField, width: i32) -> i32 {
    if field == LibraryField::TitleMerged {
        width.saturating_add(72)
    } else {
        width
    }
}
fn track_list_column_width(field: LibraryField) -> i32 {
    match field {
        LibraryField::RowIndex => 54,
        LibraryField::Title | LibraryField::TitleMerged => 320,
        LibraryField::Album => 260,
        LibraryField::Artist | LibraryField::AlbumArtist | LibraryField::Genre => 220,
        LibraryField::Year
        | LibraryField::DiscNumber
        | LibraryField::TrackNumber
        | LibraryField::Bpm => 70,
        LibraryField::Duration => 90,
        LibraryField::Favorite => FAVORITE_COLUMN_WIDTH,
        LibraryField::Tools => ui_shared::recycled_cells::ROW_ACTIONS_WIDTH,
        _ => column_width(field),
    }
}

pub fn text_column<T, F>(field: LibraryField, width: i32, value: F) -> gtk::ColumnViewColumn
where
    T: Clone + 'static,
    F: Fn(&T) -> String + 'static,
{
    let factory = gtk::SignalListItemFactory::new();
    let value = Rc::new(value);
    factory.connect_setup(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            let label = gtk::Label::new(None);
            add_field_skeleton_class(&label, field);
            label.set_xalign(0.0);
            label.set_halign(gtk::Align::Fill);
            label.set_hexpand(true);
            label.set_wrap(false);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.set_single_line_mode(true);
            item.set_child(Some(&label));
        }
    });
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(label) = item
            .child()
            .and_then(|child| child.downcast::<gtk::Label>().ok())
        else {
            return;
        };
        let Some(text) = item
            .item()
            .and_then(|object| object_item::<T, _>(object, |row| value(row)))
        else {
            label.set_text("");
            return;
        };
        label.set_text(&text);
    });
    factory.connect_unbind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(label) = item
            .child()
            .and_then(|child| child.downcast::<gtk::Label>().ok())
        {
            label.set_text("");
        }
    });
    let column = localized_column(ui_shared::settings::library_field_title(field), &factory);
    column.set_fixed_width(width);
    column
}
pub fn row_index_column_with_width(width: i32) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            let label = gtk::Label::new(None);
            label.add_css_class("muted");
            label.set_xalign(0.5);
            label.set_halign(gtk::Align::Fill);
            label.set_hexpand(true);
            label.set_wrap(false);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.set_single_line_mode(true);
            item.set_child(Some(&label));
        }
    });
    connect_sparse_bind(&factory, |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(label) = item
            .child()
            .and_then(|child| child.downcast::<gtk::Label>().ok())
        else {
            return;
        };
        label.set_text(&(item.position() + 1).to_string());
    });
    let column = gtk::ColumnViewColumn::new(Some(ROW_INDEX_COLUMN_TITLE), Some(factory));
    column.set_fixed_width(width);
    column
}

pub fn mapped_row_index_column<T: Clone + 'static>(
    shell: &Rc<CatalogUi>,
    width: i32,
    target: impl Fn(&T) -> PlaybackTarget + 'static,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let shell = Rc::clone(shell);
    factory.connect_setup(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            item.set_child(Some(&CollectionIndexCell::new(&shell)));
        }
    });
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = list_cell::<CollectionIndexCell>(item) else {
            return;
        };
        let target = item
            .item()
            .and_then(|object| object_item::<T, _>(object, &target));
        cell.bind(item.position(), target);
    });
    factory.connect_unbind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(cell) = list_cell::<CollectionIndexCell>(item) {
            cell.bind(0, None);
        }
    });
    let column = gtk::ColumnViewColumn::new(Some(ROW_INDEX_COLUMN_TITLE), Some(factory));
    column.set_fixed_width(width);
    column
}

pub fn mapped_track_row_index_column_with_width<T, Number>(
    width: i32,
    playing: TrackRowPlayingIndicator,
    track: impl Fn(&T) -> Option<&str> + 'static,
    number: Number,
) -> gtk::ColumnViewColumn
where
    T: Clone + 'static,
    Number: Fn(u32, &T) -> Option<String> + 'static,
{
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            item.set_child(Some(&ui_shared::recycled_cells::track_list_row_index_cell(
                item,
            )));
        }
    });
    let bind_playing = playing.clone();
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = item
            .child()
            .and_then(|child| child.downcast::<gtk::Overlay>().ok())
        else {
            return;
        };
        let bound = item
            .item()
            .and_then(|object| {
                object_item::<T, _>(object, |row| {
                    let text = number(item.position(), row)?;
                    let uri = track(row)?;
                    set_track_row_index_text(&cell, &text);
                    bind_playing.bind(cell.upcast_ref(), item.position(), uri);
                    Some(())
                })
            })
            .flatten();
        if bound.is_none() {
            set_track_row_index_text(&cell, "");
            bind_playing.unbind(cell.upcast_ref());
        }
    });
    factory.connect_unbind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = item
            .child()
            .and_then(|child| child.downcast::<gtk::Overlay>().ok())
        else {
            return;
        };
        playing.unbind(cell.upcast_ref());
        set_track_row_index_text(&cell, "");
    });
    let column = gtk::ColumnViewColumn::new(Some(ROW_INDEX_COLUMN_TITLE), Some(factory));
    column.set_fixed_width(width);
    column
}

#[derive(Clone)]
pub struct TrackRowPlayingIndicator {
    inner: Rc<TrackRowPlayingIndicatorInner>,
}

struct TrackRowPlayingIndicatorInner {
    position: std::cell::Cell<u32>,
    media_uri: RefCell<Option<String>>,
    paused: std::cell::Cell<bool>,
    cells: RefCell<HashMap<usize, (glib::WeakRef<gtk::Widget>, u32, String)>>,
}

impl TrackRowPlayingIndicator {
    pub fn new() -> Self {
        Self {
            inner: Rc::new(TrackRowPlayingIndicatorInner {
                position: std::cell::Cell::new(gtk::INVALID_LIST_POSITION),
                media_uri: RefCell::new(None),
                paused: std::cell::Cell::new(false),
                cells: RefCell::new(HashMap::new()),
            }),
        }
    }

    fn matches(&self, position: u32, media_uri: &str) -> bool {
        self.inner.media_uri.borrow().as_deref() == Some(media_uri)
            && (self.inner.position.get() == gtk::INVALID_LIST_POSITION
                || self.inner.position.get() == position)
    }

    pub fn bind(&self, widget: &gtk::Widget, position: u32, media_uri: &str) {
        apply_track_row_playing(
            widget,
            self.matches(position, media_uri),
            self.inner.paused.get(),
        );
        self.inner
            .cells
            .borrow_mut()
            .entry(widget.as_ptr() as usize)
            .and_modify(|(bound_widget, bound_position, uri)| {
                *bound_widget = widget.downgrade();
                *bound_position = position;
                if uri != media_uri {
                    media_uri.clone_into(uri);
                }
            })
            .or_insert_with(|| (widget.downgrade(), position, media_uri.to_owned()));
    }

    pub fn unbind(&self, widget: &gtk::Widget) {
        widget.remove_css_class("track-row-playing");
        widget.remove_css_class("track-row-paused");
        self.inner
            .cells
            .borrow_mut()
            .remove(&(widget.as_ptr() as usize));
    }

    pub fn set_current(&self, media_uri: Option<&str>, position: u32) {
        if self.inner.position.get() == position
            && self.inner.media_uri.borrow().as_deref() == media_uri
        {
            return;
        }
        self.inner.media_uri.replace(media_uri.map(str::to_owned));
        self.inner.position.set(position);
        self.refresh();
    }

    fn refresh(&self) {
        self.inner
            .cells
            .borrow_mut()
            .retain(|_, (widget, bound_position, uri)| {
                let Some(widget) = widget.upgrade() else {
                    return false;
                };
                apply_track_row_playing(
                    &widget,
                    self.matches(*bound_position, uri),
                    self.inner.paused.get(),
                );
                true
            });
    }

    pub fn set_paused(&self, paused: bool) {
        if self.inner.paused.replace(paused) != paused {
            self.refresh();
        }
    }
}

fn apply_track_row_playing(cell: &gtk::Widget, playing: bool, paused: bool) {
    ui_shared::recycled_cells::set_track_playing(cell, playing, paused);
}

pub use ui_shared::recycled_cells::{set_track_row_index_text, track_row_index_cell};

fn install_artist_list_item_context_menu(
    target: &impl IsA<gtk::Widget>,
    shell: &Rc<CatalogUi>,
    item: &gtk::ListItem,
    album_artist: bool,
) {
    install_collection_cell_drag(
        target,
        shell,
        item,
        Rc::new(move |artist: ArtistRow| {
            let target = if album_artist {
                PlaybackTarget::AlbumArtist(artist.media_uri)
            } else {
                PlaybackTarget::Artist(artist.media_uri)
            };
            (target, artist.name)
        }),
    );
    let item = item.downgrade();
    let shell = Rc::clone(shell);
    install_context_menu_openers(
        target,
        Rc::new(move |target, position| {
            let Some(artist) = item.upgrade().and_then(|item| item_at_from_item(&item)) else {
                return;
            };
            present_artist_context_menu(
                target,
                &shell.media_menus,
                artist,
                album_artist,
                None,
                position,
            );
        }),
    );
}

fn install_collection_cell_drag<T: Clone + 'static>(
    widget: &impl IsA<gtk::Widget>,
    shell: &Rc<CatalogUi>,
    item: &gtk::ListItem,
    target: Rc<dyn Fn(T) -> (PlaybackTarget, String)>,
) {
    let shell = Rc::downgrade(shell);
    let item = item.downgrade();
    install_media_drag_source(widget, move || {
        let shell = shell.upgrade()?;
        let (target, title) = target(item_at_from_item::<T>(&item.upgrade()?)?);
        Some((
            MediaDragSource::capture_target(shell.selected_library().as_deref(), target),
            title,
        ))
    });
}

pub fn artwork_column<T, F>(
    shell: &Rc<CatalogUi>,
    title: &str,
    width: i32,
    candidates: F,
    drag: Option<fn(T) -> (PlaybackTarget, String)>,
) -> gtk::ColumnViewColumn
where
    T: Clone + 'static,
    F: Fn(&T) -> ArtworkBinding + 'static,
{
    let factory = gtk::SignalListItemFactory::new();
    let shell = Rc::clone(shell);
    let candidates = Rc::new(candidates);

    let setup_shell = Rc::downgrade(&shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = RecycledArtworkCell::new(48);
        if let Some(drag) = drag
            && let Some(shell) = setup_shell.upgrade()
        {
            install_collection_cell_drag(&cell, &shell, item, Rc::new(drag));
        }
        item.set_child(Some(&cell));
    });

    let bind_shell = Rc::clone(&shell);
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = list_cell::<RecycledArtworkCell>(item) else {
            return;
        };
        let cover = cell.artwork();
        let Some(artwork) = item
            .item()
            .and_then(|object| object_item::<T, _>(object, |row| candidates(row)))
        else {
            set_cover_placeholder(&bind_shell, &cover, true);
            return;
        };
        set_cover_placeholder(&bind_shell, &cover, false);
        bind_shell
            .artwork
            .bind_artwork_tile(&cover, artwork, 48, THUMB_COVER_SIZE);
    });
    let unbind_shell = Rc::clone(&shell);
    factory.connect_unbind(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>()
            && let Some(cell) = list_cell::<RecycledArtworkCell>(item)
        {
            set_cover_placeholder(&unbind_shell, &cell.artwork(), true);
        }
    });
    let column = localized_column(title, &factory);
    column.set_fixed_width(width);
    column
}
pub fn artist_image_column(shell: &Rc<CatalogUi>, album_artist: bool) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let shell = Rc::clone(shell);

    let setup_shell = Rc::clone(&shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = RecycledArtworkCell::new(48);
        install_artist_list_item_context_menu(&cell, &setup_shell, item, album_artist);
        item.set_child(Some(&cell));
    });

    let bind_shell = Rc::clone(&shell);
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = list_cell::<RecycledArtworkCell>(item) else {
            return;
        };
        let cover = cell.artwork();
        let Some(artwork) = item.item().and_then(|object| {
            object_item::<ArtistRow, _>(object, |row| {
                opaque_artwork(row.artwork_binding.as_deref())
            })
        }) else {
            set_cover_placeholder(&bind_shell, &cover, true);
            return;
        };
        set_cover_placeholder(&bind_shell, &cover, false);
        bind_shell
            .artwork
            .bind_artwork_tile(&cover, artwork, 48, THUMB_COVER_SIZE);
    });
    let unbind_shell = Rc::clone(&shell);
    factory.connect_unbind(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>()
            && let Some(cell) = list_cell::<RecycledArtworkCell>(item)
        {
            set_cover_placeholder(&unbind_shell, &cell.artwork(), true);
        }
    });
    let column = localized_column("Image", &factory);
    column.set_fixed_width(column_width(LibraryField::Image));
    column
}
pub fn artist_text_column<F>(
    shell: &Rc<CatalogUi>,
    field: LibraryField,
    title: &str,
    width: i32,
    album_artist: bool,
    value: F,
) -> gtk::ColumnViewColumn
where
    F: Fn(&ArtistRow) -> String + 'static,
{
    let factory = gtk::SignalListItemFactory::new();
    if matches!(field, LibraryField::Title | LibraryField::TitleMerged) {
        bind_collection_title::<ArtistRow>(&factory, shell, move |row| {
            if album_artist {
                PlaybackTarget::AlbumArtist(row.media_uri.clone())
            } else {
                PlaybackTarget::Artist(row.media_uri.clone())
            }
        });
    }
    let shell = Rc::clone(shell);
    let value = Rc::new(value);

    let setup_shell = Rc::clone(&shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = RecycledBadgedTextCell::with_downloads(&setup_shell.downloads);
        add_field_skeleton_class(&cell, field);
        install_artist_list_item_context_menu(&cell, &setup_shell, item, album_artist);
        item.set_child(Some(&cell));
    });

    let bind_shell = Rc::clone(&shell);
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = list_cell::<RecycledBadgedTextCell>(item) else {
            return;
        };
        let Some((text, downloaded)) = item.item().and_then(|object| {
            object_item::<ArtistRow, _>(object, |row| {
                (
                    value(row),
                    collection_is_downloaded(row.track_count, row.downloaded_count),
                )
            })
        }) else {
            cell.clear();
            return;
        };
        cell.label().set_text(&text);
        bind_shell.bind_download_badge(&cell.downloaded(), downloaded);
    });
    factory.connect_unbind(|_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>()
            && let Some(cell) = list_cell::<RecycledBadgedTextCell>(item)
        {
            cell.clear();
        }
    });
    let column = localized_column(title, &factory);
    column.set_fixed_width(width);
    column
}

fn install_track_list_item_context_menu<T: Clone + 'static>(
    target: &impl IsA<gtk::Widget>,
    shell: &Rc<CatalogUi>,
    item: &gtk::ListItem,
    track_value: Rc<dyn Fn(&T) -> Option<String>>,
) {
    let item = item.downgrade();
    let shell = Rc::clone(shell);
    install_context_menu_openers(
        target,
        Rc::new(move |target, position| {
            let Some(track) = item
                .upgrade()
                .and_then(|item| item.item())
                .and_then(|object| object_item::<T, _>(object, |row| track_value(row)))
                .flatten()
            else {
                return;
            };
            present_track_context_menu(target, &shell, track, position);
        }),
    );
}

fn list_text_cell(item: &gtk::ListItem) -> Option<(gtk::Label, Option<gtk::Image>)> {
    if let Some(cell) = list_cell::<RecycledBadgedTextCell>(item) {
        return Some((cell.label(), Some(cell.downloaded())));
    }
    list_cell::<RecycledTextCell>(item).map(|cell| (cell.label(), None))
}

pub fn mapped_track_image_column<T, TrackValue, ArtworkValue>(
    shell: &Rc<CatalogUi>,
    title: &'static str,
    width: i32,
    track_value: TrackValue,
    artwork_value: ArtworkValue,
) -> gtk::ColumnViewColumn
where
    T: Clone + 'static,
    TrackValue: Fn(&T) -> Option<String> + 'static,
    ArtworkValue: Fn(&T) -> ArtworkBinding + 'static,
{
    let factory = gtk::SignalListItemFactory::new();
    let shell = Rc::clone(shell);
    let track_value: Rc<dyn Fn(&T) -> Option<String>> = Rc::new(track_value);
    let artwork_value = Rc::new(artwork_value);

    let setup_shell = Rc::clone(&shell);
    let setup_track_value = Rc::clone(&track_value);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = RecycledArtworkCell::new(48);
        install_track_list_item_context_menu(
            &cell,
            &setup_shell,
            item,
            Rc::clone(&setup_track_value),
        );
        item.set_child(Some(&cell));
    });

    let bind_shell = Rc::clone(&shell);
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = list_cell::<RecycledArtworkCell>(item) else {
            return;
        };
        let cover = cell.artwork();
        let Some(artwork) = item
            .item()
            .and_then(|object| object_item::<T, _>(object, |row| artwork_value(row)))
        else {
            set_cover_placeholder(&bind_shell, &cover, true);
            return;
        };
        set_cover_placeholder(&bind_shell, &cover, false);
        bind_shell
            .artwork
            .bind_artwork_tile(&cover, artwork, 48, THUMB_COVER_SIZE);
    });

    let unbind_shell = Rc::clone(&shell);
    factory.connect_unbind(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>()
            && let Some(cell) = list_cell::<RecycledArtworkCell>(item)
        {
            set_cover_placeholder(&unbind_shell, &cell.artwork(), true);
        }
    });

    let column = localized_column(title, &factory);
    column.set_fixed_width(width);
    column
}
pub fn mapped_track_position_text_column<T, TrackValue, Downloaded, Value>(
    shell: &Rc<CatalogUi>,
    field: LibraryField,
    title: &'static str,
    width: i32,
    xalign: f32,
    playing: Option<TrackRowPlayingIndicator>,
    track_value: TrackValue,
    downloaded_value: Downloaded,
    value: Value,
) -> gtk::ColumnViewColumn
where
    T: Clone + 'static,
    TrackValue: Fn(&T) -> Option<String> + 'static,
    Downloaded: Fn(&T) -> bool + 'static,
    Value: Fn(u32, &T) -> String + 'static,
{
    let factory = gtk::SignalListItemFactory::new();
    let shell = Rc::clone(shell);
    let value = Rc::new(value);
    let track_value: Rc<dyn Fn(&T) -> Option<String>> = Rc::new(track_value);
    let downloaded_value = Rc::new(downloaded_value);

    let setup_shell = Rc::clone(&shell);
    let setup_playing = playing.clone();
    let setup_track_value = Rc::clone(&track_value);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let (child, label): (gtk::Widget, gtk::Label) = if setup_playing.is_some() {
            let cell = RecycledBadgedTextCell::with_downloads(&setup_shell.downloads);
            let label = cell.label();
            (cell.upcast(), label)
        } else {
            let cell = RecycledTextCell::new();
            let label = cell.label();
            (cell.upcast(), label)
        };
        add_field_skeleton_class(&child, field);
        if setup_playing.is_some() {
            label.add_css_class("track-list-title");
        }
        label.set_xalign(xalign);
        install_track_list_item_context_menu(
            &child,
            &setup_shell,
            item,
            Rc::clone(&setup_track_value),
        );
        item.set_child(Some(&child));
    });

    let bind_shell = Rc::clone(&shell);
    let bind_playing = playing.clone();
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some((label, downloaded)) = list_text_cell(item) else {
            return;
        };
        let Some((text, download, track)) = item.item().and_then(|object| {
            object_item::<T, _>(object, |row| {
                (
                    value(item.position(), row),
                    downloaded.is_some() && downloaded_value(row),
                    bind_playing.as_ref().and_then(|_| track_value(row)),
                )
            })
        }) else {
            label.set_text("");
            if let Some(badge) = downloaded.as_ref() {
                badge.set_visible(false);
            }
            if let Some(playing) = bind_playing.as_ref() {
                playing.unbind(label.upcast_ref());
            }
            return;
        };
        label.set_text(&text);
        if let Some(downloaded) = downloaded.as_ref() {
            bind_shell
                .downloads
                .bind_download_badge(downloaded, download);
        }
        if let Some(playing) = bind_playing.as_ref() {
            if let Some(uri) = track {
                playing.bind(label.upcast_ref(), item.position(), &uri);
            }
        }
    });

    factory.connect_unbind(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>()
            && let Some((label, downloaded)) = list_text_cell(item)
        {
            label.set_text("");
            if let Some(downloaded) = downloaded.as_ref() {
                downloaded.set_visible(false);
            }
            if let Some(playing) = playing.as_ref() {
                playing.unbind(label.upcast_ref());
            }
        }
    });

    let column = localized_column(title, &factory);
    column.set_fixed_width(width);
    column
}

pub struct TrackMergedColumnValues<ItemTrack, Downloaded, Artwork, Title, Subtitle, SubtitleLinks> {
    pub track: ItemTrack,
    pub downloaded: Downloaded,
    pub artwork: Artwork,
    pub title: Title,
    pub subtitle: Subtitle,
    pub subtitle_links: SubtitleLinks,
    pub context_menu: bool,
}

pub fn track_merged_column<T, ItemTrack, Downloaded, Artwork, Title, Subtitle, SubtitleLinks>(
    shell: &Rc<CatalogUi>,
    title: &'static str,
    width: i32,
    playing: TrackRowPlayingIndicator,
    values: TrackMergedColumnValues<ItemTrack, Downloaded, Artwork, Title, Subtitle, SubtitleLinks>,
) -> gtk::ColumnViewColumn
where
    T: Clone + 'static,
    ItemTrack: Fn(&T) -> Option<String> + 'static,
    Downloaded: Fn(&T) -> bool + 'static,
    Artwork: Fn(&T) -> ArtworkBinding + 'static,
    Title: Fn(&T) -> String + 'static,
    Subtitle: Fn(&T) -> String + 'static,
    SubtitleLinks: Fn(&T) -> Option<DetailLinks> + 'static,
{
    let factory = gtk::SignalListItemFactory::new();
    let shell = Rc::clone(shell);
    let TrackMergedColumnValues {
        track: item_track,
        downloaded: downloaded_value,
        artwork: artwork_value,
        title: title_value,
        subtitle: subtitle_value,
        subtitle_links,
        context_menu,
    } = values;
    let title_value = Rc::new(title_value);
    let downloaded_value = Rc::new(downloaded_value);
    let item_track: Rc<dyn Fn(&T) -> Option<String>> = Rc::new(item_track);
    let artwork_value = Rc::new(artwork_value);
    let subtitle_value = Rc::new(subtitle_value);
    let subtitle_links = Rc::new(subtitle_links);
    let context_track: Rc<dyn Fn(&T) -> Option<String>> = Rc::clone(&item_track);

    let setup_shell = Rc::clone(&shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = RecycledMergedCell::new(
            setup_shell.route_navigation(),
            &setup_shell.downloads,
            48,
            true,
        );
        let title = cell.title();
        title.add_css_class("track-list-title");
        let subtitle = cell.subtitle();
        subtitle.add_css_class("artist-label");
        subtitle.add_css_class("table-link-label");
        subtitle.set_visible(false);
        if context_menu {
            install_track_list_item_context_menu(
                &cell,
                &setup_shell,
                item,
                Rc::clone(&context_track),
            );
        }
        item.set_child(Some(&cell));
    });

    let bind_shell = Rc::clone(&shell);
    let bind_playing = playing.clone();
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = list_cell::<RecycledMergedCell>(item) else {
            return;
        };
        let cover = cell.cover();
        let title = cell.title();
        let Some((artwork, text, subtitle, links, downloaded, track)) =
            item.item().and_then(|object| {
                object_item::<T, _>(object, |row| {
                    (
                        artwork_value(row),
                        title_value(row),
                        subtitle_value(row),
                        subtitle_links(row),
                        downloaded_value(row),
                        item_track(row),
                    )
                })
            })
        else {
            title.set_text("");
            cell.downloaded()
                .expect("track cell badge")
                .set_visible(false);
            bind_playing.unbind(title.upcast_ref());
            cell.clear_subtitle();
            clear_merged_artwork(&bind_shell, &cover);
            return;
        };
        set_cover_placeholder(&bind_shell, &cover, false);
        bind_shell
            .artwork
            .bind_artwork_tile(&cover, artwork, 48, THUMB_COVER_SIZE);
        title.set_text(&text);
        bind_shell
            .downloads
            .bind_download_badge(&cell.downloaded().expect("track cell badge"), downloaded);
        if let Some(uri) = track {
            bind_playing.bind(title.upcast_ref(), item.position(), &uri);
        }
        if subtitle.trim().is_empty() {
            cell.clear_subtitle();
        } else {
            cell.bind_subtitle(links.unwrap_or_else(|| DetailLinks::text(&subtitle)));
            cell.subtitle().set_visible(true);
        }
    });

    let unbind_shell = Rc::clone(&shell);
    factory.connect_unbind(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>()
            && let Some(cell) = list_cell::<RecycledMergedCell>(item)
        {
            let title = cell.title();
            title.set_text("");
            cell.downloaded()
                .expect("track cell badge")
                .set_visible(false);
            playing.unbind(title.upcast_ref());
            cell.clear_subtitle();
            clear_merged_artwork(&unbind_shell, &cell.cover());
        }
    });

    let column = localized_column(title, &factory);
    column.set_fixed_width(width);
    column
}
fn favorite_cell_button(item: &gtk::ListItem) -> Option<gtk::Button> {
    ui_shared::recycled_cells::row_favorite_button(item)
}

pub fn artist_favorite_column(shell: &Rc<CatalogUi>, album_artist: bool) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let shell = Rc::clone(shell);

    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let button = row_favorite_icon_button("Favorite artist");
        let actions = ui_shared::recycled_cells::RowActions::with_favorite(&button);
        set_placeholder_favorite(&button, None);
        let favorite_item = item.downgrade();
        shell.register_dynamic_favorite_button(
            Rc::new(move || {
                favorite_item
                    .upgrade()
                    .and_then(|item| item_at_from_item::<ArtistRow>(&item))
                    .map(|artist| artist_favorite_key(&artist.media_uri))
            }),
            &button,
        );
        install_artist_list_item_context_menu(&actions, &shell, item, album_artist);
        let favorite_shell = Rc::clone(&shell);
        let click_item = item.downgrade();
        button.connect_clicked(move |button| {
            let Some(artist) = click_item
                .upgrade()
                .and_then(|item| item_at_from_item::<ArtistRow>(&item))
            else {
                return;
            };
            let favorite = !favorite_button_is_active(button);
            favorite_shell.set_favorite_with_feedback(
                library::FavoriteTarget::Artist(artist.media_uri),
                favorite,
                Some(button),
            );
        });
        item.set_child(Some(&actions));
    });

    connect_sparse_bind(&factory, |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(button) = favorite_cell_button(item) else {
            return;
        };
        let favorite = item
            .item()
            .and_then(|object| object_item::<ArtistRow, _>(object, |row| row.favorite));
        set_placeholder_favorite(&button, favorite);
    });

    factory.connect_unbind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(button) = favorite_cell_button(item) {
            set_placeholder_favorite(&button, None);
        }
    });
    ui_shared::recycled_cells::row_actions_column(&factory)
}
pub fn mapped_track_favorite_column<T, TrackValue, FavoriteValue>(
    shell: &Rc<CatalogUi>,
    track_value: TrackValue,
    favorite_value: FavoriteValue,
) -> gtk::ColumnViewColumn
where
    T: Clone + 'static,
    TrackValue: Fn(&T) -> Option<String> + 'static,
    FavoriteValue: Fn(&T) -> Option<(String, bool)> + 'static,
{
    let factory = gtk::SignalListItemFactory::new();
    let shell = Rc::clone(shell);
    let track_value: Rc<dyn Fn(&T) -> Option<String>> = Rc::new(track_value);
    let favorite_value = Rc::new(favorite_value);

    let setup_shell = Rc::clone(&shell);
    let setup_track_value = Rc::clone(&track_value);
    let setup_favorite_value = Rc::clone(&favorite_value);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let button = row_favorite_icon_button("Favorite track");
        let actions = ui_shared::recycled_cells::RowActions::with_favorite(&button);
        set_placeholder_favorite(&button, None);
        install_track_list_item_context_menu(
            &actions,
            &setup_shell,
            item,
            Rc::clone(&setup_track_value),
        );
        let favorite_item = item.downgrade();
        let favorite_key_value = Rc::clone(&setup_favorite_value);
        setup_shell.register_dynamic_favorite_button(
            Rc::new(move || {
                favorite_item
                    .upgrade()
                    .and_then(|item| item_at_from_item::<T>(&item))
                    .and_then(|value| favorite_key_value(&value))
                    .map(|(track, _)| track_favorite_key(&track))
            }),
            &button,
        );
        let favorite_shell = Rc::clone(&setup_shell);
        let click_item = item.downgrade();
        let click_favorite_value = Rc::clone(&setup_favorite_value);
        button.connect_clicked(move |button| {
            let Some((track, _)) = click_item
                .upgrade()
                .and_then(|item| item_at_from_item::<T>(&item))
                .and_then(|value| click_favorite_value(&value))
            else {
                return;
            };
            let favorite = !favorite_button_is_active(button);
            favorite_shell.set_favorite_with_feedback(
                library::FavoriteTarget::Track(track),
                favorite,
                Some(button),
            );
        });
        item.set_child(Some(&actions));
    });

    let bind_shell = Rc::clone(&shell);
    let bind_favorite_value = Rc::clone(&favorite_value);
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(button) = favorite_cell_button(item) else {
            return;
        };
        let actions =
            ui_shared::recycled_cells::list_cell::<ui_shared::recycled_cells::RowActions>(item);
        let Some(favorite) = item
            .item()
            .and_then(|object| object_item::<T, _>(object, |row| bind_favorite_value(row)))
        else {
            set_placeholder_favorite(&button, None);
            if let Some(actions) = actions {
                actions.menu().set_sensitive(false);
            }
            return;
        };
        let favorite =
            favorite.map(|(track, favorite)| bind_shell.projected_track_favorite(&track, favorite));
        set_placeholder_favorite(&button, favorite);
        if let Some(actions) = actions {
            actions.menu().set_sensitive(favorite.is_some());
        }
    });

    factory.connect_unbind(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>()
            && let Some(button) = favorite_cell_button(item)
        {
            set_placeholder_favorite(&button, None);
        }
    });

    ui_shared::recycled_cells::row_actions_column(&factory)
}
