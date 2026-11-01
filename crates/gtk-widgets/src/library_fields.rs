use std::{cell::RefCell, rc::Rc};

use ::library::{PlaylistRow, QueueRow, TrackRow};
use adw::prelude::*;
use artwork::ArtworkBinding;

use localization::{msgid, tr};
use rufin_core::settings::layout::{LibraryField, LibraryLayout, LibraryListKey};
use rufin_core::settings::presentation::{track_field, track_row_field};

pub trait TrackPresentation: Clone + PartialEq + Send + Sync + 'static {
    fn track_key(&self) -> Option<library::TrackKey>;
    fn media_uri(&self) -> &str;
    fn title(&self) -> &str;
    fn artist(&self) -> &str;
    fn artwork(&self) -> Option<&[u8]>;
    fn favorite(&self) -> bool;
    fn downloaded(&self) -> bool;
    fn download_badge(&self) -> bool {
        crate::downloads::media_download_badge(self.media_uri(), self.downloaded())
    }
    fn set_downloaded(&mut self, value: bool);
    fn field(&self, field: LibraryField) -> String;
    fn track_number(&self) -> Option<i64>;
    fn links(&self, field: LibraryField) -> rufin_core::route::detail_links::DetailLinks {
        rufin_core::route::detail_links::DetailLinks::text(&self.field(field))
    }
}

impl TrackPresentation for TrackRow {
    fn track_key(&self) -> Option<library::TrackKey> {
        Some(self.track_key)
    }
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
        self.state.favorite
    }
    fn downloaded(&self) -> bool {
        self.is_downloaded
    }
    fn set_downloaded(&mut self, value: bool) {
        self.is_downloaded = value;
    }
    fn track_number(&self) -> Option<i64> {
        Some(self.track_number)
    }
    fn field(&self, field: LibraryField) -> String {
        track_row_field(self, field)
    }
    fn links(&self, field: LibraryField) -> rufin_core::route::detail_links::DetailLinks {
        rufin_core::route::detail_links::metadata_links(
            field,
            &self.field(field),
            self.links.album_media_uri.as_deref(),
            &self.links.artists,
            &self.links.album_artists,
        )
    }
}

impl TrackPresentation for std::sync::Arc<QueueRow> {
    fn track_key(&self) -> Option<library::TrackKey> {
        Some(self.track.track_key)
    }
    fn media_uri(&self) -> &str {
        &self.track.media_uri
    }
    fn title(&self) -> &str {
        &self.track.metadata.title
    }
    fn artist(&self) -> &str {
        &self.track.metadata.artist
    }
    fn artwork(&self) -> Option<&[u8]> {
        self.track.artwork_binding.as_deref()
    }
    fn favorite(&self) -> bool {
        self.track.state.favorite
    }
    fn downloaded(&self) -> bool {
        self.track.access.is_downloaded
    }
    fn set_downloaded(&mut self, value: bool) {
        std::sync::Arc::make_mut(self).track.access.is_downloaded = value;
    }
    fn track_number(&self) -> Option<i64> {
        Some(self.track.metadata.track_number)
    }
    fn field(&self, field: LibraryField) -> String {
        track_field(&self.track, field)
    }
    fn links(&self, field: LibraryField) -> rufin_core::route::detail_links::DetailLinks {
        rufin_core::route::detail_links::metadata_links(
            field,
            &self.field(field),
            self.track.links.album_media_uri.as_deref(),
            &self.track.links.artists,
            &self.track.links.album_artists,
        )
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

pub fn playlist_artwork(playlist: &PlaylistRow) -> Vec<ArtworkBinding> {
    playlist
        .artwork_binding
        .iter()
        .map(|binding| ArtworkBinding::opaque(binding))
        .collect()
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
pub fn column_fit_width(field: LibraryField, width: i32) -> i32 {
    if field == LibraryField::TitleMerged {
        width.saturating_add(72)
    } else {
        width
    }
}
pub fn column_width(field: LibraryField) -> i32 {
    match field {
        LibraryField::FilePath => 320,
        LibraryField::Source => 160,
        LibraryField::RowIndex => 48,
        LibraryField::Image => 56,
        LibraryField::Favorite => crate::favorites::FAVORITE_COLUMN_WIDTH,
        LibraryField::Tools => crate::recycled_cells::ROW_ACTIONS_WIDTH,
        LibraryField::Title | LibraryField::TitleMerged => 220,
        LibraryField::Album
        | LibraryField::Artist
        | LibraryField::AlbumArtist
        | LibraryField::Genre => 220,
        LibraryField::ReleaseDate | LibraryField::DateAdded => 118,
        LibraryField::LastPlayed => 148,
        LibraryField::PlayCount => play_count_column_width(),
        LibraryField::UserRating | LibraryField::SongCount | LibraryField::AlbumCount => 96,
        LibraryField::Year
        | LibraryField::DiscNumber
        | LibraryField::TrackNumber
        | LibraryField::Bpm => 68,
        LibraryField::Duration => 76,
        _ => 100,
    }
}
pub fn play_count_column_width() -> i32 {
    compact_header_column_width(msgid("Plays"), 56)
}
pub fn compact_header_column_width(header: &str, min_width: i32) -> i32 {
    crate::table_sizing::compact_header_text_width(&tr(header), min_width)
}
