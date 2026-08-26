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

integer_key!(AlbumDetailRouteKey);

impl AlbumDetailRouteKey {
    pub fn album(album: AlbumKey) -> Option<Self> {
        Self::album_with_genres(album, false)
    }

    pub fn album_with_genres(album: AlbumKey, has_genres: bool) -> Option<Self> {
        album
            .raw()
            .checked_mul(4)
            .and_then(|value| value.checked_add(i64::from(has_genres)))
            .map(Self::from_raw)
    }

    pub fn track(track: TrackKey) -> Option<Self> {
        track
            .raw()
            .checked_mul(4)
            .and_then(|value| value.checked_add(2))
            .map(Self::from_raw)
    }

    pub fn album_key(self) -> Option<AlbumKey> {
        (self.raw() > 0 && matches!(self.raw() % 4, 0 | 1))
            .then(|| AlbumKey::from_raw(self.raw() / 4))
    }

    pub fn album_has_genres(self) -> bool {
        self.raw() > 0 && self.raw() % 4 == 1
    }

    pub fn track_key(self) -> Option<TrackKey> {
        (self.raw() > 0 && self.raw() % 4 == 2).then(|| TrackKey::from_raw(self.raw() / 4))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn album_detail_identity_carries_genre_extent_without_changing_entity_identity() {
        let album = AlbumKey::from_raw(9);
        let plain = AlbumDetailRouteKey::album_with_genres(album, false).unwrap();
        let genres = AlbumDetailRouteKey::album_with_genres(album, true).unwrap();
        assert_ne!(plain, genres);
        assert_eq!(plain.album_key(), Some(album));
        assert_eq!(genres.album_key(), Some(album));
        assert!(!plain.album_has_genres());
        assert!(genres.album_has_genres());

        let track = TrackKey::from_raw(11);
        let track_route = AlbumDetailRouteKey::track(track).unwrap();
        assert_eq!(track_route.track_key(), Some(track));
        assert_eq!(track_route.album_key(), None);
    }
}
