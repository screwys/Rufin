use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use sources::SourceId;

fn default_true() -> bool {
    true
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum SidebarRouteItem {
    Home,
    Search,
    Favorites,
    Albums,
    Tracks,
    Artists,
    AlbumArtists,
    Genres,
    Moods,
    Folders,
    Playlists,
    SmartPlaylists,
    History,
}
impl SidebarRouteItem {
    pub fn from_stable_id(stable_id: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(stable_id.to_owned())).ok()
    }

    pub fn all() -> [Self; 13] {
        [
            Self::Home,
            Self::Search,
            Self::Favorites,
            Self::Albums,
            Self::Tracks,
            Self::Artists,
            Self::AlbumArtists,
            Self::Genres,
            Self::Moods,
            Self::History,
            Self::Folders,
            Self::Playlists,
            Self::SmartPlaylists,
        ]
    }

    fn default_visible(self) -> bool {
        !matches!(self, Self::Search | Self::Moods)
    }
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SidebarRouteItemSettings {
    pub item: SidebarRouteItem,
    pub visible: bool,
}
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum SidebarPin {
    Album {
        source_id: SourceId,
        album_id: String,
    },
    Artist {
        source_id: SourceId,
        artist_id: String,
        #[serde(default, skip_serializing_if = "is_false")]
        album_artist: bool,
    },
    Genre {
        source_id: SourceId,
        genre_id: String,
    },
    Playlist {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_id: Option<SourceId>,
        playlist_id: String,
    },
    SmartPlaylist {
        playlist_id: String,
    },
}

fn is_false(value: &bool) -> bool {
    !*value
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SidebarSettings {
    #[serde(default = "default_sidebar_route_items")]
    pub route_items: Vec<SidebarRouteItemSettings>,
    #[serde(default = "default_true")]
    pub pins_visible: bool,
    #[serde(default)]
    pub pins: Vec<SidebarPin>,
    #[serde(default)]
    pub playlist_pin_imported_sources: Vec<SourceId>,
    #[serde(default = "default_true")]
    pub server_visible: bool,
}
impl Default for SidebarSettings {
    fn default() -> Self {
        Self {
            route_items: default_sidebar_route_items(),
            pins_visible: true,
            pins: Vec::new(),
            playlist_pin_imported_sources: Vec::new(),
            server_visible: true,
        }
    }
}
impl SidebarSettings {
    pub fn sanitize(&mut self) {
        let mut sanitized = Vec::with_capacity(SidebarRouteItem::all().len());
        for entry in &self.route_items {
            if !SidebarRouteItem::all().contains(&entry.item)
                || sanitized
                    .iter()
                    .any(|existing: &SidebarRouteItemSettings| existing.item == entry.item)
            {
                continue;
            }
            sanitized.push(entry.clone());
        }
        for item in SidebarRouteItem::all() {
            if !sanitized.iter().any(|entry| entry.item == item) {
                insert_sidebar_route_item_in_default_order(
                    &mut sanitized,
                    SidebarRouteItemSettings {
                        item,
                        visible: item.default_visible(),
                    },
                );
            }
        }
        if !sanitized.iter().any(|entry| entry.visible)
            && let Some(home) = sanitized
                .iter_mut()
                .find(|entry| entry.item == SidebarRouteItem::Home)
        {
            home.visible = true;
        }
        self.route_items = sanitized;
        let mut seen = HashSet::new();
        self.pins.retain(|pin| seen.insert(pin.clone()));
        let mut seen = HashSet::new();
        self.playlist_pin_imported_sources
            .retain(|source| seen.insert(source.clone()));
    }

    pub fn is_pinned(&self, pin: &SidebarPin) -> bool {
        self.pins.contains(pin)
    }

    pub fn set_pinned(&mut self, pin: SidebarPin, pinned: bool) -> bool {
        if pinned {
            if self.pins.contains(&pin) {
                return false;
            }
            self.pins.push(pin);
            return true;
        }
        let previous_len = self.pins.len();
        self.pins.retain(|stored| stored != &pin);
        self.pins.len() != previous_len
    }

    pub fn reorder_pin(&mut self, moved: &SidebarPin, target: &SidebarPin, after: bool) -> bool {
        if moved == target {
            return false;
        }
        let Some(moved_index) = self.pins.iter().position(|pin| pin == moved) else {
            return false;
        };
        let mut reordered = self.pins.clone();
        let moved = reordered.remove(moved_index);
        let Some(target_index) = reordered.iter().position(|pin| pin == target) else {
            return false;
        };
        reordered.insert(target_index + usize::from(after), moved);
        if reordered == self.pins {
            return false;
        }
        self.pins = reordered;
        true
    }

    pub fn pin_drop_after(&self, moved: &SidebarPin, target: &SidebarPin) -> Option<bool> {
        let moved_index = self.pins.iter().position(|pin| pin == moved)?;
        let target_index = self.pins.iter().position(|pin| pin == target)?;
        (moved_index != target_index).then_some(moved_index < target_index)
    }
    pub fn import_playlist_pins_once(
        &mut self,
        source_id: SourceId,
        playlist_ids: impl IntoIterator<Item = String>,
    ) -> bool {
        if self.playlist_pin_imported_sources.contains(&source_id) {
            return false;
        }
        for playlist_id in playlist_ids {
            self.set_pinned(
                SidebarPin::Playlist {
                    source_id: Some(source_id.clone()),
                    playlist_id,
                },
                true,
            );
        }
        self.playlist_pin_imported_sources.push(source_id);
        true
    }
}
fn insert_sidebar_route_item_in_default_order(
    items: &mut Vec<SidebarRouteItemSettings>,
    entry: SidebarRouteItemSettings,
) {
    let default_order = SidebarRouteItem::all();
    let Some(entry_index) = default_order.iter().position(|item| *item == entry.item) else {
        items.push(entry);
        return;
    };
    let insert_index = items
        .iter()
        .position(|existing| {
            default_order
                .iter()
                .position(|item| *item == existing.item)
                .is_some_and(|existing_index| existing_index > entry_index)
        })
        .unwrap_or(items.len());
    items.insert(insert_index, entry);
}
fn default_sidebar_route_items() -> Vec<SidebarRouteItemSettings> {
    SidebarRouteItem::all()
        .into_iter()
        .map(|item| SidebarRouteItemSettings {
            item,
            visible: item.default_visible(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_defaults_and_sanitization_preserve_saved_behavior() {
        let defaults = SidebarSettings::default();
        assert_eq!(
            defaults
                .route_items
                .iter()
                .map(|entry| entry.item)
                .collect::<Vec<_>>(),
            SidebarRouteItem::all()
        );
        assert_eq!(
            defaults
                .route_items
                .iter()
                .filter(|entry| !entry.visible)
                .map(|entry| entry.item)
                .collect::<Vec<_>>(),
            [SidebarRouteItem::Search, SidebarRouteItem::Moods]
        );

        let mut saved = defaults.clone();
        saved.route_items.rotate_left(4);
        saved.route_items[0].visible = false;
        let expected = saved.route_items.clone();
        saved.sanitize();
        assert_eq!(saved.route_items, expected);
    }

    fn pin(id: &str) -> SidebarPin {
        SidebarPin::Playlist {
            source_id: Some(SourceId::new("test:source")),
            playlist_id: id.to_string(),
        }
    }

    #[test]
    fn saved_pins_keep_cross_source_identity_and_global_order() {
        let first = SidebarPin::Genre {
            source_id: SourceId::new("first"),
            genre_id: "rock".into(),
        };
        let second = SidebarPin::Genre {
            source_id: SourceId::new("second"),
            genre_id: "rock".into(),
        };
        let global = SidebarPin::Playlist {
            source_id: None,
            playlist_id: "mix".into(),
        };
        let saved = serde_json::json!({
            "pins": [first, second, global, first],
            "playlist_pin_imported_sources": ["first", "second"]
        });
        let mut settings: SidebarSettings = serde_json::from_value(saved).unwrap();
        settings.sanitize();
        assert_eq!(
            settings.pins,
            [first.clone(), second.clone(), global.clone()]
        );
        assert!(settings.reorder_pin(&second, &global, true));
        assert_eq!(settings.pins, [first, global, second]);
        let encoded = serde_json::to_value(&settings).unwrap();
        assert_eq!(
            encoded["playlist_pin_imported_sources"],
            serde_json::json!(["first", "second"])
        );
        assert_eq!(
            serde_json::from_value::<SidebarSettings>(encoded).unwrap(),
            settings
        );
    }

    #[test]
    fn playlist_import_appends_globally_once_and_preserves_unpinning() {
        let first = SourceId::new("plex");
        let second = SourceId::new("emby");
        let mut settings = SidebarSettings::default();
        assert!(settings.import_playlist_pins_once(first.clone(), ["mix".into()]));
        assert!(settings.import_playlist_pins_once(second.clone(), ["mix".into()]));
        let removed = settings.pins[0].clone();
        let retained = settings.pins[1].clone();
        settings.set_pinned(removed, false);
        let mut restored: SidebarSettings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert!(!restored.import_playlist_pins_once(first, ["mix".into()]));
        assert!(!restored.import_playlist_pins_once(second, ["mix".into()]));
        assert_eq!(restored.pins, [retained]);
    }

    #[test]
    fn reorder_pin_places_relative_to_the_target_and_ignores_no_ops() {
        let first = pin("first");
        let second = pin("second");
        let third = pin("third");
        let mut settings = SidebarSettings {
            pins: vec![first.clone(), second.clone(), third.clone()],
            ..SidebarSettings::default()
        };

        assert!(settings.reorder_pin(&first, &second, true));
        assert_eq!(
            settings.pins,
            [second.clone(), first.clone(), third.clone()]
        );
        assert!(settings.reorder_pin(&third, &second, false));
        assert_eq!(settings.pins, [third, second.clone(), first]);
        assert!(!settings.reorder_pin(&second, &second, false));
    }

    #[test]
    fn pin_drop_direction_follows_the_existing_order() {
        let first = pin("first");
        let second = pin("second");
        let third = pin("third");
        let settings = SidebarSettings {
            pins: vec![first.clone(), second.clone(), third.clone()],
            ..SidebarSettings::default()
        };

        assert_eq!(settings.pin_drop_after(&first, &second), Some(true));
        assert_eq!(settings.pin_drop_after(&third, &second), Some(false));
        assert_eq!(settings.pin_drop_after(&second, &second), None);
        assert_eq!(settings.pin_drop_after(&pin("missing"), &second), None);
    }
}
