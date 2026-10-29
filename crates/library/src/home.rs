//! One bounded Home read: provider sections from the Store, Rufin's own blocks from the
//! resident Catalog.

use serde::{Deserialize, Serialize};
use sqlx::Connection;

use crate::catalog::snapshot::AlbumRec;
use crate::catalog::snapshot::Packed;
use crate::catalog::views::Context;
use crate::{
    AlbumKey, AlbumRow, Catalog, FolderKey, Item, LibraryResult, ReadCancellation, Row, SourceKey,
    TagRow, TrackKey, TrackRow,
};

const HOME_LIMIT: i64 = 24;

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
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HomeProviderSection {
    pub section_id: String,
    pub title: Option<String>,
    pub rows: HomeSectionRows,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HomePage {
    pub showcase: Option<HomeAlbumRow>,
    pub explore: Vec<HomeAlbumRow>,
    pub most_played: HomeSectionRows,
    pub newly_added: HomeSectionRows,
    pub recently_played: HomeSectionRows,
    pub recently_released: HomeSectionRows,
    pub genres: Vec<TagRow>,
    pub provider_sections: Vec<HomeProviderSection>,
}

/// Album keys and titles of each Home block, chosen from the resident Catalog.
#[derive(Default)]
struct Chosen {
    showcase: Vec<(AlbumKey, String)>,
    explore: Vec<(AlbumKey, String)>,
    most_played: Vec<(TrackKey, String)>,
    recently_played: Vec<(TrackKey, String)>,
    newly_added: Vec<(AlbumKey, String)>,
    recently_released: Vec<(AlbumKey, String)>,
}

impl Catalog {
    pub async fn home_page(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        showcase_variation: i64,
        explore_variation: i64,
        blocks: &[HomeBlockKind],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<HomePage> {
        let (_permit, mut connection) = self.acquire_loaded(cancellation).await?;
        let mut transaction = connection.begin().await?;
        // Provider sections by id: title, then track and album entries in position order.
        let mut provider = std::collections::BTreeMap::<
            String,
            (Option<String>, Vec<(i64, String)>, Vec<(i64, String)>),
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
                _ => {}
            }
        }
        let (chosen, sections, genres) = {
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
            let albums = snapshot
                .albums
                .iter()
                .filter(|(slot, album)| {
                    album.source == owner && derived.albums[*slot as usize].tracks > 0
                })
                .collect::<Vec<_>>();
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
            let sections = provider
                .into_iter()
                .map(|(section, (title, tracks, albums))| {
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
                    (section, (title, tracks, albums))
                })
                .collect::<std::collections::BTreeMap<_, _>>();
            let mut chosen = Chosen::default();
            if blocks.contains(&HomeBlockKind::Showcase) && !albums.is_empty() {
                let mut ordered = albums.iter().map(|(_, album)| *album).collect::<Vec<_>>();
                ordered.sort_by_key(|album| album.key);
                let album = ordered[showcase_variation.rem_euclid(ordered.len() as i64) as usize];
                chosen
                    .showcase
                    .push((AlbumKey::from_raw(i64::from(album.key)), title(album)));
            }
            if blocks.contains(&HomeBlockKind::Explore) {
                // A fixed shuffle of album keys, entered at a point the variation picks.
                let pivot = explore_album_pivot(source, explore_variation);
                let mut ordered = albums
                    .iter()
                    .map(|(_, album)| {
                        (
                            (i64::from(album.key).wrapping_mul(1_103_515_245)) % 2_147_483_647,
                            *album,
                        )
                    })
                    .collect::<Vec<_>>();
                ordered.sort_by_key(|(hash, album)| (*hash, album.key));
                let split = ordered.partition_point(|(hash, _)| *hash < pivot);
                ordered.rotate_left(split);
                chosen.explore = ordered
                    .into_iter()
                    .take(HOME_LIMIT as usize)
                    .map(|(_, album)| (AlbumKey::from_raw(i64::from(album.key)), title(album)))
                    .collect();
            }
            let tracks = || {
                snapshot.tracks.iter().filter(|(_, track)| {
                    track.source == owner
                        && track.indexed()
                        && scope
                            .is_none_or(|folder| snapshot.track_runs(track)[3].contains(&folder))
                })
            };
            if blocks.contains(&HomeBlockKind::MostPlayed) {
                let mut played = tracks()
                    .map(|(slot, track)| (context.played(slot, track).plays, track))
                    .filter(|(plays, _)| *plays > 0)
                    .map(|(plays, track)| {
                        (
                            std::cmp::Reverse(plays),
                            snapshot.track_sort(track),
                            track.key,
                            track,
                        )
                    })
                    .collect::<Vec<_>>();
                played.sort_by(|a, b| (&a.0, &a.1, a.2).cmp(&(&b.0, &b.1, b.2)));
                chosen.most_played = played
                    .into_iter()
                    .take(HOME_LIMIT as usize)
                    .map(|(.., track)| {
                        (
                            TrackKey::from_raw(i64::from(track.key)),
                            snapshot.text(track.title).to_owned(),
                        )
                    })
                    .collect();
            }
            if blocks.contains(&HomeBlockKind::RecentlyPlayed) {
                let mut played = tracks()
                    .filter_map(|(slot, track)| {
                        Some((
                            std::cmp::Reverse(context.played(slot, track).last?),
                            track.key,
                            track,
                        ))
                    })
                    .collect::<Vec<_>>();
                played.sort_by_key(|(last, key, _)| (*last, *key));
                chosen.recently_played = played
                    .into_iter()
                    .take(HOME_LIMIT as usize)
                    .map(|(.., track)| {
                        (
                            TrackKey::from_raw(i64::from(track.key)),
                            snapshot.text(track.title).to_owned(),
                        )
                    })
                    .collect();
            }
            if blocks.contains(&HomeBlockKind::NewlyAdded) {
                let mut added = albums.iter().map(|(_, album)| *album).collect::<Vec<_>>();
                added.sort_by(|a, b| {
                    (a.added == u32::MAX)
                        .cmp(&(b.added == u32::MAX))
                        .then(b.added.cmp(&a.added))
                        .then_with(|| snapshot.album_sort(a).cmp(&snapshot.album_sort(b)))
                        .then(a.key.cmp(&b.key))
                });
                chosen.newly_added = added
                    .into_iter()
                    .take(HOME_LIMIT as usize)
                    .map(|album| (AlbumKey::from_raw(i64::from(album.key)), title(album)))
                    .collect();
            }
            if blocks.contains(&HomeBlockKind::RecentlyReleased) {
                let mut released = albums
                    .iter()
                    .map(|(_, album)| *album)
                    .filter(|album| {
                        album.release != 0 || (album.year != i16::MIN && album.year != 0)
                    })
                    .collect::<Vec<_>>();
                let dated = |value: u32| (value != 0).then_some(value);
                let year = |value: i16| (value != i16::MIN).then_some(value);
                released.sort_by(|a, b| {
                    dated(a.release)
                        .is_none()
                        .cmp(&dated(b.release).is_none())
                        .then(b.release.cmp(&a.release))
                        .then(year(a.year).is_none().cmp(&year(b.year).is_none()))
                        .then(b.year.cmp(&a.year))
                        .then_with(|| snapshot.album_sort(a).cmp(&snapshot.album_sort(b)))
                        .then(a.key.cmp(&b.key))
                });
                chosen.recently_released = released
                    .into_iter()
                    .take(HOME_LIMIT as usize)
                    .map(|album| (AlbumKey::from_raw(i64::from(album.key)), title(album)))
                    .collect();
            }
            (chosen, sections, blocks.contains(&HomeBlockKind::Genres))
        };
        let mut sections = sections;
        // A provider section replaces Rufin's own block when it has rows.
        let mut provided = |kind: HomeBlockKind, section: &str| {
            sections
                .remove(section)
                .filter(|(_, tracks, albums)| {
                    blocks.contains(&kind) && (!tracks.is_empty() || !albums.is_empty())
                })
                .map(|(_, tracks, albums)| (tracks, albums))
        };
        let most_played = provided(HomeBlockKind::MostPlayed, "most-played")
            .unwrap_or((chosen.most_played, Vec::new()));
        let newly_added = provided(HomeBlockKind::NewlyAdded, "newly-added")
            .unwrap_or((Vec::new(), chosen.newly_added));
        let recently_played = provided(HomeBlockKind::RecentlyPlayed, "recently-played")
            .unwrap_or((chosen.recently_played, Vec::new()));
        let recently_released = provided(HomeBlockKind::RecentlyReleased, "recently-released")
            .unwrap_or((Vec::new(), chosen.recently_released));
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
            .filter(|(section, _)| {
                !matches!(
                    section.as_str(),
                    "most-played" | "newly-added" | "recently-played" | "recently-released"
                )
            })
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
        for (_, (_, tracked, albumed)) in &others {
            items.extend(tracks(tracked));
            items.extend(albums(albumed));
        }
        items.extend(genres.iter().copied());
        let mut seen = std::collections::HashSet::new();
        items.retain(|item| seen.insert(*item));
        let shape = crate::read::Shape::of(None);
        let rows = self
            .fetch_on(
                &mut transaction,
                [(&items, &[]), (&[], &[])],
                None,
                &shape,
                true,
            )
            .await?
            .rows;
        let mut track_rows = std::collections::HashMap::new();
        let mut album_rows = std::collections::HashMap::new();
        let mut tag_rows = std::collections::HashMap::new();
        for row in rows {
            match row {
                Row::Track(row) => {
                    track_rows.insert(row.track_key, row);
                }
                Row::Album(row) => {
                    album_rows.insert(row.album_key, row);
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
                }
            };
        let page = HomePage {
            showcase: home_albums(chosen.showcase).pop(),
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
                    |(section_id, (title, tracks, albums))| HomeProviderSection {
                        section_id,
                        title,
                        rows: section_rows((tracks, albums)),
                    },
                )
                .collect(),
        };
        transaction.commit().await?;
        Ok(page)
    }
}

fn explore_album_pivot(source: SourceKey, variation: i64) -> i64 {
    let mut mixed = (variation as u64) ^ (source.raw() as u64).rotate_left(17);
    mixed = mixed.wrapping_add(0x9e37_79b9_7f4a_7c15);
    mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    mixed ^= mixed >> 31;
    (mixed % 2_147_483_647) as i64
}
