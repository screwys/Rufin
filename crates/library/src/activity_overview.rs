//! Calendar reports over the existing accepted-listen history.

use crate::{
    AlbumView, ArtistView, CalendarActivityPeriod, Database, LibraryResult, ReadCancellation,
    TrackView,
};

// Released versions also stored monthly counts. Keep those in the same report;
// lifetime provider counts cannot be assigned to a calendar month.
const PERIOD_ACTIVITY: &str = "WITH period_activity AS (
    SELECT media_uri,count(*) plays,sum(duration_millis) duration_millis,artist_name
    FROM listens WHERE skipped=0 AND local_period>=?1 AND local_period<?2
    GROUP BY media_uri,artist_name
    UNION ALL
    SELECT track.media_uri,baseline.play_count,track.duration_millis*baseline.play_count,track.display_artist
    FROM legacy_activity baseline JOIN sources source ON source.object_id=baseline.source_id
    JOIN tracks track ON track.source_key=source.source_key AND track.object_id=baseline.track_object_id
    WHERE baseline.item_kind='track' AND baseline.period>=?1 AND baseline.period<?2
      AND baseline.play_count>0
) ";

#[derive(Clone, Debug, Default, PartialEq, sqlx::FromRow)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ActivityTotals {
    pub plays: i64,
    pub duration_millis: i64,
    pub tracks: i64,
    pub artists: i64,
}

#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ActivityOverview {
    pub totals: ActivityTotals,
    pub previous: ActivityTotals,
    pub tracks: Vec<TrackView>,
    pub albums: Vec<AlbumView>,
    pub artists: Vec<ArtistView>,
    pub genres: Vec<GenreActivity>,
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
            "{PERIOD_ACTIVITY} SELECT track.track_key,sum(plays) plays FROM period_activity JOIN tracks track USING(media_uri)
             GROUP BY track.track_key ORDER BY plays DESC,track.media_uri LIMIT ?3"
        );
        let tracks =
            sqlx::query_as::<_, (crate::TrackKey, i64)>(sqlx::AssertSqlSafe(track_sql.as_str()))
                .bind(&from)
                .bind(&until)
                .bind(limit as i64)
                .fetch_all(&mut *connection)
                .await?;
        let album_sql = format!(
            "{PERIOD_ACTIVITY} SELECT album.album_key,sum(plays) plays FROM period_activity listen
             JOIN tracks track USING(media_uri) JOIN albums album USING(album_key)
             GROUP BY album.album_key ORDER BY plays DESC,album.sort_text,album.album_key LIMIT ?3"
        );
        let albums =
            sqlx::query_as::<_, (crate::AlbumKey, i64)>(sqlx::AssertSqlSafe(album_sql.as_str()))
                .bind(&from)
                .bind(&until)
                .bind(limit as i64)
                .fetch_all(&mut *connection)
                .await?;
        let artist_sql = format!("{PERIOD_ACTIVITY} SELECT artist.artist_key,sum(plays) plays FROM period_activity listen
             JOIN tracks track USING(media_uri) JOIN track_artists credit USING(track_key)
             JOIN artists artist USING(artist_key)
             GROUP BY artist.artist_key ORDER BY plays DESC,artist.sort_text,artist.artist_key LIMIT ?3");
        let artists =
            sqlx::query_as::<_, (crate::ArtistKey, i64)>(sqlx::AssertSqlSafe(artist_sql.as_str()))
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
        let genres = sqlx::query_as::<_, GenreActivity>(sqlx::AssertSqlSafe(genre_sql.as_str()))
            .bind(&from)
            .bind(&until)
            .bind(limit as i64)
            .fetch_all(&mut *connection)
            .await?;
        let keys = tracks.iter().map(|(key, _)| *key).collect::<Vec<_>>();
        let mut track_rows = crate::tracks::load_track_rows(&mut connection, &keys).await?;
        crate::activity::overlay_history(&mut connection, &mut track_rows, None).await?;
        for (row, (_, count)) in track_rows.iter_mut().zip(tracks) {
            row.state.play_count = count;
        }
        let keys = albums.iter().map(|(key, _)| *key).collect::<Vec<_>>();
        let mut album_rows =
            crate::collections::load_album_rows(&mut connection, &keys, false).await?;
        for (row, (_, count)) in album_rows.iter_mut().zip(albums) {
            row.state.play_count = count;
        }
        let keys = artists.iter().map(|(key, _)| *key).collect::<Vec<_>>();
        let mut artist_rows =
            crate::collections::load_artist_rows(&mut connection, &keys, false, false).await?;
        for (row, (_, count)) in artist_rows.iter_mut().zip(artists) {
            row.state.play_count = count;
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
            UNION SELECT period FROM legacy_activity WHERE length(period)=7 AND item_kind='track' AND play_count>0
            ORDER BY 1 DESC")
            .fetch_all(&mut *connection).await?)
    }
}

#[derive(Clone, Debug, sqlx::FromRow)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct GenreActivity {
    pub name: String,
    pub plays: i64,
}
