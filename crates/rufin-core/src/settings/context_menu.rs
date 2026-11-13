use std::collections::HashSet;

use serde::{Deserialize, Deserializer, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ContextMenuItem {
    /// The Play group: Play, Play Next and Play Later, each also shuffled.
    Play,
    PlayRadio,
    AddToPlaylist,
    Favorites,
    EditMetadata,
    Pins,
    GoTo,
    Download,
    Share,
}

impl ContextMenuItem {
    pub const fn all() -> [Self; 9] {
        [
            Self::Play,
            Self::PlayRadio,
            Self::AddToPlaylist,
            Self::Favorites,
            Self::EditMetadata,
            Self::Pins,
            Self::GoTo,
            Self::Download,
            Self::Share,
        ]
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ContextMenuItemSettings {
    pub item: ContextMenuItem,
    #[serde(default = "default_true")]
    pub visible: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ContextMenuSettings {
    #[serde(
        default = "default_context_menu_items",
        deserialize_with = "known_items"
    )]
    pub items: Vec<ContextMenuItemSettings>,
    #[serde(default = "default_true")]
    pub rating_visible: bool,
}

impl Default for ContextMenuSettings {
    fn default() -> Self {
        Self {
            items: default_context_menu_items(),
            rating_visible: true,
        }
    }
}

impl ContextMenuSettings {
    pub fn sanitize(&mut self) {
        let mut seen = HashSet::new();
        self.items.retain(|entry| seen.insert(entry.item));

        for default in default_context_menu_items() {
            if seen.insert(default.item) {
                self.items.push(default);
            }
        }
    }

    pub fn is_visible(&self, item: ContextMenuItem) -> bool {
        self.items
            .iter()
            .find(|entry| entry.item == item)
            .is_none_or(|entry| entry.visible)
    }
}

fn default_true() -> bool {
    true
}

/// The saved entries whose item still exists; a retired item, such as Play Next, is dropped.
fn known_items<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<ContextMenuItemSettings>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Entry {
        Known(ContextMenuItemSettings),
        Retired(serde::de::IgnoredAny),
    }
    Ok(Vec::<Entry>::deserialize(deserializer)?
        .into_iter()
        .filter_map(|entry| match entry {
            Entry::Known(entry) => Some(entry),
            Entry::Retired(_) => None,
        })
        .collect())
}

fn default_context_menu_items() -> Vec<ContextMenuItemSettings> {
    ContextMenuItem::all()
        .into_iter()
        .map(|item| ContextMenuItemSettings {
            item,
            visible: true,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{ContextMenuItem, ContextMenuItemSettings, ContextMenuSettings};

    #[test]
    fn sanitize_keeps_saved_order_and_visibility_while_restoring_missing_items() {
        let mut settings = ContextMenuSettings {
            items: vec![
                ContextMenuItemSettings {
                    item: ContextMenuItem::Download,
                    visible: false,
                },
                ContextMenuItemSettings {
                    item: ContextMenuItem::Play,
                    visible: true,
                },
                ContextMenuItemSettings {
                    item: ContextMenuItem::Download,
                    visible: true,
                },
            ],
            rating_visible: true,
        };

        settings.sanitize();

        assert_eq!(settings.items.len(), ContextMenuItem::all().len());
        assert_eq!(settings.items[0].item, ContextMenuItem::Download);
        assert!(!settings.items[0].visible);
        assert_eq!(settings.items[1].item, ContextMenuItem::Play);
        assert_eq!(
            settings
                .items
                .iter()
                .filter(|entry| entry.item == ContextMenuItem::Download)
                .count(),
            1
        );
    }

    #[test]
    fn older_settings_keep_rating_visible_and_drop_retired_items() {
        assert!(ContextMenuSettings::default().rating_visible);

        let settings: ContextMenuSettings = serde_json::from_value(serde_json::json!({
            "items": [
                { "item": "PlayNext", "visible": true },
                { "item": "Download", "visible": false },
                { "item": "PlayLater", "visible": true }
            ]
        }))
        .expect("older context menu settings");

        assert!(settings.rating_visible);
        assert_eq!(
            settings.items,
            [ContextMenuItemSettings {
                item: ContextMenuItem::Download,
                visible: false,
            }]
        );
    }
}
