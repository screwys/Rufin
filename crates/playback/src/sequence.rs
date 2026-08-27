//! Owns Playback's one compact occurrence order and traversal behavior.
//! Canonical Queue rows and media facts remain in Library SQLite.

use std::fmt;
use std::sync::Arc;

use library::{SourceKey, TrackKey};
use thiserror::Error;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OccurrenceId(String);

impl OccurrenceId {
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        assert!(!value.is_empty(), "OccurrenceId cannot be empty");
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for OccurrenceId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for OccurrenceId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl fmt::Display for OccurrenceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QueueReorderTarget {
    Before(OccurrenceId),
    After(OccurrenceId),
    End,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub enum RepeatMode {
    #[default]
    Off,
    One,
    All,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Provenance {
    Context {
        context_id: Arc<str>,
        source_rank: usize,
    },
    Manual,
    Random,
    Radio,
    AutoDj,
    Legacy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SequenceEntry {
    pub occurrence: OccurrenceId,
    pub track_key: Option<TrackKey>,
    pub canonical_position: usize,
    pub provenance: Provenance,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchItem {
    pub track_key: TrackKey,
    pub provenance: Provenance,
}

impl BatchItem {
    pub fn new(track_key: TrackKey, provenance: Provenance) -> Self {
        Self {
            track_key,
            provenance,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Batch {
    items: Vec<BatchItem>,
    shuffle_seed: u64,
    random_start: bool,
}

impl Batch {
    pub fn new(items: Vec<BatchItem>) -> Self {
        Self {
            items,
            shuffle_seed: 0,
            random_start: false,
        }
    }

    pub fn with_shuffle_intent(mut self, seed: u64, random_start: bool) -> Self {
        self.shuffle_seed = seed;
        self.random_start = random_start;
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BatchChange {
    pub rows_changed: bool,
    pub traversal_changed: bool,
}

impl BatchChange {
    pub fn durable_changed(self) -> bool {
        self.rows_changed || self.traversal_changed
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Placement {
    Replace { anchor_index: usize },
    AfterCurrent,
    End,
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
    source_key: SourceKey,
    entries: Vec<SequenceEntry>,
    selected_index: Option<usize>,
    repeat_mode: RepeatMode,
    shuffle_enabled: bool,
    revision: u64,
    progress_millis: u64,
    next_occurrence_number: u64,
}

impl Sequence {
    pub fn new(source_key: SourceKey) -> Self {
        Self {
            source_key,
            entries: Vec::new(),
            selected_index: None,
            repeat_mode: RepeatMode::Off,
            shuffle_enabled: false,
            revision: 0,
            progress_millis: 0,
            next_occurrence_number: 1,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn restore(
        source_key: SourceKey,
        entries: Vec<SequenceEntry>,
        selected: Option<OccurrenceId>,
        repeat_mode: RepeatMode,
        shuffled: bool,
        revision: u64,
        progress_millis: u64,
    ) -> Result<Self, SequenceError> {
        let selected_index = selected
            .as_ref()
            .map(|selected| {
                entries
                    .iter()
                    .position(|entry| &entry.occurrence == selected)
                    .ok_or(SequenceError::MissingSelectedOccurrence)
            })
            .transpose()?;
        let next_occurrence_number = entries
            .iter()
            .filter_map(|entry| entry.occurrence.as_str().rsplit_once(':'))
            .filter_map(|(_, number)| number.parse::<u64>().ok())
            .max()
            .unwrap_or_default()
            .saturating_add(1);
        Ok(Self {
            source_key,
            entries,
            selected_index,
            repeat_mode,
            shuffle_enabled: shuffled,
            revision,
            progress_millis,
            next_occurrence_number,
        })
    }

    pub const fn source_key(&self) -> SourceKey {
        self.source_key
    }

    pub fn entries(&self) -> &[SequenceEntry] {
        &self.entries
    }

    pub fn selected(&self) -> Option<&SequenceEntry> {
        self.selected_index
            .and_then(|index| self.entries.get(index))
    }

    pub const fn selected_index(&self) -> Option<usize> {
        self.selected_index
    }

    pub fn occurrence(&self, occurrence: &OccurrenceId) -> Option<&SequenceEntry> {
        self.entries
            .iter()
            .find(|entry| &entry.occurrence == occurrence)
    }

    pub fn occurrence_index(&self, occurrence: &OccurrenceId) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| &entry.occurrence == occurrence)
    }

    pub fn context_index(
        &self,
        context_id: &str,
        track_key: TrackKey,
        source_rank: usize,
    ) -> Option<usize> {
        self.entries.iter().position(|entry| {
            entry.track_key == Some(track_key)
                && matches!(&entry.provenance, Provenance::Context { context_id: current, source_rank: rank } if current.as_ref() == context_id && *rank == source_rank)
        })
    }

    pub fn contains_track(&self, track_key: &TrackKey) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.track_key.as_ref() == Some(track_key))
    }

    pub const fn repeat_mode(&self) -> RepeatMode {
        self.repeat_mode
    }

    pub const fn shuffle_enabled(&self) -> bool {
        self.shuffle_enabled
    }

    pub fn traversal(&self) -> impl ExactSizeIterator<Item = &OccurrenceId> {
        self.entries.iter().map(|entry| &entry.occurrence)
    }

    pub const fn revision(&self) -> u64 {
        self.revision
    }

    pub const fn progress_millis(&self) -> u64 {
        self.progress_millis
    }

    pub fn set_progress_millis(&mut self, progress_millis: u64) {
        self.progress_millis = progress_millis;
    }

    pub fn remaining_after_selected(&self) -> usize {
        self.selected_index.map_or(self.entries.len(), |index| {
            self.entries.len().saturating_sub(index + 1)
        })
    }

    pub fn peek_next_eos(&self) -> Option<&SequenceEntry> {
        self.next_index(true)
            .and_then(|index| self.entries.get(index))
    }

    pub(crate) fn next_index_eos(&self) -> Option<usize> {
        self.next_index(true)
    }

    pub fn upcoming(&self, limit: usize) -> Vec<&SequenceEntry> {
        let Some(selected) = self.selected_index else {
            return self.entries.iter().take(limit).collect();
        };
        self.entries.iter().skip(selected + 1).take(limit).collect()
    }

    pub fn apply_batch(
        &mut self,
        batch: Batch,
        placement: Placement,
    ) -> Result<bool, SequenceError> {
        self.apply_batch_with_change(batch, placement)
            .map(BatchChange::durable_changed)
    }

    pub(crate) fn apply_batch_with_change(
        &mut self,
        batch: Batch,
        placement: Placement,
    ) -> Result<BatchChange, SequenceError> {
        if batch.items.is_empty() {
            return Err(SequenceError::EmptyBatch);
        }
        if let Placement::Replace { anchor_index } = placement
            && anchor_index >= batch.items.len()
        {
            return Err(SequenceError::InvalidAnchor);
        }
        let previous = self.selected().map(|entry| entry.occurrence.clone());
        let random_start = batch.random_start;
        let shuffle_seed = batch.shuffle_seed;
        let item_count = batch.items.len();
        let canonical_start = match placement {
            Placement::Replace { .. } => 0,
            Placement::AfterCurrent => self
                .selected()
                .map_or(self.entries.len(), |entry| entry.canonical_position + 1),
            Placement::End => self.entries.len(),
        };
        if !matches!(placement, Placement::Replace { .. }) {
            for entry in &mut self.entries {
                if entry.canonical_position >= canonical_start {
                    entry.canonical_position += item_count;
                }
            }
        }
        let mut inserted = batch
            .items
            .into_iter()
            .enumerate()
            .map(|(offset, item)| SequenceEntry {
                occurrence: self.next_occurrence(),
                track_key: Some(item.track_key),
                canonical_position: canonical_start + offset,
                provenance: item.provenance,
            })
            .collect::<Vec<_>>();
        let mut selected_occurrence = match placement {
            Placement::Replace { anchor_index } => {
                let selected = inserted[anchor_index].occurrence.clone();
                *&mut self.entries = inserted;
                Some(selected)
            }
            Placement::AfterCurrent => {
                let at = self
                    .selected_index
                    .map_or(self.entries.len(), |index| index + 1);
                self.entries.splice(at..at, inserted.drain(..));
                previous
            }
            Placement::End => {
                self.entries.append(&mut inserted);
                previous
            }
        };
        if random_start {
            self.shuffle(shuffle_seed, None);
            selected_occurrence = self.entries.first().map(|entry| entry.occurrence.clone());
        } else if self.shuffle_enabled {
            self.shuffle(shuffle_seed, selected_occurrence.as_ref());
        }
        self.selected_index = selected_occurrence
            .as_ref()
            .and_then(|occurrence| self.occurrence_index(occurrence));
        self.progress_millis = 0;
        self.bump_revision();
        Ok(BatchChange {
            rows_changed: true,
            traversal_changed: self.shuffle_enabled || random_start,
        })
    }

    pub fn activate(&mut self, occurrence: &OccurrenceId) -> bool {
        self.occurrence_index(occurrence)
            .is_some_and(|index| self.activate_index(index))
    }

    pub(crate) fn activate_index(&mut self, index: usize) -> bool {
        if index >= self.entries.len() || self.selected_index == Some(index) {
            return false;
        }
        self.selected_index = Some(index);
        self.progress_millis = 0;
        true
    }

    pub(crate) fn activate_backend(&mut self, occurrence: &OccurrenceId) -> bool {
        let Some(index) = self.occurrence_index(occurrence) else {
            return false;
        };
        self.selected_index = Some(index);
        self.progress_millis = 0;
        true
    }

    pub fn remove(&mut self, occurrence: &OccurrenceId) -> Option<SequenceEntry> {
        let index = self.occurrence_index(occurrence)?;
        let selected = self.selected_index;
        let removed = self.entries.remove(index);
        for entry in &mut self.entries {
            if entry.canonical_position > removed.canonical_position {
                entry.canonical_position -= 1;
            }
        }
        self.selected_index = match selected {
            Some(selected) if selected == index => {
                self.progress_millis = 0;
                if index < self.entries.len() {
                    Some(index)
                } else if self.repeat_mode == RepeatMode::All && !self.entries.is_empty() {
                    Some(0)
                } else {
                    None
                }
            }
            Some(selected) if selected > index => Some(selected - 1),
            selected => selected,
        };
        self.bump_revision();
        Some(removed)
    }

    pub fn reorder(&mut self, occurrence: &OccurrenceId, target: &QueueReorderTarget) -> bool {
        let Some(old_position) = self
            .occurrence(occurrence)
            .map(|entry| entry.canonical_position)
        else {
            return false;
        };
        let insertion = match target {
            QueueReorderTarget::Before(target) => self
                .occurrence(target)
                .map(|entry| entry.canonical_position),
            QueueReorderTarget::After(target) => self
                .occurrence(target)
                .map(|entry| entry.canonical_position.saturating_add(1)),
            QueueReorderTarget::End => Some(self.entries.len()),
        };
        let Some(mut target) = insertion else {
            return false;
        };
        if old_position < target {
            target = target.saturating_sub(1);
        }
        let target = target.min(self.entries.len().saturating_sub(1));
        if old_position == target {
            return false;
        }
        let selected = self.selected().map(|entry| entry.occurrence.clone());
        self.set_canonical_position(occurrence, old_position, target);
        if !self.shuffle_enabled {
            self.entries.sort_by_key(|entry| entry.canonical_position);
        }
        self.selected_index = selected
            .as_ref()
            .and_then(|selected| self.occurrence_index(selected));
        self.bump_revision();
        true
    }

    fn set_canonical_position(
        &mut self,
        occurrence: &OccurrenceId,
        old_position: usize,
        target: usize,
    ) {
        for entry in &mut self.entries {
            if &entry.occurrence == occurrence {
                entry.canonical_position = target;
            } else if target < old_position
                && (target..old_position).contains(&entry.canonical_position)
            {
                entry.canonical_position += 1;
            } else if old_position < target
                && (old_position + 1..=target).contains(&entry.canonical_position)
            {
                entry.canonical_position -= 1;
            }
        }
    }

    pub fn move_after_current(&mut self, occurrence: &OccurrenceId) -> bool {
        let Some(current_index) = self.selected_index else {
            return false;
        };
        let current = self.entries[current_index].occurrence.clone();
        if &current == occurrence {
            return false;
        }
        let Some(old_index) = self.occurrence_index(occurrence) else {
            return false;
        };
        let old_position = self.entries[old_index].canonical_position;
        let current_position = self.entries[current_index].canonical_position;
        let mut canonical_target = current_position.saturating_add(1);
        if old_position < canonical_target {
            canonical_target = canonical_target.saturating_sub(1);
        }
        let traversal_target = if old_index < current_index {
            current_index
        } else {
            current_index.saturating_add(1)
        };
        let canonical_changed = old_position != canonical_target;
        let traversal_changed = old_index != traversal_target;
        if !canonical_changed && !traversal_changed {
            return false;
        }
        if canonical_changed {
            self.set_canonical_position(occurrence, old_position, canonical_target);
        }
        if traversal_changed {
            let entry = self.entries.remove(old_index);
            let current_index =
                current_index.saturating_sub(usize::from(old_index < current_index));
            self.entries.insert(current_index + 1, entry);
            self.selected_index = Some(current_index);
        } else {
            self.selected_index = Some(current_index);
        }
        self.bump_revision();
        true
    }

    pub(crate) fn clear(&mut self) -> bool {
        if self.entries.is_empty() {
            return false;
        }
        self.entries.clear();
        self.selected_index = None;
        self.progress_millis = 0;
        self.bump_revision();
        true
    }

    pub(crate) fn clear_upcoming(&mut self) -> bool {
        let keep = self.selected_index.map_or(0, |index| index + 1);
        if keep >= self.entries.len() {
            return false;
        }
        while self.entries.len() > keep {
            let removed = &mut self.entries.pop().expect("upcoming entry");
            for entry in &mut self.entries {
                if entry.canonical_position > removed.canonical_position {
                    entry.canonical_position -= 1;
                }
            }
        }
        self.bump_revision();
        true
    }

    pub fn trim_auto_dj_history(&mut self, keep: usize) -> bool {
        let Some(selected) = self.selected_index else {
            return false;
        };
        let mut auto_dj_before = self.entries[..selected]
            .iter()
            .filter(|entry| entry.provenance == Provenance::AutoDj)
            .count();
        if auto_dj_before <= keep {
            return false;
        }
        let mut index = 0;
        let mut selected = selected;
        while index < selected && auto_dj_before > keep {
            if self.entries[index].provenance == Provenance::AutoDj {
                let removed = self.entries.remove(index);
                for entry in &mut self.entries {
                    if entry.canonical_position > removed.canonical_position {
                        entry.canonical_position -= 1;
                    }
                }
                selected -= 1;
                auto_dj_before -= 1;
            } else {
                index += 1;
            }
        }
        self.selected_index = Some(selected);
        self.bump_revision();
        true
    }

    pub fn advance_manual(&mut self) -> Option<&SequenceEntry> {
        self.advance(false)
    }

    pub fn advance_eos(&mut self) -> Option<&SequenceEntry> {
        self.advance(true)
    }

    pub fn previous(&mut self) -> Option<&SequenceEntry> {
        let current = self.selected_index?;
        let previous = if current == 0 && self.repeat_mode == RepeatMode::All {
            self.entries.len().checked_sub(1)?
        } else {
            current.checked_sub(1)?
        };
        self.selected_index = Some(previous);
        self.progress_millis = 0;
        self.entries.get(previous)
    }

    pub fn peek_previous(&self) -> Option<&SequenceEntry> {
        let current = self.selected_index?;
        let previous = if current == 0 && self.repeat_mode == RepeatMode::All {
            self.entries.len().checked_sub(1)?
        } else {
            current.checked_sub(1)?
        };
        self.entries.get(previous)
    }

    pub fn set_repeat_mode(&mut self, repeat_mode: RepeatMode) {
        self.repeat_mode = repeat_mode;
    }

    pub fn set_shuffle_seed(&mut self, enabled: bool, seed: u64) -> bool {
        if self.shuffle_enabled == enabled {
            return false;
        }
        let selected = self.selected().map(|entry| entry.occurrence.clone());
        if enabled {
            self.shuffle(seed, selected.as_ref());
        } else {
            self.entries.sort_by_key(|entry| entry.canonical_position);
        }
        self.selected_index = selected
            .as_ref()
            .and_then(|selected| self.occurrence_index(selected));
        self.shuffle_enabled = enabled;
        self.bump_revision();
        true
    }

    fn advance(&mut self, eos: bool) -> Option<&SequenceEntry> {
        let index = self.next_index(eos)?;
        self.selected_index = Some(index);
        self.progress_millis = 0;
        self.entries.get(index)
    }

    fn next_index(&self, eos: bool) -> Option<usize> {
        let current = self.selected_index?;
        if eos && self.repeat_mode == RepeatMode::One {
            return Some(current);
        }
        if current + 1 < self.entries.len() {
            return Some(current + 1);
        }
        (self.repeat_mode == RepeatMode::All && !self.entries.is_empty()).then_some(0)
    }

    fn shuffle(&mut self, mut seed: u64, pinned: Option<&OccurrenceId>) {
        let entries = &mut self.entries;
        for index in (1..entries.len()).rev() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            entries.swap(index, seed as usize % (index + 1));
        }
        if let Some(pinned) = pinned
            && let Some(index) = entries.iter().position(|entry| &entry.occurrence == pinned)
        {
            entries.swap(0, index);
        }
    }

    fn next_occurrence(&mut self) -> OccurrenceId {
        let occurrence = OccurrenceId::new(format!(
            "{}:{}",
            self.source_key.raw(),
            self.next_occurrence_number
        ));
        self.next_occurrence_number = self.next_occurrence_number.wrapping_add(1).max(1);
        occurrence
    }

    fn bump_revision(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(number: i64) -> TrackKey {
        TrackKey::from_raw(number)
    }

    fn source(number: i64) -> SourceKey {
        SourceKey::from_raw(number)
    }

    fn manual(number: i64) -> BatchItem {
        BatchItem::new(track(number), Provenance::Manual)
    }

    fn occurrence_for_track(sequence: &Sequence, number: i64) -> OccurrenceId {
        sequence
            .entries()
            .iter()
            .find(|entry| entry.track_key == Some(track(number)))
            .unwrap_or_else(|| panic!("canonical Track {number}"))
            .occurrence
            .clone()
    }

    fn canonical_tracks(sequence: &Sequence) -> Vec<Option<TrackKey>> {
        let mut canonical = sequence
            .entries()
            .iter()
            .map(|entry| (entry.canonical_position, entry.track_key))
            .collect::<Vec<_>>();
        canonical.sort_by_key(|(position, _)| *position);
        canonical.into_iter().map(|(_, track)| track).collect()
    }

    #[test]
    fn backend_may_reinstall_the_current_occurrence_but_user_activation_is_a_no_op() {
        let mut sequence = Sequence::new(source(1));
        sequence
            .apply_batch(
                Batch::new(vec![manual(1)]),
                Placement::Replace { anchor_index: 0 },
            )
            .expect("queue Track");
        let current = sequence.selected().expect("selected").occurrence.clone();
        sequence.set_progress_millis(1_000);

        assert!(!sequence.activate(&current));
        assert_eq!(sequence.progress_millis(), 1_000);
        assert!(sequence.activate_backend(&current));
        assert_eq!(sequence.progress_millis(), 0);
    }

    #[test]
    fn repeat_all_wraps_previous_and_the_successor_after_removing_the_last_occurrence() {
        let mut sequence = Sequence::new(source(1));
        sequence
            .apply_batch(
                Batch::new(vec![manual(1), manual(2), manual(3)]),
                Placement::Replace { anchor_index: 0 },
            )
            .expect("queue Tracks");
        sequence.set_repeat_mode(RepeatMode::All);
        assert_eq!(
            sequence.previous().expect("wrapped").track_key,
            Some(track(3))
        );
        let selected = sequence.selected().expect("selected").occurrence.clone();
        sequence.remove(&selected).expect("remove selected");
        assert_eq!(
            sequence.selected().expect("wrapped successor").track_key,
            Some(track(1))
        );
    }

    #[test]
    fn shuffled_insertions_keep_canonical_order_for_unshuffle() {
        let mut sequence = Sequence::new(source(1));
        sequence
            .apply_batch(
                Batch::new(vec![manual(1), manual(2), manual(3)]),
                Placement::Replace { anchor_index: 1 },
            )
            .expect("queue Tracks");
        sequence.set_shuffle_seed(true, 19);
        sequence
            .apply_batch(Batch::new(vec![manual(4)]), Placement::AfterCurrent)
            .expect("insert while shuffled");
        sequence.set_shuffle_seed(false, 0);

        assert_eq!(
            sequence
                .entries()
                .iter()
                .map(|entry| entry.track_key)
                .collect::<Vec<_>>(),
            [
                Some(track(1)),
                Some(track(2)),
                Some(track(4)),
                Some(track(3))
            ]
        );
    }

    #[test]
    fn canonical_reorder_preserves_the_active_shuffled_traversal() {
        let mut sequence = Sequence::new(source(1));
        sequence
            .apply_batch(
                Batch::new(vec![manual(1), manual(2), manual(3), manual(4)]),
                Placement::Replace { anchor_index: 0 },
            )
            .expect("queue Tracks");
        sequence.set_shuffle_seed(true, 19);
        let traversal = sequence
            .entries()
            .iter()
            .map(|entry| entry.occurrence.clone())
            .collect::<Vec<_>>();
        let occurrence = occurrence_for_track(&sequence, 4);
        let target = occurrence_for_track(&sequence, 1);
        assert!(sequence.reorder(&occurrence, &QueueReorderTarget::Before(target)));
        assert_eq!(
            sequence
                .entries()
                .iter()
                .map(|entry| entry.occurrence.clone())
                .collect::<Vec<_>>(),
            traversal
        );
        assert_eq!(
            canonical_tracks(&sequence),
            [
                Some(track(4)),
                Some(track(1)),
                Some(track(2)),
                Some(track(3)),
            ]
        );
    }

    #[test]
    fn relative_reorder_targets_cover_both_directions_and_the_end() {
        let mut sequence = Sequence::new(source(1));
        sequence
            .apply_batch(
                Batch::new(vec![manual(1), manual(2), manual(3), manual(4)]),
                Placement::Replace { anchor_index: 0 },
            )
            .expect("queue Tracks");
        let one = occurrence_for_track(&sequence, 1);
        let two = occurrence_for_track(&sequence, 2);
        let three = occurrence_for_track(&sequence, 3);
        let four = occurrence_for_track(&sequence, 4);

        assert!(sequence.reorder(&four, &QueueReorderTarget::Before(two)));
        assert_eq!(
            canonical_tracks(&sequence),
            [
                Some(track(1)),
                Some(track(4)),
                Some(track(2)),
                Some(track(3)),
            ]
        );
        assert!(sequence.reorder(&four, &QueueReorderTarget::After(three)));
        assert_eq!(
            canonical_tracks(&sequence),
            [
                Some(track(1)),
                Some(track(2)),
                Some(track(3)),
                Some(track(4)),
            ]
        );
        assert!(sequence.reorder(&one, &QueueReorderTarget::End));
        assert_eq!(
            canonical_tracks(&sequence),
            [
                Some(track(2)),
                Some(track(3)),
                Some(track(4)),
                Some(track(1)),
            ]
        );
    }

    #[test]
    fn move_after_current_updates_canonical_and_shuffled_traversal_order() {
        let mut sequence = Sequence::new(source(1));
        sequence
            .apply_batch(
                Batch::new(vec![manual(1), manual(2), manual(3), manual(4)]),
                Placement::Replace { anchor_index: 0 },
            )
            .expect("queue Tracks");
        sequence.set_shuffle_seed(true, 19);
        let current = sequence
            .selected()
            .expect("current Track")
            .occurrence
            .clone();
        let occurrence = occurrence_for_track(&sequence, 4);

        assert!(sequence.move_after_current(&occurrence));
        assert_eq!(
            sequence
                .entries()
                .iter()
                .take(2)
                .map(|entry| entry.occurrence.clone())
                .collect::<Vec<_>>(),
            [current, occurrence]
        );
        assert_eq!(
            canonical_tracks(&sequence),
            [
                Some(track(1)),
                Some(track(4)),
                Some(track(2)),
                Some(track(3)),
            ]
        );
    }

    #[test]
    fn random_start_selects_the_first_shuffled_occurrence() {
        let mut sequence = Sequence::new(source(1));
        sequence
            .apply_batch(
                Batch::new(vec![manual(1), manual(2), manual(3), manual(4)])
                    .with_shuffle_intent(2, true),
                Placement::Replace { anchor_index: 0 },
            )
            .expect("random-start collection");

        assert_eq!(sequence.selected_index(), Some(0));
        assert_ne!(
            sequence.selected().and_then(|entry| entry.track_key),
            Some(track(1))
        );
        let mut canonical = sequence
            .entries()
            .iter()
            .map(|entry| (entry.canonical_position, entry.track_key))
            .collect::<Vec<_>>();
        canonical.sort_by_key(|(position, _)| *position);
        assert_eq!(
            canonical
                .into_iter()
                .map(|(_, track)| track)
                .collect::<Vec<_>>(),
            [
                Some(track(1)),
                Some(track(2)),
                Some(track(3)),
                Some(track(4))
            ]
        );
    }

    #[test]
    fn auto_dj_history_trimming_preserves_other_provenance() {
        let source = source(1);
        let entries = vec![
            SequenceEntry {
                occurrence: "one".into(),
                track_key: Some(track(1)),
                canonical_position: 0,
                provenance: Provenance::Manual,
            },
            SequenceEntry {
                occurrence: "two".into(),
                track_key: Some(track(2)),
                canonical_position: 1,
                provenance: Provenance::AutoDj,
            },
            SequenceEntry {
                occurrence: "three".into(),
                track_key: Some(track(3)),
                canonical_position: 2,
                provenance: Provenance::Radio,
            },
            SequenceEntry {
                occurrence: "four".into(),
                track_key: Some(track(4)),
                canonical_position: 3,
                provenance: Provenance::AutoDj,
            },
            SequenceEntry {
                occurrence: "five".into(),
                track_key: Some(track(5)),
                canonical_position: 4,
                provenance: Provenance::AutoDj,
            },
            SequenceEntry {
                occurrence: "current".into(),
                track_key: Some(track(6)),
                canonical_position: 5,
                provenance: Provenance::AutoDj,
            },
        ];
        let mut sequence = Sequence::restore(
            source,
            entries,
            Some("current".into()),
            RepeatMode::Off,
            false,
            0,
            0,
        )
        .expect("restore Queue");

        assert!(sequence.trim_auto_dj_history(1));
        assert_eq!(
            sequence
                .entries()
                .iter()
                .map(|entry| entry.provenance.clone())
                .collect::<Vec<_>>(),
            [
                Provenance::Manual,
                Provenance::Radio,
                Provenance::AutoDj,
                Provenance::AutoDj,
            ]
        );
    }
}
