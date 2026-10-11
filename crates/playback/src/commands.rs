use std::sync::Arc;

pub use library::{QueuePlacement as Placement, QueueReorderTarget};
use library::{RadioSeed, RandomCriteria};

use crate::{
    AudioOutput, CastNetwork, PlaybackOutput, Provenance, QueueItem, RemoteOutput, RepeatMode,
};

pub type QueuePlacement = Placement;

#[derive(Clone, Debug, PartialEq)]
pub struct PlayRequest {
    pub load: library::QueueReadRequest,
    pub placement: QueuePlacement,
    pub shuffled_start: bool,
    reactivate: bool,
}

impl PlayRequest {
    pub fn one(item: QueueItem, placement: QueuePlacement) -> Self {
        Self::items(vec![item], placement)
    }
    pub fn items(items: Vec<QueueItem>, placement: QueuePlacement) -> Self {
        Self::from_load(
            library::QueueReadRequest::items(
                items
                    .into_iter()
                    .map(|item| (item, Provenance::Manual))
                    .collect(),
            ),
            placement,
        )
    }
    pub fn uris(uris: Vec<String>, placement: QueuePlacement) -> Self {
        Self::from_load(
            library::QueueReadRequest::media_uris(uris, Provenance::Manual),
            placement,
        )
    }
    pub fn from_load(load: library::QueueReadRequest, placement: QueuePlacement) -> Self {
        Self {
            load,
            placement,
            shuffled_start: false,
            reactivate: false,
        }
    }
    pub fn ordered(
        selection: library::SongSelection,
        anchor_index: usize,
        placement: QueuePlacement,
        shuffled_start: bool,
    ) -> Self {
        let mut request = Self::from_load(library::QueueReadRequest::songs(selection), placement);
        request.load.anchor_index = anchor_index;
        request.shuffled_start = shuffled_start;
        request
    }
    pub fn captured(
        selection: library::SongSelection,
        anchor_index: usize,
        placement: QueuePlacement,
        shuffled_start: bool,
    ) -> Self {
        let mut request = Self::ordered(selection, anchor_index, placement, shuffled_start);
        request.reactivate = true;
        request
    }
    pub fn group(requests: Vec<Self>, placement: QueuePlacement) -> Self {
        let mut groups = Vec::new();
        for request in requests {
            if let library::QueueLoad::Songs(mut selections) = request.load.input {
                for (_, provenance) in &mut selections {
                    if let Provenance::Context { context_title, .. } = provenance {
                        *context_title = request.load.context_title.clone();
                    }
                }
                groups.extend(selections);
            }
        }
        Self::from_load(
            library::QueueReadRequest::new(library::QueueLoad::Songs(groups)),
            placement,
        )
    }
    pub fn selection(&self) -> Option<library::SongSelection> {
        let library::QueueLoad::Songs(groups) = &self.load.input else {
            return None;
        };
        if groups.len() == 1 {
            Some(groups[0].0.clone())
        } else {
            Some(library::SongSelection::Concat(
                groups
                    .iter()
                    .map(|(selection, _)| selection.clone())
                    .collect(),
            ))
        }
    }
    pub fn context(&self) -> Option<&str> {
        let library::QueueLoad::Songs(groups) = &self.load.input else {
            return None;
        };
        let (_, Provenance::Context { context_id, .. }) = groups.first()? else {
            return None;
        };
        Some(context_id)
    }
    pub fn with_context(mut self, context: Arc<str>) -> Self {
        match &mut self.load.input {
            library::QueueLoad::Songs(groups) => {
                for (_, provenance) in groups {
                    *provenance = Provenance::Context {
                        context_id: context.clone(),
                        context_title: None,
                        source_rank: 0,
                    };
                }
            }
            library::QueueLoad::Items(items) => {
                for (index, (_, provenance)) in items.iter_mut().enumerate() {
                    *provenance = Provenance::Context {
                        context_id: context.clone(),
                        context_title: None,
                        source_rank: index,
                    };
                }
            }
            library::QueueLoad::Uris(_, provenance) => {
                *provenance = Provenance::Context {
                    context_id: context,
                    context_title: None,
                    source_rank: 0,
                }
            }
            library::QueueLoad::Captured(_) => {}
        }
        self
    }
    pub fn with_anchor(
        mut self,
        key: library::TrackKey,
        entry: Option<library::PlaylistEntryKey>,
    ) -> Self {
        self.load.anchor = Some((key, entry));
        self
    }
    pub fn with_anchor_index(mut self, index: usize) -> Self {
        self.load.anchor_index = index;
        self
    }
    pub fn activation_context(&self) -> Option<(String, library::TrackKey, usize)> {
        if !self.reactivate
            || self.placement != QueuePlacement::Now
            || self.shuffled_start
            || self.load.shuffled.is_some()
        {
            return None;
        }
        let library::QueueLoad::Songs(groups) = &self.load.input else {
            return None;
        };
        let (
            selection,
            Provenance::Context {
                context_id,
                source_rank,
                ..
            },
        ) = groups.first()?
        else {
            return None;
        };
        let key = self
            .load
            .anchor
            .map(|(key, _)| key)
            .or_else(|| match selection {
                library::SongSelection::Tracks(keys) => keys.get(self.load.anchor_index).copied(),
                _ => None,
            })?;
        Some((
            context_id.to_string(),
            key,
            source_rank + self.load.anchor_index,
        ))
    }
    pub fn shuffled(mut self, shuffled: bool) -> Self {
        self.load.shuffled = shuffled.then_some(0);
        if shuffled {
            self.load.anchor_index = 0;
            self.load.anchor = None;
            self.shuffled_start = false;
        }
        self
    }
    pub fn with_context_title(mut self, title: Option<Arc<library::QueueContextTitle>>) -> Self {
        self.load.context_title = title;
        self
    }
    pub fn into_load(mut self, shuffle_seed: u64) -> (library::QueueReadRequest, Placement) {
        let placement = self.placement.with_anchor(self.load.anchor_index);
        self.load.random_start = self.shuffled_start.then_some(shuffle_seed);
        if self.shuffled_start {
            self.load.anchor = None;
        }
        if self.load.shuffled.is_some() {
            self.load.shuffled = Some(shuffle_seed);
        }
        (self.load, placement)
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
    fn insert(&self, selection: library::SongSelection, target: QueueReorderTarget);
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
    use crate::Provenance;
    use std::sync::Arc;

    #[test]
    fn full_context_keeps_the_absolute_route_rank_for_reactivation() {
        let keys: Arc<[library::TrackKey]> = (0..250)
            .map(|index| library::TrackKey::from_raw(index + 1))
            .collect();
        for anchor in [3, 125, 248] {
            let request = PlayRequest::captured(
                library::SongSelection::Tracks(keys.clone()),
                anchor,
                QueuePlacement::Now,
                false,
            )
            .with_context("mounted-order".into());
            assert_eq!(request.load.anchor_index, anchor);
            let Some(library::SongSelection::Tracks(captured)) = request.selection() else {
                panic!("captured route order");
            };
            assert!(Arc::ptr_eq(&captured, &keys));
            assert_eq!(captured.len(), 250);
            assert_eq!(
                request.activation_context(),
                Some(("mounted-order".into(), keys[anchor], anchor))
            );
        }
        let request = PlayRequest::from_load(
            library::QueueReadRequest::media_uris(
                (0..250).map(|index| format!("https://example.test/{index}")),
                Provenance::Random,
            ),
            QueuePlacement::Last,
        );
        let library::QueueLoad::Uris(uris, provenance) = &request.load.input else {
            panic!("external media order");
        };
        assert_eq!(uris.len(), 250);
        assert_eq!(uris[125], "https://example.test/125");
        assert_eq!(*provenance, Provenance::Random);
    }

    #[tokio::test]
    async fn collection_positions_share_one_context_identity() {
        let directory = tempfile::tempdir().unwrap();
        let database = library::Database::open(directory.path().join("queue.sqlite3"))
            .await
            .unwrap();
        let admitted = database
            .read_queue(library::QueueReadRequest::media_uris(
                vec![
                    "https://example.test/first".into(),
                    "https://example.test/second".into(),
                ],
                Provenance::Manual,
            ))
            .await
            .unwrap();
        let request = PlayRequest::captured(
            library::SongSelection::Tracks(
                admitted
                    .entries
                    .iter()
                    .map(|entry| entry.track_key)
                    .collect(),
            ),
            0,
            QueuePlacement::Now,
            false,
        )
        .with_context("genre:4".into());
        let page = database.read_queue(request.load).await.unwrap();
        let first = &page.entries[0].provenance;
        let second = &page.entries[1].provenance;
        assert!(matches!(second, Provenance::Context { source_rank: 1, .. }));
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
