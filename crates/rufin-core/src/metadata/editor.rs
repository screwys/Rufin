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
pub struct MetadataSaveSummary {
    pub tracks: u64,
    pub message: String,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct MetadataFieldPresentation {
    pub field: MetadataField,
    pub identified: bool,
    pub original_value: String,
    pub original_mixed: bool,
    pub mixed_label: String,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct MetadataEditorSnapshot {
    pub fields: Vec<MetadataFieldPresentation>,
    pub locked: Option<bool>,
    pub lock_writable: bool,
    pub lock_mixed: bool,
    pub has_changes: bool,
    pub committed: Option<bool>,
    pub track_count: u64,
    pub selected_count: u64,
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
    pub(super) source: Arc<SourceOwner>,
    pub(super) session: Option<Arc<super::EditSession>>,
    pub(super) batch: Option<super::batch::BatchScope>,
    pub(super) external_lookup_allowed: bool,
    pub(super) state: Mutex<Editing>,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl MetadataEditor {
    #[cfg_attr(feature = "bindings", uniffi::constructor)]
    pub async fn open(
        source: Arc<SourceOwner>,
        item: MetadataItemId,
    ) -> Result<Arc<Self>, SourceMetadataError> {
        let runtime = source.shared.runtime.clone();
        super::on_runtime(runtime, async {
            let (session, draft) = super::EditSession::open(&source, &item).await?;
            if draft.kind != SourceEntityKind::Track
                && matches!(session.destination, super::EditDestination::Files(_))
            {
                return super::MetadataBatch::open_scope(source, session, draft)
                    .await?
                    .scope_editor()
                    .await;
            }
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
                session: Some(session),
                batch: None,
                external_lookup_allowed,
                state,
            }))
        })
        .await
    }
}

impl MetadataEditor {
    pub(super) fn state(&self) -> MutexGuard<'_, Editing> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl MetadataEditor {
    pub fn track_batch(&self) -> Option<Arc<super::MetadataBatch>> {
        self.batch.as_ref().map(|scope| scope.batch.clone())
    }

    pub fn snapshot(&self) -> MetadataEditorSnapshot {
        let state = self.state();
        let (search_artist, search_album) = state.artwork_search_fields(&self.draft);
        MetadataEditorSnapshot {
            fields: state.fields(&self.draft),
            locked: state.locked(),
            lock_writable: state.writable(&self.draft, "locked"),
            lock_mixed: state.mixed.contains("locked"),
            has_changes: state.has_changes(&self.draft)
                || state.artwork.is_some()
                || self
                    .batch
                    .as_ref()
                    .is_some_and(|scope| scope.batch.has_changes()),
            committed: state.committed,
            track_count: self.draft.track_count as u64,
            selected_count: self.batch.as_ref().map_or(1, |scope| {
                if self.draft.kind == SourceEntityKind::Track {
                    scope.indices.len() as u64
                } else {
                    1
                }
            }),
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
                None => {
                    self.draft.artwork.binding.is_some()
                        || self.batch.as_ref().is_some_and(|scope| scope.has_artwork)
                }
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
        if state.text(&key) != value || state.mixed.contains(&key) {
            state.set_text(&key, value);
        }
        Ok(())
    }

    pub async fn stage_changes(&self) -> Result<(), SourceMetadataError> {
        let runtime = self.source.shared.runtime.clone();
        super::on_runtime(runtime, async {
            if let Some(scope) = &self.batch {
                scope.batch.stage(self).await?;
            }
            Ok(())
        })
        .await
    }

    pub async fn reset_field(&self, key: String) -> Result<(), SourceMetadataError> {
        let runtime = self.source.shared.runtime.clone();
        super::on_runtime(runtime, async {
            if self.draft.kind != SourceEntityKind::Track
                && let Some(scope) = &self.batch
            {
                scope.batch.reset_scope(key.clone()).await?;
                self.reload_batch().await?;
            } else if let Some(scope) = &self.batch {
                let mut pending = self.state().clone();
                pending.touched.remove(&key);
                scope.batch.reset(&scope.indices, key).await?;
                self.reload_batch().await?;
                let mut state = self.state();
                for key in &pending.touched {
                    if let Some(value) = pending.values.get(key) {
                        state.set_text(key, value.clone());
                    }
                }
                state.lyrics = pending.lyrics;
                state.artwork = pending.artwork;
                state.storage = pending.storage;
            } else {
                let mut state = self.state();
                if !state.undo_identified(&key) {
                    if let Some(field) = self.draft.fields.iter().find(|field| field.key == key) {
                        state.values.insert(key.clone(), field.value.clone());
                        state.touched.remove(&key);
                        if field.mixed {
                            state.mixed.insert(key);
                        }
                    }
                }
            }
            Ok(())
        })
        .await
    }

    pub fn set_locked(&self, value: bool) -> Result<(), SourceMetadataError> {
        self.set_field("locked".into(), value.to_string())
    }

    pub async fn prepare_save(&self) -> Result<MetadataSaveSummary, SourceMetadataError> {
        let runtime = self.source.shared.runtime.clone();
        super::on_runtime(runtime, async {
            self.stage_changes().await?;
            let tracks = if let Some(scope) = &self.batch {
                scope
                    .batch
                    .work(|records| Ok(records.changed.len() as u64))
                    .await?
            } else {
                let state = self.state();
                if state.has_changes(&self.draft) || state.artwork.is_some() {
                    self.draft.track_count.max(1) as u64
                } else {
                    0
                }
            };
            Ok(MetadataSaveSummary {
                tracks,
                message: trn_with(
                    "Do you want to apply metadata changes to {count} track?",
                    "Do you want to apply metadata changes to {count} tracks?",
                    tracks,
                    &[("count", &tracks.to_string())],
                ),
            })
        })
        .await
    }

    pub async fn save(&self) -> Result<(), SourceMetadataError> {
        let runtime = self.source.shared.runtime.clone();
        super::on_runtime(runtime, async {
            let edit = self
                .state()
                .edit(&self.draft)
                .map_err(SourceMetadataError::InvalidInput)?;
            if let Some(scope) = &self.batch {
                scope.batch.stage(self).await?;
                let result = scope.batch.clone().save().await;
                self.reload_batch().await?;
                return result;
            }
            let lyrics = self.state().lyrics.clone();
            if edit.changes.is_empty() && edit.artwork.is_none() && lyrics.is_none() {
                return Ok(());
            }
            let result = self
                .session
                .as_ref()
                .unwrap()
                .save(&self.source, edit, lyrics)
                .await;
            self.state().record_save_result(&result);
            result
        })
        .await
    }

    pub async fn load_current_artwork(
        &self,
    ) -> Result<Option<Arc<ImageBytes>>, SourceMetadataError> {
        let runtime = self.source.shared.runtime.clone();
        super::on_runtime(runtime, async {
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
        })
        .await
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
        let runtime = self.source.shared.runtime.clone();
        super::on_runtime(runtime, async {
            super::prepare_artwork(&self.source, bytes)
                .recv()
                .await
                .map_err(|_| SourceMetadataError::Unavailable)?
                .map_err(SourceMetadataError::Write)
        })
        .await
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
        state.artwork = (self.draft.artwork.binding.is_some()
            || self.batch.as_ref().is_some_and(|scope| scope.has_artwork))
        .then_some(ArtworkChange::Remove);
        state.artwork_revision += 1;
    }

    pub fn set_artwork_storage(&self, storage: ArtworkStorage) {
        let mut state = self.state();
        if state.storage != storage {
            state.storage = storage;
            if state.artwork.is_some() {
                state.artwork_revision += 1;
            }
        }
    }

    pub async fn search_artwork(
        &self,
        artist: String,
        album: String,
    ) -> Result<Vec<ArtworkResult>, SourceMetadataError> {
        let runtime = self.source.shared.runtime.clone();
        super::on_runtime(runtime, async {
            let query = self.state().artwork_query(&self.draft, artist, album);
            super::search_artwork(&self.source, query)
                .recv()
                .await
                .map_err(|_| SourceMetadataError::Unavailable)?
                .map_err(SourceMetadataError::Write)
        })
        .await
    }

    pub async fn download_artwork(
        &self,
        url: String,
    ) -> Result<Arc<ImageBytes>, SourceMetadataError> {
        let runtime = self.source.shared.runtime.clone();
        super::on_runtime(runtime, async {
            super::download_artwork(&self.source, url)
                .recv()
                .await
                .map_err(|_| SourceMetadataError::Unavailable)?
                .map_err(SourceMetadataError::Write)
        })
        .await
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct IdentifiedField {
    pub(super) original: String,
    pub(super) original_mixed: bool,
    pub(super) touched: bool,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct Editing {
    pub(super) mixed: HashSet<String>,
    pub(super) values: MetadataChanges,
    pub(super) touched: HashSet<String>,
    pub(super) lyrics: Option<super::lyrics::LyricsEdit>,
    pub(super) recordings: Vec<String>,
    pub(super) scan_error: Option<String>,
    pub(super) lookup_candidates: Option<Vec<super::MetadataCandidate>>,
    pub(super) lookup_result: Option<(super::MetadataCandidate, MetadataChanges)>,
    pub(super) lookup_preview: Option<super::MetadataCandidate>,
    pub(super) identified: HashMap<String, IdentifiedField>,
    pub(super) committed: Option<bool>,
    #[serde(skip)]
    pub(super) artwork: Option<ArtworkChange>,
    #[serde(skip)]
    pub(super) current: Option<Arc<ImageBytes>>,
    pub(super) storage: ArtworkStorage,
    pub(super) artwork_revision: u64,
}

impl Editing {
    pub(super) fn new(draft: &MetadataDraft) -> Self {
        let mut editor = Self {
            mixed: draft
                .fields
                .iter()
                .filter(|field| field.mixed)
                .map(|field| field.key.clone())
                .collect(),
            values: MetadataChanges::new(),
            touched: HashSet::new(),
            lyrics: None,
            recordings: Vec::new(),
            scan_error: None,
            lookup_candidates: None,
            lookup_result: None,
            lookup_preview: None,
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
                        original_mixed: field.mixed,
                        touched: false,
                    },
                );
                editor.touched.insert(field.key.clone());
            }
        }
        editor
    }

    pub(super) fn fields(&self, draft: &MetadataDraft) -> Vec<MetadataFieldPresentation> {
        draft
            .fields
            .iter()
            .filter(|field| field.key != "locked")
            .map(|field| {
                let original_value = field.value.clone();
                let original_mixed = field.mixed;
                let mut field = field.clone();
                field.value = self.text(&field.key).to_string();
                field.mixed = self.mixed.contains(&field.key);
                field.label = tr(&field.label);
                MetadataFieldPresentation {
                    identified: field.writable
                        && ((field.mixed && self.is_identified(&field.key))
                            || field.mixed != original_mixed
                            || (!field.mixed && field.value.trim() != original_value.trim())),
                    mixed_label: mixed_field_label(&field.label, draft.track_count as u64),
                    field,
                    original_value,
                    original_mixed,
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

    pub(super) fn set_text(&mut self, key: &str, value: String) {
        self.mixed.remove(key);
        self.values.insert(key.to_string(), value);
        self.touched.insert(key.to_string());
    }

    fn locked(&self) -> Option<bool> {
        if self.mixed.contains("locked") {
            return None;
        }
        self.values
            .get("locked")
            .and_then(|value| value.parse().ok())
    }

    pub(super) fn is_identified(&self, key: &str) -> bool {
        self.identified.contains_key(key)
    }

    pub(super) fn undo_identified(&mut self, key: &str) -> bool {
        let Some(field) = self.identified.remove(key) else {
            return false;
        };
        self.values.insert(key.to_string(), field.original.clone());
        if field.original_mixed {
            self.mixed.insert(key.to_string());
        } else {
            self.mixed.remove(key);
        }
        if field.touched {
            self.touched.insert(key.to_string());
        } else {
            self.touched.remove(key);
        }
        true
    }

    pub(super) fn field_changed(&self, field: &MetadataField) -> bool {
        field.writable
            && self.touched.contains(&field.key)
            && (field.mixed || self.text(&field.key).trim() != field.value.trim())
    }

    pub(super) fn has_changes(&self, draft: &MetadataDraft) -> bool {
        draft.fields.iter().any(|field| self.field_changed(field)) || self.lyrics.is_some()
    }

    pub(super) fn values(&self) -> MetadataChanges {
        self.values
            .iter()
            .map(|(key, value)| (key.clone(), value.trim().to_string()))
            .collect()
    }

    pub(super) fn edit(&self, draft: &MetadataDraft) -> Result<MetadataEdit, String> {
        self.edit_fields(
            draft,
            draft
                .fields
                .iter()
                .filter(|field| self.field_changed(field)),
        )
    }

    pub(super) fn stage_edit(&self, draft: &MetadataDraft) -> Result<MetadataEdit, String> {
        self.edit_fields(
            draft,
            draft
                .fields
                .iter()
                .filter(|field| field.writable && self.touched.contains(&field.key)),
        )
    }

    fn edit_fields<'a>(
        &self,
        draft: &MetadataDraft,
        fields: impl Iterator<Item = &'a MetadataField>,
    ) -> Result<MetadataEdit, String> {
        let mut changes = MetadataChanges::new();
        for field in fields {
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

    pub(super) fn apply_identified(
        &mut self,
        draft: &MetadataDraft,
        changes: MetadataChanges,
        fill_missing: bool,
    ) {
        for (mut key, value) in changes {
            if key == "year" && self.has_field("recording_date") {
                key = "recording_date".into();
                // A year-only lookup must not shorten a matching full date.
                if self.text(&key).split('-').next() == Some(value.as_str()) {
                    continue;
                }
            }
            if fill_missing && !self.text(&key).trim().is_empty() {
                continue;
            }
            if !self.writable(draft, &key) || self.text(&key) == value {
                continue;
            }
            if !self.is_identified(&key) {
                self.identified.insert(
                    key.clone(),
                    IdentifiedField {
                        original: self.text(&key).to_string(),
                        original_mixed: self.mixed.contains(&key),
                        touched: self.touched.contains(&key),
                    },
                );
            }
            self.set_text(&key, value);
        }
    }

    pub(super) fn begin_lookup(&mut self) {
        self.recordings.clear();
        self.lookup_preview = None;
        self.lookup_result = None;
        self.lookup_candidates = None;
        self.scan_error = None;
    }

    pub(super) fn apply_lookup_choice(&mut self, draft: &MetadataDraft, fill_missing: bool) {
        if let Some((candidate, changes)) = self.lookup_result.clone()
            && self.lookup_preview.as_ref() == Some(&candidate)
        {
            self.apply_identified(draft, changes, fill_missing);
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

pub fn mixed_field_label(field: &str, count: u64) -> String {
    trn_with(
        "{field} · Different across {count} track",
        "{field} · Different across {count} tracks",
        count,
        &[("field", field), ("count", &count.to_string())],
    )
}
