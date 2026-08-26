//! Selects only the requested bounded Radio or random candidates.
//! The persisted queue is the complete exclusion authority; request-local exclusions stay bounded.

use sqlx::{QueryBuilder, Sqlite};

use crate::{
    AlbumKey, ArtistKey, Database, FolderKey, GenreKey, LibraryError, LibraryResult, PlaylistKey,
    ReadCancellation, SourceKey, TrackKey,
};

const RADIO_LIMIT: usize = 500;
const RADIO_EXTRA_EXCLUSION_LIMIT: usize = 500;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadioSeed {
    Track(TrackKey),
    Album(AlbumKey),
    Artist(ArtistKey),
    AlbumArtist(ArtistKey),
    Genre(GenreKey),
    Playlist(PlaylistKey),
}

impl RadioSeed {
    const fn kind(self) -> i64 {
        match self {
            Self::Track(_) => 0,
            Self::Album(_) => 1,
            Self::Artist(_) => 2,
            Self::AlbumArtist(_) => 5,
            Self::Genre(_) => 3,
            Self::Playlist(_) => 4,
        }
    }

    const fn raw(self) -> i64 {
        match self {
            Self::Track(key) => key.raw(),
            Self::Album(key) => key.raw(),
            Self::Artist(key) => key.raw(),
            Self::AlbumArtist(key) => key.raw(),
            Self::Genre(key) => key.raw(),
            Self::Playlist(key) => key.raw(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub enum PlayedFilter {
    #[default]
    All,
    Unplayed,
    Played,
}

impl PlayedFilter {
    const fn code(self) -> i64 {
        match self {
            Self::All => 0,
            Self::Unplayed => 1,
            Self::Played => 2,
        }
    }
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

impl Database {
    pub async fn radio_candidates(
        &self,
        source: SourceKey,
        seed: RadioSeed,
        extra_excluded: &[TrackKey],
        requested: usize,
        require_media: bool,
        variation: i64,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackKey>> {
        require_radio_bounds(extra_excluded.len(), requested)?;
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let seed = if let RadioSeed::Playlist(playlist) = seed {
            let first = sqlx::query_scalar::<_, TrackKey>("SELECT entry.track_key FROM playlist_entries entry JOIN playlists playlist USING(playlist_key) WHERE playlist.source_key=?1 AND playlist.playlist_key=?2 AND entry.track_key IS NOT NULL ORDER BY entry.position LIMIT 1")
                .bind(source).bind(playlist).fetch_optional(&mut *connection).await?;
            let Some(first) = first else {
                Database::clear_progress(&mut connection).await?;
                return Ok(Vec::new());
            };
            RadioSeed::Track(first)
        } else {
            seed
        };
        let max_key = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(max(track_key),0) FROM tracks WHERE source_key=?1",
        )
        .bind(source)
        .fetch_one(&mut *connection)
        .await?;
        let pivot = if max_key == 0 {
            0
        } else {
            variation.rem_euclid(max_key + 1)
        };
        let mut result = Vec::with_capacity(requested);
        let mut after = pivot - 1;
        let mut wrapped = false;
        loop {
            let mut query = QueryBuilder::<Sqlite>::new(
                "SELECT track.track_key FROM tracks AS track
             WHERE track.source_key=",
            );
            query
            .push_bind(source)
            .push(" AND track.track_key>").push_bind(after)
            .push(" AND (").push_bind(seed.kind()).push("<>0 OR track.track_key<>").push_bind(seed.raw()).push(")")
            .push(" AND (").push_bind(!wrapped).push(" OR track.track_key<").push_bind(pivot).push(") AND (")
            .push_bind(!require_media)
            .push(" OR track.media_uri IS NOT NULL) AND (")
            .push_bind(seed.kind())
            .push(
                "=0 AND (
                 EXISTS (
                   SELECT 1 FROM track_artists AS candidate
                   JOIN track_artists AS seeded ON seeded.artist_key=candidate.artist_key
                   WHERE candidate.track_key=track.track_key AND seeded.track_key=",
            )
            .push_bind(seed.raw())
            .push(
                ") OR EXISTS (
                   SELECT 1 FROM track_genres AS candidate
                   JOIN track_genres AS seeded ON seeded.genre_key=candidate.genre_key
                   WHERE candidate.track_key=track.track_key AND seeded.track_key=",
            )
            .push_bind(seed.raw())
            .push(")) OR (")
            .push_bind(seed.kind())
            .push(
                "=5 AND EXISTS (
                 SELECT 1 FROM album_artists
                 WHERE album_key=track.album_key AND artist_key=",
            )
            .push_bind(seed.raw())
            .push(")) OR (")
            .push_bind(seed.kind())
            .push("=1 AND track.album_key<>").push_bind(seed.raw()).push(" AND (EXISTS (SELECT 1 FROM tracks candidate JOIN album_genres candidate_genre USING(album_key) JOIN album_genres seeded_genre ON seeded_genre.genre_key=candidate_genre.genre_key WHERE candidate.track_key=track.track_key AND seeded_genre.album_key=").push_bind(seed.raw()).push(") OR EXISTS (SELECT 1 FROM tracks candidate JOIN album_artists candidate_artist USING(album_key) JOIN album_artists seeded_artist ON seeded_artist.artist_key=candidate_artist.artist_key WHERE candidate.track_key=track.track_key AND seeded_artist.album_key=").push_bind(seed.raw()).push("))) OR (")
            .push_bind(seed.kind())
            .push(
                "=2 AND EXISTS (
                 SELECT 1 FROM track_artists
                 WHERE track_key=track.track_key AND artist_key=",
            )
            .push_bind(seed.raw())
            .push(")) OR (")
            .push_bind(seed.kind())
            .push(
                "=3 AND EXISTS (
                 SELECT 1 FROM track_genres
                 WHERE track_key=track.track_key AND genre_key=",
            )
            .push_bind(seed.raw())
            .push(")) OR (")
            .push_bind(seed.kind())
            .push(
                "=4 AND EXISTS (
                 SELECT 1 FROM playlist_entries
                 WHERE track_key=track.track_key AND playlist_key=",
            )
            .push_bind(seed.raw())
            .push(")))");
            query.push(" AND NOT EXISTS (SELECT 1 FROM queue_occurrences queued WHERE queued.source_key=").push_bind(source).push(" AND queued.track_key=track.track_key)");
            if !extra_excluded.is_empty() {
                query.push(" AND track.track_key NOT IN (");
                let mut separated = query.separated(",");
                for key in extra_excluded {
                    separated.push_bind(*key);
                }
                separated.push_unseparated(")");
            }
            query.push(" ORDER BY track.track_key LIMIT 500");
            let page = query
                .build_query_scalar::<TrackKey>()
                .persistent(false)
                .fetch_all(&mut *connection)
                .await?;
            if page.is_empty() {
                if wrapped {
                    break;
                }
                wrapped = true;
                after = -1;
                continue;
            }
            for key in page {
                after = key.raw();
                result.push(key);
                if result.len() == requested {
                    break;
                }
            }
            if result.len() == requested {
                break;
            }
        }
        Database::clear_progress(&mut connection).await?;
        Ok(result)
    }

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
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let max_key = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(max(track_key),0) FROM tracks WHERE source_key=?1",
        )
        .bind(source)
        .fetch_one(&mut *connection)
        .await?;
        let pivot = if max_key == 0 {
            0
        } else {
            criteria.variation.rem_euclid(max_key + 1)
        };
        let mut result = Vec::with_capacity(requested);
        let mut after = pivot - 1;
        let mut wrapped = false;
        loop {
            let mut query = QueryBuilder::<Sqlite>::new(
                "SELECT track.track_key FROM tracks AS track
             WHERE track.source_key=",
            );
            query
                .push_bind(source)
                .push(" AND track.track_key>")
                .push_bind(after)
                .push(" AND (")
                .push_bind(folder)
                .push(" IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=")
                .push_bind(folder)
                .push("))")
                .push(" AND (")
                .push_bind(!wrapped)
                .push(" OR track.track_key<")
                .push_bind(pivot)
                .push(")")
                .push(" AND (")
                .push_bind(criteria.min_year)
                .push(" IS NULL OR track.year>=")
                .push_bind(criteria.min_year)
                .push(") AND (")
                .push_bind(criteria.max_year)
                .push(" IS NULL OR track.year<=")
                .push_bind(criteria.max_year)
                .push(") AND (")
                .push_bind(criteria.genre)
                .push(
                    " IS NULL OR EXISTS (
                 SELECT 1 FROM track_genres AS relation
                 WHERE relation.track_key=track.track_key AND relation.genre_key=",
                )
                .push_bind(criteria.genre)
                .push(")) AND (")
                .push_bind(!criteria.require_media)
                .push(" OR track.media_uri IS NOT NULL) AND (")
                .push_bind(criteria.played.code())
                .push("=0 OR (")
                .push_bind(criteria.played.code())
                .push(
                    "=1 AND NOT EXISTS (
                 SELECT 1 FROM activity_baseline AS baseline
                 WHERE baseline.source_key=track.source_key
                   AND baseline.track_object_id=track.object_id
                   AND baseline.period='lifetime' AND baseline.item_kind='track'
                   AND baseline.play_count>0
             ) AND NOT EXISTS (
                 SELECT 1 FROM listens WHERE track_key=track.track_key
             )) OR (",
                )
                .push_bind(criteria.played.code())
                .push(
                    "=2 AND (EXISTS (
                 SELECT 1 FROM activity_baseline AS baseline
                 WHERE baseline.source_key=track.source_key
                   AND baseline.track_object_id=track.object_id
                   AND baseline.period='lifetime' AND baseline.item_kind='track'
                   AND baseline.play_count>0
             ) OR EXISTS (
                 SELECT 1 FROM listens WHERE track_key=track.track_key
                    ))))",
                );
            query.push(" AND NOT EXISTS (SELECT 1 FROM queue_occurrences queued WHERE queued.source_key=").push_bind(source).push(" AND queued.track_key=track.track_key)");
            if !extra_excluded.is_empty() {
                query.push(" AND track.track_key NOT IN (");
                let mut separated = query.separated(",");
                for key in extra_excluded {
                    separated.push_bind(*key);
                }
                separated.push_unseparated(")");
            }
            query.push(" ORDER BY track.track_key LIMIT 500");
            let page = query
                .build_query_scalar::<TrackKey>()
                .persistent(false)
                .fetch_all(&mut *connection)
                .await?;
            if page.is_empty() {
                if wrapped {
                    break;
                }
                wrapped = true;
                after = -1;
                continue;
            }
            for key in page {
                after = key.raw();
                result.push(key);
                if result.len() == requested {
                    break;
                }
            }
            if result.len() == requested {
                break;
            }
        }
        Database::clear_progress(&mut connection).await?;
        Ok(result)
    }
}

fn require_radio_bounds(extra_excluded: usize, requested: usize) -> LibraryResult<()> {
    if extra_excluded > RADIO_EXTRA_EXCLUSION_LIMIT || !(1..=RADIO_LIMIT).contains(&requested) {
        return Err(LibraryError::InvalidRequest(format!(
            "Radio reads accept at most {RADIO_LIMIT} candidates and {RADIO_EXTRA_EXCLUSION_LIMIT} request-local exclusions"
        )));
    }
    Ok(())
}
