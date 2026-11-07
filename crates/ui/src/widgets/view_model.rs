//! The one list model of windowed rows. A page's rows come from its core View, read a window
//! at a time around what is on screen; other lists read pages of their own. Rows shown keep
//! committed favorites, ratings and downloads, and a reread of the same list keeps what is on
//! screen until its windows are read again.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::future::Future;
use std::marker::PhantomData;
use std::ops::Range;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;

use gtk::prelude::*;
use gtk::{gio, glib};
use library::{ReadCancellation, Row};
use rufin_core::view::{OpenOptions, Opened, Reread, View};

const WINDOW: usize = 64;
const PENDING_CAP: usize = 8;
const READY_CAP: usize = 3;

fn window_first(position: usize) -> usize {
    position / WINDOW * WINDOW
}

/// Reads rows `range` of a list that is not a View.
pub type PageReader<T> = Rc<
    dyn Fn(
        Range<usize>,
        ReadCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<T>, String>> + Send>>,
>;

/// Reads rows `range` of a View, or of an artist page's favorite tracks.
type ViewReader<T> = fn(
    Arc<View>,
    bool,
    Range<usize>,
    ReadCancellation,
) -> Pin<Box<dyn Future<Output = Result<Vec<T>, String>> + Send>>;

/// Where a list's windows come from.
enum Source<T> {
    /// A page: its View's rows, or an artist page's favorite tracks.
    View {
        view: Arc<View>,
        favorites: bool,
        read: ViewReader<T>,
    },
    Pages(PageReader<T>),
    /// Every row was given; nothing is read or dropped. Its rows play outside a page.
    Seeded,
}

mod item_imp {
    use super::*;
    use gtk::glib::subclass::prelude::*;

    #[derive(Default)]
    pub struct RowItem {
        pub(super) value: RefCell<Option<Box<dyn Any>>>,
        pub(super) ready: Cell<bool>,
        pub(super) handlers: RefCell<BTreeMap<usize, Rc<dyn Fn()>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RowItem {
        const NAME: &'static str = "RufinRowItem";
        type Type = super::RowItem;
    }

    impl ObjectImpl for RowItem {}
}

glib::wrapper! {
    /// One row of a list: a value, or a placeholder until its window is read.
    pub struct RowItem(ObjectSubclass<item_imp::RowItem>);
}

impl RowItem {
    pub fn new<T: 'static>(value: T, ready: bool) -> Self {
        use glib::subclass::prelude::ObjectSubclassIsExt;
        let item: Self = glib::Object::new();
        item.imp().value.replace(Some(Box::new(value)));
        item.imp().ready.set(ready);
        item
    }

    fn placeholder() -> Self {
        glib::Object::new()
    }

    pub fn replace<T: 'static>(&self, value: T, ready: bool) {
        use glib::subclass::prelude::ObjectSubclassIsExt;
        let imp = self.imp();
        imp.value.replace(Some(Box::new(value)));
        imp.ready.set(ready);
        let handlers = imp.handlers.borrow().values().cloned().collect::<Vec<_>>();
        for handler in handlers {
            handler();
        }
    }

    pub fn value<T: Clone + 'static>(&self) -> Option<T> {
        use glib::subclass::prelude::ObjectSubclassIsExt;
        self.imp()
            .value
            .borrow()
            .as_ref()
            .and_then(|value| value.downcast_ref::<T>())
            .cloned()
    }

    pub fn is_ready(&self) -> bool {
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

    /// The row it shows as `T`, when it is read.
    pub fn part<T: Clone + 'static>(&self) -> Option<T> {
        self.ready_as(T::clone)
    }

    /// The row it shows as `T`: a row of that type, or the part of a library row that is one.
    fn ready_as<T: Clone + 'static, R>(&self, map: impl FnOnce(&T) -> R) -> Option<R> {
        use glib::subclass::prelude::ObjectSubclassIsExt;
        if !self.is_ready() {
            return None;
        }
        let value = self.imp().value.borrow();
        let value = value.as_ref()?;
        if let Some(row) = value.downcast_ref::<Arc<T>>() {
            return Some(map(row));
        }
        let row = value.downcast_ref::<Arc<Row>>()?;
        (&**row as &dyn Any)
            .downcast_ref::<T>()
            .or_else(|| row_part::<T>(row))
            .map(map)
    }
}

/// The part of a library row a renderer of `T` shows.
fn row_part<T: 'static>(row: &Row) -> Option<&T> {
    let part: &dyn Any = match row {
        Row::Track(track) | Row::AlbumTrack { track, .. } => track,
        Row::Album(album) | Row::AlbumHeading(album) => album,
        Row::Artist(artist) => artist,
        Row::Tag(tag) => tag,
        Row::Playlist(playlist) => playlist,
        Row::Folder(folder) => folder,
    };
    part.downcast_ref::<T>()
}

pub fn bind_row_item(item: &gtk::ListItem, render: Rc<dyn Fn(&gtk::ListItem)>) {
    if let Some(row) = item.item().and_downcast::<RowItem>() {
        let owner = item.as_ptr() as usize;
        let weak_item = item.downgrade();
        let weak_row = row.downgrade();
        let ready_render = Rc::clone(&render);
        row.connect_ready(
            owner,
            Rc::new(move || {
                let (Some(item), Some(bound)) = (weak_item.upgrade(), weak_row.upgrade()) else {
                    return;
                };
                if item.item().and_downcast::<RowItem>().as_ref() == Some(&bound) {
                    ready_render(&item);
                }
            }),
        );
    }
    render(item);
}

pub fn unbind_row_item(item: &gtk::ListItem) {
    if let Some(row) = item.item().and_downcast::<RowItem>() {
        row.disconnect_ready(item.as_ptr() as usize);
    }
}

pub(crate) fn teardown_row_item(item: &gtk::ListItem) {
    unbind_row_item(item);
    item.set_child(None::<&gtk::Widget>);
}

/// Renders a list item when it binds and again when its row is read or changes; a placeholder
/// shows as a skeleton.
pub fn connect_row_bind(
    factory: &gtk::SignalListItemFactory,
    render: impl Fn(&glib::Object) + 'static,
) {
    factory.connect_setup(|_, object| {
        if let Some(item) = object.downcast_ref::<gtk::ListItem>() {
            set_child_ready(item, false);
        }
    });
    let render = Rc::new(render);
    factory.connect_bind(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let render = Rc::clone(&render);
        bind_row_item(
            item,
            Rc::new(move |item| {
                let ready = item
                    .item()
                    .and_downcast::<RowItem>()
                    .is_none_or(|item| item.is_ready());
                render(item.upcast_ref());
                set_child_ready(item, ready);
            }),
        );
    });
    factory.connect_unbind(|_, object| {
        if let Some(item) = object.downcast_ref::<gtk::ListItem>() {
            unbind_row_item(item);
        }
    });
    factory.connect_teardown(|_, object| {
        if let Some(item) = object.downcast_ref::<gtk::ListItem>() {
            teardown_row_item(item);
        }
    });
}

fn set_child_ready(item: &gtk::ListItem, ready: bool) {
    let Some(child) = item.child() else { return };
    if ready {
        child.remove_css_class("track-skeleton");
    } else {
        child.add_css_class("track-skeleton");
    }
    child.set_sensitive(ready);
    child.set_can_target(ready);
}

/// `T` of a list object: a boxed value, or a read row that is or holds one.
pub fn object_item<T: Clone + 'static, R>(
    item: glib::Object,
    map: impl FnOnce(&T) -> R,
) -> Option<R> {
    match item.downcast::<glib::BoxedAnyObject>() {
        Ok(boxed) => boxed.try_borrow::<T>().ok().map(|item| map(&item)),
        Err(item) => item.downcast::<RowItem>().ok()?.ready_as(map),
    }
}

pub fn item_at<T: Clone + 'static>(model: &impl IsA<gio::ListModel>, position: u32) -> Option<T> {
    model
        .item(position)
        .and_then(|item| object_item(item, Clone::clone))
}

pub fn item_at_from_item<T: Clone + 'static>(item: &gtk::ListItem) -> Option<T> {
    item.item().and_then(|item| object_item(item, Clone::clone))
}

/// What the list model holds, whatever its row type.
trait AnyRows {
    fn as_any(&mut self) -> &mut dyn Any;
    fn len(&self) -> usize;
    fn item(&mut self, position: usize) -> Option<RowItem>;
}

struct State<T> {
    source: Source<T>,
    len: usize,
    ready: BTreeMap<usize, Arc<T>>,
    /// Ready windows, least recently used first.
    windows: VecDeque<usize>,
    /// Windows whose rows a reread of the same list kept until they are read again.
    stale: BTreeSet<usize>,
    objects: BTreeMap<usize, glib::WeakRef<RowItem>>,
    pending: VecDeque<usize>,
    active: Option<usize>,
    failed: Vec<usize>,
    /// The first demand waits for the route to scroll to a seed below the top.
    deferred: bool,
    running: Option<ReadCancellation>,
}

impl<T: 'static> AnyRows for State<T> {
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }

    fn len(&self) -> usize {
        self.len
    }

    fn item(&mut self, position: usize) -> Option<RowItem> {
        if position >= self.len {
            return None;
        }
        if let Some(item) = self.objects.get(&position).and_then(glib::WeakRef::upgrade) {
            return Some(item);
        }
        let item = match self.ready.get(&position) {
            Some(row) => {
                let row = Arc::clone(row);
                self.touch(window_first(position));
                RowItem::new(row, true)
            }
            None => RowItem::placeholder(),
        };
        self.objects.insert(position, item.downgrade());
        Some(item)
    }
}

impl<T> State<T> {
    fn live(&self, window: usize) -> bool {
        self.objects
            .range(window..window + WINDOW)
            .any(|(_, item)| item.upgrade().is_some())
    }

    fn touch(&mut self, window: usize) {
        self.windows.retain(|ready| *ready != window);
        self.windows.push_back(window);
    }

    /// Drops the least recently used windows over the cap; windows on screen stay, so patches
    /// reach every row shown, and so does the one touched last.
    fn evict(&mut self) {
        if matches!(self.source, Source::Seeded) {
            return;
        }
        let mut index = 0;
        while self.windows.len() > READY_CAP && index + 1 < self.windows.len() {
            if self.live(self.windows[index]) {
                index += 1;
                continue;
            }
            let evicted = self.windows.remove(index).expect("ready window");
            self.stale.remove(&evicted);
            self.ready
                .retain(|position, _| window_first(*position) != evicted);
        }
    }

    /// Seeds rows from `first` and returns the positions filled.
    fn seed(&mut self, first: usize, rows: Vec<T>) -> Range<usize> {
        let capacity = if matches!(self.source, Source::Seeded) {
            usize::MAX
        } else {
            WINDOW * READY_CAP - first % WINDOW
        };
        let mut end = first;
        for (offset, row) in rows.into_iter().take(capacity).enumerate() {
            let position = first + offset;
            if position >= self.len {
                break;
            }
            if offset == 0 || position % WINDOW == 0 {
                self.touch(window_first(position));
            }
            self.ready.insert(position, Arc::new(row));
            end = position + 1;
        }
        self.evict();
        first..end
    }

    /// Windows of live items without a current row: placeholders and kept rows.
    fn cold_windows(&mut self) -> Vec<usize> {
        let mut windows = Vec::new();
        self.objects.retain(|position, item| {
            if item.upgrade().is_none() {
                return false;
            }
            let window = window_first(*position);
            if !self.ready.contains_key(position) || self.stale.contains(&window) {
                windows.push(window);
            }
            true
        });
        windows.sort_unstable();
        windows.dedup();
        windows
    }

    /// Queues the windows `demanded`; true when the running read is no longer wanted.
    fn want(&mut self, demanded: Vec<usize>) -> bool {
        self.failed.retain(|window| demanded.contains(window));
        let cancelled = self
            .active
            .is_some_and(|active| !demanded.contains(&active));
        if cancelled {
            self.active = None;
            if let Some(cancel) = self.running.take() {
                cancel.cancel();
            }
        }
        self.pending
            .retain(|window| demanded.contains(window) && self.active != Some(*window));
        for window in demanded {
            if self.pending.len() == PENDING_CAP {
                break;
            }
            if self.active != Some(window)
                && !self.pending.contains(&window)
                && !self.failed.contains(&window)
            {
                self.pending.push_back(window);
            }
        }
        cancelled
    }

    fn reset_reads(&mut self) {
        if let Some(cancel) = self.running.take() {
            cancel.cancel();
        }
        self.active = None;
        self.pending.clear();
        self.failed.clear();
    }
}

mod imp {
    use super::*;
    use gio::subclass::prelude::*;

    #[derive(Default)]
    pub struct RowList {
        pub(super) state: RefCell<Option<Box<dyn AnyRows>>>,
        pub(super) runtime: RefCell<Option<tokio::runtime::Handle>>,
        pub(super) demand: RefCell<Option<glib::JoinHandle<()>>>,
        pub(super) start: RefCell<Option<Rc<dyn Fn(&super::RowList)>>>,
        pub(super) ready_handler: RefCell<Option<Rc<dyn Fn(u32, u32)>>>,
        pub(super) sections: RefCell<Vec<(u32, i64)>>,
        pub(super) collection_selection:
            RefCell<Option<crate::widgets::selection::CollectionSelection>>,
        pub(super) reread: RefCell<Option<(ReadCancellation, tokio::task::AbortHandle)>>,
        pub(super) reread_generation: Cell<u64>,
        /// The committed changes its rows show.
        pub(super) shown: rufin_core::shown::ShownChanges,
        /// Handles what a read of its rows missed, as the page that shows them handles any
        /// committed change.
        pub(super) missed: RefCell<Option<Rc<dyn Fn(&rufin_core::source::feed::Changed)>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RowList {
        const NAME: &'static str = "RufinRowList";
        type Type = super::RowList;
        type Interfaces = (gio::ListModel,);
    }

    impl ObjectImpl for RowList {
        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: std::sync::OnceLock<Vec<glib::subclass::Signal>> =
                std::sync::OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    glib::subclass::Signal::builder("sections-changed")
                        .param_types([u32::static_type(), u32::static_type()])
                        .build(),
                ]
            })
        }

        fn dispose(&self) {
            if let Some(demand) = self.demand.borrow_mut().take() {
                demand.abort();
            }
            if let Some((cancel, task)) = self.reread.borrow_mut().take() {
                cancel.cancel();
                task.abort();
            }
            self.start.borrow_mut().take();
            self.collection_selection.borrow_mut().take();
        }
    }

    impl ListModelImpl for RowList {
        fn item_type(&self) -> glib::Type {
            RowItem::static_type()
        }

        fn n_items(&self) -> u32 {
            self.state
                .borrow()
                .as_ref()
                .map_or(0, |state| state.len().min(u32::MAX as usize) as u32)
        }

        fn item(&self, position: u32) -> Option<glib::Object> {
            let item = self.state.borrow_mut().as_mut()?.item(position as usize)?;
            self.obj().schedule_demand();
            Some(item.upcast())
        }
    }
}

glib::wrapper! {
    /// The GTK list model of a [`Rows`].
    pub struct RowList(ObjectSubclass<imp::RowList>) @implements gio::ListModel;
}

impl RowList {
    pub fn section(&self, position: u32) -> (u32, u32) {
        use glib::subclass::prelude::ObjectSubclassIsExt;
        let len = self.n_items();
        if position >= len {
            return (len, u32::MAX);
        }
        let sections = self.imp().sections.borrow();
        let next = sections.partition_point(|(start, _)| *start <= position);
        let start = next.checked_sub(1).map_or(0, |index| sections[index].0);
        let end = sections.get(next).map_or(len, |section| section.0);
        (start, end)
    }

    pub fn section_value(&self, start: u32) -> Option<i64> {
        use glib::subclass::prelude::ObjectSubclassIsExt;
        let sections = self.imp().sections.borrow();
        sections
            .binary_search_by_key(&start, |section| section.0)
            .ok()
            .map(|index| sections[index].1)
    }

    pub fn connect_sections_changed(
        &self,
        handler: impl Fn(u32, u32) + 'static,
    ) -> glib::SignalHandlerId {
        self.connect_local("sections-changed", false, move |values| {
            handler(
                values[1].get().expect("section position"),
                values[2].get().expect("section count"),
            );
            None
        })
    }

    fn set_sections(&self, sections: Vec<(u32, i64)>) {
        use glib::subclass::prelude::ObjectSubclassIsExt;
        if *self.imp().sections.borrow() == sections {
            return;
        }
        self.imp().sections.replace(sections);
        if self.n_items() > 0 {
            self.emit_by_name::<()>("sections-changed", &[&0u32, &self.n_items()]);
        }
    }

    pub(crate) fn set_collection_selection(
        &self,
        capture: crate::widgets::selection::CollectionSelection,
    ) {
        use glib::subclass::prelude::ObjectSubclassIsExt;
        self.imp().collection_selection.replace(Some(capture));
    }

    pub(crate) fn collection_selection(
        &self,
        target: &crate::widgets::selection::PlayTarget,
        positions: &gtk::Bitset,
    ) -> Option<crate::widgets::media_drag::CollectionTargets> {
        use glib::subclass::prelude::ObjectSubclassIsExt;
        self.imp().collection_selection.borrow().as_ref()?(target, positions)
    }

    /// Reconciles the windows on screen once GTK finishes requesting and retiring items.
    fn schedule_demand(&self) {
        use glib::subclass::prelude::ObjectSubclassIsExt;
        if self.imp().demand.borrow().is_some() {
            return;
        }
        let list = self.downgrade();
        self.imp().demand.replace(Some(
            glib::MainContext::ref_thread_default().spawn_local_with_priority(
                glib::Priority::DEFAULT_IDLE,
                async move {
                    let Some(list) = list.upgrade() else { return };
                    list.imp().demand.borrow_mut().take();
                    let start = list.imp().start.borrow().clone();
                    if let Some(start) = start {
                        start(&list);
                    }
                },
            ),
        ));
    }

    /// Handles a change a read of its rows missed through [`Rows::on_missed`].
    fn show_missed(&self, missed: &rufin_core::source::feed::Changed) {
        use glib::subclass::prelude::ObjectSubclassIsExt;
        let handler = self.imp().missed.borrow().clone();
        if let Some(handler) = handler {
            handler(missed);
        }
    }

    fn emit_ready_changed(&self, position: usize, count: usize) {
        use glib::subclass::prelude::ObjectSubclassIsExt;
        let handler = self.imp().ready_handler.borrow().clone();
        if let Some(handler) = handler {
            handler(position as u32, count as u32);
        }
    }
}

/// A typed handle on a [`RowList`].
pub struct Rows<T> {
    list: RowList,
    row: PhantomData<T>,
}

impl<T> Clone for Rows<T> {
    fn clone(&self) -> Self {
        Self {
            list: self.list.clone(),
            row: PhantomData,
        }
    }
}

impl<T: Clone + PartialEq + Send + Sync + 'static> Rows<T> {
    fn with_source(source: Source<T>, len: usize, runtime: Option<tokio::runtime::Handle>) -> Self {
        use glib::subclass::prelude::ObjectSubclassIsExt;
        let list: RowList = glib::Object::new();
        list.imp().state.replace(Some(Box::new(State::<T> {
            source,
            len,
            ready: BTreeMap::new(),
            windows: VecDeque::new(),
            stale: BTreeSet::new(),
            objects: BTreeMap::new(),
            pending: VecDeque::new(),
            active: None,
            failed: Vec::new(),
            deferred: false,
            running: None,
        })));
        list.imp().runtime.replace(runtime);
        list.imp().start.replace(Some(Rc::new(|list: &RowList| {
            Self::from_list(list).start(true);
        })));
        Self::from_list(&list)
    }

    fn from_list(list: &RowList) -> Self {
        Self::of(list)
    }

    /// The typed handle on `list`, which holds rows of `T`.
    pub fn of(list: &RowList) -> Self {
        Self {
            list: list.clone(),
            row: PhantomData,
        }
    }

    /// A list of `len` rows read a page at a time by `reader`.
    pub fn paged(runtime: tokio::runtime::Handle, len: usize, reader: PageReader<T>) -> Self {
        Self::with_source(Source::Pages(reader), len, Some(runtime))
    }

    /// A list of every row it shows, outside a page.
    pub fn seeded(rows: Vec<T>) -> Self {
        let list = Self::with_source(Source::Seeded, rows.len(), None);
        list.with(|state| state.seed(0, rows));
        list
    }

    fn with<R>(&self, apply: impl FnOnce(&mut State<T>) -> R) -> R {
        use glib::subclass::prelude::ObjectSubclassIsExt;
        let mut state = self.list.imp().state.borrow_mut();
        let state = state
            .as_mut()
            .and_then(|state| state.as_any().downcast_mut::<State<T>>())
            .expect("a row list keeps the row type it was made with");
        apply(state)
    }

    pub fn list_model(&self) -> RowList {
        self.list.clone()
    }

    pub fn len(&self) -> usize {
        self.with(|state| state.len)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The read row at `position`, without asking for it.
    pub fn ready(&self, position: usize) -> Option<Arc<T>> {
        self.with(|state| state.ready.get(&position).cloned())
    }

    /// The first read row `matches` accepts.
    pub fn ready_position(&self, matches: impl Fn(&T) -> bool) -> Option<u32> {
        self.with(|state| {
            state
                .ready
                .iter()
                .find_map(|(position, row)| matches(row).then_some(*position as u32))
        })
    }

    /// The first row on screen.
    pub fn first_live_position(&self) -> Option<usize> {
        self.with(|state| {
            state
                .objects
                .iter()
                .find(|(_, item)| item.upgrade().is_some())
                .map(|(position, _)| *position)
        })
    }

    /// Where a reread starts so the rows on screen stay: the first live row.
    pub fn live_window(&self) -> library::RouteSeedWindow {
        self.first_live_position().map_or_else(
            library::RouteSeedWindow::top,
            library::RouteSeedWindow::position,
        )
    }

    pub fn connect_ready_changed(&self, handler: impl Fn(u32, u32) + 'static) {
        use glib::subclass::prelude::ObjectSubclassIsExt;
        self.list
            .imp()
            .ready_handler
            .replace(Some(Rc::new(handler)));
    }

    /// Hands what a read of the rows missed to `handler`: the page that shows them handles it
    /// as any committed change.
    pub fn on_missed(&self, handler: Rc<dyn Fn(&rufin_core::source::feed::Changed)>) {
        use glib::subclass::prelude::ObjectSubclassIsExt;
        self.list.imp().missed.replace(Some(handler));
    }

    /// Updates the read rows `matches` accepts, and the items that show them.
    pub fn update(&self, matches: impl Fn(&T) -> bool, update: impl Fn(&mut T)) {
        let changed = self.with(|state| {
            let mut changed = Vec::new();
            for (position, row) in &mut state.ready {
                if matches(row) {
                    update(Arc::make_mut(row));
                    let item = state.objects.get(position).and_then(glib::WeakRef::upgrade);
                    changed.push((*position, item, Arc::clone(row)));
                }
            }
            changed
        });
        for (position, item, row) in changed {
            if let Some(item) = item {
                item.replace(row, true);
            }
            self.list.emit_ready_changed(position, 1);
        }
    }

    /// Shows `row` at `position` when that row is read and differs.
    pub fn set_ready(&self, position: usize, row: T) {
        let changed = self.with(|state| {
            let shown = state.ready.get_mut(&position)?;
            if **shown == row {
                return None;
            }
            *shown = Arc::new(row);
            let item = state
                .objects
                .get(&position)
                .and_then(glib::WeakRef::upgrade);
            Some((item, Arc::clone(shown)))
        });
        if let Some((item, row)) = changed {
            if let Some(item) = item {
                item.replace(row, true);
            }
            self.list.emit_ready_changed(position, 1);
        }
    }

    /// Asks for the windows of `positions`, such as grid cells being laid out.
    pub fn demand_positions(&self, positions: impl IntoIterator<Item = usize>) {
        let windows = positions.into_iter().map(window_first).collect();
        self.with(|state| state.want(windows));
        self.start(false);
    }

    /// Stops reading until rows are asked for again.
    pub fn cancel(&self) {
        use glib::subclass::prelude::ObjectSubclassIsExt;
        if let Some(demand) = self.list.imp().demand.borrow_mut().take() {
            demand.abort();
        }
        self.with(State::reset_reads);
    }

    /// Starts reading once the route shows the window it was seeded at.
    pub fn resume_initial_demand(&self) {
        if self.with(|state| std::mem::replace(&mut state.deferred, false)) {
            self.start(true);
        }
    }

    /// Takes a new order of `len` rows and its rows from `first`. A `refresh` reads the same
    /// list again, so its items and the rows they show stay by position until their windows
    /// are read again, and only its tail changes; another order changes every item.
    pub fn replace(&self, len: usize, first: usize, rows: Vec<T>, refresh: bool) {
        let old_len = self.list.n_items() as usize;
        let updates = self.with(|state| {
            state.reset_reads();
            let previous = std::mem::take(&mut state.ready);
            state.windows.clear();
            state.stale.clear();
            state.len = len;
            let seeded = state.seed(first, rows);
            if !refresh {
                state.objects.clear();
                return Vec::new();
            }
            for (position, row) in previous {
                let window = window_first(position);
                if position >= len || seeded.contains(&position) || !state.live(window) {
                    continue;
                }
                if state.stale.insert(window) {
                    state.touch(window);
                }
                state.ready.insert(position, row);
            }
            state
                .objects
                .retain(|position, item| *position < len && item.upgrade().is_some());
            state
                .objects
                .iter()
                .filter_map(|(position, item)| {
                    let item = item.upgrade()?;
                    let row = state.ready.get(position).cloned();
                    let shown = item.value::<Arc<T>>();
                    (shown.as_deref() != row.as_deref()).then_some((*position, item, row))
                })
                .collect::<Vec<_>>()
        });
        if !refresh {
            self.list.items_changed(0, old_len as u32, len as u32);
            return;
        }
        let common = old_len.min(len);
        if old_len != len {
            self.list.items_changed(
                common as u32,
                (old_len - common) as u32,
                (len - common) as u32,
            );
        }
        for (position, item, row) in updates {
            match row {
                Some(row) => item.replace(row, true),
                None => item.replace((), false),
            }
            self.list.emit_ready_changed(position, 1);
        }
        self.start(true);
    }

    /// Reads the wanted windows one at a time; `reconcile` first takes the windows on screen.
    fn start(&self, reconcile: bool) {
        use glib::subclass::prelude::ObjectSubclassIsExt;
        let Some(runtime) = self.list.imp().runtime.borrow().clone() else {
            return;
        };
        let read = self.with(|state| {
            if state.deferred {
                return None;
            }
            if reconcile {
                let cold = state.cold_windows();
                state.want(cold);
            }
            if state.active.is_some() {
                return None;
            }
            loop {
                let first = state.pending.pop_front()?;
                let end = (first + WINDOW).min(state.len);
                if first >= end
                    || !state.stale.contains(&first)
                        && (first..end).all(|position| state.ready.contains_key(&position))
                {
                    continue;
                }
                state.active = Some(first);
                let cancel = ReadCancellation::new();
                state.running = Some(cancel.clone());
                let reading = match &state.source {
                    Source::View { view, .. } => Some(view.owner().reading()),
                    _ => None,
                };
                return Some((first..end, cancel, read_window(&state.source), reading));
            }
        });
        let Some((range, cancel, reader, reading)) = read else {
            return;
        };
        let task = runtime.spawn(reader(range.clone(), cancel.clone()));
        let list = self.list.downgrade();
        glib::spawn_future_local(async move {
            let rows = task.await.ok().and_then(Result::ok);
            let Some(list) = list.upgrade() else { return };
            let this = Self::from_list(&list);
            let accepted = this.with(|state| {
                // Every read that stopped being the active one was cancelled, so a read of the
                // same window started after it cannot be taken for it.
                if cancel.is_cancelled() {
                    return None;
                }
                state.active = None;
                state.running = None;
                let Some(rows) = rows.filter(|rows| rows.len() == range.len()) else {
                    state.failed.push(range.start);
                    return None;
                };
                state.touch(range.start);
                state.stale.remove(&range.start);
                let mut changed = Vec::new();
                for (position, row) in range.clone().zip(rows) {
                    if state
                        .ready
                        .get(&position)
                        .is_some_and(|shown| **shown == row)
                    {
                        continue;
                    }
                    let row = Arc::new(row);
                    state.ready.insert(position, Arc::clone(&row));
                    let item = state
                        .objects
                        .get(&position)
                        .and_then(glib::WeakRef::upgrade);
                    changed.push((position, item, row));
                }
                state.evict();
                Some(changed)
            });
            if let Some(changed) = accepted {
                let span = changed
                    .first()
                    .zip(changed.last())
                    .map(|(first, last)| first.0..last.0 + 1);
                for (_, item, row) in changed {
                    if let Some(item) = item {
                        item.replace(row, true);
                    }
                }
                if let Some(span) = span {
                    list.emit_ready_changed(span.start, span.len());
                }
                // A change committed while the window read reaches its rows too.
                if let Some(missed) = reading.and_then(rufin_core::shown::Reading::missed) {
                    list.show_missed(&missed);
                }
            }
            this.start(true);
        });
    }
}

/// The read of rows `range` from `source`.
#[allow(clippy::type_complexity)]
fn read_window<T: Send + 'static>(
    source: &Source<T>,
) -> Box<
    dyn FnOnce(
        Range<usize>,
        ReadCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<T>, String>> + Send>>,
> {
    match source {
        Source::View {
            view,
            favorites,
            read,
        } => {
            let (view, favorites, read) = (Arc::clone(view), *favorites, *read);
            Box::new(move |range, cancel| read(view, favorites, range, cancel))
        }
        Source::Pages(reader) => {
            let reader = Rc::clone(reader);
            Box::new(move |range, cancel| reader(range, cancel))
        }
        Source::Seeded => Box::new(|_, _| Box::pin(async { Ok(Vec::new()) })),
    }
}

fn read_view_rows(
    view: Arc<View>,
    favorites: bool,
    range: Range<usize>,
    cancel: ReadCancellation,
) -> Pin<Box<dyn Future<Output = Result<Vec<Row>, String>> + Send>> {
    Box::pin(async move {
        if favorites {
            view.favorite_rows(range, &cancel).await
        } else {
            view.rows(range, &cancel).await
        }
        .map_err(|error| error.to_string())
    })
}

impl Rows<Row> {
    /// The rows of the View `read` opened, seeded with the window it read.
    pub fn over_view(runtime: tokio::runtime::Handle, read: &mut Opened) -> Self {
        let rows = Self::with_source(
            Source::View {
                view: Arc::clone(&read.view),
                favorites: false,
                read: read_view_rows,
            },
            read.view.len(),
            Some(runtime),
        );
        rows.with(|state| {
            state.seed(read.first, std::mem::take(&mut read.rows));
            state.deferred = read.first > 0;
        });
        rows.list.set_sections(read.view.disc_sections().to_vec());
        rows
    }

    /// An artist page's favorite tracks, seeded with the first ones `rows`.
    pub fn favorites(runtime: tokio::runtime::Handle, view: &Arc<View>, rows: Vec<Row>) -> Self {
        let list = Self::with_source(
            Source::View {
                view: Arc::clone(view),
                favorites: true,
                read: read_view_rows,
            },
            view.favorites_len().unwrap_or(0),
            Some(runtime),
        );
        list.with(|state| state.seed(0, rows));
        list
    }

    /// The View the rows come from; a seeded list has none.
    pub fn view(&self) -> Option<Arc<View>> {
        self.with(|state| match &state.source {
            Source::View { view, .. } => Some(Arc::clone(view)),
            _ => None,
        })
    }

    /// The context id a play of the collection these rows list carries from elsewhere, and the
    /// one plays from these rows carry, tied to their order.
    pub fn context_ids(&self) -> (String, String) {
        self.with(|state| match &state.source {
            Source::View {
                view,
                favorites: true,
                ..
            } => {
                let context = view.favorites_context_id().unwrap_or_default();
                let base = context
                    .split_once("|query=")
                    .map_or(context.as_str(), |(base, _)| base)
                    .to_owned();
                (base, view.with_read(context))
            }
            Source::View { view, .. } => (
                rufin_core::view::context_base(view.route()),
                view.play_context(),
            ),
            Source::Seeded | Source::Pages(_) => (String::new(), String::new()),
        })
    }

    /// Plays row `index` as core's row rule decides: on a page through [`View::play`], else
    /// what [`PlayTarget::of_row`] plays outside a page.
    ///
    /// [`PlayTarget::of_row`]: rufin_core::view::PlayTarget::of_row
    pub fn play(
        &self,
        products: &rufin_core::runtime::ProductHandles,
        index: usize,
        placement: rufin_core::playback::QueuePlacement,
        shuffled: bool,
    ) {
        // The row as it shows anchors the play, wherever the current order has it.
        let Some(row) = self.ready(index) else {
            return;
        };
        let Some((view, favorites)) = self.with(|state| match &state.source {
            Source::View {
                view, favorites, ..
            } => Some((Arc::clone(view), *favorites)),
            _ => None,
        }) else {
            if let Some(target) = rufin_core::view::PlayTarget::of_row(&row) {
                crate::menus::media_menus::play_target(products, &target, placement, shuffled);
            }
            return;
        };
        if favorites {
            if let Row::Track(track) = row.as_ref()
                && let Some(request) =
                    view.play_favorite(track.track_key, index, placement, shuffled)
            {
                products.playback.play_selection(request);
            }
            return;
        }
        let playback = Arc::clone(&products.playback);
        products.runtime.spawn(async move {
            let cancel = library::ReadCancellation::new();
            match view.play(&row, index, placement, shuffled, &cancel).await {
                Ok(Some(request)) => playback.play_selection(request),
                Ok(None) => {}
                Err(error) => tracing::warn!(%error, "could not play the row"),
            }
        });
    }

    /// Plays or queues everything the list shows, as a page header does.
    pub fn play_all(
        &self,
        playback: &rufin_core::playback::PlaybackOwner,
        placement: rufin_core::playback::QueuePlacement,
        shuffled: bool,
    ) {
        if let Some(request) = self
            .view()
            .and_then(|view| view.play_all(placement, shuffled))
        {
            playback.play_selection(request);
        }
    }

    /// Where the current track shows: at `rank` when it is there, or where it is read.
    pub fn position_of(&self, media_uri: &str, rank: Option<usize>) -> Option<u32> {
        fn uri(row: &Row) -> Option<&str> {
            match row {
                Row::Track(track) | Row::AlbumTrack { track, .. } => Some(&track.media_uri),
                _ => None,
            }
        }
        rank.filter(|rank| {
            self.ready(*rank)
                .is_some_and(|row| uri(&row) == Some(media_uri))
        })
        .map(|rank| rank as u32)
        .or_else(|| self.ready_position(|row| uri(row) == Some(media_uri)))
    }

    /// Takes the View `read` read again over the one shown, and its rows.
    pub fn take(&self, read: &mut Opened) {
        let (previous, favorites) = self.with(|state| match &mut state.source {
            Source::View {
                view, favorites, ..
            } => (std::mem::replace(view, Arc::clone(&read.view)), *favorites),
            _ => unreachable!("only a View's rows read it again"),
        });
        let same = read.view.same_list(&previous);
        if favorites {
            let rows = read.favorites.take().unwrap_or_default();
            self.replace(read.view.favorites_len().unwrap_or(0), 0, rows, same);
        } else {
            let rows = std::mem::take(&mut read.rows);
            self.replace(read.view.len(), read.first, rows, same);
            self.list.set_sections(read.view.disc_sections().to_vec());
        }
    }

    /// Reads the page again with `options` from `window`. The latest request wins; when it is
    /// read the rows take it and `apply` gets the rest, such as its header.
    pub fn reread(
        &self,
        read: Reread,
        options: OpenOptions,
        window: library::RouteSeedWindow,
        apply: impl FnOnce(Opened) + 'static,
    ) {
        use glib::subclass::prelude::ObjectSubclassIsExt;
        let (Some(view), Some(runtime)) = (self.view(), self.list.imp().runtime.borrow().clone())
        else {
            return;
        };
        let imp = self.list.imp();
        if let Some((cancel, task)) = imp.reread.borrow_mut().take() {
            cancel.cancel();
            task.abort();
        }
        let generation = imp.reread_generation.get().wrapping_add(1);
        imp.reread_generation.set(generation);
        let cancel = ReadCancellation::new();
        let reading = view.owner().reading();
        let task = runtime.spawn({
            let cancel = cancel.clone();
            async move {
                match read {
                    Reread::Page => view.reread(options, window, &cancel).await,
                    Reread::Releases => view.reread_releases(options, window, &cancel).await,
                }
            }
        });
        imp.reread.replace(Some((cancel, task.abort_handle())));
        let list = self.list.downgrade();
        glib::spawn_future_local(async move {
            let result = task.await;
            let Some(list) = list.upgrade() else { return };
            if list.imp().reread_generation.get() != generation {
                return;
            }
            list.imp().reread.borrow_mut().take();
            match result {
                Ok(Ok(mut opened)) => {
                    Self::from_list(&list).take(&mut opened);
                    apply(opened);
                    // A change committed while it read reaches the rows read too.
                    if let Some(missed) = reading.missed() {
                        list.show_missed(&missed);
                    }
                }
                Ok(Err(library::LibraryError::ReadCancelled)) | Err(_) => {}
                Ok(Err(error)) => tracing::warn!(%error, "failed to read page again"),
            }
        });
    }

    /// Reads the page again for a committed change with `options`: where it is, unless their
    /// search moved since the read it shows.
    pub fn refresh(&self, options: OpenOptions, apply: impl FnOnce(Opened) + 'static) {
        let Some(view) = self.view() else { return };
        let window = if options.filter == view.options().filter {
            self.live_window()
        } else {
            library::RouteSeedWindow::top()
        };
        self.reread(Reread::Page, options, window, apply);
    }

    /// Reads again what changed `settings` change.
    pub fn resume(
        &self,
        settings: &rufin_core::settings::app::Settings,
        apply: impl FnOnce(Opened) + 'static,
    ) {
        let Some(view) = self.view() else { return };
        if let Some(read) = view.settings_reread(settings) {
            self.reread(
                read,
                view.options().clone(),
                library::RouteSeedWindow::top(),
                apply,
            );
        }
    }

    /// Shows a committed change in the rows read so far, through core's
    /// [`rufin_core::shown::ShownChanges`]: favorites and ratings at once, downloads once read.
    pub fn show_changes(
        &self,
        owner: &rufin_core::source::SourceOwner,
        changed: &rufin_core::source::feed::Changed,
    ) {
        use glib::subclass::prelude::ObjectSubclassIsExt;
        use library::ShowsState;
        let view = self.view();
        let (read, shown) = self.with(|state| {
            let read = state.ready.keys().copied().collect::<Vec<_>>();
            let rows = state.ready.values().map(Arc::as_ref).collect::<Vec<_>>();
            let shown = self
                .list
                .imp()
                .shown
                .show(owner, &rows, changed, view.as_deref());
            (read, shown)
        });
        for (index, row) in shown.states {
            self.set_ready(read[index], row);
        }
        let Some(downloads) = shown.downloads else {
            return;
        };
        let list = self.list.downgrade();
        glib::spawn_future_local(async move {
            let Some(rows) = downloads.await else { return };
            let Some(list) = list.upgrade() else { return };
            let shown = Self::from_list(&list);
            // The downloads go into the row now at each position when it is the same row.
            for (index, downloads) in rows {
                let position = read[index];
                let Some(current) = shown.ready(position) else {
                    continue;
                };
                let mut merged = Row::clone(&current);
                if merged.show_downloads(downloads) {
                    shown.set_ready(position, merged);
                }
            }
        });
    }

    /// The tracks at `positions`, for selections; a folder's child folders come first.
    pub fn selection(&self, positions: &gtk::Bitset) -> library::MemberSelection {
        let view = self.view();
        let folders = view
            .as_ref()
            .and_then(|view| view.folder_len())
            .unwrap_or(0);
        let favorites = self.with(|state| {
            matches!(
                state.source,
                Source::View {
                    favorites: true,
                    ..
                }
            )
        });
        let ranges = crate::widgets::selection::selected_ranges(positions)
            .into_iter()
            .filter_map(|range| {
                let start = range.start.max(folders) - folders;
                (range.end > folders).then_some(start..range.end - folders)
            })
            .collect::<Vec<_>>();
        let read = view.and_then(|view| {
            if favorites {
                view.favorites().cloned()
            } else {
                view.members().cloned()
            }
        });
        match read {
            Some(read) => library::MemberSelection::Members {
                read,
                ranges: Some(ranges),
            },
            None => library::MemberSelection::Keys {
                source: None,
                keys: self.with(|state| {
                    ranges
                        .iter()
                        .flat_map(|range| range.clone())
                        .filter_map(|position| match state.ready.get(&position)?.as_ref() {
                            Row::Track(track) | Row::AlbumTrack { track, .. } => {
                                Some(track.track_key)
                            }
                            _ => None,
                        })
                        .collect()
                }),
            },
        }
    }
}
