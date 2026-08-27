//! Retains one complete key order and only the ready rows around the visible route window.
//! Route policy, GTK factories, artwork requests, and skeleton geometry stay with the caller.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
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

const SPARSE_WINDOW_SIZE: usize = 64;
const SPARSE_PENDING_WINDOW_CAP: usize = 8;
const SPARSE_READY_WINDOW_CAP: usize = 3;

fn window_first(position: usize) -> usize {
    position / SPARSE_WINDOW_SIZE * SPARSE_WINDOW_SIZE
}

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

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SparseItem<K, R> {
    Placeholder(K),
    Ready(Arc<R>),
}

pub(crate) struct SparseModel<K, R> {
    order: Arc<[K]>,
    ready: BTreeMap<usize, Arc<R>>,
    ready_windows: VecDeque<usize>,
    generation: u64,
}

pub(crate) struct SparseRouteModel<K, R> {
    model: SparseObjectModel,
    runtime: tokio::runtime::Handle,
    load: SparseLoad<K, R>,
    windows: RefCell<SparseWindowDemand>,
    generation: Cell<u64>,
    running: Cell<Option<u64>>,
    cancellation: RefCell<Option<(u64, ReadCancellation)>>,
}

#[derive(Default)]
struct SparseWindowDemand {
    active: Option<usize>,
    pending: VecDeque<usize>,
}

impl SparseWindowDemand {
    fn replace(&mut self, mut demanded: Vec<usize>) -> bool {
        demanded.sort_unstable();
        demanded.dedup();
        let cancelled_active = self
            .active
            .is_some_and(|active| demanded.binary_search(&active).is_err());
        if cancelled_active {
            self.active = None;
        }
        self.pending.retain(|window| {
            demanded.binary_search(window).is_ok() && self.active != Some(*window)
        });
        for window in demanded {
            if self.active == Some(window) || self.pending.contains(&window) {
                continue;
            }
            if self.pending.len() == SPARSE_PENDING_WINDOW_CAP {
                break;
            }
            self.pending.push_back(window);
        }
        cancelled_active
    }

    fn start(&mut self) -> Option<usize> {
        if self.active.is_some() {
            return None;
        }
        let first = self.pending.pop_front()?;
        self.active = Some(first);
        Some(first)
    }

    fn accept(&mut self, first: usize) -> bool {
        if self.active != Some(first) {
            return false;
        }
        self.active = None;
        true
    }

    fn reset(&mut self) {
        self.active = None;
        self.pending.clear();
    }
}

mod item_imp {
    use std::any::Any;
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;
    use std::rc::Rc;

    use gtk::glib;
    use gtk::glib::subclass::prelude::*;

    #[derive(Default)]
    pub(crate) struct SparseObjectItem {
        pub(super) value: RefCell<Option<Box<dyn Any>>>,
        pub(super) ready: Cell<bool>,
        pub(super) handlers: RefCell<BTreeMap<usize, Rc<dyn Fn()>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SparseObjectItem {
        const NAME: &'static str = "RufinSparseObjectItem";
        type Type = super::SparseObjectItem;
    }

    impl ObjectImpl for SparseObjectItem {}
}

glib::wrapper! {
    pub(crate) struct SparseObjectItem(ObjectSubclass<item_imp::SparseObjectItem>);
}

impl SparseObjectItem {
    fn new<T: 'static>(value: T, ready: bool) -> Self {
        use glib::subclass::prelude::ObjectSubclassIsExt;

        let item: Self = glib::Object::new();
        item.imp().value.replace(Some(Box::new(value)));
        item.imp().ready.set(ready);
        item
    }

    fn replace<T: 'static>(&self, value: T, ready: bool) {
        use glib::subclass::prelude::ObjectSubclassIsExt;

        let imp = self.imp();
        imp.value.replace(Some(Box::new(value)));
        imp.ready.set(ready);
        let handlers = imp.handlers.borrow().values().cloned().collect::<Vec<_>>();
        for handler in handlers {
            handler();
        }
    }

    pub(crate) fn value<T: Clone + 'static>(&self) -> Option<T> {
        use glib::subclass::prelude::ObjectSubclassIsExt;

        self.imp()
            .value
            .borrow()
            .as_ref()
            .and_then(|value| value.downcast_ref::<T>())
            .cloned()
    }

    fn is_ready(&self) -> bool {
        use glib::subclass::prelude::ObjectSubclassIsExt;

        self.imp().ready.get()
    }

    fn connect_ready(&self, owner: usize, handler: Rc<dyn Fn()>) {
        use glib::subclass::prelude::ObjectSubclassIsExt;

        self.imp().handlers.borrow_mut().insert(owner, handler);
    }

    fn disconnect_ready(&self, owner: usize) {
        use glib::subclass::prelude::ObjectSubclassIsExt;

        self.imp().handlers.borrow_mut().remove(&owner);
    }

    #[cfg(test)]
    fn ready_handler_count(&self) -> usize {
        use glib::subclass::prelude::ObjectSubclassIsExt;

        self.imp().handlers.borrow().len()
    }
}

pub(crate) fn bind_sparse_item(item: &gtk::ListItem, render: Rc<dyn Fn(&gtk::ListItem)>) {
    if let Some(row) = item
        .item()
        .and_then(|item| item.downcast::<SparseObjectItem>().ok())
    {
        let owner = item.as_ptr() as usize;
        let weak_item = item.downgrade();
        let weak_row = row.downgrade();
        let ready_render = Rc::clone(&render);
        row.connect_ready(
            owner,
            Rc::new(move || {
                let Some(item) = weak_item.upgrade() else {
                    return;
                };
                let Some(bound_row) = weak_row.upgrade() else {
                    return;
                };
                if item
                    .item()
                    .and_then(|item| item.downcast::<SparseObjectItem>().ok())
                    .as_ref()
                    == Some(&bound_row)
                {
                    ready_render(&item);
                }
            }),
        );
    }
    render(item);
}

pub(crate) fn unbind_sparse_item(item: &gtk::ListItem) {
    let owner = item.as_ptr() as usize;
    if let Some(row) = item
        .item()
        .and_then(|item| item.downcast::<SparseObjectItem>().ok())
    {
        row.disconnect_ready(owner);
    }
}

pub(crate) fn connect_sparse_bind(
    factory: &gtk::SignalListItemFactory,
    render: impl Fn(&glib::Object) + 'static,
) {
    factory.connect_setup(|_, object| {
        if let Some(item) = object.downcast_ref::<gtk::ListItem>() {
            set_sparse_child_ready(item, false);
        }
    });
    let render = Rc::new(render);
    factory.connect_bind(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let render = Rc::clone(&render);
        bind_sparse_item(
            item,
            Rc::new(move |item| {
                let ready = item
                    .item()
                    .and_then(|item| item.downcast::<SparseObjectItem>().ok())
                    .is_none_or(|item| item.is_ready());
                set_sparse_child_ready(item, ready);
                render(item.upcast_ref());
            }),
        );
    });
    factory.connect_unbind(move |_, object| {
        if let Some(item) = object.downcast_ref::<gtk::ListItem>() {
            unbind_sparse_item(item);
            if item
                .item()
                .is_some_and(|item| item.is::<SparseObjectItem>())
            {
                set_sparse_child_ready(item, false);
            }
        }
    });
    factory.connect_teardown(move |_, object| {
        if let Some(item) = object.downcast_ref::<gtk::ListItem>() {
            unbind_sparse_item(item);
        }
    });
}

fn set_sparse_child_ready(item: &gtk::ListItem, ready: bool) {
    let Some(child) = item.child() else { return };
    if ready {
        child.remove_css_class("track-skeleton");
    } else {
        child.add_css_class("track-skeleton");
    }
    child.set_sensitive(ready);
    child.set_can_target(ready);
}

trait SparseObjectState {
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn len(&self) -> usize;
    fn item(&mut self, position: usize) -> Option<SparseObjectItem>;
    fn cold_live_windows(&mut self) -> Vec<usize>;
}

struct TypedSparseObjectState<K, R> {
    sparse: SparseModel<K, R>,
    objects: BTreeMap<usize, glib::WeakRef<SparseObjectItem>>,
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

    fn item(&mut self, position: usize) -> Option<SparseObjectItem> {
        if let Some(item) = self.objects.get(&position).and_then(glib::WeakRef::upgrade) {
            return Some(item);
        }
        self.objects.retain(|_, item| item.upgrade().is_some());
        let value = self.sparse.item(position)?;
        let ready = matches!(value, SparseItem::Ready(_));
        let item = SparseObjectItem::new(value, ready);
        self.objects.insert(position, item.downgrade());
        Some(item)
    }

    fn cold_live_windows(&mut self) -> Vec<usize> {
        self.objects.retain(|_, item| item.upgrade().is_some());
        self.objects
            .iter()
            .filter_map(|(position, item)| {
                let item = item.upgrade()?;
                (!item.is_ready() && !self.sparse.ready.contains_key(position))
                    .then_some(window_first(*position))
            })
            .collect()
    }
}

mod imp {
    use gio::subclass::prelude::*;

    use super::*;

    #[derive(Default)]
    pub(crate) struct SparseObjectModel {
        pub(super) state: RefCell<Option<Box<dyn SparseObjectState>>>,
        pub(super) demand: RefCell<Option<Rc<dyn Fn(u32)>>>,
        pub(super) ready_handler: RefCell<Option<Rc<dyn Fn(u32, u32)>>>,
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
            SparseObjectItem::static_type()
        }

        fn n_items(&self) -> u32 {
            self.state
                .borrow()
                .as_ref()
                .map_or(0, |state| state.len().min(u32::MAX as usize) as u32)
        }

        fn item(&self, position: u32) -> Option<glib::Object> {
            let item = {
                let mut state = self.state.borrow_mut();
                state
                    .as_mut()?
                    .item(position as usize)
                    .map(|item| item.upcast())
            };
            if item.is_some()
                && let Some(demand) = self.demand.borrow().clone()
            {
                demand(position);
            }
            item
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

    fn peek_ready<K, R>(&self, position: usize) -> Option<Arc<R>>
    where
        K: Clone + 'static,
        R: 'static,
    {
        self.with_typed_mut::<K, R, _>(|state| match state.sparse.item(position)? {
            SparseItem::Ready(row) => Some(row),
            SparseItem::Placeholder(_) => None,
        })
    }

    pub(crate) fn seed<K, R>(&self, rows: Vec<R>)
    where
        K: Clone + 'static,
        R: 'static,
    {
        self.with_typed_mut::<K, R, _>(|state| state.sparse.seed(rows));
    }

    fn cold_live_windows(&self) -> Vec<usize> {
        use glib::subclass::prelude::ObjectSubclassIsExt;

        self.imp()
            .state
            .borrow_mut()
            .as_mut()
            .map_or_else(Vec::new, |state| state.cold_live_windows())
    }

    fn set_demand(&self, demand: impl Fn(u32) + 'static) {
        use glib::subclass::prelude::ObjectSubclassIsExt;

        self.imp().demand.replace(Some(Rc::new(demand)));
    }

    pub(crate) fn connect_ready_changed(&self, handler: impl Fn(u32, u32) + 'static) {
        use glib::subclass::prelude::ObjectSubclassIsExt;

        self.imp().ready_handler.replace(Some(Rc::new(handler)));
    }

    fn emit_ready_changed(&self, position: usize, count: usize) {
        use glib::subclass::prelude::ObjectSubclassIsExt;

        let position = position.min(u32::MAX as usize) as u32;
        let count = count.min(u32::MAX as usize) as u32;
        if let Some(handler) = self.imp().ready_handler.borrow().clone() {
            handler(position, count);
        }
    }

    pub(crate) fn hydrate<K, R>(&self, visible: Range<usize>) -> Option<HydrationPage<K>>
    where
        K: Clone + 'static,
        R: 'static,
    {
        self.with_typed_mut::<K, R, _>(|state| {
            let page = state.sparse.hydrate(visible);
            state.objects.retain(|_, item| item.upgrade().is_some());
            page
        })
    }

    pub(crate) fn accept<K, R>(&self, page: &HydrationPage<K>, rows: Vec<R>) -> bool
    where
        K: Clone + 'static,
        R: 'static,
    {
        let accepted = self.with_typed_mut::<K, R, _>(|state| {
            let change = state.sparse.accept(page, rows)?;
            let updates = change
                .range
                .clone()
                .filter_map(|position| {
                    let item = state
                        .objects
                        .get(&position)
                        .and_then(glib::WeakRef::upgrade)?;
                    let value = state.sparse.item(position)?;
                    Some((item, value))
                })
                .collect::<Vec<_>>();
            Some((change, updates))
        });
        let Some((change, updates)) = accepted else {
            return false;
        };
        for (item, value) in updates {
            item.replace(value, true);
        }
        self.emit_ready_changed(change.range.start, change.range.len());
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

    fn replace_prepared<K, R>(&self, order: Vec<K>, rows: Vec<R>)
    where
        K: Clone + Eq + 'static,
        R: Clone + PartialEq + 'static,
    {
        use glib::subclass::prelude::ObjectSubclassIsExt;

        let old_len = self.n_items();
        let (same_order, updates) = self.with_typed_mut::<K, R, _>(|state| {
            let same_order = state.sparse.order.as_ref() == order.as_slice();
            state.sparse.replace_order(order);
            state.sparse.seed(rows);
            if !same_order {
                state.objects.clear();
                return (false, Vec::new());
            }
            state.objects.retain(|_, item| item.upgrade().is_some());
            let positions = state.objects.keys().copied().collect::<Vec<_>>();
            let updates = positions
                .into_iter()
                .filter_map(|position| {
                    let item = state.objects.get(&position)?.upgrade()?;
                    let value = state.sparse.item(position)?;
                    (item.value::<SparseItem<K, R>>().as_ref() != Some(&value)).then(|| {
                        let ready = matches!(value, SparseItem::Ready(_));
                        (position, item, value, ready)
                    })
                })
                .collect::<Vec<_>>();
            (true, updates)
        });
        if !same_order {
            self.items_changed(0, old_len, self.n_items());
            return;
        }
        for (position, item, value, ready) in updates {
            item.replace(value, ready);
            self.emit_ready_changed(position, 1);
        }
        if let Some(position) = self.cold_live_windows().first().copied()
            && let Some(demand) = self.imp().demand.borrow().clone()
        {
            demand(position.min(u32::MAX as usize) as u32);
        }
    }

    pub(crate) fn update_ready<K, R>(&self, key: &K, update: impl FnOnce(&mut R)) -> bool
    where
        K: Clone + Eq + 'static,
        R: Clone + 'static,
    {
        let updated = self.with_typed_mut::<K, R, _>(|state| {
            let position = state.sparse.update_ready(key, update)?;
            let item = state
                .objects
                .get(&position)
                .and_then(glib::WeakRef::upgrade);
            let value = item.as_ref().and_then(|_| state.sparse.item(position));
            Some((position, item.zip(value)))
        });
        let Some((position, item)) = updated else {
            return false;
        };
        if let Some((item, value)) = item {
            item.replace(value, true);
        }
        self.emit_ready_changed(position, 1);
        true
    }

    fn ready_position<K, R>(&self, matches: impl Fn(&R) -> bool) -> Option<usize>
    where
        K: Clone + 'static,
        R: 'static,
    {
        self.with_typed_mut::<K, R, _>(|state| {
            state
                .sparse
                .ready
                .iter()
                .find_map(|(position, row)| matches(row).then_some(*position))
        })
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
        self.with_typed_mut::<K, R, _>(|state| {
            state.objects.retain(|_, item| item.upgrade().is_some());
            state.objects.len()
        })
    }
}

impl<K, R> SparseRouteModel<K, R>
where
    K: Clone + Eq + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
{
    pub(crate) fn new(
        order: Vec<K>,
        _overscan: usize,
        runtime: tokio::runtime::Handle,
        load: SparseLoad<K, R>,
    ) -> Rc<Self> {
        let model = SparseObjectModel::new::<K, R>(order, SPARSE_WINDOW_SIZE);
        let route = Rc::new(Self {
            model: model.clone(),
            runtime,
            load,
            windows: RefCell::new(SparseWindowDemand::default()),
            generation: Cell::new(1),
            running: Cell::new(None),
            cancellation: RefCell::new(None),
        });
        let weak = Rc::downgrade(&route);
        model.set_demand(move |position| {
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

    pub(crate) fn seed(&self, rows: Vec<R>) {
        self.model.seed::<K, R>(rows);
    }

    pub(crate) fn seed_matching(&self, rows: Vec<R>, key: impl Fn(&R) -> K) -> bool {
        let order = self.order();
        if !rows
            .iter()
            .enumerate()
            .all(|(position, row)| order.get(position) == Some(&key(row)))
        {
            return false;
        }
        self.seed(rows);
        true
    }

    pub(crate) fn replace_order(&self, order: Vec<K>) {
        self.cancel();
        self.model.replace_order::<K, R>(order);
    }

    pub(crate) fn replace_prepared(
        &self,
        order: Vec<K>,
        rows: Vec<R>,
        key: impl Fn(&R) -> K,
    ) -> bool
    where
        R: PartialEq,
    {
        if !rows
            .iter()
            .enumerate()
            .all(|(position, row)| order.get(position) == Some(&key(row)))
        {
            return false;
        }
        self.cancel();
        self.model.replace_prepared::<K, R>(order, rows);
        true
    }

    pub(crate) fn ready(&self, position: u32) -> Option<Arc<R>> {
        let item = self
            .model
            .item(position)?
            .downcast::<SparseObjectItem>()
            .ok()?;
        match item.value::<SparseItem<K, R>>()? {
            SparseItem::Ready(row) => Some(row),
            SparseItem::Placeholder(_) => None,
        }
    }

    pub(crate) fn demand_positions(self: &Rc<Self>, positions: impl IntoIterator<Item = usize>) {
        let demanded = positions.into_iter().map(window_first).collect::<Vec<_>>();
        let cancel_running = self.windows.borrow_mut().replace(demanded);
        if cancel_running {
            self.cancel_running();
        }
        self.start();
    }

    pub(crate) fn peek_ready(&self, position: usize) -> Option<Arc<R>> {
        self.model.peek_ready::<K, R>(position)
    }

    pub(crate) fn ready_position(&self, matches: impl Fn(&R) -> bool) -> Option<u32> {
        self.model
            .ready_position::<K, R>(matches)
            .and_then(|position| u32::try_from(position).ok())
    }

    pub(crate) fn update_ready(&self, key: &K, update: impl FnOnce(&mut R)) -> bool {
        self.model.update_ready::<K, R>(key, update)
    }

    pub(crate) fn connect_ready_changed(&self, handler: impl Fn(u32, u32) + 'static) {
        self.model.connect_ready_changed(handler)
    }

    fn demand(self: &Rc<Self>, _: u32) {
        let cancel_running = self
            .windows
            .borrow_mut()
            .replace(self.model.cold_live_windows());
        if cancel_running {
            self.cancel_running();
        }
        self.start();
    }

    fn cancel_running(&self) {
        self.generation
            .set(self.generation.get().wrapping_add(1).max(1));
        if let Some((_, cancellation)) = self.cancellation.borrow_mut().take() {
            cancellation.cancel();
        }
        self.running.set(None);
    }

    fn start(self: &Rc<Self>) {
        if self.running.get().is_some() {
            return;
        }
        let (first, page) = loop {
            let Some(first) = self.windows.borrow_mut().start() else {
                return;
            };
            if let Some(page) = self
                .model
                .hydrate::<K, R>(first..first.saturating_add(SPARSE_WINDOW_SIZE))
            {
                break (first, page);
            }
            self.windows.borrow_mut().accept(first);
        };
        let token = self.generation.get();
        self.running.set(Some(token));
        let cancellation = ReadCancellation::new();
        self.cancellation
            .replace(Some((token, cancellation.clone())));
        let task = self
            .runtime
            .spawn((self.load)(page.keys.clone(), cancellation));
        let route = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let rows = task.await.ok().and_then(Result::ok);
            let Some(route) = route.upgrade() else {
                return;
            };
            if route.running.get() != Some(token) || route.generation.get() != token {
                return;
            }
            route.running.set(None);
            if !route.windows.borrow_mut().accept(first) {
                return;
            }
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
            let _ = route
                .windows
                .borrow_mut()
                .replace(route.model.cold_live_windows());
            route.start();
        });
    }

    fn cancel(&self) {
        self.cancel_running();
        self.windows.borrow_mut().reset();
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
    pub(crate) fn new(_: usize) -> Self {
        Self {
            order: Arc::new([]),
            ready: BTreeMap::new(),
            ready_windows: VecDeque::new(),
            generation: 1,
        }
    }

    pub(crate) fn replace_order(&mut self, order: Vec<K>) {
        self.order = order.into();
        self.ready.clear();
        self.ready_windows.clear();
        self.generation = self.generation.wrapping_add(1).max(1);
    }

    pub(crate) fn len(&self) -> usize {
        self.order.len()
    }

    pub(crate) fn order(&self) -> Arc<[K]> {
        Arc::clone(&self.order)
    }

    fn seed(&mut self, rows: Vec<R>) {
        let rows = rows.into_iter().take(SPARSE_WINDOW_SIZE);
        let mut inserted = false;
        for (position, row) in rows.enumerate() {
            if position >= self.order.len() {
                break;
            }
            self.ready.insert(position, Arc::new(row));
            inserted = true;
        }
        if inserted {
            self.ready_windows.push_back(0);
        }
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

    pub(crate) fn item(&mut self, position: usize) -> Option<SparseItem<K, R>> {
        if let Some(row) = self.ready.get(&position) {
            let row = Arc::clone(row);
            self.touch_ready_window(window_first(position));
            return Some(SparseItem::Ready(row));
        }
        self.order
            .get(position)
            .cloned()
            .map(SparseItem::Placeholder)
    }

    pub(crate) fn hydrate(&mut self, visible: Range<usize>) -> Option<HydrationPage<K>> {
        let start = window_first(visible.start).min(self.order.len());
        let end = start
            .saturating_add(SPARSE_WINDOW_SIZE)
            .min(self.order.len());
        if start == end {
            return None;
        }
        if (start..end).all(|position| self.ready.contains_key(&position)) {
            self.touch_ready_window(start);
            return None;
        }
        Some(HydrationPage {
            generation: self.generation,
            range: start..end,
            keys: self.order[start..end].to_vec(),
        })
    }

    pub(crate) fn accept(&mut self, page: &HydrationPage<K>, rows: Vec<R>) -> Option<ReadyChange> {
        if page.generation != self.generation
            || rows.len() != page.range.len()
            || page.range.end > self.order.len()
        {
            return None;
        }
        let first = window_first(page.range.start);
        if let Some(position) = self
            .ready_windows
            .iter()
            .position(|window| *window == first)
        {
            self.ready_windows.remove(position);
        } else if self.ready_windows.len() == SPARSE_READY_WINDOW_CAP
            && let Some(evicted) = self.ready_windows.pop_front()
        {
            self.ready
                .retain(|position, _| window_first(*position) != evicted);
        }
        self.ready_windows.push_back(first);
        for (position, row) in page.range.clone().zip(rows) {
            self.ready.insert(position, Arc::new(row));
        }
        Some(ReadyChange {
            range: page.range.clone(),
        })
    }

    #[cfg(test)]
    pub(crate) fn teardown(&mut self) {
        self.ready.clear();
        self.order = Arc::new([]);
        self.ready_windows.clear();
        self.generation = self.generation.wrapping_add(1).max(1);
    }

    #[cfg(test)]
    fn ready_len(&self) -> usize {
        self.ready.len()
    }

    fn touch_ready_window(&mut self, first: usize) {
        if let Some(position) = self
            .ready_windows
            .iter()
            .position(|window| *window == first)
        {
            self.ready_windows.remove(position);
            self.ready_windows.push_back(first);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_windows_keep_route_extent_and_three_exact_ready_pages() {
        let mut model = SparseModel::<u64, String>::new(8);
        model.replace_order((0..10_000).collect());
        assert_eq!(model.len(), 10_000);
        let first = model.hydrate(5_000..5_020).expect("hydration page");
        assert_eq!(first.range, 4_992..5_056);
        assert!(matches!(
            model.item(5_000),
            Some(SparseItem::Placeholder(5_000))
        ));
        model
            .accept(&first, first.keys.iter().map(u64::to_string).collect())
            .expect("accept rows");
        assert!(matches!(model.item(5_000), Some(SparseItem::Ready(_))));
        assert_eq!(model.ready_len(), 64);

        let deep = model.hydrate(9_900..9_920).expect("deep hydration");
        model
            .accept(&deep, deep.keys.iter().map(u64::to_string).collect())
            .expect("accept deep rows");
        assert_eq!(model.ready_len(), 128);
        for position in [100, 2_000] {
            let page = model
                .hydrate(position..position + 1)
                .expect("additional hydration page");
            model
                .accept(&page, page.keys.iter().map(u64::to_string).collect())
                .expect("accept additional page");
        }
        assert_eq!(
            model.ready_len(),
            SPARSE_WINDOW_SIZE * SPARSE_READY_WINDOW_CAP
        );
        assert!(matches!(
            model.item(5_000),
            Some(SparseItem::Placeholder(5_000))
        ));
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
    fn live_window_demands_keep_one_active_and_eight_pending_pages() {
        let mut demand = SparseWindowDemand::default();
        assert!(!demand.replace((0..20).map(|index| index * SPARSE_WINDOW_SIZE).collect()));
        assert_eq!(demand.pending.len(), SPARSE_PENDING_WINDOW_CAP);
        assert_eq!(demand.start(), Some(0));
        assert!(demand.replace(vec![9_984]));
        assert_eq!(demand.active, None);
        assert_eq!(demand.pending, [9_984]);
        assert_eq!(demand.start(), Some(9_984));
    }

    #[test]
    fn one_viewport_demand_finishes_each_intersecting_window_without_cancelling_its_peer() {
        let mut demand = SparseWindowDemand::default();
        assert!(!demand.replace(vec![0, SPARSE_WINDOW_SIZE]));
        assert_eq!(demand.start(), Some(0));
        assert!(!demand.replace(vec![0, SPARSE_WINDOW_SIZE]));
        assert!(demand.accept(0));
        assert_eq!(demand.start(), Some(SPARSE_WINDOW_SIZE));
    }

    #[test]
    fn gtk_model_publishes_ready_rows_through_the_existing_item_identity() {
        let model = SparseObjectModel::new::<u64, String>((0..100).collect(), 2);
        assert_eq!(model.n_items(), 100);
        let placeholder = model
            .item(50)
            .and_then(|item| item.downcast::<SparseObjectItem>().ok())
            .expect("placeholder item");
        assert!(matches!(
            placeholder.value::<SparseItem<u64, String>>(),
            Some(SparseItem::Placeholder(50))
        ));
        let notifications = Rc::new(Cell::new(0));
        let notification_count = Rc::clone(&notifications);
        let observed_ready = Rc::new(Cell::new(false));
        let observed_ready_in_handler = Rc::clone(&observed_ready);
        let notification_model = model.clone();
        placeholder.connect_ready(
            1,
            Rc::new(move || {
                notification_count.set(notification_count.get() + 1);
                observed_ready_in_handler.set(
                    notification_model
                        .item(50)
                        .and_then(|item| item.downcast::<SparseObjectItem>().ok())
                        .and_then(|item| item.value::<SparseItem<u64, String>>())
                        .is_some_and(|item| matches!(item, SparseItem::Ready(_))),
                );
            }),
        );

        let page = model
            .hydrate::<u64, String>(50..51)
            .expect("hydration request");
        let rows = page.keys.iter().map(u64::to_string).collect();
        assert!(model.accept::<u64, String>(&page, rows));
        let ready = model
            .item(50)
            .and_then(|item| item.downcast::<SparseObjectItem>().ok())
            .expect("ready item");
        assert_eq!(placeholder, ready);
        assert_eq!(notifications.get(), 1);
        assert!(observed_ready.get());
        assert!(matches!(
            ready.value::<SparseItem<u64, String>>(),
            Some(SparseItem::Ready(value)) if value.as_str() == "50"
        ));
        assert_eq!(model.object_len::<u64, String>(), 1);

        let _ = model.hydrate::<u64, String>(90..91);
        assert_eq!(model.object_len::<u64, String>(), 1);
        drop(ready);
        drop(placeholder);
        assert_eq!(model.object_len::<u64, String>(), 0);
    }

    #[test]
    fn readiness_subscription_is_replaced_and_disconnected_by_bind_owner() {
        let item = SparseObjectItem::new(SparseItem::<u64, String>::Placeholder(0), false);
        let first_calls = Rc::new(Cell::new(0));
        let second_calls = Rc::new(Cell::new(0));

        for _ in 0..32 {
            let first_calls = Rc::clone(&first_calls);
            item.connect_ready(7, Rc::new(move || first_calls.set(first_calls.get() + 1)));
        }
        assert_eq!(item.ready_handler_count(), 1);
        item.replace(
            SparseItem::<u64, String>::Ready(Arc::new("one".into())),
            true,
        );
        assert_eq!(first_calls.get(), 1);

        let second_calls_in_handler = Rc::clone(&second_calls);
        item.connect_ready(
            7,
            Rc::new(move || second_calls_in_handler.set(second_calls_in_handler.get() + 1)),
        );
        assert_eq!(item.ready_handler_count(), 1);
        item.replace(
            SparseItem::<u64, String>::Ready(Arc::new("two".into())),
            true,
        );
        assert_eq!(first_calls.get(), 1);
        assert_eq!(second_calls.get(), 1);

        item.disconnect_ready(7);
        assert_eq!(item.ready_handler_count(), 0);
        item.replace(
            SparseItem::<u64, String>::Ready(Arc::new("three".into())),
            true,
        );
        assert_eq!(second_calls.get(), 1);
    }

    #[test]
    fn folder_rows_use_the_same_sparse_ready_extraction_path() {
        use super::super::folders::FolderLink;

        let model = SparseObjectModel::new::<FolderLink, String>(
            vec![FolderLink {
                object_id: "folder:music".to_string(),
                name: "Music".to_string(),
            }],
            SPARSE_WINDOW_SIZE,
        );
        model.seed::<FolderLink, String>(vec!["Music".to_string()]);

        assert_eq!(
            super::super::library_fields::item_at::<String>(&model, 0).as_deref(),
            Some("Music")
        );
    }

    #[test]
    fn prepared_replacement_publishes_the_first_window_atomically() {
        let model = SparseObjectModel::new::<u64, String>(vec![1, 2], SPARSE_WINDOW_SIZE);
        let _old_item = model.item(0).expect("old item");

        model.replace_prepared::<u64, String>(
            vec![10, 11],
            vec!["ten".to_string(), "eleven".to_string()],
        );

        let first = model
            .item(0)
            .and_then(|item| item.downcast::<SparseObjectItem>().ok())
            .and_then(|item| item.value::<SparseItem<u64, String>>());
        assert!(matches!(first, Some(SparseItem::Ready(value)) if value.as_str() == "ten"));
    }

    #[test]
    fn identical_prepared_replacement_preserves_live_items_without_a_splice() {
        let model = SparseObjectModel::new::<u64, String>(vec![1, 2], SPARSE_WINDOW_SIZE);
        model.seed::<u64, String>(vec!["one".to_string(), "two".to_string()]);
        let item = model
            .item(0)
            .and_then(|item| item.downcast::<SparseObjectItem>().ok())
            .expect("live prepared row");
        let renders = Rc::new(Cell::new(0));
        let render_count = Rc::clone(&renders);
        item.connect_ready(1, Rc::new(move || render_count.set(render_count.get() + 1)));
        let splices = Rc::new(Cell::new(0));
        let splice_count = Rc::clone(&splices);
        model.connect_items_changed(move |_, _, _, _| splice_count.set(splice_count.get() + 1));

        model.replace_prepared::<u64, String>(
            vec![1, 2],
            vec!["one".to_string(), "two".to_string()],
        );

        let current = model
            .item(0)
            .and_then(|item| item.downcast::<SparseObjectItem>().ok())
            .expect("same live prepared row");
        assert_eq!(current, item);
        assert_eq!(renders.get(), 0);
        assert_eq!(splices.get(), 0);
    }

    #[test]
    fn prepared_replacement_updates_only_the_changed_live_row() {
        let model = SparseObjectModel::new::<u64, String>(vec![1, 2], SPARSE_WINDOW_SIZE);
        model.seed::<u64, String>(vec!["one".to_string(), "two".to_string()]);
        let first = model
            .item(0)
            .and_then(|item| item.downcast::<SparseObjectItem>().ok())
            .expect("first live row");
        let second = model
            .item(1)
            .and_then(|item| item.downcast::<SparseObjectItem>().ok())
            .expect("second live row");
        let first_renders = Rc::new(Cell::new(0));
        let first_count = Rc::clone(&first_renders);
        first.connect_ready(1, Rc::new(move || first_count.set(first_count.get() + 1)));
        let second_renders = Rc::new(Cell::new(0));
        let second_count = Rc::clone(&second_renders);
        second.connect_ready(2, Rc::new(move || second_count.set(second_count.get() + 1)));

        model.replace_prepared::<u64, String>(
            vec![1, 2],
            vec!["one".to_string(), "changed".to_string()],
        );

        assert_eq!(first_renders.get(), 0);
        assert_eq!(second_renders.get(), 1);
        assert!(matches!(
            second.value::<SparseItem<u64, String>>(),
            Some(SparseItem::Ready(value)) if value.as_str() == "changed"
        ));
    }

    #[test]
    fn prepared_replacement_reloads_a_live_row_outside_the_seed_window() {
        let order = (0..128_u64).collect::<Vec<_>>();
        let model = SparseObjectModel::new::<u64, String>(order.clone(), SPARSE_WINDOW_SIZE);
        let page = model
            .hydrate::<u64, String>(64..65)
            .expect("second sparse page");
        assert!(
            model.accept::<u64, String>(&page, (64..128).map(|value| value.to_string()).collect())
        );
        let deep = model
            .item(64)
            .and_then(|item| item.downcast::<SparseObjectItem>().ok())
            .expect("live deep row");
        let demanded = Rc::new(Cell::new(None));
        let demand_position = Rc::clone(&demanded);
        model.set_demand(move |position| {
            demand_position.set(Some(position));
        });

        model.replace_prepared::<u64, String>(
            order,
            (0..64).map(|value| value.to_string()).collect(),
        );

        assert!(matches!(
            deep.value::<SparseItem<u64, String>>(),
            Some(SparseItem::Placeholder(64))
        ));
        assert_eq!(demanded.get(), Some(64));
    }
}
