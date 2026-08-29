use std::{cell::RefCell, rc::Rc};

use ::library::{AlbumRow, ArtistRow, PlaylistRow, SmartPlaylistRow, TrackArtistLink, TrackRow};
use adw::prelude::*;
use artwork::ArtworkBinding;
use gtk::{gio, glib};

use crate::format_duration_units;
use crate::shell::Shell;
use crate::{LibraryField, LibraryLayout, LibraryListKey, LibraryListSettings};
use localization::{album_count_text, track_count_text};
use localization::{msgid, tr};

use super::sparse_model::{SparseItem, SparseObjectItem};

pub(crate) fn add_field_skeleton_class(widget: &impl IsA<gtk::Widget>, field: LibraryField) {
    let class = match field {
        LibraryField::Year
        | LibraryField::UserRating
        | LibraryField::Bpm
        | LibraryField::DiscNumber => "skeleton-short-value",
        LibraryField::TrackNumber => "skeleton-track-number",
        LibraryField::Duration => "skeleton-duration",
        LibraryField::ReleaseDate | LibraryField::DateAdded | LibraryField::LastPlayed => {
            "skeleton-date"
        }
        _ => return,
    };
    widget.add_css_class(class);
}

pub(crate) fn smart_playlist_display_name(playlist: &SmartPlaylistRow) -> String {
    match playlist.object_id.as_str() {
        "builtin:most_played" => tr(msgid("Most Played")),
        "builtin:never_played" => tr(msgid("Never Played")),
        "builtin:most_skipped" => tr(msgid("Most Skipped")),
        _ => playlist.name.clone(),
    }
}

pub(crate) fn album_item_field(album: &AlbumRow, field: LibraryField) -> String {
    match field {
        LibraryField::Title | LibraryField::TitleMerged => album.title.clone(),
        LibraryField::AlbumArtist | LibraryField::Artist => album.display_artist.clone(),
        LibraryField::Year => optional_year(album.year),
        LibraryField::ReleaseDate => album.release_date.clone().unwrap_or_default(),
        LibraryField::DateAdded => album.date_added.clone().unwrap_or_default(),
        LibraryField::LastPlayed => display_unix_date(album.last_played),
        LibraryField::PlayCount => count(album.play_count),
        LibraryField::UserRating => stored_rating(album.rating),
        LibraryField::Genre => album
            .genres
            .iter()
            .map(|genre| genre.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        LibraryField::Favorite => favorite_text(album.favorite),
        _ => String::new(),
    }
}
pub(crate) fn album_field(album: &AlbumRow, field: LibraryField) -> String {
    match field {
        LibraryField::SongCount => track_count_text(album.track_count.max(0) as u64),
        LibraryField::Duration => {
            crate::format_duration((album.duration_millis.max(0) / 1_000) as u32)
        }
        _ => album_item_field(album, field),
    }
}
pub(crate) fn artist_item_field(artist: &ArtistRow, field: LibraryField) -> String {
    match field {
        LibraryField::Title | LibraryField::TitleMerged => artist.name.clone(),
        LibraryField::LastPlayed => display_unix_date(artist.last_played),
        LibraryField::PlayCount => count(artist.play_count),
        LibraryField::UserRating => stored_rating(artist.rating),
        LibraryField::Favorite => favorite_text(artist.favorite),
        _ => String::new(),
    }
}
pub(crate) fn artist_field(artist: &ArtistRow, field: LibraryField) -> String {
    match field {
        LibraryField::AlbumCount => album_count_text(artist.album_count.max(0) as u64),
        LibraryField::SongCount => track_count_text(artist.track_count.max(0) as u64),
        _ => artist_item_field(artist, field),
    }
}
pub(crate) fn playlist_field(playlist: &PlaylistRow, field: LibraryField) -> String {
    match field {
        LibraryField::Title | LibraryField::TitleMerged => playlist.name.clone(),
        LibraryField::SongCount => track_count_text(playlist.track_count.max(0) as u64),
        LibraryField::Duration => {
            format_duration_units((playlist.duration_millis.max(0) / 1_000) as u32)
        }
        _ => String::new(),
    }
}

pub(crate) fn playlist_artwork(playlist: &PlaylistRow, prefer_server: bool) -> Vec<ArtworkBinding> {
    let bindings: &[Vec<u8>] = if prefer_server {
        playlist
            .artwork_binding
            .as_ref()
            .map(std::slice::from_ref)
            .unwrap_or(&playlist.representative_artwork)
    } else if playlist.representative_artwork.is_empty() {
        playlist
            .artwork_binding
            .as_ref()
            .map(std::slice::from_ref)
            .unwrap_or_default()
    } else {
        &playlist.representative_artwork
    };
    bindings
        .iter()
        .map(|binding| ArtworkBinding::opaque(binding))
        .collect()
}
pub(crate) fn smart_playlist_field(playlist: &SmartPlaylistRow, field: LibraryField) -> String {
    match field {
        LibraryField::Title | LibraryField::TitleMerged => smart_playlist_display_name(playlist),
        LibraryField::SongCount if playlist.track_count > 0 => {
            track_count_text(playlist.track_count as u64)
        }
        LibraryField::Duration if playlist.duration_millis > 0 => {
            format_duration_units((playlist.duration_millis / 1_000) as u32)
        }
        _ => String::new(),
    }
}
pub(crate) fn track_field(track: &TrackRow, field: LibraryField) -> String {
    match field {
        LibraryField::Title | LibraryField::TitleMerged => track.title.clone(),
        LibraryField::Artist => track.display_artist.clone(),
        LibraryField::AlbumArtist => joined_credits(&track.album_artists),
        LibraryField::Album => track.display_album.clone(),
        LibraryField::Year => optional_year(track.year),
        LibraryField::ReleaseDate => track.release_date.clone().unwrap_or_default(),
        LibraryField::DateAdded => track.date_added.clone().unwrap_or_default(),
        LibraryField::LastPlayed => display_unix_date(track.last_played),
        LibraryField::PlayCount => count(track.play_count),
        LibraryField::UserRating => stored_rating(track.rating),
        LibraryField::Genre => track
            .genres
            .iter()
            .map(|genre| genre.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        LibraryField::Bpm => track.bpm.map(|bpm| bpm.to_string()).unwrap_or_default(),
        LibraryField::DiscNumber => track.disc_number.to_string(),
        LibraryField::TrackNumber => format!("{}-{:02}", track.disc_number, track.track_number),
        LibraryField::Duration => {
            crate::format_duration((track.duration_millis.max(0) / 1_000) as u32)
        }
        LibraryField::Favorite => favorite_text(track.favorite),
        _ => String::new(),
    }
}
fn boxed_item<T: Clone + 'static>(boxed: &glib::BoxedAnyObject) -> Option<T> {
    boxed.try_borrow::<T>().ok().map(|item| item.clone())
}

pub(super) fn sparse_item<T: Clone + 'static>(item: &SparseObjectItem) -> Option<T> {
    macro_rules! ready {
        ($key:ty) => {
            if let Some(item) = item.value::<SparseItem<$key, T>>() {
                return match item {
                    SparseItem::Ready(row) => Some((*row).clone()),
                    SparseItem::Placeholder(_) => None,
                };
            }
        };
    }
    ready!(library::TrackKey);
    ready!(library::AlbumKey);
    ready!(library::ArtistKey);
    ready!(library::GenreKey);
    ready!(library::MoodKey);
    ready!(library::PlaylistKey);
    ready!(library::PlaylistEntryKey);
    ready!(library::SmartPlaylistKey);
    ready!(super::folders::FolderLink);
    None
}

fn object_item<T: Clone + 'static>(item: glib::Object) -> Option<T> {
    match item.downcast::<glib::BoxedAnyObject>() {
        Ok(boxed) => boxed_item(&boxed),
        Err(item) => item
            .downcast::<SparseObjectItem>()
            .ok()
            .and_then(|item| sparse_item(&item)),
    }
}

pub(crate) fn item_at<T: Clone + 'static>(
    model: &impl IsA<gio::ListModel>,
    position: u32,
) -> Option<T> {
    model.item(position).and_then(object_item)
}
pub(crate) fn item_at_from_item<T: Clone + 'static>(item: &gtk::ListItem) -> Option<T> {
    item.item().and_then(object_item)
}
pub(crate) fn track_artwork_at_from_item(item: &gtk::ListItem) -> Option<ArtworkBinding> {
    item_at_from_item::<TrackRow>(item).map(|track| {
        track
            .artwork_binding
            .as_deref()
            .map(ArtworkBinding::opaque)
            .unwrap_or_default()
    })
}

pub(crate) fn opaque_artwork(binding: Option<&[u8]>) -> ArtworkBinding {
    binding.map(ArtworkBinding::opaque).unwrap_or_default()
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
        LibraryField::Image => 56,
        LibraryField::Favorite => crate::favorites::FAVORITE_COLUMN_WIDTH,
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
fn count(value: i64) -> String {
    value.max(0).to_string()
}

fn stored_rating(value: Option<i64>) -> String {
    value
        .map(|value| format!("{:.1}", value as f64 / 2.0))
        .unwrap_or_default()
}

fn optional_year(year: Option<i64>) -> String {
    year.filter(|year| *year != 0)
        .map(|year| year.to_string())
        .unwrap_or_default()
}

fn display_unix_date(value: Option<i64>) -> String {
    value
        .and_then(|value| glib::DateTime::from_unix_local(value).ok())
        .and_then(|value| value.format("%Y-%m-%d").ok())
        .map(|value| value.to_string())
        .unwrap_or_default()
}
pub(crate) fn favorite_text(favorite: bool) -> String {
    if favorite { "♥" } else { "" }.to_string()
}
pub(crate) fn joined_credits(credits: &[TrackArtistLink]) -> String {
    credits
        .iter()
        .map(|credit| credit.name.trim())
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}
