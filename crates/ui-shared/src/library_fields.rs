use std::{cell::RefCell, rc::Rc};

use ::library::{AlbumRow, ArtistRow, PlaylistRow, SmartPlaylistRow, TrackRow};
use adw::prelude::*;
use artwork::ArtworkBinding;
use gtk::glib;

use crate::format_duration_units;
use localization::{album_count_text, track_count_text};
use localization::{msgid, tr};
use rufin_core::settings::layout::{
    LibraryField, LibraryLayout, LibraryListKey, LibraryListSettings,
};

pub trait TrackPresentation: Clone + PartialEq + Send + Sync + 'static {
    fn media_uri(&self) -> &str;
    fn title(&self) -> &str;
    fn artist(&self) -> &str;
    fn artwork(&self) -> Option<&[u8]>;
    fn favorite(&self) -> bool;
    fn set_favorite(&mut self, value: bool);
    fn downloaded(&self) -> bool;
    fn set_downloaded(&mut self, value: bool);
    fn field(&self, field: LibraryField) -> String;
    fn links(&self, field: LibraryField) -> crate::detail_links::DetailLinks {
        crate::detail_links::DetailLinks::text(&self.field(field))
    }
}

macro_rules! track_presentation_fields {
    () => {
        fn media_uri(&self) -> &str {
            &self.media_uri
        }
        fn title(&self) -> &str {
            &self.title
        }
        fn artist(&self) -> &str {
            &self.artist
        }
        fn artwork(&self) -> Option<&[u8]> {
            self.artwork_binding.as_deref()
        }
        fn favorite(&self) -> bool {
            self.favorite
        }
        fn set_favorite(&mut self, value: bool) {
            self.favorite = value;
        }
        fn downloaded(&self) -> bool {
            self.is_downloaded
        }
        fn set_downloaded(&mut self, value: bool) {
            self.is_downloaded = value;
        }
    };
}

impl TrackPresentation for TrackRow {
    track_presentation_fields!();
    fn field(&self, field: LibraryField) -> String {
        track_field(self, field)
    }
    fn links(&self, field: LibraryField) -> crate::detail_links::DetailLinks {
        crate::detail_links::metadata_links(
            field,
            &self.field(field),
            self.album_media_uri.as_deref(),
            &self.artists,
            &self.album_artists,
        )
    }
}

impl TrackPresentation for library::HistoryRow {
    track_presentation_fields!();
    fn links(&self, field: LibraryField) -> crate::detail_links::DetailLinks {
        crate::detail_links::metadata_links(
            field,
            &self.field(field),
            self.album_media_uri.as_deref(),
            &self.artists,
            &self.album_artists,
        )
    }
    fn field(&self, field: LibraryField) -> String {
        match field {
            LibraryField::Title | LibraryField::TitleMerged => self.title.clone(),
            LibraryField::Artist => self.artist.clone(),
            LibraryField::Album => self.album.clone(),
            LibraryField::AlbumArtist => self.album_display_artist.clone().unwrap_or_default(),
            LibraryField::Year => optional_year(self.year),
            LibraryField::ReleaseDate => self.release_date.clone().unwrap_or_default(),
            LibraryField::DateAdded => self.date_added.clone().unwrap_or_default(),
            LibraryField::LastPlayed => display_unix_date(self.last_played),
            LibraryField::PlayCount => count(self.play_count),
            LibraryField::Genre => self.genre.clone(),
            LibraryField::Bpm => self.bpm.map(|value| value.to_string()).unwrap_or_default(),
            LibraryField::UserRating => stored_rating(self.rating),
            LibraryField::DiscNumber => self
                .disc_number
                .map(|value| value.to_string())
                .unwrap_or_default(),
            LibraryField::TrackNumber => optional_track_number(self.disc_number, self.track_number),
            LibraryField::Duration => {
                crate::format_duration((self.duration_millis.max(0) / 1_000) as u32)
            }
            LibraryField::Favorite => favorite_text(self.favorite),
            _ => String::new(),
        }
    }
}

impl TrackPresentation for library::SmartPlaylistTrackRow {
    track_presentation_fields!();
    fn links(&self, field: LibraryField) -> crate::detail_links::DetailLinks {
        crate::detail_links::metadata_links(
            field,
            &self.field(field),
            self.album_media_uri.as_deref(),
            &self.artists,
            &self.album_artists,
        )
    }
    fn field(&self, field: LibraryField) -> String {
        match field {
            LibraryField::Title | LibraryField::TitleMerged => self.title.clone(),
            LibraryField::Artist => self.artist.clone(),
            LibraryField::Album => self.album.clone(),
            LibraryField::AlbumArtist => self.album_display_artist.clone().unwrap_or_default(),
            LibraryField::Year => optional_year(self.year),
            LibraryField::ReleaseDate => self.release_date.clone().unwrap_or_default(),
            LibraryField::DateAdded => self.date_added.clone().unwrap_or_default(),
            LibraryField::LastPlayed => display_unix_date(self.last_played),
            LibraryField::PlayCount => count(self.play_count),
            LibraryField::UserRating => stored_rating(self.rating),
            LibraryField::Genre => self.genre.clone(),
            LibraryField::Bpm => self.bpm.map(|value| value.to_string()).unwrap_or_default(),
            LibraryField::DiscNumber => self
                .disc_number
                .map(|value| value.to_string())
                .unwrap_or_default(),
            LibraryField::TrackNumber => optional_track_number(self.disc_number, self.track_number),
            LibraryField::Duration => {
                crate::format_duration((self.duration_millis.max(0) / 1_000) as u32)
            }
            LibraryField::Favorite => favorite_text(self.favorite),
            _ => String::new(),
        }
    }
}

pub fn add_field_skeleton_class(widget: &impl IsA<gtk::Widget>, field: LibraryField) {
    if field == LibraryField::Duration {
        widget.add_css_class("tabular-numeric");
    }
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

pub fn smart_playlist_display_name(playlist: &SmartPlaylistRow) -> String {
    match playlist.object_id.as_str() {
        "builtin:most_played" => tr(msgid("Most Played")),
        "builtin:never_played" => tr(msgid("Never Played")),
        "builtin:most_skipped" => tr(msgid("Most Skipped")),
        _ => playlist.name.clone(),
    }
}

pub fn album_item_field(album: &AlbumRow, field: LibraryField) -> String {
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
pub fn album_field(album: &AlbumRow, field: LibraryField) -> String {
    match field {
        LibraryField::SongCount => track_count_text(album.track_count.max(0) as u64),
        LibraryField::Duration => {
            crate::format_duration((album.duration_millis.max(0) / 1_000) as u32)
        }
        _ => album_item_field(album, field),
    }
}
pub fn artist_item_field(artist: &ArtistRow, field: LibraryField) -> String {
    match field {
        LibraryField::Title | LibraryField::TitleMerged => artist.name.clone(),
        LibraryField::LastPlayed => display_unix_date(artist.last_played),
        LibraryField::PlayCount => count(artist.play_count),
        LibraryField::UserRating => stored_rating(artist.rating),
        LibraryField::Favorite => favorite_text(artist.favorite),
        _ => String::new(),
    }
}
pub fn artist_field(artist: &ArtistRow, field: LibraryField) -> String {
    match field {
        LibraryField::AlbumCount => album_count_text(artist.album_count.max(0) as u64),
        LibraryField::SongCount => track_count_text(artist.track_count.max(0) as u64),
        _ => artist_item_field(artist, field),
    }
}
pub fn playlist_field(playlist: &PlaylistRow, field: LibraryField) -> String {
    match field {
        LibraryField::Title | LibraryField::TitleMerged => playlist.name.clone(),
        LibraryField::SongCount => track_count_text(playlist.track_count.max(0) as u64),
        LibraryField::Duration => {
            format_duration_units((playlist.duration_millis.max(0) / 1_000) as u32)
        }
        _ => String::new(),
    }
}

pub fn playlist_artwork(playlist: &PlaylistRow, prefer_server: bool) -> Vec<ArtworkBinding> {
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
pub fn smart_playlist_field(playlist: &SmartPlaylistRow, field: LibraryField) -> String {
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
pub fn track_field(track: &TrackRow, field: LibraryField) -> String {
    match field {
        LibraryField::Title | LibraryField::TitleMerged => track.title.clone(),
        LibraryField::Artist => track.artist.clone(),
        LibraryField::AlbumArtist => joined_credits(&track.album_artists),
        LibraryField::Album => track.album.clone(),
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
        LibraryField::TrackNumber => {
            optional_track_number(Some(track.disc_number), Some(track.track_number))
        }
        LibraryField::Duration => {
            crate::format_duration((track.duration_millis.max(0) / 1_000) as u32)
        }
        LibraryField::Favorite => favorite_text(track.favorite),
        _ => String::new(),
    }
}

pub fn optional_track_number(disc: Option<i64>, track: Option<i64>) -> String {
    match (disc, track) {
        (Some(disc), Some(track)) => format!("{disc}-{track:02}"),
        (_, Some(track)) => track.to_string(),
        _ => String::new(),
    }
}

pub fn opaque_artwork(binding: Option<&[u8]>) -> ArtworkBinding {
    binding.map(ArtworkBinding::opaque).unwrap_or_default()
}
pub const COLLECTION_GRID_CARD_MARGIN: i32 = 5;
pub const COLLECTION_GRID_MIN_CARD_WIDTH: i32 = 128;
pub const COLLECTION_GRID_MAX_CARD_WIDTH: i32 = 200;
const COLLECTION_GRID_LABEL_HEIGHT: i32 = 20;

pub fn grid_label_with_label(text: &str, css_class: &str) -> (gtk::Widget, gtk::Label) {
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
pub enum LibraryFieldSet {
    Row,
    Grid,
    Detail,
}

pub fn layout_button_content(layout: LibraryLayout) -> gtk::Widget {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.append(&gtk::Image::from_icon_name(layout_icon(layout)));
    content.append(&gtk::Label::new(Some(&tr(layout_title(layout)))));
    content.upcast()
}
pub fn sync_layout_buttons(
    buttons: &Rc<RefCell<Vec<(LibraryLayout, gtk::ToggleButton)>>>,
    active_layout: LibraryLayout,
) {
    for (layout, button) in buttons.borrow().iter() {
        button.set_active(*layout == active_layout);
    }
}
pub fn supported_layouts(key: LibraryListKey) -> Vec<LibraryLayout> {
    let mut layouts = vec![LibraryLayout::Row, LibraryLayout::Grid];
    if key.supports_layout(LibraryLayout::Detail) {
        layouts.push(LibraryLayout::Detail);
    }
    layouts
}
pub fn field_group_title(field_set: LibraryFieldSet) -> &'static str {
    match field_set {
        LibraryFieldSet::Row => msgid("Columns"),
        LibraryFieldSet::Grid => msgid("Grid labels"),
        LibraryFieldSet::Detail => msgid("Detail track columns"),
    }
}
pub fn field_set_for_layout(layout: LibraryLayout) -> LibraryFieldSet {
    match layout {
        LibraryLayout::Grid => LibraryFieldSet::Grid,
        LibraryLayout::Detail => LibraryFieldSet::Detail,
        LibraryLayout::Row => LibraryFieldSet::Row,
    }
}
pub fn active_fields_for_set(
    settings: &LibraryListSettings,
    field_set: LibraryFieldSet,
) -> &[LibraryField] {
    match field_set {
        LibraryFieldSet::Grid => &settings.grid_fields,
        LibraryFieldSet::Detail => &settings.detail_track_fields,
        LibraryFieldSet::Row => &settings.row_fields,
    }
}
pub fn active_fields_for_set_mut(
    settings: &mut LibraryListSettings,
    field_set: LibraryFieldSet,
) -> &mut Vec<LibraryField> {
    match field_set {
        LibraryFieldSet::Grid => &mut settings.grid_fields,
        LibraryFieldSet::Detail => &mut settings.detail_track_fields,
        LibraryFieldSet::Row => &mut settings.row_fields,
    }
}
pub fn available_fields_for_set(
    key: LibraryListKey,
    field_set: LibraryFieldSet,
) -> &'static [LibraryField] {
    match field_set {
        LibraryFieldSet::Grid => rufin_core::settings::layout::available_grid_fields(key),
        LibraryFieldSet::Detail => rufin_core::settings::sidebar::available_detail_track_fields(),
        LibraryFieldSet::Row => rufin_core::settings::layout::available_row_fields(key),
    }
}
pub fn set_field_enabled(
    settings: &mut LibraryListSettings,
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
pub fn move_visible_field(
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
pub fn reorder_visible_field(
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
pub fn can_toggle_field(
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
pub fn row_field_is_usable(field: LibraryField) -> bool {
    !matches!(
        field,
        LibraryField::RowIndex
            | LibraryField::Image
            | LibraryField::TrackNumber
            | LibraryField::DiscNumber
            | LibraryField::Favorite
    )
}
pub fn next_layout(key: LibraryListKey, layout: LibraryLayout) -> LibraryLayout {
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
pub fn layout_icon(layout: LibraryLayout) -> &'static str {
    match layout {
        LibraryLayout::Grid => "rufin-view-grid-symbolic",
        LibraryLayout::Row => "rufin-view-list-symbolic",
        LibraryLayout::Detail => "rufin-view-list-details-symbolic",
    }
}
pub fn layout_title(layout: LibraryLayout) -> &'static str {
    match layout {
        LibraryLayout::Grid => msgid("Grid"),
        LibraryLayout::Row => msgid("Rows"),
        LibraryLayout::Detail => msgid("Detail"),
    }
}
pub fn column_width(field: LibraryField) -> i32 {
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
pub fn play_count_column_width() -> i32 {
    compact_header_column_width(msgid("Plays"), 56)
}
pub fn compact_header_column_width(header: &str, min_width: i32) -> i32 {
    crate::table_sizing::compact_header_text_width(&tr(header), min_width)
}
pub fn count(value: i64) -> String {
    value.max(0).to_string()
}

pub fn stored_rating(value: Option<i64>) -> String {
    value
        .map(|value| format!("{:.1}", value as f64 / 2.0))
        .unwrap_or_default()
}

pub fn optional_year(year: Option<i64>) -> String {
    year.filter(|year| *year != 0)
        .map(|year| year.to_string())
        .unwrap_or_default()
}

pub fn display_unix_date(value: Option<i64>) -> String {
    value
        .and_then(|value| glib::DateTime::from_unix_local(value).ok())
        .and_then(|value| value.format("%Y-%m-%d").ok())
        .map(|value| value.to_string())
        .unwrap_or_default()
}
pub fn favorite_text(favorite: bool) -> String {
    if favorite { "♥" } else { "" }.to_string()
}
use crate::detail_links::joined_credits;
