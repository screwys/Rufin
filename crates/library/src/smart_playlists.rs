//! Owns Smart Playlist definitions and direct SQL membership, row, and list queries.
//! One shared policy implements rule, Activity-window, limit, and ordering semantics.

use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqliteRow;
use sqlx::{AssertSqlSafe, Connection, FromRow, QueryBuilder, Row, Sqlite};

use crate::{
    Database, FolderKey, LibraryError, LibraryResult, ReadCancellation, SmartPlaylistKey,
    SourceKey, TrackKey,
};

const SMART_PLAYLIST_ROW_LIMIT: usize = 64;
const SMART_PLAYLIST_RULE_LIMIT: usize = 64;
const SMART_PLAYLIST_DEFINITION_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SmartPlaylistRuleField {
    Title,
    Artist,
    Album,
    Comment,
    Genre,
    Mood,
    Bpm,
    Rating,
    Year,
    Favorite,
    Played,
    PlayCount,
    SkipCount,
    LastPlayed,
    DateAdded,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SmartPlaylistRuleOperator {
    Contains,
    NotContains,
    Equals,
    NotEquals,
    Above,
    Below,
    Between,
    Is,
    IsNot,
    Before,
    After,
    IsEmpty,
    IsNotEmpty,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SmartPlaylistRuleValue {
    Text(String),
    Number(i64),
    NumberRange { min: i64, max: i64 },
    Bool(bool),
    Date(String),
    DateRange { start: String, end: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SmartPlaylistRule {
    pub field: SmartPlaylistRuleField,
    pub operator: SmartPlaylistRuleOperator,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<SmartPlaylistRuleValue>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SmartPlaylistSort {
    Title,
    Artist,
    Album,
    Year,
    DateAdded,
    LastPlayed,
    PlayCount,
    SkipCount,
    Bpm,
    Rating,
    Duration,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum SmartPlaylistActivityPeriod {
    Weekly,
    Monthly,
    Yearly,
    #[default]
    Lifetime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SmartPlaylistListSort {
    Position,
    Title,
    TrackCount,
    Duration,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SmartPlaylistDefinition {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub match_all: Vec<SmartPlaylistRule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub match_any: Vec<SmartPlaylistRule>,
    pub sort_field: SmartPlaylistSort,
    pub descending: bool,
    #[serde(default)]
    pub activity_period: SmartPlaylistActivityPeriod,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SmartPlaylistRow {
    pub smart_playlist_key: SmartPlaylistKey,
    pub source_key: SourceKey,
    pub object_id: String,
    pub name: String,
    pub definition: SmartPlaylistDefinition,
    pub position: i64,
    pub track_count: i64,
    pub duration_millis: i64,
}

impl<'row> FromRow<'row, SqliteRow> for SmartPlaylistRow {
    fn from_row(row: &'row SqliteRow) -> Result<Self, sqlx::Error> {
        let definition = row.try_get::<String, _>("definition_json")?;
        Ok(Self {
            smart_playlist_key: row.try_get("smart_playlist_key")?,
            source_key: row.try_get("source_key")?,
            object_id: row.try_get("object_id")?,
            name: row.try_get("name")?,
            definition: serde_json::from_str(&definition)
                .map_err(|error| sqlx::Error::Decode(Box::new(error)))?,
            position: row.try_get("position")?,
            track_count: row.try_get("track_count")?,
            duration_millis: row.try_get("duration_millis")?,
        })
    }
}

impl Database {
    pub async fn smart_playlist_key_by_object(
        &self,
        source: SourceKey,
        object_id: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<SmartPlaylistKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_scalar(
            "SELECT smart_playlist_key FROM smart_playlists WHERE source_key=?1 AND object_id=?2",
        )
        .bind(source)
        .bind(object_id)
        .fetch_optional(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn smart_playlist_order(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        sort: SmartPlaylistListSort,
        descending: bool,
        now: i64,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<SmartPlaylistKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        if folder.is_none()
            && matches!(
                sort,
                SmartPlaylistListSort::Position | SmartPlaylistListSort::Title
            )
        {
            let sql = match (sort, descending) {
                (SmartPlaylistListSort::Position, false) => {
                    "SELECT smart_playlist_key FROM smart_playlists WHERE source_key=?1 ORDER BY position,smart_playlist_key"
                }
                (SmartPlaylistListSort::Position, true) => {
                    "SELECT smart_playlist_key FROM smart_playlists WHERE source_key=?1 ORDER BY position DESC,smart_playlist_key"
                }
                (SmartPlaylistListSort::Title, false) => {
                    "SELECT smart_playlist_key FROM smart_playlists WHERE source_key=?1 ORDER BY normalized_name,smart_playlist_key"
                }
                (SmartPlaylistListSort::Title, true) => {
                    "SELECT smart_playlist_key FROM smart_playlists WHERE source_key=?1 ORDER BY normalized_name DESC,smart_playlist_key"
                }
                _ => unreachable!(),
            };
            let order = sqlx::query_scalar::<_, SmartPlaylistKey>(sql)
                .bind(source)
                .fetch_all(&mut *transaction)
                .await?;
            transaction.commit().await?;
            Database::clear_progress(&mut connection).await?;
            return Ok(order);
        }
        let sql = format!("{SMART_POLICY_SQL}\n{SMART_LIST_SELECT}");
        let order = sqlx::query_scalar::<_, SmartPlaylistKey>(AssertSqlSafe(sql.as_str()))
            .persistent(false)
            .bind(source)
            .bind(now)
            .bind(folder)
            .bind(Option::<SmartPlaylistKey>::None)
            .bind(match sort {
                SmartPlaylistListSort::Position => 0_i64,
                SmartPlaylistListSort::Title => 1,
                SmartPlaylistListSort::TrackCount => 2,
                SmartPlaylistListSort::Duration => 3,
            })
            .bind(descending)
            .fetch_all(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(order)
    }

    pub async fn smart_playlist_rows(
        &self,
        source: SourceKey,
        keys: &[SmartPlaylistKey],
        folder: Option<FolderKey>,
        now: i64,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<SmartPlaylistRow>> {
        if keys.len() > SMART_PLAYLIST_ROW_LIMIT {
            return Err(LibraryError::InvalidRequest(format!(
                "Smart Playlist reads are limited to {SMART_PLAYLIST_ROW_LIMIT} keys"
            )));
        }
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let mut query =
            QueryBuilder::<Sqlite>::new("WITH requested(smart_playlist_key, ordinal) AS (");
        query.push_values(keys.iter().enumerate(), |mut row, (ordinal, key)| {
            row.push_bind(*key).push_bind(ordinal as i64);
        });
        query.push(
            ") SELECT playlist.smart_playlist_key, playlist.source_key,
                      playlist.object_id, playlist.name,
                      playlist.definition_json, playlist.position,
                      0 AS track_count, 0 AS duration_millis
               FROM requested JOIN smart_playlists AS playlist USING(smart_playlist_key)
               WHERE playlist.source_key=",
        );
        query.push_bind(source).push(" ORDER BY requested.ordinal");
        let mut result = query
            .build_query_as::<SmartPlaylistRow>()
            .persistent(false)
            .fetch_all(&mut *transaction)
            .await?;
        for row in &mut result {
            normalize_definition(&mut row.definition)?;
            let facts = membership_facts(
                &mut transaction,
                source,
                row.smart_playlist_key,
                folder,
                now,
            )
            .await?;
            row.track_count = facts.0;
            row.duration_millis = facts.1;
        }
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(result)
    }

    pub async fn create_smart_playlist(
        &self,
        source: SourceKey,
        name: &str,
        definition: &SmartPlaylistDefinition,
    ) -> LibraryResult<SmartPlaylistKey> {
        let name = require_name(name)?;
        let definition = encode_definition(definition)?;
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let result = sqlx::query(
            "INSERT INTO smart_playlists(
                 source_key, object_id, name, normalized_name,
                 definition_json, position
             ) VALUES (
                 ?1, 'rufin:smart:' || lower(hex(randomblob(16))),
                 ?2, lower(?2), ?3,
                 (SELECT COALESCE(max(position) + 1, 0)
                  FROM smart_playlists WHERE source_key=?1)
             )",
        )
        .bind(source)
        .bind(name)
        .bind(definition)
        .execute(connection)
        .await?;
        Ok(SmartPlaylistKey::from_raw(result.last_insert_rowid()))
    }

    pub async fn update_smart_playlist(
        &self,
        source: SourceKey,
        key: SmartPlaylistKey,
        name: &str,
        definition: &SmartPlaylistDefinition,
    ) -> LibraryResult<bool> {
        let name = require_name(name)?;
        let definition = encode_definition(definition)?;
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(sqlx::query(
            "UPDATE smart_playlists
             SET name=?3, normalized_name=lower(?3), definition_json=?4
             WHERE source_key=?1 AND smart_playlist_key=?2",
        )
        .bind(source)
        .bind(key)
        .bind(name)
        .bind(definition)
        .execute(connection)
        .await?
        .rows_affected()
            == 1)
    }

    pub async fn delete_smart_playlist(
        &self,
        source: SourceKey,
        key: SmartPlaylistKey,
    ) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(
            sqlx::query(
                "DELETE FROM smart_playlists WHERE source_key=?1 AND smart_playlist_key=?2",
            )
            .bind(source)
            .bind(key)
            .execute(connection)
            .await?
            .rows_affected()
                == 1,
        )
    }

    pub async fn smart_playlist_track_order(
        &self,
        source: SourceKey,
        key: SmartPlaylistKey,
        folder: Option<FolderKey>,
        now: i64,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let sql = format!("{SMART_POLICY_SQL}\n{SMART_MEMBERSHIP_SELECT}");
        let result = sqlx::query_scalar::<_, TrackKey>(AssertSqlSafe(sql.as_str()))
            .persistent(false)
            .bind(source)
            .bind(now)
            .bind(folder)
            .bind(key)
            .fetch_all(&mut *transaction)
            .await;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }
}

async fn membership_facts(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    source: SourceKey,
    key: SmartPlaylistKey,
    folder: Option<FolderKey>,
    now: i64,
) -> LibraryResult<(i64, i64)> {
    let sql = format!("{SMART_POLICY_SQL}\n{SMART_FACTS_SELECT}");
    Ok(sqlx::query_as::<_, (i64, i64)>(AssertSqlSafe(sql.as_str()))
        .persistent(false)
        .bind(source)
        .bind(now)
        .bind(folder)
        .bind(key)
        .fetch_one(&mut **transaction)
        .await?)
}

fn require_name(name: &str) -> LibraryResult<&str> {
    let name = name.trim();
    if name.is_empty() {
        return Err(LibraryError::InvalidRequest(
            "Smart Playlist name cannot be empty".to_string(),
        ));
    }
    Ok(name)
}

fn encode_definition(definition: &SmartPlaylistDefinition) -> LibraryResult<String> {
    let mut definition = definition.clone();
    normalize_definition(&mut definition)?;
    let json = serde_json::to_string(&definition)?;
    if json.len() > SMART_PLAYLIST_DEFINITION_BYTES {
        return Err(LibraryError::InvalidRequest(format!(
            "Smart Playlist definitions are limited to {SMART_PLAYLIST_DEFINITION_BYTES} bytes"
        )));
    }
    Ok(json)
}

fn normalize_definition(definition: &mut SmartPlaylistDefinition) -> LibraryResult<()> {
    validate_definition(definition)?;
    for rule in definition
        .match_all
        .iter_mut()
        .chain(&mut definition.match_any)
    {
        let empty = matches!(
            rule.operator,
            SmartPlaylistRuleOperator::IsEmpty | SmartPlaylistRuleOperator::IsNotEmpty
        );
        let valid = match rule.field {
            SmartPlaylistRuleField::Title
            | SmartPlaylistRuleField::Artist
            | SmartPlaylistRuleField::Album
            | SmartPlaylistRuleField::Comment
            | SmartPlaylistRuleField::Genre
            | SmartPlaylistRuleField::Mood => {
                empty && rule.value.is_none()
                    || matches!(
                        (&rule.operator, &rule.value),
                        (
                            SmartPlaylistRuleOperator::Contains
                                | SmartPlaylistRuleOperator::NotContains
                                | SmartPlaylistRuleOperator::Equals
                                | SmartPlaylistRuleOperator::NotEquals,
                            Some(SmartPlaylistRuleValue::Text(_))
                        )
                    )
            }
            SmartPlaylistRuleField::Bpm
            | SmartPlaylistRuleField::Rating
            | SmartPlaylistRuleField::Year
            | SmartPlaylistRuleField::PlayCount
            | SmartPlaylistRuleField::SkipCount => {
                empty && rule.value.is_none()
                    || matches!(
                        (&rule.operator, &rule.value),
                        (
                            SmartPlaylistRuleOperator::Above
                                | SmartPlaylistRuleOperator::Below
                                | SmartPlaylistRuleOperator::Equals
                                | SmartPlaylistRuleOperator::NotEquals,
                            Some(SmartPlaylistRuleValue::Number(_))
                        ) | (
                            SmartPlaylistRuleOperator::Between,
                            Some(SmartPlaylistRuleValue::NumberRange { .. })
                        )
                    )
            }
            SmartPlaylistRuleField::Favorite | SmartPlaylistRuleField::Played => matches!(
                (&rule.operator, &rule.value),
                (
                    SmartPlaylistRuleOperator::Is | SmartPlaylistRuleOperator::IsNot,
                    Some(SmartPlaylistRuleValue::Bool(_))
                )
            ),
            SmartPlaylistRuleField::LastPlayed | SmartPlaylistRuleField::DateAdded => {
                empty && rule.value.is_none()
                    || matches!(
                        (&rule.operator, &rule.value),
                        (
                            SmartPlaylistRuleOperator::Before
                                | SmartPlaylistRuleOperator::After
                                | SmartPlaylistRuleOperator::Equals
                                | SmartPlaylistRuleOperator::NotEquals,
                            Some(SmartPlaylistRuleValue::Date(_))
                        ) | (
                            SmartPlaylistRuleOperator::Between,
                            Some(SmartPlaylistRuleValue::DateRange { .. })
                        )
                    )
            }
        };
        if !valid {
            return Err(LibraryError::InvalidRequest(
                "Smart Playlist rule operator and value do not match the field".to_string(),
            ));
        }
        match &mut rule.value {
            Some(SmartPlaylistRuleValue::Text(value) | SmartPlaylistRuleValue::Date(value)) => {
                *value = value.trim().to_string()
            }
            Some(SmartPlaylistRuleValue::NumberRange { min, max }) if *min > *max => {
                std::mem::swap(min, max)
            }
            Some(SmartPlaylistRuleValue::DateRange { start, end }) => {
                if *start > *end {
                    std::mem::swap(start, end);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_definition(definition: &SmartPlaylistDefinition) -> LibraryResult<()> {
    let rules = definition.match_all.len() + definition.match_any.len();
    if rules > SMART_PLAYLIST_RULE_LIMIT {
        return Err(LibraryError::InvalidRequest(format!(
            "Smart Playlists are limited to {SMART_PLAYLIST_RULE_LIMIT} rules"
        )));
    }
    if definition.limit == Some(0) {
        return Err(LibraryError::InvalidRequest(
            "Smart Playlist limits must be greater than zero".to_string(),
        ));
    }
    Ok(())
}

const SMART_POLICY_SQL: &str = r#"
WITH definitions AS (
    SELECT smart_playlist_key AS definition_key,
           definition_json,
           position,
           normalized_name,
           CASE json_extract(definition_json, '$.activity_period')
             WHEN 'Weekly' THEN 0
             WHEN 'Monthly' THEN 1
             WHEN 'Yearly' THEN 2
             ELSE 3
           END AS activity_period,
           CASE json_extract(definition_json, '$.sort_field')
             WHEN 'Title' THEN 0
             WHEN 'Artist' THEN 1
             WHEN 'Album' THEN 2
             WHEN 'Year' THEN 3
             WHEN 'DateAdded' THEN 4
             WHEN 'LastPlayed' THEN 5
             WHEN 'PlayCount' THEN 6
             WHEN 'SkipCount' THEN 7
             WHEN 'Bpm' THEN 8
             WHEN 'Rating' THEN 9
             WHEN 'Duration' THEN 10
           END AS membership_sort,
           json_extract(definition_json, '$.descending') AS membership_descending,
           json_extract(definition_json, '$.limit') AS membership_limit
    FROM smart_playlists
    WHERE source_key=?1
      AND (?4 IS NULL OR smart_playlist_key=?4)
),
rules(
    definition_key, rule_group, field, operator, text_value, number_value,
    range_min, range_max, bool_value, date_value, date_start, date_end
) AS (
    SELECT definition.definition_key, 'all',
           json_extract(rule.value, '$.field'),
           json_extract(rule.value, '$.operator'),
           trim(json_extract(rule.value, '$.value.Text')),
           json_extract(rule.value, '$.value.Number'),
           min(
             json_extract(rule.value, '$.value.NumberRange.min'),
             json_extract(rule.value, '$.value.NumberRange.max')
           ),
           max(
             json_extract(rule.value, '$.value.NumberRange.min'),
             json_extract(rule.value, '$.value.NumberRange.max')
           ),
           json_extract(rule.value, '$.value.Bool'),
           trim(json_extract(rule.value, '$.value.Date')),
           min(
             trim(json_extract(rule.value, '$.value.DateRange.start')),
             trim(json_extract(rule.value, '$.value.DateRange.end'))
           ),
           max(
             trim(json_extract(rule.value, '$.value.DateRange.start')),
             trim(json_extract(rule.value, '$.value.DateRange.end'))
           )
    FROM definitions AS definition,
         json_each(definition.definition_json, '$.match_all') AS rule
    UNION ALL
    SELECT definition.definition_key, 'any',
           json_extract(rule.value, '$.field'),
           json_extract(rule.value, '$.operator'),
           trim(json_extract(rule.value, '$.value.Text')),
           json_extract(rule.value, '$.value.Number'),
           min(
             json_extract(rule.value, '$.value.NumberRange.min'),
             json_extract(rule.value, '$.value.NumberRange.max')
           ),
           max(
             json_extract(rule.value, '$.value.NumberRange.min'),
             json_extract(rule.value, '$.value.NumberRange.max')
           ),
           json_extract(rule.value, '$.value.Bool'),
           trim(json_extract(rule.value, '$.value.Date')),
           min(
             trim(json_extract(rule.value, '$.value.DateRange.start')),
             trim(json_extract(rule.value, '$.value.DateRange.end'))
           ),
           max(
             trim(json_extract(rule.value, '$.value.DateRange.start')),
             trim(json_extract(rule.value, '$.value.DateRange.end'))
           )
    FROM definitions AS definition,
         json_each(definition.definition_json, '$.match_any') AS rule
),
window_listens AS (
    SELECT definition.definition_key,
           listen.track_key,
           count(*) AS play_count,
           sum(listen.skipped) AS skip_count,
           max(listen.started_at) AS last_played
    FROM definitions AS definition
    JOIN listens AS listen
      ON listen.source_key=?1
     AND listen.track_key IS NOT NULL
     AND (
       definition.activity_period=3
       OR listen.started_at >= ?2 - CASE definition.activity_period
         WHEN 0 THEN 604800
         WHEN 1 THEN 2592000
         WHEN 2 THEN 31536000
       END
     )
     AND listen.started_at <= ?2
    GROUP BY definition.definition_key, listen.track_key
),
lifetime_listens AS (
    SELECT track_key, count(*) AS play_count
    FROM listens
    WHERE source_key=?1 AND track_key IS NOT NULL
    GROUP BY track_key
),
facts AS (
    SELECT definition.definition_key,
           definition.position AS definition_position,
           definition.normalized_name AS definition_name,
           definition.membership_sort,
           definition.membership_descending,
           definition.membership_limit,
           track.*,
           CASE WHEN definition.activity_period=3
             THEN COALESCE(baseline.play_count, 0)
             ELSE 0
           END + COALESCE(window.play_count, 0) AS play_count,
           CASE WHEN definition.activity_period=3
             THEN COALESCE(baseline.skip_count, 0)
             ELSE 0
           END + COALESCE(window.skip_count, 0) AS skip_count,
           CASE
             WHEN definition.activity_period<>3 THEN window.last_played
             WHEN baseline.last_played_at IS NULL THEN window.last_played
             WHEN window.last_played IS NULL THEN baseline.last_played_at
             ELSE max(baseline.last_played_at, window.last_played)
           END AS last_played,
           COALESCE(baseline.play_count, 0) +
             COALESCE(lifetime.play_count, 0) AS lifetime_play_count
    FROM definitions AS definition
    CROSS JOIN tracks AS track
    LEFT JOIN activity_baseline AS baseline
      ON baseline.source_key=track.source_key
     AND baseline.track_object_id=track.object_id
    LEFT JOIN window_listens AS window
      ON window.definition_key=definition.definition_key
     AND window.track_key=track.track_key
    LEFT JOIN lifetime_listens AS lifetime USING(track_key)
    WHERE track.source_key=?1
      AND (?3 IS NULL OR EXISTS (
        SELECT 1 FROM track_folders AS scope
        WHERE scope.track_key=track.track_key AND scope.folder_key=?3
      ))
),
rule_values AS (
    SELECT facts.*,
           rules.rule_group,
           rules.field,
           rules.operator,
           rules.text_value,
           rules.number_value,
           rules.range_min,
           rules.range_max,
           rules.bool_value,
           rules.date_value,
           rules.date_start,
           rules.date_end,
           CASE rules.field
             WHEN 'Title' THEN facts.title
             WHEN 'Artist' THEN facts.display_artist
             WHEN 'Album' THEN facts.display_album
             WHEN 'Comment' THEN facts.comment
           END AS text_fact,
           CASE rules.field
             WHEN 'Bpm' THEN facts.bpm
             WHEN 'Rating' THEN COALESCE(facts.user_rating, facts.source_rating) / 10
             WHEN 'Year' THEN facts.year
             WHEN 'PlayCount' THEN facts.play_count
             WHEN 'SkipCount' THEN facts.skip_count
           END AS number_fact,
           CASE rules.field
             WHEN 'Favorite' THEN COALESCE(facts.user_favorite, facts.source_favorite)
             WHEN 'Played' THEN facts.lifetime_play_count > 0
           END AS bool_fact,
           CASE rules.field
             WHEN 'LastPlayed' THEN date(facts.last_played, 'unixepoch')
             WHEN 'DateAdded' THEN facts.date_added
           END AS date_fact
    FROM facts
    JOIN rules USING(definition_key)
),
rule_matches(definition_key, track_key, rule_group, matched) AS (
    SELECT value.definition_key, value.track_key, value.rule_group,
      CASE
        WHEN value.field IN ('Title', 'Artist', 'Album', 'Comment') THEN
          CASE value.operator
            WHEN 'IsEmpty' THEN value.text_fact IS NULL OR trim(value.text_fact)=''
            WHEN 'IsNotEmpty' THEN value.text_fact IS NOT NULL AND trim(value.text_fact)<>''
            WHEN 'Contains' THEN value.text_fact IS NOT NULL AND value.text_value IS NOT NULL
                 AND instr(lower(value.text_fact), lower(value.text_value)) > 0
            WHEN 'NotContains' THEN value.text_fact IS NOT NULL AND value.text_value IS NOT NULL
                 AND instr(lower(value.text_fact), lower(value.text_value)) = 0
            WHEN 'Equals' THEN value.text_fact IS NOT NULL AND value.text_value IS NOT NULL
                 AND lower(value.text_fact)=lower(value.text_value)
            WHEN 'NotEquals' THEN value.text_fact IS NOT NULL AND value.text_value IS NOT NULL
                 AND lower(value.text_fact)<>lower(value.text_value)
            ELSE 0 END
        WHEN value.field = 'Genre' THEN CASE value.operator
          WHEN 'IsEmpty' THEN NOT EXISTS (
            SELECT 1 FROM track_genres WHERE track_key=value.track_key)
          WHEN 'IsNotEmpty' THEN EXISTS (
            SELECT 1 FROM track_genres WHERE track_key=value.track_key)
          WHEN 'Contains' THEN EXISTS (
            SELECT 1 FROM track_genres JOIN genres USING(genre_key)
            WHERE track_key=value.track_key
              AND instr(lower(genres.name), lower(value.text_value)) > 0)
          WHEN 'NotContains' THEN EXISTS (
            SELECT 1 FROM track_genres WHERE track_key=value.track_key) AND NOT EXISTS (
            SELECT 1 FROM track_genres JOIN genres USING(genre_key)
            WHERE track_key=value.track_key
              AND instr(lower(genres.name), lower(value.text_value)) > 0)
          WHEN 'Equals' THEN EXISTS (
            SELECT 1 FROM track_genres JOIN genres USING(genre_key)
            WHERE track_key=value.track_key AND lower(genres.name)=lower(value.text_value))
          WHEN 'NotEquals' THEN EXISTS (
            SELECT 1 FROM track_genres WHERE track_key=value.track_key) AND NOT EXISTS (
            SELECT 1 FROM track_genres JOIN genres USING(genre_key)
            WHERE track_key=value.track_key AND lower(genres.name)=lower(value.text_value))
          ELSE 0 END
        WHEN value.field = 'Mood' THEN CASE value.operator
          WHEN 'IsEmpty' THEN NOT EXISTS (
            SELECT 1 FROM track_moods WHERE track_key=value.track_key)
          WHEN 'IsNotEmpty' THEN EXISTS (
            SELECT 1 FROM track_moods WHERE track_key=value.track_key)
          WHEN 'Contains' THEN EXISTS (
            SELECT 1 FROM track_moods JOIN moods USING(mood_key)
            WHERE track_key=value.track_key
              AND instr(lower(moods.name), lower(value.text_value)) > 0)
          WHEN 'NotContains' THEN EXISTS (
            SELECT 1 FROM track_moods WHERE track_key=value.track_key) AND NOT EXISTS (
            SELECT 1 FROM track_moods JOIN moods USING(mood_key)
            WHERE track_key=value.track_key
              AND instr(lower(moods.name), lower(value.text_value)) > 0)
          WHEN 'Equals' THEN EXISTS (
            SELECT 1 FROM track_moods JOIN moods USING(mood_key)
            WHERE track_key=value.track_key AND lower(moods.name)=lower(value.text_value))
          WHEN 'NotEquals' THEN EXISTS (
            SELECT 1 FROM track_moods WHERE track_key=value.track_key) AND NOT EXISTS (
            SELECT 1 FROM track_moods JOIN moods USING(mood_key)
            WHERE track_key=value.track_key AND lower(moods.name)=lower(value.text_value))
          ELSE 0 END
        WHEN value.field IN ('Bpm', 'Rating', 'Year', 'PlayCount', 'SkipCount') THEN
          CASE value.operator
            WHEN 'IsEmpty' THEN value.number_fact IS NULL
            WHEN 'IsNotEmpty' THEN value.number_fact IS NOT NULL
            WHEN 'Above' THEN value.number_fact > value.number_value
            WHEN 'Below' THEN value.number_fact < value.number_value
            WHEN 'Equals' THEN value.number_fact = value.number_value
            WHEN 'NotEquals' THEN value.number_fact <> value.number_value
            WHEN 'Between' THEN value.number_fact BETWEEN value.range_min AND value.range_max
            ELSE 0 END
        WHEN value.field IN ('Favorite', 'Played') THEN
          CASE value.operator
            WHEN 'Is' THEN value.bool_fact = value.bool_value
            WHEN 'IsNot' THEN value.bool_fact <> value.bool_value
            ELSE 0 END
        WHEN value.field IN ('LastPlayed', 'DateAdded') THEN
          CASE value.operator
            WHEN 'IsEmpty' THEN value.date_fact IS NULL OR value.date_fact=''
            WHEN 'IsNotEmpty' THEN value.date_fact IS NOT NULL AND value.date_fact<>''
            WHEN 'Before' THEN value.date_fact < value.date_value
            WHEN 'After' THEN value.date_fact > value.date_value
            WHEN 'Equals' THEN value.date_fact = value.date_value
            WHEN 'NotEquals' THEN value.date_fact <> value.date_value
            WHEN 'Between' THEN value.date_fact BETWEEN value.date_start AND value.date_end
            ELSE 0 END
        ELSE 0
      END
    FROM rule_values AS value
),
matched AS (
    SELECT facts.*
    FROM facts
    WHERE NOT EXISTS (
      SELECT 1 FROM rule_matches
      WHERE definition_key=facts.definition_key
        AND track_key=facts.track_key
        AND rule_group='all'
        AND matched IS NOT 1
    ) AND (
      NOT EXISTS (
        SELECT 1 FROM rules
        WHERE definition_key=facts.definition_key AND rule_group='any'
      ) OR EXISTS (
        SELECT 1 FROM rule_matches
        WHERE definition_key=facts.definition_key
          AND track_key=facts.track_key
          AND rule_group='any'
          AND matched
      )
    )
),
ranked AS (
    SELECT matched.*,
           row_number() OVER (
             PARTITION BY definition_key
             ORDER BY
               CASE WHEN membership_sort=0 AND membership_descending=0 THEN sort_text END ASC,
               CASE WHEN membership_sort=0 AND membership_descending=1 THEN sort_text END DESC,
               CASE WHEN membership_sort=1 AND membership_descending=0 THEN display_artist END ASC,
               CASE WHEN membership_sort=1 AND membership_descending=1 THEN display_artist END DESC,
               CASE WHEN membership_sort=2 AND membership_descending=0 THEN display_album END ASC,
               CASE WHEN membership_sort=2 AND membership_descending=1 THEN display_album END DESC,
               CASE WHEN membership_sort=3 AND membership_descending=0 THEN year END ASC NULLS LAST,
               CASE WHEN membership_sort=3 AND membership_descending=1 THEN year END DESC NULLS LAST,
               CASE WHEN membership_sort=4 AND membership_descending=0 THEN date_added END ASC NULLS LAST,
               CASE WHEN membership_sort=4 AND membership_descending=1 THEN date_added END DESC NULLS LAST,
               CASE WHEN membership_sort=5 AND membership_descending=0 THEN last_played END ASC NULLS LAST,
               CASE WHEN membership_sort=5 AND membership_descending=1 THEN last_played END DESC NULLS LAST,
               CASE WHEN membership_sort=6 AND membership_descending=0 THEN play_count END ASC,
               CASE WHEN membership_sort=6 AND membership_descending=1 THEN play_count END DESC,
               CASE WHEN membership_sort=7 AND membership_descending=0 THEN skip_count END ASC,
               CASE WHEN membership_sort=7 AND membership_descending=1 THEN skip_count END DESC,
               CASE WHEN membership_sort=8 AND membership_descending=0 THEN bpm END ASC NULLS LAST,
               CASE WHEN membership_sort=8 AND membership_descending=1 THEN bpm END DESC NULLS LAST,
               CASE WHEN membership_sort=9 AND membership_descending=0
                 THEN COALESCE(user_rating, source_rating) END ASC NULLS LAST,
               CASE WHEN membership_sort=9 AND membership_descending=1
                 THEN COALESCE(user_rating, source_rating) END DESC NULLS LAST,
               CASE WHEN membership_sort=10 AND membership_descending=0 THEN duration_millis END ASC,
               CASE WHEN membership_sort=10 AND membership_descending=1 THEN duration_millis END DESC,
               sort_text, track_key
           ) AS membership_position
    FROM matched
),
selected AS (
    SELECT *
    FROM ranked
    WHERE membership_limit IS NULL OR membership_position<=membership_limit
)
"#;

const SMART_MEMBERSHIP_SELECT: &str = r#"
SELECT track_key
FROM selected
WHERE definition_key=?4
ORDER BY membership_position
"#;

const SMART_FACTS_SELECT: &str = r#"
SELECT count(*), COALESCE(sum(duration_millis), 0)
FROM selected
WHERE definition_key=?4
"#;

const SMART_LIST_SELECT: &str = r#"
, smart_stats AS (
    SELECT definition.definition_key,
           definition.position,
           definition.normalized_name,
           count(selected.track_key) AS track_count,
           COALESCE(sum(selected.duration_millis), 0) AS duration_millis
    FROM definitions AS definition
    LEFT JOIN selected USING(definition_key)
    GROUP BY definition.definition_key
)
SELECT definition_key
FROM smart_stats
WHERE ?3 IS NULL OR track_count>0
ORDER BY
  CASE WHEN ?5=0 AND ?6=0 THEN position END ASC,
  CASE WHEN ?5=0 AND ?6=1 THEN position END DESC,
  CASE WHEN ?5=1 AND ?6=0 THEN normalized_name END ASC,
  CASE WHEN ?5=1 AND ?6=1 THEN normalized_name END DESC,
  CASE WHEN ?5=2 AND ?6=0 THEN track_count END ASC,
  CASE WHEN ?5=2 AND ?6=1 THEN track_count END DESC,
  CASE WHEN ?5=3 AND ?6=0 THEN duration_millis END ASC,
  CASE WHEN ?5=3 AND ?6=1 THEN duration_millis END DESC,
  position, definition_key
"#;
