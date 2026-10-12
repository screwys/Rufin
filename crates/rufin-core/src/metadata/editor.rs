use localization::{msgid, tr};
use sources::{
    ArtworkEdit, MetadataChanges, MetadataDraft, MetadataEdit, MetadataField, SourceEntityKind,
    SourceMetadataError,
};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

struct IdentifiedField {
    original: String,
    touched: bool,
    undone: bool,
}

pub struct MetadataEditor {
    pub draft: Arc<MetadataDraft>,
    values: MetadataChanges,
    touched: HashSet<String>,
    token: Option<String>,
    identified: HashMap<String, IdentifiedField>,
    committed: Option<bool>,
}

impl MetadataEditor {
    pub fn new(draft: MetadataDraft) -> Self {
        let mut editor = Self {
            draft: Arc::new(draft),
            values: MetadataChanges::new(),
            touched: HashSet::new(),
            token: None,
            identified: HashMap::new(),
            committed: None,
        };
        for field in &editor.draft.fields {
            editor.values.insert(
                field.key.clone(),
                field.suggestion.as_ref().unwrap_or(&field.value).clone(),
            );
            if field.suggestion.is_some() && field.writable {
                editor.identified.insert(
                    field.key.clone(),
                    IdentifiedField {
                        original: field.value.clone(),
                        touched: false,
                        undone: false,
                    },
                );
                editor.touched.insert(field.key.clone());
            }
        }
        editor
    }

    pub fn fields(&self) -> Vec<MetadataField> {
        self.draft
            .fields
            .iter()
            .filter(|field| field.key != "locked")
            .map(|field| {
                let mut field = field.clone();
                field.value = self.text(&field.key).to_string();
                field.label = tr(&field.label);
                field
            })
            .collect()
    }

    pub fn text(&self, key: &str) -> &str {
        self.values.get(key).map(String::as_str).unwrap_or_default()
    }

    pub fn has_field(&self, key: &str) -> bool {
        self.values.contains_key(key)
    }

    pub fn writable(&self, key: &str) -> bool {
        self.draft
            .fields
            .iter()
            .any(|field| field.key == key && field.writable)
    }

    pub fn set_text(&mut self, key: &str, value: String) {
        self.values.insert(key.to_string(), value);
        self.touched.insert(key.to_string());
    }

    pub fn locked(&self) -> Option<bool> {
        self.values
            .get("locked")
            .and_then(|value| value.parse().ok())
    }

    pub fn set_locked(&mut self, value: bool) {
        if self.locked() != Some(value) {
            self.set_text("locked", value.to_string());
        }
    }

    pub fn is_identified(&self, key: &str) -> bool {
        self.identified.get(key).is_some_and(|field| !field.undone)
    }

    pub fn undo_identified(&mut self, key: &str) -> bool {
        let Some(field) = self.identified.get_mut(key).filter(|field| !field.undone) else {
            return false;
        };
        self.values.insert(key.to_string(), field.original.clone());
        if field.touched {
            self.touched.insert(key.to_string());
        } else {
            self.touched.remove(key);
        }
        field.undone = true;
        if self.identified.values().all(|field| field.undone) {
            self.token = None;
            self.identified.clear();
        }
        true
    }

    pub fn has_changes(&self) -> bool {
        !self.touched.is_empty() || self.token.is_some()
    }

    pub fn token(&self) -> Option<String> {
        self.token.clone()
    }

    pub fn values(&self) -> MetadataChanges {
        self.values
            .iter()
            .map(|(key, value)| (key.clone(), value.trim().to_string()))
            .collect()
    }

    pub fn edit(&self, artwork: Option<ArtworkEdit>) -> Result<MetadataEdit, String> {
        let mut changes = MetadataChanges::new();
        for field in &self.draft.fields {
            let restore = self.token.is_some()
                && self
                    .identified
                    .get(&field.key)
                    .is_some_and(|field| field.undone);
            if !field.writable || (!self.touched.contains(&field.key) && !restore) {
                continue;
            }
            let value = self.text(&field.key).trim();
            if field.key == "title" && value.is_empty() {
                return Err(tr(msgid("Add a title")));
            }
            if !value.is_empty()
                && matches!(
                    field.key.as_str(),
                    "track_number" | "disc_number" | "year" | "bpm"
                )
                && value
                    .parse::<u16>()
                    .ok()
                    .filter(|value| *value > 0)
                    .is_none()
            {
                return Err(tr(msgid("Use a number above zero")));
            }
            changes.insert(field.key.clone(), value.to_string());
        }
        Ok(MetadataEdit {
            kind: self.draft.kind,
            changes,
            artwork,
        })
    }

    pub fn apply_identified(&mut self, changes: MetadataChanges, token: Option<String>) {
        for (mut key, value) in changes {
            if key == "year" && self.has_field("recording_date") {
                key = "recording_date".into();
                // A year-only lookup must not shorten a matching full date.
                if self.text(&key).split('-').next() == Some(value.as_str()) {
                    continue;
                }
            }
            if !self.writable(&key) || self.text(&key) == value {
                continue;
            }
            if !self.is_identified(&key) {
                self.identified.insert(
                    key.clone(),
                    IdentifiedField {
                        original: self.text(&key).to_string(),
                        touched: self.touched.contains(&key),
                        undone: false,
                    },
                );
            }
            self.set_text(&key, value);
        }
        self.token = token;
        if self.token.is_none() {
            self.identified.retain(|_, field| !field.undone);
        }
    }

    pub fn artwork_search_fields(&self) -> (String, Option<String>) {
        let (artist, album) = match self.draft.kind {
            SourceEntityKind::Track => (
                self.text("album_artist"),
                Some(self.text("album").to_string()),
            ),
            SourceEntityKind::Album => (
                self.text("album_artist"),
                Some(self.text("title").to_string()),
            ),
            SourceEntityKind::Artist => (self.text("title"), None),
        };
        let artist = if artist.trim().is_empty() {
            self.text("artist")
        } else {
            artist
        };
        (artist.to_string(), album)
    }

    pub fn artwork_query(&self, artist: String, album: String) -> super::ArtworkQuery {
        let artist = artist.trim().to_string();
        let album = album.trim().to_string();
        let (draft_artist, draft_album) = self.artwork_search_fields();
        let same_identity = artist == draft_artist.trim()
            && album == draft_album.as_deref().unwrap_or_default().trim();
        let value = |key| {
            (!self.text(key).trim().is_empty() && same_identity)
                .then(|| self.text(key).trim().to_string())
        };
        if self.draft.kind == SourceEntityKind::Artist {
            super::ArtworkQuery::Artist {
                name: artist,
                musicbrainz_id: value("musicbrainz_artist_id"),
            }
        } else {
            super::ArtworkQuery::Album {
                artist,
                album,
                release_id: value("musicbrainz_album_id"),
                release_group_id: value("musicbrainz_release_group_id"),
            }
        }
    }

    pub fn record_save_result(&mut self, result: &Result<(), SourceMetadataError>) {
        self.committed = match result {
            Ok(()) | Err(SourceMetadataError::SavedRefreshFailed(_)) => Some(true),
            Err(SourceMetadataError::PartiallySaved { .. }) => Some(false),
            Err(_) => None,
        };
    }

    pub fn committed(&self) -> Option<bool> {
        self.committed
    }
}

pub fn identification_available(
    source_search: bool,
    external_lookup_allowed: bool,
    kind: SourceEntityKind,
    values: &MetadataChanges,
) -> bool {
    source_search
        && values
            .get("title")
            .is_some_and(|value| !value.trim().is_empty())
        || external_lookup_allowed && exact_identity(kind, values)
}

pub(super) fn exact_identity(kind: SourceEntityKind, values: &MetadataChanges) -> bool {
    let keys: &[&str] = match kind {
        SourceEntityKind::Track => &["musicbrainz_recording_id", "musicbrainz_release_track_id"],
        SourceEntityKind::Album => &["musicbrainz_album_id", "musicbrainz_release_group_id"],
        SourceEntityKind::Artist => &["musicbrainz_artist_id"],
    };
    keys.iter().any(|key| {
        values
            .get(*key)
            .is_some_and(|value| !value.trim().is_empty())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_mode_keeps_provider_source_search_available() {
        let values = [("title".into(), "Track".into())].into();
        assert!(identification_available(
            true,
            false,
            SourceEntityKind::Track,
            &values
        ));
        assert!(!identification_available(
            false,
            false,
            SourceEntityKind::Track,
            &values
        ));
    }
}
