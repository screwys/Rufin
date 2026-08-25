//! Retains one complete key order and only the ready rows around the visible route window.
//! Route policy, GTK factories, artwork requests, and skeleton geometry stay with the caller.

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HydrationPage<K> {
    pub generation: u64,
    pub range: Range<usize>,
    pub keys: Vec<K>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReadyChange {
    pub range: Range<usize>,
}

pub(crate) struct SparseModel<K, R> {
    order: Arc<[K]>,
    ready: BTreeMap<usize, Arc<R>>,
    window: Range<usize>,
    overscan: usize,
    generation: u64,
}

impl<K, R> SparseModel<K, R>
where
    K: Clone,
{
    pub(crate) fn new(overscan: usize) -> Self {
        Self {
            order: Arc::new([]),
            ready: BTreeMap::new(),
            window: 0..0,
            overscan,
            generation: 1,
        }
    }

    pub(crate) fn replace_order(&mut self, order: Vec<K>) {
        self.order = order.into();
        self.ready.clear();
        self.window = 0..0;
        self.generation = self.generation.wrapping_add(1).max(1);
    }

    pub(crate) fn len(&self) -> usize {
        self.order.len()
    }

    pub(crate) fn key(&self, position: usize) -> Option<&K> {
        self.order.get(position)
    }

    pub(crate) fn row(&self, position: usize) -> Option<&Arc<R>> {
        self.ready.get(&position)
    }

    pub(crate) fn hydrate(&mut self, visible: Range<usize>) -> Option<HydrationPage<K>> {
        let start = visible
            .start
            .saturating_sub(self.overscan)
            .min(self.order.len());
        let end = visible
            .end
            .saturating_add(self.overscan)
            .min(self.order.len());
        self.window = start..end;
        self.ready
            .retain(|position, _| self.window.contains(position));
        let missing_start = (start..end).find(|position| !self.ready.contains_key(position))?;
        let missing_end = (missing_start..end)
            .find(|position| self.ready.contains_key(position))
            .unwrap_or(end);
        Some(HydrationPage {
            generation: self.generation,
            range: missing_start..missing_end,
            keys: self.order[missing_start..missing_end].to_vec(),
        })
    }

    pub(crate) fn accept(&mut self, page: &HydrationPage<K>, rows: Vec<R>) -> Option<ReadyChange> {
        if page.generation != self.generation
            || rows.len() != page.range.len()
            || page.range.end > self.order.len()
        {
            return None;
        }
        for (position, row) in page.range.clone().zip(rows) {
            if self.window.contains(&position) {
                self.ready.insert(position, Arc::new(row));
            }
        }
        Some(ReadyChange {
            range: page.range.clone(),
        })
    }

    pub(crate) fn teardown(&mut self) {
        self.ready.clear();
        self.order = Arc::new([]);
        self.window = 0..0;
        self.generation = self.generation.wrapping_add(1).max(1);
    }

    #[cfg(test)]
    fn ready_len(&self) -> usize {
        self.ready.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_windows_keep_route_extent_and_only_visible_overscan_rows() {
        let mut model = SparseModel::<u64, String>::new(8);
        model.replace_order((0..10_000).collect());
        assert_eq!(model.len(), 10_000);
        let first = model.hydrate(5_000..5_020).expect("hydration page");
        assert_eq!(first.range, 4_992..5_028);
        model
            .accept(&first, first.keys.iter().map(u64::to_string).collect())
            .expect("accept rows");
        assert_eq!(model.ready_len(), 36);

        let deep = model.hydrate(9_900..9_920).expect("deep hydration");
        assert_eq!(model.ready_len(), 0);
        model
            .accept(&deep, deep.keys.iter().map(u64::to_string).collect())
            .expect("accept deep rows");
        assert_eq!(model.ready_len(), 36);
        assert_eq!(model.key(9_999), Some(&9_999));
    }

    #[test]
    fn replaced_or_torn_down_routes_reject_delayed_rows() {
        let mut model = SparseModel::<u64, u64>::new(2);
        model.replace_order((0..20).collect());
        let delayed = model.hydrate(5..10).expect("hydration page");
        model.replace_order((100..120).collect());
        assert!(model.accept(&delayed, delayed.keys.clone()).is_none());
        model.teardown();
        assert_eq!(model.len(), 0);
    }
}
