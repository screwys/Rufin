//! Selects only the requested bounded Radio or random candidates from the resident Catalog.
//! The persisted queue is the complete exclusion authority; request-local exclusions stay bounded.

use std::collections::{HashMap, HashSet};

use sqlx::{QueryBuilder, Sqlite};

use crate::catalog::snapshot::{Packed, Snapshot, TrackRec};
use crate::catalog::views::{Context, favorite, known, rating, year_of};
use crate::{
    AlbumKey, ArtistKey, Catalog, FolderKey, GenreKey, LibraryError, LibraryResult, PlaylistKey,
    ReadCancellation, SourceKey, TrackKey,
};

const RADIO_LIMIT: usize = 500;
const RADIO_EXTRA_EXCLUSION_LIMIT: usize = 500;

// What a track scores for each way it is close to a radio's seed.
const SAME_ARTIST: f64 = 6.0;
/// For the closest similar artist; the others by their closeness.
const SIMILAR_ARTIST: f64 = 6.0;
/// For genres as rare as `RARE_GENRES` together; commoner ones by their share of that.
const SHARED_GENRE: f64 = 3.0;
const RARE_GENRES: f64 = 0.9;
// What raises a related track's score, as a share of it, so that a far one stays far.
/// For the seed's year, falling to nothing `YEAR_REACH` years away.
const NEAR_YEAR: f64 = 0.35;
const YEAR_REACH: f64 = 10.0;
const LIKED: f64 = 0.15;
const OFTEN_PLAYED: f64 = 0.1;
/// The share of the best score a track needs while closer ones are left.
const CLOSE_ENOUGH: f64 = 0.25;
/// Two stars of five: a rating from one up to here is low.
const LOW_RATING: u8 = 40;
/// A track shorter than this is likely a skit or an interlude.
const SHORT_MILLIS: u32 = 60_000;

/// What a radio starts from. Its serde form is the HTTP API's: `{kind: track, uri}`, or
/// `{kind: album, artist, album_artist, genre or playlist, id}`.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
#[serde(from = "SeedForm", into = "SeedForm")]
pub enum RadioSeed {
    Track(String),
    Album(AlbumKey),
    Artist(ArtistKey),
    AlbumArtist(ArtistKey),
    Genre(GenreKey),
    Playlist(PlaylistKey),
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SeedForm {
    Track { uri: String },
    Album { id: AlbumKey },
    Artist { id: ArtistKey },
    AlbumArtist { id: ArtistKey },
    Genre { id: GenreKey },
    Playlist { id: PlaylistKey },
}

impl From<SeedForm> for RadioSeed {
    fn from(form: SeedForm) -> Self {
        match form {
            SeedForm::Track { uri } => Self::Track(uri),
            SeedForm::Album { id } => Self::Album(id),
            SeedForm::Artist { id } => Self::Artist(id),
            SeedForm::AlbumArtist { id } => Self::AlbumArtist(id),
            SeedForm::Genre { id } => Self::Genre(id),
            SeedForm::Playlist { id } => Self::Playlist(id),
        }
    }
}

impl From<RadioSeed> for SeedForm {
    fn from(seed: RadioSeed) -> Self {
        match seed {
            RadioSeed::Track(uri) => Self::Track { uri },
            RadioSeed::Album(id) => Self::Album { id },
            RadioSeed::Artist(id) => Self::Artist { id },
            RadioSeed::AlbumArtist(id) => Self::AlbumArtist { id },
            RadioSeed::Genre(id) => Self::Genre { id },
            RadioSeed::Playlist(id) => Self::Playlist { id },
        }
    }
}

/// How much of a refill the seed's own artists may take: a fifth, two fifths, or all of it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SameArtist {
    Few,
    #[default]
    Some,
    Many,
}

/// The user's say in what a radio draws.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RadioTaste {
    pub same_artist: SameArtist,
    /// Leaves out tracks rated two stars or lower.
    pub skip_low_rated: bool,
}

/// An artist listeners play alongside another: its MusicBrainz id and name, and how close it
/// is, from 1 for the closest of its list towards 0.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct SimilarArtist {
    pub mbid: String,
    pub name: String,
    pub closeness: f32,
}

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
pub struct RandomCriteria {
    pub min_year: Option<i64>,
    pub max_year: Option<i64>,
    pub genre: Option<GenreKey>,
    pub played: PlayedFilter,
    /// Whole albums, each of whose tracks matches, in place of tracks.
    pub albums: bool,
}

/// What a random pick chose, in the order it plays.
#[derive(Clone, Debug, PartialEq)]
pub enum RandomPick {
    Tracks(Vec<TrackKey>),
    Albums(Vec<AlbumKey>),
}

impl Catalog {
    /// Resolve the seed's catalog owner independently of the selected browse source.
    pub async fn radio_source(
        &self,
        seed: &RadioSeed,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<(SourceKey, crate::SourceId, Option<String>)>> {
        let mut connection = self.acquire_cancellable(cancellation).await?;
        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT sources.source_key,sources.object_id,seed.object_id FROM (",
        );
        match seed {
            RadioSeed::Track(uri) => {
                query
                    .push("SELECT source_key,NULL object_id FROM tracks WHERE media_uri=")
                    .push_bind(uri);
            }
            RadioSeed::Album(key) => {
                query
                    .push("SELECT source_key,object_id FROM albums WHERE album_key=")
                    .push_bind(key);
            }
            RadioSeed::Artist(key) | RadioSeed::AlbumArtist(key) => {
                query
                    .push("SELECT source_key,object_id FROM artists WHERE artist_key=")
                    .push_bind(key);
            }
            RadioSeed::Genre(key) => {
                query
                    .push("SELECT source_key,object_id FROM tags WHERE tag_key=")
                    .push_bind(key);
            }
            RadioSeed::Playlist(key) => {
                query.push("SELECT COALESCE(playlist.source_key,(SELECT track.source_key FROM playlist_entries entry JOIN tracks track USING(track_key) WHERE entry.playlist_key=playlist.playlist_key ORDER BY entry.rank,entry.playlist_entry_key LIMIT 1)) source_key,CASE WHEN playlist.owner='source' THEN playlist.object_id END object_id FROM playlists playlist WHERE playlist.playlist_key=").push_bind(key);
            }
        }
        query.push(") seed JOIN sources ON sources.source_key=seed.source_key");
        let result = query
            .build_query_as::<(SourceKey, String, Option<String>)>()
            .persistent(false)
            .fetch_optional(&mut *connection)
            .await?;
        Ok(result.map(|(key, id, object_id)| (key, crate::SourceId::new(id), object_id)))
    }

    /// Resolve provider recommendations in order, using the same queue and seed exclusions,
    /// and leaving out low-rated tracks where the user skips them.
    pub async fn recommended_track_keys(
        &self,
        source: SourceKey,
        seed: &RadioSeed,
        object_ids: &[String],
        skip_low_rated: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackKey>> {
        if object_ids.is_empty() {
            return Ok(Vec::new());
        }
        require_radio_bounds(0, object_ids.len())?;
        let mut connection = self.acquire_cancellable(cancellation).await?;
        let mut query = QueryBuilder::<Sqlite>::new("WITH requested(object_id,position) AS (");
        query.push_values(object_ids.iter().enumerate(), |mut row, (position, id)| {
            row.push_bind(id).push_bind(position as i64);
        });
        query.push(") SELECT track.track_key FROM requested JOIN tracks track ON track.object_id=requested.object_id WHERE track.source_key=").push_bind(source)
            .push(" AND track.track_key NOT IN (SELECT track_key FROM queue_entries)");
        if skip_low_rated {
            query.push(" AND coalesce((SELECT state.rating FROM user_state state WHERE state.media_uri=track.media_uri),track.native_rating,0) NOT BETWEEN 1 AND ").push_bind(i64::from(LOW_RATING));
        }
        match seed {
            RadioSeed::Track(uri) => {
                query.push(" AND track.media_uri<>").push_bind(uri);
            }
            RadioSeed::Album(key) => {
                query
                    .push(" AND (track.album_key IS NULL OR track.album_key<>")
                    .push_bind(key)
                    .push(")");
            }
            RadioSeed::Playlist(key) => {
                query.push(" AND track.media_uri IS NOT (SELECT seed.media_uri FROM playlist_entries entry JOIN tracks seed USING(track_key) WHERE entry.playlist_key=").push_bind(key)
                    .push(" AND seed.source_key=").push_bind(source).push(" ORDER BY entry.rank,entry.playlist_entry_key LIMIT 1)");
            }
            _ => {}
        }
        query.push(" GROUP BY track.track_key ORDER BY min(requested.position)");
        Ok(query
            .build_query_scalar()
            .persistent(false)
            .fetch_all(&mut *connection)
            .await?)
    }

    /// The names and MusicBrainz ids of a seed's artists in `source`: a track's or album's
    /// artists, or the artist itself. A genre or playlist has none.
    pub async fn radio_seed_artists(
        &self,
        source: SourceKey,
        seed: &RadioSeed,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<(String, Option<String>)>> {
        let mut connection = self.acquire_cancellable(cancellation).await?;
        let artists = match seed {
            RadioSeed::Track(uri) => sqlx::query_as::<_, (String, Option<String>)>("SELECT artist.name,artist.musicbrainz_artist_id FROM tracks track JOIN track_links link ON link.track_key=track.track_key AND link.role=0 JOIN artists artist ON artist.artist_key=link.target WHERE track.source_key=?1 AND track.media_uri=?2 ORDER BY link.position")
                .bind(source)
                .bind(uri),
            RadioSeed::Album(key) => sqlx::query_as("SELECT artist.name,artist.musicbrainz_artist_id FROM album_links link JOIN artists artist ON artist.artist_key=link.target WHERE link.album_key=?2 AND link.role=0 AND artist.source_key=?1 ORDER BY link.position")
                .bind(source)
                .bind(key),
            RadioSeed::Artist(key) | RadioSeed::AlbumArtist(key) => sqlx::query_as(
                "SELECT name,musicbrainz_artist_id FROM artists WHERE source_key=?1 AND artist_key=?2",
            )
            .bind(source)
            .bind(key),
            RadioSeed::Genre(_) | RadioSeed::Playlist(_) => return Ok(Vec::new()),
        };
        Ok(artists
            .fetch_all(&mut *connection)
            .await?
            .into_iter()
            .map(|(name, mbid)| (name, mbid.filter(|mbid| !mbid.is_empty())))
            .collect())
    }

    /// Tracks for a radio or Auto DJ refill, drawn at random by how close each is to the seed.
    ///
    /// A track scores for the seed's artist, for an artist of `similar`, for the seed's genres
    /// by how rare each is in the source, for its year, and for the user's own liking and
    /// plays. A recent play or many skips lower it. The draw keeps to the closer tracks, lets
    /// one artist or album take only part of it, and takes one version of a song. Where the
    /// related tracks run out those limits lift, and then any track fills up. Queued tracks
    /// are left out.
    pub async fn radio_candidates(
        &self,
        source: SourceKey,
        seed: RadioSeed,
        similar: &[SimilarArtist],
        taste: RadioTaste,
        extra_excluded: &[TrackKey],
        requested: usize,
        now: i64,
        variation: i64,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackKey>> {
        require_radio_bounds(extra_excluded.len(), requested)?;
        let (seed_track, queued, named) = {
            let mut connection = self.acquire_cancellable(cancellation).await?;
            let track = match &seed {
                RadioSeed::Track(uri) => {
                    sqlx::query_scalar::<_, i64>(
                        "SELECT track_key FROM tracks WHERE source_key=?1 AND media_uri=?2",
                    )
                    .bind(source)
                    .bind(uri)
                    .fetch_optional(&mut *connection)
                    .await?
                }
                _ => None,
            };
            // The source's artists a similar one can be: those with its id, and those without
            // any, which go by their name.
            let named = if similar.is_empty() {
                Vec::new()
            } else {
                sqlx::query_as::<_, (i64, Option<String>, String)>("SELECT artist_key,musicbrainz_artist_id,name FROM artists WHERE source_key=?1 AND (coalesce(musicbrainz_artist_id,'')='' OR musicbrainz_artist_id IN (SELECT value FROM json_each(?2)))")
                    .bind(source)
                    .bind(serde_json::to_string(
                        &similar.iter().map(|artist| &artist.mbid).collect::<Vec<_>>(),
                    )?)
                    .fetch_all(&mut *connection)
                    .await?
            };
            (track, queued_tracks(&mut connection).await?, named)
        };
        self.loaded().await;
        let snapshot = self.snapshot();
        let context = Context::new(&snapshot);
        let Some(owner) = snapshot.source_slot(source) else {
            return Ok(Vec::new());
        };
        let seed_track = match &seed {
            RadioSeed::Playlist(playlist) => {
                snapshot.playlists.get(playlist.raw()).and_then(|playlist| {
                    playlist
                        .entries
                        .iter()
                        .filter_map(|entry| snapshot.tracks.live(entry.track))
                        .find(|track| track.source == owner)
                        .map(|track| i64::from(track.key))
                })
            }
            _ => seed_track,
        };
        let seed_track = seed_track.and_then(|key| snapshot.tracks.get(key));
        let seed_album = match &seed {
            RadioSeed::Album(key) => snapshot.albums.slot(key.raw()),
            _ => seed_track.and_then(|track| context.album_of(track)),
        };
        let genre = |slot: &u32| {
            snapshot
                .tags
                .live(*slot)
                .is_some_and(|tag| tag.kind() == crate::TagKind::Genre)
        };
        let mut artists = HashSet::new();
        let mut genres = HashSet::new();
        let mut seed_year = None;
        if let Some(track) = seed_track {
            let runs = snapshot.track_runs(track);
            artists.extend(runs[0].iter().copied());
            genres.extend(runs[2].iter().copied().filter(genre));
            seed_year = year_of(track.year);
        }
        if let Some(album) = seed_album.and_then(|album| snapshot.albums.live(album)) {
            let runs = snapshot.album_runs(album);
            artists.extend(runs[0].iter().copied());
            // A track's own genres say more than its album's, which a compilation gathers.
            if genres.is_empty() {
                genres.extend(runs[1].iter().copied());
            }
            seed_year = seed_year.or(year_of(album.year));
        }
        // An album artist's radio goes by whose album a track is on, not by who performs it.
        let mut album_artist = None;
        match &seed {
            RadioSeed::Artist(key) => artists.extend(snapshot.artists.slot(key.raw())),
            RadioSeed::AlbumArtist(key) => album_artist = snapshot.artists.slot(key.raw()),
            RadioSeed::Genre(key) => genres.extend(snapshot.tags.slot(key.raw())),
            _ => {}
        }
        let by_seed_artist = |runs: &[&[u32]; 4]| {
            runs[0].iter().any(|artist| artists.contains(artist))
                || album_artist.is_some_and(|artist| runs[1].contains(&artist))
        };
        let mut closeness = HashMap::<u32, f32>::new();
        if !named.is_empty() {
            let by_id = similar
                .iter()
                .map(|artist| (artist.mbid.as_str(), artist.closeness))
                .collect::<HashMap<_, _>>();
            let mut by_name = HashMap::<String, f32>::new();
            for artist in similar {
                let close = by_name.entry(crate::name_key(&artist.name)).or_default();
                *close = close.max(artist.closeness);
            }
            for (key, mbid, name) in named {
                let close = match mbid.as_deref().filter(|mbid| !mbid.is_empty()) {
                    Some(mbid) => by_id.get(mbid).copied(),
                    None => by_name.get(&crate::name_key(&name)).copied(),
                };
                if let (Some(close), Some(slot)) = (close, snapshot.artists.slot(key)) {
                    closeness.insert(slot, close);
                }
            }
        }
        let excluded = extra_excluded
            .iter()
            .map(|key| key.raw())
            .chain(queued)
            .collect::<HashSet<_>>();
        let low_rated = |track: &TrackRec| {
            taste.skip_low_rated
                && rating(track.rating, track.native_rating)
                    .is_some_and(|rating| (1..=LOW_RATING).contains(&rating))
        };
        let eligible = || {
            snapshot.tracks.iter().filter(|(_, track)| {
                track.source == owner
                    && track.indexed()
                    && !excluded.contains(&i64::from(track.key))
                    && seed_track.is_none_or(|seed| seed.key != track.key)
                    && (!matches!(seed, RadioSeed::Album(_))
                        || seed_album.is_none_or(|album| track.album != album))
                    && !low_rated(track)
            })
        };
        // How many of the source's tracks carry each genre, and which the seed's artists play
        // most: a seed without a genre of its own goes by those.
        let mut total = 0_u32;
        let mut carried = HashMap::<u32, u32>::new();
        let mut played_by_artist = HashMap::<u32, u32>::new();
        for (_, track) in snapshot.tracks.iter() {
            if track.source != owner || !track.indexed() {
                continue;
            }
            total += 1;
            let runs = snapshot.track_runs(track);
            let seeds = genres.is_empty() && by_seed_artist(&runs);
            for tag in genres_of(&snapshot, track, runs[2]) {
                *carried.entry(*tag).or_default() += 1;
                if seeds {
                    *played_by_artist.entry(*tag).or_default() += 1;
                }
            }
        }
        if genres.is_empty() {
            let mut most = played_by_artist.into_iter().collect::<Vec<_>>();
            most.sort_unstable_by_key(|(tag, count)| (std::cmp::Reverse(*count), *tag));
            genres.extend(most.into_iter().take(3).map(|(tag, _)| tag));
        }
        // A genre weighs by how rare it is: one that half the source carries says little.
        let spread = f64::from(total.max(2)).ln();
        let rarity = genres
            .iter()
            .map(|tag| {
                let count = f64::from(carried.get(tag).copied().unwrap_or(1).max(1));
                (
                    *tag,
                    ((f64::from(total.max(2)) / count).ln() / spread).max(0.0),
                )
            })
            .collect::<HashMap<_, _>>();
        let score = |slot: u32, track: &TrackRec| {
            let runs = snapshot.track_runs(track);
            let mut related = if by_seed_artist(&runs) {
                SAME_ARTIST
            } else {
                runs[0]
                    .iter()
                    .filter_map(|artist| closeness.get(artist))
                    .fold(0.0_f32, |closest, close| closest.max(*close)) as f64
                    * SIMILAR_ARTIST
            };
            let (mut first, mut second) = (0.0_f64, 0.0_f64);
            for weight in genres_of(&snapshot, track, runs[2]).filter_map(|tag| rarity.get(tag)) {
                if *weight > first {
                    (first, second) = (*weight, first);
                } else if *weight > second {
                    second = *weight;
                }
            }
            related += SHARED_GENRE * ((first + second) / RARE_GENRES).min(1.0);
            if related <= 0.0 {
                return None;
            }
            let mut score = related;
            if let (Some(seed), Some(year)) = (seed_year, year_of(track.year)) {
                score *= 1.0
                    + NEAR_YEAR
                        * (1.0 - f64::from((seed - year).unsigned_abs()) / YEAR_REACH).max(0.0);
            }
            if favorite(track.flags)
                || rating(track.rating, track.native_rating).is_some_and(|rating| rating >= 80)
            {
                score *= 1.0 + LIKED;
            }
            let played = context.played(slot, track);
            score *= 1.0 + OFTEN_PLAYED * (f64::from(played.plays).ln_1p() / 21_f64.ln()).min(1.0);
            if played.skips >= 3 && played.skips > played.plays {
                score *= 0.5;
            }
            match played.last.map(|last| now - i64::from(last)) {
                Some(since) if since < 86_400 => score *= 0.25,
                Some(since) if since < 7 * 86_400 => score *= 0.6,
                _ => {}
            }
            Some(score)
        };
        // Each related track draws a key by its score, so a closer one comes first more often.
        let mut random = variation as u64;
        let mut best = 0.0_f64;
        let mut related = Vec::new();
        for (slot, track) in eligible() {
            if let Some(score) = score(slot, track) {
                best = best.max(score);
                let chance = ((splitmix(&mut random) >> 11) as f64 + 1.0) / (1_u64 << 53) as f64;
                related.push((chance.ln() / score, score, track));
            }
        }
        let reach = requested * 16 + 256;
        let seed_share = match taste.same_artist {
            SameArtist::Few => requested.div_ceil(5),
            SameArtist::Some => (requested * 2).div_ceil(5),
            SameArtist::Many => requested,
        };
        let artist_share = (requested * 3).div_ceil(20).max(1);
        let album_share = requested.div_ceil(10).max(1);
        let mut chosen = Vec::<&TrackRec>::with_capacity(requested);
        let mut versions = HashSet::new();
        let (mut by_seed, mut by_artist, mut on_album) = (0, HashMap::new(), HashMap::new());
        // Closer tracks under the shares first, then farther ones, then past the shares.
        for (near, limited) in [(true, true), (false, true), (false, false)] {
            if chosen.len() == requested {
                break;
            }
            let floor = if near { best * CLOSE_ENOUGH } else { 0.0 };
            let drawn = closest(
                related
                    .iter()
                    .filter(|(_, score, _)| *score >= floor)
                    .copied()
                    .collect(),
                reach,
            );
            for (_, _, track) in drawn {
                if chosen.len() == requested {
                    break;
                }
                if chosen.iter().any(|taken| taken.key == track.key) {
                    continue;
                }
                let runs = snapshot.track_runs(track);
                let seeds = by_seed_artist(&runs);
                let artist_full = if seeds {
                    by_seed >= seed_share
                } else {
                    runs[0]
                        .iter()
                        .any(|artist| by_artist.get(artist).copied().unwrap_or(0) >= artist_share)
                };
                let album_full = on_album.get(&track.album).copied().unwrap_or(0) >= album_share;
                if limited && (artist_full || album_full) {
                    continue;
                }
                // One version of a song, and no skit while longer tracks are left.
                let title = snapshot.text(track.title);
                if near && ((1..SHORT_MILLIS).contains(&track.duration_ms) || between_songs(title))
                    || !versions.insert((
                        runs[0].first().copied(),
                        crate::name_key(crate::base_title(title)),
                    ))
                {
                    continue;
                }
                if seeds {
                    by_seed += 1;
                } else {
                    for artist in runs[0] {
                        *by_artist.entry(*artist).or_default() += 1;
                    }
                }
                if context.album_of(track).is_some() {
                    *on_album.entry(track.album).or_default() += 1;
                }
                chosen.push(track);
            }
        }
        if chosen.len() < requested {
            let wanted = requested - chosen.len();
            let fill = sampled(
                eligible()
                    .map(|(_, track)| track)
                    .filter(|track| !chosen.iter().any(|taken| taken.key == track.key)),
                wanted,
                splitmix(&mut random),
            );
            chosen.extend(fill);
        }
        Ok(chosen
            .into_iter()
            .map(|track| TrackKey::from_raw(i64::from(track.key)))
            .collect())
    }

    /// `requested` tracks, or whole albums, of a source that match the criteria at `now` (unix
    /// seconds), chosen evenly as `variation` picks.
    /// The music folder narrows the tracks; an album counts with a track in it and every track
    /// matching. Queued tracks are left out, and albums holding one.
    pub async fn random_pick(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        criteria: &RandomCriteria,
        requested: usize,
        now: i64,
        variation: i64,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<RandomPick> {
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
                RandomPick::Albums(Vec::new())
            } else {
                RandomPick::Tracks(Vec::new())
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
            return Ok(RandomPick::Tracks(
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
        Ok(RandomPick::Albums(
            sampled(eligible.into_iter(), requested, seed)
                .into_iter()
                .filter_map(|album| snapshot.albums.key_of(album))
                .map(AlbumKey::from_raw)
                .collect(),
        ))
    }
}

/// A track's genres: its own, `own` among its tags, and its album's, as many servers tag
/// only the album.
fn genres_of<'a>(
    snapshot: &'a Snapshot,
    track: &TrackRec,
    own: &'a [u32],
) -> impl Iterator<Item = &'a u32> {
    let album = snapshot
        .albums
        .live(track.album)
        .map_or(&[][..], |album| snapshot.album_runs(album)[1]);
    own.iter()
        .filter(|tag| {
            snapshot
                .tags
                .live(**tag)
                .is_some_and(|tag| tag.kind() == crate::TagKind::Genre)
        })
        .chain(album.iter().filter(move |tag| !own.contains(tag)))
}

/// The `count` draws with the best keys, best first.
fn closest<'a>(
    mut drawn: Vec<(f64, f64, &'a TrackRec)>,
    count: usize,
) -> Vec<(f64, f64, &'a TrackRec)> {
    let first = |left: &(f64, f64, &TrackRec), right: &(f64, f64, &TrackRec)| {
        right
            .0
            .total_cmp(&left.0)
            .then_with(|| left.2.key.cmp(&right.2.key))
    };
    if drawn.len() > count {
        drawn.select_nth_unstable_by(count, first);
        drawn.truncate(count);
    }
    drawn.sort_unstable_by(first);
    drawn
}

/// Whether a title names what plays between an album's songs.
fn between_songs(title: &str) -> bool {
    title
        .split(|character: char| !character.is_alphanumeric())
        .any(|word| word.eq_ignore_ascii_case("skit") || word.eq_ignore_ascii_case("interlude"))
}

/// The next of a stream of evenly spread numbers.
fn splitmix(seed: &mut u64) -> u64 {
    *seed = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut n = *seed;
    n = (n ^ (n >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    n = (n ^ (n >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    n ^ (n >> 31)
}

/// `count` of `items`, each as likely as the others, in a shuffled order: one pass that holds
/// only the chosen.
fn sampled<T>(items: impl Iterator<Item = T>, count: usize, mut seed: u64) -> Vec<T> {
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

/// Keys of the tracks in the saved queue.
async fn queued_tracks(connection: &mut sqlx::SqliteConnection) -> LibraryResult<Vec<i64>> {
    Ok(
        sqlx::query_scalar("SELECT DISTINCT track_key FROM queue_entries")
            .fetch_all(connection)
            .await?,
    )
}

fn require_radio_bounds(extra_excluded: usize, requested: usize) -> LibraryResult<()> {
    if extra_excluded > RADIO_EXTRA_EXCLUSION_LIMIT || !(1..=RADIO_LIMIT).contains(&requested) {
        return Err(LibraryError::InvalidRequest(format!(
            "Radio reads accept at most {RADIO_LIMIT} candidates and {RADIO_EXTRA_EXCLUSION_LIMIT} request-local exclusions"
        )));
    }
    Ok(())
}
