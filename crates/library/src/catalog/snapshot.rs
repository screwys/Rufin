//! The resident Catalog: packed records in slots, one string arena and one edge arena. Slots stay
//! stable while the process runs; a removed record becomes a tombstone, and a reference to a
//! tombstone reads as none. Collection object ids are resident, and their URIs are built from
//! them. Track URIs and object ids, paths, details and artwork bindings are not resident; a
//! Write or a window's statement reads them by key.

use std::collections::{BTreeMap, HashMap, HashSet};

use bytemuck::{Pod, Zeroable};

use super::records::*;
use super::strings::{StrId, Strings};
use crate::tags::TagKind;
use crate::{AlbumKey, ArtistKey, FolderKey, SmartPlaylistDefinition, SourceKey, TrackKey};

pub(crate) const NONE: u32 = u32::MAX;
pub(crate) const NO_SOURCE: u16 = u16::MAX;
/// A rating or count that is not known.
pub(crate) const UNRATED: u8 = u8::MAX;

pub(crate) const INDEXED: u16 = 1;
pub(crate) const NATIVE: u16 = 1 << 1;
pub(crate) const DELETED: u16 = 1 << 2;
pub(crate) const PICTURE: u16 = 1 << 3;
pub(crate) const NATIVE_FAVORITE_KNOWN: u16 = 1 << 4;
pub(crate) const NATIVE_FAVORITE: u16 = 1 << 5;
/// The user's own favorite, which overrides the native one when it is known.
pub(crate) const FAVORITE_KNOWN: u16 = 1 << 6;
pub(crate) const FAVORITE: u16 = 1 << 7;
/// Tracks: a CUE window of a whole file.
pub(crate) const CUE: u16 = 1 << 8;
/// Tracks: a downloaded copy exists.
pub(crate) const DOWNLOADED: u16 = 1 << 9;
// Most track sort texts are the title or its lowercase, so they store no string.
pub(crate) const SORT_TITLE: u16 = 1 << 10;
pub(crate) const SORT_LOWER: u16 = 1 << 11;
/// Albums: the header comes from a local cache of a removed source.
pub(crate) const CACHED_LOCAL: u16 = 1 << 8;
/// Albums: a source observed a header.
pub(crate) const INPUT: u16 = 1 << 9;
pub(crate) const COMPILATION_KNOWN: u16 = 1 << 10;
pub(crate) const COMPILATION: u16 = 1 << 11;
/// Albums: the release class code, bits 12-14.
pub(crate) const RELEASE_SHIFT: u16 = 12;
pub(crate) const RELEASE_MASK: u16 = 0b111 << RELEASE_SHIFT;
/// Playlists: a source owns the playlist.
pub(crate) const SOURCE_OWNED: u16 = 1 << 8;
pub(crate) const WRITABLE: u16 = 1 << 9;
pub(crate) const METADATA_WRITABLE: u16 = 1 << 10;
/// Playlists: linked to a playlist file, so an edit marks the link dirty.
pub(crate) const LINKED: u16 = 1 << 11;

fn clamp_u32(value: i64) -> u32 {
    value.clamp(0, i64::from(NONE - 1)) as u32
}

fn clamp_u16(value: i64) -> u16 {
    value.clamp(0, i64::from(u16::MAX - 1)) as u16
}

fn optional_u32(value: Option<i64>) -> u32 {
    value.map_or(NONE, clamp_u32)
}

fn optional_u16(value: Option<i64>) -> u16 {
    value.map_or(u16::MAX, clamp_u16)
}

fn year(value: Option<i64>) -> i16 {
    value.map_or(i16::MIN, |year| year.clamp(-32_767, 32_767) as i16)
}

fn rating(value: Option<i64>) -> u8 {
    value.map_or(UNRATED, |rating| rating.clamp(0, 100) as u8)
}

/// The plays and last play a source reports for an album or artist.
fn native_plays(state: &NativeState) -> [u32; 2] {
    [
        optional_u32(state.play_count),
        optional_u32(state.last_played),
    ]
}

fn native_flags(indexed: bool, native: bool, picture: bool, state: &NativeState) -> u16 {
    u16::from(indexed) * INDEXED
        | u16::from(native) * NATIVE
        | u16::from(picture) * PICTURE
        | state.favorite.map_or(0, |favorite| {
            NATIVE_FAVORITE_KNOWN | u16::from(favorite) * NATIVE_FAVORITE
        })
}

/// The user's favorite and rating of a track, album or artist, kept in its record.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct UserState {
    pub(crate) favorite: Option<bool>,
    /// 0..100.
    pub(crate) rating: Option<u8>,
}

impl UserState {
    fn flags(self) -> u16 {
        self.favorite.map_or(0, |favorite| {
            FAVORITE_KNOWN | u16::from(favorite) * FAVORITE
        })
    }

    fn of(flags: u16, rating: u8) -> Self {
        Self {
            favorite: (flags & FAVORITE_KNOWN != 0).then_some(flags & FAVORITE != 0),
            rating: (rating != UNRATED).then_some(rating),
        }
    }
}

/// Plays a track's listens and carried-over counts add up to, and the counts its source reports.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
#[repr(C)]
pub(crate) struct Stats {
    pub(crate) plays: u32,
    pub(crate) skips: u32,
    pub(crate) last_played: u32,
    pub(crate) native_plays: u32,
    pub(crate) native_last_played: u32,
}

impl Default for Stats {
    fn default() -> Self {
        Self {
            plays: 0,
            skips: 0,
            last_played: NONE,
            native_plays: NONE,
            native_last_played: NONE,
        }
    }
}

impl Stats {
    /// The plays, skips and last play after one more listen.
    pub(crate) fn heard(self, skipped: bool, started_at: i64) -> Self {
        let last = (self.last_played != NONE).then_some(i64::from(self.last_played));
        Self::listened(
            i64::from(self.plays) + i64::from(!skipped),
            i64::from(self.skips) + i64::from(skipped),
            last.max(Some(started_at)),
        )
    }

    /// Plays, skips and last play counted from listens and carried-over counts.
    pub(crate) fn listened(plays: i64, skips: i64, last_played: Option<i64>) -> Self {
        Self {
            plays: clamp_u32(plays),
            skips: clamp_u32(skips),
            last_played: optional_u32(last_played),
            ..Self::default()
        }
    }
}

/// Records are plain bytes, so the image stores them as they are in memory. Keys are SQLite
/// rowids, allocated upward from 1, so they fit in 32 bits.
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
#[repr(C)]
pub(crate) struct TrackRec {
    pub(crate) key: u32,
    pub(crate) title: StrId,
    pub(crate) artist: StrId,
    pub(crate) album_title: StrId,
    pub(crate) sort: StrId,
    pub(crate) comment: StrId,
    pub(crate) format: StrId,
    pub(crate) album: u32,
    pub(crate) duration_ms: u32,
    /// The tagged date added, else the first observation, in unix seconds.
    pub(crate) added: u32,
    /// yyyymmdd.
    pub(crate) release: u32,
    /// The start of the run of artists, album artists, tags and folders.
    pub(crate) edges: u32,
    pub(crate) lens: [u16; 4],
    pub(crate) year: i16,
    pub(crate) disc: u16,
    pub(crate) number: u16,
    pub(crate) bpm: u16,
    pub(crate) source: u16,
    pub(crate) flags: u16,
    pub(crate) native_rating: u8,
    pub(crate) rating: u8,
    pub(crate) padding: u16,
}

#[derive(Clone, Copy, Debug, Pod, Zeroable)]
#[repr(C)]
pub(crate) struct AlbumRec {
    pub(crate) key: u32,
    pub(crate) title: StrId,
    pub(crate) artist: StrId,
    pub(crate) sort: StrId,
    pub(crate) object: StrId,
    pub(crate) added: u32,
    pub(crate) release: u32,
    /// The start of the run of artists and genres.
    pub(crate) edges: u32,
    /// The plays and last play its source reports; NONE when it reports none.
    pub(crate) native: [u32; 2],
    pub(crate) lens: [u16; 2],
    pub(crate) year: i16,
    pub(crate) source: u16,
    pub(crate) flags: u16,
    pub(crate) native_rating: u8,
    pub(crate) rating: u8,
}

#[derive(Clone, Copy, Debug, Pod, Zeroable)]
#[repr(C)]
pub(crate) struct ArtistRec {
    pub(crate) key: u32,
    pub(crate) name: StrId,
    pub(crate) sort: StrId,
    pub(crate) object: StrId,
    /// The plays and last play its source reports; NONE when it reports none.
    pub(crate) native: [u32; 2],
    pub(crate) source: u16,
    pub(crate) flags: u16,
    pub(crate) native_rating: u8,
    pub(crate) rating: u8,
    pub(crate) padding: u16,
}

#[derive(Clone, Copy, Debug, Pod, Zeroable)]
#[repr(C)]
pub(crate) struct TagRec {
    pub(crate) key: u32,
    pub(crate) name: StrId,
    pub(crate) sort: StrId,
    pub(crate) object: StrId,
    pub(crate) source: u16,
    pub(crate) flags: u16,
    /// A `TagKind`, as `tags.kind` stores it.
    pub(crate) kind: u8,
    pub(crate) padding: [u8; 3],
}

impl TagRec {
    pub(crate) fn kind(&self) -> TagKind {
        super::families::tag_kind(i64::from(self.kind))
    }
}

#[derive(Clone, Copy, Debug, Pod, Zeroable)]
#[repr(C)]
pub(crate) struct FolderRec {
    pub(crate) key: u32,
    pub(crate) name: StrId,
    pub(crate) sort: StrId,
    pub(crate) object: StrId,
    pub(crate) source: u16,
    pub(crate) flags: u16,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PlaylistRec {
    pub(crate) key: i64,
    pub(crate) name: StrId,
    pub(crate) object: StrId,
    /// Zero when the playlist is in no folder.
    pub(crate) folder: i64,
    pub(crate) created: u32,
    pub(crate) source: u16,
    pub(crate) flags: u16,
    /// Entries with their track slots, in rank order.
    pub(crate) entries: Vec<Entry<u32>>,
}

/// A playlist entry: its key, its rank in the playlist's order and its track.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Entry<T> {
    pub(crate) key: i64,
    pub(crate) rank: Box<[u8]>,
    pub(crate) track: T,
}

/// A Rufin or source playlist as the Store holds it, with its entries in rank order.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Playlist {
    pub(crate) key: i64,
    pub(crate) source: Option<SourceKey>,
    pub(crate) owned_by_source: bool,
    pub(crate) indexed: bool,
    pub(crate) object: String,
    pub(crate) name: String,
    pub(crate) folder: Option<i64>,
    pub(crate) created: i64,
    pub(crate) writable: bool,
    pub(crate) metadata_writable: bool,
    pub(crate) linked: bool,
    pub(crate) entries: Vec<Entry<TrackKey>>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SmartPlaylist {
    pub(crate) key: i64,
    pub(crate) object: String,
    pub(crate) name: String,
    pub(crate) folder: Option<i64>,
    pub(crate) position: i64,
    pub(crate) definition: SmartPlaylistDefinition,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PlaylistFolder {
    pub(crate) key: i64,
    pub(crate) object: String,
    pub(crate) name: String,
    pub(crate) smart: bool,
}

/// The fields every packed record shares.
pub(crate) trait Packed {
    fn key(&self) -> i64;
    fn source(&self) -> u16;
    fn flags(&self) -> u16;
    fn flags_mut(&mut self) -> &mut u16;

    fn deleted(&self) -> bool {
        self.flags() & DELETED != 0
    }

    fn indexed(&self) -> bool {
        self.flags() & INDEXED != 0
    }
}

macro_rules! packed {
    ($($record:ty),*) => {$(
        impl Packed for $record {
            fn key(&self) -> i64 {
                i64::from(self.key)
            }

            fn source(&self) -> u16 {
                self.source
            }

            fn flags(&self) -> u16 {
                self.flags
            }

            fn flags_mut(&mut self) -> &mut u16 {
                &mut self.flags
            }
        }
    )*};
}
packed!(
    TrackRec,
    AlbumRec,
    ArtistRec,
    TagRec,
    FolderRec,
    PlaylistRec
);

/// Records in slots, found by key. Keys are dense rowids, so the slots are indexed by key.
pub(crate) struct Family<R> {
    pub(crate) records: Vec<R>,
    /// Slots by key minus `base`; NONE where no live record has the key.
    pub(crate) keys: Vec<u32>,
    pub(crate) base: i64,
    pub(crate) live: usize,
}

impl<R> Default for Family<R> {
    fn default() -> Self {
        Self {
            records: Vec::new(),
            keys: Vec::new(),
            base: 0,
            live: 0,
        }
    }
}

impl<R: Packed> Family<R> {
    pub(crate) fn slot(&self, key: i64) -> Option<u32> {
        let index = usize::try_from(key.checked_sub(self.base)?).ok()?;
        self.keys.get(index).copied().filter(|slot| *slot != NONE)
    }

    pub(crate) fn get(&self, key: i64) -> Option<&R> {
        self.slot(key).map(|slot| &self.records[slot as usize])
    }

    /// The record in a slot, unless it was removed.
    pub(crate) fn live(&self, slot: u32) -> Option<&R> {
        self.records
            .get(slot as usize)
            .filter(|record| !record.deleted())
    }

    pub(crate) fn key_of(&self, slot: u32) -> Option<i64> {
        self.live(slot).map(Packed::key)
    }

    /// Writes a record into its slot, or a new one.
    fn place(&mut self, record: R) -> u32 {
        if let Some(slot) = self.slot(record.key()) {
            self.records[slot as usize] = record;
            return slot;
        }
        let key = record.key();
        if self.live == 0 {
            self.keys.clear();
            self.base = key;
        } else if key < self.base {
            let below = (self.base - key) as usize;
            self.keys.splice(0..0, std::iter::repeat_n(NONE, below));
            self.base = key;
        }
        let index = (key - self.base) as usize;
        if index >= self.keys.len() {
            self.keys.resize(index + 1, NONE);
        }
        let slot = self.records.len() as u32;
        self.keys[index] = slot;
        self.live += 1;
        self.records.push(record);
        slot
    }

    fn tombstone(&mut self, key: i64) -> Option<u32> {
        let slot = self.slot(key)?;
        self.keys[(key - self.base) as usize] = NONE;
        self.live -= 1;
        *self.records[slot as usize].flags_mut() |= DELETED;
        Some(slot)
    }

    /// Live records in slot order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (u32, &R)> {
        self.records
            .iter()
            .enumerate()
            .filter(|(_, record)| !record.deleted())
            .map(|(slot, record)| (slot as u32, record))
    }

    fn reserve(&mut self, records: usize) {
        self.records.reserve_exact(records);
        self.keys.reserve_exact(records);
    }

    fn shrink(&mut self) {
        self.records.shrink_to_fit();
        self.keys.shrink_to_fit();
    }

    fn bytes(&self) -> usize {
        self.records.capacity() * std::mem::size_of::<R>() + self.keys.capacity() * 4
    }
}

pub(crate) struct Snapshot {
    /// Counts committed Writes; the Change of a Write carries it.
    pub(crate) revision: u64,
    /// The revision that last changed catalog records. Pictures and album visibility read on
    /// a row depend only on them, not on favorites, listens or playlists.
    pub(crate) facts: u64,
    /// The revisions that last changed favorites and ratings, listens, downloads, and
    /// playlists with their entries and folders. Orders keep the stamps they depend on.
    pub(crate) states: u64,
    pub(crate) plays: u64,
    pub(crate) downloads: u64,
    pub(crate) playlist_stamp: u64,
    pub(crate) strings: Strings,
    pub(crate) edges: Vec<u32>,
    pub(crate) sources: BTreeMap<SourceKey, Source>,
    pub(crate) source_slots: Vec<SourceKey>,
    pub(crate) tracks: Family<TrackRec>,
    /// Keys of the downloaded tracks, so download counts read only them. A track removed while
    /// downloaded may stay here: its record's flag is what counts.
    pub(crate) download_keys: HashSet<u32>,
    pub(crate) albums: Family<AlbumRec>,
    pub(crate) artists: Family<ArtistRec>,
    pub(crate) tags: Family<TagRec>,
    pub(crate) folders: Family<FolderRec>,
    /// By track slot, only for tracks with plays or source counts.
    pub(crate) stats: HashMap<u32, Stats>,
    pub(crate) playlists: Family<PlaylistRec>,
    pub(crate) smart_playlists: BTreeMap<i64, SmartPlaylist>,
    pub(crate) playlist_folders: BTreeMap<i64, PlaylistFolder>,
    /// Artists and albums sort by their sort tags; otherwise by name past a leading article.
    pub(crate) sort_tags: bool,
    /// Tracks with their own picture show it, not their album's.
    pub(crate) distinct_track_covers: bool,
    /// Albums without a picture look their cover up before they borrow an artist's.
    pub(crate) cover_lookup: bool,
}

/// Leading articles a name sorts past without sort tags, Navidrome's default list.
const ARTICLES: [&str; 11] = [
    "the", "el", "la", "los", "las", "le", "les", "os", "as", "o", "a",
];

/// Whether a name is written with Latin letters. Without them, as in Japanese names, the sort
/// tag is the name's reading and is kept.
fn latin(name: &str) -> bool {
    name.chars().any(|c| {
        c.is_ascii_alphabetic()
            || matches!(c, '\u{c0}'..='\u{24f}' | '\u{1e00}'..='\u{1eff}') && c.is_alphabetic()
    })
}

/// Text lowercase with Latin letters on their base letter, as names and sort tags sort.
fn folded(text: &str) -> String {
    let mut lower = String::with_capacity(text.len());
    for character in text.chars() {
        crate::lyrics_search::fold(character, &mut lower);
    }
    lower
}

/// A name as it sorts without sort tags: folded, past a leading article.
fn plain_sort(name: &str) -> std::borrow::Cow<'_, str> {
    let lower = folded(name);
    match lower.split_once(' ') {
        Some((article, rest)) if ARTICLES.contains(&article) && !rest.trim().is_empty() => {
            rest.trim_start().to_owned().into()
        }
        _ => lower.into(),
    }
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            revision: 0,
            facts: 0,
            states: 0,
            plays: 0,
            downloads: 0,
            playlist_stamp: 0,
            strings: Strings::default(),
            edges: Vec::new(),
            sources: BTreeMap::new(),
            source_slots: Vec::new(),
            tracks: Family::default(),
            download_keys: HashSet::new(),
            albums: Family::default(),
            artists: Family::default(),
            tags: Family::default(),
            folders: Family::default(),
            stats: HashMap::new(),
            playlists: Family::default(),
            smart_playlists: BTreeMap::new(),
            playlist_folders: BTreeMap::new(),
            sort_tags: false,
            distinct_track_covers: false,
            cover_lookup: false,
        }
    }
}

impl Snapshot {
    /// The stamps of facts, states, plays, downloads and playlists.
    pub(crate) fn stamps(&self) -> [u64; 5] {
        [
            self.facts,
            self.states,
            self.plays,
            self.downloads,
            self.playlist_stamp,
        ]
    }

    /// Raises the revision and the stamps `changed` marks, in the order of `stamps()`.
    pub(crate) fn raise(&mut self, changed: [bool; 5]) -> u64 {
        self.revision += 1;
        let revision = self.revision;
        let stamps = [
            &mut self.facts,
            &mut self.states,
            &mut self.plays,
            &mut self.downloads,
            &mut self.playlist_stamp,
        ];
        for (stamp, changed) in stamps.into_iter().zip(changed) {
            if changed {
                *stamp = revision;
            }
        }
        revision
    }

    /// Puts a loaded Snapshot in place of this one, above its revision so nothing built from
    /// this one is reused, with this one's display flags. Returns the revision and this one.
    pub(crate) fn succeed(&mut self, mut loaded: Self) -> (u64, Self) {
        loaded.revision = self.revision;
        let revision = loaded.raise([true; 5]);
        loaded.sort_tags = self.sort_tags;
        loaded.distinct_track_covers = self.distinct_track_covers;
        loaded.cover_lookup = self.cover_lookup;
        (revision, std::mem::replace(self, loaded))
    }

    pub(crate) fn source(&self, key: SourceKey) -> Option<&Source> {
        self.sources.get(&key)
    }

    pub(crate) fn source_key(&self, slot: u16) -> Option<SourceKey> {
        self.source_slots.get(usize::from(slot)).copied()
    }

    pub(crate) fn source_slot(&self, key: SourceKey) -> Option<u16> {
        self.source_slots
            .iter()
            .position(|known| *known == key)
            .map(|slot| slot as u16)
    }

    fn slot_for_source(&mut self, key: Option<SourceKey>) -> u16 {
        let Some(key) = key else { return NO_SOURCE };
        match self.source_slot(key) {
            Some(slot) => slot,
            None => {
                self.source_slots.push(key);
                (self.source_slots.len() - 1) as u16
            }
        }
    }

    pub(crate) fn put_source(&mut self, source: Source) {
        self.slot_for_source(Some(source.key));
        self.sources.insert(source.key, source);
    }

    pub(crate) fn remove_source(&mut self, key: SourceKey) {
        self.sources.remove(&key);
    }

    pub(crate) fn text(&self, id: StrId) -> &str {
        self.strings.get(id)
    }

    fn run(&self, start: u32, len: usize) -> &[u32] {
        &self.edges[start as usize..start as usize + len]
    }

    /// Slots of these keys. A key the Catalog does not hold has no edge.
    fn slots<R: Packed>(family: &Family<R>, keys: impl IntoIterator<Item = i64>) -> Vec<u32> {
        keys.into_iter()
            .filter_map(|key| family.slot(key))
            .collect()
    }

    fn keys<R: Packed>(family: &Family<R>, slots: &[u32]) -> Vec<i64> {
        slots
            .iter()
            .filter_map(|slot| family.key_of(*slot))
            .collect()
    }

    /// The slots a track links: artists, album artists, tags and folders.
    pub(crate) fn track_runs(&self, record: &TrackRec) -> [&[u32]; 4] {
        let mut start = record.edges;
        record.lens.map(|len| {
            let run = self.run(start, usize::from(len));
            start += u32::from(len);
            run
        })
    }

    /// The slots an album links: artists and genres.
    pub(crate) fn album_runs(&self, record: &AlbumRec) -> [&[u32]; 2] {
        [
            self.run(record.edges, usize::from(record.lens[0])),
            self.run(
                record.edges + u32::from(record.lens[0]),
                usize::from(record.lens[1]),
            ),
        ]
    }

    /// A track's sort text.
    pub(crate) fn track_sort(&self, record: &TrackRec) -> std::borrow::Cow<'_, str> {
        if record.flags & SORT_TITLE != 0 {
            self.text(record.title).into()
        } else if record.flags & SORT_LOWER != 0 {
            self.text(record.title).to_lowercase().into()
        } else {
            self.text(record.sort).into()
        }
    }

    /// The sort text of a named row: its own, else its lowercase name.
    pub(crate) fn name_sort(&self, name: StrId, sort: StrId) -> std::borrow::Cow<'_, str> {
        if sort == StrId::NONE {
            self.text(name).to_lowercase().into()
        } else {
            self.text(sort).into()
        }
    }

    /// An artist's sort text: its sort tag as is when its name has no Latin letters to sort by,
    /// its folded sort tag when sort tags are on, else its name as it sorts.
    pub(crate) fn artist_sort(&self, artist: &ArtistRec) -> std::borrow::Cow<'_, str> {
        let name = self.text(artist.name);
        if !latin(name) {
            self.name_sort(artist.name, artist.sort)
        } else if self.sort_tags {
            folded(&self.name_sort(artist.name, artist.sort)).into()
        } else {
            plain_sort(name)
        }
    }

    /// An album's sort text, by the same rule as an artist's.
    pub(crate) fn album_sort(&self, album: &AlbumRec) -> std::borrow::Cow<'_, str> {
        let title = self.text(album.title);
        if !latin(title) {
            self.text(album.sort).into()
        } else if self.sort_tags {
            folded(self.text(album.sort)).into()
        } else {
            plain_sort(title)
        }
    }

    /// The artists, album artists, tags and folders a resident track links, by key.
    pub(crate) fn track_links(&self, record: &TrackRec) -> TrackLinks {
        let mut start = record.edges;
        let mut take = |index: usize| {
            let len = usize::from(record.lens[index]);
            let run = self.run(start, len);
            start += len as u32;
            run
        };
        let artists = take(0);
        let album_artists = take(1);
        let tags = take(2);
        let folders = take(3);
        TrackLinks {
            artists: Self::keys(&self.artists, artists)
                .into_iter()
                .map(ArtistKey::from_raw)
                .collect(),
            album_artists: Self::keys(&self.artists, album_artists)
                .into_iter()
                .map(ArtistKey::from_raw)
                .collect(),
            tags: tags
                .iter()
                .filter_map(|slot| self.tags.live(*slot))
                .map(|tag| (tag.kind(), i64::from(tag.key)))
                .collect(),
            folders: Self::keys(&self.folders, folders)
                .into_iter()
                .map(FolderKey::from_raw)
                .collect(),
        }
    }

    /// The artists and genres a resident album links, by key.
    pub(crate) fn album_links(&self, record: &AlbumRec) -> (Vec<ArtistKey>, Vec<i64>) {
        let artists = self.run(record.edges, usize::from(record.lens[0]));
        let genres = self.run(
            record.edges + u32::from(record.lens[0]),
            usize::from(record.lens[1]),
        );
        (
            Self::keys(&self.artists, artists)
                .into_iter()
                .map(ArtistKey::from_raw)
                .collect(),
            Self::keys(&self.tags, genres),
        )
    }

    pub(crate) fn put_track(&mut self, track: &Track) {
        let artists = track
            .artists
            .iter()
            .map(|key| key.raw())
            .collect::<Vec<_>>();
        let album_artists = track
            .album_artists
            .iter()
            .map(|key| key.raw())
            .collect::<Vec<_>>();
        let tags = track.tags.iter().map(|(_, key)| *key).collect::<Vec<_>>();
        let folders = track
            .folders
            .iter()
            .map(|key| key.raw())
            .collect::<Vec<_>>();
        let facts = &track.facts;
        self.pack_track(TrackInput {
            key: track.key.raw(),
            source: track.source,
            album: track.album.map(AlbumKey::raw),
            indexed: track.indexed,
            native: track.native,
            picture: pictured(track.binding.as_deref()),
            cue: track.cue_path.is_some(),
            title: &facts.title,
            artist: &facts.artist,
            album_title: &facts.album,
            sort: &facts.sort,
            comment: facts.comment.as_deref(),
            format: facts.format.as_deref(),
            duration_ms: facts.duration_ms,
            added: added(facts.date_added.as_deref(), track.first_seen),
            release: release(facts.release_date.as_deref()),
            year: facts.year,
            disc: facts.disc,
            number: facts.number,
            bpm: facts.bpm,
            state: &track.state,
            links: [&artists, &album_artists, &tags, &folders],
        });
    }

    /// Packs a track row. The load and every Write pack tracks through here.
    pub(crate) fn pack_track(&mut self, input: TrackInput<'_>) {
        self.slot_for_source(input.source);
        let (names, mut packing) = self.split();
        packing.pack(&names, input);
    }

    /// The families tracks name, beside the parts of the Snapshot packed tracks go into.
    pub(crate) fn split(&mut self) -> (Names<'_>, Packing<'_>) {
        (
            Names {
                albums: &self.albums,
                artists: &self.artists,
                tags: &self.tags,
                folders: &self.folders,
                sources: &self.source_slots,
            },
            Packing {
                strings: &mut self.strings,
                edges: &mut self.edges,
                tracks: &mut self.tracks,
                stats: &mut self.stats,
            },
        )
    }

    /// Plays and skips from listens and carried-over counts.
    pub(crate) fn set_listened(&mut self, key: TrackKey, listened: Stats) {
        let Some(slot) = self.tracks.slot(key.raw()) else {
            return;
        };
        let stats = self.stats.entry(slot).or_default();
        stats.plays = listened.plays;
        stats.skips = listened.skips;
        stats.last_played = listened.last_played;
        if *stats == Stats::default() {
            self.stats.remove(&slot);
        }
    }

    pub(crate) fn listened(&self, key: TrackKey) -> Stats {
        let stats = self
            .tracks
            .slot(key.raw())
            .and_then(|slot| self.stats.get(&slot))
            .copied()
            .unwrap_or_default();
        Stats::listened(
            i64::from(stats.plays),
            i64::from(stats.skips),
            (stats.last_played != NONE).then_some(i64::from(stats.last_played)),
        )
    }

    pub(crate) fn remove_track(&mut self, key: TrackKey) {
        if let Some(slot) = self.tracks.tombstone(key.raw()) {
            self.stats.remove(&slot);
        }
    }

    pub(crate) fn put_album(&mut self, album: &Album) {
        let facts = &album.facts;
        self.pack_album(
            album,
            pictured(album.binding.as_deref()),
            release(facts.release_date.as_deref()),
            added(facts.date_added.as_deref(), album.first_seen),
        );
    }

    /// Packs a row whose picture and dates the caller decided; the load reads no binding and
    /// parses no date.
    pub(crate) fn pack_album(
        &mut self,
        album: &Album,
        picture: bool,
        release: u32,
        added: Option<i64>,
    ) {
        let previous = self.albums.get(album.key.raw()).copied();
        let mut items = Self::slots(&self.artists, album.artists.iter().map(|key| key.raw()));
        let artists = items.len().min(usize::from(u16::MAX));
        items.truncate(artists);
        let genres = Self::slots(&self.tags, album.genres.iter().copied());
        let genres = &genres[..genres.len().min(usize::from(u16::MAX))];
        items.extend_from_slice(genres);
        let (old_start, old_len) = previous.map_or((0, 0), |record| {
            (
                record.edges,
                usize::from(record.lens[0]) + usize::from(record.lens[1]),
            )
        });
        let edges = put_run(&mut self.edges, old_start, old_len, &items);
        let source = self.slot_for_source(Some(album.source));
        let facts = &album.facts;
        let mut flags = native_flags(album.indexed, album.native, picture, &album.state)
            | u16::from(album.cached_local) * CACHED_LOCAL
            | u16::from(album.input) * INPUT
            | previous.map_or(0, |record| record.flags & (FAVORITE_KNOWN | FAVORITE));
        if let Some(compilation) = facts.compilation {
            flags |= COMPILATION_KNOWN | u16::from(compilation) * COMPILATION;
        }
        flags |= album.class.code() << RELEASE_SHIFT;
        let old = |field: fn(&AlbumRec) -> StrId| previous.as_ref().map_or(StrId::NONE, field);
        let strings = &mut self.strings;
        let record = AlbumRec {
            key: album.key.raw() as u32,
            title: strings.text(old(|record| record.title), &facts.title, true),
            artist: strings.text(old(|record| record.artist), &facts.artist, true),
            sort: strings.text(old(|record| record.sort), &facts.sort, true),
            object: strings.text(old(|record| record.object), &album.object, false),
            added: optional_u32(added),
            release,
            edges,
            native: native_plays(&album.state),
            lens: [artists as u16, genres.len() as u16],
            year: year(facts.year),
            source,
            flags,
            native_rating: rating(album.state.rating),
            rating: previous.map_or(UNRATED, |record| record.rating),
        };
        self.albums.place(record);
    }

    pub(crate) fn remove_album(&mut self, key: AlbumKey) {
        self.albums.tombstone(key.raw());
    }

    pub(crate) fn put_artist(&mut self, artist: &Artist) {
        self.pack_artist(artist, pictured(artist.binding.as_deref()));
    }

    /// Packs a row whose picture the caller decided; the load reads no binding.
    pub(crate) fn pack_artist(&mut self, artist: &Artist, picture: bool) {
        let previous = self.artists.get(artist.key.raw()).copied();
        let source = self.slot_for_source(Some(artist.source));
        let old = |field: fn(&ArtistRec) -> StrId| previous.as_ref().map_or(StrId::NONE, field);
        let strings = &mut self.strings;
        let record = ArtistRec {
            key: artist.key.raw() as u32,
            name: strings.text(old(|record| record.name), &artist.metadata.name, true),
            sort: strings.optional(
                old(|record| record.sort),
                artist.metadata.sort_text.as_deref(),
                true,
            ),
            object: strings.text(old(|record| record.object), &artist.object, false),
            native: native_plays(&artist.state),
            source,
            flags: native_flags(artist.indexed, artist.native, picture, &artist.state)
                | previous.map_or(0, |record| record.flags & (FAVORITE_KNOWN | FAVORITE)),
            native_rating: rating(artist.state.rating),
            rating: previous.map_or(UNRATED, |record| record.rating),
            padding: 0,
        };
        self.artists.place(record);
    }

    pub(crate) fn remove_artist(&mut self, key: ArtistKey) {
        self.artists.tombstone(key.raw());
    }

    pub(crate) fn put_tag(&mut self, tag: &Tag) {
        self.pack_tag(tag, pictured(tag.binding.as_deref()));
    }

    /// Packs a row whose picture the caller decided; the load reads no binding.
    pub(crate) fn pack_tag(&mut self, tag: &Tag, picture: bool) {
        let previous = self.tags.get(tag.key).copied();
        let source = self.slot_for_source(Some(tag.source));
        let old = |field: fn(&TagRec) -> StrId| previous.as_ref().map_or(StrId::NONE, field);
        let strings = &mut self.strings;
        let record = TagRec {
            key: tag.key as u32,
            name: strings.text(old(|record| record.name), &tag.metadata.name, true),
            sort: strings.optional(
                old(|record| record.sort),
                tag.metadata.sort_text.as_deref(),
                true,
            ),
            object: strings.text(old(|record| record.object), &tag.object, false),
            source,
            flags: native_flags(tag.indexed, tag.native, picture, &NativeState::default()),
            kind: tag.kind as u8,
            padding: [0; 3],
        };
        self.tags.place(record);
    }

    pub(crate) fn remove_tag(&mut self, key: i64) {
        self.tags.tombstone(key);
    }

    pub(crate) fn put_folder(&mut self, folder: &Folder) {
        self.pack_folder(folder, pictured(folder.binding.as_deref()));
    }

    /// Packs a row whose picture the caller decided; the load reads no binding.
    pub(crate) fn pack_folder(&mut self, folder: &Folder, picture: bool) {
        let previous = self.folders.get(folder.key.raw()).copied();
        let source = self.slot_for_source(Some(folder.source));
        let old = |field: fn(&FolderRec) -> StrId| previous.as_ref().map_or(StrId::NONE, field);
        let strings = &mut self.strings;
        let record = FolderRec {
            key: folder.key.raw() as u32,
            name: strings.optional(
                old(|record| record.name),
                folder.metadata.name.as_deref(),
                true,
            ),
            sort: strings.optional(
                old(|record| record.sort),
                folder.metadata.sort_text.as_deref(),
                true,
            ),
            object: strings.text(old(|record| record.object), &folder.object, false),
            source,
            flags: native_flags(
                folder.indexed,
                folder.native,
                picture,
                &NativeState::default(),
            ),
        };
        self.folders.place(record);
    }

    pub(crate) fn remove_folder(&mut self, key: FolderKey) {
        self.folders.tombstone(key.raw());
    }

    /// The user's favorite and rating of a track, album or artist.
    pub(crate) fn user_state(&self, item: UserItem) -> Option<UserState> {
        let (flags, rating) = match item {
            UserItem::Track(key) => self
                .tracks
                .get(key)
                .map(|record| (record.flags, record.rating))?,
            UserItem::Album(key) => self
                .albums
                .get(key)
                .map(|record| (record.flags, record.rating))?,
            UserItem::Artist(key) => self
                .artists
                .get(key)
                .map(|record| (record.flags, record.rating))?,
        };
        Some(UserState::of(flags, rating))
    }

    /// The favorite and rating a track, album or artist shows: the user's own, else its
    /// source's.
    pub(crate) fn shown_state(&self, item: UserItem) -> Option<(bool, Option<u8>)> {
        let (flags, rating, native) = match item {
            UserItem::Track(key) => self
                .tracks
                .get(key)
                .map(|record| (record.flags, record.rating, record.native_rating))?,
            UserItem::Album(key) => self
                .albums
                .get(key)
                .map(|record| (record.flags, record.rating, record.native_rating))?,
            UserItem::Artist(key) => self
                .artists
                .get(key)
                .map(|record| (record.flags, record.rating, record.native_rating))?,
        };
        Some((
            super::views::favorite(flags),
            super::views::rating(rating, native),
        ))
    }

    pub(crate) fn set_user_state(&mut self, item: UserItem, state: UserState) {
        let rating = state.rating.unwrap_or(UNRATED);
        let flags = state.flags();
        let record = match item {
            UserItem::Track(key) => self.tracks.slot(key).map(|slot| {
                let record = &mut self.tracks.records[slot as usize];
                (&mut record.flags, &mut record.rating)
            }),
            UserItem::Album(key) => self.albums.slot(key).map(|slot| {
                let record = &mut self.albums.records[slot as usize];
                (&mut record.flags, &mut record.rating)
            }),
            UserItem::Artist(key) => self.artists.slot(key).map(|slot| {
                let record = &mut self.artists.records[slot as usize];
                (&mut record.flags, &mut record.rating)
            }),
        };
        if let Some((record_flags, record_rating)) = record {
            *record_flags = *record_flags & !(FAVORITE_KNOWN | FAVORITE) | flags;
            *record_rating = rating;
        }
    }

    pub(crate) fn downloaded(&self, key: TrackKey) -> Option<bool> {
        self.tracks
            .get(key.raw())
            .map(|record| record.flags & DOWNLOADED != 0)
    }

    pub(crate) fn set_downloaded(&mut self, key: TrackKey, downloaded: bool) {
        if let Some(slot) = self.tracks.slot(key.raw()) {
            let record = &mut self.tracks.records[slot as usize];
            record.flags = record.flags & !DOWNLOADED | u16::from(downloaded) * DOWNLOADED;
            if downloaded {
                self.download_keys.insert(record.key);
            } else {
                self.download_keys.remove(&record.key);
            }
        }
    }

    pub(crate) fn playlist(&self, key: i64) -> Option<Playlist> {
        let record = self.playlists.get(key)?;
        Some(Playlist {
            key,
            source: self.source_key(record.source),
            owned_by_source: record.flags & SOURCE_OWNED != 0,
            indexed: record.flags & INDEXED != 0,
            object: self.text(record.object).to_owned(),
            name: self.text(record.name).to_owned(),
            folder: (record.folder != 0).then_some(record.folder),
            created: i64::from(record.created),
            writable: record.flags & WRITABLE != 0,
            metadata_writable: record.flags & METADATA_WRITABLE != 0,
            linked: record.flags & LINKED != 0,
            entries: record
                .entries
                .iter()
                .filter_map(|entry| {
                    Some(Entry {
                        key: entry.key,
                        rank: entry.rank.clone(),
                        track: TrackKey::from_raw(self.tracks.key_of(entry.track)?),
                    })
                })
                .collect(),
        })
    }

    pub(crate) fn put_playlist(&mut self, playlist: &Playlist) {
        let previous = self
            .playlists
            .get(playlist.key)
            .map(|record| (record.name, record.object));
        let source = self.slot_for_source(playlist.source);
        let name = self.strings.text(
            previous.map_or(StrId::NONE, |(name, _)| name),
            &playlist.name,
            false,
        );
        let object = self.strings.text(
            previous.map_or(StrId::NONE, |(_, object)| object),
            &playlist.object,
            false,
        );
        let entries = playlist
            .entries
            .iter()
            .map(|entry| Entry {
                key: entry.key,
                rank: entry.rank.clone(),
                track: self.tracks.slot(entry.track.raw()).unwrap_or(NONE),
            })
            .collect();
        self.playlists.place(PlaylistRec {
            key: playlist.key,
            name,
            object,
            folder: playlist.folder.unwrap_or(0),
            created: clamp_u32(playlist.created),
            source,
            flags: u16::from(playlist.indexed) * INDEXED
                | u16::from(playlist.owned_by_source) * SOURCE_OWNED
                | u16::from(playlist.writable) * WRITABLE
                | u16::from(playlist.metadata_writable) * METADATA_WRITABLE
                | u16::from(playlist.linked) * LINKED,
            entries,
        });
    }

    pub(crate) fn remove_playlist(&mut self, key: i64) {
        if let Some(slot) = self.playlists.tombstone(key) {
            self.playlists.records[slot as usize].entries = Vec::new();
        }
    }

    /// Keys of a source's listed records that pass a filter.
    pub(crate) fn indexed_keys<R: Packed>(
        &self,
        family: &Family<R>,
        source: SourceKey,
        keep: impl Fn(&R) -> bool,
    ) -> Vec<i64> {
        let Some(slot) = self.source_slot(source) else {
            return Vec::new();
        };
        family
            .iter()
            .filter(|(_, record)| record.source() == slot && record.indexed() && keep(record))
            .map(|(_, record)| record.key())
            .collect()
    }

    /// Artists of the source an indexed track credits, or an album that is indexed or has
    /// indexed members. A source's records credit only its own artists.
    pub(crate) fn credited_artists(&self, source: SourceKey) -> std::collections::HashSet<i64> {
        let mut artists = std::collections::HashSet::new();
        let mut albums = std::collections::HashSet::new();
        let Some(slot) = self.source_slot(source) else {
            return artists;
        };
        for (_, track) in self.tracks.iter() {
            if !track.indexed() || track.source != slot {
                continue;
            }
            let links = self.track_links(track);
            artists.extend(
                links
                    .artists
                    .iter()
                    .chain(&links.album_artists)
                    .map(|key| key.raw()),
            );
            if self.albums.live(track.album).is_some() {
                albums.insert(track.album);
            }
        }
        for (index, album) in self.albums.iter() {
            if album.source == slot && (album.indexed() || albums.contains(&index)) {
                artists.extend(self.album_links(album).0.iter().map(|key| key.raw()));
            }
        }
        artists
    }

    /// Keys of every record a source owns.
    pub(crate) fn source_keys<R: Packed>(&self, family: &Family<R>, source: SourceKey) -> Vec<i64> {
        let Some(slot) = self.source_slot(source) else {
            return Vec::new();
        };
        family
            .iter()
            .filter(|(_, record)| record.source() == slot)
            .map(|(_, record)| record.key())
            .collect()
    }

    /// Drops every record a source owns, except the tracks it kept as hidden rows without a
    /// source. References to dropped records read as none, as the Store's foreign keys and
    /// link removal leave them.
    pub(crate) fn forget_source(&mut self, source: SourceKey, kept: &[TrackKey]) {
        let kept = kept
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        for key in self.source_keys(&self.tracks, source) {
            let key = TrackKey::from_raw(key);
            match self.tracks.slot(key.raw()) {
                Some(slot) if kept.contains(&key) => {
                    let record = &mut self.tracks.records[slot as usize];
                    record.source = NO_SOURCE;
                    record.album = NONE;
                    record.flags &= !INDEXED;
                }
                _ => self.remove_track(key),
            }
        }
        for key in self.source_keys(&self.albums, source) {
            self.albums.tombstone(key);
        }
        for key in self.source_keys(&self.artists, source) {
            self.artists.tombstone(key);
        }
        for key in self.source_keys(&self.tags, source) {
            self.tags.tombstone(key);
        }
        for key in self.source_keys(&self.folders, source) {
            self.folders.tombstone(key);
        }
        for key in self.source_keys(&self.playlists, source) {
            self.remove_playlist(key);
        }
        self.sources.remove(&source);
    }

    /// Room for the rows a load is about to store, so it grows no family by reallocation.
    pub(crate) fn reserve(&mut self, rows: [usize; 5]) {
        let [tracks, albums, artists, tags, folders] = rows;
        self.tracks.reserve(tracks);
        self.albums.reserve(albums);
        self.artists.reserve(artists);
        self.tags.reserve(tags);
        self.folders.reserve(folders);
    }

    pub(crate) fn reserve_edges(&mut self, edges: usize) {
        self.edges.reserve_exact(edges);
    }

    /// Gives back what the load reserved and did not use.
    pub(crate) fn shrink(&mut self) {
        self.strings.shrink();
        self.edges.shrink_to_fit();
        self.tracks.shrink();
        self.albums.shrink();
        self.artists.shrink();
        self.tags.shrink();
        self.folders.shrink();
        self.stats.shrink_to_fit();
        self.playlists.shrink();
    }

    /// Bytes held by records, arenas and maps.
    pub(crate) fn bytes(&self) -> usize {
        self.strings.bytes()
            + self.edges.capacity() * 4
            + self.tracks.bytes()
            + self.albums.bytes()
            + self.artists.bytes()
            + self.tags.bytes()
            + self.folders.bytes()
            + map_bytes(&self.stats)
            + self.playlists.bytes()
            + self
                .playlists
                .records
                .iter()
                .map(|playlist| {
                    playlist.entries.capacity() * 32
                        + playlist
                            .entries
                            .iter()
                            .map(|entry| entry.rank.len())
                            .sum::<usize>()
                })
                .sum::<usize>()
    }

    pub(crate) fn track_count(&self) -> usize {
        self.tracks.live
    }

    /// Slots of catalog records, removed ones included. Removed slots and the texts and edges
    /// they held stay until the next load.
    pub(crate) fn record_slots(&self) -> usize {
        self.tracks.records.len()
            + self.albums.records.len()
            + self.artists.records.len()
            + self.tags.records.len()
            + self.folders.records.len()
    }

    pub(crate) fn removed_slots(&self) -> usize {
        self.record_slots()
            - self.tracks.live
            - self.albums.live
            - self.artists.live
            - self.tags.live
            - self.folders.live
    }

    /// Every live record by key, with references resolved to keys and texts, for comparing two
    /// Snapshots.
    #[cfg(test)]
    pub(crate) fn export(&self) -> Export {
        fn sorted<T: Ord>(mut items: Vec<T>) -> Vec<T> {
            items.sort();
            items
        }
        let source = |slot: u16| self.source_key(slot).map(SourceKey::raw);
        let text = |id: StrId| (id != StrId::NONE).then(|| self.strings.get(id).to_owned());
        Export {
            sources: self.sources.values().map(|source| format!("{source:?}")).collect(),
            tracks: sorted(
                self.tracks
                    .iter()
                    .map(|(slot, record)| {
                        let links = self.track_links(record);
                        format!(
                            "{} {:?} album={:?} {:?} {:?} {:?} sort={:?} {:?} {:?} {} {} {} {} {} {} {} {} {} {} {:?} {:?}",
                            record.key,
                            source(record.source),
                            self.albums.key_of(record.album),
                            text(record.title),
                            text(record.artist),
                            text(record.album_title),
                            text(record.sort),
                            text(record.comment),
                            text(record.format),
                            record.duration_ms,
                            record.added,
                            record.release,
                            record.year,
                            record.disc,
                            record.number,
                            record.bpm,
                            record.flags,
                            record.native_rating,
                            record.rating,
                            links,
                            self.stats.get(&slot),
                        )
                    })
                    .collect(),
            ),
            albums: sorted(
                self.albums
                    .iter()
                    .map(|(_, record)| {
                        format!(
                            "{} {:?} {:?} {:?} {:?} {} {} {} {} {} {} {:?}",
                            record.key,
                            source(record.source),
                            text(record.title),
                            text(record.artist),
                            text(record.sort),
                            record.added,
                            record.release,
                            record.year,
                            record.flags,
                            record.native_rating,
                            record.rating,
                            self.album_links(record),
                        )
                    })
                    .collect(),
            ),
            artists: sorted(
                self.artists
                    .iter()
                    .map(|(_, record)| {
                        format!(
                            "{} {:?} {:?} {:?} {} {} {}",
                            record.key,
                            source(record.source),
                            text(record.name),
                            text(record.sort),
                            record.flags,
                            record.native_rating,
                            record.rating,
                        )
                    })
                    .collect(),
            ),
            tags: sorted(
                self.tags
                    .iter()
                    .map(|(_, record)| {
                        format!(
                            "{} {:?} {:?} {:?} {:?} {}",
                            record.key,
                            record.kind(),
                            source(record.source),
                            text(record.name),
                            text(record.sort),
                            record.flags,
                        )
                    })
                    .collect(),
            ),
            folders: sorted(
                self.folders
                    .iter()
                    .map(|(_, record)| {
                        format!(
                            "{} {:?} {:?} {:?} {}",
                            record.key,
                            source(record.source),
                            text(record.name),
                            text(record.sort),
                            record.flags,
                        )
                    })
                    .collect(),
            ),
            playlists: sorted(
                self.playlists
                    .iter()
                    .filter_map(|(_, record)| self.playlist(record.key))
                    .map(|playlist| format!("{playlist:?}"))
                    .collect(),
            ),
            smart_playlists: self
                .smart_playlists
                .values()
                .map(|smart| format!("{smart:?}"))
                .collect(),
            playlist_folders: self
                .playlist_folders
                .values()
                .map(|folder| format!("{folder:?}"))
                .collect(),
        }
    }
}

/// What a hash map allocates: hashbrown keeps one control byte per bucket and buckets at 8/7 of
/// its capacity.
pub(crate) fn map_bytes<K, V>(map: &HashMap<K, V>) -> usize {
    map.capacity() * 8 / 7 * (std::mem::size_of::<(K, V)>() + 1)
}

/// Writes a run into the edge arena, in place when it fits.
fn put_run(edges: &mut Vec<u32>, start: u32, len: usize, items: &[u32]) -> u32 {
    if items.len() <= len {
        let start = start as usize;
        edges[start..start + items.len()].copy_from_slice(items);
        return start as u32;
    }
    let start = edges.len() as u32;
    edges.extend_from_slice(items);
    start
}

/// The families a track names. They do not change while the load packs tracks, so its ranges
/// share them.
pub(crate) struct Names<'a> {
    albums: &'a Family<AlbumRec>,
    artists: &'a Family<ArtistRec>,
    tags: &'a Family<TagRec>,
    folders: &'a Family<FolderRec>,
    sources: &'a [SourceKey],
}

/// Where packed tracks go: the Snapshot's own arenas, or a load range's.
pub(crate) struct Packing<'a> {
    strings: &'a mut Strings,
    edges: &'a mut Vec<u32>,
    tracks: &'a mut Family<TrackRec>,
    stats: &'a mut HashMap<u32, Stats>,
}

/// Tracks one load range packed, before they join the Snapshot.
#[derive(Default)]
pub(crate) struct TrackPart {
    strings: Strings,
    edges: Vec<u32>,
    tracks: Family<TrackRec>,
    stats: HashMap<u32, Stats>,
}

impl TrackPart {
    pub(crate) fn packing(&mut self) -> Packing<'_> {
        Packing {
            strings: &mut self.strings,
            edges: &mut self.edges,
            tracks: &mut self.tracks,
            stats: &mut self.stats,
        }
    }
}

impl Packing<'_> {
    pub(crate) fn pack(&mut self, names: &Names<'_>, input: TrackInput<'_>) {
        let previous = self.tracks.get(input.key).copied();
        let groups = [
            Snapshot::slots(names.artists, input.links[0].iter().copied()),
            Snapshot::slots(names.artists, input.links[1].iter().copied()),
            Snapshot::slots(names.tags, input.links[2].iter().copied()),
            Snapshot::slots(names.folders, input.links[3].iter().copied()),
        ];
        let lens = groups
            .each_ref()
            .map(|group| group.len().min(usize::from(u16::MAX)) as u16);
        let items = groups
            .iter()
            .zip(lens)
            .flat_map(|(group, len)| &group[..usize::from(len)])
            .copied()
            .collect::<Vec<_>>();
        let (old_start, old_len) = previous.map_or((0, 0), |record| {
            (
                record.edges,
                record.lens.iter().map(|len| usize::from(*len)).sum(),
            )
        });
        let edges = put_run(self.edges, old_start, old_len, &items);
        let source = input.source.map_or(NO_SOURCE, |key| {
            names
                .sources
                .iter()
                .position(|known| *known == key)
                .map_or(NO_SOURCE, |slot| slot as u16)
        });
        let album = input
            .album
            .and_then(|key| names.albums.slot(key))
            .unwrap_or(NONE);
        let sort = if input.sort == input.title {
            SORT_TITLE
        } else if input.sort == input.title.to_lowercase() {
            SORT_LOWER
        } else {
            0
        };
        let old = |field: fn(&TrackRec) -> StrId| previous.as_ref().map_or(StrId::NONE, field);
        let kept = previous.map_or(0, |record| {
            record.flags & (FAVORITE_KNOWN | FAVORITE | DOWNLOADED)
        });
        let strings = &mut *self.strings;
        let record = TrackRec {
            key: input.key as u32,
            title: strings.text(old(|record| record.title), input.title, false),
            artist: strings.text(old(|record| record.artist), input.artist, true),
            album_title: strings.text(old(|record| record.album_title), input.album_title, true),
            sort: if sort != 0 {
                StrId::NONE
            } else {
                strings.text(old(|record| record.sort), input.sort, false)
            },
            comment: strings.optional(old(|record| record.comment), input.comment, true),
            format: strings.optional(old(|record| record.format), input.format, true),
            album,
            duration_ms: clamp_u32(input.duration_ms),
            added: optional_u32(input.added),
            release: input.release,
            edges,
            lens,
            year: year(input.year),
            disc: clamp_u16(input.disc),
            number: clamp_u16(input.number),
            bpm: optional_u16(input.bpm),
            source,
            flags: native_flags(input.indexed, input.native, input.picture, input.state)
                | u16::from(input.cue) * CUE
                | sort
                | kept,
            native_rating: rating(input.state.rating),
            rating: previous.map_or(UNRATED, |record| record.rating),
            padding: 0,
        };
        let slot = self.tracks.place(record);
        let stats = self.stats.entry(slot).or_default();
        // A count of zero reads as no count: Jellyfin and Emby report zero for every track.
        stats.native_plays = optional_u32(
            input
                .state
                .play_count
                .filter(|plays| *plays != 0 || input.state.last_played.is_some()),
        );
        stats.native_last_played = optional_u32(input.state.last_played);
        if *stats == Stats::default() {
            self.stats.remove(&slot);
        }
    }

    /// Adds the tracks a load range packed. Their keys follow every key packed here before.
    pub(crate) fn append(&mut self, part: TrackPart) {
        let ids = self.strings.append(&part.strings);
        let id = |id: StrId| {
            if id == StrId::NONE {
                id
            } else {
                ids[id.raw() as usize]
            }
        };
        let edges = self.edges.len() as u32;
        self.edges.extend_from_slice(&part.edges);
        let base = self.tracks.records.len() as u32;
        for mut record in part.tracks.records {
            record.title = id(record.title);
            record.artist = id(record.artist);
            record.album_title = id(record.album_title);
            record.sort = id(record.sort);
            record.comment = id(record.comment);
            record.format = id(record.format);
            record.edges += edges;
            self.tracks.place(record);
        }
        self.stats.extend(
            part.stats
                .into_iter()
                .map(|(slot, stats)| (base + slot, stats)),
        );
    }
}

/// The resident columns of a track row and its links by key.
pub(crate) struct TrackInput<'a> {
    pub(crate) key: i64,
    pub(crate) source: Option<SourceKey>,
    pub(crate) album: Option<i64>,
    pub(crate) indexed: bool,
    pub(crate) native: bool,
    pub(crate) picture: bool,
    pub(crate) cue: bool,
    pub(crate) title: &'a str,
    pub(crate) artist: &'a str,
    pub(crate) album_title: &'a str,
    pub(crate) sort: &'a str,
    pub(crate) comment: Option<&'a str>,
    pub(crate) format: Option<&'a str>,
    pub(crate) duration_ms: i64,
    pub(crate) added: Option<i64>,
    pub(crate) release: u32,
    pub(crate) year: Option<i64>,
    pub(crate) disc: i64,
    pub(crate) number: i64,
    pub(crate) bpm: Option<i64>,
    pub(crate) state: &'a NativeState,
    /// Artists, album artists, tags and folders.
    pub(crate) links: [&'a [i64]; 4],
}

/// A track's links by key, as a resident record holds them.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct TrackLinks {
    pub(crate) artists: Vec<ArtistKey>,
    pub(crate) album_artists: Vec<ArtistKey>,
    pub(crate) tags: Vec<(TagKind, i64)>,
    pub(crate) folders: Vec<FolderKey>,
}

/// The rows the user can favorite and rate.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum UserItem {
    Track(i64),
    Album(i64),
    Artist(i64),
}

impl UserItem {
    pub(crate) fn key(self) -> i64 {
        match self {
            Self::Track(key) | Self::Album(key) | Self::Artist(key) => key,
        }
    }

    /// The favorite target of this item at `uri`.
    pub(crate) fn target(self, uri: String) -> crate::FavoriteTarget {
        match self {
            Self::Track(_) => crate::FavoriteTarget::Track(uri),
            Self::Album(_) => crate::FavoriteTarget::Album(uri),
            Self::Artist(_) => crate::FavoriteTarget::Artist(uri),
        }
    }
}

#[cfg(test)]
#[derive(Debug, PartialEq)]
pub(crate) struct Export {
    sources: Vec<String>,
    tracks: Vec<String>,
    albums: Vec<String>,
    artists: Vec<String>,
    tags: Vec<String>,
    folders: Vec<String>,
    playlists: Vec<String>,
    smart_playlists: Vec<String>,
    playlist_folders: Vec<String>,
}
