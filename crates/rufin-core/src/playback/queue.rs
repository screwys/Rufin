//! Compact live queue entries, two orders, and nearby resolved metadata.
use crate::playback::{QueueReorderTarget, QueueSelection};
use playback::{Provenance, QueueRow, RepeatMode};
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    sync::Arc,
};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(super) struct OccurrenceId(pub(super) u64);

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub(crate) enum SequenceError {
    #[error("the selected playback entry is missing")]
    MissingSelectedEntry,
    #[error("queue IDs do not match its entries")]
    InvalidEntryIds,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MetadataRead {
    Missing,
    Window,
}

#[derive(Clone, Debug)]
pub(super) struct Sequence {
    members: HashMap<OccurrenceId, library::QueueEntry>,
    ordinary: Vec<OccurrenceId>,
    order: Vec<OccurrenceId>,
    rows: Vec<Arc<QueueRow>>,
    selected_index: Option<usize>,
    next_id: u64,
    repeat_mode: RepeatMode,
    shuffle_enabled: bool,
    revision: u64,
    progress_millis: u64,
    dirty: Option<library::QueueUpdate>,
    metadata_read: Option<MetadataRead>,
}

impl Sequence {
    pub(super) fn new() -> Self {
        Self {
            members: HashMap::new(),
            ordinary: Vec::new(),
            order: Vec::new(),
            rows: Vec::new(),
            selected_index: None,
            next_id: 0,
            repeat_mode: RepeatMode::Off,
            shuffle_enabled: false,
            revision: 0,
            progress_millis: 0,
            dirty: None,
            metadata_read: Some(MetadataRead::Missing),
        }
    }
    pub(super) fn from_window(
        window: library::QueueRestore,
        revision: u64,
        mut next_id: u64,
    ) -> Result<Self, SequenceError> {
        let ordinary = if window.ids.is_empty() {
            (0..window.entries.len())
                .map(|_| {
                    let id = OccurrenceId(next_id);
                    next_id += 1;
                    id
                })
                .collect::<Vec<_>>()
        } else {
            if window.ids.len() != window.entries.len() {
                return Err(SequenceError::InvalidEntryIds);
            }
            next_id = next_id.max(window.ids.iter().max().unwrap() + 1);
            window.ids.iter().copied().map(OccurrenceId).collect()
        };
        let order = window.order.iter().map(|i| ordinary[*i as usize]).collect();
        let mut sequence = Self {
            members: ordinary
                .iter()
                .copied()
                .zip(window.entries.iter().cloned())
                .collect(),
            ordinary,
            order,
            rows: window.rows,
            selected_index: window.current_index,
            next_id,
            repeat_mode: window.repeat_mode,
            shuffle_enabled: window.shuffled,
            revision,
            progress_millis: window.progress_millis.max(0) as u64,
            dirty: None,
            metadata_read: Some(MetadataRead::Missing),
        };
        if sequence
            .selected_index
            .is_some_and(|i| sequence.entry_at(i).is_none())
        {
            return Err(SequenceError::MissingSelectedEntry);
        }
        for row in &mut sequence.rows {
            let id = sequence.order.get(row.position).map(|id| id.0);
            if row.occurrence_id != id {
                Arc::make_mut(row).occurrence_id = id;
            }
        }
        Ok(sequence)
    }
    pub(super) fn next_id(&self) -> u64 {
        self.next_id
    }
    pub(super) fn snapshot(&self) -> library::QueueRestore {
        let positions = self
            .ordinary
            .iter()
            .enumerate()
            .map(|(i, id)| (*id, i as u32))
            .collect::<HashMap<_, _>>();
        library::QueueRestore {
            ids: self
                .ordinary
                .iter()
                .map(|id| id.0)
                .collect::<Vec<_>>()
                .into(),
            entries: self
                .ordinary
                .iter()
                .map(|id| self.members[id].clone())
                .collect::<Vec<_>>()
                .into(),
            order: self
                .order
                .iter()
                .map(|id| positions[id])
                .collect::<Vec<_>>()
                .into(),
            rows: self.rows.clone(),
            current_index: self.selected_index,
            progress_millis: self.progress_millis as i64,
            repeat_mode: self.repeat_mode,
            shuffled: self.shuffle_enabled,
        }
    }
    fn changed(&mut self, contents: bool) {
        self.revision += 1;
        if contents {
            self.need_metadata();
        }
        self.dirty.get_or_insert_default();
    }
    fn save_order(&mut self, ordinary: Option<Range<usize>>, active: Option<Range<usize>>) {
        let dirty = self.dirty.get_or_insert_default();
        for (update, order, range) in [
            (&mut dirty.ordinary, &self.ordinary, ordinary),
            (&mut dirty.order, &self.order, active),
        ] {
            if let Some(range) = range {
                update
                    .get_or_insert_default()
                    .coalesce(library::QueueOrderUpdate {
                        rows: range.map(|i| (i, order[i].0)).collect(),
                        total: order.len(),
                    });
            }
        }
    }
    pub(super) fn take_persistence(&mut self) -> Option<library::QueueUpdate> {
        let mut update = self.dirty.take()?;
        update.current_index = self.selected_index;
        update.progress_millis = self.progress_millis as i64;
        update.repeat_mode = self.repeat_mode;
        update.shuffled = self.shuffle_enabled;
        Some(update)
    }
    pub(super) fn id_at(&self, index: usize) -> Option<OccurrenceId> {
        self.order.get(index).copied()
    }
    pub(super) fn index_of(&self, id: OccurrenceId) -> Option<usize> {
        self.order.iter().position(|i| *i == id)
    }
    pub(super) fn entry_at(&self, index: usize) -> Option<&library::QueueEntry> {
        self.members.get(&self.id_at(index)?)
    }
    pub(super) fn page_entries(
        &self,
        offset: usize,
        limit: usize,
    ) -> Vec<(usize, library::QueueEntry)> {
        self.order
            .iter()
            .enumerate()
            .skip(offset)
            .take(limit)
            .map(|(i, id)| (i, self.members[id].clone()))
            .collect()
    }
    pub(super) fn entries(&self) -> &[Arc<QueueRow>] {
        &self.rows
    }
    pub(super) fn total(&self) -> usize {
        self.order.len()
    }
    pub(super) fn at_id(&self, id: OccurrenceId) -> Option<&Arc<QueueRow>> {
        self.rows.iter().find(|row| row.occurrence_id == Some(id.0))
    }
    pub(super) fn at(&self, index: usize) -> Option<&Arc<QueueRow>> {
        self.at_id(self.id_at(index)?)
    }
    pub(super) fn selected(&self) -> Option<&Arc<QueueRow>> {
        self.at(self.selected_index?)
    }
    pub(super) fn selected_index(&self) -> Option<usize> {
        self.selected_index
    }
    pub(super) fn context_index(
        &self,
        context_id: &str,
        key: library::TrackKey,
        rank: usize,
    ) -> Option<usize> {
        self.order.iter().position(|id| {
            let e = &self.members[id];
            e.track_key == key && matches!(&e.provenance, Provenance::Context { context_id: c, source_rank, .. } if c.as_ref() == context_id && *source_rank == rank)
        })
    }
    pub(super) fn repeat_mode(&self) -> RepeatMode {
        self.repeat_mode
    }
    pub(super) fn shuffle_enabled(&self) -> bool {
        self.shuffle_enabled
    }
    pub(super) fn observe_shuffle(&mut self, enabled: bool) {
        self.shuffle_enabled = enabled;
    }
    pub(super) fn persist_membership(&mut self) {
        self.dirty = Some(library::QueueUpdate {
            replace: true,
            entries: self
                .members
                .iter()
                .map(|(id, entry)| (id.0, entry.clone()))
                .collect(),
            ..Default::default()
        });
        self.save_order(Some(0..self.total()), Some(0..self.total()));
        self.changed(true);
    }
    pub(super) fn persist_state(&mut self) {
        self.dirty.get_or_insert_default();
    }
    pub(super) fn revision(&self) -> u64 {
        self.revision
    }

    pub(super) fn progress_millis(&self) -> u64 {
        self.progress_millis
    }
    pub(super) fn set_progress_millis(&mut self, value: u64) {
        self.progress_millis = value;
    }
    pub(super) fn set_selected_duration(&mut self, millis: u64) {
        let selected = self
            .selected_index
            .and_then(|i| self.id_at(i))
            .map(|id| id.0);
        if let Some(row) = self
            .rows
            .iter_mut()
            .find(|row| row.occurrence_id == selected)
        {
            Arc::make_mut(row).track.metadata.duration_millis = millis.min(i64::MAX as u64) as i64;
        }
    }
    pub(super) fn set_repeat_mode(&mut self, value: RepeatMode) {
        if self.repeat_mode != value {
            self.repeat_mode = value;
            self.changed(false);
        }
    }
    pub(super) fn remaining_after_selected(&self) -> usize {
        self.total()
            .saturating_sub(self.selected_index.map_or(0, |i| i + 1))
    }
    pub(super) fn next_index(&self, eos: bool) -> Option<usize> {
        let current = self.selected_index?;
        if eos && self.repeat_mode == RepeatMode::One {
            return Some(current);
        }
        if current + 1 < self.total() {
            return Some(current + 1);
        }
        (self.repeat_mode == RepeatMode::All && self.total() > 0).then_some(self.total())
    }
    pub(super) fn next_index_eos(&self) -> Option<usize> {
        self.next_index(true)
    }
    pub(super) fn peek_next_eos(&self) -> Option<&Arc<QueueRow>> {
        self.at(self.next_index_eos()?)
    }
    pub(super) fn previous_index(&self) -> Option<usize> {
        let current = self.selected_index?;
        if current == 0 && self.repeat_mode == RepeatMode::All {
            self.total().checked_sub(1)
        } else {
            current.checked_sub(1)
        }
    }
    pub(super) fn activate_index(&mut self, index: usize) -> bool {
        self.activate_backend(index)
    }
    pub(super) fn activate_backend(&mut self, index: usize) -> bool {
        if index >= self.total() {
            return false;
        }
        self.selected_index = Some(index);
        self.progress_millis = 0;
        self.need_metadata();
        true
    }
    pub(super) fn start_next_pass(&mut self) {
        self.order.clone_from(&self.ordinary);
        if self.shuffle_enabled {
            library::shuffle_order(&mut self.order, self.revision.wrapping_add(1));
        }
        self.selected_index = None;
        self.trim_rows();
        self.save_order(None, Some(0..self.total()));
        self.changed(true);
    }
    pub(super) fn advance_index(&mut self, eos: bool) -> Option<usize> {
        let mut index = self.next_index(eos)?;
        if index == self.total() {
            self.start_next_pass();
            index = 0;
        }
        self.activate_backend(index);
        Some(index)
    }
    pub(super) fn shuffle(&mut self, enabled: bool, seed: u64) {
        if self.shuffle_enabled == enabled {
            return;
        }
        self.shuffle_enabled = enabled;
        let start = self.selected_index.map_or(0, |i| i + 1);
        if enabled {
            library::shuffle_order(&mut self.order[start..], seed);
        } else {
            let played = self.order[..start].iter().copied().collect::<HashSet<_>>();
            self.order.truncate(start);
            self.order.extend(
                self.ordinary
                    .iter()
                    .filter(|id| !played.contains(id))
                    .copied(),
            );
        }
        self.trim_rows();
        self.save_order(None, Some(start..self.total()));
        self.changed(true);
    }
    pub(super) fn add_page(
        &mut self,
        page: super::commands::QueueBatch,
        target: QueueReorderTarget,
        replacing: bool,
    ) {
        if page.entries.is_empty() && !replacing {
            return;
        }
        if replacing {
            self.members = HashMap::with_capacity(page.entries.len());
            self.ordinary = Vec::with_capacity(page.entries.len());
            self.order = Vec::with_capacity(page.entries.len());
            self.rows.clear();
            self.dirty = Some(library::QueueUpdate {
                replace: true,
                ..Default::default()
            });
            self.selected_index = None;
            self.progress_millis = 0;
            self.shuffle_enabled = page.shuffle.is_some();
        }
        let (target_id, after) = self.target(&target);
        let at = self.target_position(&self.order, target_id, after);
        let ordinary_at = self.target_position(&self.ordinary, target_id, after);
        let ids = page
            .entries
            .into_iter()
            .map(|entry| {
                let id = OccurrenceId(self.next_id);
                self.next_id += 1;
                self.dirty
                    .get_or_insert_default()
                    .entries
                    .insert(id.0, entry.clone());
                self.members.insert(id, entry);
                id
            })
            .collect::<Vec<_>>();
        let count = ids.len();
        self.order.splice(at..at, ids.iter().copied());
        self.ordinary
            .splice(ordinary_at..ordinary_at, ids.iter().copied());
        if let Some(selected) = &mut self.selected_index {
            if *selected >= at {
                *selected += count;
            }
        }
        if self.selected_index.is_none() && !self.order.is_empty() {
            self.selected_index = Some(if replacing { page.current_index } else { at });
        }
        if replacing && let Some(shuffle) = page.shuffle {
            match shuffle {
                super::commands::QueueShuffle::AfterSelected(seed) => {
                    let start = self.selected_index.map_or(0, |index| index + 1);
                    library::shuffle_order(&mut self.order[start..], seed);
                }
                super::commands::QueueShuffle::FromSelected(seed) => {
                    if let Some(selected) = self.selected_index {
                        self.order.swap(0, selected);
                        library::shuffle_order(&mut self.order[1..], seed);
                        self.selected_index = Some(0);
                    }
                }
            }
        }
        self.rows
            .extend(page.rows.into_iter().filter_map(|mut row| {
                let id = *ids.get(row.position)?;
                let row_mut = Arc::make_mut(&mut row);
                row_mut.occurrence_id = Some(id.0);
                Some(row)
            }));
        self.trim_rows();
        self.save_order(
            Some(ordinary_at..self.ordinary.len()),
            Some(at..self.total()),
        );
        self.changed(true);
    }
    fn target(&self, target: &QueueReorderTarget) -> (Option<OccurrenceId>, bool) {
        match target {
            QueueReorderTarget::End => (None, false),
            QueueReorderTarget::Before(position) => (self.id_at(*position), false),
            QueueReorderTarget::After(position) => (self.id_at(*position), true),
            QueueReorderTarget::BeforeOccurrence(id) => (Some(OccurrenceId(*id)), false),
            QueueReorderTarget::AfterOccurrence(id) => (Some(OccurrenceId(*id)), true),
        }
    }
    fn target_position(
        &self,
        order: &[OccurrenceId],
        id: Option<OccurrenceId>,
        after: bool,
    ) -> usize {
        id.and_then(|id| order.iter().position(|i| *i == id))
            .map_or(order.len(), |i| i + usize::from(after))
    }
    fn selection(&self, selection: &QueueSelection) -> Vec<OccurrenceId> {
        let mut ids = match selection {
            QueueSelection::Positions(positions) => positions
                .iter()
                .filter_map(|i| self.id_at(*i))
                .collect::<Vec<_>>(),
            QueueSelection::Occurrences(ids) => ids
                .iter()
                .copied()
                .map(OccurrenceId)
                .filter(|id| self.members.contains_key(id))
                .collect(),
        };
        ids.sort_unstable();
        ids.dedup();
        ids
    }
    pub(super) fn remove(&mut self, selection: &QueueSelection) {
        let removed = self.selection(selection);
        if removed.is_empty() {
            return;
        }
        let current = self.selected_index.and_then(|i| self.id_at(i));
        let mut selected = None;
        let mut kept = 0;
        let mut next = 0;
        let mut active_start = self.total();
        self.order.retain(|id| {
            let keep = removed.binary_search(id).is_err();
            if Some(*id) == current {
                next = kept;
                if keep {
                    selected = Some(kept);
                }
            }
            if !keep {
                active_start = active_start.min(kept);
            }
            kept += usize::from(keep);
            keep
        });
        let mut ordinary_start = self.ordinary.len();
        let mut position = 0;
        self.ordinary.retain(|id| {
            let keep = removed.binary_search(id).is_err();
            if !keep {
                ordinary_start = ordinary_start.min(position);
            }
            position += usize::from(keep);
            keep
        });
        let dirty = self.dirty.get_or_insert_default();
        for id in removed {
            self.members.remove(&id);
            dirty.entries.remove(&id.0);
            dirty.removed.insert(id.0);
        }
        if selected.is_none() && !self.order.is_empty() {
            selected = Some(next.min(self.total() - 1));
            self.progress_millis = 0;
        }
        self.selected_index = selected;
        self.trim_rows();
        self.save_order(
            Some(ordinary_start..self.ordinary.len()),
            Some(active_start..self.total()),
        );
        self.changed(true);
    }
    pub(super) fn reorder(&mut self, selection: &QueueSelection, target: &QueueReorderTarget) {
        let selected = self.selection(selection);
        let (target_id, after) = self.target(target);
        if selected.is_empty() || target_id.is_some_and(|id| selected.binary_search(&id).is_ok()) {
            return;
        }
        let current = self.selected_index.and_then(|i| self.id_at(i));
        let moving = self
            .order
            .iter()
            .filter(|id| selected.binary_search(id).is_ok())
            .copied()
            .collect::<Vec<_>>();
        let active = move_entries(&mut self.order, &moving, &selected, target_id, after);
        let ordinary = move_entries(&mut self.ordinary, &moving, &selected, target_id, after);
        if active.is_none() && ordinary.is_none() {
            return;
        }
        self.selected_index = current.and_then(|id| self.index_of(id));
        self.trim_rows();
        self.save_order(ordinary, active);
        self.changed(true);
    }
    pub(super) fn clear(&mut self, include_current: bool) {
        let selected = self
            .selected_index
            .filter(|_| !include_current)
            .and_then(|i| self.id_at(i));
        if self.total() == usize::from(selected.is_some()) {
            return;
        }
        self.members = selected
            .and_then(|id| self.members.remove(&id).map(|entry| (id, entry)))
            .into_iter()
            .collect();
        self.ordinary = selected.into_iter().collect();
        self.order = self.ordinary.clone();
        self.selected_index = selected.map(|_| 0);
        self.trim_rows();
        self.persist_membership();
    }
    fn window_range(&self) -> Range<usize> {
        let start = self.selected_index.unwrap_or(0).saturating_sub(10);
        start..(start + library::QUEUE_CONTEXT_LIMIT).min(self.total())
    }
    fn trim_rows(&mut self) {
        let positions = self
            .window_range()
            .map(|i| (self.order[i].0, i))
            .collect::<HashMap<_, _>>();
        self.rows.retain_mut(|row| {
            let Some(position) = row.occurrence_id.and_then(|id| positions.get(&id)).copied()
            else {
                return false;
            };
            if row.position != position {
                Arc::make_mut(row).position = position;
            }
            true
        });
        self.rows.sort_by_key(|row| row.position);
    }
    pub(super) fn read_request(&mut self) -> Option<Vec<(OccurrenceId, library::QueueEntry)>> {
        let read = self.metadata_read.take()?;
        self.trim_rows();
        let entries = self
            .window_range()
            .map(|i| self.order[i])
            .filter(|id| read == MetadataRead::Window || self.at_id(*id).is_none())
            .map(|id| (id, self.members[&id].clone()))
            .collect::<Vec<_>>();
        (!entries.is_empty()).then_some(entries)
    }
    pub(super) fn hydrate(&mut self, items: Vec<(OccurrenceId, library::TrackView)>) -> bool {
        let mut changed = false;
        let mut refreshed = false;
        let positions = self
            .window_range()
            .map(|i| (self.order[i], i))
            .collect::<HashMap<_, _>>();
        for (id, item) in items {
            if !self
                .members
                .get(&id)
                .is_some_and(|entry| entry.track_key == item.track_key)
            {
                continue;
            }
            if let Some(row) = self
                .rows
                .iter_mut()
                .find(|row| row.occurrence_id == Some(id.0))
            {
                if row.track != item {
                    Arc::make_mut(row).track = item;
                    changed = true;
                    refreshed = true;
                }
                continue;
            }
            let Some(&position) = positions.get(&id) else {
                continue;
            };
            let entry = &self.members[&id];
            changed = true;
            self.rows.push(Arc::new(QueueRow {
                occurrence_id: Some(id.0),
                position,
                track: item,
                provenance: entry.provenance.clone(),
                received_from_connect: entry.received_from_connect,
            }));
        }
        self.trim_rows();
        self.need_metadata();
        if refreshed {
            self.revision += 1;
        }
        changed
    }
    pub(super) fn rekey_tracks(
        &mut self,
        pairs: &[(library::TrackKey, library::TrackKey)],
    ) -> bool {
        let mut keys = HashMap::new();
        for &(old, new) in pairs.iter().rev() {
            keys.insert(old, keys.get(&new).copied().unwrap_or(new));
        }
        let mut changed = false;
        for (id, entry) in &mut self.members {
            if let Some(&key) = keys.get(&entry.track_key)
                && key != entry.track_key
            {
                entry.track_key = key;
                self.dirty
                    .get_or_insert_default()
                    .entries
                    .insert(id.0, entry.clone());
                changed = true;
            }
        }
        if changed {
            for row in &mut self.rows {
                if let Some(key) = keys.get(&row.track.track_key).copied() {
                    Arc::make_mut(row).track.track_key = key;
                }
            }
            self.changed(true);
            self.refresh_metadata();
        }
        changed
    }

    pub(super) fn need_metadata(&mut self) {
        self.metadata_read.get_or_insert(MetadataRead::Missing);
    }
    pub(super) fn refresh_metadata(&mut self) {
        self.metadata_read = Some(MetadataRead::Window);
    }
    pub(super) fn refreshing_metadata(&self) -> bool {
        self.metadata_read == Some(MetadataRead::Window)
    }
}

fn move_entries(
    order: &mut Vec<OccurrenceId>,
    moving: &[OccurrenceId],
    selected: &[OccurrenceId],
    target: Option<OccurrenceId>,
    after: bool,
) -> Option<Range<usize>> {
    let mut target_position = order.len();
    let positions = order
        .iter()
        .enumerate()
        .filter_map(|(i, id)| {
            if Some(*id) == target {
                target_position = i + usize::from(after);
            }
            selected.binary_search(id).is_ok().then_some(i)
        })
        .collect::<Vec<_>>();
    let target = target_position;
    let at = target - positions.partition_point(|i| *i < target);
    let start = positions[0];
    let end = positions.last().copied().unwrap() + 1;
    if start == at && end - start == moving.len() && &order[start..end] == moving {
        return None;
    }
    let mut next = positions.iter().copied().peekable();
    let mut position = 0;
    order.retain(|_| {
        let keep = next.peek() != Some(&position);
        if !keep {
            next.next();
        }
        position += 1;
        keep
    });
    order.splice(at..at, moving.iter().copied());
    Some(start.min(at)..end.max(at + moving.len()))
}

impl Default for Sequence {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::playback::TrackReference;

    fn page(start: usize, count: usize) -> super::super::commands::QueueBatch {
        let entries = (start..start + count)
            .map(|i| library::QueueEntry {
                received_from_connect: false,
                track_key: library::TrackKey::from_raw((i / 2 + 1) as i64),
                provenance: Provenance::Manual,
            })
            .collect::<Vec<_>>();
        let rows = entries
            .iter()
            .take(100)
            .enumerate()
            .map(|(i, e)| {
                Arc::new(QueueRow {
                    occurrence_id: None,
                    position: i,
                    received_from_connect: false,
                    provenance: e.provenance.clone(),
                    track: {
                        let reference = TrackReference::direct(
                            format!("https://example.test/{}", e.track_key.raw()),
                            format!("{i}"),
                            "",
                            "",
                            1000,
                        );
                        library::TrackView {
                            track_key: e.track_key,
                            source_key: None,
                            source_id: None,
                            object_id: None,
                            media_uri: reference.media_uri,
                            metadata: reference.metadata,
                            artwork_binding: reference.artwork_binding,
                            state: Default::default(),
                            access: Default::default(),
                            links: Default::default(),
                        }
                    },
                })
            })
            .collect();
        super::super::commands::QueueBatch {
            entries,
            rows,
            current_index: 0,
            shuffle: None,
        }
    }
    fn sequence(count: usize) -> Sequence {
        let mut s = Sequence::new();
        s.add_page(page(0, count), QueueReorderTarget::End, true);
        s.take_persistence();
        s
    }
    #[test]
    fn restored_membership_hydrates_a_later_duplicate_on_navigation() {
        let mut s = Sequence::from_window(sequence(10_000).snapshot(), 1, 0).unwrap();
        assert!(s.activate_index(9999));
        let selected = s.id_at(9999).unwrap();
        assert!(s.selected().is_none());
        let entries = s
            .read_request()
            .expect("later membership needs nearby metadata");
        assert!(entries.len() <= library::QUEUE_CONTEXT_LIMIT);
        assert!(entries.iter().any(|(id, _)| *id == selected));
        s.remove(&QueueSelection::Positions(vec![0]));
        s.hydrate(
            entries
                .into_iter()
                .map(|(id, entry)| {
                    (
                        id,
                        library::TrackView {
                            track_key: entry.track_key,
                            source_key: None,
                            source_id: None,
                            object_id: None,
                            media_uri: format!("https://example.test/{}", entry.track_key.raw()),
                            metadata: library::TrackMetadata {
                                duration_millis: 1000,
                                ..Default::default()
                            },
                            state: Default::default(),
                            access: Default::default(),
                            links: Default::default(),
                            artwork_binding: None,
                        },
                    )
                })
                .collect(),
        );
        assert_eq!(s.selected_index(), Some(9998));
        assert_eq!(s.selected().unwrap().occurrence_id, Some(selected.0));
        assert_eq!(s.selected().unwrap().position, 9998);
        assert_eq!(s.total(), 9999);
        assert!(s.entries().len() <= library::QUEUE_CONTEXT_LIMIT);
    }
    #[test]
    fn complete_membership_and_bounded_metadata_have_separate_ownership() {
        let mut s = sequence(10_000);
        assert_eq!(s.total(), 10_000);
        assert_eq!(s.entries().len(), 100);
        let current = s.selected().unwrap().clone();
        s.shuffle(true, 17);
        assert_eq!(s.members.len(), 10_000);
        assert!(Arc::ptr_eq(&current, s.selected().unwrap()));
        let update = s.take_persistence().unwrap();
        assert!(update.entries.is_empty());
        assert!(update.ordinary.is_none());
        assert_eq!(update.order.unwrap().rows.len(), 9999);
        assert!(!update.replace);
        let entries = s.read_request().expect("nearby metadata");
        assert!(entries.len() <= 99);
        s.shuffle(false, 17);
        assert_eq!(s.order, s.ordinary);
    }
    #[test]
    fn toggles_keep_history_and_repeat_resets_only_order() {
        let mut s = sequence(10);
        s.activate_index(2);
        s.shuffle(true, 3);
        s.advance_index(false);
        let history = s.order[..4].to_vec();
        let current = s.id_at(s.selected_index().unwrap());
        s.shuffle(false, 0);
        assert_eq!(s.order[..4], history);
        assert_eq!(s.id_at(s.selected_index().unwrap()), current);
        assert!(s.order[4..].windows(2).all(|w| w[0] < w[1]));
        s.set_repeat_mode(RepeatMode::All);
        s.activate_index(9);
        s.take_persistence();
        let members = s.members.clone();
        s.advance_index(false);
        assert_eq!(s.order, s.ordinary);
        assert_eq!(s.selected_index(), Some(0));
        assert_eq!(members, s.members);
        let update = s.take_persistence().unwrap();
        assert!(update.entries.is_empty());
        assert!(update.order.is_some());
        assert!(update.ordinary.is_none());
        s.activate_index(s.previous_index().unwrap());
        assert_eq!(s.selected_index(), Some(9));
        assert!(s.take_persistence().is_none());
    }
    #[test]
    fn additions_edits_duplicates_and_restore_keep_selected_entry() {
        let mut s = sequence(101);
        s.add_page(page(1000, 1), QueueReorderTarget::End, false);
        assert_eq!(
            s.entry_at(100).unwrap().track_key,
            library::TrackKey::from_raw(51)
        );
        assert_eq!(
            s.entry_at(101).unwrap().track_key,
            library::TrackKey::from_raw(501)
        );
        let update = s.take_persistence().unwrap();
        assert_eq!(update.entries.len(), 1);
        assert_eq!(update.ordinary.unwrap().rows.len(), 1);
        assert_eq!(update.order.unwrap().rows.len(), 1);
        s.activate_index(3);
        let current_id = s.id_at(3).unwrap();
        let current = s.entry_at(3).unwrap().clone();
        s.set_progress_millis(42000);
        let prepared_id = s.id_at(4).unwrap();
        let prepared = s.members[&prepared_id].clone();
        s.shuffle(true, 4);
        s.add_page(
            page(2000, 1),
            QueueReorderTarget::AfterOccurrence(current_id.0),
            false,
        );
        assert_eq!(
            s.entry_at(4).unwrap().track_key,
            library::TrackKey::from_raw(1001)
        );
        s.remove(&QueueSelection::Positions(vec![0]));
        assert_eq!(s.selected_index(), Some(2));
        let moving = s
            .order
            .iter()
            .find(|id| s.members[id].track_key == library::TrackKey::from_raw(501))
            .copied()
            .unwrap();
        s.reorder(
            &QueueSelection::Occurrences(vec![moving.0]),
            &QueueReorderTarget::Before(3),
        );
        s.shuffle(false, 0);
        assert_eq!(s.entry_at(s.selected_index().unwrap()), Some(&current));
        assert_eq!(
            s.entry_at(s.index_of(prepared_id).unwrap()),
            Some(&prepared)
        );
        assert_eq!(s.progress_millis(), 42000);
        let restored = Sequence::from_window(s.snapshot(), s.revision(), s.next_id()).unwrap();
        assert_eq!(restored.order, s.order);
        assert_eq!(restored.members, s.members);
        assert_eq!(
            restored.entry_at(restored.selected_index().unwrap()),
            Some(&current)
        );
        let next_id = s.next_id();
        s.add_page(page(0, 2), QueueReorderTarget::End, true);
        assert!(s.order.iter().all(|id| id.0 >= next_id));
    }
}
