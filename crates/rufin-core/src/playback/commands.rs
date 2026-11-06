use std::sync::Arc;

pub use library::{QueuePlacement, QueueReorderTarget};
use library::{RadioSeed, RandomCriteria};

use playback::{QueueProvenance, TrackReference};

#[derive(Clone, Debug, PartialEq)]
pub struct PlayRequest {
    pub input: library::QueueLoad,
    anchor: Option<(library::TrackKey, Option<library::PlaylistEntryKey>)>,
    pub placement: QueuePlacement,
    start: QueueStart,
    reactivate: bool,
}

impl PlayRequest {
    pub fn one(item: TrackReference, placement: QueuePlacement) -> Self {
        Self::items(vec![item], placement)
    }
    pub fn items(items: Vec<TrackReference>, placement: QueuePlacement) -> Self {
        Self::from_input(
            library::QueueLoad::items(
                items
                    .into_iter()
                    .map(|item| (item, QueueProvenance::Manual))
                    .collect(),
            ),
            placement,
        )
    }
    pub fn uris(uris: Vec<String>, placement: QueuePlacement) -> Self {
        Self::from_input(
            library::QueueLoad::media_uris(uris, QueueProvenance::Manual),
            placement,
        )
    }
    pub fn from_input(input: library::QueueLoad, placement: QueuePlacement) -> Self {
        Self {
            input,
            anchor: None,
            placement,
            start: QueueStart::Given,
            reactivate: false,
        }
    }
    pub fn ordered(
        selection: library::MemberSelection,
        anchor_index: usize,
        placement: QueuePlacement,
        shuffled_start: bool,
    ) -> Self {
        let mut request = Self::from_input(library::QueueLoad::tracks(selection), placement);
        request.placement = placement.with_anchor(anchor_index);
        request.start = if shuffled_start {
            QueueStart::RandomIfShuffled
        } else {
            QueueStart::Given
        };
        request
    }
    /// One track, queued by `placement` with no context.
    pub fn track(key: library::TrackKey, placement: QueuePlacement) -> Self {
        Self::tracks(
            library::MemberSelection::Keys {
                source: None,
                keys: vec![key].into(),
            },
            placement,
        )
    }
    /// What playing a selection of tracks queues: them in their order, from the first, with no
    /// context.
    pub fn tracks(selection: library::MemberSelection, placement: QueuePlacement) -> Self {
        Self::ordered(selection, 0, placement, false)
    }
    pub fn captured(
        selection: library::MemberSelection,
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
            if let library::QueueLoad::Tracks(selections) = request.input {
                groups.extend(selections);
            }
        }
        Self::from_input(library::QueueLoad::Tracks(groups), placement)
    }
    pub fn selection(&self) -> Option<library::MemberSelection> {
        let library::QueueLoad::Tracks(groups) = &self.input else {
            return None;
        };
        if groups.len() == 1 {
            Some(groups[0].0.clone())
        } else {
            Some(library::MemberSelection::Concat(
                groups
                    .iter()
                    .map(|(selection, _)| selection.clone())
                    .collect(),
            ))
        }
    }
    pub fn context(&self) -> Option<&str> {
        let library::QueueLoad::Tracks(groups) = &self.input else {
            return None;
        };
        let (_, QueueProvenance::Context { context_id, .. }) = groups.first()? else {
            return None;
        };
        Some(context_id)
    }
    pub fn with_context(mut self, context: Arc<str>) -> Self {
        match &mut self.input {
            library::QueueLoad::Tracks(groups) => {
                for (_, provenance) in groups {
                    *provenance = QueueProvenance::Context {
                        context_id: context.clone(),
                        context_title: None,
                        source_rank: 0,
                    };
                }
            }
            library::QueueLoad::Items(items) => {
                for (index, (_, provenance)) in items.iter_mut().enumerate() {
                    *provenance = QueueProvenance::Context {
                        context_id: context.clone(),
                        context_title: None,
                        source_rank: index,
                    };
                }
            }
            library::QueueLoad::Uris(_, provenance) => {
                *provenance = QueueProvenance::Context {
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
        self.anchor = Some((key, entry));
        self
    }
    pub fn with_anchor_index(mut self, index: usize) -> Self {
        self.placement = self.placement.with_anchor(index);
        self
    }
    pub fn activation_context(&self, shuffled: bool) -> Option<(String, library::TrackKey, usize)> {
        if !self.reactivate
            || !matches!(self.placement, QueuePlacement::Replace { .. })
            || self.start == QueueStart::Shuffled
            || (self.start == QueueStart::RandomIfShuffled && shuffled)
        {
            return None;
        }
        let library::QueueLoad::Tracks(groups) = &self.input else {
            return None;
        };
        let (
            selection,
            QueueProvenance::Context {
                context_id,
                source_rank,
                ..
            },
        ) = groups.first()?
        else {
            return None;
        };
        let anchor_index = match self.placement {
            QueuePlacement::Replace { anchor_index } => anchor_index,
            _ => 0,
        };
        let key = self
            .anchor
            .map(|(key, _)| key)
            .or_else(|| match selection {
                library::MemberSelection::Keys { keys, .. } => keys.get(anchor_index).copied(),
                _ => None,
            })?;
        Some((context_id.to_string(), key, source_rank + anchor_index))
    }
    pub fn shuffled(mut self, shuffled: bool) -> Self {
        if shuffled {
            self.start = QueueStart::Shuffled;
            self.placement = self.placement.with_anchor(0);
            self.anchor = None;
        } else if self.start == QueueStart::Shuffled {
            self.start = QueueStart::Given;
        }
        self
    }
    pub fn with_context_title(mut self, title: Option<Arc<library::QueueContextTitle>>) -> Self {
        let set = |provenance: &mut QueueProvenance| {
            if let QueueProvenance::Context { context_title, .. } = provenance {
                *context_title = title.clone();
            }
        };
        match &mut self.input {
            library::QueueLoad::Tracks(groups) => groups.iter_mut().for_each(|(_, p)| set(p)),
            library::QueueLoad::Items(items) => items.iter_mut().for_each(|(_, p)| set(p)),
            library::QueueLoad::Uris(_, p) => set(p),
            library::QueueLoad::Captured(entries) => Arc::make_mut(entries)
                .iter_mut()
                .for_each(|e| set(&mut e.provenance)),
        }
        self
    }
    pub(crate) async fn prepare(
        self,
        catalog: &library::Catalog,
        shuffled: bool,
        seed: u64,
    ) -> library::LibraryResult<QueueBatch> {
        let replacing = matches!(self.placement, QueuePlacement::Replace { .. });
        let random_start = replacing && shuffled && self.start == QueueStart::RandomIfShuffled;
        let selected = if random_start { None } else { self.anchor };
        let mut batch = catalog.queue_entries(self.input, selected).await?;
        let fallback = match self.placement {
            QueuePlacement::Replace { anchor_index } => anchor_index,
            _ => 0,
        };
        let selected = if self.start == QueueStart::Shuffled {
            library::shuffle_order(&mut batch.entries, seed);
            0
        } else if random_start && !batch.entries.is_empty() {
            seed as usize % batch.entries.len()
        } else {
            // A play anchored to an entry that is gone plays nothing rather than its neighbour.
            let index = match (selected, batch.anchor_index) {
                (_, Some(index)) => index,
                (Some(_), None) => {
                    return Err(library::LibraryError::InvalidRequest(localization::tr(
                        "The anchored track is gone",
                    )));
                }
                (None, None) => fallback,
            };
            index.min(batch.entries.len().saturating_sub(1))
        };
        let shuffle = (replacing && shuffled).then_some(if random_start {
            QueueShuffle::FromSelected(seed)
        } else {
            QueueShuffle::AfterSelected(seed)
        });
        let requested = batch
            .entries
            .get(selected)
            .cloned()
            .map(|entry| (selected, entry))
            .into_iter()
            .collect::<Vec<_>>();
        let rows = catalog
            .queue_rows_for_entries(&requested, &library::ReadCancellation::new())
            .await?;
        Ok(QueueBatch {
            entries: batch.entries,
            rows,
            current_index: selected,
            shuffle,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QueueStart {
    Given,
    RandomIfShuffled,
    Shuffled,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum QueueShuffle {
    AfterSelected(u64),
    FromSelected(u64),
}

#[derive(Clone, Debug)]
pub(crate) struct QueueBatch {
    pub entries: Vec<library::QueueEntry>,
    pub rows: Vec<Arc<library::QueueRow>>,
    pub current_index: usize,
    pub shuffle: Option<QueueShuffle>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum QueueSelection {
    Positions(Vec<usize>),
    Occurrences(Vec<u64>),
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

#[derive(Clone, Debug)]
pub struct QueuePage {
    pub rows: Vec<Arc<playback::QueueRow>>,
    pub total: u64,
    pub window_offset: u64,
}
