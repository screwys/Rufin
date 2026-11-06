//! The one change feed clients read. It folds the Catalog's Changes and core's own notices
//! into `Changed`. A source's catalog changes go out at most once a second while a sync
//! commits page after page, and at once when its listing completes; everything else goes out
//! on the next pass. Before a catalog change goes out, the selected library's music folders
//! and the source counts are current.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Weak};
use std::time::Duration;

use library::{Change, PlaylistKey, Scope, SourceKey, SourcePhase, StateChange};
use tokio::sync::broadcast::{self, error::RecvError, error::TryRecvError};
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep_until};

use super::{Shared, SourceOwner, configured_source};

/// While a source's changes keep coming, clients hear of them at most this often.
const PUBLISH_INTERVAL: Duration = Duration::from_secs(1);

/// What changed since the previous `Changed`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Changed {
    /// Anything may have changed: a reload, a lag or display flags.
    pub all: bool,
    /// Sources whose catalog rows changed.
    pub sources: Vec<SourceKey>,
    /// Tracks, albums and artists whose shown favorite or rating changed.
    pub items: Vec<StateChange>,
    pub listened: bool,
    /// Playlists whose rows, entries or file links changed.
    pub playlists: Vec<PlaylistKey>,
    /// The playlist, smart playlist or playlist folder lists changed.
    pub playlist_lists: bool,
    /// Tracks whose downloaded copy was added or removed.
    pub downloads: Vec<library::DownloadChange>,
    /// Sources whose stored Home rows or Explore shuffle changed.
    pub home: Vec<SourceKey>,
    /// The source just selected, or whose music-folder selection changed.
    pub scope: Option<SourceKey>,
    /// Pins were imported; only the pin list reads again.
    pub pins: bool,
    /// The lyrics Lyrics Search finds changed: a track's shown lyrics, or a source's whose
    /// lyrics were downloaded.
    pub lyrics: bool,
}

impl Changed {
    /// Adds what `later` changed.
    pub fn extend(&mut self, later: &Self) {
        self.all |= later.all;
        self.sources.extend(&later.sources);
        self.items.extend(later.items.iter().cloned());
        self.listened |= later.listened;
        self.playlists.extend(&later.playlists);
        self.playlist_lists |= later.playlist_lists;
        self.downloads.extend(later.downloads.iter().cloned());
        self.home.extend(&later.home);
        self.scope = later.scope.or(self.scope);
        self.pins |= later.pins;
        self.lyrics |= later.lyrics;
    }
}

/// A client's place in the feed.
#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct Changes(tokio::sync::Mutex<broadcast::Receiver<Arc<Changed>>>);

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Changes {
    /// The next change. A client that fell behind reads it as everything; None when the
    /// feed closed.
    pub async fn next(&self) -> Option<Changed> {
        match self.0.lock().await.recv().await {
            Ok(changed) => Some(changed.as_ref().clone()),
            Err(RecvError::Lagged(_)) => Some(Changed {
                all: true,
                ..Changed::default()
            }),
            Err(RecvError::Closed) => None,
        }
    }
}

/// Changes core announces that no Write states.
#[derive(Debug)]
pub(crate) enum Notice {
    /// A source's pages must read again after this instant, such as after an invalidated
    /// cover. Goes out at once unless the source went out since.
    Source(SourceKey, Instant),
    Playlist(PlaylistKey),
    /// The Home Explore shuffle.
    Home(SourceKey),
    /// A source selected, or its music folder.
    Scope(SourceKey),
    Pins,
    /// A source's lyrics were downloaded.
    Lyrics,
}

pub(crate) struct Feed {
    notices: mpsc::UnboundedSender<Notice>,
    changed: broadcast::Sender<Arc<Changed>>,
}

impl Feed {
    pub(crate) fn open() -> (Self, mpsc::UnboundedReceiver<Notice>) {
        let (notices, receiver) = mpsc::unbounded_channel();
        let feed = Self {
            notices,
            changed: broadcast::channel(64).0,
        };
        (feed, receiver)
    }
}

impl SourceOwner {
    pub(crate) fn notice(&self, notice: Notice) {
        let _ = self.shared.feed.notices.send(notice);
    }

    /// Starts noting what committed changes rows read from now may miss.
    pub fn reading(&self) -> crate::shown::Reading {
        crate::shown::Reading(self.shared.feed.changed.subscribe())
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl SourceOwner {
    pub fn changes(&self) -> Arc<Changes> {
        Arc::new(Changes(tokio::sync::Mutex::new(
            self.shared.feed.changed.subscribe(),
        )))
    }
}

/// Changes heard and not sent yet.
#[derive(Default)]
struct Pending {
    changed: Changed,
    /// Sources whose catalog changes wait for their interval.
    held: BTreeSet<SourceKey>,
    /// Held sources that go out without waiting.
    now: BTreeSet<SourceKey>,
}

impl Pending {
    fn add(&mut self, change: &Change) {
        for scope in &change.scopes {
            match scope {
                // Tracks outside every source show on any page, such as a file opened
                // directly or a playlist's entry.
                Scope::Reload | Scope::Display | Scope::Tracks { source: None, .. } => {
                    self.changed.all = true
                }
                Scope::Tracks {
                    source: Some(source),
                    ..
                }
                | Scope::Collections { source }
                | Scope::Source {
                    key: source,
                    phase: SourcePhase::Added | SourcePhase::Changed,
                } => {
                    self.held.insert(*source);
                }
                Scope::Source {
                    key,
                    phase: SourcePhase::Completed,
                } => {
                    self.held.insert(*key);
                    self.now.insert(*key);
                }
                Scope::State { items } => self.changed.items.extend(items.iter().cloned()),
                Scope::Listened { .. } => self.changed.listened = true,
                Scope::Playlist { key, .. } | Scope::PlaylistLink { key } => {
                    self.changed.playlists.push(*key)
                }
                Scope::Playlists | Scope::Smart { .. } | Scope::Folders => {
                    self.changed.playlist_lists = true
                }
                Scope::Home { source } => self.changed.home.push(*source),
                Scope::Downloads { tracks } => {
                    self.changed.downloads.extend(tracks.iter().cloned())
                }
                // The source's held rows, from this Write or an earlier one, are gone with it.
                Scope::Source {
                    key,
                    phase: SourcePhase::Removed,
                } => {
                    self.held.remove(key);
                    self.now.remove(key);
                }
                Scope::Lyrics { .. } => self.changed.lyrics = true,
                Scope::Rekey { .. }
                | Scope::Queue
                | Scope::Held { .. }
                | Scope::Analysis { .. } => {}
            }
        }
    }

    fn notice(&mut self, notice: Notice, published: &HashMap<SourceKey, Instant>) {
        match notice {
            Notice::Source(source, since) => {
                if published.get(&source).is_none_or(|last| *last < since) {
                    self.held.insert(source);
                    self.now.insert(source);
                }
            }
            Notice::Playlist(key) => self.changed.playlists.push(key),
            Notice::Home(source) => self.changed.home.push(source),
            Notice::Scope(source) => self.changed.scope = Some(source),
            Notice::Pins => self.changed.pins = true,
            Notice::Lyrics => self.changed.lyrics = true,
        }
    }

    /// When the next held source may go out.
    fn deadline(&self, published: &HashMap<SourceKey, Instant>) -> Option<Instant> {
        self.held
            .iter()
            .map(|source| {
                published
                    .get(source)
                    .map_or_else(Instant::now, |last| *last + PUBLISH_INTERVAL)
            })
            .min()
    }

    /// What may go out now: everything but the sources that went out less than an interval
    /// ago and did not complete since.
    fn take_ready(&mut self, published: &HashMap<SourceKey, Instant>, now: Instant) -> Changed {
        let ready = std::mem::take(&mut self.now);
        let (held, sources) = std::mem::take(&mut self.held)
            .into_iter()
            .partition::<BTreeSet<_>, _>(|source| {
                !ready.contains(source)
                    && published
                        .get(source)
                        .is_some_and(|last| now < *last + PUBLISH_INTERVAL)
            });
        self.held = held;
        Changed {
            sources: sources.into_iter().collect(),
            ..std::mem::take(&mut self.changed)
        }
    }
}

/// Runs the feed once the Catalog loaded; the first pass treats everything as changed.
pub(super) async fn run(
    shared: Weak<Shared>,
    mut changes: broadcast::Receiver<Arc<Change>>,
    mut notices: mpsc::UnboundedReceiver<Notice>,
) {
    let Some(catalog) = shared.upgrade().map(|shared| Arc::clone(&shared.catalog)) else {
        return;
    };
    catalog.loaded().await;
    drop(catalog);
    let mut pending = Pending::default();
    pending.changed.all = true;
    let mut published = HashMap::<SourceKey, Instant>::new();
    loop {
        loop {
            match changes.try_recv() {
                Ok(change) => pending.add(&change),
                // A feed that fell behind cannot know what changed.
                Err(TryRecvError::Lagged(_)) => pending.changed.all = true,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Closed) => return,
            }
        }
        while let Ok(notice) = notices.try_recv() {
            pending.notice(notice, &published);
        }
        let now = Instant::now();
        let ready = pending.take_ready(&published, now);
        if ready != Changed::default() {
            for source in &ready.sources {
                published.insert(*source, now);
            }
            let Some(shared) = shared.upgrade() else {
                return;
            };
            SourceOwner { shared }.flush(ready).await;
            continue;
        }
        let deadline = pending.deadline(&published);
        tokio::select! {
            next = changes.recv() => match next {
                Ok(change) => pending.add(&change),
                Err(RecvError::Lagged(_)) => pending.changed.all = true,
                Err(RecvError::Closed) => return,
            },
            notice = notices.recv() => match notice {
                Some(notice) => pending.notice(notice, &published),
                None => return,
            },
            () = async { sleep_until(deadline.unwrap_or_else(Instant::now)).await },
                if deadline.is_some() => {}
        }
    }
}

impl SourceOwner {
    /// Brings the selected library and the source counts up to date with a catalog change,
    /// then sends the change.
    async fn flush(&self, mut changed: Changed) {
        let shared = &self.shared;
        changed.playlists.sort_unstable();
        changed.playlists.dedup();
        changed.home.sort_unstable();
        changed.home.dedup();
        if changed.all || !changed.sources.is_empty() {
            if let Some(selected) = shared
                .selected()
                .filter(|selected| changed.all || changed.sources.contains(&selected.source_key))
                && let Ok(configured) =
                    configured_source(&shared.settings.load().sources, &selected.source_id)
            {
                match self
                    .library(
                        selected.source_id.clone(),
                        selected.source_key,
                        configured.music_folder_id,
                    )
                    .await
                {
                    Ok(library) => {
                        if let Some(current) = shared
                            .selected
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .as_mut()
                            .filter(|current| current.source_key == selected.source_key)
                        {
                            *current = library;
                        }
                    }
                    Err(error) => shared.warn_nonfatal(&error),
                }
            }
            if let Err(error) = shared.load_source_counts().await {
                shared.warn_nonfatal(&error);
            }
            shared.configured();
        }
        let _ = shared.feed.changed.send(Arc::new(changed));
    }
}
