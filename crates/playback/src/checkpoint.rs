//! Describes asynchronous Queue persistence without copying media metadata.
//! The shared compact occurrence order is the same allocation Playback traverses.

use std::sync::Arc;

use library::SourceKey;

use crate::{OccurrenceId, RepeatMode, Sequence, SequenceEntry};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct QueuePersistenceChange {
    pub expected_revision: u64,
    pub rows_changed: bool,
}

#[derive(Clone, Debug)]
pub struct QueuePersistence {
    source_key: SourceKey,
    revision: u64,
    entries: Arc<Vec<SequenceEntry>>,
    current: Option<OccurrenceId>,
    progress_millis: u64,
    repeat_mode: RepeatMode,
    shuffled: bool,
    rows_changed: bool,
}

impl QueuePersistence {
    pub(crate) fn capture(sequence: &Sequence, change: QueuePersistenceChange) -> Self {
        Self {
            source_key: sequence.source_key(),
            revision: sequence.revision(),
            entries: sequence.shared_entries(),
            current: sequence.selected().map(|entry| entry.occurrence.clone()),
            progress_millis: sequence.progress_millis(),
            repeat_mode: sequence.repeat_mode(),
            shuffled: sequence.shuffle_enabled(),
            rows_changed: change.rows_changed,
        }
    }

    pub fn coalesce(&mut self, newer: Self) {
        if self.source_key == newer.source_key && newer.revision >= self.revision {
            *self = newer;
        }
    }

    pub const fn source_key(&self) -> SourceKey {
        self.source_key
    }

    pub const fn revision(&self) -> u64 {
        self.revision
    }

    pub fn entries(&self) -> &[SequenceEntry] {
        &self.entries
    }

    pub fn current(&self) -> Option<&OccurrenceId> {
        self.current.as_ref()
    }

    pub const fn progress_millis(&self) -> u64 {
        self.progress_millis
    }

    pub const fn repeat_mode(&self) -> RepeatMode {
        self.repeat_mode
    }

    pub const fn shuffled(&self) -> bool {
        self.shuffled
    }

    pub const fn rows_changed(&self) -> bool {
        self.rows_changed
    }
}
