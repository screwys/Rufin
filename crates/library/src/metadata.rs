use std::collections::HashSet;

use thiserror::Error;

use crate::{Album, AlbumId, Artist, ArtistId, Track, TrackId, TrackSelection};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum MetadataItemId {
    Track(TrackId),
    Album(AlbumId),
    Artist(ArtistId),
}

impl MetadataItemId {
    pub fn has_exact_musicbrainz_identity(&self, values: &MetadataValues) -> bool {
        let usable = |value: Option<&str>| value.is_some_and(is_musicbrainz_id);
        match self {
            Self::Track(_) => {
                usable(values.musicbrainz_recording_id.as_deref())
                    || usable(values.musicbrainz_album_id.as_deref())
                        && usable(values.musicbrainz_release_track_id.as_deref())
            }
            Self::Album(_) => {
                usable(values.musicbrainz_album_id.as_deref())
                    || usable(values.musicbrainz_release_group_id.as_deref())
            }
            Self::Artist(_) => usable(values.musicbrainz_artist_id.as_deref()),
        }
    }
}

#[derive(Clone, Debug)]
pub enum MetadataItem {
    Track(Track),
    Album(Album),
    Artist(Artist),
}

impl MetadataItem {
    pub fn id(&self) -> MetadataItemId {
        match self {
            Self::Track(track) => MetadataItemId::Track(track.id.clone()),
            Self::Album(album) => MetadataItemId::Album(album.id.clone()),
            Self::Artist(artist) => MetadataItemId::Artist(artist.id.clone()),
        }
    }
}

/// One source-owned metadata item and its compact backing-track scope.
///
/// Remote sources may edit the aggregate item directly. File-backed sources
/// materialize the tracks only while reading or writing metadata.
#[derive(Clone, Debug)]
pub struct MetadataSubject {
    item: MetadataItem,
    tracks: Option<TrackSelection>,
}

impl MetadataSubject {
    pub fn track(track: Track) -> Self {
        Self {
            item: MetadataItem::Track(track),
            tracks: None,
        }
    }

    pub fn aggregate(item: MetadataItem, tracks: TrackSelection) -> Self {
        debug_assert!(!matches!(item, MetadataItem::Track(_)));
        Self {
            item,
            tracks: Some(tracks),
        }
    }

    pub fn item(&self) -> &MetadataItem {
        &self.item
    }

    pub fn into_item(self) -> MetadataItem {
        self.item
    }

    pub fn tracks(&self) -> Option<&TrackSelection> {
        self.tracks.as_ref()
    }

    pub fn id(&self) -> MetadataItemId {
        self.item.id()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MetadataField {
    Title,
    SortTitle,
    Artist,
    Album,
    AlbumArtist,
    TrackNumber,
    DiscNumber,
    Year,
    Genre,
    Comment,
    Bpm,
    LockData,
    MusicBrainzRecordingId,
    MusicBrainzReleaseTrackId,
    MusicBrainzAlbumId,
    MusicBrainzReleaseGroupId,
    MusicBrainzArtistId,
    Lyrics,
}

/// Metadata fields an opened source can write for one exact library item.
///
/// The source owns this decision because a Local file format and a remote
/// server may expose different write paths. A missing value means the item's
/// context menu must not offer metadata editing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataEditing {
    fields: Vec<MetadataField>,
}

impl MetadataEditing {
    pub fn new(fields: Vec<MetadataField>) -> Self {
        Self { fields }
    }

    pub fn fields(&self) -> &[MetadataField] {
        &self.fields
    }

    pub fn includes(&self, field: MetadataField) -> bool {
        self.fields.contains(&field)
    }

    pub fn identification_changes(
        &self,
        current: &MetadataValues,
        candidate: &MetadataValues,
    ) -> bool {
        self.fields
            .iter()
            .copied()
            .any(|field| identification_changes_field(current, candidate, field))
    }
}

fn identification_changes_field(
    current: &MetadataValues,
    candidate: &MetadataValues,
    field: MetadataField,
) -> bool {
    match field {
        MetadataField::Title => !candidate.title.is_empty() && candidate.title != current.title,
        MetadataField::SortTitle => changed(&current.sort_title, &candidate.sort_title),
        MetadataField::Artist => changed(&current.artist, &candidate.artist),
        MetadataField::Album => changed(&current.album, &candidate.album),
        MetadataField::AlbumArtist => changed(&current.album_artist, &candidate.album_artist),
        MetadataField::TrackNumber => changed(&current.track_number, &candidate.track_number),
        MetadataField::DiscNumber => changed(&current.disc_number, &candidate.disc_number),
        MetadataField::Year => changed(&current.year, &candidate.year),
        MetadataField::Genre => changed(&current.genre, &candidate.genre),
        MetadataField::Comment => changed(&current.comment, &candidate.comment),
        MetadataField::Bpm => changed(&current.bpm, &candidate.bpm),
        MetadataField::MusicBrainzRecordingId => changed(
            &current.musicbrainz_recording_id,
            &candidate.musicbrainz_recording_id,
        ),
        MetadataField::MusicBrainzReleaseTrackId => changed(
            &current.musicbrainz_release_track_id,
            &candidate.musicbrainz_release_track_id,
        ),
        MetadataField::MusicBrainzAlbumId => changed(
            &current.musicbrainz_album_id,
            &candidate.musicbrainz_album_id,
        ),
        MetadataField::MusicBrainzReleaseGroupId => changed(
            &current.musicbrainz_release_group_id,
            &candidate.musicbrainz_release_group_id,
        ),
        MetadataField::MusicBrainzArtistId => changed(
            &current.musicbrainz_artist_id,
            &candidate.musicbrainz_artist_id,
        ),
        MetadataField::LockData => false,
        MetadataField::Lyrics => false,
    }
}

fn changed<T: PartialEq>(current: &Option<T>, candidate: &Option<T>) -> bool {
    candidate
        .as_ref()
        .is_some_and(|candidate| current.as_ref() != Some(candidate))
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MetadataValues {
    pub title: String,
    pub sort_title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub track_number: Option<u16>,
    pub disc_number: Option<u16>,
    pub year: Option<u16>,
    pub genre: Option<String>,
    pub comment: Option<String>,
    pub bpm: Option<u16>,
    pub lock_data: Option<bool>,
    pub musicbrainz_recording_id: Option<String>,
    pub musicbrainz_release_track_id: Option<String>,
    pub musicbrainz_album_id: Option<String>,
    pub musicbrainz_release_group_id: Option<String>,
    pub musicbrainz_artist_id: Option<String>,
}

/// One metadata candidate and the source-owned operation that can apply it.
///
/// Sources without a native identification operation leave `application`
/// empty and write the reviewed values as ordinary metadata changes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataIdentification {
    pub values: MetadataValues,
    pub application: Option<MetadataApplication>,
}

impl MetadataIdentification {
    pub fn values(values: MetadataValues) -> Self {
        Self {
            values,
            application: None,
        }
    }

    pub fn source(values: MetadataValues, application: MetadataApplication) -> Self {
        Self {
            values,
            application: Some(application),
        }
    }
}

/// Opaque input returned by a source's Identify operation and consumed by Save.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataApplication(String);

impl MetadataApplication {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataDraft {
    pub item_id: MetadataItemId,
    pub editing: MetadataEditing,
    pub source_search: bool,
    pub revision: Option<String>,
    pub values: MetadataValues,
    pub scope: MetadataScope,
    pub mixed_fields: HashSet<MetadataField>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetadataScope {
    Item,
    Tracks(usize),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MetadataChange {
    Title(String),
    SortTitle(Option<String>),
    Artist(Option<String>),
    Album(Option<String>),
    AlbumArtist(Option<String>),
    TrackNumber(Option<u16>),
    DiscNumber(Option<u16>),
    Year(Option<u16>),
    Genre(Option<String>),
    Comment(Option<String>),
    Bpm(Option<u16>),
    LockData(bool),
    MusicBrainzRecordingId(Option<String>),
    MusicBrainzReleaseTrackId(Option<String>),
    MusicBrainzAlbumId(Option<String>),
    MusicBrainzReleaseGroupId(Option<String>),
    MusicBrainzArtistId(Option<String>),
    Lyrics(Option<String>),
}

impl MetadataChange {
    pub fn field(&self) -> MetadataField {
        match self {
            Self::Title(_) => MetadataField::Title,
            Self::SortTitle(_) => MetadataField::SortTitle,
            Self::Artist(_) => MetadataField::Artist,
            Self::Album(_) => MetadataField::Album,
            Self::AlbumArtist(_) => MetadataField::AlbumArtist,
            Self::TrackNumber(_) => MetadataField::TrackNumber,
            Self::DiscNumber(_) => MetadataField::DiscNumber,
            Self::Year(_) => MetadataField::Year,
            Self::Genre(_) => MetadataField::Genre,
            Self::Comment(_) => MetadataField::Comment,
            Self::Bpm(_) => MetadataField::Bpm,
            Self::LockData(_) => MetadataField::LockData,
            Self::MusicBrainzRecordingId(_) => MetadataField::MusicBrainzRecordingId,
            Self::MusicBrainzReleaseTrackId(_) => MetadataField::MusicBrainzReleaseTrackId,
            Self::MusicBrainzAlbumId(_) => MetadataField::MusicBrainzAlbumId,
            Self::MusicBrainzReleaseGroupId(_) => MetadataField::MusicBrainzReleaseGroupId,
            Self::MusicBrainzArtistId(_) => MetadataField::MusicBrainzArtistId,
            Self::Lyrics(_) => MetadataField::Lyrics,
        }
    }

    pub fn matches(&self, values: &MetadataValues) -> bool {
        let normalized = |value: &Option<String>| {
            value
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        };
        match self {
            Self::Title(value) => values.title == value.trim(),
            Self::SortTitle(value) => values.sort_title == normalized(value),
            Self::Artist(value) => values.artist == normalized(value),
            Self::Album(value) => values.album == normalized(value),
            Self::AlbumArtist(value) => values.album_artist == normalized(value),
            Self::TrackNumber(value) => values.track_number == *value,
            Self::DiscNumber(value) => values.disc_number == *value,
            Self::Year(value) => values.year == *value,
            Self::Genre(value) => values.genre == normalized(value),
            Self::Comment(value) => values.comment == normalized(value),
            Self::Bpm(value) => values.bpm == *value,
            Self::LockData(value) => values.lock_data == Some(*value),
            Self::MusicBrainzRecordingId(value) => {
                values.musicbrainz_recording_id == normalized(value)
            }
            Self::MusicBrainzReleaseTrackId(value) => {
                values.musicbrainz_release_track_id == normalized(value)
            }
            Self::MusicBrainzAlbumId(value) => values.musicbrainz_album_id == normalized(value),
            Self::MusicBrainzReleaseGroupId(value) => {
                values.musicbrainz_release_group_id == normalized(value)
            }
            Self::MusicBrainzArtistId(value) => values.musicbrainz_artist_id == normalized(value),
            Self::Lyrics(_) => true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataEdit {
    pub item_id: MetadataItemId,
    pub revision: Option<String>,
    pub application: Option<MetadataApplication>,
    pub changes: Vec<MetadataChange>,
}

impl MetadataEdit {
    pub fn validate(&self, editing: &MetadataEditing) -> Result<(), MetadataError> {
        let mut seen = HashSet::new();
        for change in &self.changes {
            let field = change.field();
            if !editing.includes(field) {
                return Err(MetadataError::Invalid {
                    field,
                    message: "This metadata field cannot be edited for this item.".to_string(),
                });
            }
            if !seen.insert(field) {
                return Err(MetadataError::Invalid {
                    field,
                    message: "This metadata field was changed more than once.".to_string(),
                });
            }
            match change {
                MetadataChange::Title(value) if value.trim().is_empty() => {
                    return Err(MetadataError::Invalid {
                        field,
                        message: "Title cannot be empty.".to_string(),
                    });
                }
                MetadataChange::TrackNumber(Some(0))
                | MetadataChange::DiscNumber(Some(0))
                | MetadataChange::Year(Some(0))
                | MetadataChange::Bpm(Some(0)) => {
                    return Err(MetadataError::Invalid {
                        field,
                        message: "Use an empty value to clear this field.".to_string(),
                    });
                }
                MetadataChange::MusicBrainzRecordingId(Some(value))
                | MetadataChange::MusicBrainzReleaseTrackId(Some(value))
                | MetadataChange::MusicBrainzAlbumId(Some(value))
                | MetadataChange::MusicBrainzReleaseGroupId(Some(value))
                | MetadataChange::MusicBrainzArtistId(Some(value))
                    if !is_musicbrainz_id(value) =>
                {
                    return Err(MetadataError::Invalid {
                        field,
                        message: "MusicBrainz IDs must be UUIDs.".to_string(),
                    });
                }
                _ => {}
            }
        }
        Ok(())
    }
}

pub fn is_musicbrainz_id(value: &str) -> bool {
    let value = value.trim().as_bytes();
    value.len() == 36
        && value.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum MetadataError {
    #[error("Metadata editing is not available for this item.")]
    Unavailable,
    #[error("Configure local file access before editing this server's metadata.")]
    LocalAccessRequired { source_path: String },
    #[error("This item changed after the metadata editor opened. Reopen it and try again.")]
    Conflict,
    #[error("{message}")]
    Invalid {
        field: MetadataField,
        message: String,
    },
    #[error("{0}")]
    Write(String),
    #[error("The metadata was saved, but Rufin could not refresh the library: {0}")]
    SavedRefreshFailed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editing(fields: &[MetadataField]) -> MetadataEditing {
        MetadataEditing::new(fields.to_vec())
    }

    fn edit(changes: Vec<MetadataChange>) -> MetadataEdit {
        MetadataEdit {
            item_id: MetadataItemId::Track(TrackId::fake(1)),
            revision: None,
            application: None,
            changes,
        }
    }

    #[test]
    fn validation_rejects_unsupported_duplicate_and_empty_required_values() {
        let title_year_editing = editing(&[MetadataField::Title, MetadataField::Year]);

        assert!(matches!(
            edit(vec![MetadataChange::Artist(Some("Artist".to_string()))])
                .validate(&title_year_editing),
            Err(MetadataError::Invalid {
                field: MetadataField::Artist,
                ..
            })
        ));
        assert!(matches!(
            edit(vec![
                MetadataChange::Year(Some(2025)),
                MetadataChange::Year(None),
            ])
            .validate(&title_year_editing),
            Err(MetadataError::Invalid {
                field: MetadataField::Year,
                ..
            })
        ));
        assert!(matches!(
            edit(vec![MetadataChange::Title("  ".to_string())]).validate(&title_year_editing),
            Err(MetadataError::Invalid {
                field: MetadataField::Title,
                ..
            })
        ));

        let mbid_editing = editing(&[MetadataField::MusicBrainzRecordingId]);
        assert!(matches!(
            edit(vec![MetadataChange::MusicBrainzRecordingId(Some(
                "not-an-mbid".to_string(),
            ))])
            .validate(&mbid_editing),
            Err(MetadataError::Invalid {
                field: MetadataField::MusicBrainzRecordingId,
                ..
            })
        ));
    }

    #[test]
    fn validation_uses_empty_numeric_values_for_clearing() {
        let editing = editing(&[MetadataField::Year]);

        assert!(
            edit(vec![MetadataChange::Year(None)])
                .validate(&editing)
                .is_ok()
        );
        assert!(matches!(
            edit(vec![MetadataChange::Year(Some(0))]).validate(&editing),
            Err(MetadataError::Invalid {
                field: MetadataField::Year,
                ..
            })
        ));
    }

    #[test]
    fn exact_musicbrainz_identity_requires_the_ids_for_each_item_kind() {
        const RECORDING_ID: &str = "01234567-89ab-cdef-0123-456789abcdef";
        const RELEASE_ID: &str = "11234567-89ab-cdef-0123-456789abcdef";
        const RELEASE_TRACK_ID: &str = "31234567-89ab-cdef-0123-456789abcdef";
        const ARTIST_ID: &str = "21234567-89ab-cdef-0123-456789abcdef";
        let track = MetadataItemId::Track(TrackId::fake(1));
        let album = MetadataItemId::Album(AlbumId::fake(1));
        let artist = MetadataItemId::Artist(ArtistId::fake(1));
        let mut values = MetadataValues::default();

        assert!(!track.has_exact_musicbrainz_identity(&values));
        values.musicbrainz_recording_id = Some("not-an-mbid".to_string());
        assert!(!track.has_exact_musicbrainz_identity(&values));
        values.musicbrainz_recording_id = Some(RECORDING_ID.to_string());
        assert!(track.has_exact_musicbrainz_identity(&values));

        values = MetadataValues {
            musicbrainz_album_id: Some(RELEASE_ID.to_string()),
            musicbrainz_release_track_id: Some(RELEASE_TRACK_ID.to_string()),
            ..MetadataValues::default()
        };
        assert!(track.has_exact_musicbrainz_identity(&values));

        values = MetadataValues {
            musicbrainz_album_id: Some(RELEASE_ID.to_string()),
            ..MetadataValues::default()
        };
        assert!(album.has_exact_musicbrainz_identity(&values));
        assert!(!artist.has_exact_musicbrainz_identity(&values));

        values.musicbrainz_artist_id = Some(ARTIST_ID.to_string());
        assert!(artist.has_exact_musicbrainz_identity(&values));
    }

    #[test]
    fn identification_candidates_need_a_changed_writable_value() {
        let current = MetadataValues {
            title: "Current".to_string(),
            artist: Some("Current artist".to_string()),
            ..MetadataValues::default()
        };
        let title_only = editing(&[MetadataField::Title]);
        let artist_only = editing(&[MetadataField::Artist]);

        assert!(!title_only.identification_changes(&current, &current));
        assert!(!title_only.identification_changes(
            &current,
            &MetadataValues {
                artist: Some("Identified artist".to_string()),
                ..current.clone()
            }
        ));
        assert!(artist_only.identification_changes(
            &current,
            &MetadataValues {
                artist: Some("Identified artist".to_string()),
                ..current.clone()
            }
        ));
        assert!(!title_only.identification_changes(
            &current,
            &MetadataValues {
                title: String::new(),
                ..current.clone()
            }
        ));
    }
}
