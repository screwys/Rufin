//! A server's public address and the local one its requests use while this server answers
//! there.

use std::sync::RwLock;
use std::time::Duration;

/// How long a local address may take to answer before the public one stays.
pub(crate) const LOCAL_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

pub(crate) struct ServerAddress<T> {
    public: T,
    local: Option<T>,
    /// The address requests use, and the probe that chose it.
    current: RwLock<(T, u64)>,
    probes: std::sync::atomic::AtomicU64,
}

impl<T: Clone> ServerAddress<T> {
    pub(crate) fn new(public: T, local: Option<T>) -> Self {
        Self {
            current: RwLock::new((public.clone(), 0)),
            public,
            local,
            probes: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub(crate) fn current(&self) -> T {
        self.current
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0
            .clone()
    }

    /// Uses the local address while `reachable` finds this server there, and the public one
    /// otherwise. A later selection wins over one still probing.
    pub(crate) async fn select(&self, reachable: impl AsyncFnOnce(&T) -> bool) {
        let Some(local) = &self.local else {
            return;
        };
        let probe = 1 + self
            .probes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let chosen = if reachable(local).await {
            local
        } else {
            &self.public
        };
        let mut current = self
            .current
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if current.1 < probe {
            *current = (chosen.clone(), probe);
        }
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
