//! The fields a library list shows, sorts by and saves in its settings.

use serde::{Deserialize, Serialize};

/// A column, card text or sort of a library list. Saved settings name these variants, so their
/// serde names stay.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum LibraryField {
    RowIndex,
    Image,
    Title,
    TitleMerged,
    Artist,
    AlbumArtist,
    Album,
    Year,
    ReleaseDate,
    DateAdded,
    LastPlayed,
    PlayCount,
    UserRating,
    Genre,
    Grouping,
    Bpm,
    Bitrate,
    SampleRate,
    BitDepth,
    Channels,
    Format,
    FilePath,
    Source,
    TrackNumber,
    DiscNumber,
    SongCount,
    AlbumCount,
    Duration,
    Favorite,
    Tools,
}

impl LibraryField {
    /// Whether plays move a list sorted by this field.
    pub fn activity_sensitive(self) -> bool {
        matches!(self, Self::PlayCount | Self::LastPlayed)
    }

    /// The sort this field orders by: a field no list sorts by orders by title.
    pub(crate) fn sort(self) -> Self {
        match self {
            Self::Image
            | Self::TitleMerged
            | Self::Bitrate
            | Self::SampleRate
            | Self::BitDepth
            | Self::Channels
            | Self::Format
            | Self::FilePath
            | Self::Grouping
            | Self::Source
            | Self::DiscNumber
            | Self::Tools => Self::Title,
            field => field,
        }
    }
}
