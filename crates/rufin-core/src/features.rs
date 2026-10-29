//! One runner for the features that follow the change feed. A feature says when it is wanted,
//! which Changes it hears and what one pass does. The runner hears the feed once for all of them
//! and starts after the Catalog loaded. It installs a feature while it is wanted and hands it the
//! Changes heard since its last pass, or a full pass when it installs, when the feed lagged and
//! when its timer is due. A feature with nothing left to do stops until it is wanted again.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use futures_util::future::BoxFuture;
use library::{Catalog, Change};
use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use tokio::time::Instant;

/// What a feature waits for after a pass.
pub(crate) enum Next {
    /// The Changes it hears; a timer set earlier stays.
    Wait,
    /// The Changes it hears, and a full pass at this time.
    At(Instant),
    /// Nothing; it stops until it is wanted again.
    Done,
}

pub(crate) trait Feature: Send + Sync + 'static {
    /// Whether the feature runs, from settings and Catalog state.
    fn wanted(&self) -> BoxFuture<'_, bool>;

    fn hears(&self, _change: &Change) -> bool {
        false
    }

    /// One pass over the Changes heard since the last one, or a full pass without them.
    fn apply(&self, changes: Option<Vec<Arc<Change>>>) -> BoxFuture<'_, Next>;

    /// The shortest time between two passes; Changes heard meanwhile wait for the next one.
    fn interval(&self) -> Duration {
        Duration::ZERO
    }

    /// The runner stopped the feature, possibly during a pass.
    fn stopped(&self) {}
}

pub(crate) struct Features {
    runtime: tokio::runtime::Handle,
    slots: Mutex<Vec<Arc<Slot>>>,
    started: AtomicBool,
}

/// A feature added to the runner.
#[derive(Clone)]
pub(crate) struct FeatureHandle(Arc<Slot>);

struct Slot {
    feature: Arc<dyn Feature>,
    runner: Weak<Features>,
    state: Mutex<State>,
    wake: tokio::sync::Notify,
    /// Held while `wanted` is asked, so checks apply in order.
    checking: tokio::sync::Mutex<()>,
}

#[derive(Default)]
struct State {
    worker: Option<tokio::task::AbortHandle>,
    full: bool,
    changes: Vec<Arc<Change>>,
}

impl Features {
    pub(crate) fn new(catalog: &Catalog, runtime: tokio::runtime::Handle) -> Arc<Self> {
        let features = Arc::new(Self {
            runtime,
            slots: Mutex::new(Vec::new()),
            started: AtomicBool::new(false),
        });
        let changes = catalog.changes();
        features
            .runtime
            .spawn(hear(Arc::downgrade(&features), catalog.clone(), changes));
        features
    }

    pub(crate) fn add(self: &Arc<Self>, feature: Arc<dyn Feature>) -> FeatureHandle {
        let slot = Arc::new(Slot {
            feature,
            runner: Arc::downgrade(self),
            state: Mutex::new(State::default()),
            wake: tokio::sync::Notify::new(),
            checking: tokio::sync::Mutex::new(()),
        });
        lock(&self.slots).push(Arc::clone(&slot));
        let handle = FeatureHandle(slot);
        handle.check();
        handle
    }

    /// Stops every feature for good.
    pub(crate) fn close(&self) {
        self.started.store(false, Ordering::Release);
        for slot in lock(&self.slots).iter() {
            slot.stop();
        }
    }

    fn slots(&self) -> Vec<Arc<Slot>> {
        lock(&self.slots).clone()
    }
}

async fn hear(
    runner: Weak<Features>,
    catalog: Catalog,
    mut changes: tokio::sync::broadcast::Receiver<Arc<Change>>,
) {
    catalog.loaded().await;
    drop(catalog);
    // The full pass each feature makes when it installs covers what committed before.
    while !matches!(
        changes.try_recv(),
        Err(TryRecvError::Empty | TryRecvError::Closed)
    ) {}
    {
        let Some(runner) = runner.upgrade() else {
            return;
        };
        runner.started.store(true, Ordering::Release);
        for slot in runner.slots() {
            FeatureHandle(slot).check();
        }
    }
    loop {
        let next = changes.recv().await;
        let Some(runner) = runner.upgrade() else {
            return;
        };
        if !runner.started.load(Ordering::Acquire) {
            continue;
        }
        match next {
            Ok(change) => {
                for slot in runner.slots() {
                    if slot.feature.hears(&change) {
                        let mut state = slot.state();
                        if state.worker.is_some() {
                            state.changes.push(Arc::clone(&change));
                            drop(state);
                            slot.wake.notify_one();
                        } else {
                            drop(state);
                            FeatureHandle(slot).check();
                        }
                    }
                }
            }
            Err(RecvError::Lagged(_)) => {
                for slot in runner.slots() {
                    FeatureHandle(slot).wake();
                }
            }
            Err(RecvError::Closed) => return,
        }
    }
}

impl FeatureHandle {
    /// Installs the feature when it became wanted and stops it when it is no longer.
    pub(crate) fn check(&self) {
        let Some(runner) = self.0.runner.upgrade() else {
            return;
        };
        if !runner.started.load(Ordering::Acquire) {
            return;
        }
        let slot = Arc::clone(&self.0);
        runner.runtime.spawn(async move {
            let _checking = slot.checking.lock().await;
            let wanted = slot.feature.wanted().await;
            let Some(runner) = slot.runner.upgrade() else {
                return;
            };
            if !runner.started.load(Ordering::Acquire) {
                return;
            }
            if !wanted {
                slot.stop();
                return;
            }
            let mut state = slot.state();
            if state.worker.is_none() {
                state.full = true;
                state.changes.clear();
                state.worker = Some(runner.runtime.spawn(work(Arc::clone(&slot))).abort_handle());
            }
        });
    }

    /// Starts the feature over with a full pass, or stops it when it is no longer wanted.
    pub(crate) fn restart(&self) {
        self.0.stop();
        self.check();
    }

    /// A full pass soon: at once while installed, else after a check.
    pub(crate) fn wake(&self) {
        let mut state = self.0.state();
        if state.worker.is_some() {
            state.full = true;
            drop(state);
            self.0.wake.notify_one();
        } else {
            drop(state);
            self.check();
        }
    }

    pub(crate) fn installed(&self) -> bool {
        self.0.state().worker.is_some()
    }
}

impl Slot {
    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    fn stop(&self) {
        let mut state = self.state();
        let worker = state.worker.take();
        state.full = false;
        state.changes.clear();
        drop(state);
        if let Some(worker) = worker {
            worker.abort();
            self.feature.stopped();
        }
    }
}

async fn work(slot: Arc<Slot>) {
    let interval = slot.feature.interval();
    let mut due = None;
    loop {
        let changes = loop {
            {
                let mut state = slot.state();
                if std::mem::take(&mut state.full) {
                    state.changes.clear();
                    break None;
                }
                if !state.changes.is_empty() {
                    break Some(std::mem::take(&mut state.changes));
                }
            }
            tokio::select! {
                () = slot.wake.notified() => {}
                () = tokio::time::sleep_until(due.unwrap_or_else(Instant::now)), if due.is_some() => {
                    due = None;
                    break None;
                }
            }
        };
        match slot.feature.apply(changes).await {
            Next::Wait => {}
            Next::At(at) => due = Some(at),
            Next::Done => {
                let mut state = slot.state();
                if !state.full && state.changes.is_empty() {
                    state.worker = None;
                    return;
                }
            }
        }
        if !interval.is_zero() {
            tokio::time::sleep(interval).await;
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
