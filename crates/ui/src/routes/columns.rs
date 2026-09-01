use std::{cell::RefCell, collections::HashMap, rc::Rc};

use ::library::{AlbumRow, ArtistRow, PlaylistRow, SmartPlaylistRow, TrackRow};
use adw::prelude::*;
use artwork::ArtworkBinding;
use gtk::glib;

use super::collection_context::{
    present_album_context_menu, present_artist_context_menu, present_track_context_menu,
};
use crate::favorites::{
    FAVORITE_COLUMN_TITLE, FAVORITE_COLUMN_WIDTH, album_favorite_key, artist_favorite_key,
    column_favorite_icon_button, favorite_button_is_active, set_favorite_button_active,
    track_favorite_key,
};
use crate::interactions::install_context_menu_openers;
use crate::localization::localized_column;
use crate::shell::Shell;
use crate::shell::cover::{ArtworkTile, THUMB_COVER_SIZE};
use crate::{LibraryField, LibraryListKey};

use super::collections::{install_playlist_reorder, install_smart_playlist_reorder};
use super::detail_links::{
    DetailLinks, album_artist_links, track_album_artist_links, track_artist_links,
};
use super::library_fields::{
    add_field_skeleton_class, album_field, artist_field, column_width, item_at_from_item,
    opaque_artwork, play_count_column_width, playlist_artwork, playlist_field,
    smart_playlist_display_name, smart_playlist_field, track_artwork_at_from_item, track_field,
};
use super::recycled_cells::{
    RecycledArtworkCell, RecycledBadgedTextCell, RecycledMergedCell, RecycledTextCell, list_cell,
};
use super::route::Route;
use super::sparse_model::connect_sparse_bind;
use super::table_links::track_link_column;

pub(crate) const ROW_INDEX_COLUMN_TITLE: &str = "\u{2003}#";
const DETAIL_TRACK_UTILITY_COLUMN_WIDTH: i32 = 48;

fn collection_is_downloaded(track_count: i64, downloaded_count: i64) -> bool {
    track_count > 0 && downloaded_count == track_count
}

fn set_cover_placeholder(shell: &Rc<Shell>, cover: &ArtworkTile, placeholder: bool) {
    if placeholder {
        shell.clear_artwork_tile(cover);
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

fn clear_merged_artwork(shell: &Rc<Shell>, cover: &ArtworkTile) {
    shell.clear_artwork_tile(cover);
    cover
        .widget()
        .remove_css_class("collection-grid-cover-skeleton");
}

fn set_placeholder_favorite(button: &gtk::Button, favorite: Option<bool>) {
    set_favorite_button_active(button, favorite.unwrap_or(false));
    button.set_visible(favorite.is_some());
    button.set_sensitive(favorite.is_some());
}

pub(crate) fn album_column(
    shell: &Rc<Shell>,
    field: LibraryField,
    playback_context: Option<String>,
) -> gtk::ColumnViewColumn {
    match field {
        LibraryField::RowIndex => mapped_row_index_column::<AlbumRow>(column_width(field)),
        LibraryField::Image => album_image_column(
            shell,
            "Image",
            column_width(LibraryField::Image),
            playback_context,
        ),
        LibraryField::TitleMerged => album_merged_column(
            shell,
            "Title",
            column_width(LibraryField::TitleMerged),
            playback_context,
        ),
        LibraryField::Title => {
            album_text_column(shell, field, "Title", 220, playback_context, |album| {
                album.title.clone()
            })
        }
        LibraryField::Favorite => album_favorite_column(shell, playback_context),
        _ => album_text_column(
            shell,
            field,
            field.title(),
            column_width(field),
            playback_context,
            move |album| album_field(album, field),
        ),
    }
}
pub(crate) fn artist_column(
    shell: &Rc<Shell>,
    field: LibraryField,
    album_artist: bool,
) -> gtk::ColumnViewColumn {
    match field {
        LibraryField::RowIndex => mapped_row_index_column::<ArtistRow>(column_width(field)),
        LibraryField::Image => artist_image_column(shell, album_artist),
        LibraryField::TitleMerged | LibraryField::Title => {
            artist_text_column(shell, field, "Title", 220, album_artist, |artist| {
                artist.name.clone()
            })
        }
        LibraryField::Favorite => artist_favorite_column(shell, album_artist),
        _ => artist_text_column(
            shell,
            field,
            field.title(),
            column_width(field),
            album_artist,
            move |artist| artist_field(artist, field),
        ),
    }
}
pub(crate) fn playlist_column(shell: &Rc<Shell>, field: LibraryField) -> gtk::ColumnViewColumn {
    match field {
        LibraryField::RowIndex => mapped_row_index_column::<PlaylistRow>(column_width(field)),
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
    shell: &Rc<Shell>,
    title: &str,
    width: i32,
    value: F,
) -> gtk::ColumnViewColumn
where
    F: Fn(&PlaylistRow) -> String + 'static,
{
    let factory = gtk::SignalListItemFactory::new();
    let value = Rc::new(value);
    let setup_shell = Rc::clone(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = RecycledBadgedTextCell::for_shell(&setup_shell);
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
    });
    let bind_shell = Rc::clone(shell);
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = list_cell::<RecycledBadgedTextCell>(item) else {
            return;
        };
        let Some(playlist) = item_at_from_item::<PlaylistRow>(item) else {
            cell.clear();
            return;
        };
        cell.label().set_text(&(value)(&playlist));
        bind_shell.bind_download_badge(
            &cell.downloaded(),
            collection_is_downloaded(playlist.track_count, playlist.downloaded_count),
        );
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

pub(crate) fn smart_playlist_column(
    shell: &Rc<Shell>,
    field: LibraryField,
) -> gtk::ColumnViewColumn {
    match field {
        LibraryField::RowIndex => mapped_row_index_column::<SmartPlaylistRow>(column_width(field)),
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
    shell: &Rc<Shell>,
    title: &str,
    width: i32,
    value: F,
) -> gtk::ColumnViewColumn
where
    F: Fn(&SmartPlaylistRow) -> String + 'static,
{
    let factory = gtk::SignalListItemFactory::new();
    let value = Rc::new(value);
    let setup_shell = Rc::clone(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = RecycledBadgedTextCell::for_shell(&setup_shell);
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
    });
    let bind_shell = Rc::clone(shell);
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = list_cell::<RecycledBadgedTextCell>(item) else {
            return;
        };
        let Some(playlist) = item_at_from_item::<SmartPlaylistRow>(item) else {
            cell.clear();
            return;
        };
        cell.label().set_text(&(value)(&playlist));
        bind_shell.bind_download_badge(
            &cell.downloaded(),
            collection_is_downloaded(playlist.track_count, playlist.downloaded_count),
        );
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

pub(crate) fn track_column_for_key(
    shell: &Rc<Shell>,
    key: LibraryListKey,
    field: LibraryField,
    playing: &TrackRowPlayingIndicator,
) -> gtk::ColumnViewColumn {
    let width = track_column_width(key, field);
    match field {
        LibraryField::RowIndex => track_row_index_column_with_width(width, playing.clone()),
        LibraryField::Image => track_image_column(shell, "Image", width),
        LibraryField::TitleMerged => track_merged_column(
            shell,
            "Title",
            width,
            playing.clone(),
            TrackMergedColumnValues {
                track: |track: &TrackRow| track.clone(),
                artwork: |track: &TrackRow| opaque_artwork(track.artwork_binding.as_deref()),
                title: |track: &TrackRow| track.title.clone(),
                subtitle: |track: &TrackRow| track.display_artist.clone(),
                subtitle_links: |track: &TrackRow| Some(track_artist_links(track)),
                context_menu: true,
            },
        ),
        LibraryField::Title => {
            let column = track_text_column(
                shell,
                field,
                "Title",
                width,
                0.0,
                Some(playing.clone()),
                |track| track.title.clone(),
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
        LibraryField::Favorite => track_favorite_column(shell),
        LibraryField::Artist => track_link_column(shell, "Artist", width, track_artist_links),
        LibraryField::AlbumArtist => track_link_column(
            shell,
            LibraryField::AlbumArtist.title(),
            width,
            track_album_artist_links,
        ),
        LibraryField::Album => track_link_column(shell, "Album", width, |track| {
            DetailLinks::route(
                &track.display_album,
                track.album_key.clone().map(Route::AlbumDetail),
            )
        }),
        _ => track_text_column(
            shell,
            field,
            track_column_title(field),
            width,
            0.0,
            None,
            move |track| track_field(track, field),
        ),
    }
}

pub(crate) fn track_column_title(field: LibraryField) -> &'static str {
    if field == LibraryField::Duration {
        "◷"
    } else {
        field.title()
    }
}

pub(crate) fn track_column_fit_width(key: LibraryListKey, field: LibraryField) -> i32 {
    column_fit_width(field, track_column_width(key, field))
}
pub(crate) fn track_column_width(key: LibraryListKey, field: LibraryField) -> i32 {
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
            LibraryField::RowIndex | LibraryField::Duration => {
                return DETAIL_TRACK_UTILITY_COLUMN_WIDTH;
            }
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
    }
}
pub(crate) fn column_fit_width(field: LibraryField, width: i32) -> i32 {
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
        _ => column_width(field),
    }
}

pub(crate) fn text_column<T, F>(field: LibraryField, width: i32, value: F) -> gtk::ColumnViewColumn
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
        let Some(data) = item_at_from_item::<T>(item) else {
            label.set_text("");
            return;
        };
        label.set_text(&(value)(&data));
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
    let column = localized_column(field.title(), &factory);
    column.set_fixed_width(width);
    column
}
pub(crate) fn row_index_column_with_width(width: i32) -> gtk::ColumnViewColumn {
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

pub(crate) fn mapped_row_index_column<T: Clone + 'static>(width: i32) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            let label = gtk::Label::new(None);
            label.add_css_class("muted");
            label.set_xalign(0.5);
            label.set_hexpand(true);
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
        let ready = item_at_from_item::<T>(item).is_some();
        let text = ready
            .then(|| (item.position() + 1).to_string())
            .unwrap_or_default();
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
    let column = gtk::ColumnViewColumn::new(Some(ROW_INDEX_COLUMN_TITLE), Some(factory));
    column.set_fixed_width(width);
    column
}

pub(crate) fn track_row_index_column_with_width(
    width: i32,
    playing: TrackRowPlayingIndicator,
) -> gtk::ColumnViewColumn {
    mapped_track_row_index_column_with_width::<TrackRow, _>(width, playing, |_| true)
}

pub(crate) fn mapped_track_row_index_column_with_width<T, Ready>(
    width: i32,
    playing: TrackRowPlayingIndicator,
    is_ready: Ready,
) -> gtk::ColumnViewColumn
where
    T: Clone + 'static,
    Ready: Fn(&T) -> bool + 'static,
{
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            item.set_child(Some(&track_row_index_cell("")));
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
        let ready = item_at_from_item::<T>(item).as_ref().is_some_and(&is_ready);
        if ready {
            set_track_row_index_text(&cell, &(item.position() + 1).to_string());
            bind_playing.bind(cell.upcast_ref(), item.position());
        } else {
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
pub(crate) struct TrackRowPlayingIndicator {
    inner: Rc<TrackRowPlayingIndicatorInner>,
}

struct TrackRowPlayingIndicatorInner {
    position: std::cell::Cell<u32>,
    paused: std::cell::Cell<bool>,
    cells: RefCell<HashMap<usize, (glib::WeakRef<gtk::Widget>, u32)>>,
}

impl TrackRowPlayingIndicator {
    pub(crate) fn new() -> Self {
        Self {
            inner: Rc::new(TrackRowPlayingIndicatorInner {
                position: std::cell::Cell::new(gtk::INVALID_LIST_POSITION),
                paused: std::cell::Cell::new(false),
                cells: RefCell::new(HashMap::new()),
            }),
        }
    }

    pub(crate) fn bind(&self, widget: &gtk::Widget, position: u32) {
        apply_track_row_playing(
            widget,
            position == self.inner.position.get(),
            self.inner.paused.get(),
        );
        self.inner
            .cells
            .borrow_mut()
            .insert(widget.as_ptr() as usize, (widget.downgrade(), position));
    }

    pub(crate) fn unbind(&self, widget: &gtk::Widget) {
        widget.remove_css_class("track-row-playing");
        widget.remove_css_class("track-row-paused");
        self.inner
            .cells
            .borrow_mut()
            .remove(&(widget.as_ptr() as usize));
    }

    pub(crate) fn set_position(&self, position: u32) {
        self.inner.position.set(position);
        self.inner
            .cells
            .borrow_mut()
            .retain(|_, (widget, bound_position)| {
                let Some(widget) = widget.upgrade() else {
                    return false;
                };
                apply_track_row_playing(
                    &widget,
                    *bound_position == position,
                    self.inner.paused.get(),
                );
                true
            });
    }

    pub(crate) fn set_paused(&self, paused: bool) {
        self.inner.paused.set(paused);
        let position = self.inner.position.get();
        self.inner
            .cells
            .borrow_mut()
            .retain(|_, (widget, bound_position)| {
                let Some(widget) = widget.upgrade() else {
                    return false;
                };
                apply_track_row_playing(&widget, *bound_position == position, paused);
                true
            });
    }
}

fn apply_track_row_playing(cell: &gtk::Widget, playing: bool, paused: bool) {
    if playing {
        cell.add_css_class("track-row-playing");
    } else {
        cell.remove_css_class("track-row-playing");
    }
    if playing && paused {
        cell.add_css_class("track-row-paused");
    } else {
        cell.remove_css_class("track-row-paused");
    }
}

pub(crate) fn track_row_index_cell(text: &str) -> gtk::Overlay {
    let cell = gtk::Overlay::new();
    cell.add_css_class("track-row-index-cell");
    cell.set_hexpand(true);
    cell.set_halign(gtk::Align::Fill);

    let label = gtk::Label::new(Some(text));
    label.add_css_class("muted");
    label.add_css_class("track-row-index-number");
    label.set_xalign(0.5);
    label.set_halign(gtk::Align::Fill);
    label.set_hexpand(true);
    label.set_single_line_mode(true);
    cell.set_child(Some(&label));

    let playing = gtk::Image::from_icon_name("rufin-media-playback-start-symbolic");
    playing.add_css_class("track-row-index-playing");
    playing.set_pixel_size(14);
    playing.set_halign(gtk::Align::Center);
    playing.set_valign(gtk::Align::Center);
    playing.set_margin_start(2);
    cell.add_overlay(&playing);

    let paused = gtk::Image::from_icon_name("rufin-media-playback-pause-symbolic");
    paused.add_css_class("track-row-index-paused");
    paused.set_pixel_size(14);
    paused.set_halign(gtk::Align::Center);
    paused.set_valign(gtk::Align::Center);
    paused.set_margin_start(2);
    cell.add_overlay(&paused);
    cell
}

pub(crate) fn set_track_row_index_text(cell: &gtk::Overlay, text: &str) {
    let Some(label) = cell
        .child()
        .and_then(|child| child.downcast::<gtk::Label>().ok())
    else {
        return;
    };
    label.set_text(text);
}

pub(crate) fn album_image_column(
    shell: &Rc<Shell>,
    title: &'static str,
    width: i32,
    playback_context: Option<String>,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let shell = Rc::clone(shell);

    let setup_shell = Rc::clone(&shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = RecycledArtworkCell::new(48);
        install_album_list_item_context_menu(&cell, &setup_shell, item, playback_context.clone());
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
        let Some(album) = item_at_from_item::<AlbumRow>(item) else {
            set_cover_placeholder(&bind_shell, &cover, true);
            return;
        };
        set_cover_placeholder(&bind_shell, &cover, false);
        bind_shell.bind_artwork_tile(
            &cover,
            opaque_artwork(album.artwork_binding.as_deref()),
            48,
            THUMB_COVER_SIZE,
        );
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

pub(crate) fn album_text_column<F>(
    shell: &Rc<Shell>,
    field: LibraryField,
    title: &'static str,
    width: i32,
    playback_context: Option<String>,
    value: F,
) -> gtk::ColumnViewColumn
where
    F: Fn(&AlbumRow) -> String + 'static,
{
    let factory = gtk::SignalListItemFactory::new();
    let shell = Rc::clone(shell);
    let value = Rc::new(value);

    let setup_shell = Rc::clone(&shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = RecycledBadgedTextCell::for_shell(&setup_shell);
        add_field_skeleton_class(&cell, field);
        install_album_list_item_context_menu(&cell, &setup_shell, item, playback_context.clone());
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
        let Some(album) = item_at_from_item::<AlbumRow>(item) else {
            cell.clear();
            return;
        };
        cell.label().set_text(&(value)(&album));
        bind_shell.bind_download_badge(
            &cell.downloaded(),
            collection_is_downloaded(album.track_count, album.downloaded_count),
        );
    });

    factory.connect_unbind(move |_, item| {
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

pub(crate) fn album_merged_column(
    shell: &Rc<Shell>,
    title: &'static str,
    width: i32,
    playback_context: Option<String>,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let shell = Rc::clone(shell);

    let setup_shell = Rc::clone(&shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = RecycledMergedCell::new(&setup_shell, 48, true);
        let subtitle = cell.subtitle();
        subtitle.add_css_class("artist-label");
        subtitle.set_visible(false);
        install_album_list_item_context_menu(&cell, &setup_shell, item, playback_context.clone());
        item.set_child(Some(&cell));
    });

    let bind_shell = Rc::clone(&shell);
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = list_cell::<RecycledMergedCell>(item) else {
            return;
        };
        let cover = cell.cover();
        let Some(album) = item_at_from_item::<AlbumRow>(item) else {
            cell.title().set_text("");
            cell.downloaded()
                .expect("album cell badge")
                .set_visible(false);
            cell.clear_subtitle();
            clear_merged_artwork(&bind_shell, &cover);
            return;
        };
        set_cover_placeholder(&bind_shell, &cover, false);
        bind_shell.bind_artwork_tile(
            &cover,
            opaque_artwork(album.artwork_binding.as_deref()),
            48,
            THUMB_COVER_SIZE,
        );
        cell.title().set_text(&album.title);
        cell.bind_subtitle(album_artist_links(&album));
        cell.subtitle()
            .set_visible(!album.display_artist.trim().is_empty());
        bind_shell.bind_download_badge(
            &cell.downloaded().expect("album cell badge"),
            collection_is_downloaded(album.track_count, album.downloaded_count),
        );
    });

    let unbind_shell = Rc::clone(&shell);
    factory.connect_unbind(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>()
            && let Some(cell) = list_cell::<RecycledMergedCell>(item)
        {
            cell.title().set_text("");
            cell.downloaded()
                .expect("album cell badge")
                .set_visible(false);
            cell.clear_subtitle();
            clear_merged_artwork(&unbind_shell, &cell.cover());
        }
    });

    let column = localized_column(title, &factory);
    column.set_fixed_width(width);
    column
}

fn install_album_list_item_context_menu(
    target: &impl IsA<gtk::Widget>,
    shell: &Rc<Shell>,
    item: &gtk::ListItem,
    playback_context: Option<String>,
) {
    let item = item.downgrade();
    let shell = Rc::clone(shell);
    install_context_menu_openers(
        target,
        Rc::new(move |target, position| {
            let Some(album) = item.upgrade().and_then(|item| item_at_from_item(&item)) else {
                return;
            };
            present_album_context_menu(
                target,
                &shell,
                album,
                playback_context.clone(),
                None,
                position,
            );
        }),
    );
}

fn install_artist_list_item_context_menu(
    target: &impl IsA<gtk::Widget>,
    shell: &Rc<Shell>,
    item: &gtk::ListItem,
    album_artist: bool,
) {
    let item = item.downgrade();
    let shell = Rc::clone(shell);
    install_context_menu_openers(
        target,
        Rc::new(move |target, position| {
            let Some(artist) = item.upgrade().and_then(|item| item_at_from_item(&item)) else {
                return;
            };
            present_artist_context_menu(target, &shell, artist, album_artist, None, position);
        }),
    );
}

pub(crate) fn artwork_column<T, F>(
    shell: &Rc<Shell>,
    title: &str,
    width: i32,
    candidates: F,
) -> gtk::ColumnViewColumn
where
    T: Clone + 'static,
    F: Fn(&T) -> ArtworkBinding + 'static,
{
    let factory = gtk::SignalListItemFactory::new();
    let shell = Rc::clone(shell);
    let candidates = Rc::new(candidates);

    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        item.set_child(Some(&RecycledArtworkCell::new(48)));
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
        let Some(data) = item_at_from_item::<T>(item) else {
            set_cover_placeholder(&bind_shell, &cover, true);
            return;
        };
        set_cover_placeholder(&bind_shell, &cover, false);
        bind_shell.bind_artwork_tile(&cover, candidates(&data), 48, THUMB_COVER_SIZE);
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
pub(crate) fn artist_image_column(shell: &Rc<Shell>, album_artist: bool) -> gtk::ColumnViewColumn {
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
        let Some(artist) = item_at_from_item::<ArtistRow>(item) else {
            set_cover_placeholder(&bind_shell, &cover, true);
            return;
        };
        set_cover_placeholder(&bind_shell, &cover, false);
        bind_shell.bind_artwork_tile(
            &cover,
            opaque_artwork(artist.artwork_binding.as_deref()),
            48,
            THUMB_COVER_SIZE,
        );
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
pub(crate) fn artist_text_column<F>(
    shell: &Rc<Shell>,
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
    let shell = Rc::clone(shell);
    let value = Rc::new(value);

    let setup_shell = Rc::clone(&shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = RecycledBadgedTextCell::for_shell(&setup_shell);
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
        let Some(artist) = item_at_from_item::<ArtistRow>(item) else {
            cell.clear();
            return;
        };
        cell.label().set_text(&(value)(&artist));
        bind_shell.bind_download_badge(
            &cell.downloaded(),
            collection_is_downloaded(artist.track_count, artist.downloaded_count),
        );
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
    shell: &Rc<Shell>,
    item: &gtk::ListItem,
    track_value: Rc<dyn Fn(&T) -> Option<TrackRow>>,
) {
    let item = item.downgrade();
    let shell = Rc::clone(shell);
    install_context_menu_openers(
        target,
        Rc::new(move |target, position| {
            let Some(track) = item
                .upgrade()
                .and_then(|item| item_at_from_item::<T>(&item))
                .and_then(|value| track_value(&value))
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

pub(crate) fn track_image_column(
    shell: &Rc<Shell>,
    title: &'static str,
    width: i32,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let shell = Rc::clone(shell);

    let setup_shell = Rc::clone(&shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = RecycledArtworkCell::new(48);
        install_track_list_item_context_menu(
            &cell,
            &setup_shell,
            item,
            Rc::new(|track: &TrackRow| Some(track.clone())),
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
        let Some(_track) = item_at_from_item::<TrackRow>(item) else {
            set_cover_placeholder(&bind_shell, &cover, true);
            return;
        };
        let Some(artwork) = track_artwork_at_from_item(item) else {
            return;
        };
        set_cover_placeholder(&bind_shell, &cover, false);
        bind_shell.bind_artwork_tile(&cover, artwork, 48, THUMB_COVER_SIZE);
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
pub(crate) fn track_text_column<F>(
    shell: &Rc<Shell>,
    field: LibraryField,
    title: &'static str,
    width: i32,
    xalign: f32,
    playing: Option<TrackRowPlayingIndicator>,
    value: F,
) -> gtk::ColumnViewColumn
where
    F: Fn(&TrackRow) -> String + 'static,
{
    track_position_text_column(
        shell,
        field,
        title,
        width,
        xalign,
        playing,
        move |_, track| value(track),
    )
}

pub(crate) fn track_position_text_column<F>(
    shell: &Rc<Shell>,
    field: LibraryField,
    title: &'static str,
    width: i32,
    xalign: f32,
    playing: Option<TrackRowPlayingIndicator>,
    value: F,
) -> gtk::ColumnViewColumn
where
    F: Fn(u32, &TrackRow) -> String + 'static,
{
    let factory = gtk::SignalListItemFactory::new();
    let shell = Rc::clone(shell);
    let value = Rc::new(value);

    let setup_shell = Rc::clone(&shell);
    let setup_playing = playing.clone();
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let (child, label): (gtk::Widget, gtk::Label) = if setup_playing.is_some() {
            let cell = RecycledBadgedTextCell::for_shell(&setup_shell);
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
            Rc::new(|track: &TrackRow| Some(track.clone())),
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
        let Some(track) = item_at_from_item::<TrackRow>(item) else {
            label.set_text("");
            if let Some(badge) = downloaded.as_ref() {
                badge.set_visible(false);
            }
            if let Some(playing) = bind_playing.as_ref() {
                playing.unbind(label.upcast_ref());
            }
            return;
        };
        label.set_text(&(value)(item.position(), &track));
        if let Some(downloaded) = downloaded.as_ref() {
            bind_shell.bind_download_badge(downloaded, track.is_downloaded);
        }
        if let Some(playing) = bind_playing.as_ref() {
            playing.bind(label.upcast_ref(), item.position());
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

pub(crate) struct TrackMergedColumnValues<ItemTrack, Artwork, Title, Subtitle, SubtitleLinks> {
    pub(crate) track: ItemTrack,
    pub(crate) artwork: Artwork,
    pub(crate) title: Title,
    pub(crate) subtitle: Subtitle,
    pub(crate) subtitle_links: SubtitleLinks,
    pub(crate) context_menu: bool,
}

pub(crate) fn track_merged_column<T, ItemTrack, Artwork, Title, Subtitle, SubtitleLinks>(
    shell: &Rc<Shell>,
    title: &'static str,
    width: i32,
    playing: TrackRowPlayingIndicator,
    values: TrackMergedColumnValues<ItemTrack, Artwork, Title, Subtitle, SubtitleLinks>,
) -> gtk::ColumnViewColumn
where
    T: Clone + 'static,
    ItemTrack: Fn(&T) -> TrackRow + 'static,
    Artwork: Fn(&T) -> ArtworkBinding + 'static,
    Title: Fn(&T) -> String + 'static,
    Subtitle: Fn(&T) -> String + 'static,
    SubtitleLinks: Fn(&T) -> Option<DetailLinks> + 'static,
{
    let factory = gtk::SignalListItemFactory::new();
    let shell = Rc::clone(shell);
    let TrackMergedColumnValues {
        track: item_track,
        artwork: artwork_value,
        title: title_value,
        subtitle: subtitle_value,
        subtitle_links,
        context_menu,
    } = values;
    let title_value = Rc::new(title_value);
    let item_track = Rc::new(item_track);
    let artwork_value = Rc::new(artwork_value);
    let subtitle_value = Rc::new(subtitle_value);
    let subtitle_links = Rc::new(subtitle_links);
    let context_track: Rc<dyn Fn(&T) -> Option<TrackRow>> = {
        let item_track = Rc::clone(&item_track);
        Rc::new(move |value| Some(item_track(value)))
    };

    let setup_shell = Rc::clone(&shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = RecycledMergedCell::new(&setup_shell, 48, true);
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
        let Some(value) = item_at_from_item::<T>(item) else {
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
        let track = item_track(&value);
        let artwork = artwork_value(&value);
        bind_shell.bind_artwork_tile(&cover, artwork, 48, THUMB_COVER_SIZE);
        title.set_text(&title_value(&value));
        let subtitle = subtitle_value(&value);
        let subtitle_links = subtitle_links(&value);
        bind_shell.bind_download_badge(
            &cell.downloaded().expect("track cell badge"),
            track.is_downloaded,
        );
        bind_playing.bind(title.upcast_ref(), item.position());
        if subtitle.trim().is_empty() {
            cell.clear_subtitle();
        } else {
            cell.bind_subtitle(subtitle_links.unwrap_or_else(|| DetailLinks::text(&subtitle)));
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
    item.child()?.downcast::<gtk::Button>().ok()
}

pub(crate) fn album_favorite_column(
    shell: &Rc<Shell>,
    playback_context: Option<String>,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let shell = Rc::clone(shell);

    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let button = column_favorite_icon_button("Favorite album");
        set_placeholder_favorite(&button, None);
        let favorite_item = item.downgrade();
        shell.register_dynamic_favorite_button(
            Rc::new(move || {
                favorite_item
                    .upgrade()
                    .and_then(|item| item_at_from_item::<AlbumRow>(&item))
                    .map(|album| album_favorite_key(&album.album_key))
            }),
            &button,
        );
        install_album_list_item_context_menu(&button, &shell, item, playback_context.clone());
        let favorite_shell = Rc::clone(&shell);
        let click_item = item.downgrade();
        button.connect_clicked(move |button| {
            let Some(album) = click_item
                .upgrade()
                .and_then(|item| item_at_from_item::<AlbumRow>(&item))
            else {
                return;
            };
            let favorite = !favorite_button_is_active(button);
            favorite_shell.set_favorite_with_feedback(
                library::FavoriteTarget::Album(album.album_key.clone()),
                favorite,
                Some(button),
            );
        });
        item.set_child(Some(&button));
    });

    connect_sparse_bind(&factory, |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(button) = favorite_cell_button(item) else {
            return;
        };
        let Some(album) = item_at_from_item::<AlbumRow>(item) else {
            set_placeholder_favorite(&button, None);
            return;
        };
        set_placeholder_favorite(&button, Some(album.favorite));
    });

    factory.connect_unbind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(button) = favorite_cell_button(item) {
            set_placeholder_favorite(&button, None);
        }
    });
    let column = gtk::ColumnViewColumn::new(Some(FAVORITE_COLUMN_TITLE), Some(factory));
    column.set_fixed_width(column_width(LibraryField::Favorite));
    column
}
pub(crate) fn artist_favorite_column(
    shell: &Rc<Shell>,
    album_artist: bool,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let shell = Rc::clone(shell);

    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let button = column_favorite_icon_button("Favorite artist");
        set_placeholder_favorite(&button, None);
        let favorite_item = item.downgrade();
        shell.register_dynamic_favorite_button(
            Rc::new(move || {
                favorite_item
                    .upgrade()
                    .and_then(|item| item_at_from_item::<ArtistRow>(&item))
                    .map(|artist| artist_favorite_key(&artist.artist_key))
            }),
            &button,
        );
        install_artist_list_item_context_menu(&button, &shell, item, album_artist);
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
                library::FavoriteTarget::Artist(artist.artist_key.clone()),
                favorite,
                Some(button),
            );
        });
        item.set_child(Some(&button));
    });

    connect_sparse_bind(&factory, |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(button) = favorite_cell_button(item) else {
            return;
        };
        let Some(artist) = item_at_from_item::<ArtistRow>(item) else {
            set_placeholder_favorite(&button, None);
            return;
        };
        set_placeholder_favorite(&button, Some(artist.favorite));
    });

    factory.connect_unbind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(button) = favorite_cell_button(item) {
            set_placeholder_favorite(&button, None);
        }
    });
    let column = gtk::ColumnViewColumn::new(Some(FAVORITE_COLUMN_TITLE), Some(factory));
    column.set_fixed_width(column_width(LibraryField::Favorite));
    column
}
pub(crate) fn track_favorite_column(shell: &Rc<Shell>) -> gtk::ColumnViewColumn {
    mapped_track_favorite_column(shell, |track: &TrackRow| Some(track.clone()))
}

pub(crate) fn mapped_track_favorite_column<T, TrackValue>(
    shell: &Rc<Shell>,
    track_value: TrackValue,
) -> gtk::ColumnViewColumn
where
    T: Clone + 'static,
    TrackValue: Fn(&T) -> Option<TrackRow> + 'static,
{
    let factory = gtk::SignalListItemFactory::new();
    let shell = Rc::clone(shell);
    let track_value: Rc<dyn Fn(&T) -> Option<TrackRow>> = Rc::new(track_value);

    let setup_shell = Rc::clone(&shell);
    let setup_track_value = Rc::clone(&track_value);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let button = column_favorite_icon_button("Favorite track");
        set_placeholder_favorite(&button, None);
        install_track_list_item_context_menu(
            &button,
            &setup_shell,
            item,
            Rc::clone(&setup_track_value),
        );
        let favorite_item = item.downgrade();
        let favorite_track_value = Rc::clone(&setup_track_value);
        setup_shell.register_dynamic_favorite_button(
            Rc::new(move || {
                favorite_item
                    .upgrade()
                    .and_then(|item| item_at_from_item::<T>(&item))
                    .and_then(|value| favorite_track_value(&value))
                    .map(|track| track_favorite_key(&track.track_key))
            }),
            &button,
        );
        let favorite_shell = Rc::clone(&setup_shell);
        let click_item = item.downgrade();
        let click_track_value = Rc::clone(&setup_track_value);
        button.connect_clicked(move |button| {
            let Some(track) = click_item
                .upgrade()
                .and_then(|item| item_at_from_item::<T>(&item))
                .and_then(|value| click_track_value(&value))
            else {
                return;
            };
            let favorite = !favorite_button_is_active(button);
            favorite_shell.set_favorite_with_feedback(
                library::FavoriteTarget::Track(track.track_key.clone()),
                favorite,
                Some(button),
            );
        });
        item.set_child(Some(&button));
    });

    let bind_shell = Rc::clone(&shell);
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(button) = favorite_cell_button(item) else {
            return;
        };
        let Some(value) = item_at_from_item::<T>(item) else {
            set_placeholder_favorite(&button, None);
            return;
        };
        let track = track_value(&value);
        let favorite = track.as_ref().is_some_and(|track| {
            bind_shell.projected_track_favorite(&track.track_key, track.favorite)
        });
        set_placeholder_favorite(&button, track.as_ref().map(|_| favorite));
    });

    factory.connect_unbind(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>()
            && let Some(button) = favorite_cell_button(item)
        {
            set_placeholder_favorite(&button, None);
        }
    });

    let column = gtk::ColumnViewColumn::new(Some(FAVORITE_COLUMN_TITLE), Some(factory));
    column.set_fixed_width(column_width(LibraryField::Favorite));
    column
}
