//! Calendar reports over the existing accepted-listen history.

use crate::{
    AlbumRow, ArtistRow, CalendarActivityPeriod, Database, HistoryRow, LibraryResult,
    ReadCancellation,
};

// Released versions also stored monthly counts. Keep those in the same report;
// lifetime provider counts cannot be assigned to a calendar month.
const PERIOD_ACTIVITY: &str = "WITH period_activity AS (
    SELECT media_uri,count(*) plays,sum(duration_millis) duration_millis,artist_name
    FROM listens WHERE skipped=0 AND local_period>=?1 AND local_period<?2
    GROUP BY media_uri,artist_name
    UNION ALL
    SELECT track.media_uri,baseline.play_count,track.duration_millis*baseline.play_count,track.display_artist
    FROM activity_baseline baseline JOIN tracks track
      ON track.source_key=baseline.source_key AND track.object_id=baseline.track_object_id
    WHERE baseline.item_kind='track' AND baseline.period>=?1 AND baseline.period<?2
      AND baseline.play_count>0
) ";

#[derive(Clone, Debug, Default, PartialEq, sqlx::FromRow)]
pub struct ActivityTotals {
    pub plays: i64,
    pub duration_millis: i64,
    pub tracks: i64,
    pub artists: i64,
}

#[derive(Clone, Debug, Default)]
pub struct ActivityOverview {
    pub totals: ActivityTotals,
    pub previous: ActivityTotals,
    pub tracks: Vec<HistoryRow>,
    pub albums: Vec<AlbumRow>,
    pub artists: Vec<ArtistRow>,
    pub genres: Vec<(String, i64)>,
}

impl CalendarActivityPeriod {
    pub fn previous(self) -> Self {
        match self {
            Self::Month { year, month: 1 } => Self::Month {
                year: year - 1,
                month: 12,
            },
            Self::Month { year, month } => Self::Month {
                year,
                month: month - 1,
            },
            Self::Year(year) => Self::Year(year - 1),
            Self::Lifetime => Self::Lifetime,
        }
    }

    fn range(self) -> (String, String) {
        match self {
            Self::Month { year, month } => {
                let (next_year, next_month) = if month == 12 {
                    (year + 1, 1)
                } else {
                    (year, month + 1)
                };
                (
                    format!("{year:04}-{month:02}"),
                    format!("{next_year:04}-{next_month:02}"),
                )
            }
            Self::Year(year) => (format!("{year:04}-01"), format!("{:04}-01", year + 1)),
            Self::Lifetime => (String::new(), "9999-99".into()),
        }
    }
}

impl Database {
    pub async fn activity_overview(
        &self,
        period: CalendarActivityPeriod,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<ActivityOverview> {
        let (from, until) = period.range();
        let (previous_from, previous_until) = period.previous().range();
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let totals_sql = format!("{PERIOD_ACTIVITY} SELECT COALESCE(sum(plays),0) plays,COALESCE(sum(duration_millis),0) duration_millis,
            count(DISTINCT media_uri) tracks,count(DISTINCT NULLIF(artist_name,'')) artists
            FROM period_activity");
        let totals = sqlx::query_as::<_, ActivityTotals>(sqlx::AssertSqlSafe(totals_sql.as_str()))
            .bind(&from)
            .bind(&until)
            .fetch_one(&mut *connection)
            .await?;
        let previous =
            sqlx::query_as::<_, ActivityTotals>(sqlx::AssertSqlSafe(totals_sql.as_str()))
                .bind(previous_from)
                .bind(previous_until)
                .fetch_one(&mut *connection)
                .await?;
        let track_sql = format!(
            "{PERIOD_ACTIVITY} SELECT media_uri,sum(plays) plays FROM period_activity
             GROUP BY media_uri ORDER BY plays DESC,media_uri LIMIT ?3"
        );
        let tracks = sqlx::query_as::<_, (String, i64)>(sqlx::AssertSqlSafe(track_sql.as_str()))
            .bind(&from)
            .bind(&until)
            .bind(limit as i64)
            .fetch_all(&mut *connection)
            .await?;
        let album_sql = format!(
            "{PERIOD_ACTIVITY} SELECT album.media_uri,sum(plays) plays FROM period_activity listen
             JOIN tracks track USING(media_uri) JOIN albums album USING(album_key)
             GROUP BY album.album_key ORDER BY plays DESC,album.sort_text,album.album_key LIMIT ?3"
        );
        let albums = sqlx::query_as::<_, (String, i64)>(sqlx::AssertSqlSafe(album_sql.as_str()))
            .bind(&from)
            .bind(&until)
            .bind(limit as i64)
            .fetch_all(&mut *connection)
            .await?;
        let artist_sql = format!("{PERIOD_ACTIVITY} SELECT artist.media_uri,sum(plays) plays FROM period_activity listen
             JOIN tracks track USING(media_uri) JOIN track_artists credit USING(track_key)
             JOIN artists artist USING(artist_key)
             GROUP BY artist.artist_key ORDER BY plays DESC,artist.sort_text,artist.artist_key LIMIT ?3");
        let artists = sqlx::query_as::<_, (String, i64)>(sqlx::AssertSqlSafe(artist_sql.as_str()))
            .bind(&from)
            .bind(&until)
            .bind(limit as i64)
            .fetch_all(&mut *connection)
            .await?;
        let genre_sql = format!(
            "{PERIOD_ACTIVITY} SELECT genre.name,sum(plays) plays FROM period_activity listen
             JOIN tracks track USING(media_uri) JOIN track_genres credit USING(track_key)
             JOIN genres genre USING(genre_key)
             GROUP BY genre.name ORDER BY plays DESC,genre.name LIMIT ?3"
        );
        let genres = sqlx::query_as::<_, (String, i64)>(sqlx::AssertSqlSafe(genre_sql.as_str()))
            .bind(&from)
            .bind(&until)
            .bind(limit as i64)
            .fetch_all(&mut *connection)
            .await?;
        drop(connection);
        drop(_permit);
        let uris = tracks
            .iter()
            .map(|(uri, _)| uri.clone())
            .collect::<Vec<_>>();
        let mut history = self.history_rows_by_uri(&uris, cancellation).await?;
        let mut track_rows = Vec::new();
        for (uri, count) in tracks {
            let row = if let Some(index) = history.iter().position(|row| row.media_uri == uri) {
                Some(history.remove(index))
            } else {
                // A monthly baseline predates individual listen records.
                self.track_row_by_uri(&uri, cancellation)
                    .await?
                    .map(history_from_track)
            };
            if let Some(mut row) = row {
                row.play_count = count;
                track_rows.push(row);
            }
        }
        let mut album_rows = Vec::new();
        for (uri, count) in albums {
            if let Some(mut row) = self.album_row_by_media_uri(&uri, cancellation).await? {
                row.play_count = count;
                album_rows.push(row);
            }
        }
        let mut artist_rows = Vec::new();
        for (uri, count) in artists {
            if let Some(mut row) = self.artist_row_by_media_uri(&uri, cancellation).await? {
                row.play_count = count;
                artist_rows.push(row);
            }
        }
        Ok(ActivityOverview {
            totals,
            previous,
            tracks: track_rows,
            albums: album_rows,
            artists: artist_rows,
            genres,
        })
    }

    pub async fn activity_months(&self) -> LibraryResult<Vec<String>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar("SELECT DISTINCT local_period FROM listens WHERE skipped=0
            UNION SELECT period FROM activity_baseline WHERE length(period)=7 AND item_kind='track' AND play_count>0
            ORDER BY 1 DESC")
            .fetch_all(&mut *connection).await?)
    }
}

fn history_from_track(row: crate::TrackRow) -> HistoryRow {
    HistoryRow {
        media_uri: row.media_uri,
        title: row.title,
        artist: row.artist,
        album: row.album,
        album_media_uri: row.album_media_uri,
        artists: row.artists,
        album_artists: row.album_artists,
        album_display_artist: row.album_display_artist,
        artwork_binding: row.artwork_binding,
        duration_millis: row.duration_millis,
        disc_number: Some(row.disc_number),
        track_number: Some(row.track_number),
        year: row.year,
        release_date: row.release_date,
        date_added: row.date_added,
        bpm: row.bpm,
        genre: row
            .genres
            .iter()
            .map(|genre| genre.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        play_count: row.play_count,
        source_format: row.source_format,
        musicbrainz_recording_id: row.musicbrainz_recording_id,
        musicbrainz_release_track_id: row.musicbrainz_release_track_id,
        last_played: row.last_played,
        favorite: row.favorite,
        rating: row.rating,
        is_downloaded: row.is_downloaded,
    }
}
