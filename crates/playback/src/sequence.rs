//! Complete lightweight playback membership, order, and nearby resolved metadata.
use crate::{Provenance, QueueItem, QueueRow, RepeatMode};
pub use library::{QueuePlacement as Placement, QueueReorderTarget};
use std::sync::{Arc, OnceLock};
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
    pub(crate) context_title: Option<Arc<library::QueueContextTitle>>,
    pub(crate) shuffle_seed: u64,
    pub(crate) random_start: bool,
    pub(crate) shuffled: bool,
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

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum SequenceError {
    #[error("the selected playback entry is missing")]
    MissingSelectedEntry,
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
    rows: Vec<Arc<QueueRow>>,
    selected_index: Option<usize>,
    repeat_mode: RepeatMode,
    shuffle_enabled: bool,
    revision: u64,
    content_id: OnceLock<String>,
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
            content_id: OnceLock::new(),
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
            rows: window.rows,
            selected_index: window.current_index,
            repeat_mode: window.repeat_mode,
            shuffle_enabled: window.shuffled,
            revision,
            content_id: OnceLock::new(),
            progress_millis: window.progress_millis.max(0) as u64,
            dirty: None,
            window_dirty: true,
        };
        if sequence
            .selected_index
            .is_some_and(|i| sequence.entry_at(i).is_none())
        {
            return Err(SequenceError::MissingSelectedEntry);
        }
        Ok(sequence)
    }
    pub(crate) fn snapshot(&self) -> library::QueueRestore {
        library::QueueRestore {
            entries: self.members.clone(),
            order: self.order.clone(),
            rows: self.rows.clone(),
            current_index: self.selected_index,
            progress_millis: self.progress_millis as i64,
            repeat_mode: self.repeat_mode,
            shuffled: self.shuffle_enabled,
        }
    }
    fn changed(&mut self, kind: QueuePersistenceKind) {
        self.revision += 1;
        if kind != QueuePersistenceKind::State {
            self.content_id.take();
        }
        self.window_dirty |= kind != QueuePersistenceKind::State;
        self.dirty = Some(self.dirty.map_or(kind, |old| old.max(kind)));
    }
    pub(crate) fn take_persistence(&mut self) -> Option<QueuePersistenceKind> {
        self.dirty.take()
    }
    pub(crate) fn entry_at(&self, index: usize) -> Option<&library::QueueEntry> {
        self.members.get(*self.order.get(index)? as usize)
    }
    pub fn entries(&self) -> &[Arc<QueueRow>] {
        &self.rows
    }
    pub fn total(&self) -> usize {
        self.order.len()
    }
    pub fn at(&self, index: usize) -> Option<&Arc<QueueRow>> {
        self.rows.iter().find(|row| row.position == index)
    }
    pub fn selected(&self) -> Option<&Arc<QueueRow>> {
        self.at(self.selected_index?)
    }
    pub fn selected_index(&self) -> Option<usize> {
        self.selected_index
    }
    pub fn context_index(&self, context_id: &str, uri: &str, rank: usize) -> Option<usize> {
        self.order.iter().position(|&i| { let e = &self.members[i as usize];
            e.media_uri.as_ref() == uri && matches!(&e.provenance, Provenance::Context { context_id: c, source_rank, .. } if c.as_ref() == context_id && *source_rank == rank) })
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
    pub fn content_id(&self) -> &str {
        self.content_id
            .get_or_init(|| library::queue_content_id(&self.members, &self.order))
    }
    pub fn progress_millis(&self) -> u64 {
        self.progress_millis
    }
    pub fn set_progress_millis(&mut self, value: u64) {
        self.progress_millis = value;
    }
    pub(crate) fn set_selected_duration(&mut self, millis: u64) {
        let selected = self.selected_index;
        if let Some(row) = self
            .rows
            .iter_mut()
            .find(|row| Some(row.position) == selected)
        {
            Arc::make_mut(row).item.duration_millis = millis.min(i64::MAX as u64) as i64;
        }
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
    pub fn peek_next_eos(&self) -> Option<&Arc<QueueRow>> {
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
    pub fn peek_previous(&self) -> Option<&Arc<QueueRow>> {
        self.at(self.previous_index()?)
    }
    pub fn upcoming(&self, limit: usize) -> Vec<&Arc<QueueRow>> {
        let start = self.selected_index.map_or(0, |i| i + 1);
        (start..self.total())
            .take(limit)
            .filter_map(|i| self.at(i))
            .collect()
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
    pub(crate) fn activate_backend(&mut self, index: usize) -> bool {
        if index >= self.total() {
            return false;
        }
        self.selected_index = Some(index);
        self.progress_millis = 0;
        self.window_dirty = true;
        true
    }
    pub(crate) fn start_next_pass(&mut self) {
        let old = self.order.clone();
        let order = Arc::make_mut(&mut self.order);
        for (i, entry) in order.iter_mut().enumerate() {
            *entry = i as u32;
        }
        if self.shuffle_enabled {
            library::shuffle_order(order, self.revision.wrapping_add(1));
        }
        let positions = inverse_order(order);
        self.remap_positions(|position| Some(positions[old[position] as usize]), &mut []);
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
    pub fn advance_manual(&mut self) -> Option<&Arc<QueueRow>> {
        let i = self.advance_index(false)?;
        self.at(i)
    }
    pub fn advance_eos(&mut self) -> Option<&Arc<QueueRow>> {
        let i = self.advance_index(true)?;
        self.at(i)
    }
    pub fn previous(&mut self) -> Option<&Arc<QueueRow>> {
        let i = self.previous_index()?;
        self.activate_index(i);
        self.at(i)
    }
    fn remap_positions(
        &mut self,
        map: impl Fn(usize) -> Option<usize>,
        pending: &mut [Option<usize>],
    ) {
        self.selected_index = self.selected_index.and_then(&map);
        for position in pending {
            *position = position.and_then(&map);
        }
        self.rows.retain_mut(|row| {
            let Some(position) = map(row.position) else {
                return false;
            };
            if row.position != position {
                Arc::make_mut(row).position = position;
            }
            true
        });
    }
    pub(crate) fn shuffle(&mut self, enabled: bool, seed: u64, pending: &mut [Option<usize>]) {
        if self.shuffle_enabled == enabled {
            return;
        }
        self.shuffle_enabled = enabled;
        let old = self.order.clone();
        let start = self.selected_index.map_or(0, |i| i + 1);
        let remaining = &mut Arc::make_mut(&mut self.order)[start..];
        if enabled {
            library::shuffle_order(remaining, seed);
        } else {
            remaining.sort_unstable();
        }
        let positions = inverse_order(&self.order);
        self.remap_positions(|position| Some(positions[old[position] as usize]), pending);
        self.changed(QueuePersistenceKind::Order);
    }
    pub(crate) fn random_start(&mut self, seed: u64) {
        self.shuffle_enabled = true;
        let old = self.order.clone();
        if let Some(selected) = self.selected_index {
            let order = Arc::make_mut(&mut self.order);
            order.swap(0, selected);
            library::shuffle_order(&mut order[1..], seed);
        }
        let positions = inverse_order(&self.order);
        self.remap_positions(|position| Some(positions[old[position] as usize]), &mut []);
        self.selected_index = (!self.order.is_empty()).then_some(0);
        self.trim_rows();
        self.content_id.take();
    }
    pub(crate) fn add_page(
        &mut self,
        page: library::QueueReadPage,
        target: QueueReorderTarget,
        replacing: bool,
        seed: Option<u64>,
        pending: &mut [Option<usize>],
    ) {
        if replacing {
            self.members = page.entries.into();
            self.order = (0..self.members.len() as u32).collect::<Vec<_>>().into();
            self.rows = page.rows;
            self.selected_index = (!self.members.is_empty()).then_some(page.current_index);
            pending.fill(None);
            self.progress_millis = 0;
            self.shuffle_enabled = false;
            if let Some(seed) = seed {
                self.shuffle(true, seed, pending);
            }
        } else {
            let at = self.target(&target);
            let canonical_at = match target {
                QueueReorderTarget::Before(position) => {
                    self.order.get(position).map(|i| *i as usize)
                }
                QueueReorderTarget::After(position) => {
                    self.order.get(position).map(|i| *i as usize + 1)
                }
                QueueReorderTarget::End => None,
            }
            .unwrap_or(self.members.len());
            let count = page.entries.len();
            self.remap_positions(
                |position| Some(position + if position >= at { count } else { 0 }),
                pending,
            );
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
            self.rows.extend(page.rows.into_iter().map(|mut row| {
                Arc::make_mut(&mut row).position += at;
                row
            }));
            if self.selected_index.is_none() && !self.order.is_empty() {
                self.selected_index = Some(at);
            }
        }
        self.trim_rows();
        self.changed(QueuePersistenceKind::Membership);
    }
    fn target(&self, target: &QueueReorderTarget) -> usize {
        match target {
            QueueReorderTarget::End => self.total(),
            QueueReorderTarget::Before(position) => (*position).min(self.total()),
            QueueReorderTarget::After(position) => position.saturating_add(1).min(self.total()),
        }
    }
    pub(crate) fn remove(&mut self, positions: &[usize], pending: &mut [Option<usize>]) {
        let mut removed = positions
            .iter()
            .copied()
            .filter(|i| *i < self.total())
            .collect::<Vec<_>>();
        removed.sort_unstable();
        removed.dedup();
        if removed.is_empty() {
            return;
        }
        let current = self.selected_index.unwrap_or(0);
        let members_to_remove = removed
            .iter()
            .map(|i| self.order[*i])
            .collect::<std::collections::HashSet<_>>();
        self.remap_positions(
            |position| {
                let before = removed.partition_point(|i| *i < position);
                (removed.get(before) != Some(&position)).then_some(position - before)
            },
            pending,
        );
        let mut remap = vec![None; self.members.len()];
        let members = self
            .members
            .iter()
            .enumerate()
            .filter(|(i, _)| !members_to_remove.contains(&(*i as u32)))
            .enumerate()
            .map(|(new, (old, member))| {
                remap[old] = Some(new as u32);
                member.clone()
            })
            .collect::<Vec<_>>();
        self.order = self
            .order
            .iter()
            .filter_map(|i| remap[*i as usize])
            .collect::<Vec<_>>()
            .into();
        self.members = members.into();
        if self.selected_index.is_none() && !self.order.is_empty() {
            self.selected_index =
                Some((current - removed.partition_point(|i| *i < current)).min(self.total() - 1));
            self.progress_millis = 0;
        }
        self.changed(QueuePersistenceKind::Membership);
    }
    pub(crate) fn reorder(
        &mut self,
        positions: &[usize],
        target: &QueueReorderTarget,
        pending: &mut [Option<usize>],
    ) {
        if matches!(target, QueueReorderTarget::Before(i) | QueueReorderTarget::After(i) if positions.contains(i))
        {
            return;
        }
        let mut moving = positions
            .iter()
            .copied()
            .filter(|i| *i < self.total())
            .collect::<Vec<_>>();
        moving.sort_unstable();
        moving.dedup();
        if moving.is_empty() {
            return;
        }
        let target_at = self.target(target);
        let at = target_at - moving.partition_point(|i| *i < target_at);
        let moving_set = moving
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        let mut display = (0..self.total())
            .filter(|i| !moving_set.contains(i))
            .collect::<Vec<_>>();
        display.splice(at..at, moving.iter().copied());
        let mut remap = vec![0; display.len()];
        for (new, old) in display.iter().enumerate() {
            remap[*old] = new;
        }
        self.remap_positions(|position| Some(remap[position]), pending);
        let moving = moving
            .into_iter()
            .map(|i| self.order[i])
            .collect::<Vec<_>>();
        let moving_set = moving
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        let target_member = match target {
            QueueReorderTarget::Before(i) | QueueReorderTarget::After(i) => {
                self.order.get(*i).copied()
            }
            QueueReorderTarget::End => None,
        };
        let mut ordinary = (0..self.members.len() as u32)
            .filter(|i| !moving_set.contains(i))
            .collect::<Vec<_>>();
        let at = target_member
            .and_then(|member| ordinary.iter().position(|i| *i == member))
            .map_or(ordinary.len(), |i| {
                i + usize::from(matches!(target, QueueReorderTarget::After(_)))
            });
        ordinary.splice(at..at, moving);
        let canonical_positions = inverse_order(&ordinary);
        let members = ordinary
            .iter()
            .map(|i| self.members[*i as usize].clone())
            .collect::<Vec<_>>();
        self.order = display
            .into_iter()
            .map(|i| canonical_positions[self.order[i] as usize] as u32)
            .collect::<Vec<_>>()
            .into();
        self.members = members.into();
        self.changed(QueuePersistenceKind::Membership);
    }
    pub(crate) fn clear(&mut self, include_current: bool, pending: &mut [Option<usize>]) {
        let selected = self.selected_index.filter(|_| !include_current);
        let member = selected.and_then(|i| self.entry_at(i).cloned());
        self.remap_positions(
            |position| (Some(position) == selected).then_some(0),
            pending,
        );
        self.members = member.into_iter().collect::<Vec<_>>().into();
        self.order = (0..self.members.len() as u32).collect::<Vec<_>>().into();
        self.trim_rows();
        self.changed(QueuePersistenceKind::Membership);
    }
    fn window_range(&self) -> std::ops::Range<usize> {
        let start = self.selected_index.unwrap_or(0).saturating_sub(10);
        start..(start + library::QUEUE_CONTEXT_LIMIT).min(self.total())
    }
    fn trim_rows(&mut self) {
        let range = self.window_range();
        self.rows.retain(|row| range.contains(&row.position));
        self.rows.sort_by_key(|row| row.position);
    }
    pub(crate) fn read_request(&mut self) -> Option<library::QueueReadRequest> {
        if !std::mem::take(&mut self.window_dirty) {
            return None;
        }
        self.trim_rows();
        let entries = self
            .window_range()
            .filter(|i| self.at(*i).is_none())
            .filter_map(|i| self.entry_at(i).cloned().map(|entry| (i, entry)))
            .collect::<Vec<_>>();
        (!entries.is_empty()).then_some(library::QueueReadRequest::Hydrate { entries })
    }
    pub(crate) fn hydrate(&mut self, page: library::QueueReadPage) {
        for row in page.rows {
            if self.at(row.position).is_none() {
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
fn inverse_order(order: &[u32]) -> Vec<usize> {
    let mut positions = vec![0; order.len()];
    for (position, member) in order.iter().enumerate() {
        positions[*member as usize] = position;
    }
    positions
}

impl Default for Sequence {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(start: usize, count: usize) -> library::QueueReadPage {
        let entries = (start..start + count)
            .map(|i| library::QueueEntry {
                received_from_connect: false,
                media_uri: format!("https://example.test/{}", i / 2).into(),
                playlist_entry_id: Some(format!("appearance:{i}").into()),
                provenance: Provenance::Manual,
            })
            .collect::<Vec<_>>();
        let rows = entries
            .iter()
            .take(100)
            .enumerate()
            .map(|(i, e)| {
                Arc::new(QueueRow {
                    position: i,
                    received_from_connect: false,
                    item: QueueItem::direct(e.media_uri.as_ref(), format!("{i}"), "", "", 1000),
                    playlist_entry_id: e.playlist_entry_id.as_deref().map(str::to_owned),
                    provenance: e.provenance.clone(),
                })
            })
            .collect();
        library::QueueReadPage {
            entries,
            rows,
            current_index: 0,
        }
    }
    fn sequence(count: usize) -> Sequence {
        let mut s = Sequence::new();
        s.add_page(page(0, count), QueueReorderTarget::End, true, None, &mut []);
        s.take_persistence();
        s
    }
    #[test]
    fn content_identity_tracks_queue_edits_but_not_playback_position() {
        let mut queue = sequence(4);
        let original = queue.content_id().to_owned();
        queue.activate_index(2);
        queue.set_progress_millis(12_000);
        queue.set_repeat_mode(RepeatMode::All);
        assert_eq!(queue.content_id(), original);
        let restored = Sequence::from_window(queue.snapshot(), 0).unwrap();
        assert_eq!(restored.content_id(), original);
        queue.reorder(&[3], &QueueReorderTarget::Before(0), &mut []);
        assert_ne!(queue.content_id(), original);
        assert_eq!(
            queue.content_id(),
            library::queue_content_id(&queue.members, &queue.order)
        );
        queue.add_page(page(8, 1), QueueReorderTarget::End, false, None, &mut []);
        assert_eq!(
            queue.content_id(),
            library::queue_content_id(&queue.members, &queue.order)
        );
        queue.random_start(17);
        assert_eq!(
            queue.content_id(),
            library::queue_content_id(&queue.members, &queue.order)
        );
    }
    #[test]
    fn restored_membership_hydrates_a_later_duplicate_on_navigation() {
        let mut s = Sequence::from_window(sequence(10_000).snapshot(), 1).unwrap();
        let selected = 9999;
        assert!(s.activate_index(9999));
        assert_eq!(s.selected_index(), Some(selected));
        assert!(s.selected().is_none());
        let Some(library::QueueReadRequest::Hydrate { entries }) = s.read_request() else {
            panic!("later membership needs nearby metadata");
        };
        assert!(entries.len() <= library::QUEUE_CONTEXT_LIMIT);
        assert!(entries.iter().any(|(position, _)| *position == selected));
        let mut hydrated = page(9989, 11);
        for row in &mut hydrated.rows {
            Arc::make_mut(row).position += 9989;
        }
        s.hydrate(hydrated);
        assert_eq!(s.selected().unwrap().position, selected);
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
        s.shuffle(true, 17, &mut []);
        assert!(Arc::ptr_eq(&membership, &s.members));
        assert!(Arc::ptr_eq(&current, s.selected().unwrap()));
        assert_eq!(s.take_persistence(), Some(QueuePersistenceKind::Order));
        let Some(library::QueueReadRequest::Hydrate { entries }) = s.read_request() else {
            panic!("nearby metadata");
        };
        assert!(entries.len() <= 99);
        s.shuffle(false, 17, &mut []);
        assert_eq!(&*s.order, &(0..10_000).collect::<Vec<u32>>());
    }
    #[test]
    fn toggles_keep_history_and_repeat_resets_only_order() {
        let mut s = sequence(10);
        s.activate_index(2);
        s.shuffle(true, 3, &mut []);
        s.advance_index(false);
        let history = s.order[..4].to_vec();
        let current = s.selected_index();
        s.shuffle(false, 0, &mut []);
        assert_eq!(s.order[..4], history);
        assert_eq!(s.selected_index(), current);
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
    fn additions_edits_duplicates_and_restore_keep_selected_entry() {
        let mut s = sequence(101);
        s.add_page(page(1000, 1), QueueReorderTarget::End, false, None, &mut []);
        assert_eq!(
            s.entry_at(100).unwrap().playlist_entry_id.as_deref(),
            Some("appearance:100")
        );
        assert_eq!(
            s.entry_at(101).unwrap().playlist_entry_id.as_deref(),
            Some("appearance:1000")
        );
        s.activate_index(3);
        let current = s.entry_at(3).unwrap().clone();
        s.set_progress_millis(42000);
        let mut pending = [Some(4)];
        s.shuffle(true, 4, &mut pending);
        let prepared = s.entry_at(pending[0].unwrap()).unwrap().clone();
        s.add_page(
            page(2000, 1),
            QueueReorderTarget::After(3),
            false,
            None,
            &mut pending,
        );
        assert_eq!(
            s.entry_at(4).unwrap().playlist_entry_id.as_deref(),
            Some("appearance:2000")
        );
        s.remove(&[0], &mut pending);
        assert_eq!(s.selected_index(), Some(2));
        let moving = s
            .order
            .iter()
            .position(|i| {
                s.members[*i as usize].playlist_entry_id.as_deref() == Some("appearance:1000")
            })
            .unwrap();
        s.reorder(&[moving], &QueueReorderTarget::Before(3), &mut pending);
        s.shuffle(false, 0, &mut pending);
        assert_eq!(s.entry_at(s.selected_index().unwrap()), Some(&current));
        assert_eq!(s.entry_at(pending[0].unwrap()), Some(&prepared));
        assert_eq!(s.progress_millis(), 42000);
        let restored = Sequence::from_window(s.snapshot(), s.revision()).unwrap();
        assert_eq!(restored.order, s.order);
        assert_eq!(restored.members, s.members);
        assert_eq!(
            restored.entry_at(restored.selected_index().unwrap()),
            Some(&current)
        );
    }
}
