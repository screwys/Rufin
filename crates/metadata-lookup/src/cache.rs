use library::Database;
use serde::{Serialize, de::DeserializeOwned};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::runtime::Handle;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CacheMode {
    Only,
    Use,
    Refresh,
}

pub struct LookupCache {
    database: Database,
    runtime: Handle,
    pub mode: CacheMode,
    network_used: AtomicBool,
    cache_used: AtomicBool,
}

pub fn cache_time() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub(crate) fn cache_key(kind: &str, input: &impl Serialize) -> String {
    format!(
        "{kind}:1:{}",
        blake3::hash(&serde_json::to_vec(input).expect("lookup request serializes"))
    )
}

impl LookupCache {
    pub fn new(database: Database, runtime: Handle, mode: CacheMode) -> Self {
        Self {
            database,
            runtime,
            mode,
            network_used: AtomicBool::new(false),
            cache_used: AtomicBool::new(false),
        }
    }

    pub fn used_network(&self) -> bool {
        self.network_used.load(Ordering::Relaxed)
    }

    pub fn used_cache(&self) -> bool {
        self.cache_used.load(Ordering::Relaxed)
    }

    pub(crate) fn fetch<T: Serialize + DeserializeOwned>(
        &self,
        key: &str,
        fetch: impl FnOnce() -> Result<T, String>,
    ) -> Result<Option<T>, String> {
        if self.mode != CacheMode::Refresh {
            let payload = self
                .runtime
                .block_on(self.database.metadata_lookup_cache(key, cache_time()))
                .map_err(|e| e.to_string())?;
            if let Some(payload) = payload
                && let Ok(value) = serde_json::from_slice(&payload)
            {
                self.cache_used.store(true, Ordering::Relaxed);
                return Ok(Some(value));
            }
        }
        if self.mode == CacheMode::Only {
            return Ok(None);
        }
        self.network_used.store(true, Ordering::Relaxed);
        let value = fetch()?;
        let payload = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
        self.runtime
            .block_on(
                self.database
                    .write_metadata_lookup_cache(key, &payload, cache_time()),
            )
            .map_err(|e| e.to_string())?;
        Ok(Some(value))
    }

    pub(crate) fn link_fingerprint(&self, uri: &str, key: &str) -> Result<(), String> {
        self.runtime
            .block_on(self.database.link_fingerprint_lookup(uri, key))
            .map_err(|e| e.to_string())
    }
}
