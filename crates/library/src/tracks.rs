//! Owns complete Track orders, bounded final rows, details, and Track metadata writes.
//! Row assembly batches relation reads within the supplied window.

use std::collections::BTreeMap;

use sqlx::{Connection, FromRow, QueryBuilder, Sqlite, SqliteConnection};

use crate::{
    AlbumKey, ArtistKey, Database, FolderKey, GenreKey, LibraryError, LibraryResult,
    ReadCancellation, SourceKey, TrackKey, loudness::recompute_album_loudness_key,
};

const TRACK_ROW_LIMIT: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrackSort {
    Title,
    TrackNumber,
    Artist,
    AlbumArtist,
    Album,
    Year,
    ReleaseDate,
    DateAdded,
    LastPlayed,
    PlayCount,
    UserRating,
    Genre,
    Bpm,
    Duration,
    Favorite,
}

impl TrackSort {
    pub(crate) const fn uses_activity(self) -> bool {
        matches!(self, Self::LastPlayed | Self::PlayCount)
    }

    pub(crate) fn order_sql(self, descending: bool) -> &'static str {
        match (self, descending) {
            (TrackSort::Title, false) => "track.sort_text ASC, track.track_key ASC",
            (TrackSort::Title, true) => "track.sort_text DESC, track.track_key ASC",
            (TrackSort::TrackNumber, false) => {
                "track.disc_number ASC, track.track_number ASC, track.sort_text, track.track_key"
            }
            (TrackSort::TrackNumber, true) => {
                "track.disc_number DESC, track.track_number DESC, track.sort_text, track.track_key"
            }
            (TrackSort::Artist, false) => {
                "track.display_artist ASC, track.sort_text, track.track_key"
            }
            (TrackSort::Artist, true) => {
                "track.display_artist DESC, track.sort_text, track.track_key"
            }
            (TrackSort::AlbumArtist, false) => {
                "(SELECT artist.sort_text FROM albums album JOIN album_artists relation USING(album_key) JOIN artists artist USING(artist_key) WHERE album.album_key=track.album_key ORDER BY relation.position LIMIT 1) ASC NULLS LAST, track.sort_text, track.track_key"
            }
            (TrackSort::AlbumArtist, true) => {
                "(SELECT artist.sort_text FROM albums album JOIN album_artists relation USING(album_key) JOIN artists artist USING(artist_key) WHERE album.album_key=track.album_key ORDER BY relation.position LIMIT 1) DESC NULLS LAST, track.sort_text, track.track_key"
            }
            (TrackSort::Album, false) => {
                "track.display_album ASC, track.sort_text, track.track_key"
            }
            (TrackSort::Album, true) => {
                "track.display_album DESC, track.sort_text, track.track_key"
            }
            (TrackSort::Year, false) => {
                "track.year ASC NULLS LAST, track.sort_text, track.track_key"
            }
            (TrackSort::Year, true) => {
                "track.year DESC NULLS LAST, track.sort_text, track.track_key"
            }
            (TrackSort::ReleaseDate, false) => {
                "track.release_date ASC NULLS LAST, track.sort_text, track.track_key"
            }
            (TrackSort::ReleaseDate, true) => {
                "track.release_date DESC NULLS LAST, track.sort_text, track.track_key"
            }
            (TrackSort::DateAdded, false) => {
                "track.date_added ASC NULLS LAST, track.sort_text, track.track_key"
            }
            (TrackSort::DateAdded, true) => {
                "track.date_added DESC NULLS LAST, track.sort_text, track.track_key"
            }
            (TrackSort::UserRating, false) => {
                "COALESCE(track.user_rating, track.source_rating) ASC NULLS LAST, track.sort_text, track.track_key"
            }
            (TrackSort::UserRating, true) => {
                "COALESCE(track.user_rating, track.source_rating) DESC NULLS LAST, track.sort_text, track.track_key"
            }
            (TrackSort::Genre, false) => {
                "(SELECT genre.sort_text FROM track_genres relation JOIN genres genre USING(genre_key) WHERE relation.track_key=track.track_key ORDER BY relation.position LIMIT 1) ASC NULLS LAST, track.sort_text, track.track_key"
            }
            (TrackSort::Genre, true) => {
                "(SELECT genre.sort_text FROM track_genres relation JOIN genres genre USING(genre_key) WHERE relation.track_key=track.track_key ORDER BY relation.position LIMIT 1) DESC NULLS LAST, track.sort_text, track.track_key"
            }
            (TrackSort::Bpm, false) => "track.bpm ASC NULLS LAST, track.sort_text, track.track_key",
            (TrackSort::Bpm, true) => "track.bpm DESC NULLS LAST, track.sort_text, track.track_key",
            (TrackSort::Duration, false) => {
                "track.duration_millis ASC, track.sort_text, track.track_key"
            }
            (TrackSort::Duration, true) => {
                "track.duration_millis DESC, track.sort_text, track.track_key"
            }
            (TrackSort::Favorite, false) => {
                "COALESCE(track.user_favorite, track.source_favorite) ASC, track.sort_text, track.track_key"
            }
            (TrackSort::Favorite, true) => {
                "COALESCE(track.user_favorite, track.source_favorite) DESC, track.sort_text, track.track_key"
            }
            (TrackSort::LastPlayed, false) => "activity.last_played ASC NULLS LAST",
            (TrackSort::LastPlayed, true) => "activity.last_played DESC NULLS LAST",
            (TrackSort::PlayCount, false) => "activity.play_count ASC",
            (TrackSort::PlayCount, true) => "activity.play_count DESC",
        }
    }
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct TrackArtistLink {
    pub artist_key: ArtistKey,
    pub name: String,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct TrackGenreLink {
    pub genre_key: GenreKey,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrackRow {
    pub track_key: TrackKey,
    pub source_key: SourceKey,
    pub source_id: String,
    pub object_id: String,
    pub album_key: Option<AlbumKey>,
    pub title: String,
    pub display_album: String,
    pub display_artist: String,
    pub duration_millis: i64,
    pub disc_number: i64,
    pub track_number: i64,
    pub year: Option<i64>,
    pub release_date: Option<String>,
    pub date_added: Option<String>,
    pub media_uri: Option<String>,
    pub source_format: Option<String>,
    pub comment: Option<String>,
    pub bpm: Option<i64>,
    pub musicbrainz_recording_id: Option<String>,
    pub musicbrainz_release_track_id: Option<String>,
    pub cue_path: Option<String>,
    pub cue_start_millis: Option<i64>,
    pub cue_end_millis: Option<i64>,
    pub loudness_analysis_key: [u8; 32],
    pub artwork_binding: Option<Vec<u8>>,
    pub favorite: bool,
    pub rating: Option<i64>,
    pub last_played: Option<i64>,
    pub play_count: i64,
    pub skip_count: i64,
    pub is_downloaded: bool,
    pub artists: Vec<TrackArtistLink>,
    pub album_artists: Vec<TrackArtistLink>,
    pub genres: Vec<TrackGenreLink>,
}

#[derive(Clone, Debug)]
pub struct TrackRoutePage {
    pub order: Vec<TrackKey>,
    pub first_rows: Vec<TrackRow>,
}

#[derive(FromRow)]
struct TrackScalar {
    track_key: TrackKey,
    source_key: SourceKey,
    source_id: String,
    object_id: String,
    album_key: Option<AlbumKey>,
    title: String,
    display_album: String,
    display_artist: String,
    duration_millis: i64,
    disc_number: i64,
    track_number: i64,
    year: Option<i64>,
    release_date: Option<String>,
    date_added: Option<String>,
    media_uri: Option<String>,
    source_format: Option<String>,
    comment: Option<String>,
    bpm: Option<i64>,
    musicbrainz_recording_id: Option<String>,
    musicbrainz_release_track_id: Option<String>,
    cue_path: Option<String>,
    cue_start_millis: Option<i64>,
    cue_end_millis: Option<i64>,
    loudness_analysis_key: Vec<u8>,
    artwork_binding: Option<Vec<u8>>,
    favorite: bool,
    rating: Option<i64>,
    last_played: Option<i64>,
    play_count: i64,
    skip_count: i64,
    is_downloaded: bool,
}

#[derive(FromRow)]
struct TrackArtistRelation {
    track_key: TrackKey,
    artist_key: ArtistKey,
    name: String,
}

#[derive(FromRow)]
struct AlbumArtistRelation {
    track_key: TrackKey,
    artist_key: ArtistKey,
    name: String,
}

#[derive(FromRow)]
struct TrackGenreRelation {
    track_key: TrackKey,
    genre_key: GenreKey,
    name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrackMetadataWrite {
    pub title: String,
    pub normalized_search: String,
    pub display_album: String,
    pub display_artist: String,
    pub sort_text: String,
    pub duration_millis: i64,
    pub disc_number: i64,
    pub track_number: i64,
    pub year: Option<i64>,
    pub release_date: Option<String>,
    pub date_added: Option<String>,
    pub media_uri: Option<String>,
    pub source_format: Option<String>,
    pub comment: Option<String>,
    pub bpm: Option<i64>,
    pub musicbrainz_recording_id: Option<String>,
    pub musicbrainz_release_track_id: Option<String>,
    pub cue_path: Option<String>,
    pub cue_start_millis: Option<i64>,
    pub cue_end_millis: Option<i64>,
    pub loudness_analysis_key: [u8; 32],
}

impl Database {
    pub async fn track_key_by_object(
        &self,
        source: SourceKey,
        object_id: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<TrackKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result =
            sqlx::query_scalar("SELECT track_key FROM tracks WHERE source_key=?1 AND object_id=?2")
                .bind(source)
                .bind(object_id)
                .fetch_optional(&mut *connection)
                .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn track_keys_by_objects(
        &self,
        source: SourceKey,
        object_ids: &[String],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackKey>> {
        if object_ids.is_empty() {
            return Ok(Vec::new());
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut result = Vec::with_capacity(object_ids.len());
        for page in object_ids.chunks(TRACK_ROW_LIMIT) {
            let mut query = QueryBuilder::<Sqlite>::new("WITH requested(object_id,position) AS (");
            query.push_values(page.iter().enumerate(), |mut row, (position, object_id)| {
                row.push_bind(object_id).push_bind(position as i64);
            });
            query.push(
                ") SELECT track.track_key FROM requested
                 JOIN tracks track ON track.object_id=requested.object_id
                 WHERE track.source_key=",
            );
            query.push_bind(source).push(" ORDER BY requested.position");
            result.extend(
                query
                    .build_query_scalar::<TrackKey>()
                    .persistent(false)
                    .fetch_all(&mut *connection)
                    .await?,
            );
        }
        Database::clear_progress(&mut connection).await?;
        Ok(result)
    }

    pub async fn track_order(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        favorites_only: bool,
        sort: TrackSort,
        descending: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackKey>> {
        self.track_order_where(
            source,
            sort,
            descending,
            favorites_only,
            folder,
            "",
            None,
            cancellation,
        )
        .await
    }

    pub async fn track_route_page(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        favorites_only: bool,
        filter: &str,
        sort: TrackSort,
        descending: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<TrackRoutePage> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let order = load_track_order(
            &mut transaction,
            source,
            sort,
            descending,
            favorites_only,
            folder,
            filter,
            false,
        )
        .await?;
        let first_rows = load_track_rows(
            &mut transaction,
            source,
            &order[..order.len().min(TRACK_ROW_LIMIT)],
        )
        .await?;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(TrackRoutePage { order, first_rows })
    }

    pub async fn live_folder_track_order(
        &self,
        source: SourceKey,
        candidates: &[TrackKey],
        filter: &str,
        sort: TrackSort,
        descending: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackKey>> {
        self.track_order_where(
            source,
            sort,
            descending,
            false,
            None,
            filter,
            Some(candidates),
            cancellation,
        )
        .await
    }

    async fn track_order_where(
        &self,
        source: SourceKey,
        sort: TrackSort,
        descending: bool,
        favorites_only: bool,
        folder: Option<FolderKey>,
        filter: &str,
        candidates: Option<&[TrackKey]>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackKey>> {
        if let Some(candidates) = candidates {
            let mut writer = self.writer().await?;
            let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
            stage_folder_route_tracks(connection, candidates).await?;
            return load_track_order(
                connection,
                source,
                sort,
                descending,
                favorites_only,
                folder,
                filter,
                true,
            )
            .await;
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = load_track_order(
            &mut connection,
            source,
            sort,
            descending,
            favorites_only,
            folder,
            filter,
            false,
        )
        .await;
        Database::clear_progress(&mut connection).await?;
        result
    }

    pub async fn track_rows(
        &self,
        source: SourceKey,
        keys: &[TrackKey],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackRow>> {
        if keys.len() > TRACK_ROW_LIMIT {
            return Err(LibraryError::InvalidRequest(format!(
                "Track row reads are limited to {TRACK_ROW_LIMIT} keys"
            )));
        }
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let result = load_track_rows(&mut transaction, source, keys).await;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        result
    }

    pub async fn update_track_metadata(
        &self,
        source: SourceKey,
        key: TrackKey,
        write: TrackMetadataWrite,
    ) -> LibraryResult<Option<TrackRow>> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let changed = sqlx::query(
            "UPDATE tracks SET
                 title=?3, normalized_search=?4, display_album=?5,
                 display_artist=?6, sort_text=?7, duration_millis=?8,
                 disc_number=?9, track_number=?10, year=?11,
                 release_date=?12, date_added=?13, media_uri=?14,
                 source_format=?15, comment=?16, bpm=?17,
                 musicbrainz_recording_id=?18, musicbrainz_release_track_id=?19,
                 cue_path=?20, cue_start_millis=?21, cue_end_millis=?22,
                 source_loudness_analysis_key=?23,
                 loudness_analysis_key=COALESCE((SELECT loudness_analysis_key FROM local_access_files access WHERE access.source_key=tracks.source_key AND access.track_object_id=tracks.object_id ORDER BY CASE access.origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END,access.local_access_file_key LIMIT 1),?23)
             WHERE source_key=?1 AND track_key=?2",
        )
        .bind(source)
        .bind(key)
        .bind(write.title)
        .bind(write.normalized_search)
        .bind(write.display_album)
        .bind(write.display_artist)
        .bind(write.sort_text)
        .bind(write.duration_millis)
        .bind(write.disc_number)
        .bind(write.track_number)
        .bind(write.year)
        .bind(write.release_date)
        .bind(write.date_added)
        .bind(write.media_uri)
        .bind(write.source_format)
        .bind(write.comment)
        .bind(write.bpm)
        .bind(write.musicbrainz_recording_id)
        .bind(write.musicbrainz_release_track_id)
        .bind(write.cue_path)
        .bind(write.cue_start_millis)
        .bind(write.cue_end_millis)
        .bind(write.loudness_analysis_key.as_slice())
        .execute(&mut *transaction)
        .await?;
        let row = if changed.rows_affected() == 1 {
            if let Some(album) = sqlx::query_scalar::<_, Option<AlbumKey>>(
                "SELECT album_key FROM tracks WHERE track_key=?1",
            )
            .bind(key)
            .fetch_one(&mut *transaction)
            .await?
            {
                recompute_album_loudness_key(&mut transaction, album).await?;
            }
            load_track_rows(&mut transaction, source, &[key])
                .await?
                .pop()
        } else {
            None
        };
        transaction.commit().await?;
        Ok(row)
    }
}

async fn stage_folder_route_tracks(
    connection: &mut SqliteConnection,
    candidates: &[TrackKey],
) -> LibraryResult<()> {
    sqlx::query(
        "CREATE TEMP TABLE IF NOT EXISTS folder_route_tracks(
            track_key INTEGER PRIMARY KEY
         ) WITHOUT ROWID",
    )
    .execute(&mut *connection)
    .await?;
    sqlx::query("DELETE FROM temp.folder_route_tracks")
        .execute(&mut *connection)
        .await?;
    for page in candidates.chunks(TRACK_ROW_LIMIT) {
        let mut insert = QueryBuilder::<Sqlite>::new(
            "INSERT OR IGNORE INTO temp.folder_route_tracks(track_key) ",
        );
        insert.push_values(page, |mut row, key| {
            row.push_bind(*key);
        });
        insert.build().execute(&mut *connection).await?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn load_track_order(
    connection: &mut SqliteConnection,
    source: SourceKey,
    sort: TrackSort,
    descending: bool,
    favorites_only: bool,
    folder: Option<FolderKey>,
    filter: &str,
    folder_subset: bool,
) -> LibraryResult<Vec<TrackKey>> {
    let simple = sort.order_sql(descending);
    let mut query = if matches!(sort, TrackSort::LastPlayed | TrackSort::PlayCount) {
        QueryBuilder::<Sqlite>::new(
            "WITH listen_activity AS (SELECT track_key,count(*) play_count,max(started_at) last_played FROM listens WHERE source_key=",
        )
    } else {
        QueryBuilder::<Sqlite>::new(
            "SELECT track.track_key FROM tracks track WHERE track.source_key=",
        )
    };
    if matches!(sort, TrackSort::LastPlayed | TrackSort::PlayCount) {
        query.push_bind(source).push(" AND track_key IS NOT NULL GROUP BY track_key), activity AS (SELECT track.track_key,COALESCE(baseline.play_count,0)+COALESCE(listen.play_count,0) play_count,CASE WHEN baseline.last_played_at IS NULL THEN listen.last_played WHEN listen.last_played IS NULL THEN baseline.last_played_at ELSE max(baseline.last_played_at,listen.last_played) END last_played FROM tracks track LEFT JOIN activity_baseline baseline ON baseline.source_key=track.source_key AND baseline.track_object_id=track.object_id AND baseline.period='lifetime' AND baseline.item_kind='track' LEFT JOIN listen_activity listen USING(track_key) WHERE track.source_key=").push_bind(source).push(") SELECT track.track_key FROM tracks track JOIN activity USING(track_key) WHERE track.source_key=").push_bind(source);
    } else {
        query.push_bind(source);
    }
    if folder_subset {
        query.push(" AND EXISTS (SELECT 1 FROM temp.folder_route_tracks candidate WHERE candidate.track_key=track.track_key)");
    }
    let filter: String = filter.trim().to_lowercase().chars().take(256).collect();
    query.push(" AND (").push_bind(!favorites_only).push(" OR COALESCE(track.user_favorite,track.source_favorite)=1) AND (").push_bind(folder).push(" IS NULL OR EXISTS (SELECT 1 FROM track_folders relation WHERE relation.track_key=track.track_key AND relation.folder_key=").push_bind(folder).push(")) AND (").push_bind(filter.is_empty()).push(" OR instr(track.normalized_search,").push_bind(&filter).push(")>0 OR CAST(track.year AS TEXT)=").push_bind(&filter).push(") ORDER BY ");
    if matches!(sort, TrackSort::LastPlayed) {
        query.push(if descending {
            "activity.last_played DESC NULLS LAST"
        } else {
            "activity.last_played ASC NULLS LAST"
        });
    } else if matches!(sort, TrackSort::PlayCount) {
        query.push(if descending {
            "activity.play_count DESC"
        } else {
            "activity.play_count ASC"
        });
    } else {
        query.push(simple);
    }
    query.push(", track.track_key");
    Ok(query
        .build_query_scalar::<TrackKey>()
        .persistent(false)
        .fetch_all(connection)
        .await?)
}

pub(crate) async fn load_track_rows(
    connection: &mut SqliteConnection,
    source: SourceKey,
    keys: &[TrackKey],
) -> LibraryResult<Vec<TrackRow>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let mut query = QueryBuilder::<Sqlite>::new("WITH requested(track_key, position) AS (");
    query.push_values(keys.iter().enumerate(), |mut row, (position, key)| {
        row.push_bind(*key).push_bind(position as i64);
    });
    query.push(") SELECT track.track_key,track.source_key,source.object_id source_id,track.object_id,track.album_key,track.title,track.display_album,track.display_artist,track.duration_millis,track.disc_number,track.track_number,track.year,track.release_date,track.date_added,track.media_uri,track.source_format,track.comment,track.bpm,track.musicbrainz_recording_id,track.musicbrainz_release_track_id,track.cue_path,track.cue_start_millis,track.cue_end_millis,track.loudness_analysis_key,COALESCE(album.artwork_binding,track.artwork_binding) artwork_binding,COALESCE(track.user_favorite,track.source_favorite) favorite,COALESCE(track.user_rating,track.source_rating)/10 rating,(SELECT max(value) FROM (SELECT baseline.last_played_at value FROM activity_baseline baseline WHERE baseline.source_key=track.source_key AND baseline.track_object_id=track.object_id AND baseline.period='lifetime' AND baseline.item_kind='track' UNION ALL SELECT listen.started_at FROM listens listen WHERE listen.source_key=track.source_key AND listen.track_key=track.track_key)) last_played,COALESCE((SELECT baseline.play_count FROM activity_baseline baseline WHERE baseline.source_key=track.source_key AND baseline.track_object_id=track.object_id AND baseline.period='lifetime' AND baseline.item_kind='track'),0)+(SELECT count(*) FROM listens listen WHERE listen.source_key=track.source_key AND listen.track_key=track.track_key) play_count,COALESCE((SELECT baseline.skip_count FROM activity_baseline baseline WHERE baseline.source_key=track.source_key AND baseline.track_object_id=track.object_id AND baseline.period='lifetime' AND baseline.item_kind='track'),0)+COALESCE((SELECT sum(listen.skipped) FROM listens listen WHERE listen.source_key=track.source_key AND listen.track_key=track.track_key),0) skip_count,EXISTS(SELECT 1 FROM local_access_files access WHERE access.source_key=track.source_key AND access.track_object_id=track.object_id AND access.origin='download') is_downloaded FROM requested JOIN tracks track USING(track_key) JOIN sources source ON source.source_key=track.source_key LEFT JOIN albums album ON album.album_key=track.album_key WHERE track.source_key=");
    query.push_bind(source).push(" ORDER BY requested.position");
    let scalars = query
        .build_query_as::<TrackScalar>()
        .persistent(false)
        .fetch_all(&mut *connection)
        .await?;
    let mut artists = BTreeMap::<TrackKey, Vec<TrackArtistLink>>::new();
    let mut query = QueryBuilder::<Sqlite>::new("WITH requested(track_key, position) AS (");
    query.push_values(keys.iter().enumerate(), |mut row, (position, key)| {
        row.push_bind(*key).push_bind(position as i64);
    });
    query.push("),unique_requested AS (SELECT track_key,min(position) position FROM requested GROUP BY track_key) SELECT relation.track_key,artist.artist_key,artist.name FROM unique_requested requested JOIN track_artists relation USING(track_key) JOIN artists artist USING(artist_key) ORDER BY requested.position,relation.position");
    for relation in query
        .build_query_as::<TrackArtistRelation>()
        .persistent(false)
        .fetch_all(&mut *connection)
        .await?
    {
        artists
            .entry(relation.track_key)
            .or_default()
            .push(TrackArtistLink {
                artist_key: relation.artist_key,
                name: relation.name,
            });
    }
    let mut album_artists = BTreeMap::<TrackKey, Vec<TrackArtistLink>>::new();
    let mut query = QueryBuilder::<Sqlite>::new("WITH requested(track_key, position) AS (");
    query.push_values(keys.iter().enumerate(), |mut row, (position, key)| {
        row.push_bind(*key).push_bind(position as i64);
    });
    query.push("),unique_requested AS (SELECT track_key,min(position) position FROM requested GROUP BY track_key) SELECT track.track_key,artist.artist_key,artist.name FROM unique_requested requested JOIN tracks track USING(track_key) JOIN album_artists relation USING(album_key) JOIN artists artist USING(artist_key) ORDER BY requested.position,relation.position");
    for relation in query
        .build_query_as::<AlbumArtistRelation>()
        .persistent(false)
        .fetch_all(&mut *connection)
        .await?
    {
        album_artists
            .entry(relation.track_key)
            .or_default()
            .push(TrackArtistLink {
                artist_key: relation.artist_key,
                name: relation.name,
            });
    }
    let mut genres = BTreeMap::<TrackKey, Vec<TrackGenreLink>>::new();
    let mut query = QueryBuilder::<Sqlite>::new("WITH requested(track_key, position) AS (");
    query.push_values(keys.iter().enumerate(), |mut row, (position, key)| {
        row.push_bind(*key).push_bind(position as i64);
    });
    query.push("),unique_requested AS (SELECT track_key,min(position) position FROM requested GROUP BY track_key) SELECT relation.track_key,genre.genre_key,genre.name FROM unique_requested requested JOIN track_genres relation USING(track_key) JOIN genres genre USING(genre_key) ORDER BY requested.position,relation.position");
    for relation in query
        .build_query_as::<TrackGenreRelation>()
        .persistent(false)
        .fetch_all(&mut *connection)
        .await?
    {
        genres
            .entry(relation.track_key)
            .or_default()
            .push(TrackGenreLink {
                genre_key: relation.genre_key,
                name: relation.name,
            });
    }
    let mut rows = Vec::with_capacity(scalars.len());
    for scalar in scalars {
        let key = scalar.track_key;
        let loudness_analysis_key =
            scalar
                .loudness_analysis_key
                .as_slice()
                .try_into()
                .map_err(|_| {
                    LibraryError::InvalidStore(
                        "Track loudness analysis key is not 32 bytes".to_string(),
                    )
                })?;
        rows.push(TrackRow {
            track_key: scalar.track_key,
            source_key: scalar.source_key,
            source_id: scalar.source_id,
            object_id: scalar.object_id,
            album_key: scalar.album_key,
            title: scalar.title,
            display_album: scalar.display_album,
            display_artist: scalar.display_artist,
            duration_millis: scalar.duration_millis,
            disc_number: scalar.disc_number,
            track_number: scalar.track_number,
            year: scalar.year,
            release_date: scalar.release_date,
            date_added: scalar.date_added,
            media_uri: scalar.media_uri,
            source_format: scalar.source_format,
            comment: scalar.comment,
            bpm: scalar.bpm,
            musicbrainz_recording_id: scalar.musicbrainz_recording_id,
            musicbrainz_release_track_id: scalar.musicbrainz_release_track_id,
            cue_path: scalar.cue_path,
            cue_start_millis: scalar.cue_start_millis,
            cue_end_millis: scalar.cue_end_millis,
            loudness_analysis_key,
            artwork_binding: scalar.artwork_binding,
            favorite: scalar.favorite,
            rating: scalar.rating,
            last_played: scalar.last_played,
            play_count: scalar.play_count,
            skip_count: scalar.skip_count,
            is_downloaded: scalar.is_downloaded,
            artists: artists.get(&key).cloned().unwrap_or_default(),
            album_artists: album_artists.get(&key).cloned().unwrap_or_default(),
            genres: genres.get(&key).cloned().unwrap_or_default(),
        });
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use tracing::field::{Field, Visit};
    use tracing_subscriber::{Layer, layer::Context, prelude::*, registry::LookupSpan};

    use super::*;
    use crate::{db::open_writer, schema};

    #[derive(Clone)]
    struct TrackRowCommands(Arc<AtomicUsize>);

    struct StatementVisitor {
        requested_window: bool,
    }

    impl Visit for StatementVisitor {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            if field.name() == "db.statement"
                && format!("{value:?}").contains("WITH requested(track_key, position)")
            {
                self.requested_window = true;
            }
        }
    }

    impl<S> Layer<S> for TrackRowCommands
    where
        S: tracing::Subscriber + for<'lookup> LookupSpan<'lookup>,
    {
        fn on_event(&self, event: &tracing::Event<'_>, _context: Context<'_, S>) {
            if event.metadata().target() != "sqlx::query" {
                return;
            }
            let mut visitor = StatementVisitor {
                requested_window: false,
            };
            event.record(&mut visitor);
            if visitor.requested_window {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    #[tokio::test]
    async fn bounded_track_row_window_uses_four_commands() {
        let commands = Arc::new(AtomicUsize::new(0));
        tracing::subscriber::set_global_default(
            tracing_subscriber::registry().with(TrackRowCommands(commands.clone())),
        )
        .expect("install Track row SQL trace");
        let file = tempfile::NamedTempFile::new().expect("create Track row Store");
        let mut connection = open_writer(file.path())
            .await
            .expect("open Track row Store");
        schema::initialize(&mut connection)
            .await
            .expect("initialize Track row Store");
        let source = sqlx::query_scalar::<_, SourceKey>("INSERT INTO sources(object_id,display_name,normalized_name,catalog_digest,artwork_digest) VALUES ('source','Source','source',zeroblob(32),zeroblob(32)) RETURNING source_key")
            .fetch_one(&mut connection).await.expect("insert source");
        let artist = sqlx::query_scalar::<_, ArtistKey>("INSERT INTO artists(source_key,object_id,name,normalized_name,sort_text,source_favorite) VALUES (?1,'artist','Artist','artist','artist',0) RETURNING artist_key")
            .bind(source).fetch_one(&mut connection).await.expect("insert Artist");
        let genre = sqlx::query_scalar::<_, GenreKey>("INSERT INTO genres(source_key,object_id,name,normalized_name,sort_text) VALUES (?1,'genre','Genre','genre','genre') RETURNING genre_key")
            .bind(source).fetch_one(&mut connection).await.expect("insert Genre");
        sqlx::query("WITH RECURSIVE sequence(value) AS (VALUES(0) UNION ALL SELECT value+1 FROM sequence WHERE value<255) INSERT INTO tracks(source_key,object_id,title,normalized_search,display_album,display_artist,sort_text,duration_millis,disc_number,track_number,source_favorite) SELECT ?1,'track-'||value,'Track','track','', 'Artist','track-'||printf('%03d',value),1000,0,value,0 FROM sequence")
            .bind(source).execute(&mut connection).await.expect("insert Track window");
        sqlx::query("INSERT INTO track_artists(track_key,artist_key,position) SELECT track_key,?1,0 FROM tracks WHERE source_key=?2")
            .bind(artist).bind(source).execute(&mut connection).await.expect("insert Track Artists");
        sqlx::query("INSERT INTO track_genres(track_key,genre_key,position) SELECT track_key,?1,0 FROM tracks WHERE source_key=?2")
            .bind(genre).bind(source).execute(&mut connection).await.expect("insert Track Genres");
        let keys = sqlx::query_scalar::<_, TrackKey>(
            "SELECT track_key FROM tracks WHERE source_key=?1 ORDER BY track_key",
        )
        .bind(source)
        .fetch_all(&mut connection)
        .await
        .expect("read Track window keys");
        commands.store(0, Ordering::Relaxed);
        let one = load_track_rows(&mut connection, source, &keys[..1])
            .await
            .expect("load one Track row");
        let one_count = commands.load(Ordering::Relaxed);
        commands.store(0, Ordering::Relaxed);
        let window = load_track_rows(&mut connection, source, &keys)
            .await
            .expect("load bounded Track row window");
        let window_count = commands.load(Ordering::Relaxed);
        assert_eq!(one.len(), 1);
        assert_eq!(window.len(), 256);
        assert_eq!(one_count, 4);
        assert_eq!(window_count, 4);
    }

    #[tokio::test]
    async fn object_identity_order_pages_without_losing_a_large_folder() {
        let file = tempfile::NamedTempFile::new().expect("create Track identity Store");
        let mut connection = open_writer(file.path())
            .await
            .expect("open Track identity Store");
        schema::initialize(&mut connection)
            .await
            .expect("initialize Track identity Store");
        let source = sqlx::query_scalar::<_, SourceKey>("INSERT INTO sources(object_id,display_name,normalized_name,catalog_digest,artwork_digest) VALUES ('source','Source','source',zeroblob(32),zeroblob(32)) RETURNING source_key")
            .fetch_one(&mut connection).await.expect("insert source");
        sqlx::query("WITH RECURSIVE sequence(value) AS (VALUES(0) UNION ALL SELECT value+1 FROM sequence WHERE value<299) INSERT INTO tracks(source_key,object_id,title,normalized_search,display_album,display_artist,sort_text,duration_millis,disc_number,track_number,source_favorite) SELECT ?1,'track-'||printf('%03d',value),'Track','track','','Artist','track-'||printf('%03d',value),1000,0,value,0 FROM sequence")
            .bind(source).execute(&mut connection).await.expect("insert large Folder Track order");
        drop(connection);

        let database = Database::open(file.path())
            .await
            .expect("open Track identity Database");
        let object_ids = (0..300)
            .map(|index| format!("track-{index:03}"))
            .collect::<Vec<_>>();
        let keys = database
            .track_keys_by_objects(source, &object_ids, &ReadCancellation::new())
            .await
            .expect("map complete large Folder identity order");
        assert_eq!(keys.len(), object_ids.len());
        assert!(keys.windows(2).all(|pair| pair[0] < pair[1]));
        let mut reversed = keys.clone();
        reversed.reverse();
        let ordered = database
            .live_folder_track_order(
                source,
                &reversed,
                "track",
                TrackSort::Title,
                false,
                &ReadCancellation::new(),
            )
            .await
            .expect("sort complete provider-live Folder order");
        assert_eq!(ordered, keys);
        assert!(
            database
                .live_folder_track_order(
                    source,
                    &reversed,
                    "missing",
                    TrackSort::Title,
                    false,
                    &ReadCancellation::new(),
                )
                .await
                .expect("filter provider-live Folder order")
                .is_empty()
        );
    }
}
