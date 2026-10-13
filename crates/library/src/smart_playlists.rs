//! Owns Smart Playlist definitions and direct SQL URI-result, row, and list queries.
//! One shared policy implements rule, Activity-window, limit, and ordering semantics.

use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use sqlx::{AssertSqlSafe, Connection, FromRow, Row, Sqlite, SqliteConnection};

use crate::{
    Database, FolderKey, LibraryError, LibraryResult, ReadCancellation, SmartPlaylistKey, SourceKey,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SmartPlaylistListSort {
    Position,
    Title,
    TrackCount,
    Duration,
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
    #[sqlx(skip)]
    pub artwork_bindings: Vec<Vec<u8>>,
    #[sqlx(skip)]
    pub artwork_binding: Option<Vec<u8>>,
    #[sqlx(skip)]
    pub representative_artwork: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SmartPlaylistValueSuggestions {
    pub genres: Vec<String>,
    pub moods: Vec<String>,
}

fn smart_list_order(sort: SmartPlaylistListSort, descending: bool) -> Option<&'static str> {
    match (sort, descending) {
        (SmartPlaylistListSort::Position, false) => Some("position,smart_playlist_key"),
        (SmartPlaylistListSort::Position, true) => Some("position DESC,smart_playlist_key"),
        (SmartPlaylistListSort::Title, false) => Some("normalized_name,smart_playlist_key"),
        (SmartPlaylistListSort::Title, true) => Some("normalized_name DESC,smart_playlist_key"),
        _ => None,
    }
}

pub(crate) async fn load_smart_playlist_rows(
    connection: &mut SqliteConnection,
    source: Option<SourceKey>,
    keys: &[SmartPlaylistKey],
    folder: Option<FolderKey>,
    now: i64,
) -> LibraryResult<Vec<SmartPlaylistRow>> {
    if keys.len() > SMART_PLAYLIST_ROW_LIMIT {
        return Err(LibraryError::InvalidRequest(format!(
            "Smart Playlist reads are limited to {SMART_PLAYLIST_ROW_LIMIT} keys"
        )));
    }
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let requested = serde_json::to_string(keys)?;
    let sql = format!(
        "{},cover_groups AS (
             SELECT selected.definition_key,COALESCE(album.media_uri,track.media_uri) cover_group,
                    count(*) track_count,COALESCE(sum(track.duration_millis),0) duration_millis,
                    count(CASE WHEN EXISTS(SELECT 1 FROM local_access_files access WHERE access.media_uri=track.media_uri AND access.origin='download') THEN 1 END) downloaded_count,
                    min(CASE WHEN COALESCE(track.artwork_binding,album.artwork_binding) IS NOT NULL THEN track.media_uri END) cover_uri
             FROM selected JOIN tracks track USING(track_key) LEFT JOIN albums album USING(album_key)
             GROUP BY selected.definition_key,cover_group
           ),summaries AS (
             SELECT definition_key,sum(track_count) OVER (PARTITION BY definition_key) track_count,
                    sum(duration_millis) OVER (PARTITION BY definition_key) duration_millis,
                    sum(downloaded_count) OVER (PARTITION BY definition_key) downloaded_count,
                    cover_uri,row_number() OVER (PARTITION BY definition_key ORDER BY cover_uri IS NULL,cover_group,cover_uri) cover_position
             FROM cover_groups
           ) SELECT request.key requested_ordinal,playlist.smart_playlist_key,
             playlist.object_id,playlist.name,playlist.definition_json,
             playlist.position,playlist.artwork_revision,
             COALESCE(summary.track_count,0) track_count,
             COALESCE(summary.duration_millis,0) duration_millis,
             COALESCE(summary.downloaded_count,0) downloaded_count,
             COALESCE(album.artwork_binding,cover.artwork_binding) cover_binding
           FROM json_each(?1) request
           JOIN smart_playlists playlist ON playlist.smart_playlist_key=request.value
           LEFT JOIN summaries summary ON summary.definition_key=request.value AND summary.cover_position<=4 AND (summary.cover_uri IS NOT NULL OR summary.cover_position=1)
           LEFT JOIN tracks cover ON cover.media_uri=summary.cover_uri
           LEFT JOIN albums album ON album.album_key=cover.album_key
           ORDER BY request.key,summary.cover_position",
        smart_membership_sql(connection, source, folder, now, Some(keys)).await?
    );
    let mut records = sqlx::query(AssertSqlSafe(sql))
        .persistent(false)
        .bind(requested)
        .fetch(&mut *connection);
    let mut result: Vec<SmartPlaylistRow> = Vec::new();
    let mut ordinal = None;
    while let Some(record) = records.try_next().await? {
        let current = record.try_get::<i64, _>("requested_ordinal")?;
        if ordinal != Some(current) {
            let mut row = SmartPlaylistRow::from_row(&record)?;
            normalize_definition(&mut row.definition)?;
            row.artwork_binding = record
                .try_get::<Option<String>, _>("artwork_revision")?
                .map(|revision| {
                    serde_json::to_vec(&crate::PlaylistArtworkBinding {
                        smart: true,
                        source_id: None,
                        object_id: row.object_id.clone(),
                        revision,
                    })
                    .expect("playlist artwork binding")
                });
            row.artwork_bindings = row.artwork_binding.iter().cloned().collect();
            result.push(row);
            ordinal = Some(current);
        }
        if let Some(binding) = record.try_get::<Option<Vec<u8>>, _>("cover_binding")? {
            let row = result.last_mut().unwrap();
            if row.artwork_binding.is_none() {
                row.artwork_bindings.push(binding.clone());
            }
            row.representative_artwork.push(binding);
        }
    }
    Ok(result)
}

impl Database {
    pub async fn ensure_default_smart_playlists(&self) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let mut changed = false;
        for (object_id, name, definition) in default_smart_playlists() {
            changed |= sqlx::query(
                "INSERT INTO smart_playlists(
                     object_id, name, normalized_name,definition_json, position
                 ) SELECT ?1, ?2, lower(?2), ?3,
                          COALESCE(max(position) + 1, 0)
                   FROM smart_playlists WHERE true
                 ON CONFLICT(object_id) DO NOTHING",
            )
            .bind(object_id)
            .bind(name)
            .bind(encode_definition(&definition)?)
            .execute(&mut *transaction)
            .await?
            .rows_affected()
                == 1;
        }
        transaction.commit().await?;
        Ok(changed)
    }

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
        let result = load_smart_playlist_rows(&mut transaction, source, keys, folder, now).await?;
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
        Ok(sqlx::query_scalar(AssertSqlSafe(format!(
            "{policy} SELECT COALESCE(track.artwork_binding,album.artwork_binding) binding
             FROM selected JOIN tracks track USING(media_uri) LEFT JOIN albums album ON album.album_key=track.album_key
             WHERE binding IS NOT NULL GROUP BY binding ORDER BY random() LIMIT 4"
        ))).persistent(false).fetch_all(&mut *connection).await?)
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
        .execute(connection)
        .await?;
        Ok(SmartPlaylistKey::from_raw(result.last_insert_rowid()))
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
        Ok(sqlx::query(
            "UPDATE smart_playlists
             SET name=?2, normalized_name=lower(?2), definition_json=?3,
                 artwork_bytes=CASE WHEN ?4 THEN ?5 ELSE artwork_bytes END,
                 artwork_revision=CASE WHEN ?4 THEN ?6 ELSE artwork_revision END
             WHERE smart_playlist_key=?1",
        )
        .bind(key)
        .bind(name)
        .bind(definition)
        .bind(artwork.is_some())
        .bind(artwork.flatten())
        .bind(
            artwork
                .flatten()
                .map(|bytes| blake3::hash(bytes).to_hex().to_string()),
        )
        .execute(connection)
        .await?
        .rows_affected()
            == 1)
    }

    pub async fn delete_smart_playlist(&self, key: SmartPlaylistKey) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(
            sqlx::query("DELETE FROM smart_playlists WHERE smart_playlist_key=?1")
                .bind(key)
                .execute(connection)
                .await?
                .rows_affected()
                == 1,
        )
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
        let positions = sqlx::query_as::<_, (Option<i64>, Option<i64>, Option<i64>)>(
            "SELECT
               (SELECT position FROM smart_playlists WHERE smart_playlist_key=?1),
               (SELECT position FROM smart_playlists WHERE smart_playlist_key=?2),
               (SELECT max(position) FROM smart_playlists)",
        )
        .bind(dragged)
        .bind(target)
        .fetch_one(&mut *transaction)
        .await?;
        let (Some(dragged_position), Some(target_position), Some(max_position)) = positions else {
            transaction.rollback().await?;
            return Ok(false);
        };
        let insertion = target_position;
        if insertion == dragged_position {
            transaction.rollback().await?;
            return Ok(false);
        }

        let temporary = max_position + 1;
        let offset = max_position + 2;
        sqlx::query(
            "UPDATE smart_playlists SET position=?2
             WHERE smart_playlist_key=?1",
        )
        .bind(dragged)
        .bind(temporary)
        .execute(&mut *transaction)
        .await?;
        if insertion < dragged_position {
            sqlx::query(
                "UPDATE smart_playlists SET position=position+?3
                 WHERE position>=?1 AND position<?2",
            )
            .bind(insertion)
            .bind(dragged_position)
            .bind(offset)
            .execute(&mut *transaction)
            .await?;
            sqlx::query(
                "UPDATE smart_playlists SET position=position-?3+1
                 WHERE position>=?1+?3 AND position<?2+?3",
            )
            .bind(insertion)
            .bind(dragged_position)
            .bind(offset)
            .execute(&mut *transaction)
            .await?;
        } else {
            sqlx::query(
                "UPDATE smart_playlists SET position=position+?3
                 WHERE position>?1 AND position<=?2",
            )
            .bind(dragged_position)
            .bind(insertion)
            .bind(offset)
            .execute(&mut *transaction)
            .await?;
            sqlx::query(
                "UPDATE smart_playlists SET position=position-?3-1
                 WHERE position>?1+?3 AND position<=?2+?3",
            )
            .bind(dragged_position)
            .bind(insertion)
            .bind(offset)
            .execute(&mut *transaction)
            .await?;
        }
        sqlx::query(
            "UPDATE smart_playlists SET position=?2
             WHERE smart_playlist_key=?1",
        )
        .bind(dragged)
        .bind(insertion)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(true)
    }
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
        sqlx::raw_sql("INSERT INTO sources(source_key,object_id,display_name,normalized_name,artwork_digest) VALUES (1,'source','Source','source',zeroblob(32));
          INSERT INTO tracks(track_key,indexed,source_key,object_id,title,normalized_search,display_album,display_artist,sort_text,duration_millis,media_uri)
          VALUES(1,1,1,'one','One','one','','','one',1000,'track:1');
          INSERT INTO listens(media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis)
          VALUES('track:1','One','','',1000000,'1970-01',1000,1000),
                ('https://example.test/missing','Missing','','',395200,'1970-01',1000,1000),
                ('https://example.test/old','Old','','',395199,'1970-01',1000,1000),
                ('https://example.test/future','Future','','',1000001,'1970-01',1000,1000);")
            .execute(&mut *connection).await.unwrap();
        for grow in [false, true] {
            if grow {
                sqlx::raw_sql("WITH RECURSIVE n(x) AS (VALUES(2) UNION ALL SELECT x+1 FROM n WHERE x<10000)
                  INSERT INTO tracks(track_key,indexed,source_key,object_id,title,normalized_search,display_album,display_artist,sort_text,duration_millis,media_uri)
                  SELECT x,1,1,CAST(x AS TEXT),'Title','title','','','title',1000,'track:'||x FROM n;")
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
        assert_eq!(rows[0].title, "One");
        assert!(rows[0].is_downloaded);
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
        F::Comment => "owner.comment".into(),
        F::Bpm => "owner.bpm".into(),
        F::Year => "owner.year".into(),
        F::DateAdded => crate::tracks::added_at!("owner").into(),
        F::Favorite => "COALESCE((SELECT favorite FROM user_media_state WHERE media_uri=owner.media_uri),owner.source_favorite)".into(),
        F::Rating => "COALESCE((SELECT rating FROM user_media_state WHERE media_uri=owner.media_uri),owner.source_rating)/10".into(),
        F::Played | F::PlayCount | F::SkipCount | F::LastPlayed => {
            let column = match field {
                F::Played | F::PlayCount => "play_count",
                F::SkipCount => "skip_count",
                _ => "last_played_at",
            };
            let local = match field {
                F::Played | F::PlayCount if period.is_none() => "owner.local_play_count".into(),
                F::LastPlayed => "activity.last_played_at".into(),
                _ => format!("COALESCE(activity.{column},0)"),
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
        F::Genre | F::Mood => unreachable!("relation rules compile at their owner"),
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
    let has_legacy = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM legacy_activity WHERE period='lifetime' AND item_kind='track')",
    ).fetch_one(&mut *connection).await?;
    let admitted = keys_json
        .as_deref()
        .map(|keys| {
            format!(
                "smart_playlist_key IN (SELECT value FROM json_each({}))",
                sql_text(keys)
            )
        })
        .unwrap_or_else(|| "1".into());
    let mut sql = format!(
        "WITH definitions AS (SELECT smart_playlist_key definition_key,position,normalized_name,COALESCE(json_extract(definition_json,'$.current'),0) current_scope FROM smart_playlists WHERE {admitted})"
    );
    let mut members = Vec::new();
    let mut periods = std::collections::BTreeSet::new();
    let mut legacy_used = false;
    for (key, definition) in &definitions {
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
        "SELECT object_id,name,definition_json,position,artwork_bytes FROM smart_playlists ORDER BY position",
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
    sqlx::query("INSERT INTO smart_playlists(object_id,name,normalized_name,definition_json,position,artwork_bytes,artwork_revision)
       VALUES(?1,?2,lower(?2),?3,CASE WHEN EXISTS(SELECT 1 FROM smart_playlists WHERE position=?4)
       THEN (SELECT COALESCE(max(position)+1,0) FROM smart_playlists) ELSE ?4 END,?5,?6)
       ON CONFLICT(object_id) DO UPDATE SET name=excluded.name,normalized_name=excluded.normalized_name,
       definition_json=excluded.definition_json,artwork_bytes=excluded.artwork_bytes,artwork_revision=excluded.artwork_revision")
       .bind(&record.object_id).bind(&record.name).bind(&record.definition_json).bind(record.position)
       .bind(&record.artwork_bytes).bind(record.artwork_bytes.as_ref().map(|bytes| blake3::hash(bytes).to_hex().to_string()))
       .execute(connection).await?;
    Ok(())
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
            "WITH export_rows AS ({rows}) SELECT 'm3u:' || result_position object_id,media_uri,title,display_artist artist,display_album album,album_display_artist,duration_millis,disc_number,track_number,year,release_date,source_format,musicbrainz_recording_id,musicbrainz_release_track_id,result_position-1 position FROM export_rows ORDER BY result_position"
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

pub(crate) async fn smart_query(
    connection: &mut SqliteConnection,
    source: Option<SourceKey>,
    key: SmartPlaylistKey,
    folder: Option<FolderKey>,
    filter: &str,
    sort: Option<crate::TrackSort>,
    descending: bool,
    now: i64,
) -> LibraryResult<crate::source_window::SourceQuery> {
    let membership = smart_membership_sql(connection, source, folder, now, Some(&[key])).await?;
    let mut query = crate::source_window::SourceQuery {
        from: format!(
            "({membership} SELECT * FROM selected) selected JOIN tracks track USING(track_key)"
        ),
        predicate: "1".into(),
        order: sort
            .map(|sort| sort.order_terms(descending, false))
            .unwrap_or_else(|| vec![SMART_RESULT_ORDER.into()]),
        uri: "track.media_uri".into(),
        entry_key: "NULL".into(),
        arguments: Default::default(),
    };
    crate::tracks::track_filter(&mut query, None, filter, false);
    Ok(query)
}

pub(crate) async fn load_smart_playlist_order<T>(
    connection: &mut SqliteConnection,
    source: Option<SourceKey>,
    folder: Option<FolderKey>,
    sort: SmartPlaylistListSort,
    descending: bool,
    filter: &str,
    now: i64,
    offset: usize,
    limit: usize,
    sections: bool,
) -> LibraryResult<Vec<T>>
where
    for<'r> T: sqlx::Decode<'r, Sqlite> + sqlx::Type<Sqlite> + Send + Unpin,
{
    let limit = if sections {
        usize::MAX
    } else {
        limit.min(SMART_PLAYLIST_ROW_LIMIT)
    };
    let filter = filter.trim();
    let keys = if let Some(order) = smart_list_order(sort, descending).filter(|_| folder.is_none())
    {
        let sql = format!(
            "SELECT smart_playlist_key FROM smart_playlists WHERE instr(normalized_name,lower(?1))>0 ORDER BY {order} LIMIT ?2 OFFSET ?3"
        );
        sqlx::query_scalar::<_, T>(AssertSqlSafe(if sections {
            crate::scroll_sections::section_sql(&sql, "normalized_name")
        } else {
            sql
        }))
        .bind(filter)
        .bind(if sections { i64::MAX } else { limit as i64 })
        .bind(offset.min(i64::MAX as usize) as i64)
        .fetch_all(&mut *connection)
        .await?
    } else {
        let policy = smart_membership_sql(&mut *connection, source, folder, now, None).await?;
        let column = match sort {
            SmartPlaylistListSort::Position => "position",
            SmartPlaylistListSort::Title => "normalized_name",
            SmartPlaylistListSort::TrackCount => "COALESCE(track_count,0)",
            SmartPlaylistListSort::Duration => "COALESCE(duration_millis,0)",
        };
        let direction = if descending { "DESC" } else { "ASC" };
        let (duration, members) = if sort == SmartPlaylistListSort::Duration {
            (
                ",COALESCE(sum(track.duration_millis),0) duration_millis",
                "selected JOIN tracks track USING(track_key)",
            )
        } else {
            ("", "selected")
        };
        let sql = format!(
            "{policy},totals AS (SELECT definition_key,count(*) track_count{duration} FROM {members} GROUP BY definition_key)
                SELECT definition_key FROM definitions LEFT JOIN totals USING(definition_key)
                WHERE (current_scope=0 OR ?1 IS NULL OR track_count>0)
                  AND instr(normalized_name,lower(?2))>0
                ORDER BY {column} {direction},position,definition_key LIMIT ?3 OFFSET ?4"
        );
        sqlx::query_scalar::<_, T>(AssertSqlSafe(if sections {
            crate::scroll_sections::section_sql(&sql, "normalized_name")
        } else {
            sql
        }))
        .persistent(false)
        .bind(folder)
        .bind(filter)
        .bind(if sections { i64::MAX } else { limit as i64 })
        .bind(offset.min(i64::MAX as usize) as i64)
        .fetch_all(&mut *connection)
        .await?
    };
    Ok(keys)
}
