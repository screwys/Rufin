//! Owns Favorite and Rating overrides plus bounded remote Favorite delivery.
//! Source clients perform network delivery and retry scheduling decisions.

use sqlx::{Connection, Sqlite, Transaction, sqlite::SqliteConnection};

use crate::{Database, LibraryError, LibraryResult, ReadCancellation};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum FavoriteTarget {
    Track(String),
    Album(String),
    Artist(String),
}

impl FavoriteTarget {
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Track(_) => "track",
            Self::Album(_) => "album",
            Self::Artist(_) => "artist",
        }
    }

    pub fn media_uri(&self) -> &str {
        match self {
            Self::Track(uri) | Self::Album(uri) | Self::Artist(uri) => uri,
        }
    }
}

impl Database {
    pub async fn user_media_state(
        &self,
        media_uri: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<(Option<bool>, Option<u8>)>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_as::<_, (Option<bool>, Option<i64>)>(
            "SELECT favorite,rating/10 FROM user_media_state WHERE media_uri=?1",
        )
        .bind(media_uri)
        .fetch_optional(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        result?
            .map(|(favorite, rating)| {
                Ok((
                    favorite,
                    rating.map(u8::try_from).transpose().map_err(|_| {
                        LibraryError::InvalidStore("invalid user Rating".to_string())
                    })?,
                ))
            })
            .transpose()
    }

    pub async fn set_rating(
        &self,
        target: &FavoriteTarget,
        rating: Option<u8>,
    ) -> LibraryResult<bool> {
        if rating.is_some_and(|rating| rating > 10) {
            return Err(LibraryError::InvalidRequest(
                "Ratings must be between 0 and 10".to_string(),
            ));
        }
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        sqlx::query(
            "INSERT INTO user_media_state(media_uri,rating) VALUES (?1,?2)
             ON CONFLICT(media_uri) DO UPDATE SET rating=excluded.rating",
        )
        .bind(target.media_uri())
        .bind(rating.map(|rating| i64::from(rating) * 10))
        .execute(&mut *connection)
        .await?;
        cleanup_empty_state(connection, target.media_uri()).await?;
        Ok(true)
    }

    pub async fn set_favorite(
        &self,
        target: &FavoriteTarget,
        favorite: bool,
    ) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        update_favorite(connection, target, Some(favorite)).await
    }

    pub async fn queue_remote_favorite(
        &self,
        target: &FavoriteTarget,
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
        let previous = effective_favorite(&mut transaction, target).await?;
        sqlx::query(
            "INSERT INTO favorite_outbox(media_uri,favorite,previous_favorite,attempts,next_attempt_at)
             VALUES (?1,?2,?3,0,?4)
             ON CONFLICT(media_uri) DO UPDATE SET
                 favorite=excluded.favorite,
                 previous_favorite=favorite_outbox.previous_favorite,
                 attempts=0,next_attempt_at=excluded.next_attempt_at",
        )
        .bind(target.media_uri())
        .bind(favorite)
        .bind(previous)
        .bind(next_attempt_at)
        .execute(&mut *transaction)
        .await?;
        update_favorite(&mut transaction, target, Some(favorite)).await?;
        transaction.commit().await?;
        Ok(true)
    }

    pub async fn acknowledge_remote_favorite(
        &self,
        target: &FavoriteTarget,
        favorite: bool,
    ) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let current = sqlx::query_scalar::<_, i64>(
            "DELETE FROM favorite_outbox WHERE media_uri=?1 AND favorite=?2 RETURNING outbox_key",
        )
        .bind(target.media_uri())
        .bind(favorite)
        .fetch_optional(&mut *transaction)
        .await?;
        if current.is_none() {
            transaction.commit().await?;
            return Ok(false);
        }
        let sql = match target {
            FavoriteTarget::Track(_) => "UPDATE tracks SET source_favorite=?2 WHERE media_uri=?1",
            FavoriteTarget::Album(_) => "UPDATE albums SET source_favorite=?2 WHERE media_uri=?1",
            FavoriteTarget::Artist(_) => "UPDATE artists SET source_favorite=?2 WHERE media_uri=?1",
        };
        let changed = sqlx::query(sql)
            .bind(target.media_uri())
            .bind(favorite)
            .execute(&mut *transaction)
            .await?
            .rows_affected()
            == 1;
        if changed {
            update_favorite(&mut transaction, target, None).await?;
        }
        transaction.commit().await?;
        Ok(changed)
    }

    pub async fn defer_remote_favorite(
        &self,
        target: &FavoriteTarget,
        favorite: bool,
        next_attempt_at: i64,
    ) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(sqlx::query(
            "UPDATE favorite_outbox SET attempts=attempts+1,next_attempt_at=?3
             WHERE media_uri=?1 AND favorite=?2",
        )
        .bind(target.media_uri())
        .bind(favorite)
        .bind(next_attempt_at)
        .execute(connection)
        .await?
        .rows_affected()
            == 1)
    }

    pub async fn reject_remote_favorite(
        &self,
        target: &FavoriteTarget,
        favorite: bool,
    ) -> LibraryResult<Option<bool>> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let previous = sqlx::query_scalar::<_, bool>(
            "DELETE FROM favorite_outbox WHERE media_uri=?1 AND favorite=?2
             RETURNING previous_favorite",
        )
        .bind(target.media_uri())
        .bind(favorite)
        .fetch_optional(&mut *transaction)
        .await?;
        if let Some(previous) = previous {
            update_favorite(&mut transaction, target, Some(previous)).await?;
        }
        transaction.commit().await?;
        Ok(previous)
    }
}

async fn effective_favorite(
    transaction: &mut Transaction<'_, Sqlite>,
    target: &FavoriteTarget,
) -> LibraryResult<bool> {
    let sql = match target {
        FavoriteTarget::Track(_) => {
            "SELECT COALESCE(
                 (SELECT favorite FROM user_media_state WHERE media_uri=?1),
                 (SELECT source_favorite FROM tracks WHERE media_uri=?1),
                 0
             )"
        }
        FavoriteTarget::Album(_) => {
            "SELECT COALESCE(
                 (SELECT favorite FROM user_media_state WHERE media_uri=?1),
                 (SELECT source_favorite FROM albums WHERE media_uri=?1),
                 0
             )"
        }
        FavoriteTarget::Artist(_) => {
            "SELECT COALESCE(
                 (SELECT favorite FROM user_media_state WHERE media_uri=?1),
                 (SELECT source_favorite FROM artists WHERE media_uri=?1),
                 0
             )"
        }
    };
    Ok(sqlx::query_scalar(sql)
        .bind(target.media_uri())
        .fetch_one(&mut **transaction)
        .await?)
}

async fn update_favorite(
    connection: &mut SqliteConnection,
    target: &FavoriteTarget,
    favorite: Option<bool>,
) -> LibraryResult<bool> {
    sqlx::query(
        "INSERT INTO user_media_state(media_uri,favorite) VALUES (?1,?2)
         ON CONFLICT(media_uri) DO UPDATE SET favorite=excluded.favorite",
    )
    .bind(target.media_uri())
    .bind(favorite)
    .execute(&mut *connection)
    .await?;
    cleanup_empty_state(connection, target.media_uri()).await?;
    Ok(true)
}

async fn cleanup_empty_state(
    connection: &mut SqliteConnection,
    media_uri: &str,
) -> LibraryResult<()> {
    sqlx::query(
        "DELETE FROM user_media_state WHERE media_uri=?1 AND favorite IS NULL AND rating IS NULL",
    )
    .bind(media_uri)
    .execute(connection)
    .await?;
    Ok(())
}

#[derive(Debug, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct UserMediaStateWrite {
    pub media_uri: String,
    pub favorite: Option<bool>,
    /// Rufin precision, independent of a provider's coarser rating scale.
    pub rating: Option<i64>,
}

impl Database {
    pub async fn import_user_media_state_jsonl(
        &self,
        input: impl std::io::BufRead,
    ) -> LibraryResult<u64> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let count = import_user_media_state_jsonl_on(&mut transaction, input).await?;
        transaction.commit().await?;
        Ok(count)
    }
}

pub(crate) async fn export_user_media_state_jsonl_on(
    connection: &mut SqliteConnection,
    mut output: impl std::io::Write,
) -> LibraryResult<u64> {
    use futures_util::TryStreamExt;
    output.write_all(b"{\"version\":1}\n")?;
    let mut rows = sqlx::query_as::<_, UserMediaStateWrite>(
        "SELECT media_uri,favorite,rating FROM user_media_state ORDER BY media_uri",
    )
    .fetch(connection);
    let mut count = 0;
    while let Some(row) = rows.try_next().await? {
        serde_json::to_writer(&mut output, &row)?;
        output.write_all(b"\n")?;
        count += 1;
    }
    Ok(count)
}
pub(crate) async fn import_user_media_state_jsonl_on(
    connection: &mut SqliteConnection,
    input: impl std::io::BufRead,
) -> LibraryResult<u64> {
    let mut lines = input.lines();
    let header: serde_json::Value =
        serde_json::from_str(&lines.next().transpose()?.unwrap_or_default())?;
    if header.get("version").and_then(serde_json::Value::as_u64) != Some(1) {
        return Err(LibraryError::InvalidRequest(
            "unsupported user media state version".into(),
        ));
    }
    let mut count = 0;
    for line in lines {
        let state: UserMediaStateWrite = serde_json::from_str(&line?)?;
        write_user_media_state(connection, &state).await?;
        count += 1;
    }
    Ok(count)
}

pub(crate) async fn write_user_media_state(
    connection: &mut SqliteConnection,
    state: &UserMediaStateWrite,
) -> LibraryResult<()> {
    if state.media_uri.is_empty()
        || state
            .rating
            .is_some_and(|rating| !(0..=100).contains(&rating))
    {
        return Err(LibraryError::InvalidRequest(
            "invalid user media state".into(),
        ));
    }
    sqlx::query(
        "INSERT INTO user_media_state(media_uri,favorite,rating) VALUES(?1,?2,?3)
        ON CONFLICT(media_uri) DO UPDATE SET favorite=excluded.favorite,rating=excluded.rating",
    )
    .bind(&state.media_uri)
    .bind(state.favorite)
    .bind(state.rating)
    .execute(&mut *connection)
    .await?;
    cleanup_empty_state(connection, &state.media_uri).await
}
