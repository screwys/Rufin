use std::{cell::RefCell, collections::HashMap, rc::Rc};

use ::library::{AlbumRow, ArtistRow, PlaylistRow, SmartPlaylistRow, TrackRow};
use adw::prelude::*;
use artwork::ArtworkBinding;
use gtk::glib;

use super::collection_context::{
    install_dynamic_album_context_menu, install_dynamic_track_context_menu,
    present_album_context_menu, present_artist_context_menu,
};
use crate::favorites::{
    album_favorite_key, artist_favorite_key, favorite_button_is_active, favorite_icon_button,
    set_favorite_button_active, track_favorite_key,
};
use crate::interactions::install_context_menu_openers;
use crate::localization::localized_column;
use crate::shell::Shell;
use crate::shell::cover::{ArtworkTile, THUMB_COVER_SIZE};
use crate::{LibraryField, LibraryListKey};

use super::detail_links::{
    DetailLinkBinding, DetailLinks, album_artist_links, track_album_artist_links,
    track_artist_links,
};
use super::factory_cells::FactoryCells;
use super::library_fields::{
    album_field, artist_field, column_width, item_at_from_item, opaque_artwork,
    play_count_column_width, playlist_artwork, playlist_field, smart_playlist_display_name,
    smart_playlist_field, track_artwork_at_from_item, track_field,
};
use super::route::Route;
use super::sparse_model::connect_sparse_bind;
use super::table_links::track_link_column;

pub(crate) const ROW_INDEX_COLUMN_TITLE: &str = "\u{2003}\u{a0}#";
pub(crate) const ALBUM_DETAIL_DURATION_COLUMN_WIDTH: i32 = 48;

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
        LibraryField::Title => album_text_column(shell, "Title", 220, playback_context, |album| {
            album.title.clone()
        }),
        LibraryField::Favorite => album_favorite_column(shell, playback_context),
        _ => album_text_column(
            shell,
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
            artist_text_column(shell, "Title", 220, album_artist, |artist| {
                artist.name.clone()
            })
        }
        LibraryField::Favorite => artist_favorite_column(shell, album_artist),
        _ => artist_text_column(
            shell,
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
        _ => text_column::<PlaylistRow, _>(field.title(), column_width(field), move |playlist| {
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
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        row.set_hexpand(true);
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        label.set_halign(gtk::Align::Start);
        label.set_hexpand(false);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_single_line_mode(true);
        row.append(&label);
        let downloaded = setup_shell.download_badge(true);
        row.append(&downloaded);
        item.set_child(Some(&row));
    });
    let bind_shell = Rc::clone(shell);
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some((label, downloaded)) = list_item_title_and_downloaded(item) else {
            return;
        };
        let Some(playlist) = item_at_from_item::<PlaylistRow>(item) else {
            label.set_text("");
            downloaded.set_visible(false);
            return;
        };
        label.set_text(&(value)(&playlist));
        bind_shell.bind_download_badge(
            &downloaded,
            collection_is_downloaded(playlist.track_count, playlist.downloaded_count),
        );
    });
    factory.connect_unbind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some((label, downloaded)) = list_item_title_and_downloaded(item) {
            label.set_text("");
            downloaded.set_visible(false);
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
        _ => text_column::<SmartPlaylistRow, _>(
            field.title(),
            column_width(field),
            move |playlist| smart_playlist_field(playlist, field),
        ),
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
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        row.set_hexpand(true);
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        label.set_halign(gtk::Align::Start);
        label.set_hexpand(false);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_single_line_mode(true);
        row.append(&label);
        let downloaded = setup_shell.download_badge(true);
        row.append(&downloaded);
        item.set_child(Some(&row));
    });
    let bind_shell = Rc::clone(shell);
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some((label, downloaded)) = list_item_title_and_downloaded(item) else {
            return;
        };
        let Some(playlist) = item_at_from_item::<SmartPlaylistRow>(item) else {
            label.set_text("");
            downloaded.set_visible(false);
            return;
        };
        label.set_text(&(value)(&playlist));
        bind_shell.bind_download_badge(
            &downloaded,
            collection_is_downloaded(playlist.track_count, playlist.downloaded_count),
        );
    });
    factory.connect_unbind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some((label, downloaded)) = list_item_title_and_downloaded(item) {
            label.set_text("");
            downloaded.set_visible(false);
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
            let column =
                track_text_column(shell, "Title", width, 0.0, Some(playing.clone()), |track| {
                    track.title.clone()
                });
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
        LibraryField::Duration => track_text_column(shell, "◷", width, 0.0, None, |track| {
            track_field(track, LibraryField::Duration)
        }),
        _ => track_text_column(shell, field.title(), width, 0.0, None, move |track| {
            track_field(track, field)
        }),
    }
}
pub(crate) fn track_column_fit_width(key: LibraryListKey, field: LibraryField) -> i32 {
    column_fit_width(field, track_column_width(key, field))
}
pub(crate) fn track_column_width(key: LibraryListKey, field: LibraryField) -> i32 {
    if key == LibraryListKey::AlbumDetailTracks && field == LibraryField::Duration {
        return ALBUM_DETAIL_DURATION_COLUMN_WIDTH;
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
        LibraryField::Favorite => 48,
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
        LibraryField::Favorite => 76,
        _ => column_width(field),
    }
}

pub(crate) fn text_column<T, F>(title: &str, width: i32, value: F) -> gtk::ColumnViewColumn
where
    T: Clone + 'static,
    F: Fn(&T) -> String + 'static,
{
    let factory = gtk::SignalListItemFactory::new();
    let value = Rc::new(value);
    factory.connect_setup(|_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            let label = gtk::Label::new(None);
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
    let column = localized_column(title, &factory);
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

#[derive(Clone)]
struct LibraryArtworkCell {
    cover: ArtworkTile,
}

#[derive(Clone)]
pub(crate) struct LibraryAlbumImageCell {
    pub(crate) cover: ArtworkTile,
    pub(crate) current_album: Rc<RefCell<Option<AlbumRow>>>,
}

#[derive(Clone)]
pub(crate) struct LibraryAlbumTextCell {
    pub(crate) label: gtk::Label,
    downloaded: gtk::Image,
    pub(crate) current_album: Rc<RefCell<Option<AlbumRow>>>,
}

#[derive(Clone)]
pub(crate) struct LibraryAlbumMergedCell {
    pub(crate) cover: ArtworkTile,
    pub(crate) title: gtk::Label,
    pub(crate) subtitle: gtk::Label,
    downloaded: gtk::Image,
    pub(crate) subtitle_links: DetailLinkBinding,
    pub(crate) current_album: Rc<RefCell<Option<AlbumRow>>>,
}

pub(crate) fn album_image_column(
    shell: &Rc<Shell>,
    title: &'static str,
    width: i32,
    playback_context: Option<String>,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let cells = FactoryCells::<LibraryAlbumImageCell>::new();
    let shell = Rc::clone(shell);

    let setup_shell = Rc::clone(&shell);
    let setup_cells = cells.clone();
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let current_album = Rc::new(RefCell::new(None::<AlbumRow>));
        let cover = ArtworkTile::new(48);
        let widget = cover.widget();
        install_dynamic_album_context_menu(
            &widget,
            &setup_shell,
            Rc::clone(&current_album),
            playback_context.clone(),
        );
        item.set_child(Some(&widget));
        setup_cells.insert(
            item,
            LibraryAlbumImageCell {
                cover,
                current_album,
            },
        );
    });

    let bind_shell = Rc::clone(&shell);
    let bind_cells = cells.clone();
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = bind_cells.get(item) else {
            return;
        };
        let Some(album) = item_at_from_item::<AlbumRow>(item) else {
            set_cover_placeholder(&bind_shell, &cell.cover, true);
            *cell.current_album.borrow_mut() = None;
            return;
        };
        set_cover_placeholder(&bind_shell, &cell.cover, false);
        bind_shell.bind_artwork_tile(
            &cell.cover,
            opaque_artwork(album.artwork_binding.as_deref()),
            48,
            THUMB_COVER_SIZE,
        );
        *cell.current_album.borrow_mut() = Some(album);
    });

    let unbind_shell = Rc::clone(&shell);
    let unbind_cells = cells.clone();
    factory.connect_unbind(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>()
            && let Some(cell) = unbind_cells.get(item)
        {
            set_cover_placeholder(&unbind_shell, &cell.cover, true);
            *cell.current_album.borrow_mut() = None;
        }
    });

    let teardown_cells = cells.clone();
    factory.connect_teardown(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            teardown_cells.remove(item);
        }
    });

    let column = localized_column(title, &factory);
    column.set_fixed_width(width);
    column
}

pub(crate) fn album_text_column<F>(
    shell: &Rc<Shell>,
    title: &'static str,
    width: i32,
    playback_context: Option<String>,
    value: F,
) -> gtk::ColumnViewColumn
where
    F: Fn(&AlbumRow) -> String + 'static,
{
    let factory = gtk::SignalListItemFactory::new();
    let cells = FactoryCells::<LibraryAlbumTextCell>::new();
    let shell = Rc::clone(shell);
    let value = Rc::new(value);

    let setup_shell = Rc::clone(&shell);
    let setup_cells = cells.clone();
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let current_album = Rc::new(RefCell::new(None::<AlbumRow>));
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        label.set_halign(gtk::Align::Start);
        label.set_hexpand(false);
        label.set_wrap(false);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_single_line_mode(true);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        row.set_hexpand(true);
        row.append(&label);
        let downloaded = setup_shell.download_badge(true);
        row.append(&downloaded);
        install_dynamic_album_context_menu(
            &row,
            &setup_shell,
            Rc::clone(&current_album),
            playback_context.clone(),
        );
        item.set_child(Some(&row));
        setup_cells.insert(
            item,
            LibraryAlbumTextCell {
                label,
                downloaded,
                current_album,
            },
        );
    });

    let bind_shell = Rc::clone(&shell);
    let bind_cells = cells.clone();
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = bind_cells.get(item) else {
            return;
        };
        let Some(album) = item_at_from_item::<AlbumRow>(item) else {
            cell.label.set_text("");
            cell.downloaded.set_visible(false);
            *cell.current_album.borrow_mut() = None;
            return;
        };
        cell.label.set_text(&(value)(&album));
        bind_shell.bind_download_badge(
            &cell.downloaded,
            collection_is_downloaded(album.track_count, album.downloaded_count),
        );
        *cell.current_album.borrow_mut() = Some(album);
    });

    let unbind_cells = cells.clone();
    factory.connect_unbind(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>()
            && let Some(cell) = unbind_cells.get(item)
        {
            cell.label.set_text("");
            cell.downloaded.set_visible(false);
            *cell.current_album.borrow_mut() = None;
        }
    });

    let teardown_cells = cells.clone();
    factory.connect_teardown(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            teardown_cells.remove(item);
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
    let cells = FactoryCells::<LibraryAlbumMergedCell>::new();
    let shell = Rc::clone(shell);

    let setup_shell = Rc::clone(&shell);
    let setup_cells = cells.clone();
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let current_album = Rc::new(RefCell::new(None::<AlbumRow>));
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.set_valign(gtk::Align::Center);

        let cover = ArtworkTile::new(48);
        row.append(&cover.widget());

        let labels = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let title = gtk::Label::new(None);
        title.set_xalign(0.0);
        title.set_wrap(false);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.set_single_line_mode(true);
        let title_row = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        title_row.append(&title);
        let downloaded = setup_shell.download_badge(true);
        title_row.append(&downloaded);
        labels.append(&title_row);

        let subtitle = gtk::Label::new(None);
        subtitle.add_css_class("artist-label");
        subtitle.set_xalign(0.0);
        subtitle.set_halign(gtk::Align::Start);
        subtitle.set_hexpand(false);
        subtitle.set_wrap(false);
        subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
        subtitle.set_single_line_mode(true);
        subtitle.set_visible(false);
        let subtitle_links = DetailLinkBinding::new(&subtitle, &setup_shell);
        labels.append(&subtitle);

        row.append(&labels);
        install_dynamic_album_context_menu(
            &row,
            &setup_shell,
            Rc::clone(&current_album),
            playback_context.clone(),
        );
        item.set_child(Some(&row));
        setup_cells.insert(
            item,
            LibraryAlbumMergedCell {
                cover,
                title,
                subtitle,
                downloaded,
                subtitle_links,
                current_album,
            },
        );
    });

    let bind_shell = Rc::clone(&shell);
    let bind_cells = cells.clone();
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = bind_cells.get(item) else {
            return;
        };
        let Some(album) = item_at_from_item::<AlbumRow>(item) else {
            cell.title.set_text("");
            cell.downloaded.set_visible(false);
            cell.subtitle_links.clear();
            cell.subtitle.set_visible(false);
            clear_merged_artwork(&bind_shell, &cell.cover);
            *cell.current_album.borrow_mut() = None;
            return;
        };
        set_cover_placeholder(&bind_shell, &cell.cover, false);
        bind_shell.bind_artwork_tile(
            &cell.cover,
            opaque_artwork(album.artwork_binding.as_deref()),
            48,
            THUMB_COVER_SIZE,
        );
        cell.title.set_text(&album.title);
        cell.subtitle_links.bind(album_artist_links(&album));
        cell.subtitle
            .set_visible(!album.display_artist.trim().is_empty());
        bind_shell.bind_download_badge(
            &cell.downloaded,
            collection_is_downloaded(album.track_count, album.downloaded_count),
        );
        *cell.current_album.borrow_mut() = Some(album);
    });

    let unbind_shell = Rc::clone(&shell);
    let unbind_cells = cells.clone();
    factory.connect_unbind(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>()
            && let Some(cell) = unbind_cells.get(item)
        {
            cell.title.set_text("");
            cell.downloaded.set_visible(false);
            cell.subtitle_links.clear();
            cell.subtitle.set_visible(false);
            clear_merged_artwork(&unbind_shell, &cell.cover);
            *cell.current_album.borrow_mut() = None;
        }
    });

    let teardown_cells = cells.clone();
    factory.connect_teardown(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            teardown_cells.remove(item);
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
    let cells = FactoryCells::<LibraryArtworkCell>::new();
    let shell = Rc::clone(shell);
    let candidates = Rc::new(candidates);

    let setup_cells = cells.clone();
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cover = ArtworkTile::new(48);
        item.set_child(Some(&cover.widget()));
        setup_cells.insert(item, LibraryArtworkCell { cover });
    });

    let bind_shell = Rc::clone(&shell);
    let bind_cells = cells.clone();
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = bind_cells.get(item) else {
            return;
        };
        let Some(data) = item_at_from_item::<T>(item) else {
            set_cover_placeholder(&bind_shell, &cell.cover, true);
            return;
        };
        set_cover_placeholder(&bind_shell, &cell.cover, false);
        bind_shell.bind_artwork_tile(&cell.cover, candidates(&data), 48, THUMB_COVER_SIZE);
    });
    let unbind_shell = Rc::clone(&shell);
    let unbind_cells = cells.clone();
    factory.connect_unbind(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>()
            && let Some(cell) = unbind_cells.get(item)
        {
            set_cover_placeholder(&unbind_shell, &cell.cover, true);
        }
    });
    let teardown_cells = cells.clone();
    factory.connect_teardown(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            teardown_cells.remove(item);
        }
    });
    let column = localized_column(title, &factory);
    column.set_fixed_width(width);
    column
}
pub(crate) fn artist_image_column(shell: &Rc<Shell>, album_artist: bool) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let cells = FactoryCells::<LibraryArtworkCell>::new();
    let shell = Rc::clone(shell);

    let setup_shell = Rc::clone(&shell);
    let setup_cells = cells.clone();
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cover = ArtworkTile::new(48);
        let widget = cover.widget();
        install_artist_list_item_context_menu(&widget, &setup_shell, item, album_artist);
        item.set_child(Some(&widget));
        setup_cells.insert(item, LibraryArtworkCell { cover });
    });

    let bind_shell = Rc::clone(&shell);
    let bind_cells = cells.clone();
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = bind_cells.get(item) else {
            return;
        };
        let Some(artist) = item_at_from_item::<ArtistRow>(item) else {
            set_cover_placeholder(&bind_shell, &cell.cover, true);
            return;
        };
        set_cover_placeholder(&bind_shell, &cell.cover, false);
        bind_shell.bind_artwork_tile(
            &cell.cover,
            opaque_artwork(artist.artwork_binding.as_deref()),
            48,
            THUMB_COVER_SIZE,
        );
    });
    let unbind_shell = Rc::clone(&shell);
    let unbind_cells = cells.clone();
    factory.connect_unbind(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>()
            && let Some(cell) = unbind_cells.get(item)
        {
            set_cover_placeholder(&unbind_shell, &cell.cover, true);
        }
    });
    let teardown_cells = cells.clone();
    factory.connect_teardown(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            teardown_cells.remove(item);
        }
    });
    let column = localized_column("Image", &factory);
    column.set_fixed_width(column_width(LibraryField::Image));
    column
}
pub(crate) fn artist_text_column<F>(
    shell: &Rc<Shell>,
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
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        row.set_hexpand(true);
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        label.set_halign(gtk::Align::Start);
        label.set_hexpand(false);
        label.set_wrap(false);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_single_line_mode(true);
        row.append(&label);
        let downloaded = setup_shell.download_badge(true);
        row.append(&downloaded);
        install_artist_list_item_context_menu(&row, &setup_shell, item, album_artist);
        item.set_child(Some(&row));
    });

    let bind_shell = Rc::clone(&shell);
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some((label, downloaded)) = list_item_title_and_downloaded(item) else {
            return;
        };
        let Some(artist) = item_at_from_item::<ArtistRow>(item) else {
            label.set_text("");
            downloaded.set_visible(false);
            return;
        };
        label.set_text(&(value)(&artist));
        bind_shell.bind_download_badge(
            &downloaded,
            collection_is_downloaded(artist.track_count, artist.downloaded_count),
        );
    });
    factory.connect_unbind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some((label, downloaded)) = list_item_title_and_downloaded(item) {
            label.set_text("");
            downloaded.set_visible(false);
        }
    });
    let column = localized_column(title, &factory);
    column.set_fixed_width(width);
    column
}

fn list_item_title_and_downloaded(item: &gtk::ListItem) -> Option<(gtk::Label, gtk::Image)> {
    let row = item.child()?.downcast::<gtk::Box>().ok()?;
    let label = row.first_child()?.downcast::<gtk::Label>().ok()?;
    let downloaded = row.last_child()?.downcast::<gtk::Image>().ok()?;
    Some((label, downloaded))
}

#[derive(Clone)]
pub(crate) struct LibraryTrackImageCell {
    pub(crate) cover: ArtworkTile,
    pub(crate) current_track: Rc<RefCell<Option<TrackRow>>>,
}

#[derive(Clone)]
pub(crate) struct LibraryTrackTextCell {
    pub(crate) label: gtk::Label,
    downloaded: Option<gtk::Image>,
    pub(crate) current_track: Rc<RefCell<Option<TrackRow>>>,
}

#[derive(Clone)]
pub(crate) struct LibraryTrackMergedCell {
    pub(crate) cover: ArtworkTile,
    pub(crate) title: gtk::Label,
    pub(crate) subtitle: gtk::Label,
    downloaded: gtk::Image,
    pub(crate) subtitle_links: DetailLinkBinding,
    pub(crate) current_track: Rc<RefCell<Option<TrackRow>>>,
}

#[derive(Clone)]
pub(crate) struct LibraryTrackFavoriteCell {
    pub(crate) button: gtk::Button,
    pub(crate) current_track: Rc<RefCell<Option<TrackRow>>>,
}

fn install_track_cell_context_menu(
    target: &impl IsA<gtk::Widget>,
    shell: &Rc<Shell>,
    current_track: Rc<RefCell<Option<TrackRow>>>,
) {
    install_dynamic_track_context_menu(target, shell, current_track);
}

pub(crate) fn track_image_column(
    shell: &Rc<Shell>,
    title: &'static str,
    width: i32,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let cells = FactoryCells::<LibraryTrackImageCell>::new();
    let shell = Rc::clone(shell);

    let setup_shell = Rc::clone(&shell);
    let setup_cells = cells.clone();
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let current_track = Rc::new(RefCell::new(None::<TrackRow>));
        let cover = ArtworkTile::new(48);
        let widget = cover.widget();
        install_track_cell_context_menu(&widget, &setup_shell, Rc::clone(&current_track));
        item.set_child(Some(&widget));
        setup_cells.insert(
            item,
            LibraryTrackImageCell {
                cover,
                current_track,
            },
        );
    });

    let bind_shell = Rc::clone(&shell);
    let bind_cells = cells.clone();
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = bind_cells.get(item) else {
            return;
        };
        let Some(track) = item_at_from_item::<TrackRow>(item) else {
            *cell.current_track.borrow_mut() = None;
            set_cover_placeholder(&bind_shell, &cell.cover, true);
            return;
        };
        let Some(artwork) = track_artwork_at_from_item(item) else {
            return;
        };
        set_cover_placeholder(&bind_shell, &cell.cover, false);
        bind_shell.bind_artwork_tile(&cell.cover, artwork, 48, THUMB_COVER_SIZE);
        *cell.current_track.borrow_mut() = Some(track);
    });

    let unbind_shell = Rc::clone(&shell);
    let unbind_cells = cells.clone();
    factory.connect_unbind(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>()
            && let Some(cell) = unbind_cells.get(item)
        {
            *cell.current_track.borrow_mut() = None;
            set_cover_placeholder(&unbind_shell, &cell.cover, true);
        }
    });

    let teardown_cells = cells.clone();
    factory.connect_teardown(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            teardown_cells.remove(item);
        }
    });

    let column = localized_column(title, &factory);
    column.set_fixed_width(width);
    column
}
pub(crate) fn track_text_column<F>(
    shell: &Rc<Shell>,
    title: &'static str,
    width: i32,
    xalign: f32,
    playing: Option<TrackRowPlayingIndicator>,
    value: F,
) -> gtk::ColumnViewColumn
where
    F: Fn(&TrackRow) -> String + 'static,
{
    track_position_text_column(shell, title, width, xalign, playing, move |_, track| {
        value(track)
    })
}

pub(crate) fn track_position_text_column<F>(
    shell: &Rc<Shell>,
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
    let cells = FactoryCells::<LibraryTrackTextCell>::new();
    let shell = Rc::clone(shell);
    let value = Rc::new(value);

    let setup_shell = Rc::clone(&shell);
    let setup_playing = playing.clone();
    let setup_cells = cells.clone();
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let current_track = Rc::new(RefCell::new(None::<TrackRow>));
        let label = gtk::Label::new(None);
        if setup_playing.is_some() {
            label.add_css_class("track-list-title");
        }
        label.set_xalign(xalign);
        label.set_halign(gtk::Align::Start);
        label.set_hexpand(false);
        label.set_wrap(false);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_single_line_mode(true);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        row.set_hexpand(true);
        row.append(&label);
        let downloaded = setup_playing.as_ref().map(|_| {
            let downloaded = setup_shell.download_badge(false);
            row.append(&downloaded);
            downloaded
        });
        install_track_cell_context_menu(&row, &setup_shell, Rc::clone(&current_track));
        item.set_child(Some(&row));
        setup_cells.insert(
            item,
            LibraryTrackTextCell {
                label,
                downloaded,
                current_track,
            },
        );
    });

    let bind_shell = Rc::clone(&shell);
    let bind_playing = playing.clone();
    let bind_cells = cells.clone();
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = bind_cells.get(item) else {
            return;
        };
        let Some(track) = item_at_from_item::<TrackRow>(item) else {
            cell.label.set_text("");
            if let Some(badge) = cell.downloaded.as_ref() {
                badge.set_visible(false);
            }
            if let Some(playing) = bind_playing.as_ref() {
                playing.unbind(cell.label.upcast_ref());
            }
            *cell.current_track.borrow_mut() = None;
            return;
        };
        cell.label.set_text(&(value)(item.position(), &track));
        if let Some(downloaded) = cell.downloaded.as_ref() {
            bind_shell.bind_download_badge(downloaded, track.is_downloaded);
        }
        if let Some(playing) = bind_playing.as_ref() {
            playing.bind(cell.label.upcast_ref(), item.position());
        }
        *cell.current_track.borrow_mut() = Some(track);
    });

    let unbind_cells = cells.clone();
    factory.connect_unbind(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>()
            && let Some(cell) = unbind_cells.get(item)
        {
            cell.label.set_text("");
            if let Some(downloaded) = cell.downloaded.as_ref() {
                downloaded.set_visible(false);
            }
            if let Some(playing) = playing.as_ref() {
                playing.unbind(cell.label.upcast_ref());
            }
            *cell.current_track.borrow_mut() = None;
        }
    });

    let teardown_cells = cells.clone();
    factory.connect_teardown(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            teardown_cells.remove(item);
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
    let cells = FactoryCells::<LibraryTrackMergedCell>::new();
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

    let setup_shell = Rc::clone(&shell);
    let setup_cells = cells.clone();
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let current_track = Rc::new(RefCell::new(None::<TrackRow>));
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.set_valign(gtk::Align::Center);

        let cover = ArtworkTile::new(48);
        row.append(&cover.widget());

        let labels = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let title = gtk::Label::new(None);
        title.add_css_class("track-list-title");
        title.set_xalign(0.0);
        title.set_wrap(false);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.set_single_line_mode(true);
        let title_row = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        title_row.append(&title);
        let downloaded = setup_shell.download_badge(false);
        title_row.append(&downloaded);
        labels.append(&title_row);

        let subtitle = gtk::Label::new(None);
        subtitle.add_css_class("artist-label");
        subtitle.add_css_class("table-link-label");
        subtitle.set_xalign(0.0);
        subtitle.set_halign(gtk::Align::Start);
        subtitle.set_hexpand(false);
        subtitle.set_wrap(false);
        subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
        subtitle.set_single_line_mode(true);
        subtitle.set_width_chars(1);
        subtitle.set_max_width_chars(28);
        subtitle.set_visible(false);
        let subtitle_binding = DetailLinkBinding::new(&subtitle, &setup_shell);
        labels.append(&subtitle);

        row.append(&labels);
        if context_menu {
            install_track_cell_context_menu(&row, &setup_shell, Rc::clone(&current_track));
        }
        item.set_child(Some(&row));
        setup_cells.insert(
            item,
            LibraryTrackMergedCell {
                cover,
                title,
                subtitle,
                downloaded,
                subtitle_links: subtitle_binding,
                current_track,
            },
        );
    });

    let bind_shell = Rc::clone(&shell);
    let bind_playing = playing.clone();
    let bind_cells = cells.clone();
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = bind_cells.get(item) else {
            return;
        };
        let Some(value) = item_at_from_item::<T>(item) else {
            cell.title.set_text("");
            cell.downloaded.set_visible(false);
            bind_playing.unbind(cell.title.upcast_ref());
            cell.subtitle_links.clear();
            cell.subtitle.set_visible(false);
            clear_merged_artwork(&bind_shell, &cell.cover);
            *cell.current_track.borrow_mut() = None;
            return;
        };
        set_cover_placeholder(&bind_shell, &cell.cover, false);
        let track = item_track(&value);
        let artwork = artwork_value(&value);
        bind_shell.bind_artwork_tile(&cell.cover, artwork, 48, THUMB_COVER_SIZE);
        cell.title.set_text(&title_value(&value));
        let subtitle = subtitle_value(&value);
        let subtitle_links = subtitle_links(&value);
        bind_shell.bind_download_badge(&cell.downloaded, track.is_downloaded);
        bind_playing.bind(cell.title.upcast_ref(), item.position());
        *cell.current_track.borrow_mut() = Some(track);
        if subtitle.trim().is_empty() {
            cell.subtitle_links.clear();
            cell.subtitle.set_visible(false);
        } else {
            cell.subtitle_links
                .bind(subtitle_links.unwrap_or_else(|| DetailLinks::text(&subtitle)));
            cell.subtitle.set_visible(true);
        }
    });

    let unbind_shell = Rc::clone(&shell);
    let unbind_cells = cells.clone();
    factory.connect_unbind(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>()
            && let Some(cell) = unbind_cells.get(item)
        {
            cell.title.set_text("");
            cell.downloaded.set_visible(false);
            playing.unbind(cell.title.upcast_ref());
            cell.subtitle_links.clear();
            cell.subtitle.set_visible(false);
            clear_merged_artwork(&unbind_shell, &cell.cover);
            *cell.current_track.borrow_mut() = None;
        }
    });

    let teardown_cells = cells.clone();
    factory.connect_teardown(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            teardown_cells.remove(item);
        }
    });

    let column = localized_column(title, &factory);
    column.set_fixed_width(width);
    column
}
fn favorite_cell_button(item: &gtk::ListItem) -> Option<gtk::Button> {
    let child = item.child()?;
    child
        .clone()
        .downcast::<gtk::Button>()
        .ok()
        .or_else(|| child.first_child()?.downcast::<gtk::Button>().ok())
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
        let button = favorite_icon_button("Favorite album");
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
        let wrapper = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        wrapper.add_css_class("favorite-skeleton-cell");
        wrapper.set_hexpand(true);
        wrapper.set_halign(gtk::Align::Fill);
        wrapper.append(&button);
        item.set_child(Some(&wrapper));
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
    let column = gtk::ColumnViewColumn::new(Some(""), Some(factory));
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
        let button = favorite_icon_button("Favorite artist");
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
        let wrapper = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        wrapper.add_css_class("favorite-skeleton-cell");
        wrapper.set_hexpand(true);
        wrapper.set_halign(gtk::Align::Fill);
        wrapper.append(&button);
        item.set_child(Some(&wrapper));
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
    let column = gtk::ColumnViewColumn::new(Some(""), Some(factory));
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
    let cells = FactoryCells::<LibraryTrackFavoriteCell>::new();
    let shell = Rc::clone(shell);

    let setup_shell = Rc::clone(&shell);
    let setup_cells = cells.clone();
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let current_track = Rc::new(RefCell::new(None::<TrackRow>));
        let button = favorite_icon_button("Favorite track");
        set_placeholder_favorite(&button, None);
        install_track_cell_context_menu(&button, &setup_shell, Rc::clone(&current_track));
        let favorite_key_track = Rc::clone(&current_track);
        setup_shell.register_dynamic_favorite_button(
            Rc::new(move || {
                favorite_key_track
                    .borrow()
                    .as_ref()
                    .map(|track| track_favorite_key(&track.track_key))
            }),
            &button,
        );
        let favorite_shell = Rc::clone(&setup_shell);
        let click_track = Rc::clone(&current_track);
        button.connect_clicked(move |button| {
            let Some(track) = click_track.borrow().as_ref().cloned() else {
                return;
            };
            let favorite = !favorite_button_is_active(button);
            favorite_shell.set_favorite_with_feedback(
                library::FavoriteTarget::Track(track.track_key.clone()),
                favorite,
                Some(button),
            );
        });
        let wrapper = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        wrapper.add_css_class("favorite-skeleton-cell");
        wrapper.set_hexpand(true);
        wrapper.set_halign(gtk::Align::Fill);
        wrapper.append(&button);
        item.set_child(Some(&wrapper));
        setup_cells.insert(
            item,
            LibraryTrackFavoriteCell {
                button,
                current_track,
            },
        );
    });

    let bind_shell = Rc::clone(&shell);
    let bind_cells = cells.clone();
    connect_sparse_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = bind_cells.get(item) else {
            return;
        };
        let Some(value) = item_at_from_item::<T>(item) else {
            set_placeholder_favorite(&cell.button, None);
            *cell.current_track.borrow_mut() = None;
            return;
        };
        let track = track_value(&value);
        let favorite = track.as_ref().is_some_and(|track| {
            bind_shell.projected_track_favorite(&track.track_key, track.favorite)
        });
        set_placeholder_favorite(&cell.button, track.as_ref().map(|_| favorite));
        *cell.current_track.borrow_mut() = track;
    });

    let unbind_cells = cells.clone();
    factory.connect_unbind(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>()
            && let Some(cell) = unbind_cells.get(item)
        {
            set_placeholder_favorite(&cell.button, None);
            *cell.current_track.borrow_mut() = None;
        }
    });

    let teardown_cells = cells.clone();
    factory.connect_teardown(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            teardown_cells.remove(item);
        }
    });

    let column = gtk::ColumnViewColumn::new(Some(""), Some(factory));
    column.set_fixed_width(column_width(LibraryField::Favorite));
    column
}
