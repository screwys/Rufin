//! Owners that follow the change feed, and the features that run only while they are wanted.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use library::{Catalog, Change, Scope};

use crate::settings::{SettingsFile, StoredSettings};

/// Runs `pass` on the Catalog's changes (see `Catalog::follow`) while its owner lives.
pub(crate) fn follow<T, Pass, Done>(
    runtime: &tokio::runtime::Handle,
    catalog: &Catalog,
    owner: Weak<T>,
    mut pass: Pass,
) -> tokio::task::AbortHandle
where
    T: Send + Sync + 'static,
    Pass: FnMut(Arc<T>, Option<Vec<Arc<Change>>>) -> Done + Send + 'static,
    Done: Future<Output = ()> + Send,
{
    runtime
        .spawn(catalog.follow(move |changes| {
            let done = owner.upgrade().map(|owner| pass(owner, changes));
            async move {
                let Some(done) = done else {
                    return false;
                };
                done.await;
                true
            }
        }))
        .abort_handle()
}

pub(crate) type Pass = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Work that runs only while the settings and the Catalog want it.
pub(crate) trait Feature: Send + Sync + 'static {
    /// Whether it runs.
    fn wanted(&self, settings: &StoredSettings, catalog: &Catalog) -> bool;
    /// Whether a change of this scope concerns it: while it runs it hears the change, and the
    /// change may decide again whether it runs.
    fn hears(&self, scope: &Scope) -> bool;
    /// One pass over the changes it hears, or a full one when it starts, after a lag and on
    /// each `period`.
    fn apply(self: Arc<Self>, changes: Option<Vec<Arc<Change>>>) -> Pass;
    /// How often it makes a full pass while it runs.
    fn period(&self) -> Option<Duration> {
        None
    }
    /// It stopped running.
    fn stopped(&self) {}
}

/// Starts each feature while it is wanted and stops it when it is not. The committed settings
/// and the Catalog's changes a feature hears decide again.
pub(crate) fn run(
    runtime: &tokio::runtime::Handle,
    settings: &SettingsFile,
    catalog: &Catalog,
    features: Vec<Arc<dyn Feature>>,
) {
    let runner = Arc::new(Runner {
        runtime: runtime.clone(),
        settings: settings.clone(),
        catalog: catalog.clone(),
        running: Mutex::new(
            features
                .into_iter()
                .map(|feature| (feature, None))
                .collect(),
        ),
    });
    let heard = Arc::clone(&runner);
    runtime.spawn(catalog.follow(move |changes| {
        heard.decide(|feature| {
            changes
                .as_deref()
                .is_none_or(|changes| hears(feature, changes))
        });
        std::future::ready(true)
    }));
    let mut committed = settings.changes();
    runtime.spawn(async move {
        committed.borrow_and_update();
        while committed.changed().await.is_ok() {
            runner.decide(|_| true);
        }
    });
}

fn hears(feature: &dyn Feature, changes: &[Arc<Change>]) -> bool {
    changes
        .iter()
        .flat_map(|change| &change.scopes)
        .any(|scope| feature.hears(scope))
}

type Running = Vec<(Arc<dyn Feature>, Option<tokio::task::AbortHandle>)>;

struct Runner {
    runtime: tokio::runtime::Handle,
    settings: SettingsFile,
    catalog: Catalog,
    running: Mutex<Running>,
}

impl Runner {
    /// Decides again for the features `concerned` names.
    fn decide(&self, concerned: impl Fn(&dyn Feature) -> bool) {
        let settings = self.settings.load();
        let mut running = self.running.lock().unwrap_or_else(|p| p.into_inner());
        for (feature, task) in running.iter_mut() {
            if !concerned(feature.as_ref()) {
                continue;
            }
            match (feature.wanted(&settings, &self.catalog), task.is_some()) {
                (true, false) => *task = Some(self.start(Arc::clone(feature))),
                (false, true) => {
                    task.take().into_iter().for_each(|task| task.abort());
                    feature.stopped();
                }
                _ => {}
            }
        }
    }

    fn start(&self, feature: Arc<dyn Feature>) -> tokio::task::AbortHandle {
        let period = feature.period();
        let heard = Arc::clone(&feature);
        let follow = self.catalog.follow(move |changes| {
            let pass = changes
                .as_deref()
                .is_none_or(|changes| hears(heard.as_ref(), changes))
                .then(|| Arc::clone(&heard).apply(changes));
            async move {
                if let Some(pass) = pass {
                    pass.await;
                }
                true
            }
        });
        self.runtime
            .spawn(async move {
                let Some(period) = period else {
                    return follow.await;
                };
                let ticks = async {
                    let mut interval =
                        tokio::time::interval_at(tokio::time::Instant::now() + period, period);
                    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    loop {
                        interval.tick().await;
                        Arc::clone(&feature).apply(None).await;
                    }
                };
                tokio::join!(follow, ticks);
            })
            .abort_handle()
    }
}
