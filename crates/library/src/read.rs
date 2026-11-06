//! What a list asks and what it gets: a `Read` names a list, its `Order` comes from the
//! resident Catalog, and the rows of a window are built from residents and one keyed statement.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use crate::catalog::fetch::{self, Got, Part, Want};
use crate::catalog::snapshot::{
    COMPILATION, CUE, DOWNLOADED, METADATA_WRITABLE, PICTURE, Packed, SOURCE_OWNED, Snapshot,
    WRITABLE,
};
use crate::catalog::strings::StrId;
use crate::catalog::views::{
    Activity, Art, Context, Derived, Downloaded, Inputs, Played, Totaled, Totals, date_text,
    favorite, known, rating, year_of,
};
use crate::*;

/// A list a client opens.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Read {
    Members(MemberRead),
    List(ListRead),
    /// The Albums list in Detail layout: each album, then its tracks.
    AlbumTracks(ListRead),
    /// Cached child folders of a source folder.
    Folders(FolderRead),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct FolderRead {
    pub source: SourceKey,
    pub parent: Option<FolderKey>,
    pub filter: String,
    pub descending: bool,
}

/// An order from the cache. Cheap to clone; a view asks the cache again on each call rather
/// than holding one, so it never reads an order the Catalog moved past.
#[derive(Clone)]
pub struct Order(pub(crate) Arc<OrderInner>);

pub(crate) struct OrderInner {
    pub(crate) read: Read,
    /// The stamps it was built at, those it does not depend on zeroed.
    pub(crate) stamp: [u64; 5],
    pub(crate) members: Members,
    /// Albums of an artist by release class, counted at build.
    pub(crate) counts: Option<[usize; 6]>,
}

/// An order's rows. Keys are SQLite rowids, which fit in 32 bits like the records' own.
pub(crate) enum Members {
    Tracks(Box<[u32]>),
    /// A playlist's entries: track and entry keys and the entry's index in the playlist.
    Entries(Box<[(u32, u32, u32)]>),
    History(Box<[Listen]>),
    /// Albums, artists, genres, moods or folders.
    Keys(Box<[u32]>),
    /// Playlists or smart playlists, with their folders as negative keys.
    Playlists(Box<[i64]>),
    AlbumTracks {
        spans: Arc<[AlbumSpan]>,
        tracks: Box<[u32]>,
    },
}

/// One listen of a History order, with its track while one is left.
#[derive(Clone, Debug)]
pub(crate) struct Listen {
    pub(crate) track: Option<TrackKey>,
    pub(crate) copy: ListenWrite,
}

/// An album of an AlbumTracks order: its heading's position and its visible track count.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlbumSpan {
    pub album: AlbumKey,
    pub first: usize,
    pub count: usize,
    pub genres: bool,
}

impl OrderInner {
    /// Bytes the order cache counts against its budget: the entry, its rows and the read that
    /// keys it, so empty orders and live folder ids count too.
    pub(crate) fn bytes(&self) -> usize {
        let texts = |texts: &[String]| {
            texts
                .iter()
                .map(|text| size_of::<String>() + text.len())
                .sum::<usize>()
        };
        let read = match &self.read {
            Read::Members(read) => {
                read.filter.len()
                    + match &read.collection {
                        Collection::Objects { ids, .. } => texts(ids),
                        _ => 0,
                    }
            }
            Read::List(read) | Read::AlbumTracks(read) => {
                read.filter.len()
                    + match &read.kind {
                        ListKind::ArtistAlbums {
                            filters: Some(filters),
                            ..
                        } => texts(filters.as_slice()),
                        _ => 0,
                    }
            }
            Read::Folders(read) => read.filter.len(),
        };
        size_of::<Self>() + read + self.members.bytes()
    }
}

impl Members {
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Tracks(keys) | Self::Keys(keys) => keys.len(),
            Self::Entries(entries) => entries.len(),
            Self::History(listens) => listens.len(),
            Self::Playlists(keys) => keys.len(),
            Self::AlbumTracks { spans, tracks } => spans.len() + tracks.len(),
        }
    }

    /// Bytes of the rows.
    fn bytes(&self) -> usize {
        match self {
            Self::Tracks(keys) | Self::Keys(keys) => keys.len() * 4,
            Self::Entries(entries) => entries.len() * 12,
            Self::History(listens) => listens.len() * 512,
            Self::Playlists(keys) => keys.len() * 8,
            Self::AlbumTracks { spans, tracks } => {
                spans.len() * std::mem::size_of::<AlbumSpan>() + tracks.len() * 4
            }
        }
    }
}

impl Order {
    pub(crate) fn read(&self) -> &Read {
        &self.0.read
    }

    pub fn len(&self) -> usize {
        self.0.members.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The items at these positions: anchors, drags and plays by index.
    pub fn items(&self, range: Range<usize>) -> Vec<Item> {
        let range = range.start.min(self.len())..range.end.min(self.len());
        let key = |key: &u32| i64::from(*key);
        match &self.0.members {
            Members::Tracks(keys) => keys[range]
                .iter()
                .map(|track| Item::Track {
                    key: TrackKey::from_raw(key(track)),
                    entry: None,
                })
                .collect(),
            Members::Entries(entries) => entries[range]
                .iter()
                .map(|(track, entry, _)| Item::Track {
                    key: TrackKey::from_raw(key(track)),
                    entry: Some(PlaylistEntryKey::from_raw(key(entry))),
                })
                .collect(),
            Members::History(_) => range.map(|index| Item::Listen(index as u32)).collect(),
            Members::Keys(keys) => keys[range]
                .iter()
                .map(|raw| {
                    let raw = key(raw);
                    match &self.0.read {
                        Read::Folders(_) => Item::Folder(FolderKey::from_raw(raw)),
                        Read::List(read) | Read::AlbumTracks(read) => match read.kind {
                            ListKind::Artists { album_artist } => Item::Artist {
                                key: ArtistKey::from_raw(raw),
                                album_artist,
                            },
                            ListKind::Genres => Item::Genre(GenreKey::from_raw(raw)),
                            ListKind::Moods => Item::Mood(MoodKey::from_raw(raw)),
                            _ => Item::Album(AlbumKey::from_raw(raw)),
                        },
                        Read::Members(_) => Item::Album(AlbumKey::from_raw(raw)),
                    }
                })
                .collect(),
            Members::Playlists(keys) => {
                let smart = matches!(
                    &self.0.read,
                    Read::List(ListRead {
                        kind: ListKind::SmartPlaylists { .. },
                        ..
                    })
                );
                keys[range]
                    .iter()
                    .map(|key| match (*key < 0, smart) {
                        (true, _) => Item::PlaylistFolder(PlaylistFolderKey::from_raw(-key)),
                        (false, true) => Item::Smart(SmartPlaylistKey::from_raw(*key)),
                        (false, false) => Item::Playlist(PlaylistKey::from_raw(*key)),
                    })
                    .collect()
            }
            Members::AlbumTracks { spans, tracks } => range
                .map(|position| {
                    let at = spans.partition_point(|span| span.first <= position) - 1;
                    let span = spans[at];
                    if position == span.first {
                        return Item::AlbumHeading(span.album);
                    }
                    let index = position - span.first - 1;
                    Item::AlbumTrack {
                        key: TrackKey::from_raw(key(&tracks[span.first - at + index])),
                        album: span.album,
                        index: index as u32,
                    }
                })
                .collect(),
        }
    }

    /// Each entry's index in its playlist, for the entries at these positions.
    fn positions(&self, range: Range<usize>) -> HashMap<i64, u32> {
        let range = range.start.min(self.len())..range.end.min(self.len());
        match &self.0.members {
            Members::Entries(entries) => entries[range]
                .iter()
                .map(|(_, entry, position)| (i64::from(*entry), *position))
                .collect(),
            _ => HashMap::new(),
        }
    }

    /// An artist's albums by release class, for ArtistAlbums orders.
    pub fn release_counts(&self) -> Option<[usize; 6]> {
        self.0.counts
    }

    pub fn album_spans(&self) -> Option<&Arc<[AlbumSpan]>> {
        match &self.0.members {
            Members::AlbumTracks { spans, .. } => Some(spans),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Item {
    Track {
        key: TrackKey,
        entry: Option<PlaylistEntryKey>,
    },
    AlbumTrack {
        key: TrackKey,
        album: AlbumKey,
        index: u32,
    },
    AlbumHeading(AlbumKey),
    Album(AlbumKey),
    Artist {
        key: ArtistKey,
        album_artist: bool,
    },
    Genre(GenreKey),
    Mood(MoodKey),
    Playlist(PlaylistKey),
    Smart(SmartPlaylistKey),
    PlaylistFolder(PlaylistFolderKey),
    Folder(FolderKey),
    /// The n-th listen of a History order.
    Listen(u32),
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum Row {
    Track(TrackRow),
    AlbumHeading(AlbumRow),
    AlbumTrack { track: TrackRow, index: u32 },
    Album(AlbumRow),
    Artist(ArtistRow),
    Tag(TagRow),
    Playlist(PlaylistListRow),
    Folder(FolderRow),
}

/// A playlist row's entry: its key and its index in the playlist's order.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PlaylistEntry {
    pub key: PlaylistEntryKey,
    pub position: u32,
}

/// A track as a list row shows it.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TrackRow {
    pub track_key: TrackKey,
    pub entry: Option<PlaylistEntry>,
    pub source_key: Option<SourceKey>,
    pub source_id: Option<String>,
    pub source_name: String,
    pub media_uri: String,
    pub object_id: Option<String>,
    /// An empty title reads as the URI.
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_millis: i64,
    pub disc_number: i64,
    pub track_number: i64,
    pub year: Option<i64>,
    pub bpm: Option<i64>,
    /// Unix seconds; shown as its UTC date.
    pub date_added: Option<i64>,
    pub source_format: Option<String>,
    pub state: TrackState,
    pub is_downloaded: bool,
    pub cue: bool,
    pub links: TrackLinks,
    /// As stored, such as "2020" or "01.03.2020".
    pub release_date: Option<String>,
    pub source_path: Option<String>,
    pub audio: AudioProperties,
    pub artwork_binding: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AlbumRow {
    pub album_key: AlbumKey,
    pub source_key: SourceKey,
    pub object_id: String,
    pub media_uri: String,
    pub title: String,
    pub artist: String,
    pub sort_text: String,
    pub year: Option<i64>,
    pub release_date: Option<String>,
    /// As stored when it is no date.
    pub date_added: Option<String>,
    pub is_compilation: bool,
    pub release_class: AlbumReleaseClass,
    pub state: TrackState,
    pub summary: CollectionSummary,
    pub links: AlbumLinks,
    pub artwork_binding: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ArtistRow {
    pub artist_key: ArtistKey,
    pub album_artist: bool,
    pub source_key: SourceKey,
    pub object_id: String,
    pub media_uri: String,
    pub name: String,
    pub sort_text: String,
    pub state: TrackState,
    pub summary: CollectionSummary,
    pub artwork_binding: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TagRow {
    pub key: i64,
    pub kind: TagKind,
    pub source_key: SourceKey,
    pub source_id: String,
    pub object_id: String,
    pub media_uri: String,
    pub name: String,
    pub summary: CollectionSummary,
    pub artwork_binding: Option<Vec<u8>>,
    /// Covers of the tracks its mosaic picked, when it has no picture of its own.
    pub representative_artwork: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct FolderRow {
    /// None for a folder from a live listing.
    pub folder_key: Option<FolderKey>,
    pub object_id: String,
    pub name: String,
    pub summary: CollectionSummary,
    pub artwork_binding: Option<Vec<u8>>,
}

/// One keyed statement: the window's rows, a detail header and an artist's favorite tracks.
#[derive(Default)]
pub struct Fetch<'a> {
    pub rows: Option<(&'a Order, Range<usize>)>,
    pub favorites: Option<(&'a Order, Range<usize>)>,
    pub header: Option<HeaderKey>,
    pub art: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeaderKey {
    Album {
        key: AlbumKey,
        activity: bool,
    },
    Artist {
        key: ArtistKey,
        album_artist: bool,
        activity: bool,
    },
    Playlist(PlaylistKey),
    /// A smart playlist counted as of `now`. Headers count in the music folder their page's
    /// rows read.
    Smart {
        key: SmartPlaylistKey,
        now: i64,
    },
    Genre(GenreKey),
    Mood(MoodKey),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Header {
    Album(Box<AlbumView>),
    Artist(Box<ArtistView>),
    Playlist(Box<PlaylistRow>),
    Smart(Box<SmartPlaylistRow>),
    Tag(TagRow),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Fetched {
    pub header: Option<Header>,
    pub rows: Vec<Row>,
    pub favorites: Vec<Row>,
}

/// A window is at most this many rows; an artist's albums at most half.
const WINDOW: usize = 256;

/// What rows show besides their own record: plays when a list asks for them, the list a
/// playlist folder's totals come from, the time and music folder smart playlists are counted
/// at, and each entry row's index in its playlist.
#[derive(Clone)]
pub(crate) struct Shape {
    pub(crate) activity: bool,
    pub(crate) now: i64,
    pub(crate) folder: Option<FolderKey>,
    pub(crate) list: Option<ListRead>,
    pub(crate) positions: HashMap<i64, u32>,
}

impl Shape {
    pub(crate) fn of(read: Option<&Read>) -> Self {
        let mut shape = Self {
            activity: false,
            now: chrono::Utc::now().timestamp(),
            folder: None,
            list: None,
            positions: HashMap::new(),
        };
        match read {
            Some(Read::List(read) | Read::AlbumTracks(read)) => {
                shape.activity = read.activity;
                shape.folder = read.folder;
                if let ListKind::SmartPlaylists { now } = read.kind {
                    shape.now = now;
                }
                shape.list = Some(read.clone());
            }
            Some(Read::Members(read)) => {
                shape.activity = read.activity;
                shape.folder = read.folder;
            }
            _ => {}
        }
        shape
    }
}

fn text(snapshot: &Snapshot, id: StrId) -> String {
    snapshot.text(id).to_owned()
}

fn optional(snapshot: &Snapshot, id: StrId) -> Option<String> {
    (id != StrId::NONE).then(|| text(snapshot, id))
}

/// A collection's URI, built from its source and object id as every writer builds it.
fn entity_uri(snapshot: &Snapshot, source: u16, kind: &str, object: StrId) -> String {
    snapshot
        .source_key(source)
        .and_then(|key| snapshot.source(key))
        .map(|source| {
            source_entity_uri(
                &SourceId::new(source.object.clone()),
                kind,
                snapshot.text(object),
            )
        })
        .unwrap_or_default()
}

/// A stored rating on the 0-10 scale rows show it in.
pub(crate) fn shown_rating(rating: Option<u8>) -> Option<i64> {
    rating.map(|rating| i64::from(rating / 10))
}

/// The state a row shows: its favorite and rating, user's first, and its plays.
fn state(flags: u16, user: u8, native: u8, played: Played) -> TrackState {
    TrackState {
        favorite: favorite(flags),
        rating: shown_rating(rating(user, native)),
        last_played: played.last.map(i64::from),
        play_count: i64::from(played.plays),
        skip_count: i64::from(played.skips),
    }
}

/// Displayed plays of an album or artist: its source's own counts, else its members' when the
/// list asks for them.
fn collection_played(native: [u32; 2], totals: &Totals, activity: bool) -> Played {
    Played {
        plays: known(native[0])
            .or(activity.then_some(totals.plays as u32))
            .unwrap_or(0),
        skips: 0,
        last: known(native[1]).or(totals.last_played.filter(|_| activity)),
    }
}

/// Builds rows from residents and what the keyed statement read for them. A first pass names
/// the statement's keys, `want`; a second builds each row from its record and the statement.
struct Builder<'a> {
    catalog: &'a Catalog,
    context: Context<'a>,
    snapshot: &'a Snapshot,
    /// Totals and pictures of the whole library, and totals of the music folder the rows read
    /// in.
    covers: Arc<Derived>,
    in_folder: std::cell::OnceCell<Arc<Derived>>,
    /// Plays, for rows that show them, and downloads: of the whole library, then of the folder.
    activity: [std::cell::OnceCell<Arc<Activity>>; 2],
    downloaded: [std::cell::OnceCell<Arc<Downloaded>>; 2],
    shape: &'a Shape,
    art: bool,
    /// Listens by smart playlist window.
    inputs: &'a Inputs,
}

impl<'a> Builder<'a> {
    fn new(
        catalog: &'a Catalog,
        snapshot: &'a Snapshot,
        shape: &'a Shape,
        inputs: &'a Inputs,
        art: bool,
    ) -> Self {
        let context = Context::new(snapshot);
        Self {
            covers: catalog.derived(&context, None),
            catalog,
            context,
            snapshot,
            in_folder: std::cell::OnceCell::new(),
            activity: Default::default(),
            downloaded: Default::default(),
            shape,
            art,
            inputs,
        }
    }

    /// The music folder a row of `kind`, of source `owner`, counts in: the one the rows read in
    /// when the one rule narrows that kind ([`Context::row_folder`]), else none.
    fn counted_in(&self, kind: CatalogKind, owner: u16) -> Option<u32> {
        self.context
            .row_folder(self.shape.folder, kind, Some(owner))
            .flatten()
    }

    /// Collection totals in `folder`, with plays when the rows show activity.
    fn totals(&self, folder: Option<u32>, activity: bool) -> Totaled<'_> {
        let derived = match folder {
            None => &self.covers,
            Some(folder) => self
                .in_folder
                .get_or_init(|| self.catalog.derived(&self.context, Some(folder))),
        };
        Totaled {
            derived,
            activity: activity.then(|| {
                &**self.activity[usize::from(folder.is_some())]
                    .get_or_init(|| self.catalog.activity(&self.context, folder))
            }),
        }
    }

    /// Downloads of each collection in `folder`, counted as [`Builder::totals`] counts tracks.
    fn downloaded(&self, folder: Option<u32>) -> &Downloaded {
        self.downloaded[usize::from(folder.is_some())]
            .get_or_init(|| self.catalog.downloaded(&self.context, folder))
    }

    fn track_art(&self, key: TrackKey) -> Option<Art> {
        let record = self.snapshot.tracks.get(key.raw())?;
        Some(self.context.track_art(&self.covers, record))
    }

    fn album_art(&self, key: AlbumKey) -> Option<Art> {
        let slot = self.snapshot.albums.slot(key.raw())?;
        Some(self.context.album_art(&self.covers, slot))
    }

    fn artist_art(&self, key: ArtistKey) -> Option<Art> {
        let slot = self.snapshot.artists.slot(key.raw())?;
        Some(self.context.artist_art(&self.covers, slot))
    }

    /// A genre shows its own picture, else its mosaic; a mood its mosaic.
    fn tag_art(&self, key: i64, want: &mut Want) {
        let Some(record) = self.snapshot.tags.get(key) else {
            return;
        };
        if record.kind() == TagKind::Genre {
            want.add(Part::TagArt, key);
        }
        if record.kind() == TagKind::Mood || record.flags & PICTURE == 0 {
            want.add(Part::TagMosaic, key);
        }
    }

    /// A playlist folder's picture is its first playlist's: smart or not, and its key.
    fn folder_cover(&self, key: PlaylistFolderKey) -> Option<(bool, i64)> {
        let snapshot = self.snapshot;
        if snapshot.playlist_folders.get(&key.raw())?.smart {
            return snapshot
                .smart_playlists
                .values()
                .filter(|smart| smart.folder == Some(key.raw()))
                .min_by_key(|smart| smart.key)
                .map(|smart| (true, smart.key));
        }
        snapshot
            .playlists
            .iter()
            .map(|(_, record)| record)
            .filter(|record| {
                record.folder == key.raw()
                    && !snapshot.text(record.name).is_empty()
                    && (record.flags & SOURCE_OWNED == 0 || record.indexed())
            })
            .min_by_key(|record| record.key)
            .map(|record| (false, record.key))
    }

    fn source(&self, slot: u16) -> Option<&'a crate::catalog::records::Source> {
        self.snapshot
            .source_key(slot)
            .and_then(|key| self.snapshot.source(key))
    }

    /// The statement's keys an item's row reads.
    fn want(&self, item: Item, listens: &[Listen], want: &mut Want) {
        let art = |want: &mut Want, art: Option<Art>| {
            if let Some(art) = art.filter(|_| self.art) {
                want.art(art);
            }
        };
        match item {
            Item::Track { key, .. } | Item::AlbumTrack { key, .. } => {
                want.add(Part::Track, key.raw());
                art(want, self.track_art(key));
            }
            Item::Listen(index) => {
                if let Some(key) = listens.get(index as usize).and_then(|listen| listen.track) {
                    self.want(Item::Track { key, entry: None }, listens, want);
                }
            }
            Item::Album(key) | Item::AlbumHeading(key) => {
                want.add(Part::AlbumText, key.raw());
                art(want, self.album_art(key));
            }
            Item::Artist { key, .. } => art(want, self.artist_art(key)),
            _ if !self.art => {}
            Item::Genre(key) => self.tag_art(key.raw(), want),
            Item::Mood(key) => self.tag_art(key.raw(), want),
            Item::Folder(key) => want.add(Part::FolderArt, key.raw()),
            Item::Playlist(key) => want.add(Part::PlaylistArt, key.raw()),
            Item::Smart(key) => want.add(Part::SmartArt, key.raw()),
            Item::PlaylistFolder(key) => match self.folder_cover(key) {
                Some((true, key)) => want.add(Part::SmartArt, key),
                Some((false, key)) => want.add(Part::PlaylistArt, key),
                None => {}
            },
        }
    }

    fn want_header(&self, header: HeaderKey, want: &mut Want) {
        match header {
            HeaderKey::Album { key, .. } => {
                want.add(Part::AlbumView, key.raw());
                self.want(Item::Album(key), &[], want);
            }
            HeaderKey::Artist {
                key, album_artist, ..
            } => {
                want.add(Part::ArtistView, key.raw());
                self.want(Item::Artist { key, album_artist }, &[], want);
            }
            HeaderKey::Playlist(key) => self.want(Item::Playlist(key), &[], want),
            HeaderKey::Smart { key, .. } => self.want(Item::Smart(key), &[], want),
            HeaderKey::Genre(key) => self.want(Item::Genre(key), &[], want),
            HeaderKey::Mood(key) => self.want(Item::Mood(key), &[], want),
        }
    }

    fn track(&self, key: TrackKey, got: &Got) -> LibraryResult<Option<TrackRow>> {
        let snapshot = self.snapshot;
        let Some(slot) = snapshot.tracks.slot(key.raw()) else {
            return Ok(None);
        };
        let record = &snapshot.tracks.records[slot as usize];
        let source = self.source(record.source);
        let album = self
            .context
            .album_of(record)
            .and_then(|slot| snapshot.albums.live(slot));
        // The row may name its album: the track is listed, or its album lists another track.
        let named = record.indexed()
            || self
                .context
                .album_of(record)
                .is_some_and(|album| self.covers.albums[album as usize].tracks > 0);
        let artist_link = |slot: &u32| {
            let artist = snapshot.artists.live(*slot)?;
            Some(TrackArtistLink {
                artist_key: ArtistKey::from_raw(i64::from(artist.key)),
                media_uri: entity_uri(snapshot, artist.source, "artist", artist.object),
                name: text(snapshot, artist.name),
            })
        };
        let runs = snapshot.track_runs(record);
        let stored = got.tracks.get(&key.raw()).cloned().unwrap_or_default();
        let title = text(snapshot, record.title);
        Ok(Some(TrackRow {
            track_key: key,
            entry: None,
            source_key: source.map(|source| source.key),
            source_id: source.map(|source| source.object.clone()),
            source_name: source.map(|source| source.name.clone()).unwrap_or_default(),
            title: if title.is_empty() {
                stored.media_uri.clone()
            } else {
                title
            },
            media_uri: stored.media_uri,
            object_id: stored.object_id,
            artist: text(snapshot, record.artist),
            album: text(snapshot, record.album_title),
            duration_millis: i64::from(record.duration_ms),
            disc_number: i64::from(record.disc),
            track_number: i64::from(record.number),
            year: year_of(record.year).map(i64::from),
            bpm: (record.bpm != u16::MAX).then_some(i64::from(record.bpm)),
            date_added: known(record.added).map(i64::from),
            source_format: optional(snapshot, record.format),
            state: state(
                record.flags,
                record.rating,
                record.native_rating,
                self.context.played(slot, record),
            ),
            is_downloaded: record.flags & DOWNLOADED != 0,
            cue: record.flags & CUE != 0,
            links: TrackLinks {
                album_key: album
                    .filter(|_| named)
                    .map(|album| AlbumKey::from_raw(i64::from(album.key))),
                album_media_uri: album
                    .filter(|_| named)
                    .map(|album| entity_uri(snapshot, album.source, "album", album.object)),
                artists: runs[0].iter().filter_map(artist_link).collect(),
                // The album's own credits: none without an album.
                album_artists: album
                    .map(|album| {
                        snapshot.album_runs(album)[0]
                            .iter()
                            .filter_map(artist_link)
                            .collect()
                    })
                    .unwrap_or_default(),
                genres: runs[2]
                    .iter()
                    .filter_map(|slot| snapshot.tags.live(*slot))
                    .filter(|tag| tag.kind() == TagKind::Genre)
                    .map(|tag| TrackGenreLink {
                        genre_key: GenreKey::from_raw(i64::from(tag.key)),
                        name: text(snapshot, tag.name),
                    })
                    .collect(),
            },
            release_date: stored.release_date,
            source_path: stored.source_path,
            audio: stored.audio,
            artwork_binding: got.binding(self.context.track_art(&self.covers, record))?,
        }))
    }

    /// A track's full view: its row, and the details, CUE window and MBIDs only it reads.
    fn track_view(&self, key: TrackKey, got: &Got) -> LibraryResult<Option<TrackView>> {
        let (Some(row), Some((metadata, access)), Some(record)) = (
            self.track(key, got)?,
            got.views.get(&key.raw()),
            self.snapshot.tracks.get(key.raw()),
        ) else {
            return Ok(None);
        };
        Ok(Some(TrackView {
            track_key: key,
            source_key: row.source_key,
            source_id: row.source_id,
            object_id: row.object_id,
            metadata: TrackMetadata {
                title: row.title,
                artist: row.artist,
                album: row.album,
                sort_text: self.snapshot.track_sort(record).into_owned(),
                duration_millis: row.duration_millis,
                disc_number: row.disc_number,
                track_number: row.track_number,
                year: row.year,
                release_date: row.release_date,
                date_added: known(record.added).and_then(date_text),
                source_format: row.source_format,
                audio_properties: row.audio,
                comment: optional(self.snapshot, record.comment),
                bpm: row.bpm,
                ..metadata.clone()
            },
            media_uri: row.media_uri,
            state: row.state,
            access: TrackAccess {
                source_path: row.source_path,
                source_name: row.source_name,
                is_downloaded: row.is_downloaded,
                ..access.clone()
            },
            links: row.links,
            artwork_binding: row.artwork_binding,
        }))
    }

    fn album(&self, key: AlbumKey, activity: bool, got: &Got) -> LibraryResult<Option<AlbumRow>> {
        let snapshot = self.snapshot;
        let Some(slot) = snapshot.albums.slot(key.raw()) else {
            return Ok(None);
        };
        let record = &snapshot.albums.records[slot as usize];
        let Some(source_key) = snapshot.source_key(record.source) else {
            return Ok(None);
        };
        let folder = self.counted_in(CatalogKind::Album, record.source);
        let mut totals = self.totals(folder, activity).album(slot);
        totals.albums = u32::from(totals.tracks > 0);
        totals.downloaded = self.downloaded(folder).albums[slot as usize];
        let runs = snapshot.album_runs(record);
        let (release_date, date_added) =
            got.album_texts.get(&key.raw()).cloned().unwrap_or_default();
        Ok(Some(AlbumRow {
            album_key: key,
            source_key,
            object_id: text(snapshot, record.object),
            media_uri: entity_uri(snapshot, record.source, "album", record.object),
            title: text(snapshot, record.title),
            artist: text(snapshot, record.artist),
            sort_text: text(snapshot, record.sort),
            year: year_of(record.year).map(i64::from),
            release_date,
            date_added,
            is_compilation: record.flags & COMPILATION != 0,
            release_class: AlbumReleaseClass::of(record.flags),
            state: state(
                record.flags,
                record.rating,
                record.native_rating,
                collection_played(record.native, &totals, activity),
            ),
            summary: totals.summary(),
            links: AlbumLinks {
                album_artists: runs[0]
                    .iter()
                    .filter_map(|slot| snapshot.artists.live(*slot))
                    .map(|artist| AlbumArtistLink {
                        artist_key: ArtistKey::from_raw(i64::from(artist.key)),
                        media_uri: entity_uri(snapshot, artist.source, "artist", artist.object),
                        name: text(snapshot, artist.name),
                    })
                    .collect(),
                genres: runs[1]
                    .iter()
                    .filter_map(|slot| snapshot.tags.live(*slot))
                    .map(|tag| AlbumGenreLink {
                        genre_key: GenreKey::from_raw(i64::from(tag.key)),
                        name: text(snapshot, tag.name),
                    })
                    .collect(),
            },
            artwork_binding: got.binding(self.context.album_art(&self.covers, slot))?,
        }))
    }

    fn artist(
        &self,
        key: ArtistKey,
        album_artist: bool,
        activity: bool,
        got: &Got,
    ) -> LibraryResult<Option<ArtistRow>> {
        let snapshot = self.snapshot;
        let Some(slot) = snapshot.artists.slot(key.raw()) else {
            return Ok(None);
        };
        let record = &snapshot.artists.records[slot as usize];
        let Some(source_key) = snapshot.source_key(record.source) else {
            return Ok(None);
        };
        let folder = self.counted_in(CatalogKind::Artist, record.source);
        let mut totals = self
            .totals(folder, activity)
            .artist(usize::from(album_artist), slot);
        totals.downloaded =
            self.downloaded(folder).artists[usize::from(album_artist)][slot as usize];
        Ok(Some(ArtistRow {
            artist_key: key,
            album_artist,
            source_key,
            object_id: text(snapshot, record.object),
            media_uri: entity_uri(snapshot, record.source, "artist", record.object),
            name: text(snapshot, record.name),
            sort_text: text(snapshot, record.sort),
            state: state(
                record.flags,
                record.rating,
                record.native_rating,
                collection_played(record.native, &totals, activity),
            ),
            summary: totals.summary(),
            artwork_binding: got.binding(self.context.artist_art(&self.covers, slot))?,
        }))
    }

    fn tag(
        &self,
        key: i64,
        kind: TagKind,
        got: &Got,
        galleries: &HashMap<i64, Vec<Vec<u8>>>,
    ) -> LibraryResult<Option<TagRow>> {
        let snapshot = self.snapshot;
        let Some(slot) = snapshot.tags.slot(key) else {
            return Ok(None);
        };
        let record = &snapshot.tags.records[slot as usize];
        let Some(source) = self.source(record.source).filter(|_| record.kind() == kind) else {
            return Ok(None);
        };
        let artwork_binding = got.own(Part::TagArt, key)?;
        Ok(Some(TagRow {
            key,
            kind,
            source_key: source.key,
            source_id: source.object.clone(),
            object_id: text(snapshot, record.object),
            media_uri: entity_uri(snapshot, record.source, kind.table().name, record.object),
            name: text(snapshot, record.name),
            summary: {
                let folder = self.counted_in(kind.catalog(), record.source);
                Totals {
                    downloaded: self.downloaded(folder).tags[slot as usize],
                    ..self.totals(folder, false).tag(slot)
                }
                .summary()
            },
            representative_artwork: artwork_binding
                .is_none()
                .then(|| galleries.get(&key).cloned())
                .flatten()
                .unwrap_or_default(),
            artwork_binding,
        }))
    }

    fn playlist(&self, key: PlaylistKey, got: &Got) -> LibraryResult<Option<PlaylistRow>> {
        let snapshot = self.snapshot;
        let Some(record) = snapshot
            .playlists
            .get(key.raw())
            .filter(|record| !snapshot.text(record.name).is_empty())
        else {
            return Ok(None);
        };
        let mut genres = HashMap::<u32, u32>::new();
        for entry in &record.entries {
            let Some(track) = snapshot.tracks.live(entry.track) else {
                continue;
            };
            for tag in snapshot.track_runs(track)[2] {
                if snapshot
                    .tags
                    .live(*tag)
                    .is_some_and(|tag| tag.kind() == TagKind::Genre)
                {
                    *genres.entry(*tag).or_default() += 1;
                }
            }
        }
        let mut genres = genres
            .into_iter()
            .map(|(slot, count)| {
                let tag = &snapshot.tags.records[slot as usize];
                (
                    std::cmp::Reverse(count),
                    snapshot.name_sort(tag.name, tag.sort),
                    tag,
                )
            })
            .collect::<Vec<_>>();
        genres.sort_by(|a, b| (&a.0, &a.1, a.2.key).cmp(&(&b.0, &b.1, b.2.key)));
        let summary = self.context.playlist_totals(record).summary();
        let object_id = text(snapshot, record.object);
        let source = self.source(record.source);
        let source_id = source.map(|source| source.object.clone());
        Ok(Some(PlaylistRow {
            playlist_key: key,
            source_key: source.map(|source| source.key),
            artwork_binding: got.playlist(false, key.raw())?,
            source_id,
            source_owned: record.flags & SOURCE_OWNED != 0,
            object_id,
            name: text(snapshot, record.name),
            writable: record.flags & WRITABLE != 0,
            metadata_writable: record.flags & METADATA_WRITABLE != 0,
            track_count: summary.track_count,
            duration_millis: summary.duration_millis,
            downloaded_count: summary.downloaded_count,
            genres: genres
                .into_iter()
                .take(2)
                .map(|(.., tag)| PlaylistGenreLink {
                    genre_key: GenreKey::from_raw(i64::from(tag.key)),
                    name: text(snapshot, tag.name),
                })
                .collect(),
        }))
    }

    /// A smart playlist's totals in the shape's music folder, as its page lists its members.
    fn smart_totals(&self, smart: &crate::catalog::snapshot::SmartPlaylist) -> Totals {
        let Some(folder) =
            self.context
                .row_folder(self.shape.folder, CatalogKind::SmartPlaylist, None)
        else {
            return Totals::default();
        };
        let opened = self
            .catalog
            .opened_smart(smart.key, self.shape.now, &smart.definition);
        self.context.smart_totals(
            &smart.definition,
            self.inputs,
            opened.as_deref().map(Vec::as_slice),
            folder,
        )
    }

    fn smart(&self, key: SmartPlaylistKey, got: &Got) -> LibraryResult<Option<SmartPlaylistRow>> {
        let Some(smart) = self.snapshot.smart_playlists.get(&key.raw()) else {
            return Ok(None);
        };
        let mut definition = smart.definition.clone();
        crate::smart_playlists::normalize_definition(&mut definition)?;
        let summary = self.smart_totals(smart).summary();
        Ok(Some(SmartPlaylistRow {
            smart_playlist_key: key,
            object_id: smart.object.clone(),
            name: smart.name.clone(),
            definition,
            position: smart.position,
            track_count: summary.track_count,
            duration_millis: summary.duration_millis,
            downloaded_count: summary.downloaded_count,
            artwork_binding: got.playlist(true, key.raw())?,
        }))
    }

    fn playlist_folder(
        &self,
        key: PlaylistFolderKey,
        got: &Got,
    ) -> LibraryResult<Option<PlaylistFolderRow>> {
        let snapshot = self.snapshot;
        let Some(folder) = snapshot.playlist_folders.get(&key.raw()) else {
            return Ok(None);
        };
        let totals = if folder.smart {
            snapshot
                .smart_playlists
                .values()
                .filter(|smart| smart.folder == Some(key.raw()))
                .fold(Totals::default(), |mut sum, smart| {
                    let totals = self.smart_totals(smart);
                    sum.tracks += totals.tracks;
                    sum.duration += totals.duration;
                    sum
                })
        } else {
            let mut children = self
                .shape
                .list
                .clone()
                .filter(|read| read.kind == ListKind::Playlists)
                .unwrap_or_else(|| ListRead::new(ListKind::Playlists, None, LibraryField::Title));
            children.filter.clear();
            if children.playlist_scope == PlaylistScope::All {
                children.source = None;
            }
            self.context
                .playlist_folder_totals(&children)
                .remove(&key.raw())
                .unwrap_or_default()
        };
        Ok(Some(PlaylistFolderRow {
            folder_key: key,
            object_id: folder.object.clone(),
            name: folder.name.clone(),
            smart: folder.smart,
            artwork_binding: match self.folder_cover(key) {
                Some((smart, key)) => got.playlist(smart, key)?,
                None => None,
            },
            track_count: i64::from(totals.tracks),
            duration_millis: totals.duration as i64,
        }))
    }

    /// The row of one item; None when the Catalog no longer holds it.
    fn row(
        &self,
        item: Item,
        listens: &[Listen],
        got: &Got,
        galleries: &HashMap<i64, Vec<Vec<u8>>>,
    ) -> LibraryResult<Option<Row>> {
        let activity = self.shape.activity;
        Ok(match item {
            Item::Track { key, entry } => self.track(key, got)?.map(|mut row| {
                row.entry = entry.and_then(|entry| {
                    Some(PlaylistEntry {
                        key: entry,
                        position: *self.shape.positions.get(&entry.raw())?,
                    })
                });
                Row::Track(row)
            }),
            Item::AlbumTrack { key, index, .. } => self
                .track(key, got)?
                .map(|track| Row::AlbumTrack { track, index }),
            Item::Listen(index) => {
                let Some(listen) = listens.get(index as usize) else {
                    return Ok(None);
                };
                let mut row = match listen.track {
                    Some(key) => self.track(key, got)?,
                    None => None,
                }
                .unwrap_or_else(|| TrackRow {
                    track_key: TrackKey::from_raw(0),
                    entry: None,
                    source_key: None,
                    source_id: None,
                    source_name: String::new(),
                    media_uri: listen.copy.media_uri.clone(),
                    object_id: None,
                    title: String::new(),
                    artist: String::new(),
                    album: String::new(),
                    duration_millis: 0,
                    disc_number: 0,
                    track_number: 0,
                    year: None,
                    bpm: None,
                    date_added: None,
                    source_format: None,
                    state: TrackState::default(),
                    is_downloaded: false,
                    cue: false,
                    links: TrackLinks::default(),
                    release_date: None,
                    source_path: None,
                    audio: AudioProperties::default(),
                    artwork_binding: None,
                });
                let listened = self
                    .snapshot
                    .tracks
                    .slot(row.track_key.raw())
                    .map(|slot| self.context.listened(slot))
                    .unwrap_or_default();
                show_listen(&mut row, &listen.copy, listened);
                Some(Row::Track(row))
            }
            Item::AlbumHeading(key) => self.album(key, activity, got)?.map(Row::AlbumHeading),
            Item::Album(key) => self.album(key, activity, got)?.map(Row::Album),
            Item::Artist { key, album_artist } => self
                .artist(key, album_artist, activity, got)?
                .map(Row::Artist),
            Item::Genre(key) => self
                .tag(key.raw(), TagKind::Genre, got, galleries)?
                .map(Row::Tag),
            Item::Mood(key) => self
                .tag(key.raw(), TagKind::Mood, got, galleries)?
                .map(Row::Tag),
            Item::Folder(key) => match self.snapshot.folders.slot(key.raw()) {
                Some(slot) => {
                    let record = &self.snapshot.folders.records[slot as usize];
                    let folder = self.counted_in(CatalogKind::Folder, record.source);
                    Some(Row::Folder(FolderRow {
                        folder_key: Some(key),
                        object_id: text(self.snapshot, record.object),
                        name: text(self.snapshot, record.name),
                        summary: Totals {
                            downloaded: self.downloaded(folder).folders[slot as usize],
                            ..self.totals(folder, false).derived.folders[slot as usize]
                        }
                        .summary(),
                        artwork_binding: got.own(Part::FolderArt, key.raw())?,
                    }))
                }
                None => None,
            },
            Item::Playlist(key) => self
                .playlist(key, got)?
                .map(|row| Row::Playlist(PlaylistListRow::Playlist(row))),
            Item::Smart(key) => self
                .smart(key, got)?
                .map(|row| Row::Playlist(PlaylistListRow::SmartPlaylist(row))),
            Item::PlaylistFolder(key) => self
                .playlist_folder(key, got)?
                .map(|row| Row::Playlist(PlaylistListRow::Folder(row))),
        })
    }

    fn header(
        &self,
        key: HeaderKey,
        got: &Got,
        galleries: &HashMap<i64, Vec<Vec<u8>>>,
    ) -> LibraryResult<Option<Header>> {
        Ok(match key {
            HeaderKey::Album { key, activity } => self.album(key, activity, got)?.map(|row| {
                Header::Album(Box::new(AlbumView {
                    row,
                    metadata: got.album_views.get(&key.raw()).cloned().unwrap_or_default(),
                }))
            }),
            HeaderKey::Artist {
                key,
                album_artist,
                activity,
            } => self.artist(key, album_artist, activity, got)?.map(|row| {
                Header::Artist(Box::new(ArtistView {
                    row,
                    musicbrainz_artist_id: got.artist_views.get(&key.raw()).cloned().flatten(),
                }))
            }),
            HeaderKey::Playlist(key) => self
                .playlist(key, got)?
                .map(|row| Header::Playlist(Box::new(row))),
            HeaderKey::Smart { key, .. } => self
                .smart(key, got)?
                .map(|row| Header::Smart(Box::new(row))),
            HeaderKey::Genre(key) => self
                .tag(key.raw(), TagKind::Genre, got, galleries)?
                .map(Header::Tag),
            HeaderKey::Mood(key) => self
                .tag(key.raw(), TagKind::Mood, got, galleries)?
                .map(Header::Tag),
        })
    }
}

/// A track as a listen recorded it, with Rufin's own counts of its listens.
pub(crate) fn show_listen(row: &mut TrackRow, listen: &ListenWrite, listened: Played) {
    row.title.clone_from(&listen.title);
    row.artist.clone_from(&listen.artist);
    row.album.clone_from(&listen.album);
    row.duration_millis = listen.duration_millis;
    row.disc_number = listen.disc_number.unwrap_or_default();
    row.track_number = listen.track_number.unwrap_or_default();
    row.year = listen.year;
    row.release_date.clone_from(&listen.release_date);
    row.source_format.clone_from(&listen.source_format);
    row.state.last_played = Some(listen.started_at);
    row.state.play_count = i64::from(listened.plays);
    row.state.skip_count = i64::from(listened.skips);
}

impl Catalog {
    /// Listens in the windows the smart playlists these items total read.
    pub(crate) async fn smart_inputs(
        &self,
        connection: &mut sqlx::SqliteConnection,
        items: impl Iterator<Item = Item>,
        now: i64,
    ) -> LibraryResult<Inputs> {
        let mut inputs = Inputs::default();
        let periods = {
            let snapshot = self.snapshot();
            let mut periods = Vec::new();
            for item in items {
                for smart in snapshot.smart_playlists.values() {
                    let counted = match item {
                        Item::Smart(key) => smart.key == key.raw(),
                        Item::PlaylistFolder(key) => smart.folder == Some(key.raw()),
                        _ => false,
                    };
                    if counted {
                        periods.extend(crate::catalog::views::smart_period(&smart.definition));
                    }
                }
            }
            periods.sort_unstable();
            periods.dedup();
            periods
        };
        if !periods.is_empty() {
            self.read_periods(connection, &mut inputs, periods, now)
                .await?;
        }
        Ok(inputs)
    }

    /// Rows of these windows and headers, read on one connection: residents, then one
    /// statement, then a second only for the mosaics of tags without a picture.
    pub(crate) async fn fetch_on(
        &self,
        connection: &mut sqlx::SqliteConnection,
        windows: [(&[Item], &[Listen]); 2],
        header: Option<HeaderKey>,
        shape: &Shape,
        art: bool,
    ) -> LibraryResult<(Option<Header>, [Vec<Row>; 2])> {
        let mut shape = shape.clone();
        if let Some(HeaderKey::Smart { now, .. }) = header {
            shape.now = now;
        }
        let inputs = self
            .smart_inputs(
                connection,
                windows
                    .iter()
                    .flat_map(|(items, _)| items.iter().copied())
                    .chain(header.and_then(|header| match header {
                        HeaderKey::Smart { key, .. } => Some(Item::Smart(key)),
                        _ => None,
                    })),
                shape.now,
            )
            .await?;
        let mut want = Want::new(true);
        {
            let snapshot = self.snapshot();
            let builder = Builder::new(self, &snapshot, &shape, &inputs, art);
            for (items, listens) in windows {
                for item in items {
                    builder.want(*item, listens, &mut want);
                }
            }
            if let Some(header) = header {
                builder.want_header(header, &mut want);
            }
        }
        let got = fetch::run(connection, &want).await?;
        let mut galleries = HashMap::new();
        if !got.mosaics.is_empty() {
            let picks = got.mosaics.iter().collect::<Vec<_>>();
            let arts = self.arts(
                picks
                    .iter()
                    .flat_map(|(_, picks)| picks.iter())
                    .map(|key| key.map(crate::catalog::views::Owner::Track)),
            );
            let mut bindings = fetch::bindings(connection, &arts).await?.into_iter();
            for (key, picks) in picks {
                let gallery = bindings.by_ref().take(picks.len()).flatten().collect();
                galleries.insert(*key, gallery);
            }
        }
        let snapshot = self.snapshot();
        let builder = Builder::new(self, &snapshot, &shape, &inputs, art);
        let mut rows = [Vec::new(), Vec::new()];
        for (built, (items, listens)) in rows.iter_mut().zip(windows) {
            for item in items {
                built.extend(builder.row(*item, listens, &got, &galleries)?);
            }
        }
        let header = match header {
            Some(header) => builder.header(header, &got, &galleries)?,
            None => None,
        };
        Ok((header, rows))
    }

    /// One keyed statement: the window's rows, a detail header and an artist's favorite tracks.
    pub async fn fetch(
        &self,
        fetch: Fetch<'_>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Fetched> {
        /// A window's items, listens and entry positions.
        type Window<'a> = (Vec<Item>, &'a [Listen], HashMap<i64, u32>);
        fn window<'a>(order: Option<&(&'a Order, Range<usize>)>) -> Window<'a> {
            let Some((order, range)) = order else {
                return (Vec::new(), &[], HashMap::new());
            };
            let limit = if matches!(
                order.read(),
                Read::List(ListRead {
                    kind: ListKind::ArtistAlbums { .. },
                    ..
                })
            ) {
                WINDOW / 2
            } else {
                WINDOW
            };
            let range = range.start..range.end.min(range.start.saturating_add(limit));
            (
                order.items(range.clone()),
                order.listens(),
                order.positions(range),
            )
        }
        let (rows, listens, positions) = window(fetch.rows.as_ref());
        let (favorites, favorite_listens, _) = window(fetch.favorites.as_ref());
        let shape = Shape {
            positions,
            ..Shape::of(fetch.rows.as_ref().map(|(order, _)| order.read()))
        };
        let mut connection = self.acquire_loaded(cancellation).await?;
        let (header, [rows, favorites]) = self
            .fetch_on(
                &mut connection,
                [(&rows, listens), (&favorites, favorite_listens)],
                fetch.header,
                &shape,
                fetch.art,
            )
            .await?;
        Ok(Fetched {
            header,
            rows,
            favorites,
        })
    }

    /// `fetch` with rows only. A window is at most 256 rows, 128 for an artist's albums.
    pub async fn rows(
        &self,
        order: &Order,
        range: Range<usize>,
        art: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<Row>> {
        Ok(self
            .fetch(
                Fetch {
                    rows: Some((order, range)),
                    art,
                    ..Fetch::default()
                },
                cancellation,
            )
            .await?
            .rows)
    }

    /// Rows of items from no order, such as pins, Home blocks and search results. A listen
    /// has no row outside its History order.
    pub async fn rows_of(
        &self,
        items: &[Item],
        art: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<Row>> {
        let mut connection = self.acquire_loaded(cancellation).await?;
        let items = &items[..items.len().min(WINDOW)];
        self.rows_of_on(&mut connection, items, &Shape::of(None), art)
            .await
    }

    /// Rows of items from no order, on a caller's connection.
    pub(crate) async fn rows_of_on(
        &self,
        connection: &mut sqlx::SqliteConnection,
        items: &[Item],
        shape: &Shape,
        art: bool,
    ) -> LibraryResult<Vec<Row>> {
        let (_, [rows, _]) = self
            .fetch_on(connection, [(items, &[]), (&[], &[])], None, shape, art)
            .await?;
        Ok(rows)
    }

    /// A collection URI's item, from resident object ids. Track URIs are not resident.
    pub fn item(&self, uri: &str) -> Option<Item> {
        let (source, kind, object) = source_entity_parts(uri)?;
        let kind = CatalogKind::of(&kind)?;
        let snapshot = self.snapshot();
        let source = snapshot.source_slot(
            snapshot
                .sources
                .values()
                .find(|known| known.object == source.as_str())?
                .key,
        )?;
        kind.item(snapshot.object_keys(kind, source, &[&object])[0]?)
    }

    /// A collection item's URI, built from its resident object id. Tracks have none here.
    pub fn uri(&self, item: Item) -> Option<String> {
        let snapshot = self.snapshot();
        let (source, kind, object) = match item {
            Item::Album(key) | Item::AlbumHeading(key) => {
                let album = snapshot.albums.get(key.raw())?;
                (album.source, "album", album.object)
            }
            Item::Artist { key, .. } => {
                let artist = snapshot.artists.get(key.raw())?;
                (artist.source, "artist", artist.object)
            }
            Item::Genre(key) => {
                let tag = snapshot.tags.get(key.raw())?;
                (tag.source, "genre", tag.object)
            }
            Item::Mood(key) => {
                let tag = snapshot.tags.get(key.raw())?;
                (tag.source, "mood", tag.object)
            }
            Item::Folder(key) => {
                let folder = snapshot.folders.get(key.raw())?;
                (folder.source, "folder", folder.object)
            }
            _ => return None,
        };
        Some(entity_uri(&snapshot, source, kind, object)).filter(|uri| !uri.is_empty())
    }

    /// Full track views by key, in the order asked, once the Catalog has loaded. Keys it does
    /// not hold are skipped; a key asked twice reads twice.
    pub(crate) async fn track_views_on(
        &self,
        connection: &mut sqlx::SqliteConnection,
        keys: &[TrackKey],
    ) -> LibraryResult<Vec<TrackView>> {
        self.loaded().await;
        let shape = Shape::of(None);
        let inputs = Inputs::default();
        let mut want = Want::new(true);
        {
            let snapshot = self.snapshot();
            let builder = Builder::new(self, &snapshot, &shape, &inputs, true);
            for key in keys {
                want.add(Part::View, key.raw());
                builder.want(
                    Item::Track {
                        key: *key,
                        entry: None,
                    },
                    &[],
                    &mut want,
                );
            }
        }
        let got = fetch::run(connection, &want).await?;
        let snapshot = self.snapshot();
        let builder = Builder::new(self, &snapshot, &shape, &inputs, true);
        let mut views = Vec::with_capacity(keys.len());
        for key in keys {
            views.extend(builder.track_view(*key, &got)?);
        }
        Ok(views)
    }

    /// The one full read of tracks: details, paths, CUE window and MBIDs.
    pub async fn tracks(
        &self,
        keys: &[TrackKey],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackView>> {
        let mut connection = self.acquire_loaded(cancellation).await?;
        self.track_views_on(&mut connection, &keys[..keys.len().min(WINDOW)])
            .await
    }

    pub async fn track(
        &self,
        uri: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<TrackView>> {
        Ok(self
            .tracks_by_uris(&[uri.to_owned()], cancellation)
            .await?
            .pop())
    }

    /// Track views by URI, in the order asked. URIs the Store does not hold are skipped.
    pub async fn tracks_by_uris(
        &self,
        uris: &[String],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackView>> {
        let mut connection = self.acquire_loaded(cancellation).await?;
        let keys = track_keys_on(&mut connection, &uris[..uris.len().min(WINDOW)]).await?;
        self.track_views_on(&mut connection, &keys).await
    }

    /// Track keys by URI, in the order asked. URIs the Store does not hold are skipped.
    pub async fn track_keys(
        &self,
        uris: &[String],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackKey>> {
        let mut connection = self.acquire_cancellable(cancellation).await?;
        track_keys_on(&mut connection, uris).await
    }

    /// How many listed tracks an album's, artist's, genre's or mood's header counts in music
    /// folder `folder`; 0 for one the Catalog does not hold, and for a playlist.
    pub fn track_count(&self, header: &HeaderKey, folder: Option<FolderKey>) -> i64 {
        let snapshot = self.snapshot();
        let shape = Shape {
            folder,
            ..Shape::of(None)
        };
        let inputs = Inputs::default();
        let builder = Builder::new(self, &snapshot, &shape, &inputs, false);
        let tag = |key: i64| {
            let slot = snapshot.tags.slot(key)?;
            let record = &snapshot.tags.records[slot as usize];
            let folder = builder.counted_in(record.kind().catalog(), record.source);
            Some(builder.totals(folder, false).tag(slot))
        };
        let totals = match *header {
            HeaderKey::Genre(key) => tag(key.raw()),
            HeaderKey::Mood(key) => tag(key.raw()),
            HeaderKey::Album { key, .. } => snapshot.albums.slot(key.raw()).map(|slot| {
                let source = snapshot.albums.records[slot as usize].source;
                let folder = builder.counted_in(CatalogKind::Album, source);
                builder.totals(folder, false).album(slot)
            }),
            HeaderKey::Artist {
                key, album_artist, ..
            } => snapshot.artists.slot(key.raw()).map(|slot| {
                let source = snapshot.artists.records[slot as usize].source;
                let folder = builder.counted_in(CatalogKind::Artist, source);
                builder
                    .totals(folder, false)
                    .artist(usize::from(album_artist), slot)
            }),
            HeaderKey::Playlist(_) | HeaderKey::Smart { .. } => None,
        };
        totals.map_or(0, |totals| totals.summary().track_count)
    }

    /// A detail header's full view, as a page shows it.
    async fn header(
        &self,
        header: HeaderKey,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<Header>> {
        let mut connection = self.acquire_loaded(cancellation).await?;
        Ok(self
            .fetch_on(
                &mut connection,
                [(&[], &[]), (&[], &[])],
                Some(header),
                &Shape::of(None),
                true,
            )
            .await?
            .0)
    }

    pub async fn album(
        &self,
        uri: &str,
        activity: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<AlbumView>> {
        self.loaded().await;
        let Some(Item::Album(key)) = self.item(uri) else {
            return Ok(None);
        };
        Ok(
            match self
                .header(HeaderKey::Album { key, activity }, cancellation)
                .await?
            {
                Some(Header::Album(view)) => Some(*view),
                _ => None,
            },
        )
    }

    pub async fn artist(
        &self,
        uri: &str,
        album_artist: bool,
        activity: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<ArtistView>> {
        self.loaded().await;
        let Some(Item::Artist { key, .. }) = self.item(uri) else {
            return Ok(None);
        };
        let header = HeaderKey::Artist {
            key,
            album_artist,
            activity,
        };
        Ok(match self.header(header, cancellation).await? {
            Some(Header::Artist(view)) => Some(*view),
            _ => None,
        })
    }

    /// Playlist rows by key, in the order asked. Unnamed playlists are skipped.
    pub async fn playlists(
        &self,
        keys: &[PlaylistKey],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<PlaylistRow>> {
        let items = keys
            .iter()
            .map(|key| Item::Playlist(*key))
            .collect::<Vec<_>>();
        Ok(self
            .rows_of(&items, true, cancellation)
            .await?
            .into_iter()
            .filter_map(|row| match row {
                Row::Playlist(PlaylistListRow::Playlist(row)) => Some(row),
                _ => None,
            })
            .collect())
    }
}

/// Track keys by media URI, in the order asked. URIs the Store does not hold are skipped.
pub(crate) async fn track_keys_on(
    connection: &mut sqlx::SqliteConnection,
    uris: &[String],
) -> LibraryResult<Vec<TrackKey>> {
    Ok(sqlx::query_scalar("SELECT track.track_key FROM json_each(?1) requested JOIN tracks track ON track.media_uri=requested.value ORDER BY CAST(requested.key AS INTEGER)")
        .bind(serde_json::to_string(uris)?)
        .fetch_all(connection)
        .await?)
}

impl CatalogKind {
    /// The kind a collection URI names.
    fn of(word: &str) -> Option<Self> {
        Some(match word {
            "album" => Self::Album,
            "artist" => Self::Artist,
            "genre" => Self::Genre,
            "mood" => Self::Mood,
            "folder" => Self::Folder,
            _ => return None,
        })
    }

    fn item(self, key: i64) -> Option<Item> {
        Some(match self {
            Self::Album => Item::Album(AlbumKey::from_raw(key)),
            Self::Artist => Item::Artist {
                key: ArtistKey::from_raw(key),
                album_artist: false,
            },
            Self::Genre => Item::Genre(GenreKey::from_raw(key)),
            Self::Mood => Item::Mood(MoodKey::from_raw(key)),
            Self::Folder => Item::Folder(FolderKey::from_raw(key)),
            Self::Playlist => Item::Playlist(PlaylistKey::from_raw(key)),
            Self::SmartPlaylist => Item::Smart(crate::SmartPlaylistKey::from_raw(key)),
            Self::PlaylistFolder => Item::PlaylistFolder(crate::PlaylistFolderKey::from_raw(key)),
            Self::Track => return None,
        })
    }
}

impl Snapshot {
    /// Keys of a source's collections of one kind by object id, in the order asked, from one
    /// pass over the family. Track object ids are not resident; unnamed playlists have none.
    pub(crate) fn object_keys(
        &self,
        kind: CatalogKind,
        source: u16,
        objects: &[&str],
    ) -> Vec<Option<i64>> {
        let mut found = objects
            .iter()
            .map(|object| (*object, None))
            .collect::<HashMap<_, Option<i64>>>();
        let mut look = |owner: u16, object: StrId, key: i64| {
            if owner == source
                && let Some(slot) = found.get_mut(self.text(object))
            {
                slot.get_or_insert(key);
            }
        };
        match kind {
            CatalogKind::Album => {
                for (_, album) in self.albums.iter() {
                    look(album.source, album.object, i64::from(album.key));
                }
            }
            CatalogKind::Artist => {
                for (_, artist) in self.artists.iter() {
                    look(artist.source, artist.object, i64::from(artist.key));
                }
            }
            CatalogKind::Genre | CatalogKind::Mood => {
                let wanted = if kind == CatalogKind::Genre {
                    TagKind::Genre
                } else {
                    TagKind::Mood
                };
                for (_, tag) in self.tags.iter().filter(|(_, tag)| tag.kind() == wanted) {
                    look(tag.source, tag.object, i64::from(tag.key));
                }
            }
            CatalogKind::Folder => {
                for (_, folder) in self.folders.iter() {
                    look(folder.source, folder.object, i64::from(folder.key));
                }
            }
            CatalogKind::Playlist => {
                for (_, playlist) in self.playlists.iter() {
                    if !self.text(playlist.name).is_empty() {
                        look(playlist.source, playlist.object, playlist.key);
                    }
                }
            }
            // Smart playlists and playlist folders belong to no source.
            CatalogKind::SmartPlaylist if source == crate::catalog::snapshot::NO_SOURCE => {
                for smart in self.smart_playlists.values() {
                    if let Some(slot) = found.get_mut(smart.object.as_str()) {
                        slot.get_or_insert(smart.key);
                    }
                }
            }
            CatalogKind::PlaylistFolder if source == crate::catalog::snapshot::NO_SOURCE => {
                for folder in self.playlist_folders.values() {
                    if let Some(slot) = found.get_mut(folder.object.as_str()) {
                        slot.get_or_insert(folder.key);
                    }
                }
            }
            CatalogKind::SmartPlaylist | CatalogKind::PlaylistFolder | CatalogKind::Track => {}
        }
        objects.iter().map(|object| found[object]).collect()
    }
}

impl Order {
    /// The listens of a History order.
    fn listens(&self) -> &[Listen] {
        match &self.0.members {
            Members::History(listens) => listens,
            _ => &[],
        }
    }

    /// The track of the n-th listen of a History order, while one is left.
    pub(crate) fn listen_track(&self, index: u32) -> Option<TrackKey> {
        self.listens().get(index as usize)?.track
    }
}
