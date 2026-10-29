//! Reads served from the resident Catalog: orders, filters, letter sections, collection totals
//! and the values computed on read. Opening a page orders slots in memory; only the rows a
//! client shows are read from the Store, by key.

use std::any::Any;
use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, PoisonError};

use sqlx::SqliteConnection;

use super::snapshot::*;
use super::strings::StrId;
use crate::tags::TagKind;
use crate::{
    AlbumReleaseClass, Catalog, CatalogSort, Collection, LibraryResult, ListKind, ListRead,
    MemberRead, PlaylistScope, SmartPlaylistDefinition, SourceKey,
};

/// Bytes of cached orders, read by the order cache's eviction.
#[cfg(target_os = "android")]
const ORDER_BUDGET: usize = 8 << 20;
#[cfg(not(target_os = "android"))]
const ORDER_BUDGET: usize = 32 << 20;

/// Kinds whose play counts come from Rufin's listens; servers report their own.
const LISTENED_KINDS: [&str; 5] = ["local", "files", "webdav", "smb", "documents"];

/// What a reader needs besides the Snapshot: which sources count Rufin's listens and the
/// distinct-covers setting.
pub(crate) struct Context<'a> {
    pub(crate) snapshot: &'a Snapshot,
    listened: Vec<bool>,
    pub(crate) distinct: bool,
}

/// A track's displayed plays: the server's counts on server sources, Rufin's listens elsewhere.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Played {
    pub(crate) plays: u32,
    pub(crate) skips: u32,
    pub(crate) last: Option<u32>,
}

fn known(value: u32) -> Option<u32> {
    (value != NONE).then_some(value)
}

/// The favorite a row shows: the user's own, else the one its source reports.
pub(crate) fn favorite(flags: u16) -> Option<bool> {
    if flags & FAVORITE_KNOWN != 0 {
        Some(flags & FAVORITE != 0)
    } else if flags & NATIVE_FAVORITE_KNOWN != 0 {
        Some(flags & NATIVE_FAVORITE != 0)
    } else {
        None
    }
}

/// The rating a row shows, 0..100: the user's own, else the one its source reports.
pub(crate) fn rating(user: u8, native: u8) -> Option<u8> {
    [user, native].into_iter().find(|rating| *rating != UNRATED)
}

fn year_of(year: i16) -> Option<i16> {
    (year != i16::MIN).then_some(year)
}

fn release_of(release: u32) -> Option<u32> {
    (release != 0).then_some(release)
}

impl<'a> Context<'a> {
    pub(crate) fn new(snapshot: &'a Snapshot, distinct: bool) -> Self {
        let listened = snapshot
            .source_slots
            .iter()
            .map(|key| {
                snapshot.source(*key).is_none_or(|source| {
                    source
                        .kind
                        .as_deref()
                        .is_none_or(|kind| LISTENED_KINDS.contains(&kind))
                })
            })
            .collect();
        Self {
            snapshot,
            listened,
            distinct,
        }
    }

    pub(crate) fn played(&self, slot: u32, record: &TrackRec) -> Played {
        let stats = self.snapshot.stats.get(&slot).copied().unwrap_or_default();
        if record.source == NO_SOURCE
            || self
                .listened
                .get(usize::from(record.source))
                .copied()
                .unwrap_or(true)
        {
            Played {
                plays: stats.plays,
                skips: stats.skips,
                last: known(stats.last_played),
            }
        } else {
            Played {
                plays: known(stats.native_plays).unwrap_or(0),
                skips: 0,
                last: known(stats.native_last_played),
            }
        }
    }

    /// Plays and skips from Rufin's listens, whatever the source.
    pub(crate) fn listened(&self, slot: u32) -> Played {
        let stats = self.snapshot.stats.get(&slot).copied().unwrap_or_default();
        Played {
            plays: stats.plays,
            skips: stats.skips,
            last: known(stats.last_played),
        }
    }

    pub(crate) fn source_slot(&self, source: SourceKey) -> Option<u16> {
        self.snapshot.source_slot(source)
    }

    /// A live track that is listed.
    pub(crate) fn visible(&self, slot: u32) -> Option<&'a TrackRec> {
        self.snapshot
            .tracks
            .live(slot)
            .filter(|record| record.indexed())
    }

    /// The album a track belongs to while that album is resident.
    pub(crate) fn album_of(&self, record: &TrackRec) -> Option<u32> {
        self.snapshot
            .albums
            .live(record.album)
            .map(|_| record.album)
    }

    /// The album-artist slots of a track: its album's artists, else its own album artists.
    pub(crate) fn album_artists(&self, record: &TrackRec) -> &'a [u32] {
        match self.snapshot.albums.live(record.album) {
            Some(album) => self.snapshot.album_runs(album)[0],
            None => self.snapshot.track_runs(record)[1],
        }
    }

    /// Whether a track belongs to a genre directly or through its album.
    fn in_genre(&self, record: &TrackRec, tag: u32) -> bool {
        self.snapshot.track_runs(record)[2].contains(&tag)
            || self
                .snapshot
                .albums
                .live(record.album)
                .is_some_and(|album| self.snapshot.album_runs(album)[1].contains(&tag))
    }

    /// The tags a track counts toward: its own, and its album's genres.
    pub(crate) fn tags_of(&self, record: &TrackRec, into: &mut Vec<u32>) {
        into.clear();
        into.extend_from_slice(self.snapshot.track_runs(record)[2]);
        if let Some(album) = self.snapshot.albums.live(record.album) {
            into.extend_from_slice(self.snapshot.album_runs(album)[1]);
        }
        into.sort_unstable();
        into.dedup();
    }

    fn tag_kind(&self, slot: u32) -> Option<TagKind> {
        self.snapshot.tags.live(slot).map(TagRec::kind)
    }

    pub(crate) fn artist_sort(&self, slot: u32) -> Option<Cow<'a, str>> {
        self.snapshot
            .artists
            .live(slot)
            .map(|artist| self.snapshot.artist_sort(artist))
    }

    /// The sort text of an album's first artist.
    pub(crate) fn album_artist_sort(&self, album: u32) -> Option<Cow<'a, str>> {
        let record = self.snapshot.albums.live(album)?;
        self.snapshot.album_runs(record)[0]
            .iter()
            .find_map(|slot| self.artist_sort(*slot))
    }

    /// The sort text of a track's first genre.
    fn genre_sort(&self, record: &TrackRec) -> Option<Cow<'a, str>> {
        self.snapshot.track_runs(record)[2].iter().find_map(|slot| {
            self.snapshot
                .tags
                .live(*slot)
                .filter(|tag| tag.kind() == TagKind::Genre)
                .map(|tag| self.snapshot.name_sort(tag.name, tag.sort))
        })
    }
}

/// Visible-track totals of one collection.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Totals {
    pub(crate) tracks: u32,
    pub(crate) duration: u64,
    pub(crate) downloaded: u32,
    pub(crate) plays: u64,
    pub(crate) last_played: Option<u32>,
    pub(crate) albums: u32,
}

impl Totals {
    fn add(&mut self, record: &TrackRec, played: Played) {
        self.tracks += 1;
        self.duration += u64::from(record.duration_ms);
        self.downloaded += u32::from(record.flags & DOWNLOADED != 0);
        self.plays += u64::from(played.plays);
        self.last_played = self.last_played.max(played.last);
    }

    pub(crate) fn summary(&self) -> crate::CollectionSummary {
        crate::CollectionSummary {
            album_count: i64::from(self.albums),
            track_count: i64::from(self.tracks),
            duration_millis: self.duration as i64,
            downloaded_count: i64::from(self.downloaded),
        }
    }
}

/// The picture an artist borrows when it has none of its own.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Borrowed {
    #[default]
    None,
    Album(u32),
    Track(u32),
}

/// Totals and borrowed pictures of every collection, from one pass over the visible tracks.
/// Built once per Catalog revision, and per music folder when a reader is scoped to one.
pub(crate) struct Derived {
    pub(crate) albums: Vec<Totals>,
    /// The first pictured visible track of each album.
    pub(crate) album_covers: Vec<u32>,
    /// Track artists and album artists.
    pub(crate) artists: [Vec<Totals>; 2],
    pub(crate) artist_covers: Vec<Borrowed>,
    pub(crate) tags: Vec<Totals>,
    pub(crate) folders: Vec<Totals>,
}

impl Derived {
    fn build(context: &Context<'_>, folder: Option<u32>) -> Self {
        let snapshot = context.snapshot;
        let mut derived = Self {
            albums: vec![Totals::default(); snapshot.albums.records.len()],
            album_covers: vec![NONE; snapshot.albums.records.len()],
            artists: [
                vec![Totals::default(); snapshot.artists.records.len()],
                vec![Totals::default(); snapshot.artists.records.len()],
            ],
            artist_covers: vec![Borrowed::None; snapshot.artists.records.len()],
            tags: vec![Totals::default(); snapshot.tags.records.len()],
            folders: vec![Totals::default(); snapshot.folders.records.len()],
        };
        let mut visible = snapshot
            .tracks
            .iter()
            .filter(|(_, record)| {
                record.indexed()
                    && folder.is_none_or(|folder| snapshot.track_runs(record)[3].contains(&folder))
            })
            .map(|(slot, record)| (context.album_of(record).unwrap_or(NONE), slot))
            .collect::<Vec<_>>();
        visible.sort_unstable();
        // Track covers ranked like an album's tracks; artist covers by album sort text.
        let mut album_cover_rank = HashMap::<u32, (u16, u16, Cow<'_, str>, i64)>::new();
        let mut artist_album = HashMap::<u32, (Cow<'_, str>, i64)>::new();
        let mut artist_track = HashMap::<u32, (Cow<'_, str>, i64)>::new();
        let mut tags = Vec::new();
        let mut owners: [Vec<u32>; 4] = Default::default();
        for group in visible.chunk_by(|left, right| left.0 == right.0) {
            let album = group[0].0;
            for owner in &mut owners {
                owner.clear();
            }
            for &(_, slot) in group {
                let record = &snapshot.tracks.records[slot as usize];
                let played = context.played(slot, record);
                let runs = snapshot.track_runs(record);
                if album != NONE {
                    derived.albums[album as usize].add(record, played);
                    if record.flags & PICTURE != 0 {
                        let rank = (
                            record.disc,
                            record.number,
                            snapshot.track_sort(record),
                            i64::from(record.key),
                        );
                        if album_cover_rank.get(&album).is_none_or(|best| rank < *best) {
                            derived.album_covers[album as usize] = slot;
                            album_cover_rank.insert(album, rank);
                        }
                    }
                } else if record.flags & PICTURE != 0 {
                    let rank = (snapshot.track_sort(record), i64::from(record.key));
                    for artist in dedup(runs[0]) {
                        if artist_track.get(&artist).is_none_or(|best| rank < *best) {
                            artist_track.insert(artist, rank.clone());
                        }
                    }
                }
                for artist in dedup(runs[0]) {
                    derived.artists[0][artist as usize].add(record, played);
                    owners[0].push(artist);
                }
                for artist in dedup(context.album_artists(record)) {
                    derived.artists[1][artist as usize].add(record, played);
                    owners[1].push(artist);
                }
                context.tags_of(record, &mut tags);
                for tag in &tags {
                    derived.tags[*tag as usize].add(record, played);
                    owners[2].push(*tag);
                }
                for folder in dedup(runs[3]) {
                    derived.folders[folder as usize].add(record, played);
                    owners[3].push(folder);
                }
            }
            if album == NONE {
                continue;
            }
            for owner in &mut owners {
                owner.sort_unstable();
                owner.dedup();
            }
            for artist in &owners[0] {
                derived.artists[0][*artist as usize].albums += 1;
            }
            for artist in &owners[1] {
                derived.artists[1][*artist as usize].albums += 1;
            }
            for tag in &owners[2] {
                derived.tags[*tag as usize].albums += 1;
            }
            for folder in &owners[3] {
                derived.folders[*folder as usize].albums += 1;
            }
            let record = &snapshot.albums.records[album as usize];
            if record.flags & PICTURE != 0 || derived.album_covers[album as usize] != NONE {
                let rank = (
                    Cow::Borrowed(snapshot.text(record.sort)),
                    i64::from(record.key),
                );
                let mut related = owners[0].clone();
                related.extend(snapshot.album_runs(record)[0]);
                related.sort_unstable();
                related.dedup();
                for artist in related {
                    if artist_album.get(&artist).is_none_or(|best| rank < *best) {
                        artist_album.insert(artist, rank.clone());
                    }
                }
            }
        }
        for (artist, (_, key)) in artist_track {
            if let Some(slot) = snapshot.tracks.slot(key) {
                derived.artist_covers[artist as usize] = Borrowed::Track(slot);
            }
        }
        for (artist, (_, key)) in artist_album {
            if let Some(slot) = snapshot.albums.slot(key) {
                derived.artist_covers[artist as usize] = Borrowed::Album(slot);
            }
        }
        derived
    }
}

fn dedup(slots: &[u32]) -> impl Iterator<Item = u32> + '_ {
    slots
        .iter()
        .enumerate()
        .filter(|(index, slot)| !slots[..*index].contains(slot))
        .map(|(_, slot)| *slot)
}

/// Where a row's picture binding comes from. The bindings are not resident; a reader fetches
/// the named rows by key.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Art {
    None,
    Track(i64),
    Album(i64),
    /// An artist's own candidates and the picture it borrows.
    Artist(i64, Option<Picture>),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Picture {
    Track(i64),
    Album(i64),
}

impl Context<'_> {
    /// Own picture when covers are distinct or the album has none, else the album's, else the
    /// first artist's.
    pub(crate) fn track_art(&self, derived: &Derived, record: &TrackRec) -> Art {
        let album = self
            .album_of(record)
            .map_or(Art::None, |album| self.album_art(derived, album));
        if record.flags & PICTURE != 0 && (self.distinct || album == Art::None) {
            return Art::Track(i64::from(record.key));
        }
        if album != Art::None {
            return album;
        }
        self.snapshot.track_runs(record)[0]
            .iter()
            .map(|artist| self.artist_art(derived, *artist))
            .find(|art| *art != Art::None)
            .unwrap_or(Art::None)
    }

    /// Own picture, else its first pictured track's, else its first artist's.
    pub(crate) fn album_art(&self, derived: &Derived, slot: u32) -> Art {
        let Some(record) = self.snapshot.albums.live(slot) else {
            return Art::None;
        };
        if record.flags & PICTURE != 0 {
            return Art::Album(i64::from(record.key));
        }
        if let Some(track) = derived
            .album_covers
            .get(slot as usize)
            .and_then(|cover| self.snapshot.tracks.live(*cover))
        {
            return Art::Track(i64::from(track.key));
        }
        self.snapshot.album_runs(record)[0]
            .first()
            .map_or(Art::None, |artist| self.artist_art(derived, *artist))
    }

    pub(crate) fn artist_art(&self, derived: &Derived, slot: u32) -> Art {
        let Some(record) = self.snapshot.artists.live(slot) else {
            return Art::None;
        };
        let borrowed = match derived.artist_covers.get(slot as usize) {
            Some(Borrowed::Album(album)) => match self.album_art(derived, *album) {
                Art::Album(key) => Some(Picture::Album(key)),
                Art::Track(key) => Some(Picture::Track(key)),
                _ => None,
            },
            Some(Borrowed::Track(track)) => self
                .snapshot
                .tracks
                .live(*track)
                .map(|track| Picture::Track(i64::from(track.key))),
            _ => None,
        };
        if record.flags & PICTURE == 0 && borrowed.is_none() {
            return Art::None;
        }
        Art::Artist(i64::from(record.key), borrowed)
    }
}

/// One row of a member order: a track slot and, in a playlist, the entry that holds it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Member {
    pub(crate) track: u32,
    pub(crate) entry: i64,
}

/// Facts a reader needs from the Store before it orders: they are not resident.
#[derive(Default)]
pub(crate) struct Inputs {
    /// Track keys of requested object ids, in request order.
    pub(crate) objects: Vec<i64>,
    /// Listens of each smart playlist window by track key: plays, skips and last play.
    pub(crate) periods: HashMap<Option<i64>, HashMap<i64, Played>>,
    /// Release classes of albums by key.
    pub(crate) classes: HashMap<i64, AlbumReleaseClass>,
    /// A smart playlist's members as of the time its page opened, by track key.
    pub(crate) smart: Option<Arc<Vec<i64>>>,
}

/// A lowercase haystack matcher for list filters.
fn contains(haystack: &str, needle: &str) -> bool {
    needle.is_empty() || haystack.to_lowercase().contains(needle)
}

/// A filter as clients type it: trimmed, lowercase and bounded.
pub(crate) fn filter_text(filter: &str) -> String {
    filter.trim().to_lowercase().chars().take(256).collect()
}

impl Context<'_> {
    /// The text a track filter matches: title, artist, album, comment and year.
    pub(crate) fn track_search(&self, record: &TrackRec) -> String {
        let snapshot = self.snapshot;
        format!(
            "{} {} {} {} {}",
            snapshot.text(record.title),
            snapshot.text(record.artist),
            snapshot.text(record.album_title),
            snapshot.text(record.comment),
            year_of(record.year)
                .map(|year| year.to_string())
                .unwrap_or_default()
        )
        .to_lowercase()
    }

    fn album_matches(&self, record: &AlbumRec, filter: &str) -> bool {
        let snapshot = self.snapshot;
        filter.is_empty()
            || format!(
                "{} {}",
                snapshot.text(record.title),
                snapshot.text(record.artist)
            )
            .to_lowercase()
            .contains(filter)
            || year_of(record.year).is_some_and(|year| year.to_string() == filter)
            || snapshot.album_runs(record)[0].iter().any(|slot| {
                snapshot
                    .artists
                    .live(*slot)
                    .is_some_and(|artist| contains(snapshot.text(artist.name), filter))
            })
            || snapshot.album_runs(record)[1].iter().any(|slot| {
                snapshot
                    .tags
                    .live(*slot)
                    .is_some_and(|tag| contains(snapshot.text(tag.name), filter))
            })
    }
}

/// Orders nulls last whatever the direction.
fn nulls_last<T: Ord>(left: Option<T>, right: Option<T>, descending: bool) -> Ordering {
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(left), Some(right)) => directed(left.cmp(&right), descending),
    }
}

fn directed(order: Ordering, descending: bool) -> Ordering {
    if descending { order.reverse() } else { order }
}

/// A track with the sort facts that need work to read.
struct TrackRow<'a> {
    member: Member,
    position: usize,
    record: &'a TrackRec,
    artist: &'a str,
    sort: Cow<'a, str>,
    played: Played,
    album_sort: Option<Cow<'a, str>>,
    album_year: Option<i16>,
    album_key: Option<i64>,
    album_artist: Option<Cow<'a, str>>,
    genre: Option<Cow<'a, str>>,
}

impl Context<'_> {
    fn track_row(&self, sort: CatalogSort, position: usize, member: Member) -> TrackRow<'_> {
        let snapshot = self.snapshot;
        let record = &snapshot.tracks.records[member.track as usize];
        let album = snapshot.albums.live(record.album);
        TrackRow {
            member,
            position,
            record,
            artist: snapshot.text(record.artist),
            sort: snapshot.track_sort(record),
            played: if matches!(sort, CatalogSort::PlayCount | CatalogSort::LastPlayed) {
                self.played(member.track, record)
            } else {
                Played::default()
            },
            album_sort: album.map(|album| snapshot.album_sort(album)),
            album_year: album.and_then(|album| year_of(album.year)),
            album_key: album.map(|album| i64::from(album.key)),
            album_artist: if sort == CatalogSort::AlbumArtist {
                album.and_then(|_| self.album_artist_sort(record.album))
            } else {
                None
            },
            genre: if sort == CatalogSort::Genre {
                self.genre_sort(record)
            } else {
                None
            },
        }
    }

    /// Text of the letter index of a track order.
    fn track_section(&self, sort: CatalogSort, row: &TrackRow<'_>) -> Option<String> {
        let snapshot = self.snapshot;
        Some(match sort {
            CatalogSort::Title => snapshot.text(row.record.title).to_owned(),
            CatalogSort::Artist => snapshot.text(row.record.artist).to_owned(),
            CatalogSort::AlbumArtist => row.album_artist.as_deref().unwrap_or_default().to_owned(),
            CatalogSort::Album => row.album_sort.as_deref().unwrap_or_default().to_owned(),
            CatalogSort::Genre => row.genre.as_deref().unwrap_or_default().to_owned(),
            _ => return None,
        })
    }
}

fn compare_tracks(
    sort: CatalogSort,
    descending: bool,
    a: &TrackRow<'_>,
    b: &TrackRow<'_>,
) -> Ordering {
    use CatalogSort as S;
    let (left, right) = (a.record, b.record);
    let key = left.key.cmp(&right.key);
    let text = a.sort.cmp(&b.sort);
    let first = |order: Ordering| directed(order, descending);
    let disc = left.disc.cmp(&right.disc);
    let number = left.number.cmp(&right.number);
    let album_tail = || {
        nulls_last(a.album_year, b.album_year, false)
            .then_with(|| a.album_sort.cmp(&b.album_sort))
            .then_with(|| nulls_last(a.album_key, b.album_key, false))
            .then(disc)
            .then(number)
    };
    let order = match sort {
        S::Position => return first(a.position.cmp(&b.position)),
        S::Title => return first(text).then(key),
        S::TrackNumber => first(disc).then(first(number)),
        S::Artist => first(a.artist.cmp(b.artist)).then_with(album_tail),
        S::AlbumArtist => nulls_last(
            a.album_artist.as_deref(),
            b.album_artist.as_deref(),
            descending,
        )
        .then_with(album_tail),
        S::Album => first(a.album_sort.cmp(&b.album_sort))
            .then_with(|| nulls_last(a.album_key, b.album_key, false))
            .then(disc)
            .then(number),
        S::Year => nulls_last(year_of(left.year), year_of(right.year), descending),
        S::ReleaseDate => nulls_last(
            release_of(left.release),
            release_of(right.release),
            descending,
        ),
        S::DateAdded => nulls_last(known(left.added), known(right.added), descending),
        S::LastPlayed => nulls_last(a.played.last, b.played.last, descending),
        S::PlayCount => first(a.played.plays.cmp(&b.played.plays)),
        S::UserRating => nulls_last(
            rating(left.rating, left.native_rating),
            rating(right.rating, right.native_rating),
            descending,
        ),
        S::Genre => nulls_last(a.genre.as_deref(), b.genre.as_deref(), descending),
        S::Bpm => nulls_last(
            (left.bpm != u16::MAX).then_some(left.bpm),
            (right.bpm != u16::MAX).then_some(right.bpm),
            descending,
        ),
        S::Duration => first(left.duration_ms.cmp(&right.duration_ms)),
        S::Favorite => first(
            favorite(left.flags)
                .unwrap_or(false)
                .cmp(&favorite(right.flags).unwrap_or(false)),
        ),
        S::AlbumCount | S::TrackCount => Ordering::Equal,
    };
    order.then(text).then(key)
}

impl Context<'_> {
    fn in_folder(&self, record: &TrackRec, folder: Option<u32>) -> bool {
        folder.is_none_or(|folder| self.snapshot.track_runs(record)[3].contains(&folder))
    }

    fn folder_slot(&self, folder: Option<crate::FolderKey>) -> Option<Option<u32>> {
        match folder {
            None => Some(None),
            Some(key) => self.snapshot.folders.slot(key.raw()).map(Some),
        }
    }

    /// Visible tracks of a source, in slot order.
    fn source_tracks(&self, source: SourceKey, folder: Option<u32>) -> Vec<u32> {
        let Some(owner) = self.source_slot(source) else {
            return Vec::new();
        };
        self.snapshot
            .tracks
            .iter()
            .filter(|(_, record)| {
                record.source == owner && record.indexed() && self.in_folder(record, folder)
            })
            .map(|(slot, _)| slot)
            .collect()
    }

    /// The members a collection holds before filters and sorting.
    fn collection_members(
        &self,
        collection: &Collection,
        folder: Option<u32>,
        inputs: &Inputs,
    ) -> Vec<Member> {
        let snapshot = self.snapshot;
        let member = |track| Member { track, entry: 0 };
        let tracks = |keep: &dyn Fn(&TrackRec) -> bool| {
            snapshot
                .tracks
                .iter()
                .filter(|(_, record)| {
                    record.indexed() && self.in_folder(record, folder) && keep(record)
                })
                .map(|(slot, _)| member(slot))
                .collect::<Vec<_>>()
        };
        match collection {
            Collection::Source(source) => self
                .source_tracks(*source, folder)
                .into_iter()
                .map(member)
                .collect(),
            Collection::Objects { .. } => {
                let mut seen = HashSet::new();
                inputs
                    .objects
                    .iter()
                    .filter_map(|key| snapshot.tracks.slot(*key))
                    .filter(|slot| seen.insert(*slot))
                    .filter(|slot| {
                        self.visible(*slot)
                            .is_some_and(|record| self.in_folder(record, folder))
                    })
                    .map(member)
                    .collect()
            }
            Collection::Album(key) => match snapshot.albums.slot(key.raw()) {
                Some(album) => tracks(&|record| record.album == album),
                None => Vec::new(),
            },
            Collection::Artist { key, album_artist } => match snapshot.artists.slot(key.raw()) {
                Some(artist) if *album_artist => {
                    tracks(&|record| self.album_artists(record).contains(&artist))
                }
                Some(artist) => tracks(&|record| snapshot.track_runs(record)[0].contains(&artist)),
                None => Vec::new(),
            },
            Collection::Genre(key) => match snapshot.tags.slot(key.raw()) {
                Some(tag) => tracks(&|record| self.in_genre(record, tag)),
                None => Vec::new(),
            },
            Collection::Mood(key) => match snapshot.tags.slot(key.raw()) {
                Some(tag) => tracks(&|record| snapshot.track_runs(record)[2].contains(&tag)),
                None => Vec::new(),
            },
            Collection::Playlist(key) => {
                let Some(playlist) = snapshot.playlists.get(key.raw()) else {
                    return Vec::new();
                };
                let scoped = playlist.source != NO_SOURCE;
                playlist
                    .entries
                    .iter()
                    .filter(|entry| {
                        snapshot
                            .tracks
                            .live(entry.track)
                            .is_some_and(|record| !scoped || self.in_folder(record, folder))
                    })
                    .map(|entry| Member {
                        track: entry.track,
                        entry: entry.key,
                    })
                    .collect()
            }
            Collection::Smart { .. } => inputs
                .smart
                .as_deref()
                .map_or_else(Vec::new, |tracks| self.listed_tracks(tracks))
                .into_iter()
                .map(member)
                .collect(),
            Collection::History { .. } => Vec::new(),
        }
    }

    /// A member read's rows in order.
    pub(crate) fn member_order(&self, read: &MemberRead, inputs: &Inputs) -> Vec<Member> {
        let Some(folder) = self.folder_slot(read.folder) else {
            return Vec::new();
        };
        let mut members = self.collection_members(&read.collection, folder, inputs);
        let filter = filter_text(&read.filter);
        let records = &self.snapshot.tracks.records;
        members.retain(|member| {
            let record = &records[member.track as usize];
            (filter.is_empty() || self.track_search(record).contains(&filter))
                && (!read.favorites_only || favorite(record.flags) == Some(true))
                && (!read.downloaded_only || record.flags & DOWNLOADED != 0)
        });
        if matches!(read.collection, Collection::Smart { .. }) && read.sort.is_none() {
            return members;
        }
        let sort = read.effective_sort();
        let mut rows = members
            .into_iter()
            .enumerate()
            .map(|(position, member)| self.track_row(sort, position, member))
            .collect::<Vec<_>>();
        rows.sort_by(|a, b| {
            compare_tracks(sort, read.descending, a, b).then(a.position.cmp(&b.position))
        });
        rows.into_iter().map(|row| row.member).collect()
    }

    /// Letter sections of a member order.
    pub(crate) fn member_sections(
        &self,
        read: &MemberRead,
        order: &[Member],
    ) -> Vec<crate::ScrollSection> {
        let sort = read.effective_sort();
        if matches!(read.collection, Collection::Smart { .. }) && read.sort.is_none() {
            return Vec::new();
        }
        sections(order.iter().enumerate().map(|(position, member)| {
            self.track_section(sort, &self.track_row(sort, position, *member))
        }))
    }

    /// Disc sections of an album order: where each disc starts, when there are several.
    pub(crate) fn disc_sections(&self, read: &MemberRead, order: &[Member]) -> Vec<(u32, i64)> {
        if read.effective_sort() != CatalogSort::TrackNumber {
            return Vec::new();
        }
        let records = &self.snapshot.tracks.records;
        let mut sections = Vec::<(u32, i64)>::new();
        for (position, member) in order.iter().enumerate() {
            let disc = i64::from(records[member.track as usize].disc);
            if sections.last().is_none_or(|(_, last)| *last != disc) {
                sections.push((position as u32, disc));
            }
        }
        if sections.iter().filter(|(_, disc)| *disc > 0).count() <= 1 {
            return Vec::new();
        }
        sections
    }
}

/// The letter-index bucket of a text: A-Z, kana rows, 漢 for ideographs, 한 for Hangul, # for
/// digits and other ASCII, and … for the rest.
pub(crate) fn bucket(text: &str) -> &'static str {
    const KANA: [&str; 10] = ["あ", "か", "さ", "た", "な", "は", "ま", "や", "ら", "わ"];
    let Some(first) = text.chars().next() else {
        return "#";
    };
    if first.is_ascii_alphabetic() {
        const LETTERS: [&str; 26] = [
            "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q",
            "R", "S", "T", "U", "V", "W", "X", "Y", "Z",
        ];
        return LETTERS[usize::from(first.to_ascii_lowercase() as u8 - b'a')];
    }
    let code = u32::from(first);
    match code {
        65382..=65437 if code != 65392 => match code {
            65382 | 65436.. => "わ",
            65383..=65387 | 65393..=65397 => "あ",
            65398..=65402 => "か",
            65403..=65407 => "さ",
            65391 | 65408..=65412 => "た",
            65413..=65417 => "な",
            65418..=65422 => "は",
            65423..=65427 => "ま",
            65388..=65390 | 65428..=65430 => "や",
            _ => "ら",
        },
        12353..=12438 | 12449..=12538 => {
            let code = if code >= 12449 { code - 96 } else { code };
            let row = match code {
                12436 => 0,
                12437 | 12438 => 1,
                ..=12362 => 0,
                ..=12372 => 1,
                ..=12382 => 2,
                ..=12393 => 3,
                ..=12398 => 4,
                ..=12413 => 5,
                ..=12418 => 6,
                ..=12424 => 7,
                ..=12429 => 8,
                _ => 9,
            };
            KANA[row]
        }
        13312..=19903 | 19968..=40959 | 63744..=64255 | 131072..=205743 => "漢",
        44032..=55203 | 4352..=4607 | 12593..=12686 => "한",
        ..128 => "#",
        _ => "…",
    }
}

/// Sections of an order from the texts of its rows: each bucket where it first appears.
pub(crate) fn sections(texts: impl Iterator<Item = Option<String>>) -> Vec<crate::ScrollSection> {
    let mut seen = HashSet::new();
    let mut sections = Vec::new();
    for (index, text) in texts.enumerate() {
        let Some(text) = text else {
            return Vec::new();
        };
        let title = bucket(&text);
        if seen.insert(title) {
            sections.push(crate::ScrollSection {
                title: title.to_owned(),
                index: index as u64,
            });
        }
    }
    sections
}

/// A release class's place in an artist's releases.
pub(crate) fn release_code(class: AlbumReleaseClass) -> usize {
    match class {
        AlbumReleaseClass::Album => 0,
        AlbumReleaseClass::Ep => 1,
        AlbumReleaseClass::Single => 2,
        AlbumReleaseClass::Collection => 3,
        AlbumReleaseClass::Other => 4,
    }
}

/// Classifies an album by its compilation flag and release types.
pub(crate) fn release_class(compilation: bool, types: &[String]) -> AlbumReleaseClass {
    let tokens = types
        .iter()
        .flat_map(|types| types.split('|'))
        .map(|token| token.trim().to_lowercase())
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    if compilation {
        AlbumReleaseClass::Collection
    } else if tokens.is_empty() || tokens.iter().any(|token| token == "album") {
        AlbumReleaseClass::Album
    } else if tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "compilation" | "compilations" | "collection" | "collections"
        )
    }) {
        AlbumReleaseClass::Collection
    } else if tokens
        .iter()
        .any(|token| matches!(token.as_str(), "ep" | "e.p."))
    {
        AlbumReleaseClass::Ep
    } else if tokens.iter().any(|token| token == "single") {
        AlbumReleaseClass::Single
    } else {
        AlbumReleaseClass::Other
    }
}

/// One row of a named list with the facts its sorts read.
struct ListRow<'a> {
    key: i64,
    sort: Cow<'a, str>,
    totals: Totals,
    rating: Option<u8>,
    favorite: bool,
    /// Albums: year, release, added and the first artist's sort text.
    year: Option<i16>,
    release: Option<u32>,
    added: Option<u32>,
    artist: Option<Cow<'a, str>>,
    class: usize,
}

impl ListRow<'_> {
    fn new(key: i64, sort: Cow<'_, str>) -> ListRow<'_> {
        ListRow {
            key,
            sort,
            totals: Totals::default(),
            rating: None,
            favorite: false,
            year: None,
            release: None,
            added: None,
            artist: None,
            class: 0,
        }
    }
}

fn compare_albums(
    sort: CatalogSort,
    descending: bool,
    a: &ListRow<'_>,
    b: &ListRow<'_>,
) -> Ordering {
    use CatalogSort as S;
    let text = a.sort.cmp(&b.sort);
    let key = a.key.cmp(&b.key);
    let order = match sort {
        S::Title => return directed(text, descending).then(key),
        S::Artist | S::AlbumArtist => {
            return nulls_last(a.artist.as_deref(), b.artist.as_deref(), descending)
                .then_with(|| nulls_last(a.year, b.year, false))
                .then(text)
                .then(key);
        }
        S::Year => nulls_last(a.year, b.year, descending),
        S::ReleaseDate => nulls_last(a.release, b.release, descending),
        S::DateAdded => nulls_last(a.added, b.added, descending),
        S::LastPlayed => nulls_last(a.totals.last_played, b.totals.last_played, descending),
        S::PlayCount => directed(a.totals.plays.cmp(&b.totals.plays), descending),
        S::UserRating => nulls_last(a.rating, b.rating, descending),
        S::Favorite => directed(a.favorite.cmp(&b.favorite), descending),
        S::TrackCount => directed(a.totals.tracks.cmp(&b.totals.tracks), descending),
        S::Duration => directed(a.totals.duration.cmp(&b.totals.duration), descending),
        _ => Ordering::Equal,
    };
    order.then(text).then(key)
}

fn compare_named(
    sort: CatalogSort,
    descending: bool,
    a: &ListRow<'_>,
    b: &ListRow<'_>,
) -> Ordering {
    use CatalogSort as S;
    let text = a.sort.cmp(&b.sort);
    let key = a.key.cmp(&b.key);
    let order = match sort {
        S::Title | S::Artist => return directed(text, descending).then(key),
        S::AlbumCount => directed(a.totals.albums.cmp(&b.totals.albums), descending),
        S::TrackCount => directed(a.totals.tracks.cmp(&b.totals.tracks), descending),
        S::Duration => directed(a.totals.duration.cmp(&b.totals.duration), descending),
        S::PlayCount => directed(a.totals.plays.cmp(&b.totals.plays), descending),
        S::LastPlayed => nulls_last(a.totals.last_played, b.totals.last_played, descending),
        S::UserRating => nulls_last(a.rating, b.rating, descending),
        S::Favorite => directed(a.favorite.cmp(&b.favorite), descending),
        _ => Ordering::Equal,
    };
    order.then(text).then(key)
}

/// Rows of the playlist lists, which sort by creation, name or totals.
struct PlaylistItem<'a> {
    key: i64,
    name: &'a str,
    sort: String,
    position: i64,
    totals: Totals,
    folder: Option<i64>,
}

fn compare_items(
    sort: CatalogSort,
    descending: bool,
    a: &PlaylistItem<'_>,
    b: &PlaylistItem<'_>,
) -> Ordering {
    let order = match sort {
        CatalogSort::Position => a.position.cmp(&b.position),
        CatalogSort::TrackCount => a.totals.tracks.cmp(&b.totals.tracks),
        CatalogSort::Duration => a.totals.duration.cmp(&b.totals.duration),
        _ => a.sort.cmp(&b.sort),
    };
    directed(order, descending)
        .then_with(|| a.sort.cmp(&b.sort))
        .then(a.key.cmp(&b.key))
}

impl Context<'_> {
    /// Totals of a playlist's entries, hidden tracks included. A source playlist in a music
    /// folder counts only the entries in it.
    pub(crate) fn playlist_totals(&self, record: &PlaylistRec, folder: Option<u32>) -> Totals {
        let mut totals = Totals::default();
        let scoped = record.source != NO_SOURCE;
        for entry in &record.entries {
            if let Some(track) = self.snapshot.tracks.live(entry.track)
                && (!scoped || self.in_folder(track, folder))
            {
                totals.add(track, Played::default());
            }
        }
        totals
    }

    /// Totals of a smart playlist's members.
    /// A smart playlist's totals: of the members its open page holds, or evaluated now.
    pub(crate) fn smart_totals(
        &self,
        definition: &SmartPlaylistDefinition,
        inputs: &Inputs,
        opened: Option<&[i64]>,
    ) -> Totals {
        let mut totals = Totals::default();
        let slots = match opened {
            Some(tracks) => self.listed_tracks(tracks),
            None => self.evaluate(definition, inputs),
        };
        for slot in slots {
            totals.add(
                &self.snapshot.tracks.records[slot as usize],
                Played::default(),
            );
        }
        totals
    }

    /// Slots of the tracks still listed, in the given order.
    fn listed_tracks(&self, keys: &[i64]) -> Vec<u32> {
        let tracks = &self.snapshot.tracks;
        keys.iter()
            .filter_map(|key| tracks.slot(*key))
            .filter(|slot| tracks.live(*slot).is_some_and(|record| record.indexed()))
            .collect()
    }

    /// Whether a playlist is listed: it has a name and its source still lists it.
    pub(crate) fn listed_playlist(&self, record: &PlaylistRec) -> bool {
        !self.snapshot.text(record.name).is_empty()
            && (record.flags & SOURCE_OWNED == 0 || record.indexed())
    }

    fn playlist_items(
        &self,
        read: &ListRead,
        folder: Option<u32>,
        inputs: &Inputs,
    ) -> Vec<PlaylistItem<'_>> {
        let snapshot = self.snapshot;
        let smart = matches!(read.kind, ListKind::SmartPlaylists { .. });
        let totals = matches!(read.sort, CatalogSort::TrackCount | CatalogSort::Duration)
            || !matches!(read.playlist_scope, PlaylistScope::All);
        if smart {
            return snapshot
                .smart_playlists
                .values()
                .map(|playlist| PlaylistItem {
                    key: playlist.key,
                    name: &playlist.name,
                    sort: playlist.name.to_lowercase(),
                    position: playlist.position,
                    totals: if totals {
                        self.smart_totals(&playlist.definition, inputs, None)
                    } else {
                        Totals::default()
                    },
                    folder: playlist.folder,
                })
                .collect();
        }
        let source = read.source.and_then(|source| self.source_slot(source));
        snapshot
            .playlists
            .iter()
            .filter(|(_, record)| {
                self.listed_playlist(record)
                    && (record.source == NO_SOURCE
                        || source.is_some_and(|source| record.source == source))
                    && (record.source == NO_SOURCE
                        || folder.is_none()
                        || self.playlist_totals(record, folder).tracks > 0)
            })
            .map(|(_, record)| {
                let name = snapshot.text(record.name);
                PlaylistItem {
                    key: record.key,
                    name,
                    sort: name.to_lowercase(),
                    position: i64::from(record.created),
                    totals: self.playlist_totals(record, folder),
                    folder: (record.folder != 0).then_some(record.folder),
                }
            })
            .collect()
    }

    /// Totals of the playlists each playlist folder holds, as the list read shows them.
    pub(crate) fn playlist_folder_totals(&self, read: &ListRead) -> HashMap<i64, Totals> {
        let folder = self.folder_slot(read.folder).flatten();
        let mut totals = HashMap::<i64, Totals>::new();
        for item in self.playlist_items(read, folder, &Inputs::default()) {
            if let Some(owner) = item.folder {
                let total = totals.entry(owner).or_default();
                total.tracks += item.totals.tracks;
                total.duration += item.totals.duration;
                total.downloaded += item.totals.downloaded;
            }
        }
        totals
    }

    /// Playlists, smart playlists and their folders in one key space: folders are negative.
    fn playlist_order(&self, read: &ListRead, folder: Option<u32>, inputs: &Inputs) -> Vec<i64> {
        let smart = matches!(read.kind, ListKind::SmartPlaylists { .. });
        let filter = filter_text(&read.filter);
        let has_folders = self
            .snapshot
            .playlist_folders
            .values()
            .any(|folder| folder.smart == smart);
        let children = self.playlist_items(read, folder, inputs);
        let mut items =
            if read.playlist_scope == PlaylistScope::All
                || read.playlist_scope == PlaylistScope::Root && !has_folders
            {
                children
            } else {
                let mut items = Vec::new();
                if read.playlist_scope != PlaylistScope::Folders {
                    let parent = match read.playlist_scope {
                        PlaylistScope::Folder { key } => Some(key.raw()),
                        _ => None,
                    };
                    items.extend(children.iter().filter(|item| item.folder == parent).map(
                        |item| PlaylistItem {
                            name: item.name,
                            sort: item.sort.clone(),
                            ..*item
                        },
                    ));
                }
                if matches!(
                    read.playlist_scope,
                    PlaylistScope::Root | PlaylistScope::Folders
                ) {
                    for playlist_folder in self.snapshot.playlist_folders.values() {
                        if playlist_folder.smart != smart {
                            continue;
                        }
                        let mut totals = Totals::default();
                        for child in children
                            .iter()
                            .filter(|child| child.folder == Some(playlist_folder.key))
                        {
                            totals.tracks += child.totals.tracks;
                            totals.duration += child.totals.duration;
                            totals.downloaded += child.totals.downloaded;
                        }
                        items.push(PlaylistItem {
                            key: -playlist_folder.key,
                            name: &playlist_folder.name,
                            sort: playlist_folder.name.to_lowercase(),
                            position: playlist_folder.key,
                            totals,
                            folder: None,
                        });
                    }
                }
                items
            };
        items.retain(|item| item.name.to_lowercase().contains(&filter));
        if smart && matches!(read.playlist_scope, PlaylistScope::All) {
            // Smart playlists keep their own order between equal values.
            items.sort_by(|a, b| {
                let order = match read.sort {
                    CatalogSort::Position => a.position.cmp(&b.position),
                    CatalogSort::TrackCount => a.totals.tracks.cmp(&b.totals.tracks),
                    CatalogSort::Duration => a.totals.duration.cmp(&b.totals.duration),
                    _ => a.sort.cmp(&b.sort),
                };
                directed(order, read.descending)
                    .then(a.position.cmp(&b.position))
                    .then(a.key.cmp(&b.key))
            });
        } else {
            items.sort_by(|a, b| compare_items(read.sort, read.descending, a, b));
        }
        items.into_iter().map(|item| item.key).collect()
    }

    /// The albums an artist has visible tracks on, in a role.
    fn artist_albums(&self, artist: u32, album_artist: bool, folder: Option<u32>) -> HashSet<u32> {
        let mut albums = HashSet::new();
        for (_, record) in self.snapshot.tracks.iter() {
            if !record.indexed() || !self.in_folder(record, folder) {
                continue;
            }
            let Some(album) = self.album_of(record) else {
                continue;
            };
            let member = if album_artist {
                self.album_artists(record).contains(&artist)
            } else {
                self.snapshot.track_runs(record)[0].contains(&artist)
            };
            if member {
                albums.insert(album);
            }
        }
        albums
    }

    fn album_row(&self, derived: &Derived, slot: u32, record: &AlbumRec) -> ListRow<'_> {
        let mut row = ListRow::new(i64::from(record.key), self.snapshot.album_sort(record));
        row.totals = derived.albums[slot as usize];
        row.rating = rating(record.rating, record.native_rating);
        row.favorite = favorite(record.flags) == Some(true);
        row.year = year_of(record.year);
        row.release = release_of(record.release);
        row.added = known(record.added);
        row.artist = self.album_artist_sort(slot);
        row
    }

    /// The keys a list read shows, in order. Playlist folders have negative keys.
    pub(crate) fn list_order(
        &self,
        read: &ListRead,
        derived: &Derived,
        inputs: &Inputs,
    ) -> Vec<i64> {
        let snapshot = self.snapshot;
        let Some(folder) = self.folder_slot(read.folder) else {
            return Vec::new();
        };
        if matches!(
            read.kind,
            ListKind::Playlists | ListKind::SmartPlaylists { .. }
        ) {
            return self.playlist_order(read, folder, inputs);
        }
        let Some(source) = read.source.and_then(|source| self.source_slot(source)) else {
            return Vec::new();
        };
        let filter = filter_text(&read.filter);
        let mut rows = Vec::new();
        match &read.kind {
            ListKind::Albums => {
                for (slot, record) in snapshot.albums.iter() {
                    let totals = derived.albums[slot as usize];
                    if record.source == source
                        && totals.tracks > 0
                        && self.album_matches(record, &filter)
                        && (!read.favorites_only || favorite(record.flags) == Some(true))
                        && (!read.downloaded_only || totals.downloaded > 0)
                    {
                        rows.push(self.album_row(derived, slot, record));
                    }
                }
                rows.sort_by(|a, b| compare_albums(read.sort, read.descending, a, b));
            }
            ListKind::ArtistAlbums {
                artist,
                album_artist,
                release_class,
                filters,
            } => {
                let Some(artist) = snapshot.artists.slot(artist.raw()) else {
                    return Vec::new();
                };
                for slot in self.artist_albums(artist, *album_artist, folder) {
                    let record = &snapshot.albums.records[slot as usize];
                    let class = inputs
                        .classes
                        .get(&i64::from(record.key))
                        .copied()
                        .unwrap_or(AlbumReleaseClass::Album);
                    let text = filters.as_ref().map_or(filter.clone(), |filters| {
                        filter_text(&filters[release_code(class)])
                    });
                    let totals = derived.albums[slot as usize];
                    let matched = text.is_empty()
                        || format!(
                            "{} {}",
                            snapshot.text(record.title),
                            snapshot.text(record.artist)
                        )
                        .to_lowercase()
                        .contains(&text)
                        || year_of(record.year).is_some_and(|year| year.to_string() == text);
                    if record.source == source
                        && matched
                        && release_class.is_none_or(|wanted| wanted == class)
                        && (!read.favorites_only || favorite(record.flags) == Some(true))
                        && (!read.downloaded_only || totals.downloaded > 0)
                    {
                        let mut row = self.album_row(derived, slot, record);
                        row.class = release_code(class);
                        rows.push(row);
                    }
                }
                rows.sort_by(|a, b| {
                    a.class
                        .cmp(&b.class)
                        .then_with(|| compare_albums(read.sort, read.descending, a, b))
                });
            }
            ListKind::Artists { album_artist } => {
                let role = usize::from(*album_artist);
                for (slot, record) in snapshot.artists.iter() {
                    let totals = derived.artists[role][slot as usize];
                    if record.source == source
                        && totals.tracks > 0
                        && contains(snapshot.text(record.name), &filter)
                        && (!read.favorites_only || favorite(record.flags) == Some(true))
                        && (!read.downloaded_only || totals.downloaded > 0)
                    {
                        let mut row =
                            ListRow::new(i64::from(record.key), snapshot.artist_sort(record));
                        row.totals = totals;
                        row.rating = rating(record.rating, record.native_rating);
                        row.favorite = favorite(record.flags) == Some(true);
                        rows.push(row);
                    }
                }
                rows.sort_by(|a, b| compare_named(read.sort, read.descending, a, b));
            }
            ListKind::Genres | ListKind::Moods => {
                let kind = if read.kind == ListKind::Genres {
                    TagKind::Genre
                } else {
                    TagKind::Mood
                };
                for (slot, record) in snapshot.tags.iter() {
                    let totals = derived.tags[slot as usize];
                    let listed = totals.tracks > 0 || folder.is_none() && record.indexed();
                    if record.source == source
                        && record.kind() == kind
                        && listed
                        && contains(snapshot.text(record.name), &filter)
                        && (!read.downloaded_only || totals.downloaded > 0)
                    {
                        let mut row = ListRow::new(
                            i64::from(record.key),
                            snapshot.name_sort(record.name, record.sort),
                        );
                        row.totals = totals;
                        rows.push(row);
                    }
                }
                rows.sort_by(|a, b| compare_named(read.sort, read.descending, a, b));
            }
            ListKind::Playlists | ListKind::SmartPlaylists { .. } => unreachable!(),
        }
        rows.into_iter().map(|row| row.key).collect()
    }

    /// Letter sections of a list order.
    pub(crate) fn list_sections(
        &self,
        read: &ListRead,
        order: &[i64],
    ) -> Vec<crate::ScrollSection> {
        let snapshot = self.snapshot;
        let texts = order.iter().map(|key| -> Option<String> {
            Some(match &read.kind {
                ListKind::Albums | ListKind::ArtistAlbums { .. } => {
                    let slot = snapshot.albums.slot(*key)?;
                    match read.sort {
                        CatalogSort::Title => snapshot
                            .album_sort(&snapshot.albums.records[slot as usize])
                            .into_owned(),
                        CatalogSort::Artist | CatalogSort::AlbumArtist => self
                            .album_artist_sort(slot)
                            .map(Cow::into_owned)
                            .unwrap_or_default(),
                        _ => return None,
                    }
                }
                ListKind::Artists { .. } => {
                    if !matches!(read.sort, CatalogSort::Title | CatalogSort::Artist) {
                        return None;
                    }
                    let artist = snapshot.artists.get(*key)?;
                    snapshot.artist_sort(artist).into_owned()
                }
                ListKind::Genres | ListKind::Moods => {
                    if !matches!(read.sort, CatalogSort::Title | CatalogSort::Artist) {
                        return None;
                    }
                    let tag = snapshot.tags.get(*key)?;
                    snapshot.name_sort(tag.name, tag.sort).into_owned()
                }
                ListKind::Playlists | ListKind::SmartPlaylists { .. } => {
                    let grouped = !matches!(read.playlist_scope, PlaylistScope::All);
                    if !grouped && read.sort != CatalogSort::Title {
                        return None;
                    }
                    if *key < 0 {
                        snapshot.playlist_folders.get(&-key)?.name.to_lowercase()
                    } else if matches!(read.kind, ListKind::SmartPlaylists { .. }) {
                        snapshot.smart_playlists.get(key)?.name.to_lowercase()
                    } else {
                        snapshot
                            .text(snapshot.playlists.get(*key)?.name)
                            .to_lowercase()
                    }
                }
            })
        });
        sections(texts)
    }
}

/// A smart playlist rule's view of one track.
enum Fact<'a> {
    Text(Option<Cow<'a, str>>),
    Number(Option<i64>),
    Date(Option<String>),
}

fn date_text(seconds: u32) -> Option<String> {
    chrono::DateTime::from_timestamp(i64::from(seconds), 0)
        .map(|time| time.format("%Y-%m-%d").to_string())
}

impl Context<'_> {
    /// Plays a smart playlist field reads: all-time rules read the displayed counts, period
    /// rules the listens in the window.
    fn smart_played(
        &self,
        slot: u32,
        record: &TrackRec,
        field: crate::SmartPlaylistRuleField,
        period: Option<&HashMap<i64, Played>>,
    ) -> Played {
        use crate::SmartPlaylistRuleField as F;
        match period {
            Some(listens) if field != F::Played => listens
                .get(&i64::from(record.key))
                .copied()
                .unwrap_or_default(),
            _ if field == F::SkipCount => self.listened(slot),
            _ => self.played(slot, record),
        }
    }

    fn smart_fact(
        &self,
        slot: u32,
        record: &TrackRec,
        field: crate::SmartPlaylistRuleField,
        period: Option<&HashMap<i64, Played>>,
    ) -> Fact<'_> {
        use crate::SmartPlaylistRuleField as F;
        let snapshot = self.snapshot;
        let text = |id: StrId| Fact::Text(Some(Cow::Borrowed(snapshot.text(id))));
        match field {
            F::Title => text(record.title),
            F::Artist => text(record.artist),
            F::Album => text(record.album_title),
            F::Comment => Fact::Text(
                (record.comment != StrId::NONE)
                    .then(|| Cow::Borrowed(snapshot.text(record.comment))),
            ),
            F::Bpm => Fact::Number((record.bpm != u16::MAX).then_some(i64::from(record.bpm))),
            F::Year => Fact::Number(year_of(record.year).map(i64::from)),
            F::DateAdded => Fact::Date(known(record.added).and_then(date_text)),
            F::Favorite => Fact::Number(favorite(record.flags).map(i64::from)),
            F::Rating => Fact::Number(
                rating(record.rating, record.native_rating).map(|rating| i64::from(rating / 10)),
            ),
            F::Played | F::PlayCount => Fact::Number(Some(i64::from(
                self.smart_played(slot, record, field, period).plays,
            ))),
            F::SkipCount => Fact::Number(Some(i64::from(
                self.smart_played(slot, record, field, period).skips,
            ))),
            F::LastPlayed => Fact::Date(
                self.smart_played(slot, record, field, period)
                    .last
                    .and_then(date_text),
            ),
            F::Genre | F::Mood => unreachable!("tag rules read the track's tags"),
        }
    }

    fn smart_rule(
        &self,
        slot: u32,
        record: &TrackRec,
        rule: &crate::SmartPlaylistRule,
        period: Option<&HashMap<i64, Played>>,
    ) -> bool {
        use crate::SmartPlaylistRuleField as F;
        use crate::SmartPlaylistRuleOperator as O;
        use crate::SmartPlaylistRuleValue as V;
        if !rule.field.operators().contains(&rule.operator) {
            return false;
        }
        if matches!(rule.field, F::Genre | F::Mood) {
            let kind = if rule.field == F::Genre {
                TagKind::Genre
            } else {
                TagKind::Mood
            };
            let names = self.snapshot.track_runs(record)[2]
                .iter()
                .filter(|slot| self.tag_kind(**slot) == Some(kind))
                .map(|slot| {
                    self.snapshot
                        .text(self.snapshot.tags.records[*slot as usize].name)
                        .to_lowercase()
                })
                .collect::<Vec<_>>();
            return match (rule.operator, &rule.value) {
                (O::IsEmpty, _) => names.is_empty(),
                (O::IsNotEmpty, _) => !names.is_empty(),
                (operator, Some(V::Text(value))) => {
                    let value = value.trim().to_lowercase();
                    let matched = names.iter().any(|name| {
                        if matches!(operator, O::Contains | O::NotContains) {
                            name.contains(&value)
                        } else {
                            *name == value
                        }
                    });
                    if matches!(operator, O::NotContains | O::NotEquals) {
                        !names.is_empty() && !matched
                    } else {
                        matched
                    }
                }
                _ => false,
            };
        }
        let fact = self.smart_fact(slot, record, rule.field, period);
        if rule.field == F::Played {
            let (Some(V::Bool(value)), Fact::Number(Some(plays))) = (&rule.value, &fact) else {
                return false;
            };
            let played = *value != (rule.operator == O::IsNot);
            return (*plays > 0) == played;
        }
        if matches!(rule.operator, O::IsEmpty | O::IsNotEmpty) {
            let empty = match &fact {
                Fact::Text(text) => text.as_deref().is_none_or(|text| text.trim().is_empty()),
                Fact::Date(date) => date.as_deref().is_none_or(str::is_empty),
                Fact::Number(number) => number.is_none(),
            };
            return empty == (rule.operator == O::IsEmpty);
        }
        let compare = |order: Ordering| match rule.operator {
            O::Equals | O::Is => order == Ordering::Equal,
            O::NotEquals | O::IsNot => order != Ordering::Equal,
            O::Above | O::After => order == Ordering::Greater,
            O::Below | O::Before => order == Ordering::Less,
            _ => false,
        };
        match (&fact, &rule.value) {
            (Fact::Text(text), Some(V::Text(value))) => {
                let Some(text) = text else { return false };
                let (text, value) = (text.to_lowercase(), value.trim().to_lowercase());
                match rule.operator {
                    O::Contains => text.contains(&value),
                    O::NotContains => !text.contains(&value),
                    _ => compare(text.cmp(&value)),
                }
            }
            (Fact::Date(date), Some(V::Date(value))) => date
                .as_deref()
                .is_some_and(|date| compare(date.cmp(value.trim()))),
            (Fact::Date(date), Some(V::DateRange { start, end }))
                if rule.operator == O::Between =>
            {
                let (start, end) = (start.trim().min(end.trim()), start.trim().max(end.trim()));
                date.as_deref()
                    .is_some_and(|date| (start..=end).contains(&date))
            }
            (Fact::Number(number), Some(V::Number(value))) => {
                number.is_some_and(|number| compare(number.cmp(value)))
            }
            (Fact::Number(number), Some(V::Bool(value))) => {
                number.is_some_and(|number| compare(number.cmp(&i64::from(*value))))
            }
            (Fact::Number(number), Some(V::NumberRange { min, max }))
                if rule.operator == O::Between =>
            {
                number.is_some_and(|number| (*min.min(max)..=*min.max(max)).contains(&number))
            }
            _ => false,
        }
    }

    /// The value a smart playlist sorts its members by.
    fn smart_sort(
        &self,
        slot: u32,
        record: &TrackRec,
        definition: &SmartPlaylistDefinition,
        period: Option<&HashMap<i64, Played>>,
    ) -> Option<SortValue> {
        use crate::SmartPlaylistRuleField as F;
        use crate::SmartPlaylistSort as S;
        let snapshot = self.snapshot;
        let text = |id: StrId| Some(SortValue::Text(snapshot.text(id).to_owned()));
        match definition.sort_field {
            S::Title => Some(SortValue::Text(snapshot.track_sort(record).into_owned())),
            S::Duration => Some(SortValue::Number(i64::from(record.duration_ms))),
            S::Artist => text(record.artist),
            S::Album => text(record.album_title),
            S::Year => year_of(record.year).map(|year| SortValue::Number(i64::from(year))),
            S::DateAdded => known(record.added).map(|added| SortValue::Number(i64::from(added))),
            S::Bpm => (record.bpm != u16::MAX).then_some(SortValue::Number(i64::from(record.bpm))),
            S::Rating => rating(record.rating, record.native_rating)
                .map(|rating| SortValue::Number(i64::from(rating))),
            S::LastPlayed => self
                .smart_played(slot, record, F::LastPlayed, period)
                .last
                .map(|last| SortValue::Number(i64::from(last))),
            S::PlayCount => Some(SortValue::Number(i64::from(
                self.smart_played(slot, record, F::PlayCount, period).plays,
            ))),
            S::SkipCount => Some(SortValue::Number(i64::from(
                self.smart_played(slot, record, F::SkipCount, period).skips,
            ))),
        }
    }

    /// A smart playlist's members in its own order, cut to its limit.
    pub(crate) fn evaluate(
        &self,
        definition: &SmartPlaylistDefinition,
        inputs: &Inputs,
    ) -> Vec<u32> {
        let snapshot = self.snapshot;
        let source = match &definition.source_id {
            Some(id) => match snapshot
                .sources
                .values()
                .find(|source| source.object == id.as_str())
                .and_then(|source| snapshot.source_slot(source.key))
            {
                Some(slot) => Some(slot),
                None => return Vec::new(),
            },
            None => None,
        };
        let period = definition
            .activity_period
            .seconds()
            .and_then(|seconds| inputs.periods.get(&Some(seconds)));
        let mut members = snapshot
            .tracks
            .iter()
            .filter(|(_, record)| {
                record.indexed() && source.is_none_or(|source| record.source == source)
            })
            .filter(|(slot, record)| {
                definition
                    .match_all
                    .iter()
                    .all(|rule| self.smart_rule(*slot, record, rule, period))
                    && (definition.match_any.is_empty()
                        || definition
                            .match_any
                            .iter()
                            .any(|rule| self.smart_rule(*slot, record, rule, period)))
            })
            .map(|(slot, record)| {
                (
                    self.smart_sort(slot, record, definition, period),
                    snapshot.track_sort(record),
                    record.key,
                    slot,
                )
            })
            .collect::<Vec<_>>();
        members.sort_by(|a, b| {
            nulls_last(a.0.as_ref(), b.0.as_ref(), definition.descending)
                .then_with(|| a.1.cmp(&b.1))
                .then(a.2.cmp(&b.2))
        });
        if let Some(limit) = definition.limit {
            members.truncate(limit);
        }
        members.into_iter().map(|member| member.3).collect()
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum SortValue {
    Number(i64),
    Text(String),
}

/// The listen windows a smart playlist reads: None when its rules need none.
pub(crate) fn smart_period(definition: &SmartPlaylistDefinition) -> Option<i64> {
    use crate::SmartPlaylistRuleField as F;
    use crate::SmartPlaylistSort as S;
    let seconds = definition.activity_period.seconds()?;
    let sorted = matches!(
        definition.sort_field,
        S::PlayCount | S::SkipCount | S::LastPlayed
    );
    let ruled = definition
        .match_all
        .iter()
        .chain(&definition.match_any)
        .any(|rule| matches!(rule.field, F::PlayCount | F::SkipCount | F::LastPlayed));
    (sorted || ruled).then_some(seconds)
}

/// Orders, totals and other values built from one revision of the Catalog, kept while that
/// revision lasts and the order budget allows.
#[derive(Default)]
pub(crate) struct Views {
    derived: Vec<BuiltDerived>,
    /// Most recently used last.
    orders: Vec<CachedOrder>,
    /// Members of the smart playlists pages opened most recently, oldest first.
    opened: Vec<OpenedSmart>,
}

/// A smart playlist's members as of the time a page opened it. The page keeps that time, so
/// plays and syncs while it stays open don't move its rows; the next visit evaluates again.
struct OpenedSmart {
    key: i64,
    now: i64,
    definition: SmartPlaylistDefinition,
    tracks: Arc<Vec<i64>>,
}

const OPENED_SMART: usize = 8;

struct BuiltDerived {
    revision: u64,
    facts: u64,
    folder: Option<u32>,
    derived: Arc<Derived>,
}

struct CachedOrder {
    key: String,
    revision: u64,
    bytes: usize,
    value: Arc<dyn Any + Send + Sync>,
}

impl Catalog {
    fn views(&self) -> std::sync::MutexGuard<'_, Views> {
        self.views_lock()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Totals and borrowed pictures of the current revision, scoped to a music folder.
    pub(crate) fn derived(&self, context: &Context<'_>, folder: Option<u32>) -> Arc<Derived> {
        let revision = context.snapshot.revision;
        self.derive(context, folder, |built| {
            built.revision == revision && built.folder == folder
        })
    }

    /// Borrowed pictures and whether an album lists a track. They change only with catalog
    /// records, so a row read after a favorite or a listen reuses an older revision's build.
    pub(crate) fn covers(&self, context: &Context<'_>) -> Arc<Derived> {
        let facts = context.snapshot.facts;
        self.derive(context, None, |built| {
            built.facts == facts && built.folder.is_none()
        })
    }

    fn derive(
        &self,
        context: &Context<'_>,
        folder: Option<u32>,
        reuse: impl Fn(&BuiltDerived) -> bool,
    ) -> Arc<Derived> {
        let find = || {
            self.views()
                .derived
                .iter()
                .find(|built| reuse(built))
                .map(|built| built.derived.clone())
        };
        if let Some(derived) = find() {
            return derived;
        }
        let _building = self.deriving();
        if let Some(derived) = find() {
            return derived;
        }
        let derived = Arc::new(Derived::build(context, folder));
        let (revision, facts) = (context.snapshot.revision, context.snapshot.facts);
        let mut views = self.views();
        views.derived.retain(|built| {
            built.facts == facts && (built.revision == revision || built.folder.is_none())
        });
        if views.derived.len() >= 4 {
            views.derived.remove(0);
        }
        views.derived.push(BuiltDerived {
            revision,
            facts,
            folder,
            derived: derived.clone(),
        });
        derived
    }

    fn cached<T: Send + Sync + 'static>(&self, key: &str, revision: u64) -> Option<Arc<T>> {
        let mut views = self.views();
        let index = views
            .orders
            .iter()
            .position(|order| order.key == key && order.revision == revision)?;
        let order = views.orders.remove(index);
        let value = order.value.clone().downcast::<T>().ok();
        views.orders.push(order);
        value
    }

    fn cache<T: Send + Sync + 'static>(
        &self,
        key: String,
        revision: u64,
        bytes: usize,
        value: Arc<T>,
    ) {
        let mut views = self.views();
        views
            .orders
            .retain(|order| order.revision == revision && order.key != key);
        views.orders.push(CachedOrder {
            key,
            revision,
            bytes,
            value,
        });
        let mut total = views.orders.iter().map(|order| order.bytes).sum::<usize>();
        while total > ORDER_BUDGET && views.orders.len() > 1 {
            total -= views.orders.remove(0).bytes;
        }
    }

    /// Facts a member read needs from the Store before it can order.
    async fn member_inputs(
        &self,
        connection: &mut SqliteConnection,
        read: &MemberRead,
    ) -> LibraryResult<Inputs> {
        let mut inputs = Inputs::default();
        match &read.collection {
            Collection::Objects { source, ids } => {
                inputs.objects = self.track_keys_by_objects(connection, *source, ids).await?;
            }
            Collection::Smart { key, now } => {
                let definition = self
                    .snapshot()
                    .smart_playlists
                    .get(&key.raw())
                    .map(|smart| smart.definition.clone());
                if let Some(definition) = definition {
                    self.read_periods(
                        connection,
                        &mut inputs,
                        smart_period(&definition).into_iter().collect(),
                        *now,
                    )
                    .await?;
                    let tracks = match self.opened_smart(key.raw(), *now, &definition) {
                        Some(tracks) => tracks,
                        None => {
                            let snapshot = self.snapshot();
                            let tracks = Arc::new(
                                Context::new(&snapshot, false)
                                    .evaluate(&definition, &inputs)
                                    .into_iter()
                                    .filter_map(|slot| snapshot.tracks.key_of(slot))
                                    .collect::<Vec<_>>(),
                            );
                            let mut views = self.views();
                            views
                                .opened
                                .retain(|opened| opened.key != key.raw() || opened.now != *now);
                            views.opened.push(OpenedSmart {
                                key: key.raw(),
                                now: *now,
                                definition,
                                tracks: tracks.clone(),
                            });
                            if views.opened.len() > OPENED_SMART {
                                views.opened.remove(0);
                            }
                            tracks
                        }
                    };
                    inputs.smart = Some(tracks);
                }
            }
            _ => {}
        }
        Ok(inputs)
    }

    /// Facts a list read needs from the Store before it can order.
    async fn list_inputs(
        &self,
        connection: &mut SqliteConnection,
        read: &ListRead,
    ) -> LibraryResult<Inputs> {
        let mut inputs = Inputs::default();
        match &read.kind {
            ListKind::ArtistAlbums {
                artist,
                album_artist,
                ..
            } => {
                let albums = {
                    let snapshot = self.snapshot();
                    let context = Context::new(&snapshot, false);
                    let folder = context.folder_slot(read.folder).flatten();
                    match snapshot.artists.slot(artist.raw()) {
                        Some(artist) => context
                            .artist_albums(artist, *album_artist, folder)
                            .into_iter()
                            .map(|slot| &snapshot.albums.records[slot as usize])
                            .map(|album| (i64::from(album.key), album.flags & COMPILATION != 0))
                            .collect::<Vec<_>>(),
                        None => Vec::new(),
                    }
                };
                inputs.classes = self.release_classes(connection, &albums).await?;
            }
            ListKind::SmartPlaylists { now } => {
                let definitions = self
                    .snapshot()
                    .smart_playlists
                    .values()
                    .map(|smart| smart.definition.clone())
                    .collect::<Vec<_>>();
                let periods = definitions.iter().filter_map(smart_period).collect();
                self.read_periods(connection, &mut inputs, periods, *now)
                    .await?;
            }
            _ => {}
        }
        Ok(inputs)
    }

    /// The members a page that opened this smart playlist at `now` holds, while its rules
    /// are unchanged.
    pub(crate) fn opened_smart(
        &self,
        key: i64,
        now: i64,
        definition: &SmartPlaylistDefinition,
    ) -> Option<Arc<Vec<i64>>> {
        self.views()
            .opened
            .iter()
            .find(|opened| {
                opened.key == key && opened.now == now && opened.definition == *definition
            })
            .map(|opened| opened.tracks.clone())
    }

    /// Listens in the windows these smart playlists read, by track.
    pub(crate) async fn read_periods(
        &self,
        connection: &mut SqliteConnection,
        inputs: &mut Inputs,
        periods: Vec<i64>,
        now: i64,
    ) -> LibraryResult<()> {
        for seconds in periods {
            if inputs.periods.contains_key(&Some(seconds)) {
                continue;
            }
            let rows = sqlx::query_as::<_, (i64, i64, i64, i64)>(
                "SELECT track.track_key,sum(listen.skipped=0),sum(listen.skipped),max(listen.started_at) FROM listens listen JOIN tracks track USING(media_uri) WHERE listen.started_at<=?1 AND listen.started_at>=?2 GROUP BY track.track_key",
            )
            .bind(now)
            .bind(now - seconds)
            .fetch_all(&mut *connection)
            .await?;
            inputs.periods.insert(
                Some(seconds),
                rows.into_iter()
                    .map(|(key, plays, skips, last)| {
                        (
                            key,
                            Played {
                                plays: plays.clamp(0, i64::from(u32::MAX)) as u32,
                                skips: skips.clamp(0, i64::from(u32::MAX)) as u32,
                                last: u32::try_from(last).ok(),
                            },
                        )
                    })
                    .collect(),
            );
        }
        Ok(())
    }

    /// Release classes of albums from the release types their metadata or a lookup holds.
    pub(crate) async fn release_classes(
        &self,
        connection: &mut SqliteConnection,
        albums: &[(i64, bool)],
    ) -> LibraryResult<HashMap<i64, AlbumReleaseClass>> {
        let mut classes = HashMap::new();
        for chunk in albums.chunks(512) {
            let keys = chunk.iter().map(|(key, _)| *key).collect::<Vec<_>>();
            let rows = sqlx::query_as::<_, (i64, Option<String>)>(
                "SELECT album.album_key,coalesce((SELECT json(accepted.release_types) FROM album_release_observations accepted WHERE accepted.album_key=album.album_key AND accepted.lookup_identity=album.release_lookup_identity),json_extract(album.metadata,'$.release_types')) FROM json_each(?1) requested JOIN albums album ON album.album_key=requested.value",
            )
            .bind(serde_json::to_string(&keys)?)
            .fetch_all(&mut *connection)
            .await?;
            let types = rows.into_iter().collect::<HashMap<_, _>>();
            for (key, compilation) in chunk {
                let release_types = types
                    .get(key)
                    .and_then(Option::as_deref)
                    .map(serde_json::from_str::<Vec<String>>)
                    .transpose()?
                    .unwrap_or_default();
                classes.insert(*key, release_class(*compilation, &release_types));
            }
        }
        Ok(classes)
    }

    /// Track keys of a source's object ids, in request order.
    pub(crate) async fn track_keys_by_objects(
        &self,
        connection: &mut SqliteConnection,
        source: SourceKey,
        ids: &[String],
    ) -> LibraryResult<Vec<i64>> {
        Ok(sqlx::query_scalar("SELECT track.track_key FROM json_each(?1) requested JOIN tracks track ON track.source_key=?2 AND track.object_id=requested.value ORDER BY CAST(requested.key AS INTEGER)")
            .bind(serde_json::to_string(ids)?)
            .bind(source)
            .fetch_all(&mut *connection)
            .await?)
    }

    /// A member read's rows in order, from the cache while the Catalog is unchanged.
    pub(crate) async fn member_order(
        &self,
        connection: &mut SqliteConnection,
        read: &MemberRead,
    ) -> LibraryResult<Arc<Vec<Member>>> {
        self.loaded().await;
        let key = format!("{read:?}");
        if let Some(order) = self.cached::<Vec<Member>>(&key, self.snapshot().revision) {
            return Ok(order);
        }
        let inputs = self.member_inputs(connection, read).await?;
        let snapshot = self.snapshot();
        let context = Context::new(&snapshot, self.distinct_track_covers());
        let order = Arc::new(context.member_order(read, &inputs));
        self.cache(key, snapshot.revision, order.len() * 16, order.clone());
        Ok(order)
    }

    /// A list read's keys in order, from the cache while the Catalog is unchanged.
    pub(crate) async fn list_order(
        &self,
        connection: &mut SqliteConnection,
        read: &ListRead,
    ) -> LibraryResult<Arc<Vec<i64>>> {
        self.loaded().await;
        let key = format!("{read:?}");
        if let Some(order) = self.cached::<Vec<i64>>(&key, self.snapshot().revision) {
            return Ok(order);
        }
        let inputs = self.list_inputs(connection, read).await?;
        let snapshot = self.snapshot();
        let context = Context::new(&snapshot, self.distinct_track_covers());
        let folder = context.folder_slot(read.folder).flatten();
        let derived = self.derived(&context, folder);
        let order = Arc::new(context.list_order(read, &derived, &inputs));
        self.cache(key, snapshot.revision, order.len() * 8, order.clone());
        Ok(order)
    }
}

/// A row whose picture a reader asks for, by key.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Owner {
    Track(i64),
    Album(i64),
    Artist(i64),
}

impl Catalog {
    /// Where the pictures of these rows come from. A row the Catalog does not hold has none.
    pub(crate) fn arts(&self, owners: impl IntoIterator<Item = Option<Owner>>) -> Vec<Art> {
        let snapshot = self.snapshot();
        let context = Context::new(&snapshot, self.distinct_track_covers());
        let derived = self.covers(&context);
        owners
            .into_iter()
            .map(|owner| match owner {
                Some(Owner::Track(key)) => snapshot
                    .tracks
                    .get(key)
                    .map_or(Art::None, |record| context.track_art(&derived, record)),
                Some(Owner::Album(key)) => snapshot
                    .albums
                    .slot(key)
                    .map_or(Art::None, |slot| context.album_art(&derived, slot)),
                Some(Owner::Artist(key)) => snapshot
                    .artists
                    .slot(key)
                    .map_or(Art::None, |slot| context.artist_art(&derived, slot)),
                None => Art::None,
            })
            .collect()
    }

    /// Track and entry keys of an order's rows. A row removed since the order was built is
    /// skipped.
    pub(crate) fn member_keys(
        &self,
        order: &[Member],
    ) -> Vec<(crate::TrackKey, Option<crate::PlaylistEntryKey>)> {
        let snapshot = self.snapshot();
        order
            .iter()
            .filter_map(|member| {
                Some((
                    crate::TrackKey::from_raw(snapshot.tracks.key_of(member.track)?),
                    (member.entry != 0).then(|| crate::PlaylistEntryKey::from_raw(member.entry)),
                ))
            })
            .collect()
    }

    /// A collection's members in its own order.
    pub(crate) async fn members(
        &self,
        connection: &mut SqliteConnection,
        collection: Collection,
    ) -> LibraryResult<Vec<crate::TrackKey>> {
        let order = self
            .member_order(connection, &MemberRead::new(collection))
            .await?;
        Ok(self
            .member_keys(&order)
            .into_iter()
            .map(|(key, _)| key)
            .collect())
    }
}

impl Context<'_> {
    /// Named child folders of a folder, or the top folders of a source, that hold a visible
    /// track.
    pub(crate) fn folder_children(&self, source: SourceKey, parent: Option<u32>) -> Vec<u32> {
        let Some(owner) = self.source_slot(source) else {
            return Vec::new();
        };
        let mut children = HashSet::new();
        for (_, record) in self.snapshot.tracks.iter() {
            if record.source != owner || !record.indexed() {
                continue;
            }
            let path = self.snapshot.track_runs(record)[3];
            let child = match parent {
                None => path.first(),
                Some(parent) => path
                    .iter()
                    .position(|folder| *folder == parent)
                    .and_then(|index| path.get(index + 1)),
            };
            if let Some(child) = child {
                children.insert(*child);
            }
        }
        children
            .into_iter()
            .filter(|slot| {
                self.snapshot
                    .folders
                    .live(*slot)
                    .is_some_and(|folder| folder.indexed() && folder.name != StrId::NONE)
            })
            .collect()
    }

    /// A folder list ordered by sort text and filtered by name.
    pub(crate) fn folder_order(
        &self,
        source: SourceKey,
        parent: Option<u32>,
        filter: &str,
        descending: bool,
    ) -> Vec<(i64, String)> {
        let filter = filter.trim().to_lowercase();
        let mut folders = self
            .folder_children(source, parent)
            .into_iter()
            .map(|slot| &self.snapshot.folders.records[slot as usize])
            .filter(|folder| contains(self.snapshot.text(folder.name), &filter))
            .map(|folder| {
                (
                    i64::from(folder.key),
                    self.snapshot
                        .name_sort(folder.name, folder.sort)
                        .into_owned(),
                )
            })
            .collect::<Vec<_>>();
        folders.sort_by(|a, b| directed(a.1.cmp(&b.1), descending).then(a.0.cmp(&b.0)));
        folders
    }

    /// The visible tracks of these albums in disc order, within a music folder, from one pass
    /// over the tracks.
    pub(crate) fn album_tracks(
        &self,
        albums: &[u32],
        folder: Option<u32>,
    ) -> HashMap<u32, Vec<u32>> {
        let snapshot = self.snapshot;
        let wanted = albums.iter().copied().collect::<HashSet<_>>();
        let mut members = HashMap::<u32, Vec<_>>::new();
        for (slot, record) in snapshot.tracks.iter() {
            if wanted.contains(&record.album) && record.indexed() && self.in_folder(record, folder)
            {
                members.entry(record.album).or_default().push((
                    record.disc,
                    record.number,
                    snapshot.track_sort(record),
                    record.key,
                    slot,
                ));
            }
        }
        members
            .into_iter()
            .map(|(album, mut tracks)| {
                tracks.sort();
                (album, tracks.into_iter().map(|track| track.4).collect())
            })
            .collect()
    }
}

impl Context<'_> {
    /// The members of each parent of a list, parent by parent: an album's tracks in disc order,
    /// an artist's, genre's or mood's by title, a playlist's entries in place.
    pub(crate) fn parent_members(
        &self,
        kind: &ListKind,
        parents: &[i64],
        inputs: &Inputs,
    ) -> Vec<Member> {
        let snapshot = self.snapshot;
        let member = |track| Member { track, entry: 0 };
        match kind {
            ListKind::Playlists => {
                return parents
                    .iter()
                    .filter_map(|key| snapshot.playlists.get(*key))
                    .flat_map(|playlist| {
                        playlist
                            .entries
                            .iter()
                            .filter(|entry| snapshot.tracks.live(entry.track).is_some())
                            .map(|entry| Member {
                                track: entry.track,
                                entry: entry.key,
                            })
                    })
                    .collect();
            }
            ListKind::SmartPlaylists { .. } => {
                return parents
                    .iter()
                    .filter_map(|key| snapshot.smart_playlists.get(key))
                    .flat_map(|smart| self.evaluate(&smart.definition, inputs))
                    .map(member)
                    .collect();
            }
            _ => {}
        }
        let (slots, sort) = match kind {
            ListKind::Albums | ListKind::ArtistAlbums { .. } => (
                parents
                    .iter()
                    .map(|key| snapshot.albums.slot(*key))
                    .collect::<Vec<_>>(),
                CatalogSort::TrackNumber,
            ),
            ListKind::Artists { .. } => (
                parents
                    .iter()
                    .map(|key| snapshot.artists.slot(*key))
                    .collect(),
                CatalogSort::Title,
            ),
            _ => (
                parents.iter().map(|key| snapshot.tags.slot(*key)).collect(),
                CatalogSort::Title,
            ),
        };
        let mut index = HashMap::<u32, Vec<usize>>::new();
        for (position, slot) in slots.iter().enumerate() {
            if let Some(slot) = slot {
                index.entry(*slot).or_default().push(position);
            }
        }
        let mut buckets = vec![Vec::new(); parents.len()];
        let mut owners = Vec::new();
        for (slot, record) in snapshot.tracks.iter() {
            if !record.indexed() {
                continue;
            }
            owners.clear();
            match kind {
                ListKind::Albums | ListKind::ArtistAlbums { .. } => {
                    owners.extend(self.album_of(record));
                }
                ListKind::Artists {
                    album_artist: false,
                } => owners.extend(dedup(snapshot.track_runs(record)[0])),
                ListKind::Artists { album_artist: true } => {
                    owners.extend(dedup(self.album_artists(record)));
                }
                ListKind::Genres => self.tags_of(record, &mut owners),
                _ => owners.extend(dedup(snapshot.track_runs(record)[2])),
            }
            for owner in &owners {
                for position in index.get(owner).into_iter().flatten() {
                    buckets[*position].push(member(slot));
                }
            }
        }
        buckets
            .into_iter()
            .flat_map(|bucket| {
                let mut rows = bucket
                    .into_iter()
                    .enumerate()
                    .map(|(position, member)| self.track_row(sort, position, member))
                    .collect::<Vec<_>>();
                rows.sort_by(|a, b| compare_tracks(sort, false, a, b));
                rows.into_iter().map(|row| row.member).collect::<Vec<_>>()
            })
            .collect()
    }
}

impl Catalog {
    /// Members of a list's parents, in list order.
    pub(crate) async fn parent_order(
        &self,
        connection: &mut SqliteConnection,
        read: &ListRead,
        parents: &[i64],
    ) -> LibraryResult<Vec<Member>> {
        let mut inputs = Inputs::default();
        if let ListKind::SmartPlaylists { now } = read.kind {
            let definitions = {
                let snapshot = self.snapshot();
                parents
                    .iter()
                    .filter_map(|key| snapshot.smart_playlists.get(key))
                    .map(|smart| smart.definition.clone())
                    .collect::<Vec<_>>()
            };
            let periods = definitions.iter().filter_map(smart_period).collect();
            self.read_periods(connection, &mut inputs, periods, now)
                .await?;
        }
        let snapshot = self.snapshot();
        Ok(Context::new(&snapshot, false).parent_members(&read.kind, parents, &inputs))
    }
}
