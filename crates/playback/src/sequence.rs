//! Owns the bounded playback queue, edits, and pending source or explicit choices.
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
    pub(crate) fn activation_context(&self, index: usize) -> Option<(String, String, usize)> {
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
    #[error("the saved playback queue exceeds 100 entries")]
    ContextLimit,
    #[error("the selected playback occurrence is missing")]
    MissingSelectedOccurrence,
}
#[derive(Clone, Debug)]
pub struct Sequence {
    entries: Vec<Arc<QueueOccurrence>>,
    sources: Vec<library::QueueInstruction>,
    pending: std::collections::VecDeque<library::QueueCursor>,
    next_id: u64,
    selected_index: Option<usize>,
    repeat_mode: RepeatMode,
    shuffle_enabled: bool,
    revision: u64,
    progress_millis: u64,
}
impl Sequence {
    pub(crate) fn snapshot(&self) -> library::QueueRestore {
        library::QueueRestore {
            occurrences: self.entries.clone(),
            current_index: self.selected_index,
            progress_millis: self.progress_millis as i64,
            repeat_mode: self.repeat_mode,
            shuffled: self.shuffle_enabled,
            sources: self.sources.clone(),
            pending: self.pending.clone(),
            next_id: self.next_id,
        }
    }

    fn keep_selected(&mut self, current: Option<OccurrenceId>, fallback: usize) {
        self.selected_index = current
            .and_then(|id| self.occurrence_index(&id))
            .or_else(|| (!self.entries.is_empty()).then(|| fallback.min(self.entries.len() - 1)));
    }

    fn target(&self, target: &QueueReorderTarget) -> usize {
        match target {
            QueueReorderTarget::Before(id) => {
                self.occurrence_index(id).unwrap_or(self.entries.len())
            }
            QueueReorderTarget::After(id) => self
                .occurrence_index(id)
                .map_or(self.entries.len(), |i| i + 1),
            QueueReorderTarget::End => self.entries.len(),
        }
    }

    fn explicit_choice(&self, row: &QueueOccurrence) -> Option<(usize, usize)> {
        let source = row.source_index?;
        let library::QueueInput::Choices(choices) = &self.sources[source].input else {
            return None;
        };
        let choice = choices.get(row.canonical_position)?.as_ref()?;
        choice.origin.or(Some((source, row.canonical_position)))
    }

    fn defer(&mut self, rows: Vec<Arc<QueueOccurrence>>, front: bool) {
        if rows.is_empty() {
            return;
        }
        let input = library::QueueInput::Choices(
            rows.into_iter()
                .map(|row| {
                    Some(library::QueueChoice {
                        origin: self.explicit_choice(&row),
                        media_uri: row.media_uri.clone(),
                        fallback: (!matches!(row.provenance, Provenance::Context { .. }))
                            .then(|| row.item.clone()),
                        provenance: row.provenance.clone(),
                    })
                })
                .collect(),
        );
        let source = self.sources.len();
        self.sources.push(library::QueueInstruction {
            input,
            repeat: false,
            seed: None,
        });
        let cursor = library::QueueCursor {
            source,
            ..Default::default()
        };
        if front {
            self.pending.push_front(cursor)
        } else {
            self.pending.push_back(cursor)
        }
    }

    pub(crate) fn read_request(&mut self) -> Option<library::QueueReadRequest> {
        let cursor = self.pending.front()?.clone();
        if let Some(index) = self.selected_index
            && index >= 20
        {
            self.entries.drain(..index - 10);
            self.selected_index = Some(10);
            self.revision += 1;
        }
        let limit = library::QUEUE_CONTEXT_LIMIT.saturating_sub(self.entries.len());
        (limit > 0).then(|| library::QueueReadRequest {
            input: self.sources[cursor.source].input.clone(),
            cursor,
            limit,
            history: false,
            backwards: false,
        })
    }

    pub(crate) fn add_page(
        &mut self,
        mut page: library::QueueReadPage,
        target: QueueReorderTarget,
        replacing: bool,
        seed: Option<u64>,
    ) {
        let current = self.selected().map(|row| row.occurrence.clone());
        if replacing {
            self.entries.clear();
            self.sources.clear();
            self.pending.clear();
            self.selected_index = None;
            self.progress_millis = 0;
            self.shuffle_enabled = seed.is_some();
        }
        let following = if let library::QueueInput::Groups(inputs) = page.input {
            let mut inputs = inputs.into_iter();
            page.input = inputs
                .next()
                .unwrap_or(library::QueueInput::Choices(Arc::from([])));
            inputs.collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let source = self.sources.len();
        self.sources.push(library::QueueInstruction {
            input: page.input,
            repeat: true,
            seed: page.cursor.seed,
        });
        let following = following
            .into_iter()
            .map(|input| {
                let source = self.sources.len();
                self.sources.push(library::QueueInstruction {
                    input,
                    repeat: true,
                    seed: page.cursor.seed,
                });
                library::QueueCursor {
                    source,
                    seed: page.cursor.seed,
                    ..Default::default()
                }
            })
            .collect::<Vec<_>>();
        let at = if replacing { 0 } else { self.target(&target) };
        let rows = self.admit(source, page.items);
        let mut next = page.cursor;
        next.source = source;
        if target == QueueReorderTarget::End && !replacing && !self.pending.is_empty() {
            // Play Last follows all preceding continuation, including entry 101.
            self.pending.push_back(library::QueueCursor {
                offset: 0,
                after: None,
                ..next
            });
            self.pending.extend(following);
        } else {
            let mut inserted_end = at + rows.len();
            self.entries.splice(at..at, rows);
            if let Some(index) = current.as_ref().and_then(|id| self.occurrence_index(id))
                && index >= 100
            {
                self.entries.drain(..index - 10);
                inserted_end = inserted_end.saturating_sub(index - 10);
            }
            let mut new_tail = Vec::new();
            if self.entries.len() > 100 {
                let mut excess = self.entries.split_off(100);
                let old_tail = excess.split_off(inserted_end.saturating_sub(100).min(excess.len()));
                self.defer(old_tail, true);
                new_tail = excess;
            }
            for cursor in following.into_iter().rev() {
                self.pending.push_front(cursor);
            }
            if !page.exhausted {
                self.pending.push_front(next);
            }
            self.defer(new_tail, true);
        }
        if replacing {
            self.selected_index =
                (!self.entries.is_empty()).then(|| page.current_index.min(self.entries.len() - 1));
        } else {
            self.keep_selected(current, at);
        }
        self.revision += 1;
    }

    fn admit(
        &mut self,
        source: usize,
        items: Vec<(QueueItem, Provenance, usize, Option<String>)>,
    ) -> Vec<Arc<QueueOccurrence>> {
        items
            .into_iter()
            .map(|(item, provenance, rank, playlist_entry_id)| {
                self.next_id += 1;
                Arc::new(QueueOccurrence {
                    occurrence: OccurrenceId::new(format!("queue:{}", self.next_id)),
                    item,
                    canonical_position: rank,
                    source_index: Some(source),
                    playlist_entry_id,
                    provenance,
                })
            })
            .collect()
    }

    pub(crate) fn refill(&mut self, page: library::QueueReadPage) {
        let Some(mut cursor) = self.pending.pop_front() else {
            return;
        };
        let rows = self.admit(cursor.source, page.items);
        cursor.after = page.cursor.after;
        cursor.offset = page.cursor.offset;
        if !page.exhausted {
            self.pending.push_front(cursor);
        }
        self.entries.extend(rows);
        if self.selected_index.is_none() && !self.entries.is_empty() {
            self.selected_index = Some(0);
        }
        if self.entries.len() > 100 {
            let excess = self.entries.split_off(100);
            self.defer(excess, true);
        }
        self.revision += 1;
    }

    pub(crate) fn remove(&mut self, ids: &[OccurrenceId]) {
        let current = self.selected().map(|row| row.occurrence.clone());
        let at = self.selected_index.unwrap_or(0);
        let choices = self
            .entries
            .iter()
            .filter(|row| ids.contains(&row.occurrence))
            .flat_map(|row| {
                [
                    self.explicit_choice(row),
                    row.source_index
                        .map(|source| (source, row.canonical_position)),
                ]
                .into_iter()
                .flatten()
            })
            .collect::<Vec<_>>();
        for (source, position) in choices {
            if let library::QueueInput::Choices(choices) = &mut self.sources[source].input {
                if let Some(choice) = Arc::make_mut(choices).get_mut(position) {
                    *choice = None;
                }
            }
        }
        self.entries.retain(|row| !ids.contains(&row.occurrence));
        self.keep_selected(current, at);
        self.revision += 1;
    }

    pub(crate) fn reorder(&mut self, ids: &[OccurrenceId], target: &QueueReorderTarget) {
        if matches!(target,QueueReorderTarget::Before(id)|QueueReorderTarget::After(id) if ids.contains(id))
        {
            return;
        }
        let current = self.selected().map(|row| row.occurrence.clone());
        let rows = self
            .entries
            .iter()
            .filter(|row| ids.contains(&row.occurrence))
            .cloned()
            .collect::<Vec<_>>();
        let moving_current = current.as_ref().is_some_and(|id| ids.contains(id));
        self.entries.retain(|row| !ids.contains(&row.occurrence));
        if *target == QueueReorderTarget::End && !self.pending.is_empty() && !moving_current {
            self.defer(rows, false);
        } else {
            let at = self.target(target);
            self.entries.splice(at..at, rows);
            if moving_current && *target == QueueReorderTarget::End {
                self.pending.clear();
            }
        }
        self.keep_selected(current, 0);
        self.revision += 1;
    }

    pub(crate) fn clear(&mut self, include_current: bool) {
        let current = (!include_current)
            .then(|| self.selected().cloned())
            .flatten();
        self.entries = current.into_iter().collect();
        self.selected_index = (!self.entries.is_empty()).then_some(0);
        self.sources.clear();
        self.pending.clear();
        for row in &mut self.entries {
            Arc::make_mut(row).source_index = None;
        }
        self.revision += 1;
    }

    pub(crate) fn restart(&mut self, seed: Option<u64>) {
        if let Some(index) = self.selected_index {
            self.entries.truncate(index + 1);
        }
        self.pending = self
            .sources
            .iter()
            .enumerate()
            .filter(|(_, source)| source.repeat)
            .map(|(source, _)| library::QueueCursor {
                source,
                seed,
                ..Default::default()
            })
            .collect();
        for source in &mut self.sources {
            source.input.clear_anchor();
            source.seed = seed;
        }
    }

    pub(crate) fn previous_request(&self) -> Option<library::QueueReadRequest> {
        let (source, instruction) = self
            .sources
            .iter()
            .enumerate()
            .rev()
            .find(|(_, source)| source.repeat)?;
        let mut input = instruction.input.clone();
        input.clear_anchor();
        Some(library::QueueReadRequest {
            input,
            cursor: library::QueueCursor {
                source,
                seed: instruction.seed,
                ..Default::default()
            },
            limit: 1,
            history: false,
            backwards: true,
        })
    }

    pub(crate) fn prepend_previous(&mut self, page: library::QueueReadPage) {
        let rows = self.admit(page.cursor.source, page.items);
        self.entries.splice(0..0, rows);
        if self.entries.len() > 100 {
            let excess = self.entries.split_off(100);
            self.defer(excess, true);
        }
        self.selected_index = (!self.entries.is_empty()).then_some(0);
        self.revision += 1;
    }

    pub(crate) fn shuffle(&mut self, enabled: bool, seed: u64) {
        if enabled == self.shuffle_enabled {
            return;
        }
        self.shuffle_enabled = enabled;
        // Begin a fresh source traversal anchored at the current item.
        let current = self.selected().cloned();
        if let Some(row) = current
            && let Some(source) = row.source_index
        {
            let mut input = self.sources[source].input.clone();
            if let library::QueueInput::Source { reference, .. } = &mut input {
                reference.anchor_uri = Some(row.media_uri.clone());
                if let library::QueueScope::Playlist { anchor_entry, .. } = &mut reference.scope {
                    *anchor_entry = row.playlist_entry_id.clone();
                }
            }
            self.sources[source].input = input;
            self.sources[source].seed = enabled.then_some(seed);
            let at = self.selected_index.unwrap();
            let additions = self
                .entries
                .drain(at + 1..)
                .filter(|entry| entry.source_index != Some(source))
                .collect();
            self.pending.retain(|cursor| cursor.source != source);
            self.pending.push_front(library::QueueCursor {
                source,
                seed: enabled.then_some(seed),
                anchor: Some(row.canonical_position),
                offset: 1,
                ..Default::default()
            });
            self.defer(additions, true);
        }
        self.revision += 1;
    }
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            sources: Vec::new(),
            pending: Default::default(),
            next_id: 0,
            selected_index: None,
            repeat_mode: RepeatMode::Off,
            shuffle_enabled: false,
            revision: 0,
            progress_millis: 0,
        }
    }
    pub fn from_window(
        window: library::QueueRestore,
        revision: u64,
    ) -> Result<Self, SequenceError> {
        if window.occurrences.len() > library::QUEUE_CONTEXT_LIMIT {
            return Err(SequenceError::ContextLimit);
        }
        let sequence = Self {
            entries: window.occurrences,
            sources: window.sources,
            pending: window.pending,
            next_id: window.next_id,
            selected_index: window.current_index,
            repeat_mode: window.repeat_mode,
            shuffle_enabled: window.shuffled,
            revision,
            progress_millis: window.progress_millis.max(0) as u64,
        };
        if sequence.selected_index.is_some() && sequence.selected().is_none() {
            return Err(SequenceError::MissingSelectedOccurrence);
        }
        Ok(sequence)
    }
    pub fn entries(&self) -> &[Arc<QueueOccurrence>] {
        &self.entries
    }
    pub(crate) fn artwork_uris(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|entry| entry.media_uri.clone())
            .collect()
    }

    pub(crate) fn refresh_artwork(&mut self, bindings: &[(String, Option<Vec<u8>>)]) -> bool {
        let mut changed = false;
        for entry in self.entries.iter_mut() {
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
        self.entries.len()
    }
    pub fn at(&self, index: usize) -> Option<&Arc<QueueOccurrence>> {
        self.entries.get(index)
    }
    pub fn selected(&self) -> Option<&Arc<QueueOccurrence>> {
        self.at(self.selected_index?)
    }
    pub fn selected_index(&self) -> Option<usize> {
        self.selected_index
    }
    pub fn occurrence(&self, id: &OccurrenceId) -> Option<&Arc<QueueOccurrence>> {
        self.entries.iter().find(|entry| &entry.occurrence == id)
    }
    pub fn occurrence_index(&self, id: &OccurrenceId) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| &entry.occurrence == id)
    }
    pub fn context_index(&self, context_id: &str, uri: &str, rank: usize) -> Option<usize> {
        self.entries.iter().position(|entry|entry.media_uri==uri && matches!(&entry.provenance,Provenance::Context{context_id:context,source_rank} if context.as_ref()==context_id && *source_rank==rank))
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
        self.selected_index.map_or(self.entries.len(), |index| {
            self.entries.len().saturating_sub(index + 1)
        })
    }
    pub(crate) fn has_more(&self) -> bool {
        !self.pending.is_empty()
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
        if current + 1 < self.entries.len() {
            return Some(current + 1);
        }
        (self.has_more() || (self.repeat_mode == RepeatMode::All && self.entries.len() > 0))
            .then_some(self.entries.len())
    }
    pub fn previous_index(&self) -> Option<usize> {
        let current = self.selected_index?;
        if current == 0 && self.repeat_mode == RepeatMode::All {
            Some(self.entries.len())
        } else {
            current.checked_sub(1)
        }
    }
    pub fn peek_previous(&self) -> Option<&Arc<QueueOccurrence>> {
        self.at(self.previous_index()?)
    }
    pub fn upcoming(&self, limit: usize) -> Vec<&Arc<QueueOccurrence>> {
        let start = self.selected_index.map_or(0, |index| index + 1);
        (start..self.entries.len())
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
        self.has_more()
            && (self.entries.len() < 100 || self.selected_index.is_some_and(|index| index >= 20))
    }
}
impl Default for Sequence {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod queue_tests {
    use super::*;
    use library::{Database, QueueInput, QueueReadRequest};

    fn choices(start: usize, count: usize) -> QueueInput {
        QueueInput::Items(
            (start..start + count)
                .map(|i| {
                    (
                        QueueItem::direct(
                            format!("https://example.test/{i}"),
                            i.to_string(),
                            "",
                            "",
                            1000,
                        ),
                        Provenance::Manual,
                    )
                })
                .collect(),
        )
    }
    async fn read(database: &Database, input: QueueInput, limit: usize) -> library::QueueReadPage {
        database
            .read_queue(QueueReadRequest {
                input,
                cursor: Default::default(),
                limit,
                history: false,
                backwards: false,
            })
            .await
            .unwrap()
    }
    async fn fill(database: &Database, sequence: &mut Sequence) {
        while let Some(request) = sequence.read_request() {
            let page = database.read_queue(request).await.unwrap();
            sequence.refill(page);
        }
    }
    async fn take(database: &Database, sequence: &mut Sequence, count: usize) -> Vec<String> {
        let mut titles = Vec::new();
        for _ in 0..count {
            titles.push(sequence.selected().unwrap().title.clone());
            fill(database, sequence).await;
            sequence.advance_manual();
            assert!(sequence.entries.len() <= 100);
        }
        titles
    }
    #[tokio::test]
    async fn play_last_follows_101_and_large_explicit_additions_survive_full_window_insertion() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("queue.sqlite3"))
            .await
            .unwrap();
        for count in [101, 500] {
            let mut sequence = Sequence::new();
            sequence.add_page(
                read(&db, choices(0, count), 100).await,
                QueueReorderTarget::End,
                true,
                None,
            );
            sequence.add_page(
                read(&db, choices(1000, 1), 0).await,
                QueueReorderTarget::End,
                false,
                None,
            );
            assert_eq!(
                take(&db, &mut sequence, count + 1).await,
                (0..count)
                    .chain([1000])
                    .map(|i| i.to_string())
                    .collect::<Vec<_>>()
            );
        }
        let mut sequence = Sequence::new();
        sequence.add_page(
            read(&db, choices(0, 101), 100).await,
            QueueReorderTarget::End,
            true,
            None,
        );
        let current = sequence.selected().unwrap().clone();
        sequence.add_page(
            read(&db, choices(1000, 500), 100).await,
            QueueReorderTarget::After(current.occurrence.clone()),
            false,
            None,
        );
        assert!(Arc::ptr_eq(&current, sequence.selected().unwrap()));
        assert_eq!(
            take(&db, &mut sequence, 601).await,
            [0].into_iter()
                .chain(1000..1500)
                .chain(1..101)
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
        );
    }
    #[tokio::test]
    async fn shuffle_retains_current_and_explicit_additions_without_replaying_current() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("queue.sqlite3"))
            .await
            .unwrap();
        let mut sequence = Sequence::new();
        sequence.add_page(
            read(&db, choices(0, 101), 100).await,
            QueueReorderTarget::End,
            true,
            None,
        );
        sequence.activate_index(17);
        let current = sequence.selected().unwrap().clone();
        sequence.add_page(
            read(&db, choices(1000, 1), 100).await,
            QueueReorderTarget::After(current.occurrence.clone()),
            false,
            None,
        );
        sequence.shuffle(true, 71);
        assert!(Arc::ptr_eq(&current, sequence.selected().unwrap()));
        let mut titles = take(&db, &mut sequence, 102).await;
        titles.sort();
        let mut expected = (0..101)
            .chain([1000])
            .map(|i| i.to_string())
            .collect::<Vec<_>>();
        expected.sort();
        assert_eq!(titles, expected);
        let saved = sequence.snapshot();
        db.save_queue(&saved).await.unwrap();
        assert_eq!(db.restore_queue().await.unwrap(), saved);
    }
    #[tokio::test]
    async fn playlist_anchor_history_shuffle_and_deleted_source_keep_exact_appearances() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("queue.sqlite3"))
            .await
            .unwrap();
        let uri = "https://example.test/repeated".to_string();
        let (key, _) = db
            .create_playlist(None, "Repeated", &vec![uri.clone(); 120])
            .await
            .unwrap()
            .unwrap();
        let order = db
            .playlist_entry_order(
                key,
                None,
                library::PlaylistEntrySort::Position,
                false,
                "",
                &library::ReadCancellation::new(),
            )
            .await
            .unwrap();
        let input = QueueInput::PlaylistQuery {
            key,
            folder: None,
            filter: String::new(),
            sort: library::PlaylistEntrySort::Position,
            descending: false,
            context_id: "playlist".into(),
            anchor_entry: Some(order[51]),
            anchor_uri: Some(uri),
        };
        let page = db
            .read_queue(QueueReadRequest {
                input,
                cursor: library::QueueCursor {
                    anchor: Some(51),
                    ..Default::default()
                },
                limit: 100,
                history: true,
                backwards: false,
            })
            .await
            .unwrap();
        let mut sequence = Sequence::new();
        sequence.add_page(page, QueueReorderTarget::End, true, None);
        assert_eq!(sequence.selected_index(), Some(10));
        let previous = sequence.at(9).unwrap().playlist_entry_id.clone();
        sequence.previous();
        assert_eq!(sequence.selected().unwrap().playlist_entry_id, previous);
        sequence.activate_index(13);
        let current = sequence.selected().unwrap().clone();
        sequence.shuffle(true, 71);
        let mut appearances = Vec::new();
        for _ in 0..120 {
            appearances.push(
                sequence
                    .selected()
                    .unwrap()
                    .playlist_entry_id
                    .clone()
                    .unwrap(),
            );
            fill(&db, &mut sequence).await;
            sequence.advance_manual();
        }
        assert_eq!(appearances.first(), current.playlist_entry_id.as_ref());
        let mut unique = appearances.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), 120);
        sequence = Sequence::new();
        sequence.add_page(
            read(
                &db,
                QueueInput::Collection {
                    collection: library::QueueCollection::Playlist(key),
                    folder: None,
                    context_id: "playlist".into(),
                },
                100,
            )
            .await,
            QueueReorderTarget::End,
            true,
            None,
        );
        sequence.add_page(
            read(&db, choices(1000, 1), 0).await,
            QueueReorderTarget::End,
            false,
            None,
        );
        db.delete_playlist(None, key).await.unwrap();
        let titles = take(&db, &mut sequence, 101).await;
        assert_eq!(titles.last().unwrap(), "1000");
    }

    #[tokio::test]
    async fn removing_a_displaced_explicit_choice_also_removes_it_from_repeat() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("queue.sqlite3"))
            .await
            .unwrap();
        let mut sequence = Sequence::new();
        sequence.add_page(
            read(&db, choices(0, 101), 100).await,
            QueueReorderTarget::End,
            true,
            None,
        );
        let current = sequence.selected().unwrap().occurrence.clone();
        sequence.add_page(
            read(&db, choices(1000, 2), 100).await,
            QueueReorderTarget::After(current),
            false,
            None,
        );
        sequence.activate_index(99);
        fill(&db, &mut sequence).await;
        let removed = sequence
            .entries
            .iter()
            .find(|row| row.title == "99")
            .unwrap()
            .occurrence
            .clone();
        sequence.remove(&[removed]);
        sequence.activate_index(sequence.entries.len() - 1);
        sequence.restart(None);
        fill(&db, &mut sequence).await;
        sequence.advance_manual();
        let titles = take(&db, &mut sequence, 102).await;
        assert!(!titles.iter().any(|title| title == "99"));
        let last = sequence
            .entries
            .iter()
            .find(|row| row.title == "1001")
            .unwrap()
            .occurrence
            .clone();
        sequence.remove(&[last]);
        let previous = db
            .read_queue(sequence.previous_request().unwrap())
            .await
            .unwrap();
        assert_eq!(previous.items[0].0.title, "1000");
    }

    #[tokio::test]
    async fn grouped_sources_are_separate_ordered_instructions() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("queue.sqlite3"))
            .await
            .unwrap();
        let mut inputs = Vec::new();
        let expected = (0..104)
            .map(|i| format!("https://example.test/{i}"))
            .collect::<Vec<_>>();
        for uris in [&expected[..101], &expected[101..]] {
            let (key, _) = db
                .create_playlist(None, "Group", uris)
                .await
                .unwrap()
                .unwrap();
            inputs.push(QueueInput::Collection {
                collection: library::QueueCollection::Playlist(key),
                folder: None,
                context_id: "group".into(),
            });
        }
        let mut sequence = Sequence::new();
        sequence.add_page(
            read(&db, QueueInput::Groups(inputs), 100).await,
            QueueReorderTarget::End,
            true,
            None,
        );
        assert_eq!(sequence.sources.len(), 2);
        assert!(
            sequence
                .sources
                .iter()
                .all(|source| matches!(source.input, QueueInput::Source { .. }))
        );
        let mut actual = Vec::new();
        for _ in 0..104 {
            actual.push(sequence.selected().unwrap().media_uri.clone());
            fill(&db, &mut sequence).await;
            sequence.advance_manual();
        }
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn bounded_reordering_and_duplicate_removal_preserve_current_identity_and_progress() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("queue.sqlite3"))
            .await
            .unwrap();
        let mut sequence = Sequence::new();
        sequence.add_page(
            read(&db, choices(0, 6), 100).await,
            QueueReorderTarget::End,
            true,
            None,
        );
        sequence.activate_index(2);
        sequence.set_progress_millis(42000);
        let current = sequence.selected().unwrap().clone();
        let moved = sequence.at(5).unwrap().occurrence.clone();
        sequence.reorder(
            std::slice::from_ref(&moved),
            &QueueReorderTarget::After(current.occurrence.clone()),
        );
        assert_eq!(
            sequence
                .entries
                .iter()
                .map(|row| row.title.as_str())
                .collect::<Vec<_>>(),
            ["0", "1", "2", "5", "3", "4"]
        );
        sequence.remove(&[moved]);
        assert!(Arc::ptr_eq(&current, sequence.selected().unwrap()));
        assert_eq!(sequence.progress_millis(), 42000);
        let mut duplicates = choices(10, 2);
        if let QueueInput::Items(items) = &mut duplicates {
            items[1].0.media_uri = items[0].0.media_uri.clone();
        }
        sequence.add_page(
            read(&db, duplicates, 100).await,
            QueueReorderTarget::After(current.occurrence.clone()),
            false,
            None,
        );
        let first = sequence.at(3).unwrap().occurrence.clone();
        let second = sequence.at(4).unwrap().occurrence.clone();
        sequence.remove(&[first]);
        assert!(sequence.occurrence(&second).is_some());
        assert!(Arc::ptr_eq(&current, sequence.selected().unwrap()));
        assert_eq!(sequence.progress_millis(), 42000);
    }
}
