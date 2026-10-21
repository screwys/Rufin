//! Owns Smart Playlist definitions and direct SQL URI-result, row, and list queries.
//! One shared policy implements rule, Activity-window, limit, and ordering semantics.

use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use sqlx::{AssertSqlSafe, Connection, FromRow, SqliteConnection};

use crate::{
    CatalogSort, Database, FolderKey, LibraryError, LibraryResult, ReadCancellation,
    SmartPlaylistKey, SourceKey,
};

const SMART_PLAYLIST_ROW_LIMIT: usize = 64;
const SMART_PLAYLIST_RULE_LIMIT: usize = 64;
const SMART_PLAYLIST_DEFINITION_BYTES: usize = 256 * 1024;
const MOST_PLAYED_OBJECT_ID: &str = "builtin:most_played";
const NEVER_PLAYED_OBJECT_ID: &str = "builtin:never_played";

const MOST_SKIPPED_OBJECT_ID: &str = "builtin:most_skipped";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
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
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
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
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SmartPlaylistRuleValue {
    Text(String),
    Number(i64),
    NumberRange { min: i64, max: i64 },
    Bool(bool),
    Date(String),
    DateRange { start: String, end: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SmartPlaylistRuleValueKind {
    None,
    Text,
    Number,
    NumberRange,
    Date,
    DateRange,
    Bool,
}

const TEXT_OPERATORS: &[SmartPlaylistRuleOperator] = &[
    SmartPlaylistRuleOperator::Contains,
    SmartPlaylistRuleOperator::NotContains,
    SmartPlaylistRuleOperator::Equals,
    SmartPlaylistRuleOperator::NotEquals,
    SmartPlaylistRuleOperator::IsEmpty,
    SmartPlaylistRuleOperator::IsNotEmpty,
];
const NUMBER_OPERATORS: &[SmartPlaylistRuleOperator] = &[
    SmartPlaylistRuleOperator::Above,
    SmartPlaylistRuleOperator::Below,
    SmartPlaylistRuleOperator::Equals,
    SmartPlaylistRuleOperator::NotEquals,
    SmartPlaylistRuleOperator::Between,
    SmartPlaylistRuleOperator::IsEmpty,
    SmartPlaylistRuleOperator::IsNotEmpty,
];
const BOOL_OPERATORS: &[SmartPlaylistRuleOperator] = &[
    SmartPlaylistRuleOperator::Is,
    SmartPlaylistRuleOperator::IsNot,
];
const DATE_OPERATORS: &[SmartPlaylistRuleOperator] = &[
    SmartPlaylistRuleOperator::Before,
    SmartPlaylistRuleOperator::After,
    SmartPlaylistRuleOperator::Equals,
    SmartPlaylistRuleOperator::NotEquals,
    SmartPlaylistRuleOperator::Between,
    SmartPlaylistRuleOperator::IsEmpty,
    SmartPlaylistRuleOperator::IsNotEmpty,
];

impl SmartPlaylistRuleField {
    pub const ALL: [Self; 15] = [
        Self::Title,
        Self::Artist,
        Self::Album,
        Self::Comment,
        Self::Genre,
        Self::Mood,
        Self::Bpm,
        Self::Rating,
        Self::Year,
        Self::Favorite,
        Self::Played,
        Self::PlayCount,
        Self::SkipCount,
        Self::LastPlayed,
        Self::DateAdded,
    ];

    pub fn operators(self) -> &'static [SmartPlaylistRuleOperator] {
        match self {
            Self::Title | Self::Artist | Self::Album | Self::Comment | Self::Genre | Self::Mood => {
                TEXT_OPERATORS
            }
            Self::Favorite | Self::Played => BOOL_OPERATORS,
            Self::LastPlayed | Self::DateAdded => DATE_OPERATORS,
            _ => NUMBER_OPERATORS,
        }
    }

    pub fn value_kind(
        self,
        operator: SmartPlaylistRuleOperator,
    ) -> Option<SmartPlaylistRuleValueKind> {
        if !self.operators().contains(&operator) {
            return None;
        }
        if matches!(
            operator,
            SmartPlaylistRuleOperator::IsEmpty | SmartPlaylistRuleOperator::IsNotEmpty
        ) {
            return Some(SmartPlaylistRuleValueKind::None);
        }
        Some(match self {
            Self::Favorite | Self::Played => SmartPlaylistRuleValueKind::Bool,
            Self::LastPlayed | Self::DateAdded
                if operator == SmartPlaylistRuleOperator::Between =>
            {
                SmartPlaylistRuleValueKind::DateRange
            }
            Self::LastPlayed | Self::DateAdded => SmartPlaylistRuleValueKind::Date,
            Self::Bpm | Self::Rating | Self::Year | Self::PlayCount | Self::SkipCount
                if operator == SmartPlaylistRuleOperator::Between =>
            {
                SmartPlaylistRuleValueKind::NumberRange
            }
            Self::Bpm | Self::Rating | Self::Year | Self::PlayCount | Self::SkipCount => {
                SmartPlaylistRuleValueKind::Number
            }
            _ => SmartPlaylistRuleValueKind::Text,
        })
    }

    pub fn number_bounds(self) -> (i64, i64, i64) {
        match self {
            Self::Rating => (0, 10, 0),
            Self::Year => (0, 3000, 2000),
            Self::Bpm => (0, 1000, 120),
            _ => (0, i32::MAX as i64, 0),
        }
    }

    pub fn default_value(
        self,
        operator: SmartPlaylistRuleOperator,
    ) -> Option<SmartPlaylistRuleValue> {
        match self.value_kind(operator)? {
            SmartPlaylistRuleValueKind::None => None,
            SmartPlaylistRuleValueKind::Text => Some(SmartPlaylistRuleValue::Text(String::new())),
            SmartPlaylistRuleValueKind::Number => {
                Some(SmartPlaylistRuleValue::Number(self.number_bounds().2))
            }
            SmartPlaylistRuleValueKind::NumberRange => Some(SmartPlaylistRuleValue::NumberRange {
                min: self.number_bounds().2,
                max: self.number_bounds().2,
            }),
            SmartPlaylistRuleValueKind::Date => Some(SmartPlaylistRuleValue::Date(String::new())),
            SmartPlaylistRuleValueKind::DateRange => Some(SmartPlaylistRuleValue::DateRange {
                start: String::new(),
                end: String::new(),
            }),
            SmartPlaylistRuleValueKind::Bool => Some(SmartPlaylistRuleValue::Bool(true)),
        }
    }

    pub fn default_rule(self) -> SmartPlaylistRule {
        let operator = self.operators()[0];
        SmartPlaylistRule {
            field: self,
            operator,
            value: self.default_value(operator),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SmartPlaylistRule {
    pub field: SmartPlaylistRuleField,
    pub operator: SmartPlaylistRuleOperator,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<SmartPlaylistRuleValue>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
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

impl SmartPlaylistSort {
    pub const ALL: [Self; 11] = [
        Self::Title,
        Self::Artist,
        Self::Album,
        Self::Year,
        Self::DateAdded,
        Self::LastPlayed,
        Self::PlayCount,
        Self::SkipCount,
        Self::Bpm,
        Self::Rating,
        Self::Duration,
    ];
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SmartPlaylistActivityPeriod {
    Weekly,
    Monthly,
    Yearly,
    #[default]
    Lifetime,
}

impl SmartPlaylistActivityPeriod {
    fn seconds(self) -> Option<i64> {
        match self {
            Self::Weekly => Some(604800),
            Self::Monthly => Some(2592000),
            Self::Yearly => Some(31536000),
            Self::Lifetime => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SmartPlaylistDefinition {
    #[serde(default)]
    pub current: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub match_all: Vec<SmartPlaylistRule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub match_any: Vec<SmartPlaylistRule>,
    pub sort_field: SmartPlaylistSort,
    pub descending: bool,
    #[serde(default)]
    pub activity_period: SmartPlaylistActivityPeriod,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<crate::LibraryIndex>,
}

impl Default for SmartPlaylistDefinition {
    fn default() -> Self {
        Self {
            current: false,
            match_all: Vec::new(),
            match_any: Vec::new(),
            sort_field: SmartPlaylistSort::Title,
            descending: false,
            activity_period: SmartPlaylistActivityPeriod::Lifetime,
            limit: None,
        }
    }
}

fn default_smart_playlists() -> [(&'static str, &'static str, SmartPlaylistDefinition); 3] {
    [
        (
            MOST_PLAYED_OBJECT_ID,
            "Most Played",
            SmartPlaylistDefinition {
                current: false,
                match_all: vec![SmartPlaylistRule {
                    field: SmartPlaylistRuleField::Played,
                    operator: SmartPlaylistRuleOperator::Is,
                    value: Some(SmartPlaylistRuleValue::Bool(true)),
                }],
                match_any: Vec::new(),
                sort_field: SmartPlaylistSort::PlayCount,
                descending: true,
                activity_period: SmartPlaylistActivityPeriod::Lifetime,
                limit: None,
            },
        ),
        (
            NEVER_PLAYED_OBJECT_ID,
            "Never Played",
            SmartPlaylistDefinition {
                current: false,
                match_all: vec![SmartPlaylistRule {
                    field: SmartPlaylistRuleField::Played,
                    operator: SmartPlaylistRuleOperator::Is,
                    value: Some(SmartPlaylistRuleValue::Bool(false)),
                }],
                match_any: Vec::new(),
                sort_field: SmartPlaylistSort::Title,
                descending: false,
                activity_period: SmartPlaylistActivityPeriod::Lifetime,
                limit: None,
            },
        ),
        (
            MOST_SKIPPED_OBJECT_ID,
            "Most Skipped",
            SmartPlaylistDefinition {
                current: false,
                match_all: vec![SmartPlaylistRule {
                    field: SmartPlaylistRuleField::SkipCount,
                    operator: SmartPlaylistRuleOperator::Above,
                    value: Some(SmartPlaylistRuleValue::Number(0)),
                }],
                match_any: Vec::new(),
                sort_field: SmartPlaylistSort::SkipCount,
                descending: true,
                activity_period: SmartPlaylistActivityPeriod::Lifetime,
                limit: None,
            },
        ),
    ]
}

#[derive(Clone, Debug, PartialEq, FromRow)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SmartPlaylistRow {
    pub smart_playlist_key: SmartPlaylistKey,
    pub object_id: String,
    pub name: String,
    #[sqlx(rename = "definition_json", json)]
    pub definition: SmartPlaylistDefinition,
    pub position: i64,
    pub track_count: i64,
    pub duration_millis: i64,
    pub downloaded_count: i64,
    pub artwork_binding: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SmartPlaylistValueSuggestions {
    pub genres: Vec<String>,
    pub moods: Vec<String>,
}

pub(crate) async fn load_smart_playlist_requested(
    connection: &mut SqliteConnection,
    source: Option<SourceKey>,
    requested: crate::query::RequestedKeys,
    folder: Option<FolderKey>,
    now: i64,
) -> LibraryResult<Vec<SmartPlaylistRow>> {
    let sql = format!(
        "WITH requested AS ({}) SELECT playlist.smart_playlist_key,playlist.object_id,playlist.name,
         playlist.definition_json,playlist.position,{} artwork_binding,
         0 track_count,0 duration_millis,0 downloaded_count
         FROM requested JOIN smart_playlists playlist ON playlist.smart_playlist_key=requested.value
         ORDER BY requested.ordinal",
        requested.sql, crate::artwork::preview_sql("playlist.artwork_binding")
    );
    let mut result =
        sqlx::query_as_with::<_, SmartPlaylistRow, _>(AssertSqlSafe(sql), requested.arguments)
            .persistent(false)
            .fetch_all(&mut *connection)
            .await?;
    if result.is_empty() {
        return Ok(result);
    }
    let definitions = result
        .iter()
        .map(|row| (row.smart_playlist_key.raw(), row.definition.clone()))
        .collect::<Vec<_>>();
    let membership =
        smart_membership_from_definitions(connection, source, folder, now, &definitions).await?;
    let totals = sqlx::query_as::<_,(i64,i64,i64,i64)>(AssertSqlSafe(format!(
        "{membership} SELECT selected.definition_key,count(*),COALESCE(sum(track.duration_millis),0),
         count(CASE WHEN track.is_downloaded THEN 1 END)
         FROM selected JOIN tracks track USING(track_key) GROUP BY selected.definition_key"
    ))).fetch_all(&mut *connection).await?.into_iter().map(|(key,count,duration,downloaded)| (key,(count,duration,downloaded))).collect::<std::collections::HashMap<_,_>>();
    for row in &mut result {
        if let Some(&(count, duration, downloaded)) = totals.get(&row.smart_playlist_key.raw()) {
            row.track_count = count;
            row.duration_millis = duration;
            row.downloaded_count = downloaded;
        }
    }
    let needs_context = source.is_some()
        && result.iter().any(|row| {
            row.definition.current
                && row.artwork_binding.as_ref().is_none_or(|binding| {
                    serde_json::from_slice::<Vec<crate::ArtworkCandidate>>(binding).is_ok_and(
                        |candidates| {
                            !candidates
                                .iter()
                                .any(crate::ArtworkCandidate::is_automatic_playlist)
                        },
                    )
                })
        });
    let context: (Option<String>, Option<String>) = if needs_context {
        sqlx::query_as("SELECT (SELECT object_id FROM sources WHERE source_key=?1),(SELECT object_id FROM folders WHERE folder_key=?2)").bind(source).bind(folder).fetch_one(&mut *connection).await?
    } else {
        (None, None)
    };
    for row in &mut result {
        normalize_definition(&mut row.definition)?;
        row.artwork_binding = Some(crate::artwork::playlist_artwork(
            row.artwork_binding.take(),
            (true, None, row.object_id.clone()),
            if row.definition.current {
                context.clone()
            } else {
                (None, None)
            },
        )?);
    }
    Ok(result)
}

pub(crate) async fn create_default_smart_playlists(
    connection: &mut SqliteConnection,
) -> LibraryResult<()> {
    for (position, (object_id, name, definition)) in
        default_smart_playlists().into_iter().enumerate()
    {
        sqlx::query(
            "INSERT INTO smart_playlists(object_id,name,normalized_name,definition_json,position)
             VALUES (?1,?2,lower(?2),?3,?4)",
        )
        .bind(object_id)
        .bind(name)
        .bind(encode_definition(&definition)?)
        .bind(position as i64)
        .execute(&mut *connection)
        .await?;
    }
    Ok(())
}

pub(crate) async fn sync_smart_on(
    connection: &mut SqliteConnection,
    key: SmartPlaylistKey,
) -> LibraryResult<()> {
    let (identity, position): (String, i64) = sqlx::query_as(
        "SELECT object_id,position FROM smart_playlists WHERE smart_playlist_key=?1",
    )
    .bind(key)
    .fetch_one(&mut *connection)
    .await?;
    crate::connect::mark_current_on(connection, "smart", &identity, false).await?;
    crate::profile::write_local_on(
        connection,
        &[crate::ConnectRecord {
            kind: "smart_order".into(),
            key: identity,
            value: Some(serde_json::json!({"position":position})),
        }],
    )
    .await
}

impl Database {
    pub async fn smart_playlist_value_suggestions(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<SmartPlaylistValueSuggestions> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let genres = sqlx::query_scalar::<_, String>(
            "SELECT genre.name FROM genres genre WHERE genre.source_key=?1
             AND (?2 IS NULL OR EXISTS (
               SELECT 1 FROM track_genres relation JOIN track_folders scope USING(track_key)
               WHERE relation.genre_key=genre.genre_key AND scope.folder_key=?2))
             ORDER BY genre.sort_text,genre.genre_key LIMIT 100",
        )
        .bind(source)
        .bind(folder)
        .fetch_all(&mut *transaction)
        .await?;
        let moods = sqlx::query_scalar::<_, String>(
            "SELECT mood.name FROM moods mood WHERE mood.source_key=?1
             AND (?2 IS NULL OR EXISTS (
               SELECT 1 FROM track_moods relation JOIN track_folders scope USING(track_key)
               WHERE relation.mood_key=mood.mood_key AND scope.folder_key=?2))
             ORDER BY mood.sort_text,mood.mood_key LIMIT 100",
        )
        .bind(source)
        .bind(folder)
        .fetch_all(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(SmartPlaylistValueSuggestions { genres, moods })
    }

    pub async fn smart_playlist_key_by_object(
        &self,
        object_id: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<SmartPlaylistKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(
            sqlx::query_scalar("SELECT smart_playlist_key FROM smart_playlists WHERE object_id=?1")
                .bind(object_id)
                .fetch_optional(&mut *connection)
                .await?,
        )
    }

    pub async fn smart_playlist_rows(
        &self,
        source: Option<SourceKey>,
        keys: &[SmartPlaylistKey],
        folder: Option<FolderKey>,
        now: i64,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<SmartPlaylistRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let result = load_smart_playlist_requested(
            &mut transaction,
            source,
            crate::query::RequestedKeys::batch(keys)?,
            folder,
            now,
        )
        .await?;
        transaction.commit().await?;
        Ok(result)
    }

    pub async fn random_smart_playlist_artwork(
        &self,
        key: SmartPlaylistKey,
        source: Option<SourceKey>,
        folder: Option<FolderKey>,
        now: i64,
    ) -> LibraryResult<Vec<Vec<u8>>> {
        let mut connection = self.acquire_reader().await?;
        let policy =
            smart_membership_sql(&mut connection, source, folder, now, Some(&[key])).await?;
        let choices =
            crate::artwork::playlist_artwork_choices_on(&mut connection, &policy, &[], true)
                .await?;
        crate::artwork::raw_playlist_artwork_on(&mut connection, &choices)
            .await?
            .iter()
            .map(|recipe| serde_json::to_vec(recipe).map_err(Into::into))
            .collect()
    }

    pub async fn create_smart_playlist(
        &self,
        name: &str,
        definition: &SmartPlaylistDefinition,
    ) -> LibraryResult<SmartPlaylistKey> {
        let name = require_name(name)?;
        let definition = encode_definition(definition)?;
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let result = sqlx::query(
            "INSERT INTO smart_playlists(
                 object_id, name, normalized_name,definition_json, position
             ) VALUES (
                 'rufin:smart:' || lower(hex(randomblob(16))),
                 ?1, lower(?1), ?2,
                 (SELECT COALESCE(max(position) + 1, 0) FROM smart_playlists)
             )",
        )
        .bind(name)
        .bind(definition)
        .execute(&mut *transaction)
        .await?;
        let key = SmartPlaylistKey::from_raw(result.last_insert_rowid());
        sync_smart_on(&mut transaction, key).await?;
        transaction.commit().await?;
        Ok(key)
    }

    pub async fn update_smart_playlist(
        &self,
        key: SmartPlaylistKey,
        name: &str,
        definition: &SmartPlaylistDefinition,
        artwork: Option<Option<&[u8]>>,
    ) -> LibraryResult<bool> {
        let name = require_name(name)?;
        let definition = encode_definition(definition)?;
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let revision = artwork
            .flatten()
            .map(|bytes| blake3::hash(bytes).to_hex().to_string());
        let binding =
            if artwork.is_some() {
                let (object, previous): (String, Option<Vec<u8>>) = sqlx::query_as(
                "SELECT object_id,artwork_binding FROM smart_playlists WHERE smart_playlist_key=?1",
            ).bind(key).fetch_one(&mut *transaction).await?;
                let incoming = revision.as_ref().map(|revision| {
                    crate::artwork::playlist_binding(true, None, object, revision.clone())
                });
                crate::artwork::merge_playlist_artwork(previous.as_deref(), incoming.as_deref())?
            } else {
                None
            };
        let changed = sqlx::query(
            "UPDATE smart_playlists
             SET name=?2, normalized_name=lower(?2), definition_json=?3,
                 artwork_bytes=CASE WHEN ?4 THEN ?5 ELSE artwork_bytes END,
                 artwork_revision=CASE WHEN ?4 THEN ?6 ELSE artwork_revision END, artwork_binding=CASE WHEN ?4 THEN ?7 ELSE artwork_binding END
             WHERE smart_playlist_key=?1",
        )
        .bind(key)
        .bind(name)
        .bind(definition)
        .bind(artwork.is_some())
        .bind(artwork.flatten())
        .bind(revision).bind(binding)
        .execute(&mut *transaction)
        .await?
        .rows_affected()
            == 1;
        if changed {
            sync_smart_on(&mut transaction, key).await?;
        }
        transaction.commit().await?;
        Ok(changed)
    }

    pub async fn delete_smart_playlist(&self, key: SmartPlaylistKey) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let identity: Option<String> = sqlx::query_scalar(
            "DELETE FROM smart_playlists WHERE smart_playlist_key=?1 RETURNING object_id",
        )
        .bind(key)
        .fetch_optional(&mut *transaction)
        .await?;
        if let Some(identity) = &identity {
            crate::profile::write_local_on(
                &mut transaction,
                &[crate::ConnectRecord {
                    kind: "smart".into(),
                    key: identity.clone(),
                    value: None,
                }],
            )
            .await?;
        }
        transaction.commit().await?;
        Ok(identity.is_some())
    }

    pub async fn move_smart_playlist(
        &self,
        dragged: SmartPlaylistKey,
        target: SmartPlaylistKey,
    ) -> LibraryResult<bool> {
        if dragged == target {
            return Ok(false);
        }
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let (dragged_position,target_position): (Option<i64>,Option<i64>)=sqlx::query_as(
            "SELECT(SELECT position FROM smart_playlists WHERE smart_playlist_key=?1),(SELECT position FROM smart_playlists WHERE smart_playlist_key=?2)"
        ).bind(dragged).bind(target).fetch_one(&mut *transaction).await?;
        let (Some(dragged_position), Some(target_position)) = (dragged_position, target_position)
        else {
            transaction.rollback().await?;
            return Ok(false);
        };
        let mut order:Vec<(SmartPlaylistKey,String,i64)>=sqlx::query_as(
            "SELECT smart_playlist_key,object_id,position FROM smart_playlists WHERE position BETWEEN ?1 AND ?2 ORDER BY position"
        ).bind(dragged_position.min(target_position)).bind(dragged_position.max(target_position)).fetch_all(&mut *transaction).await?;
        let positions = order.iter().map(|row| row.2).collect::<Vec<_>>();
        if dragged_position < target_position {
            order.rotate_left(1);
        } else {
            order.rotate_right(1);
        }
        let final_positions = order
            .iter()
            .zip(&positions)
            .map(|(row, position)| (row.0, *position))
            .collect::<Vec<_>>();
        write_positions_on(&mut transaction, &final_positions).await?;
        let records = order
            .into_iter()
            .zip(positions)
            .map(|((_, key, _), position)| crate::ConnectRecord {
                kind: "smart_order".into(),
                key,
                value: Some(serde_json::json!({"position":position})),
            })
            .collect::<Vec<_>>();
        crate::profile::write_local_on(&mut transaction, &records).await?;
        transaction.commit().await?;
        Ok(true)
    }
}

/// Store accepted playlist positions.
pub(crate) async fn write_positions_on(
    connection: &mut SqliteConnection,
    positions: &[(SmartPlaylistKey, i64)],
) -> LibraryResult<bool> {
    let mut changed = false;
    for page in positions.chunks(SMART_PLAYLIST_ROW_LIMIT) {
        let rows = serde_json::to_string(page)?;
        changed |= sqlx::query(
            "WITH input AS MATERIALIZED(SELECT json_extract(value,'$[0]') id,json_extract(value,'$[1]') position FROM json_each(?1)),
             changed AS MATERIALIZED(SELECT input.* FROM input JOIN smart_playlists old ON old.smart_playlist_key=input.id WHERE old.position<>input.position),
             affected AS MATERIALIZED(SELECT old.smart_playlist_key id,old.position FROM changed input JOIN smart_playlists old ON old.smart_playlist_key=input.id OR old.position=input.position),
             moved AS MATERIALIZED(SELECT id,(SELECT coalesce(max(position),-1)+1 FROM smart_playlists)+row_number() OVER(ORDER BY position) position FROM (SELECT DISTINCT id,position FROM affected))
             UPDATE smart_playlists SET position=moved.position FROM moved WHERE smart_playlists.smart_playlist_key=moved.id"
        ).bind(&rows).execute(&mut *connection).await?.rows_affected()>0;
        changed |= sqlx::query(
            "WITH input AS(SELECT json_extract(value,'$[0]') id,json_extract(value,'$[1]') position FROM json_each(?1))
             UPDATE smart_playlists SET position=input.position FROM input WHERE smart_playlists.smart_playlist_key=input.id AND smart_playlists.position<>input.position"
        ).bind(rows).execute(&mut *connection).await?.rows_affected()>0;
    }
    Ok(changed)
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
        let valid = matches!(
            (rule.field.value_kind(rule.operator), &rule.value),
            (Some(SmartPlaylistRuleValueKind::None), None)
                | (
                    Some(SmartPlaylistRuleValueKind::Text),
                    Some(SmartPlaylistRuleValue::Text(_))
                )
                | (
                    Some(SmartPlaylistRuleValueKind::Number),
                    Some(SmartPlaylistRuleValue::Number(_))
                )
                | (
                    Some(SmartPlaylistRuleValueKind::NumberRange),
                    Some(SmartPlaylistRuleValue::NumberRange { .. })
                )
                | (
                    Some(SmartPlaylistRuleValueKind::Date),
                    Some(SmartPlaylistRuleValue::Date(_))
                )
                | (
                    Some(SmartPlaylistRuleValueKind::DateRange),
                    Some(SmartPlaylistRuleValue::DateRange { .. })
                )
                | (
                    Some(SmartPlaylistRuleValueKind::Bool),
                    Some(SmartPlaylistRuleValue::Bool(_))
                )
        );
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

fn sql_text(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(test)]
mod source_window_tests {
    use super::*;

    #[tokio::test]
    async fn recent_played_members_and_row_hydration_do_not_walk_unrelated_catalog() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(directory.path().join("store.sqlite"))
            .await
            .unwrap();
        let definition = SmartPlaylistDefinition {
            current: false,
            match_all: vec![SmartPlaylistRule {
                field: SmartPlaylistRuleField::PlayCount,
                operator: SmartPlaylistRuleOperator::Above,
                value: Some(SmartPlaylistRuleValue::Number(0)),
            }],
            activity_period: SmartPlaylistActivityPeriod::Weekly,
            sort_field: SmartPlaylistSort::PlayCount,
            descending: true,
            ..Default::default()
        };
        let key = database
            .create_smart_playlist("Recent", &definition)
            .await
            .unwrap();
        let mut writer = database.writer().await.unwrap();
        let connection = writer.as_mut().unwrap();
        sqlx::raw_sql("INSERT INTO sources(source_key,object_id,display_name,normalized_name,artwork_digest) VALUES (1,'source','Source','source',zeroblob(32));\n          INSERT INTO tracks(track_key,indexed,source_key,object_id,media_uri,metadata) VALUES (1,1,1,'one','track:1',jsonb(json_object('title',COALESCE('One',''),'album',COALESCE('',''),'artist',COALESCE('',''),'sort_text',COALESCE('one',''),'duration_millis',COALESCE(1000,0))));\n          INSERT INTO listens(media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis)\n          VALUES('track:1','One','','',1000000,'1970-01',1000,1000),\n                ('https://example.test/missing','Missing','','',395200,'1970-01',1000,1000),\n                ('https://example.test/old','Old','','',395199,'1970-01',1000,1000),\n                ('https://example.test/future','Future','','',1000001,'1970-01',1000,1000);")
            .execute(&mut *connection).await.unwrap();
        for grow in [false, true] {
            if grow {
                sqlx::raw_sql("WITH RECURSIVE n(x) AS (VALUES(2) UNION ALL SELECT x+1 FROM n WHERE x<10000)\n                  INSERT INTO tracks(track_key,indexed,source_key,object_id,media_uri,metadata) SELECT x,1,1,CAST(x AS TEXT),'track:'||x,jsonb(json_object('title',COALESCE('Title',''),'album',COALESCE('',''),'artist',COALESCE('',''),'sort_text',COALESCE('title',''),'duration_millis',COALESCE(1000,0))) FROM n;")
                    .execute(&mut *connection).await.unwrap();
            }
            let sql = format!(
                "{} SELECT media_uri FROM selected ORDER BY {SMART_RESULT_ORDER}",
                smart_membership_sql(connection, None, None, 1000000, Some(&[key]))
                    .await
                    .unwrap()
            );
            let work = Arc::new(AtomicUsize::new(0));
            let counter = work.clone();
            connection
                .lock_handle()
                .await
                .unwrap()
                .set_progress_handler(100, move || {
                    counter.fetch_add(100, Ordering::Relaxed);
                    true
                });
            let order: Vec<String> = sqlx::query_scalar(AssertSqlSafe(sql))
                .fetch_all(&mut *connection)
                .await
                .unwrap();
            connection
                .lock_handle()
                .await
                .unwrap()
                .remove_progress_handler();
            assert_eq!(order, ["track:1"]);
            assert!(
                work.load(Ordering::Relaxed) < 5000,
                "recent membership visited unrelated catalog: {}",
                work.load(Ordering::Relaxed)
            );
        }
        // Download metadata is another logical view: loading one row must not
        // materialize every locator even when the catalog owner wins.
        sqlx::raw_sql(
            "INSERT INTO main.local_locators(media_uri,origin,path,root,relative_path,access_uri)
          SELECT media_uri,'download',object_id,'',object_id,media_uri FROM tracks;",
        )
        .execute(&mut *connection)
        .await
        .unwrap();
        let work = Arc::new(AtomicUsize::new(0));
        let counter = work.clone();
        connection
            .lock_handle()
            .await
            .unwrap()
            .set_progress_handler(100, move || {
                counter.fetch_add(100, Ordering::Relaxed);
                true
            });
        let rows = crate::tracks::load_track_rows(connection, &[crate::TrackKey::from_raw(1)])
            .await
            .unwrap();
        connection
            .lock_handle()
            .await
            .unwrap()
            .remove_progress_handler();
        assert_eq!(rows[0].metadata.title, "One");
        assert!(rows[0].access.is_downloaded);
        assert!(
            work.load(Ordering::Relaxed) < 5000,
            "row hydration visited unrelated downloads: {}",
            work.load(Ordering::Relaxed)
        );
    }
}

fn source_fact(
    field: SmartPlaylistRuleField,
    definition: &SmartPlaylistDefinition,
    legacy: bool,
) -> String {
    use SmartPlaylistRuleField as F;
    let period = if field == F::Played {
        None
    } else {
        definition.activity_period.seconds()
    };
    match field {
        F::Title => "owner.title".into(),
        F::Artist => "owner.display_artist".into(),
        F::Album => "owner.display_album".into(),
        F::Comment => "json_extract(owner.metadata,'$.comment')".into(),
        F::Bpm => "owner.bpm".into(),
        F::Year => "owner.year".into(),
        F::DateAdded => crate::tracks::added_at!("owner").into(),
        F::Favorite => "COALESCE((SELECT favorite FROM user_media_state WHERE media_uri=owner.media_uri),(SELECT favorite FROM native_media_state WHERE media_uri=owner.media_uri))".into(),
        F::Rating => "COALESCE((SELECT rating FROM user_media_state WHERE media_uri=owner.media_uri),(SELECT rating FROM native_media_state WHERE media_uri=owner.media_uri))/10".into(),
        F::Played | F::PlayCount | F::SkipCount | F::LastPlayed => {
            let column = match field {
                F::Played | F::PlayCount => "play_count",
                F::SkipCount => "skip_count",
                _ => "last_played_at"
            };
            let local = match field {
                F::Played | F::PlayCount if period.is_none() => "owner.local_play_count".into(),
                F::LastPlayed => "activity.last_played_at".into(),
                _ => format!("COALESCE(activity.{column},0)")
            };
            if period.is_some() || !legacy {
                return local;
            }
            if field == F::LastPlayed {
                format!("COALESCE(max({local},legacy.{column}),{local},legacy.{column})")
            } else {
                format!("({local}+COALESCE(legacy.{column},0))")
            }
        }
        F::Genre | F::Mood => unreachable!("relation rules compile at their owner")
    }
}

fn source_rule(
    rule: &SmartPlaylistRule,
    definition: &SmartPlaylistDefinition,
    legacy: bool,
) -> String {
    use SmartPlaylistRuleField as F;
    use SmartPlaylistRuleOperator as O;
    use SmartPlaylistRuleValue as V;
    if !rule.field.operators().contains(&rule.operator) {
        return "0".into();
    }
    if matches!(rule.field, F::Genre | F::Mood) {
        let (relation, table, key) = if rule.field == F::Genre {
            ("track_genres", "genres", "genre_key")
        } else {
            ("track_moods", "moods", "mood_key")
        };
        let exists = format!("EXISTS(SELECT 1 FROM {relation} WHERE track_key=owner.track_key)");
        if rule.operator == O::IsEmpty {
            return format!("NOT {exists}");
        }
        if rule.operator == O::IsNotEmpty {
            return exists;
        }
        let Some(V::Text(value)) = &rule.value else {
            return "0".into();
        };
        let value = sql_text(value.trim());
        let comparison = if matches!(rule.operator, O::Contains | O::NotContains) {
            format!("instr(lower(name),lower({value}))>0")
        } else {
            format!("lower(name)=lower({value})")
        };
        let matched = format!(
            "EXISTS(SELECT 1 FROM {relation} JOIN {table} USING({key}) WHERE track_key=owner.track_key AND {comparison})"
        );
        return if matches!(rule.operator, O::NotContains | O::NotEquals) {
            format!("({exists} AND NOT {matched})")
        } else {
            matched
        };
    }
    let mut fact = source_fact(rule.field, definition, legacy);
    if rule.field == F::Played {
        let Some(V::Bool(value)) = rule.value else {
            return "0".into();
        };
        let played = value != (rule.operator == O::IsNot);
        return format!("{fact}{}0", if played { ">" } else { "=" });
    }
    if matches!(rule.field, F::LastPlayed | F::DateAdded) {
        fact = format!("date({fact},'unixepoch')");
    }
    let text = matches!(rule.field, F::Title | F::Artist | F::Album | F::Comment);
    let date = matches!(rule.field, F::LastPlayed | F::DateAdded);
    if matches!(rule.operator, O::IsEmpty | O::IsNotEmpty) {
        let empty = if text {
            format!("({fact} IS NULL OR trim({fact})='')")
        } else if date {
            format!("({fact} IS NULL OR {fact}='')")
        } else {
            format!("({fact} IS NULL)")
        };
        return if rule.operator == O::IsEmpty {
            empty
        } else {
            format!("NOT {empty}")
        };
    }
    let value = match &rule.value {
        Some(V::Text(value)) if text => sql_text(value.trim()),
        Some(V::Date(value)) if date => sql_text(value.trim()),
        Some(V::Number(value)) => value.to_string(),
        Some(V::Bool(value)) => i32::from(*value).to_string(),
        Some(V::NumberRange { min, max }) if rule.operator == O::Between => {
            return format!("{fact} BETWEEN {} AND {}", min.min(max), min.max(max));
        }
        Some(V::DateRange { start, end }) if rule.operator == O::Between => {
            return format!(
                "{fact} BETWEEN {} AND {}",
                sql_text(start.trim().min(end.trim())),
                sql_text(start.trim().max(end.trim()))
            );
        }
        _ => return "0".into(),
    };
    if matches!(rule.operator, O::Contains | O::NotContains) {
        return format!(
            "instr(lower({fact}),lower({value})){}0",
            if rule.operator == O::Contains {
                ">"
            } else {
                "="
            }
        );
    }
    let operator = match rule.operator {
        O::Equals | O::Is => "=",
        O::NotEquals | O::IsNot => "<>",
        O::Above | O::After => ">",
        O::Below | O::Before => "<",
        _ => return "0".into(),
    };
    if text {
        format!("lower({fact}){operator}lower({value})")
    } else {
        format!("{fact}{operator}{value}")
    }
}

fn source_rules(definition: &SmartPlaylistDefinition, legacy: bool) -> String {
    let all = definition
        .match_all
        .iter()
        .map(|rule| format!("({})", source_rule(rule, definition, legacy)))
        .collect::<Vec<_>>();
    let any = definition
        .match_any
        .iter()
        .map(|rule| format!("({})", source_rule(rule, definition, legacy)))
        .collect::<Vec<_>>();
    format!(
        "({}) AND ({})",
        if all.is_empty() {
            "1".into()
        } else {
            all.join(" AND ")
        },
        if any.is_empty() {
            "1".into()
        } else {
            any.join(" OR ")
        }
    )
}

fn source_sort(definition: &SmartPlaylistDefinition, legacy: bool) -> String {
    use SmartPlaylistRuleField as F;
    use SmartPlaylistSort as S;
    let field = match definition.sort_field {
        S::Title => return "owner.sort_text".into(),
        S::Duration => return "owner.duration_millis".into(),
        S::Artist => F::Artist,
        S::Album => F::Album,
        S::Year => F::Year,
        S::DateAdded => F::DateAdded,
        S::LastPlayed => F::LastPlayed,
        S::PlayCount => F::PlayCount,
        S::SkipCount => F::SkipCount,
        S::Bpm => F::Bpm,
        S::Rating => F::Rating,
    };
    let fact = source_fact(field, definition, legacy);
    if field == F::Rating {
        fact.strip_suffix("/10").unwrap().to_string()
    } else {
        fact
    }
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

pub(crate) async fn smart_membership_sql(
    connection: &mut SqliteConnection,
    source: Option<SourceKey>,
    folder: Option<FolderKey>,
    now: i64,
    keys: Option<&[SmartPlaylistKey]>,
) -> LibraryResult<String> {
    let keys_json = keys.map(serde_json::to_string).transpose()?;
    let definitions = sqlx::query_as::<_, (i64, String)>(
        "SELECT smart_playlist_key,definition_json FROM smart_playlists
         WHERE ?1 IS NULL OR smart_playlist_key IN(SELECT value FROM json_each(?1)) ORDER BY smart_playlist_key"
    ).bind(&keys_json).fetch_all(&mut *connection).await?
        .into_iter().map(|(key,json)| Ok((key,serde_json::from_str::<SmartPlaylistDefinition>(&json)?)))
        .collect::<LibraryResult<Vec<_>>>()?;
    smart_membership_from_definitions(connection, source, folder, now, &definitions).await
}

async fn smart_membership_from_definitions(
    connection: &mut SqliteConnection,
    source: Option<SourceKey>,
    folder: Option<FolderKey>,
    now: i64,
    definitions: &[(i64, SmartPlaylistDefinition)],
) -> LibraryResult<String> {
    let has_legacy = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM legacy_activity WHERE period='lifetime' AND item_kind='track')",
    ).fetch_one(&mut *connection).await?;
    let admitted = format!(
        "smart_playlist_key IN (SELECT value FROM json_each({}))",
        sql_text(&serde_json::to_string(
            &definitions.iter().map(|(key, _)| key).collect::<Vec<_>>()
        )?)
    );
    let mut sql = format!(
        "WITH definitions AS (SELECT smart_playlist_key definition_key,position,normalized_name,COALESCE(json_extract(definition_json,'$.current'),0) current_scope FROM smart_playlists WHERE {admitted})"
    );
    let mut members = Vec::new();
    let mut periods = std::collections::BTreeSet::new();
    let mut legacy_used = false;
    for (key, definition) in definitions {
        use SmartPlaylistRuleField as F;
        use SmartPlaylistRuleOperator as O;
        use SmartPlaylistRuleValue as V;
        use SmartPlaylistSort as S;
        let period = definition.activity_period.seconds();
        let sort_field = match definition.sort_field {
            S::PlayCount => Some(F::PlayCount),
            S::SkipCount => Some(F::SkipCount),
            S::LastPlayed => Some(F::LastPlayed),
            _ => None,
        };
        let mut activity = false;
        let mut legacy = false;
        for field in definition
            .match_all
            .iter()
            .chain(&definition.match_any)
            .map(|rule| rule.field)
            .chain(sort_field)
        {
            activity |= matches!(field, F::SkipCount | F::LastPlayed)
                || (field == F::PlayCount && period.is_some());
            legacy |= has_legacy
                && (field == F::Played
                    || (period.is_none()
                        && matches!(field, F::PlayCount | F::SkipCount | F::LastPlayed)));
        }
        legacy_used |= legacy;
        if activity {
            periods.insert(period);
        }

        let mut scope = "owner.indexed=1".to_string();
        if definition.current {
            scope.push_str(&format!(
                " AND owner.source_key={}",
                source
                    .map(|key| key.raw().to_string())
                    .unwrap_or_else(|| "NULL".into())
            ));
            if let Some(folder) = folder {
                scope.push_str(&format!(" AND EXISTS(SELECT 1 FROM track_folders WHERE track_key=owner.track_key AND folder_key={})",folder.raw()));
            }
        }
        let positive = definition.match_all.iter().find_map(|rule| {
            match (rule.field, rule.operator, &rule.value) {
                (field @ (F::PlayCount | F::SkipCount), O::Above, Some(V::Number(min)))
                    if *min >= 0 =>
                {
                    Some(field)
                }
                (F::Played, O::Is, Some(V::Bool(true)))
                | (F::Played, O::IsNot, Some(V::Bool(false))) => Some(F::Played),
                _ => None,
            }
        });
        let period_name = format!("activity_{}", period.unwrap_or(0));
        let activity_first =
            period.is_some() && matches!(positive, Some(F::PlayCount | F::SkipCount));
        let mut relation = if activity_first {
            format!("{period_name} activity CROSS JOIN tracks owner USING(media_uri)")
        } else if let Some(field) = positive.filter(|field| *field == F::SkipCount || legacy) {
            let (candidates, column) = if field == F::SkipCount {
                (
                    format!(
                        "SELECT track_key FROM {period_name} CROSS JOIN tracks USING(media_uri) WHERE skip_count>0"
                    ),
                    "skip_count",
                )
            } else {
                (
                    "SELECT track_key FROM tracks WHERE indexed=1 AND local_play_count>0".into(),
                    "play_count",
                )
            };
            let baseline = if legacy {
                format!(" UNION SELECT track_key FROM legacy WHERE {column}>0")
            } else {
                String::new()
            };
            format!("({candidates}{baseline}) played CROSS JOIN tracks owner USING(track_key)")
        } else if activity && definition.match_all.is_empty() && definition.match_any.is_empty() {
            // Ranking needs only the indexed key, URI and sort fields before the limit.
            let index = if definition.current {
                "tracks_local_play_order_idx"
            } else {
                "tracks_global_play_order_idx"
            };
            format!("tracks owner INDEXED BY {index}")
        } else {
            "tracks owner".into()
        };
        if activity && !activity_first {
            relation.push_str(&format!(
                " LEFT JOIN {period_name} activity USING(media_uri)"
            ));
        }
        if legacy {
            relation.push_str(" LEFT JOIN legacy USING(track_key)");
        }
        let sort = source_sort(definition, legacy);
        let rules = source_rules(definition, legacy);
        let descending = i32::from(definition.descending);
        let direction = if definition.descending { "DESC" } else { "ASC" };
        let membership_order = definition.limit.map(|limit| {
            format!(" ORDER BY sort_value {direction} NULLS LAST,owner.sort_text,owner.media_uri LIMIT {limit}")
        }).unwrap_or_default();
        sql.push_str(&format!(
            ",result_{key} AS NOT MATERIALIZED (SELECT {key} definition_key,owner.track_key,owner.media_uri,{descending} descending,owner.sort_text,{sort} sort_value FROM {relation} WHERE {scope} AND ({rules}){membership_order})"
        ));
        members.push(format!("SELECT * FROM result_{key}"));
    }
    for period in periods {
        let lower = period
            .map(|seconds| format!(" AND started_at>={}", now - seconds))
            .unwrap_or_default();
        sql.push_str(&format!(",activity_{} AS MATERIALIZED (
            SELECT media_uri,count(*) play_count,sum(skipped) skip_count,max(started_at) last_played_at
            FROM listens WHERE started_at<={now}{lower} GROUP BY media_uri)", period.unwrap_or(0)));
    }
    if legacy_used {
        sql.push_str(",legacy AS MATERIALIZED (
            SELECT track.track_key,old.play_count,old.skip_count,old.last_played_at
            FROM legacy_activity old JOIN sources source ON source.object_id=old.source_id
            CROSS JOIN tracks track ON track.source_key=source.source_key AND track.object_id=old.track_object_id
            WHERE old.period='lifetime' AND old.item_kind='track')");
    }
    if members.is_empty() {
        members.push("SELECT NULL definition_key,NULL track_key,NULL media_uri,0 descending,NULL sort_text,NULL sort_value WHERE 0".into());
    }
    sql.push_str(&format!(
        ",selected AS NOT MATERIALIZED ({})",
        members.join(" UNION ALL ")
    ));
    Ok(sql)
}

const SMART_RESULT_ORDER: &str =
    "CASE WHEN selected.descending=0 THEN selected.sort_value END ASC NULLS LAST,
    CASE WHEN selected.descending=1 THEN selected.sort_value END DESC NULLS LAST,
    selected.sort_text,selected.media_uri";

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct SmartPlaylistWrite {
    #[serde(default)]
    pub artwork_bytes: Option<Vec<u8>>,
    #[serde(default)]
    pub artwork_binding: Option<Vec<u8>>,
    pub object_id: String,
    pub name: String,
    pub definition_json: String,
    pub position: i64,
}

pub(crate) async fn export_smart_playlists_jsonl_on(
    connection: &mut SqliteConnection,
    mut output: impl std::io::Write,
) -> LibraryResult<u64> {
    output.write_all(b"{\"version\":1}\n")?;
    let mut rows = sqlx::query_as::<_, SmartPlaylistWrite>(
        "SELECT object_id,name,definition_json,position,artwork_bytes,artwork_binding FROM smart_playlists ORDER BY position",
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
pub(crate) async fn import_smart_playlists_jsonl_on(
    connection: &mut SqliteConnection,
    input: impl std::io::BufRead,
) -> LibraryResult<u64> {
    let mut lines = input.lines();
    let header: serde_json::Value =
        serde_json::from_str(&lines.next().transpose()?.unwrap_or_default())?;
    if header.get("version").and_then(serde_json::Value::as_u64) != Some(1) {
        return Err(crate::LibraryError::InvalidRequest(
            "unsupported Smart playlist version".into(),
        ));
    }
    let mut count = 0;
    for line in lines {
        let record = serde_json::from_str::<SmartPlaylistWrite>(&line?)?;
        write_smart_playlist(connection, &record).await?;
        count += 1;
    }
    Ok(count)
}

pub(crate) async fn write_smart_playlist(
    connection: &mut SqliteConnection,
    record: &SmartPlaylistWrite,
) -> LibraryResult<()> {
    if record.object_id.is_empty() || record.name.trim().is_empty() || record.position < 0 {
        return Err(crate::LibraryError::InvalidRequest(
            "invalid Smart playlist identity or order".into(),
        ));
    }
    let _: SmartPlaylistDefinition = serde_json::from_str(&record.definition_json)?;
    let revision = record
        .artwork_bytes
        .as_ref()
        .map(|bytes| blake3::hash(bytes).to_hex().to_string());
    let previous: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT artwork_binding FROM smart_playlists WHERE object_id=?1")
            .bind(&record.object_id)
            .fetch_optional(&mut *connection)
            .await?
            .flatten();
    let incoming = record
        .artwork_binding
        .as_deref()
        .map(crate::normalize_artwork_binding)
        .transpose()?
        .or_else(|| {
            revision.as_ref().map(|revision| {
                crate::artwork::playlist_binding(
                    true,
                    None,
                    record.object_id.clone(),
                    revision.clone(),
                )
            })
        });
    let binding = crate::artwork::merge_playlist_artwork(previous.as_deref(), incoming.as_deref())?;
    let key = sqlx::query_scalar("INSERT INTO smart_playlists(object_id,name,normalized_name,definition_json,position,artwork_bytes,artwork_revision,artwork_binding)
       VALUES(?1,?2,lower(?2),?3,CASE WHEN EXISTS(SELECT 1 FROM smart_playlists WHERE position=?4)
       THEN (SELECT COALESCE(max(position)+1,0) FROM smart_playlists) ELSE ?4 END,?5,?6,?7)
       ON CONFLICT(object_id) DO UPDATE SET name=excluded.name,normalized_name=excluded.normalized_name,
       definition_json=excluded.definition_json,artwork_bytes=excluded.artwork_bytes,artwork_revision=excluded.artwork_revision,artwork_binding=excluded.artwork_binding RETURNING smart_playlist_key")
       .bind(&record.object_id).bind(&record.name).bind(&record.definition_json).bind(record.position)
       .bind(&record.artwork_bytes).bind(revision).bind(binding)
       .fetch_one(&mut *connection).await?;
    sync_smart_on(connection, key).await
}

impl Database {
    pub async fn export_smart_playlist_file(
        &self,
        key: SmartPlaylistKey,
        source: Option<SourceKey>,
        folder: Option<FolderKey>,
        now: i64,
        file: &std::path::Path,
        mode: crate::PlaylistPathMode,
        output: impl std::io::Write,
    ) -> LibraryResult<u64> {
        use futures_util::TryStreamExt;
        let (_export, mut connection) = self.acquire_export().await?;
        let mut output =
            crate::playlist_format::PlaylistWriter::new(output, file, mode, None, None)?;
        let query = smart_query(&mut connection, source, key, folder, "", None, false, now).await?;
        let rows = query.select(&format!(
            "track.*,row_number() OVER(ORDER BY {}) result_position",
            query.order.join(",")
        ));
        let sql = format!(
            "WITH export_rows AS ({rows}) SELECT 'm3u:' || result_position object_id,media_uri,json(metadata) metadata,result_position-1 position FROM export_rows ORDER BY result_position"
        );
        let mut rows = sqlx::query_as_with::<_, crate::PlaylistEntryWrite, _>(
            AssertSqlSafe(sql),
            query.arguments.clone(),
        )
        .persistent(false)
        .fetch(&mut *connection);
        while let Some(entry) = rows.try_next().await? {
            output.entry(&entry)?;
        }
        output.finish()
    }
}

pub(crate) async fn smart_parent_query(
    connection: &mut SqliteConnection,
    source: Option<SourceKey>,
    folder: Option<FolderKey>,
    now: i64,
    requested: crate::query::RequestedKeys,
) -> LibraryResult<crate::query::OrderedQuery> {
    let definitions=sqlx::query_as_with::<_,(i64,String),_>(AssertSqlSafe(format!(
        "SELECT playlist.smart_playlist_key,playlist.definition_json FROM ({}) requested JOIN smart_playlists playlist ON playlist.smart_playlist_key=requested.value ORDER BY requested.ordinal",requested.sql
    )),requested.arguments.clone()).fetch_all(&mut *connection).await?.into_iter().map(|(key,json)| Ok((key,serde_json::from_str(&json)?))).collect::<LibraryResult<Vec<_>>>()?;
    let membership =
        smart_membership_from_definitions(connection, source, folder, now, &definitions).await?;
    Ok(crate::query::OrderedQuery {
        key: "track.track_key".into(),
        text: None,
        from: format!(
            "({membership} SELECT selected.*,parent.ordinal FROM selected JOIN ({}) parent ON parent.value=selected.definition_key) selected JOIN tracks track USING(track_key)",
            requested.sql
        ),
        predicate: "1".into(),
        order: vec!["selected.ordinal".into(), SMART_RESULT_ORDER.into()],
        entry_key: "NULL".into(),
        arguments: requested.arguments,
    })
}

pub(crate) async fn smart_query(
    connection: &mut SqliteConnection,
    source: Option<SourceKey>,
    key: SmartPlaylistKey,
    folder: Option<FolderKey>,
    filter: &str,
    sort: Option<crate::CatalogSort>,
    descending: bool,
    now: i64,
) -> LibraryResult<crate::query::OrderedQuery> {
    let membership = smart_membership_sql(connection, source, folder, now, Some(&[key])).await?;
    let mut query = crate::query::OrderedQuery {
        key: "track.track_key".into(),
        text: sort.and_then(|sort| sort.section_text(false)),
        from: format!(
            "({membership} SELECT * FROM selected) selected JOIN tracks track USING(track_key)"
        ),
        predicate: "1".into(),
        order: sort
            .map(|sort| sort.order_terms(descending, false))
            .unwrap_or_else(|| vec![SMART_RESULT_ORDER.into()]),
        entry_key: "NULL".into(),
        arguments: Default::default(),
    };
    crate::tracks::track_filter(&mut query, None, filter, false);
    Ok(query)
}

pub(crate) async fn smart_list_query(
    connection: &mut SqliteConnection,
    source: Option<SourceKey>,
    folder: Option<FolderKey>,
    sort: CatalogSort,
    descending: bool,
    filter: &str,
    now: i64,
) -> LibraryResult<crate::query::OrderedQuery> {
    let text = (sort == CatalogSort::Title).then(|| "playlist.normalized_name".into());
    let field = match sort {
        CatalogSort::Position => "playlist.position",
        CatalogSort::Title => "playlist.normalized_name",
        CatalogSort::TrackCount => "playlist.track_count",
        CatalogSort::Duration => "playlist.duration_millis",
        _ => unreachable!("playlist sort field"),
    };
    let from = if folder.is_none() && matches!(sort, CatalogSort::Position | CatalogSort::Title) {
        "smart_playlists playlist".into()
    } else {
        let policy = smart_membership_sql(connection, source, folder, now, None).await?;
        format!(
            "({policy}, totals AS (SELECT definition_key,count(*) track_count,COALESCE(sum(track.duration_millis),0) duration_millis FROM selected JOIN tracks track INDEXED BY tracks_duration_idx USING(track_key) GROUP BY definition_key) SELECT definitions.*,definition_key smart_playlist_key,COALESCE(track_count,0) track_count,COALESCE(duration_millis,0) duration_millis FROM definitions LEFT JOIN totals USING(definition_key) WHERE current_scope=0 OR {} OR track_count>0) playlist",
            if folder.is_none() { "1" } else { "0" }
        )
    };
    let direction = if descending { "DESC" } else { "ASC" };
    Ok(crate::query::OrderedQuery {
        key: "playlist.smart_playlist_key".into(),
        text,
        from,
        predicate: format!(
            "instr(playlist.normalized_name,{})>0",
            crate::query::quote(&filter.trim().to_lowercase())
        ),
        order: vec![format!(
            "{field} {direction},playlist.position,playlist.smart_playlist_key"
        )],
        entry_key: "NULL".into(),
        arguments: Default::default(),
    })
}
