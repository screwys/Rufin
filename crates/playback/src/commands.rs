use std::sync::Arc;

use library::{RadioSeed, RandomCriteria, SourceKey, TrackKey};

use crate::{
    AudioOutput, Batch, BatchItem, CastNetwork, OccurrenceId, Placement, PlaybackMedia,
    PlaybackOutput, Provenance, QueueReorderTarget, RemoteOutput, RepeatMode, SourceSessionEpoch,
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

#[derive(Clone)]
pub struct LoadedPlayRequest {
    pub source_key: SourceKey,
    pub source_session_epoch: SourceSessionEpoch,
    pub order: Arc<[TrackKey]>,
    pub anchor: PlaybackMedia,
    pub anchor_index: usize,
    pub placement: QueuePlacement,
    pub origin: QueueOrigin,
    pub shuffled_start: bool,
}

impl LoadedPlayRequest {
    pub fn now(
        source_key: SourceKey,
        source_session_epoch: SourceSessionEpoch,
        order: Arc<[TrackKey]>,
        anchor: PlaybackMedia,
        anchor_index: usize,
    ) -> Option<Self> {
        let anchor_key = anchor.track_key?;
        (order.get(anchor_index) == Some(&anchor_key)).then(|| Self {
            source_key,
            source_session_epoch,
            order,
            anchor,
            anchor_index,
            placement: QueuePlacement::Now,
            origin: QueueOrigin::Manual,
            shuffled_start: false,
        })
    }

    pub fn one(
        source_key: SourceKey,
        source_session_epoch: SourceSessionEpoch,
        track: PlaybackMedia,
        placement: QueuePlacement,
    ) -> Option<Self> {
        let track_key = track.track_key?;
        Some(Self {
            source_key,
            source_session_epoch,
            order: Arc::from([track_key]),
            anchor: track,
            anchor_index: 0,
            placement,
            origin: QueueOrigin::Manual,
            shuffled_start: false,
        })
    }

    pub fn manual(
        source_key: SourceKey,
        source_session_epoch: SourceSessionEpoch,
        order: Arc<[TrackKey]>,
        anchor: PlaybackMedia,
        placement: QueuePlacement,
    ) -> Option<Self> {
        let anchor_key = anchor.track_key?;
        (order.first() == Some(&anchor_key)).then(|| Self {
            source_key,
            source_session_epoch,
            order,
            anchor,
            anchor_index: 0,
            placement,
            origin: QueueOrigin::Manual,
            shuffled_start: false,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn context(
        source_key: SourceKey,
        source_session_epoch: SourceSessionEpoch,
        order: Arc<[TrackKey]>,
        anchor: PlaybackMedia,
        anchor_index: usize,
        placement: QueuePlacement,
        context_id: impl Into<String>,
        shuffled_start: bool,
    ) -> Option<Self> {
        let anchor_key = anchor.track_key?;
        if order.get(anchor_index) != Some(&anchor_key) {
            return None;
        }
        Some(Self {
            source_key,
            source_session_epoch,
            order,
            anchor,
            anchor_index,
            placement,
            origin: QueueOrigin::Context(context_id.into()),
            shuffled_start,
        })
    }

    pub fn random(
        source_key: SourceKey,
        source_session_epoch: SourceSessionEpoch,
        order: Arc<[TrackKey]>,
        anchor: PlaybackMedia,
        placement: QueuePlacement,
    ) -> Option<Self> {
        let anchor_key = anchor.track_key?;
        (order.first() == Some(&anchor_key)).then(|| Self {
            source_key,
            source_session_epoch,
            order,
            anchor,
            anchor_index: 0,
            placement,
            origin: QueueOrigin::Random,
            shuffled_start: false,
        })
    }

    pub(crate) fn activation_context(&self) -> Option<(String, TrackKey, usize)> {
        let QueueOrigin::Context(context_id) = &self.origin else {
            return None;
        };
        let anchor_track_id = self.anchor.track_key?;
        (self.placement == QueuePlacement::Now && !self.shuffled_start)
            .then(|| (context_id.clone(), anchor_track_id, self.anchor_index))
    }

    pub fn placement(&self) -> Placement {
        self.placement.into()
    }

    pub fn compact_batch(self, shuffle_seed: u64) -> Option<(Batch, Placement, PlaybackMedia)> {
        let placement = match self.placement {
            QueuePlacement::Now => Placement::Replace {
                anchor_index: self.anchor_index,
            },
            QueuePlacement::Next => Placement::AfterCurrent,
            QueuePlacement::Last => Placement::End,
        };
        let anchor_track_id = self.anchor.track_key?;
        if self.order.get(self.anchor_index) != Some(&anchor_track_id) {
            return None;
        }
        let origin = self.origin;
        let context_id = match &origin {
            QueueOrigin::Context(context_id) => Some(Arc::<str>::from(context_id.as_str())),
            QueueOrigin::Manual | QueueOrigin::Random | QueueOrigin::Radio => None,
        };
        let items = self
            .order
            .into_iter()
            .copied()
            .enumerate()
            .map(|(source_rank, track_key)| {
                let provenance = compact_provenance(&origin, context_id.as_ref(), source_rank);
                BatchItem::new(track_key, provenance)
            })
            .collect();
        Some((
            Batch::new(items).with_shuffle_intent(shuffle_seed, self.shuffled_start),
            placement,
            self.anchor,
        ))
    }
}

fn compact_provenance(
    origin: &QueueOrigin,
    context_id: Option<&Arc<str>>,
    source_rank: usize,
) -> Provenance {
    match origin {
        QueueOrigin::Context(_) => Provenance::Context {
            context_id: Arc::clone(
                context_id.expect("Context Queue origin has one shared identity"),
            ),
            source_rank,
        },
        QueueOrigin::Manual => Provenance::Manual,
        QueueOrigin::Random => Provenance::Random,
        QueueOrigin::Radio => Provenance::Radio,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueueReorderRequest {
    pub occurrence: OccurrenceId,
    pub target: QueueReorderTarget,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RandomPlayRequest {
    pub placement: QueuePlacement,
    pub requested: usize,
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
    fn remove_many(&self, occurrences: Vec<OccurrenceId>);
    fn activate(&self, occurrence: OccurrenceId);
    fn move_after_current(&self, occurrence: OccurrenceId);
    fn reorder(&self, request: QueueReorderRequest);
    fn clear(&self, include_current: bool);
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

#[cfg(test)]
mod tests {
    use super::{QueueOrigin, compact_provenance};
    use crate::Provenance;
    use std::sync::Arc;

    #[test]
    fn collection_occurrences_share_one_context_identity() {
        let origin = QueueOrigin::Context("genre:4".to_string());
        let context = Arc::<str>::from("genre:4");
        let first = compact_provenance(&origin, Some(&context), 0);
        let second = compact_provenance(&origin, Some(&context), 1);
        let (
            Provenance::Context {
                context_id: first, ..
            },
            Provenance::Context {
                context_id: second, ..
            },
        ) = (first, second)
        else {
            panic!("Context Queue entries keep Context provenance");
        };

        assert!(Arc::ptr_eq(&first, &second));
    }
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
