use std::sync::Arc;

use crate::host::{AndroidError, AndroidRuntime, error};
use rufin_core::{
    runtime::{CacheKind, StorageSummary},
    settings::StorageReset,
};

#[derive(uniffi::Object)]
pub struct AndroidStorage {
    runtime: Arc<AndroidRuntime>,
}

impl AndroidStorage {
    pub(crate) fn new(runtime: Arc<AndroidRuntime>) -> Self {
        Self { runtime }
    }
}

#[uniffi::export]
impl AndroidStorage {
    pub async fn summary(&self, clear: Option<CacheKind>) -> Result<StorageSummary, AndroidError> {
        let products = self.runtime.products.clone();
        self.runtime
            .products
            .runtime
            .spawn(async move { products.storage_summary(clear).await })
            .await
            .map_err(error)
    }

    pub fn reset(&self, kind: StorageReset) -> Result<(), AndroidError> {
        self.runtime.reset_storage(kind)
    }
}

#[uniffi::export]
pub fn cache_entry_count_text(count: u64) -> String {
    localization::trn_with(
        "{count} entry",
        "{count} entries",
        count,
        &[("count", &count.to_string())],
    )
}
