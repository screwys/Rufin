use std::sync::Arc;

pub use library::{QueuePlacement as Placement, QueueReorderTarget};
use library::{RadioSeed, RandomCriteria};

use playback::{Provenance, TrackReference};

pub type QueuePlacement = Placement;

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
                    .map(|item| (item, Provenance::Manual))
                    .collect(),
            ),
            placement,
        )
    }
    pub fn uris(uris: Vec<String>, placement: QueuePlacement) -> Self {
        Self::from_input(
            library::QueueLoad::media_uris(uris, Provenance::Manual),
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
        selection: library::TrackSelection,
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
    pub fn captured(
        selection: library::TrackSelection,
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
    pub fn selection(&self) -> Option<library::TrackSelection> {
        let library::QueueLoad::Tracks(groups) = &self.input else {
            return None;
        };
        if groups.len() == 1 {
            Some(groups[0].0.clone())
        } else {
            Some(library::TrackSelection::Concat(
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
        let (_, Provenance::Context { context_id, .. }) = groups.first()? else {
            return None;
        };
        Some(context_id)
    }
    pub fn with_context(mut self, context: Arc<str>) -> Self {
        match &mut self.input {
            library::QueueLoad::Tracks(groups) => {
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
        self.anchor = Some((key, entry));
        self
    }
    pub fn with_anchor_index(mut self, index: usize) -> Self {
        self.placement = self.placement.with_anchor(index);
        self
    }
    pub fn activation_context(&self, shuffled: bool) -> Option<(String, library::TrackKey, usize)> {
        if !self.reactivate
            || !matches!(self.placement, Placement::Replace { .. })
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
            Provenance::Context {
                context_id,
                source_rank,
                ..
            },
        ) = groups.first()?
        else {
            return None;
        };
        let anchor_index = match self.placement {
            Placement::Replace { anchor_index } => anchor_index,
            _ => 0,
        };
        let key = self
            .anchor
            .map(|(key, _)| key)
            .or_else(|| match selection {
                library::TrackSelection::Query {
                    query:
                        library::CatalogQuery {
                            subject:
                                library::CatalogSubject::Keys {
                                    kind: library::CatalogKind::Track,
                                    keys,
                                    ..
                                },
                            ..
                        },
                    ranges: None,
                } => keys
                    .get(anchor_index)
                    .copied()
                    .map(library::TrackKey::from_raw),
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
        let set = |provenance: &mut Provenance| {
            if let Provenance::Context { context_title, .. } = provenance {
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
        database: &library::Database,
        shuffled: bool,
        seed: u64,
    ) -> library::LibraryResult<QueueBatch> {
        let replacing = matches!(self.placement, Placement::Replace { .. });
        let random_start = replacing && shuffled && self.start == QueueStart::RandomIfShuffled;
        let selected = if random_start { None } else { self.anchor };
        let mut batch = database.queue_entries(self.input, selected).await?;
        let fallback = match self.placement {
            Placement::Replace { anchor_index } => anchor_index,
            _ => 0,
        };
        let selected = if self.start == QueueStart::Shuffled {
            library::shuffle_order(&mut batch.entries, seed);
            0
        } else if random_start && !batch.entries.is_empty() {
            seed as usize % batch.entries.len()
        } else {
            batch
                .anchor_index
                .unwrap_or(fallback)
                .min(batch.entries.len().saturating_sub(1))
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
        let rows = database
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
