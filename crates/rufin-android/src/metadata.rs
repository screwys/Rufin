use std::sync::{Arc, Mutex};

use rufin_core::{
    metadata::{self, MetadataEditor, MetadataField, MetadataReceiver},
    runtime::SourceHandle,
};
use sources::{ArtworkChange, ArtworkEdit, ArtworkStorage, ImageBytes, SourceMetadataError};

use crate::{
    host::{AndroidError, error},
    library::AndroidLibrary,
};

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum AndroidMetadataError {
    #[error("Metadata editing is no longer available")]
    Unavailable,
    #[error("Metadata changed before it was saved")]
    Conflict,
    #[error("Local access is required for {source_path}")]
    LocalAccessRequired {
        source_id: String,
        source_path: String,
    },
    #[error("Metadata was saved but its source refresh failed: {reason}")]
    SavedRefreshFailed { reason: String },
    #[error("Some changes were saved: {reason}")]
    PartiallySaved { reason: String },
    #[error("{reason}")]
    Failure { reason: String },
}

impl From<SourceMetadataError> for AndroidMetadataError {
    fn from(value: SourceMetadataError) -> Self {
        match value {
            SourceMetadataError::Unavailable => Self::Unavailable,
            SourceMetadataError::Conflict => Self::Conflict,
            SourceMetadataError::LocalAccessRequired {
                source_id,
                source_path,
            } => Self::LocalAccessRequired {
                source_id: source_id.to_string(),
                source_path,
            },
            SourceMetadataError::SavedRefreshFailed(message) => {
                Self::SavedRefreshFailed { reason: message }
            }
            SourceMetadataError::PartiallySaved { message, .. } => {
                Self::PartiallySaved { reason: message }
            }
            SourceMetadataError::Write(message) => Self::Failure { reason: message },
        }
    }
}

#[derive(uniffi::Record)]
pub struct AndroidMetadataField {
    pub id: String,
    pub label: String,
    pub value: String,
    pub kind: String,
    pub writable: bool,
    pub mixed: bool,
    pub identified: bool,
}

#[derive(Clone, Copy, PartialEq, uniffi::Enum)]
pub enum AndroidArtworkStorage {
    Embedded,
    Folder,
    Server,
}

impl From<ArtworkStorage> for AndroidArtworkStorage {
    fn from(value: ArtworkStorage) -> Self {
        match value {
            ArtworkStorage::Embedded => Self::Embedded,
            ArtworkStorage::Folder => Self::Folder,
            ArtworkStorage::Server => Self::Server,
        }
    }
}

impl From<AndroidArtworkStorage> for ArtworkStorage {
    fn from(value: AndroidArtworkStorage) -> Self {
        match value {
            AndroidArtworkStorage::Embedded => Self::Embedded,
            AndroidArtworkStorage::Folder => Self::Folder,
            AndroidArtworkStorage::Server => Self::Server,
        }
    }
}

#[derive(uniffi::Record)]
pub struct AndroidMetadataState {
    pub fields: Vec<AndroidMetadataField>,
    pub locked: Option<bool>,
    pub lock_writable: bool,
    pub has_changes: bool,
    pub can_identify: bool,
    pub committed: Option<bool>,
    pub track_count: u64,
    pub scope_summary: String,
    pub artwork_revision: u64,
    pub max_artwork_bytes: u64,
    pub artwork_storage: AndroidArtworkStorage,
    pub can_embed_artwork: bool,
    pub can_remove_artwork: bool,
    pub has_current_artwork: bool,
    pub external_lookup_allowed: bool,
    pub search_artist: String,
    pub search_album: Option<String>,
}

#[derive(uniffi::Record)]
pub struct AndroidArtworkResult {
    pub title: String,
    pub detail: String,
    pub thumbnail_url: String,
    pub image_url: String,
    pub source_url: String,
}

struct EditorState {
    editor: MetadataEditor,
    artwork: Option<ArtworkChange>,
    current: Option<Arc<ImageBytes>>,
    storage: ArtworkStorage,
    artwork_revision: u64,
}

#[derive(uniffi::Object)]
pub struct AndroidMetadataEditor {
    source: SourceHandle,
    runtime: tokio::runtime::Handle,
    media_uri: String,
    external_lookup_allowed: bool,
    state: Mutex<EditorState>,
}

impl AndroidMetadataEditor {
    fn state(&self) -> Result<std::sync::MutexGuard<'_, EditorState>, AndroidError> {
        self.state.lock().map_err(error)
    }
}

#[uniffi::export]
impl AndroidLibrary {
    pub async fn edit_metadata(
        &self,
        kind: String,
        media_uri: String,
    ) -> Result<Arc<AndroidMetadataEditor>, AndroidMetadataError> {
        let receiver = match kind.as_str() {
            "track" => {
                MetadataReceiver::Track(metadata::track_metadata(&self.source, media_uri.clone()))
            }
            "album" | "album_header" => {
                MetadataReceiver::Album(metadata::album_metadata(&self.source, media_uri.clone()))
            }
            "artist" => {
                MetadataReceiver::Artist(metadata::artist_metadata(&self.source, media_uri.clone()))
            }
            _ => {
                return Err(AndroidMetadataError::Failure {
                    reason: "Choose a track, album or artist".into(),
                });
            }
        };
        let draft = self
            .runtime
            .spawn(async move { receiver.recv().await })
            .await
            .map_err(|value| AndroidMetadataError::Failure {
                reason: value.to_string(),
            })??;
        let storage = draft.artwork().storage;
        Ok(Arc::new(AndroidMetadataEditor {
            source: self.source.clone(),
            runtime: self.runtime.clone(),
            media_uri,
            external_lookup_allowed: self.settings.load().allows_external_metadata_lookup(),
            state: Mutex::new(EditorState {
                editor: MetadataEditor::new(draft),
                artwork: None,
                current: None,
                storage,
                artwork_revision: 0,
            }),
        }))
    }
}

#[uniffi::export]
impl AndroidMetadataEditor {
    pub fn snapshot(&self) -> Result<AndroidMetadataState, AndroidError> {
        let state = self.state()?;
        let editor = &state.editor;
        let (search_artist, search_album) = editor.artwork_search_fields();
        Ok(AndroidMetadataState {
            fields: editor
                .fields()
                .into_iter()
                .map(|field| AndroidMetadataField {
                    id: serde_json::to_string(&field.field).expect("Metadata field identifier"),
                    label: field.label,
                    value: field.value,
                    kind: match field.kind {
                        sources::MetadataFieldKind::Number => "Number",
                        sources::MetadataFieldKind::Date => "Date",
                        sources::MetadataFieldKind::Boolean => "Boolean",
                        sources::MetadataFieldKind::List => "List",
                        sources::MetadataFieldKind::Text => "Text",
                    }
                    .into(),
                    writable: field.writable,
                    mixed: field.mixed,
                    identified: editor.is_identified(field.field),
                })
                .collect(),
            locked: editor.locked(),
            lock_writable: editor.writable(MetadataField::Locked),
            has_changes: editor.has_changes() || state.artwork.is_some(),
            can_identify: editor.values().is_ok_and(|values| {
                metadata::identification_available(
                    editor.draft.source_search(),
                    self.external_lookup_allowed,
                    &values,
                )
            }),
            committed: editor.committed(),
            track_count: editor.draft.track_count() as u64,
            scope_summary: localization::trn_with(
                "Changes apply to {count} track",
                "Changes apply to {count} tracks",
                editor.draft.track_count() as u64,
                &[("count", &editor.draft.track_count().to_string())],
            ),
            artwork_revision: state.artwork_revision,
            max_artwork_bytes: metadata::MAX_ARTWORK_BYTES as u64,
            artwork_storage: state.storage.into(),
            can_embed_artwork: editor.draft.artwork().can_embed,
            can_remove_artwork: match &state.artwork {
                Some(ArtworkChange::Replace(_)) => true,
                Some(ArtworkChange::Remove) => false,
                None => editor.draft.artwork().binding.is_some(),
            },
            has_current_artwork: state.current.is_some(),
            external_lookup_allowed: self.external_lookup_allowed,
            search_artist,
            search_album,
        })
    }

    pub fn set_field(&self, id: String, value: String) -> Result<(), AndroidError> {
        let mut state = self.state()?;
        let field: MetadataField = serde_json::from_str(&id).map_err(error)?;
        let available = state.editor.has_field(field) && state.editor.writable(field);
        if !available {
            return Err(error("This source cannot edit this field"));
        }
        state.editor.set_text(field, value);
        Ok(())
    }

    pub fn set_locked(&self, locked: bool) -> Result<(), AndroidError> {
        let mut state = self.state()?;
        if !state.editor.writable(MetadataField::Locked) {
            return Err(error("This source cannot edit this field"));
        }
        state.editor.set_locked(locked);
        Ok(())
    }

    pub fn undo_identified(&self, id: String) -> Result<(), AndroidError> {
        let mut state = self.state()?;
        let field = serde_json::from_str(&id).map_err(error)?;
        state.editor.undo_identified(field);
        Ok(())
    }

    pub async fn identify(&self) -> Result<(), AndroidError> {
        let receiver = {
            let state = self.state()?;
            let values = state.editor.values().map_err(error)?;
            if !metadata::identification_available(
                state.editor.draft.source_search(),
                self.external_lookup_allowed,
                &values,
            ) {
                return Ok(());
            }
            values.identify(&self.source, self.media_uri.clone())
        };
        if let Some(identified) = self
            .runtime
            .spawn(async move { receiver.recv().await })
            .await
            .map_err(error)?
            .map_err(error)?
        {
            self.state()?
                .editor
                .apply_identified(identified.values, identified.token);
        }
        Ok(())
    }

    pub async fn save(&self) -> Result<(), AndroidMetadataError> {
        let (revision, token, edit, previous_artwork) = {
            let state = self
                .state
                .lock()
                .map_err(|value| AndroidMetadataError::Failure {
                    reason: value.to_string(),
                })?;
            let artwork = state.artwork.clone().map(|change| ArtworkEdit {
                change,
                storage: state.storage,
            });
            let edit = state
                .editor
                .edit(artwork)
                .map_err(|message| AndroidMetadataError::Failure { reason: message })?;
            (
                state.editor.draft.revision(),
                state.editor.token(),
                edit,
                state.editor.draft.artwork().binding.clone(),
            )
        };
        let receiver = metadata::write_reviewed_metadata(
            &self.source,
            self.media_uri.clone(),
            revision,
            token,
            edit,
            previous_artwork,
        );
        let result = self
            .runtime
            .spawn(async move {
                receiver
                    .recv()
                    .await
                    .unwrap_or(Err(SourceMetadataError::Unavailable))
            })
            .await
            .map_err(|value| AndroidMetadataError::Failure {
                reason: value.to_string(),
            })?;
        self.state
            .lock()
            .map_err(|value| AndroidMetadataError::Failure {
                reason: value.to_string(),
            })?
            .editor
            .record_save_result(&result);
        result.map_err(Into::into)
    }

    pub async fn load_current_artwork(&self) -> Result<(), AndroidError> {
        let binding = self.state()?.editor.draft.artwork().binding.clone();
        let Some(binding) = binding else {
            return Ok(());
        };
        let receiver = metadata::current_artwork(&self.source, binding);
        let image = self
            .runtime
            .spawn(async move { receiver.recv().await })
            .await
            .map_err(error)?
            .map_err(error)?
            .map_err(error)?;
        let mut state = self.state()?;
        state.current = image;
        state.artwork_revision += 1;
        Ok(())
    }

    pub fn artwork_bytes(&self) -> Result<Option<Vec<u8>>, AndroidError> {
        let state = self.state()?;
        Ok(match &state.artwork {
            Some(ArtworkChange::Replace(image)) => Some(image.bytes.clone()),
            Some(ArtworkChange::Remove) => None,
            None => state.current.as_ref().map(|image| image.bytes.clone()),
        })
    }

    pub async fn import_artwork(&self, bytes: Vec<u8>) -> Result<(), AndroidError> {
        let receiver = metadata::prepare_artwork(&self.source, bytes);
        let image = self
            .runtime
            .spawn(async move { receiver.recv().await })
            .await
            .map_err(error)?
            .map_err(error)?
            .map_err(error)?;
        let mut state = self.state()?;
        state.artwork = Some(ArtworkChange::Replace(image));
        state.artwork_revision += 1;
        Ok(())
    }

    pub fn use_current_artwork(&self) -> Result<(), AndroidError> {
        let mut state = self.state()?;
        if let Some(image) = state.current.clone() {
            state.artwork = Some(ArtworkChange::Replace(image));
            state.artwork_revision += 1;
        }
        Ok(())
    }

    pub fn remove_artwork(&self) -> Result<(), AndroidError> {
        let mut state = self.state()?;
        state.artwork = state
            .editor
            .draft
            .artwork()
            .binding
            .is_some()
            .then_some(ArtworkChange::Remove);
        state.artwork_revision += 1;
        Ok(())
    }

    pub fn set_artwork_storage(&self, storage: AndroidArtworkStorage) -> Result<(), AndroidError> {
        let mut state = self.state()?;
        let editing = state.editor.draft.artwork();
        if editing.storage == ArtworkStorage::Server
            || storage == AndroidArtworkStorage::Server
            || storage == AndroidArtworkStorage::Embedded && !editing.can_embed
        {
            return Err(error("This source cannot use that artwork storage"));
        }
        state.storage = storage.into();
        Ok(())
    }

    pub async fn search_artwork(
        &self,
        artist: String,
        album: String,
    ) -> Result<Vec<AndroidArtworkResult>, AndroidError> {
        let query = self.state()?.editor.artwork_query(artist, album);
        let receiver = metadata::search_artwork(&self.source, query);
        let results = self
            .runtime
            .spawn(async move { receiver.recv().await })
            .await
            .map_err(error)?
            .map_err(error)?
            .map_err(error)?;
        Ok(results
            .into_iter()
            .map(|result| AndroidArtworkResult {
                title: result.title,
                detail: result.detail,
                thumbnail_url: result.thumbnail_url,
                image_url: result.image_url,
                source_url: result.source_url,
            })
            .collect())
    }

    pub async fn image_bytes(&self, url: String) -> Result<Vec<u8>, AndroidError> {
        let receiver = metadata::download_artwork(&self.source, url);
        let image = self
            .runtime
            .spawn(async move { receiver.recv().await })
            .await
            .map_err(error)?
            .map_err(error)?
            .map_err(error)?;
        Ok(image.bytes.clone())
    }

    pub async fn choose_artwork(&self, url: String) -> Result<(), AndroidError> {
        let receiver = metadata::download_artwork(&self.source, url);
        let image = self
            .runtime
            .spawn(async move { receiver.recv().await })
            .await
            .map_err(error)?
            .map_err(error)?
            .map_err(error)?;
        let mut state = self.state()?;
        state.artwork = Some(ArtworkChange::Replace(image));
        state.artwork_revision += 1;
        Ok(())
    }
}
