//! Retains one complete key order and only the ready rows around the visible route window.
//! Route policy, GTK factories, artwork requests, and skeleton geometry stay with the caller.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::future::Future;
use std::ops::Range;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;

use gtk::prelude::*;
use gtk::{gio, glib};
use library::ReadCancellation;

pub(crate) type SparseLoad<K, R> = Arc<
    dyn Fn(Vec<K>, ReadCancellation) -> Pin<Box<dyn Future<Output = Result<Vec<R>, String>> + Send>>
        + Send
        + Sync,
>;

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

#[derive(Clone, Debug)]
pub(crate) enum SparseItem<K, R> {
    Placeholder(K),
    Ready(Arc<R>),
}

pub(crate) struct SparseModel<K, R> {
    order: Arc<[K]>,
    ready: BTreeMap<usize, Arc<R>>,
    window: Range<usize>,
    overscan: usize,
    generation: u64,
}

pub(crate) struct SparseRouteModel<K, R> {
    model: SparseObjectModel,
    runtime: tokio::runtime::Handle,
    load: SparseLoad<K, R>,
    demand: RefCell<Range<usize>>,
    generation: Cell<u64>,
    running: Cell<Option<u64>>,
    cancellation: RefCell<Option<(u64, ReadCancellation)>>,
    overscan: usize,
}

trait SparseObjectState {
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn len(&self) -> usize;
    fn item(&mut self, position: usize) -> Option<glib::BoxedAnyObject>;
}

struct TypedSparseObjectState<K, R> {
    sparse: SparseModel<K, R>,
    objects: BTreeMap<usize, glib::BoxedAnyObject>,
}

impl<K, R> SparseObjectState for TypedSparseObjectState<K, R>
where
    K: Clone + 'static,
    R: 'static,
{
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn len(&self) -> usize {
        self.sparse.len()
    }

    fn item(&mut self, position: usize) -> Option<glib::BoxedAnyObject> {
        if let Some(item) = self.objects.get(&position) {
            return Some(item.clone());
        }
        let item = glib::BoxedAnyObject::new(self.sparse.item(position)?);
        self.objects.insert(position, item.clone());
        Some(item)
    }
}

mod imp {
    use gio::subclass::prelude::*;

    use super::*;

    #[derive(Default)]
    pub(crate) struct SparseObjectModel {
        pub(super) state: RefCell<Option<Box<dyn SparseObjectState>>>,
        pub(super) demand: RefCell<Option<Rc<dyn Fn(u32)>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SparseObjectModel {
        const NAME: &'static str = "RufinSparseObjectModel";
        type Type = super::SparseObjectModel;
        type Interfaces = (gio::ListModel,);
    }

    impl ObjectImpl for SparseObjectModel {}

    impl ListModelImpl for SparseObjectModel {
        fn item_type(&self) -> glib::Type {
            glib::BoxedAnyObject::static_type()
        }

        fn n_items(&self) -> u32 {
            self.state
                .borrow()
                .as_ref()
                .map_or(0, |state| state.len().min(u32::MAX as usize) as u32)
        }

        fn item(&self, position: u32) -> Option<glib::Object> {
            if let Some(demand) = self.demand.borrow().as_ref() {
                demand(position);
            }
            self.state
                .borrow_mut()
                .as_mut()?
                .item(position as usize)
                .map(|item| item.upcast())
        }
    }
}

glib::wrapper! {
    pub(crate) struct SparseObjectModel(ObjectSubclass<imp::SparseObjectModel>)
        @implements gio::ListModel;
}

impl SparseObjectModel {
    pub(crate) fn new<K, R>(order: Vec<K>, overscan: usize) -> Self
    where
        K: Clone + 'static,
        R: 'static,
    {
        use glib::subclass::prelude::ObjectSubclassIsExt;

        let model: Self = glib::Object::new();
        let mut sparse = SparseModel::new(overscan);
        sparse.replace_order(order);
        model
            .imp()
            .state
            .replace(Some(Box::new(TypedSparseObjectState::<K, R> {
                sparse,
                objects: BTreeMap::new(),
            })));
        model
    }

    pub(crate) fn order<K, R>(&self) -> Arc<[K]>
    where
        K: Clone + 'static,
        R: 'static,
    {
        self.with_typed_mut::<K, R, _>(|state| state.sparse.order())
    }

    pub(crate) fn set_demand(&self, demand: impl Fn(u32) + 'static) {
        use glib::subclass::prelude::ObjectSubclassIsExt;

        self.imp().demand.replace(Some(Rc::new(demand)));
    }

    pub(crate) fn hydrate<K, R>(&self, visible: Range<usize>) -> Option<HydrationPage<K>>
    where
        K: Clone + 'static,
        R: 'static,
    {
        self.with_typed_mut::<K, R, _>(|state| {
            let page = state.sparse.hydrate(visible);
            let window = state.sparse.window();
            state
                .objects
                .retain(|position, _| window.contains(position));
            page
        })
    }

    pub(crate) fn accept<K, R>(&self, page: &HydrationPage<K>, rows: Vec<R>) -> bool
    where
        K: Clone + 'static,
        R: 'static,
    {
        let change = self.with_typed_mut::<K, R, _>(|state| {
            let change = state.sparse.accept(page, rows)?;
            for position in change.range.clone() {
                state.objects.remove(&position);
            }
            Some(change)
        });
        let Some(change) = change else {
            return false;
        };
        self.items_changed(
            change.range.start.min(u32::MAX as usize) as u32,
            change.range.len().min(u32::MAX as usize) as u32,
            change.range.len().min(u32::MAX as usize) as u32,
        );
        true
    }

    pub(crate) fn replace_order<K, R>(&self, order: Vec<K>)
    where
        K: Clone + 'static,
        R: 'static,
    {
        let old_len = self.n_items();
        self.with_typed_mut::<K, R, _>(|state| {
            state.sparse.replace_order(order);
            state.objects.clear();
        });
        self.items_changed(0, old_len, self.n_items());
    }

    pub(crate) fn update_ready<K, R>(&self, key: &K, update: impl FnOnce(&mut R)) -> bool
    where
        K: Clone + Eq + 'static,
        R: Clone + 'static,
    {
        let position = self.with_typed_mut::<K, R, _>(|state| {
            let position = state.sparse.update_ready(key, update)?;
            state.objects.remove(&position);
            Some(position)
        });
        let Some(position) = position else {
            return false;
        };
        let position = position.min(u32::MAX as usize) as u32;
        self.items_changed(position, 1, 1);
        true
    }

    fn with_typed_mut<K, R, T>(
        &self,
        apply: impl FnOnce(&mut TypedSparseObjectState<K, R>) -> T,
    ) -> T
    where
        K: Clone + 'static,
        R: 'static,
    {
        use glib::subclass::prelude::ObjectSubclassIsExt;

        let mut state = self.imp().state.borrow_mut();
        let state = state
            .as_mut()
            .and_then(|state| state.as_any_mut().downcast_mut())
            .expect("SparseObjectModel concrete row type must match its constructor");
        apply(state)
    }

    #[cfg(test)]
    fn object_len<K, R>(&self) -> usize
    where
        K: Clone + 'static,
        R: 'static,
    {
        self.with_typed_mut::<K, R, _>(|state| state.objects.len())
    }
}

impl<K, R> SparseRouteModel<K, R>
where
    K: Clone + Eq + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
{
    pub(crate) fn new(
        order: Vec<K>,
        overscan: usize,
        runtime: tokio::runtime::Handle,
        load: SparseLoad<K, R>,
    ) -> Rc<Self> {
        let route = Rc::new(Self {
            model: SparseObjectModel::new::<K, R>(order, overscan),
            runtime,
            load,
            demand: RefCell::new(0..0),
            generation: Cell::new(1),
            running: Cell::new(None),
            cancellation: RefCell::new(None),
            overscan,
        });
        let weak = Rc::downgrade(&route);
        route.model.set_demand(move |position| {
            if let Some(route) = weak.upgrade() {
                route.demand(position);
            }
        });
        route
    }

    pub(crate) fn list_model(&self) -> SparseObjectModel {
        self.model.clone()
    }

    pub(crate) fn len(&self) -> usize {
        self.model.n_items() as usize
    }

    pub(crate) fn order(&self) -> Arc<[K]> {
        self.model.order::<K, R>()
    }

    pub(crate) fn replace_order(&self, order: Vec<K>) {
        self.cancel();
        self.model.replace_order::<K, R>(order);
        self.demand.replace(0..0);
    }

    pub(crate) fn ready(&self, position: u32) -> Option<Arc<R>> {
        let item = self
            .model
            .item(position)?
            .downcast::<glib::BoxedAnyObject>()
            .ok()?;
        let item = item.borrow::<SparseItem<K, R>>();
        match &*item {
            SparseItem::Ready(row) => Some(Arc::clone(row)),
            SparseItem::Placeholder(_) => None,
        }
    }

    pub(crate) fn update_ready(&self, key: &K, update: impl FnOnce(&mut R)) -> bool {
        self.model.update_ready::<K, R>(key, update)
    }

    fn demand(self: &Rc<Self>, position: u32) {
        let position = position as usize;
        if position >= self.len() {
            return;
        }
        let deep_change = {
            let mut demand = self.demand.borrow_mut();
            let deep = !demand.is_empty()
                && (position.saturating_add(self.overscan) < demand.start
                    || position > demand.end.saturating_add(self.overscan));
            if demand.is_empty()
                || position.saturating_add(self.overscan) < demand.start
                || position > demand.end.saturating_add(self.overscan)
            {
                *demand = position..position.saturating_add(1);
            } else {
                demand.start = demand.start.min(position);
                demand.end = demand.end.max(position.saturating_add(1));
            }
            deep
        };
        if deep_change {
            self.cancel();
        }
        self.start();
    }

    fn start(self: &Rc<Self>) {
        if self.running.get().is_some() {
            return;
        }
        let Some(page) = self.model.hydrate::<K, R>(self.demand.borrow().clone()) else {
            return;
        };
        let token = self.generation.get();
        self.running.set(Some(token));
        let cancellation = ReadCancellation::new();
        self.cancellation
            .replace(Some((token, cancellation.clone())));
        let task = self
            .runtime
            .spawn((self.load)(page.keys.clone(), cancellation));
        let route = Rc::clone(self);
        glib::spawn_future_local(async move {
            let rows = task.await.ok().and_then(Result::ok);
            if route.running.get() != Some(token) || route.generation.get() != token {
                return;
            }
            route.running.set(None);
            if route
                .cancellation
                .borrow()
                .as_ref()
                .is_some_and(|(owner, _)| *owner == token)
            {
                route.cancellation.borrow_mut().take();
            }
            if let Some(rows) = rows {
                route.model.accept::<K, R>(&page, rows);
            }
            route.start();
        });
    }

    fn cancel(&self) {
        self.generation
            .set(self.generation.get().wrapping_add(1).max(1));
        if let Some((_, cancellation)) = self.cancellation.borrow_mut().take() {
            cancellation.cancel();
        }
        self.running.set(None);
    }
}

impl<K, R> Drop for SparseRouteModel<K, R> {
    fn drop(&mut self) {
        if let Some((_, cancellation)) = self.cancellation.get_mut().take() {
            cancellation.cancel();
        }
    }
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

    pub(crate) fn order(&self) -> Arc<[K]> {
        Arc::clone(&self.order)
    }

    #[cfg(test)]
    pub(crate) fn key(&self, position: usize) -> Option<&K> {
        self.order.get(position)
    }

    pub(crate) fn update_ready(&mut self, key: &K, update: impl FnOnce(&mut R)) -> Option<usize>
    where
        K: Eq,
        R: Clone,
    {
        let position = self
            .ready
            .keys()
            .copied()
            .find(|position| self.order.get(*position) == Some(key))?;
        let row = self.ready.get_mut(&position)?;
        update(Arc::make_mut(row));
        Some(position)
    }

    pub(crate) fn window(&self) -> Range<usize> {
        self.window.clone()
    }

    pub(crate) fn item(&self, position: usize) -> Option<SparseItem<K, R>> {
        if let Some(row) = self.ready.get(&position) {
            return Some(SparseItem::Ready(Arc::clone(row)));
        }
        self.order
            .get(position)
            .cloned()
            .map(SparseItem::Placeholder)
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

    #[cfg(test)]
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
        assert!(matches!(
            model.item(5_000),
            Some(SparseItem::Placeholder(5_000))
        ));
        model
            .accept(&first, first.keys.iter().map(u64::to_string).collect())
            .expect("accept rows");
        assert!(matches!(model.item(5_000), Some(SparseItem::Ready(_))));
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

    #[test]
    fn gtk_model_replaces_placeholder_identity_before_publishing_ready_rows() {
        let model = SparseObjectModel::new::<u64, String>((0..100).collect(), 2);
        assert_eq!(model.n_items(), 100);
        let placeholder = model
            .item(50)
            .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
            .expect("placeholder item");
        assert!(matches!(
            &*placeholder.borrow::<SparseItem<u64, String>>(),
            SparseItem::Placeholder(50)
        ));

        let page = model
            .hydrate::<u64, String>(50..51)
            .expect("hydration request");
        let rows = page.keys.iter().map(u64::to_string).collect();
        assert!(model.accept::<u64, String>(&page, rows));
        let ready = model
            .item(50)
            .and_then(|item| item.downcast::<glib::BoxedAnyObject>().ok())
            .expect("ready item");
        assert_ne!(placeholder, ready);
        assert!(matches!(
            &*ready.borrow::<SparseItem<u64, String>>(),
            SparseItem::Ready(value) if value.as_str() == "50"
        ));
        assert_eq!(model.object_len::<u64, String>(), 1);

        let _ = model.hydrate::<u64, String>(90..91);
        assert_eq!(model.object_len::<u64, String>(), 0);
    }
}
