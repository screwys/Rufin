//! Playback's bounded projection of the persistent application Queue.
use crate::{OccurrenceId, Provenance, QueueItem, QueueOccurrence, RepeatMode};
pub use library::{QueuePlacement as Placement, QueueReorderTarget};
use std::sync::Arc;
use thiserror::Error;

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
    pub(crate) input: library::QueueInput,
    pub(crate) shuffle_seed: u64,
    pub(crate) random_start: bool,
    pub(crate) identity: Option<String>,
    pub(crate) window: Option<Box<library::QueueRestore>>,
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
            shuffle_seed: 0,
            random_start: false,
            identity: None,
            window: None,
        }
    }
    pub fn with_shuffle_intent(mut self, seed: u64, random_start: bool) -> Self {
        self.shuffle_seed = seed;
        self.random_start = random_start;
        self
    }
    pub fn input(&self) -> &library::QueueInput {
        &self.input
    }
    pub fn with_prepared_window(
        mut self,
        identity: String,
        window: Option<library::QueueRestore>,
    ) -> Self {
        self.identity = Some(identity);
        self.window = window.map(Box::new);
        self
    }
    pub(crate) fn activation_context(&self, index: usize) -> Option<(String, String, usize)> {
        match &self.input {
            library::QueueInput::Items(items) => {
                let (
                    item,
                    Provenance::Context {
                        context_id,
                        source_rank,
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
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum SequenceError {
    #[error("a playback batch cannot be empty")]
    EmptyBatch,
    #[error("the playback batch anchor is outside the batch")]
    InvalidAnchor,
    #[error("the playback traversal is not an exact occurrence permutation")]
    InvalidTraversal,
    #[error("the selected playback occurrence is missing")]
    MissingSelectedOccurrence,
}
#[derive(Clone, Debug)]
pub struct Sequence {
    entries: Vec<Arc<QueueOccurrence>>,
    window_start: usize,
    wrap_previous: Option<Arc<QueueOccurrence>>,
    wrap_next: Option<Arc<QueueOccurrence>>,
    total: usize,
    selected_index: Option<usize>,
    repeat_mode: RepeatMode,
    shuffle_enabled: bool,
    revision: u64,
    progress_millis: u64,
    durable: bool,
}
impl Sequence {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            wrap_previous: None,
            wrap_next: None,
            window_start: 0,
            total: 0,
            selected_index: None,
            repeat_mode: RepeatMode::Off,
            shuffle_enabled: false,
            revision: 0,
            progress_millis: 0,
            durable: true,
        }
    }
    pub fn from_window(
        window: library::QueueRestore,
        revision: u64,
    ) -> Result<Self, SequenceError> {
        if window.occurrences.len() > library::QUEUE_CONTEXT_LIMIT {
            return Err(SequenceError::InvalidTraversal);
        }
        let sequence = Self {
            wrap_previous: window.wrap_previous.map(Arc::new),
            wrap_next: window.wrap_next.map(Arc::new),
            entries: window.occurrences.into_iter().map(Arc::new).collect(),
            window_start: window.window_start,
            total: window.total,
            selected_index: window.current_index,
            repeat_mode: window.repeat_mode,
            shuffle_enabled: window.shuffled,
            revision,
            progress_millis: window.progress_millis.max(0) as u64,
            durable: true,
        };
        if sequence.selected_index.is_some() && sequence.selected().is_none() {
            return Err(SequenceError::MissingSelectedOccurrence);
        }
        Ok(sequence)
    }
    pub fn replace_window(
        &self,
        window: library::QueueRestore,
        revision: u64,
    ) -> Result<Self, SequenceError> {
        let mut next = Self::from_window(window, revision)?;
        for entry in next
            .entries
            .iter_mut()
            .chain(next.wrap_previous.iter_mut())
            .chain(next.wrap_next.iter_mut())
        {
            if let Some(existing) = self.occurrence(&entry.occurrence)
                && existing.as_ref() == entry.as_ref()
            {
                *entry = Arc::clone(existing);
            }
        }
        Ok(next)
    }
    pub fn entries(&self) -> &[Arc<QueueOccurrence>] {
        &self.entries
    }
    pub(crate) fn mark_prepared(&mut self) {
        self.durable = false;
    }

    pub(crate) fn is_durable(&self) -> bool {
        self.durable
    }

    pub(crate) fn mark_durable(&mut self) {
        self.durable = true;
    }

    pub(crate) fn prepared_window(&self) -> Option<Vec<Arc<QueueOccurrence>>> {
        (!self.durable).then(|| {
            self.entries
                .iter()
                .chain(self.wrap_previous.iter())
                .chain(self.wrap_next.iter())
                .cloned()
                .collect()
        })
    }
    pub(crate) fn artwork_uris(&self) -> Vec<String> {
        self.entries
            .iter()
            .chain(self.wrap_previous.iter())
            .chain(self.wrap_next.iter())
            .map(|entry| entry.media_uri.clone())
            .collect()
    }

    pub(crate) fn refresh_artwork(&mut self, bindings: &[(String, Option<Vec<u8>>)]) -> bool {
        let mut changed = false;
        for entry in self
            .entries
            .iter_mut()
            .chain(self.wrap_previous.iter_mut())
            .chain(self.wrap_next.iter_mut())
        {
            if let Some((_, binding)) = bindings.iter().find(|(uri, _)| uri == &entry.media_uri)
                && entry.artwork_binding != *binding
            {
                Arc::make_mut(entry).item.artwork_binding = binding.clone();
                changed = true;
            }
        }
        changed
    }
    pub fn total(&self) -> usize {
        self.total
    }
    pub fn window_start(&self) -> usize {
        self.window_start
    }
    pub fn at(&self, index: usize) -> Option<&Arc<QueueOccurrence>> {
        if index == 0 && self.window_start > 0 {
            return self.wrap_next.as_ref();
        }
        if index + 1 == self.total && self.window_start + self.entries.len() < self.total {
            return self.wrap_previous.as_ref();
        }
        self.entries.get(index.checked_sub(self.window_start)?)
    }
    pub fn selected(&self) -> Option<&Arc<QueueOccurrence>> {
        self.at(self.selected_index?)
    }
    pub fn selected_index(&self) -> Option<usize> {
        self.selected_index
    }
    pub fn occurrence(&self, id: &OccurrenceId) -> Option<&Arc<QueueOccurrence>> {
        self.entries
            .iter()
            .chain(self.wrap_previous.iter())
            .chain(self.wrap_next.iter())
            .find(|entry| &entry.occurrence == id)
    }
    pub fn occurrence_index(&self, id: &OccurrenceId) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| &entry.occurrence == id)
            .map(|index| index + self.window_start)
            .or_else(|| {
                self.wrap_previous
                    .as_ref()
                    .filter(|entry| &entry.occurrence == id)
                    .map(|_| self.total - 1)
            })
            .or_else(|| {
                self.wrap_next
                    .as_ref()
                    .filter(|entry| &entry.occurrence == id)
                    .map(|_| 0)
            })
    }
    pub fn context_index(&self, context_id: &str, uri: &str, rank: usize) -> Option<usize> {
        self.entries.iter().position(|entry|entry.media_uri==uri && matches!(&entry.provenance,Provenance::Context{context_id:context,source_rank} if context.as_ref()==context_id && *source_rank==rank)).map(|index|index+self.window_start)
    }
    pub fn repeat_mode(&self) -> RepeatMode {
        self.repeat_mode
    }
    pub fn shuffle_enabled(&self) -> bool {
        self.shuffle_enabled
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn progress_millis(&self) -> u64 {
        self.progress_millis
    }
    pub fn set_progress_millis(&mut self, value: u64) {
        self.progress_millis = value;
    }
    pub fn set_repeat_mode(&mut self, value: RepeatMode) {
        self.repeat_mode = value;
    }
    pub fn remaining_after_selected(&self) -> usize {
        self.selected_index
            .map_or(self.total, |index| self.total.saturating_sub(index + 1))
    }
    pub fn peek_next_eos(&self) -> Option<&Arc<QueueOccurrence>> {
        self.at(self.next_index_eos()?)
    }
    pub(crate) fn next_index_eos(&self) -> Option<usize> {
        self.next_index(true)
    }
    pub(crate) fn next_index(&self, eos: bool) -> Option<usize> {
        let current = self.selected_index?;
        if eos && self.repeat_mode == RepeatMode::One {
            return Some(current);
        }
        if current + 1 < self.total {
            return Some(current + 1);
        }
        (self.repeat_mode == RepeatMode::All && self.total > 0).then_some(0)
    }
    pub fn previous_index(&self) -> Option<usize> {
        let current = self.selected_index?;
        if current == 0 && self.repeat_mode == RepeatMode::All {
            self.total.checked_sub(1)
        } else {
            current.checked_sub(1)
        }
    }
    pub fn peek_previous(&self) -> Option<&Arc<QueueOccurrence>> {
        self.at(self.previous_index()?)
    }
    pub fn upcoming(&self, limit: usize) -> Vec<&Arc<QueueOccurrence>> {
        let start = self
            .selected_index
            .map_or(self.window_start, |index| index + 1);
        (start..self.total)
            .take(limit)
            .filter_map(|index| self.at(index))
            .collect()
    }
    pub fn activate(&mut self, id: &OccurrenceId) -> bool {
        self.occurrence_index(id)
            .is_some_and(|index| self.activate_index(index))
    }
    pub(crate) fn activate_index(&mut self, index: usize) -> bool {
        if self.at(index).is_none() || self.selected_index == Some(index) {
            return false;
        }
        self.selected_index = Some(index);
        self.progress_millis = 0;
        true
    }
    pub(crate) fn activate_backend(&mut self, id: &OccurrenceId) -> bool {
        let Some(index) = self.occurrence_index(id) else {
            return false;
        };
        self.selected_index = Some(index);
        self.progress_millis = 0;
        true
    }
    pub fn advance_manual(&mut self) -> Option<&Arc<QueueOccurrence>> {
        let index = self.next_index(false)?;
        self.activate_index(index);
        self.at(index)
    }
    pub fn advance_eos(&mut self) -> Option<&Arc<QueueOccurrence>> {
        let index = self.next_index(true)?;
        if self.at(index).is_none() {
            return None;
        };
        self.selected_index = Some(index);
        self.progress_millis = 0;
        self.at(index)
    }
    pub fn previous(&mut self) -> Option<&Arc<QueueOccurrence>> {
        let index = self.previous_index()?;
        self.activate_index(index);
        self.at(index)
    }
    pub fn needs_refill(&self) -> bool {
        self.selected_index.is_some_and(|index| {
            (self.window_start > 0 && index < self.window_start + 20)
                || (self.window_start + self.entries.len() < self.total
                    && index + 20 >= self.window_start + self.entries.len())
        })
    }
}
impl Default for Sequence {
    fn default() -> Self {
        Self::new()
    }
}
