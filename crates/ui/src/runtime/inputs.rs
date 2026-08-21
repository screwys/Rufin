use crate::SettingsHandle;
use library::{
    AcceptedLibraryChange, HomeSectionKind, HomeSnapshot, Library, MusicFolderId, SourceId, Track,
    TrackSelection,
};
use playback::{LoadedPlayRequest, PlaybackProjection, QueuePlacement, SourceSessionEpoch};
use std::sync::Arc;

use super::source::{ConfiguredSources, SelectedSourceHandle, SourceOperation, SourceProgress};
use super::{DiagnosticsHandle, ProductHandles, ProductReceivers};

#[derive(Clone)]
pub struct SelectedLibrary {
    pub source_id: SourceId,
    pub source_session_epoch: SourceSessionEpoch,
    pub music_folder_id: Option<MusicFolderId>,
    pub playlist_tracks_can_repeat: bool,
    pub artwork: artwork::SourceImages,
    pub library: Arc<Library>,
    pub home: Arc<HomeSnapshot>,
    pub operations: SelectedSourceHandle,
}

impl SelectedLibrary {
    pub fn play_request(
        &self,
        tracks: TrackSelection,
        anchor_index: usize,
        placement: QueuePlacement,
        context_id: impl Into<String>,
        shuffled_start: bool,
    ) -> Option<LoadedPlayRequest> {
        LoadedPlayRequest::context(
            self.source_id.clone(),
            self.source_session_epoch,
            tracks,
            anchor_index,
            placement,
            context_id,
            shuffled_start,
        )
    }

    pub fn one_track(&self, track: Track, placement: QueuePlacement) -> LoadedPlayRequest {
        LoadedPlayRequest::one(
            self.source_id.clone(),
            self.source_session_epoch,
            track,
            placement,
        )
    }
}

/// Ordered selected-source lifecycle publication.
///
/// Source gate state, the drop-before-build handoff, and source replacement
/// share one lane. A new source carries its Library and Playback together;
/// same-source refreshes publish the accepted Library before its matching
/// Playback update without creating another state owner.
pub enum SourceEvent {
    Configured(ConfiguredSources),
    Selected {
        configured: ConfiguredSources,
        selected: SelectedLibrary,
        playback: Box<PlaybackProjection>,
    },
    LibraryReplaced {
        configured: ConfiguredSources,
        selected: SelectedLibrary,
    },
    Operation(SourceOperation),
    ArtworkPreparation {
        source_id: SourceId,
        revision: u64,
        progress: Option<SourceProgress>,
    },
    Home(HomePublication),
    HomeReplaced {
        source_id: SourceId,
        source_session_epoch: SourceSessionEpoch,
        home: Arc<HomeSnapshot>,
    },
    LibraryUpdate(SelectedLibraryUpdate),
    Notice(SourceNotice),
    ReleaseSelected {
        acknowledged: async_channel::Sender<()>,
    },
}

pub struct PlaybackPublication {
    pub source_id: SourceId,
    pub source_session_epoch: SourceSessionEpoch,
    pub projection: PlaybackProjection,
}

#[derive(Clone)]
pub struct HomePublication {
    pub source_id: SourceId,
    pub source_session_epoch: SourceSessionEpoch,
    pub kind: HomeSectionKind,
    pub home: Arc<HomeSnapshot>,
}

#[derive(Clone, Debug)]
pub struct SelectedLibraryUpdate {
    pub source_id: SourceId,
    pub source_session_epoch: SourceSessionEpoch,
    pub change: AcceptedLibraryChange,
    pub home: Option<Arc<HomeSnapshot>>,
}

#[derive(Clone, Debug)]
pub struct SourceNotice {
    pub source_id: SourceId,
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
