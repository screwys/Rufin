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
        smart_policy_sql(connection).await?
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
        smart_policy_sql(connection).await?
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
            smart_policy_sql(&mut transaction).await?
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

    /// Materialize the selected SQL relation directly into Queue's input table.
    /// No hydrated rows or complete URI vector cross the playback boundary.
    pub(crate) async fn seed_smart_queue(
        &self,
        transaction: &mut sqlx::Transaction<'_, Sqlite>,
        key: SmartPlaylistKey,
        source: Option<SourceKey>,
        folder: Option<FolderKey>,
        now: i64,
    ) -> LibraryResult<()> {
        let policy = smart_policy_sql(transaction).await?;
        let sql = format!(
            "{policy} INSERT INTO temp.queue_input(media_uri,source_rank) SELECT media_uri,result_position-1 FROM selected ORDER BY result_position"
        );
        sqlx::query(AssertSqlSafe(sql.as_str()))
            .persistent(false)
            .bind(source)
            .bind(now)
            .bind(folder)
            .bind(serde_json::to_string(&[key.raw()])?)
            .execute(&mut **transaction)
            .await?;
        Ok(())
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
            smart_policy_sql(&mut connection).await?
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
                COALESCE(baseline.play_count,0)+(SELECT count(*) FROM listens played WHERE played.media_uri=requested.value) play_count,
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

const SMART_POLICY_PREFIX: &str = r#"
WITH definitions AS (
    SELECT smart_playlist_key AS definition_key,
           definition_json,
           position,
           normalized_name,
           COALESCE(json_extract(definition_json, '$.current'),0) AS current_scope,
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
           END AS result_sort,
           json_extract(definition_json, '$.descending') AS result_descending,
           json_extract(definition_json, '$.limit') AS result_limit
    FROM smart_playlists
    WHERE ?4 IS NULL OR smart_playlist_key IN (SELECT value FROM json_each(?4))
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
)
"#;

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

const SMART_POLICY_SUFFIX: &str = r#"
window_listens AS (
    SELECT definition.definition_key,listen.media_uri,
           count(*) AS play_count,
           sum(listen.skipped) AS skip_count,
           max(listen.started_at) AS last_played
    FROM definitions AS definition
    JOIN listens AS listen
      ON (
        definition.current_scope=0
        OR listen.source_id=(
          SELECT object_id FROM sources WHERE source_key=?1
        )
      )
     AND (
       definition.activity_period=3
       OR listen.started_at >= ?2 - CASE definition.activity_period
         WHEN 0 THEN 604800
         WHEN 1 THEN 2592000
         WHEN 2 THEN 31536000
       END
     )
     AND listen.started_at <= ?2
    GROUP BY definition.definition_key,listen.media_uri
),
lifetime_listens AS (
    SELECT definition.definition_key,listen.media_uri,count(*) AS play_count
    FROM definitions AS definition
    JOIN listens AS listen
      ON definition.current_scope=0
      OR listen.source_id=(
        SELECT object_id FROM sources WHERE source_key=?1
      )
    GROUP BY definition.definition_key,listen.media_uri
),
facts AS (
    SELECT definition.definition_key,
           definition.position AS definition_position,
           definition.normalized_name AS definition_name,
           definition.result_sort,
           definition.result_descending,
           definition.result_limit,
           owner.media_uri,owner.source_key,owner.track_key,owner.object_id,
           owner.title,owner.display_artist,owner.display_album,owner.duration_millis,
           owner.year,owner.date_added,owner.comment,owner.bpm,
           owner.source_rating,owner.source_favorite,owner.sort_text,
           CASE WHEN definition.activity_period=3
             THEN COALESCE(baseline.play_count,0)
             ELSE 0
           END + COALESCE(window.play_count,0) AS play_count,
           CASE WHEN definition.activity_period=3
             THEN COALESCE(baseline.skip_count,0)
             ELSE 0
           END + COALESCE(window.skip_count,0) AS skip_count,
           CASE
             WHEN definition.activity_period<>3 THEN window.last_played
             WHEN baseline.last_played_at IS NULL THEN window.last_played
             WHEN window.last_played IS NULL THEN baseline.last_played_at
             ELSE max(baseline.last_played_at,window.last_played)
           END AS last_played,
           COALESCE(baseline.play_count,0)+COALESCE(lifetime.play_count,0) AS lifetime_play_count
    FROM definitions AS definition
    CROSS JOIN media_rows owner
    LEFT JOIN activity_baseline AS baseline
      ON baseline.source_key=owner.source_key
     AND baseline.track_object_id=owner.object_id
     AND baseline.period='lifetime' AND baseline.item_kind='track'
    LEFT JOIN window_listens AS window
      ON window.definition_key=definition.definition_key
     AND window.media_uri=owner.media_uri
    LEFT JOIN lifetime_listens AS lifetime
      ON lifetime.definition_key=definition.definition_key
     AND lifetime.media_uri=owner.media_uri
    WHERE (
      definition.current_scope=0
      OR (
        ?1 IS NOT NULL AND owner.source_key=?1
        AND (
          ?3 IS NULL
          OR EXISTS (
            SELECT 1 FROM track_folders AS scope
            WHERE scope.track_key=owner.track_key AND scope.folder_key=?3
          )
        )
      )
    )
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
             WHEN 'Rating' THEN COALESCE(
               (SELECT state.rating FROM user_media_state state
                WHERE state.media_uri=facts.media_uri),
               facts.source_rating
             ) / 10
             WHEN 'Year' THEN facts.year
             WHEN 'PlayCount' THEN facts.play_count
             WHEN 'SkipCount' THEN facts.skip_count
           END AS number_fact,
           CASE rules.field
             WHEN 'Favorite' THEN COALESCE(
               (SELECT state.favorite FROM user_media_state state
                WHERE state.media_uri=facts.media_uri),
               facts.source_favorite
             )
             WHEN 'Played' THEN facts.lifetime_play_count > 0
           END AS bool_fact,
           CASE rules.field
             WHEN 'LastPlayed' THEN date(facts.last_played,'unixepoch')
             WHEN 'DateAdded' THEN facts.date_added
           END AS date_fact
    FROM facts
    JOIN rules USING(definition_key)
),
rule_matches(definition_key,media_uri,rule_group,matched) AS (
    SELECT value.definition_key,value.media_uri,value.rule_group,
      CASE
        WHEN value.field IN ('Title','Artist','Album','Comment') THEN
          CASE value.operator
            WHEN 'IsEmpty' THEN value.text_fact IS NULL OR trim(value.text_fact)=''
            WHEN 'IsNotEmpty' THEN value.text_fact IS NOT NULL AND trim(value.text_fact)<>''
            WHEN 'Contains' THEN value.text_fact IS NOT NULL AND value.text_value IS NOT NULL
                 AND instr(lower(value.text_fact),lower(value.text_value))>0
            WHEN 'NotContains' THEN value.text_fact IS NOT NULL AND value.text_value IS NOT NULL
                 AND instr(lower(value.text_fact),lower(value.text_value))=0
            WHEN 'Equals' THEN value.text_fact IS NOT NULL AND value.text_value IS NOT NULL
                 AND lower(value.text_fact)=lower(value.text_value)
            WHEN 'NotEquals' THEN value.text_fact IS NOT NULL AND value.text_value IS NOT NULL
                 AND lower(value.text_fact)<>lower(value.text_value)
            ELSE 0 END
        WHEN value.field='Genre' THEN CASE value.operator
          WHEN 'IsEmpty' THEN NOT EXISTS (
            SELECT 1 FROM track_genres WHERE track_key=value.track_key)
          WHEN 'IsNotEmpty' THEN EXISTS (
            SELECT 1 FROM track_genres WHERE track_key=value.track_key)
          WHEN 'Contains' THEN EXISTS (
            SELECT 1 FROM track_genres JOIN genres USING(genre_key)
            WHERE track_key=value.track_key
              AND instr(lower(genres.name),lower(value.text_value))>0)
          WHEN 'NotContains' THEN EXISTS (
            SELECT 1 FROM track_genres WHERE track_key=value.track_key) AND NOT EXISTS (
            SELECT 1 FROM track_genres JOIN genres USING(genre_key)
            WHERE track_key=value.track_key
              AND instr(lower(genres.name),lower(value.text_value))>0)
          WHEN 'Equals' THEN EXISTS (
            SELECT 1 FROM track_genres JOIN genres USING(genre_key)
            WHERE track_key=value.track_key AND lower(genres.name)=lower(value.text_value))
          WHEN 'NotEquals' THEN EXISTS (
            SELECT 1 FROM track_genres WHERE track_key=value.track_key) AND NOT EXISTS (
            SELECT 1 FROM track_genres JOIN genres USING(genre_key)
            WHERE track_key=value.track_key AND lower(genres.name)=lower(value.text_value))
          ELSE 0 END
        WHEN value.field='Mood' THEN CASE value.operator
          WHEN 'IsEmpty' THEN NOT EXISTS (
            SELECT 1 FROM track_moods WHERE track_key=value.track_key)
          WHEN 'IsNotEmpty' THEN EXISTS (
            SELECT 1 FROM track_moods WHERE track_key=value.track_key)
          WHEN 'Contains' THEN EXISTS (
            SELECT 1 FROM track_moods JOIN moods USING(mood_key)
            WHERE track_key=value.track_key
              AND instr(lower(moods.name),lower(value.text_value))>0)
          WHEN 'NotContains' THEN EXISTS (
            SELECT 1 FROM track_moods WHERE track_key=value.track_key) AND NOT EXISTS (
            SELECT 1 FROM track_moods JOIN moods USING(mood_key)
            WHERE track_key=value.track_key
              AND instr(lower(moods.name),lower(value.text_value))>0)
          WHEN 'Equals' THEN EXISTS (
            SELECT 1 FROM track_moods JOIN moods USING(mood_key)
            WHERE track_key=value.track_key AND lower(moods.name)=lower(value.text_value))
          WHEN 'NotEquals' THEN EXISTS (
            SELECT 1 FROM track_moods WHERE track_key=value.track_key) AND NOT EXISTS (
            SELECT 1 FROM track_moods JOIN moods USING(mood_key)
            WHERE track_key=value.track_key AND lower(moods.name)=lower(value.text_value))
          ELSE 0 END
        WHEN value.field IN ('Bpm','Rating','Year','PlayCount','SkipCount') THEN
          CASE value.operator
            WHEN 'IsEmpty' THEN value.number_fact IS NULL
            WHEN 'IsNotEmpty' THEN value.number_fact IS NOT NULL
            WHEN 'Above' THEN value.number_fact>value.number_value
            WHEN 'Below' THEN value.number_fact<value.number_value
            WHEN 'Equals' THEN value.number_fact=value.number_value
            WHEN 'NotEquals' THEN value.number_fact<>value.number_value
            WHEN 'Between' THEN value.number_fact BETWEEN value.range_min AND value.range_max
            ELSE 0 END
        WHEN value.field IN ('Favorite','Played') THEN
          CASE value.operator
            WHEN 'Is' THEN value.bool_fact=value.bool_value
            WHEN 'IsNot' THEN value.bool_fact<>value.bool_value
            ELSE 0 END
        WHEN value.field IN ('LastPlayed','DateAdded') THEN
          CASE value.operator
            WHEN 'IsEmpty' THEN value.date_fact IS NULL OR value.date_fact=''
            WHEN 'IsNotEmpty' THEN value.date_fact IS NOT NULL AND value.date_fact<>''
            WHEN 'Before' THEN value.date_fact<value.date_value
            WHEN 'After' THEN value.date_fact>value.date_value
            WHEN 'Equals' THEN value.date_fact=value.date_value
            WHEN 'NotEquals' THEN value.date_fact<>value.date_value
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
        AND media_uri=facts.media_uri
        AND rule_group='all'
        AND matched IS NOT 1
    ) AND (
      NOT EXISTS (
        SELECT 1 FROM rules
        WHERE definition_key=facts.definition_key AND rule_group='any'
      ) OR EXISTS (
        SELECT 1 FROM rule_matches
        WHERE definition_key=facts.definition_key
          AND media_uri=facts.media_uri
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
               CASE WHEN result_sort=0 AND result_descending=0 THEN sort_text END ASC,
               CASE WHEN result_sort=0 AND result_descending=1 THEN sort_text END DESC,
               CASE WHEN result_sort=1 AND result_descending=0 THEN display_artist END ASC,
               CASE WHEN result_sort=1 AND result_descending=1 THEN display_artist END DESC,
               CASE WHEN result_sort=2 AND result_descending=0 THEN display_album END ASC,
               CASE WHEN result_sort=2 AND result_descending=1 THEN display_album END DESC,
               CASE WHEN result_sort=3 AND result_descending=0 THEN year END ASC NULLS LAST,
               CASE WHEN result_sort=3 AND result_descending=1 THEN year END DESC NULLS LAST,
               CASE WHEN result_sort=4 AND result_descending=0 THEN date_added END ASC NULLS LAST,
               CASE WHEN result_sort=4 AND result_descending=1 THEN date_added END DESC NULLS LAST,
               CASE WHEN result_sort=5 AND result_descending=0 THEN last_played END ASC NULLS LAST,
               CASE WHEN result_sort=5 AND result_descending=1 THEN last_played END DESC NULLS LAST,
               CASE WHEN result_sort=6 AND result_descending=0 THEN play_count END ASC,
               CASE WHEN result_sort=6 AND result_descending=1 THEN play_count END DESC,
               CASE WHEN result_sort=7 AND result_descending=0 THEN skip_count END ASC,
               CASE WHEN result_sort=7 AND result_descending=1 THEN skip_count END DESC,
               CASE WHEN result_sort=8 AND result_descending=0 THEN bpm END ASC NULLS LAST,
               CASE WHEN result_sort=8 AND result_descending=1 THEN bpm END DESC NULLS LAST,
               CASE WHEN result_sort=9 AND result_descending=0
                 THEN COALESCE(
                   (SELECT state.rating FROM user_media_state state
                    WHERE state.media_uri=matched.media_uri),
                   matched.source_rating
                 ) END ASC NULLS LAST,
               CASE WHEN result_sort=9 AND result_descending=1
                 THEN COALESCE(
                   (SELECT state.rating FROM user_media_state state
                    WHERE state.media_uri=matched.media_uri),
                   matched.source_rating
                 ) END DESC NULLS LAST,
               CASE WHEN result_sort=10 AND result_descending=0 THEN duration_millis END ASC,
               CASE WHEN result_sort=10 AND result_descending=1 THEN duration_millis END DESC,
               sort_text,media_uri
           ) AS result_position
    FROM matched
),
selected AS MATERIALIZED (
    SELECT * FROM ranked
    WHERE result_limit IS NULL OR result_position<=result_limit
)
"#;

async fn smart_policy_sql(connection: &mut SqliteConnection) -> LibraryResult<String> {
    let sources = sqlx::query_as::<_, (i64, String)>("SELECT source_key,object_id FROM sources")
        .fetch_all(connection)
        .await?;
    // URI escaping leaves no SQL quoting characters in a canonical source prefix.
    // This scope exists only for this Smart evaluation, never as stored admission state.
    let scope = if sources.is_empty() {
        "SELECT NULL,NULL WHERE false".to_string()
    } else {
        format!(
            "VALUES {}",
            sources
                .into_iter()
                .map(|(key, id)| {
                    let prefix =
                        crate::keys::source_entity_prefix(&crate::SourceId::new(id), "track");
                    format!("({key},'{prefix}')")
                })
                .collect::<Vec<_>>()
                .join(",")
        )
    };
    Ok([
        &format!("WITH source_scope(source_key,prefix) AS ({scope}),\n"),
        SMART_POLICY_PREFIX
            .trim_start()
            .strip_prefix("WITH ")
            .unwrap(),
        ",\n",
        SMART_CANDIDATES,
        ",\n",
        SMART_POLICY_SUFFIX,
    ]
    .concat())
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
        let policy = smart_policy_sql(&mut connection).await?;
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
