use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use library::{Database, PlaylistEntryKey, PlaylistEntryRow, PlaylistKey, SourceKey};
use playback::SourceSessionEpoch;

use crate::LibraryListSettings;

use super::sparse_model::{SparseObjectModel, SparseRouteModel};

const PLAYLIST_ENTRY_OVERSCAN: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlaylistEntryProjectionRequest {
    pub(crate) query: String,
    pub(crate) settings: LibraryListSettings,
}

#[derive(Clone)]
pub(crate) struct PlaylistEntryModel {
    inner: Rc<PlaylistEntryModelState>,
}

struct PlaylistEntryModelState {
    source_key: SourceKey,
    source_session_epoch: SourceSessionEpoch,
    playlist_key: PlaylistKey,
    sparse: Rc<SparseRouteModel<PlaylistEntryKey, PlaylistEntryRow>>,
    database: Arc<Database>,
    runtime: tokio::runtime::Handle,
    folder: Option<library::FolderKey>,
    request: RefCell<PlaylistEntryProjectionRequest>,
}

impl PlaylistEntryModel {
    pub(crate) fn new(
        selected: &crate::runtime::SelectedLibrary,
        playlist_key: PlaylistKey,
        order: Vec<PlaylistEntryKey>,
        settings: LibraryListSettings,
    ) -> Self {
        let source_key = selected.source_key;
        let folder = selected.music_folder_key;
        let database = Arc::clone(&selected.database);
        let loader_database = Arc::clone(&database);
        let load = Arc::new(
            move |keys: Vec<PlaylistEntryKey>, cancellation: library::ReadCancellation| {
                let database = Arc::clone(&loader_database);
                Box::pin(async move {
                    database
                        .playlist_entry_rows(source_key, &keys, folder, &cancellation)
                        .await
                        .map_err(|error| error.to_string())
                }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
            },
        );
        Self {
            inner: Rc::new(PlaylistEntryModelState {
                source_key,
                source_session_epoch: selected.source_session_epoch,
                playlist_key,
                sparse: SparseRouteModel::new(
                    order,
                    PLAYLIST_ENTRY_OVERSCAN,
                    selected.runtime.clone(),
                    load,
                ),
                database,
                runtime: selected.runtime.clone(),
                folder,
                request: RefCell::new(PlaylistEntryProjectionRequest {
                    query: String::new(),
                    settings,
                }),
            }),
        }
    }

    pub(crate) fn list_model(&self) -> SparseObjectModel {
        self.inner.sparse.list_model()
    }

    pub(crate) fn source_is_empty(&self) -> bool {
        self.inner.sparse.len() == 0
    }

    pub(crate) fn projection_request(&self) -> PlaylistEntryProjectionRequest {
        self.inner.request.borrow().clone()
    }

    pub(crate) fn set_query(&self, query: &str) -> bool {
        let query = query.trim();
        let mut request = self.inner.request.borrow_mut();
        if request.query == query {
            return false;
        }
        request.query = query.to_string();
        true
    }

    pub(crate) fn apply_settings(&self, settings: LibraryListSettings) -> bool {
        let mut request = self.inner.request.borrow_mut();
        if request.settings == settings {
            return false;
        }
        request.settings = settings;
        true
    }

    pub(crate) fn replace_order(&self, order: Vec<PlaylistEntryKey>) {
        self.inner.sparse.replace_order(order);
    }

    pub(crate) fn ready(&self, position: u32) -> Option<Arc<PlaylistEntryRow>> {
        self.inner.sparse.ready(position)
    }

    pub(crate) fn visible_context_id(&self) -> String {
        let request = self.inner.request.borrow();
        format!(
            "playlist:{}|query={}|sort={:?}|descending={}",
            self.inner.playlist_key,
            request.query,
            request.settings.sort_key,
            request.settings.descending
        )
    }

    pub(crate) fn position_for_current(
        &self,
        track: library::TrackKey,
        context_id: &str,
        source_rank: usize,
    ) -> Option<u32> {
        if context_id == self.visible_context_id()
            && self.inner.sparse.order().get(source_rank).is_some()
        {
            return u32::try_from(source_rank).ok();
        }
        if context_id != format!("playlist:{}", self.inner.playlist_key) {
            return None;
        }
        (0..self.inner.sparse.len()).find_map(|position| {
            let row = self.ready(position as u32)?;
            (row.track_key == Some(track) && row.position.max(0) as usize == source_rank)
                .then_some(position as u32)
        })
    }

    pub(crate) fn activate(&self, position: u32, queue: playback::QueueHandle) {
        let Some(row) = self.ready(position) else {
            return;
        };
        let database = Arc::clone(&self.inner.database);
        let source = self.inner.source_key;
        let epoch = self.inner.source_session_epoch;
        let playlist = self.inner.playlist_key;
        let folder = self.inner.folder;
        let entry = row.playlist_entry_key;
        let entries = self.inner.sparse.order();
        let context = self.visible_context_id();
        self.inner.runtime.spawn(async move {
            let cancellation = library::ReadCancellation::new();
            let Some((order, anchor_index, anchor)) = database
                .playlist_projection_playback(
                    source,
                    playlist,
                    &entries,
                    entry,
                    folder,
                    &cancellation,
                )
                .await
                .ok()
                .flatten()
            else {
                return;
            };
            let Some(request) = playback::LoadedPlayRequest::context(
                source,
                epoch,
                order.into(),
                playback::PlaybackMedia::from(anchor),
                anchor_index,
                playback::QueuePlacement::Now,
                context,
                false,
            ) else {
                return;
            };
            queue.play_loaded(request);
        });
    }
}
