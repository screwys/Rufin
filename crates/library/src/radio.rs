//! Selects only the requested bounded Radio or random candidates.
//! The persisted queue is the complete exclusion authority; request-local exclusions stay bounded.

use sqlx::{QueryBuilder, Sqlite};

use crate::{
    AlbumKey, ArtistKey, Database, FolderKey, GenreKey, LibraryError, LibraryResult, PlaylistKey,
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
    /// Resolve the seed's catalog owner independently of the selected browse source.
    pub async fn radio_source(
        &self,
        seed: &RadioSeed,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<(SourceKey, crate::SourceId)>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT source_key,object_id FROM sources WHERE source_key=(",
        );
        match seed {
            RadioSeed::Track(uri) => {
                query
                    .push("SELECT source_key FROM tracks WHERE media_uri=")
                    .push_bind(uri);
            }
            RadioSeed::Album(key) => {
                query
                    .push("SELECT source_key FROM albums WHERE album_key=")
                    .push_bind(key);
            }
            RadioSeed::Artist(key) | RadioSeed::AlbumArtist(key) => {
                query
                    .push("SELECT source_key FROM artists WHERE artist_key=")
                    .push_bind(key);
            }
            RadioSeed::Genre(key) => {
                query
                    .push("SELECT source_key FROM genres WHERE genre_key=")
                    .push_bind(key);
            }
            RadioSeed::Playlist(key) => {
                query.push("SELECT COALESCE(playlist.source_key,(SELECT track.source_key FROM playlist_entries entry JOIN tracks track USING(media_uri) WHERE entry.playlist_key=playlist.playlist_key ORDER BY entry.position LIMIT 1)) FROM playlists playlist WHERE playlist.playlist_key=").push_bind(key);
            }
        }
        query.push(")");
        let result = query
            .build_query_as::<(SourceKey, String)>()
            .persistent(false)
            .fetch_optional(&mut *connection)
            .await?;
        Database::clear_progress(&mut connection).await?;
        Ok(result.map(|(key, id)| (key, crate::SourceId::new(id))))
    }

    /// Admit provider recommendations in order, using the same queue and seed exclusions.
    pub async fn admit_radio_candidates(
        &self,
        source: SourceKey,
        seed: &RadioSeed,
        object_ids: &[String],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<String>> {
        if object_ids.is_empty() {
            return Ok(Vec::new());
        }
        require_radio_bounds(0, object_ids.len())?;
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut query = QueryBuilder::<Sqlite>::new("WITH requested(object_id,position) AS (");
        query.push_values(object_ids.iter().enumerate(), |mut row, (position, id)| {
            row.push_bind(id).push_bind(position as i64);
        });
        query.push(") SELECT track.media_uri FROM requested JOIN tracks track ON track.object_id=requested.object_id WHERE track.source_key=").push_bind(source)
            .push(" AND NOT EXISTS (SELECT 1 FROM queue_occurrences queued WHERE queued.media_uri=track.media_uri)");
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
                query.push(" AND track.media_uri IS NOT (SELECT entry.media_uri FROM playlist_entries entry JOIN tracks seed ON seed.media_uri=entry.media_uri WHERE entry.playlist_key=").push_bind(key)
                    .push(" AND seed.source_key=").push_bind(source).push(" ORDER BY entry.position LIMIT 1)");
            }
            _ => {}
        }
        query.push(" GROUP BY track.media_uri ORDER BY min(requested.position)");
        let result = query
            .build_query_scalar()
            .persistent(false)
            .fetch_all(&mut *connection)
            .await?;
        Database::clear_progress(&mut connection).await?;
        Ok(result)
    }

    pub async fn radio_candidates(
        &self,
        source: SourceKey,
        seed: RadioSeed,
        extra_excluded: &[String],
        requested: usize,
        require_media: bool,
        variation: i64,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<String>> {
        require_radio_bounds(extra_excluded.len(), requested)?;
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let (seed_kind, seed_raw) = if let RadioSeed::Playlist(playlist) = seed {
            let first = sqlx::query_scalar::<_, TrackKey>("SELECT track.track_key FROM playlist_entries entry JOIN tracks track ON track.media_uri=entry.media_uri AND track.source_key=?1 WHERE entry.playlist_key=?2 ORDER BY entry.position LIMIT 1")
                .bind(source).bind(playlist).fetch_optional(&mut *connection).await?;
            let Some(first) = first else {
                Database::clear_progress(&mut connection).await?;
                return Ok(Vec::new());
            };
            (0, first.raw())
        } else {
            match seed {
                RadioSeed::Track(media_uri) => {
                    let key = sqlx::query_scalar::<_, TrackKey>(
                        "SELECT track_key FROM tracks WHERE source_key=?1 AND media_uri=?2",
                    )
                    .bind(source)
                    .bind(media_uri)
                    .fetch_optional(&mut *connection)
                    .await?;
                    let Some(key) = key else {
                        Database::clear_progress(&mut connection).await?;
                        return Ok(Vec::new());
                    };
                    (0, key.raw())
                }
                RadioSeed::Album(key) => (1, key.raw()),
                RadioSeed::Artist(key) => (2, key.raw()),
                RadioSeed::AlbumArtist(key) => (5, key.raw()),
                RadioSeed::Genre(key) => (3, key.raw()),
                RadioSeed::Playlist(_) => unreachable!(),
            }
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
        let mut related = true;
        loop {
            let mut query = QueryBuilder::<Sqlite>::new("");
            if related {
                query
                    .push("WITH seed(kind,id) AS (VALUES (")
                    .push_bind(seed_kind)
                    .push(",")
                    .push_bind(seed_raw)
                    .push(
                        ")), seed_albums AS (
                        SELECT album_key FROM tracks,seed WHERE kind=0 AND track_key=id
                        UNION SELECT id FROM seed WHERE kind=1
                    ), seed_artists AS (
                        SELECT artist_key FROM track_artists,seed WHERE kind=0 AND track_key=id
                        UNION SELECT artist_key FROM album_artists WHERE album_key IN seed_albums
                        UNION SELECT id FROM seed WHERE kind IN (2,5)
                    ), seed_genres AS (
                        SELECT genre_key FROM track_genres,seed WHERE kind=0 AND track_key=id
                        UNION SELECT genre_key FROM album_genres WHERE album_key IN seed_albums
                        UNION SELECT id FROM seed WHERE kind=3
                    ) ",
                    );
            }
            query.push(
                "SELECT track.track_key,track.media_uri FROM tracks AS track
             WHERE track.source_key=",
            );
            query
                .push_bind(source)
                .push(" AND track.track_key>")
                .push_bind(after)
                .push(" AND (")
                .push_bind(seed_kind)
                .push("<>0 OR track.track_key<>")
                .push_bind(seed_raw)
                .push(")")
                .push(" AND (")
                .push_bind(seed_kind)
                .push("<>1 OR track.album_key IS NULL OR track.album_key<>")
                .push_bind(seed_raw)
                .push(")")
                .push(" AND (")
                .push_bind(!wrapped)
                .push(" OR track.track_key<")
                .push_bind(pivot)
                .push(") AND (")
                .push_bind(!require_media)
                .push(" OR track.media_uri IS NOT NULL)");
            if related {
                query.push(" AND ((").push_bind(seed_kind).push("<>5 AND EXISTS (
                    SELECT 1 FROM track_artists WHERE track_key=track.track_key AND artist_key IN seed_artists
                )) OR EXISTS (
                    SELECT 1 FROM album_artists WHERE album_key=track.album_key AND artist_key IN seed_artists
                ) OR EXISTS (
                    SELECT 1 FROM track_genres WHERE track_key=track.track_key AND genre_key IN seed_genres
                ) OR EXISTS (
                    SELECT 1 FROM album_genres WHERE album_key=track.album_key AND genre_key IN seed_genres
                ))");
            }
            query.push(" AND NOT EXISTS (SELECT 1 FROM queue_occurrences queued WHERE queued.media_uri=track.media_uri)");
            if !extra_excluded.is_empty() || !result.is_empty() {
                query.push(" AND track.media_uri NOT IN (");
                let mut separated = query.separated(",");
                for media_uri in extra_excluded.iter().chain(&result) {
                    separated.push_bind(media_uri);
                }
                separated.push_unseparated(")");
            }
            query.push(" ORDER BY track.track_key LIMIT 500");
            let page = query
                .build_query_as::<(TrackKey, String)>()
                .persistent(false)
                .fetch_all(&mut *connection)
                .await?;
            if page.is_empty() {
                if wrapped {
                    if !related {
                        break;
                    }
                    related = false;
                    wrapped = false;
                    after = pivot - 1;
                    continue;
                }
                wrapped = true;
                after = -1;
                continue;
            }
            for (key, media_uri) in page {
                after = key.raw();
                result.push(media_uri);
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
        extra_excluded: &[String],
        requested: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<String>> {
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
                "SELECT track.track_key,track.media_uri FROM tracks AS track
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
                 SELECT 1 FROM listens WHERE media_uri=track.media_uri
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
                 SELECT 1 FROM listens WHERE media_uri=track.media_uri
                    ))))",
                );
            query.push(" AND NOT EXISTS (SELECT 1 FROM queue_occurrences queued WHERE queued.media_uri=track.media_uri)");
            if !extra_excluded.is_empty() {
                query.push(" AND track.media_uri NOT IN (");
                let mut separated = query.separated(",");
                for media_uri in extra_excluded {
                    separated.push_bind(media_uri);
                }
                separated.push_unseparated(")");
            }
            query.push(" ORDER BY track.track_key LIMIT 500");
            let page = query
                .build_query_as::<(TrackKey, String)>()
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
            for (key, media_uri) in page {
                after = key.raw();
                result.push(media_uri);
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
