//! Owns bounded provider and Rufin-defined Home sections.
//! Random-looking sections use indexed pivots rather than catalog-sized Rust state.

use sqlx::{Connection, FromRow};

use crate::{
    AlbumKey, Database, FolderKey, GenreKey, LibraryResult, ReadCancellation, SourceKey, TrackKey,
};

const HOME_LIMIT: usize = 24;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HomeEntryKind {
    Track,
    Album,
    Artist,
    Playlist,
}

impl HomeEntryKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Track => "track",
            Self::Album => "album",
            Self::Artist => "artist",
            Self::Playlist => "playlist",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct HomeEntryInput {
    pub section_id: String,
    pub position: i64,
    pub kind: HomeEntryKind,
    pub entity_object_id: String,
    pub title: String,
    pub subtitle: String,
    pub artwork_binding: Option<Vec<u8>>,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct HomeTrackRow {
    pub track_key: TrackKey,
    pub title: String,
    pub display_artist: String,
    pub display_album: String,
    pub artwork_binding: Option<Vec<u8>>,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct HomeAlbumRow {
    pub album_key: AlbumKey,
    pub title: String,
    pub display_artist: String,
    pub artwork_binding: Option<Vec<u8>>,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct HomeGenreRow {
    pub genre_key: GenreKey,
    pub name: String,
    pub artwork_binding: Option<Vec<u8>>,
}

impl Database {
    pub async fn home_explore_tracks(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        variation: i64,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<HomeTrackRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let pivot = sqlx::query_scalar::<_, i64>("SELECT COALESCE(max(track.track_key),0) FROM tracks track WHERE track.source_key=?1 AND (?2 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?2))")
            .bind(source).bind(folder).fetch_one(&mut *transaction).await?;
        let pivot = if pivot == 0 {
            0
        } else {
            variation.rem_euclid(pivot + 1)
        };
        let mut result = sqlx::query_as::<_, HomeTrackRow>(
            "SELECT track_key, title, display_artist, display_album, artwork_binding
             FROM tracks WHERE source_key=?1 AND track_key>=?2 AND (?3 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=tracks.track_key AND scope.folder_key=?3))
             ORDER BY track_key LIMIT 24",
        )
        .bind(source)
        .bind(pivot).bind(folder).fetch_all(&mut *transaction).await?;
        if result.len() < HOME_LIMIT {
            let rest = sqlx::query_as::<_, HomeTrackRow>("SELECT track_key,title,display_artist,display_album,artwork_binding FROM tracks WHERE source_key=?1 AND track_key<?2 AND (?4 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=tracks.track_key AND scope.folder_key=?4)) ORDER BY track_key LIMIT ?3")
                .bind(source).bind(pivot).bind((HOME_LIMIT-result.len()) as i64).bind(folder).fetch_all(&mut *transaction).await?;
            result.extend(rest);
        }
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(result)
    }

    pub async fn home_showcase_album(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        variation: i64,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<HomeAlbumRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let pivot = sqlx::query_scalar::<_, i64>("SELECT COALESCE(max(album.album_key),0) FROM albums album WHERE album.source_key=?1 AND (?2 IS NULL OR EXISTS (SELECT 1 FROM tracks track JOIN track_folders scope USING(track_key) WHERE track.album_key=album.album_key AND scope.folder_key=?2))").bind(source).bind(folder).fetch_one(&mut *connection).await?;
        let pivot = if pivot == 0 {
            0
        } else {
            variation.rem_euclid(pivot + 1)
        };
        let result = sqlx::query_as::<_, HomeAlbumRow>(
            "SELECT album_key, title, display_artist, artwork_binding
             FROM albums WHERE source_key=?1 AND album_key>=?2 AND (?3 IS NULL OR EXISTS (SELECT 1 FROM tracks track JOIN track_folders scope USING(track_key) WHERE track.album_key=albums.album_key AND scope.folder_key=?3))
             ORDER BY album_key LIMIT 1",
        )
        .bind(source)
        .bind(pivot)
        .bind(folder)
        .fetch_optional(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn provider_home_tracks(
        &self,
        source: SourceKey,
        section_id: &str,
        folder: Option<FolderKey>,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<HomeTrackRow>> {
        let limit = limit.clamp(1, HOME_LIMIT) as i64;
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_as::<_, HomeTrackRow>(
            "SELECT track.track_key, entry.title, track.display_artist,
                    track.display_album,
                    COALESCE(entry.artwork_binding, track.artwork_binding) AS artwork_binding
             FROM home_entries AS entry
             JOIN tracks AS track ON track.track_key=entry.entity_key
             WHERE entry.source_key=?1 AND entry.section_id=?2
               AND entry.entity_kind='track'
               AND (?4 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?4))
             ORDER BY entry.position LIMIT ?3",
        )
        .bind(source)
        .bind(section_id)
        .bind(limit)
        .bind(folder)
        .fetch_all(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn provider_home_albums(
        &self,
        source: SourceKey,
        section_id: &str,
        folder: Option<FolderKey>,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<HomeAlbumRow>> {
        let limit = limit.clamp(1, HOME_LIMIT) as i64;
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_as::<_, HomeAlbumRow>(
            "SELECT album.album_key, entry.title, album.display_artist,
                    COALESCE(entry.artwork_binding, album.artwork_binding) AS artwork_binding
             FROM home_entries AS entry
             JOIN albums AS album ON album.album_key=entry.entity_key
             WHERE entry.source_key=?1 AND entry.section_id=?2
               AND entry.entity_kind='album'
               AND (?4 IS NULL OR EXISTS (SELECT 1 FROM tracks track JOIN track_folders scope USING(track_key) WHERE track.album_key=album.album_key AND scope.folder_key=?4))
             ORDER BY entry.position LIMIT ?3",
        )
        .bind(source)
        .bind(section_id)
        .bind(limit)
        .bind(folder)
        .fetch_all(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn home_most_played_tracks(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<HomeTrackRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_as::<_, HomeTrackRow>(
            "WITH listen_count AS (
                 SELECT track_key, count(*) AS plays FROM listens
                 WHERE source_key=?1 AND track_key IS NOT NULL GROUP BY track_key
             )
             SELECT track.track_key, track.title, track.display_artist,
                    track.display_album, track.artwork_binding
             FROM tracks AS track
             LEFT JOIN activity_baseline AS baseline
               ON baseline.source_key=track.source_key
              AND baseline.track_object_id=track.object_id
             LEFT JOIN listen_count AS listen USING(track_key)
             WHERE track.source_key=?1
               AND COALESCE(baseline.play_count, 0) + COALESCE(listen.plays, 0) > 0
               AND (?2 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?2))
             ORDER BY COALESCE(baseline.play_count, 0) + COALESCE(listen.plays, 0) DESC,
                      track.sort_text, track.track_key LIMIT 24",
        )
        .bind(source)
        .bind(folder)
        .fetch_all(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn home_recently_played_tracks(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<HomeTrackRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_as::<_, HomeTrackRow>(
            "WITH latest AS (
                 SELECT track_key, max(started_at) AS played_at
                 FROM listens
                 WHERE source_key=?1 AND track_key IS NOT NULL GROUP BY track_key
             ) SELECT track.track_key, track.title, track.display_artist,
                      track.display_album, track.artwork_binding
               FROM latest JOIN tracks AS track USING(track_key)
               WHERE (?2 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?2))
               ORDER BY latest.played_at DESC, track.track_key LIMIT 24",
        )
        .bind(source)
        .bind(folder)
        .fetch_all(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn home_newly_added_albums(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<HomeAlbumRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_as::<_, HomeAlbumRow>(
            "SELECT album_key, title, display_artist, artwork_binding
             FROM albums WHERE source_key=?1 AND (?2 IS NULL OR EXISTS (SELECT 1 FROM tracks track JOIN track_folders scope USING(track_key) WHERE track.album_key=albums.album_key AND scope.folder_key=?2))
             ORDER BY COALESCE(unixepoch(date_added), first_seen_at) DESC NULLS LAST,
                      sort_text, album_key LIMIT 24",
        )
        .bind(source)
        .bind(folder)
        .fetch_all(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn home_recently_released_albums(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<HomeAlbumRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_as::<_, HomeAlbumRow>(
            "SELECT album_key, title, display_artist, artwork_binding
             FROM albums WHERE source_key=?1
               AND (release_date IS NOT NULL OR COALESCE(year, 0) <> 0)
               AND (?2 IS NULL OR EXISTS (SELECT 1 FROM tracks track JOIN track_folders scope USING(track_key) WHERE track.album_key=albums.album_key AND scope.folder_key=?2))
             ORDER BY release_date DESC NULLS LAST, year DESC NULLS LAST,
                      sort_text, album_key LIMIT 24",
        )
        .bind(source)
        .bind(folder)
        .fetch_all(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn home_featured_genres(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<HomeGenreRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_as::<_, HomeGenreRow>(
            "SELECT genre.genre_key, genre.name, genre.artwork_binding
             FROM genres AS genre
             LEFT JOIN track_genres AS relation USING(genre_key)
             WHERE genre.source_key=?1 AND (?2 IS NULL OR EXISTS (SELECT 1 FROM track_genres credit JOIN track_folders scope USING(track_key) WHERE credit.genre_key=genre.genre_key AND scope.folder_key=?2))
             GROUP BY genre.genre_key
             ORDER BY count(relation.track_key) DESC, genre.sort_text, genre.genre_key
             LIMIT 12",
        )
        .bind(source)
        .bind(folder)
        .fetch_all(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }
}
