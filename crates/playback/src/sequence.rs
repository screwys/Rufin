//! Complete lightweight playback membership, order, and nearby resolved metadata.
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
    #[error("the selected playback occurrence is missing")]
    MissingSelectedOccurrence,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum QueuePersistenceKind {
    State,
    Order,
    Membership,
}

#[derive(Clone, Debug)]
pub struct Sequence {
    members: Arc<[library::QueueEntry]>,
    order: Arc<[u32]>,
    rows: Vec<Arc<QueueOccurrence>>,
    selected_index: Option<usize>,
    repeat_mode: RepeatMode,
    shuffle_enabled: bool,
    revision: u64,
    progress_millis: u64,
    dirty: Option<QueuePersistenceKind>,
    // Position ticks do not change the metadata window.
    window_dirty: bool,
}

impl Sequence {
    pub fn new() -> Self {
        Self {
            members: Arc::from([]),
            order: Arc::from([]),
            rows: Vec::new(),
            selected_index: None,
            repeat_mode: RepeatMode::Off,
            shuffle_enabled: false,
            revision: 0,
            progress_millis: 0,
            dirty: None,
            window_dirty: true,
        }
    }
    pub fn from_window(
        window: library::QueueRestore,
        revision: u64,
    ) -> Result<Self, SequenceError> {
        let sequence = Self {
            members: window.entries,
            order: window.order,
            rows: window.occurrences,
            selected_index: window.current_index,
            repeat_mode: window.repeat_mode,
            shuffle_enabled: window.shuffled,
            revision,
            progress_millis: window.progress_millis.max(0) as u64,
            dirty: None,
            window_dirty: true,
        };
        if sequence
            .selected_index
            .is_some_and(|i| sequence.entry_at(i).is_none())
        {
            return Err(SequenceError::MissingSelectedOccurrence);
        }
        Ok(sequence)
    }
    pub(crate) fn snapshot(&self) -> library::QueueRestore {
        library::QueueRestore {
            entries: self.members.clone(),
            order: self.order.clone(),
            occurrences: self.rows.clone(),
            current_index: self.selected_index,
            progress_millis: self.progress_millis as i64,
            repeat_mode: self.repeat_mode,
            shuffled: self.shuffle_enabled,
            next_id: 0,
        }
    }
    fn changed(&mut self, kind: QueuePersistenceKind) {
        self.revision += 1;
        self.window_dirty |= kind != QueuePersistenceKind::State;
        self.dirty = Some(self.dirty.map_or(kind, |old| old.max(kind)));
    }
    pub(crate) fn take_persistence(&mut self) -> Option<QueuePersistenceKind> {
        self.dirty.take()
    }
    pub(crate) fn entry_at(&self, index: usize) -> Option<&library::QueueEntry> {
        self.members.get(*self.order.get(index)? as usize)
    }
    pub(crate) fn selected_id(&self) -> Option<&OccurrenceId> {
        Some(&self.entry_at(self.selected_index?)?.occurrence)
    }
    pub fn entries(&self) -> &[Arc<QueueOccurrence>] {
        &self.rows
    }
    pub fn total(&self) -> usize {
        self.order.len()
    }
    pub fn at(&self, index: usize) -> Option<&Arc<QueueOccurrence>> {
        self.occurrence(&self.entry_at(index)?.occurrence)
    }
    pub fn selected(&self) -> Option<&Arc<QueueOccurrence>> {
        self.at(self.selected_index?)
    }
    pub fn selected_index(&self) -> Option<usize> {
        self.selected_index
    }
    pub fn occurrence(&self, id: &OccurrenceId) -> Option<&Arc<QueueOccurrence>> {
        self.rows.iter().find(|row| &row.occurrence == id)
    }
    pub fn occurrence_index(&self, id: &OccurrenceId) -> Option<usize> {
        if let Some(current) = self.selected_index {
            for i in [current, current + 1] {
                if self
                    .entry_at(i)
                    .is_some_and(|entry| &entry.occurrence == id)
                {
                    return Some(i);
                }
            }
        }
        self.order
            .iter()
            .position(|&i| &self.members[i as usize].occurrence == id)
    }
    pub fn context_index(&self, context_id: &str, uri: &str, rank: usize) -> Option<usize> {
        self.order.iter().position(|&i| { let e = &self.members[i as usize];
            e.media_uri.as_ref() == uri && matches!(&e.provenance, Provenance::Context { context_id: c, source_rank } if c.as_ref() == context_id && *source_rank == rank) })
    }
    pub fn repeat_mode(&self) -> RepeatMode {
        self.repeat_mode
    }
    pub fn shuffle_enabled(&self) -> bool {
        self.shuffle_enabled
    }
    pub(crate) fn observe_shuffle(&mut self, enabled: bool) {
        self.shuffle_enabled = enabled;
    }
    pub(crate) fn persist_membership(&mut self) {
        self.changed(QueuePersistenceKind::Membership);
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
        if self.repeat_mode != value {
            self.repeat_mode = value;
            self.changed(QueuePersistenceKind::State);
        }
    }
    pub fn remaining_after_selected(&self) -> usize {
        self.total()
            .saturating_sub(self.selected_index.map_or(0, |i| i + 1))
    }
    pub(crate) fn next_index(&self, eos: bool) -> Option<usize> {
        let current = self.selected_index?;
        if eos && self.repeat_mode == RepeatMode::One {
            return Some(current);
        }
        if current + 1 < self.total() {
            return Some(current + 1);
        }
        (self.repeat_mode == RepeatMode::All && self.total() > 0).then_some(self.total())
    }
    pub(crate) fn next_index_eos(&self) -> Option<usize> {
        self.next_index(true)
    }
    pub fn peek_next_eos(&self) -> Option<&Arc<QueueOccurrence>> {
        self.at(self.next_index_eos()?)
    }
    pub fn previous_index(&self) -> Option<usize> {
        let current = self.selected_index?;
        if current == 0 && self.repeat_mode == RepeatMode::All {
            self.total().checked_sub(1)
        } else {
            current.checked_sub(1)
        }
    }
    pub fn peek_previous(&self) -> Option<&Arc<QueueOccurrence>> {
        self.at(self.previous_index()?)
    }
    pub fn upcoming(&self, limit: usize) -> Vec<&Arc<QueueOccurrence>> {
        let start = self.selected_index.map_or(0, |i| i + 1);
        (start..self.total())
            .take(limit)
            .filter_map(|i| self.at(i))
            .collect()
    }
    pub fn activate(&mut self, id: &OccurrenceId) -> bool {
        self.occurrence_index(id)
            .is_some_and(|i| self.activate_index(i))
    }
    pub(crate) fn activate_index(&mut self, index: usize) -> bool {
        if index >= self.total() || self.selected_index == Some(index) {
            return false;
        }
        self.selected_index = Some(index);
        self.progress_millis = 0;
        self.window_dirty = true;
        true
    }
    pub(crate) fn activate_backend(&mut self, id: &OccurrenceId) -> bool {
        let Some(i) = self.occurrence_index(id) else {
            return false;
        };
        self.selected_index = Some(i);
        self.progress_millis = 0;
        self.window_dirty = true;
        true
    }
    pub(crate) fn start_next_pass(&mut self) {
        let order = Arc::make_mut(&mut self.order);
        for (i, entry) in order.iter_mut().enumerate() {
            *entry = i as u32;
        }
        if self.shuffle_enabled {
            shuffle_indices(order, self.revision.wrapping_add(1));
        }
        self.selected_index = None;
        self.changed(QueuePersistenceKind::Order);
    }
    pub(crate) fn advance_index(&mut self, eos: bool) -> Option<usize> {
        let mut index = self.next_index(eos)?;
        if index == self.total() {
            self.start_next_pass();
            index = 0;
        }
        self.selected_index = Some(index);
        self.progress_millis = 0;
        self.window_dirty = true;
        Some(index)
    }
    pub fn advance_manual(&mut self) -> Option<&Arc<QueueOccurrence>> {
        let i = self.advance_index(false)?;
        self.at(i)
    }
    pub fn advance_eos(&mut self) -> Option<&Arc<QueueOccurrence>> {
        let i = self.advance_index(true)?;
        self.at(i)
    }
    pub fn previous(&mut self) -> Option<&Arc<QueueOccurrence>> {
        let i = self.previous_index()?;
        self.activate_index(i);
        self.at(i)
    }
    pub(crate) fn shuffle(&mut self, enabled: bool, seed: u64) {
        if self.shuffle_enabled == enabled {
            return;
        }
        self.shuffle_enabled = enabled;
        let start = self.selected_index.map_or(0, |i| i + 1);
        let remaining = &mut Arc::make_mut(&mut self.order)[start..];
        if enabled {
            shuffle_indices(remaining, seed);
        } else {
            remaining.sort_unstable();
        }
        self.changed(QueuePersistenceKind::Order);
    }
    pub(crate) fn random_start(&mut self, seed: u64) {
        self.shuffle_enabled = true;
        if let Some(selected) = self.selected_index {
            let order = Arc::make_mut(&mut self.order);
            order.swap(0, selected);
            shuffle_indices(&mut order[1..], seed);
        }
        self.selected_index = (!self.order.is_empty()).then_some(0);
        self.trim_rows();
    }
    pub(crate) fn add_page(
        &mut self,
        page: library::QueueReadPage,
        target: QueueReorderTarget,
        replacing: bool,
        seed: Option<u64>,
    ) {
        if replacing {
            self.members = page.entries.into();
            self.order = (0..self.members.len() as u32).collect::<Vec<_>>().into();
            self.rows = page.occurrences;
            self.selected_index = (!self.members.is_empty()).then_some(page.current_index);
            self.progress_millis = 0;
            self.shuffle_enabled = false;
            if let Some(seed) = seed {
                self.shuffle(true, seed);
            }
        } else {
            let current = self.selected_id().cloned();
            let at = self.target(&target, self.order.iter().copied());
            let canonical_at = self.target(&target, 0..self.members.len() as u32);
            let count = page.entries.len();
            let mut members = self.members.to_vec();
            members.splice(canonical_at..canonical_at, page.entries);
            let mut order = self.order.to_vec();
            for i in &mut order {
                if *i as usize >= canonical_at {
                    *i += count as u32;
                }
            }
            order.splice(
                at..at,
                (canonical_at..canonical_at + count).map(|i| i as u32),
            );
            self.members = members.into();
            self.order = order.into();
            self.rows.extend(page.occurrences);
            self.keep_selected(current, at);
        }
        self.trim_rows();
        self.changed(QueuePersistenceKind::Membership);
    }
    fn target(
        &self,
        target: &QueueReorderTarget,
        mut order: impl ExactSizeIterator<Item = u32>,
    ) -> usize {
        let end = order.len();
        match target {
            QueueReorderTarget::End => end,
            QueueReorderTarget::Before(id) | QueueReorderTarget::After(id) => order
                .position(|i| &self.members[i as usize].occurrence == id)
                .map_or(end, |i| {
                    i + usize::from(matches!(target, QueueReorderTarget::After(_)))
                }),
        }
    }
    fn keep_selected(&mut self, id: Option<OccurrenceId>, fallback: usize) {
        self.selected_index = id
            .and_then(|id| self.occurrence_index(&id))
            .or_else(|| (!self.order.is_empty()).then(|| fallback.min(self.order.len() - 1)));
    }
    pub(crate) fn remove(&mut self, ids: &[OccurrenceId]) {
        let ids = ids.iter().collect::<std::collections::HashSet<_>>();
        let current = self.selected_id().cloned();
        let at = self.selected_index.unwrap_or(0);
        let mut remap = vec![None; self.members.len()];
        let members = self
            .members
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                if ids.contains(&e.occurrence) {
                    None
                } else {
                    Some((i, e.clone()))
                }
            })
            .enumerate()
            .map(|(next, (old, e))| {
                remap[old] = Some(next as u32);
                e
            })
            .collect::<Vec<_>>();
        self.order = self
            .order
            .iter()
            .filter_map(|&i| remap[i as usize])
            .collect::<Vec<_>>()
            .into();
        self.members = members.into();
        self.rows.retain(|r| !ids.contains(&r.occurrence));
        self.keep_selected(current, at);
        self.changed(QueuePersistenceKind::Membership);
    }
    pub(crate) fn reorder(&mut self, ids: &[OccurrenceId], target: &QueueReorderTarget) {
        if matches!(target, QueueReorderTarget::Before(id) | QueueReorderTarget::After(id) if ids.contains(id))
        {
            return;
        }
        let current = self.selected_id().cloned();
        let ids = ids.iter().collect::<std::collections::HashSet<_>>();
        let moving = self
            .order
            .iter()
            .copied()
            .filter(|&i| ids.contains(&self.members[i as usize].occurrence))
            .collect::<Vec<_>>();
        let moving_set = moving
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        let mut order = self
            .order
            .iter()
            .copied()
            .filter(|i| !moving_set.contains(i))
            .collect::<Vec<_>>();
        let at = self.target(target, order.iter().copied());
        order.splice(at..at, moving.iter().copied());
        let mut ordinary = (0..self.members.len() as u32)
            .filter(|i| !moving_set.contains(i))
            .collect::<Vec<_>>();
        let at = self.target(target, ordinary.iter().copied());
        ordinary.splice(at..at, moving);
        let mut remap = vec![0; ordinary.len()];
        let members = ordinary
            .into_iter()
            .enumerate()
            .map(|(new, old)| {
                remap[old as usize] = new as u32;
                self.members[old as usize].clone()
            })
            .collect::<Vec<_>>();
        self.order = order
            .into_iter()
            .map(|i| remap[i as usize])
            .collect::<Vec<_>>()
            .into();
        self.members = members.into();
        self.keep_selected(current, 0);
        self.changed(QueuePersistenceKind::Membership);
    }
    pub(crate) fn clear(&mut self, include_current: bool) {
        let member = (!include_current)
            .then(|| self.entry_at(self.selected_index?).cloned())
            .flatten();
        self.members = member.into_iter().collect::<Vec<_>>().into();
        self.order = (0..self.members.len() as u32).collect::<Vec<_>>().into();
        self.selected_index = (!self.members.is_empty()).then_some(0);
        self.trim_rows();
        self.changed(QueuePersistenceKind::Membership);
    }
    fn window_range(&self) -> std::ops::Range<usize> {
        let start = self.selected_index.unwrap_or(0).saturating_sub(10);
        start..(start + library::QUEUE_CONTEXT_LIMIT).min(self.total())
    }
    fn trim_rows(&mut self) {
        let ids = self
            .window_range()
            .filter_map(|i| self.entry_at(i).map(|e| e.occurrence.clone()))
            .collect::<Vec<_>>();
        self.rows.retain(|r| ids.contains(&r.occurrence));
        self.rows
            .sort_by_key(|r| ids.iter().position(|id| id == &r.occurrence));
    }
    pub(crate) fn read_request(&mut self) -> Option<library::QueueReadRequest> {
        if !std::mem::take(&mut self.window_dirty) {
            return None;
        }
        self.trim_rows();
        let entries = self
            .window_range()
            .filter_map(|i| self.entry_at(i))
            .filter(|e| self.occurrence(&e.occurrence).is_none())
            .cloned()
            .collect::<Vec<_>>();
        (!entries.is_empty()).then_some(library::QueueReadRequest::Hydrate { entries })
    }
    pub(crate) fn hydrate(&mut self, page: library::QueueReadPage) {
        for row in page.occurrences {
            if self.occurrence(&row.occurrence).is_none() {
                self.rows.push(row);
            }
        }
        self.trim_rows();
        self.window_dirty = true;
    }
    pub(crate) fn need_metadata(&mut self) {
        self.window_dirty = true;
    }
    pub(crate) fn artwork_uris(&self) -> Vec<String> {
        self.rows.iter().map(|e| e.media_uri.clone()).collect()
    }
    pub(crate) fn refresh_artwork(&mut self, bindings: &[(String, Option<Vec<u8>>)]) -> bool {
        let mut changed = false;
        for row in &mut self.rows {
            if let Some((_, binding)) = bindings.iter().find(|(u, _)| u == &row.media_uri)
                && &row.artwork_binding != binding
            {
                Arc::make_mut(row).item.artwork_binding = binding.clone();
                changed = true;
            }
        }
        changed
    }
}
impl Default for Sequence {
    fn default() -> Self {
        Self::new()
    }
}

fn shuffle_indices(values: &mut [u32], mut seed: u64) {
    for i in (1..values.len()).rev() {
        seed = seed.wrapping_add(0x9e3779b97f4a7c15);
        let mut n = seed;
        n = (n ^ (n >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        n = (n ^ (n >> 27)).wrapping_mul(0x94d049bb133111eb);
        n ^= n >> 31;
        values.swap(i, ((u128::from(n) * (i as u128 + 1)) >> 64) as usize);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(start: usize, count: usize) -> library::QueueReadPage {
        let entries = (start..start + count)
            .map(|i| library::QueueEntry {
                occurrence: OccurrenceId::new(format!("entry:{i}")),
                media_uri: format!("https://example.test/{}", i / 2).into(),
                playlist_entry_id: Some(format!("appearance:{i}").into()),
                provenance: Provenance::Manual,
            })
            .collect::<Vec<_>>();
        let occurrences = entries
            .iter()
            .take(100)
            .enumerate()
            .map(|(i, e)| {
                Arc::new(QueueOccurrence {
                    occurrence: e.occurrence.clone(),
                    item: QueueItem::direct(e.media_uri.as_ref(), format!("{i}"), "", "", 1000),
                    canonical_position: i,
                    source_index: None,
                    playlist_entry_id: e.playlist_entry_id.as_deref().map(str::to_owned),
                    provenance: e.provenance.clone(),
                })
            })
            .collect();
        library::QueueReadPage {
            entries,
            occurrences,
            current_index: 0,
        }
    }
    fn sequence(count: usize) -> Sequence {
        let mut s = Sequence::new();
        s.add_page(page(0, count), QueueReorderTarget::End, true, None);
        s.take_persistence();
        s
    }
    #[test]
    fn restored_membership_hydrates_a_later_duplicate_on_navigation() {
        let mut s = Sequence::from_window(sequence(10_000).snapshot(), 1).unwrap();
        let selected = OccurrenceId::new("entry:9999");
        assert!(s.activate_index(9999));
        assert_eq!(s.selected_id(), Some(&selected));
        assert!(s.selected().is_none());
        let Some(library::QueueReadRequest::Hydrate { entries }) = s.read_request() else {
            panic!("later membership needs nearby metadata");
        };
        assert!(entries.len() <= library::QUEUE_CONTEXT_LIMIT);
        assert!(entries.iter().any(|entry| entry.occurrence == selected));
        s.hydrate(page(9989, 11));
        assert_eq!(s.selected().unwrap().occurrence, selected);
        assert_eq!(s.total(), 10_000);
        assert!(s.entries().len() <= library::QUEUE_CONTEXT_LIMIT);
    }

    #[test]
    fn complete_membership_and_bounded_metadata_have_separate_ownership() {
        let mut s = sequence(10_000);
        assert_eq!(s.total(), 10_000);
        assert_eq!(s.entries().len(), 100);
        let membership = s.members.clone();
        let current = s.selected().unwrap().clone();
        s.shuffle(true, 17);
        assert!(Arc::ptr_eq(&membership, &s.members));
        assert!(Arc::ptr_eq(&current, s.selected().unwrap()));
        assert_eq!(s.take_persistence(), Some(QueuePersistenceKind::Order));
        let Some(library::QueueReadRequest::Hydrate { entries }) = s.read_request() else {
            panic!("nearby metadata");
        };
        assert!(entries.len() <= 99);
        s.shuffle(false, 17);
        assert_eq!(&*s.order, &(0..10_000).collect::<Vec<u32>>());
    }
    #[test]
    fn toggles_keep_history_and_repeat_resets_only_order() {
        let mut s = sequence(10);
        s.activate_index(2);
        s.shuffle(true, 3);
        s.advance_index(false);
        let history = s.order[..4].to_vec();
        let current = s.selected_id().cloned();
        s.shuffle(false, 0);
        assert_eq!(s.order[..4], history);
        assert_eq!(s.selected_id(), current.as_ref());
        assert!(s.order[4..].windows(2).all(|w| w[0] < w[1]));
        s.set_repeat_mode(RepeatMode::All);
        s.activate_index(9);
        s.take_persistence();
        let members = s.members.clone();
        s.advance_index(false);
        assert_eq!(&*s.order, &(0..10).collect::<Vec<u32>>());
        assert_eq!(s.selected_index(), Some(0));
        assert!(Arc::ptr_eq(&members, &s.members));
        assert_eq!(s.take_persistence(), Some(QueuePersistenceKind::Order));
        s.previous();
        assert_eq!(s.selected_index(), Some(9));
        assert_eq!(s.take_persistence(), None);
    }
    #[test]
    fn additions_edits_duplicates_and_restore_keep_occurrence_identity() {
        let mut s = sequence(101);
        s.add_page(page(1000, 1), QueueReorderTarget::End, false, None);
        assert_eq!(s.entry_at(100).unwrap().occurrence.as_str(), "entry:100");
        assert_eq!(s.entry_at(101).unwrap().occurrence.as_str(), "entry:1000");
        s.activate_index(3);
        let current = s.selected_id().unwrap().clone();
        s.set_progress_millis(42000);
        s.shuffle(true, 4);
        s.add_page(
            page(2000, 1),
            QueueReorderTarget::After(current.clone()),
            false,
            None,
        );
        assert_eq!(s.entry_at(4).unwrap().occurrence.as_str(), "entry:2000");
        s.remove(&[OccurrenceId::from("entry:0")]);
        assert!(s.occurrence_index(&OccurrenceId::from("entry:1")).is_some());
        s.reorder(
            &[OccurrenceId::from("entry:1000")],
            &QueueReorderTarget::Before(OccurrenceId::from("entry:2000")),
        );
        s.shuffle(false, 0);
        assert_eq!(s.selected_id(), Some(&current));
        assert_eq!(s.progress_millis(), 42000);
        let restored = Sequence::from_window(s.snapshot(), s.revision()).unwrap();
        assert_eq!(restored.order, s.order);
        assert_eq!(restored.members, s.members);
        assert_eq!(restored.selected_id(), Some(&current));
    }
}
