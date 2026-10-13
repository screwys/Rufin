use super::{ArtworkResult, MetadataItemId};
use crate::source::SourceOwner;
use localization::{msgid, tr, trn_with};
use sources::{
    ArtworkChange, ArtworkEdit, ArtworkStorage, ImageBytes, MetadataChanges, MetadataDraft,
    MetadataEdit, MetadataField, SourceEntityKind, SourceMetadataError,
};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, MutexGuard},
};

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct MetadataFieldPresentation {
    pub field: MetadataField,
    pub identified: bool,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct MetadataEditorSnapshot {
    pub fields: Vec<MetadataFieldPresentation>,
    pub locked: Option<bool>,
    pub lock_writable: bool,
    pub has_changes: bool,
    pub can_identify: bool,
    pub committed: Option<bool>,
    pub track_count: u64,
    pub scope_summary: String,
    pub artwork_revision: u64,
    pub max_artwork_bytes: u64,
    pub artwork_storage: ArtworkStorage,
    pub can_embed_artwork: bool,
    pub can_remove_artwork: bool,
    pub has_current_artwork: bool,
    pub external_lookup_allowed: bool,
    pub search_artist: String,
    pub search_album: Option<String>,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct MetadataEditor {
    pub draft: Arc<MetadataDraft>,
    source: Arc<SourceOwner>,
    item: MetadataItemId,
    external_lookup_allowed: bool,
    state: Mutex<Editing>,
}

impl MetadataEditor {
    pub async fn open(
        source: Arc<SourceOwner>,
        item: MetadataItemId,
    ) -> Result<Arc<Self>, SourceMetadataError> {
        let draft = super::read_metadata(&source, item.clone())
            .recv()
            .await
            .unwrap_or(Err(SourceMetadataError::Unavailable))?;
        let state = Mutex::new(Editing::new(&draft));
        let external_lookup_allowed = source
            .shared
            .settings
            .load()
            .ui
            .allows_external_metadata_lookup();
        Ok(Arc::new(Self {
            draft: Arc::new(draft),
            source,
            item,
            external_lookup_allowed,
            state,
        }))
    }

    fn state(&self) -> MutexGuard<'_, Editing> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl MetadataEditor {
    pub fn snapshot(&self) -> MetadataEditorSnapshot {
        let state = self.state();
        let (search_artist, search_album) = state.artwork_search_fields(&self.draft);
        MetadataEditorSnapshot {
            fields: state.fields(&self.draft),
            locked: state.locked(),
            lock_writable: state.writable(&self.draft, "locked"),
            has_changes: state.has_changes() || state.artwork.is_some(),
            can_identify: identification_available(
                self.draft.source_search,
                self.external_lookup_allowed,
                self.draft.kind,
                &state.values,
            ),
            committed: state.committed,
            track_count: self.draft.track_count as u64,
            scope_summary: trn_with(
                "Changes apply to {count} track",
                "Changes apply to {count} tracks",
                self.draft.track_count as u64,
                &[("count", &self.draft.track_count.to_string())],
            ),
            artwork_revision: state.artwork_revision,
            max_artwork_bytes: super::MAX_ARTWORK_BYTES as u64,
            artwork_storage: state.storage,
            can_embed_artwork: self.draft.artwork.can_embed,
            can_remove_artwork: match &state.artwork {
                Some(ArtworkChange::Replace(_)) => true,
                Some(ArtworkChange::Remove) => false,
                None => self.draft.artwork.binding.is_some(),
            },
            has_current_artwork: state.current.is_some(),
            external_lookup_allowed: self.external_lookup_allowed,
            search_artist,
            search_album,
        }
    }

    pub fn set_field(&self, key: String, value: String) -> Result<(), SourceMetadataError> {
        let mut state = self.state();
        if !state.writable(&self.draft, &key) {
            return Err(SourceMetadataError::InvalidInput(tr(
                "This source cannot edit this field",
            )));
        }
        if state.text(&key) != value {
            state.set_text(&key, value);
        }
        Ok(())
    }

    pub fn set_locked(&self, value: bool) -> Result<(), SourceMetadataError> {
        self.set_field("locked".into(), value.to_string())
    }

    pub fn undo_identified(&self, key: String) -> bool {
        self.state().undo_identified(&key)
    }

    pub async fn identify(&self) -> Result<(), SourceMetadataError> {
        let values = self.state().values();
        if !identification_available(
            self.draft.source_search,
            self.external_lookup_allowed,
            self.draft.kind,
            &values,
        ) {
            return Ok(());
        }
        if let Some(identified) = super::identify_metadata(
            &self.source,
            self.item.media_uri().into(),
            self.draft.kind,
            values,
        )
        .recv()
        .await
        .unwrap_or_else(|_| Err(tr("Metadata editing is no longer available")))
        .map_err(SourceMetadataError::Write)?
        {
            self.state()
                .apply_identified(&self.draft, identified.changes, identified.token);
        }
        Ok(())
    }

    pub async fn save(&self) -> Result<(), SourceMetadataError> {
        let (edit, token) = {
            let state = self.state();
            (
                state
                    .edit(&self.draft)
                    .map_err(SourceMetadataError::InvalidInput)?,
                state.token.clone(),
            )
        };
        let result = super::write_reviewed_metadata(
            &self.source,
            self.item.media_uri().into(),
            self.draft.revision.clone(),
            token,
            edit,
            self.draft.artwork.binding.clone(),
        )
        .recv()
        .await
        .unwrap_or(Err(SourceMetadataError::Unavailable));
        self.state().record_save_result(&result);
        result
    }

    pub async fn load_current_artwork(
        &self,
    ) -> Result<Option<Arc<ImageBytes>>, SourceMetadataError> {
        let Some(binding) = self.draft.artwork.binding.clone() else {
            return Ok(None);
        };
        let image = super::current_artwork(&self.source, binding)
            .recv()
            .await
            .map_err(|_| SourceMetadataError::Unavailable)?
            .map_err(SourceMetadataError::Write)?;
        let mut state = self.state();
        state.current = image.clone();
        state.artwork_revision += 1;
        Ok(image)
    }

    pub fn artwork(&self) -> Option<Arc<ImageBytes>> {
        let state = self.state();
        match &state.artwork {
            Some(ArtworkChange::Replace(image)) => Some(image.clone()),
            Some(ArtworkChange::Remove) => None,
            None => state.current.clone(),
        }
    }

    pub fn artwork_changed(&self) -> bool {
        self.state().artwork.is_some()
    }

    pub async fn prepare_artwork(
        &self,
        bytes: Vec<u8>,
    ) -> Result<Arc<ImageBytes>, SourceMetadataError> {
        super::prepare_artwork(&self.source, bytes)
            .recv()
            .await
            .map_err(|_| SourceMetadataError::Unavailable)?
            .map_err(SourceMetadataError::Write)
    }

    pub fn select_artwork(&self, image: Arc<ImageBytes>) {
        let mut state = self.state();
        state.artwork = Some(ArtworkChange::Replace(image));
        state.artwork_revision += 1;
    }

    pub fn use_current_artwork(&self) {
        let mut state = self.state();
        if let Some(image) = state.current.clone() {
            state.artwork = Some(ArtworkChange::Replace(image));
            state.artwork_revision += 1;
        }
    }

    pub fn remove_artwork(&self) {
        let mut state = self.state();
        state.artwork = self
            .draft
            .artwork
            .binding
            .is_some()
            .then_some(ArtworkChange::Remove);
        state.artwork_revision += 1;
    }

    pub fn set_artwork_storage(&self, storage: ArtworkStorage) {
        self.state().storage = storage;
    }

    pub async fn search_artwork(
        &self,
        artist: String,
        album: String,
    ) -> Result<Vec<ArtworkResult>, SourceMetadataError> {
        let query = self.state().artwork_query(&self.draft, artist, album);
        super::search_artwork(&self.source, query)
            .recv()
            .await
            .map_err(|_| SourceMetadataError::Unavailable)?
            .map_err(SourceMetadataError::Write)
    }

    pub async fn download_artwork(
        &self,
        url: String,
    ) -> Result<Arc<ImageBytes>, SourceMetadataError> {
        super::download_artwork(&self.source, url)
            .recv()
            .await
            .map_err(|_| SourceMetadataError::Unavailable)?
            .map_err(SourceMetadataError::Write)
    }
}

struct IdentifiedField {
    original: String,
    touched: bool,
    undone: bool,
}

struct Editing {
    values: MetadataChanges,
    touched: HashSet<String>,
    token: Option<String>,
    identified: HashMap<String, IdentifiedField>,
    committed: Option<bool>,
    artwork: Option<ArtworkChange>,
    current: Option<Arc<ImageBytes>>,
    storage: ArtworkStorage,
    artwork_revision: u64,
}

impl Editing {
    fn new(draft: &MetadataDraft) -> Self {
        let mut editor = Self {
            values: MetadataChanges::new(),
            touched: HashSet::new(),
            token: None,
            identified: HashMap::new(),
            committed: None,
            artwork: None,
            current: None,
            storage: draft.artwork.storage,
            artwork_revision: 0,
        };
        for field in &draft.fields {
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

    fn fields(&self, draft: &MetadataDraft) -> Vec<MetadataFieldPresentation> {
        draft
            .fields
            .iter()
            .filter(|field| field.key != "locked")
            .map(|field| {
                let mut field = field.clone();
                field.value = self.text(&field.key).to_string();
                field.label = tr(&field.label);
                MetadataFieldPresentation {
                    identified: self.is_identified(&field.key),
                    field,
                }
            })
            .collect()
    }

    fn text(&self, key: &str) -> &str {
        self.values.get(key).map(String::as_str).unwrap_or_default()
    }

    fn has_field(&self, key: &str) -> bool {
        self.values.contains_key(key)
    }

    fn writable(&self, draft: &MetadataDraft, key: &str) -> bool {
        draft
            .fields
            .iter()
            .any(|field| field.key == key && field.writable)
    }

    fn set_text(&mut self, key: &str, value: String) {
        self.values.insert(key.to_string(), value);
        self.touched.insert(key.to_string());
    }

    fn locked(&self) -> Option<bool> {
        self.values
            .get("locked")
            .and_then(|value| value.parse().ok())
    }

    fn is_identified(&self, key: &str) -> bool {
        self.identified.get(key).is_some_and(|field| !field.undone)
    }

    fn undo_identified(&mut self, key: &str) -> bool {
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

    fn has_changes(&self) -> bool {
        !self.touched.is_empty() || self.token.is_some()
    }

    fn values(&self) -> MetadataChanges {
        self.values
            .iter()
            .map(|(key, value)| (key.clone(), value.trim().to_string()))
            .collect()
    }

    fn edit(&self, draft: &MetadataDraft) -> Result<MetadataEdit, String> {
        let mut changes = MetadataChanges::new();
        for field in &draft.fields {
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
            kind: draft.kind,
            changes,
            artwork: self.artwork.clone().map(|change| ArtworkEdit {
                change,
                storage: self.storage,
            }),
        })
    }

    fn apply_identified(
        &mut self,
        draft: &MetadataDraft,
        changes: MetadataChanges,
        token: Option<String>,
    ) {
        for (mut key, value) in changes {
            if key == "year" && self.has_field("recording_date") {
                key = "recording_date".into();
                // A year-only lookup must not shorten a matching full date.
                if self.text(&key).split('-').next() == Some(value.as_str()) {
                    continue;
                }
            }
            if !self.writable(draft, &key) || self.text(&key) == value {
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

    fn artwork_search_fields(&self, draft: &MetadataDraft) -> (String, Option<String>) {
        let (artist, album) = match draft.kind {
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

    fn artwork_query(
        &self,
        draft: &MetadataDraft,
        artist: String,
        album: String,
    ) -> super::ArtworkQuery {
        let artist = artist.trim().to_string();
        let album = album.trim().to_string();
        let (draft_artist, draft_album) = self.artwork_search_fields(draft);
        let same_identity = artist == draft_artist.trim()
            && album == draft_album.as_deref().unwrap_or_default().trim();
        let value = |key| {
            (!self.text(key).trim().is_empty() && same_identity)
                .then(|| self.text(key).trim().to_string())
        };
        if draft.kind == SourceEntityKind::Artist {
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

    fn record_save_result(&mut self, result: &Result<(), SourceMetadataError>) {
        self.committed = match result {
            Ok(()) | Err(SourceMetadataError::SavedRefreshFailed(_)) => Some(true),
            Err(SourceMetadataError::PartiallySaved { .. }) => Some(false),
            Err(_) => None,
        };
    }
}

fn identification_available(
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
