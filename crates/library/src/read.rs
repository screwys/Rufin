//! What a list asks and what it gets: a `Read` names a list, its `Order` comes from the
//! resident Catalog, and the rows of a window are built from residents and one keyed statement.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use crate::catalog::fetch::{self, Got, Want};
use crate::catalog::snapshot::{
    COMPILATION, CUE, DOWNLOADED, METADATA_WRITABLE, NONE, PICTURE, Packed, SOURCE_OWNED, Snapshot,
    WRITABLE,
};
use crate::catalog::strings::StrId;
use crate::catalog::views::{Art, Context, Derived, Played, Totals, favorite, rating};
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
    /// A playlist's entries: track and entry keys.
    Entries(Box<[(u32, u32)]>),
    History(Box<[Listen]>),
    /// Albums, artists, genres, moods or folders.
    Keys(Box<[u32]>),
    /// Playlists or smart playlists, with their folders as negative keys.
    Playlists(Box<[i64]>),
    AlbumTracks {
        spans: Box<[AlbumSpan]>,
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

    /// Bytes the order cache counts against its budget.
    pub(crate) fn bytes(&self) -> usize {
        match self {
            Self::Tracks(keys) | Self::Keys(keys) => keys.len() * 4,
            Self::Entries(entries) => entries.len() * 8,
            Self::History(listens) => listens.len() * 512,
            Self::Playlists(keys) => keys.len() * 8,
            Self::AlbumTracks { spans, tracks } => {
                spans.len() * std::mem::size_of::<AlbumSpan>() + tracks.len() * 4
            }
        }
    }
}

impl Order {
    pub fn read(&self) -> &Read {
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
                .map(|(track, entry)| Item::Track {
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
                        index: index as u32,
                    }
                })
                .collect(),
        }
    }

    /// An artist's albums by release class, for ArtistAlbums orders.
    pub fn release_counts(&self) -> Option<[usize; 6]> {
        self.0.counts
    }

    pub fn album_spans(&self) -> Option<&[AlbumSpan]> {
        match &self.0.members {
            Members::AlbumTracks { spans, .. } => Some(spans),
            _ => None,
        }
    }

    /// Everything the order lists, or the ranges asked, for playing or queueing.
    pub fn selection(&self, ranges: Option<Vec<Range<usize>>>) -> Option<MemberSelection> {
        match &self.0.read {
            Read::Members(read) => Some(MemberSelection::Members {
                read: read.clone(),
                ranges,
            }),
            Read::List(read) | Read::AlbumTracks(read) => Some(MemberSelection::Parents {
                read: read.clone(),
                ranges,
            }),
            Read::Folders(_) => None,
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
    pub folder_key: FolderKey,
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
/// playlist folder's totals come from, the time smart playlists are evaluated at, and the
/// playlist whose entries the rows are.
#[derive(Clone)]
pub(crate) struct Shape {
    pub(crate) activity: bool,
    pub(crate) now: i64,
    pub(crate) list: Option<ListRead>,
    pub(crate) playlist: Option<PlaylistKey>,
}

impl Shape {
    pub(crate) fn of(read: Option<&Read>) -> Self {
        let mut shape = Self {
            activity: false,
            now: chrono::Utc::now().timestamp(),
            list: None,
            playlist: None,
        };
        match read {
            Some(Read::List(read) | Read::AlbumTracks(read)) => {
                shape.activity = read.activity;
                if let ListKind::SmartPlaylists { now } = read.kind {
                    shape.now = now;
                }
                shape.list = Some(read.clone());
            }
            Some(Read::Members(read)) => {
                shape.activity = read.activity;
                if let Collection::Playlist(key) = read.collection {
                    shape.playlist = Some(key);
                }
            }
            _ => {}
        }
        shape
    }
}

/// Where a row's picture comes from.
#[derive(Clone, Debug)]
enum Cover {
    None,
    Art(Art),
    Tag(i64),
    Folder(i64),
    Playlist {
        smart: bool,
        key: i64,
        source_id: Option<String>,
        object_id: String,
    },
}

/// A row before the statement filled it.
struct Draft {
    row: Row,
    cover: Cover,
    /// The tag whose mosaic covers it, when it has no picture of its own.
    mosaic: Option<i64>,
    /// The listen a History row shows its track as.
    listen: Option<ListenWrite>,
}

/// Rows of a plan, in the order asked.
#[derive(Default)]
struct Plan {
    rows: Vec<Draft>,
    favorites: Vec<Draft>,
    header: Option<HeaderDraft>,
}

enum HeaderDraft {
    Album(Box<AlbumView>, Art),
    Artist(Box<ArtistView>, Art),
    Row(Box<Draft>),
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

fn year(value: i16) -> Option<i64> {
    (value != i16::MIN).then_some(i64::from(value))
}

fn known(value: u32) -> Option<i64> {
    (value != NONE).then_some(i64::from(value))
}

/// Displayed play facts of an album or artist: its source's own counts, else its members'
/// when the list asks for them.
fn played(native: [u32; 2], totals: &Totals, activity: bool) -> (i64, Option<i64>) {
    let (plays, last) = if activity {
        (Some(totals.plays as i64), totals.last_played.map(i64::from))
    } else {
        (None, None)
    };
    (
        known(native[0]).or(plays).unwrap_or(0),
        known(native[1]).or(last),
    )
}

fn state(flags: u16, user: u8, native: u8, plays: (i64, Option<i64>)) -> TrackState {
    TrackState {
        favorite: favorite(flags).unwrap_or(false),
        rating: rating(user, native).map(|rating| i64::from(rating / 10)),
        last_played: plays.1,
        play_count: plays.0,
        skip_count: 0,
    }
}

/// Builds rows from residents and names what the statement reads for them.
struct Builder<'a> {
    catalog: &'a Catalog,
    context: Context<'a>,
    snapshot: &'a Snapshot,
    covers: Arc<Derived>,
    derived: Arc<Derived>,
    shape: &'a Shape,
    art: bool,
    want: Want,
    /// Listens by smart playlist window.
    inputs: &'a crate::catalog::views::Inputs,
    /// Entry positions by entry key, read once the first entry row asks.
    positions: Option<HashMap<i64, u32>>,
}

impl Builder<'_> {
    /// Each entry's index in its playlist's order: of the rows' playlist, else of every one.
    fn positions(&mut self) -> &HashMap<i64, u32> {
        let snapshot = self.snapshot;
        let playlist = self.shape.playlist;
        self.positions.get_or_insert_with(|| {
            let mut positions = HashMap::new();
            for (_, record) in snapshot.playlists.iter() {
                if playlist.is_some_and(|key| key.raw() != record.key) {
                    continue;
                }
                for (position, entry) in record.entries.iter().enumerate() {
                    positions.insert(entry.key, position as u32);
                }
            }
            positions
        })
    }

    fn draft(&mut self, row: Row, cover: Cover) -> Draft {
        if self.art {
            match &cover {
                Cover::None => {}
                Cover::Art(art) => self.want.art(*art),
                Cover::Tag(key) => {
                    self.want.tag_art.insert(*key);
                }
                Cover::Folder(key) => {
                    self.want.folder_art.insert(*key);
                }
                Cover::Playlist { smart, key, .. } => {
                    if *smart {
                        self.want.smart_art.insert(*key);
                    } else {
                        self.want.playlist_art.insert(*key);
                    }
                }
            }
        }
        Draft {
            row,
            cover,
            mosaic: None,
            listen: None,
        }
    }

    fn track(&mut self, key: TrackKey) -> Option<(TrackRow, Art)> {
        let snapshot = self.snapshot;
        let slot = snapshot.tracks.slot(key.raw())?;
        let record = &snapshot.tracks.records[slot as usize];
        let played = self.context.played(slot, record);
        let source = snapshot
            .source_key(record.source)
            .and_then(|key| snapshot.source(key));
        let album = self
            .context
            .album_of(record)
            .and_then(|slot| snapshot.albums.live(slot));
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
        self.want.tracks.insert(key.raw());
        let row = TrackRow {
            track_key: key,
            entry: None,
            source_key: source.map(|source| source.key),
            source_id: source.map(|source| source.object.clone()),
            source_name: source.map(|source| source.name.clone()).unwrap_or_default(),
            media_uri: String::new(),
            object_id: None,
            title: text(snapshot, record.title),
            artist: text(snapshot, record.artist),
            album: text(snapshot, record.album_title),
            duration_millis: i64::from(record.duration_ms),
            disc_number: i64::from(record.disc),
            track_number: i64::from(record.number),
            year: year(record.year),
            bpm: (record.bpm != u16::MAX).then_some(i64::from(record.bpm)),
            date_added: known(record.added),
            source_format: optional(snapshot, record.format),
            state: TrackState {
                favorite: favorite(record.flags).unwrap_or(false),
                rating: rating(record.rating, record.native_rating)
                    .map(|rating| i64::from(rating / 10)),
                last_played: played.last.map(i64::from),
                play_count: i64::from(played.plays),
                skip_count: i64::from(played.skips),
            },
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
            release_date: None,
            source_path: None,
            audio: AudioProperties::default(),
            artwork_binding: None,
        };
        Some((row, self.context.track_art(&self.covers, record)))
    }

    fn album(&mut self, key: AlbumKey, activity: bool) -> Option<(AlbumRow, Art)> {
        let snapshot = self.snapshot;
        let slot = snapshot.albums.slot(key.raw())?;
        let record = &snapshot.albums.records[slot as usize];
        let mut totals = self.derived.albums[slot as usize];
        totals.albums = u32::from(totals.tracks > 0);
        let runs = snapshot.album_runs(record);
        self.want.album_texts.insert(key.raw());
        let row = AlbumRow {
            album_key: key,
            source_key: snapshot.source_key(record.source)?,
            object_id: text(snapshot, record.object),
            media_uri: entity_uri(snapshot, record.source, "album", record.object),
            title: text(snapshot, record.title),
            artist: text(snapshot, record.artist),
            sort_text: text(snapshot, record.sort),
            year: year(record.year),
            release_date: None,
            date_added: None,
            is_compilation: record.flags & COMPILATION != 0,
            release_class: AlbumReleaseClass::of(record.flags),
            state: state(
                record.flags,
                record.rating,
                record.native_rating,
                played(record.native, &totals, activity),
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
            artwork_binding: None,
        };
        Some((row, self.context.album_art(&self.derived, slot)))
    }

    fn artist(
        &mut self,
        key: ArtistKey,
        album_artist: bool,
        activity: bool,
    ) -> Option<(ArtistRow, Art)> {
        let snapshot = self.snapshot;
        let slot = snapshot.artists.slot(key.raw())?;
        let record = &snapshot.artists.records[slot as usize];
        let totals = self.derived.artists[usize::from(album_artist)][slot as usize];
        let row = ArtistRow {
            artist_key: key,
            album_artist,
            source_key: snapshot.source_key(record.source)?,
            object_id: text(snapshot, record.object),
            media_uri: entity_uri(snapshot, record.source, "artist", record.object),
            name: text(snapshot, record.name),
            sort_text: text(snapshot, record.sort),
            state: state(
                record.flags,
                record.rating,
                record.native_rating,
                played(record.native, &totals, activity),
            ),
            summary: totals.summary(),
            artwork_binding: None,
        };
        Some((row, self.context.artist_art(&self.derived, slot)))
    }

    fn tag(&mut self, key: i64, kind: TagKind) -> Option<Draft> {
        let snapshot = self.snapshot;
        let slot = snapshot.tags.slot(key)?;
        let record = &snapshot.tags.records[slot as usize];
        if record.kind() != kind {
            return None;
        }
        let source = snapshot
            .source_key(record.source)
            .and_then(|key| snapshot.source(key))?;
        let row = TagRow {
            key,
            kind,
            source_key: source.key,
            source_id: source.object.clone(),
            object_id: text(snapshot, record.object),
            media_uri: entity_uri(snapshot, record.source, kind.table().name, record.object),
            name: text(snapshot, record.name),
            summary: self.derived.tags[slot as usize].summary(),
            artwork_binding: None,
            representative_artwork: Vec::new(),
        };
        // A mood shows its mosaic; a genre its own picture, else its mosaic.
        let cover = if kind == TagKind::Genre {
            Cover::Tag(key)
        } else {
            Cover::None
        };
        let mut draft = self.draft(Row::Tag(row), cover);
        if self.art && (kind == TagKind::Mood || record.flags & PICTURE == 0) {
            self.want.tag_mosaics.insert(key);
            draft.mosaic = Some(key);
        }
        Some(draft)
    }

    fn folder(&mut self, key: FolderKey) -> Option<Draft> {
        let snapshot = self.snapshot;
        let slot = snapshot.folders.slot(key.raw())?;
        let record = &snapshot.folders.records[slot as usize];
        let row = FolderRow {
            folder_key: key,
            object_id: text(snapshot, record.object),
            name: text(snapshot, record.name),
            summary: self.derived.folders[slot as usize].summary(),
            artwork_binding: None,
        };
        Some(self.draft(Row::Folder(row), Cover::Folder(key.raw())))
    }

    fn playlist(&mut self, key: PlaylistKey) -> Option<(PlaylistRow, Cover)> {
        let snapshot = self.snapshot;
        let record = snapshot.playlists.get(key.raw())?;
        if snapshot.text(record.name).is_empty() {
            return None;
        }
        let source = snapshot
            .source_key(record.source)
            .and_then(|key| snapshot.source(key));
        let totals = self.context.playlist_totals(record, None);
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
        let summary = totals.summary();
        let object_id = text(snapshot, record.object);
        let source_id = source.map(|source| source.object.clone());
        let row = PlaylistRow {
            playlist_key: key,
            source_key: source.map(|source| source.key),
            source_id: source_id.clone(),
            source_owned: record.flags & SOURCE_OWNED != 0,
            object_id: object_id.clone(),
            name: text(snapshot, record.name),
            writable: record.flags & WRITABLE != 0,
            metadata_writable: record.flags & METADATA_WRITABLE != 0,
            artwork_binding: None,
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
        };
        Some((
            row,
            Cover::Playlist {
                smart: false,
                key: key.raw(),
                source_id,
                object_id,
            },
        ))
    }

    fn smart(&mut self, key: SmartPlaylistKey) -> LibraryResult<Option<(SmartPlaylistRow, Cover)>> {
        let Some(smart) = self.snapshot.smart_playlists.get(&key.raw()) else {
            return Ok(None);
        };
        let mut definition = smart.definition.clone();
        crate::smart_playlists::normalize_definition(&mut definition)?;
        let opened = self
            .catalog
            .opened_smart(key.raw(), self.shape.now, &smart.definition);
        let summary = self
            .context
            .smart_totals(
                &smart.definition,
                self.inputs,
                opened.as_deref().map(Vec::as_slice),
            )
            .summary();
        Ok(Some((
            SmartPlaylistRow {
                smart_playlist_key: key,
                object_id: smart.object.clone(),
                name: smart.name.clone(),
                definition,
                position: smart.position,
                track_count: summary.track_count,
                duration_millis: summary.duration_millis,
                downloaded_count: summary.downloaded_count,
                artwork_binding: None,
            },
            Cover::Playlist {
                smart: true,
                key: key.raw(),
                source_id: None,
                object_id: smart.object.clone(),
            },
        )))
    }

    fn playlist_folder(&mut self, key: PlaylistFolderKey) -> Option<(PlaylistFolderRow, Cover)> {
        let snapshot = self.snapshot;
        let folder = snapshot.playlist_folders.get(&key.raw())?;
        let mut row = PlaylistFolderRow {
            folder_key: key,
            object_id: folder.object.clone(),
            name: folder.name.clone(),
            smart: folder.smart,
            artwork_binding: None,
            track_count: 0,
            duration_millis: 0,
        };
        // Its cover is its first playlist's.
        let cover = if folder.smart {
            for smart in snapshot.smart_playlists.values() {
                if smart.folder == Some(key.raw()) {
                    let opened =
                        self.catalog
                            .opened_smart(smart.key, self.shape.now, &smart.definition);
                    let totals = self.context.smart_totals(
                        &smart.definition,
                        self.inputs,
                        opened.as_deref().map(Vec::as_slice),
                    );
                    row.track_count += i64::from(totals.tracks);
                    row.duration_millis += totals.duration as i64;
                }
            }
            snapshot
                .smart_playlists
                .values()
                .filter(|smart| smart.folder == Some(key.raw()))
                .min_by_key(|smart| smart.key)
                .map(|smart| Cover::Playlist {
                    smart: true,
                    key: smart.key,
                    source_id: None,
                    object_id: smart.object.clone(),
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
            if let Some(totals) = self
                .context
                .playlist_folder_totals(&children)
                .get(&key.raw())
            {
                row.track_count = i64::from(totals.tracks);
                row.duration_millis = totals.duration as i64;
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
                .map(|record| Cover::Playlist {
                    smart: false,
                    key: record.key,
                    source_id: snapshot
                        .source_key(record.source)
                        .and_then(|key| snapshot.source(key))
                        .map(|source| source.object.clone()),
                    object_id: text(snapshot, record.object),
                })
        };
        Some((row, cover.unwrap_or(Cover::None)))
    }

    /// The draft of one item; None when the Catalog no longer holds it.
    fn item(&mut self, item: Item, listens: &[Listen]) -> LibraryResult<Option<Draft>> {
        let activity = self.shape.activity;
        Ok(match item {
            Item::Track { key, entry } => self.track(key).map(|(mut row, art)| {
                row.entry = entry.and_then(|entry| {
                    Some(PlaylistEntry {
                        key: entry,
                        position: *self.positions().get(&entry.raw())?,
                    })
                });
                self.draft(Row::Track(row), Cover::Art(art))
            }),
            Item::AlbumTrack { key, index } => self
                .track(key)
                .map(|(track, art)| self.draft(Row::AlbumTrack { track, index }, Cover::Art(art))),
            Item::Listen(index) => {
                let Some(listen) = listens.get(index as usize) else {
                    return Ok(None);
                };
                let (row, cover) = match listen.track.and_then(|key| self.track(key)) {
                    Some((row, art)) => (row, Cover::Art(art)),
                    None => (
                        TrackRow {
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
                        },
                        Cover::None,
                    ),
                };
                let mut draft = self.draft(Row::Track(row), cover);
                draft.listen = Some(listen.copy.clone());
                Some(draft)
            }
            Item::AlbumHeading(key) => self
                .album(key, activity)
                .map(|(row, art)| self.draft(Row::AlbumHeading(row), Cover::Art(art))),
            Item::Album(key) => self
                .album(key, activity)
                .map(|(row, art)| self.draft(Row::Album(row), Cover::Art(art))),
            Item::Artist { key, album_artist } => self
                .artist(key, album_artist, activity)
                .map(|(row, art)| self.draft(Row::Artist(row), Cover::Art(art))),
            Item::Genre(key) => self.tag(key.raw(), TagKind::Genre),
            Item::Mood(key) => self.tag(key.raw(), TagKind::Mood),
            Item::Folder(key) => self.folder(key),
            Item::Playlist(key) => self.playlist(key).map(|(row, cover)| {
                self.draft(Row::Playlist(PlaylistListRow::Playlist(row)), cover)
            }),
            Item::Smart(key) => self.smart(key)?.map(|(row, cover)| {
                self.draft(Row::Playlist(PlaylistListRow::SmartPlaylist(row)), cover)
            }),
            Item::PlaylistFolder(key) => self
                .playlist_folder(key)
                .map(|(row, cover)| self.draft(Row::Playlist(PlaylistListRow::Folder(row)), cover)),
        })
    }

    fn header(&mut self, key: HeaderKey) -> LibraryResult<Option<HeaderDraft>> {
        Ok(match key {
            HeaderKey::Album { key, activity } => self.album(key, activity).map(|(row, art)| {
                self.want.album_views.insert(key.raw());
                if self.art {
                    self.want.art(art);
                }
                HeaderDraft::Album(Box::new(album_view(row)), art)
            }),
            HeaderKey::Artist {
                key,
                album_artist,
                activity,
            } => self.artist(key, album_artist, activity).map(|(row, art)| {
                self.want.artist_views.insert(key.raw());
                if self.art {
                    self.want.art(art);
                }
                HeaderDraft::Artist(Box::new(artist_view(row)), art)
            }),
            HeaderKey::Playlist(key) => self
                .item(Item::Playlist(key), &[])?
                .map(|draft| HeaderDraft::Row(Box::new(draft))),
            HeaderKey::Smart { key, .. } => self
                .item(Item::Smart(key), &[])?
                .map(|draft| HeaderDraft::Row(Box::new(draft))),
            HeaderKey::Genre(key) => self
                .item(Item::Genre(key), &[])?
                .map(|draft| HeaderDraft::Row(Box::new(draft))),
            HeaderKey::Mood(key) => self
                .item(Item::Mood(key), &[])?
                .map(|draft| HeaderDraft::Row(Box::new(draft))),
        })
    }
}

/// An album row as the start of its full view; the statement reads the rest.
fn album_view(row: AlbumRow) -> AlbumView {
    AlbumView {
        album_key: row.album_key,
        source_key: row.source_key,
        object_id: row.object_id,
        media_uri: row.media_uri,
        metadata: AlbumMetadata::default(),
        state: row.state,
        summary: row.summary,
        links: row.links,
        artwork_binding: None,
    }
}

fn artist_view(row: ArtistRow) -> ArtistView {
    ArtistView {
        artist_key: row.artist_key,
        source_key: row.source_key,
        object_id: row.object_id,
        media_uri: row.media_uri,
        metadata: ArtistMetadata {
            name: row.name,
            sort_text: (!row.sort_text.is_empty()).then_some(row.sort_text),
            musicbrainz_artist_id: None,
        },
        state: row.state,
        summary: row.summary,
        artwork_binding: None,
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

impl Got {
    /// Fills a drafted row with what the statement read.
    fn finish(&self, draft: &mut Draft) -> LibraryResult<()> {
        let binding = match &draft.cover {
            Cover::None => None,
            Cover::Art(art) => self.binding(*art)?,
            Cover::Tag(key) => self.own(&self.tag_art, *key)?,
            Cover::Folder(key) => self.own(&self.folder_art, *key)?,
            Cover::Playlist {
                smart,
                key,
                source_id,
                object_id,
            } => self.playlist(*smart, *key, (source_id.clone(), object_id.clone()))?,
        };
        let track = |row: &mut TrackRow| {
            if let Some(text) = self.tracks.get(&row.track_key.raw()) {
                row.media_uri.clone_from(&text.media_uri);
                row.object_id.clone_from(&text.object_id);
                row.release_date.clone_from(&text.release_date);
                row.source_path.clone_from(&text.source_path);
                row.audio = text.audio.clone();
            }
            if row.title.is_empty() {
                row.title.clone_from(&row.media_uri);
            }
            row.artwork_binding.clone_from(&binding);
        };
        match &mut draft.row {
            Row::Track(row) | Row::AlbumTrack { track: row, .. } => track(row),
            Row::Album(row) | Row::AlbumHeading(row) => {
                if let Some((release, added)) = self.album_texts.get(&row.album_key.raw()) {
                    row.release_date.clone_from(release);
                    row.date_added.clone_from(added);
                }
                row.artwork_binding = binding;
            }
            Row::Artist(row) => row.artwork_binding = binding,
            Row::Tag(row) => row.artwork_binding = binding,
            Row::Folder(row) => row.artwork_binding = binding,
            Row::Playlist(PlaylistListRow::Playlist(row)) => row.artwork_binding = binding,
            Row::Playlist(PlaylistListRow::SmartPlaylist(row)) => row.artwork_binding = binding,
            Row::Playlist(PlaylistListRow::Folder(row)) => row.artwork_binding = binding,
        }
        Ok(())
    }
}

impl Catalog {
    /// Plans rows under one hold of the Catalog.
    fn plan(
        &self,
        windows: [(&[Item], &[Listen]); 2],
        header: Option<HeaderKey>,
        shape: &Shape,
        art: bool,
        inputs: &crate::catalog::views::Inputs,
    ) -> LibraryResult<(Plan, Want)> {
        let snapshot = self.snapshot();
        let context = Context::new(&snapshot);
        let covers = self.covers(&context);
        let derived = self.derived(&context, None);
        let mut shape = shape.clone();
        if let Some(HeaderKey::Smart { now, .. }) = header {
            shape.now = now;
        }
        let mut builder = Builder {
            catalog: self,
            context,
            snapshot: &snapshot,
            covers,
            derived,
            shape: &shape,
            art,
            want: Want::new(true),
            inputs,
            positions: None,
        };
        let mut plan = Plan::default();
        for (index, (items, listens)) in windows.into_iter().enumerate() {
            for item in items {
                if let Some(draft) = builder.item(*item, listens)? {
                    if index == 0 {
                        plan.rows.push(draft);
                    } else {
                        plan.favorites.push(draft);
                    }
                }
            }
        }
        if let Some(key) = header {
            plan.header = builder.header(key)?;
        }
        Ok((plan, builder.want))
    }

    /// Listens in the windows the smart playlists these items total read.
    async fn smart_inputs(
        &self,
        connection: &mut sqlx::SqliteConnection,
        items: impl Iterator<Item = Item>,
        header: Option<HeaderKey>,
        now: i64,
    ) -> LibraryResult<crate::catalog::views::Inputs> {
        let mut inputs = crate::catalog::views::Inputs::default();
        let periods = {
            let snapshot = self.snapshot();
            let mut periods = Vec::new();
            let mut add = |smart: &crate::catalog::snapshot::SmartPlaylist| {
                periods.extend(crate::catalog::views::smart_period(&smart.definition));
            };
            for item in items.chain(header.and_then(|header| match header {
                HeaderKey::Smart { key, .. } => Some(Item::Smart(key)),
                _ => None,
            })) {
                match item {
                    Item::Smart(key) => {
                        if let Some(smart) = snapshot.smart_playlists.get(&key.raw()) {
                            add(smart);
                        }
                    }
                    Item::PlaylistFolder(key) => {
                        for smart in snapshot.smart_playlists.values() {
                            if smart.folder == Some(key.raw()) {
                                add(smart);
                            }
                        }
                    }
                    _ => {}
                }
            }
            periods.sort_unstable();
            periods.dedup();
            periods
        };
        let now = match header {
            Some(HeaderKey::Smart { now, .. }) => now,
            _ => now,
        };
        if periods.is_empty() {
            return Ok(inputs);
        }
        self.read_periods(connection, &mut inputs, periods, now)
            .await?;
        Ok(inputs)
    }

    /// Rows of these windows and a header, read on one connection: residents, then one
    /// statement, then a second only for the mosaics of tags without a picture.
    pub(crate) async fn fetch_on(
        &self,
        connection: &mut sqlx::SqliteConnection,
        windows: [(&[Item], &[Listen]); 2],
        header: Option<HeaderKey>,
        shape: &Shape,
        art: bool,
    ) -> LibraryResult<Fetched> {
        let inputs = self
            .smart_inputs(
                connection,
                windows.iter().flat_map(|(items, _)| items.iter().copied()),
                header,
                shape.now,
            )
            .await?;
        let (mut plan, want) = self.plan(windows, header, shape, art, &inputs)?;
        let got = fetch::run(connection, &want).await?;
        let mosaics = plan
            .rows
            .iter()
            .chain(&plan.favorites)
            .chain(plan.header.iter().filter_map(|header| match header {
                HeaderDraft::Row(draft) => Some(&**draft),
                _ => None,
            }))
            .filter_map(|draft| draft.mosaic)
            .filter_map(|key| Some((key, got.mosaics.get(&key)?)))
            .collect::<Vec<_>>();
        let galleries = if mosaics.is_empty() {
            HashMap::new()
        } else {
            let arts = self.arts(
                mosaics
                    .iter()
                    .flat_map(|(_, picks)| picks.iter())
                    .map(|key| key.map(crate::catalog::views::Owner::Track)),
            );
            let mut bindings = fetch::bindings(connection, &arts).await?.into_iter();
            mosaics
                .iter()
                .map(|(key, picks)| {
                    (
                        *key,
                        bindings
                            .by_ref()
                            .take(picks.len())
                            .flatten()
                            .collect::<Vec<_>>(),
                    )
                })
                .collect()
        };
        let listened = |key: TrackKey| {
            let snapshot = self.snapshot();
            let context = Context::new(&snapshot);
            snapshot
                .tracks
                .slot(key.raw())
                .map(|slot| context.listened(slot))
                .unwrap_or_default()
        };
        let finish = |mut draft: Draft| -> LibraryResult<Row> {
            got.finish(&mut draft)?;
            if let (Row::Track(row), Some(listen)) = (&mut draft.row, &draft.listen) {
                show_listen(row, listen, listened(row.track_key));
            }
            if let (Row::Tag(row), Some(key)) = (&mut draft.row, draft.mosaic)
                && row.artwork_binding.is_none()
            {
                row.representative_artwork = galleries.get(&key).cloned().unwrap_or_default();
            }
            Ok(draft.row)
        };
        let header = match plan.header.take() {
            None => None,
            Some(HeaderDraft::Album(mut view, art)) => {
                if let Some((object_id, media_uri, metadata)) =
                    got.album_views.get(&view.album_key.raw())
                {
                    view.object_id.clone_from(object_id);
                    view.media_uri.clone_from(media_uri);
                    view.metadata = serde_json::from_str(metadata)?;
                }
                view.artwork_binding = got.binding(art)?;
                Some(Header::Album(view))
            }
            Some(HeaderDraft::Artist(mut view, art)) => {
                if let Some((object_id, media_uri, mbid)) =
                    got.artist_views.get(&view.artist_key.raw())
                {
                    view.object_id.clone_from(object_id);
                    view.media_uri.clone_from(media_uri);
                    view.metadata.musicbrainz_artist_id.clone_from(mbid);
                }
                view.artwork_binding = got.binding(art)?;
                Some(Header::Artist(view))
            }
            Some(HeaderDraft::Row(draft)) => Some(match finish(*draft)? {
                Row::Tag(row) => Header::Tag(row),
                Row::Playlist(PlaylistListRow::Playlist(row)) => Header::Playlist(Box::new(row)),
                Row::Playlist(PlaylistListRow::SmartPlaylist(row)) => Header::Smart(Box::new(row)),
                _ => unreachable!("headers are tags and playlists"),
            }),
        };
        Ok(Fetched {
            header,
            rows: plan
                .rows
                .into_iter()
                .map(finish)
                .collect::<LibraryResult<_>>()?,
            favorites: plan
                .favorites
                .into_iter()
                .map(finish)
                .collect::<LibraryResult<_>>()?,
        })
    }

    /// One keyed statement: the window's rows, a detail header and an artist's favorite tracks.
    pub async fn fetch(
        &self,
        fetch: Fetch<'_>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Fetched> {
        fn window<'a>(order: Option<&(&'a Order, Range<usize>)>) -> (Vec<Item>, &'a [Listen]) {
            let Some((order, range)) = order else {
                return (Vec::new(), &[]);
            };
            {
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
                let end = range.end.min(range.start.saturating_add(limit));
                (order.items(range.start..end), order.listens())
            }
        }
        let (rows, listens) = window(fetch.rows.as_ref());
        let (favorites, favorite_listens) = window(fetch.favorites.as_ref());
        let shape = Shape::of(fetch.rows.as_ref().map(|(order, _)| order.read()));
        let (_permit, mut connection) = self.acquire_loaded(cancellation).await?;
        self.fetch_on(
            &mut connection,
            [(&rows, listens), (&favorites, favorite_listens)],
            fetch.header,
            &shape,
            fetch.art,
        )
        .await
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
        let (_permit, mut connection) = self.acquire_loaded(cancellation).await?;
        self.rows_of_on(&mut connection, items, &Shape::of(None), art)
            .await
    }

    pub(crate) async fn rows_of_on(
        &self,
        connection: &mut sqlx::SqliteConnection,
        items: &[Item],
        shape: &Shape,
        art: bool,
    ) -> LibraryResult<Vec<Row>> {
        let items = &items[..items.len().min(WINDOW)];
        Ok(self
            .fetch_on(connection, [(items, &[]), (&[], &[])], None, shape, art)
            .await?
            .rows)
    }

    /// A collection URI's item, from resident object ids. Track URIs are not resident.
    pub fn item(&self, uri: &str) -> Option<Item> {
        let (source, kind, object) = source_entity_parts(uri)?;
        let snapshot = self.snapshot();
        let source = snapshot.source_slot(
            snapshot
                .sources
                .values()
                .find(|known| known.object == source.as_str())?
                .key,
        )?;
        let find = |object_of: &dyn Fn(u32) -> Option<(u16, StrId, i64)>, len: usize| {
            (0..len as u32).find_map(|slot| {
                let (owner, id, key) = object_of(slot)?;
                (owner == source && snapshot.text(id) == object).then_some(key)
            })
        };
        let tag = |wanted: TagKind| {
            find(
                &|slot| {
                    snapshot
                        .tags
                        .live(slot)
                        .filter(|tag| tag.kind() == wanted)
                        .map(|tag| (tag.source, tag.object, i64::from(tag.key)))
                },
                snapshot.tags.records.len(),
            )
        };
        match kind.as_str() {
            "album" => find(
                &|slot| {
                    snapshot
                        .albums
                        .live(slot)
                        .map(|album| (album.source, album.object, i64::from(album.key)))
                },
                snapshot.albums.records.len(),
            )
            .map(|key| Item::Album(AlbumKey::from_raw(key))),
            "artist" => find(
                &|slot| {
                    snapshot
                        .artists
                        .live(slot)
                        .map(|artist| (artist.source, artist.object, i64::from(artist.key)))
                },
                snapshot.artists.records.len(),
            )
            .map(|key| Item::Artist {
                key: ArtistKey::from_raw(key),
                album_artist: false,
            }),
            "folder" => find(
                &|slot| {
                    snapshot
                        .folders
                        .live(slot)
                        .map(|folder| (folder.source, folder.object, i64::from(folder.key)))
                },
                snapshot.folders.records.len(),
            )
            .map(|key| Item::Folder(FolderKey::from_raw(key))),
            "genre" => tag(TagKind::Genre).map(|key| Item::Genre(GenreKey::from_raw(key))),
            "mood" => tag(TagKind::Mood).map(|key| Item::Mood(MoodKey::from_raw(key))),
            _ => None,
        }
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

    /// Full track views by key, in the order asked, on a caller's connection so a Write reads
    /// the tracks it staged. Keys the Store does not hold are skipped; a key asked twice reads
    /// twice.
    pub(crate) async fn track_views_on(
        &self,
        connection: &mut sqlx::SqliteConnection,
        keys: &[TrackKey],
    ) -> LibraryResult<Vec<TrackView>> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let mut want = Want::new(true);
        let resident = {
            let snapshot = self.snapshot();
            let context = Context::new(&snapshot);
            let covers = self.covers(&context);
            keys.iter()
                .map(|key| {
                    want.views.insert(key.raw());
                    let slot = snapshot.tracks.slot(key.raw())?;
                    let record = &snapshot.tracks.records[slot as usize];
                    let played = context.played(slot, record);
                    let art = context.track_art(&covers, record);
                    want.art(art);
                    Some((
                        TrackState {
                            favorite: favorite(record.flags).unwrap_or(false),
                            rating: rating(record.rating, record.native_rating)
                                .map(|rating| i64::from(rating / 10)),
                            last_played: played.last.map(i64::from),
                            play_count: i64::from(played.plays),
                            skip_count: i64::from(played.skips),
                        },
                        record.flags & DOWNLOADED != 0,
                        // The row may name its album: the track is listed, or its album lists
                        // another track.
                        record.indexed()
                            || context
                                .album_of(record)
                                .is_some_and(|album| covers.albums[album as usize].tracks > 0),
                        art,
                    ))
                })
                .collect::<Vec<_>>()
        };
        let got = fetch::run(connection, &want).await?;
        let mut views = Vec::with_capacity(keys.len());
        for (key, resident) in keys.iter().zip(resident) {
            let Some(mut view) = got.views.get(&key.raw()).cloned() else {
                continue;
            };
            if let Some((state, downloaded, album, art)) = resident {
                view.state = state;
                view.access.is_downloaded = downloaded;
                if !album {
                    view.links.album_key = None;
                    view.links.album_media_uri = None;
                }
                view.artwork_binding = got.binding(art)?;
            }
            views.push(view);
        }
        Ok(views)
    }

    /// The one full read of tracks: details, paths, CUE window and MBIDs.
    pub async fn tracks(
        &self,
        keys: &[TrackKey],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackView>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
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
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let keys =
            crate::query::track_keys_by_uris(&mut connection, &uris[..uris.len().min(WINDOW)])
                .await?;
        self.track_views_on(&mut connection, &keys).await
    }

    async fn header_view(
        &self,
        header: HeaderKey,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<Header>> {
        let (_permit, mut connection) = self.acquire_loaded(cancellation).await?;
        Ok(self
            .fetch_on(
                &mut connection,
                [(&[], &[]), (&[], &[])],
                Some(header),
                &Shape::of(None),
                true,
            )
            .await?
            .header)
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
        Ok(self.albums(&[key], activity, cancellation).await?.pop())
    }

    pub async fn albums(
        &self,
        keys: &[AlbumKey],
        activity: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<AlbumView>> {
        let mut views = Vec::new();
        for key in &keys[..keys.len().min(WINDOW)] {
            if let Some(Header::Album(view)) = self
                .header_view(
                    HeaderKey::Album {
                        key: *key,
                        activity,
                    },
                    cancellation,
                )
                .await?
            {
                views.push(*view);
            }
        }
        Ok(views)
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
        Ok(self
            .artists(&[key], album_artist, activity, cancellation)
            .await?
            .pop())
    }

    pub async fn artists(
        &self,
        keys: &[ArtistKey],
        album_artist: bool,
        activity: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<ArtistView>> {
        let mut views = Vec::new();
        for key in &keys[..keys.len().min(WINDOW)] {
            if let Some(Header::Artist(view)) = self
                .header_view(
                    HeaderKey::Artist {
                        key: *key,
                        album_artist,
                        activity,
                    },
                    cancellation,
                )
                .await?
            {
                views.push(*view);
            }
        }
        Ok(views)
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

    pub async fn smart_playlist_rows(
        &self,
        keys: &[SmartPlaylistKey],
        now: i64,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<SmartPlaylistRow>> {
        let items = keys.iter().map(|key| Item::Smart(*key)).collect::<Vec<_>>();
        let (_permit, mut connection) = self.acquire_loaded(cancellation).await?;
        let shape = Shape {
            now,
            ..Shape::of(None)
        };
        Ok(self
            .rows_of_on(&mut connection, &items, &shape, true)
            .await?
            .into_iter()
            .filter_map(|row| match row {
                Row::Playlist(PlaylistListRow::SmartPlaylist(row)) => Some(row),
                _ => None,
            })
            .collect())
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
