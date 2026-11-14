//! One bounded Home read: provider sections from the Store, Rufin's own blocks from the
//! resident Catalog.

use serde::{Deserialize, Serialize};
use sqlx::Connection;

use crate::catalog::snapshot::Packed;
use crate::catalog::snapshot::{AlbumRec, TrackRec};
use crate::catalog::views::Context;
use crate::{
    AlbumKey, AlbumRow, ArtistKey, ArtistRow, Catalog, FolderKey, Item, LibraryResult,
    ReadCancellation, Row, SourceKey, TagRow, TrackKey, TrackRow,
};

const HOME_LIMIT: i64 = 24;

/// The first rows of `items` in `order`, sorted, without sorting the rest.
fn first<T>(mut items: Vec<T>, order: impl Fn(&T, &T) -> std::cmp::Ordering) -> Vec<T> {
    let limit = HOME_LIMIT as usize;
    if items.len() > limit {
        items.select_nth_unstable_by(limit - 1, &order);
        items.truncate(limit);
    }
    items.sort_by(order);
    items
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum HomeBlockKind {
    Showcase,
    Explore,
    MostPlayed,
    NewlyAdded,
    RecentlyPlayed,
    RecentlyReleased,
    Genres,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum HomeSectionKind {
    Explore,
    MostPlayed,
    NewlyAdded,
    RecentlyPlayed,
    RecentlyReleased,
}

impl HomeBlockKind {
    pub const fn all() -> [Self; 7] {
        [
            Self::Showcase,
            Self::Explore,
            Self::MostPlayed,
            Self::NewlyAdded,
            Self::RecentlyPlayed,
            Self::RecentlyReleased,
            Self::Genres,
        ]
    }

    pub const fn section_kind(self) -> Option<HomeSectionKind> {
        match self {
            Self::Explore => Some(HomeSectionKind::Explore),
            Self::MostPlayed => Some(HomeSectionKind::MostPlayed),
            Self::NewlyAdded => Some(HomeSectionKind::NewlyAdded),
            Self::RecentlyPlayed => Some(HomeSectionKind::RecentlyPlayed),
            Self::RecentlyReleased => Some(HomeSectionKind::RecentlyReleased),
            Self::Showcase | Self::Genres => None,
        }
    }
}

impl HomeSectionKind {
    /// The sections a source provides, in the order it lists them.
    pub const SOURCE_SECTIONS: [Self; 4] = [
        Self::MostPlayed,
        Self::NewlyAdded,
        Self::RecentlyPlayed,
        Self::RecentlyReleased,
    ];

    /// The id a source's own section of this kind is stored under. Explore is Rufin's only.
    pub const fn source_section(self) -> Option<&'static str> {
        match self {
            Self::Explore => None,
            Self::MostPlayed => Some("most-played"),
            Self::NewlyAdded => Some("newly-added"),
            Self::RecentlyPlayed => Some("recently-played"),
            Self::RecentlyReleased => Some("recently-released"),
        }
    }
}

/// Whether `section` holds one of Rufin's blocks as the server reads it, rather than a section
/// of the server's own.
fn rufin_section(section: &str) -> bool {
    HomeSectionKind::SOURCE_SECTIONS
        .iter()
        .any(|kind| kind.source_section() == Some(section))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum HomeEntryKind {
    Track,
    Album,
    Artist,
    Playlist,
}

impl HomeEntryKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Track => "track",
            Self::Album => "album",
            Self::Artist => "artist",
            Self::Playlist => "playlist",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HomeEntryInput {
    pub position: i64,
    pub kind: HomeEntryKind,
    pub entity_object_id: String,
    pub title: String,
    pub subtitle: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HomeTrackRow {
    pub track: TrackRow,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HomeAlbumRow {
    pub album: AlbumRow,
    pub title: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HomeSectionRows {
    pub tracks: Vec<HomeTrackRow>,
    pub albums: Vec<HomeAlbumRow>,
    pub artists: Vec<ArtistRow>,
}

impl HomeSectionRows {
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty() && self.albums.is_empty() && self.artists.is_empty()
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HomeProviderSection {
    pub section_id: String,
    pub title: Option<String>,
    pub rows: HomeSectionRows,
}

/// The showcase album and its neighbours in the order its steps walk; with one album all
/// three are that album.
#[derive(Clone, Debug, PartialEq)]
pub struct HomeShowcase {
    pub previous: HomeAlbumRow,
    pub current: HomeAlbumRow,
    pub next: HomeAlbumRow,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HomePage {
    pub showcase: Option<HomeShowcase>,
    pub explore: Vec<HomeAlbumRow>,
    pub most_played: HomeSectionRows,
    pub newly_added: HomeSectionRows,
    pub recently_played: HomeSectionRows,
    pub recently_released: HomeSectionRows,
    pub genres: Vec<TagRow>,
    pub provider_sections: Vec<HomeProviderSection>,
    /// Whether a play changes the page: it shows Rufin's own Most played or Recently played.
    pub reads_plays: bool,
}

/// Album keys and titles of the showcase and Explore, chosen from the resident Catalog.
#[derive(Default)]
struct Chosen {
    showcase: Vec<(AlbumKey, String)>,
    explore: Vec<(AlbumKey, String)>,
}

/// A block's tracks and albums, each with the title Home gives it.
type BlockRows = (Vec<(TrackKey, String)>, Vec<(AlbumKey, String)>);

impl Catalog {
    pub async fn home_page(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        showcase_position: i64,
        explore_variation: i64,
        blocks: &[HomeBlockKind],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<HomePage> {
        let mut connection = self.acquire_loaded(cancellation).await?;
        let mut transaction = connection.begin().await?;
        // Provider sections by id: title, then track, album and artist entries in position order.
        let mut provider = std::collections::BTreeMap::<
            String,
            (
                Option<String>,
                Vec<(i64, String)>,
                Vec<(i64, String)>,
                Vec<i64>,
            ),
        >::new();
        for (section, section_title, kind, key, title) in sqlx::query_as::<_, (String, Option<String>, String, i64, String)>(
            "SELECT section_id,section_title,entity_kind,entity_key,title FROM home_entries WHERE source_key=?1 ORDER BY section_id,position",
        )
        .bind(source)
        .fetch_all(&mut *transaction)
        .await?
        {
            let entry = provider.entry(section).or_default();
            entry.0 = entry.0.take().max(section_title);
            match kind.as_str() {
                "track" => entry.1.push((key, title)),
                "album" => entry.2.push((key, title)),
                "artist" => entry.3.push(key),
                _ => {}
            }
        }
        // A server that lists its own sections, as Plex does, leads Home with them: of Rufin's
        // blocks only the showcase stays.
        let showcase_only = [HomeBlockKind::Showcase];
        let blocks = if provider.keys().any(|section| !rufin_section(section)) {
            &showcase_only[..usize::from(blocks.contains(&HomeBlockKind::Showcase))]
        } else {
            blocks
        };
        let (chosen, rows, reads_plays, sections, genres) = {
            let snapshot = self.snapshot();
            let context = Context::new(&snapshot);
            let Some(owner) = snapshot.source_slot(source) else {
                return Ok(HomePage::default());
            };
            let scope = match folder {
                Some(folder) => match snapshot.folders.slot(folder.raw()) {
                    Some(slot) => Some(slot),
                    None => return Ok(HomePage::default()),
                },
                None => None,
            };
            let derived = self.derived(&context, scope);
            // Newly added and Recently released sort the selected source's albums with tracks
            // in the music folder; the showcase and Explore walk the slots instead.
            let albums = || {
                snapshot.albums.iter().filter_map(|(slot, album)| {
                    (album.source == owner && derived.albums[slot as usize].tracks > 0)
                        .then_some(album)
                })
            };
            let title = |album: &AlbumRec| snapshot.text(album.title).to_owned();
            let visible_track = |key: i64| {
                snapshot.tracks.get(key).is_some_and(|track| {
                    track.indexed()
                        && scope
                            .is_none_or(|folder| snapshot.track_runs(track)[3].contains(&folder))
                })
            };
            let visible_album = |key: i64| {
                snapshot
                    .albums
                    .slot(key)
                    .is_some_and(|slot| derived.albums[slot as usize].tracks > 0)
            };
            // An artist counts as an album artist when it has album-artist tracks.
            let visible_artist = |key: i64| {
                let slot = snapshot.artists.slot(key)? as usize;
                let album_artist = derived.artists[1][slot].tracks > 0;
                (album_artist || derived.artists[0][slot].tracks > 0).then_some(Item::Artist {
                    key: ArtistKey::from_raw(key),
                    album_artist,
                })
            };
            let mut sections = provider
                .into_iter()
                .map(|(section, (title, tracks, albums, artists))| {
                    let tracks = tracks
                        .into_iter()
                        .filter(|(key, _)| visible_track(*key))
                        .take(HOME_LIMIT as usize)
                        .map(|(key, title)| (TrackKey::from_raw(key), title))
                        .collect::<Vec<_>>();
                    let albums = albums
                        .into_iter()
                        .filter(|(key, _)| visible_album(*key))
                        .take(HOME_LIMIT as usize)
                        .map(|(key, title)| (AlbumKey::from_raw(key), title))
                        .collect::<Vec<_>>();
                    let artists = artists
                        .into_iter()
                        .filter_map(visible_artist)
                        .take(HOME_LIMIT as usize)
                        .collect::<Vec<_>>();
                    (section, (title, tracks, albums, artists))
                })
                .collect::<std::collections::BTreeMap<_, _>>();
            let mut chosen = Chosen::default();
            let walk = AlbumWalk::new(&snapshot, owner, &derived);
            if blocks.contains(&HomeBlockKind::Showcase) {
                chosen.showcase = walk
                    .showcase(showcase_position)
                    .map(|positions| positions.map(|position| walk.member(position)).to_vec())
                    .unwrap_or_default();
            }
            if blocks.contains(&HomeBlockKind::Explore) {
                // The walk entered at a point the variation picks.
                chosen.explore = walk
                    .from(explore_album_pivot(source, explore_variation))
                    .take(HOME_LIMIT as usize)
                    .map(|position| walk.member(position))
                    .collect();
            }
            // A provider section replaces Rufin's own block when it has rows. It holds the
            // server's top rows of the whole library, so a selected folder keeps Rufin's own,
            // which is chosen only where it shows.
            let mut provided = |kind: HomeBlockKind| -> Option<BlockRows> {
                kind.section_kind()
                    .filter(|_| folder.is_none())
                    .and_then(HomeSectionKind::source_section)
                    .and_then(|section| sections.remove(section))
                    .filter(|(_, tracks, albums, _)| {
                        blocks.contains(&kind) && (!tracks.is_empty() || !albums.is_empty())
                    })
                    .map(|(_, tracks, albums, _)| (tracks, albums))
            };
            // The selected source's tracks in the music folder that hold plays or source
            // counts, each with its plays.
            let played = || {
                snapshot.stats.keys().filter_map(|slot| {
                    let track = snapshot.tracks.live(*slot)?;
                    (track.source == owner
                        && track.indexed()
                        && scope
                            .is_none_or(|folder| snapshot.track_runs(track)[3].contains(&folder)))
                    .then(|| (context.played(*slot, track), track))
                })
            };
            let track_rows = |tracks: Vec<&TrackRec>| -> BlockRows {
                let rows = tracks.into_iter().map(|track| {
                    (
                        TrackKey::from_raw(i64::from(track.key)),
                        snapshot.text(track.title).to_owned(),
                    )
                });
                (rows.collect(), Vec::new())
            };
            let album_rows = |albums: Vec<&AlbumRec>| -> BlockRows {
                let rows = albums
                    .into_iter()
                    .map(|album| (AlbumKey::from_raw(i64::from(album.key)), title(album)));
                (Vec::new(), rows.collect())
            };
            let mut reads_plays = false;
            let most_played = match provided(HomeBlockKind::MostPlayed) {
                Some(rows) => rows,
                None if blocks.contains(&HomeBlockKind::MostPlayed) => {
                    reads_plays = true;
                    let played = played()
                        .map(|(played, track)| (played.plays, track))
                        .filter(|(plays, _)| *plays > 0)
                        .collect::<Vec<_>>();
                    let order = |a: &(u32, &TrackRec), b: &(u32, &TrackRec)| {
                        (b.0.cmp(&a.0))
                            .then_with(|| snapshot.track_sort(a.1).cmp(&snapshot.track_sort(b.1)))
                            .then(a.1.key.cmp(&b.1.key))
                    };
                    track_rows(first(played, order).into_iter().map(|row| row.1).collect())
                }
                None => BlockRows::default(),
            };
            let recently_played = match provided(HomeBlockKind::RecentlyPlayed) {
                Some(rows) => rows,
                None if blocks.contains(&HomeBlockKind::RecentlyPlayed) => {
                    reads_plays = true;
                    let played = played()
                        .filter_map(|(played, track)| {
                            Some(((std::cmp::Reverse(played.last?), track.key), track))
                        })
                        .collect::<Vec<_>>();
                    track_rows(
                        first(played, |a, b| a.0.cmp(&b.0))
                            .into_iter()
                            .map(|row| row.1)
                            .collect(),
                    )
                }
                None => BlockRows::default(),
            };
            let newly_added = match provided(HomeBlockKind::NewlyAdded) {
                Some(rows) => rows,
                None if blocks.contains(&HomeBlockKind::NewlyAdded) => {
                    let order = |a: &&AlbumRec, b: &&AlbumRec| {
                        (a.added == u32::MAX)
                            .cmp(&(b.added == u32::MAX))
                            .then(b.added.cmp(&a.added))
                            .then_with(|| snapshot.album_sort(a).cmp(&snapshot.album_sort(b)))
                            .then(a.key.cmp(&b.key))
                    };
                    album_rows(first(albums().collect(), order))
                }
                None => BlockRows::default(),
            };
            let recently_released = match provided(HomeBlockKind::RecentlyReleased) {
                Some(rows) => rows,
                None if blocks.contains(&HomeBlockKind::RecentlyReleased) => {
                    let released = albums()
                        .filter(|album| {
                            album.release != 0 || (album.year != i16::MIN && album.year != 0)
                        })
                        .collect::<Vec<_>>();
                    let dated = |value: u32| (value != 0).then_some(value);
                    let year = |value: i16| (value != i16::MIN).then_some(value);
                    let order = |a: &&AlbumRec, b: &&AlbumRec| {
                        dated(a.release)
                            .is_none()
                            .cmp(&dated(b.release).is_none())
                            .then(b.release.cmp(&a.release))
                            .then(year(a.year).is_none().cmp(&year(b.year).is_none()))
                            .then(b.year.cmp(&a.year))
                            .then_with(|| snapshot.album_sort(a).cmp(&snapshot.album_sort(b)))
                            .then(a.key.cmp(&b.key))
                    };
                    album_rows(first(released, order))
                }
                None => BlockRows::default(),
            };
            (
                chosen,
                [most_played, newly_added, recently_played, recently_released],
                reads_plays,
                sections,
                blocks.contains(&HomeBlockKind::Genres),
            )
        };
        let [most_played, newly_added, recently_played, recently_released] = rows;
        let genres = if genres {
            let read = crate::ListRead {
                folder,
                descending: true,
                ..crate::ListRead::new(
                    crate::ListKind::Genres,
                    Some(source),
                    crate::LibraryField::SongCount,
                )
            };
            self.order_on(&mut transaction, &crate::Read::List(read))
                .await?
                .items(0..12)
        } else {
            Vec::new()
        };
        let others = sections
            .into_iter()
            .filter(|(section, _)| !rufin_section(section))
            .take(12)
            .collect::<Vec<_>>();
        // Every block's rows in one statement.
        let tracks = |members: &[(TrackKey, String)]| {
            members
                .iter()
                .map(|(key, _)| Item::Track {
                    key: *key,
                    entry: None,
                })
                .collect::<Vec<_>>()
        };
        let albums = |members: &[(AlbumKey, String)]| {
            members
                .iter()
                .map(|(key, _)| Item::Album(*key))
                .collect::<Vec<_>>()
        };
        let mut items = albums(&chosen.showcase);
        items.extend(albums(&chosen.explore));
        for (tracked, albumed) in [
            &most_played,
            &newly_added,
            &recently_played,
            &recently_released,
        ] {
            items.extend(tracks(tracked));
            items.extend(albums(albumed));
        }
        for (_, (_, tracked, albumed, artists)) in &others {
            items.extend(tracks(tracked));
            items.extend(albums(albumed));
            items.extend(artists.iter().copied());
        }
        items.extend(genres.iter().copied());
        let mut seen = std::collections::HashSet::new();
        items.retain(|item| seen.insert(*item));
        // Each row counts in the music folder as the one rule narrows its kind.
        let shape = crate::read::Shape {
            folder,
            ..crate::read::Shape::of(None)
        };
        let rows = self
            .rows_of_on(&mut transaction, &items, &shape, true)
            .await?;
        let mut track_rows = std::collections::HashMap::new();
        let mut album_rows = std::collections::HashMap::new();
        let mut artist_rows = std::collections::HashMap::new();
        let mut tag_rows = std::collections::HashMap::new();
        for row in rows {
            match row {
                Row::Track(row) => {
                    track_rows.insert(row.track_key, row);
                }
                Row::Album(row) => {
                    album_rows.insert(row.album_key, row);
                }
                Row::Artist(row) => {
                    artist_rows.insert(row.artist_key, row);
                }
                Row::Tag(row) => {
                    tag_rows.insert(row.key, row);
                }
                _ => {}
            }
        }
        let home_tracks = |members: Vec<(TrackKey, String)>| {
            members
                .into_iter()
                .filter_map(|(key, title)| {
                    Some(HomeTrackRow {
                        track: track_rows.get(&key)?.clone(),
                        title,
                    })
                })
                .collect::<Vec<_>>()
        };
        let home_albums = |members: Vec<(AlbumKey, String)>| {
            members
                .into_iter()
                .filter_map(|(key, title)| {
                    let album = album_rows.get(&key)?.clone();
                    let title = if title.is_empty() {
                        album.title.clone()
                    } else {
                        title
                    };
                    Some(HomeAlbumRow { album, title })
                })
                .collect::<Vec<_>>()
        };
        let section_rows =
            |(tracks, albums): (Vec<(TrackKey, String)>, Vec<(AlbumKey, String)>)| {
                HomeSectionRows {
                    tracks: home_tracks(tracks),
                    albums: home_albums(albums),
                    artists: Vec::new(),
                }
            };
        let showcase = showcase_rows(chosen.showcase, &album_rows);
        let page = HomePage {
            showcase,
            explore: home_albums(chosen.explore),
            most_played: section_rows(most_played),
            newly_added: section_rows(newly_added),
            recently_played: section_rows(recently_played),
            recently_released: section_rows(recently_released),
            genres: genres
                .into_iter()
                .filter_map(|item| match item {
                    Item::Genre(key) => tag_rows.get(&key.raw()).cloned(),
                    _ => None,
                })
                .collect(),
            provider_sections: others
                .into_iter()
                .map(
                    |(section_id, (title, tracks, albums, artists))| HomeProviderSection {
                        section_id,
                        title,
                        rows: HomeSectionRows {
                            artists: artists
                                .iter()
                                .filter_map(|item| match item {
                                    Item::Artist { key, .. } => artist_rows.get(key).cloned(),
                                    _ => None,
                                })
                                .collect(),
                            ..section_rows((tracks, albums))
                        },
                    },
                )
                .collect(),
            reads_plays,
        };
        transaction.commit().await?;
        Ok(page)
    }
}

impl Catalog {
    /// Steps Home's showcase from `position` one album forward or back. Answers the position it
    /// lands on, which the next step starts from, and the showcase there: three picks and one
    /// read of their rows, not a Home read.
    pub async fn step_home_showcase(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        position: i64,
        forward: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<(i64, Option<HomeShowcase>)> {
        let mut connection = self.acquire_loaded(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let picked = {
            let snapshot = self.snapshot();
            let context = Context::new(&snapshot);
            let owner = snapshot.source_slot(source);
            let scope = folder.map(|folder| snapshot.folders.slot(folder.raw()));
            match (owner, scope) {
                (Some(owner), None | Some(Some(_))) => {
                    let derived = self.derived(&context, scope.flatten());
                    let walk = AlbumWalk::new(&snapshot, owner, &derived);
                    let step = if forward { 1 } else { -1 };
                    walk.land(position, 1)
                        .and_then(|current| walk.land(current + step, step))
                        .and_then(|landed| Some((landed, walk.showcase(landed)?)))
                        .map(|(landed, positions)| {
                            (
                                landed,
                                positions.map(|position| walk.member(position)).to_vec(),
                            )
                        })
                }
                _ => None,
            }
        };
        let Some((landed, members)) = picked else {
            return Ok((position, None));
        };
        let items = members
            .iter()
            .map(|(key, _)| Item::Album(*key))
            .collect::<Vec<_>>();
        let shape = crate::read::Shape {
            folder,
            ..crate::read::Shape::of(None)
        };
        let rows = self
            .rows_of_on(&mut transaction, &items, &shape, true)
            .await?;
        transaction.commit().await?;
        let album_rows = rows
            .into_iter()
            .filter_map(|row| match row {
                Row::Album(row) => Some((row.album_key, row)),
                _ => None,
            })
            .collect();
        Ok((landed, showcase_rows(members, &album_rows)))
    }
}

/// The showcase of the previous, current and next album `members`: none without its current
/// row; a neighbour without its row shows the current album.
fn showcase_rows(
    members: Vec<(AlbumKey, String)>,
    rows: &std::collections::HashMap<AlbumKey, AlbumRow>,
) -> Option<HomeShowcase> {
    let row = |(key, title): (AlbumKey, String)| {
        let album = rows.get(&key)?.clone();
        let title = if title.is_empty() {
            album.title.clone()
        } else {
            title
        };
        Some(HomeAlbumRow { album, title })
    };
    let [previous, current, next] = <[_; 3]>::try_from(members).ok()?;
    let current = row(current)?;
    let beside = |member| row(member).unwrap_or_else(|| current.clone());
    Some(HomeShowcase {
        previous: beside(previous),
        next: beside(next),
        current,
    })
}

/// A walk over the Catalog's album slots that visits every slot once a cycle: position `v`
/// is slot `v × stride mod n`, the stride coprime with `n` and near `n × 0.618`, so albums
/// imported together do not come up together. Only the selected source's albums with tracks
/// in the music folder count; the others are passed over. No list is built.
struct AlbumWalk<'a> {
    snapshot: &'a crate::catalog::snapshot::Snapshot,
    owner: u16,
    derived: &'a crate::catalog::views::Derived,
    slots: i64,
    stride: i64,
}

impl<'a> AlbumWalk<'a> {
    fn new(
        snapshot: &'a crate::catalog::snapshot::Snapshot,
        owner: u16,
        derived: &'a crate::catalog::views::Derived,
    ) -> Self {
        let slots = snapshot.albums.records.len() as i64;
        let mut stride = ((slots as f64 * 0.618) as i64).max(1);
        while gcd(stride, slots.max(1)) != 1 {
            stride += 1;
        }
        Self {
            snapshot,
            owner,
            derived,
            slots,
            stride,
        }
    }

    fn slot(&self, position: i64) -> u32 {
        (position.rem_euclid(self.slots) * self.stride % self.slots) as u32
    }

    fn counts(&self, position: i64) -> bool {
        let slot = self.slot(position);
        self.snapshot
            .albums
            .live(slot)
            .is_some_and(|album| album.source == self.owner)
            && self
                .derived
                .albums
                .get(slot as usize)
                .is_some_and(|album| album.tracks > 0)
    }

    /// The first counted position from `position` going `step` (1 or −1); none within a cycle
    /// means no album counts.
    fn land(&self, position: i64, step: i64) -> Option<i64> {
        (0..self.slots)
            .map(|offset| position + offset * step)
            .find(|position| self.counts(*position))
    }

    /// The counted positions from `position` on, each once.
    fn from(&self, position: i64) -> impl Iterator<Item = i64> + '_ {
        (0..self.slots)
            .map(move |offset| position + offset)
            .filter(|position| self.counts(*position))
    }

    /// The album shown at `position` and the ones before and after it in the walk.
    fn showcase(&self, position: i64) -> Option<[i64; 3]> {
        let current = self.land(position, 1)?;
        let previous = self.land(current - 1, -1)?;
        let next = self.land(current + 1, 1)?;
        Some([previous, current, next])
    }

    fn member(&self, position: i64) -> (AlbumKey, String) {
        let album = &self.snapshot.albums.records[self.slot(position) as usize];
        (
            AlbumKey::from_raw(i64::from(album.key)),
            self.snapshot.text(album.title).to_owned(),
        )
    }
}

fn gcd(mut a: i64, mut b: i64) -> i64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

fn explore_album_pivot(source: SourceKey, variation: i64) -> i64 {
    let mut mixed = (variation as u64) ^ (source.raw() as u64).rotate_left(17);
    mixed = mixed.wrapping_add(0x9e37_79b9_7f4a_7c15);
    mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    mixed ^= mixed >> 31;
    (mixed % 2_147_483_647) as i64
}
