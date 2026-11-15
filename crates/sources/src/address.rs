//! The address a server's requests use, chosen again when the network changes or a request
//! finds no server there.

use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// How long a local address may take to answer before the next one is used.
pub(crate) const LOCAL_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// The address requests use now.
pub(crate) struct Chosen<T> {
    /// The address, and the probe that chose it.
    current: RwLock<(T, u64)>,
    probes: AtomicU64,
}

impl<T: Clone + PartialEq> Chosen<T> {
    pub(crate) fn new(first: T) -> Self {
        Self {
            current: RwLock::new((first, 0)),
            probes: AtomicU64::new(0),
        }
    }

    pub(crate) fn current(&self) -> T {
        self.current
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0
            .clone()
    }

    /// Uses the address `find` returns, and keeps the current one when it returns none. A
    /// later choice wins over one still probing. Returns whether the address changed.
    pub(crate) async fn choose(&self, find: impl AsyncFnOnce() -> Option<T>) -> bool {
        let probe = 1 + self.probes.fetch_add(1, Ordering::SeqCst);
        let Some(found) = find().await else {
            return false;
        };
        let mut current = self
            .current
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if current.1 >= probe {
            return false;
        }
        let moved = current.0 != found;
        *current = (found, probe);
        moved
    }
}

/// A server's public address and the local one its requests use while this server answers
/// there.
pub(crate) struct ServerAddress<T> {
    public: T,
    local: Option<T>,
    chosen: Chosen<T>,
}

impl<T: Clone + PartialEq> ServerAddress<T> {
    pub(crate) fn new(public: T, local: Option<T>) -> Self {
        Self {
            chosen: Chosen::new(public.clone()),
            public,
            local,
        }
    }

    pub(crate) fn current(&self) -> T {
        self.chosen.current()
    }

    /// Uses the local address while `reachable` finds this server there, and the public one
    /// otherwise. Returns whether the address changed.
    pub(crate) async fn select(&self, reachable: impl AsyncFnOnce(&T) -> bool) -> bool {
        let Some(local) = &self.local else {
            return false;
        };
        self.chosen
            .choose(async || {
                Some(if reachable(local).await {
                    local.clone()
                } else {
                    self.public.clone()
                })
            })
            .await
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for ServerAddress<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerAddress")
            .field("public", &self.public)
            .field("local", &self.local)
            .finish_non_exhaustive()
    }
}

/// The saved local address, trimmed, or None when it is empty.
pub(crate) fn optional_local(raw: Option<&str>) -> Option<&str> {
    raw.map(str::trim).filter(|address| !address.is_empty())
}
