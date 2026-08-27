use crate::SettingsHandle;
use library::{Database, FolderKey, FolderRow, SourceKey, TrackKey};
use playback::{
    LoadedPlayRequest, PlaybackMedia, PlaybackProjection, QueuePlacement, SourceSessionEpoch,
};
use std::sync::Arc;

use super::source::{ConfiguredSources, SelectedSourceHandle, SourceOperation, SourceProgress};
use super::{DiagnosticsHandle, ProductHandles, ProductReceivers};

#[derive(Clone)]
pub struct SelectedLibrary {
    pub source_key: SourceKey,
    pub source_session_epoch: SourceSessionEpoch,
    pub music_folder_key: Option<FolderKey>,
    pub music_folder_object_id: Option<String>,
    pub music_folders: Arc<[FolderRow]>,
    pub playlist_tracks_can_repeat: bool,
    pub artwork: artwork::SourceImages,
    pub database: Arc<Database>,
    pub runtime: tokio::runtime::Handle,
    pub operations: SelectedSourceHandle,
}

impl SelectedLibrary {
    pub fn play_request(
        &self,
        order: Arc<[TrackKey]>,
        anchor: PlaybackMedia,
        anchor_index: usize,
        placement: QueuePlacement,
        context_id: impl Into<String>,
        shuffled_start: bool,
    ) -> Option<LoadedPlayRequest> {
        LoadedPlayRequest::context(
            self.source_key,
            self.source_session_epoch,
            order,
            anchor,
            anchor_index,
            placement,
            context_id,
            shuffled_start,
        )
    }

    pub fn one_track(
        &self,
        track: PlaybackMedia,
        placement: QueuePlacement,
    ) -> Option<LoadedPlayRequest> {
        LoadedPlayRequest::one(self.source_key, self.source_session_epoch, track, placement)
    }
}

/// Ordered selected-source lifecycle publication.
///
/// Source gate state, the drop-before-build handoff, and source replacement
/// share one lane. A new source carries its Library and Playback together;
/// same-source refreshes publish the accepted catalog before its matching
/// Playback update without creating another state owner.
pub enum SourceEvent {
    Configured(ConfiguredSources),
    Selected {
        configured: ConfiguredSources,
        selected: SelectedLibrary,
        playback: Box<PlaybackProjection>,
    },
    CatalogReplaced {
        configured: ConfiguredSources,
        selected: SelectedLibrary,
    },
    Operation(SourceOperation),
    ArtworkPreparation {
        source_key: SourceKey,
        revision: u64,
        progress: Option<SourceProgress>,
    },
    CatalogPublished(CatalogPublication),
    Notice(SourceNotice),
    ReleaseSelected {
        acknowledged: async_channel::Sender<()>,
    },
}

pub struct PlaybackPublication {
    pub source_key: SourceKey,
    pub source_session_epoch: SourceSessionEpoch,
    pub projection: PlaybackProjection,
}

pub struct VisualizerPublication {
    pub source_key: SourceKey,
    pub source_session_epoch: SourceSessionEpoch,
    pub run: playback::RunId,
    pub levels: Vec<f64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CatalogPublication {
    pub source_key: SourceKey,
    pub source_session_epoch: SourceSessionEpoch,
    pub favorite: Option<FavoriteSettlement>,
    pub change: CatalogChange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogChange {
    Broad,
    Home,
    Playlists,
    Album(library::AlbumKey),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FavoriteSettlement {
    pub target: library::FavoriteTarget,
    pub requested: bool,
    pub effective: bool,
}

#[derive(Clone, Debug)]
pub struct SourceNotice {
    pub source_key: SourceKey,
    pub source_session_epoch: SourceSessionEpoch,
    pub kind: SourceNoticeKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceNoticeKind {
    ServerUnreachable,
    FavoriteRejected,
}

pub struct RuntimeInputs {
    pub diagnostics: DiagnosticsHandle,
    pub products: ProductHandles,
    pub settings: SettingsHandle,
    pub receivers: ProductReceivers,
    pub configured_sources: ConfiguredSources,
    pub source_operation: SourceOperation,
    pub release_history: super::ReleaseHistory,
}
