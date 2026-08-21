use std::sync::Arc;

use library::{
    LibraryQueryError, LibraryQueryResult, RadioSeed, RandomCriteria, SourceId, Track, TrackId,
    TrackList, TrackSelection,
};

use crate::{
    AudioOutput, Batch, BatchItem, CastNetwork, OccurrenceId, Placement, PlaybackOutput,
    Provenance, QueuePage, QueuePageQuery, RemoteOutput, RepeatMode, SourceSessionEpoch,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueuePlacement {
    Now,
    Next,
    Last,
}

impl From<QueuePlacement> for Placement {
    fn from(value: QueuePlacement) -> Self {
        match value {
            QueuePlacement::Now => Self::Replace { anchor_index: 0 },
            QueuePlacement::Next => Self::AfterCurrent,
            QueuePlacement::Last => Self::End,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QueueOrigin {
    Context(String),
    Manual,
    Random,
    Radio,
}

/// One already-loaded ordered music selection.
///
/// Routes pass either an existing shallow Library order or a small
/// already-materialized selection. Rufin prepares the compact order away from
/// GTK, asks Playback for exact context activation, and materializes complete
/// Track values only when activation misses.
#[derive(Clone, Debug)]
pub enum LoadedTrackSelection {
    Shallow(TrackSelection),
    Materialized(Arc<[Track]>),
}

enum SelectionAnchor {
    Deferred,
    Missing,
    Present(TrackId),
}

impl LoadedTrackSelection {
    fn anchor(&self, position: usize) -> LibraryQueryResult<SelectionAnchor> {
        match self {
            Self::Shallow(selection) => match selection.prepared() {
                Some(tracks) => Ok(tracks
                    .track(position)?
                    .map_or(SelectionAnchor::Missing, |track| {
                        SelectionAnchor::Present(track.id.clone())
                    })),
                None => Ok(SelectionAnchor::Deferred),
            },
            Self::Materialized(tracks) => Ok(tracks
                .get(position)
                .map_or(SelectionAnchor::Missing, |track| {
                    SelectionAnchor::Present(track.id.clone())
                })),
        }
    }

    fn prepare(self) -> LibraryQueryResult<Self> {
        match self {
            Self::Shallow(selection) => Ok(Self::Shallow(selection.prepare()?.into())),
            Self::Materialized(tracks) => Ok(Self::Materialized(tracks)),
        }
    }

    fn materialize_owned(self) -> LibraryQueryResult<Vec<Track>> {
        match self {
            Self::Shallow(selection) => selection.prepare()?.materialize_owned(),
            Self::Materialized(tracks) => Ok(tracks.iter().cloned().collect()),
        }
    }

    pub fn materialize(&self) -> LibraryQueryResult<Arc<[Track]>> {
        match self {
            Self::Shallow(selection) => selection.clone().prepare()?.materialize(),
            Self::Materialized(tracks) => Ok(Arc::clone(tracks)),
        }
    }
}

impl From<TrackList> for LoadedTrackSelection {
    fn from(value: TrackList) -> Self {
        Self::Shallow(value.into())
    }
}

impl From<TrackSelection> for LoadedTrackSelection {
    fn from(value: TrackSelection) -> Self {
        Self::Shallow(value)
    }
}

impl From<Arc<[Track]>> for LoadedTrackSelection {
    fn from(value: Arc<[Track]>) -> Self {
        Self::Materialized(value)
    }
}

#[derive(Clone)]
pub struct LoadedPlayRequest {
    pub source_id: SourceId,
    pub source_session_epoch: SourceSessionEpoch,
    pub tracks: LoadedTrackSelection,
    anchor_track_id: Option<TrackId>,
    pub anchor_index: usize,
    pub placement: QueuePlacement,
    pub origin: QueueOrigin,
    pub shuffled_start: bool,
}

impl LoadedPlayRequest {
    pub fn now(
        source_id: SourceId,
        source_session_epoch: SourceSessionEpoch,
        tracks: Arc<[Track]>,
        anchor_index: usize,
    ) -> Self {
        let anchor_track_id = tracks
            .get(anchor_index)
            .expect("a loaded Play request must identify an available Track")
            .id
            .clone();
        Self {
            source_id,
            source_session_epoch,
            tracks: tracks.into(),
            anchor_track_id: Some(anchor_track_id),
            anchor_index,
            placement: QueuePlacement::Now,
            origin: QueueOrigin::Manual,
            shuffled_start: false,
        }
    }

    pub fn one(
        source_id: SourceId,
        source_session_epoch: SourceSessionEpoch,
        track: Track,
        placement: QueuePlacement,
    ) -> Self {
        let anchor_track_id = track.id.clone();
        Self {
            source_id,
            source_session_epoch,
            tracks: Arc::<[Track]>::from([track]).into(),
            anchor_track_id: Some(anchor_track_id),
            anchor_index: 0,
            placement,
            origin: QueueOrigin::Manual,
            shuffled_start: false,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn context(
        source_id: SourceId,
        source_session_epoch: SourceSessionEpoch,
        tracks: impl Into<LoadedTrackSelection>,
        anchor_index: usize,
        placement: QueuePlacement,
        context_id: impl Into<String>,
        shuffled_start: bool,
    ) -> Option<Self> {
        let tracks = tracks.into();
        let anchor_track_id = match tracks.anchor(anchor_index).ok()? {
            SelectionAnchor::Deferred => None,
            SelectionAnchor::Missing => return None,
            SelectionAnchor::Present(track_id) => Some(track_id),
        };
        Some(Self {
            source_id,
            source_session_epoch,
            tracks,
            anchor_track_id,
            anchor_index,
            placement,
            origin: QueueOrigin::Context(context_id.into()),
            shuffled_start,
        })
    }

    pub(crate) fn activation_context(&self) -> Option<(String, TrackId, usize)> {
        let QueueOrigin::Context(context_id) = &self.origin else {
            return None;
        };
        let anchor_track_id = self.anchor_track_id.as_ref()?;
        (self.placement == QueuePlacement::Now && !self.shuffled_start).then(|| {
            (
                context_id.clone(),
                anchor_track_id.clone(),
                self.anchor_index,
            )
        })
    }

    /// Prepares only the compact Library slot order.
    ///
    /// Rufin runs this on its loaded-Play executor before asking Playback for
    /// exact context activation. Complete Track handles remain unmaterialized.
    pub fn prepare(mut self) -> LibraryQueryResult<Option<Self>> {
        self.tracks = self.tracks.prepare()?;
        let anchor_track_id = match self.tracks.anchor(self.anchor_index)? {
            SelectionAnchor::Present(track_id) => track_id,
            SelectionAnchor::Missing => return Ok(None),
            SelectionAnchor::Deferred => {
                unreachable!("a prepared loaded Track selection must have a concrete order")
            }
        };
        if self
            .anchor_track_id
            .as_ref()
            .is_some_and(|expected| expected != &anchor_track_id)
        {
            return Err(LibraryQueryError::StaleTrackSelection);
        }
        self.anchor_track_id = Some(anchor_track_id);
        Ok(Some(self))
    }

    pub fn placement(&self) -> Placement {
        self.placement.into()
    }

    pub fn materialize_batch(self, shuffle_seed: u64) -> LibraryQueryResult<(Batch, Placement)> {
        let placement = match self.placement {
            QueuePlacement::Now => Placement::Replace {
                anchor_index: self.anchor_index,
            },
            QueuePlacement::Next => Placement::AfterCurrent,
            QueuePlacement::Last => Placement::End,
        };
        let tracks = self.tracks.materialize_owned()?;
        let anchor_track_id = self
            .anchor_track_id
            .ok_or(LibraryQueryError::StaleTrackSelection)?;
        if tracks
            .get(self.anchor_index)
            .is_none_or(|track| track.id != anchor_track_id)
        {
            return Err(LibraryQueryError::StaleTrackSelection);
        }
        let origin = self.origin;
        let items = tracks
            .into_iter()
            .enumerate()
            .map(|(source_rank, track)| {
                let provenance = match &origin {
                    QueueOrigin::Context(context_id) => Provenance::Context {
                        context_id: context_id.clone(),
                        source_rank,
                    },
                    QueueOrigin::Manual => Provenance::Manual,
                    QueueOrigin::Random => Provenance::Random,
                    QueueOrigin::Radio => Provenance::Radio,
                };
                BatchItem::new(track, provenance)
            })
            .collect();
        Ok((
            Batch::new(items).with_shuffle_intent(shuffle_seed, self.shuffled_start),
            placement,
        ))
    }
}

pub struct QueueReorderRequest {
    pub occurrence: OccurrenceId,
    pub target_index: usize,
    pub after: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RandomPlayRequest {
    pub placement: QueuePlacement,
    pub criteria: RandomCriteria,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RadioPlayRequest {
    pub placement: QueuePlacement,
    pub seed: RadioSeed,
}

impl RadioPlayRequest {
    pub fn now(seed: RadioSeed) -> Self {
        Self {
            placement: QueuePlacement::Now,
            seed,
        }
    }

    pub fn next(seed: RadioSeed) -> Self {
        Self {
            placement: QueuePlacement::Next,
            seed,
        }
    }

    pub fn last(seed: RadioSeed) -> Self {
        Self {
            placement: QueuePlacement::Last,
            seed,
        }
    }
}

pub trait QueueCommandPort: Send + Sync {
    fn play_loaded(&self, request: LoadedPlayRequest);
    fn remove(&self, occurrence: OccurrenceId);
    fn activate(&self, occurrence: OccurrenceId);
    fn move_after_current(&self, occurrence: OccurrenceId);
    fn reorder(&self, request: QueueReorderRequest);
    fn clear(&self);
    fn request_page(&self, query: QueuePageQuery) -> Option<QueuePage>;
}

pub trait RadioCommandPort: Send + Sync {
    fn play_random(&self, request: RandomPlayRequest);
    fn play_radio(&self, request: RadioPlayRequest);
}

pub trait TransportCommandPort: Send + Sync {
    fn play_pause(&self);
    fn play(&self);
    fn pause(&self);
    fn stop(&self);
    fn next(&self);
    fn previous(&self);
    fn seek_seconds(&self, seconds: u32);
    fn seek_millis(&self, millis: u64);
    fn set_volume(&self, volume: f64);
    fn persist_volume(&self, volume: f64);
    fn set_muted(&self, muted: bool);
    fn toggle_shuffle(&self);
    fn set_shuffle(&self, enabled: bool);
    fn cycle_repeat(&self);
    fn set_repeat(&self, repeat: RepeatMode);
    fn toggle_auto_dj(&self);
    fn set_visualizer_enabled(&self, enabled: bool);
    fn available_audio_outputs(&self) -> Vec<AudioOutput>;
    fn available_cast_networks(&self) -> Vec<CastNetwork>;
    fn playback_output(&self) -> PlaybackOutput;
    fn discover_remote_outputs(&self) -> Result<Vec<RemoteOutput>, String>;
    fn select_playback_output(&self, output: PlaybackOutput) -> Result<(), String>;
    fn shutdown(&self);
}

pub type TransportHandle = Arc<dyn TransportCommandPort>;
pub type QueueHandle = Arc<dyn QueueCommandPort>;
pub type RadioHandle = Arc<dyn RadioCommandPort>;

#[derive(Clone)]
pub struct PlaybackHandles {
    pub transport: TransportHandle,
    pub queue: QueueHandle,
    pub radio: RadioHandle,
}
