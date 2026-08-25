//! Owns Favorite and Rating overrides plus bounded remote Favorite delivery.
//! Source clients perform network delivery and retry scheduling decisions.

use sqlx::sqlite::SqliteConnection;
use sqlx::{Connection, Row, Sqlite, Transaction};

use crate::{AlbumKey, ArtistKey, Database, LibraryError, LibraryResult, SourceKey, TrackKey};

const FAVORITE_DELIVERY_LIMIT: usize = 500;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FavoriteTarget {
    Track(TrackKey),
    Album(AlbumKey),
    Artist(ArtistKey),
}

impl FavoriteTarget {
    const fn kind(self) -> &'static str {
        match self {
            Self::Track(_) => "track",
            Self::Album(_) => "album",
            Self::Artist(_) => "artist",
        }
    }

    const fn raw(self) -> i64 {
        match self {
            Self::Track(key) => key.raw(),
            Self::Album(key) => key.raw(),
            Self::Artist(key) => key.raw(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PendingFavorite {
    pub target: FavoriteTarget,
    pub favorite: bool,
    pub attempts: i64,
    pub next_attempt_at: i64,
}

impl Database {
    pub async fn set_track_rating(
        &self,
        source: SourceKey,
        track: TrackKey,
        rating: Option<u8>,
    ) -> LibraryResult<bool> {
        self.set_rating(source, "tracks", "track_key", track.raw(), rating)
            .await
    }

    pub async fn set_album_rating(
        &self,
        source: SourceKey,
        album: AlbumKey,
        rating: Option<u8>,
    ) -> LibraryResult<bool> {
        self.set_rating(source, "albums", "album_key", album.raw(), rating)
            .await
    }

    pub async fn set_artist_rating(
        &self,
        source: SourceKey,
        artist: ArtistKey,
        rating: Option<u8>,
    ) -> LibraryResult<bool> {
        self.set_rating(source, "artists", "artist_key", artist.raw(), rating)
            .await
    }

    async fn set_rating(
        &self,
        source: SourceKey,
        table: &'static str,
        key_column: &'static str,
        key: i64,
        rating: Option<u8>,
    ) -> LibraryResult<bool> {
        if rating.is_some_and(|rating| rating > 10) {
            return Err(LibraryError::InvalidRequest(
                "Ratings must be between 0 and 10".to_string(),
            ));
        }
        let stored = rating.map(|rating| i64::from(rating) * 10);
        let sql = match (table, key_column) {
            ("tracks", "track_key") => {
                "UPDATE tracks SET user_rating=?3 WHERE source_key=?1 AND track_key=?2"
            }
            ("albums", "album_key") => {
                "UPDATE albums SET user_rating=?3 WHERE source_key=?1 AND album_key=?2"
            }
            ("artists", "artist_key") => {
                "UPDATE artists SET user_rating=?3 WHERE source_key=?1 AND artist_key=?2"
            }
            _ => unreachable!("fixed Rating owner"),
        };
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(sqlx::query(sql)
            .bind(source)
            .bind(key)
            .bind(stored)
            .execute(connection)
            .await?
            .rows_affected()
            == 1)
    }

    pub async fn set_track_favorite(
        &self,
        source: SourceKey,
        track: TrackKey,
        favorite: bool,
    ) -> LibraryResult<bool> {
        self.set_local_favorite(source, FavoriteTarget::Track(track), favorite)
            .await
    }

    pub async fn set_album_favorite(
        &self,
        source: SourceKey,
        album: AlbumKey,
        favorite: bool,
    ) -> LibraryResult<bool> {
        self.set_local_favorite(source, FavoriteTarget::Album(album), favorite)
            .await
    }

    pub async fn set_artist_favorite(
        &self,
        source: SourceKey,
        artist: ArtistKey,
        favorite: bool,
    ) -> LibraryResult<bool> {
        self.set_local_favorite(source, FavoriteTarget::Artist(artist), favorite)
            .await
    }

    async fn set_local_favorite(
        &self,
        source: SourceKey,
        target: FavoriteTarget,
        favorite: bool,
    ) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        update_favorite(connection, source, target, Some(favorite)).await
    }

    pub async fn queue_remote_favorite(
        &self,
        source: SourceKey,
        target: FavoriteTarget,
        favorite: bool,
        next_attempt_at: i64,
    ) -> LibraryResult<bool> {
        if next_attempt_at < 0 {
            return Err(LibraryError::InvalidRequest(
                "Favorite delivery time cannot be negative".to_string(),
            ));
        }
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let Some(previous) = effective_favorite(&mut transaction, source, target).await? else {
            transaction.rollback().await?;
            return Ok(false);
        };
        sqlx::query(
            "INSERT INTO favorite_outbox(
                 source_key, entity_kind, entity_key, favorite,
                 previous_favorite, attempts, next_attempt_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6)
             ON CONFLICT(source_key, entity_kind, entity_key) DO UPDATE SET
                 favorite=excluded.favorite,
                 previous_favorite=favorite_outbox.previous_favorite,
                 attempts=0, next_attempt_at=excluded.next_attempt_at",
        )
        .bind(source)
        .bind(target.kind())
        .bind(target.raw())
        .bind(favorite)
        .bind(previous)
        .bind(next_attempt_at)
        .execute(&mut *transaction)
        .await?;
        update_favorite(&mut transaction, source, target, Some(favorite)).await?;
        transaction.commit().await?;
        Ok(true)
    }

    pub async fn due_remote_favorites(
        &self,
        source: SourceKey,
        now: i64,
        limit: usize,
    ) -> LibraryResult<Vec<PendingFavorite>> {
        if now < 0 || limit == 0 {
            return Ok(Vec::new());
        }
        let limit = limit.min(FAVORITE_DELIVERY_LIMIT) as i64;
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let rows = sqlx::query(
            "SELECT entity_kind, entity_key, favorite, attempts, next_attempt_at
             FROM favorite_outbox
             WHERE source_key=?1 AND next_attempt_at<=?2
             ORDER BY next_attempt_at, outbox_key LIMIT ?3",
        )
        .bind(source)
        .bind(now)
        .bind(limit)
        .fetch_all(connection)
        .await?;
        rows.into_iter()
            .map(|row| {
                let kind = row.try_get::<String, _>(0)?;
                let key = row.try_get::<i64, _>(1)?;
                let target = match kind.as_str() {
                    "track" => FavoriteTarget::Track(TrackKey::from_raw(key)),
                    "album" => FavoriteTarget::Album(AlbumKey::from_raw(key)),
                    "artist" => FavoriteTarget::Artist(ArtistKey::from_raw(key)),
                    _ => {
                        return Err(LibraryError::InvalidStore(format!(
                            "unknown Favorite kind {kind}"
                        )));
                    }
                };
                Ok(PendingFavorite {
                    target,
                    favorite: row.try_get(2)?,
                    attempts: row.try_get(3)?,
                    next_attempt_at: row.try_get(4)?,
                })
            })
            .collect()
    }

    pub async fn complete_remote_favorite(
        &self,
        source: SourceKey,
        target: FavoriteTarget,
        favorite: bool,
    ) -> LibraryResult<bool> {
        self.delete_outbox(source, target, favorite).await
    }

    pub async fn acknowledge_remote_favorite(
        &self,
        source: SourceKey,
        target: FavoriteTarget,
        favorite: bool,
    ) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let current = sqlx::query_scalar::<_, i64>(
            "DELETE FROM favorite_outbox
             WHERE source_key=?1 AND entity_kind=?2 AND entity_key=?3 AND favorite=?4
             RETURNING outbox_key",
        )
        .bind(source)
        .bind(target.kind())
        .bind(target.raw())
        .bind(favorite)
        .fetch_optional(&mut *transaction)
        .await?;
        if current.is_none() {
            transaction.commit().await?;
            return Ok(false);
        }
        let sql = match target {
            FavoriteTarget::Track(_) => {
                "UPDATE tracks SET source_favorite=?3, user_favorite=NULL
                 WHERE source_key=?1 AND track_key=?2"
            }
            FavoriteTarget::Album(_) => {
                "UPDATE albums SET source_favorite=?3, user_favorite=NULL
                 WHERE source_key=?1 AND album_key=?2"
            }
            FavoriteTarget::Artist(_) => {
                "UPDATE artists SET source_favorite=?3, user_favorite=NULL
                 WHERE source_key=?1 AND artist_key=?2"
            }
        };
        let changed = sqlx::query(sql)
            .bind(source)
            .bind(target.raw())
            .bind(favorite)
            .execute(&mut *transaction)
            .await?
            .rows_affected()
            == 1;
        transaction.commit().await?;
        Ok(changed)
    }

    pub async fn defer_remote_favorite(
        &self,
        source: SourceKey,
        target: FavoriteTarget,
        favorite: bool,
        next_attempt_at: i64,
    ) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(sqlx::query(
            "UPDATE favorite_outbox
             SET attempts=attempts+1, next_attempt_at=?5
             WHERE source_key=?1 AND entity_kind=?2 AND entity_key=?3 AND favorite=?4",
        )
        .bind(source)
        .bind(target.kind())
        .bind(target.raw())
        .bind(favorite)
        .bind(next_attempt_at)
        .execute(connection)
        .await?
        .rows_affected()
            == 1)
    }

    pub async fn reject_remote_favorite(
        &self,
        source: SourceKey,
        target: FavoriteTarget,
        favorite: bool,
    ) -> LibraryResult<Option<bool>> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let previous = sqlx::query_scalar::<_, bool>(
            "DELETE FROM favorite_outbox
             WHERE source_key=?1 AND entity_kind=?2 AND entity_key=?3 AND favorite=?4
             RETURNING previous_favorite",
        )
        .bind(source)
        .bind(target.kind())
        .bind(target.raw())
        .bind(favorite)
        .fetch_optional(&mut *transaction)
        .await?;
        if let Some(previous) = previous {
            update_favorite(&mut transaction, source, target, Some(previous)).await?;
        }
        transaction.commit().await?;
        Ok(previous)
    }

    async fn delete_outbox(
        &self,
        source: SourceKey,
        target: FavoriteTarget,
        favorite: bool,
    ) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(sqlx::query(
            "DELETE FROM favorite_outbox
             WHERE source_key=?1 AND entity_kind=?2 AND entity_key=?3 AND favorite=?4",
        )
        .bind(source)
        .bind(target.kind())
        .bind(target.raw())
        .bind(favorite)
        .execute(connection)
        .await?
        .rows_affected()
            == 1)
    }
}

async fn effective_favorite(
    transaction: &mut Transaction<'_, Sqlite>,
    source: SourceKey,
    target: FavoriteTarget,
) -> LibraryResult<Option<bool>> {
    let sql = match target {
        FavoriteTarget::Track(_) => {
            "SELECT COALESCE(user_favorite, source_favorite)
             FROM tracks WHERE source_key=?1 AND track_key=?2"
        }
        FavoriteTarget::Album(_) => {
            "SELECT COALESCE(user_favorite, source_favorite)
             FROM albums WHERE source_key=?1 AND album_key=?2"
        }
        FavoriteTarget::Artist(_) => {
            "SELECT COALESCE(user_favorite, source_favorite)
             FROM artists WHERE source_key=?1 AND artist_key=?2"
        }
    };
    Ok(sqlx::query_scalar(sql)
        .bind(source)
        .bind(target.raw())
        .fetch_optional(&mut **transaction)
        .await?)
}

async fn update_favorite(
    connection: &mut SqliteConnection,
    source: SourceKey,
    target: FavoriteTarget,
    favorite: Option<bool>,
) -> LibraryResult<bool> {
    let sql = match target {
        FavoriteTarget::Track(_) => {
            "UPDATE tracks SET user_favorite=?3 WHERE source_key=?1 AND track_key=?2"
        }
        FavoriteTarget::Album(_) => {
            "UPDATE albums SET user_favorite=?3 WHERE source_key=?1 AND album_key=?2"
        }
        FavoriteTarget::Artist(_) => {
            "UPDATE artists SET user_favorite=?3 WHERE source_key=?1 AND artist_key=?2"
        }
    };
    Ok(sqlx::query(sql)
        .bind(source)
        .bind(target.raw())
        .bind(favorite)
        .execute(connection)
        .await?
        .rows_affected()
        == 1)
}
