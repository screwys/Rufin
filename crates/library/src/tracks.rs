//! Owns complete Track orders, bounded final rows, details, and Track metadata writes.
//! Each bounded row query includes its ordered artist and genre links.

use sqlx::sqlite::SqliteRow;
use sqlx::{Connection, FromRow, QueryBuilder, Row, Sqlite, SqliteConnection};

use crate::{
    AlbumKey, ArtistKey, Database, FolderKey, GenreKey, LibraryError, LibraryResult,
    ReadCancellation, RouteSeedWindow, SourceKey, TrackKey,
    loudness::{recompute_album_loudness_key, source_track_loudness_key},
};

const TRACK_ROW_LIMIT: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
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

    pub(crate) fn order_terms(self, descending: bool) -> Vec<String> {
        let fields: &[(&str, bool)] = match self {
            Self::Title => &[("track.sort_text", false), ("track.track_key", false)],
            Self::TrackNumber => &[
                ("track.disc_number", false),
                ("track.track_number", false),
                ("track.sort_text", false),
                ("track.track_key", false),
            ],
            Self::Artist => &[
                ("track.display_artist", false),
                ("track.sort_text", false),
                ("track.track_key", false),
            ],
            Self::AlbumArtist => &[
                (
                    "(SELECT artist.sort_text FROM albums album JOIN album_artists relation USING(album_key) JOIN artists artist USING(artist_key) WHERE album.album_key=track.album_key ORDER BY relation.position LIMIT 1)",
                    true,
                ),
                ("track.sort_text", false),
                ("track.track_key", false),
            ],
            Self::Album => &[
                ("track.display_album", false),
                ("track.sort_text", false),
                ("track.track_key", false),
            ],
            Self::Year => &[
                ("track.year", true),
                ("track.sort_text", false),
                ("track.track_key", false),
            ],
            Self::ReleaseDate => &[
                ("track.release_date", true),
                ("track.sort_text", false),
                ("track.track_key", false),
            ],
            Self::DateAdded => &[
                ("track.date_added", true),
                ("track.sort_text", false),
                ("track.track_key", false),
            ],
            Self::UserRating => &[
                (
                    "COALESCE((SELECT state.rating FROM user_media_state state WHERE state.media_uri=track.media_uri),track.source_rating)",
                    true,
                ),
                ("track.sort_text", false),
                ("track.track_key", false),
            ],
            Self::Genre => &[
                (
                    "(SELECT genre.sort_text FROM track_genres relation JOIN genres genre USING(genre_key) WHERE relation.track_key=track.track_key ORDER BY relation.position LIMIT 1)",
                    true,
                ),
                ("track.sort_text", false),
                ("track.track_key", false),
            ],
            Self::Bpm => &[
                ("track.bpm", true),
                ("track.sort_text", false),
                ("track.track_key", false),
            ],
            Self::Duration => &[
                ("track.duration_millis", false),
                ("track.sort_text", false),
                ("track.track_key", false),
            ],
            Self::Favorite => &[
                (
                    "COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=track.media_uri),track.source_favorite)",
                    false,
                ),
                ("track.sort_text", false),
                ("track.track_key", false),
            ],
            Self::LastPlayed => &[("activity.last_played", true), ("track.track_key", false)],
            Self::PlayCount => &[("activity.play_count", false), ("track.track_key", false)],
        };
        let mut terms = Vec::new();
        for (i, (field, nulls_last)) in fields.iter().enumerate() {
            let desc = descending && (i == 0 || (self == Self::TrackNumber && i == 1));
            let field = if self == Self::TrackNumber && i < 2 {
                format!("coalesce({field},-1)")
            } else {
                (*field).to_string()
            };
            terms.push(format!(
                "{field} {}{}",
                if desc { "DESC" } else { "ASC" },
                if *nulls_last { " NULLS LAST" } else { "" },
            ));
        }
        terms
    }
}

#[derive(Clone, Debug, FromRow, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct TrackArtistLink {
    pub artist_key: ArtistKey,
    pub media_uri: String,
    pub name: String,
}

// Included only in bounded row projections, with track and album already joined by identity.
pub(crate) const TRACK_LINK_COLUMNS: &str = "
    album.media_uri album_media_uri,
    (SELECT json_group_array(json_object('artist_key',artist.artist_key,'media_uri',artist.media_uri,'name',artist.name) ORDER BY credit.position)
       FROM track_artists credit JOIN artists artist USING(artist_key)
       WHERE credit.track_key=track.track_key) artists,
    (SELECT json_group_array(json_object('artist_key',artist.artist_key,'media_uri',artist.media_uri,'name',artist.name) ORDER BY credit.position)
       FROM album_artists credit JOIN artists artist USING(artist_key)
       WHERE credit.album_key=track.album_key) album_artists,";

#[derive(Clone, Debug, FromRow, PartialEq, serde::Deserialize)]
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
    pub album_media_uri: Option<String>,
    pub title: String,
    pub album: String,
    pub artist: String,
    pub album_display_artist: Option<String>,
    pub duration_millis: i64,
    pub disc_number: i64,
    pub track_number: i64,
    pub year: Option<i64>,
    pub release_date: Option<String>,
    pub date_added: Option<String>,
    pub media_uri: String,
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
    pub musicbrainz_album_id: Option<String>,
    pub musicbrainz_release_group_id: Option<String>,
    pub primary_artist_musicbrainz_id: Option<String>,
    pub artists: Vec<TrackArtistLink>,
    pub album_artists: Vec<TrackArtistLink>,
    pub genres: Vec<TrackGenreLink>,
}

impl<'row> FromRow<'row, SqliteRow> for TrackRow {
    fn from_row(row: &'row SqliteRow) -> Result<Self, sqlx::Error> {
        let loudness_analysis_key = row
            .try_get::<Vec<u8>, _>("loudness_analysis_key")?
            .try_into()
            .map_err(|_| {
                sqlx::Error::Decode("Track loudness analysis key is not 32 bytes".into())
            })?;
        Ok(Self {
            track_key: row.try_get("track_key")?,
            source_key: row.try_get("source_key")?,
            source_id: row.try_get("source_id")?,
            object_id: row.try_get("object_id")?,
            album_key: row.try_get("album_key")?,
            album_media_uri: row.try_get("album_media_uri")?,
            title: row.try_get("title")?,
            album: row.try_get("album")?,
            artist: row.try_get("artist")?,
            album_display_artist: row.try_get("album_display_artist")?,
            duration_millis: row.try_get("duration_millis")?,
            disc_number: row.try_get("disc_number")?,
            track_number: row.try_get("track_number")?,
            year: row.try_get("year")?,
            release_date: row.try_get("release_date")?,
            date_added: row.try_get("date_added")?,
            media_uri: row.try_get("media_uri")?,
            source_format: row.try_get("source_format")?,
            comment: row.try_get("comment")?,
            bpm: row.try_get("bpm")?,
            musicbrainz_recording_id: row.try_get("musicbrainz_recording_id")?,
            musicbrainz_release_track_id: row.try_get("musicbrainz_release_track_id")?,
            cue_path: row.try_get("cue_path")?,
            cue_start_millis: row.try_get("cue_start_millis")?,
            cue_end_millis: row.try_get("cue_end_millis")?,
            loudness_analysis_key,
            artwork_binding: row.try_get("artwork_binding")?,
            favorite: row.try_get("favorite")?,
            rating: row.try_get("rating")?,
            last_played: row.try_get("last_played")?,
            play_count: row.try_get("play_count")?,
            skip_count: row.try_get("skip_count")?,
            is_downloaded: row.try_get("is_downloaded")?,
            musicbrainz_album_id: row.try_get("musicbrainz_album_id")?,
            musicbrainz_release_group_id: row.try_get("musicbrainz_release_group_id")?,
            primary_artist_musicbrainz_id: row.try_get("primary_artist_musicbrainz_id")?,
            artists: serde_json::from_str(row.try_get("artists")?)
                .map_err(|error| sqlx::Error::Decode(error.into()))?,
            album_artists: serde_json::from_str(row.try_get("album_artists")?)
                .map_err(|error| sqlx::Error::Decode(error.into()))?,
            genres: serde_json::from_str(row.try_get("genres")?)
                .map_err(|error| sqlx::Error::Decode(error.into()))?,
        })
    }
}

#[derive(Clone, Debug)]
pub struct TrackRoutePage {
    pub query: TrackQuery,
    pub count: usize,
    pub disc_sections: Vec<(u32, i64)>,
    pub first_row_position: usize,
    pub first_rows: Vec<TrackRow>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TrackQuery {
    pub source: SourceKey,
    pub collection: Option<crate::QueueCollection>,
    pub folder: Option<FolderKey>,
    pub favorites_only: bool,
}

impl TrackQuery {
    pub(crate) fn sql(
        &self,
        filter: &str,
        sort: TrackSort,
        descending: bool,
    ) -> crate::source_window::SourceQuery {
        let mut query = match &self.collection {
            Some(collection) => crate::collections::collection_track_query(
                self.source,
                sort,
                descending,
                collection,
            ),
            None => track_query(self.source, sort, descending, false, None, ""),
        };
        track_filter(&mut query, self.folder, filter, self.favorites_only);
        query
    }

    pub fn queue_query(&self) -> crate::QueueQuery {
        match &self.collection {
            Some(collection) => crate::QueueQuery::Collection {
                collection: collection.clone(),
                favorites_only: self.favorites_only,
            },
            None => crate::QueueQuery::Tracks {
                source: self.source,
                favorites_only: self.favorites_only,
                recursive: true,
            },
        }
    }
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
    pub source_format: Option<String>,
    pub comment: Option<String>,
    pub bpm: Option<i64>,
    pub musicbrainz_recording_id: Option<String>,
    pub musicbrainz_release_track_id: Option<String>,
    pub cue_path: Option<String>,
    pub cue_start_millis: Option<i64>,
    pub cue_end_millis: Option<i64>,
}

impl Database {
    pub async fn selected_track_uris(
        &self,
        input: &crate::QueueInput,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<String>> {
        match input {
            crate::QueueInput::MediaUris { order, .. } => Ok(order.to_vec()),
            crate::QueueInput::TrackSelection {
                query,
                folder,
                filter,
                sort,
                descending,
                ranges,
            } => {
                let (_permit, mut connection) = self.acquire_general(cancellation).await?;
                let mut transaction = connection.begin().await?;
                let rows = query_selected_track_uris_on(
                    &mut transaction,
                    query,
                    *folder,
                    filter,
                    *sort,
                    *descending,
                    ranges,
                )
                .await?;
                transaction.commit().await?;
                Ok(rows)
            }
            _ => unreachable!("track selection input"),
        }
    }
    pub async fn track_count(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        favorites_only: bool,
        filter: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<i64> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let query = track_query(source, TrackSort::Title, false, false, folder, filter);
        Ok(if favorites_only {
            // Use the existing source-favorite index, then add locally favorited
            // tracks which are not source favorites. The sets do not overlap.
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT (SELECT count(*) FROM tracks track WHERE {} AND track.source_favorite=1
                    AND COALESCE((SELECT favorite FROM user_media_state state WHERE state.media_uri=track.media_uri),1)=1)
                 + (SELECT count(*) FROM user_media_state state CROSS JOIN tracks track ON track.media_uri=state.media_uri
                    WHERE state.favorite=1 AND track.source_favorite=0 AND {})",
                query.predicate, query.predicate
            )))
            .fetch_one(&mut *connection).await?
        } else {
            query.count(&mut connection).await?
        })
    }

    /// Reads one page without materializing the source's full track order.
    #[allow(clippy::too_many_arguments)]
    pub async fn track_page(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        favorites_only: bool,
        filter: &str,
        sort: TrackSort,
        descending: bool,
        offset: usize,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackRow>> {
        self.query_tracks_page(
            &TrackQuery {
                source,
                collection: None,
                folder,
                favorites_only,
            },
            filter,
            sort,
            descending,
            offset,
            limit,
            cancellation,
        )
        .await
    }

    pub async fn query_tracks_page(
        &self,
        query: &TrackQuery,
        filter: &str,
        sort: TrackSort,
        descending: bool,
        offset: usize,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let query = query.sql(filter, sort, descending);
        let sql = format!("{} LIMIT ?1 OFFSET ?2", query.select("track.track_key"));
        let keys = sqlx::query_scalar::<_, TrackKey>(sqlx::AssertSqlSafe(sql))
            .bind(limit.min(TRACK_ROW_LIMIT) as i64)
            .bind(offset.min(i64::MAX as usize) as i64)
            .persistent(false)
            .fetch_all(&mut *transaction)
            .await?;
        let rows = load_track_rows(&mut transaction, &keys).await?;
        transaction.commit().await?;
        Ok(rows)
    }

    pub async fn track_uris_for_source(
        &self,
        source: SourceKey,
        tracks: &[TrackKey],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<String>> {
        if tracks.is_empty() {
            return Ok(Vec::new());
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut query = QueryBuilder::<Sqlite>::new("WITH requested(track_key,ordinal) AS (");
        query.push_values(tracks.iter().enumerate(), |mut row, (ordinal, track)| {
            row.push_bind(*track).push_bind(ordinal as i64);
        });
        query
            .push(") SELECT track.media_uri FROM requested JOIN tracks track USING(track_key) WHERE track.source_key=")
            .push_bind(source)
            .push(" ORDER BY requested.ordinal");
        Ok(query
            .build_query_scalar::<String>()
            .persistent(false)
            .fetch_all(&mut *connection)
            .await?)
    }

    pub async fn track_source_by_uri(
        &self,
        media_uri: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<crate::SourceId>> {
        let mut connection = tokio::select! {
            result = self.acquire_reader() => result?,
            () = cancellation.cancelled() => return Err(LibraryError::ReadCancelled),
        };
        let source = sqlx::query_scalar::<_, String>(
            "SELECT source.object_id FROM tracks track JOIN sources source USING(source_key)
             WHERE track.media_uri=?1",
        )
        .bind(media_uri)
        .fetch_optional(&mut *connection)
        .await?;
        Ok(source.map(crate::SourceId::new))
    }

    pub async fn track_row_by_uri(
        &self,
        media_uri: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<TrackRow>> {
        self.track_rows_by_uri(&[media_uri.to_string()], cancellation)
            .await
            .map(|mut rows| rows.pop())
    }

    pub async fn track_rows_by_uri(
        &self,
        media_uris: &[String],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackRow>> {
        if media_uris.len() > TRACK_ROW_LIMIT {
            return Err(LibraryError::InvalidRequest(format!(
                "Track row reads are limited to {TRACK_ROW_LIMIT} media URIs"
            )));
        }
        if media_uris.is_empty() {
            return Ok(Vec::new());
        }
        let mut connection = tokio::select! {
            result = self.acquire_reader() => result?,
            () = cancellation.cancelled() => return Err(LibraryError::ReadCancelled),
        };
        let mut transaction = connection.begin().await?;
        let mut query = QueryBuilder::<Sqlite>::new("WITH requested(media_uri,ordinal) AS (");
        query.push_values(
            media_uris.iter().enumerate(),
            |mut row, (ordinal, media_uri)| {
                row.push_bind(media_uri).push_bind(ordinal as i64);
            },
        );
        query.push(
            ") SELECT track.track_key
               FROM requested JOIN tracks track USING(media_uri) ORDER BY requested.ordinal",
        );
        let keys = query
            .build_query_scalar::<TrackKey>()
            .persistent(false)
            .fetch_all(&mut *transaction)
            .await?;
        let result = load_track_rows(&mut transaction, &keys).await?;
        transaction.commit().await?;
        Ok(result)
    }

    pub async fn track_key_by_object(
        &self,
        source: SourceKey,
        object_id: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<TrackKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(
            sqlx::query_scalar("SELECT track_key FROM tracks WHERE source_key=?1 AND object_id=?2")
                .bind(source)
                .bind(object_id)
                .fetch_optional(&mut *connection)
                .await?,
        )
    }

    pub async fn track_media_uris_by_objects(
        &self,
        source: SourceKey,
        object_ids: &[String],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<String>> {
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
                ") SELECT track.media_uri FROM requested
                 JOIN tracks track ON track.object_id=requested.object_id
                 WHERE track.source_key=",
            );
            query.push_bind(source).push(" ORDER BY requested.position");
            result.extend(
                query
                    .build_query_scalar::<String>()
                    .persistent(false)
                    .fetch_all(&mut *connection)
                    .await?,
            );
        }
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
    ) -> LibraryResult<Vec<String>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let order = load_track_order(
            &mut connection,
            source,
            sort,
            descending,
            favorites_only,
            folder,
            "",
        )
        .await;
        Ok(order?.into_iter().map(|(_, media_uri)| media_uri).collect())
    }

    pub async fn query_track_route_page(
        &self,
        query: &TrackQuery,
        filter: &str,
        sort: TrackSort,
        descending: bool,
        window: RouteSeedWindow,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<TrackRoutePage> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let sql = query.sql(filter, sort, descending);
        let count = sql.count(&mut transaction).await?.max(0) as usize;
        let range = window.range(count);
        let disc_sections = if matches!(
            query.collection,
            Some(crate::QueueCollection::Album(_) | crate::QueueCollection::AlbumKey(_))
        ) {
            crate::collections::album_disc_sections(&sql, sort, descending, &mut transaction)
                .await?
        } else {
            Vec::new()
        };
        let keys = sqlx::query_scalar::<_, TrackKey>(sqlx::AssertSqlSafe(format!(
            "{} LIMIT ?1 OFFSET ?2",
            sql.select("track.track_key")
        )))
        .bind(range.len() as i64)
        .bind(range.start as i64)
        .persistent(false)
        .fetch_all(&mut *transaction)
        .await?;
        let first_rows = load_track_rows(&mut transaction, &keys).await?;
        transaction.commit().await?;
        Ok(TrackRoutePage {
            query: query.clone(),
            count,
            disc_sections,
            first_row_position: range.start,
            first_rows,
        })
    }

    pub async fn live_folder_track_order(
        &self,
        source: SourceKey,
        candidates: &[String],
        filter: &str,
        sort: TrackSort,
        descending: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<String>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut query = track_query(source, sort, descending, false, None, filter);
        query
            .predicate
            .push_str(" AND track.media_uri IN (SELECT value FROM json_each(?1))");
        Ok(
            sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(query.select("track.media_uri")))
                .bind(serde_json::to_string(candidates)?)
                .persistent(false)
                .fetch_all(&mut *connection)
                .await?,
        )
    }

    pub async fn track_rows(
        &self,
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
        let result = load_track_rows(&mut transaction, keys).await;
        transaction.commit().await?;
        result
    }

    pub async fn update_track_metadata(
        &self,
        source: SourceKey,
        key: TrackKey,
        write: TrackMetadataWrite,
    ) -> LibraryResult<crate::ScanOutcome> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let current = sqlx::query_as::<_, (String, Option<AlbumKey>)>(
            "SELECT media_uri,album_key FROM tracks WHERE source_key=?1 AND track_key=?2",
        )
        .bind(source)
        .bind(key)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some((media_uri, album)) = current else {
            transaction.commit().await?;
            return Ok(crate::ScanOutcome::Stale);
        };
        let loudness_analysis_key = source_track_loudness_key(
            Some(&media_uri),
            write.source_format.as_deref(),
            write.duration_millis,
            write.cue_path.as_deref(),
            write.cue_start_millis,
            write.cue_end_millis,
        );
        let changed = sqlx::query(
            "UPDATE tracks SET
                 title=?3, normalized_search=?4, display_album=?5,
                 display_artist=?6, sort_text=?7, duration_millis=?8,
                 disc_number=?9, track_number=?10, year=?11,
                 release_date=?12, date_added=?13,
                 source_format=?14, comment=?15, bpm=?16,
                 musicbrainz_recording_id=?17, musicbrainz_release_track_id=?18,
                 cue_path=?19, cue_start_millis=?20, cue_end_millis=?21,
                 source_loudness_analysis_key=?22,
                 loudness_analysis_key=COALESCE((SELECT loudness_analysis_key FROM local_access_files access WHERE access.media_uri=tracks.media_uri ORDER BY CASE access.origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END,access.local_access_file_key LIMIT 1),?22)
             WHERE source_key=?1 AND track_key=?2
               AND (title,normalized_search,display_album,display_artist,sort_text,duration_millis,
                    disc_number,track_number,year,release_date,date_added,source_format,comment,bpm,
                    musicbrainz_recording_id,musicbrainz_release_track_id,cue_path,cue_start_millis,cue_end_millis)
                   IS NOT (?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)",
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
        .bind(write.source_format)
        .bind(write.comment)
        .bind(write.bpm)
        .bind(write.musicbrainz_recording_id)
        .bind(write.musicbrainz_release_track_id)
        .bind(write.cue_path)
        .bind(write.cue_start_millis)
        .bind(write.cue_end_millis)
        .bind(loudness_analysis_key.as_slice())
        .execute(&mut *transaction)
        .await?;
        let changed = changed.rows_affected() == 1;
        if changed {
            if let Some(album) = album {
                recompute_album_loudness_key(&mut transaction, album).await?;
            }
        }
        let outcome = crate::scan::metadata_publication(&mut transaction, source, changed).await?;
        transaction.commit().await?;
        Ok(outcome)
    }
}

pub(crate) async fn query_selected_track_uris_on(
    connection: &mut SqliteConnection,
    query: &crate::QueueQuery,
    folder: Option<FolderKey>,
    filter: &str,
    sort: TrackSort,
    descending: bool,
    ranges: &[std::ops::Range<usize>],
) -> LibraryResult<Vec<String>> {
    let query = match query {
        crate::QueueQuery::Tracks {
            source,
            favorites_only,
            ..
        } => track_query(*source, sort, descending, *favorites_only, folder, filter),
        crate::QueueQuery::Collection {
            collection,
            favorites_only,
        } => {
            crate::collections::playback_query(
                connection,
                collection,
                folder,
                filter,
                sort,
                descending,
                *favorites_only,
            )
            .await?
        }
        crate::QueueQuery::SmartDisplay { key, source } => {
            return crate::smart_playlists::smart_sorted_uris_on(
                connection,
                *source,
                *key,
                folder,
                filter,
                sort,
                descending,
                crate::smart_playlists::now(),
                Some(ranges),
            )
            .await;
        }
        crate::QueueQuery::Smart { .. } => unreachable!("selection uses display order"),
    };
    crate::source_window::selected_values_on(connection, &query, &query.uri, ranges).await
}

async fn load_track_order(
    connection: &mut SqliteConnection,
    source: SourceKey,
    sort: TrackSort,
    descending: bool,
    favorites_only: bool,
    folder: Option<FolderKey>,
    filter: &str,
) -> LibraryResult<Vec<(TrackKey, String)>> {
    let query = track_query(source, sort, descending, favorites_only, folder, filter);
    Ok(sqlx::query_as::<_, (TrackKey, String)>(sqlx::AssertSqlSafe(
        query.select("track.track_key,track.media_uri"),
    ))
    .persistent(false)
    .fetch_all(connection)
    .await?)
}

pub(crate) fn track_query(
    source: SourceKey,
    sort: TrackSort,
    descending: bool,
    favorites_only: bool,
    folder: Option<FolderKey>,
    filter: &str,
) -> crate::source_window::SourceQuery {
    let mut query = crate::source_window::SourceQuery {
        from: "tracks track".into(),
        predicate: format!("track.source_key={}", source.raw()),
        order: sort.order_terms(descending),
        uri: "track.media_uri".into(),
        entry_key: "NULL".into(),
    };
    track_filter(&mut query, folder, filter, favorites_only);
    if sort.uses_activity() {
        for field in &mut query.order {
            *field=field.replace("activity.play_count","(track.local_play_count+COALESCE((SELECT play_count FROM activity_baseline baseline WHERE baseline.source_key=track.source_key AND baseline.track_object_id=track.object_id AND baseline.period='lifetime' AND baseline.item_kind='track'),0))")
                .replace("activity.last_played","(SELECT max(value) FROM (SELECT max(started_at) value FROM listens WHERE media_uri=track.media_uri UNION ALL SELECT last_played_at FROM activity_baseline baseline WHERE baseline.source_key=track.source_key AND baseline.track_object_id=track.object_id AND baseline.period='lifetime' AND baseline.item_kind='track'))");
        }
    }
    query
}

pub(crate) fn track_filter(
    query: &mut crate::source_window::SourceQuery,
    folder: Option<FolderKey>,
    filter: &str,
    favorites_only: bool,
) {
    if let Some(folder) = folder {
        query.predicate.push_str(&format!(" AND EXISTS(SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key={})",folder.raw()));
    }
    if favorites_only {
        query.predicate.push_str(" AND COALESCE((SELECT favorite FROM user_media_state state WHERE state.media_uri=track.media_uri),track.source_favorite)=1");
    }
    let filter: String = filter.trim().to_lowercase().chars().take(256).collect();
    if !filter.is_empty() {
        let filter = crate::source_window::quote(&filter);
        query.predicate.push_str(&format!(
            " AND (instr(track.normalized_search,{filter})>0 OR CAST(track.year AS TEXT)={filter})"
        ));
    }
}

pub(crate) async fn load_track_rows(
    connection: &mut SqliteConnection,
    keys: &[TrackKey],
) -> LibraryResult<Vec<TrackRow>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let mut query = QueryBuilder::<Sqlite>::new("WITH requested(track_key, position) AS (");
    query.push_values(keys.iter().enumerate(), |mut row, (position, key)| {
        row.push_bind(*key).push_bind(position as i64);
    });
    query.push(") SELECT ").push(TRACK_LINK_COLUMNS);
    query.push("track.track_key,track.source_key,source.object_id source_id,
                        track.object_id,track.album_key,track.title,
                        track.display_album album,track.display_artist artist,
                        album.display_artist album_display_artist,
                        track.duration_millis,track.disc_number,track.track_number,
                        track.year,track.release_date,track.date_added,track.media_uri,
                        track.source_format,track.comment,track.bpm,
                        track.musicbrainz_recording_id,track.musicbrainz_release_track_id,
                        track.cue_path,track.cue_start_millis,track.cue_end_millis,
                        track.loudness_analysis_key,track.artwork_binding,
                        COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=track.media_uri),track.source_favorite) favorite,
                        COALESCE((SELECT state.rating FROM user_media_state state WHERE state.media_uri=track.media_uri),track.source_rating)/10 rating,
                        (SELECT max(value) FROM (
                           SELECT baseline.last_played_at value FROM activity_baseline baseline
                           WHERE baseline.source_key=track.source_key
                             AND baseline.track_object_id=track.object_id
                             AND baseline.period='lifetime' AND baseline.item_kind='track'
                           UNION ALL SELECT max(listen.started_at) FROM listens listen
                           WHERE listen.media_uri=track.media_uri
                        )) last_played,
                        COALESCE((SELECT baseline.play_count FROM activity_baseline baseline
                                  WHERE baseline.source_key=track.source_key
                                    AND baseline.track_object_id=track.object_id
                                    AND baseline.period='lifetime' AND baseline.item_kind='track'),0)
                          +(SELECT count(*) FROM listens listen WHERE listen.media_uri=track.media_uri) play_count,
                        COALESCE((SELECT baseline.skip_count FROM activity_baseline baseline
                                  WHERE baseline.source_key=track.source_key
                                    AND baseline.track_object_id=track.object_id
                                    AND baseline.period='lifetime' AND baseline.item_kind='track'),0)
                          +COALESCE((SELECT sum(listen.skipped) FROM listens listen WHERE listen.media_uri=track.media_uri),0) skip_count,
                        EXISTS(SELECT 1 FROM local_access_files access
                               WHERE access.media_uri=track.media_uri AND access.origin='download') is_downloaded,
                        album.musicbrainz_release_id musicbrainz_album_id,
                        album.musicbrainz_release_group_id,
                        COALESCE(
                          (SELECT artist.musicbrainz_artist_id FROM track_artists credit
                           JOIN artists artist USING(artist_key)
                           WHERE credit.track_key=track.track_key ORDER BY credit.position LIMIT 1),
                          (SELECT artist.musicbrainz_artist_id FROM album_artists credit
                           JOIN artists artist USING(artist_key)
                           WHERE credit.album_key=track.album_key ORDER BY credit.position LIMIT 1)
                        ) primary_artist_musicbrainz_id,
                        (SELECT json_group_array(json_object('genre_key',genre.genre_key,'name',genre.name) ORDER BY relation.position)
                         FROM track_genres relation JOIN genres genre USING(genre_key)
                         WHERE relation.track_key=track.track_key) genres
                 FROM requested JOIN tracks track USING(track_key)
                 JOIN sources source ON source.source_key=track.source_key
                 LEFT JOIN albums album USING(album_key)
                 ORDER BY requested.position");
    Ok(query
        .build_query_as::<TrackRow>()
        .persistent(false)
        .fetch_all(connection)
        .await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db::open_writer, schema};

    #[tokio::test]
    async fn bounded_track_row_window_preserves_order_and_relations() {
        let file = tempfile::NamedTempFile::new().expect("create Track row Store");
        let mut connection = open_writer(file.path())
            .await
            .expect("open Track row Store");
        schema::initialize_durable(&mut connection)
            .await
            .expect("initialize durable Store");
        let mut catalog = open_writer(&file.path().with_extension("catalog.sqlite"))
            .await
            .unwrap();
        schema::initialize_catalog(&mut catalog).await.unwrap();
        drop(catalog);
        schema::attach_catalog(
            &mut connection,
            &file.path().with_extension("catalog.sqlite"),
        )
        .await
        .unwrap();
        let source = sqlx::query_scalar::<_, SourceKey>(
            "INSERT INTO sources(object_id,display_name,normalized_name,artwork_digest) VALUES ('source','Source','source',zeroblob(32)) RETURNING source_key",
        )
        .fetch_one(&mut connection)
        .await
        .expect("insert source");
        let artist = sqlx::query_scalar::<_, ArtistKey>("INSERT INTO artists(source_key,object_id,media_uri,name,normalized_name,sort_text,source_favorite) VALUES (?1,'artist','rufin:source/artist/%73%6F%75%72%63%65/%61%72%74%69%73%74','Artist','artist','artist',0) RETURNING artist_key")
            .bind(source).fetch_one(&mut connection).await.expect("insert Artist");
        let genre = sqlx::query_scalar::<_, GenreKey>("INSERT INTO genres(source_key,object_id,name,normalized_name,sort_text) VALUES (?1,'genre','Genre','genre','genre') RETURNING genre_key")
            .bind(source).fetch_one(&mut connection).await.expect("insert Genre");
        sqlx::query("WITH RECURSIVE sequence(value) AS (VALUES(0) UNION ALL SELECT value+1 FROM sequence WHERE value<255) INSERT INTO tracks(source_key,object_id,media_uri,title,normalized_search,display_album,display_artist,sort_text,duration_millis,disc_number,track_number,source_favorite) SELECT ?1,'track-'||value,'file:///track-'||value,'Track','track','', 'Artist','track-'||printf('%03d',value),1000,0,value,0 FROM sequence")
            .bind(source).execute(&mut connection).await.expect("insert Track window");
        sqlx::query("INSERT INTO track_artists(track_key,artist_key,position) SELECT track_key,?1,0 FROM tracks WHERE source_key=?2")
            .bind(artist).bind(source).execute(&mut connection).await.expect("insert Track Artists");
        sqlx::query("INSERT INTO track_genres(track_key,genre_key,position) SELECT track_key,?1,0 FROM tracks WHERE source_key=?2")
            .bind(genre).bind(source).execute(&mut connection).await.expect("insert Track Genres");
        let mut keys = sqlx::query_scalar::<_, TrackKey>(
            "SELECT track_key FROM tracks WHERE source_key=?1 ORDER BY track_key",
        )
        .bind(source)
        .fetch_all(&mut connection)
        .await
        .expect("read Track window keys");
        keys.reverse();
        let one = load_track_rows(&mut connection, &keys[..1])
            .await
            .expect("load one Track row");
        let window = load_track_rows(&mut connection, &keys)
            .await
            .expect("load bounded Track row window");
        assert_eq!(one.len(), 1);
        assert_eq!(window.len(), 256);
        assert_eq!(one[0].track_key, keys[0]);
        for (row, key) in window.iter().zip(&keys) {
            assert_eq!(row.track_key, *key);
            assert_eq!(row.artists.len(), 1);
            assert_eq!(row.artists[0].artist_key, artist);
            assert_eq!(row.genres.len(), 1);
            assert_eq!(row.genres[0].genre_key, genre);
        }
    }

    #[tokio::test]
    async fn object_identity_order_pages_without_losing_a_large_folder() {
        let file = tempfile::NamedTempFile::new().expect("create Track identity Store");
        let mut connection = open_writer(file.path())
            .await
            .expect("open Track identity Store");
        schema::initialize_durable(&mut connection)
            .await
            .expect("initialize durable Store");
        let mut catalog = open_writer(&file.path().with_extension("catalog.sqlite"))
            .await
            .unwrap();
        schema::initialize_catalog(&mut catalog).await.unwrap();
        drop(catalog);
        schema::attach_catalog(
            &mut connection,
            &file.path().with_extension("catalog.sqlite"),
        )
        .await
        .unwrap();
        let source = sqlx::query_scalar::<_, SourceKey>(
            "INSERT INTO sources(object_id,display_name,normalized_name,artwork_digest) VALUES ('source','Source','source',zeroblob(32)) RETURNING source_key",
        )
        .fetch_one(&mut connection)
        .await
        .expect("insert source");
        sqlx::query("WITH RECURSIVE sequence(value) AS (VALUES(0) UNION ALL SELECT value+1 FROM sequence WHERE value<299) INSERT INTO tracks(source_key,object_id,title,normalized_search,display_album,display_artist,sort_text,duration_millis,disc_number,track_number,media_uri,source_favorite) SELECT ?1,'track-'||printf('%03d',value),'Track','track','','Artist','track-'||printf('%03d',value),1000,0,value,'file:///track-'||printf('%03d',value),0 FROM sequence")
            .bind(source).execute(&mut connection).await.expect("insert large Folder Track order");
        drop(connection);

        let database = Database::open(file.path())
            .await
            .expect("open Track identity Database");
        let object_ids = (0..300)
            .map(|index| format!("track-{index:03}"))
            .collect::<Vec<_>>();
        let media_uris = database
            .track_media_uris_by_objects(source, &object_ids, &ReadCancellation::new())
            .await
            .expect("map complete large Folder identity order");
        assert_eq!(media_uris.len(), object_ids.len());
        assert!(media_uris.windows(2).all(|pair| pair[0] < pair[1]));
        let mut reversed = media_uris.clone();
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
        assert_eq!(
            ordered,
            (0..300)
                .map(|index| format!("file:///track-{index:03}"))
                .collect::<Vec<_>>()
        );
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
