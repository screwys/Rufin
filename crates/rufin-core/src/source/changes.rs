//! Publishes the Catalog's Changes as the catalog publications clients read today. A source's catalog changes publish at most once a second
//! while a sync commits page after page, and at once when its listing completes. Plays,
//! favorites, ratings, playlists and Home rows publish at once.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Weak};
use std::time::Duration;

use library::{Change, PlaylistKey, Scope, SourceKey, SourcePhase};
use tokio::sync::broadcast::{Receiver, error::RecvError};
use tokio::time::{Instant, sleep_until};

use super::{Shared, SourceOwner};

/// While a source's changes keep coming, clients hear of them at most this often.
const PUBLISH_INTERVAL: Duration = Duration::from_secs(1);

/// What changed for one source, as clients refresh for it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct SourceChanged {
    pub(super) catalog: bool,
    pub(super) playlists: bool,
    pub(super) activity: bool,
    pub(super) home: bool,
}

/// Committed changes that are not published yet.
#[derive(Default)]
pub(super) struct Pending {
    /// Catalog changes of sources, held until their interval passes.
    pub(super) sources: BTreeMap<SourceKey, SourceChanged>,
    /// Sources whose listing completed, published without waiting.
    completed: Vec<SourceKey>,
    pub(super) reloaded: bool,
    pub(super) activity: bool,
    pub(super) playlists: Vec<Option<PlaylistKey>>,
    pub(super) home: Vec<SourceKey>,
    /// Tracks, albums and artists whose shown favorite or rating changed.
    pub(super) states: Vec<library::StateChange>,
}

impl Pending {
    fn add(&mut self, change: &Change) {
        for scope in &change.scopes {
            match scope {
                Scope::Reload => self.reloaded = true,
                Scope::Tracks {
                    source: Some(source),
                    ..
                }
                | Scope::Collections { source } => self.source(*source).catalog = true,
                Scope::Source { key, phase } => match phase {
                    SourcePhase::Removed => {}
                    SourcePhase::Completed => {
                        self.source(*key).catalog = true;
                        self.completed.push(*key);
                    }
                    SourcePhase::Added | SourcePhase::Changed => self.source(*key).catalog = true,
                },
                Scope::Listened { .. } => self.activity = true,
                Scope::Playlist { key, .. } => self.playlists.push(Some(*key)),
                Scope::Playlists | Scope::Smart { .. } | Scope::Folders => {
                    self.playlists.push(None)
                }
                Scope::Home { source } => self.home.push(*source),
                Scope::State { items } => self.states.extend(items.iter().cloned()),
                Scope::Tracks { source: None, .. }
                | Scope::Rekey { .. }
                | Scope::Queue
                | Scope::Downloads { .. }
                | Scope::Lyrics { .. } => {}
            }
        }
    }

    fn source(&mut self, source: SourceKey) -> &mut SourceChanged {
        self.sources.entry(source).or_default()
    }

    /// When the next held source may publish.
    fn deadline(&self, published: &HashMap<SourceKey, Instant>) -> Option<Instant> {
        self.sources
            .keys()
            .map(|source| {
                published
                    .get(source)
                    .map_or_else(Instant::now, |last| *last + PUBLISH_INTERVAL)
            })
            .min()
    }

    /// Takes what may publish now: everything but the catalog changes of sources that
    /// published less than an interval ago and did not complete since.
    fn take_ready(&mut self, published: &HashMap<SourceKey, Instant>, now: Instant) -> Self {
        let completed = std::mem::take(&mut self.completed);
        let (held, sources) =
            std::mem::take(&mut self.sources)
                .into_iter()
                .partition(|(source, _)| {
                    !completed.contains(source)
                        && published
                            .get(source)
                            .is_some_and(|last| now < *last + PUBLISH_INTERVAL)
                });
        self.sources = held;
        Self {
            sources,
            completed: Vec::new(),
            reloaded: std::mem::take(&mut self.reloaded),
            activity: std::mem::take(&mut self.activity),
            playlists: std::mem::take(&mut self.playlists),
            home: std::mem::take(&mut self.home),
            states: std::mem::take(&mut self.states),
        }
    }

    fn is_empty(&self) -> bool {
        self.sources.is_empty()
            && !self.reloaded
            && !self.activity
            && self.playlists.is_empty()
            && self.home.is_empty()
            && self.states.is_empty()
    }
}

/// The one consumer of the Catalog's Changes for clients.
pub(super) async fn run(shared: Weak<Shared>, mut changes: Receiver<Arc<Change>>) {
    let mut pending = Pending::default();
    let mut published = HashMap::<SourceKey, Instant>::new();
    loop {
        let deadline = pending.deadline(&published);
        tokio::select! {
            next = changes.recv() => match next {
                Ok(change) => pending.add(&change),
                // A consumer that fell behind cannot know what changed.
                Err(RecvError::Lagged(_)) => pending.reloaded = true,
                Err(RecvError::Closed) => return,
            },
            () = async { sleep_until(deadline.unwrap_or_else(Instant::now)).await },
                if deadline.is_some() => {}
        }
        loop {
            match changes.try_recv() {
                Ok(change) => pending.add(&change),
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {
                    pending.reloaded = true;
                }
                Err(_) => break,
            }
        }
        let now = Instant::now();
        let ready = pending.take_ready(&published, now);
        if ready.is_empty() {
            continue;
        }
        for source in ready.sources.keys() {
            published.insert(*source, now);
        }
        let Some(shared) = shared.upgrade() else {
            return;
        };
        SourceOwner { shared }.publish_changes(ready).await;
    }
}
