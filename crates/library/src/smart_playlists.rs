//! Owns Smart Playlist definitions and direct SQL URI-result, row, and list queries.
//! One shared policy implements rule, Activity-window, limit, and ordering semantics.

use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqliteRow;
use sqlx::{AssertSqlSafe, Connection, FromRow, Row, SqliteConnection};

use crate::{
    Database, FolderKey, LibraryError, LibraryResult, ReadCancellation, RouteSeedWindow,
    SmartPlaylistKey, SourceKey,
};

const SMART_PLAYLIST_ROW_LIMIT: usize = 64;
const SMART_PLAYLIST_RULE_LIMIT: usize = 64;
const SMART_PLAYLIST_DEFINITION_BYTES: usize = 256 * 1024;
const MOST_PLAYED_OBJECT_ID: &str = "builtin:most_played";
const NEVER_PLAYED_OBJECT_ID: &str = "builtin:never_played";
pub(crate) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64)
}
const MOST_SKIPPED_OBJECT_ID: &str = "builtin:most_skipped";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SmartSourceReference {
    pub object_id: String,
    pub source_id: Option<crate::SourceId>,
    pub folder_object_id: Option<String>,
}

pub(crate) async fn smart_source_reference(
    connection: &mut SqliteConnection,
    key: SmartPlaylistKey,
    source: Option<SourceKey>,
    folder: Option<FolderKey>,
) -> LibraryResult<Option<SmartSourceReference>> {
    let Some(object_id) = sqlx::query_scalar::<_, String>(
        "SELECT object_id FROM smart_playlists WHERE smart_playlist_key=?1",
    )
    .bind(key)
    .fetch_optional(&mut *connection)
    .await?
    else {
        return Ok(None);
    };
    let source_id = if let Some(source) = source {
        let Some(id) =
            sqlx::query_scalar::<_, String>("SELECT object_id FROM sources WHERE source_key=?1")
                .bind(source)
                .fetch_optional(&mut *connection)
                .await?
        else {
            return Ok(None);
        };
        Some(crate::SourceId::new(id))
    } else {
        None
    };
    let folder_object_id = if let Some(folder) = folder {
        let Some(id) =
            sqlx::query_scalar::<_, String>("SELECT object_id FROM folders WHERE folder_key=?1")
                .bind(folder)
                .fetch_optional(&mut *connection)
                .await?
        else {
            return Ok(None);
        };
        Some(id)
    } else {
        None
    };
    Ok(Some(SmartSourceReference {
        object_id,
        source_id,
        folder_object_id,
    }))
}

pub(crate) async fn smart_members_ref(
    connection: &mut SqliteConnection,
    reference: &SmartSourceReference,
    now: i64,
    filter: &str,
    display_sort: Option<crate::TrackSort>,
    descending: bool,
) -> LibraryResult<Vec<String>> {
    let Some((key, source, folder)) = resolve_smart_reference(connection, reference).await? else {
        return Ok(Vec::new());
    };
    if let Some(sort) = display_sort {
        smart_sorted_uris_on(
            connection, source, key, folder, filter, sort, descending, now, None,
        )
        .await
    } else {
        smart_uri_page(connection, source, key, folder, filter, now, None).await
    }
}

async fn resolve_smart_reference(
    connection: &mut SqliteConnection,
    reference: &SmartSourceReference,
) -> LibraryResult<Option<(SmartPlaylistKey, Option<SourceKey>, Option<FolderKey>)>> {
    let Some((key,current))=sqlx::query_as::<_,(SmartPlaylistKey,bool)>("SELECT smart_playlist_key,COALESCE(json_extract(definition_json,'$.current'),0) FROM smart_playlists WHERE object_id=?1").bind(&reference.object_id).fetch_optional(&mut *connection).await? else {return Ok(None);};
    let source =
        sqlx::query_scalar::<_, SourceKey>("SELECT source_key FROM sources WHERE object_id=?1")
            .bind(reference.source_id.as_ref().map(crate::SourceId::as_str))
            .fetch_optional(&mut *connection)
            .await?;
    let folder = if current && let Some(id) = &reference.folder_object_id {
        let Some(folder) = sqlx::query_scalar::<_, FolderKey>(
            "SELECT folder_key FROM folders WHERE source_key=?1 AND object_id=?2",
        )
        .bind(source)
        .bind(id)
        .fetch_optional(&mut *connection)
        .await?
        else {
            return Ok(None);
        };
        Some(folder)
    } else {
        None
    };
    Ok(Some((key, source, folder)))
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct SmartPlaylistTrackRow {
    pub audio_properties: crate::AudioProperties,
    pub source_path: Option<String>,
    pub source_name: Option<String>,
    pub media_uri: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_media_uri: Option<String>,
    #[sqlx(skip)]
    pub artists: Vec<crate::TrackArtistLink>,
    #[sqlx(skip)]
    pub album_artists: Vec<crate::TrackArtistLink>,
    pub album_display_artist: Option<String>,
    pub artwork_binding: Option<Vec<u8>>,
    pub duration_millis: i64,
    pub disc_number: Option<i64>,
    pub track_number: Option<i64>,
    pub year: Option<i64>,
    pub release_date: Option<String>,
    pub date_added: Option<String>,
    pub source_format: Option<String>,
    pub musicbrainz_recording_id: Option<String>,
    pub musicbrainz_release_track_id: Option<String>,
    pub bpm: Option<i64>,
    pub genre: String,
    pub play_count: i64,
    pub last_played: Option<i64>,
    pub favorite: bool,
    pub rating: Option<i64>,
    pub is_downloaded: bool,
}

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
    pub limit: Option<usize>,
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

#[derive(Clone, Debug, PartialEq)]
pub struct SmartPlaylistRow {
    pub smart_playlist_key: SmartPlaylistKey,
    pub object_id: String,
    pub name: String,
    pub definition: SmartPlaylistDefinition,
    pub position: i64,
    pub track_count: i64,
    pub duration_millis: i64,
    pub downloaded_count: i64,
    pub artwork_bindings: Vec<Vec<u8>>,
    pub artwork_binding: Option<Vec<u8>>,
    pub representative_artwork: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SmartPlaylistValueSuggestions {
    pub genres: Vec<String>,
    pub moods: Vec<String>,
}

impl<'row> FromRow<'row, SqliteRow> for SmartPlaylistRow {
    fn from_row(row: &'row SqliteRow) -> Result<Self, sqlx::Error> {
        let definition = row.try_get::<String, _>("definition_json")?;
        let object_id: String = row.try_get("object_id")?;
        let artwork_binding =
            row.try_get::<Option<String>, _>("artwork_revision")?
                .map(|revision| {
                    serde_json::to_vec(&crate::PlaylistArtworkBinding {
                        smart: true,
                        source_id: None,
                        object_id: object_id.clone(),
                        revision,
                    })
                    .expect("playlist artwork binding")
                });
        Ok(Self {
            smart_playlist_key: row.try_get("smart_playlist_key")?,
            object_id,
            name: row.try_get("name")?,
            definition: serde_json::from_str(&definition)
                .map_err(|error| sqlx::Error::Decode(Box::new(error)))?,
            position: row.try_get("position")?,
            track_count: row.try_get("track_count")?,
            duration_millis: row.try_get("duration_millis")?,
            downloaded_count: 0,
            artwork_bindings: artwork_binding.iter().cloned().collect(),
            artwork_binding,
            representative_artwork: Vec::new(),
        })
    }
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

async fn load_smart_playlist_rows(
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
        "{} SELECT request.key requested_ordinal,playlist.smart_playlist_key,
             playlist.object_id,playlist.name,playlist.definition_json,
             playlist.position,playlist.artwork_revision,
             COALESCE(summary.track_count,0) track_count,
             COALESCE(summary.duration_millis,0) duration_millis,
             COALESCE(summary.downloaded_count,0) downloaded_count,
             COALESCE(album.artwork_binding,cover.artwork_binding) cover_binding
           FROM json_each(?4) request
           JOIN smart_playlists playlist ON playlist.smart_playlist_key=request.value
           LEFT JOIN summaries summary ON summary.definition_key=request.value
           LEFT JOIN tracks cover ON cover.media_uri=summary.cover_uri
           LEFT JOIN albums album ON album.album_key=cover.album_key
           ORDER BY request.key,summary.cover_position",
        smart_policy_sql(connection, now, Some(keys), SmartOutput::Summary).await?
    );
    let mut records = sqlx::query(AssertSqlSafe(sql))
        .persistent(false)
        .bind(source)
        .bind(now)
        .bind(folder)
        .bind(requested)
        .fetch(&mut *connection);
    let mut result: Vec<SmartPlaylistRow> = Vec::new();
    let mut ordinal = None;
    while let Some(record) = records.try_next().await? {
        let current = record.try_get::<i64, _>("requested_ordinal")?;
        if ordinal != Some(current) {
            let mut row = SmartPlaylistRow::from_row(&record)?;
            normalize_definition(&mut row.definition)?;
            row.downloaded_count = record.try_get("downloaded_count")?;
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
    pub async fn smart_playlist_count(
        &self,
        source: Option<SourceKey>,
        folder: Option<FolderKey>,
        filter: &str,
        now: i64,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<i64> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(if folder.is_none() {
            sqlx::query_scalar(
                "SELECT count(*) FROM smart_playlists WHERE instr(normalized_name,lower(?1))>0",
            )
            .bind(filter.trim())
            .fetch_one(&mut *connection)
            .await?
        } else {
            let policy = smart_policy_sql(&mut connection, now, None, SmartOutput::Exists).await?;
            sqlx::query_scalar(AssertSqlSafe(format!("{policy} SELECT count(*) FROM definitions WHERE instr(normalized_name,lower(?5))>0 AND (current_scope=0 OR ?3 IS NULL OR EXISTS(SELECT 1 FROM selected WHERE selected.definition_key=definitions.definition_key))")))
                .bind(source).bind(now).bind(folder).bind(Option::<SmartPlaylistKey>::None)
                .bind(filter.trim()).fetch_one(&mut *connection).await?
        })
    }

    pub async fn smart_playlist_track_count(
        &self,
        source: Option<SourceKey>,
        key: SmartPlaylistKey,
        folder: Option<FolderKey>,
        filter: &str,
        now: i64,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<i64> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let filter = filter.trim().to_lowercase();
        let input = smart_display_input(&mut connection, now, key).await?;
        Ok(if filter.is_empty() {
            sqlx::query_scalar(AssertSqlSafe(format!("SELECT count(*) FROM ({input})")))
                .bind(source)
                .bind(now)
                .bind(folder)
                .bind(serde_json::to_string(&[key.raw()])?)
                .fetch_one(&mut *connection)
                .await?
        } else {
            // Keep the existing Unicode text matching without retaining the result set.
            let mut records = sqlx::query(AssertSqlSafe(smart_track_sql(
                &input,
                "SELECT title,artist,album,coalesce(year,0) year FROM smart_tracks",
            )))
            .bind(source)
            .bind(now)
            .bind(folder)
            .bind(serde_json::to_string(&[key.raw()])?)
            .fetch(&mut *connection);
            let mut count = 0;
            while let Some(row) = records.try_next().await? {
                count += i64::from(matches_smart_text(&row, &filter));
            }
            count
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn smart_playlist_page(
        &self,
        source: Option<SourceKey>,
        folder: Option<FolderKey>,
        sort: SmartPlaylistListSort,
        descending: bool,
        filter: &str,
        now: i64,
        offset: usize,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<SmartPlaylistRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let limit = limit.min(SMART_PLAYLIST_ROW_LIMIT);
        let filter = filter.trim();
        let keys = if let Some(order) =
            smart_list_order(sort, descending).filter(|_| folder.is_none())
        {
            let sql = format!(
                "SELECT smart_playlist_key FROM smart_playlists WHERE instr(normalized_name,lower(?1))>0 ORDER BY {order} LIMIT ?2 OFFSET ?3"
            );
            sqlx::query_scalar::<_, SmartPlaylistKey>(AssertSqlSafe(sql))
                .bind(filter)
                .bind(limit as i64)
                .bind(offset.min(i64::MAX as usize) as i64)
                .fetch_all(&mut *transaction)
                .await?
        } else {
            let totals = matches!(
                sort,
                SmartPlaylistListSort::TrackCount | SmartPlaylistListSort::Duration
            );
            let policy = smart_policy_sql(
                &mut transaction,
                now,
                None,
                if totals {
                    SmartOutput::Totals
                } else {
                    SmartOutput::Exists
                },
            )
            .await?;
            let join = if totals {
                "LEFT JOIN selected USING(definition_key)"
            } else {
                ""
            };
            let admitted = if totals {
                "track_count>0"
            } else {
                "EXISTS(SELECT 1 FROM selected WHERE selected.definition_key=definitions.definition_key)"
            };
            let column = match sort {
                SmartPlaylistListSort::Position => "position",
                SmartPlaylistListSort::Title => "normalized_name",
                SmartPlaylistListSort::TrackCount => "track_count",
                SmartPlaylistListSort::Duration => "duration_millis",
            };
            let direction = if descending { "DESC" } else { "ASC" };
            let sql = format!(
                "{policy} SELECT definition_key FROM definitions {join}
                WHERE (current_scope=0 OR ?3 IS NULL OR {admitted})
                  AND instr(normalized_name,lower(?5))>0
                ORDER BY {column} {direction},position,definition_key LIMIT ?6 OFFSET ?7"
            );
            sqlx::query_scalar::<_, SmartPlaylistKey>(AssertSqlSafe(sql))
                .persistent(false)
                .bind(source)
                .bind(now)
                .bind(folder)
                .bind(Option::<SmartPlaylistKey>::None)
                .bind(filter)
                .bind(limit as i64)
                .bind(offset.min(i64::MAX as usize) as i64)
                .fetch_all(&mut *transaction)
                .await?
        };
        let rows = load_smart_playlist_rows(&mut transaction, source, &keys, folder, now).await?;
        transaction.commit().await?;
        Ok(rows)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn smart_playlist_track_page(
        &self,
        source: Option<SourceKey>,
        key: SmartPlaylistKey,
        folder: Option<FolderKey>,
        filter: &str,
        now: i64,
        offset: usize,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<SmartPlaylistTrackRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let uris = smart_uri_page(
            &mut transaction,
            source,
            key,
            folder,
            filter,
            now,
            Some((offset, limit.min(256))),
        )
        .await?;
        let rows = load_smart_track_rows(&mut transaction, &uris).await?;
        transaction.commit().await?;
        Ok(rows)
    }

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

    pub async fn smart_playlist_route_page(
        &self,
        source: Option<SourceKey>,
        folder: Option<FolderKey>,
        sort: SmartPlaylistListSort,
        descending: bool,
        now: i64,
        window: RouteSeedWindow,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<(usize, usize, Vec<SmartPlaylistRow>)> {
        let count = self
            .smart_playlist_count(source, folder, "", now, cancellation)
            .await?
            .max(0) as usize;
        let range = window.range(count);
        let rows = self
            .smart_playlist_page(
                source,
                folder,
                sort,
                descending,
                "",
                now,
                range.start,
                range.len(),
                cancellation,
            )
            .await?;
        Ok((count, range.start, rows))
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
            smart_policy_sql(&mut connection, now, Some(&[key]), SmartOutput::Members).await?;
        Ok(sqlx::query_scalar(AssertSqlSafe(format!(
            "{policy} SELECT COALESCE(track.artwork_binding,album.artwork_binding) binding
             FROM selected JOIN tracks track USING(media_uri) LEFT JOIN albums album ON album.album_key=track.album_key
             WHERE binding IS NOT NULL GROUP BY binding ORDER BY random() LIMIT 4"
        ))).persistent(false).bind(source).bind(now).bind(folder)
            .bind(serde_json::to_string(&[key.raw()])?).fetch_all(&mut *connection).await?)
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

    pub async fn smart_playlist_media_uri_order(
        &self,
        source: Option<SourceKey>,
        key: SmartPlaylistKey,
        folder: Option<FolderKey>,
        now: i64,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<String>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let sql = format!(
            "{}\nSELECT media_uri FROM selected ORDER BY {SMART_RESULT_ORDER}",
            smart_policy_sql(
                &mut connection,
                now,
                Some(std::slice::from_ref(&key)),
                SmartOutput::Members
            )
            .await?
        );
        Ok(sqlx::query_scalar::<_, String>(AssertSqlSafe(sql.as_str()))
            .persistent(false)
            .bind(source)
            .bind(now)
            .bind(folder)
            .bind(serde_json::to_string(&[key.raw()])?)
            .fetch_all(&mut *connection)
            .await?)
    }

    pub async fn smart_playlist_track_rows(
        &self,
        media_uris: &[String],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<SmartPlaylistTrackRow>> {
        if media_uris.len() > 256 {
            return Err(LibraryError::InvalidRequest(
                "Smart row window exceeds 256".into(),
            ));
        }
        let mut connection = tokio::select! {
            result = self.acquire_reader() => result?,
            () = cancellation.cancelled() => return Err(LibraryError::ReadCancelled),
        };
        load_smart_track_rows(&mut connection, media_uris).await
    }

    pub async fn smart_playlist_sorted_track_page(
        &self,
        source: Option<SourceKey>,
        key: SmartPlaylistKey,
        folder: Option<FolderKey>,
        filter: &str,
        sort: crate::TrackSort,
        descending: bool,
        now: i64,
        offset: usize,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<SmartPlaylistTrackRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let uris = smart_sorted_uris_on(
            &mut transaction,
            source,
            key,
            folder,
            filter,
            sort,
            descending,
            now,
            Some(std::slice::from_ref(
                &(offset..offset.saturating_add(limit.min(256))),
            )),
        )
        .await?;
        let rows = load_smart_track_rows(&mut transaction, &uris).await?;
        transaction.commit().await?;
        Ok(rows)
    }
}

async fn load_smart_track_rows(
    connection: &mut SqliteConnection,
    media_uris: &[String],
) -> LibraryResult<Vec<SmartPlaylistTrackRow>> {
    let sql = smart_track_sql(
        "SELECT key,value FROM json_each(?1)",
        "SELECT media_uri,title,artist,album,album_media_uri,artists,album_artists,
                album_display_artist,artwork_binding,duration_millis,disc_number,track_number,
                year,release_date,date_added,source_format,source_path,source_name,audio_properties,musicbrainz_recording_id,
                musicbrainz_release_track_id,bpm,genre,play_count,last_played,favorite,rating,is_downloaded
         FROM smart_tracks ORDER BY position",
    );
    sqlx::query(AssertSqlSafe(sql))
        .bind(serde_json::to_string(media_uris)?)
        .fetch_all(connection)
        .await?
        .iter()
        .map(|row| {
            let mut entry = SmartPlaylistTrackRow::from_row(row)?;
            entry.artists = serde_json::from_str(row.try_get("artists")?)?;
            entry.album_artists = serde_json::from_str(row.try_get("album_artists")?)?;
            Ok(entry)
        })
        .collect()
}

// SQLite inlines this projection, evaluating only the facts used by the caller.
// A URI order therefore does not assemble artwork or metadata links for every member.
fn smart_track_sql(input: &str, projection: &str) -> String {
    let snapshot = |columns: [&str; 5]| {
        let values = columns
            .iter()
            .enumerate()
            .filter(|(_, column)| **column != "NULL")
            .map(|(kind, column)| format!("WHEN {} THEN {column}", kind + 1))
            .collect::<Vec<_>>()
            .join(" ");
        format!("CASE winner.kind {values} END")
    };
    format!(
        "WITH input AS NOT MATERIALIZED ({input}), smart_tracks AS NOT MATERIALIZED (
         SELECT {links} requested.key position,requested.value media_uri,COALESCE(track.title,{title},requested.value) title,
                COALESCE(track.display_artist,{artist},'') artist,COALESCE(track.display_album,{album},'') album,
                COALESCE(album.display_artist,{album_artist}) album_display_artist,
                COALESCE((SELECT artist.name FROM album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key=track.album_key ORDER BY credit.position LIMIT 1),track.display_artist,{artist},'') sort_album_artist,
                COALESCE((SELECT min(genre.name COLLATE NOCASE) FROM track_genres credit JOIN genres genre USING(genre_key) WHERE credit.track_key=track.track_key),'') sort_genre,
                track.artwork_binding,COALESCE(track.duration_millis,{duration},0) duration_millis,
                COALESCE(track.disc_number,{disc_number}) disc_number,
                COALESCE(track.track_number,{track_number}) track_number,
                COALESCE(track.year,{year}) year,track.date_added,track.bpm,
                COALESCE(track.release_date,{release_date}) release_date,
                COALESCE(track.source_format,{source_format}) source_format,
                COALESCE(track.musicbrainz_recording_id,{recording_id}) musicbrainz_recording_id,
                COALESCE(track.musicbrainz_release_track_id,{release_track_id}) musicbrainz_release_track_id,
                COALESCE(track.audio_properties,'{{}}') audio_properties,track.source_path,(SELECT display_name FROM sources WHERE source_key=track.source_key) source_name,
                COALESCE((SELECT group_concat(genre.name,', ') FROM track_genres credit JOIN genres genre USING(genre_key) WHERE credit.track_key=track.track_key),'') genre,
                COALESCE(state.favorite,track.source_favorite,0) favorite,
                COALESCE(state.rating,track.source_rating)/10 rating,
                COALESCE(track.local_play_count,(SELECT count(*) FROM listens played WHERE played.media_uri=requested.value)) play_count,
                (SELECT max(played_at) FROM (SELECT baseline.last_played_at played_at FROM activity_baseline baseline WHERE baseline.source_key=track.source_key AND baseline.track_object_id=track.object_id AND baseline.period='lifetime' AND baseline.item_kind='track' UNION ALL SELECT max(started_at) FROM listens played WHERE played.media_uri=requested.value)) last_played,
                EXISTS(SELECT 1 FROM local_access_files downloaded WHERE downloaded.media_uri=requested.value AND downloaded.origin='download') is_downloaded
         FROM input requested
         LEFT JOIN tracks track ON track.media_uri=requested.value
         LEFT JOIN albums album USING(album_key)
         LEFT JOIN main.playlist_entries entry ON entry.playlist_entry_key=(
             SELECT playlist_entry_key FROM main.playlist_entries WHERE track.track_key IS NULL AND media_uri=requested.value
             ORDER BY title IS NULL,snapshot_at DESC,playlist_entry_key DESC LIMIT 1)
         LEFT JOIN catalog.native_playlist_entries native ON native.playlist_entry_key=(
             SELECT playlist_entry_key FROM catalog.native_playlist_entries WHERE track.track_key IS NULL AND media_uri=requested.value
             ORDER BY title IS NULL,snapshot_at DESC,playlist_entry_key ASC LIMIT 1)
         LEFT JOIN queue_occurrences occurrence ON occurrence.queue_occurrence_key=(
             SELECT queue_occurrence_key FROM queue_occurrences WHERE track.track_key IS NULL AND media_uri=requested.value
             ORDER BY snapshot_at DESC,queue_occurrence_key DESC LIMIT 1)
         LEFT JOIN listens listen ON listen.listen_key=(
             SELECT listen_key FROM listens WHERE track.track_key IS NULL AND media_uri=requested.value ORDER BY started_at DESC,listen_key DESC LIMIT 1)
         LEFT JOIN main.local_locators locator ON track.track_key IS NULL
             AND locator.media_uri=requested.value AND locator.origin='download'
         LEFT JOIN catalog.local_access_metadata access ON access.access_uri=locator.access_uri
         LEFT JOIN (SELECT 1 kind UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4 UNION ALL SELECT 5) winner
         ON track.track_key IS NULL AND winner.kind=(SELECT kind FROM (
             SELECT 1 kind,entry.title IS NULL missing,entry.snapshot_at stamp,entry.playlist_entry_key public_key WHERE entry.playlist_entry_key IS NOT NULL
             UNION ALL SELECT 2,native.title IS NULL,native.snapshot_at,-native.playlist_entry_key WHERE native.playlist_entry_key IS NOT NULL
             UNION ALL SELECT 3,0,occurrence.snapshot_at,occurrence.queue_occurrence_key WHERE occurrence.queue_occurrence_key IS NOT NULL
             UNION ALL SELECT 4,0,listen.started_at,listen.listen_key WHERE listen.listen_key IS NOT NULL
             UNION ALL SELECT 5,0,COALESCE(access.mtime_ns,0)/1000000000,locator.local_access_file_key WHERE locator.local_access_file_key IS NOT NULL
         ) ORDER BY missing,stamp DESC,public_key DESC,kind LIMIT 1)
         LEFT JOIN user_media_state state ON state.media_uri=requested.value
         ) {projection}",
         links = crate::tracks::TRACK_LINK_COLUMNS,
         title = snapshot(["COALESCE(entry.title,'')", "COALESCE(native.title,'')", "occurrence.title", "listen.track_title", "COALESCE(access.title,'')"]),
         artist = snapshot(["COALESCE(entry.artist,'')", "COALESCE(native.artist,'')", "occurrence.artist", "listen.artist_name", "COALESCE(access.artist,'')"]),
         album = snapshot(["COALESCE(entry.album,'')", "COALESCE(native.album,'')", "occurrence.album", "listen.album_title", "COALESCE(access.album,'')"]),
         album_artist = snapshot(["entry.album_display_artist", "native.album_display_artist", "occurrence.album_display_artist", "NULL", "NULL"]),
         duration = snapshot(["COALESCE(entry.duration_millis,0)", "COALESCE(native.duration_millis,0)", "occurrence.duration_millis", "listen.duration_millis", "COALESCE(access.duration_millis,0)"]),
         disc_number = snapshot(["entry.disc_number", "native.disc_number", "occurrence.disc_number", "listen.disc_number", "COALESCE(access.disc_number,0)"]),
         track_number = snapshot(["entry.track_number", "native.track_number", "occurrence.track_number", "listen.track_number", "COALESCE(access.track_number,0)"]),
         year = snapshot(["entry.year", "native.year", "occurrence.year", "listen.year", "NULL"]),
         release_date = snapshot(["entry.release_date", "native.release_date", "occurrence.release_date", "listen.release_date", "NULL"]),
         source_format = snapshot(["entry.source_format", "native.source_format", "occurrence.source_format", "listen.source_format", "NULL"]),
         recording_id = snapshot(["entry.musicbrainz_recording_id", "native.musicbrainz_recording_id", "occurrence.musicbrainz_recording_id", "listen.musicbrainz_recording_id", "NULL"]),
         release_track_id = snapshot(["entry.musicbrainz_release_track_id", "native.musicbrainz_release_track_id", "occurrence.musicbrainz_release_track_id", "listen.musicbrainz_release_track_id", "NULL"]),
    )
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
          INSERT INTO tracks(track_key,source_key,object_id,title,normalized_search,display_album,display_artist,sort_text,duration_millis,media_uri)
          VALUES(1,1,'one','One','one','','','one',1000,'track:1');
          INSERT INTO listens(media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis)
          VALUES('track:1','One','','',1000000,'1970-01',1000,1000),
                ('https://example.test/missing','Missing','','',395200,'1970-01',1000,1000),
                ('https://example.test/old','Old','','',395199,'1970-01',1000,1000),
                ('https://example.test/future','Future','','',1000001,'1970-01',1000,1000);")
            .execute(&mut *connection).await.unwrap();
        for grow in [false, true] {
            if grow {
                sqlx::raw_sql("WITH RECURSIVE n(x) AS (VALUES(2) UNION ALL SELECT x+1 FROM n WHERE x<10000)
                  INSERT INTO tracks(track_key,source_key,object_id,title,normalized_search,display_album,display_artist,sort_text,duration_millis,media_uri)
                  SELECT x,1,CAST(x AS TEXT),'Title','title','','','title',1000,'track:'||x FROM n;")
                    .execute(&mut *connection).await.unwrap();
            }
            let sql = format!(
                "{} SELECT media_uri FROM selected ORDER BY {SMART_RESULT_ORDER}",
                smart_policy_sql(connection, 1000000, Some(&[key]), SmartOutput::Members)
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
                .bind(Option::<SourceKey>::None)
                .bind(1000000_i64)
                .bind(Option::<FolderKey>::None)
                .bind(serde_json::to_string(&[key]).unwrap())
                .fetch_all(&mut *connection)
                .await
                .unwrap();
            connection
                .lock_handle()
                .await
                .unwrap()
                .remove_progress_handler();
            assert_eq!(order, ["https://example.test/missing", "track:1"]);
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
        let rows = load_smart_track_rows(connection, &["track:1".into()])
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

    #[tokio::test]
    async fn stable_source_references_survive_catalog_and_definition_key_changes() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(directory.path().join("store.sqlite"))
            .await
            .unwrap();
        let mut writer = database.writer().await.unwrap();
        let connection = writer.as_mut().unwrap();
        sqlx::raw_sql("INSERT INTO sources(source_key,object_id,display_name,normalized_name,artwork_digest) VALUES (1,'source','Source','source',zeroblob(32)); INSERT INTO genres(genre_key,source_key,object_id,name,normalized_name,sort_text) VALUES(1,1,'genre','Genre','genre','genre'); INSERT INTO folders(folder_key,source_key,object_id,name,normalized_name,sort_text) VALUES(1,1,'folder','Folder','folder','folder');").execute(&mut *connection).await.unwrap();
        let definition = SmartPlaylistDefinition {
            current: true,
            ..SmartPlaylistDefinition::default()
        };
        let key=sqlx::query_scalar::<_,SmartPlaylistKey>("INSERT INTO smart_playlists(object_id,name,normalized_name,definition_json,position) VALUES('smart','Smart','smart',?1,0) RETURNING smart_playlist_key").bind(serde_json::to_string(&definition).unwrap()).fetch_one(&mut *connection).await.unwrap();
        let source = sqlx::query_scalar::<_, SourceKey>("SELECT source_key FROM sources")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        let folder = sqlx::query_scalar::<_, FolderKey>("SELECT folder_key FROM folders")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        let genre = sqlx::query_scalar::<_, crate::GenreKey>("SELECT genre_key FROM genres")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        let smart = smart_source_reference(connection, key, Some(source), Some(folder))
            .await
            .unwrap()
            .unwrap();
        let collection = crate::collections::canonical_collection_on(
            connection,
            &crate::QueueCollection::Genre(genre),
            Some(folder),
        )
        .await
        .unwrap()
        .unwrap();
        sqlx::raw_sql("DELETE FROM sources; UPDATE smart_playlists SET smart_playlist_key=500; INSERT INTO sources(source_key,object_id,display_name,normalized_name,artwork_digest) VALUES (99,'source','Source','source',zeroblob(32)); INSERT INTO genres(genre_key,source_key,object_id,name,normalized_name,sort_text) VALUES(88,99,'genre','Genre','genre','genre'); INSERT INTO folders(folder_key,source_key,object_id,name,normalized_name,sort_text) VALUES(77,99,'folder','Folder','folder','folder');").execute(&mut *connection).await.unwrap();
        let (key, source, folder) = resolve_smart_reference(connection, &smart)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (key.raw(), source.unwrap().raw(), folder.unwrap().raw()),
            (500, 99, 77)
        );
        let (collection, folder) =
            crate::collections::resolve_collection_reference(connection, &collection)
                .await
                .unwrap()
                .unwrap();
        assert!(matches!(collection,crate::QueueCollection::Genre(key) if key.raw()==88));
        assert_eq!(folder.unwrap().raw(), 77);
        sqlx::query("DELETE FROM folders")
            .execute(&mut *connection)
            .await
            .unwrap();
        assert!(
            resolve_smart_reference(connection, &smart)
                .await
                .unwrap()
                .is_none(),
            "a missing folder must not widen the source scope"
        );
    }
}

fn source_fact(
    field: SmartPlaylistRuleField,
    definition: &SmartPlaylistDefinition,
    catalog: bool,
    now: i64,
) -> String {
    use SmartPlaylistRuleField as F;
    let period = definition.activity_period.seconds();
    let time = period
        .map(|seconds| format!(" AND started_at>={} AND started_at<={now}", now - seconds))
        .unwrap_or_else(|| format!(" AND started_at<={now}"));
    let local_count = if catalog {
        "owner.local_play_count".to_string()
    } else {
        "(SELECT count(*) FROM listens WHERE media_uri=owner.media_uri)".to_string()
    };
    match field {
        F::Title => "owner.title".into(),
        F::Artist => "owner.display_artist".into(),
        F::Album => "owner.display_album".into(),
        F::Comment => "owner.comment".into(),
        F::Bpm => "owner.bpm".into(),
        F::Year => "owner.year".into(),
        F::DateAdded => "owner.date_added".into(),
        F::Favorite => "COALESCE((SELECT favorite FROM user_media_state WHERE media_uri=owner.media_uri),owner.source_favorite)".into(),
        F::Rating => "COALESCE((SELECT rating FROM user_media_state WHERE media_uri=owner.media_uri),owner.source_rating)/10".into(),
        F::Played => local_count,
        F::PlayCount if period.is_none() => local_count,
        F::PlayCount => format!("(SELECT count(*) FROM listens WHERE media_uri=owner.media_uri{time})"),
        F::SkipCount | F::LastPlayed => {
            let aggregate = if field == F::SkipCount { "COALESCE(sum(skipped),0)" } else { "max(started_at)" };
            let local = format!("(SELECT {aggregate} FROM listens WHERE media_uri=owner.media_uri{time})");
            if period.is_some() { return local; }
            let column = if field == F::SkipCount { "skip_count" } else { "last_played_at" };
            let baseline = format!("(SELECT {column} FROM activity_baseline WHERE source_key=owner.source_key AND track_object_id=owner.object_id AND period='lifetime' AND item_kind='track')");
            if field == F::SkipCount { format!("({local}+COALESCE({baseline},0))") }
            else { format!("COALESCE(max({local},{baseline}),{local},{baseline})") }
        }
        F::Genre | F::Mood => unreachable!("relation rules compile at their owner"),
    }
}

fn source_rule(
    rule: &SmartPlaylistRule,
    definition: &SmartPlaylistDefinition,
    catalog: bool,
    now: i64,
) -> String {
    use SmartPlaylistRuleField as F;
    use SmartPlaylistRuleOperator as O;
    use SmartPlaylistRuleValue as V;
    if !rule.field.operators().contains(&rule.operator) {
        return "0".into();
    }
    if rule.field == F::PlayCount
        && rule.operator == O::Above
        && let Some(V::Number(minimum)) = rule.value
        && minimum >= 0
        && let Some(seconds) = definition.activity_period.seconds()
    {
        return format!(
            "owner.media_uri IN(SELECT media_uri FROM listens WHERE started_at>={} AND started_at<={now} GROUP BY media_uri HAVING count(*)>{minimum})",
            now - seconds
        );
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
    let mut fact = source_fact(rule.field, definition, catalog, now);
    if rule.field == F::Played {
        let Some(V::Bool(value)) = rule.value else {
            return "0".into();
        };
        let played = value != (rule.operator == O::IsNot);
        return format!("{fact}{}0", if played { ">" } else { "=" });
    }
    if rule.field == F::LastPlayed {
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

fn source_rules(definition: &SmartPlaylistDefinition, catalog: bool, now: i64) -> String {
    let all = definition
        .match_all
        .iter()
        .map(|rule| format!("({})", source_rule(rule, definition, catalog, now)))
        .collect::<Vec<_>>();
    let any = definition
        .match_any
        .iter()
        .map(|rule| format!("({})", source_rule(rule, definition, catalog, now)))
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

fn fallback_candidates(definitions: &[(i64, SmartPlaylistDefinition)]) -> String {
    use SmartPlaylistRuleField as F;
    use SmartPlaylistRuleOperator as O;
    use SmartPlaylistRuleValue as V;
    let unplayed = definitions.iter().all(|(_, definition)| {
        let zero = |rule: &SmartPlaylistRule| {
            matches!(
                (rule.field, rule.operator, &rule.value),
                (F::Played, O::Is, Some(V::Bool(false)))
                    | (F::Played, O::IsNot, Some(V::Bool(true)))
            ) || (definition.activity_period == SmartPlaylistActivityPeriod::Lifetime
                && matches!(
                    (rule.field, rule.operator, &rule.value),
                    (F::PlayCount, O::Equals, Some(V::Number(0)))
                ))
        };
        definition.match_all.iter().any(zero)
            || (!definition.match_any.is_empty() && definition.match_any.iter().all(zero))
    });
    // Membership rules and visible rows use the same metadata owner lookup.
    format!(
        "requested AS MATERIALIZED (
            SELECT media_uri FROM playlist_entries entry
            WHERE NOT EXISTS(SELECT 1 FROM tracks WHERE media_uri=entry.media_uri)
            UNION SELECT media_uri FROM queue_occurrences occurrence
            WHERE NOT EXISTS(SELECT 1 FROM tracks WHERE media_uri=occurrence.media_uri)
            UNION SELECT media_uri FROM listens listen WHERE {played}
              AND NOT EXISTS(SELECT 1 FROM tracks WHERE media_uri=listen.media_uri)
            UNION SELECT media_uri FROM local_access_files access WHERE origin='download'
              AND NOT EXISTS(SELECT 1 FROM tracks WHERE media_uri=access.media_uri)
         ), media_rows AS NOT MATERIALIZED ({rows})",
        played = i32::from(!unplayed),
        rows = smart_track_sql(
            "SELECT NULL key,media_uri value FROM requested",
            "SELECT media_uri,source.source_key,NULL track_key,NULL object_id,
                    title,artist display_artist,album display_album,duration_millis,year,
                    NULL date_added,NULL comment,NULL bpm,NULL source_rating,NULL source_favorite,
                    lower(title) sort_text
             FROM smart_tracks
             LEFT JOIN source_scope source ON substr(media_uri,1,length(source.prefix))=source.prefix",
        ),
    )
}

fn source_sort(definition: &SmartPlaylistDefinition, catalog: bool, now: i64) -> String {
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
    let fact = source_fact(field, definition, catalog, now);
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum SmartOutput {
    Members,
    Export,
    Summary,
    Totals,
    Exists,
}

async fn smart_policy_sql(
    connection: &mut SqliteConnection,
    now: i64,
    selected: Option<&[SmartPlaylistKey]>,
    output: SmartOutput,
) -> LibraryResult<String> {
    let sources = sqlx::query_as::<_, (i64, String)>("SELECT source_key,object_id FROM sources")
        .fetch_all(&mut *connection)
        .await?;
    let scope = if sources.is_empty() {
        "SELECT NULL,NULL WHERE false".to_string()
    } else {
        format!(
            "VALUES {}",
            sources
                .into_iter()
                .map(|(key, id)| format!(
                    "({key},{})",
                    sql_text(&crate::keys::source_entity_prefix(
                        &crate::SourceId::new(id),
                        "track"
                    ))
                ))
                .collect::<Vec<_>>()
                .join(",")
        )
    };
    let definitions = sqlx::query_as::<_, (i64, String)>(
        "SELECT smart_playlist_key,definition_json FROM smart_playlists
         WHERE ?1 IS NULL OR smart_playlist_key IN(SELECT value FROM json_each(?1)) ORDER BY smart_playlist_key"
    ).bind(selected.map(serde_json::to_string).transpose()?).fetch_all(connection).await?
        .into_iter().map(|(key,json)| Ok((key,serde_json::from_str::<SmartPlaylistDefinition>(&json)?)))
        .collect::<LibraryResult<Vec<_>>>()?;
    let mut sql = format!(
        "WITH parameters AS (SELECT ?1,?2,?3,?4),source_scope(source_key,prefix) AS ({scope}),definitions AS (SELECT smart_playlist_key definition_key,position,normalized_name,COALESCE(json_extract(definition_json,'$.current'),0) current_scope FROM smart_playlists WHERE ?4 IS NULL OR smart_playlist_key IN(SELECT value FROM json_each(?4)))"
    );
    sql.push_str(&format!(",{}", fallback_candidates(&definitions)));
    let metadata = if output == SmartOutput::Export {
        "owner.title,owner.display_artist,owner.display_album,owner.year,"
    } else {
        ""
    };
    let mut selected = Vec::new();
    for (key, definition) in &definitions {
        if output == SmartOutput::Exists && !definition.current {
            continue;
        }
        let admitted = format!("EXISTS(SELECT 1 FROM definitions WHERE definition_key={key})");
        let scope = if definition.current {
            "owner.source_key=?1 AND (?3 IS NULL OR EXISTS(SELECT 1 FROM track_folders WHERE track_key=owner.track_key AND folder_key=?3))"
        } else {
            "1"
        };
        let descending = i32::from(definition.descending);
        let ordered = matches!(output, SmartOutput::Members | SmartOutput::Export)
            || (matches!(output, SmartOutput::Summary | SmartOutput::Totals)
                && definition.limit.is_some());
        let branches=[true,false].into_iter().map(|catalog| {
            let table=if catalog {"tracks".to_string()} else {"media_rows".to_string()};
            let sort = if ordered { format!(",{descending} descending,owner.sort_text,{} sort_value",source_sort(definition,catalog,now)) } else { String::new() };
            format!("SELECT {key} definition_key,owner.media_uri,{metadata}owner.duration_millis{sort} FROM {table} owner WHERE {admitted} AND ({scope}) AND ({})",source_rules(definition,catalog,now))
        }).collect::<Vec<_>>().join(" UNION ALL ");
        let direction = if definition.descending { "DESC" } else { "ASC" };
        let order = format!("sort_value {direction} NULLS LAST,sort_text,media_uri");
        let limited =
            if let Some(limit) = definition.limit.filter(|_| output != SmartOutput::Exists) {
                format!("{branches} ORDER BY {order} LIMIT {limit}")
            } else {
                branches
            };
        sql.push_str(&format!(",result_{key} AS NOT MATERIALIZED ({limited})"));
        if output == SmartOutput::Exists {
            selected.push(format!(
                "SELECT {key} definition_key WHERE EXISTS(SELECT 1 FROM result_{key})"
            ));
            continue;
        }
        if output == SmartOutput::Totals {
            selected.push(format!("SELECT {key} definition_key,count(*) track_count,COALESCE(sum(duration_millis),0) duration_millis FROM result_{key}"));
            continue;
        }
        if output == SmartOutput::Summary {
            sql.push_str(&format!(
                ",groups_{key} AS (SELECT COALESCE(album.media_uri,track.media_uri) cover_group,count(*) track_count,COALESCE(sum(result.duration_millis),0) duration_millis,count(CASE WHEN EXISTS(SELECT 1 FROM local_access_files access WHERE access.media_uri=result.media_uri AND access.origin='download') THEN 1 END) downloaded_count,min(CASE WHEN COALESCE(track.artwork_binding,album.artwork_binding) IS NOT NULL THEN track.media_uri END) cover_uri FROM result_{key} result LEFT JOIN tracks track ON track.media_uri=result.media_uri LEFT JOIN albums album ON album.album_key=track.album_key GROUP BY COALESCE(album.media_uri,track.media_uri)),summary_{key} AS (SELECT {key} definition_key,sum(track_count) OVER () track_count,sum(duration_millis) OVER () duration_millis,sum(downloaded_count) OVER () downloaded_count,cover_uri,row_number() OVER (ORDER BY cover_uri IS NULL,cover_group,cover_uri) cover_position FROM groups_{key})"
            ));
            selected.push(format!("SELECT * FROM summary_{key} WHERE cover_position<=4 AND (cover_uri IS NOT NULL OR cover_position=1)"));
            continue;
        }
        let columns = if output == SmartOutput::Export {
            "*"
        } else {
            "definition_key,media_uri,duration_millis,descending,sort_text,sort_value"
        };
        selected.push(format!("SELECT {columns} FROM result_{key}"));
    }
    if selected.is_empty() {
        selected.push(if output == SmartOutput::Exists {
            "SELECT NULL definition_key WHERE 0".into()
        } else if output == SmartOutput::Totals {
            "SELECT NULL definition_key,0 track_count,0 duration_millis WHERE 0".into()
        } else if output == SmartOutput::Summary {
            "SELECT NULL definition_key,0 track_count,0 duration_millis,0 downloaded_count,NULL cover_uri,0 cover_position WHERE 0".into()
        } else {
            "SELECT NULL definition_key,NULL media_uri,NULL title,NULL display_artist,NULL display_album,NULL year,0 duration_millis,0 descending,NULL sort_text,NULL sort_value WHERE 0"
                .into()
        });
    }
    let relation = if output == SmartOutput::Summary {
        "summaries"
    } else {
        "selected"
    };
    sql.push_str(&format!(
        ",{relation} AS MATERIALIZED ({})",
        selected.join(" UNION ALL ")
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
        let policy = smart_policy_sql(
            &mut connection,
            now,
            Some(std::slice::from_ref(&key)),
            SmartOutput::Export,
        )
        .await?;
        let sql = format!(
            "{policy},export_rows AS (SELECT selected.*,row_number() OVER(ORDER BY {SMART_RESULT_ORDER}) result_position FROM selected) SELECT 'm3u:' || result_position object_id,media_uri,title,display_artist artist,display_album album,NULL album_display_artist,0 snapshot_at,duration_millis,NULL disc_number,NULL track_number,year,NULL release_date,NULL source_format,NULL musicbrainz_recording_id,NULL musicbrainz_release_track_id,result_position-1 position FROM export_rows ORDER BY result_position"
        );
        let mut rows = sqlx::query_as::<_, crate::PlaylistEntryWrite>(AssertSqlSafe(sql.as_str()))
            .persistent(false)
            .bind(source)
            .bind(now)
            .bind(folder)
            .bind(serde_json::to_string(&[key.raw()])?)
            .fetch(&mut *connection);
        while let Some(entry) = rows.try_next().await? {
            output.entry(&entry)?;
        }
        output.finish()
    }
}

fn matches_smart_text(row: &SqliteRow, filter: &str) -> bool {
    filter.is_empty()
        || ["title", "artist", "album"]
            .into_iter()
            .any(|field| row.get::<&str, _>(field).to_lowercase().contains(filter))
        || row.get::<i64, _>("year").to_string().contains(filter)
}

#[allow(clippy::too_many_arguments)]
async fn smart_uri_page(
    connection: &mut SqliteConnection,
    source: Option<SourceKey>,
    key: SmartPlaylistKey,
    folder: Option<FolderKey>,
    filter: &str,
    now: i64,
    page: Option<(usize, usize)>,
) -> LibraryResult<Vec<String>> {
    if page.is_some_and(|(_, limit)| limit == 0) {
        return Ok(Vec::new());
    }
    let filter = filter.trim().to_lowercase();
    let policy = smart_policy_sql(
        connection,
        now,
        Some(std::slice::from_ref(&key)),
        if filter.is_empty() {
            SmartOutput::Members
        } else {
            SmartOutput::Export
        },
    )
    .await?
    .replace(
        ",selected AS MATERIALIZED (",
        ",selected AS NOT MATERIALIZED (",
    );
    let columns = if filter.is_empty() {
        "media_uri"
    } else {
        "media_uri,title,display_artist artist,display_album album,coalesce(year,0) year"
    };
    let suffix = if filter.is_empty() {
        page.map(|(offset, limit)| {
            format!(" LIMIT {limit} OFFSET {}", offset.min(i64::MAX as usize))
        })
        .unwrap_or_default()
    } else {
        String::new()
    };
    let sql =
        format!("{policy} SELECT {columns} FROM selected ORDER BY {SMART_RESULT_ORDER}{suffix}");
    let mut records = sqlx::query(AssertSqlSafe(sql))
        .persistent(false)
        .bind(source)
        .bind(now)
        .bind(folder)
        .bind(serde_json::to_string(&[key.raw()])?)
        .fetch(connection);
    let mut uris = Vec::new();
    let mut skipped = 0;
    while let Some(row) = records.try_next().await? {
        if !matches_smart_text(&row, &filter) {
            continue;
        }
        if !filter.is_empty()
            && let Some((offset, limit)) = page
        {
            if skipped < offset {
                skipped += 1;
                continue;
            }
            uris.push(row.try_get("media_uri")?);
            if uris.len() == limit {
                break;
            }
        } else {
            uris.push(row.try_get("media_uri")?);
        }
    }
    Ok(uris)
}

fn smart_display_order(sort: crate::TrackSort, descending: bool) -> String {
    let direction = if descending { "DESC" } else { "ASC" };
    let field = match sort {
        crate::TrackSort::Title => "title COLLATE NOCASE",
        crate::TrackSort::TrackNumber => "coalesce(disc_number,0)",
        crate::TrackSort::Artist => "artist COLLATE NOCASE",
        crate::TrackSort::AlbumArtist => "sort_album_artist COLLATE NOCASE",
        crate::TrackSort::Album => "album COLLATE NOCASE",
        crate::TrackSort::Year => "coalesce(year,0)",
        crate::TrackSort::ReleaseDate => "release_date",
        crate::TrackSort::DateAdded => "date_added",
        crate::TrackSort::LastPlayed => "last_played",
        crate::TrackSort::PlayCount => "play_count",
        crate::TrackSort::UserRating => "rating",
        crate::TrackSort::Genre => "sort_genre COLLATE NOCASE",
        crate::TrackSort::Bpm => "bpm",
        crate::TrackSort::Duration => "duration_millis",
        crate::TrackSort::Favorite => "favorite",
    };
    let number = if sort == crate::TrackSort::TrackNumber {
        format!(",coalesce(track_number,0) {direction}")
    } else {
        String::new()
    };
    let nulls = if matches!(
        sort,
        crate::TrackSort::ReleaseDate
            | crate::TrackSort::DateAdded
            | crate::TrackSort::LastPlayed
            | crate::TrackSort::PlayCount
            | crate::TrackSort::UserRating
            | crate::TrackSort::Bpm
    ) {
        " NULLS LAST"
    } else {
        ""
    };
    format!(
        "{field} {direction}{nulls}{number},album COLLATE NOCASE {direction},coalesce(disc_number,0) {direction},coalesce(track_number,0) {direction},title COLLATE NOCASE {direction},media_uri {direction}"
    )
}

async fn smart_display_input(
    connection: &mut SqliteConnection,
    now: i64,
    key: SmartPlaylistKey,
) -> LibraryResult<String> {
    let policy = smart_policy_sql(
        connection,
        now,
        Some(std::slice::from_ref(&key)),
        SmartOutput::Members,
    )
    .await?
    .replace(
        ",selected AS MATERIALIZED (",
        ",selected AS NOT MATERIALIZED (",
    );
    Ok(format!(
        "{policy} SELECT NULL key,media_uri value FROM selected"
    ))
}

pub(crate) async fn smart_sorted_uris_on(
    connection: &mut SqliteConnection,
    source: Option<SourceKey>,
    key: SmartPlaylistKey,
    folder: Option<FolderKey>,
    filter: &str,
    sort: crate::TrackSort,
    descending: bool,
    now: i64,
    ranges: Option<&[std::ops::Range<usize>]>,
) -> LibraryResult<Vec<String>> {
    if ranges.is_some_and(|ranges| ranges.iter().all(std::ops::Range::is_empty)) {
        return Ok(Vec::new());
    }
    let input = smart_display_input(connection, now, key).await?;
    let filter = filter.trim().to_lowercase();
    let columns = if filter.is_empty() {
        "media_uri"
    } else {
        "media_uri,title,artist,album,coalesce(year,0) year"
    };
    let mut projection = format!(
        "SELECT {columns} FROM smart_tracks ORDER BY {}",
        smart_display_order(sort, descending)
    );
    let direct_range = ranges
        .filter(|ranges| filter.is_empty() && ranges.len() == 1)
        .map(|ranges| &ranges[0]);
    if let Some(range) = direct_range {
        projection.push_str(&format!(" LIMIT {} OFFSET {}", range.len(), range.start));
    }
    let mut records = sqlx::query(AssertSqlSafe(smart_track_sql(&input, &projection)))
        .persistent(false)
        .bind(source)
        .bind(now)
        .bind(folder)
        .bind(serde_json::to_string(&[key.raw()])?)
        .fetch(connection);
    let mut uris = Vec::new();
    let mut position = 0;
    let mut remaining = ranges.unwrap_or_default();
    while let Some(row) = records.try_next().await? {
        if !matches_smart_text(&row, &filter) {
            continue;
        }
        if ranges.is_none() || direct_range.is_some() {
            uris.push(row.try_get("media_uri")?);
        } else {
            while remaining.first().is_some_and(|range| position >= range.end) {
                remaining = &remaining[1..];
            }
            let Some(range) = remaining.first() else {
                break;
            };
            if position >= range.start {
                uris.push(row.try_get("media_uri")?);
            }
        }
        position += 1;
    }
    Ok(uris)
}
