//! Quick mix entries: Home's mixes, each a copy of a smart playlist template's rules or of one of
//! the user's smart playlists', kept apart from where it came from.

use serde::{Deserialize, Serialize};

use super::{SettingsError, SettingsOwner};
use crate::playlists::{SmartPlaylistTemplate, smart_playlist_templates};

/// The most entries Quick mix holds.
pub const MAX_QUICK_MIX: usize = 6;

/// The glyph of an entry copied from one of the user's smart playlists.
const SMART_PLAYLIST_ICON: &str = "rufin-smart-playlists-symbolic";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct QuickMixEntry {
    /// The template it was copied from, by its untranslated name, which it shows translated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// The smart playlist it was copied from, by object id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playlist: Option<String>,
    /// The name of the smart playlist it was copied from, as it was then.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub definition: library::SmartPlaylistDefinition,
}

impl QuickMixEntry {
    fn of_template(template: &SmartPlaylistTemplate) -> Self {
        Self {
            template: Some(template.name.to_owned()),
            playlist: None,
            name: String::new(),
            definition: template.definition.clone(),
        }
    }

    /// Its name, translated when it is a template's.
    pub fn title(&self) -> String {
        match &self.template {
            Some(template) => localization::tr(template),
            None => self.name.clone(),
        }
    }

    /// Its glyph: its template's, else the smart playlists' one.
    pub fn icon(&self) -> &'static str {
        self.template
            .as_deref()
            .and_then(|name| {
                smart_playlist_templates()
                    .into_iter()
                    .find(|template| template.name == name)
            })
            .map_or(SMART_PLAYLIST_ICON, |template| template.icon)
    }
}

/// Random, Recently added and Never played fill from the start everywhere; Rediscover on a
/// server; On repeat and Favorites as Rufin is used and things are marked.
pub fn default_quick_mix() -> Vec<QuickMixEntry> {
    let templates = smart_playlist_templates();
    [
        "Random",
        "Recently added",
        "Never played",
        "On repeat",
        "Rediscover",
        "Favorites",
    ]
    .into_iter()
    .filter_map(|name| templates.iter().find(|template| template.name == name))
    .map(QuickMixEntry::of_template)
    .collect()
}

/// What a Quick mix choice stands for.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum QuickMixChoiceKind {
    /// An entry, by its place; choosing it removes it.
    Entry { index: u32 },
    /// A template no entry was copied from, by its untranslated name.
    Template { name: String },
    /// A smart playlist no entry was copied from, by object id.
    Playlist { object_id: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct QuickMixChoice {
    pub kind: QuickMixChoiceKind,
    pub title: String,
    pub icon: String,
    /// Whether it is in Quick mix.
    pub added: bool,
}

/// Quick mix as its editor lists it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct QuickMixEditor {
    /// The entries in their Home order, then the templates no entry was copied from.
    pub choices: Vec<QuickMixChoice>,
    /// The user's smart playlists by name, those an entry was copied from added.
    pub playlists: Vec<QuickMixChoice>,
    /// Whether another entry fits.
    pub can_add: bool,
}

/// `entries` and the templates as the editor lists them, with `playlists`, the user's smart
/// playlists as object id and name.
pub(crate) fn quick_mix_editor(
    entries: &[QuickMixEntry],
    playlists: Vec<(String, String)>,
) -> QuickMixEditor {
    let entry = |(index, entry): (usize, &QuickMixEntry)| QuickMixChoice {
        kind: QuickMixChoiceKind::Entry {
            index: index as u32,
        },
        title: entry.title(),
        icon: entry.icon().to_owned(),
        added: true,
    };
    let mut choices = entries.iter().enumerate().map(entry).collect::<Vec<_>>();
    choices.extend(
        smart_playlist_templates()
            .into_iter()
            .filter(|template| {
                !entries
                    .iter()
                    .any(|entry| entry.template.as_deref() == Some(template.name))
            })
            .map(|template| QuickMixChoice {
                kind: QuickMixChoiceKind::Template {
                    name: template.name.to_owned(),
                },
                title: localization::tr(template.name),
                icon: template.icon.to_owned(),
                added: false,
            }),
    );
    let playlists = playlists
        .into_iter()
        .map(|(object_id, name)| {
            let copied = entries
                .iter()
                .position(|entry| entry.playlist.as_deref() == Some(object_id.as_str()));
            QuickMixChoice {
                kind: match copied {
                    Some(index) => QuickMixChoiceKind::Entry {
                        index: index as u32,
                    },
                    None => QuickMixChoiceKind::Playlist { object_id },
                },
                title: name,
                icon: SMART_PLAYLIST_ICON.to_owned(),
                added: copied.is_some(),
            }
        })
        .collect();
    QuickMixEditor {
        choices,
        playlists,
        can_add: entries.len() < MAX_QUICK_MIX,
    }
}

/// Appends `entry` when another fits; false when Quick mix is full.
pub(crate) fn add_quick_mix(entries: &mut Vec<QuickMixEntry>, entry: QuickMixEntry) -> bool {
    if entries.len() >= MAX_QUICK_MIX {
        return false;
    }
    entries.push(entry);
    true
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl SettingsOwner {
    /// Appends a copy of the template named `name`.
    pub fn add_quick_mix_template(&self, name: String) -> Result<bool, SettingsError> {
        let Some(template) = smart_playlist_templates()
            .into_iter()
            .find(|template| template.name == name)
        else {
            return Ok(false);
        };
        self.file
            .update(|stored| {
                Ok(add_quick_mix(
                    &mut stored.ui.application.quick_mix,
                    QuickMixEntry::of_template(&template),
                ))
            })
            .map_err(Into::into)
    }

    pub fn remove_quick_mix(&self, index: u32) -> Result<bool, SettingsError> {
        self.file
            .update(|stored| {
                let entries = &mut stored.ui.application.quick_mix;
                let removed = (index as usize) < entries.len();
                if removed {
                    entries.remove(index as usize);
                }
                Ok(removed)
            })
            .map_err(Into::into)
    }

    /// Moves the entry at `source` before or after the one at `target`.
    pub fn reorder_quick_mix(
        &self,
        source: u32,
        target: u32,
        after: bool,
    ) -> Result<bool, SettingsError> {
        self.file
            .update(|stored| {
                let entries = &mut stored.ui.application.quick_mix;
                let (source, target) = (source as usize, target as usize);
                if source == target || source >= entries.len() || target >= entries.len() {
                    return Ok(false);
                }
                let before = entries.clone();
                let entry = entries.remove(source);
                let target = if target > source { target - 1 } else { target };
                entries.insert(target + usize::from(after), entry);
                Ok(*entries != before)
            })
            .map_err(Into::into)
    }
}
