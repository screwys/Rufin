use std::sync::Arc;

pub use library::{QueuePlacement as Placement, QueueReorderTarget};
use library::{RadioSeed, RandomCriteria};

use crate::{
    AudioOutput, CastNetwork, PlaybackOutput, Provenance, QueueItem, RemoteOutput, RepeatMode,
};

pub type QueuePlacement = Placement;

#[derive(Clone, Debug, PartialEq)]
pub struct BatchItem {
    pub item: QueueItem,
    pub provenance: Provenance,
}
impl BatchItem {
    pub fn direct(item: QueueItem, provenance: Provenance) -> Self {
        Self { item, provenance }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct Batch {
    pub input: library::QueueInput,
    pub context_title: Option<Arc<library::QueueContextTitle>>,
    pub shuffle_seed: u64,
    pub random_start: bool,
    pub shuffled: bool,
}
impl Batch {
    pub fn new(items: Vec<BatchItem>) -> Self {
        Self::from_input(library::QueueInput::Items(
            items
                .into_iter()
                .map(|item| (item.item, item.provenance))
                .collect(),
        ))
    }
    pub fn from_input(input: library::QueueInput) -> Self {
        Self {
            input,
            context_title: None,
            shuffle_seed: 0,
            random_start: false,
            shuffled: false,
        }
    }
    pub fn with_shuffle_intent(mut self, seed: u64, random_start: bool) -> Self {
        self.shuffle_seed = seed;
        self.random_start = random_start;
        self
    }
    pub fn with_context_title(mut self, title: Option<Arc<library::QueueContextTitle>>) -> Self {
        self.context_title = title;
        self
    }
    pub fn input(&self) -> &library::QueueInput {
        &self.input
    }
    pub fn context_title(&self) -> Option<&Arc<library::QueueContextTitle>> {
        self.context_title.as_ref()
    }
    pub fn shuffled_seed(&self) -> Option<u64> {
        self.shuffled.then_some(self.shuffle_seed)
    }
    fn activation_context(&self, index: usize) -> Option<(String, String, usize)> {
        match &self.input {
            library::QueueInput::Query {
                context_id,
                anchor_uri: Some(uri),
                ..
            } => Some((context_id.to_string(), uri.clone(), index)),
            library::QueueInput::PlaylistQuery {
                context_id,
                anchor_uri: Some(uri),
                ..
            } => Some((context_id.to_string(), uri.clone(), index)),
            library::QueueInput::Items(items) => {
                let (
                    item,
                    Provenance::Context {
                        context_id,
                        source_rank,
                        ..
                    },
                ) = items.get(index)?
                else {
                    return None;
                };
                Some((context_id.to_string(), item.media_uri.clone(), *source_rank))
            }
            library::QueueInput::Uris {
                order,
                context_id,
                source_start,
            } => Some((
                context_id.to_string(),
                order.get(index)?.clone(),
                source_start + index,
            )),
            library::QueueInput::PlaylistEntries { context_id, order } if index < order.len() => {
                Some((context_id.to_string(), String::new(), index))
            }
            _ => None,
        }
    }
}

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

    pub fn activation_context(&self) -> Option<(String, String, usize)> {
        (self.reactivate
            && self.placement == QueuePlacement::Now
            && !self.shuffled_start
            && !self.batch.shuffled)
            .then(|| self.batch.activation_context(self.anchor_index))
            .flatten()
    }

    pub fn shuffled(mut self, shuffled: bool) -> Self {
        self.batch.shuffled = shuffled;
        if shuffled {
            self.anchor_index = 0;
            self.batch.input.clear_anchor();
            self.shuffled_start = false;
        }
        self
    }

    pub fn with_context_title(mut self, title: Option<Arc<library::QueueContextTitle>>) -> Self {
        self.batch = self.batch.with_context_title(title);
        self
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
pub enum QueueSelection {
    Positions(Vec<usize>),
    Occurrences(Vec<u64>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueueReorderRequest {
    pub selection: QueueSelection,
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
    fn page(&self, offset: u64, limit: u32, filter: String) -> Result<QueuePage, String>;
    fn snapshot(&self) -> Result<library::QueueRestore, String>;
    fn media_uris(&self) -> Result<Vec<String>, String>;
    fn play(&self, request: PlayRequest);
    fn insert(&self, input: library::QueueInput, target: QueueReorderTarget);
    fn remove(&self, position: usize);
    fn remove_many(&self, selection: QueueSelection);
    fn activate(&self, position: usize);
    fn activate_occurrence(&self, occurrence_id: u64);
    fn move_after_current(&self, position: usize);
    fn move_occurrence_after_current(&self, occurrence_id: u64);
    fn reorder(&self, request: QueueReorderRequest);
    fn clear(&self, include_current: bool);
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct QueuePage {
    pub rows: Vec<Arc<crate::QueueRow>>,
    pub total: u64,
    pub window_offset: u64,
}

pub trait RadioCommandPort: Send + Sync {
    fn candidates(
        &self,
        seed: RadioSeed,
        requested: usize,
        cancellation: library::ReadCancellation,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<String>, String>> + Send>>;
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
    fn set_shuffle(&self, enabled: bool);
    fn cycle_repeat(&self);
    fn set_repeat(&self, repeat: RepeatMode);
    fn toggle_auto_dj(&self);
    fn set_visualizer_enabled(&self, enabled: bool);
    fn available_audio_outputs(&self) -> Vec<AudioOutput>;
    fn available_cast_networks(&self) -> Vec<CastNetwork>;
    fn playback_output(&self) -> PlaybackOutput;
    fn discover_remote_outputs(&self) -> Result<Vec<RemoteOutput>, String>;
    fn select_playback_output(
        &self,
        output: PlaybackOutput,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(), String>;
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
        let request = PlayRequest::ordered(
            library::QueueInput::MediaUris {
                order: items
                    .iter()
                    .map(|item| item.media_uri.clone())
                    .collect::<Vec<_>>()
                    .into(),
                provenance: Provenance::Random,
            },
            0,
            QueuePlacement::Last,
            false,
        );
        let library::QueueInput::MediaUris {
            order: captured,
            provenance,
        } = &request.batch.input
        else {
            panic!("random media order")
        };
        assert_eq!(captured.len(), 250);
        assert_eq!(captured[125], "https://example.test/125");
        assert_eq!(*provenance, Provenance::Random);
    }

    #[tokio::test]
    async fn collection_positions_share_one_context_identity() {
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
                context_title: None,
                input: request.batch.input.clone(),
                anchor_index: 0,
                random_start: None,
                shuffled: None,
            })
            .await
            .unwrap();
        let first = &page.entries[0].provenance;
        let second = &page.entries[1].provenance;
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
    pub updates: crate::PlaybackUpdates,
    pub transport: TransportHandle,
    pub queue: QueueHandle,
    pub radio: RadioHandle,
}
