use std::{cell::RefCell, cmp::Ordering, rc::Rc};

use ::library::{
    AlbumSummary, ArtistSummary, PlaylistSummary, SmartPlaylist, SmartPlaylistSummary, Track,
};
use adw::prelude::*;
use artwork::ArtworkBinding;
use gtk::{gio, glib};

use crate::format_duration_units;
use crate::shell::Shell;
use crate::{LibraryField, LibraryLayout, LibraryListKey, LibraryListSettings};
use localization::{album_count_text, track_count_text};
use localization::{msgid, tr};

use super::album_detail::ALBUM_DETAIL_META_LABEL_HEIGHT;

pub(crate) fn smart_playlist_display_name(playlist: &SmartPlaylist) -> String {
    playlist
        .builtin
        .map(|builtin| tr(builtin.title()))
        .unwrap_or_else(|| playlist.name.clone())
}

pub(crate) fn sort_playlists(playlists: &mut [PlaylistSummary], settings: &LibraryListSettings) {
    playlists.sort_by(|left, right| {
        apply_desc(
            compare_playlist(left, right, settings.sort_key),
            settings.descending,
        )
    });
}
pub(crate) fn sort_smart_playlists(
    playlists: &mut [SmartPlaylistSummary],
    settings: &LibraryListSettings,
) {
    playlists.sort_by(|left, right| {
        apply_desc(
            compare_smart_playlist(left, right, settings.sort_key),
            settings.descending,
        )
    });
}
pub(crate) fn sort_tracks(tracks: &mut [Track], settings: &LibraryListSettings) {
    tracks.sort_by(|left, right| {
        ::library::compare_tracks(
            left,
            right,
            settings.sort_key.track_sort(),
            settings.descending,
        )
    });
}
pub(crate) fn compare_album(
    left: &AlbumSummary,
    right: &AlbumSummary,
    field: LibraryField,
) -> Ordering {
    let left_album = &left.album;
    let right_album = &right.album;
    match field {
        LibraryField::AlbumArtist => cmp_string(&left_album.artist, &right_album.artist),
        LibraryField::Year => left_album.year.cmp(&right_album.year),
        LibraryField::ReleaseDate => {
            cmp_option_string(&left_album.release_date, &right_album.release_date)
        }
        LibraryField::DateAdded => {
            cmp_option_string(&left_album.date_added, &right_album.date_added)
        }
        LibraryField::LastPlayed => {
            cmp_option_string(&left_album.last_played, &right_album.last_played)
        }
        LibraryField::PlayCount => cmp_option_u32(left_album.play_count, right_album.play_count),
        LibraryField::UserRating => cmp_option_u8(left_album.user_rating, right_album.user_rating),
        LibraryField::SongCount => left.track_count.cmp(&right.track_count),
        LibraryField::Duration => left.duration_seconds.cmp(&right.duration_seconds),
        LibraryField::Favorite => left_album.favorite.cmp(&right_album.favorite),
        _ => cmp_string(&left_album.title, &right_album.title),
    }
    .then_with(|| cmp_string(&left_album.title, &right_album.title))
}
pub(crate) fn compare_artist(
    left: &ArtistSummary,
    right: &ArtistSummary,
    field: LibraryField,
) -> Ordering {
    let left_artist = &left.artist;
    let right_artist = &right.artist;
    match field {
        LibraryField::AlbumCount => left.album_count.cmp(&right.album_count),
        LibraryField::SongCount => left.track_count.cmp(&right.track_count),
        LibraryField::LastPlayed => {
            cmp_option_string(&left_artist.last_played, &right_artist.last_played)
        }
        LibraryField::PlayCount => cmp_option_u32(left_artist.play_count, right_artist.play_count),
        LibraryField::UserRating => {
            cmp_option_u8(left_artist.user_rating, right_artist.user_rating)
        }
        LibraryField::Favorite => left_artist.favorite.cmp(&right_artist.favorite),
        _ => cmp_string(&left_artist.name, &right_artist.name),
    }
    .then_with(|| cmp_string(&left_artist.name, &right_artist.name))
}
pub(crate) fn compare_playlist(
    left: &PlaylistSummary,
    right: &PlaylistSummary,
    field: LibraryField,
) -> Ordering {
    match field {
        LibraryField::SongCount => left.track_count.cmp(&right.track_count),
        LibraryField::Duration => left.duration_seconds.cmp(&right.duration_seconds),
        _ => cmp_string(&left.playlist.name, &right.playlist.name),
    }
    .then_with(|| cmp_string(&left.playlist.name, &right.playlist.name))
}
pub(crate) fn compare_smart_playlist(
    left: &SmartPlaylistSummary,
    right: &SmartPlaylistSummary,
    field: LibraryField,
) -> Ordering {
    match field {
        LibraryField::RowIndex => left
            .smart_playlist
            .position
            .cmp(&right.smart_playlist.position),
        LibraryField::SongCount => left.track_count.cmp(&right.track_count),
        LibraryField::Duration => left.duration_seconds.cmp(&right.duration_seconds),
        _ => cmp_string(&left.smart_playlist.name, &right.smart_playlist.name),
    }
    .then_with(|| cmp_string(&left.smart_playlist.name, &right.smart_playlist.name))
}
pub(crate) fn album_field_missing(album: &AlbumSummary, field: LibraryField) -> bool {
    match field {
        LibraryField::ReleaseDate => album.album.release_date.is_none(),
        LibraryField::DateAdded => album.album.date_added.is_none(),
        LibraryField::LastPlayed => album.album.last_played.is_none(),
        LibraryField::PlayCount => album.album.play_count.is_none(),
        LibraryField::UserRating => album.album.user_rating.is_none(),
        _ => false,
    }
}
pub(crate) fn artist_field_missing(artist: &ArtistSummary, field: LibraryField) -> bool {
    match field {
        LibraryField::LastPlayed => artist.artist.last_played.is_none(),
        LibraryField::PlayCount => artist.artist.play_count.is_none(),
        LibraryField::UserRating => artist.artist.user_rating.is_none(),
        _ => false,
    }
}
pub(crate) fn album_item_field(album: &library::Album, field: LibraryField) -> String {
    match field {
        LibraryField::Title | LibraryField::TitleMerged => album.title.clone(),
        LibraryField::AlbumArtist | LibraryField::Artist => album.artist.clone(),
        LibraryField::Year => nonzero_year(album.year),
        LibraryField::ReleaseDate => album.release_date.clone().unwrap_or_default(),
        LibraryField::DateAdded => album.date_added.clone().unwrap_or_default(),
        LibraryField::LastPlayed => display_calendar_date(album.last_played.as_deref()),
        LibraryField::PlayCount => option_count(album.play_count),
        LibraryField::UserRating => option_rating(album.user_rating),
        LibraryField::Genre => album.genre_names().collect::<Vec<_>>().join(", "),
        LibraryField::Favorite => favorite_text(album.favorite),
        _ => String::new(),
    }
}
pub(crate) fn album_field(album: &AlbumSummary, field: LibraryField) -> String {
    match field {
        LibraryField::SongCount => track_count_text(album.track_count.into()),
        LibraryField::Duration => crate::format_duration(album.duration_seconds),
        _ => album_item_field(&album.album, field),
    }
}
pub(crate) fn artist_item_field(artist: &library::Artist, field: LibraryField) -> String {
    match field {
        LibraryField::Title | LibraryField::TitleMerged => artist.name.clone(),
        LibraryField::LastPlayed => display_calendar_date(artist.last_played.as_deref()),
        LibraryField::PlayCount => option_count(artist.play_count),
        LibraryField::UserRating => option_rating(artist.user_rating),
        LibraryField::Favorite => favorite_text(artist.favorite),
        _ => String::new(),
    }
}
pub(crate) fn artist_field(artist: &ArtistSummary, field: LibraryField) -> String {
    match field {
        LibraryField::AlbumCount => album_count_text(artist.album_count.into()),
        LibraryField::SongCount => track_count_text(artist.track_count.into()),
        _ => artist_item_field(&artist.artist, field),
    }
}
pub(crate) fn playlist_field(playlist: &PlaylistSummary, field: LibraryField) -> String {
    match field {
        LibraryField::Title | LibraryField::TitleMerged => playlist.playlist.name.clone(),
        LibraryField::SongCount => track_count_text(playlist.track_count.into()),
        LibraryField::Duration => format_duration_units(playlist.duration_seconds),
        _ => String::new(),
    }
}
pub(crate) fn smart_playlist_field(playlist: &SmartPlaylistSummary, field: LibraryField) -> String {
    match field {
        LibraryField::Title | LibraryField::TitleMerged => {
            smart_playlist_display_name(&playlist.smart_playlist)
        }
        LibraryField::SongCount if playlist.track_count > 0 => {
            track_count_text(playlist.track_count.into())
        }
        LibraryField::Duration if playlist.duration_seconds > 0 => {
            format_duration_units(playlist.duration_seconds)
        }
        _ => String::new(),
    }
}
pub(crate) fn track_field(track: &Track, field: LibraryField) -> String {
    match field {
        LibraryField::Title | LibraryField::TitleMerged => track.title.clone(),
        LibraryField::Artist => track.artist.clone(),
        LibraryField::AlbumArtist => joined_credits(track.album_artist_credits()),
        LibraryField::Album => track.album.clone(),
        LibraryField::Year => nonzero_year(track.year),
        LibraryField::ReleaseDate => track.release_date.clone().unwrap_or_default(),
        LibraryField::DateAdded => track.date_added.clone().unwrap_or_default(),
        LibraryField::LastPlayed => display_calendar_date(track.last_played.as_deref()),
        LibraryField::PlayCount => option_count(track.play_count),
        LibraryField::UserRating => option_rating(track.user_rating),
        LibraryField::Genre => track.genre_names().collect::<Vec<_>>().join(", "),
        LibraryField::Bpm => track.bpm.map(|bpm| bpm.to_string()).unwrap_or_default(),
        LibraryField::DiscNumber => track.disc_number.to_string(),
        LibraryField::TrackNumber => format!("{}-{:02}", track.disc_number, track.track_number),
        LibraryField::Duration => crate::format_duration(track.duration_seconds),
        LibraryField::Favorite => favorite_text(track.favorite),
        _ => String::new(),
    }
}
pub(crate) fn album_matches_query(album: &AlbumSummary, query: &str) -> bool {
    album.album.title.to_lowercase().contains(query)
        || album.album.artist.to_lowercase().contains(query)
        || album
            .album
            .genre_names()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
            .contains(query)
        || album.album.year.to_string().contains(query)
}
pub(crate) fn artist_matches_query(artist: &ArtistSummary, query: &str) -> bool {
    artist.artist.name.to_lowercase().contains(query)
}
pub(crate) fn playlist_matches_query(playlist: &PlaylistSummary, query: &str) -> bool {
    playlist.playlist.name.to_lowercase().contains(query)
}
pub(crate) fn smart_playlist_matches_query(playlist: &SmartPlaylistSummary, query: &str) -> bool {
    playlist.smart_playlist.name.to_lowercase().contains(query)
}
fn boxed_item<T: Clone + 'static>(boxed: &glib::BoxedAnyObject) -> Option<T> {
    boxed.try_borrow::<T>().ok().map(|item| item.clone())
}

pub(crate) fn item_at<T: Clone + 'static>(
    model: &impl IsA<gio::ListModel>,
    position: u32,
) -> Option<T> {
    model
        .item(position)
        .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
        .and_then(|boxed| boxed_item(&boxed))
}
pub(crate) fn item_at_from_item<T: Clone + 'static>(item: &gtk::ListItem) -> Option<T> {
    item.item()
        .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
        .and_then(|boxed| boxed_item(&boxed))
}
pub(crate) fn track_artwork_at_from_item(item: &gtk::ListItem) -> Option<ArtworkBinding> {
    let boxed = item
        .item()
        .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())?;
    boxed_item::<Track>(&boxed).map(|track| ArtworkBinding::track(&track))
}
pub(crate) fn clear_list_item_child(_: &gtk::SignalListItemFactory, item: &glib::Object) {
    if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
        item.set_child(None::<&gtk::Widget>);
    }
}
pub(crate) const COLLECTION_GRID_CARD_GAP: i32 = 2;
pub(crate) const COLLECTION_GRID_CARD_MARGIN: i32 = 5;
pub(crate) const COLLECTION_GRID_MIN_CARD_WIDTH: i32 = 128;
pub(crate) const COLLECTION_GRID_MAX_CARD_WIDTH: i32 = 200;
const COLLECTION_GRID_LABEL_HEIGHT: i32 = 20;

pub(crate) fn grid_label_with_label(text: &str, css_class: &str) -> (gtk::Widget, gtk::Label) {
    let label = gtk::Label::new(Some(text));
    if !css_class.is_empty() {
        label.add_css_class(css_class);
    }
    label.set_xalign(0.0);
    label.set_justify(gtk::Justification::Left);
    label.set_wrap(false);
    label.set_single_line_mode(true);
    label.set_lines(1);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    configure_collection_grid_label(&label);
    if !text.is_empty() {
        label.set_tooltip_text(Some(text));
    }

    (label.clone().upcast(), label)
}
pub(crate) fn grid_title_with_label(text: &str, css_class: &str) -> (gtk::Widget, gtk::Label) {
    let label = gtk::Label::new(Some(text));
    if !css_class.is_empty() {
        label.add_css_class(css_class);
    }
    label.set_xalign(0.0);
    label.set_justify(gtk::Justification::Left);
    label.set_wrap(false);
    label.set_single_line_mode(true);
    label.set_lines(1);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    configure_collection_grid_label(&label);
    if !text.is_empty() {
        label.set_tooltip_text(Some(text));
    }

    (label.clone().upcast(), label)
}

fn configure_collection_grid_label(label: &gtk::Label) {
    label.set_width_request(1);
    label.set_height_request(COLLECTION_GRID_LABEL_HEIGHT);
    label.set_halign(gtk::Align::Fill);
    label.set_valign(gtk::Align::Center);
    label.set_hexpand(true);
    label.set_vexpand(false);
    label.set_yalign(0.5);
}
pub(crate) fn album_detail_meta_label(text: &str, css_class: &str, width: i32) -> gtk::Widget {
    let wrap = css_class == "track-title";
    let height = if wrap {
        ALBUM_DETAIL_META_LABEL_HEIGHT * 2
    } else {
        ALBUM_DETAIL_META_LABEL_HEIGHT
    };
    let label = gtk::Label::new(Some(text));
    if !css_class.is_empty() {
        label.add_css_class(css_class);
    }
    label.set_xalign(0.5);
    label.set_wrap(wrap);
    if wrap {
        label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        label.set_lines(2);
        label.set_single_line_mode(false);
    } else {
        label.set_single_line_mode(true);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    }
    label.set_width_chars(1);
    label.set_halign(gtk::Align::Fill);
    label.set_hexpand(false);

    let clip = gtk::ScrolledWindow::new();
    clip.add_css_class("card-label-clip");
    clip.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Never);
    clip.set_overflow(gtk::Overflow::Hidden);
    clip.set_width_request(width);
    clip.set_height_request(height);
    clip.set_size_request(width, height);
    clip.set_min_content_width(width);
    clip.set_max_content_width(width);
    clip.set_min_content_height(height);
    clip.set_max_content_height(height);
    clip.set_propagate_natural_width(false);
    clip.set_propagate_natural_height(false);
    clip.set_hexpand(false);
    clip.set_child(Some(&label));
    clip.upcast()
}
pub(crate) fn album_fact_text(album: &AlbumSummary) -> String {
    format!(
        "{} • {} • {}",
        nonzero_year(album.album.year),
        track_count_text(album.track_count.into()),
        format_duration_units(album.duration_seconds)
    )
}
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum LibraryFieldSet {
    Row,
    Grid,
    Detail,
}
pub(crate) fn populate_library_field_rows(
    shell: &Rc<Shell>,
    key: LibraryListKey,
    group: &adw::PreferencesGroup,
    rows: &Rc<RefCell<Vec<adw::ActionRow>>>,
) {
    let settings = shell.settings.current.borrow().library_list(key);
    populate_library_field_rows_for_set(
        shell,
        key,
        field_set_for_layout(settings.layout),
        group,
        rows,
    );
}
pub(crate) fn populate_library_field_rows_for_set(
    shell: &Rc<Shell>,
    key: LibraryListKey,
    field_set: LibraryFieldSet,
    group: &adw::PreferencesGroup,
    rows: &Rc<RefCell<Vec<adw::ActionRow>>>,
) {
    for row in rows.borrow_mut().drain(..) {
        group.remove(&row);
    }

    let settings = shell.settings.current.borrow().library_list(key);
    group.set_title(&tr(field_group_title(field_set)));

    let active = active_fields_for_set(&settings, field_set).to_vec();
    let mut order = active.clone();
    for field in available_fields_for_set(key, field_set) {
        if !order.contains(field) {
            order.push(*field);
        }
    }
    for field in order {
        let row = library_field_config_row(shell, key, field_set, field, &active, group, rows);
        group.add(&row);
        rows.borrow_mut().push(row);
    }
}
pub(crate) fn library_field_config_row(
    shell: &Rc<Shell>,
    key: LibraryListKey,
    field_set: LibraryFieldSet,
    field: LibraryField,
    active: &[LibraryField],
    group: &adw::PreferencesGroup,
    rows: &Rc<RefCell<Vec<adw::ActionRow>>>,
) -> adw::ActionRow {
    let enabled = active.contains(&field);
    let row = adw::ActionRow::builder()
        .title(tr(field.title()))
        .subtitle(if enabled { tr("Visible") } else { tr("Hidden") })
        .build();

    let drag = gtk::Image::from_icon_name("rufin-list-drag-handle-symbolic");
    drag.add_css_class("dim-label");
    drag.set_tooltip_text(Some(&tr("Drag to reorder")));
    row.add_prefix(&drag);

    let check = gtk::CheckButton::new();
    check.set_active(enabled);
    check.set_sensitive(can_toggle_field(active, field_set, field));
    check.set_valign(gtk::Align::Center);
    row.add_prefix(&check);
    row.set_activatable_widget(Some(&check));

    let up = gtk::Button::from_icon_name("rufin-go-up-symbolic");
    up.add_css_class("flat");
    up.set_tooltip_text(Some(&tr("Move up")));
    up.set_valign(gtk::Align::Center);
    up.set_sensitive(enabled);
    row.add_suffix(&up);

    let down = gtk::Button::from_icon_name("rufin-go-down-symbolic");
    down.add_css_class("flat");
    down.set_tooltip_text(Some(&tr("Move down")));
    down.set_valign(gtk::Align::Center);
    down.set_sensitive(enabled);
    row.add_suffix(&down);

    {
        let shell = Rc::clone(shell);
        let group = group.downgrade();
        let rows = Rc::downgrade(rows);
        check.connect_toggled(move |check| {
            let (Some(group), Some(rows)) = (group.upgrade(), rows.upgrade()) else {
                return;
            };
            shell.update_library_list_settings(key, |settings| {
                set_field_enabled(settings, key, field_set, field, check.is_active());
            });
            populate_library_field_rows_for_set(&shell, key, field_set, &group, &rows);
        });
    }
    {
        let shell = Rc::clone(shell);
        let group = group.downgrade();
        let rows = Rc::downgrade(rows);
        up.connect_clicked(move |_| {
            let (Some(group), Some(rows)) = (group.upgrade(), rows.upgrade()) else {
                return;
            };
            shell.update_library_list_settings(key, |settings| {
                move_visible_field(settings, field_set, field, -1);
            });
            populate_library_field_rows_for_set(&shell, key, field_set, &group, &rows);
        });
    }
    {
        let shell = Rc::clone(shell);
        let group = group.downgrade();
        let rows = Rc::downgrade(rows);
        down.connect_clicked(move |_| {
            let (Some(group), Some(rows)) = (group.upgrade(), rows.upgrade()) else {
                return;
            };
            shell.update_library_list_settings(key, |settings| {
                move_visible_field(settings, field_set, field, 1);
            });
            populate_library_field_rows_for_set(&shell, key, field_set, &group, &rows);
        });
    }

    let source = gtk::DragSource::builder()
        .actions(gtk::gdk::DragAction::MOVE)
        .build();
    let field_id = library_field_drag_id(field).to_string();
    source.connect_prepare(move |_, _, _| {
        Some(gtk::gdk::ContentProvider::for_value(&field_id.to_value()))
    });
    drag.add_controller(source);

    let drop_target = gtk::DropTarget::new(String::static_type(), gtk::gdk::DragAction::MOVE);
    let shell = Rc::clone(shell);
    let group = group.downgrade();
    let rows = Rc::downgrade(rows);
    let row_for_drop = row.downgrade();
    drop_target.connect_drop(move |_, value, _, y| {
        let Ok(source_id) = value.get::<String>() else {
            return false;
        };
        let Some(source_field) = library_field_from_drag_id(&source_id) else {
            return false;
        };
        if source_field == field {
            return false;
        }
        let Some(row) = row_for_drop.upgrade() else {
            return false;
        };
        let (Some(group), Some(rows)) = (group.upgrade(), rows.upgrade()) else {
            return false;
        };
        let after = y > f64::from(row.height()) / 2.0;
        shell.update_library_list_settings(key, |settings| {
            reorder_visible_field(settings, field_set, source_field, field, after);
        });
        populate_library_field_rows_for_set(&shell, key, field_set, &group, &rows);
        true
    });
    row.add_controller(drop_target);

    row
}
pub(crate) fn layout_button_content(layout: LibraryLayout) -> gtk::Widget {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.append(&gtk::Image::from_icon_name(layout_icon(layout)));
    content.append(&gtk::Label::new(Some(&tr(layout_title(layout)))));
    content.upcast()
}
pub(crate) fn sync_layout_buttons(
    buttons: &Rc<RefCell<Vec<(LibraryLayout, gtk::ToggleButton)>>>,
    active_layout: LibraryLayout,
) {
    for (layout, button) in buttons.borrow().iter() {
        button.set_active(*layout == active_layout);
    }
}
pub(crate) fn supported_layouts(key: LibraryListKey) -> Vec<LibraryLayout> {
    let mut layouts = vec![LibraryLayout::Row, LibraryLayout::Grid];
    if key.supports_layout(LibraryLayout::Detail) {
        layouts.push(LibraryLayout::Detail);
    }
    layouts
}
pub(crate) fn field_group_title(field_set: LibraryFieldSet) -> &'static str {
    match field_set {
        LibraryFieldSet::Row => msgid("Columns"),
        LibraryFieldSet::Grid => msgid("Grid labels"),
        LibraryFieldSet::Detail => msgid("Detail track columns"),
    }
}
pub(crate) fn field_set_for_layout(layout: LibraryLayout) -> LibraryFieldSet {
    match layout {
        LibraryLayout::Grid => LibraryFieldSet::Grid,
        LibraryLayout::Detail => LibraryFieldSet::Detail,
        LibraryLayout::Row => LibraryFieldSet::Row,
    }
}
pub(crate) fn active_fields_for_set(
    settings: &LibraryListSettings,
    field_set: LibraryFieldSet,
) -> &[LibraryField] {
    match field_set {
        LibraryFieldSet::Grid => &settings.grid_fields,
        LibraryFieldSet::Detail => &settings.detail_track_fields,
        LibraryFieldSet::Row => &settings.row_fields,
    }
}
pub(crate) fn active_fields_for_set_mut(
    settings: &mut LibraryListSettings,
    field_set: LibraryFieldSet,
) -> &mut Vec<LibraryField> {
    match field_set {
        LibraryFieldSet::Grid => &mut settings.grid_fields,
        LibraryFieldSet::Detail => &mut settings.detail_track_fields,
        LibraryFieldSet::Row => &mut settings.row_fields,
    }
}
pub(crate) fn available_fields_for_set(
    key: LibraryListKey,
    field_set: LibraryFieldSet,
) -> &'static [LibraryField] {
    match field_set {
        LibraryFieldSet::Grid => crate::available_grid_fields(key),
        LibraryFieldSet::Detail => crate::available_detail_track_fields(),
        LibraryFieldSet::Row => crate::available_row_fields(key),
    }
}
pub(crate) fn set_field_enabled(
    settings: &mut LibraryListSettings,
    _key: LibraryListKey,
    field_set: LibraryFieldSet,
    field: LibraryField,
    enabled: bool,
) {
    let fields = active_fields_for_set_mut(settings, field_set);
    if enabled {
        if !fields.contains(&field) {
            fields.push(field);
        }
    } else {
        fields.retain(|candidate| *candidate != field);
    }
}
pub(crate) fn move_visible_field(
    settings: &mut LibraryListSettings,
    field_set: LibraryFieldSet,
    field: LibraryField,
    delta: isize,
) {
    let fields = active_fields_for_set_mut(settings, field_set);
    let Some(index) = fields.iter().position(|candidate| *candidate == field) else {
        return;
    };
    let new_index = if delta < 0 {
        index.saturating_sub(1)
    } else {
        (index + 1).min(fields.len().saturating_sub(1))
    };
    fields.swap(index, new_index);
}
pub(crate) fn reorder_visible_field(
    settings: &mut LibraryListSettings,
    field_set: LibraryFieldSet,
    source: LibraryField,
    target: LibraryField,
    after: bool,
) {
    let fields = active_fields_for_set_mut(settings, field_set);
    let Some(source_index) = fields.iter().position(|field| *field == source) else {
        return;
    };
    let field = fields.remove(source_index);
    let Some(mut target_index) = fields.iter().position(|field| *field == target) else {
        fields.insert(source_index.min(fields.len()), field);
        return;
    };
    if after {
        target_index += 1;
    }
    fields.insert(target_index.min(fields.len()), field);
}
pub(crate) fn can_toggle_field(
    active: &[LibraryField],
    field_set: LibraryFieldSet,
    field: LibraryField,
) -> bool {
    if !active.contains(&field) {
        return true;
    }
    if field_set == LibraryFieldSet::Grid {
        return true;
    }
    !row_field_is_usable(field)
        || active
            .iter()
            .filter(|field| row_field_is_usable(**field))
            .count()
            > 1
}
pub(crate) fn row_field_is_usable(field: LibraryField) -> bool {
    !matches!(
        field,
        LibraryField::RowIndex
            | LibraryField::Image
            | LibraryField::TrackNumber
            | LibraryField::DiscNumber
            | LibraryField::Favorite
    )
}
pub(crate) fn library_field_drag_id(field: LibraryField) -> &'static str {
    match field {
        LibraryField::RowIndex => "RowIndex",
        LibraryField::Image => "Image",
        LibraryField::Title => "Title",
        LibraryField::TitleMerged => "TitleMerged",
        LibraryField::Artist => "Artist",
        LibraryField::AlbumArtist => "AlbumArtist",
        LibraryField::Album => "Album",
        LibraryField::Year => "Year",
        LibraryField::ReleaseDate => "ReleaseDate",
        LibraryField::DateAdded => "DateAdded",
        LibraryField::LastPlayed => "LastPlayed",
        LibraryField::PlayCount => "PlayCount",
        LibraryField::UserRating => "UserRating",
        LibraryField::Genre => "Genre",
        LibraryField::Bpm => "Bpm",
        LibraryField::TrackNumber => "TrackNumber",
        LibraryField::DiscNumber => "DiscNumber",
        LibraryField::SongCount => "SongCount",
        LibraryField::AlbumCount => "AlbumCount",
        LibraryField::Duration => "Duration",
        LibraryField::Favorite => "Favorite",
    }
}
pub(crate) fn library_field_from_drag_id(id: &str) -> Option<LibraryField> {
    [
        LibraryField::RowIndex,
        LibraryField::Image,
        LibraryField::Title,
        LibraryField::TitleMerged,
        LibraryField::Artist,
        LibraryField::AlbumArtist,
        LibraryField::Album,
        LibraryField::Year,
        LibraryField::ReleaseDate,
        LibraryField::DateAdded,
        LibraryField::LastPlayed,
        LibraryField::PlayCount,
        LibraryField::UserRating,
        LibraryField::Genre,
        LibraryField::Bpm,
        LibraryField::TrackNumber,
        LibraryField::DiscNumber,
        LibraryField::SongCount,
        LibraryField::AlbumCount,
        LibraryField::Duration,
        LibraryField::Favorite,
    ]
    .into_iter()
    .find(|field| library_field_drag_id(*field) == id)
}
pub(crate) fn next_layout(key: LibraryListKey, layout: LibraryLayout) -> LibraryLayout {
    if key.supports_layout(LibraryLayout::Detail) {
        match layout {
            LibraryLayout::Grid => LibraryLayout::Detail,
            LibraryLayout::Detail => LibraryLayout::Row,
            LibraryLayout::Row => LibraryLayout::Grid,
        }
    } else {
        match layout {
            LibraryLayout::Grid => LibraryLayout::Row,
            LibraryLayout::Row | LibraryLayout::Detail => LibraryLayout::Grid,
        }
    }
}
pub(crate) fn layout_icon(layout: LibraryLayout) -> &'static str {
    match layout {
        LibraryLayout::Grid => "rufin-view-grid-symbolic",
        LibraryLayout::Row => "rufin-view-list-symbolic",
        LibraryLayout::Detail => "rufin-view-list-details-symbolic",
    }
}
pub(crate) fn layout_title(layout: LibraryLayout) -> &'static str {
    match layout {
        LibraryLayout::Grid => msgid("Grid"),
        LibraryLayout::Row => msgid("Rows"),
        LibraryLayout::Detail => msgid("Detail"),
    }
}
pub(crate) fn column_width(field: LibraryField) -> i32 {
    match field {
        LibraryField::RowIndex => 48,
        LibraryField::Image | LibraryField::Favorite => 56,
        LibraryField::Title | LibraryField::TitleMerged => 220,
        LibraryField::Album
        | LibraryField::Artist
        | LibraryField::AlbumArtist
        | LibraryField::Genre => 220,
        LibraryField::ReleaseDate | LibraryField::DateAdded | LibraryField::LastPlayed => 118,
        LibraryField::PlayCount => play_count_column_width(),
        LibraryField::UserRating | LibraryField::SongCount | LibraryField::AlbumCount => 96,
        LibraryField::Year
        | LibraryField::DiscNumber
        | LibraryField::TrackNumber
        | LibraryField::Bpm => 68,
        LibraryField::Duration => 76,
    }
}
pub(crate) fn play_count_column_width() -> i32 {
    compact_header_column_width(msgid("Plays"), 56)
}
pub(crate) fn compact_header_column_width(header: &str, min_width: i32) -> i32 {
    let width = tr(header).chars().count().min(i32::MAX as usize / 8) as i32 * 8 + 20;
    width.max(min_width)
}
pub(crate) fn apply_desc(ordering: Ordering, descending: bool) -> Ordering {
    if descending {
        ordering.reverse()
    } else {
        ordering
    }
}
pub(crate) fn cmp_string(left: &str, right: &str) -> Ordering {
    left.to_lowercase().cmp(&right.to_lowercase())
}
pub(crate) fn cmp_option_string(left: &Option<String>, right: &Option<String>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => cmp_string(left, right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}
pub(crate) fn cmp_option_u32(left: Option<u32>, right: Option<u32>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}
pub(crate) fn cmp_option_u8(left: Option<u8>, right: Option<u8>) -> Ordering {
    cmp_option_u32(left.map(u32::from), right.map(u32::from))
}
pub(crate) fn option_count(value: Option<u32>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}
pub(crate) fn option_rating(value: Option<u8>) -> String {
    value
        .map(|value| format!("{:.1}", f64::from(value) / 2.0))
        .unwrap_or_default()
}
pub(crate) fn favorite_text(favorite: bool) -> String {
    if favorite { "♥" } else { "" }.to_string()
}
pub(crate) fn nonzero_year(year: u16) -> String {
    if year == 0 {
        String::new()
    } else {
        year.to_string()
    }
}

fn display_calendar_date(value: Option<&str>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    value
        .get(..10)
        .filter(|date| {
            date.as_bytes().get(4) == Some(&b'-') && date.as_bytes().get(7) == Some(&b'-')
        })
        .unwrap_or(value)
        .to_string()
}
pub(crate) fn joined_credits(credits: &[library::ArtistCredit]) -> String {
    credits
        .iter()
        .map(|credit| credit.name.trim())
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use ::library::SmartPlaylistSummary;

    use crate::{LibraryField, LibraryListKey, LibraryListSettings};

    use super::{
        LibraryFieldSet, album_field, album_item_field, display_calendar_date, set_field_enabled,
        smart_playlist_field,
    };

    fn smart_playlist_with_stats(track_count: u32, duration_seconds: u32) -> SmartPlaylistSummary {
        crate::test_support::smart_playlist_summary(
            crate::test_support::smart_playlist("test", "Smart Mix"),
            track_count,
            duration_seconds,
        )
    }

    #[test]
    fn cards_smart_zeroes() {
        let empty = smart_playlist_with_stats(0, 0);
        assert!(smart_playlist_field(&empty, LibraryField::SongCount).is_empty());
        assert!(smart_playlist_field(&empty, LibraryField::Duration).is_empty());

        let resolved = smart_playlist_with_stats(2, 120);
        assert_eq!(
            smart_playlist_field(&resolved, LibraryField::SongCount),
            "2 tracks"
        );
        assert_eq!(
            smart_playlist_field(&resolved, LibraryField::Duration),
            "2m 0s"
        );
    }

    #[test]
    fn live_album_fields_do_not_invent_library_summary_counts() {
        let album = crate::test_support::album("album", "Live result");
        assert_eq!(album_item_field(&album, LibraryField::Title), "Live result");
        assert!(album_item_field(&album, LibraryField::SongCount).is_empty());

        let summary = crate::test_support::album_summary(album, 3, 180);
        assert_eq!(album_field(&summary, LibraryField::SongCount), "3 tracks");
    }

    #[test]
    fn last_played_keeps_timestamp_precision_out_of_visible_cells() {
        assert_eq!(
            display_calendar_date(Some("2026-07-13T17:31:12Z")),
            "2026-07-13"
        );
        assert_eq!(
            display_calendar_date(Some("provider-value")),
            "provider-value"
        );
        assert!(display_calendar_date(None).is_empty());
    }

    #[test]
    fn enabling_field_appends() {
        let mut settings = LibraryListSettings::for_key(LibraryListKey::Tracks);

        set_field_enabled(
            &mut settings,
            LibraryListKey::Tracks,
            LibraryFieldSet::Row,
            LibraryField::DiscNumber,
            true,
        );

        assert_eq!(settings.row_fields.last(), Some(&LibraryField::DiscNumber));
    }
}
