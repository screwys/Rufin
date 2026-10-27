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

/// Totals of smart playlists by key: their members as the Catalog evaluates them now.
pub(crate) async fn smart_totals(
    catalog: &Catalog,
    connection: &mut SqliteConnection,
    keys: &[i64],
    now: i64,
) -> LibraryResult<std::collections::HashMap<i64, crate::catalog::views::Totals>> {
    catalog.loaded().await;
    let definitions = {
        let snapshot = catalog.snapshot();
        keys.iter()
            .filter_map(|key| snapshot.smart_playlists.get(key))
            .map(|smart| (smart.key, smart.definition.clone()))
            .collect::<Vec<_>>()
    };
    let mut inputs = crate::catalog::views::Inputs::default();
    catalog
        .read_periods(
            connection,
            &mut inputs,
            definitions
                .iter()
                .filter_map(|(_, definition)| crate::catalog::views::smart_period(definition))
                .collect(),
            now,
        )
        .await?;
    let snapshot = catalog.snapshot();
    let context = crate::catalog::views::Context::new(&snapshot, false);
    Ok(definitions
        .iter()
        .map(|(key, definition)| {
            let opened = catalog.opened_smart(*key, now, definition);
            (
                *key,
                context.smart_totals(definition, &inputs, opened.as_deref().map(Vec::as_slice)),
            )
        })
        .collect())
}

/// Smart playlist rows by key, in the order asked.
pub(crate) async fn load_smart_playlist_rows(
    catalog: &Catalog,
    connection: &mut SqliteConnection,
    keys: &[SmartPlaylistKey],
    now: i64,
) -> LibraryResult<Vec<SmartPlaylistRow>> {
    let raw = keys.iter().map(|key| key.raw()).collect::<Vec<_>>();
    let totals = smart_totals(catalog, connection, &raw, now).await?;
    let mut rows = load_smart_playlist_headers(connection, keys).await?;
    for row in &mut rows {
        set_smart_totals(row, &totals);
    }
    Ok(rows)
}

/// Smart playlist rows by key without their totals, which take an evaluation.
pub(crate) async fn load_smart_playlist_headers(
    connection: &mut SqliteConnection,
    keys: &[SmartPlaylistKey],
) -> LibraryResult<Vec<SmartPlaylistRow>> {
    #[derive(FromRow)]
    struct Row {
        mosaic: Option<String>,
        #[sqlx(flatten)]
        playlist: SmartPlaylistRow,
    }
    let rows = sqlx::query_as::<_, Row>(AssertSqlSafe(format!(
        "SELECT playlist.mosaic,playlist.smart_playlist_key,playlist.object_id,playlist.name,playlist.definition_json,playlist.position,
            0 track_count,0 duration_millis,0 downloaded_count,{} artwork_binding
         FROM json_each(?1) requested JOIN smart_playlists playlist ON playlist.smart_playlist_key=requested.value
         ORDER BY CAST(requested.key AS INTEGER)",
        crate::artwork::preview_sql("playlist.artwork_binding")
    )))
    .bind(serde_json::to_string(&keys.iter().map(|key| key.raw()).collect::<Vec<_>>())?)
    .fetch_all(&mut *connection)
    .await?;
    rows.into_iter()
        .map(
            |Row {
                 mosaic,
                 mut playlist,
             }| {
                normalize_definition(&mut playlist.definition)?;
                playlist.artwork_binding = crate::artwork::playlist_artwork(
                    playlist.artwork_binding.take(),
                    (true, None, playlist.object_id.clone()),
                    mosaic.as_deref(),
                )?;
                Ok(playlist)
            },
        )
        .collect()
}

pub(crate) fn set_smart_totals(
    playlist: &mut SmartPlaylistRow,
    totals: &std::collections::HashMap<i64, crate::catalog::views::Totals>,
) {
    let summary = totals
        .get(&playlist.smart_playlist_key.raw())
        .map(crate::catalog::views::Totals::summary)
        .unwrap_or_default();
    playlist.track_count = summary.track_count;
    playlist.duration_millis = summary.duration_millis;
    playlist.downloaded_count = summary.downloaded_count;
}

/// Totals of smart playlist folders by key: of the smart playlists each holds.
pub(crate) async fn smart_folder_totals(
    catalog: &Catalog,
    connection: &mut SqliteConnection,
    folders: &[i64],
    now: i64,
) -> LibraryResult<std::collections::HashMap<i64, (i64, i64)>> {
    let members = {
        let snapshot = catalog.snapshot();
        snapshot
            .smart_playlists
            .values()
            .filter(|smart| smart.folder.is_some_and(|folder| folders.contains(&folder)))
            .map(|smart| (smart.folder.unwrap_or_default(), smart.key))
            .collect::<Vec<_>>()
    };
    let totals = smart_totals(
        catalog,
        connection,
        &members.iter().map(|(_, key)| *key).collect::<Vec<_>>(),
        now,
    )
    .await?;
    let mut folders = std::collections::HashMap::<i64, (i64, i64)>::new();
    for (folder, key) in members {
        if let Some(totals) = totals.get(&key) {
            let folder = folders.entry(folder).or_default();
            folder.0 += i64::from(totals.tracks);
            folder.1 += totals.duration as i64;
        }
    }
    Ok(folders)
}

/// Smart playlist folder rows by key: totals of their smart playlists and the first one's
/// artwork.
pub(crate) async fn load_smart_folder_rows(
    catalog: &Catalog,
    connection: &mut SqliteConnection,
    folders: &[i64],
    now: i64,
) -> LibraryResult<Vec<crate::PlaylistFolderRow>> {
    let totals = smart_folder_totals(catalog, connection, folders, now).await?;
    #[derive(FromRow)]
    struct Row {
        #[sqlx(flatten)]
        folder: crate::PlaylistFolderRow,
        cover: Option<String>,
        mosaic: Option<String>,
    }
    let rows: Vec<Row> = sqlx::query_as(AssertSqlSafe(format!(
        "SELECT folder.folder_key,folder.object_id,folder.name,folder.smart,{} artwork_binding,0 track_count,0 duration_millis,cover.object_id cover,cover.mosaic
         FROM json_each(?1) requested JOIN playlist_folders folder ON folder.folder_key=requested.value AND folder.smart
         LEFT JOIN smart_playlists cover ON cover.smart_playlist_key=(SELECT min(child.smart_playlist_key) FROM smart_playlists child WHERE child.playlist_folder_key=folder.folder_key)
         ORDER BY CAST(requested.key AS INTEGER)",
        crate::artwork::preview_sql("cover.artwork_binding")
    )))
    .bind(serde_json::to_string(folders)?)
    .fetch_all(&mut *connection)
    .await?;
    rows.into_iter()
        .map(
            |Row {
                 mut folder,
                 cover,
                 mosaic,
             }| {
                if let Some((tracks, duration)) = totals.get(&folder.folder_key.raw()) {
                    folder.track_count = *tracks;
                    folder.duration_millis = *duration;
                }
                folder.artwork_binding = match cover {
                    Some(id) => crate::artwork::playlist_artwork(
                        folder.artwork_binding.take(),
                        (true, None, id),
                        mosaic.as_deref(),
                    )?,
                    None => None,
                };
                Ok(folder)
            },
        )
        .collect()
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
        let scoped = match folder {
            Some(folder) => {
                let Some(folder) = snapshot.folders.slot(folder.raw()) else {
                    return Ok(SmartPlaylistValueSuggestions::default());
                };
                Some(
                    snapshot
                        .tracks
                        .iter()
                        .filter(|(_, track)| snapshot.track_runs(track)[3].contains(&folder))
                        .flat_map(|(_, track)| snapshot.track_runs(track)[2].iter().copied())
                        .collect::<std::collections::HashSet<_>>(),
                )
            }
            None => None,
        };
        let names = |kind: crate::TagKind| {
            let mut tags = snapshot
                .tags
                .iter()
                .filter(|(slot, tag)| {
                    tag.kind == kind
                        && tag.source == owner
                        && scoped.as_ref().is_none_or(|scoped| scoped.contains(slot))
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
            tags.into_iter()
                .take(100)
                .map(|(.., name)| name)
                .collect::<Vec<_>>()
        };
        Ok(SmartPlaylistValueSuggestions {
            genres: names(crate::TagKind::Genre),
            moods: names(crate::TagKind::Mood),
        })
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
        keys: &[SmartPlaylistKey],
        now: i64,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<SmartPlaylistRow>> {
        let (_permit, mut connection) = self.acquire_loaded(cancellation).await?;
        load_smart_playlist_rows(self, &mut connection, keys, now).await
    }

    pub async fn random_smart_playlist_artwork(
        &self,
        key: SmartPlaylistKey,
        now: i64,
    ) -> LibraryResult<Vec<Vec<u8>>> {
        let mut connection = self.acquire_reader().await?;
        let members = self
            .members(&mut connection, crate::Collection::Smart { key, now })
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
        let transaction = write.connection().await?;
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
        if let Some(folder) = folder {
            crate::playlist_folders::move_to_folder_on(
                transaction,
                crate::PlaylistFolderMember::SmartPlaylist { key },
                Some(folder),
            )
            .await?;
        }
        write.touch_smart_playlist(key.raw());
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
        let transaction = write.connection().await?;
        let revision = artwork
            .flatten()
            .map(|bytes| blake3::hash(bytes).to_hex().to_string());
        let binding = match &revision {
            Some(revision) => {
                let object: String = sqlx::query_scalar(
                    "SELECT object_id FROM smart_playlists WHERE smart_playlist_key=?1",
                )
                .bind(key)
                .fetch_one(&mut *transaction)
                .await?;
                Some(crate::artwork::playlist_binding(
                    true,
                    None,
                    object,
                    revision.clone(),
                ))
            }
            None => None,
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
            write.touch_smart_playlist(key.raw());
            // The image itself is read by key.
            if artwork.is_some() {
                write.changed(crate::catalog::write::Scope::Smart { key });
            }
        }
        write.commit().await?;
        Ok(changed)
    }

    pub async fn delete_smart_playlist(&self, key: SmartPlaylistKey) -> LibraryResult<bool> {
        let mut write = self.write().await?;
        let deleted = sqlx::query("DELETE FROM smart_playlists WHERE smart_playlist_key=?1")
            .bind(key)
            .execute(write.connection().await?)
            .await?
            .rows_affected()
            > 0;
        write.touch_smart_playlist(key.raw());
        write.commit().await?;
        Ok(deleted)
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
    pub position: i64,
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
    let binding = record
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
    sqlx::query("INSERT INTO smart_playlists(object_id,name,normalized_name,definition_json,position,artwork_bytes,artwork_revision,artwork_binding,mosaic)
       VALUES(?1,?2,lower(?2),?3,CASE WHEN EXISTS(SELECT 1 FROM smart_playlists WHERE position=?4)
       THEN (SELECT COALESCE(max(position)+1,0) FROM smart_playlists) ELSE ?4 END,?5,?6,?7,?8)
       ON CONFLICT(object_id) DO UPDATE SET name=excluded.name,normalized_name=excluded.normalized_name,
       definition_json=excluded.definition_json,artwork_bytes=excluded.artwork_bytes,artwork_revision=excluded.artwork_revision,artwork_binding=excluded.artwork_binding,mosaic=coalesce(excluded.mosaic,smart_playlists.mosaic)")
       .bind(&record.object_id).bind(&record.name).bind(&record.definition_json).bind(record.position)
       .bind(&record.artwork_bytes).bind(revision).bind(binding).bind(&record.mosaic)
       .execute(&mut *connection).await?;
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
            .members(&mut connection, crate::Collection::Smart { key, now })
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
