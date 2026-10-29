//! Selects only the requested bounded Radio or random candidates from the resident Catalog.
//! The persisted queue is the complete exclusion authority; request-local exclusions stay bounded.

use std::collections::HashSet;

use sqlx::{QueryBuilder, Sqlite};

use crate::catalog::snapshot::Packed;
use crate::catalog::snapshot::TrackRec;
use crate::catalog::views::Context;
use crate::{
    AlbumKey, ArtistKey, Catalog, FolderKey, GenreKey, LibraryError, LibraryResult, PlaylistKey,
    ReadCancellation, SourceKey, TrackKey,
};

const RADIO_LIMIT: usize = 500;
const RADIO_EXTRA_EXCLUSION_LIMIT: usize = 500;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RadioSeed {
    Track(String),
    Album(AlbumKey),
    Artist(ArtistKey),
    AlbumArtist(ArtistKey),
    Genre(GenreKey),
    Playlist(PlaylistKey),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum PlayedFilter {
    #[default]
    All,
    Unplayed,
    Played,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RandomCriteria {
    pub min_year: Option<i64>,
    pub max_year: Option<i64>,
    pub genre: Option<GenreKey>,
    pub played: PlayedFilter,
    pub require_media: bool,
    pub variation: i64,
}

impl Catalog {
    /// Resolve the seed's catalog owner independently of the selected browse source.
    pub async fn radio_source(
        &self,
        seed: &RadioSeed,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<(SourceKey, crate::SourceId, Option<String>)>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
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
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
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

    /// Tracks related to a seed by artist, album or genre, from a variation-chosen point in key
    /// order, then any others when the related ones run out. Queued tracks are left out.
    pub async fn radio_candidates(
        &self,
        source: SourceKey,
        seed: RadioSeed,
        extra_excluded: &[TrackKey],
        requested: usize,
        _require_media: bool,
        variation: i64,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackKey>> {
        require_radio_bounds(extra_excluded.len(), requested)?;
        let (seed_track, queued) = {
            let (_permit, mut connection) = self.acquire_general(cancellation).await?;
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
        let context = Context::new(&snapshot, false);
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
        let by_artist = !matches!(seed, RadioSeed::AlbumArtist(_));
        let candidates = pivoted(
            snapshot
                .tracks
                .iter()
                .filter(|(_, track)| {
                    track.source == owner
                        && track.indexed()
                        && !excluded.contains(&i64::from(track.key))
                        && seed_track.is_none_or(|seed| seed.key != track.key)
                        && (!matches!(seed, RadioSeed::Album(_))
                            || seed_album.is_none_or(|album| track.album != album))
                })
                .map(|(_, track)| track),
            variation,
        );
        let related = |track: &TrackRec| {
            let runs = snapshot.track_runs(track);
            (by_artist && runs[0].iter().any(|artist| artists.contains(artist)))
                || related_albums.contains(&track.album)
                || runs[2].iter().any(|genre| genres.contains(genre))
        };
        let mut chosen = Vec::with_capacity(requested);
        if related_seed {
            chosen.extend(
                candidates
                    .iter()
                    .filter(|track| related(track))
                    .map(|track| i64::from(track.key))
                    .take(requested),
            );
        }
        let picked = chosen.iter().copied().collect::<HashSet<_>>();
        let rest = requested - chosen.len();
        chosen.extend(
            candidates
                .iter()
                .map(|track| i64::from(track.key))
                .filter(|key| !picked.contains(key))
                .take(rest),
        );
        Ok(chosen.into_iter().map(TrackKey::from_raw).collect())
    }

    /// Visible tracks of a source that match the criteria, from a variation-chosen point in
    /// key order. Queued tracks are left out.
    pub async fn random_candidates(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        criteria: &RandomCriteria,
        extra_excluded: &[TrackKey],
        requested: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackKey>> {
        require_radio_bounds(extra_excluded.len(), requested)?;
        let queued = {
            let (_permit, mut connection) = self.acquire_general(cancellation).await?;
            queued_tracks(&mut connection).await?
        };
        self.loaded().await;
        let snapshot = self.snapshot();
        let context = Context::new(&snapshot, false);
        let Some(owner) = snapshot.source_slot(source) else {
            return Ok(Vec::new());
        };
        let folder = match folder {
            Some(folder) => match snapshot.folders.slot(folder.raw()) {
                Some(slot) => Some(slot),
                None => return Ok(Vec::new()),
            },
            None => None,
        };
        let genre = match criteria.genre {
            Some(genre) => match snapshot.tags.slot(genre.raw()) {
                Some(slot) => Some(slot),
                None => return Ok(Vec::new()),
            },
            None => None,
        };
        let excluded = extra_excluded
            .iter()
            .map(|key| key.raw())
            .chain(queued)
            .collect::<HashSet<_>>();
        let year = |track: &TrackRec| (track.year != i16::MIN).then_some(i64::from(track.year));
        let candidates = pivoted(
            snapshot
                .tracks
                .iter()
                .filter(|(slot, track)| {
                    let runs = snapshot.track_runs(track);
                    let plays = context.played(*slot, track).plays;
                    track.source == owner
                        && track.indexed()
                        && !excluded.contains(&i64::from(track.key))
                        && folder.is_none_or(|folder| runs[3].contains(&folder))
                        && criteria
                            .min_year
                            .is_none_or(|min| year(track).is_some_and(|year| year >= min))
                        && criteria
                            .max_year
                            .is_none_or(|max| year(track).is_some_and(|year| year <= max))
                        && genre.is_none_or(|genre| runs[2].contains(&genre))
                        && match criteria.played {
                            PlayedFilter::All => true,
                            PlayedFilter::Unplayed => plays == 0,
                            PlayedFilter::Played => plays > 0,
                        }
                })
                .map(|(_, track)| track),
            criteria.variation,
        );
        Ok(candidates
            .into_iter()
            .take(requested)
            .map(|track| TrackKey::from_raw(i64::from(track.key)))
            .collect())
    }
}

/// Tracks in key order from a point a variation picks, wrapping around.
fn pivoted<'a>(tracks: impl Iterator<Item = &'a TrackRec>, variation: i64) -> Vec<&'a TrackRec> {
    let mut tracks = tracks.collect::<Vec<_>>();
    tracks.sort_unstable_by_key(|track| track.key);
    let max = tracks.last().map_or(0, |track| i64::from(track.key));
    let pivot = if max == 0 {
        0
    } else {
        variation.rem_euclid(max + 1)
    };
    let split = tracks.partition_point(|track| i64::from(track.key) < pivot);
    tracks.rotate_left(split);
    tracks
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
