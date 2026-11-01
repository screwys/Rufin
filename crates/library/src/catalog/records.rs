//! Owned rows of the resident tables. A Write reads these by key, stages and compares them, and
//! the Snapshot packs their resident part. Every field is a column the Store holds, so a staged
//! row and the row the Store holds compare column by column.

use crate::tags::TagKind;
use crate::{
    AlbumKey, AlbumMetadata, ArtistKey, ArtistMetadata, FolderKey, FolderMetadata, NativeUserState,
    SourceKey, TagMetadata, TrackKey, TrackMetadata,
};

/// Whether an artwork binding names a picture: it is neither missing nor an empty list.
pub(crate) fn pictured(binding: Option<&[u8]>) -> bool {
    let Some(binding) = binding else {
        return false;
    };
    let mut text = binding.iter().filter(|byte| !byte.is_ascii_whitespace());
    !matches!(
        (text.next(), text.next(), text.next()),
        (None, _, _) | (Some(b'['), Some(b']'), None)
    )
}

/// The `flags` bit of a row with this binding.
pub(crate) fn picture_flag(binding: Option<&[u8]>) -> i64 {
    i64::from(pictured(binding)) * crate::schema::HAS_PICTURE
}

/// A unix time as SQLite's unixepoch() reads ISO-8601 text.
pub(crate) fn unix_time(text: &str) -> Option<i64> {
    use chrono::{DateTime, NaiveDate, NaiveDateTime};
    let text = text.trim();
    if let Ok(time) = DateTime::parse_from_rfc3339(text) {
        return Some(time.timestamp());
    }
    for format in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(time) = NaiveDateTime::parse_from_str(text, format) {
            return Some(time.and_utc().timestamp());
        }
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .ok()
        .map(|date| {
            date.and_hms_opt(0, 0, 0)
                .unwrap_or_default()
                .and_utc()
                .timestamp()
        })
}

/// Packs YYYY, YYYY-MM or YYYY-MM-DD, and day-first DD.MM.YYYY or DD/MM/YYYY, as yyyymmdd.
/// Missing parts are zero.
pub(crate) fn release(text: Option<&str>) -> u32 {
    let Some(text) = text else { return 0 };
    let parts: Vec<Option<u32>> = text
        .trim()
        .splitn(3, ['-', '.', '/'])
        .map(|part| {
            part.get(..part.len().min(4))
                .and_then(|digits| digits.parse::<u32>().ok())
        })
        .collect();
    let day_first =
        parts.len() == 3 && text.trim().len() >= 8 && parts[2].is_some_and(|year| year >= 1000);
    let (year, month, day) = if day_first {
        (parts[2], parts[1], parts[0])
    } else {
        (
            parts.first().copied().flatten(),
            parts.get(1).copied().flatten(),
            parts.get(2).copied().flatten(),
        )
    };
    year.unwrap_or(0).min(9999) * 10_000
        + month.unwrap_or(0).min(99) * 100
        + day.unwrap_or(0).min(99)
}

/// The ordering and display time of a row: its tagged date added, else its first observation.
pub(crate) fn added(date_added: Option<&str>, first_seen: Option<i64>) -> Option<i64> {
    date_added.and_then(unix_time).or(first_seen)
}

/// Favorites, ratings and play counts a source reports for its own row.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct NativeState {
    pub(crate) favorite: Option<bool>,
    /// 0..100.
    pub(crate) rating: Option<i64>,
    pub(crate) play_count: Option<i64>,
    pub(crate) last_played: Option<i64>,
}

impl NativeState {
    /// Sparse source facts: an absent field keeps the stored value.
    pub(crate) fn merge(&mut self, incoming: &NativeUserState) {
        self.favorite = incoming.favorite.or(self.favorite);
        if let Some(rating) = incoming.rating {
            self.rating = rating;
        }
        self.play_count = incoming.play_count.or(self.play_count);
        self.last_played = incoming.last_played.or(self.last_played);
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Source {
    pub(crate) key: SourceKey,
    pub(crate) object: String,
    pub(crate) kind: Option<String>,
    pub(crate) name: String,
    pub(crate) complete: bool,
    pub(crate) freshness: Option<Vec<u8>>,
    /// Server time at the start of the last completed listing or catch-up.
    pub(crate) synced_at: Option<i64>,
}

impl Source {
    /// Whether the source reads files: its albums come from file tags.
    pub(crate) fn file_source(&self) -> bool {
        self.kind
            .as_deref()
            .and_then(crate::SourceKind::parse)
            .is_some_and(crate::SourceKind::file_library)
    }
}

/// The typed columns a track takes from its metadata. `track_details.metadata` holds the rest.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct TrackFacts {
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) album: String,
    pub(crate) sort: String,
    pub(crate) duration_ms: i64,
    pub(crate) disc: i64,
    pub(crate) number: i64,
    pub(crate) year: Option<i64>,
    pub(crate) release_date: Option<String>,
    pub(crate) date_added: Option<String>,
    pub(crate) bpm: Option<i64>,
    pub(crate) comment: Option<String>,
    pub(crate) format: Option<String>,
}

impl TrackFacts {
    pub(crate) fn of(metadata: &TrackMetadata) -> Self {
        Self {
            title: metadata.title.clone(),
            artist: metadata.artist.clone(),
            album: metadata.album.clone(),
            sort: metadata.sort_text.clone(),
            duration_ms: metadata.duration_millis,
            disc: metadata.disc_number,
            number: metadata.track_number,
            year: metadata.year,
            release_date: metadata.release_date.clone(),
            date_added: metadata.date_added.clone(),
            bpm: metadata.bpm,
            comment: metadata.comment.clone(),
            format: metadata.source_format.clone(),
        }
    }
}

/// Track columns a Write stores and never reads back: a set one is written.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct TrackWrites {
    /// The details JSON.
    pub(crate) metadata: Option<serde_json::Value>,
}

/// A `tracks` row with its links and the `track_details` columns the merge compares.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Track {
    pub(crate) key: TrackKey,
    pub(crate) source: Option<SourceKey>,
    pub(crate) object: Option<String>,
    pub(crate) uri: String,
    pub(crate) album: Option<AlbumKey>,
    pub(crate) indexed: bool,
    /// Set by the Write when a track stops being listed, cleared when it is listed again.
    pub(crate) hidden_since: Option<i64>,
    pub(crate) native: bool,
    pub(crate) facts: TrackFacts,
    pub(crate) first_seen: Option<i64>,
    pub(crate) state: NativeState,
    pub(crate) binding: Option<Vec<u8>>,
    pub(crate) source_path: Option<String>,
    pub(crate) cue_path: Option<String>,
    pub(crate) cue_start: Option<i64>,
    pub(crate) cue_end: Option<i64>,
    /// When the audio may have changed, and the hash of the audio analysis is keyed by.
    pub(crate) audio_revision: Option<Vec<u8>>,
    pub(crate) audio_hash: Option<Vec<u8>>,
    pub(crate) artists: Vec<ArtistKey>,
    pub(crate) album_artists: Vec<ArtistKey>,
    pub(crate) tags: Vec<(TagKind, i64)>,
    pub(crate) folders: Vec<FolderKey>,
    pub(crate) writes: TrackWrites,
}

impl Track {
    pub(crate) fn new(source: Option<SourceKey>, object: Option<String>, uri: String) -> Self {
        Self {
            key: TrackKey::from_raw(0),
            source,
            object,
            uri,
            album: None,
            indexed: false,
            hidden_since: None,
            native: false,
            facts: TrackFacts::default(),
            first_seen: None,
            state: NativeState::default(),
            binding: None,
            source_path: None,
            cue_path: None,
            cue_start: None,
            cue_end: None,
            audio_revision: None,
            audio_hash: None,
            artists: Vec::new(),
            album_artists: Vec::new(),
            tags: Vec::new(),
            folders: Vec::new(),
            writes: TrackWrites::default(),
        }
    }

    /// The path that orders album members.
    pub(crate) fn member_path(&self) -> Option<&str> {
        self.cue_path.as_deref().or(self.source_path.as_deref())
    }

    /// The `tracks.flags` bits, copied from the details columns so the load never reads them.
    pub(crate) fn flags(&self) -> i64 {
        picture_flag(self.binding.as_deref())
            | i64::from(self.cue_path.is_some()) * crate::schema::TRACK_CUE
    }
}

/// The typed columns an album takes from its resolved metadata.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct AlbumFacts {
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) sort: String,
    pub(crate) year: Option<i64>,
    pub(crate) release_date: Option<String>,
    pub(crate) date_added: Option<String>,
    pub(crate) compilation: Option<bool>,
}

impl AlbumFacts {
    pub(crate) fn of(metadata: &AlbumMetadata) -> Self {
        Self {
            title: metadata.title.clone(),
            artist: metadata.artist.clone(),
            sort: metadata.sort_text.clone(),
            year: metadata.year,
            release_date: metadata.release_date.clone(),
            date_added: metadata.date_added.clone(),
            compilation: metadata.is_compilation,
        }
    }
}

/// Album columns a Write stores and never reads back: a set one is written.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct AlbumWrites {
    /// The header a source observed; None clears it.
    pub(crate) header: Option<Option<serde_json::Value>>,
    /// The rest of the resolved metadata.
    pub(crate) metadata: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Album {
    pub(crate) key: AlbumKey,
    pub(crate) source: SourceKey,
    pub(crate) object: String,
    pub(crate) uri: String,
    pub(crate) indexed: bool,
    pub(crate) native: bool,
    pub(crate) cached_local: bool,
    pub(crate) facts: AlbumFacts,
    pub(crate) first_seen: Option<i64>,
    pub(crate) release_identity: Option<String>,
    pub(crate) release_lookup_identity: Option<String>,
    pub(crate) state: NativeState,
    pub(crate) binding: Option<Vec<u8>>,
    /// Whether a source observed a header; the header itself is read by key.
    pub(crate) input: bool,
    /// Resolved from its compilation flag and release types, and kept in `flags`.
    pub(crate) class: crate::AlbumReleaseClass,
    pub(crate) artists: Vec<ArtistKey>,
    pub(crate) genres: Vec<i64>,
    pub(crate) writes: AlbumWrites,
}

impl Album {
    pub(crate) fn new(source: SourceKey, object: &str, uri: String) -> Self {
        Self {
            key: AlbumKey::from_raw(0),
            source,
            object: object.to_owned(),
            uri,
            indexed: false,
            native: false,
            cached_local: false,
            facts: AlbumFacts::default(),
            first_seen: None,
            release_identity: None,
            release_lookup_identity: None,
            state: NativeState::default(),
            binding: None,
            input: false,
            class: crate::AlbumReleaseClass::Album,
            artists: Vec::new(),
            genres: Vec::new(),
            writes: AlbumWrites::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Artist {
    pub(crate) key: ArtistKey,
    pub(crate) source: SourceKey,
    pub(crate) object: String,
    pub(crate) uri: String,
    pub(crate) indexed: bool,
    pub(crate) native: bool,
    pub(crate) metadata: ArtistMetadata,
    pub(crate) state: NativeState,
    pub(crate) binding: Option<Vec<u8>>,
}

impl Artist {
    pub(crate) fn new(source: SourceKey, object: &str, uri: String) -> Self {
        Self {
            key: ArtistKey::from_raw(0),
            source,
            object: object.to_owned(),
            uri,
            indexed: false,
            native: false,
            metadata: ArtistMetadata::default(),
            state: NativeState::default(),
            binding: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Tag {
    pub(crate) kind: TagKind,
    pub(crate) key: i64,
    pub(crate) source: SourceKey,
    pub(crate) object: String,
    pub(crate) uri: String,
    pub(crate) indexed: bool,
    pub(crate) native: bool,
    pub(crate) metadata: TagMetadata,
    pub(crate) binding: Option<Vec<u8>>,
}

impl Tag {
    pub(crate) fn new(kind: TagKind, source: SourceKey, object: &str, uri: String) -> Self {
        Self {
            kind,
            key: 0,
            source,
            object: object.to_owned(),
            uri,
            indexed: false,
            native: false,
            metadata: TagMetadata::default(),
            binding: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Folder {
    pub(crate) key: FolderKey,
    pub(crate) source: SourceKey,
    pub(crate) object: String,
    pub(crate) uri: String,
    pub(crate) indexed: bool,
    pub(crate) native: bool,
    pub(crate) metadata: FolderMetadata,
    pub(crate) binding: Option<Vec<u8>>,
}

impl Folder {
    pub(crate) fn new(source: SourceKey, object: &str, uri: String) -> Self {
        Self {
            key: FolderKey::from_raw(0),
            source,
            object: object.to_owned(),
            uri,
            indexed: false,
            native: false,
            metadata: FolderMetadata::default(),
            binding: None,
        }
    }
}
