//! Owns Smart Playlist definitions and direct SQL URI-result, row, and list queries.
//! One shared policy implements rule, Activity-window, limit, and ordering semantics.

use std::collections::BTreeMap;

use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqliteRow;
use sqlx::{AssertSqlSafe, Connection, FromRow, QueryBuilder, Row, Sqlite, SqliteConnection};

use crate::{
    Database, FolderKey, LibraryError, LibraryResult, ReadCancellation, RouteSeedWindow,
    SmartPlaylistKey, SourceKey,
};

const SMART_PLAYLIST_ROW_LIMIT: usize = 64;
const SMART_PLAYLIST_RULE_LIMIT: usize = 64;
const SMART_PLAYLIST_DEFINITION_BYTES: usize = 256 * 1024;
const MOST_PLAYED_OBJECT_ID: &str = "builtin:most_played";
const NEVER_PLAYED_OBJECT_ID: &str = "builtin:never_played";
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

pub(crate) async fn smart_source_window_ref(
    connection: &mut SqliteConnection,
    reference: &SmartSourceReference,
    now: i64,
    after: Option<&str>,
    limit: usize,
    seed: Option<u64>,
    anchor: Option<&str>,
) -> LibraryResult<Vec<(String, String)>> {
    let Some((key, source, folder)) = resolve_smart_reference(connection, reference).await? else {
        return Ok(Vec::new());
    };
    smart_source_window(
        connection, key, source, folder, now, after, limit, seed, anchor,
    )
    .await
}

pub(crate) async fn smart_source_last_ref(
    connection: &mut SqliteConnection,
    reference: &SmartSourceReference,
    now: i64,
    seed: Option<u64>,
) -> LibraryResult<Option<(String, String)>> {
    let Some((key, source, folder)) = resolve_smart_reference(connection, reference).await? else {
        return Ok(None);
    };
    smart_source_last(connection, key, source, folder, now, seed).await
}

pub(crate) async fn smart_source_history_ref(
    connection: &mut SqliteConnection,
    reference: &SmartSourceReference,
    now: i64,
    anchor: &str,
    limit: usize,
) -> LibraryResult<Vec<(String, String)>> {
    let Some((key, source, folder)) = resolve_smart_reference(connection, reference).await? else {
        return Ok(Vec::new());
    };
    read_smart_source_window(
        connection,
        key,
        source,
        folder,
        now,
        None,
        limit,
        None,
        Some(anchor),
        true,
    )
    .await
}

#[derive(Clone)]
pub struct SmartPlaylistDetailPage {
    pub summary: SmartPlaylistRow,
    pub tracks: Vec<String>,
    pub first_row_position: usize,
    pub first_rows: Vec<SmartPlaylistTrackRow>,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct SmartPlaylistTrackRow {
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
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SmartPlaylistValueSuggestions {
    pub genres: Vec<String>,
    pub moods: Vec<String>,
}

impl<'row> FromRow<'row, SqliteRow> for SmartPlaylistRow {
    fn from_row(row: &'row SqliteRow) -> Result<Self, sqlx::Error> {
        let definition = row.try_get::<String, _>("definition_json")?;
        Ok(Self {
            smart_playlist_key: row.try_get("smart_playlist_key")?,
            object_id: row.try_get("object_id")?,
            name: row.try_get("name")?,
            definition: serde_json::from_str(&definition)
                .map_err(|error| sqlx::Error::Decode(Box::new(error)))?,
            position: row.try_get("position")?,
            track_count: row.try_get("track_count")?,
            duration_millis: row.try_get("duration_millis")?,
            downloaded_count: 0,
            artwork_bindings: Vec::new(),
        })
    }
}

async fn load_smart_playlist_page(
    connection: &mut SqliteConnection,
    source: Option<SourceKey>,
    folder: Option<FolderKey>,
    sort: SmartPlaylistListSort,
    descending: bool,
    now: i64,
    window: RouteSeedWindow,
) -> LibraryResult<(Vec<SmartPlaylistKey>, usize, Vec<SmartPlaylistRow>)> {
    if folder.is_none()
        && matches!(
            sort,
            SmartPlaylistListSort::Position | SmartPlaylistListSort::Title
        )
    {
        let sql = match (sort, descending) {
            (SmartPlaylistListSort::Position, false) => {
                "SELECT smart_playlist_key FROM smart_playlists ORDER BY position,smart_playlist_key"
            }
            (SmartPlaylistListSort::Position, true) => {
                "SELECT smart_playlist_key FROM smart_playlists ORDER BY position DESC,smart_playlist_key"
            }
            (SmartPlaylistListSort::Title, false) => {
                "SELECT smart_playlist_key FROM smart_playlists ORDER BY normalized_name,smart_playlist_key"
            }
            (SmartPlaylistListSort::Title, true) => {
                "SELECT smart_playlist_key FROM smart_playlists ORDER BY normalized_name DESC,smart_playlist_key"
            }
            _ => unreachable!(),
        };
        let order = sqlx::query_scalar::<_, SmartPlaylistKey>(sql)
            .fetch_all(&mut *connection)
            .await?;
        let seed = window.range(order.len());
        let position = seed.start;
        let rows = load_smart_playlist_rows(connection, source, &order[seed], folder, now).await?;
        return Ok((order, position, rows));
    }
    let sql = format!(
        "{}\n{SMART_LIST_PAGE_SELECT}",
        smart_policy_sql(connection, now).await?
    );
    let mut records = sqlx::query(AssertSqlSafe(sql.as_str()))
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
        .bind(window.relative)
        .bind(window.limit as i64)
        .fetch(connection);
    let mut order = Vec::new();
    let mut rows: Vec<SmartPlaylistRow> = Vec::new();
    while let Some(record) = records.try_next().await? {
        let key = record.try_get("definition_key")?;
        if order.last() != Some(&key) {
            order.push(key);
        }
        if record
            .try_get::<Option<i64>, _>("smart_playlist_key")?
            .is_none()
        {
            continue;
        }
        if rows.last().is_none_or(|row| row.smart_playlist_key != key) {
            let mut row = SmartPlaylistRow::from_row(&record)?;
            normalize_definition(&mut row.definition)?;
            row.downloaded_count = record.try_get("downloaded_count")?;
            rows.push(row);
        }
        if let Some(binding) = record.try_get("artwork_binding")? {
            rows.last_mut().unwrap().artwork_bindings.push(binding);
        }
    }
    let position = window.range(order.len()).start;
    Ok((order, position, rows))
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
    let mut query = QueryBuilder::<Sqlite>::new("WITH requested(smart_playlist_key, ordinal) AS (");
    query.push_values(keys.iter().enumerate(), |mut row, (ordinal, key)| {
        row.push_bind(*key).push_bind(ordinal as i64);
    });
    query.push(
        ") SELECT playlist.smart_playlist_key,playlist.object_id, playlist.name,
                  playlist.definition_json, playlist.position,
                  0 AS track_count, 0 AS duration_millis
           FROM requested JOIN smart_playlists AS playlist USING(smart_playlist_key)",
    );
    query.push(" ORDER BY requested.ordinal");
    let mut result = query
        .build_query_as::<SmartPlaylistRow>()
        .persistent(false)
        .fetch_all(&mut *connection)
        .await?;
    let requested = serde_json::to_string(&keys.iter().map(|key| key.raw()).collect::<Vec<_>>())?;
    let facts_sql = format!(
        "{}\n, smart_stats AS (SELECT definition_key,count(*) track_count,COALESCE(sum(duration_millis),0) duration_millis,count(CASE WHEN EXISTS(SELECT 1 FROM local_access_files access WHERE access.media_uri=selected.media_uri AND access.origin='download') THEN 1 END) downloaded_count FROM selected GROUP BY definition_key), ranked_artwork AS (SELECT definition_key,track.artwork_binding,row_number() OVER (PARTITION BY definition_key ORDER BY result_position) artwork_position FROM selected JOIN tracks track USING(media_uri) WHERE track.artwork_binding IS NOT NULL) SELECT definition.definition_key,COALESCE(stats.track_count,0),COALESCE(stats.duration_millis,0),COALESCE(stats.downloaded_count,0),artwork.artwork_binding FROM definitions definition LEFT JOIN smart_stats stats USING(definition_key) LEFT JOIN ranked_artwork artwork ON artwork.definition_key=definition.definition_key AND artwork.artwork_position<=4 ORDER BY definition.position,definition.definition_key,artwork.artwork_position",
        smart_policy_sql(connection, now).await?
    );
    let facts = sqlx::query_as::<_, (SmartPlaylistKey, i64, i64, i64, Option<Vec<u8>>)>(
        AssertSqlSafe(facts_sql.as_str()),
    )
    .persistent(false)
    .bind(source)
    .bind(now)
    .bind(folder)
    .bind(requested)
    .fetch_all(&mut *connection)
    .await?;
    let mut counts = BTreeMap::new();
    let mut artwork = BTreeMap::<SmartPlaylistKey, Vec<Vec<u8>>>::new();
    for (key, tracks, duration, downloaded, binding) in facts {
        counts.insert(key, (tracks, duration, downloaded));
        if let Some(binding) = binding {
            artwork.entry(key).or_default().push(binding);
        }
    }
    for row in &mut result {
        normalize_definition(&mut row.definition)?;
        if let Some((tracks, duration, downloaded)) = counts.get(&row.smart_playlist_key) {
            row.track_count = *tracks;
            row.duration_millis = *duration;
            row.downloaded_count = *downloaded;
        }
        row.artwork_bindings = artwork.remove(&row.smart_playlist_key).unwrap_or_default();
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
        Database::clear_progress(&mut connection).await?;
        Ok(SmartPlaylistValueSuggestions { genres, moods })
    }

    pub async fn smart_playlist_key_by_object(
        &self,
        object_id: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<SmartPlaylistKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result =
            sqlx::query_scalar("SELECT smart_playlist_key FROM smart_playlists WHERE object_id=?1")
                .bind(object_id)
                .fetch_optional(&mut *connection)
                .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
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
    ) -> LibraryResult<(Vec<SmartPlaylistKey>, usize, Vec<SmartPlaylistRow>)> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let page = load_smart_playlist_page(
            &mut transaction,
            source,
            folder,
            sort,
            descending,
            now,
            window,
        )
        .await?;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(page)
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
        Database::clear_progress(&mut connection).await?;
        Ok(result)
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
    ) -> LibraryResult<bool> {
        let name = require_name(name)?;
        let definition = encode_definition(definition)?;
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(sqlx::query(
            "UPDATE smart_playlists
             SET name=?2, normalized_name=lower(?2), definition_json=?3
             WHERE smart_playlist_key=?1",
        )
        .bind(key)
        .bind(name)
        .bind(definition)
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

    pub async fn smart_playlist_detail(
        &self,
        source: Option<SourceKey>,
        key: SmartPlaylistKey,
        folder: Option<FolderKey>,
        now: i64,
        window: RouteSeedWindow,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<SmartPlaylistDetailPage>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let Some(mut summary) = sqlx::query_as::<_, SmartPlaylistRow>(
            "SELECT smart_playlist_key,object_id,name,definition_json,position,
                    0 AS track_count,0 AS duration_millis
             FROM smart_playlists WHERE smart_playlist_key=?1",
        )
        .bind(key)
        .fetch_optional(&mut *transaction)
        .await?
        else {
            return Ok(None);
        };
        normalize_definition(&mut summary.definition)?;
        let sql = format!(
            "{}\nSELECT selected.media_uri,selected.duration_millis,EXISTS(SELECT 1 FROM local_access_files access WHERE access.media_uri=selected.media_uri AND access.origin='download'),track.artwork_binding FROM selected LEFT JOIN tracks track USING(media_uri) ORDER BY result_position",
            smart_policy_sql(&mut transaction, now).await?
        );
        let mut selected =
            sqlx::query_as::<_, (String, i64, bool, Option<Vec<u8>>)>(AssertSqlSafe(sql.as_str()))
                .persistent(false)
                .bind(source)
                .bind(now)
                .bind(folder)
                .bind(serde_json::to_string(&[key.raw()])?)
                .fetch(&mut *transaction);
        let mut order = Vec::new();
        while let Some((media_uri, duration, downloaded, artwork)) = selected.try_next().await? {
            order.push(media_uri);
            summary.track_count += 1;
            summary.duration_millis += duration;
            summary.downloaded_count += i64::from(downloaded);
            if summary.artwork_bindings.len() < 4
                && let Some(artwork) = artwork
            {
                summary.artwork_bindings.push(artwork);
            }
        }
        drop(selected);
        let seed = window.range(order.len());
        let first_row_position = seed.start;
        let first_rows = load_smart_track_rows(&mut transaction, &order[seed]).await?;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(Some(SmartPlaylistDetailPage {
            summary,
            tracks: order,
            first_row_position,
            first_rows,
        }))
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
            "{}\n{SMART_MEDIA_URI_SELECT}",
            smart_policy_sql(&mut connection, now).await?
        );
        let media_uris = sqlx::query_scalar::<_, String>(AssertSqlSafe(sql.as_str()))
            .persistent(false)
            .bind(source)
            .bind(now)
            .bind(folder)
            .bind(serde_json::to_string(&[key.raw()])?)
            .fetch_all(&mut *connection)
            .await?;
        Database::clear_progress(&mut connection).await?;
        Ok(media_uris)
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
}

async fn load_smart_track_rows(
    connection: &mut SqliteConnection,
    media_uris: &[String],
) -> LibraryResult<Vec<SmartPlaylistTrackRow>> {
    let mut candidates = SMART_CANDIDATES.replace(
        "FROM tracks track",
        "FROM requested JOIN tracks track USING(media_uri)",
    );
    candidates = candidates.replace(
        "FROM playlist_entries entry",
        "FROM playlist_snapshots entry",
    );
    for (table, alias, key, filter, order) in [
        (
            "queue_occurrences",
            "occurrence",
            "queue_occurrence_key",
            "",
            "snapshot_at DESC,queue_occurrence_key DESC",
        ),
        (
            "listens",
            "listen",
            "listen_key",
            "",
            "started_at DESC,listen_key DESC",
        ),
        (
            "local_access_files",
            "access",
            "local_access_file_key",
            "AND origin='download'",
            "mtime_ns DESC,local_access_file_key DESC",
        ),
    ] {
        candidates = candidates.replace(
            &format!("FROM {table} {alias}"),
            &format!("FROM requested JOIN {table} {alias} ON {alias}.{key}=(SELECT {key} FROM {table} WHERE media_uri=requested.media_uri {filter} ORDER BY {order} LIMIT 1)"),
        );
    }
    let sql = format!(
        "WITH definitions(current_scope) AS (VALUES(0)),
              source_scope(source_key,prefix) AS (SELECT NULL,NULL WHERE false),
              requested AS (SELECT DISTINCT value media_uri FROM json_each(?1)),
              {snapshots},
         {candidates}
         SELECT {links} requested.value media_uri,COALESCE(owner.title,requested.value) title,
                COALESCE(owner.display_artist,'') artist,COALESCE(owner.display_album,'') album,
                COALESCE(album.display_artist,entry.album_display_artist,occurrence.album_display_artist) album_display_artist,
                track.artwork_binding,COALESCE(owner.duration_millis,0) duration_millis,
                COALESCE(track.disc_number,entry.disc_number,occurrence.disc_number,listen.disc_number,access.disc_number) disc_number,
                COALESCE(track.track_number,entry.track_number,occurrence.track_number,listen.track_number,access.track_number) track_number,
                owner.year,owner.date_added,owner.bpm,
                COALESCE(track.release_date,entry.release_date,occurrence.release_date,listen.release_date) release_date,
                COALESCE(track.source_format,entry.source_format,occurrence.source_format,listen.source_format) source_format,
                COALESCE(track.musicbrainz_recording_id,entry.musicbrainz_recording_id,occurrence.musicbrainz_recording_id,listen.musicbrainz_recording_id) musicbrainz_recording_id,
                COALESCE(track.musicbrainz_release_track_id,entry.musicbrainz_release_track_id,occurrence.musicbrainz_release_track_id,listen.musicbrainz_release_track_id) musicbrainz_release_track_id,
                COALESCE((SELECT group_concat(genre.name,', ') FROM track_genres credit JOIN genres genre USING(genre_key) WHERE credit.track_key=track.track_key),'') genre,
                COALESCE(state.favorite,owner.source_favorite,0) favorite,
                COALESCE(state.rating,owner.source_rating)/10 rating,
                COALESCE(track.local_play_count,(SELECT count(*) FROM listens played WHERE played.media_uri=requested.value)) play_count,
                max(COALESCE(baseline.last_played_at,0),COALESCE((SELECT max(started_at) FROM listens played WHERE played.media_uri=requested.value),0)) last_played,
                EXISTS(SELECT 1 FROM local_access_files downloaded WHERE downloaded.media_uri=requested.value AND downloaded.origin='download') is_downloaded
         FROM json_each(?1) requested LEFT JOIN media_rows owner ON owner.media_uri=requested.value
         LEFT JOIN tracks track ON track.media_uri=requested.value
         LEFT JOIN albums album USING(album_key)
         LEFT JOIN playlist_snapshots entry ON owner.owner_kind=1 AND entry.playlist_entry_key=-owner.owner_tiebreak
         LEFT JOIN queue_occurrences occurrence ON owner.owner_kind=2 AND occurrence.queue_occurrence_key=-owner.owner_tiebreak
         LEFT JOIN listens listen ON owner.owner_kind=3 AND listen.listen_key=-owner.owner_tiebreak
         LEFT JOIN local_access_files access ON owner.owner_kind=4 AND access.local_access_file_key=-owner.owner_tiebreak
         LEFT JOIN user_media_state state ON state.media_uri=requested.value
         LEFT JOIN activity_baseline baseline ON baseline.source_key=track.source_key AND baseline.track_object_id=track.object_id AND baseline.period='lifetime' AND baseline.item_kind='track'
         ORDER BY requested.key",
         snapshots = crate::playlists::PLAYLIST_URI_SNAPSHOTS,
         links = crate::tracks::TRACK_LINK_COLUMNS,
    );
    sqlx::query(AssertSqlSafe(sql.as_str()))
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
    async fn stable_source_references_survive_catalog_and_definition_key_changes() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(directory.path().join("store.sqlite"))
            .await
            .unwrap();
        let mut writer = database.writer().await.unwrap();
        let connection = writer.as_mut().unwrap();
        sqlx::raw_sql("INSERT INTO sources(source_key,object_id,display_name,normalized_name,catalog_digest,artwork_digest) VALUES(1,'source','Source','source',zeroblob(32),zeroblob(32)); INSERT INTO genres(genre_key,source_key,object_id,name,normalized_name,sort_text) VALUES(1,1,'genre','Genre','genre','genre'); INSERT INTO folders(folder_key,source_key,object_id,name,normalized_name,sort_text) VALUES(1,1,'folder','Folder','folder','folder');").execute(&mut *connection).await.unwrap();
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
        sqlx::raw_sql("DELETE FROM sources; UPDATE smart_playlists SET smart_playlist_key=500; INSERT INTO sources(source_key,object_id,display_name,normalized_name,catalog_digest,artwork_digest) VALUES(99,'source','Source','source',zeroblob(32),zeroblob(32)); INSERT INTO genres(genre_key,source_key,object_id,name,normalized_name,sort_text) VALUES(88,99,'genre','Genre','genre','genre'); INSERT INTO folders(folder_key,source_key,object_id,name,normalized_name,sort_text) VALUES(77,99,'folder','Folder','folder','folder');").execute(&mut *connection).await.unwrap();
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

    #[tokio::test]
    #[ignore = "large isolated Store query verification"]
    async fn sparse_million_track_source_windows_use_indexed_facts() {
        use std::sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        };
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(directory.path().join("store.sqlite"))
            .await
            .unwrap();
        let mut writer = database.writer().await.unwrap();
        let connection = writer.as_mut().unwrap();
        let source=sqlx::query_scalar::<_,SourceKey>("INSERT INTO sources(object_id,display_name,normalized_name,catalog_digest,artwork_digest) VALUES('source','Source','source',zeroblob(32),zeroblob(32)) RETURNING source_key").fetch_one(&mut *connection).await.unwrap();
        let definition = SmartPlaylistDefinition {
            current: true,
            match_all: vec![SmartPlaylistRule {
                field: SmartPlaylistRuleField::Played,
                operator: SmartPlaylistRuleOperator::Is,
                value: Some(SmartPlaylistRuleValue::Bool(false)),
            }],
            ..SmartPlaylistDefinition::default()
        };
        let key=sqlx::query_scalar::<_,SmartPlaylistKey>("INSERT INTO smart_playlists(object_id,name,normalized_name,definition_json,position) VALUES('never','Never','never',?1,0) RETURNING smart_playlist_key").bind(serde_json::to_string(&definition).unwrap()).fetch_one(&mut *connection).await.unwrap();
        let mut previous = 0;
        for size in [300_000, 1_000_000] {
            sqlx::query("WITH RECURSIVE n(i) AS(VALUES(?2) UNION ALL SELECT i+1 FROM n WHERE i<?3) INSERT INTO tracks(track_key,source_key,object_id,media_uri,title,normalized_search,display_album,display_artist,sort_text,duration_millis,local_play_count) SELECT i,?1,'track-'||i,'file:///track-'||printf('%07d',i),'Track','track','Album','Artist',printf('%07d',i),1000,1 FROM n").bind(source).bind(previous+1).bind(size).execute(&mut *connection).await.unwrap();
            sqlx::query("UPDATE tracks SET local_play_count=0 WHERE track_key>?1")
                .bind(size - 100)
                .execute(&mut *connection)
                .await
                .unwrap();
            sqlx::query("INSERT INTO listens(media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis,skipped) SELECT media_uri,title,display_artist,display_album,1700000000,'2023-11',1000,1000,0 FROM tracks WHERE track_key>?1 AND local_play_count>0").bind(previous).execute(&mut *connection).await.unwrap();
            previous = size;
            for empty in [false, true] {
                if empty {
                    sqlx::query("UPDATE tracks SET local_play_count=1 WHERE local_play_count=0")
                        .execute(&mut *connection)
                        .await
                        .unwrap();
                }
                for shuffle in [None, Some(192837)] {
                    let ticks = Arc::new(AtomicU64::new(0));
                    let count = ticks.clone();
                    connection
                        .lock_handle()
                        .await
                        .unwrap()
                        .set_progress_handler(100, move || {
                            count.fetch_add(1, Ordering::Relaxed);
                            true
                        });
                    let started = std::time::Instant::now();
                    let rows = smart_source_window(
                        connection,
                        key,
                        Some(source),
                        None,
                        2000000000,
                        None,
                        100,
                        shuffle,
                        None,
                    )
                    .await
                    .unwrap();
                    let next = smart_source_window(
                        connection,
                        key,
                        Some(source),
                        None,
                        2000000000,
                        rows.last().map(|pair| pair.1.as_str()),
                        100,
                        shuffle,
                        None,
                    )
                    .await
                    .unwrap();
                    let elapsed = started.elapsed();
                    connection
                        .lock_handle()
                        .await
                        .unwrap()
                        .remove_progress_handler();
                    let steps = ticks.load(Ordering::Relaxed) * 100;
                    tracing::info!(
                        "actual Smart reader tracks={size} empty={empty} shuffle={shuffle:?}: {elapsed:?}, ~{steps} VM instructions including continuation"
                    );
                    assert_eq!(rows.len(), if empty { 0 } else { 100 });
                    assert!(next.is_empty());
                    assert!(
                        steps < 50_000,
                        "sparse source read traversed unrelated catalog rows"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn indexed_local_activity_continuation_preserves_rules_and_limits() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(directory.path().join("store.sqlite"))
            .await
            .unwrap();
        let mut writer = database.writer().await.unwrap();
        let connection = writer.as_mut().unwrap();
        let source = sqlx::query_scalar::<_,SourceKey>("INSERT INTO sources(object_id,display_name,normalized_name,catalog_digest,artwork_digest) VALUES('source','Source','source',zeroblob(32),zeroblob(32)) RETURNING source_key").fetch_one(&mut *connection).await.unwrap();
        sqlx::query("WITH RECURSIVE n(i) AS(VALUES(0) UNION ALL SELECT i+1 FROM n WHERE i<299) INSERT INTO tracks(source_key,object_id,media_uri,title,normalized_search,display_album,display_artist,sort_text,duration_millis,local_play_count) SELECT ?1,'track-'||i,'file:///track-'||printf('%03d',i),'Track '||i,'track','Album','Artist',printf('%03d',i),1000,CASE WHEN i%3=0 THEN 0 ELSE 1 END FROM n").bind(source).execute(&mut *connection).await.unwrap();
        let definition = SmartPlaylistDefinition {
            current: true,
            match_all: vec![SmartPlaylistRule {
                field: SmartPlaylistRuleField::Played,
                operator: SmartPlaylistRuleOperator::Is,
                value: Some(SmartPlaylistRuleValue::Bool(false)),
            }],
            limit: Some(73),
            ..SmartPlaylistDefinition::default()
        };
        let key=sqlx::query_scalar::<_,SmartPlaylistKey>("INSERT INTO smart_playlists(object_id,name,normalized_name,definition_json,position) VALUES('test','Test','test',?1,0) RETURNING smart_playlist_key").bind(serde_json::to_string(&definition).unwrap()).fetch_one(&mut *connection).await.unwrap();
        let mut entries = Vec::new();
        loop {
            let page = smart_source_window(
                connection,
                key,
                Some(source),
                None,
                2000000000,
                entries
                    .last()
                    .map(|pair: &(String, String)| pair.1.as_str()),
                17,
                None,
                None,
            )
            .await
            .unwrap();
            if page.is_empty() {
                break;
            }
            entries.extend(page);
        }
        assert_eq!(entries.len(), 73);
        assert_eq!(
            entries
                .iter()
                .map(|pair| pair.0.clone())
                .collect::<Vec<_>>(),
            (0..73)
                .map(|i| format!("file:///track-{:03}", i * 3))
                .collect::<Vec<_>>()
        );
        for seed in [0, 1, 2147483646] {
            let mut shuffled = Vec::new();
            loop {
                let page = smart_source_window(
                    connection,
                    key,
                    Some(source),
                    None,
                    2000000000,
                    shuffled
                        .last()
                        .map(|pair: &(String, String)| pair.1.as_str()),
                    17,
                    Some(seed),
                    None,
                )
                .await
                .unwrap();
                if page.is_empty() {
                    break;
                }
                shuffled.extend(page);
            }
            assert_eq!(shuffled.len(), 73);
            let mut actual = shuffled.into_iter().map(|pair| pair.0).collect::<Vec<_>>();
            actual.sort();
            assert_eq!(
                actual,
                entries
                    .iter()
                    .map(|pair| pair.0.clone())
                    .collect::<Vec<_>>()
            );
        }
        let anchored = smart_source_window(
            connection,
            key,
            Some(source),
            None,
            2000000000,
            None,
            100,
            None,
            Some("file:///track-210"),
        )
        .await
        .unwrap();
        assert_eq!(
            anchored
                .iter()
                .map(|pair| pair.0.as_str())
                .collect::<Vec<_>>(),
            [
                "file:///track-210",
                "file:///track-213",
                "file:///track-216"
            ]
        );
        let reference = smart_source_reference(connection, key, Some(source), None)
            .await
            .unwrap()
            .unwrap();
        let history =
            smart_source_history_ref(connection, &reference, 2000000000, "file:///track-210", 11)
                .await
                .unwrap();
        assert_eq!(
            history.iter().map(|row| row.0.clone()).collect::<Vec<_>>(),
            (60..=70)
                .rev()
                .map(|i| format!("file:///track-{:03}", i * 3))
                .collect::<Vec<_>>()
        );
        let anchored_shuffle = smart_source_window(
            connection,
            key,
            Some(source),
            None,
            2000000000,
            None,
            100,
            Some(1),
            Some("file:///track-210"),
        )
        .await
        .unwrap();
        assert_eq!(anchored_shuffle.first().unwrap().0, "file:///track-210");
        assert_eq!(anchored_shuffle.len(), 73);
        assert_eq!(
            smart_source_last(connection, key, Some(source), None, 2000000000, None)
                .await
                .unwrap()
                .unwrap()
                .0,
            "file:///track-216"
        );
        let shuffled = smart_source_window(
            connection,
            key,
            Some(source),
            None,
            2000000000,
            None,
            100,
            Some(1),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            smart_source_last(connection, key, Some(source), None, 2000000000, Some(1))
                .await
                .unwrap()
                .unwrap()
                .0,
            shuffled.last().unwrap().0
        );
        for sort in SmartPlaylistSort::ALL {
            for descending in [false, true] {
                let mut definition = definition.clone();
                definition.sort_field = sort;
                definition.descending = descending;
                sqlx::query(
                    "UPDATE smart_playlists SET definition_json=?1 WHERE smart_playlist_key=?2",
                )
                .bind(serde_json::to_string(&definition).unwrap())
                .bind(key)
                .execute(&mut *connection)
                .await
                .unwrap();
                let policy = smart_policy_sql(connection, 2000000000).await.unwrap();
                let full = sqlx::query_scalar::<_, String>(AssertSqlSafe(format!(
                    "{policy} SELECT media_uri FROM selected ORDER BY result_position"
                )))
                .bind(source)
                .bind(2000000000_i64)
                .bind(Option::<i64>::None)
                .bind(serde_json::to_string(&[key.raw()]).unwrap())
                .fetch_all(&mut *connection)
                .await
                .unwrap();
                let mut pages = Vec::new();
                loop {
                    let page = smart_source_window(
                        connection,
                        key,
                        Some(source),
                        None,
                        2000000000,
                        pages.last().map(|pair: &(String, String)| pair.1.as_str()),
                        17,
                        None,
                        None,
                    )
                    .await
                    .unwrap();
                    if page.is_empty() {
                        break;
                    }
                    pages.extend(page);
                }
                assert_eq!(
                    pages.into_iter().map(|pair| pair.0).collect::<Vec<_>>(),
                    full,
                    "{sort:?} descending={descending}"
                );
            }
        }
    }
}

fn source_fact(
    field: SmartPlaylistRuleField,
    definition: &SmartPlaylistDefinition,
    catalog: bool,
    now: i64,
) -> String {
    use SmartPlaylistRuleField as F;
    let period = match definition.activity_period {
        SmartPlaylistActivityPeriod::Weekly => Some(604800),
        SmartPlaylistActivityPeriod::Monthly => Some(2592000),
        SmartPlaylistActivityPeriod::Yearly => Some(31536000),
        SmartPlaylistActivityPeriod::Lifetime => None,
    };
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

fn fallback_candidates(definition: &SmartPlaylistDefinition) -> String {
    use SmartPlaylistRuleField as F;
    use SmartPlaylistRuleOperator as O;
    use SmartPlaylistRuleValue as V;
    let zero = |rule: &SmartPlaylistRule| {
        matches!(
            (rule.field, rule.operator, &rule.value),
            (F::Played, O::Is, Some(V::Bool(false))) | (F::Played, O::IsNot, Some(V::Bool(true)))
        ) || (definition.activity_period == SmartPlaylistActivityPeriod::Lifetime
            && matches!(
                (rule.field, rule.operator, &rule.value),
                (F::PlayCount, O::Equals, Some(V::Number(0)))
            ))
    };
    let unplayed = definition.match_all.iter().any(zero)
        || (!definition.match_any.is_empty() && definition.match_any.iter().all(zero));
    let mut candidates = SMART_CANDIDATES.replace(
        "FROM tracks track\n    WHERE",
        "FROM tracks track\n    WHERE 0 AND (",
    );
    candidates = candidates.replacen(
        "OR track.source_key=?1\n    UNION ALL",
        "OR track.source_key=?1)\n    UNION ALL",
        1,
    );
    for (table, alias) in [
        ("playlist_entries", "entry"),
        ("queue_occurrences", "occurrence"),
        ("listens", "listen"),
        ("local_access_files", "access"),
    ] {
        // A listen-owned entry has a nonzero local count. Eliminate that owner
        // for zero-count predicates before touching the listening history.
        let filter = if table == "listens" && unplayed {
            "0"
        } else {
            "NOT EXISTS(SELECT 1 FROM tracks WHERE media_uri=fallback.media_uri)"
        };
        candidates = candidates.replace(
            &format!("FROM {table} {alias}"),
            &format!("FROM (SELECT * FROM {table} fallback WHERE {filter}) {alias}"),
        );
    }
    candidates
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

/// Read a bounded continuation directly from the source's facts. The cursor holds
/// only the last ordering values and consumed definition limit, never membership.
pub(crate) async fn smart_source_window(
    connection: &mut SqliteConnection,
    key: SmartPlaylistKey,
    source: Option<SourceKey>,
    folder: Option<FolderKey>,
    now: i64,
    after: Option<&str>,
    limit: usize,
    shuffle_seed: Option<u64>,
    anchor_uri: Option<&str>,
) -> LibraryResult<Vec<(String, String)>> {
    read_smart_source_window(
        connection,
        key,
        source,
        folder,
        now,
        after,
        limit,
        shuffle_seed,
        anchor_uri,
        false,
    )
    .await
}

pub(crate) async fn smart_source_last(
    connection: &mut SqliteConnection,
    key: SmartPlaylistKey,
    source: Option<SourceKey>,
    folder: Option<FolderKey>,
    now: i64,
    shuffle_seed: Option<u64>,
) -> LibraryResult<Option<(String, String)>> {
    Ok(read_smart_source_window(
        connection,
        key,
        source,
        folder,
        now,
        None,
        1,
        shuffle_seed,
        None,
        true,
    )
    .await?
    .pop())
}

async fn read_smart_source_window(
    connection: &mut SqliteConnection,
    key: SmartPlaylistKey,
    source: Option<SourceKey>,
    folder: Option<FolderKey>,
    now: i64,
    after: Option<&str>,
    limit: usize,
    shuffle_seed: Option<u64>,
    anchor_uri: Option<&str>,
    last: bool,
) -> LibraryResult<Vec<(String, String)>> {
    let Some(json) = sqlx::query_scalar::<_, String>(
        "SELECT definition_json FROM smart_playlists WHERE smart_playlist_key=?1",
    )
    .bind(key)
    .fetch_optional(&mut *connection)
    .await?
    else {
        return Ok(Vec::new());
    };
    let definition: SmartPlaylistDefinition = serde_json::from_str(&json)?;
    let cursor = after
        .map(serde_json::from_str::<serde_json::Value>)
        .transpose()?;
    let consumed = cursor
        .as_ref()
        .and_then(|cursor| cursor.get(3).and_then(serde_json::Value::as_u64))
        .unwrap_or(0) as usize;
    let limit = limit.min(100).min(
        definition
            .limit
            .map(|max| max.saturating_sub(consumed))
            .unwrap_or(100),
    );
    if limit == 0 || (definition.current && source.is_none()) {
        return Ok(Vec::new());
    }
    let candidates = fallback_candidates(&definition);
    let sources = sqlx::query_as::<_, (i64, String)>("SELECT source_key,object_id FROM sources")
        .fetch_all(&mut *connection)
        .await?;
    let scope = if sources.is_empty() {
        "SELECT NULL,NULL WHERE 0".into()
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
    let scope_filter = if definition.current {
        "owner.source_key=?1 AND (?3 IS NULL OR EXISTS(SELECT 1 FROM track_folders WHERE track_key=owner.track_key AND folder_key=?3))"
    } else {
        "1"
    };
    let direction = if definition.descending { "DESC" } else { "ASC" };
    let header = format!(
        "WITH parameters AS (SELECT ?1,?2,?3,?4,?5,?6,?7,?8),definitions(current_scope) AS (VALUES({})),source_scope(source_key,prefix) AS ({scope}),{candidates}",
        i32::from(definition.current)
    );
    let branch = |catalog: bool, extra: &str| {
        let table = if catalog { "tracks" } else { "media_rows" };
        format!(
            "SELECT media_uri,sort_text,{} sort_value FROM {table} owner WHERE {scope_filter} AND {} AND ({extra})",
            source_sort(&definition, catalog, now),
            source_rules(&definition, catalog, now)
        )
    };
    let mut boundary = cursor
        .as_ref()
        .and_then(|value| value.get(6))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    if after.is_none() && (shuffle_seed.is_some() || anchor_uri.is_some() || last) {
        if let Some(max) = definition.limit {
            let sql = format!(
                "{header} SELECT json_array(sort_value,sort_text,media_uri) FROM ({} UNION ALL {}) ORDER BY sort_value {direction} NULLS LAST,sort_text,media_uri LIMIT 1 OFFSET {}",
                branch(true, "1"),
                branch(false, "1"),
                max - 1
            );
            if let Some(json) = sqlx::query_scalar::<_, String>(AssertSqlSafe(sql))
                .persistent(false)
                .bind(source)
                .bind(now)
                .bind(folder)
                .fetch_optional(&mut *connection)
                .await?
            {
                boundary = serde_json::from_str(&json)?;
            }
        }
    }
    let boundary_filter = if boundary.is_null() {
        "1".into()
    } else {
        format!("NOT ({})", source_after(&definition, "?7", false))
    };
    let anchor = if after.is_none() {
        if let Some(uri) = anchor_uri {
            let sql = format!(
                "{header} SELECT json_array(sort_value,sort_text,media_uri),catalog FROM (SELECT *,1 catalog FROM ({}) UNION ALL SELECT *,0 catalog FROM ({})) LIMIT 1",
                branch(true, "owner.media_uri=?6"),
                branch(false, "owner.media_uri=?6")
            );
            sqlx::query_as::<_, (String, bool)>(AssertSqlSafe(sql))
                .persistent(false)
                .bind(source)
                .bind(now)
                .bind(folder)
                .bind(Option::<&str>::None)
                .bind(limit as i64)
                .bind(uri)
                .fetch_optional(&mut *connection)
                .await?
        } else {
            None
        }
    } else {
        None
    };
    let mut output = Vec::new();
    if let Some(seed) = shuffle_seed {
        let catalog_first = cursor
            .as_ref()
            .and_then(|value| value.get(7))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(anchor.as_ref().is_none_or(|(_, catalog)| *catalog));
        let mut pivot = cursor
            .as_ref()
            .and_then(|value| value.get(5))
            .cloned()
            .unwrap_or_else(|| serde_json::json!([seed % 2147483647, ""]));
        if after.is_none() {
            if let Some(uri) = anchor_uri.filter(|_| anchor.is_some()) {
                pivot = if catalog_first {
                    let key = sqlx::query_scalar::<_, i64>(
                        "SELECT (track_key*1103515245)%2147483647 FROM tracks WHERE media_uri=?1",
                    )
                    .bind(uri)
                    .fetch_one(&mut *connection)
                    .await?;
                    serde_json::json!([key, uri])
                } else {
                    serde_json::json!([uri, uri])
                };
            }
        }
        let first_phase = cursor
            .as_ref()
            .and_then(|value| value.get(4))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let phases: Vec<_> = if last {
            (first_phase..4).rev().collect()
        } else {
            (first_phase..4).collect()
        };
        for phase in phases {
            let catalog = (phase < 2) == catalog_first;
            let table = if catalog { "tracks" } else { "media_rows" };
            let order = if catalog {
                "(owner.track_key*1103515245)%2147483647"
            } else {
                "owner.media_uri"
            };
            let phase_pivot = if phase < 2 {
                pivot.clone()
            } else if catalog {
                serde_json::json!([seed % 2147483647, ""])
            } else {
                serde_json::json!(["", ""])
            };
            let phase_filter = format!(
                "({order},owner.media_uri){}(json_extract(?8,'$[0]'),json_extract(?8,'$[1]'))",
                if phase % 2 == 0 { ">=" } else { "<" }
            );
            let continuation = if phase == first_phase && after.is_some() {
                format!(
                    "AND ({order},owner.media_uri)>(json_extract(?4,'$[0]'),json_extract(?4,'$[2]'))"
                )
            } else {
                String::new()
            };
            let direction = if last { "DESC" } else { "ASC" };
            let sql = format!(
                "{header} SELECT media_uri,json_array(shuffle_value,sort_text,media_uri) FROM (SELECT owner.media_uri,owner.sort_text,{} sort_value,{order} shuffle_value FROM {table} owner WHERE {scope_filter} AND {} AND ({boundary_filter}) AND {phase_filter} {continuation} ORDER BY shuffle_value {direction},owner.media_uri {direction} LIMIT ?5)",
                source_sort(&definition, catalog, now),
                source_rules(&definition, catalog, now)
            );
            let rows = sqlx::query_as::<_, (String, String)>(AssertSqlSafe(sql))
                .persistent(false)
                .bind(source)
                .bind(now)
                .bind(folder)
                .bind(after)
                .bind((limit - output.len()) as i64)
                .bind(anchor_uri)
                .bind(boundary.to_string())
                .bind(phase_pivot.to_string())
                .fetch_all(&mut *connection)
                .await?;
            for (uri, json) in rows {
                let mut position: Vec<serde_json::Value> = serde_json::from_str(&json)?;
                position.extend([
                    serde_json::json!(consumed + output.len() + 1),
                    serde_json::json!(phase),
                    pivot.clone(),
                    boundary.clone(),
                    serde_json::json!(catalog_first),
                ]);
                output.push((uri, serde_json::to_string(&position)?));
            }
            if output.len() == limit {
                break;
            }
        }
    } else {
        let direction = if last {
            if definition.descending { "ASC" } else { "DESC" }
        } else {
            direction
        };
        let nulls = if last { "FIRST" } else { "LAST" };
        let tie_direction = if last { "DESC" } else { "ASC" };
        let position = after.or_else(|| anchor.as_ref().map(|(json, _)| json.as_str()));
        let continuation = if position.is_none() {
            "1".into()
        } else if last {
            format!("NOT ({})", source_after(&definition, "?4", false))
        } else {
            source_after(&definition, "?4", after.is_none())
        };
        let branches=[true,false].into_iter().map(|catalog|format!("SELECT * FROM ({} ORDER BY sort_value {direction} NULLS {nulls},sort_text {tie_direction},media_uri {tie_direction} LIMIT ?5)",branch(catalog,&format!("({boundary_filter}) AND ({continuation})")))).collect::<Vec<_>>().join(" UNION ALL ");
        let sql = format!(
            "{header} SELECT media_uri,json_array(sort_value,sort_text,media_uri) FROM ({branches}) ORDER BY sort_value {direction} NULLS {nulls},sort_text {tie_direction},media_uri {tie_direction} LIMIT ?5"
        );
        let rows = sqlx::query_as::<_, (String, String)>(AssertSqlSafe(sql))
            .persistent(false)
            .bind(source)
            .bind(now)
            .bind(folder)
            .bind(position)
            .bind(limit as i64)
            .bind(anchor_uri)
            .bind(boundary.to_string())
            .fetch_all(&mut *connection)
            .await?;
        for (uri, json) in rows {
            let mut position: Vec<serde_json::Value> = serde_json::from_str(&json)?;
            position.extend([
                serde_json::json!(consumed + output.len() + 1),
                serde_json::Value::Null,
                serde_json::Value::Null,
                boundary.clone(),
            ]);
            output.push((uri, serde_json::to_string(&position)?));
        }
    }
    Ok(output)
}

fn source_after(definition: &SmartPlaylistDefinition, parameter: &str, inclusive: bool) -> String {
    let equal = if inclusive { "=" } else { "" };
    if definition.sort_field == SmartPlaylistSort::Title && !definition.descending {
        return format!(
            "(sort_text,media_uri)>{equal}(json_extract({parameter},'$[1]'),json_extract({parameter},'$[2]'))"
        );
    }
    let comparison = if definition.descending { "<" } else { ">" };
    format!(
        "((sort_value IS NULL AND json_extract({parameter},'$[0]') IS NOT NULL) OR (sort_value IS NOT NULL AND json_extract({parameter},'$[0]') IS NOT NULL AND sort_value{comparison}json_extract({parameter},'$[0]')) OR (sort_value IS json_extract({parameter},'$[0]') AND (sort_text,media_uri)>{equal}(json_extract({parameter},'$[1]'),json_extract({parameter},'$[2]'))))"
    )
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

const SMART_CANDIDATES: &str = r#"
owner_rows(
    media_uri,owner_rank,owner_order,owner_tiebreak,source_key,track_key,object_id,
    title,display_artist,display_album,duration_millis,year,date_added,comment,bpm,
    source_rating,source_favorite,sort_text,owner_kind
) AS (
    SELECT track.media_uri,0,0,track.track_key,track.source_key,track.track_key,track.object_id,
           track.title,track.display_artist,track.display_album,track.duration_millis,
           track.year,track.date_added,track.comment,track.bpm,track.source_rating,
           track.source_favorite,track.sort_text,0
    FROM tracks track
    WHERE EXISTS(SELECT 1 FROM definitions WHERE current_scope=0)
       OR track.source_key=?1
    UNION ALL
    SELECT entry.media_uri,CASE WHEN entry.title IS NULL THEN 2 ELSE 1 END,
           -entry.snapshot_at,-entry.playlist_entry_key,source.source_key,NULL,NULL,
           COALESCE(entry.title,''),COALESCE(entry.artist,''),COALESCE(entry.album,''),
           COALESCE(entry.duration_millis,0),entry.year,NULL,NULL,NULL,NULL,NULL,
           lower(COALESCE(entry.title,'')),1
    FROM playlist_entries entry
    LEFT JOIN source_scope source ON substr(entry.media_uri,1,length(source.prefix))=source.prefix
    WHERE EXISTS(SELECT 1 FROM definitions WHERE current_scope=0) OR source.source_key=?1
    UNION ALL
    SELECT occurrence.media_uri,1,-occurrence.snapshot_at,-occurrence.queue_occurrence_key,
           source.source_key,NULL,NULL,
           occurrence.title,occurrence.artist,occurrence.album,occurrence.duration_millis,
           occurrence.year,NULL,NULL,NULL,NULL,NULL,lower(occurrence.title),2
    FROM queue_occurrences occurrence
    LEFT JOIN source_scope source ON substr(occurrence.media_uri,1,length(source.prefix))=source.prefix
    WHERE EXISTS(SELECT 1 FROM definitions WHERE current_scope=0) OR source.source_key=?1
    UNION ALL
    SELECT listen.media_uri,1,-listen.started_at,-listen.listen_key,source.source_key,NULL,NULL,
           listen.track_title,listen.artist_name,listen.album_title,listen.duration_millis,
           listen.year,NULL,NULL,NULL,NULL,NULL,lower(listen.track_title),3
    FROM listens listen
    LEFT JOIN source_scope source ON substr(listen.media_uri,1,length(source.prefix))=source.prefix
    WHERE EXISTS(SELECT 1 FROM definitions WHERE current_scope=0) OR source.source_key=?1
    UNION ALL
    SELECT access.media_uri,1,-(access.mtime_ns/1000000000),-access.local_access_file_key,
           source.source_key,NULL,NULL,
           access.title,access.artist,access.album,access.duration_millis,NULL,
           NULL,NULL,NULL,NULL,NULL,lower(access.title),4
    FROM local_access_files access
    LEFT JOIN source_scope source ON substr(access.media_uri,1,length(source.prefix))=source.prefix
    WHERE access.origin='download'
      AND (EXISTS(SELECT 1 FROM definitions WHERE current_scope=0) OR source.source_key=?1)
),
preferred_owner_rows AS (
    SELECT owner_rows.*,
           row_number() OVER (
             PARTITION BY media_uri
             ORDER BY owner_rank,owner_order,owner_tiebreak
           ) owner_position
    FROM owner_rows
),
media_rows AS (
    SELECT * FROM preferred_owner_rows WHERE owner_position=1
)
"#;

async fn smart_policy_sql(connection: &mut SqliteConnection, now: i64) -> LibraryResult<String> {
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
    let definitions=sqlx::query_as::<_,(i64,String)>("SELECT smart_playlist_key,definition_json FROM smart_playlists ORDER BY smart_playlist_key").fetch_all(connection).await?;
    let mut sql = format!(
        "WITH parameters AS (SELECT ?1,?2,?3,?4),source_scope(source_key,prefix) AS ({scope}),definitions AS (SELECT smart_playlist_key definition_key,position,normalized_name,COALESCE(json_extract(definition_json,'$.current'),0) current_scope FROM smart_playlists WHERE ?4 IS NULL OR smart_playlist_key IN(SELECT value FROM json_each(?4)))"
    );
    let mut selected = Vec::new();
    for (key, json) in definitions {
        let definition: SmartPlaylistDefinition = serde_json::from_str(&json)?;
        let admitted = format!("EXISTS(SELECT 1 FROM definitions WHERE definition_key={key})");
        let candidates = fallback_candidates(&definition)
            .replace(
                "preferred_owner_rows",
                &format!("preferred_owner_rows_{key}"),
            )
            .replace("owner_rows(", &format!("owner_rows_{key}("))
            .replace("owner_rows.*", &format!("owner_rows_{key}.*"))
            .replace("FROM owner_rows", &format!("FROM owner_rows_{key}"))
            .replace("media_rows", &format!("media_rows_{key}"))
            .replace(
                "fallback WHERE ",
                &format!("fallback WHERE {admitted} AND "),
            );
        sql.push_str(&format!(",{candidates}"));
        let scope = if definition.current {
            "owner.source_key=?1 AND (?3 IS NULL OR EXISTS(SELECT 1 FROM track_folders WHERE track_key=owner.track_key AND folder_key=?3))"
        } else {
            "1"
        };
        let branches=[true,false].into_iter().map(|catalog| {
            let table=if catalog {"tracks".to_string()} else {format!("media_rows_{key}")};
            format!("SELECT {key} definition_key,owner.media_uri,owner.title,owner.display_artist,owner.display_album,owner.duration_millis,owner.year,owner.sort_text,{} sort_value FROM {table} owner WHERE {admitted} AND ({scope}) AND ({})",source_sort(&definition,catalog,now),source_rules(&definition,catalog,now))
        }).collect::<Vec<_>>().join(" UNION ALL ");
        let direction = if definition.descending { "DESC" } else { "ASC" };
        sql.push_str(&format!(",result_{key} AS (SELECT *,row_number() OVER(ORDER BY sort_value {direction} NULLS LAST,sort_text,media_uri) result_position FROM ({branches}))"));
        selected.push(format!(
            "SELECT * FROM result_{key}{}",
            definition
                .limit
                .map(|limit| format!(" WHERE result_position<={limit}"))
                .unwrap_or_default()
        ));
    }
    if selected.is_empty() {
        selected.push("SELECT NULL definition_key,NULL media_uri,NULL title,NULL display_artist,NULL display_album,0 duration_millis,NULL year,NULL sort_text,NULL sort_value,0 result_position WHERE 0".into());
    }
    sql.push_str(&format!(
        ",selected AS MATERIALIZED ({})",
        selected.join(" UNION ALL ")
    ));
    Ok(sql)
}

const SMART_MEDIA_URI_SELECT: &str = r#"
SELECT media_uri
FROM selected
ORDER BY result_position
"#;

const SMART_LIST_PAGE_SELECT: &str = r#"
, smart_stats AS (
    SELECT definition.definition_key,
           definition.current_scope,
           definition.position,
           definition.normalized_name,
           count(selected.media_uri) AS track_count,
           COALESCE(sum(selected.duration_millis), 0) AS duration_millis
    FROM definitions AS definition
    LEFT JOIN selected USING(definition_key)
    GROUP BY definition.definition_key
), ordered AS (
SELECT *, count(*) OVER() total, row_number() OVER (ORDER BY
  CASE WHEN ?5=0 AND ?6=0 THEN position END ASC,
  CASE WHEN ?5=0 AND ?6=1 THEN position END DESC,
  CASE WHEN ?5=1 AND ?6=0 THEN normalized_name END ASC,
  CASE WHEN ?5=1 AND ?6=1 THEN normalized_name END DESC,
  CASE WHEN ?5=2 AND ?6=0 THEN track_count END ASC,
  CASE WHEN ?5=2 AND ?6=1 THEN track_count END DESC,
  CASE WHEN ?5=3 AND ?6=0 THEN duration_millis END ASC,
  CASE WHEN ?5=3 AND ?6=1 THEN duration_millis END DESC,
  position, definition_key)-1 row_position
FROM smart_stats
WHERE current_scope=0 OR ?3 IS NULL OR track_count>0
), seed AS (
  SELECT * FROM ordered
  WHERE row_position>=CAST(round((total-1)*?7) AS INTEGER)/?8*?8
    AND row_position<CAST(round((total-1)*?7) AS INTEGER)/?8*?8+?8
), seed_downloads AS (
  SELECT definition_key,count(CASE WHEN EXISTS(
    SELECT 1 FROM local_access_files access
    WHERE access.media_uri=selected.media_uri AND access.origin='download'
  ) THEN 1 END) downloaded_count
  FROM selected JOIN seed USING(definition_key) GROUP BY definition_key
), ranked_artwork AS (
  SELECT definition_key,track.artwork_binding,
    row_number() OVER (PARTITION BY definition_key ORDER BY result_position) artwork_position
  FROM selected JOIN seed USING(definition_key) JOIN tracks track USING(media_uri)
  WHERE track.artwork_binding IS NOT NULL
)
SELECT ordered.definition_key,playlist.smart_playlist_key,playlist.object_id,playlist.name,
  playlist.definition_json,playlist.position,seed.track_count,seed.duration_millis,
  COALESCE(downloads.downloaded_count,0) downloaded_count,artwork.artwork_binding
FROM ordered LEFT JOIN seed USING(definition_key)
LEFT JOIN smart_playlists playlist ON playlist.smart_playlist_key=seed.definition_key
LEFT JOIN seed_downloads downloads ON downloads.definition_key=seed.definition_key
LEFT JOIN ranked_artwork artwork ON artwork.definition_key=seed.definition_key AND artwork.artwork_position<=4
ORDER BY ordered.row_position,artwork.artwork_position
"#;

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct SmartPlaylistWrite {
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
        "SELECT object_id,name,definition_json,position FROM smart_playlists ORDER BY position",
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
    sqlx::query("INSERT INTO smart_playlists(object_id,name,normalized_name,definition_json,position)
       VALUES(?1,?2,lower(?2),?3,CASE WHEN EXISTS(SELECT 1 FROM smart_playlists WHERE position=?4)
       THEN (SELECT COALESCE(max(position)+1,0) FROM smart_playlists) ELSE ?4 END)
       ON CONFLICT(object_id) DO UPDATE SET name=excluded.name,normalized_name=excluded.normalized_name,
       definition_json=excluded.definition_json")
       .bind(&record.object_id).bind(&record.name).bind(&record.definition_json).bind(record.position)
       .execute(connection).await?;
    Ok(())
}

impl Database {
    pub async fn export_smart_playlist_m3u(
        &self,
        key: SmartPlaylistKey,
        source: Option<SourceKey>,
        folder: Option<FolderKey>,
        now: i64,
        file: &std::path::Path,
        mut output: impl std::io::Write,
    ) -> LibraryResult<u64> {
        use futures_util::TryStreamExt;
        let (_permit, mut connection) = self.acquire_general(&ReadCancellation::new()).await?;
        output.write_all(b"#EXTM3U\n")?;
        let policy = smart_policy_sql(&mut connection, now).await?;
        let sql = format!(
            "{policy} SELECT 'm3u:' || result_position object_id,media_uri,title,display_artist artist,display_album album,NULL album_display_artist,0 snapshot_at,duration_millis,NULL disc_number,NULL track_number,year,NULL release_date,NULL source_format,NULL musicbrainz_recording_id,NULL musicbrainz_release_track_id,result_position-1 position FROM selected ORDER BY result_position"
        );
        let mut rows = sqlx::query_as::<_, crate::PlaylistEntryWrite>(AssertSqlSafe(sql.as_str()))
            .persistent(false)
            .bind(source)
            .bind(now)
            .bind(folder)
            .bind(serde_json::to_string(&[key.raw()])?)
            .fetch(&mut *connection);
        let mut count = 0;
        while let Some(entry) = rows.try_next().await? {
            if crate::m3u::write_m3u_entry(&mut output, file, &entry)? {
                count += 1;
            }
        }
        Ok(count)
    }
}
