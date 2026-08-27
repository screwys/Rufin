use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use library::{PlaylistEntryKey, PlaylistEntryOrder, PlaylistEntryRow, PlaylistKey, SourceKey};
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
    tracks: RefCell<Arc<[library::TrackKey]>>,
    track_positions: RefCell<Arc<[usize]>>,
    request: RefCell<PlaylistEntryProjectionRequest>,
}

impl PlaylistEntryModel {
    pub(crate) fn new(
        selected: &crate::runtime::SelectedLibrary,
        playlist_key: PlaylistKey,
        order: PlaylistEntryOrder,
        first_rows: Vec<PlaylistEntryRow>,
        settings: LibraryListSettings,
    ) -> Self {
        let source_key = selected.source_key;
        let folder = selected.music_folder_key;
        let loader_database = Arc::clone(&selected.database);
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
        let PlaylistEntryOrder {
            entries,
            tracks,
            track_positions,
        } = order;
        let sparse = SparseRouteModel::new(
            entries,
            PLAYLIST_ENTRY_OVERSCAN,
            selected.runtime.clone(),
            load,
        );
        sparse.seed_matching(first_rows, |row| row.playlist_entry_key);
        Self {
            inner: Rc::new(PlaylistEntryModelState {
                source_key,
                source_session_epoch: selected.source_session_epoch,
                playlist_key,
                sparse,
                tracks: RefCell::new(tracks.into()),
                track_positions: RefCell::new(track_positions.into()),
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

    pub(crate) fn replace_order(&self, order: PlaylistEntryOrder) {
        self.inner.tracks.replace(order.tracks.into());
        self.inner
            .track_positions
            .replace(order.track_positions.into());
        self.inner.sparse.replace_order(order.entries);
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
            && let Some(position) = self.inner.track_positions.borrow().get(source_rank)
        {
            return u32::try_from(*position).ok();
        }
        if context_id != format!("playlist:{}", self.inner.playlist_key) {
            return None;
        }
        self.inner.sparse.ready_position(|row| {
            row.track_key == Some(track) && row.position.max(0) as usize == source_rank
        })
    }

    pub(crate) fn activate(&self, position: u32, queue: playback::QueueHandle) {
        let Some(row) = self.ready(position) else {
            return;
        };
        let source = self.inner.source_key;
        let epoch = self.inner.source_session_epoch;
        let position = position as usize;
        let positions = self.inner.track_positions.borrow();
        let Ok(anchor_index) = positions.binary_search(&position) else {
            return;
        };
        let order = self.inner.tracks.borrow().clone();
        if order.get(anchor_index) != row.track_key.as_ref() {
            return;
        }
        let context = self.visible_context_id();
        let Some(track) = row.track.clone() else {
            return;
        };
        if let Some(request) = playback::LoadedPlayRequest::context(
            source,
            epoch,
            order,
            playback::PlaybackMedia::from(track),
            anchor_index,
            playback::QueuePlacement::Now,
            context,
            false,
        ) {
            queue.play_loaded(request);
        }
    }
}
