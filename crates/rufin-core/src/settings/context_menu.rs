use std::collections::HashSet;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum ContextMenuItem {
    Play,
    PlayNext,
    PlayLater,
    PlayRadio,
    AddToPlaylist,
    Favorites,
    EditMetadata,
    Pins,
    #[serde(alias = "GoToArtist", alias = "GoToAlbum")]
    GoTo,
    Download,
}

impl ContextMenuItem {
    pub const fn all() -> [Self; 10] {
        [
            Self::Play,
            Self::PlayNext,
            Self::PlayLater,
            Self::PlayRadio,
            Self::AddToPlaylist,
            Self::Favorites,
            Self::EditMetadata,
            Self::Pins,
            Self::GoTo,
            Self::Download,
        ]
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextMenuItemSettings {
    pub item: ContextMenuItem,
    #[serde(default = "default_true")]
    pub visible: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextMenuSettings {
    #[serde(default = "default_context_menu_items")]
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

    pub fn position(&self, item: ContextMenuItem) -> usize {
        self.items
            .iter()
            .position(|entry| entry.item == item)
            .unwrap_or(usize::MAX)
    }
}

fn default_true() -> bool {
    true
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
    fn legacy_artist_and_album_destinations_merge_into_one_go_to_setting() {
        let mut settings: ContextMenuSettings = serde_json::from_value(serde_json::json!({
            "items": [
                { "item": "GoToAlbum", "visible": false },
                { "item": "GoToArtist", "visible": true }
            ]
        }))
        .expect("legacy context menu settings");

        settings.sanitize();

        assert_eq!(
            settings
                .items
                .iter()
                .filter(|entry| entry.item == ContextMenuItem::GoTo)
                .count(),
            1
        );
        assert_eq!(settings.items[0].item, ContextMenuItem::GoTo);
        assert!(!settings.items[0].visible);
    }

    #[test]
    fn rating_is_visible_by_default_and_in_older_settings() {
        assert!(ContextMenuSettings::default().rating_visible);

        let settings: ContextMenuSettings = serde_json::from_value(serde_json::json!({
            "items": []
        }))
        .expect("older context menu settings");

        assert!(settings.rating_visible);
    }
}
