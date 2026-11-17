//! Picks tracks or whole albums at random from what a smart playlist definition matches in the
//! resident Catalog, as Mix asks.

use std::collections::HashSet;

use crate::catalog::snapshot::{Packed, TrackRec};
use crate::catalog::views::{Context, Inputs, favorite, smart_period};
use crate::radio::{queued_tracks, require_radio_bounds};
use crate::{
    AlbumKey, Catalog, FolderKey, LibraryResult, ReadCancellation, SmartPlaylistDefinition,
    TrackKey,
};

/// What Mix picks from. The first five are the kinds it always had, saved under their names;
/// the rest are smart playlist templates.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum MixKind {
    #[default]
    All,
    Unplayed,
    Played,
    /// Not played in the last six months, or never.
    NotPlayedRecently,
    /// Added in the last 30 days.
    RecentlyAdded,
    ThisTimeLastYear,
    OnRepeat,
    Rediscover,
    MostPlayed,
    RecentlyPlayed,
    ForgottenFavorites,
    NewAndUnplayed,
    TopRated,
    MostSkipped,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MixCriteria {
    /// The kind's rules with Mix's own filters added, read in the selected source. A limit
    /// cuts the kind to its first tracks in its order, which Mix then picks from.
    pub definition: SmartPlaylistDefinition,
    /// Whole albums in place of tracks.
    pub albums: bool,
    /// Favorites only: favorite tracks, or favorite albums when it picks albums.
    pub favorites: bool,
}

/// What a random pick chose, in the order it plays.
#[derive(Clone, Debug, PartialEq)]
pub enum MixPick {
    Tracks(Vec<TrackKey>),
    Albums(Vec<AlbumKey>),
}

impl Catalog {
    /// `requested` tracks, or whole albums, of what `criteria` matches in music folder `folder`
    /// at `now` (unix seconds), chosen evenly as `variation` picks. Queued tracks are left out,
    /// and albums holding one.
    ///
    /// An album counts with a track in the folder when its tracks are what the kind picks: each
    /// of them for a kind of rules alone, any of them for a kind cut to its first tracks.
    pub async fn mix_pick(
        &self,
        folder: Option<FolderKey>,
        criteria: &MixCriteria,
        requested: usize,
        now: i64,
        variation: i64,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<MixPick> {
        require_radio_bounds(0, requested)?;
        let mut definition = criteria.definition.clone();
        if criteria.favorites && !criteria.albums {
            definition
                .conditions
                .push(crate::SmartPlaylistCondition::Rule(
                    crate::SmartPlaylistRule {
                        field: crate::SmartPlaylistRuleField::Favorite,
                        operator: crate::SmartPlaylistRuleOperator::Is,
                        value: Some(crate::SmartPlaylistRuleValue::Bool(true)),
                    },
                ));
        }
        let definition = &definition;
        let (queued, inputs) = {
            let mut connection = self.acquire_loaded(cancellation).await?;
            let queued = queued_tracks(&mut connection).await?;
            let mut inputs = Inputs::default();
            let periods = smart_period(definition).into_iter().collect();
            self.read_periods(&mut connection, &mut inputs, periods, now)
                .await?;
            (queued.into_iter().collect::<HashSet<_>>(), inputs)
        };
        let snapshot = self.snapshot();
        let context = Context::new(&snapshot);
        let seed = variation as u64;
        let free = |track: &TrackRec| !queued.contains(&i64::from(track.key));
        let Some(folder) = context.folder_slot(folder, None) else {
            return Ok(if criteria.albums {
                MixPick::Albums(Vec::new())
            } else {
                MixPick::Tracks(Vec::new())
            });
        };
        if !criteria.albums {
            let (tracks, _) = context.quick_mix_tracks(definition, &inputs, folder, variation);
            let chosen = sampled(
                tracks
                    .into_iter()
                    .filter_map(|(_, slot)| snapshot.tracks.live(slot).filter(|track| free(track))),
                requested,
                seed,
            );
            return Ok(MixPick::Tracks(
                chosen
                    .into_iter()
                    .map(|track| TrackKey::from_raw(i64::from(track.key)))
                    .collect(),
            ));
        }
        let rules = definition;
        let ranked = rules.limit.is_some() && rules.sort_field != crate::SmartPlaylistSort::Random;
        let (picked, _) = context.quick_mix_tracks(rules, &inputs, None, variation);
        let picked = picked
            .into_iter()
            .map(|(_, slot)| slot)
            .collect::<HashSet<_>>();
        let source = rules
            .source_id
            .as_ref()
            .and_then(|id| {
                snapshot
                    .sources
                    .values()
                    .find(|source| source.object == id.as_str())
            })
            .and_then(|source| snapshot.source_slot(source.key));
        // Each album's slot: whether a track is in the folder, whether one is picked, whether
        // all are, and whether one is queued.
        let mut albums = std::collections::HashMap::<u32, [bool; 4]>::new();
        for (slot, track) in snapshot.tracks.iter() {
            if !track.indexed() || source.is_some_and(|source| track.source != source) {
                continue;
            }
            if let Some(album) = context.album_of(track) {
                let [inside, any, all, queued] =
                    albums.entry(album).or_insert([false, false, true, false]);
                *inside |= context.in_source_folder(track, folder);
                *any |= picked.contains(&slot);
                *all &= picked.contains(&slot);
                *queued |= !free(track);
            }
        }
        let mut eligible = albums
            .into_iter()
            .filter(|(album, [inside, any, all, queued])| {
                *inside
                    && !*queued
                    && if ranked { *any } else { *all }
                    && (!criteria.favorites
                        || favorite(snapshot.albums.records[*album as usize].flags))
            })
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
