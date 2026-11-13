//! Owns Smart Playlist definitions and direct SQL URI-result, row, and list queries.
//! One shared policy implements rule, Activity-window, limit, and ordering semantics.

use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use sqlx::{AssertSqlSafe, FromRow, SqliteConnection};

use crate::{Catalog, FolderKey, LibraryError, LibraryResult, SmartPlaylistKey};

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
    Genre,
    Mood,
    Comment,
    Year,
    Bpm,
    Compilation,
    Playlist,
    Favorite,
    Rating,
    DateAdded,
    Played,
    PlayCount,
    SkipCount,
    LastPlayed,
}

/// The part of the field menu a field is listed in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SmartPlaylistFieldSection {
    Track,
    Library,
    Listening,
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
    /// Within the last number of days.
    InTheLast,
    /// Longer ago than the number of days, or never.
    NotInTheLast,
    IsEmpty,
    IsNotEmpty,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SmartPlaylistRuleValue {
    Text(String),
    Number(i64),
    NumberRange {
        min: i64,
        max: i64,
    },
    Bool(bool),
    Date(String),
    DateRange {
        start: String,
        end: String,
    },
    /// A playlist by object id.
    Playlist(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SmartPlaylistRuleValueKind {
    None,
    /// Text the library's values are suggested for.
    Text,
    Number,
    NumberRange,
    Days,
    Date,
    DateRange,
    Bool,
    Playlist,
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
    SmartPlaylistRuleOperator::InTheLast,
    SmartPlaylistRuleOperator::NotInTheLast,
    SmartPlaylistRuleOperator::Before,
    SmartPlaylistRuleOperator::After,
    SmartPlaylistRuleOperator::Equals,
    SmartPlaylistRuleOperator::NotEquals,
    SmartPlaylistRuleOperator::Between,
    SmartPlaylistRuleOperator::IsEmpty,
    SmartPlaylistRuleOperator::IsNotEmpty,
];

impl SmartPlaylistRuleField {
    /// In menu order, section by section.
    pub const ALL: [Self; 17] = [
        Self::Title,
        Self::Artist,
        Self::Album,
        Self::Genre,
        Self::Mood,
        Self::Comment,
        Self::Year,
        Self::Bpm,
        Self::Compilation,
        Self::Playlist,
        Self::Favorite,
        Self::Rating,
        Self::DateAdded,
        Self::Played,
        Self::PlayCount,
        Self::SkipCount,
        Self::LastPlayed,
    ];

    pub fn section(self) -> SmartPlaylistFieldSection {
        match self {
            Self::Playlist | Self::Favorite | Self::Rating | Self::DateAdded => {
                SmartPlaylistFieldSection::Library
            }
            Self::Played | Self::PlayCount | Self::SkipCount | Self::LastPlayed => {
                SmartPlaylistFieldSection::Listening
            }
            _ => SmartPlaylistFieldSection::Track,
        }
    }

    pub fn operators(self) -> &'static [SmartPlaylistRuleOperator] {
        match self {
            Self::Title | Self::Artist | Self::Album | Self::Comment | Self::Genre | Self::Mood => {
                TEXT_OPERATORS
            }
            Self::Favorite | Self::Played | Self::Compilation | Self::Playlist => BOOL_OPERATORS,
            Self::LastPlayed | Self::DateAdded => DATE_OPERATORS,
            _ => NUMBER_OPERATORS,
        }
    }

    pub fn value_kind(
        self,
        operator: SmartPlaylistRuleOperator,
    ) -> Option<SmartPlaylistRuleValueKind> {
        use SmartPlaylistRuleOperator as O;
        use SmartPlaylistRuleValueKind as K;
        if !self.operators().contains(&operator) {
            return None;
        }
        Some(match (self, operator) {
            (_, O::IsEmpty | O::IsNotEmpty) => K::None,
            (_, O::InTheLast | O::NotInTheLast) => K::Days,
            (Self::Playlist, _) => K::Playlist,
            (
                Self::Title | Self::Artist | Self::Album | Self::Comment | Self::Genre | Self::Mood,
                _,
            ) => K::Text,
            (Self::Favorite | Self::Played | Self::Compilation, _) => K::Bool,
            (Self::LastPlayed | Self::DateAdded, O::Between) => K::DateRange,
            (Self::LastPlayed | Self::DateAdded, _) => K::Date,
            (_, O::Between) => K::NumberRange,
            _ => K::Number,
        })
    }

    /// The lowest, highest and first value of a number this rule takes.
    pub fn number_bounds(self, operator: SmartPlaylistRuleOperator) -> (i64, i64, i64) {
        if self.value_kind(operator) == Some(SmartPlaylistRuleValueKind::Days) {
            return (1, 36500, 30);
        }
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
        use SmartPlaylistRuleValueKind as K;
        let first = self.number_bounds(operator).2;
        match self.value_kind(operator)? {
            K::None => None,
            K::Text => Some(SmartPlaylistRuleValue::Text(String::new())),
            K::Number | K::Days => Some(SmartPlaylistRuleValue::Number(first)),
            K::NumberRange => Some(SmartPlaylistRuleValue::NumberRange {
                min: first,
                max: first,
            }),
            K::Date => Some(SmartPlaylistRuleValue::Date(String::new())),
            K::DateRange => Some(SmartPlaylistRuleValue::DateRange {
                start: String::new(),
                end: String::new(),
            }),
            K::Bool => Some(SmartPlaylistRuleValue::Bool(true)),
            K::Playlist => Some(SmartPlaylistRuleValue::Playlist(String::new())),
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

/// Whether every condition must hold, or one.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SmartPlaylistMatch {
    #[default]
    All,
    Any,
}

impl SmartPlaylistMatch {
    fn holds(self, mut conditions: impl Iterator<Item = bool>) -> bool {
        match self {
            Self::All => conditions.all(|held| held),
            Self::Any => conditions.any(|held| held),
        }
    }
}

/// A rule, or a group of rules with its own match. Groups hold rules only: with all and any at
/// both levels, that expresses every combination.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SmartPlaylistCondition {
    Rule(SmartPlaylistRule),
    Group {
        matching: SmartPlaylistMatch,
        rules: Vec<SmartPlaylistRule>,
    },
}

impl SmartPlaylistCondition {
    fn rules(&self) -> &[SmartPlaylistRule] {
        match self {
            Self::Rule(rule) => std::slice::from_ref(rule),
            Self::Group { rules, .. } => rules,
        }
    }
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
    /// A new shuffle each time the playlist is evaluated.
    Random,
}

impl SmartPlaylistSort {
    pub const ALL: [Self; 12] = [
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
        Self::Random,
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
#[serde(from = "StoredDefinition")]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SmartPlaylistDefinition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<crate::SourceId>,
    pub matching: SmartPlaylistMatch,
    pub conditions: Vec<SmartPlaylistCondition>,
    pub sort_field: SmartPlaylistSort,
    pub descending: bool,
    #[serde(default)]
    pub activity_period: SmartPlaylistActivityPeriod,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<crate::LibraryIndex>,
}

impl SmartPlaylistDefinition {
    /// Whether its rules or order read plays, so its listening period applies.
    pub fn reads_plays(&self) -> bool {
        use SmartPlaylistRuleField as F;
        use SmartPlaylistSort as S;
        matches!(self.sort_field, S::PlayCount | S::SkipCount | S::LastPlayed)
            || self.rules().any(|rule| {
                matches!(
                    rule.field,
                    F::Played | F::PlayCount | F::SkipCount | F::LastPlayed
                )
            })
    }

    /// Every rule, those in groups included.
    pub fn rules(&self) -> impl Iterator<Item = &SmartPlaylistRule> {
        self.conditions
            .iter()
            .flat_map(SmartPlaylistCondition::rules)
    }

    /// Whether the conditions hold, with `rule` telling whether a rule holds.
    pub(crate) fn holds(&self, mut rule: impl FnMut(&SmartPlaylistRule) -> bool) -> bool {
        // No conditions, in the definition or a group, leave every track in.
        let empty_or = |matching: SmartPlaylistMatch, held: &mut dyn Iterator<Item = bool>| {
            let mut held = held.peekable();
            held.peek().is_none() || matching.holds(held)
        };
        empty_or(
            self.matching,
            &mut self.conditions.iter().map(|condition| match condition {
                SmartPlaylistCondition::Rule(one) => rule(one),
                SmartPlaylistCondition::Group { matching, rules } => {
                    empty_or(*matching, &mut rules.iter().map(&mut rule))
                }
            }),
        )
    }
}

/// A definition as stored: those saved before groups hold an all list and an any list, which
/// held together. Backups and Connect peers still carry them.
#[derive(Deserialize)]
struct StoredDefinition {
    #[serde(default)]
    source_id: Option<crate::SourceId>,
    #[serde(default)]
    matching: SmartPlaylistMatch,
    #[serde(default)]
    conditions: Vec<SmartPlaylistCondition>,
    #[serde(default)]
    match_all: Vec<SmartPlaylistRule>,
    #[serde(default)]
    match_any: Vec<SmartPlaylistRule>,
    sort_field: SmartPlaylistSort,
    descending: bool,
    #[serde(default)]
    activity_period: SmartPlaylistActivityPeriod,
    #[serde(default)]
    limit: Option<crate::LibraryIndex>,
}

impl From<StoredDefinition> for SmartPlaylistDefinition {
    fn from(stored: StoredDefinition) -> Self {
        let StoredDefinition {
            source_id,
            mut matching,
            mut conditions,
            match_all,
            match_any,
            sort_field,
            descending,
            activity_period,
            limit,
        } = stored;
        if match_all.is_empty() && !match_any.is_empty() {
            matching = SmartPlaylistMatch::Any;
            conditions.extend(match_any.into_iter().map(SmartPlaylistCondition::Rule));
        } else {
            conditions.extend(match_all.into_iter().map(SmartPlaylistCondition::Rule));
            if !match_any.is_empty() {
                conditions.push(SmartPlaylistCondition::Group {
                    matching: SmartPlaylistMatch::Any,
                    rules: match_any,
                });
            }
        }
        Self {
            source_id,
            matching,
            conditions,
            sort_field,
            descending,
            activity_period,
            limit,
        }
    }
}

impl Default for SmartPlaylistDefinition {
    fn default() -> Self {
        Self {
            source_id: None,
            matching: SmartPlaylistMatch::All,
            conditions: Vec::new(),
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
                matching: SmartPlaylistMatch::All,
                conditions: vec![SmartPlaylistCondition::Rule(SmartPlaylistRule {
                    field: SmartPlaylistRuleField::Played,
                    operator: SmartPlaylistRuleOperator::Is,
                    value: Some(SmartPlaylistRuleValue::Bool(true)),
                })],
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
                matching: SmartPlaylistMatch::All,
                conditions: vec![SmartPlaylistCondition::Rule(SmartPlaylistRule {
                    field: SmartPlaylistRuleField::Played,
                    operator: SmartPlaylistRuleOperator::Is,
                    value: Some(SmartPlaylistRuleValue::Bool(false)),
                })],
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
                matching: SmartPlaylistMatch::All,
                conditions: vec![SmartPlaylistCondition::Rule(SmartPlaylistRule {
                    field: SmartPlaylistRuleField::SkipCount,
                    operator: SmartPlaylistRuleOperator::Above,
                    value: Some(SmartPlaylistRuleValue::Number(0)),
                })],
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

/// A value a rule can take, as the editor offers it: the text, or a playlist's object id, the
/// name shown for it and how many tracks hold it.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SmartPlaylistValueChoice {
    pub value: String,
    pub name: String,
    pub tracks: u64,
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
    /// The values of `field` whose names hold `text`, ignoring case: those starting with it
    /// first, then by name; `limit` of them. They come from the definition's source, every
    /// source without one, and its music folder; playlists from every folder, as playlists
    /// list them.
    pub async fn smart_playlist_values(
        &self,
        field: crate::SmartPlaylistRuleField,
        text: &str,
        source: Option<&crate::SourceId>,
        folder: Option<FolderKey>,
        limit: usize,
    ) -> LibraryResult<Vec<SmartPlaylistValueChoice>> {
        use crate::SmartPlaylistRuleField as F;
        self.loaded().await;
        let snapshot = self.snapshot();
        let owner = match source {
            Some(id) => match snapshot
                .sources
                .values()
                .find(|known| known.object == id.as_str())
                .and_then(|known| snapshot.source_slot(known.key))
            {
                Some(slot) => Some(slot),
                None => return Ok(Vec::new()),
            },
            None => None,
        };
        let context = crate::catalog::views::Context::new(&snapshot);
        let Some(folder) = context.folder_slot(folder, owner) else {
            return Ok(Vec::new());
        };
        let derived = self.derived(&context, folder);
        let owned = |slot: u16| owner.is_none_or(|owner| owner == slot);
        // Name, sort key, value and tracks.
        let mut found: Vec<(String, String, String, u64)> = Vec::new();
        let mut push = |name: &str, sort: String, value: String, tracks: u32| {
            found.push((name.to_owned(), sort, value, u64::from(tracks)));
        };
        match field {
            F::Genre | F::Mood => {
                let kind = if field == F::Genre {
                    crate::TagKind::Genre
                } else {
                    crate::TagKind::Mood
                };
                for (slot, tag) in snapshot.tags.iter() {
                    let tracks = derived.tags[slot as usize].tracks;
                    if tag.kind() == kind && owned(tag.source) && tracks > 0 {
                        let name = snapshot.text(tag.name);
                        push(
                            name,
                            snapshot.name_sort(tag.name, tag.sort).into_owned(),
                            name.to_owned(),
                            tracks,
                        );
                    }
                }
            }
            F::Artist => {
                for (slot, artist) in snapshot.artists.iter() {
                    let tracks = derived.artists[0][slot as usize].tracks;
                    if owned(artist.source) && tracks > 0 {
                        let name = snapshot.text(artist.name);
                        push(
                            name,
                            snapshot.name_sort(artist.name, artist.sort).into_owned(),
                            name.to_owned(),
                            tracks,
                        );
                    }
                }
            }
            F::Album => {
                for (slot, album) in snapshot.albums.iter() {
                    let tracks = derived.albums[slot as usize].tracks;
                    if owned(album.source) && tracks > 0 {
                        let name = snapshot.text(album.title);
                        push(
                            name,
                            snapshot.name_sort(album.title, album.sort).into_owned(),
                            name.to_owned(),
                            tracks,
                        );
                    }
                }
            }
            F::Playlist => {
                for (_, playlist) in snapshot.playlists.iter() {
                    if owned(playlist.source)
                        || playlist.source == crate::catalog::snapshot::NO_SOURCE
                    {
                        let name = snapshot.text(playlist.name);
                        let tracks = context.playlist_totals(playlist).tracks;
                        push(
                            name,
                            name.to_lowercase(),
                            snapshot.text(playlist.object).to_owned(),
                            tracks,
                        );
                    }
                }
            }
            _ => {}
        }
        // Rules match text, so one album title stands for every album of that title.
        if field != F::Playlist {
            found.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
            found.dedup_by(|later, kept| {
                let same = later.0.to_lowercase() == kept.0.to_lowercase();
                if same {
                    kept.3 += later.3;
                }
                same
            });
        }
        let text = text.trim().to_lowercase();
        let mut found = found
            .into_iter()
            .filter_map(|(name, sort, value, tracks)| {
                let lower = name.to_lowercase();
                let at = lower.find(&text)?;
                Some((
                    (at != 0, sort),
                    SmartPlaylistValueChoice {
                        value,
                        name,
                        tracks,
                    },
                ))
            })
            .collect::<Vec<_>>();
        found.sort_by(|a, b| a.0.cmp(&b.0));
        found.truncate(limit);
        Ok(found.into_iter().map(|(_, choice)| choice).collect())
    }

    /// How many tracks `definition` matches at `now` in the music folder `folder`, as its page
    /// would list them: the count the editor shows while its rules change.
    pub async fn smart_playlist_match_count(
        &self,
        definition: &SmartPlaylistDefinition,
        folder: Option<FolderKey>,
        now: i64,
    ) -> LibraryResult<u64> {
        self.loaded().await;
        let mut inputs = crate::catalog::views::Inputs::default();
        let periods = crate::catalog::views::smart_period(definition)
            .into_iter()
            .collect();
        let mut connection = self.acquire_reader().await?;
        self.read_periods(&mut connection, &mut inputs, periods, now)
            .await?;
        let snapshot = self.snapshot();
        let context = crate::catalog::views::Context::new(&snapshot);
        let Some(folder) = context.row_folder(folder, crate::CatalogKind::SmartPlaylist, None)
        else {
            return Ok(0);
        };
        Ok(u64::from(
            context
                .smart_totals(definition, &inputs, None, folder)
                .tracks,
        ))
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
            crate::playlist_folders::add_to_folder_on(
                &mut write,
                &[crate::PlaylistFolderMember::SmartPlaylist { key }],
                folder,
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

/// A smart playlist by object id: its folders by object id, its definition as JSON and the
/// mosaic's track URIs as a JSON list.
#[derive(Clone, Debug, PartialEq)]
pub struct SmartPlaylistCopy {
    pub object_id: String,
    pub name: String,
    pub folders: Vec<String>,
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
        let rows: Vec<(String, String, String, String, Option<String>)> = sqlx::query_as(
            "SELECT smart.object_id,smart.name,(SELECT json_group_array(folder.object_id) FROM playlist_folder_smart_playlists member JOIN playlist_folders folder USING(folder_key) WHERE member.smart_playlist_key=smart.smart_playlist_key),smart.definition_json,smart.mosaic FROM smart_playlists smart
             WHERE ?1 IS NULL OR smart.object_id IN(SELECT value FROM json_each(?1)) ORDER BY smart.object_id",
        )
        .bind(objects.map(serde_json::to_string).transpose()?)
        .fetch_all(&mut *connection)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(object_id, name, folders, definition, mosaic)| SmartPlaylistCopy {
                    object_id,
                    name,
                    folders: crate::playlist_folders::object_ids(&folders),
                    definition,
                    mosaic,
                },
            )
            .collect())
    }

    /// Writes smart playlists by object id, in one Write. A new one is placed last; it is in
    /// the folders of its copy that are here; a missing mosaic keeps the stored one.
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
                    folders: Some(playlist.folders.clone()),
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
    let rules = definition.rules().count();
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
    // An empty group holds nothing to match.
    definition.conditions.retain(|condition| {
        !matches!(condition, SmartPlaylistCondition::Group { rules, .. } if rules.is_empty())
    });
    for condition in &mut definition.conditions {
        let rules = match condition {
            SmartPlaylistCondition::Rule(rule) => std::slice::from_mut(rule),
            SmartPlaylistCondition::Group { rules, .. } => rules.as_mut_slice(),
        };
        for rule in rules {
            normalize_rule(rule)?;
        }
    }
    Ok(())
}

fn normalize_rule(rule: &mut SmartPlaylistRule) -> LibraryResult<()> {
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
    let (low, high, _) = rule.field.number_bounds(rule.operator);
    match &mut rule.value {
        Some(SmartPlaylistRuleValue::Text(value) | SmartPlaylistRuleValue::Date(value)) => {
            *value = value.trim().to_string()
        }
        Some(SmartPlaylistRuleValue::Number(days))
            if rule.field.value_kind(rule.operator) == Some(SmartPlaylistRuleValueKind::Days) =>
        {
            *days = (*days).clamp(low, high)
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
    /// Its folders by object id; None keeps those it is in. Backups restore folders
    /// separately.
    #[serde(skip)]
    #[sqlx(skip)]
    pub folders: Option<Vec<String>>,
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
        "INSERT INTO smart_playlists(object_id,name,normalized_name,definition_json,position,mosaic)
         VALUES(?1,?2,lower(?2),?3,CASE WHEN ?4 IS NULL OR EXISTS(SELECT 1 FROM smart_playlists WHERE position=?4)
             THEN (SELECT COALESCE(max(position)+1,0) FROM smart_playlists) ELSE ?4 END,?5)
         ON CONFLICT(object_id) DO UPDATE SET name=excluded.name,normalized_name=excluded.normalized_name,
             definition_json=excluded.definition_json,mosaic=coalesce(excluded.mosaic,mosaic)
         WHERE (name,definition_json,mosaic) IS NOT
             (excluded.name,excluded.definition_json,coalesce(excluded.mosaic,mosaic))
         RETURNING {}",
        crate::catalog::write::SMART_COLUMNS
    )))
    .bind(&record.object_id)
    .bind(&record.name)
    .bind(&definition)
    .bind(record.position)
    .bind(&record.mosaic)
    .fetch_optional(write.connection().await?)
    .await?;
    if let Some(row) = row {
        write.put_smart(row);
    }
    if let Some(folders) = &record.folders {
        let key: i64 =
            sqlx::query_scalar("SELECT smart_playlist_key FROM smart_playlists WHERE object_id=?1")
                .bind(&record.object_id)
                .fetch_one(write.connection().await?)
                .await?;
        crate::playlist_folders::set_folders_on(
            write,
            &crate::PlaylistFolderMember::SmartPlaylist {
                key: SmartPlaylistKey::from_raw(key),
            },
            folders,
        )
        .await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Definitions saved before groups, in the Store, backups and Connect, keep their meaning.
    #[test]
    fn definitions_with_all_and_any_lists_read_as_conditions() {
        let rule = |text: &str| SmartPlaylistRule {
            field: SmartPlaylistRuleField::Title,
            operator: SmartPlaylistRuleOperator::Contains,
            value: Some(SmartPlaylistRuleValue::Text(text.into())),
        };
        let stored = |all: &[&str], any: &[&str]| {
            let lists = serde_json::json!({
                "match_all": all.iter().map(|text| rule(text)).collect::<Vec<_>>(),
                "match_any": any.iter().map(|text| rule(text)).collect::<Vec<_>>(),
                "sort_field": "Title",
                "descending": false,
            });
            serde_json::from_value::<SmartPlaylistDefinition>(lists).unwrap()
        };
        let both = stored(&["a"], &["b", "c"]);
        assert_eq!(both.matching, SmartPlaylistMatch::All);
        assert_eq!(
            both.conditions,
            [
                SmartPlaylistCondition::Rule(rule("a")),
                SmartPlaylistCondition::Group {
                    matching: SmartPlaylistMatch::Any,
                    rules: vec![rule("b"), rule("c")],
                },
            ]
        );
        let any = stored(&[], &["b"]);
        assert_eq!(any.matching, SmartPlaylistMatch::Any);
        assert_eq!(any.conditions, [SmartPlaylistCondition::Rule(rule("b"))]);
        let written = serde_json::to_string(&both).unwrap();
        assert_eq!(
            serde_json::from_str::<SmartPlaylistDefinition>(&written).unwrap(),
            both
        );
    }
}
