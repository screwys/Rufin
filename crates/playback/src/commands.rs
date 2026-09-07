use std::sync::Arc;

use library::{RadioSeed, RandomCriteria};

use crate::{
    AudioOutput, Batch, BatchItem, CastNetwork, OccurrenceId, Placement, PlaybackOutput,
    Provenance, QueueItem, QueueReorderTarget, RemoteOutput, RepeatMode,
};

pub type QueuePlacement = Placement;

#[derive(Clone)]
pub struct PlayRequest {
    pub batch: Batch,
    pub anchor_index: usize,
    pub placement: QueuePlacement,
    /// Whole-collection start intent, resolved against the current Shuffle setting before admission.
    pub shuffled_start: bool,
    reactivate: bool,
}

impl PlayRequest {
    pub fn one(item: QueueItem, placement: QueuePlacement) -> Self {
        Self {
            batch: Batch::new(vec![BatchItem::direct(item, Provenance::Manual)]),
            anchor_index: 0,
            placement,
            shuffled_start: false,
            reactivate: false,
        }
    }

    pub fn ordered(
        input: library::QueueInput,
        anchor_index: usize,
        placement: QueuePlacement,
        shuffled_start: bool,
    ) -> Self {
        Self {
            batch: Batch::from_input(input),
            anchor_index,
            placement,
            shuffled_start,
            reactivate: false,
        }
    }

    pub fn captured(
        input: library::QueueInput,
        anchor_index: usize,
        placement: QueuePlacement,
        shuffled_start: bool,
    ) -> Self {
        let mut request = Self::ordered(input, anchor_index, placement, shuffled_start);
        request.reactivate = true;
        request
    }

    pub fn random(items: Arc<[QueueItem]>, placement: QueuePlacement) -> Option<Self> {
        (!items.is_empty()).then(|| Self {
            batch: Batch::new(
                items
                    .iter()
                    .cloned()
                    .map(|item| BatchItem::direct(item, Provenance::Random))
                    .collect(),
            ),
            anchor_index: 0,
            placement,
            shuffled_start: false,
            reactivate: false,
        })
    }

    pub fn activation_context(&self) -> Option<(String, String, usize)> {
        (self.reactivate && self.placement == QueuePlacement::Now && !self.shuffled_start)
            .then(|| self.batch.activation_context(self.anchor_index))
            .flatten()
    }

    pub fn compact_batch(self, shuffle_seed: u64) -> (Batch, Placement) {
        let placement = self.placement.with_anchor(self.anchor_index);
        (
            self.batch
                .with_shuffle_intent(shuffle_seed, self.shuffled_start),
            placement,
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueueReorderRequest {
    pub occurrences: Vec<OccurrenceId>,
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
    fn play(&self, request: PlayRequest);
    fn insert(&self, input: library::QueueInput, target: QueueReorderTarget);
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
    use super::{PlayRequest, QueuePlacement};
    use crate::{Provenance, QueueItem};
    use std::sync::Arc;

    #[test]
    fn full_context_keeps_the_absolute_route_rank_for_reactivation() {
        let items = (0..250)
            .map(|index| {
                QueueItem::direct(
                    format!("https://example.test/{index}"),
                    index.to_string(),
                    "",
                    "",
                    0,
                )
            })
            .collect::<Vec<_>>();
        let order: Arc<[String]> = items.iter().map(|item| item.media_uri.clone()).collect();
        for anchor in [3, 125, 248] {
            let request = PlayRequest::captured(
                library::QueueInput::Uris {
                    order: Arc::clone(&order),
                    context_id: "mounted-order".into(),
                    source_start: 20,
                },
                anchor,
                QueuePlacement::Now,
                false,
            );
            assert_eq!(request.anchor_index, anchor);
            let library::QueueInput::Uris {
                order: captured, ..
            } = &request.batch.input
            else {
                panic!("captured route order")
            };
            assert_eq!(captured.len(), 250);
            assert_eq!(captured[anchor], format!("https://example.test/{anchor}"));
            assert_eq!(
                request.activation_context(),
                Some((
                    "mounted-order".into(),
                    format!("https://example.test/{anchor}"),
                    20 + anchor
                ))
            );
        }
        let request = PlayRequest::ordered(
            library::QueueInput::MediaUris {
                order,
                provenance: Provenance::Manual,
            },
            0,
            QueuePlacement::Next,
            false,
        );
        let library::QueueInput::MediaUris { order, .. } = &request.batch.input else {
            panic!("selected media order")
        };
        assert_eq!(order.len(), 250);
        let request = PlayRequest::random(items.into(), QueuePlacement::Last).unwrap();
        let library::QueueInput::Items(captured) = &request.batch.input else {
            panic!("explicit random items")
        };
        assert_eq!(captured.len(), 250);
        assert_eq!(captured[125].0.title, "125");
    }

    #[tokio::test]
    async fn collection_occurrences_share_one_context_identity() {
        let directory = tempfile::tempdir().unwrap();
        let database = library::Database::open(directory.path().join("queue.sqlite3"))
            .await
            .unwrap();
        let request = PlayRequest::captured(
            library::QueueInput::Uris {
                order: Arc::from([
                    "rufin://source/track/first".to_string(),
                    "rufin://source/track/second".to_string(),
                ]),
                context_id: "genre:4".into(),
                source_start: 75,
            },
            0,
            QueuePlacement::Now,
            false,
        );
        let page = database
            .read_queue(library::QueueReadRequest {
                input: request.batch.input.clone(),
                cursor: Default::default(),
                limit: 100,
                history: false,
                backwards: false,
            })
            .await
            .unwrap();
        let first = &page.items[0].1;
        let second = &page.items[1].1;
        assert!(matches!(
            second,
            Provenance::Context {
                source_rank: 76,
                ..
            }
        ));
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
