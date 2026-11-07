//! The user's favorites and ratings, and the outbox that delivers them to servers. Source
//! clients send the deliveries and decide when to retry.

use sqlx::sqlite::SqliteConnection;

use crate::catalog::families::Row;
use crate::catalog::records::{Album, Artist, NativeState, Track};
use crate::catalog::write::{STATE_DELETED, STATE_RETURNING, StateRow, Write};

use crate::{Catalog, LibraryError, LibraryResult, ReadCancellation, SourceId};

/// Sparse source facts. An absent field leaves the stored value alone.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NativeUserState {
    pub favorite: Option<bool>,
    /// Outer None means absent; Some(None) clears the source rating (0..100).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "native_rating"
    )]
    pub rating: Option<Option<i64>>,
    pub play_count: Option<i64>,
    pub last_played: Option<i64>,
}

fn native_rating<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<i64>>, D::Error> {
    <Option<i64> as serde::Deserialize>::deserialize(deserializer).map(Some)
}

impl NativeUserState {
    pub fn merge(&mut self, incoming: Self) {
        self.favorite = incoming.favorite.or(self.favorite);
        self.rating = incoming.rating.or(self.rating);
        self.play_count = incoming.play_count.or(self.play_count);
        self.last_played = incoming.last_played.or(self.last_played);
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
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

/// What a delivery writes to the source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliveryKind {
    Favorite,
    Rating,
}

impl DeliveryKind {
    /// The outbox kind, which is also the user_state column it holds.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Favorite => "favorite",
            Self::Rating => "rating",
        }
    }
}

/// A favorite or rating waiting for its server. A favorite's value is 0 or 1, a rating's
/// 0..100, and a cleared rating has none.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Delivery {
    pub media_uri: String,
    pub kind: DeliveryKind,
    pub value: Option<i64>,
}

impl Catalog {
    pub async fn favorite(&self, target: &FavoriteTarget) -> LibraryResult<bool> {
        let mut connection = self.acquire_reader().await?;
        let sql = format!(
            "SELECT COALESCE((SELECT favorite FROM user_state WHERE media_uri=?1),(SELECT native_favorite FROM {}s WHERE media_uri=?1),0)",
            target.kind()
        );
        Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .bind(target.media_uri())
            .fetch_one(&mut *connection)
            .await?)
    }

    pub async fn user_state(
        &self,
        media_uri: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<(Option<bool>, Option<u8>)>> {
        let mut connection = self.acquire_cancellable(cancellation).await?;
        let result = sqlx::query_as::<_, (Option<bool>, Option<i64>)>(
            "SELECT favorite,rating/10 FROM user_state WHERE media_uri=?1",
        )
        .bind(media_uri)
        .fetch_optional(&mut *connection)
        .await;
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

    /// Sets the user's favorite. With `deliver`, the favorite also waits in the outbox until
    /// its server accepts it.
    pub async fn set_favorite(
        &self,
        target: &FavoriteTarget,
        favorite: bool,
        deliver: bool,
    ) -> LibraryResult<()> {
        self.set_user_value(
            target,
            DeliveryKind::Favorite,
            Some(i64::from(favorite)),
            deliver,
        )
        .await
    }

    /// Sets the user's rating out of 10. With `deliver`, the rating also waits in the outbox
    /// until its server accepts it.
    pub async fn set_rating(
        &self,
        target: &FavoriteTarget,
        rating: Option<u8>,
        deliver: bool,
    ) -> LibraryResult<()> {
        if rating.is_some_and(|rating| rating > 10) {
            return Err(LibraryError::InvalidRequest(
                "Ratings must be between 0 and 10".to_string(),
            ));
        }
        let value = rating.map(|rating| i64::from(rating) * 10);
        self.set_user_value(target, DeliveryKind::Rating, value, deliver)
            .await
    }

    async fn set_user_value(
        &self,
        target: &FavoriteTarget,
        kind: DeliveryKind,
        value: Option<i64>,
        deliver: bool,
    ) -> LibraryResult<()> {
        let uri = target.media_uri();
        let column = kind.as_str();
        let mut write = self.write().await?;
        let connection = write.connection().await?;
        let changed: Vec<StateRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "INSERT INTO user_state(media_uri,{column},changed_at) VALUES(?1,?2,unixepoch())
             ON CONFLICT(media_uri) DO UPDATE SET {column}=excluded.{column},changed_at=excluded.changed_at
             WHERE {column} IS NOT excluded.{column}{STATE_RETURNING}"
        )))
        .bind(uri)
        .bind(value)
        .fetch_all(&mut *connection)
        .await?;
        if deliver {
            sqlx::query(
                "INSERT INTO outbox(media_uri,kind,value,next_attempt_at) VALUES(?1,?2,?3,unixepoch())
                 ON CONFLICT(media_uri,kind) DO UPDATE SET value=excluded.value,next_attempt_at=excluded.next_attempt_at",
            )
            .bind(uri)
            .bind(kind.as_str())
            .bind(value)
            .execute(&mut *connection)
            .await?;
        }
        // A URI the Catalog holds no row for shows nothing; Rufin still holds its value.
        if deliver || !changed.is_empty() {
            write.held(vec![uri.to_owned()]);
        }
        // A cleared rating on its way to the server no longer shows the server's.
        if deliver && kind == DeliveryKind::Rating && value.is_none() {
            put_native(&mut write, uri, kind, None).await?;
        }
        write.set_states(changed);
        write.commit().await?;
        Ok(())
    }

    /// Deliveries due at `now`, oldest first.
    pub async fn due_deliveries(&self, now: i64, limit: usize) -> LibraryResult<Vec<Delivery>> {
        let mut connection = self.acquire_reader().await?;
        let rows: Vec<(String, String, Option<i64>)> = sqlx::query_as(
            "SELECT media_uri,kind,value FROM outbox WHERE next_attempt_at<=?1
             ORDER BY next_attempt_at,media_uri,kind LIMIT ?2",
        )
        .bind(now)
        .bind(limit as i64)
        .fetch_all(&mut *connection)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(media_uri, kind, value)| Delivery {
                media_uri,
                kind: if kind == "rating" {
                    DeliveryKind::Rating
                } else {
                    DeliveryKind::Favorite
                },
                value,
            })
            .collect())
    }

    /// When the next waiting delivery is due, if any waits.
    pub async fn next_delivery_at(&self) -> LibraryResult<Option<i64>> {
        let mut connection = self.acquire_reader().await?;
        Ok(
            sqlx::query_scalar("SELECT min(next_attempt_at) FROM outbox")
                .fetch_one(&mut *connection)
                .await?,
        )
    }

    /// The server accepted a delivery. Unless Rufin keeps its own copy, the value becomes the
    /// source's and Rufin's copy goes. A delivery the user changed since waits for the newer
    /// value and changes nothing.
    pub async fn delivered(&self, delivery: &Delivery, keep_copy: bool) -> LibraryResult<bool> {
        let mut write = self.write().await?;
        let removed =
            sqlx::query("DELETE FROM outbox WHERE media_uri=?1 AND kind=?2 AND value IS ?3")
                .bind(&delivery.media_uri)
                .bind(delivery.kind.as_str())
                .bind(delivery.value)
                .execute(write.connection().await?)
                .await?
                .rows_affected()
                > 0;
        if !removed {
            return Ok(false);
        }
        write.held(vec![delivery.media_uri.clone()]);
        if !keep_copy {
            put_native(
                &mut write,
                &delivery.media_uri,
                delivery.kind,
                delivery.value,
            )
            .await?;
            drop_copy(&mut write, &delivery.media_uri, delivery.kind).await?;
        }
        write.commit().await?;
        Ok(true)
    }

    /// The server of `source` could not take a delivery now; every one it waits for waits
    /// until `next_attempt_at`.
    pub async fn defer_deliveries(
        &self,
        source: &SourceId,
        next_attempt_at: i64,
    ) -> LibraryResult<()> {
        let prefixes = crate::keys::source_entity_kinds()
            .map(|kind| crate::keys::source_entity_prefix(source, kind))
            .collect::<Vec<_>>();
        let mut write = self.write().await?;
        sqlx::query("UPDATE outbox SET next_attempt_at=?2 WHERE next_attempt_at<?2 AND EXISTS(SELECT 1 FROM json_each(?1) prefix WHERE substr(media_uri,1,length(prefix.value))=prefix.value)")
            .bind(serde_json::to_string(&prefixes)?)
            .bind(next_attempt_at)
            .execute(write.connection().await?)
            .await?;
        write.commit().await?;
        Ok(())
    }

    /// Drops a delivery no server will take. Without `keep_copy`, Rufin's copy goes too, so the
    /// source's own value shows again.
    pub async fn drop_delivery(&self, delivery: &Delivery, keep_copy: bool) -> LibraryResult<bool> {
        let mut write = self.write().await?;
        let removed =
            sqlx::query("DELETE FROM outbox WHERE media_uri=?1 AND kind=?2 AND value IS ?3")
                .bind(&delivery.media_uri)
                .bind(delivery.kind.as_str())
                .bind(delivery.value)
                .execute(write.connection().await?)
                .await?
                .rows_affected()
                > 0;
        if removed {
            write.held(vec![delivery.media_uri.clone()]);
        }
        if removed && !keep_copy {
            drop_copy(&mut write, &delivery.media_uri, delivery.kind).await?;
        }
        write.commit().await?;
        Ok(removed)
    }
}

/// Puts a value as the one the source reports on the row of a server URI.
async fn put_native(
    write: &mut Write<'_>,
    uri: &str,
    kind: DeliveryKind,
    value: Option<i64>,
) -> LibraryResult<()> {
    async fn put<R: Row>(
        write: &mut Write<'_>,
        uri: &str,
        apply: impl Fn(&mut NativeState),
    ) -> LibraryResult<()> {
        if let Some(mut row) = write.by_natural::<R>(uri).await? {
            if let Some(state) = row.state() {
                apply(state);
            }
            write.put(row);
        }
        Ok(())
    }
    let apply = |state: &mut NativeState| match kind {
        DeliveryKind::Favorite => state.favorite = value.map(|value| value != 0),
        DeliveryKind::Rating => state.rating = value,
    };
    // Deliveries go to servers, whose URIs name the kind of their row.
    match crate::source_entity_parts(uri)
        .map(|(_, kind, _)| kind)
        .as_deref()
    {
        Some("album") => put::<Album>(write, uri, apply).await,
        Some("artist") => put::<Artist>(write, uri, apply).await,
        _ => put::<Track>(write, uri, apply).await,
    }
}

/// Drops Rufin's copy of one value: the whole row when it holds no other value.
async fn drop_copy(
    write: &mut Write<'_>,
    media_uri: &str,
    kind: DeliveryKind,
) -> LibraryResult<()> {
    let (column, other) = match kind {
        DeliveryKind::Favorite => ("favorite", "rating"),
        DeliveryKind::Rating => ("rating", "favorite"),
    };
    let connection = write.connection().await?;
    let mut states: Vec<StateRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "DELETE FROM user_state WHERE media_uri=?1 AND {other} IS NULL{STATE_DELETED}"
    )))
    .bind(media_uri)
    .fetch_all(&mut *connection)
    .await?;
    if states.is_empty() {
        states = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "UPDATE user_state SET {column}=NULL,changed_at=unixepoch() WHERE media_uri=?1 AND {column} IS NOT NULL{STATE_RETURNING}"
        )))
        .bind(media_uri)
        .fetch_all(&mut *connection)
        .await?;
    }
    write.states_changed(states);
    Ok(())
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct UserMediaStateWrite {
    pub media_uri: String,
    pub favorite: Option<bool>,
    /// Rufin precision, independent of a provider's coarser rating scale.
    pub rating: Option<i64>,
    /// When the value was set. Backups do not carry it; a restored value is set when restored.
    #[serde(skip)]
    #[sqlx(default)]
    pub changed_at: Option<i64>,
}

impl Catalog {
    /// Rufin's favorites and ratings, all of them or those of `uris`, each with whether a
    /// server delivery still waits for it.
    pub async fn user_states(
        &self,
        uris: Option<&[String]>,
    ) -> LibraryResult<Vec<(UserMediaStateWrite, bool)>> {
        let mut connection = self.acquire_reader().await?;
        let rows: Vec<(String, Option<bool>, Option<i64>, i64, bool)> = sqlx::query_as(
            "SELECT state.media_uri,state.favorite,state.rating,state.changed_at,
                EXISTS(SELECT 1 FROM outbox WHERE outbox.media_uri=state.media_uri)
             FROM user_state state
             WHERE ?1 IS NULL OR state.media_uri IN(SELECT value FROM json_each(?1))
             ORDER BY state.media_uri",
        )
        .bind(uris.map(serde_json::to_string).transpose()?)
        .fetch_all(&mut *connection)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(media_uri, favorite, rating, changed_at, delivering)| {
                (
                    UserMediaStateWrite {
                        media_uri,
                        favorite,
                        rating,
                        changed_at: Some(changed_at),
                    },
                    delivering,
                )
            })
            .collect())
    }

    /// Writes favorites and ratings as they are, in one Write, with no server delivery. A
    /// state with neither value removes the row.
    pub async fn put_user_states(&self, states: &[UserMediaStateWrite]) -> LibraryResult<()> {
        let mut write = self.write().await?;
        for state in states {
            write_user_media_state(&mut write, state).await?;
        }
        write.commit().await?;
        Ok(())
    }
}

pub(crate) async fn export_user_media_state_jsonl_on(
    connection: &mut SqliteConnection,
    mut output: impl std::io::Write,
) -> LibraryResult<u64> {
    use futures_util::TryStreamExt;
    output.write_all(b"{\"version\":1}\n")?;
    let mut rows = sqlx::query_as::<_, UserMediaStateWrite>(
        "SELECT media_uri,favorite,rating FROM user_state ORDER BY media_uri",
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
    write: &mut Write<'_>,
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
        write_user_media_state(write, &state).await?;
        count += 1;
    }
    Ok(count)
}

/// Returns whether the row changed.
pub(crate) async fn write_user_media_state(
    write: &mut Write<'_>,
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
    let connection = write.connection().await?;
    let states: Vec<StateRow> = if state.favorite.is_none() && state.rating.is_none() {
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "DELETE FROM user_state WHERE media_uri=?1{STATE_DELETED}"
        )))
        .bind(&state.media_uri)
        .fetch_all(&mut *connection)
        .await?
    } else {
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "INSERT INTO user_state(media_uri,favorite,rating,changed_at) VALUES(?1,?2,?3,coalesce(?4,unixepoch()))
            ON CONFLICT(media_uri) DO UPDATE SET favorite=excluded.favorite,rating=excluded.rating,changed_at=excluded.changed_at
            WHERE (favorite,rating) IS NOT (excluded.favorite,excluded.rating){STATE_RETURNING}"
        )))
        .bind(&state.media_uri)
        .bind(state.favorite)
        .bind(state.rating)
        .bind(state.changed_at)
        .fetch_all(&mut *connection)
        .await?
    };
    write.states_changed(states);
    Ok(())
}
