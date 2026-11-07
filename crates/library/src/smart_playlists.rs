//! Owns Smart Playlist definitions and direct SQL URI-result, row, and list queries.
//! One shared policy implements rule, Activity-window, limit, and ordering semantics.

use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use sqlx::{AssertSqlSafe, FromRow, SqliteConnection};

use crate::{
    Catalog, FolderKey, LibraryError, LibraryResult, ReadCancellation, SmartPlaylistKey, SourceKey,
};

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
        let between = operator == SmartPlaylistRuleOperator::Between;
        Some(match self {
            Self::Title | Self::Artist | Self::Album | Self::Comment | Self::Genre | Self::Mood => {
                SmartPlaylistRuleValueKind::Text
            }
            Self::Favorite | Self::Played => SmartPlaylistRuleValueKind::Bool,
            Self::LastPlayed | Self::DateAdded if between => SmartPlaylistRuleValueKind::DateRange,
            Self::LastPlayed | Self::DateAdded => SmartPlaylistRuleValueKind::Date,
            _ if between => SmartPlaylistRuleValueKind::NumberRange,
            _ => SmartPlaylistRuleValueKind::Number,
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
    pub const ALL: [Self; 4] = [Self::Weekly, Self::Monthly, Self::Yearly, Self::Lifetime];

    pub(crate) fn seconds(self) -> Option<i64> {
        match self {
            Self::Weekly => Some(604800),
            Self::Monthly => Some(2592000),
            Self::Yearly => Some(31536000),
            Self::Lifetime => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SmartPlaylistDefinition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<crate::SourceId>,
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
            source_id: None,
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
                source_id: None,
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
                source_id: None,
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
                source_id: None,
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
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SmartPlaylistRow {
    pub smart_playlist_key: SmartPlaylistKey,
    pub object_id: String,
    pub name: String,
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

/// Totals of smart playlists by key: their members as of `now` in the music folder `folder`, as
/// their pages list them.
pub(crate) async fn smart_totals(
    catalog: &Catalog,
    connection: &mut SqliteConnection,
    keys: &[i64],
    now: i64,
    folder: Option<crate::FolderKey>,
) -> LibraryResult<std::collections::HashMap<i64, crate::catalog::views::Totals>> {
    catalog.loaded().await;
    let definitions = {
        let snapshot = catalog.snapshot();
        keys.iter()
            .filter_map(|key| snapshot.smart_playlists.get(key))
            .map(|smart| (smart.key, smart.definition.clone()))
            .collect::<Vec<_>>()
    };
    let inputs = catalog
        .smart_inputs(
            connection,
            keys.iter()
                .map(|key| crate::Item::Smart(crate::SmartPlaylistKey::from_raw(*key))),
            now,
        )
        .await?;
    let snapshot = catalog.snapshot();
    let context = crate::catalog::views::Context::new(&snapshot);
    let folder_key = folder;
    let Some(folder) = context.row_folder(folder, crate::CatalogKind::SmartPlaylist, None) else {
        return Ok(std::collections::HashMap::new());
    };
    Ok(definitions
        .iter()
        .map(|(key, definition)| {
            let opened = catalog.opened_smart(*key, now, folder_key, definition);
            (
                *key,
                context.smart_totals(
                    definition,
                    &inputs,
                    opened.as_deref().map(Vec::as_slice),
                    folder,
                ),
            )
        })
        .collect())
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

impl Catalog {
    /// Genre and mood names of a source, for rule values.
    pub async fn smart_playlist_value_suggestions(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        _cancellation: &ReadCancellation,
    ) -> LibraryResult<SmartPlaylistValueSuggestions> {
        self.loaded().await;
        let snapshot = self.snapshot();
        let Some(owner) = snapshot.source_slot(source) else {
            return Ok(SmartPlaylistValueSuggestions::default());
        };
        let context = crate::catalog::views::Context::new(&snapshot);
        let Some(folder) = context.folder_slot(folder, Some(owner)) else {
            return Ok(SmartPlaylistValueSuggestions::default());
        };
        // Tags a visible track of the music folder carries.
        let derived = self.derived(&context, folder);
        let names = |kind: crate::TagKind| {
            let mut tags = snapshot
                .tags
                .iter()
                .filter(|(slot, tag)| {
                    tag.kind() == kind
                        && tag.source == owner
                        && derived.tags[*slot as usize].tracks > 0
                })
                .map(|(_, tag)| {
                    (
                        snapshot.name_sort(tag.name, tag.sort),
                        tag.key,
                        snapshot.text(tag.name).to_owned(),
                    )
                })
                .collect::<Vec<_>>();
            tags.sort();
            tags.into_iter().map(|(.., name)| name).collect::<Vec<_>>()
        };
        Ok(SmartPlaylistValueSuggestions {
            genres: names(crate::TagKind::Genre),
            moods: names(crate::TagKind::Mood),
        })
    }

    pub async fn random_smart_playlist_artwork(
        &self,
        key: SmartPlaylistKey,
        now: i64,
    ) -> LibraryResult<Vec<Vec<u8>>> {
        let mut connection = self.acquire_reader().await?;
        let members = self
            .members(
                &mut connection,
                crate::MemberRead::new(crate::Collection::Smart { key, now }),
            )
            .await?;
        let choices =
            crate::artwork::playlist_artwork_choices_on(self, &mut connection, &members, &[], true)
                .await?;
        crate::artwork::raw_playlist_artwork_on(self, &mut connection, &choices)
            .await?
            .iter()
            .map(|recipe| serde_json::to_vec(recipe).map_err(Into::into))
            .collect()
    }

    pub async fn create_smart_playlist(
        &self,
        name: &str,
        definition: &SmartPlaylistDefinition,
        folder: Option<crate::PlaylistFolderKey>,
    ) -> LibraryResult<SmartPlaylistKey> {
        let name = require_name(name)?;
        let definition = encode_definition(definition)?;
        let mut write = self.write().await?;
        let row: crate::catalog::write::SmartRow = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "INSERT INTO smart_playlists(
                 object_id, name, normalized_name,definition_json, position
             ) VALUES (
                 'rufin:smart:' || lower(hex(randomblob(16))),
                 ?1, lower(?1), ?2,
                 (SELECT COALESCE(max(position) + 1, 0) FROM smart_playlists)
             ) RETURNING {}",
            crate::catalog::write::SMART_COLUMNS
        )))
        .bind(name)
        .bind(definition)
        .fetch_one(write.connection().await?)
        .await?;
        let key = SmartPlaylistKey::from_raw(row.0);
        write.put_smart(row);
        if let Some(folder) = folder {
            crate::playlist_folders::move_to_folder_on(
                &mut write,
                crate::PlaylistFolderMember::SmartPlaylist { key },
                Some(folder),
            )
            .await?;
        }
        write.commit().await?;
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
        let mut write = self.write().await?;
        let row: Option<crate::catalog::write::SmartRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "UPDATE smart_playlists SET name=?2, normalized_name=lower(?2), definition_json=?3 WHERE smart_playlist_key=?1 RETURNING {}",
            crate::catalog::write::SMART_COLUMNS
        )))
        .bind(key)
        .bind(name)
        .bind(definition)
        .fetch_optional(write.connection().await?)
        .await?;
        let changed = row.is_some();
        if let Some(row) = row {
            let object_id = row.5.clone();
            write.put_smart(row);
            if let Some(bytes) = artwork {
                let image = crate::playlists::PlaylistArtworkRecord {
                    source_id: None,
                    object_id,
                    bytes: bytes.map(<[u8]>::to_vec),
                    content_type: None,
                    artwork_binding: None,
                    mosaic: None,
                };
                crate::playlists::write_playlist_artwork(&mut write, &image).await?;
            }
        }
        write.commit().await?;
        Ok(changed)
    }

    /// Deletes smart playlists in one Write, and returns how many went.
    pub async fn delete_smart_playlists(&self, keys: &[SmartPlaylistKey]) -> LibraryResult<usize> {
        if keys.is_empty() {
            return Ok(0);
        }
        let mut write = self.write().await?;
        let deleted: Vec<i64> = sqlx::query_scalar(
            "DELETE FROM smart_playlists WHERE smart_playlist_key IN(SELECT value FROM json_each(?1)) RETURNING smart_playlist_key",
        )
        .bind(serde_json::to_string(keys)?)
        .fetch_all(write.connection().await?)
        .await?;
        for key in &deleted {
            write.remove_smart(*key);
        }
        write.commit().await?;
        Ok(deleted.len())
    }
}

/// A smart playlist by object id: its folder by object id, its definition as JSON and the
/// mosaic's track URIs as a JSON list.
#[derive(Clone, Debug, PartialEq)]
pub struct SmartPlaylistCopy {
    pub object_id: String,
    pub name: String,
    pub folder: Option<String>,
    pub definition: String,
    pub mosaic: Option<String>,
}

impl Catalog {
    /// Smart playlists by object id, all of them or those of these object ids.
    pub async fn smart_playlist_copies(
        &self,
        objects: Option<&[String]>,
    ) -> LibraryResult<Vec<SmartPlaylistCopy>> {
        let mut connection = self.acquire_reader().await?;
        let rows: Vec<(String, String, Option<String>, String, Option<String>)> = sqlx::query_as(
            "SELECT smart.object_id,smart.name,folder.object_id,smart.definition_json,smart.mosaic FROM smart_playlists smart
             LEFT JOIN playlist_folders folder ON folder.folder_key=smart.playlist_folder_key
             WHERE ?1 IS NULL OR smart.object_id IN(SELECT value FROM json_each(?1)) ORDER BY smart.object_id",
        )
        .bind(objects.map(serde_json::to_string).transpose()?)
        .fetch_all(&mut *connection)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(object_id, name, folder, definition, mosaic)| SmartPlaylistCopy {
                    object_id,
                    name,
                    folder,
                    definition,
                    mosaic,
                },
            )
            .collect())
    }

    /// Writes smart playlists by object id, in one Write. A new one is placed last; a folder
    /// that is not here leaves it outside folders; a missing mosaic keeps the stored one.
    pub async fn put_smart_playlist_copies(
        &self,
        playlists: &[SmartPlaylistCopy],
    ) -> LibraryResult<()> {
        let mut write = self.write().await?;
        for playlist in playlists {
            write_smart_playlist(
                &mut write,
                &SmartPlaylistWrite {
                    artwork_bytes: None,
                    artwork_binding: None,
                    mosaic: playlist.mosaic.clone(),
                    object_id: playlist.object_id.clone(),
                    name: playlist.name.clone(),
                    definition_json: playlist.definition.clone(),
                    position: None,
                    folder: playlist.folder.clone(),
                },
            )
            .await?;
        }
        write.commit().await?;
        Ok(())
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
        // A value of the kind the field's default for this operator has.
        let valid = rule.field.value_kind(rule.operator).is_some()
            && rule.value.as_ref().map(std::mem::discriminant)
                == rule
                    .field
                    .default_value(rule.operator)
                    .as_ref()
                    .map(std::mem::discriminant);
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

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct SmartPlaylistWrite {
    #[serde(default)]
    pub artwork_bytes: Option<Vec<u8>>,
    #[serde(default)]
    pub artwork_binding: Option<Vec<u8>>,
    #[serde(default)]
    pub mosaic: Option<String>,
    pub object_id: String,
    pub name: String,
    pub definition_json: String,
    /// Its place in the order; without one, or when another holds it, it goes last.
    pub position: Option<i64>,
    /// Its folder by object id; backups restore folders separately.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[sqlx(default)]
    pub folder: Option<String>,
}

pub(crate) async fn export_smart_playlists_jsonl_on(
    connection: &mut SqliteConnection,
    mut output: impl std::io::Write,
) -> LibraryResult<u64> {
    output.write_all(b"{\"version\":1}\n")?;
    let mut rows = sqlx::query_as::<_, SmartPlaylistWrite>(
        "SELECT object_id,name,definition_json,position,artwork_bytes,artwork_binding,mosaic FROM smart_playlists ORDER BY position",
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
    write: &mut crate::catalog::write::Write<'_>,
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
        write_smart_playlist(write, &record).await?;
        count += 1;
    }
    Ok(count)
}

/// The one smart playlist row writer: creates or updates it by object id. Its picture, when
/// the record has one, goes through the playlist image writer; a missing mosaic or picture keeps
/// the stored one.
pub(crate) async fn write_smart_playlist(
    write: &mut crate::catalog::write::Write<'_>,
    record: &SmartPlaylistWrite,
) -> LibraryResult<()> {
    if record.object_id.is_empty()
        || record.name.trim().is_empty()
        || record.position.is_some_and(|position| position < 0)
    {
        return Err(crate::LibraryError::InvalidRequest(
            "invalid Smart playlist identity or order".into(),
        ));
    }
    // Backups and Connect write here, so a definition is checked as an edit's is.
    let definition = encode_definition(&serde_json::from_str(&record.definition_json)?)?;
    let row: Option<crate::catalog::write::SmartRow> = sqlx::query_as(AssertSqlSafe(format!(
        "INSERT INTO smart_playlists(object_id,name,normalized_name,definition_json,position,playlist_folder_key,mosaic)
         VALUES(?1,?2,lower(?2),?3,CASE WHEN ?4 IS NULL OR EXISTS(SELECT 1 FROM smart_playlists WHERE position=?4)
             THEN (SELECT COALESCE(max(position)+1,0) FROM smart_playlists) ELSE ?4 END,
             (SELECT folder_key FROM playlist_folders WHERE object_id=?5 AND smart),?6)
         ON CONFLICT(object_id) DO UPDATE SET name=excluded.name,normalized_name=excluded.normalized_name,
             definition_json=excluded.definition_json,playlist_folder_key=excluded.playlist_folder_key,
             mosaic=coalesce(excluded.mosaic,mosaic)
         WHERE (name,definition_json,playlist_folder_key,mosaic) IS NOT
             (excluded.name,excluded.definition_json,excluded.playlist_folder_key,coalesce(excluded.mosaic,mosaic))
         RETURNING {}",
        crate::catalog::write::SMART_COLUMNS
    )))
    .bind(&record.object_id)
    .bind(&record.name)
    .bind(&definition)
    .bind(record.position)
    .bind(&record.folder)
    .bind(&record.mosaic)
    .fetch_optional(write.connection().await?)
    .await?;
    if let Some(row) = row {
        write.put_smart(row);
    }
    if record.artwork_bytes.is_some() || record.artwork_binding.is_some() {
        crate::playlists::write_playlist_artwork(
            write,
            &crate::playlists::PlaylistArtworkRecord {
                source_id: None,
                object_id: record.object_id.clone(),
                bytes: record.artwork_bytes.clone(),
                content_type: None,
                artwork_binding: record.artwork_binding.clone(),
                mosaic: None,
            },
        )
        .await?;
    }
    Ok(())
}

impl Catalog {
    pub async fn export_smart_playlist_file(
        &self,
        key: SmartPlaylistKey,
        now: i64,
        file: &std::path::Path,
        mode: crate::PlaylistPathMode,
        output: impl std::io::Write,
    ) -> LibraryResult<u64> {
        let (_export, mut connection) = self.acquire_export().await?;
        let members = self
            .members(
                &mut connection,
                crate::MemberRead::new(crate::Collection::Smart { key, now }),
            )
            .await?;
        let mut output =
            crate::playlist_format::PlaylistWriter::new(output, file, mode, None, None)?;
        let metadata = crate::metadata::track_metadata_sql("track", "details");
        for (page, keys) in members.chunks(512).enumerate() {
            let entries = sqlx::query_as::<_, crate::PlaylistEntryWrite>(AssertSqlSafe(format!(
                "SELECT 'm3u:'||(CAST(requested.key AS INTEGER)+?2+1) object_id,track.media_uri,{metadata} metadata,CAST(requested.key AS INTEGER)+?2 position
                 FROM json_each(?1) requested JOIN tracks track ON track.track_key=requested.value LEFT JOIN track_details details ON details.track_key=track.track_key
                 ORDER BY CAST(requested.key AS INTEGER)"
            )))
            .bind(serde_json::to_string(keys)?)
            .bind((page * 512) as i64)
            .fetch_all(&mut *connection)
            .await?;
            for entry in &entries {
                output.entry(entry)?;
            }
        }
        output.finish()
    }
}
