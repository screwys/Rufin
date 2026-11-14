//! Selects only the requested bounded Radio or random candidates from the resident Catalog.
//! The persisted queue is the complete exclusion authority; request-local exclusions stay bounded.

use std::collections::HashSet;

use sqlx::{QueryBuilder, Sqlite};

use crate::catalog::snapshot::Packed;
use crate::catalog::snapshot::TrackRec;
use crate::catalog::views::{Context, known};
use crate::{
    AlbumKey, ArtistKey, Catalog, FolderKey, GenreKey, LibraryError, LibraryResult, PlaylistKey,
    ReadCancellation, SourceKey, TrackKey,
};

const RADIO_LIMIT: usize = 500;
const RADIO_EXTRA_EXCLUSION_LIMIT: usize = 500;

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

    /// Resolve provider recommendations in order, using the same queue and seed exclusions.
    pub async fn recommended_track_keys(
        &self,
        source: SourceKey,
        seed: &RadioSeed,
        object_ids: &[String],
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

    /// The MusicBrainz ids of a seed's artists in `source`: a track's or album's artists, or the
    /// artist itself. A genre or playlist has none.
    pub async fn radio_artist_mbids(
        &self,
        source: SourceKey,
        seed: &RadioSeed,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<String>> {
        let mut connection = self.acquire_cancellable(cancellation).await?;
        let artists = match seed {
            RadioSeed::Track(uri) => sqlx::query_scalar::<_, Option<String>>("SELECT artist.musicbrainz_artist_id FROM tracks track JOIN track_links link ON link.track_key=track.track_key AND link.role=0 JOIN artists artist ON artist.artist_key=link.target WHERE track.source_key=?1 AND track.media_uri=?2 ORDER BY link.position")
                .bind(source)
                .bind(uri),
            RadioSeed::Album(key) => sqlx::query_scalar("SELECT artist.musicbrainz_artist_id FROM album_links link JOIN artists artist ON artist.artist_key=link.target WHERE link.album_key=?2 AND link.role=0 AND artist.source_key=?1 ORDER BY link.position")
                .bind(source)
                .bind(key),
            RadioSeed::Artist(key) | RadioSeed::AlbumArtist(key) => sqlx::query_scalar(
                "SELECT musicbrainz_artist_id FROM artists WHERE source_key=?1 AND artist_key=?2",
            )
            .bind(source)
            .bind(key),
            RadioSeed::Genre(_) | RadioSeed::Playlist(_) => return Ok(Vec::new()),
        };
        Ok(artists
            .fetch_all(&mut *connection)
            .await?
            .into_iter()
            .flatten()
            .filter(|mbid| !mbid.is_empty())
            .collect())
    }

    /// The artists of `source` with one of the MusicBrainz ids `mbids`.
    pub async fn artists_with_mbids(
        &self,
        source: SourceKey,
        mbids: &[String],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<ArtistKey>> {
        if mbids.is_empty() {
            return Ok(Vec::new());
        }
        let mut connection = self.acquire_cancellable(cancellation).await?;
        Ok(sqlx::query_scalar("SELECT artist_key FROM artists WHERE source_key=?1 AND musicbrainz_artist_id IN (SELECT value FROM json_each(?2))")
            .bind(source)
            .bind(serde_json::to_string(mbids)?)
            .fetch_all(&mut *connection)
            .await?)
    }

    /// Tracks related to a seed, chosen evenly within each kind of relation: its artists, the
    /// artists of `similar`, its genres and albums by its artists, then any others when the
    /// related ones run out. Queued tracks are left out.
    pub async fn radio_candidates(
        &self,
        source: SourceKey,
        seed: RadioSeed,
        similar: &[ArtistKey],
        extra_excluded: &[TrackKey],
        requested: usize,
        variation: i64,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackKey>> {
        require_radio_bounds(extra_excluded.len(), requested)?;
        let (seed_track, queued) = {
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
            (track, queued_tracks(&mut connection).await?)
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
        if let Some(track) = seed_track {
            let runs = snapshot.track_runs(track);
            artists.extend(runs[0].iter().copied());
            genres.extend(runs[2].iter().copied().filter(genre));
        }
        if let Some(album) = seed_album.and_then(|album| snapshot.albums.live(album)) {
            let runs = snapshot.album_runs(album);
            artists.extend(runs[0].iter().copied());
            genres.extend(runs[1].iter().copied());
        }
        match &seed {
            RadioSeed::Artist(key) | RadioSeed::AlbumArtist(key) => {
                artists.extend(snapshot.artists.slot(key.raw()));
            }
            RadioSeed::Genre(key) => genres.extend(snapshot.tags.slot(key.raw())),
            _ => {}
        }
        let related_seed =
            seed_track.is_some() || !matches!(seed, RadioSeed::Track(_) | RadioSeed::Playlist(_));
        let related_albums = snapshot
            .albums
            .iter()
            .filter(|(_, album)| {
                let runs = snapshot.album_runs(album);
                runs[0].iter().any(|artist| artists.contains(artist))
                    || runs[1].iter().any(|genre| genres.contains(genre))
            })
            .map(|(slot, _)| slot)
            .collect::<HashSet<_>>();
        let excluded = extra_excluded
            .iter()
            .map(|key| key.raw())
            .chain(queued)
            .collect::<HashSet<_>>();
        let similar = similar
            .iter()
            .filter_map(|artist| snapshot.artists.slot(artist.raw()))
            .collect::<HashSet<_>>();
        let by_artist = !matches!(seed, RadioSeed::AlbumArtist(_));
        let eligible = || {
            snapshot
                .tracks
                .iter()
                .map(|(_, track)| track)
                .filter(|track| {
                    track.source == owner
                        && track.indexed()
                        && !excluded.contains(&i64::from(track.key))
                        && seed_track.is_none_or(|seed| seed.key != track.key)
                        && (!matches!(seed, RadioSeed::Album(_))
                            || seed_album.is_none_or(|album| track.album != album))
                })
        };
        // How close a track is to the seed: its artist, an artist of `similar`, its albums'
        // artists or genres, or nothing.
        let closeness = |track: &TrackRec| {
            let runs = snapshot.track_runs(track);
            if !related_seed {
                3
            } else if by_artist && runs[0].iter().any(|artist| artists.contains(artist)) {
                0
            } else if runs[0].iter().any(|artist| similar.contains(artist)) {
                1
            } else if related_albums.contains(&track.album)
                || runs[2].iter().any(|genre| genres.contains(genre))
            {
                2
            } else {
                3
            }
        };
        // A third each from the seed's artists and from similar ones, the rest from its genres;
        // a group that runs short leaves its share to the next, and unrelated tracks fill up.
        let share = requested.div_ceil(3);
        let mut chosen = Vec::<&TrackRec>::with_capacity(requested);
        for (group, share) in [(0, share), (1, share), (2, requested), (3, requested)] {
            if group == 3 {
                // The related ones play mixed, ahead of the unrelated.
                crate::shuffle_order(&mut chosen, variation as u64);
            }
            let wanted = (chosen.len() + share).min(requested) - chosen.len();
            if wanted > 0 {
                chosen.extend(sampled(
                    eligible().filter(|track| closeness(track) == group),
                    wanted,
                    (variation as u64).wrapping_add(group),
                ));
            }
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

/// `count` of `items`, each as likely as the others, in a shuffled order: one pass that holds
/// only the chosen.
fn sampled<T>(items: impl Iterator<Item = T>, count: usize, mut seed: u64) -> Vec<T> {
    let mut next = || {
        seed = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut n = seed;
        n = (n ^ (n >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        n = (n ^ (n >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        n ^ (n >> 31)
    };
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
