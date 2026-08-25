//! Shared integer identities used at Library and consumer boundaries.

use std::fmt;

macro_rules! integer_key {
    ($name:ident) => {
        #[derive(
            Clone,
            Copy,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
            serde::Deserialize,
            serde::Serialize,
            sqlx::Type,
        )]
        #[repr(transparent)]
        #[sqlx(transparent)]
        pub struct $name(i64);

        impl $name {
            pub const fn from_raw(value: i64) -> Self {
                Self(value)
            }

            pub const fn raw(self) -> i64 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

integer_key!(SourceKey);
integer_key!(TrackKey);
integer_key!(AlbumKey);
integer_key!(ArtistKey);
integer_key!(GenreKey);
integer_key!(MoodKey);
integer_key!(FolderKey);
integer_key!(PlaylistKey);
integer_key!(PlaylistEntryKey);
integer_key!(SmartPlaylistKey);
integer_key!(ListenKey);
integer_key!(ListenOutboxKey);
integer_key!(QueueOccurrenceKey);
integer_key!(LocalFileKey);
integer_key!(LocalAccessFileKey);
