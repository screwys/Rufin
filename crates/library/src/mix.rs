//! Picks tracks or whole albums at random from the resident Catalog, as Mix asks.

use std::collections::HashSet;

use crate::catalog::snapshot::{Packed, TrackRec};
use crate::catalog::views::{Context, known};
use crate::radio::{queued_tracks, require_radio_bounds};
use crate::{
    AlbumKey, Catalog, FolderKey, GenreKey, LibraryResult, ReadCancellation, SourceKey, TrackKey,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum PlayedFilter {
    #[default]
    All,
    Unplayed,
    Played,
    /// Not played in the last six months, or never.
    NotPlayedRecently,
    /// Added in the last 30 days.
    RecentlyAdded,
}

const RECENT_PLAY_SECONDS: i64 = 180 * 86_400;
const RECENT_ADDITION_SECONDS: i64 = 30 * 86_400;

#[derive(Clone, Debug, PartialEq)]
pub struct MixCriteria {
    pub min_year: Option<i64>,
    pub max_year: Option<i64>,
    pub genre: Option<GenreKey>,
    pub played: PlayedFilter,
    /// Whole albums, each of whose tracks matches, in place of tracks.
    pub albums: bool,
}

/// What a random pick chose, in the order it plays.
#[derive(Clone, Debug, PartialEq)]
pub enum MixPick {
    Tracks(Vec<TrackKey>),
    Albums(Vec<AlbumKey>),
}

impl Catalog {
    /// `requested` tracks, or whole albums, of a source that match the criteria at `now` (unix
    /// seconds), chosen evenly as `variation` picks.
    /// The music folder narrows the tracks; an album counts with a track in it and every track
    /// matching. Queued tracks are left out, and albums holding one.
    pub async fn mix_pick(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        criteria: &MixCriteria,
        requested: usize,
        now: i64,
        variation: i64,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<MixPick> {
        require_radio_bounds(0, requested)?;
        let queued = {
            let mut connection = self.acquire_cancellable(cancellation).await?;
            queued_tracks(&mut connection).await?
        };
        self.loaded().await;
        let snapshot = self.snapshot();
        let context = Context::new(&snapshot);
        let none = || {
            if criteria.albums {
                MixPick::Albums(Vec::new())
            } else {
                MixPick::Tracks(Vec::new())
            }
        };
        let Some(owner) = snapshot.source_slot(source) else {
            return Ok(none());
        };
        let folder = match folder {
            Some(folder) => match snapshot.folders.slot(folder.raw()) {
                Some(slot) => Some(slot),
                None => return Ok(none()),
            },
            None => None,
        };
        let genre = match criteria.genre {
            Some(genre) => match snapshot.tags.slot(genre.raw()) {
                Some(slot) => Some(slot),
                None => return Ok(none()),
            },
            None => None,
        };
        let queued = queued.into_iter().collect::<HashSet<_>>();
        let year = |track: &TrackRec| (track.year != i16::MIN).then_some(i64::from(track.year));
        let matches = |slot: u32, track: &TrackRec| {
            let played = context.played(slot, track);
            !queued.contains(&i64::from(track.key))
                && criteria
                    .min_year
                    .is_none_or(|min| year(track).is_some_and(|year| year >= min))
                && criteria
                    .max_year
                    .is_none_or(|max| year(track).is_some_and(|year| year <= max))
                && genre.is_none_or(|genre| snapshot.track_runs(track)[2].contains(&genre))
                && match criteria.played {
                    PlayedFilter::All => true,
                    PlayedFilter::Unplayed => played.plays == 0,
                    PlayedFilter::Played => played.plays > 0,
                    PlayedFilter::NotPlayedRecently => played
                        .last
                        .is_none_or(|last| i64::from(last) < now - RECENT_PLAY_SECONDS),
                    PlayedFilter::RecentlyAdded => known(track.added)
                        .is_some_and(|added| i64::from(added) >= now - RECENT_ADDITION_SECONDS),
                }
        };
        let in_folder = |track: &TrackRec| {
            folder.is_none_or(|folder| snapshot.track_runs(track)[3].contains(&folder))
        };
        let tracks = snapshot
            .tracks
            .iter()
            .filter(|(_, track)| track.source == owner && track.indexed());
        let seed = variation as u64;
        if !criteria.albums {
            let chosen = sampled(
                tracks.filter(|(slot, track)| in_folder(track) && matches(*slot, track)),
                requested,
                seed,
            );
            return Ok(MixPick::Tracks(
                chosen
                    .into_iter()
                    .map(|(_, track)| TrackKey::from_raw(i64::from(track.key)))
                    .collect(),
            ));
        }
        // Each album's slot with whether a track is in the folder and whether all match.
        let mut albums = std::collections::HashMap::<u32, (bool, bool)>::new();
        for (slot, track) in tracks {
            if let Some(album) = context.album_of(track) {
                let (inside, all) = albums.entry(album).or_insert((false, true));
                *inside |= in_folder(track);
                *all = *all && matches(slot, track);
            }
        }
        let mut eligible = albums
            .into_iter()
            .filter(|(_, (inside, all))| *inside && *all)
            .map(|(album, _)| album)
            .collect::<Vec<_>>();
        // The map's order differs between runs; the variation alone picks.
        eligible.sort_unstable();
        Ok(MixPick::Albums(
            sampled(eligible.into_iter(), requested, seed)
                .into_iter()
                .filter_map(|album| snapshot.albums.key_of(album))
                .map(AlbumKey::from_raw)
                .collect(),
        ))
    }
}

/// The next of a stream of evenly spread numbers.
pub(crate) fn splitmix(seed: &mut u64) -> u64 {
    *seed = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut n = *seed;
    n = (n ^ (n >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    n = (n ^ (n >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    n ^ (n >> 31)
}

/// `count` of `items`, each as likely as the others, in a shuffled order: one pass that holds
/// only the chosen.
pub(crate) fn sampled<T>(items: impl Iterator<Item = T>, count: usize, mut seed: u64) -> Vec<T> {
    let mut next = || splitmix(&mut seed);
    let mut chosen = Vec::with_capacity(count);
    for (seen, item) in items.enumerate() {
        if chosen.len() < count {
            chosen.push(item);
        } else {
            let at = ((u128::from(next()) * (seen as u128 + 1)) >> 64) as usize;
            if at < count {
                chosen[at] = item;
            }
        }
    }
    crate::shuffle_order(&mut chosen, next());
    chosen
}
